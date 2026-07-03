//! The `log` subcommand: a byte-compatible port of the frozen Python logger
//! (`sensorwatch/logger.py` + the `sensorwatch/__main__.py` loop).
//!
//! [`LogWriter`] owns the files: daily rotation on local-day rollover,
//! retention pruning at startup and on each rollover, and swallowed write
//! errors so a full disk never kills the monitor. Every time-dependent
//! method takes `now` explicitly (the injectable-clock seam the Python tests
//! got from monkeypatching `pendulum.today`); only the live loop reads the
//! wall clock.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use jiff::civil::Date;
use jiff::Zoned;
use sensorwatch::{Error, Reading, Session};

use crate::cli::LogArgs;
use crate::config::Config;
use crate::jsonl::{self, LogEntry};

/// Prefix for daily log files: `<log_dir>/<LOG_PREFIX><YYYY-MM-DD>.jsonl`.
const LOG_PREFIX: &str = "sensors_";

/// Line terminator matching the Python logger's text-mode writes: CRLF on
/// Windows (where every existing file was produced), LF elsewhere.
const LINE_ENDING: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// The logger loop, ported from `sensorwatch/__main__.py`.
pub(crate) fn run(args: &LogArgs) -> ExitCode {
    // The reader depends on HWiNFO64's Windows shared memory; fail fast like
    // the Python CLI rather than warning every interval. The warn-and-retry
    // path below is for "HWiNFO not running", which only exists on Windows.
    if !cfg!(windows) {
        eprintln!(
            "sensorwatch log requires Windows (HWiNFO64 shared memory); platform is {}.",
            std::env::consts::OS
        );
        return ExitCode::from(1);
    }

    let config = Config::load(args.config.as_deref());
    log::info!(
        "Starting sensorwatch: interval={}s, log_dir={}, retention={} days",
        config.interval_seconds,
        config.log_dir,
        config.retention_days,
    );
    if config.sensor_include.is_empty() {
        log::info!("Sensor filter: capturing ALL sensors");
    } else {
        log::info!("Sensor filter (include): {:?}", config.sensor_include);
    }

    let shutdown = Arc::new((Mutex::new(false), Condvar::new()));
    {
        // The `termination` feature covers SIGINT/SIGTERM on Unix and the
        // CTRL_C/CTRL_BREAK/CTRL_CLOSE console events on Windows — a
        // superset of the Python handler set (SIGINT, SIGTERM, SIGBREAK).
        let shutdown = Arc::clone(&shutdown);
        if let Err(err) = ctrlc::set_handler(move || {
            let (lock, cvar) = &*shutdown;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }) {
            eprintln!("Could not install the shutdown signal handler: {err}");
            return ExitCode::from(1);
        }
    }

    let mut writer = match LogWriter::new(
        &config.log_dir,
        config.retention_days,
        &Zoned::now(),
        LINE_ENDING,
    ) {
        Ok(writer) => writer,
        Err(err) => {
            eprintln!(
                "Could not prepare the log directory {}: {err}",
                config.log_dir
            );
            return ExitCode::from(1);
        }
    };

    // Warns on the first unavailable poll only, then retries silently every
    // interval; it never re-warns, even if the source comes back and drops
    // out again — the Python latch is also process-lifetime.
    let mut hwinfo_warned = false;
    // interval_seconds is already >= 1 from config; keep Python's defensive
    // guard anyway.
    let interval = Duration::from_secs(config.interval_seconds.max(1) as u64);
    while !*shutdown.0.lock().unwrap() {
        match collect_live() {
            Ok(readings) => {
                let now = Zoned::now();
                let entries: Vec<LogEntry<'_>> = readings
                    .iter()
                    .filter(|r| config.matches_sensor(&r.sensor))
                    .map(LogEntry::from)
                    .collect();
                if entries.is_empty() {
                    log::debug!("No readings matched sensor filters");
                } else {
                    writer.write(&entries, &now);
                    log::debug!("Logged {} readings", entries.len());
                }
            }
            Err(Error::UnsupportedPlatform) => {
                // Unreachable behind the cfg!(windows) gate above, but keeps
                // the fatal/retry split airtight if that ever changes.
                eprintln!("sensorwatch log requires Windows (HWiNFO64 shared memory).");
                return ExitCode::from(1);
            }
            Err(err) => {
                if !hwinfo_warned {
                    log::warn!(
                        "HWiNFO64 shared memory not available ({err}) — is it \
                         running with shared memory enabled?"
                    );
                    hwinfo_warned = true;
                }
            }
        }
        if wait_for_shutdown(&shutdown, interval) {
            break;
        }
    }

    log::info!("sensorwatch stopped.");
    ExitCode::SUCCESS
}

/// One poll: open a session, copy a snapshot out, drop the session. Opening
/// per tick mirrors the Python reader (which maps the shared memory afresh
/// on every poll) and stays robust to HWiNFO restarts.
fn collect_live() -> sensorwatch::Result<Vec<Reading>> {
    let mut session = Session::new()?;
    let snapshot = session.snapshot()?;
    snapshot.to_vec()
}

/// Sleep for `interval` or until shutdown is flagged, whichever comes first;
/// returns whether shutdown was requested. The condition-variable wait
/// replaces Python's 0.1 s polling loop: shutdown wakes instantly. The outer
/// loop guards against spurious wakeups.
fn wait_for_shutdown(shutdown: &(Mutex<bool>, Condvar), interval: Duration) -> bool {
    let (lock, cvar) = shutdown;
    // An absurd interval_seconds is a valid TOML integer the config floor
    // (>= 1) accepts, and it overflows Instant arithmetic. Degrade to "sleep
    // until shutdown" instead of panicking, like the Python loop survives it.
    let deadline = Instant::now().checked_add(interval);
    let mut flagged = lock.lock().unwrap();
    while !*flagged {
        match deadline {
            Some(deadline) => {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return false;
                };
                flagged = cvar.wait_timeout(flagged, remaining).unwrap().0;
            }
            None => flagged = cvar.wait(flagged).unwrap(),
        }
    }
    true
}

/// Writes sensor readings as JSON Lines with daily file rotation.
pub(crate) struct LogWriter {
    log_dir: PathBuf,
    retention_days: i64,
    line_ending: &'static str,
    current_date: Option<Date>,
    file: Option<File>,
}

impl LogWriter {
    /// Create the log directory if needed and run retention once. No file is
    /// opened until the first write (Python opens lazily too).
    pub(crate) fn new(
        log_dir: impl Into<PathBuf>,
        retention_days: i64,
        now: &Zoned,
        line_ending: &'static str,
    ) -> std::io::Result<LogWriter> {
        let writer = LogWriter {
            log_dir: log_dir.into(),
            retention_days,
            line_ending,
            current_date: None,
            file: None,
        };
        std::fs::create_dir_all(&writer.log_dir)?;
        writer.cleanup_old_files(now.date());
        Ok(writer)
    }

    /// Write a single sample (all readings at one timestamp) as one JSONL
    /// record. IO failures — disk full, permission denied — are logged and
    /// swallowed to keep the monitor alive (Python parity).
    pub(crate) fn write(&mut self, entries: &[LogEntry<'_>], now: &Zoned) {
        let record = jsonl::format_record(now, entries);
        if let Err(err) = self.write_record(&record, now) {
            log::warn!("Failed to write log record ({err})");
        }
    }

    fn write_record(&mut self, record: &str, now: &Zoned) -> std::io::Result<()> {
        self.ensure_file(now)?;
        let file = self.file.as_mut().expect("ensure_file opened a file");
        let mut line = String::with_capacity(record.len() + self.line_ending.len());
        line.push_str(record);
        line.push_str(self.line_ending);
        // One write_all per record on an unbuffered File: no torn lines, and
        // the moral equivalent of Python's flush-after-every-write.
        file.write_all(line.as_bytes())
    }

    /// Open a new file if the (local) date has rolled over.
    fn ensure_file(&mut self, now: &Zoned) -> std::io::Result<()> {
        let today = now.date();
        if self.current_date == Some(today) && self.file.is_some() {
            return Ok(());
        }

        let rolled_over = self.current_date.is_some();
        self.file = None; // closes the previous file
        self.current_date = Some(today);
        let path = self.log_path(today);
        log::info!("Opening log file: {}", path.display());
        self.file = Some(File::options().create(true).append(true).open(&path)?);
        // Re-run retention on each daily rollover so a long-running process
        // purges old files without needing a restart.
        if rolled_over {
            self.cleanup_old_files(today);
        }
        Ok(())
    }

    fn log_path(&self, date: Date) -> PathBuf {
        // civil::Date displays as ISO `YYYY-MM-DD`.
        self.log_dir.join(format!("{LOG_PREFIX}{date}.jsonl"))
    }

    /// Delete log files strictly older than `retention_days` (a file exactly
    /// at the cutoff is kept). `retention_days <= 0` disables pruning.
    fn cleanup_old_files(&self, today: Date) {
        if self.retention_days <= 0 {
            return;
        }
        // An absurd retention value overflows date arithmetic; nothing on
        // disk can be that old, so degrade to a no-op instead of panicking.
        let Ok(span) = jiff::Span::new().try_days(self.retention_days) else {
            return;
        };
        let Ok(cutoff) = today.checked_sub(span) else {
            return;
        };

        let entries = match std::fs::read_dir(&self.log_dir) {
            Ok(entries) => entries,
            Err(err) => {
                log::warn!("Failed to scan log directory ({err})");
                return;
            }
        };
        let mut removed = 0u32;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name
                .to_str()
                .and_then(|n| n.strip_prefix(LOG_PREFIX))
                .and_then(|n| n.strip_suffix(".jsonl"))
            else {
                continue;
            };
            // Files whose date stem does not parse are skipped, not deleted.
            let Ok(file_date) = stem.parse::<Date>() else {
                continue;
            };
            if file_date < cutoff && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            log::info!(
                "Cleaned up {removed} log file(s) older than {} days",
                self.retention_days
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::{Offset, TimeZone};

    use super::*;
    use crate::testutil::TempDir;

    /// A local-zone timestamp in a fixed -05:00 zone (like the machine the
    /// Python fixtures were designed around).
    fn at(y: i16, m: i8, d: i8, hour: i8, minute: i8) -> Zoned {
        let tz = TimeZone::fixed(Offset::from_seconds(-5 * 3600).unwrap());
        date(y, m, d)
            .at(hour, minute, 48, 500_000_000)
            .to_zoned(tz)
            .unwrap()
    }

    fn entry() -> LogEntry<'static> {
        LogEntry {
            sensor: "MEG Ai1600T",
            reading: "+12V",
            kind: "Voltage",
            value: 12.03,
            min: 12.01,
            max: 12.17,
            avg: 12.06,
            unit: "V",
        }
    }

    fn touch(dir: &TempDir, name: &str) {
        std::fs::write(dir.path().join(name), b"{}\n").unwrap();
    }

    fn file_names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // Ports tests/test_logger.py::test_write_record_shape.
    #[test]
    fn write_record_shape() {
        let dir = TempDir::new();
        let now = at(2026, 2, 18, 8, 17);
        let mut writer = LogWriter::new(dir.path(), 30, &now, "\n").unwrap();
        writer.write(&[entry()], &now);

        let path = dir.path().join("sensors_2026-02-18.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            format!("{}\n", jsonl::format_record(&now, &[entry()]))
        );
        assert!(content
            .starts_with(r#"{"timestamp": "2026-02-18T08:17:48.500000-05:00", "sensors": ["#));
    }

    // Ports tests/test_logger.py::test_daily_rollover_opens_new_file.
    #[test]
    fn daily_rollover_opens_new_file() {
        let dir = TempDir::new();
        let day1 = at(2026, 2, 18, 23, 59);
        let day2 = at(2026, 2, 19, 0, 1);
        let mut writer = LogWriter::new(dir.path(), 0, &day1, "\n").unwrap();
        writer.write(&[entry()], &day1);
        writer.write(&[entry()], &day2);

        assert_eq!(
            file_names(&dir),
            vec!["sensors_2026-02-18.jsonl", "sensors_2026-02-19.jsonl"]
        );
        for name in file_names(&dir) {
            let content = std::fs::read_to_string(dir.path().join(name)).unwrap();
            assert_eq!(content.lines().count(), 1);
        }
    }

    // Ports tests/test_logger.py::test_cleanup_removes_files_older_than_retention,
    // plus the strict-`<` boundary: a file exactly retention_days old is kept.
    #[test]
    fn retention_on_startup_removes_only_files_older_than_cutoff() {
        let dir = TempDir::new();
        touch(&dir, "sensors_2026-05-06.jsonl"); // 40 days before "today"
        touch(&dir, "sensors_2026-05-16.jsonl"); // exactly 30 days: kept
        touch(&dir, "sensors_2026-06-14.jsonl"); // 1 day: kept
        let now = at(2026, 6, 15, 12, 0);
        let _writer = LogWriter::new(dir.path(), 30, &now, "\n").unwrap();

        assert_eq!(
            file_names(&dir),
            vec!["sensors_2026-05-16.jsonl", "sensors_2026-06-14.jsonl"]
        );
    }

    // Ports tests/test_logger.py::test_cleanup_skips_malformed_and_unrelated_filenames.
    #[test]
    fn retention_skips_malformed_and_unrelated_filenames() {
        let dir = TempDir::new();
        touch(&dir, "sensors_2020-01-01.jsonl"); // ancient: removed
        touch(&dir, "sensors_not-a-date.jsonl"); // unparseable stem: kept
        touch(&dir, "unrelated.jsonl"); // outside the prefix: kept
        let now = at(2026, 6, 15, 12, 0);
        let _writer = LogWriter::new(dir.path(), 30, &now, "\n").unwrap();

        assert_eq!(
            file_names(&dir),
            vec!["sensors_not-a-date.jsonl", "unrelated.jsonl"]
        );
    }

    // Ports tests/test_logger.py::test_cleanup_disabled_when_retention_non_positive.
    #[test]
    fn retention_disabled_when_zero() {
        let dir = TempDir::new();
        touch(&dir, "sensors_1999-01-01.jsonl");
        let now = at(2026, 6, 15, 12, 0);
        let _writer = LogWriter::new(dir.path(), 0, &now, "\n").unwrap();

        assert_eq!(file_names(&dir), vec!["sensors_1999-01-01.jsonl"]);
    }

    #[test]
    fn retention_survives_absurd_values() {
        let dir = TempDir::new();
        touch(&dir, "sensors_1999-01-01.jsonl");
        let now = at(2026, 6, 15, 12, 0);
        let _writer = LogWriter::new(dir.path(), i64::MAX, &now, "\n").unwrap();

        // Nothing can be older than an i64::MAX-day cutoff: no-op, no panic.
        assert_eq!(file_names(&dir), vec!["sensors_1999-01-01.jsonl"]);
    }

    // New coverage the Python suite lacks: retention re-runs on each daily
    // rollover (logger.py re-invokes _cleanup_old_files after reopening).
    #[test]
    fn retention_reruns_on_rollover() {
        let dir = TempDir::new();
        let day1 = at(2026, 6, 15, 12, 0);
        let day2 = at(2026, 6, 16, 12, 0);
        let mut writer = LogWriter::new(dir.path(), 30, &day1, "\n").unwrap();

        // Planted after startup, so only a rollover cleanup can remove it.
        touch(&dir, "sensors_2026-01-01.jsonl");
        writer.write(&[entry()], &day1); // first open is not a rollover
        assert!(dir.path().join("sensors_2026-01-01.jsonl").exists());

        writer.write(&[entry()], &day2); // rollover: cleanup re-runs
        assert!(!dir.path().join("sensors_2026-01-01.jsonl").exists());
        assert!(dir.path().join("sensors_2026-06-16.jsonl").exists());
    }

    #[test]
    fn write_failure_is_warned_and_swallowed() {
        let dir = TempDir::new();
        let now = at(2026, 6, 15, 12, 0);
        let mut writer = LogWriter::new(dir.path().join("logs"), 0, &now, "\n").unwrap();
        // Yank the directory out from under the writer: the open in
        // ensure_file fails, and write must swallow the error, not panic.
        std::fs::remove_dir_all(dir.path().join("logs")).unwrap();
        writer.write(&[entry()], &now);
    }

    /// Byte-compatibility lock: replay the exact timestamps and readings
    /// that `tests/golden/generate_fixture.py` fed the frozen Python logger,
    /// and require identical files. The fixture avoids the three documented
    /// divergences (nonzero microseconds, known types, finite floats), so
    /// the comparison is exact; it is committed with LF endings (enforced by
    /// .gitattributes) and the writer is pinned to "\n" to match.
    #[test]
    #[rustfmt::skip] // keep the fixture rows aligned with generate_fixture.py
    fn golden_bytes_match_python_fixture() {
        let dir = TempDir::new();
        let est = TimeZone::fixed(Offset::from_seconds(-5 * 3600).unwrap());
        let ist = TimeZone::fixed(Offset::from_seconds(5 * 3600 + 30 * 60).unwrap());

        let ts1 = date(2026, 2, 18).at(8, 17, 48, 123_456_000).to_zoned(est).unwrap();
        let ts2 = date(2026, 2, 18).at(20, 0, 0, 42_000).to_zoned(ist).unwrap();
        let ts3 = date(2026, 2, 19)
            .at(23, 59, 59, 999_999_000)
            .to_zoned(TimeZone::UTC)
            .unwrap();

        let e = |sensor, reading, kind, value, min, max, avg, unit| LogEntry {
            sensor,
            reading,
            kind,
            value,
            min,
            max,
            avg,
            unit,
        };
        let record1 = [
            e("MEG Ai1600T", "+12V", "Voltage", 12.03, 12.01, 12.17, 12.06, "V"),
            e("MEG Ai1600T", "PSU Temp", "Temperature", 45.5, 44.0, 47.25, 45.75, "°C"),
            e("MEG Ai1600T", "Fan 1", "Fan", 1210.0, 0.0, 1650.0, 1180.5, "RPM"),
        ];
        let record2 = [
            e("CPU Package", "Core Clock", "Clock", 4550.0, 800.0, 5125.0, 3901.25, "MHz"),
            e("CPU Package", "Offset Rail", "Other", -0.125, -0.5, 0.007, -0.0625, ""),
        ];
        let record3 = [
            e("GPU", "GPU Usage", "Usage", 87.5, 0.0, 100.0, 42.25, "%"),
            e("GPU", "Nothing", "None", 1.0, 1.0, 1.0, 1.0, ""),
        ];

        let mut writer = LogWriter::new(dir.path(), 0, &ts1, "\n").unwrap();
        writer.write(&record1, &ts1);
        writer.write(&record2, &ts2);
        writer.write(&record3, &ts3);
        drop(writer);

        let golden: [(&str, &[u8]); 2] = [
            (
                "sensors_2026-02-18.jsonl",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/golden/sensors_2026-02-18.jsonl"
                )),
            ),
            (
                "sensors_2026-02-19.jsonl",
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/golden/sensors_2026-02-19.jsonl"
                )),
            ),
        ];
        for (name, expected) in golden {
            assert!(
                !expected.contains(&b'\r'),
                "golden fixture {name} must be LF-only (check .gitattributes)"
            );
            let written = std::fs::read(dir.path().join(name)).unwrap();
            assert_eq!(
                written,
                expected,
                "byte divergence from the Python logger in {name}:\n rust: {}\n python: {}",
                String::from_utf8_lossy(&written),
                String::from_utf8_lossy(expected),
            );
        }
        assert_eq!(file_names(&dir).len(), 2, "exactly the two golden files");
    }

    #[test]
    fn wait_times_out_and_reports_no_shutdown() {
        let shutdown = (Mutex::new(false), Condvar::new());
        assert!(!wait_for_shutdown(&shutdown, Duration::from_millis(10)));
    }

    #[test]
    fn wait_survives_absurd_interval_when_already_flagged() {
        // interval_seconds = i64::MAX passes the config floor; the deadline
        // computation must not panic on Instant overflow.
        let shutdown = (Mutex::new(false), Condvar::new());
        *shutdown.0.lock().unwrap() = true;
        assert!(wait_for_shutdown(&shutdown, Duration::from_secs(u64::MAX)));
    }

    #[test]
    fn wait_without_a_deadline_wakes_on_the_shutdown_signal() {
        // The overflowed-deadline branch: no timeout, woken only by the
        // condvar — exactly what the ctrlc handler delivers.
        let shutdown = Arc::new((Mutex::new(false), Condvar::new()));
        let waker = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            *waker.0.lock().unwrap() = true;
            waker.1.notify_all();
        });
        assert!(wait_for_shutdown(&shutdown, Duration::from_secs(u64::MAX)));
        handle.join().unwrap();
    }

    #[test]
    fn crlf_line_ending_is_honored() {
        let dir = TempDir::new();
        let now = at(2026, 2, 18, 8, 17);
        let mut writer = LogWriter::new(dir.path(), 0, &now, "\r\n").unwrap();
        writer.write(&[entry()], &now);
        let bytes = std::fs::read(dir.path().join("sensors_2026-02-18.jsonl")).unwrap();
        assert!(bytes.ends_with(b"}\r\n"));
    }
}

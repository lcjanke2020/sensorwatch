//! The replay source: previously logged `sensors_*.jsonl` files as a tick
//! stream — the keystone that makes the rule engine (and the `report`
//! digest) fully developable and testable on machines with no live sensor
//! source. Replay never sleeps, and never emits [`Tick::Unavailable`]: in a
//! logged stream an outage is simply the absence of records (gap detection
//! over timestamps is the `report` command's feature).
//!
//! Replayed logs are parsed external input (SECURITY.md), and old files may
//! predate this parser, so reading is the LENIENT direction — the mirror
//! image of the strict rules parser: unknown JSON keys are tolerated
//! (forward compatibility), a malformed line is warned about, counted, and
//! skipped, and reads are bounded (`MAX_LINE_BYTES`) so a hostile line
//! cannot balloon memory. The counter is surfaced via
//! [`ReplaySource::skipped_lines`] so callers can report it rather than
//! silently pretending full coverage.
//!
//! Two Python-era quirks are handled deliberately:
//!
//! - The frozen Python logger serialized non-finite floats as bare
//!   `NaN`/`Infinity`/`-Infinity` tokens (invalid strict JSON; the Rust
//!   logger writes `null` — divergence 3 in `jsonl.rs`). A line that fails
//!   strict parsing gets one fixup pass replacing those tokens with `null`
//!   OUTSIDE JSON strings only, then a re-parse.
//! - All non-finite encodings (`null` and the fixed-up tokens alike)
//!   decode to `f64::NAN`: the wire format cannot distinguish NaN from
//!   ±infinity once written, and one canonical NaN keeps the stale rule's
//!   bit-identity well-defined.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use jiff::Timestamp;
use serde::Deserialize;

use crate::labels::normalize_type_label;
use crate::limits::MAX_LINE_BYTES;
use crate::source::{Sample, SampleReading, SampleSource, Tick};

/// How many malformed lines get an individual warning per file before the
/// per-file end-of-file summary takes over.
const DETAILED_WARNINGS_PER_FILE: u32 = 3;

/// The exact canonical prefix every sensor-log line begins with — both loggers'
/// frozen writer contract (`jsonl.rs`: key order `timestamp, sensors`, Python
/// `": "` separator). Note the space after the colon. Lines that do not start
/// with this (foreign key order, lenient formats) are never eligible for the
/// leading-timestamp precheck; they take the full parse path unchanged.
const TIMESTAMP_PREFIX: &[u8] = br#"{"timestamp": ""#;

pub(crate) struct ReplaySource {
    /// Files not yet opened, in caller order (callers sort;
    /// `sensors_YYYY-MM-DD` names sort chronologically).
    pending: std::vec::IntoIter<PathBuf>,
    current: Option<FileReader>,
    skipped_lines: u64,
    /// Files successfully opened so far — the honest "scanned" count. A
    /// candidate that exists but fails `File::open` (e.g. permissions) is
    /// warned and skipped, and is NOT counted here.
    opened_files: usize,
    /// The inclusive `[since, until]` window, set ONLY by `report` (via
    /// [`ReplaySource::with_window`]). When present, an out-of-window line whose
    /// canonical leading timestamp parses is dropped without materializing its
    /// readings. `watch --replay` never sets it, so its behavior is unchanged.
    window: Option<(Timestamp, Timestamp)>,
}

struct FileReader {
    path: PathBuf,
    reader: BufReader<File>,
    line_number: u64,
    detailed_warnings: u32,
    skipped_in_file: u64,
}

impl ReplaySource {
    /// Lazily construct over the given files: no IO happens until the first
    /// [`SampleSource::next_tick`] call (the `report` command streams
    /// windows far larger than memory). A file that cannot be opened, or
    /// fails mid-read, is warned about and skipped.
    pub(crate) fn from_files(paths: Vec<PathBuf>) -> ReplaySource {
        ReplaySource {
            pending: paths.into_iter(),
            current: None,
            skipped_lines: 0,
            opened_files: 0,
            window: None,
        }
    }

    /// Restrict the full parse to lines whose canonical leading timestamp falls
    /// in `[since, until]` (inclusive — matching `report`'s own post-parse
    /// `timestamp < since || timestamp > until` window filter exactly). A cheap
    /// precheck `report` uses so a `--last 1h` run does not fully parse the
    /// readings of the up-to-two-days of out-of-window lines the ±1-day
    /// candidate padding pulls in. Builder, so only `report` opts in;
    /// `watch --replay` wants every record and never calls this.
    pub(crate) fn with_window(mut self, since: Timestamp, until: Timestamp) -> ReplaySource {
        self.window = Some((since, until));
        self
    }

    /// Malformed (or oversized) lines skipped so far, across all files. The
    /// `report` command surfaces this count in its meta block (`skipped_lines`)
    /// so a digest never silently pretends full coverage; `watch` ignores it.
    /// Exact for every in-window line and every no-window (`watch`) run; under a
    /// window ([`ReplaySource::with_window`], report only), an out-of-window line
    /// that is valid JSON but not a sensor record is elided uncounted — the
    /// LEO-350 decision-2 caveat, an out-of-window edge that does not affect the
    /// window's own coverage.
    pub(crate) fn skipped_lines(&self) -> u64 {
        self.skipped_lines
    }

    /// Files successfully opened so far. `report` uses this for
    /// `meta.files_scanned` — a candidate that could not be opened is excluded,
    /// so the count reflects actual coverage rather than mere selection.
    pub(crate) fn files_opened(&self) -> usize {
        self.opened_files
    }

    fn finish_current_file(&mut self) {
        if let Some(fr) = self.current.take() {
            if fr.skipped_in_file > 0 {
                log::warn!(
                    "{}: skipped {} malformed line(s) during replay",
                    fr.path.display(),
                    fr.skipped_in_file
                );
            }
        }
    }
}

impl SampleSource for ReplaySource {
    fn next_tick(&mut self) -> Option<Tick> {
        let mut line = Vec::new();
        loop {
            let Some(fr) = self.current.as_mut() else {
                let path = self.pending.next()?; // all files exhausted
                match File::open(&path) {
                    Ok(file) => {
                        self.opened_files += 1;
                        self.current = Some(FileReader {
                            path,
                            reader: BufReader::new(file),
                            line_number: 0,
                            detailed_warnings: 0,
                            skipped_in_file: 0,
                        });
                    }
                    Err(err) => {
                        log::warn!("{}: cannot open for replay ({err})", path.display());
                    }
                }
                continue;
            };

            fr.line_number += 1;
            match read_line_bounded(&mut fr.reader, &mut line) {
                Err(err) => {
                    log::warn!(
                        "{}: read error during replay ({err}); abandoning file",
                        fr.path.display()
                    );
                    self.finish_current_file();
                }
                Ok(LineOutcome::Eof) => self.finish_current_file(),
                Ok(LineOutcome::Oversized) => {
                    fr.record_skip(&mut self.skipped_lines, "line exceeds the replay size cap");
                }
                Ok(LineOutcome::Line) => {
                    // Blank lines (e.g. between CRLF-era records) are not
                    // data and not an anomaly: skipped silently, uncounted.
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    // Leading-timestamp precheck (report sets a window; watch
                    // never does): an out-of-window line need not have its
                    // readings materialized. Only a line with the exact
                    // canonical `{"timestamp": "` prefix AND a parseable
                    // timestamp is eligible; anything else falls through to the
                    // full lenient parse below, unchanged.
                    if let Some((since, until)) = self.window {
                        if let Some(ts) = leading_timestamp(&line) {
                            // Out of window AND syntactically valid JSON: drop it
                            // here without materializing readings — report's own
                            // post-parse window filter would have dropped it too.
                            //
                            // CAVEAT (LEO-350 decision 2): this also elides the
                            // handling of an out-of-window line that `IgnoredAny`
                            // ACCEPTS but `RawRecord` would REJECT — any valid
                            // JSON that is not a sensor record: a missing/wrong-
                            // typed `sensors` key, entries missing required
                            // fields, etc. Such a line is skipped entirely, so it
                            // no longer (a) increments `skipped_lines`, (b) emits
                            // `record_skip`'s per-line/per-file WARN, or (c)
                            // consumes the 3-per-file detailed-warning budget —
                            // all of which the full parse would do. Everything
                            // else stays exact: invalid-JSON garbage and
                            // Python-token lines both fail this `IgnoredAny` check
                            // and fall through to `parse_line`, so garbage is
                            // still counted with the same reason as today, and an
                            // out-of-window record that only needs the fixup is
                            // parsed and then window-dropped by report exactly as
                            // before. Out-of-window hostile-input-only edge;
                            // accepted (it cannot affect the window's own data).
                            if (ts < since || ts > until)
                                && serde_json::from_slice::<serde::de::IgnoredAny>(&line).is_ok()
                            {
                                continue;
                            }
                        }
                    }
                    match parse_line(&line) {
                        Ok(sample) => return Some(Tick::Sample(sample)),
                        Err(reason) => fr.record_skip(&mut self.skipped_lines, &reason),
                    }
                }
            }
        }
    }
}

impl FileReader {
    fn record_skip(&mut self, total: &mut u64, reason: &str) {
        self.skipped_in_file += 1;
        *total += 1;
        if self.detailed_warnings < DETAILED_WARNINGS_PER_FILE {
            self.detailed_warnings += 1;
            log::warn!(
                "{}:{}: skipped malformed replay line ({reason})",
                self.path.display(),
                self.line_number
            );
        }
    }
}

enum LineOutcome {
    /// A line is in the buffer (its terminator stripped).
    Line,
    /// The line exceeded [`MAX_LINE_BYTES`]; it was discarded unbuffered.
    Oversized,
    Eof,
}

/// Read one `\n`-terminated line into `buf`, bounded: once the line passes
/// [`MAX_LINE_BYTES`] the buffer is dropped and the rest of the line is
/// consumed without being stored, so memory stays bounded no matter what is
/// on disk. A final line without a terminator counts as a line.
fn read_line_bounded(
    reader: &mut BufReader<File>,
    buf: &mut Vec<u8>,
) -> std::io::Result<LineOutcome> {
    buf.clear();
    let mut oversized = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            // EOF: whatever accumulated is the (unterminated) last line.
            return Ok(if oversized {
                LineOutcome::Oversized
            } else if buf.is_empty() {
                LineOutcome::Eof
            } else {
                LineOutcome::Line
            });
        }
        let newline = chunk.iter().position(|&b| b == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if !oversized {
            if buf.len() + take <= MAX_LINE_BYTES {
                buf.extend_from_slice(&chunk[..take]);
            } else {
                oversized = true;
                buf.clear();
            }
        }
        match newline {
            Some(pos) => {
                reader.consume(pos + 1);
                return Ok(if oversized {
                    LineOutcome::Oversized
                } else {
                    LineOutcome::Line
                });
            }
            None => {
                let len = chunk.len();
                reader.consume(len);
            }
        }
    }
}

/// One JSONL record as written by either logger. Unknown keys are tolerated
/// on purpose (forward compatibility — the snapshot format, for example,
/// carries an extra `source` key). Absent or `null` value fields both read
/// as NaN; the string fields are required — a record without them is not a
/// sensor log line.
#[derive(Deserialize)]
struct RawRecord {
    timestamp: String,
    sensors: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    sensor: String,
    reading: String,
    #[serde(rename = "type")]
    type_: String,
    value: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    avg: Option<f64>,
    unit: String,
}

/// Extract and parse the leading timestamp of a line WITHOUT parsing the rest,
/// but only when the line begins with the exact canonical [`TIMESTAMP_PREFIX`].
/// The timestamp is the bytes up to the next `"` (a valid RFC 3339 instant never
/// contains a quote or escape), parsed with the SAME `str::parse::<Timestamp>`
/// that [`parse_line`] uses, so an eligible line's precheck verdict matches what
/// the full parse would compute. Foreign/lenient encodings (no prefix,
/// unparseable timestamp, non-UTF-8) return `None` and take the full path.
fn leading_timestamp(line: &[u8]) -> Option<Timestamp> {
    let rest = line.strip_prefix(TIMESTAMP_PREFIX)?;
    // A valid RFC 3339 instant is ≤ ~35 bytes, so bound the closing-quote scan:
    // a hostile canonical-prefix line with no early quote stays O(1) here instead
    // of scanning up to the 4 MiB line cap. Behavior-identical — a quoted span
    // longer than this can't parse as a `Timestamp`, so it takes the full path
    // (via `None`) either way.
    const MAX_TIMESTAMP_BYTES: usize = 48;
    let head = &rest[..rest.len().min(MAX_TIMESTAMP_BYTES)];
    let end = head.iter().position(|&b| b == b'"')?;
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
}

pub(crate) fn parse_line(bytes: &[u8]) -> Result<Sample, String> {
    let record: RawRecord = match serde_json::from_slice(bytes) {
        Ok(record) => record,
        Err(strict_err) => match fixup_python_tokens(bytes) {
            Some(fixed) => {
                serde_json::from_slice(&fixed).map_err(|err| format!("invalid JSON: {err}"))?
            }
            None => return Err(format!("invalid JSON: {strict_err}")),
        },
    };
    // Both loggers write RFC 3339 with an offset or `Z`; pendulum-era lines
    // may have zero fractional digits. All of it parses as a jiff Timestamp.
    let timestamp: Timestamp = record
        .timestamp
        .parse()
        .map_err(|err| format!("invalid timestamp {:?}: {err}", record.timestamp))?;
    let readings = record
        .sensors
        .into_iter()
        .map(|entry| SampleReading {
            kind: normalize_type_label(&entry.type_),
            sensor: entry.sensor,
            reading: entry.reading,
            value: entry.value.unwrap_or(f64::NAN),
            min: entry.min.unwrap_or(f64::NAN),
            max: entry.max.unwrap_or(f64::NAN),
            avg: entry.avg.unwrap_or(f64::NAN),
            unit: entry.unit,
        })
        .collect();
    Ok(Sample {
        timestamp,
        raw_timestamp: record.timestamp,
        readings,
    })
}

/// Replace the Python-era bare `NaN`/`Infinity`/`-Infinity` tokens with
/// `null`, OUTSIDE JSON strings only — a sensor could legitimately be named
/// `"NaN sensor"`. In-string state is tracked byte-wise: a quote toggles it
/// unless escaped by a backslash. Returns `None` when nothing was replaced,
/// so the caller does not re-parse pointlessly.
pub(crate) fn fixup_python_tokens(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push(b);
            i += 1;
            continue;
        }
        // Longest token first, so the minus sign binds to `-Infinity`.
        let token = [&b"-Infinity"[..], b"Infinity", b"NaN"]
            .into_iter()
            .find(|token| bytes[i..].starts_with(token));
        match token {
            Some(token) => {
                out.extend_from_slice(b"null");
                changed = true;
                i += token.len();
            }
            None => {
                out.push(b);
                i += 1;
            }
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::testutil::TempDir;

    /// A realistic Rust-era line (the golden fixture's shape).
    const GOOD_LINE: &str = r#"{"timestamp": "2026-02-18T08:17:48.123456-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}]}"#;

    fn write_file(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn collect(source: &mut ReplaySource) -> Vec<Sample> {
        let mut out = Vec::new();
        while let Some(tick) = source.next_tick() {
            match tick {
                Tick::Sample(sample) => out.push(sample),
                Tick::Unavailable { .. } => panic!("replay never emits Unavailable"),
            }
        }
        out
    }

    fn replay_str(content: &str) -> (Vec<Sample>, u64) {
        let dir = TempDir::new();
        let path = write_file(&dir, "sensors_2026-02-18.jsonl", content.as_bytes());
        let mut source = ReplaySource::from_files(vec![path]);
        let samples = collect(&mut source);
        (samples, source.skipped_lines())
    }

    /// As [`replay_str`] but with a `report`-style inclusive window, so the
    /// leading-timestamp precheck is active.
    fn replay_str_windowed(content: &str, since: &str, until: &str) -> (Vec<Sample>, u64) {
        let dir = TempDir::new();
        let path = write_file(&dir, "sensors_2026-02-18.jsonl", content.as_bytes());
        let mut source = ReplaySource::from_files(vec![path])
            .with_window(since.parse().unwrap(), until.parse().unwrap());
        let samples = collect(&mut source);
        (samples, source.skipped_lines())
    }

    // ---- LEO-350 G: leading-timestamp precheck + the approved caveat ----

    #[test]
    fn leading_timestamp_extracts_only_the_canonical_prefix() {
        // Canonical prefix + real fractional-offset timestamp: extracted and
        // parsed identically to what the full parse would read.
        assert_eq!(
            leading_timestamp(GOOD_LINE.as_bytes()),
            Some("2026-02-18T08:17:48.123456-05:00".parse().unwrap())
        );
        // Foreign key order (no exact prefix) is ineligible.
        assert_eq!(
            leading_timestamp(br#"{"sensors": [], "timestamp": "2026-02-18T08:00:00Z"}"#),
            None
        );
        // Prefix present but the timestamp does not parse.
        assert_eq!(
            leading_timestamp(br#"{"timestamp": "not a time", "sensors": []}"#),
            None
        );
        // Prefix present but no closing quote.
        assert_eq!(leading_timestamp(br#"{"timestamp": "2026-02-18"#), None);
    }

    #[test]
    fn windowed_in_window_line_is_parsed_and_returned() {
        // The precheck only elides out-of-window work; an in-window record is
        // materialized and returned exactly as an unwindowed run would.
        let (samples, skipped) =
            replay_str_windowed(GOOD_LINE, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert_eq!(skipped, 0);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].readings[0].sensor, "MEG Ai1600T");
    }

    #[test]
    fn windowed_out_of_window_valid_record_is_dropped_uncounted() {
        // The optimization: an out-of-window VALID record's readings are never
        // materialized; it is neither returned nor counted (report's window
        // filter would have dropped it anyway).
        let line = r#"{"timestamp": "2020-01-01T00:00:00Z", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": "V"}]}"#;
        let (samples, skipped) =
            replay_str_windowed(line, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert!(samples.is_empty(), "out-of-window record dropped");
        assert_eq!(skipped, 0, "and not counted");
    }

    #[test]
    fn windowed_out_of_window_garbage_tail_is_still_counted() {
        // Valid canonical prefix + valid (out-of-window) timestamp + garbage
        // tail: not valid JSON, so it fails the precheck's syntax check, falls
        // through to the full parse, and is counted in skipped_lines exactly as
        // an unwindowed run would — the precheck must not hide real garbage.
        let line = r#"{"timestamp": "2020-01-01T00:00:00Z", not valid json"#;
        let (samples, skipped) =
            replay_str_windowed(line, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert!(samples.is_empty());
        assert_eq!(skipped, 1);
        // Same line unwindowed (the watch path) is also counted — proving the
        // precheck did not change garbage handling.
        assert_eq!(replay_str(line).1, 1);
    }

    #[test]
    fn windowed_out_of_window_valid_json_missing_sensors_is_uncounted_caveat() {
        // THE approved caveat (LEO-350 decision 2): an out-of-window line that
        // is syntactically valid JSON but not a sensor record (no `sensors`
        // key) is skipped UNCOUNTED under a window.
        let line = r#"{"timestamp": "2020-01-01T00:00:00Z", "not_sensors": 1}"#;
        let (samples, skipped) =
            replay_str_windowed(line, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert!(samples.is_empty());
        assert_eq!(skipped, 0, "the caveat: uncounted out of window");
        // WITHOUT a window (watch --replay), the SAME line is counted — the
        // window is exactly what elides the count, so watch is unaffected.
        assert_eq!(
            replay_str(line).1,
            1,
            "no window: the semantically-invalid line is counted as today"
        );
    }

    #[test]
    fn windowed_out_of_window_wrong_typed_sensors_is_uncounted_caveat() {
        // The caveat's class is broader than a missing `sensors` key: ANY line
        // IgnoredAny accepts but RawRecord rejects is elided out of window —
        // here `sensors` is a number, not an array.
        let line = r#"{"timestamp": "2020-01-01T00:00:00Z", "sensors": 5}"#;
        let (samples, skipped) =
            replay_str_windowed(line, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert!(samples.is_empty());
        assert_eq!(
            skipped, 0,
            "wrong-typed `sensors` is uncounted out of window"
        );
        assert_eq!(replay_str(line).1, 1, "no window: counted as today");
    }

    #[test]
    fn leading_timestamp_accepts_a_real_writer_record() {
        // Executable tie between TIMESTAMP_PREFIX and the writer it mirrors: a
        // record the logger actually emits must be precheck-eligible. If
        // jsonl.rs's byte layout (key order / `": "` separator) ever changes,
        // this fails instead of the precheck silently degrading to dead code.
        use crate::jsonl::{format_record_raw, LogEntry};
        let entry = LogEntry {
            sensor: "MEG Ai1600T",
            reading: "+12V",
            kind: "Voltage",
            value: 12.03,
            min: 12.01,
            max: 12.17,
            avg: 12.06,
            unit: "V",
        };
        let record = format_record_raw("2026-02-18T08:17:48.123456-05:00", &[entry]);
        assert_eq!(
            leading_timestamp(record.as_bytes()),
            Some("2026-02-18T08:17:48.123456-05:00".parse().unwrap())
        );
    }

    #[test]
    fn windowed_foreign_key_order_line_falls_through_to_full_parse() {
        // A record that does NOT start with the exact canonical prefix (sensors
        // first) is ineligible for the precheck: it takes the full lenient parse
        // and, being in window, is returned unchanged.
        let line = r#"{"sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": "V"}], "timestamp": "2026-02-18T08:00:00Z"}"#;
        let (samples, skipped) =
            replay_str_windowed(line, "2026-02-18T00:00:00Z", "2026-02-19T00:00:00Z");
        assert_eq!(skipped, 0);
        assert_eq!(
            samples.len(),
            1,
            "foreign key order still parses via full path"
        );
    }

    #[test]
    fn golden_fixture_round_trips() {
        // The actual bytes the byte-compat tests lock against the frozen
        // Python logger: if replay reads these, it reads real logs.
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
        let mut source = ReplaySource::from_files(vec![
            golden.join("sensors_2026-02-18.jsonl"),
            golden.join("sensors_2026-02-19.jsonl"),
        ]);
        let samples = collect(&mut source);
        assert_eq!(source.skipped_lines(), 0);
        assert_eq!(samples.len(), 3);

        let first = &samples[0];
        assert_eq!(first.raw_timestamp, "2026-02-18T08:17:48.123456-05:00");
        assert_eq!(first.readings.len(), 3);
        assert_eq!(first.readings[0].sensor, "MEG Ai1600T");
        assert_eq!(first.readings[0].reading, "+12V");
        assert_eq!(first.readings[0].kind, "Voltage");
        assert_eq!(first.readings[0].value, 12.03);
        assert_eq!(first.readings[1].unit, "°C");

        // Files in caller order, lines in file order.
        assert!(samples[0].timestamp < samples[1].timestamp);
        assert!(samples[1].timestamp < samples[2].timestamp);
        assert_eq!(samples[2].raw_timestamp, "2026-02-19T23:59:59.999999Z");
    }

    #[test]
    fn python_non_finite_tokens_are_fixed_up() {
        let line = r#"{"timestamp": "2026-02-18T08:17:48.123456-05:00", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": NaN, "min": -Infinity, "max": Infinity, "avg": 12.0, "unit": "V"}]}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        assert_eq!(samples.len(), 1);
        let r = &samples[0].readings[0];
        // The wire format cannot distinguish non-finite flavors once
        // written (the Rust logger nulls them all): everything reads NaN.
        assert!(r.value.is_nan());
        assert!(r.min.is_nan());
        assert!(r.max.is_nan());
        assert_eq!(r.avg, 12.0);
    }

    #[test]
    fn tokens_inside_strings_survive_the_fixup() {
        // The value's bare NaN forces the fixup path; the sensor name must
        // come through untouched.
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [{"sensor": "NaN Infinity -Infinity", "reading": "R", "type": "Other", "value": NaN, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": ""}]}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        assert_eq!(samples[0].readings[0].sensor, "NaN Infinity -Infinity");
        assert!(samples[0].readings[0].value.is_nan());
    }

    #[test]
    fn escaped_quotes_do_not_confuse_in_string_tracking() {
        // `"evil \" NaN"` — the escaped quote must not end the string, or
        // the in-string NaN would be nulled and the JSON corrupted.
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [{"sensor": "evil \" NaN", "reading": "R", "type": "Other", "value": NaN, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": ""}]}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        assert_eq!(samples[0].readings[0].sensor, "evil \" NaN");
        assert!(samples[0].readings[0].value.is_nan());
    }

    #[test]
    fn null_values_map_to_nan() {
        let line = r#"{"timestamp": "2026-02-18T08:17:48.123456-05:00", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": null, "min": null, "max": null, "avg": null, "unit": "V"}]}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        let r = &samples[0].readings[0];
        assert!(r.value.is_nan() && r.min.is_nan() && r.max.is_nan() && r.avg.is_nan());
    }

    #[test]
    fn type_labels_fold_to_the_canonical_vocabulary() {
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [{"sensor": "S", "reading": "A", "type": "unknown(35)", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": ""}, {"sensor": "S", "reading": "B", "type": "Temperature", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": ""}]}"#;
        let (samples, _) = replay_str(line);
        assert_eq!(samples[0].readings[0].kind, "unknown");
        assert_eq!(samples[0].readings[1].kind, "Temperature");
    }

    #[test]
    fn pendulum_zero_fraction_timestamp_parses() {
        // pendulum omits fractional digits at exactly zero microseconds.
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": []}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        assert_eq!(samples[0].raw_timestamp, "2026-02-18T08:17:48-05:00");
    }

    #[test]
    fn crlf_lines_parse_and_blank_lines_are_uncounted() {
        // Python-era files are CRLF end-to-end and may carry blank lines;
        // neither is data loss, so skipped_lines stays 0.
        let content = format!("{GOOD_LINE}\r\n\r\n   \r\n{GOOD_LINE}\r\n");
        let (samples, skipped) = replay_str(&content);
        assert_eq!(samples.len(), 2);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn malformed_line_is_skipped_counted_and_recovery_continues() {
        let content = format!("{GOOD_LINE}\nthis is not json\n{GOOD_LINE}\n");
        let (samples, skipped) = replay_str(&content);
        assert_eq!(samples.len(), 2);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn missing_timestamp_key_skips_the_line() {
        let (samples, skipped) = replay_str(r#"{"sensors": []}"#);
        assert!(samples.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn unparseable_timestamp_skips_the_line() {
        let (samples, skipped) = replay_str(r#"{"timestamp": "not a time", "sensors": []}"#);
        assert!(samples.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn wrong_value_types_skip_the_line() {
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": "12.0", "min": 0.0, "max": 0.0, "avg": 0.0, "unit": "V"}]}"#;
        let (samples, skipped) = replay_str(line);
        assert!(samples.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // Forward compatibility: e.g. the snapshot format's extra `source`.
        let line = r#"{"timestamp": "2026-02-18T08:17:48-05:00", "future_key": {"a": 1}, "sensors": [{"source": "HWiNFO", "sensor": "S", "reading": "R", "type": "Voltage", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": "V"}]}"#;
        let (samples, skipped) = replay_str(line);
        assert_eq!(skipped, 0);
        assert_eq!(samples[0].readings[0].sensor, "S");
    }

    #[test]
    fn files_replay_in_caller_order_and_skips_accumulate_across_files() {
        let dir = TempDir::new();
        let a = write_file(
            &dir,
            "sensors_2026-02-18.jsonl",
            format!("{GOOD_LINE}\ngarbage\n").as_bytes(),
        );
        let b = write_file(
            &dir,
            "sensors_2026-02-19.jsonl",
            format!("also garbage\n{GOOD_LINE}\n").as_bytes(),
        );
        let mut source = ReplaySource::from_files(vec![a, b]);
        let samples = collect(&mut source);
        assert_eq!(samples.len(), 2);
        assert_eq!(source.skipped_lines(), 2);
    }

    #[test]
    fn unopenable_file_is_skipped_and_replay_continues() {
        let dir = TempDir::new();
        let a = write_file(&dir, "a.jsonl", format!("{GOOD_LINE}\n").as_bytes());
        let missing = dir.path().join("nope.jsonl");
        let b = write_file(&dir, "b.jsonl", format!("{GOOD_LINE}\n").as_bytes());
        let mut source = ReplaySource::from_files(vec![a, missing, b]);
        assert_eq!(collect(&mut source).len(), 2);
    }

    #[test]
    fn empty_file_yields_no_ticks() {
        let (samples, skipped) = replay_str("");
        assert!(samples.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn empty_sensors_array_is_a_valid_zero_reading_sample() {
        // Nothing writes these today, but a third-party producer might; a
        // missing rule correctly sees every series as absent in it.
        let (samples, skipped) =
            replay_str(r#"{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": []}"#);
        assert_eq!(skipped, 0);
        assert_eq!(samples.len(), 1);
        assert!(samples[0].readings.is_empty());
    }

    #[test]
    fn oversized_line_is_skipped_bounded_and_recovery_continues() {
        // One line over the cap: counted, not buffered, next line intact.
        let mut content = Vec::new();
        content.extend_from_slice(GOOD_LINE.as_bytes());
        content.push(b'\n');
        content.extend_from_slice(&vec![b'x'; MAX_LINE_BYTES + 1]);
        content.push(b'\n');
        content.extend_from_slice(GOOD_LINE.as_bytes());
        content.push(b'\n');

        let dir = TempDir::new();
        let path = write_file(&dir, "big.jsonl", &content);
        let mut source = ReplaySource::from_files(vec![path]);
        let samples = collect(&mut source);
        assert_eq!(samples.len(), 2);
        assert_eq!(source.skipped_lines(), 1);
    }

    #[test]
    fn deeply_nested_json_is_skipped_not_crashed() {
        // serde_json's recursion limit turns a bracket bomb into a parse
        // error, which is just another skipped line.
        let bomb = "[".repeat(1000);
        let (samples, skipped) = replay_str(&bomb);
        assert!(samples.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn invalid_utf8_is_skipped() {
        let dir = TempDir::new();
        let mut content = GOOD_LINE.as_bytes().to_vec();
        content.push(b'\n');
        content.extend_from_slice(b"{\"timestamp\": \"\xff\xfe\", \"sensors\": []}\n");
        let path = write_file(&dir, "bad-utf8.jsonl", &content);
        let mut source = ReplaySource::from_files(vec![path]);
        assert_eq!(collect(&mut source).len(), 1);
        assert_eq!(source.skipped_lines(), 1);
    }

    #[test]
    fn top_level_non_objects_are_skipped() {
        let (samples, skipped) = replay_str("\"just a string\"\n[1, 2]\n42\n");
        assert!(samples.is_empty());
        assert_eq!(skipped, 3);
    }
}

//! Binary-level tests for the `watch` subcommand's CLI contract.
//!
//! Everything is driven through recorded `--replay` fixtures so the tests are
//! deterministic and platform-independent: the emitted event JSON, spool
//! contents, event files, and the exit-code contract. The one live test
//! (`one_shot_timeout_exits_zero`) runs only off Windows, where the live
//! source deterministically reports the sensor source as unavailable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

// ---- process helpers (mirroring tests/log_cli.rs) ----

fn sensorwatch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .output()
        .expect("failed to run the sensorwatch binary")
}

/// Run to completion, but kill and fail if the process outlives 10 s — so a
/// regression that fails to terminate (a replay that never exhausts, a
/// timeout that never elapses) fails fast instead of hanging CI. Compiled on
/// all platforms: `follow_replay` relies on it to guard the follow loop.
fn run_bounded(args: &[&str]) -> Output {
    use std::time::{Duration, Instant};

    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait().expect("could not poll the child") {
            Some(_) => return child.wait_with_output().expect("could not collect output"),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`sensorwatch {}` did not exit within 10s", args.join(" "));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---- fixtures ----

/// A rising-then-falling `+12V` series: never violates, then two violating
/// samples (fires the for_samples=2 rule on the second), then a recovery
/// (clears it). The lines are column-0 so the file bytes are clean JSONL.
const FIXTURE: &str = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 11.9, "max": 12.1, "avg": 12.0, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.5, "min": 11.4, "max": 12.1, "avg": 11.95, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:20.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.4, "min": 11.4, "max": 12.1, "avg": 11.9, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:30.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.9, "min": 11.4, "max": 12.1, "avg": 11.92, "unit": "V"}]}
"#;

/// A single non-violating sample: no rule fires.
const NON_FIRING: &str = r#"{"timestamp": "2026-02-18T09:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 11.9, "max": 12.1, "avg": 12.0, "unit": "V"}]}
"#;

/// The canonical critical threshold rule (fires on the 3rd sample of FIXTURE).
const PSU_RULE: &str = r#"
[[rules]]
name = "psu-12v-sag"
kind = "threshold"
sensor = "MEG Ai1600T"
reading = "+12V"
type = "Voltage"
metric = "value"
op = "<"
threshold = 11.6
clear = 11.8
for_samples = 2
severity = "critical"
"#;

/// A warning rule (fires on the 2nd sample) plus the critical rule (fires on
/// the 3rd) — for the --min-severity filter test.
const TWO_RULES: &str = r#"
[[rules]]
name = "warn-sag"
kind = "threshold"
sensor = "MEG Ai1600T"
reading = "+12V"
type = "Voltage"
metric = "value"
op = "<"
threshold = 11.9
for_samples = 1
severity = "warning"

[[rules]]
name = "crit-sag"
kind = "threshold"
sensor = "MEG Ai1600T"
reading = "+12V"
type = "Voltage"
metric = "value"
op = "<"
threshold = 11.6
clear = 11.8
for_samples = 2
severity = "critical"
"#;

/// The exact event stdout expects for FIXTURE + PSU_RULE on a fresh state dir.
const FIRED_EVENT: &str = r#"{"schema_version":1,"seq":1,"id":"psu-12v-sag-1","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.4,"unit":"V","threshold":11.6,"samples_in_violation":2}"#;

// ---- temp dir (integration tests cannot see crate::testutil) ----

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "sensorwatch-watch-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a config.toml with a 1 s interval, a `logs/` state dir under `dir`,
/// and the given `[[rules]]` TOML; returns the config path. The log_dir is a
/// TOML literal string so Windows backslashes need no escaping.
fn write_config(dir: &Path, rules_toml: &str) -> PathBuf {
    let log_dir = dir.join("logs");
    let config = format!(
        "[general]\ninterval_seconds = 1\nlog_dir = '{}'\nretention_days = 30\n{rules_toml}",
        log_dir.display()
    );
    let path = dir.join("config.toml");
    std::fs::write(&path, config).unwrap();
    path
}

fn write_str(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("temp path is valid UTF-8")
}

// ---- tests ----

#[test]
fn watch_help_lists_flags() {
    let output = sensorwatch(&["watch", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for flag in [
        "--config",
        "--verbose",
        "--follow",
        "--timeout",
        "--rule",
        "--min-severity",
        "--spool-dir",
        "--replay",
    ] {
        assert!(text.contains(flag), "help is missing {flag}:\n{text}");
    }
}

#[test]
fn zero_rules_is_usage_error() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), ""); // [general] only, no [[rules]]
    let output = sensorwatch(&["watch", "--config", arg(&config)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("rules"));
    assert!(stdout(&output).is_empty());
}

#[test]
fn invalid_rules_exit_two() {
    // A threshold rule missing its required `op`.
    let dir = TempDir::new();
    let config = write_config(
        dir.path(),
        r#"
[[rules]]
name = "bad"
kind = "threshold"
sensor = "MEG Ai1600T"
metric = "value"
threshold = 11.6
severity = "warning"
"#,
    );
    let output = sensorwatch(&["watch", "--config", arg(&config)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid rules config"));
    assert!(stdout(&output).is_empty());
}

#[test]
fn unknown_rule_filter_exits_two() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let output = sensorwatch(&["watch", "--config", arg(&config), "--rule", "nope"]);
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("unknown --rule"), "{err}");
    assert!(
        err.contains("psu-12v-sag"),
        "should list available names: {err}"
    );
}

#[test]
fn one_shot_fires_ten_with_stdout_and_spool() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let fixture = write_str(dir.path(), "fix.jsonl", FIXTURE);
    let spool = dir.path().join("spool");
    let output = run_bounded(&[
        "watch",
        "--config",
        arg(&config),
        "--replay",
        arg(&fixture),
        "--spool-dir",
        arg(&spool),
    ]);
    assert_eq!(output.status.code(), Some(10));

    let out = stdout(&output);
    assert_eq!(out.trim_end(), FIRED_EVENT);

    // Exactly one spool file, exact name, content == the stdout line (both
    // JSON + LF).
    let spool_file = spool.join("0000000001-psu-12v-sag.json");
    assert_eq!(std::fs::read_to_string(&spool_file).unwrap(), out);
    assert_eq!(
        std::fs::read_dir(&spool).unwrap().count(),
        1,
        "exactly one spool file"
    );

    // The sequence persisted.
    let seq = std::fs::read_to_string(dir.path().join("logs").join("watch.seq")).unwrap();
    assert_eq!(seq.trim(), "1");
}

#[test]
fn one_shot_replay_exhausted_exits_zero() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let fixture = write_str(dir.path(), "fix.jsonl", NON_FIRING);
    let output = run_bounded(&["watch", "--config", arg(&config), "--replay", arg(&fixture)]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
}

// Off Windows the live source only ever yields Unavailable, so a threshold
// rule freezes and the heartbeat deadline is what ends the run.
#[cfg(not(windows))]
#[test]
fn one_shot_timeout_exits_zero() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let output = run_bounded(&["watch", "--config", arg(&config), "--timeout", "1"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
}

#[test]
fn follow_replay_writes_event_file_and_exits_zero() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let fixture = write_str(dir.path(), "fix.jsonl", FIXTURE);
    let output = run_bounded(&[
        "watch",
        "--follow",
        "--config",
        arg(&config),
        "--replay",
        arg(&fixture),
    ]);
    assert_eq!(output.status.code(), Some(0));

    let log_dir = dir.path().join("logs");
    let mut event_files = Vec::new();
    let mut sensor_files = Vec::new();
    for entry in std::fs::read_dir(&log_dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.starts_with("events_") && name.ends_with(".jsonl") {
            event_files.push(name);
        } else if name.starts_with("sensors_") {
            sensor_files.push(name);
        }
    }
    assert_eq!(
        event_files.len(),
        1,
        "exactly one events file: {event_files:?}"
    );
    assert!(
        sensor_files.is_empty(),
        "replay follow must not re-log sensors: {sensor_files:?}"
    );

    let content = std::fs::read_to_string(log_dir.join(&event_files[0])).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "fired + cleared: {content}");
    assert!(
        lines[0].contains(r#""state":"fired""#) && lines[0].contains(r#""seq":1"#),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(r#""state":"cleared""#) && lines[1].contains(r#""seq":2"#),
        "{}",
        lines[1]
    );
}

#[test]
fn sequence_persists_across_runs() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let fixture = write_str(dir.path(), "fix.jsonl", FIXTURE);
    let config = arg(&config).to_owned();
    let fixture = arg(&fixture).to_owned();
    let args = ["watch", "--config", &config, "--replay", &fixture];

    let first = run_bounded(&args);
    assert_eq!(first.status.code(), Some(10));

    let second = run_bounded(&args);
    assert_eq!(second.status.code(), Some(10));
    let out = stdout(&second);
    assert!(out.contains(r#""seq":2"#), "{out}");
    assert!(out.contains(r#""id":"psu-12v-sag-2""#), "{out}");
}

#[test]
fn min_severity_filters_rules() {
    // Baseline: without a filter the warning rule (2nd sample) beats the
    // critical rule (3rd sample).
    let base = TempDir::new();
    let base_cfg = write_config(base.path(), TWO_RULES);
    let base_fix = write_str(base.path(), "fix.jsonl", FIXTURE);
    let baseline = run_bounded(&[
        "watch",
        "--config",
        arg(&base_cfg),
        "--replay",
        arg(&base_fix),
    ]);
    assert_eq!(baseline.status.code(), Some(10));
    let baseline_out = stdout(&baseline);
    assert!(
        baseline_out.contains(r#""rule":"warn-sag""#),
        "{baseline_out}"
    );

    // With --min-severity critical the warning rule is filtered out, so the
    // critical rule's event surfaces instead.
    let dir = TempDir::new();
    let config = write_config(dir.path(), TWO_RULES);
    let fixture = write_str(dir.path(), "fix.jsonl", FIXTURE);
    let output = run_bounded(&[
        "watch",
        "--config",
        arg(&config),
        "--replay",
        arg(&fixture),
        "--min-severity",
        "critical",
    ]);
    assert_eq!(output.status.code(), Some(10));
    let out = stdout(&output);
    assert!(out.contains(r#""rule":"crit-sag""#), "{out}");
    assert!(out.contains(r#""severity":"critical""#), "{out}");
}

// A follow-mode watcher must translate SIGINT into exit 130 (unlike `log`,
// which exits 0). Unix-only: it sends a real signal via `kill -INT`.
#[cfg(unix)]
#[test]
fn sigint_exits_130() {
    use std::time::{Duration, Instant};

    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(["watch", "--follow", "--config", arg(&config)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");

    std::thread::sleep(Duration::from_secs(1));
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("failed to run kill");
    assert!(killed.success(), "kill -INT failed");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("could not poll the child") {
            assert_eq!(status.code(), Some(130), "SIGINT should map to exit 130");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`watch --follow` did not exit within 10s of SIGINT");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

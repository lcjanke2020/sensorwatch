//! Binary-level tests for the `watch` subcommand's CLI contract.
//!
//! Everything is driven through recorded `--replay` fixtures so the tests are
//! deterministic and platform-independent: the emitted event JSON, spool
//! contents, event files, and the exit-code contract. The one live test
//! (`one_shot_timeout_exits_zero`) runs only off Windows, where the live
//! source deterministically reports the sensor source as unavailable.

// The `kill -INT` regression is the only direct `Command` user, and it is
// Unix-only, so gate the import to keep the Windows `-D warnings` gate green.
#[cfg(unix)]
use std::process::Command;

mod common;
use common::*;

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
    let config = write_config(dir.path(), "", 1, false); // [general] only, no [[rules]]
    let output = sensorwatch(&["watch", "--config", arg(&config)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("rules"));
    assert!(stdout(&output).is_empty());
}

#[test]
fn empty_rules_error_emits_no_config_warning() {
    // Regression for LEO-350 review finding 1: the shared loader must not run
    // the lenient config parse before watch's empty-rules rejection. A
    // warn-provoking `[general]` value (interval_seconds below the minimum)
    // would otherwise print a config warning on this exit-2 stderr that the
    // pre-LEO-350 binary never emitted. `[general]`-only config, no `[[rules]]`.
    let dir = TempDir::new();
    let config = write_str(
        dir.path(),
        "config.toml",
        "[general]\ninterval_seconds = 0\n",
    );
    let output = sensorwatch(&["watch", "--config", arg(&config)]);
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("has no [[rules]] to evaluate"), "{err}");
    assert!(
        !err.contains("interval_seconds"),
        "the lenient config warning must not precede the empty-rules usage error:\n{err}"
    );
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
        1,
        false,
    );
    let output = sensorwatch(&["watch", "--config", arg(&config)]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid rules config"));
    assert!(stdout(&output).is_empty());
}

#[test]
fn unknown_rule_filter_exits_two() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE, 1, false);
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
    let config = write_config(dir.path(), PSU_RULE, 1, false);
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
    let config = write_config(dir.path(), PSU_RULE, 1, false);
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
    let config = write_config(dir.path(), PSU_RULE, 1, false);
    let output = run_bounded(&["watch", "--config", arg(&config), "--timeout", "1"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
}

#[test]
fn follow_replay_writes_event_file_and_exits_zero() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE, 1, false);
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
    let config = write_config(dir.path(), PSU_RULE, 1, false);
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
    let base_cfg = write_config(base.path(), TWO_RULES, 1, false);
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
    let config = write_config(dir.path(), TWO_RULES, 1, false);
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

// Readiness gate for the signal tests: `watch` installs its shutdown handler
// (watch.rs step 6) strictly BEFORE `SeqStore::open` creates the logs/ state
// dir (step 7), so the dir appearing proves the signal-to-130 mapping is armed.
// The config is written with create_logs_dir=false, so the gate cannot pass
// early. This replaces a fixed pre-signal sleep, which raced on loaded runners
// (a signal delivered before the handler is installed kills the process with
// the default disposition, not exit 130).
#[cfg(any(unix, windows))]
fn wait_until_armed(child: &mut std::process::Child, dir: &std::path::Path) {
    use std::time::{Duration, Instant};

    let logs_dir = dir.join("logs");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !logs_dir.exists() {
        if let Some(status) = child.try_wait().expect("could not poll the child") {
            panic!("watcher exited before arming its handler: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("logs/ state dir never appeared within 10s; watcher not armed");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Poll the child for the exit-130 contract, with a bounded kill-on-timeout.
#[cfg(any(unix, windows))]
fn wait_for_exit_130(mut child: std::process::Child, what: &str) {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("could not poll the child") {
            assert_eq!(status.code(), Some(130), "{what} should map to exit 130");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`watch --follow` did not exit within 10s of {what}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// A follow-mode watcher must translate SIGINT into exit 130 (unlike `log`,
// which exits 0). Unix-only: it sends a real signal via `kill -INT`.
#[cfg(unix)]
#[test]
fn sigint_exits_130() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE, 1, false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(["watch", "--follow", "--config", arg(&config)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");

    wait_until_armed(&mut child, dir.path());

    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("failed to run kill");
    assert!(killed.success(), "kill -INT failed");

    wait_for_exit_130(child, "SIGINT");
}

// The Windows counterpart: a real console control event must map to exit 130
// through the ctrlc `termination` handler. CTRL_BREAK is used because — unlike
// CTRL_C — it can be delivered to a single process group, so the event reaches
// only the spawned watcher (in its own group via CREATE_NEW_PROCESS_GROUP),
// never this test runner.
#[cfg(windows)]
#[test]
fn ctrl_break_exits_130() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CTRL_BREAK_EVENT: u32 = 1;
    // Minimal hand-declared kernel32 surface — no new dependency for one call.
    // GenerateConsoleCtrlEvent requires sender and target to share a console,
    // which a child spawned without DETACHED_PROCESS/CREATE_NO_WINDOW does.
    #[link(name = "kernel32")]
    extern "system" {
        fn GenerateConsoleCtrlEvent(ctrl_event: u32, process_group_id: u32) -> i32;
    }

    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE, 1, false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(["watch", "--follow", "--config", arg(&config)])
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");

    wait_until_armed(&mut child, dir.path());

    // SAFETY: plain Win32 call; the process-group id is the child's pid, which
    // is a valid group id because the child was spawned as a new group leader.
    let delivered = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    if delivered == 0 {
        // Fail loudly — no silent skip. A skip here would be invisible in CI
        // (libtest captures stderr on passing tests), so a delivery regression
        // (e.g. losing CREATE_NEW_PROCESS_GROUP, or a runner without a shared
        // console) would quietly erase the Windows half of the exit-130
        // coverage while staying green — the same failure shape as the fixed
        // pre-signal sleep this file replaced. A green run of this test is
        // therefore proof the event was delivered and mapped to exit 130; if
        // an environment genuinely cannot deliver console events, descope this
        // leg deliberately (with a rationale) rather than skipping silently.
        let err = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        panic!("GenerateConsoleCtrlEvent(CTRL_BREAK) failed: {err}");
    }

    wait_for_exit_130(child, "CTRL_BREAK");
}

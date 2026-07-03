//! Binary-level tests for the `report` subcommand's CLI contract.
//!
//! Everything is driven through recorded log fixtures under a per-test temp
//! directory, so the tests are deterministic and platform-independent: the
//! digest JSON, the aggregates, the re-derived violations, the sampling gaps,
//! the byte cap, the display filters, and the exit-code contract. The windows
//! are pinned with explicit `--since`/`--until` (never "now"), which is what
//! makes the exact-bytes assertions reproducible on any machine or time zone.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

// ---- process helpers (mirroring tests/watch_cli.rs) ----

fn sensorwatch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .output()
        .expect("failed to run the sensorwatch binary")
}

/// Run to completion, killing and failing if the process outlives 10 s — a
/// regression that fails to terminate fails fast instead of hanging CI.
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

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON")
}

// ---- fixtures ----

/// The canonical critical threshold rule: fires on the 2nd consecutive `+12V`
/// sample below 11.6, clears at 11.8. Matches the plan's PSU_RULE.
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

/// Day 1: five samples of `+12V` (V) and `PSU Temperature` (°C). The lifetime
/// `min`/`max`/`avg` fields are deliberately WRONG for the window (temperature
/// `max: 47.0`, `+12V min: 11.0`) so that reading them instead of recomputing
/// would fail the aggregate assertions. Sample 4's temperature is `null`.
const DAY1: &str = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 30.0, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 30.5, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-18T08:00:20.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.5, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 31.0, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-18T08:00:30.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.25, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": null, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-18T08:02:30.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 32.0, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
"#;

/// Day 2: three more samples (s6–s8), continuing both series.
const DAY2: &str = r#"{"timestamp": "2026-02-19T08:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.5, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 33.0, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-19T08:00:10.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 33.5, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
{"timestamp": "2026-02-19T08:00:20.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.25, "min": 11.0, "max": 12.9, "avg": 12.4, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "PSU Temperature", "type": "Temperature", "value": 34.0, "min": 20.0, "max": 47.0, "avg": 40.0, "unit": "°C"}]}
"#;

/// The exact fired-event bytes the full-window digest embeds (compact),
/// digest-local seq 1.
const FIRED_EVENT: &str = r#"{"schema_version":1,"seq":1,"id":"psu-12v-sag-1","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:30.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.25,"unit":"V","threshold":11.6,"samples_in_violation":2}"#;

/// The exact cleared-event bytes, digest-local seq 2.
const CLEARED_EVENT: &str = r#"{"schema_version":1,"seq":2,"id":"psu-12v-sag-2","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"cleared","timestamp":"2026-02-18T08:02:30.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":12.0,"unit":"V","threshold":11.6,"samples_in_violation":2}"#;

/// The frozen event key set (order is asserted separately via exact bytes).
const EVENT_KEYS: [&str; 14] = [
    "schema_version",
    "seq",
    "id",
    "rule",
    "type",
    "severity",
    "state",
    "timestamp",
    "sensor",
    "reading",
    "value",
    "unit",
    "threshold",
    "samples_in_violation",
];

// The replay line-size cap (mirrors replay.rs); a line one byte over is dropped.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

// ---- temp dir (integration tests cannot see crate::testutil) ----

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "sensorwatch-report-test-{}-{}",
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

/// Write a config.toml with a 10 s interval (so the gap threshold is 30 s), a
/// `logs/` state dir under `dir`, and the given `[[rules]]` TOML; returns the
/// config path. The log_dir is a TOML literal string so backslashes need no
/// escaping. The `logs/` directory is created so the default-window
/// (zero-sample) cases still have a directory to scan.
fn write_config(dir: &Path, rules_toml: &str) -> PathBuf {
    let log_dir = dir.join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let config = format!(
        "[general]\ninterval_seconds = 10\nlog_dir = '{}'\nretention_days = 30\n{rules_toml}",
        log_dir.display()
    );
    let path = dir.join("config.toml");
    std::fs::write(&path, config).unwrap();
    path
}

/// Write a log file (raw bytes) under `dir/logs/`.
fn write_log(dir: &Path, name: &str, content: &[u8]) {
    std::fs::write(dir.join("logs").join(name), content).unwrap();
}

/// The full 8-sample fixture (day 1 + day 2) under a fresh temp dir + config.
fn full_fixture(rules: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new();
    let config = write_config(dir.path(), rules);
    write_log(dir.path(), "sensors_2026-02-18.jsonl", DAY1.as_bytes());
    write_log(dir.path(), "sensors_2026-02-19.jsonl", DAY2.as_bytes());
    (dir, config)
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("temp path is valid UTF-8")
}

/// The canonical full-window flags: covers all 8 samples across both days.
const FULL_WINDOW: [&str; 4] = [
    "--since",
    "2026-02-18T00:00:00-05:00",
    "--until",
    "2026-02-19T12:00:00-05:00",
];

// ---- tests ----

#[test]
fn report_help_lists_flags() {
    let output = sensorwatch(&["report", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for flag in [
        "--config",
        "--since",
        "--until",
        "--last",
        "--max-bytes",
        "--top",
        "--match",
        "--type",
        "--log-dir",
        "--indent",
        "--verbose",
    ] {
        assert!(text.contains(flag), "help is missing {flag}:\n{text}");
    }
}

#[test]
fn full_window_digest_has_exact_values_and_key_order() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut args = vec!["report", "--config", arg(&config)];
    args.extend_from_slice(&FULL_WINDOW);
    let output = run_bounded(&args);
    assert_eq!(output.status.code(), Some(0));

    let out = stdout(&output);
    // Compact key order (a substring pins the meta layout contract).
    assert!(
        out.contains(r#"{"schema_version":1,"meta":{"window":{"since":"2026-02-18T05:00:00Z","until":"2026-02-19T17:00:00Z"}"#),
        "meta key order/window wrong:\n{out}"
    );
    // The two violations appear verbatim, in chronological (seq) order.
    assert!(out.contains(FIRED_EVENT), "missing fired event:\n{out}");
    assert!(out.contains(CLEARED_EVENT), "missing cleared event:\n{out}");
    let fired_at = out.find(FIRED_EVENT).unwrap();
    let cleared_at = out.find(CLEARED_EVENT).unwrap();
    assert!(fired_at < cleared_at, "fired must precede cleared");

    let d = json(&output);
    let meta = &d["meta"];
    assert_eq!(meta["samples"], 8);
    assert_eq!(meta["files_scanned"], 2);
    assert_eq!(meta["series_total"], 2);
    assert_eq!(meta["skipped_lines"], 0);
    assert_eq!(meta["rules_evaluated"], 1);
    assert_eq!(meta["first_sample"], "2026-02-18T08:00:00.000000-05:00");
    assert_eq!(meta["last_sample"], "2026-02-19T08:00:20.000000-05:00");
    let trunc = &meta["truncated"];
    for key in ["readings", "violations", "gaps"] {
        assert_eq!(
            trunc[format!("{key}_shown")],
            trunc[format!("{key}_total")],
            "{key} shown != total"
        );
    }

    // Reading rows: +12V is forced first (in violation), Temp second.
    let readings = d["readings"].as_array().unwrap();
    assert_eq!(readings.len(), 2);
    let v12 = &readings[0];
    assert_eq!(v12["reading"], "+12V");
    assert_eq!(v12["samples"], 8);
    assert_eq!(v12["non_finite"], 0);
    assert_eq!(v12["first"], 12.0);
    assert_eq!(v12["last"], 12.25);
    assert_eq!(v12["min"], 11.25);
    assert_eq!(v12["max"], 12.5);
    assert_eq!(v12["avg"], 11.9375);
    assert_eq!(v12["delta"], 0.25);
    assert_eq!(v12["in_violation"], true);

    let temp = &readings[1];
    assert_eq!(temp["reading"], "PSU Temperature");
    assert_eq!(temp["samples"], 8);
    assert_eq!(temp["non_finite"], 1);
    assert_eq!(temp["first"], 30.0);
    assert_eq!(temp["last"], 34.0);
    assert_eq!(temp["min"], 30.0);
    assert_eq!(temp["max"], 34.0);
    assert_eq!(temp["avg"], 32.0);
    assert_eq!(temp["delta"], 4.0);
    assert_eq!(temp["in_violation"], false);

    // Gaps: the 2-minute pause, then the overnight gap. Threshold is 30 s.
    let gaps = d["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2);
    assert_eq!(gaps[0]["seconds"], 120);
    assert_eq!(gaps[1]["seconds"], 86250);
}

#[test]
fn sub_window_re_derives_a_fresh_engine() {
    // Day 2 only: no violation streak carries over, no synthetic leading gap.
    let (_dir, config) = full_fixture(PSU_RULE);
    let output = run_bounded(&[
        "report",
        "--config",
        arg(&config),
        "--since",
        "2026-02-19T00:00:00-05:00",
        "--until",
        "2026-02-19T12:00:00-05:00",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let d = json(&output);
    assert_eq!(d["meta"]["samples"], 3);
    assert!(
        d["violations"].as_array().unwrap().is_empty(),
        "fresh engine → no violations"
    );
    assert!(
        d["gaps"].as_array().unwrap().is_empty(),
        "no synthetic leading gap"
    );

    // Movement ranking flips: Temp (0.0303) outranks +12V (0.02); neither is
    // in violation, so there is no forced tier.
    let readings = d["readings"].as_array().unwrap();
    assert_eq!(readings[0]["reading"], "PSU Temperature");
    assert_eq!(readings[1]["reading"], "+12V");
    let v12 = &readings[1];
    assert_eq!(v12["first"], 12.5);
    assert_eq!(v12["last"], 12.25);
    assert_eq!(v12["min"], 12.0);
    assert_eq!(v12["max"], 12.5);
    assert_eq!(v12["avg"], 12.25);
    assert_eq!(v12["delta"], -0.25);
    assert_eq!(v12["in_violation"], false);
    assert_eq!(readings[0]["avg"], 33.5);
    assert_eq!(readings[0]["delta"], 1.0);
}

#[test]
fn trailing_last_window_ending_at_until() {
    // A 24 h window ending at s8's minute captures s3..s8 (6 samples): both
    // day-1 violations still fall inside it.
    let (_dir, config) = full_fixture(PSU_RULE);
    let output = run_bounded(&[
        "report",
        "--config",
        arg(&config),
        "--until",
        "2026-02-19T08:00:20-05:00",
        "--last",
        "24h",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let d = json(&output);
    assert_eq!(d["meta"]["samples"], 6, "s3..s8 inside the trailing 24 h");
    assert_eq!(
        d["violations"].as_array().unwrap().len(),
        2,
        "both violations persist"
    );
}

#[test]
fn violation_keys_match_the_frozen_event_and_no_state_is_written() {
    let (dir, config) = full_fixture(PSU_RULE);
    let mut args = vec!["report", "--config", arg(&config)];
    args.extend_from_slice(&FULL_WINDOW);
    let output = run_bounded(&args);
    assert_eq!(output.status.code(), Some(0));

    let d = json(&output);
    let violation = &d["violations"][0];
    let mut keys: Vec<&str> = violation
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected = EVENT_KEYS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "violation keys must equal the frozen Event keys"
    );

    // Read-only guarantee: report never writes watch.seq (or any state).
    assert!(
        !dir.path().join("logs").join("watch.seq").exists(),
        "report must not create watch.seq"
    );
}

#[test]
fn filters_are_display_only() {
    let (_dir, config) = full_fixture(PSU_RULE);

    // --match temperature: only the Temp row; violations (on +12V) hidden; the
    // meta sample count is unfiltered.
    let mut a = vec!["report", "--config", arg(&config), "--match", "temperature"];
    a.extend_from_slice(&FULL_WINDOW);
    let d = json(&run_bounded(&a));
    let readings = d["readings"].as_array().unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(readings[0]["reading"], "PSU Temperature");
    assert!(d["violations"].as_array().unwrap().is_empty());
    assert_eq!(d["meta"]["samples"], 8, "meta.samples is unfiltered");
    assert_eq!(d["meta"]["series_total"], 2, "series_total is unfiltered");
    assert_eq!(
        d["meta"]["truncated"]["readings_total"], 1,
        "post-filter total"
    );

    // --type VOLTAGE: the +12V row plus both (voltage-typed) violations.
    let mut b = vec!["report", "--config", arg(&config), "--type", "VOLTAGE"];
    b.extend_from_slice(&FULL_WINDOW);
    let d = json(&run_bounded(&b));
    let readings = d["readings"].as_array().unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(readings[0]["reading"], "+12V");
    assert_eq!(d["violations"].as_array().unwrap().len(), 2);

    // --match nomatch: empty arrays, still exit 0.
    let mut c = vec!["report", "--config", arg(&config), "--match", "nomatch"];
    c.extend_from_slice(&FULL_WINDOW);
    let out = run_bounded(&c);
    assert_eq!(out.status.code(), Some(0));
    let d = json(&out);
    assert!(d["readings"].as_array().unwrap().is_empty());
    assert!(d["violations"].as_array().unwrap().is_empty());
}

#[test]
fn top_selector_keeps_the_forced_row() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut args = vec!["report", "--config", arg(&config), "--top", "1"];
    args.extend_from_slice(&FULL_WINDOW);
    let d = json(&run_bounded(&args));
    let readings = d["readings"].as_array().unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(
        readings[0]["reading"], "+12V",
        "the forced violation row survives --top 1"
    );
    assert_eq!(d["meta"]["truncated"]["readings_total"], 2);
    assert_eq!(d["meta"]["truncated"]["readings_shown"], 1);
}

/// A 2-sample fixture with 30 `S00..S29` series (each moving 1.0 → 2.0) plus a
/// violating `+12V` pair. Used to force the byte cap to drop rows.
fn many_series_fixture() -> String {
    let mut out = String::new();
    for (ts, sval) in [
        ("2026-02-18T08:00:00.000000-05:00", 1.0),
        ("2026-02-18T08:00:10.000000-05:00", 2.0),
    ] {
        let mut readings = String::from(
            r#"{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.0, "min": 11.0, "max": 12.9, "avg": 12.0, "unit": "V"}"#,
        );
        for i in 0..30 {
            readings.push_str(&format!(
                r#", {{"sensor": "Bank", "reading": "S{i:02}", "type": "Voltage", "value": {sval:.1}, "min": 0.0, "max": 9.0, "avg": 1.0, "unit": "V"}}"#
            ));
        }
        out.push_str(&format!(
            "{{\"timestamp\": \"{ts}\", \"sensors\": [{readings}]}}\n"
        ));
    }
    out
}

#[test]
fn byte_cap_drops_rows_but_keeps_meta_violations_and_forced_row() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    write_log(
        dir.path(),
        "sensors_2026-02-18.jsonl",
        many_series_fixture().as_bytes(),
    );

    let output = run_bounded(&[
        "report",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:00:00-05:00",
        "--max-bytes",
        "2000",
        "--top",
        "40",
    ]);
    assert_eq!(output.status.code(), Some(0));

    // The cap covers the JSON text; the trailing newline is excluded.
    let out = stdout(&output);
    assert!(
        out.trim_end().len() <= 2000,
        "digest is {} bytes",
        out.trim_end().len()
    );

    let d = json(&output);
    // Meta, the violation, and the forced +12V row all survive; some rows drop.
    assert!(d["meta"].is_object());
    assert_eq!(
        d["violations"].as_array().unwrap().len(),
        1,
        "fired violation kept"
    );
    let readings = d["readings"].as_array().unwrap();
    assert!(
        readings.iter().any(|r| r["reading"] == "+12V"),
        "forced row kept"
    );
    let trunc = &d["meta"]["truncated"];
    assert_eq!(trunc["readings_total"], 31, "30 banks + +12V");
    assert!(
        trunc["readings_shown"].as_u64().unwrap() < 31,
        "some rows dropped to fit"
    );
    assert_eq!(
        trunc["readings_shown"].as_u64().unwrap() as usize,
        readings.len(),
        "shown counter matches the array"
    );
}

#[test]
fn max_bytes_below_clap_floor_is_a_usage_error() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut args = vec!["report", "--config", arg(&config), "--max-bytes", "100"];
    args.extend_from_slice(&FULL_WINDOW);
    let output = sensorwatch(&args);
    assert_eq!(output.status.code(), Some(2), "clap rejects < 512");
}

#[test]
fn cannot_fit_even_minimal_digest_is_usage_error() {
    // A 512-byte cap with 16-space indentation cannot hold even the meta-only
    // digest, so the fitter fails with a pointed message.
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut args = vec![
        "report",
        "--config",
        arg(&config),
        "--max-bytes",
        "512",
        "--indent",
        "16",
    ];
    args.extend_from_slice(&FULL_WINDOW);
    let output = run_bounded(&args);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("max-bytes"),
        "stderr should mention max-bytes:\n{}",
        stderr(&output)
    );
}

#[test]
fn garbage_line_is_counted_in_skipped_lines() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    // Day 1 with a garbage line spliced in; day 2 intact. Total valid: 8.
    let day1_with_garbage = format!("{DAY1}this is not json\n");
    write_log(
        dir.path(),
        "sensors_2026-02-18.jsonl",
        day1_with_garbage.as_bytes(),
    );
    write_log(dir.path(), "sensors_2026-02-19.jsonl", DAY2.as_bytes());

    let mut args = vec!["report", "--config", arg(&config)];
    args.extend_from_slice(&FULL_WINDOW);
    let d = json(&run_bounded(&args));
    assert_eq!(d["meta"]["skipped_lines"], 1);
    assert_eq!(
        d["meta"]["samples"], 8,
        "the 8 valid samples still aggregate"
    );
}

#[test]
fn empty_log_dir_yields_a_clean_zero_sample_digest() {
    // Config points at an empty logs/ directory: the zero-sample digest is the
    // dead-logger signal, and it still exits 0.
    let dir = TempDir::new();
    let config = write_config(dir.path(), PSU_RULE);
    let mut args = vec!["report", "--config", arg(&config)];
    args.extend_from_slice(&FULL_WINDOW);
    let output = run_bounded(&args);
    assert_eq!(output.status.code(), Some(0));
    let d = json(&output);
    assert_eq!(d["meta"]["samples"], 0);
    assert_eq!(d["meta"]["files_scanned"], 0);
    assert!(d["meta"]["first_sample"].is_null());
    assert!(d["meta"]["last_sample"].is_null());
    assert!(d["readings"].as_array().unwrap().is_empty());
    assert!(d["violations"].as_array().unwrap().is_empty());
    assert!(d["gaps"].as_array().unwrap().is_empty());
}

#[test]
fn nonexistent_log_dir_override_yields_zero_samples() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut args = vec![
        "report",
        "--config",
        arg(&config),
        "--log-dir",
        "/nonexistent/sensorwatch/logs",
    ];
    args.extend_from_slice(&FULL_WINDOW);
    let output = run_bounded(&args);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json(&output)["meta"]["samples"], 0);
}

#[test]
fn bad_window_arguments_are_usage_errors() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let base = ["report", "--config"];
    let cfg = arg(&config);

    // Unparseable --since.
    assert_eq!(
        sensorwatch(&[base[0], base[1], cfg, "--since", "not-a-time"])
            .status
            .code(),
        Some(2)
    );
    // since after until.
    assert_eq!(
        sensorwatch(&[
            base[0],
            base[1],
            cfg,
            "--since",
            "2026-02-19T00:00:00-05:00",
            "--until",
            "2026-02-18T00:00:00-05:00",
        ])
        .status
        .code(),
        Some(2)
    );
    // Zero-length duration.
    assert_eq!(
        sensorwatch(&[base[0], base[1], cfg, "--last", "0s"])
            .status
            .code(),
        Some(2)
    );
    // Overflowing duration.
    assert_eq!(
        sensorwatch(&[base[0], base[1], cfg, "--last", "999999999999999999d"])
            .status
            .code(),
        Some(2)
    );
    // --since conflicts with --last (clap).
    assert_eq!(
        sensorwatch(&[
            base[0],
            base[1],
            cfg,
            "--since",
            "2026-02-18T00:00:00-05:00",
            "--last",
            "24h"
        ])
        .status
        .code(),
        Some(2)
    );
}

#[test]
fn invalid_rules_are_a_usage_error() {
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
    let mut args = vec!["report", "--config", arg(&config)];
    args.extend_from_slice(&FULL_WINDOW);
    let output = sensorwatch(&args);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid rules config"));
}

#[test]
fn no_config_at_all_reports_zero_rules() {
    // Point --config at a nonexistent path and run from a directory with no
    // ./config.toml fallback, so config resolution yields nothing: report
    // proceeds over zero rules rather than erroring.
    let dir = TempDir::new();
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args([
            "report",
            "--config",
            "/nonexistent/config.toml",
            "--log-dir",
            arg(&logs),
            "--since",
            "2026-02-18T00:00:00-05:00",
            "--until",
            "2026-02-19T12:00:00-05:00",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json(&output)["meta"]["rules_evaluated"], 0);
}

#[test]
fn indented_output_parses_to_the_same_value_as_compact() {
    let (_dir, config) = full_fixture(PSU_RULE);
    let mut compact_args = vec!["report", "--config", arg(&config)];
    compact_args.extend_from_slice(&FULL_WINDOW);
    let compact = json(&run_bounded(&compact_args));

    let mut indent_args = vec!["report", "--config", arg(&config), "--indent", "2"];
    indent_args.extend_from_slice(&FULL_WINDOW);
    let out = run_bounded(&indent_args);
    assert!(stdout(&out).contains('\n'), "indent 2 is multi-line");
    let indented = json(&out);
    assert_eq!(compact, indented);
}

#[test]
fn hostile_input_is_bounded_and_flows_valid_records_through() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), "");
    // A good line, a Python-era bare-NaN line, a CRLF line, and one oversized
    // line (one byte over the cap). The valid records aggregate; the oversized
    // line is skipped and counted.
    let good = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "PSU", "reading": "+12V", "type": "Voltage", "value": 12.0, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": "V"}]}"#;
    let nan_line = r#"{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "PSU", "reading": "+12V", "type": "Voltage", "value": NaN, "min": -Infinity, "max": Infinity, "avg": 12.0, "unit": "V"}]}"#;
    let crlf_line = r#"{"timestamp": "2026-02-18T08:00:20.000000-05:00", "sensors": [{"sensor": "PSU", "reading": "+12V", "type": "Voltage", "value": 12.5, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": "V"}]}"#;

    let mut content: Vec<u8> = Vec::new();
    content.extend_from_slice(good.as_bytes());
    content.push(b'\n');
    content.extend_from_slice(nan_line.as_bytes());
    content.push(b'\n');
    content.extend_from_slice(crlf_line.as_bytes());
    content.extend_from_slice(b"\r\n"); // CRLF terminator
    content.extend_from_slice(&vec![b'x'; MAX_LINE_BYTES + 1]);
    content.push(b'\n');
    write_log(dir.path(), "sensors_2026-02-18.jsonl", &content);

    let output = run_bounded(&[
        "report",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:00:00-05:00",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let d = json(&output);
    assert_eq!(
        d["meta"]["skipped_lines"], 1,
        "only the oversized line is skipped"
    );
    assert_eq!(
        d["meta"]["samples"], 3,
        "good + NaN + CRLF records flow through"
    );
    // The NaN sample is counted but does not enter the finite aggregates.
    let v12 = &d["readings"][0];
    assert_eq!(v12["reading"], "+12V");
    assert_eq!(v12["samples"], 3);
    assert_eq!(v12["non_finite"], 1);
    assert_eq!(v12["first"], 12.0);
    assert_eq!(v12["last"], 12.5);
}

#[test]
fn golden_python_fixtures_replay_cleanly() {
    // Cross-check against the byte-compat golden logs (real Python-logger
    // output): a window over 2026-02-18/19 must produce a sane digest, exit 0.
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let dir = TempDir::new();
    let config = write_config(dir.path(), "");
    let output = run_bounded(&[
        "report",
        "--config",
        arg(&config),
        "--log-dir",
        arg(&golden),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-20T00:00:00Z",
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let d = json(&output);
    // Three golden samples (two in the day-1 file, one in day-2), seven
    // distinct series across them.
    assert_eq!(d["meta"]["samples"], 3);
    assert_eq!(d["meta"]["files_scanned"], 2);
    assert_eq!(d["meta"]["series_total"], 7);
    assert_eq!(d["meta"]["skipped_lines"], 0);
}

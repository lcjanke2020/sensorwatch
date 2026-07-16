//! Binary-level tests for the `export` subcommand's CLI contract.
//!
//! Everything is driven through recorded log fixtures under a per-test temp
//! directory, and every window is pinned with explicit `--since`/`--until`
//! (never "now"), so the row-level assertions reproduce on any machine or
//! time zone. Exports are read back with the `parquet` crate — a regular
//! dependency of the crate (integration tests link regular dependencies, the
//! same way `common` uses `serde_json`), so this adds nothing to the tree.
//! Parquet files are written only into `TempDir`s, never into the repo.

use std::path::{Path, PathBuf};

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::{Field, Row};

mod common;
use common::*;

// ---- fixtures (DAY1/DAY2 shared with report_cli.rs) ----

/// Day 1: five samples of `+12V` (V) and `PSU Temperature` (°C). Sample 4's
/// temperature is `null` — the NULL-mapping probe. The lifetime `min`/`max`/
/// `avg` fields are populated so exporting them by accident would be visible.
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

/// The canonical full-window flags: covers all 8 samples across both days.
const FULL_WINDOW: [&str; 4] = [
    "--since",
    "2026-02-18T00:00:00-05:00",
    "--until",
    "2026-02-19T12:00:00-05:00",
];

/// The full 8-sample fixture (day 1 + day 2) under a fresh temp dir + config
/// with zero rules (`export` never reads rules).
fn full_fixture() -> (TempDir, PathBuf) {
    let dir = TempDir::new();
    let config = write_config(dir.path(), "", 10, true);
    write_log(dir.path(), "sensors_2026-02-18.jsonl", DAY1.as_bytes());
    write_log(dir.path(), "sensors_2026-02-19.jsonl", DAY2.as_bytes());
    (dir, config)
}

// ---- parquet read-back helpers ----

fn rows(path: &Path) -> Vec<Row> {
    let file = std::fs::File::open(path).expect("open the exported parquet");
    SerializedFileReader::new(file)
        .expect("read the exported parquet")
        .get_row_iter(None)
        .expect("row iter")
        .map(|row| row.expect("row"))
        .collect()
}

fn field(row: &Row, index: usize) -> Field {
    row.get_column_iter()
        .nth(index)
        .expect("column index in range")
        .1
        .clone()
}

fn timestamp_micros(row: &Row, index: usize) -> i64 {
    match field(row, index) {
        Field::TimestampMicros(v) => v,
        Field::Long(v) => v,
        other => panic!("expected a micros timestamp, got {other:?}"),
    }
}

fn string(row: &Row, index: usize) -> String {
    match field(row, index) {
        Field::Str(s) => s,
        other => panic!("expected a string, got {other:?}"),
    }
}

fn double(row: &Row, index: usize) -> f64 {
    match field(row, index) {
        Field::Double(v) => v,
        other => panic!("expected a double, got {other:?}"),
    }
}

fn micros_of(instant: &str) -> i64 {
    instant
        .parse::<jiff::Timestamp>()
        .expect("test instant parses")
        .as_microsecond()
}

// ---- tests ----

#[test]
fn export_help_lists_flags() {
    let output = sensorwatch(&["export", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for flag in [
        "--config",
        "--since",
        "--until",
        "--last",
        "--out",
        "--log-dir",
        "--verbose",
    ] {
        assert!(text.contains(flag), "help is missing {flag}:\n{text}");
    }
}

#[test]
fn full_window_exports_one_row_per_reading_per_sample() {
    let (dir, config) = full_fixture();
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        FULL_WINDOW[0],
        FULL_WINDOW[1],
        FULL_WINDOW[2],
        FULL_WINDOW[3],
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let all = rows(&out);
    assert_eq!(all.len(), 16, "8 samples x 2 readings");
    // Ordering is file order, then line order, then readings-array order.
    let first = &all[0];
    assert_eq!(
        timestamp_micros(first, 0),
        micros_of("2026-02-18T08:00:00.000000-05:00")
    );
    assert_eq!(string(first, 1), "MEG Ai1600T");
    assert_eq!(string(first, 2), "+12V");
    assert_eq!(string(first, 3), "Voltage");
    assert_eq!(double(first, 4), 12.0);
    assert_eq!(string(first, 5), "V");
    // Sample 4's temperature (row 7) is JSON null -> SQL NULL.
    assert!(matches!(field(&all[7], 4), Field::Null));
    assert_eq!(string(&all[7], 2), "PSU Temperature");
    // The last row is day 2's final temperature.
    assert_eq!(double(&all[15], 4), 34.0);

    let text = stderr(&output);
    assert!(
        text.contains("wrote 16 rows from 8 samples"),
        "summary missing: {text}"
    );
    assert!(
        text.contains("(2 files scanned, 0 skipped lines)"),
        "summary missing: {text}"
    );
}

#[test]
fn trailing_last_window_ending_at_until() {
    let (dir, config) = full_fixture();
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--until",
        "2026-02-19T08:00:20-05:00",
        "--last",
        "24h",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    // The trailing day covers s3..s8 (both window edges inclusive).
    assert_eq!(rows(&out).len(), 12);
    assert!(
        stderr(&output).contains("wrote 12 rows from 6 samples"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn hostile_sensor_strings_round_trip() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), "", 10, true);
    // JSON-escaped hostile strings: an embedded quote, a newline, non-ASCII.
    let line = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "evil \" quote", "reading": "two\nlines — 温度", "type": "Voltage", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": "µV"}]}
"#;
    write_log(dir.path(), "sensors_2026-02-18.jsonl", line.as_bytes());
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:59:59-05:00",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let all = rows(&out);
    assert_eq!(all.len(), 1);
    assert_eq!(string(&all[0], 1), "evil \" quote");
    assert_eq!(string(&all[0], 2), "two\nlines — 温度");
    assert_eq!(string(&all[0], 5), "µV");
}

#[test]
fn garbage_and_oversized_lines_are_counted_and_skipped() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), "", 10, true);
    let good1 = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": "V"}]}"#;
    let good2 = r#"{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "S", "reading": "R", "type": "Voltage", "value": 2.0, "min": 1.0, "max": 2.0, "avg": 1.5, "unit": "V"}]}"#;
    let mut content = Vec::new();
    content.extend_from_slice(good1.as_bytes());
    content.push(b'\n');
    content.extend_from_slice(b"this is not json\n");
    content.extend(vec![b'x'; common::limits::MAX_LINE_BYTES + 1]);
    content.push(b'\n');
    content.extend_from_slice(good2.as_bytes());
    content.push(b'\n');
    write_log(dir.path(), "sensors_2026-02-18.jsonl", &content);

    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:59:59-05:00",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(rows(&out).len(), 2, "both good lines flow through");
    assert!(
        stderr(&output).contains("2 skipped lines"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn zero_sample_window_writes_a_schema_only_file_and_exits_0() {
    let dir = TempDir::new();
    let config = write_config(dir.path(), "", 10, true);
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-19T00:00:00Z",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(out.is_file(), "a schema-only file must still be written");

    let file = std::fs::File::open(&out).unwrap();
    let reader = SerializedFileReader::new(file).expect("schema-only file is valid parquet");
    let schema = reader.metadata().file_metadata().schema_descr();
    assert_eq!(schema.num_columns(), 6);
    assert_eq!(rows(&out).len(), 0);
    assert!(
        stderr(&output).contains("wrote 0 rows from 0 samples"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn nonexistent_log_dir_override_yields_schema_only_export() {
    let dir = TempDir::new();
    let out = dir.path().join("out.parquet");
    let missing = dir.path().join("nope");
    let output = sensorwatch(&[
        "export",
        "--log-dir",
        arg(&missing),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-19T00:00:00Z",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(rows(&out).len(), 0);
}

#[test]
fn bad_window_args_are_usage_errors() {
    let dir = TempDir::new();
    let out = dir.path().join("out.parquet");
    let out = arg(&out);
    let cases: &[&[&str]] = &[
        &["export", "--since", "not-a-time", "--out", out],
        &["export", "--last", "0s", "--out", out],
        &["export", "--last", "999999999999999999d", "--out", out],
        &[
            "export",
            "--since",
            "2026-02-19T00:00:00Z",
            "--until",
            "2026-02-18T00:00:00Z",
            "--out",
            out,
        ],
        // clap-level: --since conflicts with --last; --out is required.
        &[
            "export",
            "--since",
            "2026-02-18T00:00:00Z",
            "--last",
            "24h",
            "--out",
            out,
        ],
        &["export", "--last", "24h"],
    ];
    for args in cases {
        let output = sensorwatch(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "expected usage error for {args:?}; stderr: {}",
            stderr(&output)
        );
    }
}

#[test]
fn malformed_config_toml_is_usage_error() {
    let dir = TempDir::new();
    let config = write_str(dir.path(), "config.toml", "this is not toml [");
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-19T00:00:00Z",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("could not parse config"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn unreadable_existing_config_is_fatal() {
    let dir = TempDir::new();
    let out = dir.path().join("out.parquet");
    // A directory exists but cannot be read as a file — cross-platform.
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(dir.path()),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-19T00:00:00Z",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("could not read config"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn out_path_in_missing_directory_is_fatal() {
    let (dir, config) = full_fixture();
    let out = dir.path().join("no-such-dir").join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        FULL_WINDOW[0],
        FULL_WINDOW[1],
        FULL_WINDOW[2],
        FULL_WINDOW[3],
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot create"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn existing_out_file_is_truncated() {
    let (dir, config) = full_fixture();
    let out = dir.path().join("out.parquet");
    let full = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        FULL_WINDOW[0],
        FULL_WINDOW[1],
        FULL_WINDOW[2],
        FULL_WINDOW[3],
        "--out",
        arg(&out),
    ]);
    assert_eq!(full.status.code(), Some(0));
    assert_eq!(rows(&out).len(), 16);

    // Re-export day 1 only to the same path: truncate, not append.
    let day1 = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:59:59-05:00",
        "--out",
        arg(&out),
    ]);
    assert_eq!(day1.status.code(), Some(0), "stderr: {}", stderr(&day1));
    assert_eq!(rows(&out).len(), 10);
}

#[test]
fn export_is_read_only() {
    let (dir, config) = full_fixture();
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(&config),
        FULL_WINDOW[0],
        FULL_WINDOW[1],
        FULL_WINDOW[2],
        FULL_WINDOW[3],
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0));

    let logs = dir.path().join("logs");
    assert!(
        !logs.join("watch.seq").exists(),
        "export must never touch watch state"
    );
    let mut names: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["sensors_2026-02-18.jsonl", "sensors_2026-02-19.jsonl"],
        "the log dir must be exactly the two fixtures"
    );
}

#[test]
fn golden_fixture_exports_seven_rows() {
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let dir = TempDir::new();
    let out = dir.path().join("out.parquet");
    let output = sensorwatch(&[
        "export",
        "--log-dir",
        arg(&golden),
        "--since",
        "2026-02-18T00:00:00Z",
        "--until",
        "2026-02-20T00:00:00Z",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let all = rows(&out);
    assert_eq!(all.len(), 7, "3 + 2 + 2 readings over 3 samples");
    // Row 0: the Python-fixture +12V reading, timestamp offset-preserved
    // on the wire but exported as the UTC instant.
    assert_eq!(
        timestamp_micros(&all[0], 0),
        micros_of("2026-02-18T08:17:48.123456-05:00")
    );
    assert_eq!(string(&all[0], 1), "MEG Ai1600T");
    assert_eq!(string(&all[0], 2), "+12V");
    assert_eq!(string(&all[0], 3), "Voltage");
    assert_eq!(double(&all[0], 4), 12.03);
    assert_eq!(string(&all[0], 5), "V");
    // °C survives, negatives survive, the empty unit survives.
    assert_eq!(string(&all[1], 5), "°C");
    assert_eq!(string(&all[4], 2), "Offset Rail");
    assert_eq!(string(&all[4], 3), "Other");
    assert_eq!(double(&all[4], 4), -0.125);
    assert_eq!(string(&all[4], 5), "");
    // The last row is day 2's `None`-typed reading at the day's last instant.
    assert_eq!(string(&all[6], 3), "None");
    assert_eq!(
        timestamp_micros(&all[6], 0),
        micros_of("2026-02-19T23:59:59.999999Z")
    );
    assert!(
        stderr(&output).contains("(2 files scanned, 0 skipped lines)"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn config_is_ignored_when_log_dir_given() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("logs")).unwrap();
    write_log(dir.path(), "sensors_2026-02-18.jsonl", DAY1.as_bytes());
    let out = dir.path().join("out.parquet");
    // --config points at a directory (unreadable as a file); --log-dir must
    // make that irrelevant.
    let output = sensorwatch(&[
        "export",
        "--config",
        arg(dir.path()),
        "--log-dir",
        arg(&dir.path().join("logs")),
        "--since",
        "2026-02-18T00:00:00-05:00",
        "--until",
        "2026-02-18T23:59:59-05:00",
        "--out",
        arg(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(rows(&out).len(), 10);
}

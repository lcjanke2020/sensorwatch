//! Cross-language end-to-end test (LEO-415): synthetic HWiNFO buffers through
//! the real C parser (via the safe binding's `Snapshot::from_buffer`), the
//! logger's JSONL serialization, the replay reader, and the watch engine, down
//! to the exact emitted event JSON. Before this test, the C-side fixtures
//! stopped at parsed readings and the engine suites started from hand-written
//! JSONL; this ties the two halves together, so it fails if the parser, the
//! JSONL contract, or the event schema drifts.
//!
//! In-crate (not `tests/`) because it threads `pub(crate)` seams — the same
//! placement as the engine's replay end-to-end test.

// Shared with rust/sensorwatch/tests; not every helper is used here.
#[allow(dead_code)]
#[path = "../../sensorwatch/tests/common/mod.rs"]
mod synth;

use jiff::civil::date;
use jiff::tz::{Offset, TimeZone};
use jiff::Zoned;
use sensorwatch::Snapshot;

use crate::engine::{Engine, TransitionState};
use crate::event::Event;
use crate::jsonl::{self, LogEntry};
use crate::replay::ReplaySource;
use crate::rules::RuleSet;
use crate::source::SampleSource as _;
use crate::testutil::TempDir;

use synth::{build_buffer, Entry, Sensor};

/// (value, min, max, avg) per 10 s sample — the +12V-sag series shared with
/// `tests/watch_cli.rs` FIXTURE: never violates, two violating samples (fires
/// the for_samples=2 rule on the second), then a recovery (clears it).
const SAG_SERIES: [(f64, f64, f64, f64); 4] = [
    (12.03, 11.9, 12.1, 12.0),
    (11.5, 11.4, 12.1, 11.95),
    (11.4, 11.4, 12.1, 11.9),
    (11.9, 11.4, 12.1, 11.92),
];

/// The canonical critical threshold rule (`tests/watch_cli.rs` PSU_RULE).
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

/// The bytes step 2 must serialize to — `tests/watch_cli.rs` FIXTURE. Matching
/// it byte-for-byte proves the buffer→snapshot→LogEntry path reproduces the
/// JSONL contract the black-box CLI tests (and the Python golden fixtures)
/// already pin from the other side.
const EXPECTED_JSONL: &str = r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 11.9, "max": 12.1, "avg": 12.0, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.5, "min": 11.4, "max": 12.1, "avg": 11.95, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:20.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.4, "min": 11.4, "max": 12.1, "avg": 11.9, "unit": "V"}]}
{"timestamp": "2026-02-18T08:00:30.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.9, "min": 11.4, "max": 12.1, "avg": 11.92, "unit": "V"}]}
"#;

/// The exact fired-event line (`tests/watch_cli.rs` FIRED_EVENT) — the frozen
/// schema_version-1 contract of docs/agent-monitoring.md.
const FIRED_EVENT: &str = r#"{"schema_version":1,"seq":1,"id":"psu-12v-sag-1","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.4,"unit":"V","threshold":11.6,"samples_in_violation":2}"#;

/// Fixed -05:00 zone timestamps 10 s apart (the zone the fixtures use).
fn sample_time(index: usize) -> Zoned {
    let tz = TimeZone::fixed(Offset::from_seconds(-5 * 3600).unwrap());
    date(2026, 2, 18)
        .at(8, 0, i8::try_from(index * 10).unwrap(), 0)
        .to_zoned(tz)
        .unwrap()
}

#[test]
fn synthetic_buffer_to_event_json_pipeline() {
    // 1. Synthetic HWiNFO buffers → the real C parser → safe-crate Readings.
    // 2. The logger's serialization: Reading → LogEntry → record lines.
    let mut lines = String::new();
    for (i, (value, min, max, avg)) in SAG_SERIES.iter().enumerate() {
        let buf = build_buffer(
            &[Sensor::named("MEG Ai1600T")],
            &[Entry {
                type_code: 2, // Voltage
                sensor_idx: 0,
                reading_user: Some("+12V"),
                reading_orig: None,
                unit: Some("V"),
                value: *value,
                minimum: *min,
                maximum: *max,
                average: *avg,
            }],
        );
        let snapshot = Snapshot::from_buffer(&buf).expect("parse synthetic buffer");
        let readings = snapshot.to_vec().expect("materialize readings");
        assert_eq!(readings.len(), 1);

        let entries: Vec<LogEntry<'_>> = readings.iter().map(LogEntry::from).collect();
        lines.push_str(&jsonl::format_record(&sample_time(i), &entries));
        lines.push('\n');
    }
    assert_eq!(lines, EXPECTED_JSONL, "JSONL contract drifted");

    // 3. Write as a daily log file and stream it through the real replay reader.
    let dir = TempDir::new();
    let path = dir.path().join("sensors_2026-02-18.jsonl");
    std::fs::write(&path, &lines).expect("write replay file");

    // 4. The watch engine over the replayed ticks.
    let mut engine = Engine::new(RuleSet::from_toml_str(PSU_RULE).expect("rules parse"));
    let mut source = ReplaySource::from_files(vec![path]);
    let mut transitions = Vec::new();
    while let Some(tick) = source.next_tick() {
        transitions.extend(engine.observe(&tick));
    }
    assert_eq!(source.skipped_lines(), 0, "no line may be dropped");

    // Fires on the second violating sample, clears on the recovery.
    assert_eq!(transitions.len(), 2);
    assert_eq!(transitions[0].state, TransitionState::Fired);
    assert_eq!(transitions[1].state, TransitionState::Cleared);

    // 5. The exact event JSON — identical to what `watch --replay` prints.
    let event = Event::from_transition(&transitions[0], 1);
    assert_eq!(event.to_json(), FIRED_EVENT, "event schema drifted");
}

#[test]
fn corpus_seed_parses_through_the_binding() {
    // Anchor the Rust side on the committed fuzz corpus seed the C and Python
    // suites consume (this crate is repo-only, so the repo-relative path is
    // fine here; the published crates must not reach outside their roots).
    let seed = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fuzz/corpus/parse/valid_multi.bin"
    );
    let data = std::fs::read(seed).expect("read committed corpus seed");
    let snapshot = Snapshot::from_buffer(&data).expect("seed parses");
    assert!(!snapshot.is_empty());
    assert_eq!(snapshot.source(), "HWiNFO");
    for reading in &snapshot {
        reading.expect("every seed reading is accessible");
    }
}

//! The agent event contract: the JSON record `watch` emits, its persisted
//! sequence counter, and the atomic spool files that carry events to agents.
//!
//! Unlike the `log`/`snapshot` output, events have no Python counterpart and
//! no byte-compat constraint, so they serialize with plain compact
//! `serde_json` (no [`crate::jsonl::PythonFormatter`]). The struct field order
//! is the JSON key order — the frozen schema in `docs/agent-monitoring.md`.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::{Transition, TransitionState};
use crate::rules::{RuleKind, Severity};

/// The event schema version. Additive changes keep `1`; a rename or removal
/// of an existing key bumps it (see `docs/agent-monitoring.md`).
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The file that persists the last-used sequence number, in `log_dir`.
const SEQ_FILE: &str = "watch.seq";

/// One agent wake-up event. Borrows its strings from the [`Transition`] it
/// describes; `id` is the one owned field (derived, not carried).
///
/// `value`/`threshold` are `Option<f64>`; a `Some(NaN)` value serializes to
/// `null` (serde_json renders non-finite floats as null, before any
/// formatter — the same divergence documented in `jsonl.rs`), so a stale rule
/// firing on repeated nulls still emits a valid `"value":null`.
#[derive(Serialize)]
pub(crate) struct Event<'a> {
    schema_version: u32,
    seq: u64,
    id: String,
    rule: &'a str,
    #[serde(rename = "type")]
    kind: RuleKind,
    severity: Severity,
    state: TransitionState,
    timestamp: &'a str,
    sensor: Option<&'a str>,
    reading: Option<&'a str>,
    value: Option<f64>,
    unit: Option<&'a str>,
    threshold: Option<f64>,
    samples_in_violation: u32,
}

impl<'a> Event<'a> {
    /// Build the event for a transition, stamped with its persisted sequence.
    pub(crate) fn from_transition(t: &'a Transition, seq: u64) -> Event<'a> {
        Event {
            schema_version: SCHEMA_VERSION,
            seq,
            id: format!("{}-{seq}", t.rule),
            rule: &t.rule,
            kind: t.kind,
            severity: t.severity,
            state: t.state,
            timestamp: &t.raw_timestamp,
            sensor: t.sensor.as_deref(),
            reading: t.reading.as_deref(),
            value: t.value,
            unit: t.unit.as_deref(),
            threshold: t.threshold,
            samples_in_violation: t.samples_in_violation,
        }
    }

    /// Serialize to one compact JSON line (no trailing newline — the caller
    /// owns the line ending, which differs across stdout, spool, and the
    /// daily event file).
    pub(crate) fn to_json(&self) -> String {
        // Serializing borrowed strings, small ints, and floats into a String
        // cannot fail — same justification as `jsonl::format_record`.
        serde_json::to_string(self).expect("serializing an Event to a String cannot fail")
    }
}

/// The persisted, monotonically increasing sequence counter.
///
/// `seq` is monotonic, not dense: a crash between persisting and emitting
/// skips a number, which is fine (ack cursors key off `seq`, never wall
/// clock). Persisting BEFORE emitting is the invariant — the reverse could
/// reuse a number and corrupt an agent's cursor. Single-watcher-per-state-dir
/// is assumed; concurrency is out of scope (see `docs/agent-monitoring.md`).
#[derive(Debug)]
pub(crate) struct SeqStore {
    path: PathBuf,
    last: u64,
}

impl SeqStore {
    /// Open (or initialize) the counter under `dir`. A missing file starts at
    /// 0 (so the first event is seq 1); unparseable content is fatal, naming
    /// the file — never a silent restart at 0.
    pub(crate) fn open(dir: impl AsRef<Path>) -> Result<SeqStore, String> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|err| {
            format!(
                "could not create the state directory {}: {err}",
                dir.display()
            )
        })?;
        let path = dir.join(SEQ_FILE);
        let last = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let trimmed = text.trim();
                trimmed.parse::<u64>().map_err(|err| {
                    format!(
                        "the sequence file {} is corrupt (expected a decimal integer, got {trimmed:?}): {err}",
                        path.display()
                    )
                })?
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => 0,
            Err(err) => {
                return Err(format!(
                    "could not read the sequence file {}: {err}",
                    path.display()
                ))
            }
        };
        Ok(SeqStore { path, last })
    }

    /// Reserve, persist, and return the next sequence number. The persist is
    /// crash-safe (write a sibling `.tmp`, then rename over the file) and
    /// happens before the caller emits anywhere.
    pub(crate) fn next(&mut self) -> io::Result<u64> {
        let next = self.last + 1;
        let tmp = self.path.with_file_name(format!("{SEQ_FILE}.tmp"));
        std::fs::write(&tmp, format!("{next}\n"))?;
        std::fs::rename(&tmp, &self.path)?;
        self.last = next;
        Ok(next)
    }
}

/// The spool file name for an event: `{seq:010}-{slug}.json`. The zero-padded
/// sequence makes lexicographic order match numeric order, and (being unique)
/// makes names collision-free.
pub(crate) fn spool_file_name(seq: u64, rule: &str) -> String {
    format!("{seq:010}-{}.json", slug(rule))
}

/// The rule name reduced to a filesystem-safe slug: lowercased, every
/// character outside `[a-z0-9._-]` replaced with `-`, truncated to 50 bytes,
/// and `"rule"` if nothing survives. The output is pure ASCII, so the
/// truncation never splits a character.
fn slug(rule: &str) -> String {
    let mut s: String = rule
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    s.truncate(50);
    if s.is_empty() {
        "rule".to_owned()
    } else {
        s
    }
}

/// Atomically write `json` (plus a trailing LF — machine artifact, always
/// LF) as a spool file: write `{name}.tmp`, then rename to `{name}`. Agents
/// glob `*.json`, so the `.tmp` staging file never matches mid-write.
pub(crate) fn write_spool(dir: &Path, name: &str, json: &str) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let final_path = dir.join(name);
    std::fs::write(&tmp, format!("{json}\n"))?;
    std::fs::rename(&tmp, &final_path)
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::testutil::TempDir;

    /// The `psu-12v-sag` fired-threshold transition from the engine tests —
    /// the schema example in `docs/agent-monitoring.md`.
    fn threshold_transition() -> Transition {
        Transition {
            rule: "psu-12v-sag".to_owned(),
            kind: RuleKind::Threshold,
            severity: Severity::Critical,
            state: TransitionState::Fired,
            raw_timestamp: "2026-02-18T08:00:20.000000-05:00".to_owned(),
            timestamp: "2026-02-18T08:00:20.000000-05:00"
                .parse::<Timestamp>()
                .unwrap(),
            sensor: Some("MEG Ai1600T".to_owned()),
            reading: Some("+12V".to_owned()),
            value: Some(11.4),
            unit: Some("V".to_owned()),
            threshold: Some(11.6),
            samples_in_violation: 2,
        }
    }

    fn source_unavailable_transition() -> Transition {
        Transition {
            rule: "sensors-offline".to_owned(),
            kind: RuleKind::SourceUnavailable,
            severity: Severity::Warning,
            state: TransitionState::Fired,
            raw_timestamp: "2026-02-18T08:00:20.000000-05:00".to_owned(),
            timestamp: "2026-02-18T08:00:20.000000-05:00"
                .parse::<Timestamp>()
                .unwrap(),
            sensor: None,
            reading: None,
            value: None,
            unit: None,
            threshold: None,
            samples_in_violation: 1,
        }
    }

    #[test]
    fn threshold_event_json_is_exact_and_under_1kb() {
        let t = threshold_transition();
        let json = Event::from_transition(&t, 42).to_json();
        assert_eq!(
            json,
            r#"{"schema_version":1,"seq":42,"id":"psu-12v-sag-42","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.4,"unit":"V","threshold":11.6,"samples_in_violation":2}"#
        );
        assert!(json.len() < 1024, "event JSON is {} bytes", json.len());
    }

    #[test]
    fn source_unavailable_event_nulls_every_series_field() {
        let t = source_unavailable_transition();
        let json = Event::from_transition(&t, 1).to_json();
        assert!(json.contains(r#""type":"source-unavailable""#), "{json}");
        assert!(json.contains(r#""sensor":null"#), "{json}");
        assert!(json.contains(r#""reading":null"#), "{json}");
        assert!(json.contains(r#""value":null"#), "{json}");
        assert!(json.contains(r#""unit":null"#), "{json}");
        assert!(json.contains(r#""threshold":null"#), "{json}");
    }

    #[test]
    fn nan_value_serializes_as_null() {
        let mut t = threshold_transition();
        t.value = Some(f64::NAN);
        let json = Event::from_transition(&t, 1).to_json();
        assert!(json.contains(r#""value":null"#), "{json}");
    }

    #[test]
    fn seq_store_starts_at_one_and_persists() {
        let dir = TempDir::new();
        let mut store = SeqStore::open(dir.path()).unwrap();
        assert_eq!(store.next().unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("watch.seq")).unwrap(),
            "1\n"
        );
        assert_eq!(store.next().unwrap(), 2);
        // No staging file lingers after a successful rename.
        assert!(!dir.path().join("watch.seq.tmp").exists());
    }

    #[test]
    fn seq_store_reopen_resumes_from_the_file() {
        let dir = TempDir::new();
        SeqStore::open(dir.path()).unwrap().next().unwrap(); // persists "1\n"
        let mut reopened = SeqStore::open(dir.path()).unwrap();
        assert_eq!(reopened.next().unwrap(), 2);
    }

    #[test]
    fn seq_store_rejects_corrupt_file() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("watch.seq"), "not-a-number\n").unwrap();
        let err = SeqStore::open(dir.path()).unwrap_err();
        assert!(err.contains("watch.seq"), "{err}");
        assert!(err.contains("corrupt"), "{err}");
    }

    #[test]
    fn spool_file_name_pads_and_slugs() {
        assert_eq!(
            spool_file_name(42, "psu-12v-sag"),
            "0000000042-psu-12v-sag.json"
        );
        // Uppercase folds; spaces and punctuation become '-'.
        assert_eq!(
            spool_file_name(7, "CPU Temp/Hot!"),
            "0000000007-cpu-temp-hot-.json"
        );
        // Every character maps to exactly one, so a non-empty name yields a
        // non-empty slug (here, all dashes).
        assert_eq!(spool_file_name(5, "///"), "0000000005----.json");
        // Only a truly empty name degrades to "rule".
        assert_eq!(spool_file_name(5, ""), "0000000005-rule.json");
    }

    #[test]
    fn spool_slug_truncates_to_fifty_chars() {
        let long = "a".repeat(100);
        let name = spool_file_name(1, &long);
        // "0000000001-" (11) + 50 slug chars + ".json" (5) = 66.
        assert_eq!(name, format!("0000000001-{}.json", "a".repeat(50)));
    }

    #[test]
    fn write_spool_is_atomic_and_leaves_no_tmp() {
        let dir = TempDir::new();
        let name = spool_file_name(1, "psu-12v-sag");
        write_spool(dir.path(), &name, r#"{"seq":1}"#).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&name)).unwrap(),
            "{\"seq\":1}\n"
        );
        assert!(!dir.path().join(format!("{name}.tmp")).exists());
    }
}

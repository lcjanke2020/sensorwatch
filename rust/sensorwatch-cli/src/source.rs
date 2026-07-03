//! The sample-source abstraction: one poll = one [`Tick`], from live
//! hardware or from replayed log files.
//!
//! Sources own where timestamps come from — [`LiveSource`] is the ONLY place
//! the wall clock enters the rule-evaluation pipeline; the replay source
//! carries timestamps from the data stream. Downstream (the engine, and the
//! `watch`/`report` commands built on it) never reads a clock, which is what
//! makes evaluation fully deterministic under replay.
//!
//! Sources never sleep and own no shutdown logic: pacing, the shutdown
//! condvar, and the platform gate stay in the command loop (see the `log`
//! loop in `logger.rs`, the template the `watch` command follows).

use jiff::{Timestamp, Zoned};
use sensorwatch::{Reading, Session};

use crate::jsonl;
use crate::labels::type_label;

/// One reading, source-neutral. Deliberately NOT [`sensorwatch::Reading`]:
/// the logged JSONL stream has no `source` field (a replayed reading would
/// have to fabricate one), and its `type` is a string label, not the
/// `#[non_exhaustive]` `ReadingType`. `kind` holds the canonical Title-case
/// label from `labels.rs`, giving rules exact-match `type` semantics that
/// are identical across live and replay.
#[derive(Debug, Clone)]
pub(crate) struct SampleReading {
    pub sensor: String,
    pub reading: String,
    /// Canonical type label (`labels::normalize_type_label` vocabulary).
    pub kind: &'static str,
    /// Replayed JSON `null` (the logger's non-finite encoding) maps to NaN.
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub unit: String,
}

impl From<&Reading> for SampleReading {
    fn from(r: &Reading) -> SampleReading {
        SampleReading {
            sensor: r.sensor.clone(),
            reading: r.reading.clone(),
            kind: type_label(r.kind),
            value: r.value,
            min: r.minimum,
            max: r.maximum,
            avg: r.average,
            unit: r.unit.clone(),
        }
    }
}

/// All readings observed at one stream timestamp.
#[derive(Debug, Clone)]
pub(crate) struct Sample {
    /// Parsed instant, for windowing and gap detection (`report`).
    pub timestamp: Timestamp,
    /// The timestamp exactly as the stream rendered it, passed through
    /// verbatim into transition/event payloads so replayed events carry the
    /// bytes of the log they came from.
    pub raw_timestamp: String,
    pub readings: Vec<SampleReading>,
}

/// One observation from a source: either a sample, or an affirmative
/// "polled and had nothing" (HWiNFO gone). Only the live source emits
/// [`Tick::Unavailable`] — in a replayed log an outage is simply the absence
/// of records, which is why the engine freezes non-source rules on
/// unavailability: live and replay then agree.
#[derive(Debug, Clone)]
pub(crate) enum Tick {
    Sample(Sample),
    Unavailable {
        timestamp: Timestamp,
        raw_timestamp: String,
    },
}

/// A stream of ticks. `None` means the stream is exhausted — only replay
/// ends; the live source always yields. Implementations never block beyond
/// the read/poll itself: no sleeping, no signal handling.
pub(crate) trait SampleSource {
    fn next_tick(&mut self) -> Option<Tick>;
}

/// The live source: a fresh HWiNFO session per poll, exactly like the `log`
/// loop's `collect_live` (the per-tick reopen is what makes recovery from
/// HWiNFO restarts automatic — "reopens the session as HWiNFO comes and
/// goes" is inherent, not managed). Every error, including
/// `UnsupportedPlatform`, folds to [`Tick::Unavailable`]: whether an
/// unavailable source is fatal or retryable is the command's call (the
/// `cfg!(windows)` gate pattern in `logger.rs`), and folding keeps the trait
/// total — which also gives Linux CI a real `LiveSource` test.
pub(crate) struct LiveSource;

impl SampleSource for LiveSource {
    fn next_tick(&mut self) -> Option<Tick> {
        // One clock read per poll; rendered with the log-file formatter so
        // live event timestamps stay byte-shaped like logged ones.
        let now = Zoned::now();
        let raw_timestamp = jsonl::format_timestamp(&now);
        let timestamp = now.timestamp();
        match collect_live() {
            Ok(readings) => Some(Tick::Sample(Sample {
                timestamp,
                raw_timestamp,
                readings: readings.iter().map(SampleReading::from).collect(),
            })),
            Err(err) => {
                log::debug!("Sensor source unavailable this poll ({err})");
                Some(Tick::Unavailable {
                    timestamp,
                    raw_timestamp,
                })
            }
        }
    }
}

/// One poll: open a session, copy a snapshot out, drop the session. The
/// deliberate third copy of this helper (`snapshot.rs`, `logger.rs`) — the
/// duplication keeps the golden-tested `log` path frozen.
fn collect_live() -> sensorwatch::Result<Vec<Reading>> {
    let mut session = Session::new()?;
    let snapshot = session.snapshot()?;
    snapshot.to_vec()
}

#[cfg(test)]
mod tests {
    use sensorwatch::ReadingType;

    use super::*;

    #[test]
    fn sample_reading_from_reading_drops_source_and_folds_kind() {
        let reading = Reading {
            source: "HWiNFO".to_owned(),
            sensor: "MEG Ai1600T".to_owned(),
            reading: "+12V".to_owned(),
            unit: "V".to_owned(),
            kind: ReadingType::Voltage,
            value: 12.03,
            minimum: 12.01,
            maximum: 12.17,
            average: 12.06,
        };
        let sample = SampleReading::from(&reading);
        assert_eq!(sample.sensor, "MEG Ai1600T");
        assert_eq!(sample.reading, "+12V");
        assert_eq!(sample.kind, "Voltage");
        assert_eq!(sample.value, 12.03);
        assert_eq!(sample.min, 12.01);
        assert_eq!(sample.max, 12.17);
        assert_eq!(sample.avg, 12.06);
        assert_eq!(sample.unit, "V");
    }

    #[test]
    fn unknown_kind_folds_to_bare_unknown_label() {
        let reading = Reading {
            source: "HWiNFO".to_owned(),
            sensor: "S".to_owned(),
            reading: "R".to_owned(),
            unit: String::new(),
            kind: ReadingType::Unknown,
            value: 1.0,
            minimum: 1.0,
            maximum: 1.0,
            average: 1.0,
        };
        assert_eq!(SampleReading::from(&reading).kind, "unknown");
    }

    // The checklist's degrade-correctly assertion: off Windows the live
    // source deterministically reports Unavailable (UnsupportedPlatform is
    // folded — fatal-vs-retry is the command loop's decision, not ours).
    #[cfg(not(windows))]
    #[test]
    fn live_source_off_windows_yields_unavailable_with_parseable_timestamp() {
        let mut source = LiveSource;
        let tick = source.next_tick().expect("live source always yields");
        match tick {
            Tick::Unavailable { raw_timestamp, .. } => {
                assert!(
                    raw_timestamp.parse::<Timestamp>().is_ok(),
                    "raw timestamp {raw_timestamp:?} must parse back"
                );
            }
            Tick::Sample(_) => panic!("no live sensor source exists off Windows"),
        }
    }
}

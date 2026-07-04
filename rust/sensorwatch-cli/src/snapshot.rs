//! The `snapshot` subcommand: one-shot live readings as a JSON array.
//!
//! The output contract matches the agent skill's Python helper
//! (`skills/sensorwatch/scripts/snapshot.py`): a JSON array of objects with
//! keys `source, sensor, reading, type, value, min, max, avg, unit` in that
//! order, Title-case `type` labels with `"unknown"` for unrecognized
//! categories, and exit codes 0/1/2. One deliberate divergence: non-finite
//! values serialize as `null` (valid JSON), where Python emits a bare `NaN`.
//!
//! Filtering and rendering are pure functions over [`Reading`] values so they
//! stay unit-testable off Windows; only [`collect_live`] touches a `Session`.

use std::process::ExitCode;

use sensorwatch::{Reading, Session};
use serde::Serialize;

use crate::cli::{SnapshotArgs, TypeFilter};
use crate::labels::{filter_label, type_label};

/// Apply the `--type` and `--match` filters.
fn filter<'a>(
    readings: &'a [Reading],
    type_filter: Option<TypeFilter>,
    needle: Option<&str>,
) -> Vec<&'a Reading> {
    let needle = needle.map(str::to_lowercase);
    readings
        .iter()
        .filter(|r| type_filter.is_none_or(|f| filter_label(f) == type_label(r.kind)))
        .filter(|r| {
            needle.as_deref().is_none_or(|n| {
                r.sensor.to_lowercase().contains(n) || r.reading.to_lowercase().contains(n)
            })
        })
        .collect()
}

/// One JSON object in the output array. Field declaration order is the
/// serialized key order — the contract shared with the Python helper.
#[derive(Serialize)]
struct Entry<'a> {
    source: &'a str,
    sensor: &'a str,
    reading: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    value: f64,
    min: f64,
    max: f64,
    avg: f64,
    unit: &'a str,
}

impl<'a> From<&'a Reading> for Entry<'a> {
    fn from(r: &'a Reading) -> Self {
        Entry {
            source: &r.source,
            sensor: &r.sensor,
            reading: &r.reading,
            kind: type_label(r.kind),
            value: r.value,
            min: r.minimum,
            max: r.maximum,
            avg: r.average,
            unit: &r.unit,
        }
    }
}

/// Serialize the readings as a JSON array: compact for `indent == 0`, pretty
/// with an `indent`-space unit otherwise.
fn render(readings: &[&Reading], indent: u32) -> serde_json::Result<String> {
    let entries: Vec<Entry<'_>> = readings.iter().copied().map(Entry::from).collect();
    crate::render::to_json_string(&entries, indent)
}

/// The only impure step: open a session and copy out one snapshot.
fn collect_live() -> sensorwatch::Result<Vec<Reading>> {
    let mut session = Session::new()?;
    let snapshot = session.snapshot()?;
    snapshot.to_vec()
}

pub fn run(args: &SnapshotArgs) -> ExitCode {
    let readings = match collect_live() {
        Ok(readings) => readings,
        Err(err) => {
            eprintln!("Could not read sensors: {err}");
            eprintln!(
                "Ensure you are on Windows and HWiNFO64 is running with \
                 \"Shared Memory Support\" enabled and the sensors window open."
            );
            return ExitCode::from(1);
        }
    };
    let selected = filter(&readings, args.type_filter, args.r#match.as_deref());
    log::debug!(
        "{} readings from the source, {} after filters",
        readings.len(),
        selected.len()
    );
    match render(&selected, args.indent) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Could not serialize the snapshot: {err}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use sensorwatch::ReadingType;

    use super::*;

    fn reading(sensor: &str, name: &str, kind: ReadingType, value: f64) -> Reading {
        Reading {
            source: "HWiNFO".to_owned(),
            sensor: sensor.to_owned(),
            reading: name.to_owned(),
            unit: "V".to_owned(),
            kind,
            value,
            minimum: value - 0.5,
            maximum: value + 0.5,
            average: value,
        }
    }

    #[test]
    fn type_filter_selects_matching_kinds() {
        let readings = [
            reading("CPU", "Core", ReadingType::Temperature, 55.0),
            reading("PSU", "+12V", ReadingType::Voltage, 12.1),
            reading("PSU", "Weird", ReadingType::Unknown, 1.0),
        ];
        let temps = filter(&readings, Some(TypeFilter::Temperature), None);
        assert_eq!(temps.len(), 1);
        assert_eq!(temps[0].sensor, "CPU");
        let unknown = filter(&readings, Some(TypeFilter::Unknown), None);
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].reading, "Weird");
    }

    #[test]
    fn match_filter_is_case_insensitive_over_both_names() {
        let readings = [
            reading("PSU", "+12V", ReadingType::Voltage, 12.1),
            reading("CPU Package", "Core Voltage", ReadingType::Voltage, 1.2),
            reading("GPU", "Hot Spot", ReadingType::Temperature, 70.0),
        ];
        // Matches the reading name of the first and nothing else.
        let by_reading = filter(&readings, None, Some("12v"));
        assert_eq!(by_reading.len(), 1);
        assert_eq!(by_reading[0].sensor, "PSU");
        // Matches the sensor name, case-insensitively.
        let by_sensor = filter(&readings, None, Some("cpu pack"));
        assert_eq!(by_sensor.len(), 1);
        assert_eq!(by_sensor[0].reading, "Core Voltage");
        // Matches neither name.
        assert!(filter(&readings, None, Some("nvme")).is_empty());
    }

    #[test]
    fn filters_combine_and_empty_input_stays_empty() {
        let readings = [
            reading("PSU", "+12V", ReadingType::Voltage, 12.1),
            reading("PSU", "Temp", ReadingType::Temperature, 40.0),
        ];
        let both = filter(&readings, Some(TypeFilter::Voltage), Some("psu"));
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].reading, "+12V");
        assert!(filter(&[], Some(TypeFilter::Voltage), Some("psu")).is_empty());
    }

    #[test]
    fn render_locks_key_order_and_layout_at_indent_2() {
        let r = reading("PSU", "+12V", ReadingType::Voltage, 12.5);
        let json = render(&[&r], 2).unwrap();
        assert_eq!(
            json,
            r#"[
  {
    "source": "HWiNFO",
    "sensor": "PSU",
    "reading": "+12V",
    "type": "Voltage",
    "value": 12.5,
    "min": 12.0,
    "max": 13.0,
    "avg": 12.5,
    "unit": "V"
  }
]"#
        );
    }

    #[test]
    fn render_honors_indent_widths_and_compact_mode() {
        let r = reading("PSU", "+12V", ReadingType::Voltage, 12.5);
        let compact = render(&[&r], 0).unwrap();
        assert_eq!(compact.lines().count(), 1);
        assert!(compact.starts_with(r#"[{"source":"HWiNFO","sensor":"PSU""#));
        let wide = render(&[&r], 4).unwrap();
        assert!(wide.contains("\n    {\n        \"source\": \"HWiNFO\","));
    }

    #[test]
    fn render_of_no_readings_is_an_empty_array() {
        assert_eq!(render(&[], 2).unwrap(), "[]");
        assert_eq!(render(&[], 0).unwrap(), "[]");
    }

    #[test]
    fn non_finite_values_serialize_as_null() {
        let mut r = reading("PSU", "+12V", ReadingType::Voltage, 12.5);
        r.value = f64::NAN;
        r.minimum = f64::NEG_INFINITY;
        r.maximum = f64::INFINITY;
        let json = render(&[&r], 0).unwrap();
        assert!(json.contains(r#""value":null"#));
        assert!(json.contains(r#""min":null"#));
        assert!(json.contains(r#""max":null"#));
        assert!(json.contains(r#""avg":12.5"#));
    }
}

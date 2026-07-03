//! Byte-compatible JSONL record formatting for the `log` subcommand.
//!
//! The contract is the frozen Python logger's output (`sensorwatch/logger.py`
//! writing `json.dumps(record, ensure_ascii=False)` with a
//! `pendulum.now("local").to_iso8601_string()` timestamp), so analyses over
//! directories mixing old and new files keep working. That means Python's
//! default separators (`", "` between items, `": "` after keys), raw UTF-8
//! for non-ASCII, key order `timestamp, sensors` with per-reading keys
//! `sensor, reading, type, value, min, max, avg, unit` (no `source` key —
//! unlike the `snapshot` output), and pendulum's ISO-8601 rendering.
//!
//! Three deliberate, documented divergences:
//! 1. Unknown reading-type codes emit a bare `"unknown"` (the Python logger
//!    writes `unknown(<N>)`; the safe wrapper folds the raw code away, and
//!    the skill's snapshot helper set this vocabulary).
//! 2. Timestamps always carry six fractional digits (pendulum omits them at
//!    exactly zero microseconds; the strings parse identically).
//! 3. Non-finite values serialize as `null` — valid JSON — where Python
//!    emits bare `NaN`/`Infinity` tokens.
//!
//! Float rendering tail risk, documented rather than engineered around:
//! Python's `repr` and serde_json both emit the shortest correctly-rounded
//! decimal, so digits always agree, but the scientific-notation rendering
//! differs (`1e+16`/`1e-05` vs `1e16`/`1e-5`) and the switch thresholds may
//! too. Real sensor magnitudes (V, °C, RPM, W, MHz, %) never reach either
//! regime.

use std::io;

use jiff::Zoned;
use sensorwatch::Reading;
use serde::Serialize;

use crate::labels::type_label;
use crate::source::SampleReading;

/// Reproduces Python's default `json.dumps` separators: `", "` and `": "`.
/// Everything else (string escaping, number rendering) inherits serde_json's
/// compact defaults, which already match `ensure_ascii=False` output.
struct PythonFormatter;

impl serde_json::ser::Formatter for PythonFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(b": ")
    }
}

/// One reading inside a record's `sensors` array. Field declaration order is
/// the serialized key order — the contract shared with the Python logger's
/// `SensorReading.to_dict()`.
#[derive(Serialize)]
pub(crate) struct LogEntry<'a> {
    pub sensor: &'a str,
    pub reading: &'a str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub unit: &'a str,
}

impl<'a> From<&'a Reading> for LogEntry<'a> {
    fn from(r: &'a Reading) -> Self {
        LogEntry {
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

/// `watch --follow` logs the same sample the engine evaluated, whose readings
/// are already [`SampleReading`]s (source-neutral, `kind` a canonical label).
/// Building the record from those keeps live-follow sensor logs byte-shaped
/// like the `log` command's.
impl<'a> From<&'a SampleReading> for LogEntry<'a> {
    fn from(r: &'a SampleReading) -> Self {
        LogEntry {
            sensor: &r.sensor,
            reading: &r.reading,
            kind: r.kind,
            value: r.value,
            min: r.min,
            max: r.max,
            avg: r.avg,
            unit: &r.unit,
        }
    }
}

/// One JSONL record: all readings sampled at one timestamp.
#[derive(Serialize)]
struct Record<'a> {
    timestamp: &'a str,
    sensors: &'a [LogEntry<'a>],
}

/// Pendulum's `to_iso8601_string()` rendering of a local-zone timestamp,
/// except that the six fractional digits are always present (divergence 2).
///
/// The `Z` suffix appears iff the zone's IANA name is exactly `"UTC"` —
/// pendulum keys this off the timezone *name*, not a zero offset, so e.g.
/// Europe/London in winter renders `+00:00`. (jiff normalizes a fixed zero
/// offset to the UTC zone, so that constructed edge renders `Z`; only named
/// zones can hit the zero-offset `+00:00` path, same as pendulum.)
pub(crate) fn format_timestamp(now: &Zoned) -> String {
    let micros = now.subsec_nanosecond() / 1_000;
    let suffix = if now.time_zone().iana_name() == Some("UTC") {
        "Z".to_owned()
    } else {
        let secs = now.offset().seconds();
        let (sign, secs) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
        // Pre-1972 zones can carry a sub-minute offset component (pendulum
        // would render ±HH:MM:SS); unreachable for a live clock — truncated.
        format!("{sign}{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}{suffix}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        micros,
    )
}

/// Serialize one record from an already-rendered timestamp string, without the
/// trailing line ending (the writer owns that, since it is platform-dependent).
/// Taking the timestamp as a string keeps record serialization independent of
/// how the timestamp was produced: [`format_record`] renders `now` and
/// delegates here.
pub(crate) fn format_record_raw(timestamp: &str, entries: &[LogEntry<'_>]) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonFormatter);
    Record {
        timestamp,
        sensors: entries,
    }
    .serialize(&mut serializer)
    .expect("serializing strings and floats to a Vec cannot fail");
    String::from_utf8(out).expect("serde_json output is UTF-8")
}

/// Serialize one record, rendering `now` with the pendulum-compatible
/// timestamp formatter.
pub(crate) fn format_record(now: &Zoned, entries: &[LogEntry<'_>]) -> String {
    format_record_raw(&format_timestamp(now), entries)
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::{Offset, TimeZone};

    use super::*;

    fn zoned(nanos: i32, offset_seconds: i32) -> Zoned {
        let tz = TimeZone::fixed(Offset::from_seconds(offset_seconds).unwrap());
        date(2026, 2, 18).at(8, 17, 48, nanos).to_zoned(tz).unwrap()
    }

    fn entry(value: f64) -> LogEntry<'static> {
        LogEntry {
            sensor: "MEG Ai1600T",
            reading: "+12V",
            kind: "Voltage",
            value,
            min: 12.01,
            max: 12.17,
            avg: 12.06,
            unit: "V",
        }
    }

    #[test]
    fn timestamp_always_carries_six_fraction_digits() {
        // Divergence 2, pinned: pendulum omits the fraction at zero micros.
        assert_eq!(
            format_timestamp(&zoned(0, -5 * 3600)),
            "2026-02-18T08:17:48.000000-05:00"
        );
        assert_eq!(
            format_timestamp(&zoned(123_456_000, -5 * 3600)),
            "2026-02-18T08:17:48.123456-05:00"
        );
        // Sub-microsecond precision truncates, like CPython's clock.
        assert_eq!(
            format_timestamp(&zoned(42_999, -5 * 3600)),
            "2026-02-18T08:17:48.000042-05:00"
        );
    }

    #[test]
    fn timestamp_offsets_render_signed_with_half_hours() {
        assert_eq!(
            format_timestamp(&zoned(999_999_000, 19_800)),
            "2026-02-18T08:17:48.999999+05:30"
        );
        assert_eq!(
            format_timestamp(&zoned(500_000_000, 10 * 3600)),
            "2026-02-18T08:17:48.500000+10:00"
        );
    }

    #[test]
    fn timestamp_utc_zone_renders_z() {
        let now = date(2026, 2, 18)
            .at(8, 17, 48, 123_456_000)
            .to_zoned(TimeZone::UTC)
            .unwrap();
        assert_eq!(format_timestamp(&now), "2026-02-18T08:17:48.123456Z");
    }

    #[test]
    fn timestamp_zero_offset_named_zone_renders_plus_zero_not_z() {
        // Pendulum keys `Z` off the zone name "UTC", not a zero offset:
        // London in winter is +00:00. (Windows CI resolves the zone from
        // jiff's bundled tzdb; Unix uses the system zoneinfo.)
        let tz = TimeZone::get("Europe/London").unwrap();
        let now = date(2026, 2, 18)
            .at(8, 17, 48, 123_456_000)
            .to_zoned(tz)
            .unwrap();
        assert_eq!(format_timestamp(&now), "2026-02-18T08:17:48.123456+00:00");
    }

    #[test]
    fn record_matches_python_separators_and_key_order() {
        let entries = [entry(12.03), entry(12.05)];
        assert_eq!(
            format_record(&zoned(123_456_000, -5 * 3600), &entries),
            r#"{"timestamp": "2026-02-18T08:17:48.123456-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}, {"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.05, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}]}"#
        );
    }

    #[test]
    fn non_finite_values_render_null_with_python_formatter() {
        // Divergence 3, pinned: serde_json's serializer emits null for
        // non-finite floats before the formatter is consulted.
        let mut e = entry(f64::NAN);
        e.min = f64::NEG_INFINITY;
        e.max = f64::INFINITY;
        let record = format_record(&zoned(1_000, 0), &[e]);
        assert!(record.contains(r#""value": null"#));
        assert!(record.contains(r#""min": null"#));
        assert!(record.contains(r#""max": null"#));
        assert!(record.contains(r#""avg": 12.06"#));
    }

    #[test]
    fn unicode_passes_through_unescaped() {
        // `ensure_ascii=False` parity: the degree sign stays raw UTF-8.
        let mut e = entry(45.5);
        e.unit = "°C";
        e.kind = "Temperature";
        let record = format_record(&zoned(1_000, 0), &[e]);
        assert!(record.contains(r#""unit": "°C""#));
    }

    // ---- LEO-336: format_record_raw + LogEntry::from(&SampleReading) ----

    #[test]
    fn format_record_raw_matches_format_record_for_the_same_timestamp() {
        let now = zoned(123_456_000, -5 * 3600);
        let entries = [entry(12.03), entry(12.05)];
        assert_eq!(
            format_record_raw(&format_timestamp(&now), &entries),
            format_record(&now, &entries)
        );
    }

    #[test]
    fn log_entry_from_sample_reading_maps_every_field() {
        let sample = SampleReading {
            sensor: "MEG Ai1600T".to_owned(),
            reading: "+12V".to_owned(),
            kind: "Voltage",
            value: 11.4,
            min: 11.3,
            max: 12.1,
            avg: 11.9,
            unit: "V".to_owned(),
        };
        let entry = LogEntry::from(&sample);
        assert_eq!(entry.sensor, "MEG Ai1600T");
        assert_eq!(entry.reading, "+12V");
        assert_eq!(entry.kind, "Voltage");
        assert_eq!(entry.value, 11.4);
        assert_eq!(entry.min, 11.3);
        assert_eq!(entry.max, 12.1);
        assert_eq!(entry.avg, 11.9);
        assert_eq!(entry.unit, "V");
    }

    #[test]
    fn control_characters_escape_like_python() {
        let mut e = entry(1.0);
        e.sensor = "line\nbreak\u{1f}";
        let record = format_record(&zoned(1_000, 0), &[e]);
        assert!(record.contains(r#""sensor": "line\nbreak\u001f""#));
    }
}

//! The pure logic behind `report`: window parsing, per-series windowed
//! aggregation, gap detection, ranking, and the hard byte-cap digest emitter.
//!
//! Everything here is clock-free — the "now", the time zone, and the window
//! instants are injected by [`crate::report`], so the digest a window produces
//! is a deterministic function of the logs it reads (the same property the
//! engine relies on). That is what lets the integration tests pin exact bytes.
//!
//! **Why the aggregates are recomputed, not read.** Each logged reading carries
//! HWiNFO's own `min`/`max`/`avg`, but those are *source-lifetime* numbers —
//! the extremes since HWiNFO started, not over the report window — so reading
//! them would silently answer a different question than the one asked. This
//! module folds fresh `min`/`max`/`avg`/`first`/`last` over the window's own
//! samples and never touches [`crate::source::SampleReading`]'s `min`/`max`/
//! `avg` fields.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::engine::Transition;
use crate::event::Event;
use crate::exit;
use crate::logger::LOG_PREFIX;
use crate::source::Sample;

/// The digest's own schema version, independent of the event schema's
/// [`crate::event::SCHEMA_VERSION`]: the two evolve separately (a change to a
/// reading-row field must not force the frozen event contract to bump, and
/// vice versa).
const DIGEST_SCHEMA_VERSION: u32 = 1;

/// At most this many gaps are retained for display (the largest by duration);
/// the exact total is still counted. A safety valve for pathological logs —
/// real windows produce a handful.
const MAX_GAPS: usize = 1024;

/// A sampling gap is any pause longer than this many sampling intervals. Three
/// intervals tolerates one dropped sample plus jitter without crying wolf.
const GAP_INTERVAL_MULTIPLIER: i128 = 3;

// ===========================================================================
// Window parsing (decisions 3 and 5)
// ===========================================================================

/// Parse a `--last` duration into whole seconds. Accepts a bare unsigned
/// integer of seconds, or one or more `<int><unit>` segments with
/// case-insensitive units `d`/`h`/`m`/`s` (`d` = 86400 s), e.g. `24h`, `90m`,
/// `1d12h`. Rejects the empty string, a zero total, signs, unknown units,
/// trailing garbage, and any overflow — all as usage-error strings.
///
/// Hand-rolled rather than `jiff::Span` arithmetic: jiff refuses calendar
/// units against a bare `Timestamp` (a day is not a fixed span in civil time),
/// but a trailing window wants a uniform `d = 86400 s`.
pub(crate) fn parse_duration_secs(s: &str) -> Result<i64, String> {
    if s.is_empty() {
        return Err("duration is empty".to_owned());
    }
    // A bare integer is a count of seconds.
    if s.bytes().all(|b| b.is_ascii_digit()) {
        let secs: i64 = s
            .parse()
            .map_err(|_| format!("duration {s:?} is too large to represent"))?;
        return positive(secs);
    }
    // Otherwise a sequence of `<int><unit>` segments.
    let mut total: i64 = 0;
    let mut digits = String::new();
    let mut saw_segment = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let unit_secs: i64 = match ch.to_ascii_lowercase() {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            other => {
                return Err(format!(
                    "duration {s:?} has an unknown unit {other:?}; use d, h, m, or s"
                ))
            }
        };
        if digits.is_empty() {
            return Err(format!(
                "duration {s:?} has a unit with no number before it"
            ));
        }
        let count: i64 = digits
            .parse()
            .map_err(|_| format!("duration {s:?} has a number too large to represent"))?;
        digits.clear();
        let segment = count
            .checked_mul(unit_secs)
            .ok_or_else(|| format!("duration {s:?} overflows"))?;
        total = total
            .checked_add(segment)
            .ok_or_else(|| format!("duration {s:?} overflows"))?;
        saw_segment = true;
    }
    if !digits.is_empty() {
        return Err(format!("duration {s:?} has trailing digits with no unit"));
    }
    if !saw_segment {
        return Err(format!("duration {s:?} is not a valid duration"));
    }
    positive(total)
}

fn positive(secs: i64) -> Result<i64, String> {
    if secs <= 0 {
        Err("duration must be greater than zero".to_owned())
    } else {
        Ok(secs)
    }
}

/// Which edge of a bare `YYYY-MM-DD` a [`parse_when`] value snaps to.
pub(crate) enum DayEdge {
    /// Local midnight at the start of the day.
    Start,
    /// The last instant of the day (next local midnight minus one nanosecond).
    End,
}

/// Parse a `--since`/`--until` value into an absolute instant.
///
/// Three accepted forms, tried in order:
/// 1. A full RFC 3339 instant (with an offset or `Z`): time-zone independent.
/// 2. A local civil `YYYY-MM-DDTHH:MM:SS`, interpreted in `tz`.
/// 3. A bare local `YYYY-MM-DD`: `Start` = local midnight, `End` = next local
///    midnight − 1 ns.
///
/// The discriminator between 2 and 3 is structural (does the string carry a
/// time-of-day separator?), because jiff's civil parsers are each lenient in
/// one direction — `Date` drops a trailing time, `DateTime` invents midnight
/// for a missing one — so a plain trial-parse chain cannot tell a bare date
/// from a midnight date-time, and the `End` edge would silently collapse to
/// the start of the day.
pub(crate) fn parse_when(s: &str, tz: &TimeZone, edge: DayEdge) -> Result<Timestamp, String> {
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(ts);
    }
    let has_time = s.bytes().any(|b| matches!(b, b'T' | b't' | b' '));
    if has_time {
        let dt = s
            .parse::<jiff::civil::DateTime>()
            .map_err(|err| format!("{s:?} is not a valid local date-time: {err}"))?;
        return dt
            .to_zoned(tz.clone())
            .map(|z| z.timestamp())
            .map_err(|err| format!("could not interpret {s:?} in the local time zone: {err}"));
    }
    let date = s.parse::<jiff::civil::Date>().map_err(|err| {
        format!(
            "{s:?} is not a valid time (expected RFC 3339, YYYY-MM-DDTHH:MM:SS, or \
             YYYY-MM-DD): {err}"
        )
    })?;
    let zoned = match edge {
        DayEdge::Start => date.to_zoned(tz.clone()).map_err(|err| {
            format!("could not interpret date {s:?} in the local time zone: {err}")
        })?,
        DayEdge::End => date
            .tomorrow()
            .map_err(|err| format!("date {s:?} has no representable next day: {err}"))?
            .to_zoned(tz.clone())
            .map_err(|err| format!("could not interpret date {s:?} in the local time zone: {err}"))?
            .checked_sub(SignedDuration::from_nanos(1))
            .map_err(|err| format!("could not compute the end of day for {s:?}: {err}"))?,
    };
    Ok(zoned.timestamp())
}

/// The `sensors_YYYY-MM-DD.jsonl` files that could hold samples in
/// `[since, until]`, by filename date. The local-date range is padded ±1 day:
/// the writer's and reader's time zones can differ, so a sample near local
/// midnight may sit in an adjacent day's file. Over-inclusion is harmless — the
/// streaming pass filters every sample by its actual instant — and it keeps the
/// selection independent of the machine's time zone (so tests are portable).
///
/// Candidate filenames are constructed and `stat`ed; the directory is never
/// enumerated (a missing file is simply skipped), so an unreadable or absent
/// `log_dir` yields no candidates rather than an error.
pub(crate) fn candidate_files(
    log_dir: &Path,
    since: Timestamp,
    until: Timestamp,
    tz: &TimeZone,
) -> Vec<PathBuf> {
    let start = since.to_zoned(tz.clone()).date();
    let end = until.to_zoned(tz.clone()).date();
    let first = start.yesterday().unwrap_or(start);
    let last = end.tomorrow().unwrap_or(end);

    let mut files = Vec::new();
    let mut date = first;
    loop {
        let path = log_dir.join(format!("{LOG_PREFIX}{date}.jsonl"));
        if path.is_file() {
            files.push(path);
        }
        if date >= last {
            break;
        }
        match date.tomorrow() {
            Ok(next) => date = next,
            Err(_) => break, // civil date ceiling — unreachable for real logs
        }
    }
    files
}

// ===========================================================================
// Aggregation
// ===========================================================================

/// One sampling gap: no sample for longer than 3× the configured interval.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Gap {
    /// Raw stream timestamp of the sample before the gap.
    from: String,
    /// Raw stream timestamp of the sample after the gap.
    to: String,
    /// Whole seconds between them (truncated).
    seconds: i64,
}

/// Windowed aggregates for one `(sensor, reading)` series. `min`/`max`/`sum`/
/// `first`/`last` fold over the finite `value`s only; non-finite samples are
/// counted but never enter a comparison (a NaN would poison every extreme).
#[derive(Debug)]
struct SeriesAgg {
    kind: &'static str,
    unit: String,
    samples: u64,
    non_finite: u64,
    finite: u64,
    sum: f64,
    min: f64,
    max: f64,
    first: Option<f64>,
    last: Option<f64>,
    in_violation: bool,
}

impl SeriesAgg {
    fn new(kind: &'static str) -> SeriesAgg {
        SeriesAgg {
            kind,
            unit: String::new(),
            samples: 0,
            non_finite: 0,
            finite: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            first: None,
            last: None,
            in_violation: false,
        }
    }

    fn observe(&mut self, value: f64, unit: &str) {
        self.samples += 1;
        // Last-seen unit: a series' unit is stable in practice, and the unit
        // is present even on a null-value line.
        if self.unit != unit {
            self.unit = unit.to_owned();
        }
        if value.is_finite() {
            self.finite += 1;
            self.sum += value;
            if value < self.min {
                self.min = value;
            }
            if value > self.max {
                self.max = value;
            }
            if self.first.is_none() {
                self.first = Some(value);
            }
            self.last = Some(value);
        } else {
            self.non_finite += 1;
        }
    }

    fn min_opt(&self) -> Option<f64> {
        (self.finite > 0).then_some(self.min)
    }

    fn max_opt(&self) -> Option<f64> {
        (self.finite > 0).then_some(self.max)
    }

    fn avg(&self) -> Option<f64> {
        (self.finite > 0).then(|| self.sum / self.finite as f64)
    }

    fn delta(&self) -> Option<f64> {
        match (self.first, self.last) {
            (Some(first), Some(last)) => Some(last - first),
            _ => None,
        }
    }
}

/// Accumulates the whole-window picture in a single streaming pass: record
/// counts, first/last timestamps, gaps, and per-series aggregates. No synthetic
/// leading/trailing gap is ever emitted — `meta.last_sample` versus
/// `window.until` is the liveness signal, so a quiet tail is visible without
/// inventing an edge gap.
pub(crate) struct Aggregator {
    interval_seconds: i64,
    samples: u64,
    first_sample: Option<String>,
    last_sample: Option<String>,
    /// The latest *in-order* sample, the gap-detection baseline. Distinct from
    /// `last_sample`, which advances on every record including out-of-order ones.
    prev_ts: Option<Timestamp>,
    prev_raw: Option<String>,
    series: BTreeMap<(String, String), SeriesAgg>,
    gaps: Vec<Gap>,
    gaps_total: u64,
}

impl Aggregator {
    pub(crate) fn new(interval_seconds: i64) -> Aggregator {
        Aggregator {
            interval_seconds,
            samples: 0,
            first_sample: None,
            last_sample: None,
            prev_ts: None,
            prev_raw: None,
            series: BTreeMap::new(),
            gaps: Vec::new(),
            gaps_total: 0,
        }
    }

    /// Fold one in-window sample. The caller guarantees `sample` falls inside
    /// the window (out-of-window samples are dropped before this call).
    pub(crate) fn observe(&mut self, sample: &Sample) {
        self.samples += 1;
        if self.first_sample.is_none() {
            self.first_sample = Some(sample.raw_timestamp.clone());
        }
        self.last_sample = Some(sample.raw_timestamp.clone());

        // Gap detection runs against the latest *in-order* sample. A stale or
        // backfilled record (delta ≤ 0) is untrusted input: it neither trips a
        // gap itself nor regresses the baseline, so it cannot fabricate a
        // phantom gap on the next in-order sample. The baseline is therefore the
        // max timestamp seen; the residual trade is a single *future*-dated
        // corrupt record (bounded above by `--until`, so at worst ≈ now) which
        // both fabricates one gap and freezes detection until real timestamps
        // pass it — fail-quiet, needs corrupt in-window data, window-bounded.
        match self.prev_ts {
            None => {
                self.prev_ts = Some(sample.timestamp);
                self.prev_raw = Some(sample.raw_timestamp.clone());
            }
            Some(prev_ts) => {
                let delta = sample.timestamp.as_nanosecond() - prev_ts.as_nanosecond();
                if delta > 0 {
                    let threshold =
                        i128::from(self.interval_seconds) * GAP_INTERVAL_MULTIPLIER * 1_000_000_000;
                    if delta > threshold {
                        // jiff bounds Timestamp to ±9999 years, so this quotient
                        // fits i64 for any real pair; saturate rather than wrap
                        // if a corrupt value ever pushed it past the bound.
                        let seconds = i64::try_from(delta / 1_000_000_000).unwrap_or(i64::MAX);
                        let from = self
                            .prev_raw
                            .clone()
                            .expect("prev_raw is set whenever prev_ts is");
                        self.push_gap(Gap {
                            from,
                            to: sample.raw_timestamp.clone(),
                            seconds,
                        });
                    }
                    self.prev_ts = Some(sample.timestamp);
                    self.prev_raw = Some(sample.raw_timestamp.clone());
                }
            }
        }

        // First occurrence of a `(sensor, reading)` within a sample wins,
        // matching the engine's duplicate handling (`engine.rs`), so the digest
        // aggregates and the re-derived violations can never disagree about a
        // duplicated reading in one record. Pre-sized to the reading count to
        // avoid rehashing on records with many readings.
        let mut seen: std::collections::HashSet<(&str, &str)> =
            std::collections::HashSet::with_capacity(sample.readings.len());
        for reading in &sample.readings {
            if !seen.insert((reading.sensor.as_str(), reading.reading.as_str())) {
                continue;
            }
            self.series
                .entry((reading.sensor.clone(), reading.reading.clone()))
                .or_insert_with(|| SeriesAgg::new(reading.kind))
                .observe(reading.value, &reading.unit);
        }
    }

    /// Record a gap, keeping the [`MAX_GAPS`] largest while preserving
    /// chronological (insertion) order; `gaps_total` stays exact regardless.
    fn push_gap(&mut self, gap: Gap) {
        self.gaps_total += 1;
        if self.gaps.len() < MAX_GAPS {
            self.gaps.push(gap);
            return;
        }
        // Evict the smallest retained gap if this one is larger, keeping the
        // remaining Vec in chronological order.
        let mut min_idx = 0;
        for (i, g) in self.gaps.iter().enumerate() {
            if g.seconds < self.gaps[min_idx].seconds {
                min_idx = i;
            }
        }
        if gap.seconds > self.gaps[min_idx].seconds {
            self.gaps.remove(min_idx);
            self.gaps.push(gap);
        }
    }

    /// Flag a `(sensor, reading)` series as having had a rule transition in the
    /// window (drives the forced ranking tier and the `in_violation` field).
    pub(crate) fn mark_in_violation(&mut self, sensor: &str, reading: &str) {
        if let Some(series) = self
            .series
            .get_mut(&(sensor.to_owned(), reading.to_owned()))
        {
            series.in_violation = true;
        }
    }

    pub(crate) fn samples(&self) -> u64 {
        self.samples
    }

    /// The number of distinct `(sensor, reading)` series seen in the window.
    pub(crate) fn series_count(&self) -> usize {
        self.series.len()
    }

    /// The canonical type label of a series, if it appeared in the window —
    /// used to resolve a violation's `--type` against its series (a violation
    /// with no in-window series yields `None`).
    pub(crate) fn series_kind(&self, sensor: &str, reading: &str) -> Option<&'static str> {
        self.series
            .get(&(sensor.to_owned(), reading.to_owned()))
            .map(|s| s.kind)
    }
}

// ===========================================================================
// Digest assembly, ranking, and byte-cap fitting
// ===========================================================================

/// The bundle of window/stream facts `report` hands to [`emit`], keeping the
/// call to two data arguments (Aggregator, transitions).
pub(crate) struct Emit<'a> {
    pub since: Timestamp,
    pub until: Timestamp,
    pub log_dir: &'a str,
    pub files_scanned: usize,
    pub interval_seconds: i64,
    pub skipped_lines: u64,
    pub rules_evaluated: usize,
    /// The `--match` needles, **pre-lowercased once** by the caller (also used
    /// for the scan-time violation count, so the fold happens exactly once).
    pub matches: &'a [String],
    /// Canonical type label the `--type` filter selects, if any.
    pub type_filter: Option<&'a str>,
    pub top: u32,
    pub max_bytes: u64,
    pub indent: u32,
}

/// One reading row in the digest. Field declaration order is the serialized key
/// order. Every aggregate is `Option`: a zero-finite series renders them all
/// `null` (and serde_json would render a stray NaN as `null` regardless).
#[derive(Serialize)]
struct ReadingRow<'a> {
    sensor: &'a str,
    reading: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    unit: &'a str,
    samples: u64,
    non_finite: u64,
    first: Option<f64>,
    last: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    avg: Option<f64>,
    delta: Option<f64>,
    in_violation: bool,
}

impl ReadingRow<'_> {
    /// Relative movement across the window: the forced-tier ranking's
    /// secondary key. Zero-finite rows score 0.0.
    ///
    /// The denominator is floored by `max(|first|, |last|, 1.0)` rather than a
    /// tiny epsilon: a series starting at exactly 0.0 (a fan or rail idling,
    /// then spinning up) would otherwise divide by ~0 and post an astronomical
    /// score that monopolizes `--top`, evicting genuinely interesting movers.
    /// With the floor, a 0→1200 RPM fan scores 1.0 and a 40→95 °C spike 0.58 —
    /// both plausibly ranked, neither pathological.
    ///
    /// Each value is divided by the floor *before* the subtraction, so an
    /// f64-extreme pair (untrusted input, e.g. `±1e308`) can't overflow
    /// `last − first` to `+inf` and rank above everything: with the floor
    /// `d ≥ |first|, |last|`, each quotient lands in `[-1, 1]` and the score in
    /// `[0, 2]`.
    fn score(&self) -> f64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) => {
                let d = first.abs().max(last.abs()).max(1.0);
                (last / d - first / d).abs()
            }
            _ => 0.0,
        }
    }
}

#[derive(Serialize)]
struct Window<'a> {
    since: &'a str,
    until: &'a str,
}

#[derive(Serialize)]
struct Truncated {
    readings_shown: usize,
    readings_total: usize,
    violations_shown: usize,
    violations_total: usize,
    gaps_shown: usize,
    gaps_total: u64,
}

#[derive(Serialize)]
struct Meta<'a> {
    window: Window<'a>,
    log_dir: &'a str,
    files_scanned: usize,
    interval_seconds: i64,
    samples: u64,
    skipped_lines: u64,
    first_sample: Option<&'a str>,
    last_sample: Option<&'a str>,
    series_total: usize,
    rules_evaluated: usize,
    truncated: Truncated,
}

#[derive(Serialize)]
struct Digest<'a> {
    schema_version: u32,
    meta: Meta<'a>,
    violations: &'a [Event<'a>],
    gaps: &'a [Gap],
    readings: &'a [ReadingRow<'a>],
}

/// The assembled, filtered, ranked model — everything the byte-cap fitter
/// drops from, plus the fixed meta facts it never touches.
struct Assembled<'a> {
    since: String,
    until: String,
    log_dir: &'a str,
    files_scanned: usize,
    interval_seconds: i64,
    samples: u64,
    skipped_lines: u64,
    first_sample: Option<&'a str>,
    last_sample: Option<&'a str>,
    series_total: usize,
    rules_evaluated: usize,
    readings_total: usize,
    gaps_total: u64,
    violations_total: usize,
    rows: Vec<ReadingRow<'a>>,
    gaps: Vec<Gap>,
    violations: Vec<Event<'a>>,
}

impl<'a> Assembled<'a> {
    fn meta(&self) -> Meta<'_> {
        Meta {
            window: Window {
                since: &self.since,
                until: &self.until,
            },
            log_dir: self.log_dir,
            files_scanned: self.files_scanned,
            interval_seconds: self.interval_seconds,
            samples: self.samples,
            skipped_lines: self.skipped_lines,
            first_sample: self.first_sample,
            last_sample: self.last_sample,
            series_total: self.series_total,
            rules_evaluated: self.rules_evaluated,
            truncated: Truncated {
                readings_shown: self.rows.len(),
                readings_total: self.readings_total,
                violations_shown: self.violations.len(),
                violations_total: self.violations_total,
                gaps_shown: self.gaps.len(),
                gaps_total: self.gaps_total,
            },
        }
    }

    fn render(&self, indent: u32) -> String {
        let digest = Digest {
            schema_version: DIGEST_SCHEMA_VERSION,
            meta: self.meta(),
            violations: &self.violations,
            gaps: &self.gaps,
            readings: &self.rows,
        };
        render_json(&digest, indent)
    }

    /// Shrink the digest to fit `max_bytes`, dropping detail worst-first: the
    /// lowest-ranked reading row (from the end), then the smallest gap (oldest
    /// on a tie), then the oldest violation (lowest seq). Meta, and the last of
    /// each section, are preserved as far as possible; only a digest whose
    /// irreducible core still overflows fails. The measured length excludes the
    /// trailing newline `println!` adds. This is the documented answer to the
    /// ROADMAP "digest truncation semantics" question.
    fn fit(mut self, max_bytes: u64, indent: u32) -> Result<String, String> {
        loop {
            let json = self.render(indent);
            if json.len() as u64 <= max_bytes {
                return Ok(json);
            }
            if self.rows.pop().is_some() {
                continue;
            }
            if !self.gaps.is_empty() {
                let mut min_idx = 0;
                for (i, gap) in self.gaps.iter().enumerate() {
                    if gap.seconds < self.gaps[min_idx].seconds {
                        min_idx = i;
                    }
                }
                self.gaps.remove(min_idx);
                continue;
            }
            if !self.violations.is_empty() {
                self.violations.remove(0);
                continue;
            }
            // Only meta remains and it still overflows.
            let minimal = self.render(indent).len();
            return Err(format!(
                "even the minimal digest is {minimal} bytes, over --max-bytes {max_bytes}; \
                 raise --max-bytes or lower --indent"
            ));
        }
    }
}

/// Serialize a digest: compact for `indent == 0`, else pretty with an
/// `indent`-space unit. Duplicates the `PrettyFormatter` pattern from the
/// frozen `snapshot.rs` deliberately (that module must not be touched).
fn render_json(digest: &Digest<'_>, indent: u32) -> String {
    if indent == 0 {
        return serde_json::to_string(digest).expect("serializing a Digest cannot fail");
    }
    let indent_unit = vec![b' '; indent as usize];
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent_unit);
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    digest
        .serialize(&mut serializer)
        .expect("serializing a Digest cannot fail");
    String::from_utf8(out).expect("serde_json output is UTF-8")
}

/// Does a reading row pass the display filters? `--match` is an
/// any-of-substrings test over the sensor and reading names; `--type` is an
/// exact canonical-label match. `matches` arrives **pre-lowercased** (folded
/// once by the caller), so only the row strings are lowered here.
fn row_passes(
    sensor: &str,
    reading: &str,
    kind: &str,
    matches: &[String],
    type_filter: Option<&str>,
) -> bool {
    let match_ok = matches.is_empty() || {
        let sensor = sensor.to_lowercase();
        let reading = reading.to_lowercase();
        matches
            .iter()
            .any(|n| sensor.contains(n.as_str()) || reading.contains(n.as_str()))
    };
    match_ok && type_filter.is_none_or(|label| label == kind)
}

/// Does a violation pass the display filters? `--match` runs over the
/// transition's own sensor/reading strings; the `--type` is resolved via the
/// violation's in-window series, so a violation with no in-window series (e.g.
/// a `missing` rule, or a source-unavailable event with no series at all) fails
/// any `--type` filter while still being matchable by name. `matches` arrives
/// **pre-lowercased**.
///
/// `pub(crate)` so `report` can count exact post-filter violation totals during
/// the streaming scan, using the same predicate `emit` uses to build the shown
/// list (one definition, no drift).
pub(crate) fn violation_passes(
    t: &Transition,
    matches: &[String],
    type_filter: Option<&str>,
    agg: &Aggregator,
) -> bool {
    let match_ok = matches.is_empty() || {
        let sensor = t.sensor.as_deref().map(str::to_lowercase);
        let reading = t.reading.as_deref().map(str::to_lowercase);
        matches.iter().any(|n| {
            sensor.as_deref().is_some_and(|s| s.contains(n.as_str()))
                || reading.as_deref().is_some_and(|r| r.contains(n.as_str()))
        })
    };
    if !match_ok {
        return false;
    }
    match type_filter {
        None => true,
        Some(label) => match (t.sensor.as_deref(), t.reading.as_deref()) {
            (Some(s), Some(r)) => agg.series_kind(s, r).is_some_and(|kind| kind == label),
            _ => false,
        },
    }
}

/// Assemble, filter, rank, fit, and print the digest. Prints the JSON to stdout
/// and returns exit 0 on success (including a zero-sample digest); on the
/// pathological "cannot fit even the minimal digest" case, warns on stderr and
/// returns exit 2.
pub(crate) fn emit(
    cfg: &Emit<'_>,
    agg: &Aggregator,
    transitions: &[Transition],
    violations_total: usize,
) -> ExitCode {
    // Reading rows: display-filtered, then ranked (violation tier first, then
    // relative movement desc, then (sensor, reading) asc).
    let mut rows: Vec<ReadingRow<'_>> = agg
        .series
        .iter()
        .filter(|((sensor, reading), s)| {
            row_passes(sensor, reading, s.kind, cfg.matches, cfg.type_filter)
        })
        .map(|((sensor, reading), s)| ReadingRow {
            sensor,
            reading,
            kind: s.kind,
            unit: &s.unit,
            samples: s.samples,
            non_finite: s.non_finite,
            first: s.first,
            last: s.last,
            min: s.min_opt(),
            max: s.max_opt(),
            avg: s.avg(),
            delta: s.delta(),
            in_violation: s.in_violation,
        })
        .collect();
    let readings_total = rows.len();
    rank_rows(&mut rows);
    // `--top` caps the *mover* tier only; every in-violation row is kept (the
    // byte-cap fitter is the sole thing that may later drop one). Ranking puts
    // the forced tier contiguously at the front, so keeping
    // `max(#in_violation, top)` rows preserves all violation rows plus up to
    // `top` rows total.
    let forced = rows.iter().filter(|r| r.in_violation).count();
    rows.truncate(forced.max(cfg.top as usize));

    // Violations shown: digest-local seq 1..n over the retained transitions,
    // display-filtered (hidden seqs simply don't appear). `violations_total` is
    // the exact post-filter count from the full scan — it may exceed what was
    // retained (the transition cap) or shown (the byte cap), so shown < total
    // is honest data-loss signalling, not a silent claim of completeness.
    let violations: Vec<Event<'_>> = transitions
        .iter()
        .enumerate()
        .filter(|(_, t)| violation_passes(t, cfg.matches, cfg.type_filter, agg))
        .map(|(i, t)| Event::from_transition(t, (i + 1) as u64))
        .collect();

    let assembled = Assembled {
        since: cfg.since.to_string(),
        until: cfg.until.to_string(),
        log_dir: cfg.log_dir,
        files_scanned: cfg.files_scanned,
        interval_seconds: cfg.interval_seconds,
        samples: agg.samples,
        skipped_lines: cfg.skipped_lines,
        first_sample: agg.first_sample.as_deref(),
        last_sample: agg.last_sample.as_deref(),
        series_total: agg.series.len(),
        rules_evaluated: cfg.rules_evaluated,
        readings_total,
        gaps_total: agg.gaps_total,
        violations_total,
        rows,
        gaps: agg.gaps.clone(),
        violations,
    };

    match assembled.fit(cfg.max_bytes, cfg.indent) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("sensorwatch report: {message}");
            ExitCode::from(exit::USAGE)
        }
    }
}

/// Rank reading rows in place: `in_violation` rows form a forced top tier;
/// within a tier, relative movement descending; ties broken by
/// `(sensor, reading)` ascending.
fn rank_rows(rows: &mut [ReadingRow<'_>]) {
    rows.sort_by(|a, b| {
        b.in_violation
            .cmp(&a.in_violation)
            .then_with(|| {
                b.score()
                    .partial_cmp(&a.score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| (a.sensor, a.reading).cmp(&(b.sensor, b.reading)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::Offset;

    // ---- parse_duration_secs ----

    #[test]
    fn duration_bare_integer_is_seconds() {
        assert_eq!(parse_duration_secs("3600").unwrap(), 3600);
        assert_eq!(parse_duration_secs("1").unwrap(), 1);
    }

    #[test]
    fn duration_units_and_compounds() {
        assert_eq!(parse_duration_secs("24h").unwrap(), 86_400);
        assert_eq!(parse_duration_secs("90m").unwrap(), 5_400);
        assert_eq!(parse_duration_secs("45s").unwrap(), 45);
        assert_eq!(parse_duration_secs("7d").unwrap(), 604_800);
        assert_eq!(parse_duration_secs("1d12h").unwrap(), 129_600);
        // Units are case-insensitive.
        assert_eq!(parse_duration_secs("1D12H").unwrap(), 129_600);
    }

    #[test]
    fn duration_rejects_bad_input() {
        for bad in [
            "", "0s", "0", "5x", "-5", "+5", "1d12", "d", "h30", "24 h", "1.5h",
        ] {
            assert!(
                parse_duration_secs(bad).is_err(),
                "expected {bad:?} to error"
            );
        }
    }

    #[test]
    fn duration_rejects_overflow() {
        assert!(parse_duration_secs("999999999999999999d").is_err());
        assert!(parse_duration_secs("99999999999999999999999").is_err());
    }

    // ---- parse_when ----

    fn tz_minus5() -> TimeZone {
        TimeZone::fixed(Offset::constant(-5))
    }

    #[test]
    fn parse_when_rfc3339_is_tz_independent() {
        let ts = parse_when("2026-02-18T08:00:00-05:00", &tz_minus5(), DayEdge::Start).unwrap();
        assert_eq!(ts.to_string(), "2026-02-18T13:00:00Z");
        // A different tz argument does not change an offset-carrying instant.
        let same = parse_when("2026-02-18T08:00:00-05:00", &TimeZone::UTC, DayEdge::Start).unwrap();
        assert_eq!(ts, same);
    }

    #[test]
    fn parse_when_local_datetime_uses_tz() {
        let ts = parse_when("2026-02-18T08:30:00", &tz_minus5(), DayEdge::Start).unwrap();
        // 08:30 at −05:00 is 13:30 UTC — the time-of-day is not swallowed.
        assert_eq!(ts.to_string(), "2026-02-18T13:30:00Z");
    }

    #[test]
    fn parse_when_bare_date_start_edge_is_local_midnight() {
        let ts = parse_when("2026-02-18", &tz_minus5(), DayEdge::Start).unwrap();
        // Local midnight −05:00 is 05:00 UTC.
        assert_eq!(ts.to_string(), "2026-02-18T05:00:00Z");
    }

    #[test]
    fn parse_when_bare_date_end_edge_is_last_instant_of_day() {
        let ts = parse_when("2026-02-18", &tz_minus5(), DayEdge::End).unwrap();
        // Next local midnight (02-19 00:00 −05:00 = 02-19 05:00Z) minus 1 ns.
        assert_eq!(ts.to_string(), "2026-02-19T04:59:59.999999999Z");
    }

    #[test]
    fn parse_when_rejects_garbage() {
        assert!(parse_when("not-a-time", &tz_minus5(), DayEdge::Start).is_err());
        assert!(parse_when("2026-13-40", &tz_minus5(), DayEdge::Start).is_err());
    }

    // ---- candidate_files ----

    #[test]
    fn candidate_files_pad_one_day_each_side_and_skip_absent() {
        let dir = crate::testutil::TempDir::new();
        // Create files for 02-17 .. 02-20; window is 02-18 .. 02-19.
        for day in ["2026-02-17", "2026-02-18", "2026-02-19", "2026-02-20"] {
            std::fs::write(dir.path().join(format!("sensors_{day}.jsonl")), b"").unwrap();
        }
        let since = parse_when("2026-02-18", &tz_minus5(), DayEdge::Start).unwrap();
        let until = parse_when("2026-02-19", &tz_minus5(), DayEdge::End).unwrap();
        let files = candidate_files(dir.path(), since, until, &tz_minus5());
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        // Padding pulls in 02-17 and 02-20 as well; all four exist.
        assert_eq!(
            names,
            vec![
                "sensors_2026-02-17.jsonl",
                "sensors_2026-02-18.jsonl",
                "sensors_2026-02-19.jsonl",
                "sensors_2026-02-20.jsonl",
            ]
        );
    }

    #[test]
    fn candidate_files_absent_dir_yields_nothing() {
        let dir = crate::testutil::TempDir::new();
        let missing = dir.path().join("does-not-exist");
        let since = parse_when("2026-02-18", &tz_minus5(), DayEdge::Start).unwrap();
        let until = parse_when("2026-02-18", &tz_minus5(), DayEdge::End).unwrap();
        assert!(candidate_files(&missing, since, until, &tz_minus5()).is_empty());
    }

    // ---- aggregation ----

    fn sample(raw: &str, readings: Vec<(&str, &str, &'static str, f64, &str)>) -> Sample {
        Sample {
            timestamp: raw.parse().unwrap(),
            raw_timestamp: raw.to_owned(),
            readings: readings
                .into_iter()
                .map(
                    |(sensor, reading, kind, value, unit)| crate::source::SampleReading {
                        sensor: sensor.to_owned(),
                        reading: reading.to_owned(),
                        kind,
                        value,
                        min: f64::NAN,
                        max: f64::NAN,
                        avg: f64::NAN,
                        unit: unit.to_owned(),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn aggregate_folds_finite_and_counts_non_finite() {
        let mut agg = Aggregator::new(10);
        // +12V across three samples, one with a NaN (a logged null).
        agg.observe(&sample(
            "2026-02-18T08:00:00-05:00",
            vec![("PSU", "+12V", "Voltage", 12.0, "V")],
        ));
        agg.observe(&sample(
            "2026-02-18T08:00:10-05:00",
            vec![("PSU", "+12V", "Voltage", f64::NAN, "V")],
        ));
        agg.observe(&sample(
            "2026-02-18T08:00:20-05:00",
            vec![("PSU", "+12V", "Voltage", 12.5, "V")],
        ));
        let s = &agg.series[&("PSU".to_owned(), "+12V".to_owned())];
        assert_eq!(s.samples, 3);
        assert_eq!(s.non_finite, 1);
        assert_eq!(s.first, Some(12.0));
        assert_eq!(s.last, Some(12.5));
        assert_eq!(s.min_opt(), Some(12.0));
        assert_eq!(s.max_opt(), Some(12.5));
        assert_eq!(s.avg(), Some(12.25)); // (12.0 + 12.5) / 2 finite
        assert_eq!(s.delta(), Some(0.5));
    }

    #[test]
    fn aggregate_zero_finite_series_has_null_aggregates() {
        let mut agg = Aggregator::new(10);
        agg.observe(&sample(
            "2026-02-18T08:00:00-05:00",
            vec![("PSU", "Dead", "Voltage", f64::NAN, "V")],
        ));
        let s = &agg.series[&("PSU".to_owned(), "Dead".to_owned())];
        assert_eq!(s.samples, 1);
        assert_eq!(s.non_finite, 1);
        assert_eq!(s.avg(), None);
        assert_eq!(s.min_opt(), None);
        assert_eq!(s.delta(), None);
    }

    #[test]
    fn gap_boundary_is_strictly_greater_than_three_intervals() {
        // interval 10 → threshold exactly 30 s.
        let mut agg = Aggregator::new(10);
        agg.observe(&sample("2026-02-18T08:00:00-05:00", vec![]));
        // Exactly 30 s later: not a gap.
        agg.observe(&sample("2026-02-18T08:00:30-05:00", vec![]));
        assert_eq!(agg.gaps_total, 0, "exactly 3× interval is not a gap");
        // 30 s + 1 µs after the last: a gap.
        agg.observe(&sample("2026-02-18T08:01:00.000001-05:00", vec![]));
        assert_eq!(agg.gaps_total, 1, "just over 3× interval is a gap");
        assert_eq!(agg.gaps[0].seconds, 30);
        assert_eq!(agg.gaps[0].from, "2026-02-18T08:00:30-05:00");
        assert_eq!(agg.gaps[0].to, "2026-02-18T08:01:00.000001-05:00");
    }

    #[test]
    fn out_of_order_samples_do_not_produce_a_gap() {
        let mut agg = Aggregator::new(10);
        agg.observe(&sample("2026-02-18T08:05:00-05:00", vec![]));
        agg.observe(&sample("2026-02-18T08:00:00-05:00", vec![])); // earlier
        assert_eq!(agg.gaps_total, 0);
    }

    #[test]
    fn out_of_order_sample_does_not_regress_the_gap_baseline() {
        // A stale record must not become the baseline: otherwise the next
        // in-order sample measures its gap against the wrong point and
        // fabricates a phantom outage.
        let mut agg = Aggregator::new(10); // threshold 30 s
        agg.observe(&sample("2026-02-18T08:00:00-05:00", vec![]));
        agg.observe(&sample("2026-02-18T07:00:00-05:00", vec![])); // stale, 1 h back
        agg.observe(&sample("2026-02-18T08:00:10-05:00", vec![])); // 10 s after #1
                                                                   // Baseline stayed at 08:00:00, so 10 s < 30 s → no gap. Without the
                                                                   // guard, #3 vs the stale #2 would report a ~3610 s gap.
        assert_eq!(agg.gaps_total, 0);
    }

    #[test]
    fn duplicate_series_in_one_sample_keeps_first_occurrence() {
        // The engine folds a duplicate (sensor, reading) within a sample to its
        // first occurrence; the aggregator must agree, or the digest would show
        // a min/last the engine never evaluated.
        let mut agg = Aggregator::new(10);
        agg.observe(&sample(
            "2026-02-18T08:00:00-05:00",
            vec![
                ("PSU", "+12V", "Voltage", 12.0, "V"),
                ("PSU", "+12V", "Voltage", 11.0, "V"), // duplicate — ignored
            ],
        ));
        let s = &agg.series[&("PSU".to_owned(), "+12V".to_owned())];
        assert_eq!(s.samples, 1, "the duplicate is not a second sample");
        assert_eq!(s.first, Some(12.0));
        assert_eq!(s.last, Some(12.0));
        assert_eq!(s.min_opt(), Some(12.0), "the 11.0 duplicate never entered");
        assert_eq!(s.max_opt(), Some(12.0));
    }

    // ---- ranking ----

    fn row(
        sensor: &'static str,
        reading: &'static str,
        first: f64,
        last: f64,
        viol: bool,
    ) -> ReadingRow<'static> {
        ReadingRow {
            sensor,
            reading,
            kind: "Voltage",
            unit: "V",
            samples: 2,
            non_finite: 0,
            first: Some(first),
            last: Some(last),
            min: Some(first.min(last)),
            max: Some(first.max(last)),
            avg: Some((first + last) / 2.0),
            delta: Some(last - first),
            in_violation: viol,
        }
    }

    #[test]
    fn ranking_forces_violation_tier_above_higher_scoring_rows() {
        // A: big movement, not in violation. B: tiny movement, in violation.
        let mut rows = vec![
            row("A", "big", 10.0, 20.0, false),   // score 1.0
            row("B", "small", 12.0, 12.25, true), // score ~0.02, but violation
        ];
        rank_rows(&mut rows);
        assert_eq!(rows[0].sensor, "B", "violation tier wins");
        assert_eq!(rows[1].sensor, "A");
    }

    #[test]
    fn score_floors_denominator_for_zero_start_series() {
        // A fan idling at 0 then spinning up: bounded score, not ~1e12.
        let fan = row("Fan", "PSU Fan", 0.0, 1200.0, false);
        assert!(
            (fan.score() - 1.0).abs() < 1e-9,
            "0→1200 should score 1.0, got {}",
            fan.score()
        );
        // A 40→95 °C spike still ranks below the full-swing fan, and neither is
        // astronomically large (the pre-fix zero-start score was ~8e11).
        let temp = row("GPU", "Hot", 40.0, 95.0, false);
        assert!(temp.score() < fan.score());
        assert!(fan.score() < 10.0, "zero-start score stays bounded");
    }

    #[test]
    fn score_stays_finite_for_f64_extreme_values() {
        // Untrusted input near opposite f64 extremes: `last - first` would
        // overflow to +inf and outrank everything. Dividing first keeps the
        // score finite and bounded in [0, 2].
        let extreme = row("Evil", "Wide", -1e308, 1e308, false);
        assert!(extreme.score().is_finite(), "score must not be +inf");
        assert!(extreme.score() <= 2.0 + 1e-9, "score is bounded by 2");
        // It still ranks below a normal in-violation row (forced tier wins).
        let mut rows = vec![extreme, row("PSU", "+12V", 12.0, 11.5, true)];
        rank_rows(&mut rows);
        assert_eq!(
            rows[0].reading, "+12V",
            "violation tier beats any finite score"
        );
    }

    #[test]
    fn ranking_sorts_by_score_then_name_within_a_tier() {
        let mut rows = vec![
            row("Z", "z", 10.0, 10.1, false),  // score ≈ 0.0099
            row("A", "a", 10.0, 11.0, false),  // score ≈ 0.091
            row("M", "m1", 10.0, 10.5, false), // score ≈ 0.048
            row("M", "m2", 10.0, 10.5, false), // ties m1 → name asc
        ];
        rank_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.reading).collect();
        assert_eq!(order, vec!["a", "m1", "m2", "z"]);
    }

    // ---- fitting ----

    fn assembled_with_rows(n: usize) -> Assembled<'static> {
        // Distinct series so ranking is stable; leaked names keep 'static.
        // Rank as `emit` would before handing rows to the fitter.
        let mut rows: Vec<ReadingRow<'static>> = (0..n)
            .map(|i| {
                let name: &'static str = Box::leak(format!("S{i:02}").into_boxed_str());
                row(name, name, 1.0, 1.0 + i as f64 * 0.01, false)
            })
            .collect();
        rank_rows(&mut rows);
        let total = rows.len();
        Assembled {
            since: "2026-02-18T05:00:00Z".to_owned(),
            until: "2026-02-19T05:00:00Z".to_owned(),
            log_dir: "/logs",
            files_scanned: 1,
            interval_seconds: 10,
            samples: 2,
            skipped_lines: 0,
            first_sample: Some("2026-02-18T08:00:00-05:00"),
            last_sample: Some("2026-02-18T08:00:10-05:00"),
            series_total: total,
            rules_evaluated: 0,
            readings_total: total,
            gaps_total: 0,
            violations_total: 0,
            rows,
            gaps: Vec::new(),
            violations: Vec::new(),
        }
    }

    #[test]
    fn fit_respects_cap_across_a_sweep_and_keeps_counters_consistent() {
        for cap in [512u64, 1024, 2048, 4096, 8192, 16384] {
            let assembled = assembled_with_rows(30);
            let json = assembled.fit(cap, 0).expect("30 tiny rows fit under 512+");
            assert!(json.len() as u64 <= cap, "cap {cap}: {} bytes", json.len());
            // The shown counter matches the array length actually rendered.
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            let shown = value["readings"].as_array().unwrap().len();
            assert_eq!(
                value["meta"]["truncated"]["readings_shown"]
                    .as_u64()
                    .unwrap() as usize,
                shown
            );
            assert_eq!(
                value["meta"]["truncated"]["readings_total"]
                    .as_u64()
                    .unwrap(),
                30
            );
        }
    }

    #[test]
    fn fit_drops_lowest_ranked_rows_first_from_the_end() {
        let assembled = assembled_with_rows(30);
        // A cap that forces some rows out but keeps meta.
        let json = assembled.fit(700, 0).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let shown = value["readings"].as_array().unwrap();
        assert!(shown.len() < 30, "some rows dropped");
        // The highest-movement row (S29) ranks first and survives; the lowest
        // (S00, zero movement) is dropped from the end.
        assert_eq!(shown[0]["reading"], "S29");
    }

    #[test]
    fn fit_too_small_cap_reports_minimal_overflow() {
        let assembled = assembled_with_rows(5);
        // 100 bytes cannot hold even the meta-only digest.
        let err = assembled.fit(100, 0).unwrap_err();
        assert!(err.contains("even the minimal digest"), "{err}");
        assert!(err.contains("--max-bytes 100"), "{err}");
    }

    #[test]
    fn compact_and_indented_digests_parse_to_the_same_value() {
        let compact = assembled_with_rows(3).fit(8192, 0).unwrap();
        let indented = assembled_with_rows(3).fit(8192, 2).unwrap();
        let a: serde_json::Value = serde_json::from_str(&compact).unwrap();
        let b: serde_json::Value = serde_json::from_str(&indented).unwrap();
        assert_eq!(a, b);
        assert!(indented.contains('\n'), "indent 2 is multi-line");
        assert!(!compact.contains('\n'), "indent 0 is one line");
    }
}

//! Deterministic rule evaluation.
//!
//! Everything is sample-count based; the engine never reads a clock —
//! timestamps are copied from ticks into transitions. Identical tick
//! sequences therefore produce identical transition sequences, which is the
//! property that makes the engine testable under replay and lets the
//! `report` command re-derive violations from logs, bit for bit.
//!
//! State is kept per (rule × matched series), a series being one
//! `(sensor, reading)` pair: a rule whose matchers cover several readings
//! fires and clears independently for each. Series keys are the exact
//! case-preserved stream strings; matching is case-insensitive. A duplicate
//! `(sensor, reading)` within one sample is folded to its first occurrence.
//!
//! Transition order within one tick is deterministic: rule declaration
//! order; within a rule, sample order for present readings, then
//! lexicographic `(sensor, reading)` order for missing-series checks.
//!
//! Two rules the naive implementations get wrong, spelled out:
//!
//! - **NaN is unevaluable, not a verdict.** Every comparison involving NaN
//!   is false, so "violates" is naturally false — but so is the *clear*
//!   test, and clearing on `!op(NaN, clear)` would spuriously close a fired
//!   alert on a dead reading. A NaN compared value resets an unfired streak
//!   and holds a fired state, nothing more.
//! - **Unavailable ticks freeze non-source rules entirely** — no counter
//!   movement in either direction, so a violation streak survives an
//!   outage. In a replayed log an outage is simply no records at all, so
//!   freezing is exactly what makes live evaluation agree with replaying
//!   the same period. Only `source-unavailable` rules observe Unavailable
//!   ticks. Similarly, a series absent from a present sample resets its
//!   counters (an affirmative discontinuity) but never clears a fired state
//!   — a false all-clear because data vanished would be worse than silence.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use jiff::Timestamp;

use crate::rules::{Metric, Rule, RuleKind, RuleSet, Severity};
use crate::source::{Sample, SampleReading, Tick};

/// A fired-or-cleared edge for one rule (and series, where applicable) —
/// everything the `watch` event payload needs except the persisted sequence
/// number, which the command layer owns.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Transition {
    pub rule: String,
    pub kind: RuleKind,
    pub severity: Severity,
    pub state: TransitionState,
    /// The triggering tick's timestamp, verbatim from the stream.
    pub raw_timestamp: String,
    pub timestamp: Timestamp,
    /// The triggering series; `None` for source-unavailable rules.
    pub sensor: Option<String>,
    pub reading: Option<String>,
    /// The compared quantity at the edge: the selected metric (threshold),
    /// the window delta (rate), or the reading value (stale). `None` for
    /// missing and source-unavailable rules.
    pub value: Option<f64>,
    /// The series' last-seen unit; `None` for source-unavailable rules.
    pub unit: Option<String>,
    /// The configured threshold, for threshold/rate rules.
    pub threshold: Option<f64>,
    /// On `Fired`: the debounce count the streak just reached
    /// (`for_samples`). On `Cleared`: the total number of samples on which
    /// the fire condition held over the whole episode, debounce lead-in
    /// included.
    pub samples_in_violation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionState {
    Fired,
    Cleared,
}

pub(crate) struct Engine {
    rules: Vec<Rule>,
    states: Vec<RuleState>,
    /// Monotonic tick counter; series presence is tracked by stamping
    /// `last_seen_tick`, so no per-tick set allocations.
    tick_index: u64,
}

/// All evaluation state for one rule.
#[derive(Default)]
struct RuleState {
    /// Per-series state, keyed by the exact `(sensor, reading)` strings.
    /// BTreeMap so missing-series checks iterate deterministically.
    series: BTreeMap<(String, String), SeriesState>,
    /// The scalar gate for source-unavailable rules.
    source: Gate,
}

/// Per-(rule × series) state. One struct serves every rule kind; the fields
/// a kind does not use stay idle and empty.
#[derive(Default)]
struct SeriesState {
    gate: Gate,
    /// Last-seen unit, carried into transition payloads.
    unit: String,
    last_seen_tick: u64,
    /// Stale: `to_bits` of (value, min, max, avg) from the previous sample.
    baseline: Option<[u64; 4]>,
    /// Rate: the previous `window_samples - 1` metric values, oldest first.
    window: VecDeque<f64>,
}

/// The debounce/hysteresis state machine shared by every rule kind: `streak`
/// counts consecutive violating samples toward `for_samples`; `total` counts
/// every violating sample of the current episode for the Cleared payload.
#[derive(Default)]
struct Gate {
    fired: bool,
    streak: u32,
    total: u32,
}

/// What one evaluated sample means for a gate. NaN never produces a verdict
/// (see the module docs); the hysteresis band produces `Holds`.
enum Verdict {
    Violates,
    Recovers,
    Holds,
}

enum GateEvent {
    Fired,
    Cleared { total: u32 },
}

impl Gate {
    fn apply(&mut self, verdict: Verdict, for_samples: u32) -> Option<GateEvent> {
        match verdict {
            Verdict::Violates => {
                if self.fired {
                    self.total += 1;
                    None
                } else {
                    self.streak += 1;
                    if self.streak == for_samples {
                        self.fired = true;
                        self.total = self.streak;
                        Some(GateEvent::Fired)
                    } else {
                        None
                    }
                }
            }
            Verdict::Recovers => {
                if self.fired {
                    let total = self.total;
                    *self = Gate::default();
                    Some(GateEvent::Cleared { total })
                } else {
                    self.streak = 0;
                    None
                }
            }
            Verdict::Holds => None,
        }
    }

    /// An unevaluable (NaN) sample: resets an unfired streak, holds a fired
    /// state. Distinct from `Recovers` — it must never clear.
    fn unevaluable(&mut self) {
        if !self.fired {
            self.streak = 0;
        }
    }
}

impl Engine {
    pub(crate) fn new(rules: RuleSet) -> Engine {
        let rules = rules.into_rules();
        let states = rules.iter().map(|_| RuleState::default()).collect();
        Engine {
            rules,
            states,
            tick_index: 0,
        }
    }

    /// Feed one tick; returns every transition it caused, in deterministic
    /// order.
    pub(crate) fn observe(&mut self, tick: &Tick) -> Vec<Transition> {
        self.tick_index += 1;
        let mut out = Vec::new();
        match tick {
            Tick::Sample(sample) => self.observe_sample(sample, &mut out),
            Tick::Unavailable {
                timestamp,
                raw_timestamp,
            } => self.observe_unavailable(*timestamp, raw_timestamp, &mut out),
        }
        out
    }

    fn observe_unavailable(
        &mut self,
        timestamp: Timestamp,
        raw_timestamp: &str,
        out: &mut Vec<Transition>,
    ) {
        // Only source-unavailable rules observe these ticks; every other
        // rule's state is frozen (module docs).
        for (rule, state) in self.rules.iter().zip(self.states.iter_mut()) {
            if rule.kind != RuleKind::SourceUnavailable {
                continue;
            }
            if let Some(event) = state.source.apply(Verdict::Violates, rule.for_samples) {
                out.push(source_transition(rule, event, timestamp, raw_timestamp));
            }
        }
    }

    fn observe_sample(&mut self, sample: &Sample, out: &mut Vec<Transition>) {
        // Lowercase each reading's names once per tick, not once per rule.
        let lowered: Vec<(String, String)> = sample
            .readings
            .iter()
            .map(|r| (r.sensor.to_lowercase(), r.reading.to_lowercase()))
            .collect();

        let tick_index = self.tick_index;
        for (rule, state) in self.rules.iter().zip(self.states.iter_mut()) {
            if rule.kind == RuleKind::SourceUnavailable {
                // Any sample recovers the source.
                if let Some(event) = state.source.apply(Verdict::Recovers, rule.for_samples) {
                    out.push(source_transition(
                        rule,
                        event,
                        sample.timestamp,
                        &sample.raw_timestamp,
                    ));
                }
                continue;
            }

            // Present pass, in sample order.
            for (reading, (sensor_lower, reading_lower)) in
                sample.readings.iter().zip(lowered.iter())
            {
                if !rule
                    .matcher
                    .matches(sensor_lower, reading_lower, reading.kind)
                {
                    continue;
                }
                let series = state
                    .series
                    .entry((reading.sensor.clone(), reading.reading.clone()))
                    .or_default();
                if series.last_seen_tick == tick_index {
                    // Duplicate (sensor, reading) in one sample: first wins.
                    continue;
                }
                series.last_seen_tick = tick_index;
                series.unit = reading.unit.clone();

                let evaluation = match rule.kind {
                    RuleKind::Threshold => evaluate_threshold(rule, series, reading),
                    RuleKind::Rate => evaluate_rate(rule, series, reading),
                    RuleKind::Stale => evaluate_stale(rule, series, reading),
                    RuleKind::Missing => {
                        // Presence recovers; the entry itself records
                        // "previously seen" for the absent pass below.
                        series
                            .gate
                            .apply(Verdict::Recovers, rule.for_samples)
                            .map(|event| (event, None))
                    }
                    RuleKind::SourceUnavailable => unreachable!("handled above"),
                };
                if let Some((event, value)) = evaluation {
                    out.push(series_transition(
                        rule,
                        event,
                        sample,
                        &(reading.sensor.clone(), reading.reading.clone()),
                        value,
                        &reading.unit,
                    ));
                }
            }

            // Absent pass, in series-key order.
            for (key, series) in state.series.iter_mut() {
                if series.last_seen_tick == tick_index {
                    continue;
                }
                match rule.kind {
                    RuleKind::Missing => {
                        // Absent after previously seen: the fire condition.
                        if let Some(event) = series.gate.apply(Verdict::Violates, rule.for_samples)
                        {
                            let unit = series.unit.clone();
                            out.push(series_transition(rule, event, sample, key, None, &unit));
                        }
                    }
                    RuleKind::Threshold | RuleKind::Rate | RuleKind::Stale => {
                        // An affirmative discontinuity: reset the counters,
                        // but never clear a fired state (that takes an
                        // actual recovered sample). The absence itself is a
                        // `missing` rule's condition, and the two compose.
                        series.gate.streak = 0;
                        series.window.clear();
                        series.baseline = None;
                    }
                    RuleKind::SourceUnavailable => unreachable!("handled above"),
                }
            }
        }
    }
}

/// Evaluate a threshold rule for one present reading; returns the gate event
/// and the compared value.
fn evaluate_threshold(
    rule: &Rule,
    series: &mut SeriesState,
    reading: &SampleReading,
) -> Option<(GateEvent, Option<f64>)> {
    let compared = select_metric(rule.metric, reading);
    let verdict = value_verdict(rule, &mut series.gate, compared)?;
    series
        .gate
        .apply(verdict, rule.for_samples)
        .map(|event| (event, Some(compared)))
}

/// Evaluate a rate rule: the compared quantity is the delta between the
/// current metric and the value `window_samples - 1` samples back. Until the
/// window is full nothing is evaluated — no streak movement, no clearing.
fn evaluate_rate(
    rule: &Rule,
    series: &mut SeriesState,
    reading: &SampleReading,
) -> Option<(GateEvent, Option<f64>)> {
    let current = select_metric(rule.metric, reading);
    let width = rule.window_samples.expect("validated: rate has a window") as usize - 1;
    let delta = if series.window.len() == width {
        // Oldest first: front is the value `width` samples back. A NaN
        // anywhere in the span makes the delta NaN — unevaluable, like any
        // other NaN (a delta across an unknown is unknowable).
        Some(current - series.window.front().copied().unwrap_or(f64::NAN))
    } else {
        None
    };
    series.window.push_back(current);
    if series.window.len() > width {
        series.window.pop_front();
    }

    let delta = delta?;
    let verdict = value_verdict(rule, &mut series.gate, delta)?;
    series
        .gate
        .apply(verdict, rule.for_samples)
        .map(|event| (event, Some(delta)))
}

/// Evaluate a stale rule: the fire condition is the current reading being
/// bit-identical to the previous one across the FULL (value, min, max, avg)
/// tuple. Value-only would false-fire on quantized sensors that legitimately
/// hold an exact value while HWiNFO's lifetime average keeps moving; the
/// frozen full tuple is precisely "the source stopped updating this
/// reading". Bit comparison keeps NaN well-defined: a reading that decodes
/// to the same NaN payload every sample (replayed nulls do) is stale.
fn evaluate_stale(
    rule: &Rule,
    series: &mut SeriesState,
    reading: &SampleReading,
) -> Option<(GateEvent, Option<f64>)> {
    let tuple = [
        reading.value.to_bits(),
        reading.min.to_bits(),
        reading.max.to_bits(),
        reading.avg.to_bits(),
    ];
    let verdict = match series.baseline {
        // The first sample only establishes the baseline; nothing can fire.
        None => {
            series.baseline = Some(tuple);
            return None;
        }
        Some(previous) if previous == tuple => Verdict::Violates,
        Some(_) => {
            series.baseline = Some(tuple);
            Verdict::Recovers
        }
    };
    // `for_samples` counts repeats, so the rule fires on the
    // (for_samples + 1)th identical sample. The payload value is the frozen
    // (or, on Cleared, the freshly changed) reading value.
    series
        .gate
        .apply(verdict, rule.for_samples)
        .map(|event| (event, Some(reading.value)))
}

/// The three-way verdict for a compared value against threshold/clear.
/// Returns `None` when the sample is unevaluable (NaN) — the gate has
/// already been adjusted.
fn value_verdict(rule: &Rule, gate: &mut Gate, compared: f64) -> Option<Verdict> {
    let op = rule.op.expect("validated: value rules have an op");
    let threshold = rule.threshold.expect("validated: value rules have one");
    if compared.is_nan() {
        gate.unevaluable();
        return None;
    }
    Some(if gate.fired {
        // clear_level = clear.unwrap_or(threshold): with no configured
        // hysteresis this degenerates to exactly "fire condition false".
        let clear_level = rule.clear.unwrap_or(threshold);
        if !op.compare(compared, clear_level) {
            Verdict::Recovers
        } else if op.compare(compared, threshold) {
            Verdict::Violates
        } else {
            // The hysteresis band: recovered past the threshold but not yet
            // past the clear level. Holds silently.
            Verdict::Holds
        }
    } else if op.compare(compared, threshold) {
        Verdict::Violates
    } else {
        Verdict::Recovers
    })
}

fn select_metric(metric: Metric, reading: &SampleReading) -> f64 {
    match metric {
        Metric::Value => reading.value,
        Metric::Min => reading.min,
        Metric::Max => reading.max,
        Metric::Avg => reading.avg,
    }
}

fn series_transition(
    rule: &Rule,
    event: GateEvent,
    sample: &Sample,
    key: &(String, String),
    value: Option<f64>,
    unit: &str,
) -> Transition {
    let (state, samples_in_violation) = split_event(event, rule.for_samples);
    Transition {
        rule: rule.name.clone(),
        kind: rule.kind,
        severity: rule.severity,
        state,
        raw_timestamp: sample.raw_timestamp.clone(),
        timestamp: sample.timestamp,
        sensor: Some(key.0.clone()),
        reading: Some(key.1.clone()),
        value,
        unit: Some(unit.to_owned()),
        threshold: rule.threshold,
        samples_in_violation,
    }
}

fn source_transition(
    rule: &Rule,
    event: GateEvent,
    timestamp: Timestamp,
    raw_timestamp: &str,
) -> Transition {
    let (state, samples_in_violation) = split_event(event, rule.for_samples);
    Transition {
        rule: rule.name.clone(),
        kind: rule.kind,
        severity: rule.severity,
        state,
        raw_timestamp: raw_timestamp.to_owned(),
        timestamp,
        sensor: None,
        reading: None,
        value: None,
        unit: None,
        threshold: None,
        samples_in_violation,
    }
}

fn split_event(event: GateEvent, for_samples: u32) -> (TransitionState, u32) {
    match event {
        GateEvent::Fired => (TransitionState::Fired, for_samples),
        GateEvent::Cleared { total } => (TransitionState::Cleared, total),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleSet;

    // ---- synthetic-stream builders ----

    /// Deterministic tick timestamps: 10 s apart, raw string deliberately
    /// NOT derived from the parsed timestamp, so tests prove the engine
    /// passes the stream's rendering through verbatim.
    fn tick_time(i: i64) -> (Timestamp, String) {
        let ts = Timestamp::from_second(1_770_000_000 + i * 10).unwrap();
        (ts, format!("raw-{i}"))
    }

    fn sample(i: i64, readings: Vec<SampleReading>) -> Tick {
        let (timestamp, raw_timestamp) = tick_time(i);
        Tick::Sample(Sample {
            timestamp,
            raw_timestamp,
            readings,
        })
    }

    fn unavailable(i: i64) -> Tick {
        let (timestamp, raw_timestamp) = tick_time(i);
        Tick::Unavailable {
            timestamp,
            raw_timestamp,
        }
    }

    // One argument per SampleReading field, deliberately.
    #[allow(clippy::too_many_arguments)]
    fn reading_full(
        sensor: &str,
        name: &str,
        kind: &'static str,
        value: f64,
        min: f64,
        max: f64,
        avg: f64,
        unit: &str,
    ) -> SampleReading {
        SampleReading {
            sensor: sensor.to_owned(),
            reading: name.to_owned(),
            kind,
            value,
            min,
            max,
            avg,
            unit: unit.to_owned(),
        }
    }

    /// A Voltage reading with FIXED min/max/avg, so for stale rules a
    /// repeated value is a fully repeated tuple.
    fn r(sensor: &str, name: &str, value: f64) -> SampleReading {
        reading_full(sensor, name, "Voltage", value, 0.0, 100.0, 50.0, "V")
    }

    fn engine(rules_toml: &str) -> Engine {
        Engine::new(RuleSet::from_toml_str(rules_toml).expect("test rules parse"))
    }

    /// Feed `ticks` numbered from 1; return (tick number, transition) pairs.
    fn run(engine: &mut Engine, ticks: Vec<Tick>) -> Vec<(i64, Transition)> {
        let mut out = Vec::new();
        for (index, tick) in ticks.into_iter().enumerate() {
            let number = index as i64 + 1;
            for transition in engine.observe(&tick) {
                out.push((number, transition));
            }
        }
        out
    }

    fn states(transitions: &[(i64, Transition)]) -> Vec<(i64, TransitionState)> {
        transitions.iter().map(|(i, t)| (*i, t.state)).collect()
    }

    // ---- threshold ----

    const CPU_GT_90_FOR_3: &str = r#"
        [[rules]]
        name = "cpu-hot"
        kind = "threshold"
        sensor = "cpu"
        op = ">"
        threshold = 90.0
        for_samples = 3
    "#;

    #[test]
    fn threshold_debounce_fires_at_count_not_below_and_resets_on_recovery() {
        let mut engine = engine(CPU_GT_90_FOR_3);
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0)]), // streak 1
                sample(2, vec![r("CPU", "Temp", 95.0)]), // streak 2 — one below the count
                sample(3, vec![r("CPU", "Temp", 89.0)]), // resets
                sample(4, vec![r("CPU", "Temp", 95.0)]), // streak 1
                sample(5, vec![r("CPU", "Temp", 95.0)]), // streak 2
                sample(6, vec![r("CPU", "Temp", 95.0)]), // streak 3 — fires
            ],
        );
        assert_eq!(states(&out), vec![(6, TransitionState::Fired)]);
        assert_eq!(out[0].1.samples_in_violation, 3);
    }

    #[test]
    fn threshold_hysteresis_band_holds_silently_and_clears_past_clear_level() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            clear = 85.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 91.0)]), // fires (for_samples 1)
                sample(2, vec![r("CPU", "Temp", 87.0)]), // band: holds silently
                sample(3, vec![r("CPU", "Temp", 91.0)]), // still fired, violating again
                sample(4, vec![r("CPU", "Temp", 86.0)]), // band again
                sample(5, vec![r("CPU", "Temp", 84.0)]), // past clear: cleared
            ],
        );
        assert_eq!(
            states(&out),
            vec![(1, TransitionState::Fired), (5, TransitionState::Cleared)]
        );
        // Episode total: the firing sample and the tick-3 violation held the
        // condition; band samples did not.
        assert_eq!(out[1].1.samples_in_violation, 2);
        assert_eq!(out[1].1.value, Some(84.0));
    }

    #[test]
    fn threshold_without_clear_rearms_on_first_non_violating_sample() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 91.0)]),
                sample(2, vec![r("CPU", "Temp", 89.9)]),
            ],
        );
        assert_eq!(
            states(&out),
            vec![(1, TransitionState::Fired), (2, TransitionState::Cleared)]
        );
    }

    #[test]
    fn threshold_refire_requires_a_fresh_streak() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0)]),
                sample(2, vec![r("CPU", "Temp", 95.0)]), // fires
                sample(3, vec![r("CPU", "Temp", 80.0)]), // clears
                sample(4, vec![r("CPU", "Temp", 95.0)]), // streak 1 only
                sample(5, vec![r("CPU", "Temp", 95.0)]), // fires again
            ],
        );
        assert_eq!(
            states(&out),
            vec![
                (2, TransitionState::Fired),
                (3, TransitionState::Cleared),
                (5, TransitionState::Fired),
            ]
        );
    }

    #[test]
    fn threshold_metric_selects_the_compared_field() {
        for (metric, value, min, max, avg) in [
            ("value", 95.0, 0.0, 0.0, 0.0),
            ("min", 0.0, 95.0, 0.0, 0.0),
            ("max", 0.0, 0.0, 95.0, 0.0),
            ("avg", 0.0, 0.0, 0.0, 95.0),
        ] {
            let mut engine = engine(&format!(
                r#"
                [[rules]]
                name = "m"
                kind = "threshold"
                sensor = "cpu"
                metric = "{metric}"
                op = ">"
                threshold = 90.0
                "#
            ));
            let out = run(
                &mut engine,
                vec![sample(
                    1,
                    vec![reading_full(
                        "CPU", "Temp", "Voltage", value, min, max, avg, "V",
                    )],
                )],
            );
            assert_eq!(out.len(), 1, "metric {metric} should fire");
            assert_eq!(out[0].1.value, Some(95.0), "metric {metric}");
        }
    }

    #[test]
    fn threshold_op_boundaries() {
        // (op, threshold, firing value, non-firing value) — the boundary
        // sample pins strict vs inclusive comparison.
        for (op, firing, non_firing) in [
            (">", 90.1, 90.0),
            (">=", 90.0, 89.9),
            ("<", 89.9, 90.0),
            ("<=", 90.0, 90.1),
        ] {
            let mut engine = engine(&format!(
                r#"
                [[rules]]
                name = "b"
                kind = "threshold"
                sensor = "cpu"
                op = "{op}"
                threshold = 90.0
                "#
            ));
            let out = run(
                &mut engine,
                vec![
                    sample(1, vec![r("CPU", "Temp", non_firing)]),
                    sample(2, vec![r("CPU", "Temp", firing)]),
                ],
            );
            assert_eq!(states(&out), vec![(2, TransitionState::Fired)], "op {op}");
        }
    }

    #[test]
    fn threshold_transition_payload_is_complete() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "psu-12v-sag"
            kind = "threshold"
            sensor = "MEG"
            reading = "+12V"
            type = "Voltage"
            op = "<"
            threshold = 11.6
            clear = 11.8
            for_samples = 2
            severity = "critical"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("MEG Ai1600T", "+12V", 11.5)]),
                sample(2, vec![r("MEG Ai1600T", "+12V", 11.4)]),
                sample(3, vec![r("MEG Ai1600T", "+12V", 11.9)]),
            ],
        );
        assert_eq!(out.len(), 2);

        let fired = &out[0].1;
        assert_eq!(fired.rule, "psu-12v-sag");
        assert_eq!(fired.kind, RuleKind::Threshold);
        assert_eq!(fired.severity, Severity::Critical);
        assert_eq!(fired.state, TransitionState::Fired);
        assert_eq!(fired.raw_timestamp, "raw-2");
        assert_eq!(fired.timestamp, tick_time(2).0);
        assert_eq!(fired.sensor.as_deref(), Some("MEG Ai1600T"));
        assert_eq!(fired.reading.as_deref(), Some("+12V"));
        assert_eq!(fired.value, Some(11.4));
        assert_eq!(fired.unit.as_deref(), Some("V"));
        assert_eq!(fired.threshold, Some(11.6));
        assert_eq!(fired.samples_in_violation, 2);

        let cleared = &out[1].1;
        assert_eq!(cleared.state, TransitionState::Cleared);
        assert_eq!(cleared.raw_timestamp, "raw-3");
        assert_eq!(cleared.value, Some(11.9));
        assert_eq!(cleared.samples_in_violation, 2);
    }

    #[test]
    fn threshold_nan_resets_an_unfired_streak() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0)]),
                sample(2, vec![r("CPU", "Temp", f64::NAN)]), // unevaluable: resets
                sample(3, vec![r("CPU", "Temp", 95.0)]),     // streak 1 again
                sample(4, vec![r("CPU", "Temp", 95.0)]),     // fires
            ],
        );
        assert_eq!(states(&out), vec![(4, TransitionState::Fired)]);
    }

    #[test]
    fn threshold_nan_never_clears_a_fired_rule() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            clear = 85.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0)]),     // fires
                sample(2, vec![r("CPU", "Temp", f64::NAN)]), // must NOT clear
                sample(3, vec![r("CPU", "Temp", 84.0)]),     // clears
            ],
        );
        assert_eq!(
            states(&out),
            vec![(1, TransitionState::Fired), (3, TransitionState::Cleared)]
        );
    }

    // ---- rate ----

    #[test]
    fn rate_evaluates_nothing_until_the_window_is_full() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "temp-spike"
            kind = "rate"
            sensor = "psu"
            op = ">"
            threshold = 25.0
            window_samples = 3
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "Temp", 0.0)]),
                sample(2, vec![r("PSU", "Temp", 26.0)]), // huge jump, window not full
                sample(3, vec![r("PSU", "Temp", 26.0)]), // delta vs sample 1 = 26 > 25
            ],
        );
        assert_eq!(states(&out), vec![(3, TransitionState::Fired)]);
        assert_eq!(out[0].1.value, Some(26.0));
        assert_eq!(out[0].1.threshold, Some(25.0));
    }

    #[test]
    fn rate_delta_spans_window_minus_one_intervals() {
        // window_samples = 3: the delta at sample i is v[i] - v[i-2].
        let mut engine = engine(
            r#"
            [[rules]]
            name = "temp-spike"
            kind = "rate"
            sensor = "psu"
            op = ">"
            threshold = 25.0
            window_samples = 3
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "Temp", 10.0)]),
                sample(2, vec![r("PSU", "Temp", 20.0)]),
                sample(3, vec![r("PSU", "Temp", 35.0)]), // 35 - 10 = 25: NOT > 25
                sample(4, vec![r("PSU", "Temp", 46.0)]), // 46 - 20 = 26: fires
                sample(5, vec![r("PSU", "Temp", 40.0)]), // 40 - 35 = 5: clears
            ],
        );
        assert_eq!(
            states(&out),
            vec![(4, TransitionState::Fired), (5, TransitionState::Cleared)]
        );
        assert_eq!(out[0].1.value, Some(26.0));
    }

    #[test]
    fn rate_negative_direction_detects_drops() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "rail-drop"
            kind = "rate"
            sensor = "psu"
            op = "<"
            threshold = -20.0
            window_samples = 3
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 100.0)]),
                sample(2, vec![r("PSU", "+12V", 90.0)]),
                sample(3, vec![r("PSU", "+12V", 75.0)]), // 75 - 100 = -25 < -20
            ],
        );
        assert_eq!(states(&out), vec![(3, TransitionState::Fired)]);
        assert_eq!(out[0].1.value, Some(-25.0));
    }

    #[test]
    fn rate_series_absence_resets_the_window() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "temp-spike"
            kind = "rate"
            sensor = "psu"
            op = ">"
            threshold = 25.0
            window_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "Temp", 0.0)]),
                sample(2, vec![r("Other", "X", 1.0)]), // PSU absent: window drops
                sample(3, vec![r("PSU", "Temp", 100.0)]), // refill only — 100-vs-0 must NOT fire
                sample(4, vec![r("PSU", "Temp", 130.0)]), // 130 - 100 = 30: fires
            ],
        );
        assert_eq!(states(&out), vec![(4, TransitionState::Fired)]);
        assert_eq!(out[0].1.value, Some(30.0));
    }

    #[test]
    fn rate_fired_state_persists_across_absence_and_refill() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "temp-spike"
            kind = "rate"
            sensor = "psu"
            op = ">"
            threshold = 25.0
            clear = 5.0
            window_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "Temp", 0.0)]),
                sample(2, vec![r("PSU", "Temp", 30.0)]), // delta 30: fires
                sample(3, vec![r("Other", "X", 1.0)]),   // absence: window drops, fired holds
                sample(4, vec![r("PSU", "Temp", 100.0)]), // refill: unevaluable, fired holds
                sample(5, vec![r("PSU", "Temp", 107.0)]), // delta 7: band, holds
                sample(6, vec![r("PSU", "Temp", 107.0)]), // delta 0: past clear, clears
            ],
        );
        assert_eq!(
            states(&out),
            vec![(2, TransitionState::Fired), (6, TransitionState::Cleared)]
        );
    }

    // ---- stale ----

    #[test]
    fn stale_fires_on_the_nth_repeat_not_below() {
        // for_samples = 2 repeats — the third identical sample fires.
        let mut engine = engine(
            r#"
            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]), // baseline
                sample(2, vec![r("PSU", "+12V", 12.0)]), // repeat 1
                sample(3, vec![r("PSU", "+12V", 12.0)]), // repeat 2: fires
            ],
        );
        assert_eq!(states(&out), vec![(3, TransitionState::Fired)]);
        assert_eq!(out[0].1.value, Some(12.0));
        assert_eq!(out[0].1.samples_in_violation, 2);
    }

    #[test]
    fn stale_value_repeat_with_moving_average_is_not_stale() {
        // The discriminating case for full-tuple identity: a quantized
        // sensor holds exactly 45.0 while HWiNFO's lifetime avg keeps
        // moving. Value-only comparison would false-fire here.
        let mut engine = engine(
            r#"
            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"
            "#,
        );
        let ticks = (1..=5)
            .map(|i| {
                sample(
                    i,
                    vec![reading_full(
                        "PSU",
                        "Temp",
                        "Temperature",
                        45.0,
                        44.0,
                        47.0,
                        45.0 + (i as f64) * 0.001, // lifetime avg still moving
                        "°C",
                    )],
                )
            })
            .collect();
        let out = run(&mut engine, ticks);
        assert!(out.is_empty(), "moving avg must not read as stale: {out:?}");
    }

    #[test]
    fn stale_repeated_nulls_are_stale() {
        // Replay maps JSON null to the one canonical NaN, so a dead reading
        // emitting nulls is bit-identical sample over sample — exactly the
        // staleness condition. (This is why staleness is bit comparison,
        // not `==`, which is always false for NaN.)
        let mut engine = engine(
            r#"
            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", f64::NAN)]),
                sample(2, vec![r("PSU", "+12V", f64::NAN)]),
            ],
        );
        assert_eq!(states(&out), vec![(2, TransitionState::Fired)]);
        assert!(out[0].1.value.unwrap().is_nan());
    }

    #[test]
    fn stale_clears_and_rebaselines_on_change() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]), // baseline
                sample(2, vec![r("PSU", "+12V", 12.0)]), // repeat: fires
                sample(3, vec![r("PSU", "+12V", 12.1)]), // change: clears, re-baselines
                sample(4, vec![r("PSU", "+12V", 12.1)]), // repeat of the NEW value: fires
            ],
        );
        assert_eq!(
            states(&out),
            vec![
                (2, TransitionState::Fired),
                (3, TransitionState::Cleared),
                (4, TransitionState::Fired),
            ]
        );
        assert_eq!(out[1].1.value, Some(12.1));
    }

    #[test]
    fn stale_baseline_drops_on_absence() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]), // baseline
                sample(2, vec![r("PSU", "+12V", 12.0)]), // repeat 1
                sample(3, vec![r("Other", "X", 1.0)]),   // absent: baseline drops
                sample(4, vec![r("PSU", "+12V", 12.0)]), // re-baseline only
                sample(5, vec![r("PSU", "+12V", 12.0)]), // repeat 1
                sample(6, vec![r("PSU", "+12V", 12.0)]), // repeat 2: fires
            ],
        );
        assert_eq!(states(&out), vec![(6, TransitionState::Fired)]);
    }

    // ---- missing ----

    #[test]
    fn missing_never_seen_never_fires() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "gone"
            kind = "missing"
            reading = "ghost"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]),
                sample(2, vec![r("PSU", "+12V", 12.0)]),
            ],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn missing_fires_after_n_absent_samples_and_clears_on_reappearance() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "gone"
            kind = "missing"
            sensor = "psu"
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]), // seen
                sample(2, vec![r("Other", "X", 1.0)]),   // absent 1
                sample(3, vec![r("Other", "X", 1.0)]),   // absent 2: fires
                sample(4, vec![r("PSU", "+12V", 12.0)]), // reappears: clears
            ],
        );
        assert_eq!(
            states(&out),
            vec![(3, TransitionState::Fired), (4, TransitionState::Cleared)]
        );
        let fired = &out[0].1;
        assert_eq!(fired.sensor.as_deref(), Some("PSU"));
        assert_eq!(fired.reading.as_deref(), Some("+12V"));
        assert_eq!(fired.value, None);
        assert_eq!(fired.unit.as_deref(), Some("V"));
        assert_eq!(fired.threshold, None);
        assert_eq!(out[1].1.samples_in_violation, 2);
    }

    #[test]
    fn missing_unavailable_ticks_are_not_absence() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "gone"
            kind = "missing"
            sensor = "psu"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]),
                unavailable(2), // an outage is not evidence about a reading
                unavailable(3),
                sample(4, vec![r("PSU", "+12V", 12.0)]),
            ],
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn missing_streak_survives_an_outage() {
        // The freeze rule: absent 1, outage, absent 2 — the outage neither
        // counts nor breaks "consecutive".
        let mut engine = engine(
            r#"
            [[rules]]
            name = "gone"
            kind = "missing"
            sensor = "psu"
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU", "+12V", 12.0)]),
                sample(2, vec![r("Other", "X", 1.0)]), // absent 1
                unavailable(3),                        // frozen
                sample(4, vec![r("Other", "X", 1.0)]), // absent 2: fires
            ],
        );
        assert_eq!(states(&out), vec![(4, TransitionState::Fired)]);
    }

    // ---- source-unavailable ----

    #[test]
    fn source_unavailable_debounce_boundary_and_clear() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "hwinfo-gone"
            kind = "source-unavailable"
            for_samples = 3
            severity = "critical"
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                unavailable(1),
                unavailable(2),                          // one below the count
                unavailable(3),                          // fires
                unavailable(4),                          // still down
                sample(5, vec![r("PSU", "+12V", 12.0)]), // clears
            ],
        );
        assert_eq!(
            states(&out),
            vec![(3, TransitionState::Fired), (5, TransitionState::Cleared)]
        );
        let fired = &out[0].1;
        assert_eq!(fired.kind, RuleKind::SourceUnavailable);
        assert_eq!(fired.sensor, None);
        assert_eq!(fired.reading, None);
        assert_eq!(fired.value, None);
        assert_eq!(fired.unit, None);
        assert_eq!(fired.threshold, None);
        assert_eq!(fired.samples_in_violation, 3);
        assert_eq!(out[1].1.samples_in_violation, 4);
    }

    #[test]
    fn source_unavailable_recounts_after_recovery() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "hwinfo-gone"
            kind = "source-unavailable"
            for_samples = 2
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                unavailable(1),
                sample(2, vec![]), // even an empty sample is contact with the source
                unavailable(3),
                unavailable(4), // fires: fresh streak of 2
            ],
        );
        assert_eq!(states(&out), vec![(4, TransitionState::Fired)]);
    }

    // ---- cross-cutting ----

    #[test]
    fn multiple_rules_fire_and_clear_independently() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "critical-level"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            severity = "critical"

            [[rules]]
            name = "warning-level"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 80.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 85.0)]), // warning only
                sample(2, vec![r("CPU", "Temp", 95.0)]), // critical joins
                sample(3, vec![r("CPU", "Temp", 70.0)]), // both clear
            ],
        );
        let summary: Vec<(i64, &str, TransitionState)> = out
            .iter()
            .map(|(i, t)| (*i, t.rule.as_str(), t.state))
            .collect();
        assert_eq!(
            summary,
            vec![
                (1, "warning-level", TransitionState::Fired),
                (2, "critical-level", TransitionState::Fired),
                // Declaration order within the tick.
                (3, "critical-level", TransitionState::Cleared),
                (3, "warning-level", TransitionState::Cleared),
            ]
        );
    }

    #[test]
    fn one_rule_tracks_each_matched_series_independently() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "rail-sag"
            kind = "threshold"
            reading = "+12V"
            op = "<"
            threshold = 11.6
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("PSU A", "+12V", 11.5), r("PSU B", "+12V", 12.0)]),
                sample(2, vec![r("PSU A", "+12V", 11.5), r("PSU B", "+12V", 11.4)]),
            ],
        );
        let summary: Vec<(i64, &str, TransitionState)> = out
            .iter()
            .map(|(i, t)| (*i, t.sensor.as_deref().unwrap(), t.state))
            .collect();
        assert_eq!(
            summary,
            vec![
                (1, "PSU A", TransitionState::Fired),
                (2, "PSU B", TransitionState::Fired),
            ]
        );
    }

    #[test]
    fn unavailable_ticks_freeze_a_threshold_streak() {
        let mut engine = engine(CPU_GT_90_FOR_3);
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0)]), // streak 1
                sample(2, vec![r("CPU", "Temp", 95.0)]), // streak 2
                unavailable(3),                          // frozen, not reset
                sample(4, vec![r("CPU", "Temp", 95.0)]), // streak 3: fires
            ],
        );
        assert_eq!(states(&out), vec![(4, TransitionState::Fired)]);
    }

    #[test]
    fn matchers_apply_case_insensitively_with_exact_type() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "meg-temps"
            kind = "threshold"
            sensor = "meg"
            type = "Temperature"
            op = ">"
            threshold = 50.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![sample(
                1,
                vec![
                    // Matches: case-insensitive substring + exact type.
                    reading_full(
                        "MEG Ai1600T",
                        "PSU Temp",
                        "Temperature",
                        60.0,
                        0.0,
                        70.0,
                        45.0,
                        "°C",
                    ),
                    // Wrong type.
                    reading_full("MEG Ai1600T", "+12V", "Voltage", 60.0, 0.0, 70.0, 45.0, "V"),
                    // Wrong sensor.
                    reading_full(
                        "Corsair",
                        "Water Temp",
                        "Temperature",
                        60.0,
                        0.0,
                        70.0,
                        45.0,
                        "°C",
                    ),
                ],
            )],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.sensor.as_deref(), Some("MEG Ai1600T"));
        assert_eq!(out[0].1.reading.as_deref(), Some("PSU Temp"));
    }

    #[test]
    fn duplicate_series_in_one_sample_first_occurrence_wins() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0), r("CPU", "Temp", 10.0)]),
                sample(2, vec![r("CPU", "Temp", 10.0), r("CPU", "Temp", 95.0)]),
            ],
        );
        assert_eq!(
            states(&out),
            vec![(1, TransitionState::Fired), (2, TransitionState::Cleared)]
        );
        assert_eq!(out[0].1.value, Some(95.0));
        assert_eq!(out[1].1.value, Some(10.0));
    }

    #[test]
    fn identical_streams_produce_identical_transitions() {
        let rules_toml = r#"
            [[rules]]
            name = "cpu-hot"
            kind = "threshold"
            sensor = "cpu"
            op = ">"
            threshold = 90.0
            clear = 85.0
            for_samples = 2

            [[rules]]
            name = "frozen"
            kind = "stale"
            sensor = "psu"

            [[rules]]
            name = "hwinfo-gone"
            kind = "source-unavailable"
            for_samples = 2
        "#;
        let stream = || {
            vec![
                sample(1, vec![r("CPU", "Temp", 95.0), r("PSU", "+12V", 12.0)]),
                unavailable(2),
                sample(3, vec![r("CPU", "Temp", 95.0), r("PSU", "+12V", 12.0)]),
                unavailable(4),
                unavailable(5),
                sample(6, vec![r("CPU", "Temp", 84.0), r("PSU", "+12V", 12.0)]),
                sample(7, vec![r("CPU", "Temp", 84.0)]),
            ]
        };
        let mut a = engine(rules_toml);
        let mut b = engine(rules_toml);
        let out_a: Vec<Transition> = run(&mut a, stream()).into_iter().map(|(_, t)| t).collect();
        let out_b: Vec<Transition> = run(&mut b, stream()).into_iter().map(|(_, t)| t).collect();
        assert!(!out_a.is_empty());
        assert_eq!(out_a, out_b);
    }

    // ---- end to end: rules + engine over replayed mixed-era log files ----

    #[test]
    fn end_to_end_replay_over_mixed_era_log_files() {
        use crate::replay::ReplaySource;
        use crate::source::SampleSource as _;
        use crate::testutil::TempDir;

        // Day 1: written by the Rust logger (LF, 6-digit fractions).
        let day1 = concat!(
            r#"{"timestamp": "2026-02-18T08:00:00.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}]}"#,
            "\n",
            r#"{"timestamp": "2026-02-18T08:00:10.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.5, "min": 11.4, "max": 12.17, "avg": 12.0, "unit": "V"}]}"#,
            "\n",
            r#"{"timestamp": "2026-02-18T08:00:20.000000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.4, "min": 11.4, "max": 12.17, "avg": 11.9, "unit": "V"}]}"#,
            "\n",
        );
        // Day 2: written by the frozen Python logger — CRLF, a corrupt
        // line, a bare NaN token, an `unknown(35)` type label, and a
        // zero-microsecond (fraction-less) pendulum timestamp.
        let day2 = concat!(
            "corrupted by a partial write\r\n",
            r#"{"timestamp": "2026-02-19T08:00:00.500000-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.9, "min": 11.4, "max": 12.17, "avg": 11.9, "unit": "V"}, {"sensor": "Mystery", "reading": "Odd", "type": "unknown(35)", "value": NaN, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": ""}]}"#,
            "\r\n",
            r#"{"timestamp": "2026-02-19T08:00:10-05:00", "sensors": [{"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 11.9, "min": 11.4, "max": 12.17, "avg": 11.9, "unit": "V"}, {"sensor": "Mystery", "reading": "Odd", "type": "unknown(35)", "value": NaN, "min": 0.0, "max": 0.0, "avg": 0.0, "unit": ""}]}"#,
            "\r\n",
        );
        let dir = TempDir::new();
        let path1 = dir.path().join("sensors_2026-02-18.jsonl");
        let path2 = dir.path().join("sensors_2026-02-19.jsonl");
        std::fs::write(&path1, day1).unwrap();
        std::fs::write(&path2, day2).unwrap();

        let mut engine = engine(
            r#"
            [[rules]]
            name = "psu-12v-sag"
            kind = "threshold"
            reading = "+12V"
            op = "<"
            threshold = 11.6
            clear = 11.8
            for_samples = 2
            severity = "critical"

            [[rules]]
            name = "odd-frozen"
            kind = "stale"
            type = "unknown"
            "#,
        );
        let mut source = ReplaySource::from_files(vec![path1, path2]);
        let mut transitions = Vec::new();
        while let Some(tick) = source.next_tick() {
            transitions.extend(engine.observe(&tick));
        }

        assert_eq!(source.skipped_lines(), 1, "exactly the corrupt line");
        let summary: Vec<(&str, TransitionState, &str)> = transitions
            .iter()
            .map(|t| (t.rule.as_str(), t.state, t.raw_timestamp.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                // The sag debounces across day 1 and fires on its second
                // consecutive violating sample.
                (
                    "psu-12v-sag",
                    TransitionState::Fired,
                    "2026-02-18T08:00:20.000000-05:00"
                ),
                // Day 2 opens recovered past the clear level.
                (
                    "psu-12v-sag",
                    TransitionState::Cleared,
                    "2026-02-19T08:00:00.500000-05:00"
                ),
                // The NaN-valued unknown reading repeats bit-identically on
                // the fraction-less pendulum timestamp: stale.
                (
                    "odd-frozen",
                    TransitionState::Fired,
                    "2026-02-19T08:00:10-05:00"
                ),
            ]
        );
        assert_eq!(transitions[0].value, Some(11.4));
        assert_eq!(transitions[0].samples_in_violation, 2);
        assert_eq!(transitions[1].value, Some(11.9));
        assert!(transitions[2].value.unwrap().is_nan());
    }

    #[test]
    fn transition_order_is_rule_declaration_then_sample_order() {
        let mut engine = engine(
            r#"
            [[rules]]
            name = "second-declared-matches-first" # name order != declaration order
            kind = "threshold"
            sensor = "a"
            op = ">"
            threshold = 90.0

            [[rules]]
            name = "a-rule"
            kind = "threshold"
            sensor = "a"
            op = ">"
            threshold = 80.0
            "#,
        );
        let out = run(
            &mut engine,
            vec![sample(1, vec![r("A", "R1", 95.0), r("A", "R2", 95.0)])],
        );
        let summary: Vec<(&str, &str)> = out
            .iter()
            .map(|(_, t)| (t.rule.as_str(), t.reading.as_deref().unwrap()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("second-declared-matches-first", "R1"),
                ("second-declared-matches-first", "R2"),
                ("a-rule", "R1"),
                ("a-rule", "R2"),
            ]
        );
    }
}

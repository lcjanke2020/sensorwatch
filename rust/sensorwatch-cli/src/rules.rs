//! Declarative alert rules: the OPTIONAL `[[rules]]` array in `config.toml`.
//!
//! Parsing here is STRICT — the opposite contract from `config.rs`'s lenient
//! warn-and-fall-back walking. That leniency is Python parity and applies to
//! `[general]`/`[sensors]` only; a silently discarded (or silently defaulted)
//! alert rule is worse than a crash at startup, so any invalid rule entry is
//! a hard error. The two parsers deliberately read the same document: the
//! future `watch` command runs both over one file, and `log` keeps ignoring
//! `rules` by construction (its `section()` walker never looks at the key).
//!
//! "No rules section" is `Ok` and empty — distinguishable from `Err`, so
//! `watch` can treat zero configured rules as its own usage error while old
//! configs keep loading unchanged everywhere else.
//!
//! Parsing is serde-derive rather than dynamic `toml::Table` walking because
//! strictness is what derive gives for free (`deny_unknown_fields`, typed
//! fields, enum vocabularies) and `toml::from_str` errors carry line/column
//! spans a startup error should point at. Semantic validation then runs as a
//! second pass that AGGREGATES every violation — an author fixing a rules
//! file wants the full list once, not one error per attempt.

use serde::Deserialize;

use crate::labels::CANONICAL_LABELS;

/// A parsed, validated rule set.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// Strictly parse the `rules` array out of a config document (the same
    /// text `Config::from_toml_str` reads leniently). Absent `rules` key —
    /// including an empty or config-only document — is `Ok` and empty.
    pub(crate) fn from_toml_str(text: &str) -> Result<RuleSet, RulesError> {
        let doc: RulesDoc = toml::from_str(text).map_err(RulesError::Parse)?;
        let raw = doc.rules.unwrap_or_default();

        let mut errors = Vec::new();
        let mut rules = Vec::with_capacity(raw.len());
        for (index, entry) in raw.iter().enumerate() {
            match validate(entry, index) {
                Ok(rule) => rules.push(rule),
                Err(mut entry_errors) => errors.append(&mut entry_errors),
            }
        }
        // Duplicate detection runs over the validated names so its message
        // never doubles up with the empty-name error.
        for (i, rule) in rules.iter().enumerate() {
            if rules[..i].iter().any(|r| r.name == rule.name) {
                errors.push(format!("rule '{}': duplicate rule name", rule.name));
            }
        }

        if errors.is_empty() {
            Ok(RuleSet { rules })
        } else {
            Err(RulesError::Invalid(errors))
        }
    }

    pub(crate) fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub(crate) fn into_rules(self) -> Vec<Rule> {
        self.rules
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// A validated rule, defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    /// Unique (trimmed) rule name; the future event id derives from it.
    pub name: String,
    pub kind: RuleKind,
    pub matcher: Matcher,
    /// Which per-reading field feeds the comparison (threshold/rate only).
    pub metric: Metric,
    /// Present iff `kind` is threshold or rate.
    pub op: Option<Op>,
    /// Present iff `kind` is threshold or rate.
    pub threshold: Option<f64>,
    /// Hysteresis re-arm level; `None` means "clears at `threshold`".
    pub clear: Option<f64>,
    /// Consecutive-samples debounce: fire on the Nth violating sample.
    pub for_samples: u32,
    /// Trailing window size in samples, including the current one
    /// (rate only): the delta spans `window_samples - 1` intervals.
    pub window_samples: Option<u32>,
    pub severity: Severity,
}

/// Rule matchers, with the same semantics as the CLI's snapshot filters:
/// case-insensitive substring on the sensor and reading names
/// (independently), exact match on the canonical type label. All present
/// fields must match (AND); an absent field matches everything.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Matcher {
    /// Pre-lowercased, like `SensorFilter`'s patterns.
    sensor: Option<String>,
    reading: Option<String>,
    kind: Option<&'static str>,
}

impl Matcher {
    /// Match one reading. Takes pre-lowered names so the engine lowercases
    /// each reading once per tick, not once per rule.
    pub(crate) fn matches(
        &self,
        sensor_lower: &str,
        reading_lower: &str,
        kind: &'static str,
    ) -> bool {
        self.sensor
            .as_deref()
            .is_none_or(|pat| sensor_lower.contains(pat))
            && self
                .reading
                .as_deref()
                .is_none_or(|pat| reading_lower.contains(pat))
            && self.kind.is_none_or(|label| label == kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RuleKind {
    Threshold,
    Rate,
    Stale,
    Missing,
    /// TOML value `source-unavailable` — kebab-case, matching the event
    /// `kind` vocabulary the `watch` command emits.
    SourceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    Info,
    Warning,
    Critical,
}

/// The per-reading field a comparison reads — the JSONL key vocabulary.
/// `min`/`max`/`avg` are HWiNFO's source-lifetime aggregates, carried on
/// every reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Metric {
    Value,
    Min,
    Max,
    Avg,
}

/// Comparison operators, spelled as symbols in the config. `==`/`!=` are
/// deliberately absent: float equality thresholds are a footgun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum Op {
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
}

impl Op {
    /// `lhs op rhs`. Any comparison involving NaN is false, which the engine
    /// treats explicitly (an unevaluable sample, not a verdict).
    pub(crate) fn compare(self, lhs: f64, rhs: f64) -> bool {
        match self {
            Op::Gt => lhs > rhs,
            Op::Ge => lhs >= rhs,
            Op::Lt => lhs < rhs,
            Op::Le => lhs <= rhs,
        }
    }
}

pub(crate) enum RulesError {
    /// Malformed TOML, wrong types, unknown keys or enum values — the
    /// underlying error carries a line/column span into the document.
    Parse(toml::de::Error),
    /// Every semantic violation found, one message per line.
    Invalid(Vec<String>),
}

impl std::fmt::Display for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RulesError::Parse(err) => write!(f, "invalid rules config: {err}"),
            RulesError::Invalid(errors) => {
                write!(f, "invalid rules config:")?;
                for error in errors {
                    write!(f, "\n  {error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::fmt::Debug for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for RulesError {}

/// The raw document: only the `rules` key, so `[general]`/`[sensors]` (and
/// any future lenient sections) pass through untouched.
#[derive(Deserialize)]
struct RulesDoc {
    rules: Option<Vec<RawRule>>,
}

/// One `[[rules]]` entry as written. `deny_unknown_fields` makes a typo'd
/// key a hard, spanned parse error instead of a silently inert setting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    name: Option<String>,
    kind: Option<RuleKind>,
    sensor: Option<String>,
    reading: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
    metric: Option<Metric>,
    op: Option<Op>,
    threshold: Option<f64>,
    clear: Option<f64>,
    for_samples: Option<u32>,
    window_samples: Option<u32>,
    severity: Option<Severity>,
}

/// Bounds for the sample counters. The upper bound caps per-series state
/// (the rate ring buffer in particular) — rule configs are parsed external
/// input, per SECURITY.md.
const MAX_SAMPLES: u32 = 1_000_000;

/// Validate one entry, returning the validated rule or EVERY problem found
/// with it. `index` names the entry when its own name is unusable.
fn validate(raw: &RawRule, index: usize) -> Result<Rule, Vec<String>> {
    let mut errors = Vec::new();

    let name = raw.name.as_deref().map(str::trim).unwrap_or_default();
    let label = if name.is_empty() {
        format!("rules[{index}]")
    } else {
        format!("rule '{name}'")
    };
    if raw.name.is_none() {
        errors.push(format!("{label}: missing required field 'name'"));
    } else if name.is_empty() {
        errors.push(format!("{label}: 'name' must not be empty"));
    }

    let Some(kind) = raw.kind else {
        errors.push(format!("{label}: missing required field 'kind'"));
        return Err(errors);
    };
    let value_based = matches!(kind, RuleKind::Threshold | RuleKind::Rate);

    // Matchers. threshold/rate compare values against one number, so a rule
    // matching everything would compare across mixed units — always a config
    // bug. stale/missing are unit-agnostic and may be matcher-less ("any
    // previously-seen reading vanishes/freezes"). source-unavailable is
    // about the source, not a reading: configured matchers would be silently
    // meaningless, and strict parsing says so instead.
    let sensor = matcher_pattern(&raw.sensor, "sensor", &label, &mut errors);
    let reading = matcher_pattern(&raw.reading, "reading", &label, &mut errors);
    let type_matcher = match raw.type_.as_deref() {
        None => None,
        Some(raw_label) => match parse_type_label(raw_label) {
            Some(canonical) => Some(canonical),
            None => {
                errors.push(format!(
                    "{label}: 'type' must be one of {}, or 'unknown' (got '{raw_label}')",
                    CANONICAL_LABELS.join(", "),
                ));
                None
            }
        },
    };
    match kind {
        RuleKind::Threshold | RuleKind::Rate => {
            // Keyed off what was WRITTEN, not what validated: a present-but-
            // invalid matcher already produced its own error above.
            if raw.sensor.is_none() && raw.reading.is_none() && raw.type_.is_none() {
                errors.push(format!(
                    "{label}: {} rules require at least one of 'sensor', 'reading', or 'type' \
                     (matching every reading would compare across mixed units)",
                    kind_name(kind),
                ));
            }
        }
        RuleKind::Stale | RuleKind::Missing => {}
        RuleKind::SourceUnavailable => {
            if raw.sensor.is_some() || raw.reading.is_some() || raw.type_.is_some() {
                errors.push(format!(
                    "{label}: source-unavailable rules do not take 'sensor', 'reading', or \
                     'type' (the whole source is gone, not one reading)"
                ));
            }
        }
    }

    // Comparison fields: required for the value-based kinds, forbidden
    // elsewhere (an inert `threshold` on a stale rule is a config bug).
    if value_based {
        if raw.op.is_none() {
            errors.push(format!(
                "{label}: {} rules require 'op' (one of \">\", \">=\", \"<\", \"<=\")",
                kind_name(kind),
            ));
        }
        if raw.threshold.is_none() {
            errors.push(format!(
                "{label}: {} rules require 'threshold'",
                kind_name(kind)
            ));
        }
    } else {
        for (present, field) in [
            (raw.op.is_some(), "op"),
            (raw.threshold.is_some(), "threshold"),
            (raw.clear.is_some(), "clear"),
            (raw.metric.is_some(), "metric"),
        ] {
            if present {
                errors.push(format!(
                    "{label}: {} rules do not take '{field}'",
                    kind_name(kind)
                ));
            }
        }
    }

    for (value, field) in [(raw.threshold, "threshold"), (raw.clear, "clear")] {
        if let Some(v) = value {
            if !v.is_finite() {
                errors.push(format!("{label}: '{field}' must be finite (got {v})"));
            }
        }
    }

    // Hysteresis must sit on the recovery side of the threshold; equality is
    // allowed and degenerates to exactly "no hysteresis". This ordering also
    // guarantees a clearing sample can never simultaneously be a violation.
    if let (Some(op), Some(threshold), Some(clear)) = (raw.op, raw.threshold, raw.clear) {
        if threshold.is_finite() && clear.is_finite() {
            let ok = match op {
                Op::Gt | Op::Ge => clear <= threshold,
                Op::Lt | Op::Le => clear >= threshold,
            };
            if !ok {
                errors.push(format!(
                    "{label}: 'clear' must be on the recovery side of 'threshold' \
                     (op '{}' requires clear {} threshold)",
                    op_symbol(op),
                    match op {
                        Op::Gt | Op::Ge => "<=",
                        Op::Lt | Op::Le => ">=",
                    },
                ));
            }
        }
    }

    let for_samples = raw.for_samples.unwrap_or(1);
    if !(1..=MAX_SAMPLES).contains(&for_samples) {
        errors.push(format!(
            "{label}: 'for_samples' must be between 1 and {MAX_SAMPLES} (got {for_samples})"
        ));
    }

    match (kind, raw.window_samples) {
        (RuleKind::Rate, None) => {
            errors.push(format!("{label}: rate rules require 'window_samples'"));
        }
        // The window includes the current sample, so 1 would make the delta
        // identically zero.
        (RuleKind::Rate, Some(w)) if !(2..=MAX_SAMPLES).contains(&w) => {
            errors.push(format!(
                "{label}: 'window_samples' must be between 2 and {MAX_SAMPLES} (got {w})"
            ));
        }
        (RuleKind::Rate, Some(_)) => {}
        (_, Some(_)) => {
            errors.push(format!(
                "{label}: only rate rules take 'window_samples' ({} rules are \
                 per-sample)",
                kind_name(kind),
            ));
        }
        (_, None) => {}
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Rule {
        name: name.to_owned(),
        kind,
        matcher: Matcher {
            sensor,
            reading,
            kind: type_matcher,
        },
        metric: raw.metric.unwrap_or(Metric::Value),
        op: raw.op,
        threshold: raw.threshold,
        clear: raw.clear,
        for_samples,
        window_samples: raw.window_samples,
        severity: raw.severity.unwrap_or(Severity::Warning),
    })
}

/// Trim and pre-lowercase a name matcher; a present-but-blank pattern is an
/// error (it would silently match everything).
fn matcher_pattern(
    raw: &Option<String>,
    field: &str,
    label: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    let raw = raw.as_deref()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        errors.push(format!("{label}: '{field}' must not be empty"));
        return None;
    }
    Some(trimmed.to_lowercase())
}

/// The rule-config `type` vocabulary: the nine canonical labels plus
/// `unknown`, case-insensitive. Unlike `normalize_type_label` (which folds
/// stream garbage), an unrecognized config value is a validation error.
fn parse_type_label(raw: &str) -> Option<&'static str> {
    if raw.eq_ignore_ascii_case("unknown") {
        return Some("unknown");
    }
    CANONICAL_LABELS
        .iter()
        .find(|label| raw.eq_ignore_ascii_case(label))
        .copied()
}

fn kind_name(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Threshold => "threshold",
        RuleKind::Rate => "rate",
        RuleKind::Stale => "stale",
        RuleKind::Missing => "missing",
        RuleKind::SourceUnavailable => "source-unavailable",
    }
}

fn op_symbol(op: Op) -> &'static str {
    match op {
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<RuleSet, RulesError> {
        RuleSet::from_toml_str(text)
    }

    fn parse_one(text: &str) -> Rule {
        let set = parse(text).expect("rules parse");
        assert_eq!(set.rules().len(), 1);
        set.rules()[0].clone()
    }

    /// The messages of an expected `Invalid` result.
    fn invalid(text: &str) -> Vec<String> {
        match parse(text) {
            Err(RulesError::Invalid(errors)) => errors,
            Err(RulesError::Parse(err)) => panic!("expected semantic errors, got parse: {err}"),
            Ok(_) => panic!("expected semantic errors, got Ok"),
        }
    }

    fn assert_parse_error(text: &str, expected_fragment: &str) {
        match parse(text) {
            Err(RulesError::Parse(err)) => {
                let message = err.to_string();
                assert!(
                    message.contains(expected_fragment),
                    "parse error should mention {expected_fragment:?}: {message}"
                );
            }
            Err(RulesError::Invalid(errors)) => {
                panic!("expected parse error, got semantic: {errors:?}")
            }
            Ok(_) => panic!("expected parse error, got Ok"),
        }
    }

    const THRESHOLD_RULE: &str = r#"
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
        for_samples = 3
        severity = "critical"
    "#;

    // ---- happy paths ----

    #[test]
    fn threshold_rule_parses_fully() {
        let rule = parse_one(THRESHOLD_RULE);
        assert_eq!(rule.name, "psu-12v-sag");
        assert_eq!(rule.kind, RuleKind::Threshold);
        assert_eq!(rule.metric, Metric::Value);
        assert_eq!(rule.op, Some(Op::Lt));
        assert_eq!(rule.threshold, Some(11.6));
        assert_eq!(rule.clear, Some(11.8));
        assert_eq!(rule.for_samples, 3);
        assert_eq!(rule.window_samples, None);
        assert_eq!(rule.severity, Severity::Critical);
        assert!(rule.matcher.matches("meg ai1600t", "+12v", "Voltage"));
        assert!(!rule.matcher.matches("corsair rm850", "+12v", "Voltage"));
        assert!(!rule.matcher.matches("meg ai1600t", "+5v", "Voltage"));
        assert!(!rule.matcher.matches("meg ai1600t", "+12v", "Current"));
    }

    #[test]
    fn rate_rule_parses() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "psu-temp-spike"
            kind = "rate"
            reading = "PSU Temp"
            op = ">"
            threshold = 15.0
            window_samples = 6
            "#,
        );
        assert_eq!(rule.kind, RuleKind::Rate);
        assert_eq!(rule.window_samples, Some(6));
    }

    #[test]
    fn stale_and_missing_rules_may_be_matcherless() {
        // "Any previously-seen reading freezes/vanishes" is a legitimate,
        // unit-agnostic rule — unlike a matcher-less threshold.
        let text = r#"
            [[rules]]
            name = "anything-stale"
            kind = "stale"
            for_samples = 10

            [[rules]]
            name = "anything-missing"
            kind = "missing"
            for_samples = 5
        "#;
        let set = parse(text).expect("rules parse");
        assert_eq!(set.rules().len(), 2);
        assert!(set.rules()[0]
            .matcher
            .matches("any sensor", "any reading", "Other"));
    }

    #[test]
    fn source_unavailable_rule_parses_without_matchers() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "hwinfo-gone"
            kind = "source-unavailable"
            for_samples = 3
            severity = "critical"
            "#,
        );
        assert_eq!(rule.kind, RuleKind::SourceUnavailable);
        assert_eq!(rule.for_samples, 3);
    }

    #[test]
    fn all_ops_parse() {
        for (symbol, op) in [(">", Op::Gt), (">=", Op::Ge), ("<", Op::Lt), ("<=", Op::Le)] {
            let rule = parse_one(&format!(
                r#"
                [[rules]]
                name = "r"
                kind = "threshold"
                sensor = "s"
                op = "{symbol}"
                threshold = 1.0
                "#
            ));
            assert_eq!(rule.op, Some(op), "op {symbol}");
        }
    }

    #[test]
    fn defaults_are_value_one_sample_warning_no_clear() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "gpu-hot"
            kind = "threshold"
            type = "Temperature"
            op = ">"
            threshold = 90.0
            "#,
        );
        assert_eq!(rule.metric, Metric::Value);
        assert_eq!(rule.for_samples, 1);
        assert_eq!(rule.severity, Severity::Warning);
        assert_eq!(rule.clear, None);
    }

    #[test]
    fn type_matcher_is_case_insensitive_and_stored_canonical() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "any-voltage"
            kind = "threshold"
            type = "voltage"
            op = "<"
            threshold = 11.0
            "#,
        );
        assert!(rule.matcher.matches("s", "r", "Voltage"));
        assert!(!rule.matcher.matches("s", "r", "Current"));
    }

    #[test]
    fn unknown_type_matcher_matches_folded_labels() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "odd-readings"
            kind = "threshold"
            type = "unknown"
            op = ">"
            threshold = 0.0
            "#,
        );
        // Python-era "unknown(35)" entries fold to "unknown" on the sample
        // side, so the matcher sees the canonical label.
        assert!(rule.matcher.matches("s", "r", "unknown"));
        assert!(!rule.matcher.matches("s", "r", "Other"));
    }

    #[test]
    fn name_matchers_are_trimmed_and_lowercased() {
        let rule = parse_one(
            r#"
            [[rules]]
            name = "r"
            kind = "threshold"
            sensor = "  MEG Ai1600T  "
            op = "<"
            threshold = 1.0
            "#,
        );
        assert!(rule.matcher.matches("meg ai1600t psu", "anything", "Other"));
    }

    #[test]
    fn clear_equal_to_threshold_is_allowed() {
        // Degenerates to exactly "no hysteresis" by the engine's
        // clear_level = clear.unwrap_or(threshold) definition.
        let rule = parse_one(
            r#"
            [[rules]]
            name = "r"
            kind = "threshold"
            sensor = "s"
            op = ">"
            threshold = 90.0
            clear = 90.0
            "#,
        );
        assert_eq!(rule.clear, Some(90.0));
    }

    // ---- absent / empty documents ----

    #[test]
    fn absent_rules_key_is_ok_and_empty() {
        for text in [
            "",
            "[general]\ninterval_seconds = 5\n",
            "[sensors]\ninclude = []\n",
        ] {
            let set = parse(text).expect("no rules section is fine");
            assert!(set.is_empty(), "for {text:?}");
        }
    }

    #[test]
    fn empty_rules_array_is_ok_and_empty() {
        assert!(parse("rules = []\n").expect("empty array").is_empty());
    }

    #[test]
    fn coexists_with_the_lenient_config_sections() {
        // The shipped config.toml shape plus a rules block: the strict path
        // reads `rules`, the lenient path keeps reading its own sections
        // from the very same text, each blind to the other's keys.
        let text = r#"
            [general]
            interval_seconds = 5
            log_dir = "custom_logs"
            retention_days = 7

            [sensors]
            include = ["MEG"]
            exclude = ["Virtual"]

            [[rules]]
            name = "psu-12v-sag"
            kind = "threshold"
            reading = "+12V"
            op = "<"
            threshold = 11.6
        "#;
        let set = parse(text).expect("rules parse alongside config sections");
        assert_eq!(set.rules().len(), 1);
    }

    // ---- parse (spanned) errors ----

    #[test]
    fn malformed_toml_is_a_parse_error() {
        assert!(matches!(
            parse("this is not [ valid toml"),
            Err(RulesError::Parse(_))
        ));
    }

    #[test]
    fn unknown_rule_field_is_a_parse_error() {
        // deny_unknown_fields: a typo'd key must never be silently inert.
        assert_parse_error(
            r#"
            [[rules]]
            name = "r"
            kind = "threshold"
            sensor = "s"
            op = ">"
            treshold = 90.0
            "#,
            "treshold",
        );
    }

    #[test]
    fn unknown_enum_values_are_parse_errors() {
        assert_parse_error("[[rules]]\nname = \"r\"\nkind = \"treshold\"\n", "treshold");
        assert_parse_error(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \"=>\"\nthreshold = 1.0\n",
            "=>",
        );
        assert_parse_error(
            "[[rules]]\nname = \"r\"\nkind = \"stale\"\nseverity = \"fatal\"\n",
            "fatal",
        );
        assert_parse_error(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \">\"\nthreshold = 1.0\nmetric = \"minimum\"\n",
            "minimum",
        );
    }

    #[test]
    fn wrong_field_types_are_parse_errors() {
        assert!(matches!(
            parse("[[rules]]\nname = 5\nkind = \"stale\"\n"),
            Err(RulesError::Parse(_))
        ));
        assert!(matches!(
            parse("rules = \"not an array\"\n"),
            Err(RulesError::Parse(_))
        ));
        // u32 rejects negatives with a spanned error.
        assert!(matches!(
            parse("[[rules]]\nname = \"r\"\nkind = \"stale\"\nfor_samples = -1\n"),
            Err(RulesError::Parse(_))
        ));
    }

    // ---- semantic (aggregated) errors ----

    #[test]
    fn missing_name_and_kind_are_reported() {
        let errors = invalid("[[rules]]\nkind = \"stale\"\n");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("rules[0]"), "{errors:?}");
        assert!(errors[0].contains("'name'"), "{errors:?}");

        let errors = invalid("[[rules]]\nname = \"r\"\n");
        assert!(errors[0].contains("'kind'"), "{errors:?}");
    }

    #[test]
    fn blank_name_is_reported_by_index() {
        let errors = invalid("[[rules]]\nname = \"  \"\nkind = \"stale\"\n");
        assert!(errors[0].starts_with("rules[0]:"), "{errors:?}");
    }

    #[test]
    fn duplicate_names_are_reported() {
        let errors = invalid(
            r#"
            [[rules]]
            name = "same"
            kind = "stale"

            [[rules]]
            name = "same"
            kind = "missing"
            "#,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("duplicate rule name"), "{errors:?}");
    }

    #[test]
    fn threshold_without_matchers_is_rejected() {
        let errors =
            invalid("[[rules]]\nname = \"r\"\nkind = \"threshold\"\nop = \">\"\nthreshold = 1.0\n");
        assert!(errors[0].contains("at least one of"), "{errors:?}");
    }

    #[test]
    fn source_unavailable_with_matchers_is_rejected() {
        let errors =
            invalid("[[rules]]\nname = \"r\"\nkind = \"source-unavailable\"\nsensor = \"MEG\"\n");
        assert!(errors[0].contains("do not take"), "{errors:?}");
    }

    #[test]
    fn bad_type_label_is_rejected_with_the_vocabulary() {
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\ntype = \"Volts\"\nop = \">\"\nthreshold = 1.0\n",
        );
        assert!(errors[0].contains("Voltage"), "{errors:?}");
        assert!(errors[0].contains("'Volts'"), "{errors:?}");
    }

    #[test]
    fn value_kinds_require_op_and_threshold() {
        let errors = invalid("[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\n");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("'op'")), "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("'threshold'")),
            "{errors:?}"
        );
    }

    #[test]
    fn non_value_kinds_reject_comparison_fields() {
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"stale\"\nop = \">\"\nthreshold = 1.0\nclear = 0.5\nmetric = \"avg\"\n",
        );
        assert_eq!(errors.len(), 4, "{errors:?}");
        for field in ["'op'", "'threshold'", "'clear'", "'metric'"] {
            assert!(errors.iter().any(|e| e.contains(field)), "{errors:?}");
        }
    }

    #[test]
    fn non_finite_threshold_and_clear_are_rejected() {
        // TOML happily parses `inf` and `nan` float literals.
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \">\"\nthreshold = inf\n",
        );
        assert!(errors[0].contains("finite"), "{errors:?}");
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \">\"\nthreshold = 1.0\nclear = nan\n",
        );
        assert!(errors[0].contains("finite"), "{errors:?}");
    }

    #[test]
    fn clear_on_the_wrong_side_is_rejected_for_both_directions() {
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \">\"\nthreshold = 90.0\nclear = 95.0\n",
        );
        assert!(errors[0].contains("recovery side"), "{errors:?}");
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \"<\"\nthreshold = 11.0\nclear = 10.5\n",
        );
        assert!(errors[0].contains("recovery side"), "{errors:?}");
    }

    #[test]
    fn for_samples_zero_is_rejected() {
        let errors = invalid("[[rules]]\nname = \"r\"\nkind = \"stale\"\nfor_samples = 0\n");
        assert!(errors[0].contains("'for_samples'"), "{errors:?}");
    }

    #[test]
    fn window_samples_rules() {
        // Required for rate.
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"rate\"\nsensor = \"s\"\nop = \">\"\nthreshold = 1.0\n",
        );
        assert!(errors[0].contains("'window_samples'"), "{errors:?}");
        // Minimum 2: the window includes the current sample.
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"rate\"\nsensor = \"s\"\nop = \">\"\nthreshold = 1.0\nwindow_samples = 1\n",
        );
        assert!(errors[0].contains("between 2"), "{errors:?}");
        // Forbidden elsewhere.
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\nop = \">\"\nthreshold = 1.0\nwindow_samples = 5\n",
        );
        assert!(errors[0].contains("only rate rules"), "{errors:?}");
    }

    #[test]
    fn blank_matcher_pattern_is_rejected() {
        let errors = invalid(
            "[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"  \"\nop = \">\"\nthreshold = 1.0\n",
        );
        assert!(errors.iter().any(|e| e.contains("'sensor'")), "{errors:?}");
    }

    #[test]
    fn errors_aggregate_across_rules() {
        let errors = invalid(
            r#"
            [[rules]]
            name = "a"
            kind = "threshold"
            sensor = "s"
            op = ">"
            threshold = inf

            [[rules]]
            name = "b"
            kind = "rate"
            sensor = "s"
            op = ">"
            threshold = 1.0
            "#,
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("rule 'a'"), "{errors:?}");
        assert!(errors[1].contains("rule 'b'"), "{errors:?}");
    }

    #[test]
    fn display_lists_every_message() {
        let err =
            parse("[[rules]]\nname = \"r\"\nkind = \"threshold\"\nsensor = \"s\"\n").unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("invalid rules config:"), "{rendered}");
        assert!(rendered.contains("'op'"), "{rendered}");
        assert!(rendered.contains("'threshold'"), "{rendered}");
    }
}

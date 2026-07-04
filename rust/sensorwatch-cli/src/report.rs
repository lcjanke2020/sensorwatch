//! The `report` subcommand: turn logged JSONL history into a bounded JSON
//! digest an agent can read on a small context budget.
//!
//! **Protocol role.** The agent monitoring protocol forbids agents from ever
//! reading the raw `sensors_*.jsonl` logs directly; `report` is the sanctioned
//! alternative — one call yields per-series window aggregates, re-derived rule
//! violations, sampling gaps, and a meta block (sample counts plus first/last
//! timestamps) that doubles as a liveness check. A zero-sample digest is not an
//! error: it is the "logger is dead / machine was off" signal, so `report`
//! still exits 0 and prints it.
//!
//! **Why lifetime aggregates are ignored.** Each logged reading carries
//! HWiNFO's own `min`/`max`/`avg`, but those are extremes over HWiNFO's
//! *session* lifetime, not the report window; reporting them would answer the
//! wrong question. `report` folds fresh aggregates over the window's own
//! samples instead (see [`crate::digest`]).
//!
//! **Fresh-engine caveat.** Violations are re-derived by replaying the window's
//! samples through a *fresh* [`Engine`]. A live watcher whose `for_samples`
//! debounce lead-in began *before* the window opened may therefore have fired
//! on a sample this digest never sees, so a windowed violation set can differ
//! from what the live watcher emitted at the window's leading edge. Widen the
//! window to capture the lead-in when this matters.
//!
//! **Read-only guarantee.** `report` never writes state — in particular it
//! never touches `watch.seq`. The digest-local `seq`/`id` on each violation are
//! per-run ordinals (1..n in transition order) for human reference only; they
//! are NOT the `watch` ack cursor and must never be fed to one.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;

use jiff::{SignedDuration, Zoned};

use crate::cli::{ReportArgs, TypeFilter};
use crate::config::Config;
use crate::digest::{self, parse_duration_secs, parse_when, Aggregator, DayEdge, Emit};
use crate::engine::Engine;
use crate::exit;
use crate::replay::ReplaySource;
use crate::rules::RuleSet;
use crate::source::{SampleSource, Tick};

/// At most this many rule transitions are retained for the digest (oldest
/// dropped); the byte-cap fitter drops further from this set. Real windows
/// produce far fewer.
const VIOLATION_CAP: usize = 512;

pub(crate) fn run(args: &ReportArgs) -> ExitCode {
    // Config + rules. `report` reads the document once and feeds both parsers,
    // exactly like `watch`; but with no config at all it proceeds over zero
    // rules rather than erroring (a digest is still useful without alerts).
    let (rules, config) = match Config::config_path(args.config.as_deref()) {
        Some(path) => {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    // `config_path` only returns existing paths, so this is an
                    // I/O fault on a present file — a fatal preparation failure,
                    // not a usage error.
                    eprintln!(
                        "sensorwatch report: could not read config {}: {err}",
                        path.display()
                    );
                    return ExitCode::from(exit::FATAL);
                }
            };
            let rules = match RuleSet::from_toml_str(&text) {
                Ok(rules) => rules,
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::from(exit::USAGE);
                }
            };
            let config = Config::from_toml_str(&text).unwrap_or_default();
            (rules, config)
        }
        None => {
            // No config resolved, so gap detection runs on the DEFAULT cadence.
            // Reading another machine's logs (the cross-platform "pure file
            // reading" flow) without its config silently uses a 10 s interval —
            // warn, because a logger that actually sampled slower would report
            // spurious gaps. `report` is stdout-first, so this stays on stderr.
            let config = Config::default();
            log::warn!(
                "no config found; gap detection uses the default interval_seconds={} — pass \
                 --config to match the logger's real cadence",
                config.interval_seconds
            );
            (
                RuleSet::from_toml_str("").expect("an empty document is an empty rule set"),
                config,
            )
        }
    };

    // Resolve the log directory (override wins). A missing or non-directory
    // path is not fatal: candidate-file stats simply find nothing and the
    // zero-sample digest is the signal.
    let log_dir: PathBuf = match &args.log_dir {
        Some(path) => path.clone(),
        None => PathBuf::from(&config.log_dir),
    };
    if !log_dir.is_dir() {
        log::warn!(
            "log directory {} is missing or not a directory; reporting an empty window",
            log_dir.display()
        );
    }
    let log_dir_display = log_dir.display().to_string();

    // Window resolution. The system time zone anchors bare/local forms; `until`
    // defaults to now (the test-determinism hook), `since` to `until − last`.
    let tz = jiff::tz::TimeZone::system();
    let until = match &args.until {
        Some(when) => match parse_when(when, &tz, DayEdge::End) {
            Ok(ts) => ts,
            Err(err) => return usage(format!("--until {err}")),
        },
        None => Zoned::now().timestamp(),
    };
    let since = match &args.since {
        Some(when) => match parse_when(when, &tz, DayEdge::Start) {
            Ok(ts) => ts,
            Err(err) => return usage(format!("--since {err}")),
        },
        None => {
            let last_secs = match parse_duration_secs(&args.last) {
                Ok(secs) => secs,
                Err(err) => return usage(format!("--last {err}")),
            };
            match until.checked_sub(SignedDuration::from_secs(last_secs)) {
                Ok(ts) => ts,
                Err(_) => {
                    return usage(
                        "the --last window reaches before the earliest representable time",
                    )
                }
            }
        }
    };
    if since > until {
        return usage(format!(
            "the window is empty: since ({since}) is after until ({until})"
        ));
    }

    // One streaming pass over the candidate files: aggregate every in-window
    // sample and re-derive rule transitions with a fresh engine. Display
    // filters do NOT touch this pass — rules see every in-window sample, and
    // the config's include/exclude are not reapplied (logs were already
    // filtered at write time).
    let files = digest::candidate_files(&log_dir, since, until, &tz);
    let rules_evaluated = rules.rules().len();
    // Case-fold the --match needles once for the whole run: the row filter, the
    // violation filter, and the scan-time violation count all reuse this.
    let match_needles: Vec<String> = args.r#match.iter().map(|s| s.to_lowercase()).collect();
    let type_filter = type_filter_label(args.type_filter);
    let mut source = ReplaySource::from_files(files);
    let mut engine = Engine::new(rules);
    let mut agg = Aggregator::new(config.interval_seconds);
    // Retained transitions cap DISPLAY only; `violations_total` counts the
    // exact post-filter total across the whole scan so the cap can never hide
    // data loss (mirrors how `gaps_total` stays exact while gaps are capped).
    let mut transitions: VecDeque<_> = VecDeque::new();
    let mut violations_total: usize = 0;

    while let Some(tick) = source.next_tick() {
        // Replay never emits Unavailable; a non-sample tick has no instant to
        // window on, so skip defensively.
        let Tick::Sample(sample) = &tick else {
            continue;
        };
        if sample.timestamp < since || sample.timestamp > until {
            continue;
        }
        agg.observe(sample);
        for transition in engine.observe(&tick) {
            // Mark the series BEFORE any capping: a series whose transitions all
            // fall in the evicted prefix must still keep its forced ranking tier.
            if let (Some(sensor), Some(reading)) = (&transition.sensor, &transition.reading) {
                agg.mark_in_violation(sensor, reading);
            }
            // Count the exact post-filter total BEFORE the display cap; the
            // series kind is already known (the sample was just aggregated), so
            // this uses the same predicate `emit` uses to build the shown list.
            if digest::violation_passes(&transition, &match_needles, type_filter, &agg) {
                violations_total += 1;
            }
            if transitions.len() == VIOLATION_CAP {
                transitions.pop_front();
            }
            transitions.push_back(transition);
        }
    }
    let skipped_lines = source.skipped_lines();
    // `files_scanned` is files actually opened and read, not merely selected: a
    // candidate that exists but cannot be opened (e.g. permissions) is warned to
    // stderr and excluded here, so meta stays an honest coverage signal.
    let files_scanned = source.files_opened();
    let transitions: Vec<_> = transitions.into_iter().collect();

    log::debug!(
        "report: {} sample(s), {} series, {} transition(s) retained, {} violation(s) total, \
         {} skipped line(s), {} file(s) scanned",
        agg.samples(),
        agg.series_count(),
        transitions.len(),
        violations_total,
        skipped_lines,
        files_scanned,
    );

    let cfg = Emit {
        since,
        until,
        log_dir: &log_dir_display,
        files_scanned,
        interval_seconds: config.interval_seconds,
        skipped_lines,
        rules_evaluated,
        matches: &match_needles,
        type_filter,
        top: args.top,
        max_bytes: args.max_bytes,
        indent: args.indent,
    };
    digest::emit(&cfg, &agg, &transitions, violations_total)
}

/// Emit a usage message on stderr and return the usage exit code.
fn usage(message: impl AsRef<str>) -> ExitCode {
    eprintln!("sensorwatch report: {}", message.as_ref());
    ExitCode::from(exit::USAGE)
}

/// Map the clap `--type` value onto the canonical reading-type label the digest
/// filters against — the same vocabulary as `snapshot --type`.
fn type_filter_label(filter: Option<TypeFilter>) -> Option<&'static str> {
    filter.map(|f| match f {
        TypeFilter::None => "None",
        TypeFilter::Temperature => "Temperature",
        TypeFilter::Voltage => "Voltage",
        TypeFilter::Fan => "Fan",
        TypeFilter::Current => "Current",
        TypeFilter::Power => "Power",
        TypeFilter::Clock => "Clock",
        TypeFilter::Usage => "Usage",
        TypeFilter::Other => "Other",
        TypeFilter::Unknown => "unknown",
    })
}

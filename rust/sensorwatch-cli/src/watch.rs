//! The `watch` subcommand: evaluate the config's `[[rules]]` against live
//! (or replayed) samples and turn the first firing rule into an agent
//! wake-up.
//!
//! Two modes share one transport contract (persist a sequence number, write
//! a spool file, emit the event JSON):
//!
//! - **One-shot** (default): exit 10 on the first `Fired` transition after
//!   printing its event to stdout; exit 0 if `--timeout` elapses (an agent
//!   heartbeat) or replay is exhausted first.
//! - **Follow** (`--follow`): run until interrupted, appending every fired
//!   AND cleared event to daily `events_*.jsonl` files (and, when live, also
//!   logging sensors like `log`).
//!
//! The engine never reads a clock — pacing, the shutdown condvar, and the
//! source live here, exactly like the `log` loop this is modeled on. Off
//! Windows the live source only ever yields `Unavailable`, so only
//! `source-unavailable` rules can fire; `--replay` runs anywhere.

use std::process::ExitCode;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use jiff::Zoned;

use crate::cli::{MinSeverity, WatchArgs};
use crate::config::Config;
use crate::engine::{Engine, TransitionState};
use crate::event::{self, Event, SeqStore};
use crate::exit;
use crate::jsonl::LogEntry;
use crate::logger::{self, LogWriter, EVENT_PREFIX, LINE_ENDING, LOG_PREFIX};
use crate::replay::ReplaySource;
use crate::rules::{RuleSet, Severity};
use crate::source::{LiveSource, SampleSource, Tick};

/// Entry point: resolve config → parse rules strictly → filter → prepare the
/// state directory and source → run the selected mode. Every early return
/// maps onto the exit-code contract in [`crate::exit`].
pub(crate) fn run(args: &WatchArgs) -> ExitCode {
    // Step 2: resolve the config path and read its text once. `watch` reads
    // the document a single time and feeds it to both parsers (strict rules,
    // lenient config) — the LEO-335 single-document design.
    let Some(config_path) = Config::config_path(args.config.as_deref()) else {
        match &args.config {
            Some(path) => eprintln!(
                "sensorwatch watch: no config file found (looked at {}, then ./config.toml); \
                 watch needs a [[rules]] section to evaluate.",
                path.display()
            ),
            None => eprintln!(
                "sensorwatch watch: no config file found (looked at ./config.toml); watch \
                 needs a [[rules]] section to evaluate."
            ),
        }
        return ExitCode::from(exit::USAGE);
    };
    // Step 3: read the resolved document once and strict-parse its rules via the
    // loader shared with `report`. A read failure is a fatal preparation fault;
    // a malformed/invalid rules TOML is a usage error — both messaged inside the
    // loader (they differ from report's only by the subcommand word). The loader
    // returns the once-read text; the lenient config parse stays at Step 5 below.
    let (mut rules, text) = match Config::load_rules_and_text(&config_path, "watch") {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if rules.is_empty() {
        eprintln!(
            "sensorwatch watch: {} has no [[rules]] to evaluate.",
            config_path.display()
        );
        return ExitCode::from(exit::USAGE);
    }

    // Step 4: validate --rule names, then apply --rule / --min-severity by
    // retaining rules in the set before the engine is built.
    if let Err(code) = apply_filters(&mut rules, args) {
        return code;
    }

    // Step 5: lenient config for interval / log_dir / retention / sensor filter.
    // Parsed HERE (after the empty-rules and filter checks), NOT in the loader,
    // so its warn-and-fall-back `log::warn!`s keep their original ordering — they
    // must not precede the exit-2 stderr of those checks (review finding 1). The
    // document already parsed as TOML in Step 3, so this cannot fail; default
    // defensively regardless.
    let config = Config::from_toml_str(&text).unwrap_or_default();

    let mode = if args.follow { "follow" } else { "one-shot" };
    let source_desc = if args.replay.is_empty() {
        "live".to_owned()
    } else {
        format!("replay ({} file(s))", args.replay.len())
    };
    log::info!(
        "Starting sensorwatch watch: mode={mode}, rules={}, source={source_desc}, log_dir={}",
        rules.rules().len(),
        config.log_dir,
    );
    if let Some(spool) = &args.spool_dir {
        log::info!("Spooling events to {}", spool.display());
    }

    // Step 6: shutdown handler (both modes map the signal to exit 130).
    let shutdown = match logger::install_shutdown_handler() {
        Ok(shutdown) => shutdown,
        Err(err) => {
            eprintln!("Could not install the shutdown signal handler: {err}");
            return ExitCode::from(exit::FATAL);
        }
    };

    // Step 7: the persisted sequence counter (seq integrity IS the contract).
    let mut seq_store = match SeqStore::open(&config.log_dir) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("sensorwatch watch: {err}");
            return ExitCode::from(exit::FATAL);
        }
    };

    // Step 8: the spool directory, if requested.
    if let Some(spool) = &args.spool_dir {
        if let Err(err) = std::fs::create_dir_all(spool) {
            eprintln!(
                "sensorwatch watch: could not create the spool directory {}: {err}",
                spool.display()
            );
            return ExitCode::from(exit::FATAL);
        }
    }

    // Step 9: build the source. `LiveSource` and `ReplaySource` are distinct
    // types; a trait object keeps the mode loop monomorphic.
    let mut source: Box<dyn SampleSource> = if args.replay.is_empty() {
        Box::new(LiveSource)
    } else {
        Box::new(ReplaySource::from_files(args.replay.clone()))
    };
    let mut engine = Engine::new(rules);

    // Step 10: run the selected mode.
    if args.follow {
        run_follow(
            args,
            &config,
            &mut engine,
            source.as_mut(),
            &mut seq_store,
            &shutdown,
        )
    } else {
        run_one_shot(
            args,
            &config,
            &mut engine,
            source.as_mut(),
            &mut seq_store,
            &shutdown,
        )
    }
}

/// Validate `--rule` names against the parsed set, then retain the rules the
/// `--rule` / `--min-severity` filters select. An unknown name, or an empty
/// result, is a usage error.
fn apply_filters(rules: &mut RuleSet, args: &WatchArgs) -> Result<(), ExitCode> {
    if !args.rules.is_empty() {
        let available: Vec<&str> = rules.rules().iter().map(|r| r.name.as_str()).collect();
        for name in &args.rules {
            if !available.contains(&name.as_str()) {
                eprintln!(
                    "sensorwatch watch: unknown --rule {name:?}; available rules: {}",
                    available.join(", ")
                );
                return Err(ExitCode::from(exit::USAGE));
            }
        }
        let wanted = args.rules.clone();
        rules.retain(|r| wanted.iter().any(|n| n == &r.name));
    }

    if let Some(min) = args.min_severity {
        let floor = severity_of(min).rank();
        rules.retain(|r| r.severity.rank() >= floor);
    }

    if rules.is_empty() {
        eprintln!(
            "sensorwatch watch: no rules remain after applying the --rule / --min-severity \
             filters."
        );
        return Err(ExitCode::from(exit::USAGE));
    }
    Ok(())
}

/// Map the clap `--min-severity` value onto the rules vocabulary, keeping
/// clap out of `rules.rs`.
fn severity_of(min: MinSeverity) -> Severity {
    match min {
        MinSeverity::Info => Severity::Info,
        MinSeverity::Warning => Severity::Warning,
        MinSeverity::Critical => Severity::Critical,
    }
}

/// One-shot: block until the first firing rule, emit its event, exit 10.
fn run_one_shot(
    args: &WatchArgs,
    config: &Config,
    engine: &mut Engine,
    source: &mut dyn SampleSource,
    seq_store: &mut SeqStore,
    shutdown: &(Mutex<bool>, Condvar),
) -> ExitCode {
    let is_live = args.replay.is_empty();
    let interval = Duration::from_secs(config.interval_seconds.max(1) as u64);
    // The heartbeat deadline is computed once; an absurd timeout that overflows
    // the Instant simply degrades to "no deadline". `--timeout` is inert with
    // `--replay` — replay never waits, so a slow drain must never be mistaken
    // for an all-quiet heartbeat and drop a provable fire.
    let deadline = if is_live {
        args.timeout
            .and_then(|secs| Instant::now().checked_add(Duration::from_secs(secs)))
    } else {
        None
    };

    loop {
        if shutdown_requested(shutdown) {
            return ExitCode::from(exit::INTERRUPTED);
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                return ExitCode::SUCCESS; // heartbeat: timed out with no event
            }
        }

        let Some(tick) = source.next_tick() else {
            return ExitCode::SUCCESS; // replay exhausted (live never ends)
        };

        for transition in engine.observe(&tick) {
            // A fresh engine only clears after firing, and we exit on the
            // fire — so `Cleared` cannot appear before this return. Filtering
            // is defensive.
            if transition.state != TransitionState::Fired {
                continue;
            }
            // Persist BEFORE emitting: a crash here only skips a seq, never
            // reuses one.
            let seq = match seq_store.next() {
                Ok(seq) => seq,
                Err(err) => {
                    eprintln!("sensorwatch watch: could not persist the sequence number: {err}");
                    return ExitCode::from(exit::FATAL);
                }
            };
            let json = Event::from_transition(&transition, seq).to_json();
            spool_if_configured(args, seq, &transition.rule, &json);
            // stdout event = JSON + LF (println! always uses '\n').
            println!("{json}");
            return ExitCode::from(exit::EVENT_FIRED);
        }

        // Pace live polling; replay drains back-to-back (no waiting).
        if is_live {
            let wait = match deadline {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(remaining) => remaining.min(interval),
                    None => return ExitCode::SUCCESS,
                },
                None => interval,
            };
            if logger::wait_for_shutdown(shutdown, wait) {
                return ExitCode::from(exit::INTERRUPTED);
            }
        }
    }
}

/// Follow: run until interrupted (exit 130) or replay exhaustion (exit 0),
/// appending every fired and cleared event to daily event files.
fn run_follow(
    args: &WatchArgs,
    config: &Config,
    engine: &mut Engine,
    source: &mut dyn SampleSource,
    seq_store: &mut SeqStore,
    shutdown: &(Mutex<bool>, Condvar),
) -> ExitCode {
    let is_live = args.replay.is_empty();
    let interval = Duration::from_secs(config.interval_seconds.max(1) as u64);

    // The event file uses the same rotation/retention machinery as the
    // sensor log, keyed off the wall clock at write time.
    let mut event_writer = match LogWriter::new(
        &config.log_dir,
        EVENT_PREFIX,
        config.retention_days,
        &Zoned::now(),
        LINE_ENDING,
    ) {
        Ok(writer) => writer,
        Err(err) => {
            eprintln!(
                "sensorwatch watch: could not prepare the event directory {}: {err}",
                config.log_dir
            );
            return ExitCode::from(exit::FATAL);
        }
    };

    // Live follow also logs sensors byte-compatibly; replay follow does not
    // (re-logging would duplicate history).
    let filter = config.sensor_filter();
    let mut sensor_writer = None;
    if is_live {
        match LogWriter::new(
            &config.log_dir,
            LOG_PREFIX,
            config.retention_days,
            &Zoned::now(),
            LINE_ENDING,
        ) {
            Ok(writer) => sensor_writer = Some(writer),
            Err(err) => {
                eprintln!(
                    "sensorwatch watch: could not prepare the log directory {}: {err}",
                    config.log_dir
                );
                return ExitCode::from(exit::FATAL);
            }
        }
    }
    let mut unavailable_warned = false;

    loop {
        if shutdown_requested(shutdown) {
            return ExitCode::from(exit::INTERRUPTED);
        }
        let Some(tick) = source.next_tick() else {
            return ExitCode::SUCCESS; // replay exhausted
        };

        // One wall-clock read per tick, shared by this tick's sensor row and
        // all of its events: a tick that straddles midnight then rotates every
        // record it produced into the same daily file, never split across two.
        let now = Zoned::now();

        if is_live {
            match &tick {
                Tick::Sample(sample) => {
                    let entries: Vec<LogEntry<'_>> = sample
                        .readings
                        .iter()
                        .filter(|r| filter.matches(&r.sensor))
                        .map(LogEntry::from)
                        .collect();
                    if let Some(writer) = sensor_writer.as_mut() {
                        if !entries.is_empty() {
                            writer.write(&entries, &now);
                        }
                    }
                }
                Tick::Unavailable { .. } => {
                    if !unavailable_warned {
                        log::warn!(
                            "Sensor source unavailable — only source-unavailable rules can fire \
                             until it returns."
                        );
                        unavailable_warned = true;
                    }
                }
            }
        }

        for transition in engine.observe(&tick) {
            let seq = match seq_store.next() {
                Ok(seq) => seq,
                Err(err) => {
                    eprintln!("sensorwatch watch: could not persist the sequence number: {err}");
                    return ExitCode::from(exit::FATAL);
                }
            };
            let json = Event::from_transition(&transition, seq).to_json();
            // Daily event file (rotated on this tick's `now`) then spool —
            // never stdout in follow mode.
            event_writer.write_raw(&json, &now);
            spool_if_configured(args, seq, &transition.rule, &json);
        }

        if is_live && logger::wait_for_shutdown(shutdown, interval) {
            return ExitCode::from(exit::INTERRUPTED);
        }
    }
}

/// Write the event to the spool directory when `--spool-dir` is set. A spool
/// failure is warned and swallowed: durability failing must never suppress a
/// detected event (one-shot still prints stdout and exits 10).
fn spool_if_configured(args: &WatchArgs, seq: u64, rule: &str, json: &str) {
    if let Some(spool) = &args.spool_dir {
        let name = event::spool_file_name(seq, rule);
        if let Err(err) = event::write_spool(spool, &name, json) {
            log::warn!("Failed to spool event {name} ({err})");
        }
    }
}

fn shutdown_requested(shutdown: &(Mutex<bool>, Condvar)) -> bool {
    *shutdown
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

//! Binary-level tests for the `log` subcommand's CLI contract.
//!
//! There is deliberately no *binary-level* loop test: driving a long-running
//! sampler through the executable would need spawn/sleep/kill nondeterminism
//! in CI. The loop itself is covered in-crate instead: `logger::run_loop` is
//! the sampling loop with its source/clock/wait/shutdown injected, and its
//! bounded test proves interval pacing, daily rotation, rollover retention,
//! and the sensor filter wired together (src/logger.rs). The remaining
//! pieces (config, record bytes) stay unit tested — including a golden
//! byte-comparison against a Python-logger fixture — and only the live
//! HWiNFO source path is verified manually on Windows.

mod common;
use common::*;

/// Off Windows the logger must fail fast (before touching config or the log
/// directory), matching the Python CLI's platform gate — under both the
/// canonical name and the `run` alias.
#[cfg(not(windows))]
#[test]
fn log_exits_one_fast_off_windows() {
    for subcommand in ["log", "run"] {
        let output = run_bounded(&[subcommand]);
        assert_eq!(output.status.code(), Some(1), "subcommand {subcommand}");
        assert!(stderr(&output).contains("requires Windows"));
        assert!(stdout(&output).is_empty());
    }
}

#[test]
fn log_help_lists_flags_under_both_names() {
    for subcommand in ["log", "run"] {
        let output = sensorwatch(&[subcommand, "--help"]);
        assert_eq!(output.status.code(), Some(0), "subcommand {subcommand}");
        let text = stdout(&output);
        assert!(text.contains("--config"));
        assert!(text.contains("--verbose"));
    }
}

#[test]
fn top_level_help_shows_the_run_alias() {
    let output = sensorwatch(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    // visible_alias renders in the top-level subcommand list.
    assert!(stdout(&output).contains("[aliases: run]"));
}

#[test]
fn log_usage_errors_exit_two() {
    let unknown_flag = sensorwatch(&["log", "--no-such-flag"]);
    assert_eq!(unknown_flag.status.code(), Some(2));
    let missing_value = sensorwatch(&["log", "--config"]);
    assert_eq!(missing_value.status.code(), Some(2));
}

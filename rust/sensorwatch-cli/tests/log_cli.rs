//! Binary-level tests for the `log` subcommand's CLI contract.
//!
//! There is deliberately no live-loop test: driving a long-running sampler
//! would need spawn/sleep/kill nondeterminism in CI. The loop's pieces
//! (config, record bytes, rotation, retention) are unit tested inside the
//! crate — including a golden byte-comparison against a Python-logger
//! fixture — and live behavior is verified manually on Windows.

use std::process::{Command, Output};

fn sensorwatch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .output()
        .expect("failed to run the sensorwatch binary")
}

// Only the off-Windows fast-exit test inspects stderr; keep the helper behind
// the same cfg so it is not dead code under the Windows clippy gate.
#[cfg(not(windows))]
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Run to completion, but kill and fail if the process outlives `10 s` —
/// so a regression that reaches the sampling loop fails fast instead of
/// hanging CI on `.output()`.
#[cfg(not(windows))]
fn run_bounded(args: &[&str]) -> Output {
    use std::time::{Duration, Instant};

    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait().expect("could not poll the child") {
            Some(_) => return child.wait_with_output().expect("could not collect output"),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`sensorwatch {}` did not exit within 10s — \
                     the off-Windows fast-exit path regressed into the loop",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

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

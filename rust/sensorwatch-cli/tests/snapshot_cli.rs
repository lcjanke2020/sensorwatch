//! Binary-level tests of the exit-code contract, run against the real
//! `sensorwatch` executable via `CARGO_BIN_EXE`.
//!
//! Usage errors (exit 2) and help/version (exit 0) are platform-independent.
//! The live path is deterministic off Windows — `Session::new` always fails
//! with `UnsupportedPlatform`, so exit 1 is asserted exactly there. On Windows
//! the outcome depends on whether HWiNFO64 is running, so the test accepts
//! either side of the contract (mirroring the wrapper's self-skipping live
//! test).

use std::process::{Command, Output};

fn sensorwatch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .output()
        .expect("failed to spawn the sensorwatch binary")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn unknown_type_is_a_usage_error() {
    let out = sensorwatch(&["snapshot", "--type", "BOGUS"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("possible values"), "stderr was: {err}");
    assert!(err.contains("TEMPERATURE"), "stderr was: {err}");
    assert!(stdout(&out).is_empty());
}

#[test]
fn negative_or_non_integer_indent_is_a_usage_error() {
    assert_eq!(
        sensorwatch(&["snapshot", "--indent=-3"]).status.code(),
        Some(2)
    );
    assert_eq!(
        sensorwatch(&["snapshot", "--indent", "x"]).status.code(),
        Some(2)
    );
}

#[test]
fn unknown_subcommands_and_flags_are_usage_errors() {
    assert_eq!(sensorwatch(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(sensorwatch(&[]).status.code(), Some(2));
    assert_eq!(
        sensorwatch(&["snapshot", "--bogus-flag"]).status.code(),
        Some(2)
    );
}

#[test]
fn help_and_version_exit_zero() {
    let help = sensorwatch(&["snapshot", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    let text = stdout(&help);
    for flag in ["--type", "--match", "--indent"] {
        assert!(text.contains(flag), "snapshot --help missing {flag}");
    }
    assert_eq!(sensorwatch(&["--help"]).status.code(), Some(0));
    let version = sensorwatch(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(stdout(&version).contains("0.1.0"));
}

#[cfg(not(windows))]
#[test]
fn live_snapshot_off_windows_exits_one_with_a_message() {
    for args in [
        &["snapshot"][..],
        &["snapshot", "--type", "TEMPERATURE"][..],
    ] {
        let out = sensorwatch(args);
        assert_eq!(out.status.code(), Some(1));
        let err = stderr(&out);
        assert!(err.contains("Could not read sensors"), "stderr was: {err}");
        assert!(stdout(&out).is_empty());
    }
}

#[cfg(windows)]
#[test]
fn live_snapshot_on_windows_honors_the_contract() {
    let out = sensorwatch(&["snapshot"]);
    match out.status.code() {
        // HWiNFO is running: a JSON array on stdout.
        Some(0) => {
            let text = stdout(&out);
            let trimmed = text.trim();
            assert!(trimmed.starts_with('['), "stdout was: {trimmed}");
            assert!(trimmed.ends_with(']'), "stdout was: {trimmed}");
        }
        // HWiNFO is not running: the unavailable message, nothing on stdout.
        Some(1) => {
            assert!(stderr(&out).contains("Could not read sensors"));
            assert!(stdout(&out).is_empty());
        }
        code => panic!("unexpected exit code {code:?}"),
    }
}

//! Shared harness for the binary-level integration tests.
//!
//! `src/testutil.rs` is crate-internal (`#[cfg(test)]`), so integration test
//! binaries cannot see it — which is why every `tests/*.rs` file used to carry
//! its own copy of these helpers. This module is the single home; each test
//! file pulls it in with `mod common;`.
//!
//! Each integration binary compiles `common` separately and uses a different
//! subset of it, so `#![allow(dead_code)]` keeps the unused helpers from
//! tripping the `-D warnings` gate.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

// The replay line-size cap, single-sourced from the crate so the oversized-line
// fixtures probe the real boundary — if the const moves, the fixtures move with
// it. `#[path]` is relative to this file (`tests/common/`); reference it as
// `common::limits::MAX_LINE_BYTES` (a re-export would trip `unused_imports` in
// the binaries that don't touch it, which `allow(dead_code)` does not cover).
#[path = "../../src/limits.rs"]
pub mod limits;

/// Run the binary to completion with `.output()`, which drains stdout/stderr
/// concurrently — safe for any output size.
pub fn sensorwatch(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .output()
        .expect("failed to run the sensorwatch binary")
}

/// Run to completion, killing and failing if the process outlives 10 s — so a
/// regression that never terminates (a replay that never exhausts, a timeout
/// that never elapses) fails fast instead of hanging CI.
///
/// Both pipes are drained on their own threads, so a child that writes more
/// than a pipe buffer's worth (>~64 KB) never blocks on a full stdout while the
/// main thread waits for it to exit — the poll-then-drain design this replaces
/// deadlocked exactly there.
pub fn run_bounded(args: &[&str]) -> Output {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut child = Command::new(env!("CARGO_BIN_EXE_sensorwatch"))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn the sensorwatch binary");

    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().expect("could not poll the child") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`sensorwatch {}` did not exit within 10s", args.join(" "));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // The child has exited, so both pipes are at EOF; the reader threads finish.
    let stdout = stdout_reader.join().expect("stdout reader thread panicked");
    let stderr = stderr_reader.join().expect("stderr reader thread panicked");
    Output {
        status,
        stdout,
        stderr,
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON")
}

pub fn arg(path: &Path) -> &str {
    path.to_str().expect("temp path is valid UTF-8")
}

// ---- temp dir (integration tests cannot see crate::testutil) ----

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A process-unique temporary directory, removed on drop (best effort).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "sensorwatch-cli-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a `config.toml` with the given sampling `interval_seconds`, a `logs/`
/// state dir under `dir`, and the given `[[rules]]` TOML; returns the config
/// path. `log_dir` is a TOML literal string so Windows backslashes need no
/// escaping. When `create_logs_dir` is set the `logs/` directory is created up
/// front — the zero-sample `report` cases need a directory to scan; the `watch`
/// tests do not.
pub fn write_config(
    dir: &Path,
    rules_toml: &str,
    interval_seconds: u64,
    create_logs_dir: bool,
) -> PathBuf {
    let log_dir = dir.join("logs");
    if create_logs_dir {
        std::fs::create_dir_all(&log_dir).unwrap();
    }
    let config = format!(
        "[general]\ninterval_seconds = {interval_seconds}\nlog_dir = '{}'\nretention_days = 30\n{rules_toml}",
        log_dir.display()
    );
    let path = dir.join("config.toml");
    std::fs::write(&path, config).unwrap();
    path
}

/// Write an arbitrary file (text) directly under `dir`; returns its path.
pub fn write_str(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Write a log file (raw bytes) under `dir/logs/`.
pub fn write_log(dir: &Path, name: &str, content: &[u8]) {
    std::fs::write(dir.join("logs").join(name), content).unwrap();
}

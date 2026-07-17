//! The sensorwatch command-line interface, exposed as a library.
//!
//! `main.rs` is a one-line shim over [`run`]. Exposing the crate as a library
//! (rather than binary-only) lets the cargo-fuzz targets under `fuzz/` reach the
//! untrusted-input replay parsers through the narrow [`fuzz`] surface, without
//! making the parser internals fully public.
//!
//! Behavior is identical to the binary: parse arguments, dispatch to the
//! subcommand, and map errors onto the CLI-wide exit-code contract (see
//! [`exit`]): 0 clean, 1 fatal (source/platform/state failure), 2 usage, 10
//! `watch` one-shot event fired, 130 `watch` interrupted.

#![warn(rust_2018_idioms)]

mod cli;
mod config;
mod digest;
#[cfg(test)]
mod e2e;
mod engine;
mod event;
mod exit;
mod export;
mod jsonl;
mod labels;
mod limits;
mod logger;
mod render;
mod replay;
mod report;
mod rules;
mod snapshot;
mod source;
#[cfg(test)]
mod testutil;
mod watch;

pub mod fuzz;

use clap::Parser;

/// Run the CLI and return the process exit code (see the module-level contract).
/// The binary's `main` is a thin shim over this.
pub fn run() -> std::process::ExitCode {
    let cli = cli::Cli::parse();
    // Logging goes to stderr, so it can never pollute the JSON contract on
    // stdout. `log` defaults to info (the Python logger's startup and
    // warning lines); `snapshot` keeps env_logger's error default; RUST_LOG
    // overrides either default. An explicit `--verbose` pins the filter to
    // debug and beats RUST_LOG — the flag is a direct user request, the env
    // var may be ambient.
    let mut builder = match &cli.command {
        cli::Command::Log(args) if args.verbose => debug_builder(),
        cli::Command::Watch(args) if args.verbose => debug_builder(),
        cli::Command::Report(args) if args.verbose => debug_builder(),
        cli::Command::Export(args) if args.verbose => debug_builder(),
        cli::Command::Log(_) | cli::Command::Watch(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        }
        // `report` is stdout-first (the JSON digest is the product; skipped
        // lines surface in-band via meta), but it defaults to `warn` rather than
        // `snapshot`'s `error` so provenance caveats — a missing log dir, a
        // defaulted sampling interval — reach stderr without `--verbose`.
        // `export` is file-first for the same reason: replay skip warnings and
        // a missing log dir must reach stderr unprompted.
        cli::Command::Report(_) | cli::Command::Export(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        }
        cli::Command::Snapshot(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error"))
        }
    };
    builder.init();
    match cli.command {
        cli::Command::Snapshot(args) => snapshot::run(&args),
        cli::Command::Log(args) => logger::run(&args),
        cli::Command::Watch(args) => watch::run(&args),
        cli::Command::Report(args) => report::run(&args),
        cli::Command::Export(args) => export::run(&args),
    }
}

/// The `--verbose` logging builder: pin the filter to debug, beating
/// RUST_LOG. The flag is a direct user request; the env var may be ambient.
fn debug_builder() -> env_logger::Builder {
    let mut builder = env_logger::Builder::new();
    builder.parse_filters("debug");
    builder
}

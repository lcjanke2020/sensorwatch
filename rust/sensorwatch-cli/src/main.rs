//! The sensorwatch command-line interface.
//!
//! A thin binary over the safe [`sensorwatch`] wrapper crate: parse arguments,
//! dispatch to the subcommand, and map errors onto the CLI-wide exit-code
//! contract (see [`exit`]): 0 clean, 1 fatal (source/platform/state failure),
//! 2 usage, 10 `watch` one-shot event fired, 130 `watch` interrupted.

#![warn(rust_2018_idioms)]

mod cli;
mod config;
mod digest;
mod engine;
mod event;
mod exit;
mod jsonl;
mod labels;
mod logger;
mod replay;
mod report;
mod rules;
mod snapshot;
mod source;
#[cfg(test)]
mod testutil;
mod watch;

use clap::Parser;

fn main() -> std::process::ExitCode {
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
        cli::Command::Log(_) | cli::Command::Watch(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        }
        // `report` is stdout-first (the JSON digest is the product; skipped
        // lines surface in-band via meta), but it defaults to `warn` rather than
        // `snapshot`'s `error` so provenance caveats — a missing log dir, a
        // defaulted sampling interval — reach stderr without `--verbose`.
        cli::Command::Report(_) => {
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
    }
}

/// The `--verbose` logging builder: pin the filter to debug, beating
/// RUST_LOG. The flag is a direct user request; the env var may be ambient.
fn debug_builder() -> env_logger::Builder {
    let mut builder = env_logger::Builder::new();
    builder.parse_filters("debug");
    builder
}

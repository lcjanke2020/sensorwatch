//! The sensorwatch command-line interface.
//!
//! A thin binary over the safe [`sensorwatch`] wrapper crate: parse arguments,
//! dispatch to the subcommand, and map errors onto the exit-code contract
//! (0 = success, 1 = sensor source unavailable or platform unsupported,
//! 2 = usage error — clap's default for parse and validation failures).

#![warn(rust_2018_idioms)]

mod cli;
mod config;
mod jsonl;
mod labels;
mod logger;
mod snapshot;
// The rule engine (LEO-335) is exercised only by its tests until the `watch`
// command (LEO-336) wires it to the CLI; the dead_code allows come off in
// that PR.
#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod replay;
#[allow(dead_code)]
mod rules;
#[allow(dead_code)]
mod source;
#[cfg(test)]
mod testutil;

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
        cli::Command::Log(args) if args.verbose => {
            let mut builder = env_logger::Builder::new();
            builder.parse_filters("debug");
            builder
        }
        cli::Command::Log(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        }
        cli::Command::Snapshot(_) => {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error"))
        }
    };
    builder.init();
    match cli.command {
        cli::Command::Snapshot(args) => snapshot::run(&args),
        cli::Command::Log(args) => logger::run(&args),
    }
}

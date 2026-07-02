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
#[cfg(test)]
mod testutil;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = cli::Cli::parse();
    // Logging goes to stderr, so it can never pollute the JSON contract on
    // stdout. `log` defaults to info (the Python logger's startup and
    // warning lines; debug with --verbose); `snapshot` keeps env_logger's
    // error default. RUST_LOG overrides either.
    let default_filter = match &cli.command {
        cli::Command::Log(args) if args.verbose => "debug",
        cli::Command::Log(_) => "info",
        cli::Command::Snapshot(_) => "error",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_filter))
        .init();
    match cli.command {
        cli::Command::Snapshot(args) => snapshot::run(&args),
        cli::Command::Log(args) => logger::run(&args),
    }
}

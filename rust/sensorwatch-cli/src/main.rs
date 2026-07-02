//! The sensorwatch command-line interface.
//!
//! A thin binary over the safe [`sensorwatch`] wrapper crate: parse arguments,
//! dispatch to the subcommand, and map errors onto the exit-code contract
//! (0 = success, 1 = sensor source unavailable or platform unsupported,
//! 2 = usage error — clap's default for parse and validation failures).

#![warn(rust_2018_idioms)]

mod cli;
mod snapshot;

use clap::Parser;

fn main() -> std::process::ExitCode {
    // Logging goes to stderr under RUST_LOG control, so it can never pollute
    // the JSON contract on stdout.
    env_logger::init();
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Snapshot(args) => snapshot::run(&args),
    }
}

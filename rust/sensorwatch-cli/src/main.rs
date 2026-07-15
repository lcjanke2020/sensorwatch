//! The sensorwatch command-line interface.
//!
//! A thin binary shim: all logic lives in the `sensorwatch_cli` library
//! ([`sensorwatch_cli::run`]). The library split lets the cargo-fuzz targets in
//! `fuzz/` link the crate and reach the untrusted-input replay parsers; see
//! `lib.rs` for the argument dispatch and the exit-code contract.

fn main() -> std::process::ExitCode {
    sensorwatch_cli::run()
}

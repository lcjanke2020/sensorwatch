//! The clap command-line surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Read hardware sensor data published by HWiNFO64's shared-memory feed.
#[derive(Parser)]
#[command(name = "sensorwatch", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print a one-shot live sensor snapshot as a JSON array on stdout.
    ///
    /// Exits 0 after printing (possibly an empty array), 1 when the sensor
    /// source is unavailable or the platform is unsupported, 2 on usage
    /// errors.
    Snapshot(SnapshotArgs),

    /// Run the logger loop: sample sensors on an interval and append JSON
    /// Lines records to daily files until interrupted.
    ///
    /// A byte-compatible port of the Python `sensorwatch` logger: same
    /// config schema, file layout (`<log_dir>/sensors_YYYY-MM-DD.jsonl`),
    /// rotation and retention behavior, and record bytes. Exits 0 on a
    /// signal-requested shutdown, 1 when the platform is unsupported or the
    /// log directory cannot be prepared, 2 on usage errors.
    #[command(visible_alias = "run")]
    Log(LogArgs),
}

#[derive(Args)]
pub struct SnapshotArgs {
    /// Only include readings of this type (case-insensitive).
    #[arg(long = "type", value_enum, ignore_case = true, value_name = "TYPE")]
    pub type_filter: Option<TypeFilter>,

    /// Only include readings whose sensor or reading name contains this
    /// substring (case-insensitive).
    #[arg(long, value_name = "SUBSTRING")]
    pub r#match: Option<String>,

    /// JSON indentation in spaces, 0 to 16; 0 prints a single compact line.
    #[arg(
        long,
        default_value_t = 2,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(..=16)
    )]
    pub indent: u32,
}

#[derive(Args)]
pub struct LogArgs {
    /// Path to config.toml (default: ./config.toml if present, else the
    /// built-in defaults).
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Enable debug logging (per-sample detail on stderr; takes precedence
    /// over RUST_LOG).
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

/// The `--type` filter vocabulary — the upper-case names the Python tooling
/// uses (`ReadingType.__members__`), accepted in any case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "UPPER")]
pub enum TypeFilter {
    None,
    Temperature,
    Voltage,
    Fan,
    Current,
    Power,
    Clock,
    Usage,
    Other,
    Unknown,
}

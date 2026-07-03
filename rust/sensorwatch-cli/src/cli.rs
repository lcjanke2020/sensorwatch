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

    /// Evaluate the config's `[[rules]]` against live samples and emit a
    /// structured JSON event when a rule fires — the agent wake-up primitive.
    ///
    /// Two modes. Blocking one-shot (default): wait for the first firing
    /// rule, print one JSON event to stdout (and spool it when `--spool-dir`
    /// is set), and exit 10; if `--timeout` elapses first, exit 0 (an agent
    /// heartbeat). Follow mode (`--follow`): run until interrupted, logging
    /// sensors like `log` while appending every fired and cleared event to
    /// daily `events_YYYY-MM-DD.jsonl` files.
    ///
    /// Exit codes: 0 clean (timeout, or `--replay` exhausted), 10 a rule
    /// fired (one-shot), 1 fatal (state/spool/source preparation failure), 2
    /// usage (invalid or zero rules, unknown `--rule`), 130 interrupted by a
    /// signal (both modes, including Windows Ctrl-C). Source loss is not an
    /// exit code — it surfaces as a `source-unavailable` event. Off Windows
    /// there is no live sensor source, so only `source-unavailable` rules can
    /// fire; `--replay` evaluates rules over recorded logs on any platform.
    Watch(WatchArgs),
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

#[derive(Args)]
pub struct WatchArgs {
    /// Path to config.toml (default: ./config.toml if present). The rules to
    /// evaluate live in this file's `[[rules]]` array.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Enable debug logging on stderr (takes precedence over RUST_LOG).
    #[arg(long, short = 'v')]
    pub verbose: bool,

    /// Follow mode: run until interrupted, logging sensors and appending
    /// every fired and cleared event to daily event files. Absent = a
    /// blocking one-shot that exits on the first firing rule.
    #[arg(long)]
    pub follow: bool,

    /// One-shot heartbeat deadline in whole seconds: exit 0 if no rule fires
    /// within this many seconds. Inert with `--replay` (replay never waits).
    #[arg(
        long,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(1..),
        conflicts_with = "follow"
    )]
    pub timeout: Option<u64>,

    /// Only evaluate the rule with this exact (case-sensitive) name;
    /// repeatable. An unknown name is a usage error listing the available
    /// names.
    #[arg(long = "rule", value_name = "NAME")]
    pub rules: Vec<String>,

    /// Only evaluate rules with at least this severity.
    #[arg(
        long = "min-severity",
        value_enum,
        ignore_case = true,
        value_name = "SEV"
    )]
    pub min_severity: Option<MinSeverity>,

    /// Directory for atomic per-event JSON spool files (one file per event,
    /// written temp-then-rename). Absent = no spool.
    #[arg(long = "spool-dir", value_name = "PATH")]
    pub spool_dir: Option<PathBuf>,

    /// Evaluate rules over recorded `sensors_*.jsonl` files in argument order
    /// instead of the live source (no pacing); repeatable. Answers "does my
    /// rule fire on yesterday's logs?" and drives the integration tests.
    #[arg(long, value_name = "FILE")]
    pub replay: Vec<PathBuf>,
}

/// The `--min-severity` filter vocabulary. Mapped to `rules::Severity` in
/// `watch.rs`, which keeps clap out of `rules.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum MinSeverity {
    Info,
    Warning,
    Critical,
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

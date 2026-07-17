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

    /// Summarize logged sensor history into a bounded JSON digest on stdout —
    /// the sanctioned way for an agent to read history without ever touching
    /// the raw JSONL logs.
    ///
    /// Selects the `sensors_YYYY-MM-DD.jsonl` files overlapping a window (a
    /// `--since` instant or a trailing `--last` duration ending at `--until`),
    /// streams them line by line, and emits per-(sensor, reading) window
    /// aggregates, re-derived rule violations, sampling gaps, and a meta block
    /// (sample counts and first/last timestamps — a one-call liveness check).
    /// A `--max-bytes` cap and a `--top` selector bound the output: the meta
    /// block always survives, and if the digest still overflows, detail is
    /// dropped worst-first — reading rows, then gaps, then the oldest
    /// violations (so `truncated.violations_shown < violations_total` is the
    /// signal an early violation was dropped, never that it did not happen).
    ///
    /// Exit codes: 0 whenever a digest is printed — including a zero-sample
    /// digest, which is itself the "logger is dead" signal; 1 fatal (an
    /// existing config that cannot be read); 2 usage (invalid `[[rules]]`, bad
    /// window/duration arguments, or a digest that cannot fit `--max-bytes`
    /// even fully truncated). Pure file reading — works on any platform, with
    /// or without a live sensor source.
    Report(ReportArgs),

    /// Export a window of logged sensor history as a flat Apache Parquet file
    /// (Snappy) — one row per reading per sample — for per-sample SQL analysis
    /// with any Parquet reader (DuckDB, Polars, pandas) on the consumer side.
    ///
    /// The deep-analysis complement to `report`: the digest stays the
    /// first-line, bounded surface; `export` materializes the samples
    /// themselves when a per-sample question genuinely needs them. Six fixed
    /// columns, in file order: timestamp (TIMESTAMP, microseconds, UTC),
    /// sensor, reading, type (strings), value (nullable DOUBLE — absent,
    /// null, and non-finite readings become SQL NULL), unit (string).
    /// HWiNFO's source-lifetime min/max/avg are deliberately not exported.
    /// Streams through the same bounded lenient parser as `report` (malformed
    /// and oversized lines are skipped and counted); memory stays bounded via
    /// fixed-size row groups. `--out` is created or truncated — but an --out
    /// that aliases a selected input log is refused, checked both by path
    /// and by file identity (hard links included), so the export can never
    /// destroy the history it reads. Read-only over the logs; never touches
    /// `watch.seq`. Pure file reading — works on any platform.
    ///
    /// Exit codes: 0 an export was written — including a zero-row window (a
    /// valid schema-only file, the "logger is dead" signal); 1 fatal (an
    /// existing config that cannot be read, the output file cannot be
    /// created or written, or a pre-existing --out whose distinctness from
    /// the input logs cannot be verified — the guard fails closed rather
    /// than truncate unverified); 2 usage (bad window/duration arguments, a
    /// malformed config, missing --out, or an --out naming a selected input
    /// log).
    Export(ExportArgs),
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

#[derive(Args)]
pub struct ReportArgs {
    /// Path to config.toml (default: ./config.toml if present). Supplies the
    /// `log_dir`, sampling `interval_seconds`, and the `[[rules]]` re-derived
    /// over the window. `report` works with zero rules.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Window start: an RFC 3339 instant, a local `YYYY-MM-DDTHH:MM:SS`, or a
    /// bare `YYYY-MM-DD` (the start of that local day). Overrides `--last`.
    #[arg(long, value_name = "WHEN", conflicts_with = "last")]
    pub since: Option<String>,

    /// Window end (same forms as `--since`; a bare `YYYY-MM-DD` means the END
    /// of that local day). Defaults to now — the hook that makes tests
    /// time-independent.
    #[arg(long, value_name = "WHEN")]
    pub until: Option<String>,

    /// Trailing window length ending at `--until`, when `--since` is absent.
    /// Accepts `24h`, `90m`, `45s`, `7d`, compound `1d12h` (case-insensitive
    /// units), or a bare integer of seconds.
    #[arg(long, default_value = "24h", value_name = "DURATION")]
    pub last: String,

    /// Hard upper bound on the digest's JSON size in bytes (the trailing
    /// newline is excluded). The meta block always survives; to fit, detail is
    /// dropped worst-first — reading rows, then gaps, then the oldest violations.
    #[arg(
        long = "max-bytes",
        default_value_t = 8192,
        value_name = "BYTES",
        value_parser = clap::value_parser!(u64).range(512..)
    )]
    pub max_bytes: u64,

    /// Target number of reading rows: the top-N by relative movement. Rows in
    /// violation are always kept even beyond N (only the byte cap can drop
    /// them), so the shown count can exceed N when many series are in violation.
    #[arg(
        long,
        default_value_t = 20,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub top: u32,

    /// Only show reading rows (and violations) whose sensor or reading name
    /// contains this substring (case-insensitive); repeatable, matching any.
    /// A display filter only — it never affects rule evaluation or the meta
    /// sample counts.
    #[arg(long, value_name = "SUBSTRING")]
    pub r#match: Vec<String>,

    /// Only show reading rows (and violations) of this type (case-insensitive).
    /// A display filter only, like `--match`.
    #[arg(long = "type", value_enum, ignore_case = true, value_name = "TYPE")]
    pub type_filter: Option<TypeFilter>,

    /// Read logs from this directory instead of the config's `log_dir`. A
    /// missing or unreadable directory yields a clean zero-sample digest.
    #[arg(long = "log-dir", value_name = "PATH")]
    pub log_dir: Option<PathBuf>,

    /// JSON indentation in spaces, 0 to 16; 0 prints a single compact line.
    /// Indentation counts against `--max-bytes`.
    #[arg(
        long,
        default_value_t = 0,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(..=16)
    )]
    pub indent: u32,

    /// Enable debug logging on stderr (takes precedence over RUST_LOG).
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    /// Path to config.toml (default: ./config.toml if present). Supplies only
    /// the `log_dir`; `[[rules]]` and `interval_seconds` are not read, and the
    /// config is not consulted at all when `--log-dir` is given.
    #[arg(long, short = 'c', value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Window start: an RFC 3339 instant, a local `YYYY-MM-DDTHH:MM:SS`, or a
    /// bare `YYYY-MM-DD` (the start of that local day). Overrides `--last`.
    #[arg(long, value_name = "WHEN", conflicts_with = "last")]
    pub since: Option<String>,

    /// Window end (same forms as `--since`; a bare `YYYY-MM-DD` means the END
    /// of that local day). Defaults to now; pass it explicitly for a
    /// reproducible window, as tests and scripts should.
    #[arg(long, value_name = "WHEN")]
    pub until: Option<String>,

    /// Trailing window length ending at `--until`, when `--since` is absent.
    /// Accepts `24h`, `90m`, `45s`, `7d`, compound `1d12h` (case-insensitive
    /// units), or a bare integer of seconds.
    #[arg(long, default_value = "24h", value_name = "DURATION")]
    pub last: String,

    /// Write the Parquet file to this path (created, or truncated if it
    /// already exists — except a path that names one of the selected input
    /// logs, which is refused). Required.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub out: PathBuf,

    /// Read logs from this directory instead of the config's `log_dir`. A
    /// missing or unreadable directory yields a clean zero-row export.
    #[arg(long = "log-dir", value_name = "PATH")]
    pub log_dir: Option<PathBuf>,

    /// Enable debug logging on stderr (takes precedence over RUST_LOG).
    #[arg(long, short = 'v')]
    pub verbose: bool,
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

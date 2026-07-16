//! The `export` subcommand: materialize a window of logged JSONL history as a
//! flat Apache Parquet file — the sanctioned deep-analysis surface.
//!
//! **Protocol role.** `report` stays the first-line, bounded way to read
//! history; its digest is deliberately aggregate-only, so per-sample questions
//! ("at what minute did the GPU peak?") are out of its reach. `export` is the
//! complement: it streams a window through the same bounded lenient replay
//! parser and writes **one row per reading per sample** to a Parquet file that
//! any SQL engine (DuckDB, Polars, pandas) queries on the consumer side. The
//! file is a product for tools, not for context windows — agents query it and
//! read bounded results; they never read the file (or the raw logs) directly.
//!
//! **Schema.** Six fixed columns, in file order: `timestamp` (TIMESTAMP,
//! microseconds, adjusted-to-UTC), `sensor`, `reading`, `type`, `value`
//! (nullable DOUBLE), `unit` — the non-timestamp, non-value columns are UTF8
//! strings, and `type` is the canonical Title-case label used everywhere in
//! the CLI. A reading whose value is absent, JSON `null`, or non-finite
//! (including the Python logger's bare `NaN`/`Infinity` tokens, which the
//! replay parser folds to NaN) becomes SQL `NULL` — NULL is what SQL
//! aggregates and comparisons handle sanely; NaN is not.
//!
//! **Why lifetime aggregates are excluded.** Each logged reading carries
//! HWiNFO's own `min`/`max`/`avg`, but those are extremes over HWiNFO's
//! *session* lifetime, not any analysis window — the same reason `report`
//! ignores them. Window aggregates are one `GROUP BY` away in SQL.
//!
//! **Bounded memory.** Rows are buffered per Parquet row group and flushed at
//! a fixed size, so memory is bounded by one row group no matter how large the
//! window — the export never holds a whole day in memory (the same guarantee
//! as `report`).
//!
//! **Read-only guarantee.** `export` never writes state — in particular it
//! never touches `watch.seq`. Its only write is the `--out` file, which is
//! created or truncated; an `--out` that aliases one of the selected input
//! logs — by path (symlinks, `.`/`..` segments, Windows case folding) or, on
//! Unix, by file identity (hard links share an inode but canonicalize to two
//! different paths) — is refused (usage error) so the export can never
//! destroy the history it reads. The output is opened without truncation and
//! truncated only after the guard passes, through the same handle, so the
//! file that was checked is the file that gets truncated. std has no stable
//! Windows file-identity API, so an NTFS hard link to a log is the one alias
//! the Windows check misses. A mid-write failure exits 1 and may leave a
//! partial file behind.
//!
//! **Timestamps are UTC instants.** The log line's original local offset
//! (`raw_timestamp`) is not preserved as a column; convert in SQL
//! (`timestamp AT TIME ZONE ...`) for local wall-clock questions. Sub-µs
//! precision would be truncated by `as_microsecond`, but both loggers write
//! exactly six fractional digits, so nothing is lost.

use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use jiff::{SignedDuration, Zoned};
use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;

use crate::cli::ExportArgs;
use crate::config::Config;
use crate::digest::{self, parse_duration_secs, parse_when, DayEdge};
use crate::exit;
use crate::replay::ReplaySource;
use crate::source::{SampleReading, SampleSource, Tick};

/// Rows buffered per Parquet row group before flushing to the sink. Bounds
/// memory to one row group's worth of values (~20 MB at typical row width)
/// no matter the window size.
const ROWS_PER_ROW_GROUP: usize = 100_000;

/// The flat export schema. `TIMESTAMP(MICROS,true)` is microseconds since the
/// Unix epoch, adjusted-to-UTC — the faithful encoding of a [`jiff::Timestamp`]
/// (an instant). Column order is load-bearing: [`ParquetSink::flush_row_group`]
/// writes columns positionally in exactly this order.
const MESSAGE_TYPE: &str = "
  message sensorwatch_export {
    required int64 timestamp (TIMESTAMP(MICROS,true));
    required binary sensor (STRING);
    required binary reading (STRING);
    required binary type (STRING);
    optional double value;
    required binary unit (STRING);
  }
";

pub(crate) fn run(args: &ExportArgs) -> ExitCode {
    // Config resolution. `export` needs the config for exactly one thing —
    // `log_dir` — so `--log-dir` skips the config entirely, and `[[rules]]`
    // are never parsed (no violations are re-derived here). Unlike `report`'s
    // warn-and-default lenient parse, a syntactically malformed config is a
    // usage error: silently defaulting `log_dir` would produce a misleading
    // empty-but-successful export from the wrong directory.
    let config = match &args.log_dir {
        Some(_) => Config::default(),
        None => match Config::config_path(args.config.as_deref()) {
            Some(path) => {
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(err) => {
                        eprintln!(
                            "sensorwatch export: could not read config {}: {err}",
                            path.display()
                        );
                        return ExitCode::from(exit::FATAL);
                    }
                };
                match Config::from_toml_str(&text) {
                    Ok(config) => config,
                    Err(err) => {
                        return usage(format!("could not parse config {}: {err}", path.display()))
                    }
                }
            }
            None => Config::default(),
        },
    };

    // Resolve the log directory (override wins). A missing or non-directory
    // path is not fatal: candidate-file stats simply find nothing and the
    // schema-only export is the signal, mirroring report's zero-sample digest.
    let log_dir: PathBuf = match &args.log_dir {
        Some(path) => path.clone(),
        None => PathBuf::from(&config.log_dir),
    };
    if !log_dir.is_dir() {
        log::warn!(
            "log directory {} is missing or not a directory; exporting an empty window",
            log_dir.display()
        );
    }

    // Window resolution — identical to `report` (the same flags, forms, and
    // defaults). `--until` defaults to now, which is time-dependent by nature;
    // reproducible windows come from passing it explicitly, as every test does.
    let tz = jiff::tz::TimeZone::system();
    let until = match &args.until {
        Some(when) => match parse_when(when, &tz, DayEdge::End) {
            Ok(ts) => ts,
            Err(err) => return usage(format!("--until {err}")),
        },
        None => Zoned::now().timestamp(),
    };
    let since = match &args.since {
        Some(when) => match parse_when(when, &tz, DayEdge::Start) {
            Ok(ts) => ts,
            Err(err) => return usage(format!("--since {err}")),
        },
        None => {
            let last_secs = match parse_duration_secs(&args.last) {
                Ok(secs) => secs,
                Err(err) => return usage(format!("--last {err}")),
            };
            match until.checked_sub(SignedDuration::from_secs(last_secs)) {
                Ok(ts) => ts,
                Err(_) => {
                    return usage(
                        "the --last window reaches before the earliest representable time",
                    )
                }
            }
        }
    };
    if since > until {
        return usage(format!(
            "the window is empty: since ({since}) is after until ({until})"
        ));
    }

    // Select the input files BEFORE the output is opened: `--out` accepts any
    // path, and create/truncate would otherwise destroy the very history being
    // exported if it named (or aliased) a selected input log.
    let files = digest::candidate_files(&log_dir, since, until, &tz);

    // Open the output WITHOUT truncating, then let the alias guard judge the
    // opened handle itself. Only a pre-existing file can alias an input (a
    // freshly created one has a fresh identity and a fresh canonical path), a
    // refused output is left exactly as it was found, and truncating through
    // the validated handle — not by reopening the path — means the file that
    // was checked is the file that gets truncated.
    let file = match File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&args.out)
    {
        Ok(file) => file,
        Err(err) => {
            eprintln!(
                "sensorwatch export: cannot create {}: {err}",
                args.out.display()
            );
            return ExitCode::from(exit::FATAL);
        }
    };
    if aliases_selected_input(&file, &args.out, &files) {
        return usage(format!(
            "--out {} is one of the selected input logs; refusing to overwrite history",
            args.out.display()
        ));
    }
    // Truncate now that the handle is validated — even a zero-sample window
    // leaves a valid schema-only file behind.
    if let Err(err) = file.set_len(0) {
        eprintln!(
            "sensorwatch export: cannot truncate {}: {err}",
            args.out.display()
        );
        return ExitCode::from(exit::FATAL);
    }
    let mut sink = match ParquetSink::new(file) {
        Ok(sink) => sink,
        Err(err) => {
            eprintln!("sensorwatch export: cannot initialize the parquet writer: {err}");
            return ExitCode::from(exit::FATAL);
        }
    };

    // One streaming pass over the candidate files — the `report` loop minus
    // the engine and aggregator. The window precheck lets replay drop
    // out-of-window lines cheaply; the post-parse filter below still covers
    // the lines that reach the full parse.
    let mut source = ReplaySource::from_files(files).with_window(since, until);
    let mut samples: u64 = 0;
    while let Some(tick) = source.next_tick() {
        // Replay never emits Unavailable; a non-sample tick has no instant to
        // window on, so skip defensively.
        let Tick::Sample(sample) = &tick else {
            continue;
        };
        if sample.timestamp < since || sample.timestamp > until {
            continue;
        }
        samples += 1;
        let micros = sample.timestamp.as_microsecond();
        for reading in &sample.readings {
            if let Err(err) = sink.push(micros, reading) {
                eprintln!(
                    "sensorwatch export: failed writing {}: {err}",
                    args.out.display()
                );
                return ExitCode::from(exit::FATAL);
            }
        }
    }
    let skipped_lines = source.skipped_lines();
    let files_scanned = source.files_opened();

    let rows_written = match sink.close() {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!(
                "sensorwatch export: failed writing {}: {err}",
                args.out.display()
            );
            return ExitCode::from(exit::FATAL);
        }
    };

    // The summary is operationally significant (skipped_lines is a
    // data-integrity signal), so it prints unconditionally — on stderr, since
    // the product is the file, not stdout.
    eprintln!(
        "sensorwatch export: wrote {rows_written} rows from {samples} samples \
         ({files_scanned} files scanned, {skipped_lines} skipped lines) to {}",
        args.out.display()
    );
    ExitCode::SUCCESS
}

/// Emit a usage message on stderr and return the usage exit code.
fn usage(message: impl AsRef<str>) -> ExitCode {
    eprintln!("sensorwatch export: {}", message.as_ref());
    ExitCode::from(exit::USAGE)
}

/// True when the already-open (and not yet truncated) output is one of the
/// selected input logs. Two alias mechanisms are checked: canonical-path
/// equality — symlinks, `.`/`..` segments, Windows case folding — and, on
/// Unix, the opened handle's file identity (`st_dev`, `st_ino`), which also
/// catches hard links, where two directory entries share an inode but
/// canonicalize to two different paths. std exposes no stable Windows
/// file-identity API (`MetadataExt::file_index` is nightly-only), so Windows
/// keeps the path check alone. Candidates exist on disk (`candidate_files`
/// stats them), so both sides canonicalize; a candidate that fails to is
/// unreadable and moot.
fn aliases_selected_input(out: &File, out_path: &std::path::Path, inputs: &[PathBuf]) -> bool {
    let out_canonical = std::fs::canonicalize(out_path).ok();
    #[cfg(unix)]
    let out_id = {
        use std::os::unix::fs::MetadataExt;
        out.metadata().ok().map(|meta| (meta.dev(), meta.ino()))
    };
    #[cfg(not(unix))]
    let _ = out;
    inputs.iter().any(|input| {
        if let (Some(out_canonical), Ok(input_canonical)) =
            (out_canonical.as_ref(), std::fs::canonicalize(input))
        {
            if *out_canonical == input_canonical {
                return true;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Some(out_id), Ok(meta)) = (out_id, std::fs::metadata(input)) {
                if out_id == (meta.dev(), meta.ino()) {
                    return true;
                }
            }
        }
        false
    })
}

/// A columnar sink over [`SerializedFileWriter`]: buffers up to
/// `rows_per_group` flattened rows in parallel column vectors and flushes
/// them as one Parquet row group.
struct ParquetSink<W: std::io::Write + Send> {
    writer: SerializedFileWriter<W>,
    rows_per_group: usize,
    timestamps: Vec<i64>,
    sensors: Vec<ByteArray>,
    readings: Vec<ByteArray>,
    types: Vec<ByteArray>,
    /// Dense: present (finite) values only; `value_defs` carries the per-row
    /// definition levels (1 = present, 0 = NULL) that map them back to rows.
    values: Vec<f64>,
    value_defs: Vec<i16>,
    units: Vec<ByteArray>,
    rows_written: u64,
}

impl<W: std::io::Write + Send> ParquetSink<W> {
    fn new(sink: W) -> parquet::errors::Result<Self> {
        Self::with_rows_per_group(sink, ROWS_PER_ROW_GROUP)
    }

    /// The test seam: unit tests shrink the row-group size to exercise the
    /// flush boundary without pushing 100k rows.
    fn with_rows_per_group(sink: W, rows_per_group: usize) -> parquet::errors::Result<Self> {
        let schema = Arc::new(parse_message_type(MESSAGE_TYPE)?);
        // Snappy matches the repo's existing Parquet artifact
        // (examples/psu-efficiency) and every mainstream reader. Dictionary
        // encoding is on by default — ideal for the four highly repetitive
        // string columns.
        let properties = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .build(),
        );
        Ok(ParquetSink {
            writer: SerializedFileWriter::new(sink, schema, properties)?,
            rows_per_group,
            timestamps: Vec::with_capacity(rows_per_group),
            sensors: Vec::with_capacity(rows_per_group),
            readings: Vec::with_capacity(rows_per_group),
            types: Vec::with_capacity(rows_per_group),
            values: Vec::with_capacity(rows_per_group),
            value_defs: Vec::with_capacity(rows_per_group),
            units: Vec::with_capacity(rows_per_group),
            rows_written: 0,
        })
    }

    /// Buffer one flattened row; flushes a row group when the buffer fills.
    fn push(&mut self, timestamp_micros: i64, r: &SampleReading) -> parquet::errors::Result<()> {
        self.timestamps.push(timestamp_micros);
        self.sensors.push(ByteArray::from(r.sensor.as_str()));
        self.readings.push(ByteArray::from(r.reading.as_str()));
        self.types.push(ByteArray::from(r.kind));
        // ReplaySource folds absent/`null`/Python non-finite tokens to NaN
        // (and ±Infinity parses to itself), so `is_finite` is the single,
        // total present-vs-NULL gate.
        if r.value.is_finite() {
            self.values.push(r.value);
            self.value_defs.push(1);
        } else {
            self.value_defs.push(0);
        }
        self.units.push(ByteArray::from(r.unit.as_str()));
        if self.value_defs.len() >= self.rows_per_group {
            self.flush_row_group()?;
        }
        Ok(())
    }

    /// Write the buffered rows as one row group. Columns are written
    /// positionally in [`MESSAGE_TYPE`] order — the six explicit blocks (the
    /// value types differ per column) must match the schema string.
    fn flush_row_group(&mut self) -> parquet::errors::Result<()> {
        if self.value_defs.is_empty() {
            return Ok(());
        }
        let mut group = self.writer.next_row_group()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<Int64Type>()
            .write_batch(&self.timestamps, None, None)?;
        column.close()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<ByteArrayType>()
            .write_batch(&self.sensors, None, None)?;
        column.close()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<ByteArrayType>()
            .write_batch(&self.readings, None, None)?;
        column.close()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<ByteArrayType>()
            .write_batch(&self.types, None, None)?;
        column.close()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<DoubleType>()
            .write_batch(&self.values, Some(&self.value_defs), None)?;
        column.close()?;
        let mut column = group.next_column()?.expect("schema declares 6 columns");
        column
            .typed::<ByteArrayType>()
            .write_batch(&self.units, None, None)?;
        column.close()?;
        group.close()?;
        self.rows_written += self.value_defs.len() as u64;
        self.timestamps.clear();
        self.sensors.clear();
        self.readings.clear();
        self.types.clear();
        self.values.clear();
        self.value_defs.clear();
        self.units.clear();
        Ok(())
    }

    /// Flush the final partial row group and write the file footer. A file
    /// closed with zero row groups is a valid schema-only Parquet file — the
    /// zero-sample case. Returns the total rows written.
    fn close(mut self) -> parquet::errors::Result<u64> {
        self.flush_row_group()?;
        self.writer.close()?;
        Ok(self.rows_written)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::Path;

    use parquet::basic::{LogicalType, TimeUnit, TimestampType, Type as PhysicalType};
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::{Field, Row};

    use super::*;
    use crate::labels;
    use crate::testutil::TempDir;

    fn reading(
        sensor: &str,
        name: &str,
        kind: &'static str,
        value: f64,
        unit: &str,
    ) -> SampleReading {
        SampleReading {
            sensor: sensor.to_owned(),
            reading: name.to_owned(),
            kind,
            value,
            // Deliberately populated: these must never reach the file.
            min: 1.0,
            max: 99.0,
            avg: 50.0,
            unit: unit.to_owned(),
        }
    }

    fn reader(path: &Path) -> SerializedFileReader<File> {
        SerializedFileReader::new(File::open(path).expect("open parquet")).expect("read parquet")
    }

    fn rows(path: &Path) -> Vec<Row> {
        reader(path)
            .get_row_iter(None)
            .expect("row iter")
            .map(|row| row.expect("row"))
            .collect()
    }

    fn field(row: &Row, index: usize) -> Field {
        row.get_column_iter()
            .nth(index)
            .expect("column index in range")
            .1
            .clone()
    }

    fn timestamp_micros(row: &Row, index: usize) -> i64 {
        match field(row, index) {
            Field::TimestampMicros(v) => v,
            Field::Long(v) => v,
            other => panic!("expected a micros timestamp, got {other:?}"),
        }
    }

    fn string(row: &Row, index: usize) -> String {
        match field(row, index) {
            Field::Str(s) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }

    #[test]
    fn schema_only_file_for_zero_rows() {
        let dir = TempDir::new();
        let path = dir.path().join("empty.parquet");
        let sink = ParquetSink::new(File::create(&path).unwrap()).unwrap();
        assert_eq!(sink.close().unwrap(), 0);

        let reader = reader(&path);
        let schema = reader.metadata().file_metadata().schema_descr();
        assert_eq!(schema.num_columns(), 6);
        let names: Vec<String> = (0..6).map(|i| schema.column(i).name().to_owned()).collect();
        assert_eq!(
            names,
            ["timestamp", "sensor", "reading", "type", "value", "unit"]
        );
        assert_eq!(reader.metadata().num_row_groups(), 0);
        assert_eq!(rows(&path).len(), 0);
    }

    #[test]
    fn timestamp_is_micros_utc_and_schema_types_match() {
        let dir = TempDir::new();
        let path = dir.path().join("one.parquet");
        let ts: jiff::Timestamp = "2026-02-18T08:17:48.123456-05:00".parse().unwrap();
        let mut sink = ParquetSink::new(File::create(&path).unwrap()).unwrap();
        sink.push(
            ts.as_microsecond(),
            &reading("S", "R", "Voltage", 12.5, "V"),
        )
        .unwrap();
        assert_eq!(sink.close().unwrap(), 1);

        let reader = reader(&path);
        let schema = reader.metadata().file_metadata().schema_descr();
        match schema.column(0).logical_type_ref() {
            Some(LogicalType::Timestamp(TimestampType {
                is_adjusted_to_u_t_c: true,
                unit: TimeUnit::MICROS,
            })) => {}
            other => panic!("timestamp logical type is {other:?}"),
        }
        assert_eq!(schema.column(4).physical_type(), PhysicalType::DOUBLE);
        assert_eq!(schema.column(4).max_def_level(), 1, "value is optional");
        for index in [1, 2, 3, 5] {
            assert_eq!(
                schema.column(index).physical_type(),
                PhysicalType::BYTE_ARRAY
            );
            assert!(
                matches!(
                    schema.column(index).logical_type_ref(),
                    Some(LogicalType::String)
                ),
                "column {index} must be a UTF8 string"
            );
        }
        let all = rows(&path);
        assert_eq!(timestamp_micros(&all[0], 0), ts.as_microsecond());
    }

    #[test]
    fn non_finite_values_map_to_null() {
        let dir = TempDir::new();
        let path = dir.path().join("nulls.parquet");
        let mut sink = ParquetSink::new(File::create(&path).unwrap()).unwrap();
        for value in [12.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            sink.push(0, &reading("S", "R", "Voltage", value, "V"))
                .unwrap();
        }
        assert_eq!(sink.close().unwrap(), 4);

        let all = rows(&path);
        assert_eq!(all.len(), 4);
        assert!(matches!(field(&all[0], 4), Field::Double(v) if v == 12.5));
        for row in &all[1..] {
            assert!(matches!(field(row, 4), Field::Null));
        }
        // The lifetime min/max/avg set on every pushed reading are provably
        // absent: six columns is the whole schema.
        assert_eq!(all[0].get_column_iter().count(), 6);
    }

    #[test]
    fn row_groups_flush_at_the_boundary() {
        let dir = TempDir::new();
        let path = dir.path().join("groups.parquet");
        let mut sink = ParquetSink::with_rows_per_group(File::create(&path).unwrap(), 2).unwrap();
        for index in 0..5 {
            sink.push(index, &reading("S", "R", "Voltage", index as f64, "V"))
                .unwrap();
        }
        assert_eq!(sink.close().unwrap(), 5);

        let reader = reader(&path);
        assert_eq!(reader.metadata().num_row_groups(), 3);
        let group_rows: Vec<i64> = (0..3)
            .map(|i| reader.metadata().row_group(i).num_rows())
            .collect();
        assert_eq!(group_rows, [2, 2, 1]);
        let all = rows(&path);
        assert_eq!(all.len(), 5);
        for (index, row) in all.iter().enumerate() {
            assert_eq!(timestamp_micros(row, 0), index as i64, "insertion order");
        }
    }

    #[test]
    fn snappy_compression_is_set() {
        let dir = TempDir::new();
        let path = dir.path().join("snappy.parquet");
        let mut sink = ParquetSink::new(File::create(&path).unwrap()).unwrap();
        sink.push(0, &reading("S", "R", "Voltage", 1.0, "V"))
            .unwrap();
        sink.close().unwrap();

        let reader = reader(&path);
        let group = reader.metadata().row_group(0);
        for index in 0..group.num_columns() {
            assert_eq!(group.column(index).compression(), Compression::SNAPPY);
        }
    }

    #[test]
    fn hostile_strings_round_trip() {
        let dir = TempDir::new();
        let path = dir.path().join("hostile.parquet");
        let kind = labels::normalize_type_label("unknown(35)");
        let mut sink = ParquetSink::new(File::create(&path).unwrap()).unwrap();
        sink.push(0, &reading("evil \" NaN", "line\nbreak", kind, 1.0, "°C"))
            .unwrap();
        sink.close().unwrap();

        let all = rows(&path);
        assert_eq!(string(&all[0], 1), "evil \" NaN");
        assert_eq!(string(&all[0], 2), "line\nbreak");
        assert_eq!(string(&all[0], 3), kind);
        assert_eq!(string(&all[0], 5), "°C");
    }
}

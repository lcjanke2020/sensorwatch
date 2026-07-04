//! Configuration for the `log` subcommand — a port of the Python
//! `sensorwatch/config.py`, keeping its schema and lenient coercions: bad
//! values warn and fall back to per-key defaults rather than crashing the
//! logger. Parsing walks a dynamic [`toml::Table`] because serde derive
//! cannot express warn-and-fall-back semantics.
//!
//! One deliberate tightening over Python: `log_dir` accepts only TOML
//! strings (Python `str()`-coerces any value, so `log_dir = 5` silently
//! becomes the directory `"5"`); non-strings warn and use the default.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::exit;
use crate::rules::RuleSet;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Config {
    pub interval_seconds: i64,
    pub log_dir: String,
    pub retention_days: i64,
    pub sensor_include: Vec<String>,
    pub sensor_exclude: Vec<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            interval_seconds: 10,
            log_dir: "logs".to_owned(),
            retention_days: 30,
            sensor_include: Vec::new(),
            sensor_exclude: Vec::new(),
        }
    }
}

impl Config {
    /// Load the config: the explicit `--config` path if given, else
    /// `config.toml` in the current directory (the static-binary analog of
    /// the Python package-relative lookup), else built-in defaults.
    pub(crate) fn load(explicit: Option<&Path>) -> Config {
        Config::load_from(explicit, &[Path::new("config.toml")])
    }

    /// [`Config::load`] with an injectable fallback chain, so tests never
    /// depend on the process working directory.
    fn load_from(explicit: Option<&Path>, fallbacks: &[&Path]) -> Config {
        if let Some(path) = explicit {
            if path.exists() {
                return Config::load_file(path);
            }
            // Python silently falls through here; a typo'd --config quietly
            // ignoring your file is a footgun, so warn (output is unchanged).
            // The fallback chain may still find ./config.toml, so don't claim
            // "defaults" — name the lookup instead.
            log::warn!(
                "Config file {} not found; falling back to the default config lookup",
                path.display()
            );
        }
        for candidate in fallbacks {
            if candidate.exists() {
                return Config::load_file(candidate);
            }
        }
        Config::default()
    }

    /// Resolve the config path `watch` will read. Unlike [`Config::load`],
    /// which reads leniently and returns a `Config`, `watch` needs the *path*
    /// so it can read the text once and feed both the strict rules parser and
    /// the lenient config parser (the LEO-335 single-document design). The
    /// resolution mirrors `load`: an existing explicit `--config` wins; a
    /// given-but-missing path warns and falls through to `./config.toml`;
    /// absent that, `None` (watch treats "no config" as a zero-rules usage
    /// error). The `_from` split keeps tests independent of the process cwd.
    pub(crate) fn config_path(explicit: Option<&Path>) -> Option<PathBuf> {
        Config::config_path_from(explicit, &[Path::new("config.toml")])
    }

    fn config_path_from(explicit: Option<&Path>, fallbacks: &[&Path]) -> Option<PathBuf> {
        if let Some(path) = explicit {
            if path.exists() {
                return Some(path.to_path_buf());
            }
            log::warn!(
                "Config file {} not found; falling back to the default config lookup",
                path.display()
            );
        }
        fallbacks
            .iter()
            .find(|candidate| candidate.exists())
            .map(|candidate| candidate.to_path_buf())
    }

    /// Read an already-resolved config document once and parse it for the
    /// subcommands that need both the alert rules and the general config
    /// (`watch`, `report`): the strict rules parser first, then the lenient
    /// config parser. `path` must already exist ([`Config::config_path`] only
    /// returns existing paths), so a read failure is an I/O fault on a present
    /// file — a fatal *preparation* failure (exit 1), not a usage error. On
    /// error the message is printed here and the exit code is returned for the
    /// caller to propagate; the two subcommands' messages differ ONLY by the
    /// `subcommand` word, so single-sourcing them keeps them from drifting.
    ///
    /// The divergent handling stays at the callers by design: `report`'s
    /// no-config arm proceeds over zero rules while `watch`'s errors, and
    /// `watch`'s empty-rules rejection and `--rule`/`--min-severity` filtering
    /// both run after this returns.
    ///
    /// **The document is deliberately parsed twice** (LEO-350 decision 1), and
    /// that is load-bearing, not redundant: `rules.rs` parses from the raw text
    /// so a `toml::de::Error` on a bad rule carries the line/column span a
    /// startup error should point at (`RawRule`'s `deny_unknown_fields`), while
    /// `config.rs` walks a pre-built `toml::Table` for its warn-and-fall-back
    /// leniency. Deserializing the rules from a shared pre-parsed `Value` would
    /// silently drop those spans on semantic rule errors — a stderr regression —
    /// so the two parsers keep reading the document independently.
    pub(crate) fn load_rules_and_config(
        path: &Path,
        subcommand: &str,
    ) -> Result<(RuleSet, Config), ExitCode> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!(
                    "sensorwatch {subcommand}: could not read config {}: {err}",
                    path.display()
                );
                return Err(ExitCode::from(exit::FATAL));
            }
        };
        let rules = match RuleSet::from_toml_str(&text) {
            Ok(rules) => rules,
            Err(err) => {
                eprintln!("{err}");
                return Err(ExitCode::from(exit::USAGE));
            }
        };
        let config = Config::from_toml_str(&text).unwrap_or_default();
        Ok((rules, config))
    }

    /// Read and parse one file; unreadable or malformed TOML warns and
    /// yields the defaults (it does not continue down the fallback chain,
    /// matching Python's `Config.load`).
    fn load_file(path: &Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                log::warn!("Failed to load config ({err}), using defaults");
                return Config::default();
            }
        };
        match Config::from_toml_str(&text) {
            Ok(config) => config,
            Err(err) => {
                log::warn!("Failed to load config ({err}), using defaults");
                Config::default()
            }
        }
    }

    /// Parse a TOML document, falling back to defaults for missing or
    /// invalid keys. Only a syntactically malformed document is an error.
    /// `pub(crate)` so `watch` can parse config out of already-read text
    /// (it reads the file once and also hands the text to the rules parser).
    pub(crate) fn from_toml_str(text: &str) -> Result<Config, toml::de::Error> {
        let data: toml::Table = text.parse()?;
        let defaults = Config::default();

        let general = section(&data, "general");
        let sensors = section(&data, "sensors");
        fn get<'a>(table: Option<&'a toml::Table>, key: &str) -> Option<&'a toml::Value> {
            table.and_then(|t| t.get(key))
        }

        let log_dir = match get(general, "log_dir") {
            None => defaults.log_dir,
            Some(toml::Value::String(s)) => s.clone(),
            Some(other) => {
                log::warn!(
                    "Config 'log_dir' must be a string, got {}; using {:?}",
                    other.type_str(),
                    defaults.log_dir
                );
                defaults.log_dir
            }
        };

        Ok(Config {
            interval_seconds: as_int(
                "interval_seconds",
                get(general, "interval_seconds"),
                defaults.interval_seconds,
                1,
            ),
            log_dir,
            retention_days: as_int(
                "retention_days",
                get(general, "retention_days"),
                defaults.retention_days,
                0,
            ),
            sensor_include: clean_str_list("sensors.include", get(sensors, "include")),
            sensor_exclude: clean_str_list("sensors.exclude", get(sensors, "exclude")),
        })
    }

    /// Build the reusable matcher, lowercasing the patterns once. The config
    /// keeps them as configured (the startup log prints them verbatim, like
    /// Python); the filter holds the normalized copies the hot path needs.
    pub(crate) fn sensor_filter(&self) -> SensorFilter {
        SensorFilter {
            include: self
                .sensor_include
                .iter()
                .map(|p| p.to_lowercase())
                .collect(),
            exclude: self
                .sensor_exclude
                .iter()
                .map(|p| p.to_lowercase())
                .collect(),
        }
    }
}

/// The include/exclude filters with patterns pre-lowercased, so per-reading
/// matching allocates only the sensor name's lowercase form.
pub(crate) struct SensorFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl SensorFilter {
    /// Check a sensor name against the include/exclude filters:
    /// case-insensitive substring matches, an empty include list includes
    /// everything, and exclude always applies after include.
    pub(crate) fn matches(&self, sensor_name: &str) -> bool {
        let name_lower = sensor_name.to_lowercase();

        if !self.include.is_empty()
            && !self
                .include
                .iter()
                .any(|pat| name_lower.contains(pat.as_str()))
        {
            return false;
        }

        !self
            .exclude
            .iter()
            .any(|pat| name_lower.contains(pat.as_str()))
    }
}

/// A named table, tolerating its absence; a non-table value warns and reads
/// as absent (Python would crash on `general = 5` — lenient is the contract).
fn section<'a>(data: &'a toml::Table, key: &str) -> Option<&'a toml::Table> {
    match data.get(key) {
        None => None,
        Some(toml::Value::Table(table)) => Some(table),
        Some(other) => {
            log::warn!(
                "Config '{key}' must be a table, got {}; ignoring",
                other.type_str()
            );
            None
        }
    }
}

/// Coerce a config value to an integer >= `minimum`, falling back to
/// `default`. Booleans are rejected explicitly to mirror the Python guard
/// (there `bool` is an `int` subclass; in TOML they are distinct types, but
/// the warning contract stays the same).
fn as_int(key: &str, value: Option<&toml::Value>, default: i64, minimum: i64) -> i64 {
    let value = match value {
        None => return default,
        Some(toml::Value::Integer(i)) => *i,
        Some(other) => {
            log::warn!(
                "Config '{key}' must be an integer, got {other} ({}); using {default}",
                other.type_str()
            );
            return default;
        }
    };
    if value < minimum {
        log::warn!("Config '{key}' ({value}) is below minimum {minimum}; using {default}");
        return default;
    }
    value
}

/// Coerce a config value to a list of non-empty, trimmed strings. Non-list
/// values (e.g. a bare `include = "MEG"` string) are rejected with a warning;
/// non-string elements are dropped silently (Python parity).
fn clean_str_list(key: &str, value: Option<&toml::Value>) -> Vec<String> {
    let items = match value {
        None => return Vec::new(),
        Some(toml::Value::Array(items)) => items,
        Some(other) => {
            log::warn!(
                "Config '{key}' must be a list of strings, got {}; ignoring",
                other.type_str()
            );
            return Vec::new();
        }
    };
    items
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn int(v: i64) -> toml::Value {
        toml::Value::Integer(v)
    }

    // ---- as_int (ports tests/test_config.py::TestAsInt) ----

    #[test]
    fn as_int_accepts_valid_and_minimum() {
        assert_eq!(as_int("k", Some(&int(5)), 10, 1), 5);
        assert_eq!(as_int("k", Some(&int(1)), 10, 1), 1);
    }

    #[test]
    fn as_int_rejects_bool() {
        assert_eq!(as_int("k", Some(&toml::Value::Boolean(true)), 10, 1), 10);
        assert_eq!(as_int("k", Some(&toml::Value::Boolean(false)), 10, 1), 10);
    }

    #[test]
    fn as_int_rejects_below_minimum() {
        assert_eq!(as_int("k", Some(&int(0)), 10, 1), 10);
        assert_eq!(as_int("k", Some(&int(-5)), 10, 1), 10);
    }

    #[test]
    fn as_int_rejects_non_int() {
        assert_eq!(
            as_int("k", Some(&toml::Value::String("5".into())), 10, 1),
            10
        );
        assert_eq!(as_int("k", Some(&toml::Value::Float(3.5)), 10, 1), 10);
    }

    #[test]
    fn as_int_missing_key_uses_default() {
        assert_eq!(as_int("k", None, 10, 1), 10);
    }

    // ---- clean_str_list (ports TestCleanStrList) ----

    #[test]
    fn clean_str_list_missing_key_returns_empty() {
        assert!(clean_str_list("k", None).is_empty());
    }

    #[test]
    fn clean_str_list_rejects_bare_string() {
        // Must not be iterated character-by-character.
        let v = toml::Value::String("MEG".into());
        assert!(clean_str_list("k", Some(&v)).is_empty());
    }

    #[test]
    fn clean_str_list_rejects_non_list() {
        assert!(clean_str_list("k", Some(&int(42))).is_empty());
    }

    #[test]
    fn clean_str_list_strips_surrounding_whitespace() {
        let v = toml::Value::Array(vec![toml::Value::String("  MEG Ai1600T  ".into())]);
        assert_eq!(clean_str_list("k", Some(&v)), vec!["MEG Ai1600T"]);
    }

    #[test]
    fn clean_str_list_drops_empty_and_non_string_entries() {
        let v = toml::Value::Array(vec![
            toml::Value::String("a".into()),
            toml::Value::String("".into()),
            toml::Value::String("   ".into()),
            int(5),
            toml::Value::String("b".into()),
        ]);
        assert_eq!(clean_str_list("k", Some(&v)), vec!["a", "b"]);
    }

    // ---- sensor_filter().matches (ports TestMatchesSensor) ----

    fn filter_of(include: &[&str], exclude: &[&str]) -> SensorFilter {
        Config {
            sensor_include: include.iter().map(|s| s.to_string()).collect(),
            sensor_exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..Config::default()
        }
        .sensor_filter()
    }

    #[test]
    fn matches_empty_include_matches_all() {
        let filter = filter_of(&[], &[]);
        assert!(filter.matches("Anything At All"));
    }

    #[test]
    fn matches_include_is_case_insensitive_substring() {
        // Upper-case pattern vs mixed-case name pins the pattern-side
        // normalization done once in sensor_filter().
        let filter = filter_of(&["MEG AI"], &[]);
        assert!(filter.matches("MEG Ai1600T"));
        assert!(!filter.matches("Corsair RM850"));
    }

    #[test]
    fn matches_exclude_is_case_insensitive_substring() {
        let filter = filter_of(&[], &["gpu"]);
        assert!(!filter.matches("GPU Hot Spot"));
        assert!(filter.matches("CPU Package"));
    }

    #[test]
    fn matches_exclude_wins_over_include() {
        let filter = filter_of(&["psu"], &["psu"]);
        assert!(!filter.matches("PSU +12V"));
    }

    // ---- from_toml_str / load (ports TestConfigLoad) ----

    const FULL: &str = r#"
        [general]
        interval_seconds = 5
        log_dir = "custom_logs"
        retention_days = 7

        [sensors]
        include = ["MEG"]
        exclude = ["Virtual"]
    "#;

    #[test]
    fn from_toml_full() {
        let config = Config::from_toml_str(FULL).unwrap();
        assert_eq!(
            config,
            Config {
                interval_seconds: 5,
                log_dir: "custom_logs".to_owned(),
                retention_days: 7,
                sensor_include: vec!["MEG".to_owned()],
                sensor_exclude: vec!["Virtual".to_owned()],
            }
        );
    }

    #[test]
    fn from_toml_missing_keys_use_defaults() {
        assert_eq!(
            Config::from_toml_str("[general]\n").unwrap(),
            Config::default()
        );
        assert_eq!(Config::from_toml_str("").unwrap(), Config::default());
    }

    #[test]
    fn from_toml_invalid_values_fall_back() {
        let text = r#"
            [general]
            interval_seconds = 0
            retention_days = -1
            log_dir = 5

            [sensors]
            include = "MEG"
        "#;
        let config = Config::from_toml_str(text).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn from_toml_str_errors_on_malformed() {
        assert!(Config::from_toml_str("this is not [ valid toml").is_err());
    }

    #[test]
    fn load_malformed_toml_returns_defaults() {
        let dir = TempDir::new();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not [ valid toml").unwrap();
        assert_eq!(Config::load_from(Some(&path), &[]), Config::default());
    }

    #[test]
    fn load_missing_explicit_path_falls_back() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope.toml");
        assert_eq!(Config::load_from(Some(&missing), &[]), Config::default());

        let fallback = dir.path().join("fallback.toml");
        std::fs::write(&fallback, "[general]\ninterval_seconds = 3\n").unwrap();
        let config = Config::load_from(Some(&missing), &[&fallback]);
        assert_eq!(config.interval_seconds, 3);
    }

    #[test]
    fn load_prefers_explicit_over_fallback() {
        let dir = TempDir::new();
        let explicit = dir.path().join("explicit.toml");
        let fallback = dir.path().join("fallback.toml");
        std::fs::write(&explicit, "[general]\ninterval_seconds = 3\n").unwrap();
        std::fs::write(&fallback, "[general]\ninterval_seconds = 7\n").unwrap();
        let config = Config::load_from(Some(&explicit), &[&fallback]);
        assert_eq!(config.interval_seconds, 3);
    }

    // ---- LEO-336: config_path resolution (watch) ----

    #[test]
    fn config_path_prefers_existing_explicit() {
        let dir = TempDir::new();
        let explicit = dir.path().join("explicit.toml");
        let fallback = dir.path().join("fallback.toml");
        std::fs::write(&explicit, "").unwrap();
        std::fs::write(&fallback, "").unwrap();
        assert_eq!(
            Config::config_path_from(Some(&explicit), &[&fallback]),
            Some(explicit)
        );
    }

    #[test]
    fn config_path_falls_back_when_explicit_missing() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope.toml");
        let fallback = dir.path().join("fallback.toml");
        std::fs::write(&fallback, "").unwrap();
        assert_eq!(
            Config::config_path_from(Some(&missing), &[&fallback]),
            Some(fallback)
        );
    }

    #[test]
    fn config_path_is_none_when_nothing_exists() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope.toml");
        assert_eq!(Config::config_path_from(Some(&missing), &[]), None);
        assert_eq!(Config::config_path_from(None, &[]), None);
    }
}

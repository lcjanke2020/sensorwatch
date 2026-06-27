"""Tests for sensorwatch.config — config parsing, validation, and filtering.

Pure logic, no I/O beyond tmp_path TOML files; runs identically on any OS.
"""

import logging
import tomllib

import pytest

from sensorwatch.config import Config, _as_int, _clean_str_list


class TestAsInt:
    def test_accepts_valid_int(self):
        assert _as_int("k", 5, default=10, minimum=1) == 5

    def test_accepts_value_at_minimum(self):
        assert _as_int("k", 1, default=10, minimum=1) == 1

    def test_rejects_bool(self):
        # bool is an int subclass but is never a valid count/interval here.
        assert _as_int("k", True, default=10, minimum=1) == 10
        assert _as_int("k", False, default=10, minimum=0) == 10

    def test_rejects_below_minimum(self):
        assert _as_int("k", 0, default=10, minimum=1) == 10
        assert _as_int("k", -5, default=10, minimum=0) == 10

    def test_rejects_non_int(self):
        assert _as_int("k", "5", default=10, minimum=1) == 10
        assert _as_int("k", 3.5, default=10, minimum=1) == 10

    def test_none_returns_default_without_warning(self, caplog):
        with caplog.at_level(logging.WARNING):
            assert _as_int("k", None, default=10, minimum=1) == 10
        # A missing key (None) is normal — it must not warn.
        assert [r for r in caplog.records if r.levelno >= logging.WARNING] == []

    def test_invalid_value_warns(self, caplog):
        with caplog.at_level(logging.WARNING):
            _as_int("k", "nope", default=10, minimum=1)
        assert any(r.levelno >= logging.WARNING for r in caplog.records)


class TestCleanStrList:
    def test_none_returns_empty(self):
        assert _clean_str_list("k", None) == []

    def test_bare_string_rejected(self):
        # Must NOT iterate a bare string character-by-character.
        assert _clean_str_list("k", "MEG") == []

    def test_non_list_rejected(self):
        assert _clean_str_list("k", 42) == []

    def test_strips_surrounding_whitespace(self):
        assert _clean_str_list("k", ["  MEG Ai1600T  "]) == ["MEG Ai1600T"]

    def test_drops_empty_and_non_str_entries(self):
        assert _clean_str_list("k", ["a", "", "   ", 5, None, "b"]) == ["a", "b"]


class TestMatchesSensor:
    def test_empty_include_matches_all(self):
        cfg = Config()
        assert cfg.matches_sensor("anything at all") is True

    def test_include_is_case_insensitive_substring(self):
        cfg = Config(sensor_include=["meg"])
        assert cfg.matches_sensor("MSI MEG Ai1600T") is True
        assert cfg.matches_sensor("CPU Package") is False

    def test_exclude_is_case_insensitive_substring(self):
        cfg = Config(sensor_exclude=["cpu"])
        assert cfg.matches_sensor("CPU Package") is False
        assert cfg.matches_sensor("GPU Hot Spot") is True

    def test_exclude_wins_over_include(self):
        cfg = Config(sensor_include=["meg"], sensor_exclude=["ai1600"])
        assert cfg.matches_sensor("MEG Ai1600T") is False


class TestConfigLoad:
    def test_from_toml_full(self, tmp_path):
        p = tmp_path / "config.toml"
        p.write_text(
            "[general]\n"
            "interval_seconds = 5\n"
            'log_dir = "out"\n'
            "retention_days = 7\n"
            "[sensors]\n"
            'include = ["MEG"]\n'
            'exclude = ["GPU"]\n',
            encoding="utf-8",
        )
        cfg = Config.from_toml(p)
        assert cfg.interval_seconds == 5
        assert cfg.log_dir == "out"
        assert cfg.retention_days == 7
        assert cfg.sensor_include == ["MEG"]
        assert cfg.sensor_exclude == ["GPU"]

    def test_from_toml_missing_keys_use_defaults(self, tmp_path):
        p = tmp_path / "config.toml"
        p.write_text("[general]\n", encoding="utf-8")
        cfg = Config.from_toml(p)
        assert cfg == Config()

    def test_from_toml_invalid_values_fall_back(self, tmp_path):
        p = tmp_path / "config.toml"
        p.write_text(
            "[general]\n"
            "interval_seconds = 0\n"   # below minimum 1
            "retention_days = -1\n"    # below minimum 0
            "[sensors]\n"
            'include = "MEG"\n',       # bare string, not a list
            encoding="utf-8",
        )
        cfg = Config.from_toml(p)
        assert cfg.interval_seconds == 10
        assert cfg.retention_days == 30
        assert cfg.sensor_include == []

    def test_from_toml_raises_on_malformed(self, tmp_path):
        p = tmp_path / "config.toml"
        p.write_text("this is = = not [[ valid toml", encoding="utf-8")
        with pytest.raises(tomllib.TOMLDecodeError):
            Config.from_toml(p)

    def test_load_malformed_toml_returns_defaults(self, tmp_path):
        # load() swallows a decode error and falls back to defaults.
        p = tmp_path / "config.toml"
        p.write_text("this is = = not [[ valid toml", encoding="utf-8")
        assert Config.load(p) == Config()

    def test_load_missing_file_does_not_raise(self, tmp_path):
        # An explicit but absent path falls through to the bundled-config lookup
        # and ultimately defaults; it must never raise.
        cfg = Config.load(tmp_path / "does_not_exist.toml")
        assert isinstance(cfg, Config)

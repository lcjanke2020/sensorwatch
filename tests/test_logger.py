"""Tests for sensorwatch.logger — JSONL record shape, daily rotation, retention.

Nothing depends on the wall clock: the write/rotation tests inject explicit
timestamps, and the retention-cleanup tests freeze ``pendulum.today`` (the clock
``_cleanup_old_files`` reads) via the ``frozen_today`` fixture. File operations
use pytest's tmp_path.
"""

import json

import pendulum
import pytest

from sensorwatch.logger import LOG_PREFIX, SensorLogger

# Fixed reference date for retention tests (a Monday; value is arbitrary).
FROZEN_TODAY = pendulum.datetime(2026, 6, 15, 12, 0, 0, tz="UTC")


@pytest.fixture
def frozen_today(monkeypatch):
    """Freeze ``pendulum.today`` so retention-cutoff math is deterministic."""
    monkeypatch.setattr(pendulum, "today", lambda tz="local": FROZEN_TODAY)
    return FROZEN_TODAY


def _read_jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def test_write_record_shape(tmp_path):
    ts = pendulum.datetime(2026, 2, 18, 8, 17, 48, tz="UTC")
    readings = [
        {"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03},
    ]
    with SensorLogger(tmp_path, retention_days=30) as logger:
        logger.write(readings, timestamp=ts)

    path = tmp_path / f"{LOG_PREFIX}2026-02-18.jsonl"
    assert path.exists()

    records = _read_jsonl(path)
    assert len(records) == 1
    rec = records[0]
    assert set(rec.keys()) == {"timestamp", "sensors"}
    assert rec["timestamp"] == ts.to_iso8601_string()
    assert rec["sensors"] == readings


def test_daily_rollover_opens_new_file(tmp_path):
    day1 = pendulum.datetime(2026, 2, 18, 23, 59, 0, tz="UTC")
    day2 = day1.add(days=1)
    # retention_days=0 disables cleanup so the (synthetic, long-past) day1 file
    # survives the rollover and we test rotation in isolation.
    with SensorLogger(tmp_path, retention_days=0) as logger:
        logger.write([{"n": 1}], timestamp=day1)
        logger.write([{"n": 2}], timestamp=day2)

    f1 = tmp_path / f"{LOG_PREFIX}2026-02-18.jsonl"
    f2 = tmp_path / f"{LOG_PREFIX}2026-02-19.jsonl"
    assert f1.exists() and f2.exists()
    assert len(_read_jsonl(f1)) == 1
    assert len(_read_jsonl(f2)) == 1
    assert _read_jsonl(f1)[0]["sensors"] == [{"n": 1}]
    assert _read_jsonl(f2)[0]["sensors"] == [{"n": 2}]


def test_cleanup_removes_files_older_than_retention(tmp_path, frozen_today):
    old_file = tmp_path / f"{LOG_PREFIX}{frozen_today.subtract(days=40).to_date_string()}.jsonl"
    recent_file = tmp_path / f"{LOG_PREFIX}{frozen_today.subtract(days=1).to_date_string()}.jsonl"
    old_file.write_text("{}\n", encoding="utf-8")
    recent_file.write_text("{}\n", encoding="utf-8")

    # Constructor runs _cleanup_old_files with a 30-day window.
    SensorLogger(tmp_path, retention_days=30).close()

    assert not old_file.exists()
    assert recent_file.exists()


def test_cleanup_skips_malformed_and_unrelated_filenames(tmp_path, frozen_today):
    old_valid = tmp_path / f"{LOG_PREFIX}{frozen_today.subtract(days=40).to_date_string()}.jsonl"
    malformed = tmp_path / f"{LOG_PREFIX}not-a-date.jsonl"
    unrelated = tmp_path / "unrelated.jsonl"
    for f in (old_valid, malformed, unrelated):
        f.write_text("{}\n", encoding="utf-8")

    SensorLogger(tmp_path, retention_days=30).close()

    assert not old_valid.exists()   # old + parseable date -> removed
    assert malformed.exists()       # unparseable date -> skipped
    assert unrelated.exists()       # outside the glob -> untouched


def test_cleanup_disabled_when_retention_non_positive(tmp_path, frozen_today):
    ancient = tmp_path / f"{LOG_PREFIX}{frozen_today.subtract(days=999).to_date_string()}.jsonl"
    ancient.write_text("{}\n", encoding="utf-8")

    SensorLogger(tmp_path, retention_days=0).close()

    assert ancient.exists()

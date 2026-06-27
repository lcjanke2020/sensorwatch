"""JSONL logger with daily file rotation and retention cleanup."""

from __future__ import annotations

import json
import logging
from pathlib import Path

import pendulum

log = logging.getLogger(__name__)

# Prefix for daily log files: logs/<LOG_PREFIX><YYYY-MM-DD>.jsonl
LOG_PREFIX = "sensors_"


class SensorLogger:
    """Writes sensor readings as JSON Lines with daily file rotation."""

    def __init__(self, log_dir: str | Path, retention_days: int = 30):
        self.log_dir = Path(log_dir)
        self.retention_days = retention_days
        self._current_date: pendulum.Date | None = None
        self._file = None

        self.log_dir.mkdir(parents=True, exist_ok=True)
        self._cleanup_old_files()

    def _log_path(self, d: pendulum.Date) -> Path:
        return self.log_dir / f"{LOG_PREFIX}{d.to_date_string()}.jsonl"

    def _ensure_file(self, now: pendulum.DateTime) -> None:
        """Open a new file if the date has rolled over."""
        today = now.date()
        if self._current_date == today and self._file is not None:
            return

        rolled_over = self._current_date is not None
        self.close()
        self._current_date = today
        path = self._log_path(today)
        log.info("Opening log file: %s", path)
        self._file = open(path, "a", encoding="utf-8")
        # Re-run retention on each daily rollover so a long-running process
        # purges old files without needing a restart.
        if rolled_over:
            self._cleanup_old_files()

    def write(self, readings: list[dict], timestamp: pendulum.DateTime | None = None) -> None:
        """Write a single sample (all readings at one timestamp) as one JSONL record."""
        now = timestamp or pendulum.now("local")
        record = {
            "timestamp": now.to_iso8601_string(),
            "sensors": readings,
        }
        try:
            self._ensure_file(now)
            self._file.write(json.dumps(record, ensure_ascii=False) + "\n")
            self._file.flush()
        except OSError as exc:
            # Disk full, permission denied, etc. — log and keep the monitor alive.
            log.warning("Failed to write log record (%s)", exc)

    def close(self) -> None:
        if self._file is not None:
            self._file.close()
            self._file = None

    def _cleanup_old_files(self) -> None:
        """Delete log files older than retention_days."""
        if self.retention_days <= 0:
            return

        cutoff = pendulum.today("local").subtract(days=self.retention_days).date()
        removed = 0
        for path in self.log_dir.glob(f"{LOG_PREFIX}*.jsonl"):
            try:
                file_date = pendulum.parse(path.stem.removeprefix(LOG_PREFIX)).date()
            except ValueError:
                continue
            if file_date < cutoff:
                path.unlink()
                removed += 1

        if removed:
            log.info("Cleaned up %d log file(s) older than %d days", removed, self.retention_days)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

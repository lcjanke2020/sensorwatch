"""Configuration loading for sensorwatch."""

import logging
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

log = logging.getLogger(__name__)


def _clean_str_list(items: list) -> list[str]:
    """Filter a list to only non-empty strings."""
    return [s for s in items if isinstance(s, str) and s.strip()]


@dataclass
class Config:
    interval_seconds: int = 10
    log_dir: str = "logs"
    retention_days: int = 30
    sensor_include: list[str] = field(default_factory=list)
    sensor_exclude: list[str] = field(default_factory=list)

    @classmethod
    def from_toml(cls, path: Path) -> "Config":
        """Load config from a TOML file, falling back to defaults for missing keys."""
        defaults = cls()
        with open(path, "rb") as f:
            data = tomllib.load(f)

        general = data.get("general", {})
        sensors = data.get("sensors", {})

        return cls(
            interval_seconds=general.get("interval_seconds", defaults.interval_seconds),
            log_dir=general.get("log_dir", defaults.log_dir),
            retention_days=general.get("retention_days", defaults.retention_days),
            sensor_include=_clean_str_list(sensors.get("include", defaults.sensor_include)),
            sensor_exclude=_clean_str_list(sensors.get("exclude", defaults.sensor_exclude)),
        )

    @classmethod
    def load(cls, path: Path | None = None) -> "Config":
        """Load config from file if it exists, otherwise use defaults."""
        try:
            if path and path.exists():
                return cls.from_toml(path)

            # Try default location next to the package
            default = Path(__file__).resolve().parent.parent / "config.toml"
            if default.exists():
                return cls.from_toml(default)
        except (tomllib.TOMLDecodeError, OSError) as exc:
            log.warning("Failed to load config (%s), using defaults", exc)

        return cls()

    def matches_sensor(self, sensor_name: str) -> bool:
        """Check if a sensor name matches the include/exclude filters."""
        name_lower = sensor_name.lower()

        # If include list is empty, include everything
        if self.sensor_include:
            if not any(pat.lower() in name_lower for pat in self.sensor_include):
                return False

        # Exclude always applies
        if any(pat.lower() in name_lower for pat in self.sensor_exclude):
            return False

        return True

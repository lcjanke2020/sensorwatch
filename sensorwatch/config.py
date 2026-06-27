"""Configuration loading for sensorwatch."""

import logging
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

log = logging.getLogger(__name__)


def _clean_str_list(key: str, items: object) -> list[str]:
    """Coerce a config value to a list of non-empty strings.

    Non-list values (e.g. a bare ``include = "MEG"`` string) are rejected with a
    warning rather than silently iterated character-by-character.
    """
    if items is None:
        return []
    if not isinstance(items, list):
        log.warning("Config '%s' must be a list of strings, got %s; ignoring", key, type(items).__name__)
        return []
    # Strip surrounding whitespace so a pattern like " MEG Ai1600T " still matches.
    return [s.strip() for s in items if isinstance(s, str) and s.strip()]


def _as_int(key: str, value: object, default: int, minimum: int) -> int:
    """Coerce a config value to an int >= minimum, falling back to default."""
    # bool is an int subclass but is never a valid count/interval here.
    if isinstance(value, bool) or not isinstance(value, int):
        if value is not None:
            log.warning("Config '%s' must be an integer, got %r; using %d", key, value, default)
        return default
    if value < minimum:
        log.warning("Config '%s' (%d) is below minimum %d; using %d", key, value, minimum, default)
        return default
    return value


@dataclass
class Config:
    interval_seconds: int = 10
    log_dir: str = "logs"
    retention_days: int = 30
    sensor_include: list[str] = field(default_factory=list)
    sensor_exclude: list[str] = field(default_factory=list)

    @classmethod
    def from_toml(cls, path: Path) -> "Config":
        """Load config from a TOML file, falling back to defaults for missing/invalid keys."""
        defaults = cls()
        with open(path, "rb") as f:
            data = tomllib.load(f)

        general = data.get("general", {})
        sensors = data.get("sensors", {})

        return cls(
            interval_seconds=_as_int(
                "interval_seconds", general.get("interval_seconds"), defaults.interval_seconds, minimum=1),
            log_dir=str(general.get("log_dir", defaults.log_dir)),
            retention_days=_as_int(
                "retention_days", general.get("retention_days"), defaults.retention_days, minimum=0),
            sensor_include=_clean_str_list("sensors.include", sensors.get("include")),
            sensor_exclude=_clean_str_list("sensors.exclude", sensors.get("exclude")),
        )

    @classmethod
    def load(cls, path: Path | None = None) -> "Config":
        """Load config from file if it exists, otherwise use defaults."""
        try:
            if path and path.exists():
                return cls.from_toml(path)

            # Look for a bundled config next to the package first (wheel install),
            # then the project root (editable/dev checkout).
            here = Path(__file__).resolve().parent
            for candidate in (here / "config.toml", here.parent / "config.toml"):
                if candidate.exists():
                    return cls.from_toml(candidate)
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

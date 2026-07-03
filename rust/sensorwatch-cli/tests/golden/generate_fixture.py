"""Generate the golden JSONL fixture for the Rust `log` subcommand's
byte-compatibility test, using the frozen Python logger as the oracle.

Run from the repo root (Linux/macOS — the fixture is committed with LF
endings, protected by .gitattributes):

    uv run python rust/sensorwatch-cli/tests/golden/generate_fixture.py

The Rust test `logger::tests::golden_bytes_match_python_fixture` replays the
same timestamps and readings through `LogWriter` and byte-compares the
resulting files, locking in the JSON separators, key order, timestamp
rendering, float formatting, and daily rotation.

The content deliberately avoids the three documented divergences so the
comparison can be exact: every timestamp has nonzero microseconds (the Rust
port always writes six fractional digits where pendulum omits them at zero),
only known reading types appear (unknown codes render differently), and all
values are finite floats (non-finite render as null in Rust) written as
Python floats, never ints (Python would render `1`, Rust `1.0`).
"""

import pathlib
import sys

import pendulum

REPO_ROOT = pathlib.Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO_ROOT))

from sensorwatch.logger import SensorLogger  # noqa: E402

GOLDEN_DIR = pathlib.Path(__file__).resolve().parent

EST = pendulum.FixedTimezone(-5 * 3600)  # -05:00
IST = pendulum.FixedTimezone(5 * 3600 + 30 * 60)  # +05:30

RECORDS = [
    (
        pendulum.datetime(2026, 2, 18, 8, 17, 48, 123456, tz=EST),
        [
            {"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage",
             "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"},
            {"sensor": "MEG Ai1600T", "reading": "PSU Temp", "type": "Temperature",
             "value": 45.5, "min": 44.0, "max": 47.25, "avg": 45.75, "unit": "°C"},
            {"sensor": "MEG Ai1600T", "reading": "Fan 1", "type": "Fan",
             "value": 1210.0, "min": 0.0, "max": 1650.0, "avg": 1180.5, "unit": "RPM"},
        ],
    ),
    (
        pendulum.datetime(2026, 2, 18, 20, 0, 0, 42, tz=IST),
        [
            {"sensor": "CPU Package", "reading": "Core Clock", "type": "Clock",
             "value": 4550.0, "min": 800.0, "max": 5125.0, "avg": 3901.25, "unit": "MHz"},
            {"sensor": "CPU Package", "reading": "Offset Rail", "type": "Other",
             "value": -0.125, "min": -0.5, "max": 0.007, "avg": -0.0625, "unit": ""},
        ],
    ),
    (
        pendulum.datetime(2026, 2, 19, 23, 59, 59, 999999, tz="UTC"),
        [
            {"sensor": "GPU", "reading": "GPU Usage", "type": "Usage",
             "value": 87.5, "min": 0.0, "max": 100.0, "avg": 42.25, "unit": "%"},
            {"sensor": "GPU", "reading": "Nothing", "type": "None",
             "value": 1.0, "min": 1.0, "max": 1.0, "avg": 1.0, "unit": ""},
        ],
    ),
]


def main() -> None:
    for stale in GOLDEN_DIR.glob("sensors_*.jsonl"):
        stale.unlink()
    with SensorLogger(GOLDEN_DIR, retention_days=0) as logger:
        for timestamp, readings in RECORDS:
            logger.write(readings, timestamp=timestamp)
    for path in sorted(GOLDEN_DIR.glob("sensors_*.jsonl")):
        print(f"wrote {path.relative_to(REPO_ROOT)} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

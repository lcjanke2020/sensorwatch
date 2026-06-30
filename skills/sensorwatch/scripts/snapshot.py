#!/usr/bin/env python3
"""One-shot sensorwatch snapshot -> JSON.

Prints the current hardware sensor readings (from HWiNFO64, via the sensorwatch
native binding) as a JSON array on stdout. A quick, agent-friendly way to grab
the live hardware state without standing up the logger.

The "type" label matches the CLI logger's JSON Lines vocabulary (title-case, e.g.
"Temperature"), so a snapshot and a logged record describe a reading the same way.

Exit codes:
  0  a snapshot was printed (possibly an empty array)
  1  sensorwatch / HWiNFO is unavailable -- not Windows, HWiNFO not running with
     Shared Memory Support enabled, or the native extension is not built

Examples:
  python snapshot.py
  python snapshot.py --type TEMPERATURE
  python snapshot.py --match 12V
  python snapshot.py --indent 0        # single compact line
"""

from __future__ import annotations

import argparse
import json
import sys


def _nonnegative_int(value: str) -> int:
    """argparse type: an int >= 0, so --indent rejects negatives up front."""
    ivalue = int(value)
    if ivalue < 0:
        raise argparse.ArgumentTypeError("must be >= 0")
    return ivalue


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Print a live sensorwatch snapshot as JSON."
    )
    parser.add_argument(
        "--type",
        dest="type_filter",
        default=None,
        help="Only include readings of this ReadingType "
        "(e.g. TEMPERATURE, VOLTAGE, FAN, CURRENT, POWER, CLOCK, USAGE).",
    )
    parser.add_argument(
        "--match",
        default=None,
        help="Only include readings whose sensor or reading name contains this "
        "substring (case-insensitive).",
    )
    parser.add_argument(
        "--indent",
        type=_nonnegative_int,
        default=2,
        help="JSON indent in spaces (default 2). Use 0 for a single compact line.",
    )
    args = parser.parse_args()

    # Import lazily so a missing/unbuilt extension is reported cleanly here
    # rather than as an import error at startup.
    try:
        from sensorwatch.native import Session, SensorwatchError
    except ImportError as exc:
        print(
            f"sensorwatch native binding unavailable: {exc}\n"
            "Install a sensorwatch wheel (`pip install sensorwatch`) or build it "
            "from source with a C compiler.",
            file=sys.stderr,
        )
        return 1

    # The canonical reading-type labels, shared with the pure-Python reader and
    # the CLI logger, so this helper's "type" matches a logged record's "type".
    from sensorwatch.hwinfo_shm import SENSOR_TYPES

    try:
        with Session() as session:        # raises off-Windows or if HWiNFO is down
            # Materialize readings while the snapshot is open so its native
            # allocation is freed promptly; Reading objects are plain dataclasses
            # and stay valid afterwards.
            with session.snapshot() as snapshot:
                readings = list(snapshot)
    except SensorwatchError as exc:
        print(
            f"Could not read sensors: {exc}\n"
            "Ensure you are on Windows and HWiNFO64 is running with Shared Memory "
            "Support enabled and the sensors window open.",
            file=sys.stderr,
        )
        return 1

    match: str | None = args.match.lower() if args.match else None
    type_filter: str | None = args.type_filter.upper() if args.type_filter else None

    out: list[dict] = []
    for r in readings:
        if type_filter and r.type.name != type_filter:
            continue
        if match and match not in r.sensor.lower() and match not in r.reading.lower():
            continue
        out.append(
            {
                "source": r.source,
                "sensor": r.sensor,
                "reading": r.reading,
                # Title-case label (e.g. "Temperature") matching the logger's
                # JSONL; ReadingType.UNKNOWN (255) falls back to "Unknown".
                "type": SENSOR_TYPES.get(int(r.type), r.type.name.title()),
                "value": r.value,
                "min": r.minimum,
                "max": r.maximum,
                "avg": r.average,
                "unit": r.unit,
            }
        )

    indent = args.indent or None  # 0 -> None -> single compact line
    print(json.dumps(out, indent=indent, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""heartbeat.py — record watcher liveness and the maintenance marker.

The wake-up loop updates durable liveness state through this script (so the
"helper scripts do every mechanical write" rule holds and no agent hand-edits
JSON). One `--kind` per call:

  * `heartbeat` — the watcher ran fine (a clean timeout, or it emitted an event):
    set `last_heartbeat = now`, reset `consecutive_watch_failures = 0`.
  * `failure`   — the watcher died fatally (exit 1): increment
    `consecutive_watch_failures`. The result's `monitoring_blind` flips true at
    `--blind-after` (default 3) consecutive failures — the cue to open a
    monitoring-blind incident (a blind monitor is itself reportable).
  * `maintenance` — set `last_maintenance_date = now`'s date and journal a
    `maintenance` line (run on the first heartbeat after midnight).

    python heartbeat.py --state-dir <dir> --kind heartbeat|failure|maintenance \\
        [--blind-after 3] [--now <iso>]

JSON result on stdout; exit 0 success, 1 fatal (corrupt state), 2 usage.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="heartbeat.py",
        description="Record watcher liveness / the maintenance marker.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--kind", required=True, choices=("heartbeat", "failure", "maintenance"))
    parser.add_argument("--blind-after", type=int, default=3,
                        help="consecutive failures that mark the monitor blind (default 3)")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def _load_heartbeat(state_dir: Path) -> dict:
    hb = st.load_json(state_dir / "heartbeat.json")
    if not st._is_int(hb.get("consecutive_watch_failures", 0)):
        raise st.Fatal("heartbeat.json: consecutive_watch_failures is not an int")
    return hb


def run(args: argparse.Namespace) -> int:
    if args.blind_after < 1:
        raise st.Usage("--blind-after must be >= 1")
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    hb_path = state_dir / "heartbeat.json"
    hb = _load_heartbeat(state_dir)

    if args.kind == "heartbeat":
        hb["last_heartbeat"] = st.iso(now)
        hb["consecutive_watch_failures"] = 0
        st.save_json_atomic(hb_path, hb)
        st.emit({"kind": "heartbeat", "last_heartbeat": hb["last_heartbeat"],
                 "consecutive_watch_failures": 0})
        return 0

    if args.kind == "failure":
        failures = hb.get("consecutive_watch_failures", 0) + 1
        hb["consecutive_watch_failures"] = failures
        st.save_json_atomic(hb_path, hb)
        st.emit({"kind": "failure", "consecutive_watch_failures": failures,
                 "monitoring_blind": failures >= args.blind_after})
        return 0

    # maintenance — journal first (the bundle's recording-before-state anchor),
    # then stamp the date.
    st.journal_append(state_dir, now, "maintenance", detail={"date": st.date_str(now)})
    hb["last_maintenance_date"] = st.date_str(now)
    st.save_json_atomic(hb_path, hb)
    st.emit({"kind": "maintenance", "last_maintenance_date": hb["last_maintenance_date"]})
    return 0


def main(argv: list[str] | None = None) -> int:
    st.force_utf8_io()
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except (st.Usage, st.Fatal) as exc:
        return st.die(exc)


if __name__ == "__main__":
    raise SystemExit(main())

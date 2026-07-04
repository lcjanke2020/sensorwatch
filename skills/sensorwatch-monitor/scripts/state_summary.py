#!/usr/bin/env python3
"""state_summary.py — THE bootstrap read for a fresh (or compacted) session.

Emits, to stdout, under a hard ``--max-bytes`` cap (default 4096):

  1. bootstrap.md verbatim  (the header — NEVER truncated)
  2. cursor + heartbeat one-liners
  3. today's escalation counters
  4. open incidents, one line each (rule, opened, snooze_until, event count)
  5. spool/pending count + lowest pending seq

This is the ONLY sanctioned way to reconstruct the monitor from disk — disk is
the source of truth, the context window is a cache. Truncation drops
oldest-incident detail first and never the header; if the essential floor
(header + status one-liners) itself exceeds the cap, that is exit 1 — the state
needs a maintenance pass, and the message says which capped file to trim.

    python state_summary.py --state-dir <dir> [--max-bytes 4096] [--now <iso>]

Exit 0 success, 1 fatal (unreadable state / cap floor exceeded), 2 usage.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="state_summary.py",
        description="Emit the bounded state summary (the bootstrap read).",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--max-bytes", type=int, default=st.SUMMARY_MAX_BYTES)
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def _field(lines: list[str], key: str, default: str = "?") -> str:
    prefix = f"- {key}:"
    for line in lines:
        if line.startswith(prefix):
            return line[len(prefix):].strip() or default
    return default


def _read_open_incidents(state_dir: Path) -> list[dict]:
    open_dir = state_dir / "incidents" / "open"
    incidents = []
    if not open_dir.is_dir():
        return incidents
    for path in sorted(open_dir.glob("*.md")):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        incidents.append({
            "rule": _field(lines, "rule", path.stem),
            "opened": _field(lines, "opened"),
            "snooze_until": _field(lines, "snooze_until"),
            "events": _field(lines, "events", "?"),
        })
    return incidents


def _pending_stats(state_dir: Path) -> tuple[int, int | None]:
    pending = state_dir / "spool" / "pending"
    if not pending.is_dir():
        return 0, None
    seqs = []
    count = 0
    for path in pending.glob("*.json"):
        count += 1
        digits = ""
        for ch in path.name:
            if ch.isdigit():
                digits += ch
            else:
                break
        if digits:
            seqs.append(int(digits))
    return count, (min(seqs) if seqs else None)


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    max_bytes = args.max_bytes

    bootstrap_path = state_dir / "bootstrap.md"
    try:
        header = bootstrap_path.read_text(encoding="utf-8").rstrip("\n")
    except OSError as exc:
        raise st.Fatal(f"cannot read bootstrap.md: {exc} (run init_state.py?)") from exc

    cursor = st.load_json(state_dir / "cursor.json")
    heartbeat = st.load_json(state_dir / "heartbeat.json")
    esc = st.load_json(state_dir / "escalation.json")

    today = st.date_str(now)
    notifications_today = esc.get("notifications_today", 0) if esc.get("date") == today else 0
    open_rules = sum(1 for info in esc.get("per_rule", {}).values() if info.get("open"))

    pending_count, lowest_seq = _pending_stats(state_dir)
    incidents = _read_open_incidents(state_dir)

    status_lines = [
        f"## monitor status @ {st.iso(now)}",
        f"cursor: last_acked_seq={cursor.get('last_acked_seq', 0)}, "
        f"acked_ids={len(cursor.get('acked_ids_recent', []))} recent, "
        f"updated={cursor.get('updated', '?')}",
        f"heartbeat: last={heartbeat.get('last_heartbeat', '?')}, "
        f"watch_failures={heartbeat.get('consecutive_watch_failures', 0)}, "
        f"last_maintenance={heartbeat.get('last_maintenance_date', '?')}",
        f"escalation: notifications_today={notifications_today}, open_rules={open_rules}",
        f"spool: pending={pending_count}, "
        f"lowest_pending_seq={'none' if lowest_seq is None else lowest_seq}",
        f"open incidents ({len(incidents)}):",
    ]
    base = header + "\n\n" + "\n".join(status_lines)

    base_bytes = len(base.encode("utf-8"))
    if base_bytes > max_bytes:
        header_bytes = len(header.encode("utf-8"))
        header_lines = header.count("\n") + 1
        if header_bytes > max_bytes // 2 or header_lines > st.BOOTSTRAP_LINE_CAP:
            culprit = (
                f"bootstrap.md ({header_lines} lines / {header_bytes}B, "
                f"cap {st.BOOTSTRAP_LINE_CAP} lines)"
            )
        else:
            culprit = "the status block"
        raise st.Fatal(
            f"summary floor {base_bytes}B exceeds --max-bytes "
            f"{max_bytes}; {culprit} needs the maintenance pass"
        )

    # Detail lines, newest-first, so truncation drops the OLDEST first.
    detail = sorted(incidents, key=lambda i: i["opened"], reverse=True)
    detail_lines = [
        f"  {i['rule']}  opened={i['opened']}  snooze_until={i['snooze_until']}  events={i['events']}"
        for i in detail
    ]

    output = base
    kept = 0
    for line in detail_lines:
        trial = output + "\n" + line
        if len(trial.encode("utf-8")) <= max_bytes:
            output = trial
            kept += 1
        else:
            break

    omitted = len(detail_lines) - kept
    if omitted:
        marker = f"\n  ... {omitted} older incident(s) omitted — run the maintenance pass"
        while kept > 0 and len((output + marker).encode("utf-8")) > max_bytes:
            output = output.rsplit("\n", 1)[0]
            kept -= 1
            omitted += 1
            marker = f"\n  ... {omitted} older incident(s) omitted — run the maintenance pass"
        if len((output + marker).encode("utf-8")) <= max_bytes:
            output = output + marker

    sys.stdout.write(output + "\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except (st.Usage, st.Fatal) as exc:
        return st.die(exc)


if __name__ == "__main__":
    raise SystemExit(main())

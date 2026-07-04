#!/usr/bin/env python3
"""init_state.py — create (or top up) a sensorwatch-monitor state directory.

Idempotent: existing files are left untouched (human-curated bootstrap.md /
baseline.md and the JSON ledgers are never clobbered); only missing pieces are
created. WARNS — does not refuse — when the target is inside a git work tree,
because monitor state (baselines, thresholds, incidents) is machine-local and
must never be committed.

    python init_state.py --state-dir <dir> [--now <iso>]

Result JSON on stdout; exit 0 success, 1 fatal, 2 usage.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402

TEMPLATES = Path(__file__).parent.parent / "templates"

BASELINE_STUB = """\
<!--
  baseline.md — the curated "normal" for this machine, in prose. Machine-local,
  not in git. Cap: 150 lines. The maintenance pass re-curates this toward the cap
  (trim stale ranges, fold in what a week of quiet taught you). This is what
  triage compares an event against — keep it honest and short.
-->

# Baseline — what "normal" looks like here

<!-- One short section per watched series: typical range, and what a real
     excursion (vs sensor noise) looks like. Filled in on the monitored machine;
     do not paste real machine-tuned numbers into a copy you might share. -->
"""

# Sub-directories that make up the state tree.
DIRS = (
    "journal",
    "incidents/open",
    "incidents/closed",
    "spool/pending",
    "spool/acked",
    "outbox",
)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="init_state.py",
        description="Create or top up a sensorwatch-monitor state directory.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--now", help="ISO-8601 timestamp to stamp state with (default: now UTC)")
    return parser


def _write_if_absent(path: Path, content: str, created: list, existed: list) -> None:
    if path.exists():
        existed.append(path.name)
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    created.append(path.name)


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)

    created: list[str] = []
    existed: list[str] = []

    state_dir.mkdir(parents=True, exist_ok=True)
    for rel in DIRS:
        (state_dir / rel).mkdir(parents=True, exist_ok=True)

    # Markdown (human + agent judgment) — bootstrap from the template, baseline
    # from the inline stub.
    try:
        bootstrap_template = (TEMPLATES / "bootstrap.md").read_text(encoding="utf-8")
    except OSError as exc:
        raise st.Fatal(f"cannot read bootstrap template: {exc}") from exc
    _write_if_absent(state_dir / "bootstrap.md", bootstrap_template, created, existed)
    _write_if_absent(state_dir / "baseline.md", BASELINE_STUB, created, existed)

    # JSON ledgers (machine-updated).
    cursor = state_dir / "cursor.json"
    if cursor.exists():
        existed.append(cursor.name)
    else:
        st.save_json_atomic(cursor, {
            "schema_version": st.SCHEMA_VERSION,
            "last_acked_seq": 0,
            "acked_ids_recent": [],
            "updated": st.iso(now),
        })
        created.append(cursor.name)

    heartbeat = state_dir / "heartbeat.json"
    if heartbeat.exists():
        existed.append(heartbeat.name)
    else:
        st.save_json_atomic(heartbeat, {
            "schema_version": st.SCHEMA_VERSION,
            "last_heartbeat": st.iso(now),
            "consecutive_watch_failures": 0,
            "last_maintenance_date": st.date_str(now),
        })
        created.append(heartbeat.name)

    escalation = state_dir / "escalation.json"
    if escalation.exists():
        existed.append(escalation.name)
    else:
        st.save_json_atomic(escalation, {
            "schema_version": st.SCHEMA_VERSION,
            "per_rule": {},
            "date": st.date_str(now),
            "notifications_today": 0,
        })
        created.append(escalation.name)

    worktree = st.find_git_worktree(state_dir)
    if worktree is not None:
        print(
            f"warning: state dir is inside a git work tree ({worktree}); monitor "
            f"state must never be committed — prefer a machine-local location",
            file=sys.stderr,
        )

    st.journal_append(
        state_dir, now, "init",
        detail={"created": created, "existed": existed},
    )

    st.emit({
        "state_dir": str(state_dir),
        "created": created,
        "existed": existed,
        "in_git_worktree": worktree is not None,
    })
    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except (st.Usage, st.Fatal) as exc:
        return st.die(exc)


if __name__ == "__main__":
    raise SystemExit(main())

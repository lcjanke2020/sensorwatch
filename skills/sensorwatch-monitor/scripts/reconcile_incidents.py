#!/usr/bin/env python3
"""reconcile_incidents.py — auto-close recovered incidents from a report digest.

The arm-per-wake ``watch`` tracks fired/cleared per process, so a rule that
recovers while no watcher is armed never emits a cross-restart ``cleared``
event — its incident would stay open forever. This script closes that gap on
the heartbeat wake: it reads the digest the wake's single ``report`` call
produced (``report`` re-derives violations by replaying the same engine) and
reconciles every ``incidents/open/*.md`` against it.

    python reconcile_incidents.py --state-dir <dir> --digest <report.json> \\
        [--dry-run] [--now <iso>]

Verdict per open incident, from the digest's ``violations[]`` transitions:

* ``recovered``    — the rule's latest in-window transition is ``cleared``.
  Closed via open_incident.py's ``--close`` path (journal-first, atomic
  open→closed move), using the digest's re-derived cleared event.
* ``still-firing`` — the latest transition is ``fired``. No change.
* ``indeterminate`` — no transition for the rule in the window (the window may
  not span the fire; rules the engine no longer evaluates land here too), or
  the freshness gate failed. Never closed: absence of evidence is not
  recovery. These stay manual.

Trust boundaries, in order:

1. **Freshness gate.** If the digest has zero samples, or ``meta.last_sample``
   trails ``meta.window.until`` by more than 3× the sampling interval (the
   same multiple the digest's own gap detection uses), every verdict is
   ``indeterminate`` — a dead or stalled logger must never look like recovery.
2. **Latest transition wins.** ``violations[]`` is chronological, and the
   digest's byte-cap fitter drops violations oldest-first, so the surviving
   transitions are a chronological suffix: the last shown match for a rule IS
   that rule's latest transition. A rule whose transitions were all truncated
   away simply shows none → ``indeterminate`` (conservative by construction).

Also emits a ``logger_health`` block computed from the same digest (``gaps[]``
vs the window length) — the deterministic input for the SKILL's escalate-on-
gap-density step. Thresholds: density above ``GAP_DENSITY_THRESHOLD`` of the
window, or any single gap over ``SINGLE_GAP_MAX_SECONDS``, is ``degraded``.

Read-only against incidents except for closes; ``--dry-run`` writes nothing
(no journal, no close) and reports what would happen. JSON result on stdout;
exit 0 success (including nothing-to-do), 1 fatal, 2 usage (unreadable or
malformed digest).
"""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402
import open_incident  # noqa: E402

DIGEST_SCHEMA_VERSION = 1

# Freshness: last_sample may trail window.until by at most this multiple of the
# sampling interval — the same 3× the digest's own gap detection uses.
FRESHNESS_INTERVAL_MULTIPLE = 3

# logger_health thresholds (restated in SKILL.md's heartbeat procedure).
GAP_DENSITY_THRESHOLD = 0.10   # >10% of the window inside gaps → degraded
SINGLE_GAP_MAX_SECONDS = 900   # any one gap >15 min → degraded


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="reconcile_incidents.py",
        description="Auto-close recovered incidents against a report digest.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument(
        "--digest", required=True,
        help="path to a saved `sensorwatch report` JSON digest",
    )
    parser.add_argument(
        "--dry-run", action="store_true",
        help="report verdicts only; write nothing (no journal, no close)",
    )
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def _load_digest(path: Path) -> dict:
    """Read + shape-check the digest. The digest is caller-provided input (the
    redirected stdout of the wake's report call), so problems are Usage (exit
    2), never Fatal — a bad digest must not look like corrupt monitor state."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise st.Usage(f"cannot read digest {path}: {exc}") from exc
    try:
        digest = json.loads(text)
    except json.JSONDecodeError as exc:
        raise st.Usage(f"digest {path} is not JSON: {exc}") from exc
    if not isinstance(digest, dict):
        raise st.Usage("digest is not a JSON object")
    if digest.get("schema_version") != DIGEST_SCHEMA_VERSION:
        raise st.Usage(
            f"unsupported digest schema_version {digest.get('schema_version')!r} "
            f"(this consumer pins {DIGEST_SCHEMA_VERSION})"
        )
    meta = digest.get("meta")
    if not isinstance(meta, dict):
        raise st.Usage("digest has no meta object")
    for key in ("violations", "gaps"):
        if not isinstance(digest.get(key), list):
            raise st.Usage(f"digest {key!r} is not a list")
    return digest


def _window_seconds(meta: dict) -> int:
    window = meta.get("window") or {}
    since = st.parse_iso(str(window.get("since")))
    until = st.parse_iso(str(window.get("until")))
    seconds = int((until - since).total_seconds())
    if seconds <= 0:
        raise st.Usage(f"digest window is empty or inverted ({window})")
    return seconds


def _check_freshness(meta: dict) -> tuple[bool, str]:
    """(fresh, reason). Zero samples or a stale last_sample means the digest
    cannot prove recovery for anything."""
    samples = meta.get("samples", 0)
    if not samples:
        return False, "no samples in window (logger dead or window empty)"
    last_sample = meta.get("last_sample")
    if not last_sample:
        return False, "digest has samples but no last_sample timestamp"
    until = st.parse_iso(str((meta.get("window") or {}).get("until")))
    last = st.parse_iso(str(last_sample))
    interval = meta.get("interval_seconds") or 0
    allowed = max(int(interval), 1) * FRESHNESS_INTERVAL_MULTIPLE
    lag = (until - last).total_seconds()
    if lag > allowed:
        return False, (
            f"last_sample trails window end by {int(lag)}s "
            f"(> {allowed}s = {FRESHNESS_INTERVAL_MULTIPLE}x interval)"
        )
    return True, f"last_sample within {allowed}s of window end"


def _latest_transitions(violations: list) -> dict:
    """rule -> its last transition event in digest order. violations[] is
    chronological and the byte-cap fitter drops oldest-first, so the last
    shown match is the rule's true latest transition (see module docstring)."""
    latest: dict = {}
    for event in violations:
        if isinstance(event, dict) and isinstance(event.get("rule"), str):
            latest[event["rule"]] = event
    return latest


def _logger_health(digest: dict, window_seconds: int) -> dict:
    meta = digest["meta"]
    gaps = digest["gaps"]
    truncated = meta.get("truncated") or {}
    gap_count = truncated.get("gaps_total", len(gaps))
    gap_seconds = 0
    largest = 0
    for gap in gaps:
        seconds = gap.get("seconds") if isinstance(gap, dict) else None
        if isinstance(seconds, int) and seconds > 0:
            gap_seconds += seconds
            largest = max(largest, seconds)
    # gaps[] is display-capped (largest 1024 kept); when more existed, the sum
    # underestimates — flag it so "degraded by density" can't silently read as
    # "ok" just because the tail was dropped.
    undercounted = bool(gap_count and len(gaps) < gap_count)
    density = round(gap_seconds / window_seconds, 4)
    if not meta.get("samples"):
        verdict, reason = "degraded", "no samples in window"
    elif largest > SINGLE_GAP_MAX_SECONDS:
        verdict, reason = "degraded", f"single gap of {largest}s (> {SINGLE_GAP_MAX_SECONDS}s)"
    elif density > GAP_DENSITY_THRESHOLD:
        verdict, reason = "degraded", (
            f"gap density {density} (> {GAP_DENSITY_THRESHOLD} of window)"
        )
    elif undercounted:
        # The visible gaps pass, but the digest dropped some — don't certify ok.
        verdict, reason = "degraded", (
            f"{gap_count} gaps total but only {len(gaps)} shown — density is a floor"
        )
    else:
        verdict, reason = "ok", "gap density within threshold"
    return {
        "verdict": verdict,
        "reason": reason,
        "gap_count": gap_count,
        "gap_seconds": gap_seconds,
        "largest_gap_seconds": largest,
        "window_seconds": window_seconds,
        "density": density,
        "undercounted": undercounted,
    }


def _close_via_open_incident(state_dir: Path, event: dict, now_iso: str) -> dict:
    """Close through open_incident.py's own --close path (journal-first, atomic
    move) so there is exactly one close implementation. The digest's re-derived
    cleared event is materialized to a temp file because --event-file is that
    path's contract; its stdout (one JSON line) is captured and returned."""
    tmp = state_dir / f"reconcile-cleared.tmp.{os.getpid()}.json"
    tmp.write_text(json.dumps(event, separators=(",", ":")), encoding="utf-8")
    try:
        argv = [
            "--state-dir", str(state_dir),
            "--event-file", str(tmp),
            "--close",
            "--now", now_iso,
        ]
        args = open_incident.build_parser().parse_args(argv)
        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            code = open_incident.run(args)
        if code != 0:  # pragma: no cover - run() raises rather than returning nonzero
            raise st.Fatal(f"open_incident --close returned {code}")
        return json.loads(buffer.getvalue())
    finally:
        with contextlib.suppress(OSError):
            tmp.unlink()


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    digest = _load_digest(Path(args.digest))
    meta = digest["meta"]

    window_seconds = _window_seconds(meta)
    fresh, fresh_reason = _check_freshness(meta)
    logger_health = _logger_health(digest, window_seconds)
    latest = _latest_transitions(digest["violations"])
    truncated = bool(
        (meta.get("truncated") or {}).get("violations_shown", 0)
        < (meta.get("truncated") or {}).get("violations_total", 0)
    )

    incidents = st.read_open_incidents(state_dir)
    verdicts: list[dict] = []
    closed: list[str] = []

    for incident in incidents:
        rule = incident["rule"]
        record = {"rule": rule, "closed": False}
        if not fresh:
            record.update(verdict="indeterminate", reason=f"freshness gate: {fresh_reason}")
            verdicts.append(record)
            continue
        event = latest.get(rule)
        if event is None:
            reason = "no transition for this rule in the digest window"
            if truncated:
                reason += " (violations list was truncated — window may need widening)"
            record.update(verdict="indeterminate", reason=reason)
        elif event.get("state") == "cleared":
            record.update(verdict="recovered", reason=f"latest transition is cleared ({event.get('id')})")
            if not args.dry_run:
                # Validate against the frozen event contract before acting on it.
                st.validate_event(event)
                # Journal the decision first (recording-before-mutation, as
                # everywhere else); open_incident then journals the close itself.
                st.journal_append(
                    state_dir, now, "auto-close",
                    rule=rule, event_id=event.get("id"),
                    detail={"source": "reconcile", "digest_window_seconds": window_seconds},
                )
                result = _close_via_open_incident(state_dir, event, st.iso(now))
                record["closed"] = result.get("action") == "close"
                record["incident_file"] = result.get("incident_file")
                if record["closed"]:
                    closed.append(rule)
        else:
            record.update(verdict="still-firing", reason=f"latest transition is fired ({event.get('id')})")
        verdicts.append(record)

    st.emit({
        "action": "reconcile",
        "dry_run": bool(args.dry_run),
        "fresh": fresh,
        "freshness_reason": fresh_reason,
        "checked": len(incidents),
        "closed": closed,
        "verdicts": verdicts,
        "logger_health": logger_health,
    })
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

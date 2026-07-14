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

* ``recovered``    — the rule's latest in-window transition is ``cleared``
  AND that clear is newer than everything the incident has recorded. Closed
  via open_incident.py's ``--close`` path (journal-first, atomic open→closed
  move), using the digest's re-derived cleared event.
* ``still-firing`` — the latest transition is ``fired``. No change.
* ``indeterminate`` — no transition for the rule in the window (the window may
  not span the fire; rules the engine no longer evaluates land here too), the
  freshness gate failed, the clear is not newer than the incident's newest
  recorded event, or the transition's state/shape is unrecognized. Never
  closed: absence of evidence is not recovery. These stay manual.

Trust boundaries, in order:

1. **Freshness gate.** If the digest's window ended more than
   ``DIGEST_MAX_AGE_SECONDS`` before ``--now`` (a leftover file from an
   earlier wake proves nothing about *current* state), the digest has zero
   samples, or ``meta.last_sample`` trails ``meta.window.until`` by more than
   3× the sampling interval (the same multiple the digest's own gap detection
   uses), every verdict is ``indeterminate`` — a dead or stalled logger must
   never look like recovery.
2. **Latest transition wins.** ``violations[]`` is chronological, and the
   digest's byte-cap fitter drops violations oldest-first, so the surviving
   transitions are a chronological suffix: the last shown match for a rule IS
   that rule's latest transition. A rule whose transitions were all truncated
   away simply shows none → ``indeterminate`` (conservative by construction).
3. **Recovery must postdate the incident's record.** The independent watcher
   can observe a fire while the logger is blind (a gap), after which the log
   still holds an *older* clear for the same rule. A close therefore also
   requires the cleared transition's timestamp to be strictly newer than the
   incident's newest recorded event line (falling back to the ``opened``
   header when the record has no event lines). Fail closed: if ANY event
   bullet's timestamp does not parse, the record is unorderable — a partial
   maximum could omit the newest fire — and the incident stays open for a
   human (validate_event now rejects unparseable timestamps before recording,
   so this guards legacy/hand-edited files).

Also emits a ``logger_health`` block computed from the same digest (``gaps[]``
vs the window length) — the deterministic input for the SKILL's escalate-on-
gap-density step. Thresholds: density above ``GAP_DENSITY_THRESHOLD`` of the
window, or any single gap over ``SINGLE_GAP_MAX_SECONDS``, is ``degraded``; a
feed that fails the freshness gate is ``degraded`` too (a dead tail is the
blindest gap of all, even though the aggregator emits no trailing ``gaps``
entry for it).

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

# The digest itself must be from THIS wake: a window that ended more than this
# many seconds before --now is a leftover file (e.g. a crashed wake's
# last-report.json) and proves nothing about current state. Generous enough
# for report → triage → reconcile within one wake; far below a watch cycle.
DIGEST_MAX_AGE_SECONDS = 600

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
    # Everything read downstream is validated HERE, so a malformed digest is
    # always the documented usage exit 2 — never an uncaught traceback.
    window = meta.get("window")
    if not isinstance(window, dict):
        raise st.Usage("digest meta.window is not an object")
    for key in ("since", "until"):
        if not isinstance(window.get(key), str):
            raise st.Usage(f"digest meta.window.{key} is not a string")
    if not _is_count(meta.get("samples")):
        raise st.Usage("digest meta.samples is not a non-negative integer")
    interval = meta.get("interval_seconds")
    if not isinstance(interval, int) or isinstance(interval, bool) or interval < 1:
        raise st.Usage("digest meta.interval_seconds is not a positive integer")
    last_sample = meta.get("last_sample")
    if last_sample is not None and not isinstance(last_sample, str):
        raise st.Usage("digest meta.last_sample is neither null nor a string")
    truncated = meta.get("truncated", {})
    if not isinstance(truncated, dict):
        raise st.Usage("digest meta.truncated is not an object")
    gaps_total = truncated.get("gaps_total", len(digest["gaps"]))
    if not _is_count(gaps_total):
        raise st.Usage("digest meta.truncated.gaps_total is not a non-negative integer")
    for key in ("violations_shown", "violations_total"):
        if key in truncated and not _is_count(truncated[key]):
            raise st.Usage(f"digest meta.truncated.{key} is not a non-negative integer")
    return digest


def _is_count(value: object) -> bool:
    """A non-negative int (bool excluded — JSON true is never a count)."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _window_seconds(meta: dict) -> int:
    window = meta["window"]  # shape-guaranteed by _load_digest
    since = st.parse_iso(window["since"])
    until = st.parse_iso(window["until"])
    seconds = int((until - since).total_seconds())
    if seconds <= 0:
        raise st.Usage(f"digest window is empty or inverted ({window})")
    return seconds


def _check_freshness(meta: dict, now) -> tuple[bool, str]:
    """(fresh, reason). A stale digest FILE, zero samples, or a stale
    last_sample all mean the digest cannot prove recovery for anything."""
    until = st.parse_iso(str(meta["window"]["until"]))
    digest_age = (now - until).total_seconds()
    if digest_age > DIGEST_MAX_AGE_SECONDS:
        return False, (
            f"digest window ended {int(digest_age)}s before --now "
            f"(> {DIGEST_MAX_AGE_SECONDS}s) — a leftover digest proves nothing current"
        )
    samples = meta.get("samples", 0)
    if not samples:
        return False, "no samples in window (logger dead or window empty)"
    last_sample = meta.get("last_sample")
    if not last_sample:
        return False, "digest has samples but no last_sample timestamp"
    last = st.parse_iso(str(last_sample))
    allowed = meta["interval_seconds"] * FRESHNESS_INTERVAL_MULTIPLE
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


def _logger_health(digest: dict, window_seconds: int, fresh: bool, fresh_reason: str) -> dict:
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
    # Compare the RAW ratio; round only for display — rounding first would
    # read a just-over-threshold density (e.g. 0.10004) as ok. Six decimals
    # keep a boundary value distinguishable from the threshold itself, so a
    # degraded reason never prints the contradiction "0.1 (> 0.1)".
    raw_density = gap_seconds / window_seconds
    density = round(raw_density, 6)
    if not fresh:
        # A dead/stalled tail is the blindest gap of all, but the aggregator
        # only counts gaps BETWEEN samples — it never emits a trailing entry.
        # Without this fold, a feed dead for 99% of the window reads "ok" and
        # the SKILL's degraded-only escalation never fires.
        verdict, reason = "degraded", f"feed not fresh: {fresh_reason}"
    elif largest > SINGLE_GAP_MAX_SECONDS:
        verdict, reason = "degraded", f"single gap of {largest}s (> {SINGLE_GAP_MAX_SECONDS}s)"
    elif raw_density > GAP_DENSITY_THRESHOLD:
        verdict, reason = "degraded", (
            f"gap density {raw_density:.6f} (> {GAP_DENSITY_THRESHOLD} of window)"
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


def _incident_evidence_time(incident: dict) -> tuple:
    """``(evidence_time, block_reason)`` — the newest instant the incident has
    RECORDED (latest event-line timestamp, else the ``opened`` header), or
    ``(None, why)`` when the record cannot be trusted to order evidence.

    Fail-closed rule: if ANY event-shaped bullet has an unparseable timestamp,
    the whole record is unorderable — a partial maximum over the lines that DO
    parse could omit the newest fire, which is precisely the fire the ordering
    guard exists to protect."""
    try:
        lines = incident["path"].read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        return None, f"incident file unreadable ({exc.__class__.__name__}) — cannot order recovery evidence"
    latest, unorderable = st.incident_latest_event_time(lines)
    if unorderable:
        return None, (
            "incident record contains an event line with an unparseable timestamp — "
            "cannot prove the clear postdates it (fail closed; resolve by hand)"
        )
    if latest is not None:
        return latest, None
    try:
        return st.parse_iso(str(incident["opened"])), None
    except st.Usage:
        return None, "incident record has no parseable timestamps — cannot order recovery evidence"


def _recovered_or_blocked(event: dict, incident: dict) -> dict:
    """For a latest-transition ``cleared``: ``recovered``, or ``indeterminate``
    with the blocking reason. Both checks are per-incident so one bad record
    can never abort the run mid-loop (the JSON output contract survives):

    * the event must satisfy the frozen 14-key contract (it is about to be
      handed to open_incident.py --close);
    * the clear must be strictly newer than the incident's newest recorded
      event — the watcher can observe a fire while the logger is blind, and an
      older log-derived clear is not evidence that THAT fire recovered.
    """
    try:
        st.validate_event(event)
        cleared_at = st.parse_iso(str(event["timestamp"]))
    except st.Usage as exc:
        return {"verdict": "indeterminate",
                "reason": f"cleared transition fails the event contract: {exc}"}
    evidence, block_reason = _incident_evidence_time(incident)
    if evidence is None:
        return {"verdict": "indeterminate", "reason": block_reason}
    if cleared_at <= evidence:
        return {"verdict": "indeterminate",
                "reason": (
                    f"cleared transition ({event.get('id')} @ {event['timestamp']}) is not newer "
                    f"than the incident's newest recorded event ({st.iso(evidence)}) — "
                    "the fire may postdate the log's evidence (e.g. during a logger gap)"
                )}
    return {"verdict": "recovered",
            "reason": f"latest transition is cleared ({event.get('id')}), newer than the incident record"}


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    digest = _load_digest(Path(args.digest))
    meta = digest["meta"]

    window_seconds = _window_seconds(meta)
    fresh, fresh_reason = _check_freshness(meta, now)
    logger_health = _logger_health(digest, window_seconds, fresh, fresh_reason)
    latest = _latest_transitions(digest["violations"])
    trunc_meta = meta.get("truncated") or {}
    truncated = trunc_meta.get("violations_shown", 0) < trunc_meta.get("violations_total", 0)

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
        state = event.get("state") if event is not None else None
        if event is None:
            reason = "no transition for this rule in the digest window"
            if truncated:
                reason += (
                    " (the violations list was byte-cap truncated — "
                    "raise report --max-bytes or narrow the window)"
                )
            record.update(verdict="indeterminate", reason=reason)
        elif state == "cleared":
            record.update(**_recovered_or_blocked(event, incident))
            if record["verdict"] == "recovered" and not args.dry_run:
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
        elif state == "fired":
            record.update(verdict="still-firing", reason=f"latest transition is fired ({event.get('id')})")
        else:
            record.update(
                verdict="indeterminate",
                reason=f"latest transition has unrecognized state {state!r} — not evidence",
            )
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

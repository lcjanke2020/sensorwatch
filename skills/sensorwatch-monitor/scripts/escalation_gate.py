#!/usr/bin/env python3
"""escalation_gate.py — the deterministic escalation decision.

A deterministic function of the escalation ledger, the set of currently-open
critical incidents (``incidents/open/`` — the lifecycle authority), and the
event's shape. Returns a tier and a decision. ``--commit`` records the tier and
rolls the daily counter, but **not** the cooldown/daily-count of a delivery —
that is notify.py's job, on actual delivery (so a crash before delivery cannot
suppress the redelivery). The gate reads ``last_notified``; notify writes it.

    python escalation_gate.py --state-dir <dir> --rule <name> \\
        --severity info|warning|critical --state fired|cleared \\
        [--persistence-events N] [--cooldown-hours 6] [--daily-cap 5] \\
        [--commit] [--now <iso>]

Tier ladder (documented in SKILL.md):
  0 journal-only   info
  1 incident       warning
  2 notify         warning persisting >=3 events, or critical
  3 notify+issue   critical persisting >=3 events — the routed notification
                   plus a tracker-ready draft (notify.py --issue-draft →
                   outbox/issues/), one invocation, one cooldown record
  4 combination    >=2 distinct critical rules open at once

Decision: allow | suppress (inside the per-rule cooldown) | batch (today's
notification count is at the daily cap → one batched digest goes out instead).
Tiers 0-1 are local writes and are always allowed. Exit 0 success, 1 fatal,
2 usage.
"""

from __future__ import annotations

import argparse
import sys
from datetime import timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="escalation_gate.py",
        description="Decide (and optionally commit) an escalation tier.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--rule", required=True)
    parser.add_argument("--severity", required=True, choices=st.SEVERITIES)
    parser.add_argument("--state", required=True, choices=st.STATES)
    parser.add_argument(
        "--persistence-events", type=int, default=1,
        help="events this incident has accumulated (default 1)",
    )
    parser.add_argument("--cooldown-hours", type=int, default=st.DEFAULT_COOLDOWN_HOURS)
    parser.add_argument("--daily-cap", type=int, default=st.DEFAULT_DAILY_CAP)
    parser.add_argument("--commit", action="store_true", help="record the outcome to the ledger")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def _compute_tier(severity: str, state: str, persistence: int, open_criticals: set, rule: str) -> int:
    if state == "cleared":
        return 0  # a clear resolves; escalation is about firing
    if severity == "info":
        return 0
    if severity == "warning":
        return 2 if persistence >= 3 else 1
    # critical
    tier = 3 if persistence >= 3 else 2
    # The set of concurrently-open criticals is derived from incidents/open/ —
    # the single incident-lifecycle authority — so closing an incident (which
    # moves its file out of open/) can never leave a phantom that promotes an
    # unrelated critical to tier 4.
    if len(open_criticals | {rule}) >= 2:
        tier = 4
    return tier


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    today = st.date_str(now)

    esc_path = state_dir / "escalation.json"
    esc = st.load_escalation(state_dir)
    per_rule = esc.setdefault("per_rule", {})

    open_criticals = {
        inc["rule"] for inc in st.read_open_incidents(state_dir)
        if inc["severity"] == "critical"
    }
    tier = _compute_tier(args.severity, args.state, args.persistence_events, open_criticals, args.rule)

    esc_date = esc.get("date")
    today_count = esc.get("notifications_today", 0) if esc_date == today else 0

    # Decision.
    if tier <= 1:
        decision = "allow"
        reason = f"tier {tier}: local write, no notification"
    else:
        last_notified = per_rule.get(args.rule, {}).get("last_notified")
        cooldown = timedelta(hours=args.cooldown_hours)
        if last_notified is not None and (now - st.parse_iso_state(last_notified)) < cooldown:
            decision = "suppress"
            reason = f"within {args.cooldown_hours}h per-rule cooldown"
        elif today_count >= args.daily_cap:
            decision = "batch"
            reason = f"daily cap {args.daily_cap} reached ({today_count} today) — batch digest"
        else:
            decision = "allow"
            reason = f"tier {tier}: notify"

    committed = False
    if args.commit:
        # Roll the daily counter if the ledger's date is stale.
        if esc_date != today:
            esc["date"] = today
            esc["notifications_today"] = 0
        entry = per_rule.setdefault(args.rule, {})
        entry["severity"] = args.severity
        entry["tier"] = tier
        entry["last_gate"] = st.iso(now)
        # The per-rule cooldown (last_notified) and the daily count are recorded
        # by notify.py when a notice is actually delivered — NOT here. Otherwise a
        # crash between this commit and delivery would arm the cooldown for a
        # notification that never went out, and the redelivery would be silently
        # suppressed. "No delivery, no cooldown."
        st.save_json_atomic(esc_path, esc)
        st.journal_append(
            state_dir, now, "gate",
            rule=args.rule,
            detail={"decision": decision, "tier": tier, "severity": args.severity, "state": args.state},
        )
        committed = True

    st.emit({
        "decision": decision,
        "tier": tier,
        "reason": reason,
        "rule": args.rule,
        "severity": args.severity,
        "state": args.state,
        "committed": committed,
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

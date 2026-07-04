#!/usr/bin/env python3
"""notify.py — deliver an escalation notice through a pluggable adapter.

LEO-338 ships stub adapters only:

  * ``outbox`` — write outbox/<utc-stamp>-<slug>.md atomically (the durable,
    inspectable stand-in for a real channel).
  * ``stderr`` — print the notice to stderr.

The real transport (email is the default candidate) is LEO-339's decision; until
then tier >=2 delivery lands in the outbox. Adding a channel = registering one
function in ADAPTERS. On successful delivery notify records the per-rule cooldown
(last_notified) and bumps the daily notification count — it is the SOLE writer of
those, so the cooldown is armed only when a notice actually went out (the gate
reads them to decide; a crash before delivery leaves them un-armed, so the
redelivery re-notifies instead of being silently suppressed).

    python notify.py --state-dir <dir> --adapter outbox|stderr --rule <name> \\
        --severity <s> --tier <n> [--incident-file <path>] [--summary "..."] [--now <iso>]

The body is plain prose that *references* the incident-file path — it never
embeds sensor data (public-repo + Linear-WAF hygiene). Exit 0 success, 1 fatal,
2 usage (unknown adapter).
"""

from __future__ import annotations

import argparse
import sys
from datetime import timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402


def _one_line(text: str) -> str:
    """Collapse any newlines/runs of whitespace to single spaces so a multi-line
    --summary cannot restructure the notice or corrupt the journal line."""
    return " ".join(text.split())


def _body(args: argparse.Namespace, now_iso: str) -> str:
    summary = _one_line(args.summary) if args.summary else "See the referenced incident file for detail."
    lines = [
        f"# sensorwatch monitor — {args.severity} (tier {args.tier})",
        "",
        f"- rule: {args.rule}",
        f"- severity: {args.severity}",
        f"- tier: {args.tier}",
        f"- at: {now_iso}",
    ]
    if args.incident_file:
        lines.append(f"- incident: {args.incident_file}")
    lines.append("")
    lines.append(summary)
    lines.append("")
    lines.append(
        "_Real transport pending LEO-339; this notice was delivered by a stub "
        "adapter. Sensor data lives in the incident file, not here._"
    )
    return "\n".join(lines) + "\n"


def _deliver_outbox(state_dir: Path, now, slug: str, body: str) -> str:
    stamp = now.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    outbox = state_dir / "outbox"
    outbox.mkdir(parents=True, exist_ok=True)
    # Second-precision stamps collide when two notices for one rule land in the
    # same second (or under a pinned --now); disambiguate so neither overwrites
    # the other. write_text_atomic uses a pid-suffixed tmp + os.replace.
    path = outbox / f"{stamp}-{slug}.md"
    n = 1
    while path.exists():
        path = outbox / f"{stamp}-{slug}-{n}.md"
        n += 1
    st.write_text_atomic(path, body)
    return str(path)


def _deliver_stderr(state_dir: Path, now, slug: str, body: str) -> str:
    sys.stderr.write(body)
    return "stderr"


ADAPTERS = {
    "outbox": _deliver_outbox,
    "stderr": _deliver_stderr,
}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="notify.py",
        description="Deliver an escalation notice through a stub adapter.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--adapter", required=True, help="outbox | stderr")
    parser.add_argument("--rule", required=True)
    parser.add_argument("--severity", required=True, choices=st.SEVERITIES)
    parser.add_argument("--tier", required=True, type=int, help="escalation tier (0-4)")
    parser.add_argument("--incident-file", help="path referenced (never embedded) in the notice")
    parser.add_argument("--summary", help="one-line plain-prose summary")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)

    adapter = ADAPTERS.get(args.adapter)
    if adapter is None:
        raise st.Usage(
            f"unknown adapter {args.adapter!r}; available: {', '.join(sorted(ADAPTERS))}"
        )

    slug = st.slugify(args.rule)
    target = adapter(state_dir, now, slug, _body(args, st.iso(now)))

    st.journal_append(
        state_dir, now, "notify",
        rule=args.rule,
        detail={"adapter": args.adapter, "tier": args.tier, "target": target},
    )

    # A delivered notice spends the rule's cooldown and a daily-cap slot. Recording
    # it HERE — after delivery — rather than in escalation_gate --commit is what
    # closes the lost-notification gap: no delivery, no cooldown, so a redelivery
    # re-notifies instead of being suppressed. notify is the sole writer of
    # last_notified / notifications_today; the gate only reads them.
    _record_delivery(state_dir, now, args.rule)

    st.emit({
        "adapter": args.adapter,
        "delivered": True,
        "tier": args.tier,
        "target": target,
    })
    return 0


def _record_delivery(state_dir: Path, now, rule: str) -> None:
    esc_path = state_dir / "escalation.json"
    esc = st.load_escalation(state_dir)
    today = st.date_str(now)
    if esc.get("date") != today:  # daily-count roll-over
        esc["date"] = today
        esc["notifications_today"] = 0
    entry = esc.setdefault("per_rule", {}).setdefault(rule, {})
    entry["last_notified"] = st.iso(now)
    esc["notifications_today"] = esc.get("notifications_today", 0) + 1
    st.save_json_atomic(esc_path, esc)


def main(argv: list[str] | None = None) -> int:
    st.force_utf8_io()
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except (st.Usage, st.Fatal) as exc:
        return st.die(exc)


if __name__ == "__main__":
    raise SystemExit(main())

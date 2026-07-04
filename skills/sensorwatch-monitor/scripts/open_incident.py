#!/usr/bin/env python3
"""open_incident.py — open, update, or close an incident from an event.

    # classify a fired event (benign journals only; anomaly/incident open a file)
    python open_incident.py --state-dir <dir> --event-file <...> \\
        --classification benign|anomaly|incident [--snooze 6h] [--now <iso>]

    # close on a cleared event (append a closing note, move open -> closed)
    python open_incident.py --state-dir <dir> --event-file <...> --close [--now <iso>]

Incident files are one-per-rule Markdown (incidents/open/<slug>.md) rendered from
templates/incident.md; the ``- key: value`` header is machine-maintained and read
by state_summary.py. Journal-first, then the file write / move — same recording
order as ack_event.py. Exit 0 success, 1 fatal, 2 usage.
"""

from __future__ import annotations

import argparse
import sys
from datetime import timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402

TEMPLATE = Path(__file__).parent.parent / "templates" / "incident.md"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="open_incident.py",
        description="Open, update, or close an incident from an event.",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--event-file", required=True, help="the spooled event")
    parser.add_argument(
        "--classification", choices=st.CLASSIFICATIONS,
        help="required unless --close",
    )
    parser.add_argument("--close", action="store_true", help="close the incident (cleared event)")
    parser.add_argument("--snooze", default=st.DEFAULT_SNOOZE, help="snooze window (default 6h)")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def _event_line(event: dict) -> str:
    value = event["value"]
    unit = event["unit"] or ""
    val = "n/a" if value is None else f"{value} {unit}".strip()
    return f"- {event['id']} @ {event['timestamp']}  {event['state']}  value={val}"


def _render_new(event: dict, classification: str, opened: str, snooze_until: str) -> str:
    text = TEMPLATE.read_text(encoding="utf-8")
    fields = {
        "rule": event["rule"],
        "severity": event["severity"],
        "classification": classification,
        "opened": opened,
        "snooze_until": snooze_until,
        "events": "1",
        "status": "open",
        "events_block": _event_line(event),
    }
    for key, value in fields.items():
        text = text.replace("{" + key + "}", value)
    return text


def _set_field(lines: list[str], key: str, value: str) -> bool:
    prefix = f"- {key}:"
    for i, line in enumerate(lines):
        if line.startswith(prefix):
            lines[i] = f"- {key}: {value}"
            return True
    return False


def _get_field(lines: list[str], key: str) -> str | None:
    prefix = f"- {key}:"
    for line in lines:
        if line.startswith(prefix):
            return line[len(prefix):].strip()
    return None


def _insert_before_notes(lines: list[str], new_line: str) -> None:
    for i, line in enumerate(lines):
        if line.strip() == "## Notes":
            lines.insert(i, new_line)
            return
    lines.append(new_line)


def _incident_path(state_dir: Path, rule: str, closed: bool = False) -> Path:
    sub = "closed" if closed else "open"
    return state_dir / "incidents" / sub / f"{st.slugify(rule)}.md"


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)
    event = st.load_event(Path(args.event_file))
    rule = event["rule"]
    event_id = event["id"]
    open_path = _incident_path(state_dir, rule)

    if args.close:
        # Close: journal first, then append a closing note and move open->closed.
        st.journal_append(
            state_dir, now, "close",
            rule=rule, event_id=event_id, detail={"seq": event["seq"]},
        )
        if not open_path.exists():
            # Nothing was open (e.g. the fire was classified benign) — the clear
            # is still recorded in the journal; there is no file to move.
            st.emit({"action": "close-noop", "rule": rule, "event_id": event_id})
            return 0
        lines = open_path.read_text(encoding="utf-8").splitlines()
        _set_field(lines, "status", "closed")
        if not _set_field(lines, "closed", st.iso(now)):
            # add a closed: line right after status:
            for i, line in enumerate(lines):
                if line.startswith("- status:"):
                    lines.insert(i + 1, f"- closed: {st.iso(now)}")
                    break
        _insert_before_notes(lines, _event_line(event))
        lines.append(f"- closed {st.iso(now)}: cleared by {event_id} (seq {event['seq']})")

        closed_path = _incident_path(state_dir, rule, closed=True)
        if closed_path.exists():
            closed_path = closed_path.with_name(f"{st.slugify(rule)}-{event['seq']}.md")
        closed_path.parent.mkdir(parents=True, exist_ok=True)
        closed_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        open_path.unlink()
        st.emit({
            "action": "close",
            "rule": rule,
            "event_id": event_id,
            "incident_file": str(closed_path),
        })
        return 0

    # Non-close: classification required.
    if args.classification is None:
        raise st.Usage("--classification is required unless --close")

    snooze_seconds = st.parse_duration(args.snooze)
    snooze_until = st.iso(now + timedelta(seconds=snooze_seconds))

    # Journal first (every classification, including benign).
    st.journal_append(
        state_dir, now, "open",
        rule=rule, event_id=event_id,
        detail={"classification": args.classification, "seq": event["seq"]},
    )

    if args.classification == "benign":
        # Benign journals but does NOT create an incident file.
        st.emit({
            "action": "benign",
            "rule": rule,
            "event_id": event_id,
            "incident_file": None,
        })
        return 0

    if not open_path.exists():
        open_path.parent.mkdir(parents=True, exist_ok=True)
        open_path.write_text(
            _render_new(event, args.classification, st.iso(now), snooze_until),
            encoding="utf-8",
        )
        st.emit({
            "action": "open",
            "rule": rule,
            "event_id": event_id,
            "incident_file": str(open_path),
            "events": 1,
            "snooze_until": snooze_until,
        })
        return 0

    # Update an existing open incident: append the event, bump the count,
    # re-snooze, and adopt the (possibly escalated) classification.
    existing_text = open_path.read_text(encoding="utf-8")
    lines = existing_text.splitlines()

    # Idempotency: a redelivered event (crash after the incident write but
    # before ack) must not double-append or double-count. Re-snooze only.
    if f"- {event_id} @" in existing_text:
        _set_field(lines, "snooze_until", snooze_until)
        open_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        current = _get_field(lines, "events")
        try:
            count = int(current)
        except (TypeError, ValueError):
            count = 0
        st.emit({
            "action": "update-deduped",
            "rule": rule,
            "event_id": event_id,
            "incident_file": str(open_path),
            "events": count,
            "snooze_until": snooze_until,
        })
        return 0

    current = _get_field(lines, "events")
    try:
        count = int(current) + 1 if current is not None else 1
    except ValueError:
        count = 1
    _set_field(lines, "events", str(count))
    _set_field(lines, "snooze_until", snooze_until)
    _set_field(lines, "classification", args.classification)
    _insert_before_notes(lines, _event_line(event))
    open_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    st.emit({
        "action": "update",
        "rule": rule,
        "event_id": event_id,
        "incident_file": str(open_path),
        "events": count,
        "snooze_until": snooze_until,
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

#!/usr/bin/env python3
"""ack_event.py — acknowledge a spooled event, crash-safe and idempotent.

The event file must live in ``spool/pending/``. Write order is
journal -> cursor -> move (recording before acknowledgment): a crash between
steps redelivers the event, and the id already recorded in the cursor's ring
makes the redelivery a no-op.

    python ack_event.py --state-dir <dir> --event-file <spool/pending/...> \\
        [--note "..."] [--now <iso>]

Idempotent on the event id: a second ack of the same event (id already in
``cursor.acked_ids_recent`` or the file already in ``spool/acked/``) leaves the
cursor unchanged and reports ``"deduped": true``. Exit 0 success, 1 fatal,
2 usage (malformed event / event not in spool/pending).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ack_event.py",
        description="Acknowledge a spooled event (idempotent, crash-safe).",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument("--event-file", required=True, help="path inside spool/pending/")
    parser.add_argument("--note", help="optional note recorded in the journal")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)

    pending_dir = (state_dir / "spool" / "pending").resolve()
    acked_dir = state_dir / "spool" / "acked"
    event_path = Path(args.event_file)

    # The event must be a file directly inside spool/pending/.
    resolved = event_path.resolve()
    if resolved.parent != pending_dir:
        raise st.Usage(
            f"--event-file must be inside {pending_dir}, got {resolved}"
        )

    event = st.load_event(event_path)  # validates the 14-key contract (exit 2)
    event_id = event["id"]
    seq = event["seq"]
    rule = event["rule"]
    basename = event_path.name
    acked_target = acked_dir / basename

    cursor_path = state_dir / "cursor.json"
    cursor = st.load_cursor(state_dir)
    ring = cursor.get("acked_ids_recent", [])

    # Idempotency: id already acked, or the file already sitting in acked/.
    if event_id in ring or acked_target.exists():
        moved = False
        if event_path.exists():
            acked_dir.mkdir(parents=True, exist_ok=True)
            if acked_target.exists():
                # Already acked: the pending file is a redundant duplicate.
                # watch never deletes spool files, so cleanup is ours.
                event_path.unlink()
            else:
                # Self-heal a move interrupted after the cursor was written.
                event_path.replace(acked_target)
                moved = True
        st.emit({
            "deduped": True,
            "event_id": event_id,
            "seq": seq,
            "last_acked_seq": cursor.get("last_acked_seq", 0),
            "moved": moved,
        })
        return 0

    # 1. Journal first — the recording-before-acknowledgment anchor.
    st.journal_append(
        state_dir, now, "ack",
        rule=rule, event_id=event_id,
        detail={"seq": seq, "note": args.note} if args.note else {"seq": seq},
    )

    # 2. Cursor: advance the high-water mark (never regress on a redelivered
    #    lower seq), ring-append the id (drop oldest past the cap).
    cursor["last_acked_seq"] = max(seq, cursor.get("last_acked_seq", 0))
    ring.append(event_id)
    if len(ring) > st.ACKED_IDS_CAP:
        del ring[: len(ring) - st.ACKED_IDS_CAP]
    cursor["acked_ids_recent"] = ring
    cursor["updated"] = st.iso(now)
    st.save_json_atomic(cursor_path, cursor)

    # 3. Move pending -> acked (last, so a crash before it just redelivers).
    acked_dir.mkdir(parents=True, exist_ok=True)
    event_path.replace(acked_target)

    st.emit({
        "deduped": False,
        "event_id": event_id,
        "seq": seq,
        "last_acked_seq": cursor["last_acked_seq"],
        "moved_to": str(acked_target),
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

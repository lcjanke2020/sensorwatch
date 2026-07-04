"""Shared helpers for the sensorwatch-monitor state scripts.

Stdlib only, ``pathlib`` everywhere, atomic JSON writes. Imported by the sibling
scripts via ``sys.path.insert(0, str(Path(__file__).parent))`` so they stay
runnable as plain files — the skill's consumers invoke them by path, never as an
installed package.

Conventions shared across every script (see the sensorwatch-monitor SKILL.md):

* JSON result on stdout, human diagnostics on stderr.
* Exit codes: ``0`` success, ``1`` fatal (unreadable/corrupt state),
  ``2`` usage (bad args — argparse's default — or a malformed event).
* Machine-updated state is JSON; human-and-agent judgment is Markdown.
* Time is injected with ``--now <iso>`` so nothing reads the wall clock under
  test; it defaults to the real UTC clock only when omitted.
"""

from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

# ---- shared constants (every cap is restated in SKILL.md) ----

SCHEMA_VERSION = 1
STATE_ENV = "SENSORWATCH_MONITOR_STATE"

ACKED_IDS_CAP = 64  # cursor.acked_ids_recent ring length

BOOTSTRAP_LINE_CAP = 60
BASELINE_LINE_CAP = 150
INCIDENT_LINE_CAP = 80

SUMMARY_MAX_BYTES = 4096

DEFAULT_SNOOZE = "6h"
DEFAULT_COOLDOWN_HOURS = 6
DEFAULT_DAILY_CAP = 5

SEVERITIES = ("info", "warning", "critical")
STATES = ("fired", "cleared")
CLASSIFICATIONS = ("benign", "anomaly", "incident")

# The frozen 14-key event contract (docs/agent-monitoring.md). Order is fixed by
# the emitter; consumers validate presence + type and ignore unknown keys
# (additive-schema tolerance: additive changes keep schema_version at 1).
EVENT_KEYS = (
    "schema_version", "seq", "id", "rule", "type", "severity", "state",
    "timestamp", "sensor", "reading", "value", "unit", "threshold",
    "samples_in_violation",
)


# ---- small process helpers ----

class Usage(Exception):
    """Raised for a usage error (bad args / malformed event) — maps to exit 2."""


class Fatal(Exception):
    """Raised for unreadable or corrupt state — maps to exit 1."""


def die(exc: BaseException) -> int:
    """Print a diagnostic to stderr and return the matching exit code."""
    if isinstance(exc, Usage):
        code = 2
    elif isinstance(exc, Fatal):
        code = 1
    else:  # pragma: no cover - defensive
        code = 1
    print(f"error: {exc}", file=sys.stderr)
    return code


def emit(result: dict) -> None:
    """Write the machine-readable result as one compact JSON line on stdout."""
    json.dump(result, sys.stdout, separators=(",", ":"), sort_keys=False)
    sys.stdout.write("\n")


def _is_int(value: object) -> bool:
    """int, but not bool (bool is an int subclass and never a valid count)."""
    return isinstance(value, int) and not isinstance(value, bool)


def _is_number(value: object) -> bool:
    return (isinstance(value, (int, float)) and not isinstance(value, bool))


# ---- time ----

def parse_iso(text: str) -> datetime:
    """Parse an ISO-8601 timestamp (accepts a trailing ``Z``)."""
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00"))
    except (ValueError, AttributeError) as exc:
        raise Usage(f"not an ISO-8601 timestamp: {text!r}") from exc


def resolve_now(now_arg: str | None) -> datetime:
    """The injected ``--now``, or the real UTC clock when omitted."""
    if now_arg:
        return parse_iso(now_arg)
    return datetime.now(timezone.utc)


def iso(dt: datetime) -> str:
    return dt.isoformat()


def date_str(dt: datetime) -> str:
    return dt.date().isoformat()


def month_str(dt: datetime) -> str:
    return dt.strftime("%Y-%m")


def parse_duration(text: str) -> int:
    """Parse ``24h`` / ``90m`` / ``7d`` / ``1d12h`` / ``30s`` / bare seconds
    into whole seconds — the same vocabulary as the CLI's ``--last``."""
    raw = (text or "").strip().lower()
    if not raw:
        raise Usage("empty duration")
    if raw.isdigit():
        return int(raw)
    units = {"d": 86400, "h": 3600, "m": 60, "s": 1}
    total = 0
    num = ""
    seen = False
    for ch in raw:
        if ch.isdigit():
            num += ch
        elif ch in units:
            if not num:
                raise Usage(f"bad duration: {text!r}")
            total += int(num) * units[ch]
            num = ""
            seen = True
        else:
            raise Usage(f"bad duration: {text!r}")
    if num or not seen:
        raise Usage(f"bad duration: {text!r}")
    return total


# ---- filesystem / paths ----

def slugify(name: str) -> str:
    """Lowercase, fold anything but ``[a-z0-9-_]`` to ``-`` — matches the
    watcher's spool slug so incident filenames stay filesystem-portable."""
    out = "".join(
        ch if (ch.isalnum() and ch.isascii()) or ch in "-_" else "-"
        for ch in name.lower()
    )
    return out.strip("-") or "rule"


def resolve_state_dir(state_dir_arg: str | None) -> Path:
    """From ``--state-dir`` or ``$SENSORWATCH_MONITOR_STATE``; there is no
    in-repo default (baselines/thresholds reveal hardware specs)."""
    raw = state_dir_arg or os.environ.get(STATE_ENV)
    if not raw:
        raise Usage(
            f"no state dir: pass --state-dir or set ${STATE_ENV}"
        )
    return Path(raw)


def find_git_worktree(path: Path) -> Path | None:
    """Walk up from ``path`` looking for a ``.git`` marker (dir or file).
    Returns the work-tree root, or ``None``. Used to warn when a state dir is
    inside a git tree (state must never be committed — it is machine-local)."""
    try:
        current = path.resolve()
    except OSError:
        current = path
    for candidate in (current, *current.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


def load_json(path: Path) -> dict:
    """Read a JSON state file; a missing or corrupt file is Fatal (exit 1)."""
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise Fatal(f"state file missing: {path} (run init_state.py?)") from exc
    except OSError as exc:
        raise Fatal(f"cannot read state file {path}: {exc}") from exc
    try:
        data = json.loads(text)
    except json.JSONDecodeError as exc:
        raise Fatal(f"corrupt state file {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise Fatal(f"state file {path} is not a JSON object")
    return data


def save_json_atomic(path: Path, obj: dict) -> None:
    """Write JSON via a temp file + ``os.replace`` so a reader never sees a
    half-written state file (a crash leaves the old file intact)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    text = json.dumps(obj, separators=(",", ":"), sort_keys=False)
    try:
        tmp.write_text(text, encoding="utf-8")
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def load_event(path: Path) -> dict:
    """Read + validate a spool event file against the frozen 14-key contract.
    A malformed event is a Usage error (exit 2), never Fatal — a bad event must
    not look like corrupt state."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise Usage(f"cannot read event file {path}: {exc}") from exc
    try:
        event = json.loads(text)
    except json.JSONDecodeError as exc:
        raise Usage(f"event file {path} is not JSON: {exc}") from exc
    validate_event(event)
    return event


def validate_event(event: object) -> None:
    """Enforce the frozen contract: object, schema_version 1, all 14 keys
    present with the right type. Unknown extra keys are tolerated."""
    if not isinstance(event, dict):
        raise Usage("event is not a JSON object")
    missing = [k for k in EVENT_KEYS if k not in event]
    if missing:
        raise Usage(f"event missing keys: {', '.join(missing)}")
    if event["schema_version"] != SCHEMA_VERSION:
        raise Usage(
            f"unsupported schema_version {event['schema_version']!r} "
            f"(this consumer pins {SCHEMA_VERSION})"
        )
    checks = {
        "seq": _is_int,
        "id": lambda v: isinstance(v, str) and bool(v),
        "rule": lambda v: isinstance(v, str) and bool(v),
        "type": lambda v: isinstance(v, str),
        "severity": lambda v: v in SEVERITIES,
        "state": lambda v: v in STATES,
        "timestamp": lambda v: isinstance(v, str) and bool(v),
        "sensor": lambda v: v is None or isinstance(v, str),
        "reading": lambda v: v is None or isinstance(v, str),
        "value": lambda v: v is None or _is_number(v),
        "unit": lambda v: v is None or isinstance(v, str),
        "threshold": lambda v: v is None or _is_number(v),
        "samples_in_violation": _is_int,
    }
    for key, ok in checks.items():
        if not ok(event[key]):
            raise Usage(f"event field {key!r} has an invalid value: {event[key]!r}")


# ---- journal (append-only, monthly by filename) ----

def journal_append(
    state_dir: Path,
    now: datetime,
    action: str,
    *,
    rule: str | None = None,
    event_id: str | None = None,
    detail: object = None,
) -> Path:
    """Append one action record to ``journal/journal-YYYY-MM.jsonl``.

    Journal-first is the recording-before-acknowledgment anchor: every state
    mutation writes here before it touches the cursor/incident/spool, so a crash
    mid-write redelivers rather than silently dropping the event.
    """
    record = {
        "ts": iso(now),
        "action": action,
        "rule": rule,
        "event_id": event_id,
        "detail": detail,
    }
    journal_dir = state_dir / "journal"
    journal_dir.mkdir(parents=True, exist_ok=True)
    path = journal_dir / f"journal-{month_str(now)}.jsonl"
    line = json.dumps(record, separators=(",", ":"), sort_keys=False)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(line + "\n")
    return path

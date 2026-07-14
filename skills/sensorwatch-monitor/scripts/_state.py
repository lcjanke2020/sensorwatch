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


def force_utf8_io() -> None:
    """Emit/read UTF-8 regardless of platform locale. A redirected stdout on
    Windows defaults to the ANSI code page (cp1252), which cannot encode e.g. a
    '≥' that a user's bootstrap.md might contain — state_summary echoes that file
    verbatim. Call once at the top of every script's main()."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            try:
                reconfigure(encoding="utf-8")
            except (ValueError, OSError):  # pragma: no cover - stream already used
                pass


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
    """Parse an ISO-8601 timestamp (accepts a trailing ``Z``). A timestamp with
    no offset is assumed UTC, so aware and naive ``--now`` inputs never mix — a
    bare subtraction of the two would otherwise raise an uncaught ``TypeError``
    (outside the JSON + exit-code contract)."""
    try:
        dt = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except (ValueError, AttributeError) as exc:
        raise Usage(f"not an ISO-8601 timestamp: {text!r}") from exc
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def parse_iso_state(text: str) -> datetime:
    """:func:`parse_iso` for a timestamp read from a STATE file — a bad value is
    corrupt state (Fatal, exit 1), not a caller usage error (exit 2)."""
    try:
        return parse_iso(text)
    except Usage as exc:
        raise Fatal(f"corrupt state timestamp: {exc}") from exc


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
    """Rule name → filesystem-safe slug, byte-identical to the Rust watcher's
    spool slug (``event.rs`` ``slug``): ASCII-lowercase, every character outside
    ``[a-z0-9._-]`` folded to ``-``, truncated to 50 bytes, ``"rule"`` if nothing
    survives — and, crucially, **no** leading/trailing strip. Matching it exactly
    keeps the incident filename aligned with the spool filename, so two distinct
    rules (e.g. ``psu.12v`` vs ``psu-12v``) never collide into one incident."""
    out = []
    for ch in name:
        c = chr(ord(ch) + 32) if "A" <= ch <= "Z" else ch  # ASCII-lowercase only
        out.append(c if ("a" <= c <= "z" or "0" <= c <= "9" or c in "._-") else "-")
    s = "".join(out)[:50]  # pure ASCII here, so char slice == byte truncation
    return s or "rule"


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


def write_text_atomic(path: Path, text: str) -> None:
    """Write ``text`` via a temp file + ``os.replace`` so a reader never sees a
    half-written file (a crash leaves the old file intact). Used for JSON state
    and for incident Markdown, whose machine-read header must stay consistent."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    try:
        tmp.write_text(text, encoding="utf-8")
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def save_json_atomic(path: Path, obj: dict) -> None:
    """Write JSON atomically (see :func:`write_text_atomic`)."""
    write_text_atomic(path, json.dumps(obj, separators=(",", ":"), sort_keys=False))


def load_cursor(state_dir: Path) -> dict:
    """Load + shape-check ``cursor.json`` — a controlled Fatal beats an
    ``AttributeError`` deep in the ack path when the file was hand-edited into a
    bad shape (it is documented as corrupt-state → exit 1)."""
    cursor = load_json(state_dir / "cursor.json")
    if not isinstance(cursor.get("acked_ids_recent", []), list):
        raise Fatal("cursor.json: acked_ids_recent is not a list")
    if not _is_int(cursor.get("last_acked_seq", 0)):
        raise Fatal("cursor.json: last_acked_seq is not an int")
    return cursor


def load_escalation(state_dir: Path) -> dict:
    """Load + shape-check ``escalation.json``. Validating the counters here (not at
    use) means notify.py fails BEFORE delivery on a wrong-shape ledger, instead of
    raising a ``TypeError`` after the outbox notice is already written."""
    esc = load_json(state_dir / "escalation.json")
    per_rule = esc.get("per_rule", {})
    if not isinstance(per_rule, dict):
        raise Fatal("escalation.json: per_rule is not an object")
    for rule, info in per_rule.items():
        if not isinstance(info, dict):
            raise Fatal(f"escalation.json: per_rule[{rule!r}] is not an object")
    if not _is_int(esc.get("notifications_today", 0)):
        raise Fatal("escalation.json: notifications_today is not an int")
    date = esc.get("date")
    if date is not None and not isinstance(date, str):
        raise Fatal("escalation.json: date is not a string")
    return esc


# ---- incident header (the one machine-maintained block inside an incident .md) ----
# open_incident.py is the sole WRITER; state_summary.py and escalation_gate.py are
# READERS via read_open_incidents. Keep this the single reader/writer pair so the
# header format cannot silently desync across scripts.

def incident_get_field(lines: list, key: str, default=None):
    prefix = f"- {key}:"
    for line in lines:
        if line.startswith(prefix):
            return line[len(prefix):].strip()
    return default


def incident_set_field(lines: list, key: str, value: str) -> bool:
    prefix = f"- {key}:"
    for i, line in enumerate(lines):
        if line.startswith(prefix):
            lines[i] = f"- {key}: {value}"
            return True
    return False


def incident_latest_event_time(lines: list) -> tuple:
    """``(newest_event_time | None, unorderable)`` over an incident's
    ``- <id> @ <ts> …`` event bullets. Used by the reconciler to order recovery
    evidence against what the incident has already recorded — a ``cleared``
    older than the incident's newest event must never close it (the fire may
    have happened while the logger was blind).

    ``unorderable`` is True when ANY event-shaped bullet carries a timestamp
    that does not parse: a partial maximum over the remaining bullets could
    silently omit the newest fire, so consumers must fail closed rather than
    trust it (validate_event rejects unparseable timestamps before recording,
    so this arises only from legacy or hand-edited files — exactly the records
    that should stay with a human)."""
    latest: datetime | None = None
    unorderable = False
    for line in lines:
        if not line.startswith("- ") or " @ " not in line:
            continue
        # The event-line format is `- <id> @ <ts>  <state>  value=…` — TWO
        # spaces after the timestamp field. Splitting on that double space
        # consumes the timestamp losslessly even if a legacy line recorded a
        # space-separated ISO form ("2026-02-18 08:03:00-05:00"); a first-
        # whitespace tokenizer would truncate that to its bare date, which
        # still parses (as midnight) and silently mis-orders the evidence.
        token = line.split(" @ ", 1)[1].split("  ", 1)[0].strip()
        try:
            ts = parse_iso(token)
        except Usage:
            unorderable = True
            continue
        if latest is None or ts > latest:
            latest = ts
    return latest, unorderable


def read_open_incidents(state_dir: Path) -> list:
    """One record per ``incidents/open/*.md``, parsed from its header block."""
    open_dir = state_dir / "incidents" / "open"
    out = []
    if not open_dir.is_dir():
        return out
    for path in sorted(open_dir.glob("*.md")):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        out.append({
            "path": path,
            "rule": incident_get_field(lines, "rule", path.stem),
            "severity": incident_get_field(lines, "severity", "") or "",
            "opened": incident_get_field(lines, "opened", "?"),
            "snooze_until": incident_get_field(lines, "snooze_until", "?"),
            "events": incident_get_field(lines, "events", "?"),
            "status": incident_get_field(lines, "status", "open"),
            "line_count": len(lines),
        })
    return out


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


def _parseable_ts(value: object) -> bool:
    """A non-empty, ISO-8601-parseable timestamp string **with no internal
    whitespace** (the ``T`` form the emitter uses). The emitter's timestamps
    are replay-stable sample timestamps and always qualify; enforcing this
    here means nothing unorderable is ever RECORDED into the cursor or an
    incident file (the reconciler's evidence ordering depends on it). The
    whitespace restriction exists because ``fromisoformat`` also accepts a
    space-separated datetime, which would round-trip ambiguously through the
    whitespace-delimited incident-line format."""
    if not isinstance(value, str) or not value or any(ch.isspace() for ch in value):
        return False
    try:
        parse_iso(value)
    except Usage:
        return False
    return True


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
        "timestamp": _parseable_ts,
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

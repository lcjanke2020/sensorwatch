#!/usr/bin/env python3
"""notify.py — deliver an escalation notice through pluggable delivery channels.

Two modes, one CLI:

  * **Routed** (no ``--adapter``): the event's severity selects a list of
    channels from ``<state>/notify.toml`` (``[severity]`` table, else ``default``,
    else ``["outbox"]`` when no config file exists). The list is delivered in
    order, per-channel outcomes are journaled, and the per-rule cooldown is armed
    iff at least one channel succeeded. Exit 0 when every channel delivered, 1
    when any failed.
  * **Explicit** (``--adapter X``): today's single-channel mode, output and
    journal shape unchanged. Used to force one channel (e.g. ``outbox`` for a
    dry run, or a single real transport).

Delivery channels (all stdlib-only):

  * ``ntfy`` — POST the notice to a hosted or self-hosted ntfy topic (the default
    transport; a long random topic name is the shared secret).
  * ``pushover`` — POST to the Pushover messages API (emergency priority + a
    receipt for the acknowledge-required path).
  * ``smtp`` — submit the notice over generic SMTP (bring-your-own credentials;
    subsumes transactional email providers, which all expose an SMTP endpoint).
  * ``outbox`` — write ``outbox/<utc-stamp>-<slug>.md`` atomically (the durable,
    inspectable fallback when no ``notify.toml`` is present).
  * ``stderr`` — print the notice to stderr.

Channel routing and transport credentials live in ``<state>/notify.toml`` in the
machine-local state directory — never in the repo (the state dir is private for
exactly that reason). Adding a channel = a delivery function in ``ADAPTERS`` plus
its config schema (required keys / defaults) in the validators.

On successful delivery notify records the per-rule cooldown (``last_notified``)
and bumps the daily notification count — it is the SOLE writer of those, so the
cooldown is armed only when a notice actually went out (the gate reads them to
decide; a crash before delivery leaves them un-armed, so the redelivery
re-notifies instead of being silently suppressed).

``--issue-draft`` (the tier-3 action; the SKILL directs it when the gate
returns tier >= 3) additionally writes a structured, tracker-ready draft to
``outbox/issues/<utc-stamp>-<slug>.md`` in the SAME invocation. The draft is
written before the delivery attempt so it exists even when every channel fails
(durable evidence is most valuable exactly then), and it is NOT a delivery:
only the notification itself records the cooldown/daily count, exactly once.

    # routed: severity -> channels from <state>/notify.toml
    python notify.py --state-dir <dir> --rule <name> --severity <s> --tier <n> \\
        [--issue-draft] [--incident-file <path>] [--summary "..."] [--now <iso>]
    # explicit: force one channel
    python notify.py --state-dir <dir> --adapter outbox|stderr|ntfy|pushover|smtp \\
        --rule <name> --severity <s> --tier <n> [...]

The body is plain prose that *references* the incident-file path — it never
embeds sensor data (public-repo hygiene, and keeps the draft safe to hand to any
tracker WAF later). Exit 0 success, 1 fatal
(any channel failed / unreadable ledger), 2 usage (bad args, unknown adapter,
malformed notify.toml).
"""

from __future__ import annotations

import argparse
import http.client
import json
import re
import ssl
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from datetime import timezone
from email.message import EmailMessage
from pathlib import Path
from smtplib import SMTP, SMTPException

sys.path.insert(0, str(Path(__file__).parent))
import _state as st  # noqa: E402

# Channels that read a ``notify.toml`` section; ``outbox``/``stderr`` need none.
CONFIGURED_ADAPTERS = frozenset({"ntfy", "pushover", "smtp"})

# Priority defaults per severity (documented in SKILL.md's notify.toml example).
NTFY_DEFAULT_PRIORITY = {"info": 3, "warning": 4, "critical": 5}
PUSHOVER_DEFAULT_PRIORITY = {"info": -1, "warning": 0, "critical": 2}
PUSHOVER_EMERGENCY = 2  # priority that requires retry/expire and returns a receipt

# ntfy's documented topic charset. The topic is a URL path segment AND the shared
# secret, so an out-of-charset char ('#'/'?' redirect the POST, non-ASCII crashes
# the encoder) must be rejected at config time, not delivered to the wrong topic.
NTFY_TOPIC_RE = re.compile(r"[A-Za-z0-9_-]{1,64}")


def _one_line(text: str) -> str:
    """Collapse any newlines/runs of whitespace to single spaces so a multi-line
    --summary cannot restructure the notice or corrupt the journal line."""
    return " ".join(text.split())


def _title(args: argparse.Namespace) -> str:
    """ASCII-only notice title (shared by ntfy/pushover/smtp). A plain hyphen, not
    the body's em-dash: http.client encodes header values latin-1, so an em-dash
    in an ntfy ``Title:`` header would raise UnicodeEncodeError."""
    return f"sensorwatch monitor - {args.severity} (tier {args.tier})"


def _body(args: argparse.Namespace, now_iso: str) -> str:
    summary = _one_line(args.summary) if args.summary else "See the referenced incident file for detail."
    lines = [
        f"# sensorwatch monitor — {args.severity} (tier {args.tier})",
        "",
        f"- rule: {_one_line(args.rule)}",           # newlines can't restructure the notice
        f"- severity: {args.severity}",
        f"- tier: {args.tier}",
        f"- at: {now_iso}",
    ]
    if args.incident_file:
        lines.append(f"- incident: {_one_line(args.incident_file)}")
    lines.append("")
    lines.append(summary)
    lines.append("")
    lines.append("_Sensor data lives in the incident file, not here._")
    return "\n".join(lines) + "\n"


def _read_secret(path: str | None, what: str) -> str:
    """Read a single-line secret from a 0600 file. Missing/unreadable → Fatal. The
    error names the *path*, never the secret's contents."""
    if not path:
        raise st.Fatal(f"{what} not configured")
    try:
        return Path(path).expanduser().read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise st.Fatal(f"cannot read {what}: {exc}") from exc


def _unique_stamped_path(directory: Path, now, slug: str) -> Path:
    """``<dir>/<utc-stamp>-<slug>.md``, disambiguated with ``-<n>``. Second-
    precision stamps collide when two notices for one rule land in the same
    second (or under a pinned --now); neither may overwrite the other."""
    stamp = now.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = directory / f"{stamp}-{slug}.md"
    n = 1
    while path.exists():
        path = directory / f"{stamp}-{slug}-{n}.md"
        n += 1
    return path


# ---- delivery channels: (state_dir, now, slug, body, args, cfg) -> target_str ----

def _deliver_outbox(state_dir: Path, now, slug: str, body: str, args, cfg) -> str:
    outbox = state_dir / "outbox"
    outbox.mkdir(parents=True, exist_ok=True)
    # write_text_atomic uses a pid-suffixed tmp + os.replace.
    path = _unique_stamped_path(outbox, now, slug)
    st.write_text_atomic(path, body)
    return str(path)


def _deliver_stderr(state_dir: Path, now, slug: str, body: str, args, cfg) -> str:
    sys.stderr.write(body)
    return "stderr"


def _deliver_ntfy(state_dir: Path, now, slug: str, body: str, args, cfg) -> str:
    server = (cfg.get("server") or "https://ntfy.sh").rstrip("/")
    topic = cfg.get("topic")
    if not topic:  # defensive: routed/explicit config validation catches this first
        raise st.Fatal("ntfy: no topic configured")
    priority = (cfg.get("priority") or {}).get(args.severity, NTFY_DEFAULT_PRIORITY[args.severity])
    token = cfg.get("token") or ""
    # Build the request INSIDE the try: a schemeless server ('ntfy.sh' vs
    # 'https://ntfy.sh') makes Request() raise ValueError whose message echoes the
    # full URL — including the secret topic. Every failure below re-raises a Fatal
    # that names NO URL / topic (only urllib.error.URLError.reason is a topic-free
    # transport cause, so it stays interpolated).
    try:
        req = urllib.request.Request(f"{server}/{topic}", data=body.encode("utf-8"), method="POST")
        req.add_header("Title", _title(args))
        req.add_header("Priority", str(priority))
        req.add_header("Tags", "sensorwatch")
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        with urllib.request.urlopen(req, timeout=10) as resp:
            status = resp.status
    except urllib.error.HTTPError as exc:
        raise st.Fatal(f"ntfy: HTTP {exc.code}") from exc
    except urllib.error.URLError as exc:
        raise st.Fatal(f"ntfy: {exc.reason}") from exc
    except (http.client.HTTPException, ValueError) as exc:
        # ValueError ('unknown url type', UnicodeError) / InvalidURL messages can
        # echo the URL — the topic is the secret, so NEVER interpolate them.
        raise st.Fatal(f"ntfy: invalid server or topic ({type(exc).__name__})") from exc
    if not 200 <= status < 300:
        raise st.Fatal(f"ntfy: HTTP {status}")
    # The topic is the shared secret on hosted ntfy.sh — keep it OUT of the
    # journaled/stdout target (it still travels in the HTTP path at transit time,
    # but st.emit output is parsed back into the driving agent's context).
    return f"ntfy:{server}"


def _deliver_pushover(state_dir: Path, now, slug: str, body: str, args, cfg) -> str:
    api_url = cfg.get("api_url") or "https://api.pushover.net/1/messages.json"
    token = _read_secret(cfg.get("token_file"), "pushover token_file")
    user = _read_secret(cfg.get("user_file"), "pushover user_file")
    priority = (cfg.get("priority") or {}).get(args.severity, PUSHOVER_DEFAULT_PRIORITY[args.severity])
    fields = {
        "token": token,
        "user": user,
        "title": _title(args),
        "message": body,
        "priority": str(priority),
    }
    if int(priority) == PUSHOVER_EMERGENCY:  # emergency: retry until acknowledged
        fields["retry"] = str(cfg.get("retry", 60))
        fields["expire"] = str(cfg.get("expire", 3600))
    data = urllib.parse.urlencode(fields).encode("utf-8")
    try:
        req = urllib.request.Request(api_url, data=data, method="POST")
        with urllib.request.urlopen(req, timeout=10) as resp:
            status = resp.status
            payload = resp.read(65536).decode("utf-8")   # cap: a hostile api_url can't balloon RAM
    except urllib.error.HTTPError as exc:
        # Never echo the request (it carries token/user); report the code only.
        raise st.Fatal(f"pushover: HTTP {exc.code}") from exc
    except urllib.error.URLError as exc:
        raise st.Fatal(f"pushover: {exc.reason}") from exc
    except (http.client.HTTPException, ValueError) as exc:
        raise st.Fatal(f"pushover: invalid request ({type(exc).__name__})") from exc
    try:
        parsed = json.loads(payload)
    except json.JSONDecodeError as exc:
        raise st.Fatal("pushover: response was not JSON") from exc
    if not 200 <= status < 300 or parsed.get("status") != 1:
        raise st.Fatal(f"pushover: API status {parsed.get('status')!r}")
    receipt = parsed.get("receipt")
    return f"pushover:receipt={receipt}" if receipt else "pushover:ok"


def _deliver_smtp(state_dir: Path, now, slug: str, body: str, args, cfg) -> str:
    host = cfg.get("host")
    port = int(cfg.get("port", 587))
    mail_from = cfg.get("mail_from")
    mail_to = cfg.get("mail_to")
    username = cfg.get("username")
    # Read the password before connecting so a missing file fails cleanly.
    password = _read_secret(cfg.get("password_file"), "smtp password_file") if username else None

    msg = EmailMessage()
    msg["From"] = mail_from
    msg["To"] = mail_to
    msg["Subject"] = _title(args)
    msg.set_content(body)

    try:
        with SMTP(host, port, timeout=10) as smtp:
            if cfg.get("starttls", True):
                smtp.starttls(context=ssl.create_default_context())
            if username:
                smtp.login(username, password)
            smtp.send_message(msg)
    except (SMTPException, OSError, ssl.SSLError) as exc:
        raise st.Fatal(f"smtp: {exc}") from exc
    return f"smtp:{mail_to}"


ADAPTERS = {
    "outbox": _deliver_outbox,
    "stderr": _deliver_stderr,
    "ntfy": _deliver_ntfy,
    "pushover": _deliver_pushover,
    "smtp": _deliver_smtp,
}


# ---- tier-3 issue draft (an artifact, not a delivery channel) ----

def _issue_draft_body(args: argparse.Namespace, now_iso: str) -> str:
    """A tracker-ready draft: everything a human needs to file the issue by
    hand. Same hygiene as the notice body — plain prose that *references* the
    incident file, never sensor data."""
    summary = _one_line(args.summary) if args.summary else "See the referenced incident file for detail."
    incident = _one_line(args.incident_file) if args.incident_file else "(no incident file referenced)"
    lines = [
        f"# Issue draft: {_one_line(args.rule)} — {args.severity} (tier {args.tier})",
        "",
        f"Suggested title: sensorwatch: {_one_line(args.rule)} {args.severity} persisting (tier {args.tier})",
        "",
        f"- rule: {_one_line(args.rule)}",
        f"- severity: {args.severity}",
        f"- tier: {args.tier}",
        f"- drafted: {now_iso}",
        f"- incident: {incident}",
        "",
        "## Summary",
        "",
        summary,
        "",
        "## Timeline",
        "",
        "The incident file (path above) carries this rule's event history; the",
        "journal (journal/journal-YYYY-MM.jsonl) holds the full record.",
        "",
        "## Suggested next actions",
        "",
        "- [ ] Review the incident file and baseline.md for context",
        "- [ ] Confirm the reading against an independent source (BIOS / vendor tool)",
        "- [ ] File in the tracker of record, linking the incident-file path",
        "- [ ] Close the incident (open_incident.py --close) once resolved",
        "",
        "_Sensor data lives in the incident file, not here._",
    ]
    return "\n".join(lines) + "\n"


def _write_issue_draft(state_dir: Path, now, slug: str, args: argparse.Namespace) -> str:
    """Write the draft to ``outbox/issues/``. A distinct directory keeps the
    tier-3 deliverable separate from notification-fallback notices, and the
    write is NOT a delivery — the caller must not record cooldown/daily-count
    for it. Failure is Fatal: the draft is the tier-3 deliverable, not a
    side effect."""
    issues = state_dir / "outbox" / "issues"
    try:
        issues.mkdir(parents=True, exist_ok=True)
        path = _unique_stamped_path(issues, now, slug)
        st.write_text_atomic(path, _issue_draft_body(args, st.iso(now)))
    except OSError as exc:
        raise st.Fatal(f"issue draft: {exc}") from exc
    return str(path)


# ---- notify.toml (machine-local channel routing + transport config) ----

def _load_config(state_dir: Path) -> dict | None:
    """Parse ``<state>/notify.toml``. Returns ``None`` when the file is **absent**
    (routed mode then takes the outbox fallback — the LEO-338 behavior); a
    present-but-empty file parses to ``{}``. A parse error / unreadable file is a
    Usage error (exit 2) so it fails before any side effect. The absent-vs-present
    distinction is what lets a half-configured file be a loud error instead of a
    silent outbox write (see ``_run_routed`` / ``_routed_adapters``)."""
    path = state_dir / "notify.toml"
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except FileNotFoundError:
        return None
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise st.Usage(f"notify.toml: {exc}") from exc


def _as_name_list(where: str, value: object) -> list:
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise st.Usage(f"notify.toml: {where} must be a list of adapter names")
    return value


def _routed_adapters(config: dict, severity: str) -> list:
    """Channels for this severity in a PRESENT config: ``[severity][sev]`` (``[]``
    is a valid, deliberate mute), else ``default``. A present config that routes
    nothing for the severity is a **config error**, not a silent outbox write — the
    outbox fallback is reserved for a truly-absent notify.toml (see
    ``_run_routed``), so a half-configured file can never bury a critical notice in
    the outbox and arm the cooldown for a delivery that never reached a phone."""
    severity_table = config.get("severity", {})
    if not isinstance(severity_table, dict):
        raise st.Usage("notify.toml: [severity] is not a table")
    if severity in severity_table:
        return _as_name_list(f"severity.{severity}", severity_table[severity])
    if "default" in config:
        return _as_name_list("default", config["default"])
    raise st.Usage(
        f"notify.toml: nothing routed for severity {severity!r} — add a "
        f"[severity].{severity} entry (use [] to mute) or a top-level 'default'"
    )


def _validate_config(config: dict) -> None:
    """All config checks that must precede any delivery: every adapter name
    referenced in ``default`` and each ``[severity]`` list must be registered."""
    referenced: list[str] = []
    if "default" in config:
        referenced += _as_name_list("default", config["default"])
    severity_table = config.get("severity", {})
    if not isinstance(severity_table, dict):
        raise st.Usage("notify.toml: [severity] is not a table")
    unknown_sev = sorted(k for k in severity_table if k not in st.SEVERITIES)
    if unknown_sev:  # a typo'd key must be loud, not a silent miss → outbox fallback
        raise st.Usage(
            f"notify.toml: unknown severity key(s) in [severity]: "
            f"{', '.join(unknown_sev)}; valid: {', '.join(st.SEVERITIES)}"
        )
    for sev, names in severity_table.items():
        referenced += _as_name_list(f"severity.{sev}", names)
    unknown = sorted({name for name in referenced if name not in ADAPTERS})
    if unknown:
        raise st.Usage(
            f"notify.toml: unknown adapter(s): {', '.join(unknown)}; "
            f"available: {', '.join(sorted(ADAPTERS))}"
        )


def _is_plain_int(value: object) -> bool:
    """int, but not bool (a TOML bool is never a valid port/priority/retry)."""
    return isinstance(value, int) and not isinstance(value, bool)


def _require_str(name: str, key: str, value: object) -> None:
    """A required field: present and a non-empty string. Catches both a missing key
    and a wrong-typed one (``token_file = 123``) as a clean config error, so
    neither reaches delivery-time code where a sibling channel could already have
    fired and armed the cooldown."""
    if not isinstance(value, str) or not value:
        raise st.Usage(f"notify.toml: [{name}] requires '{key}' as a non-empty string")


def _optional_str(name: str, key: str, cfg: dict) -> None:
    """An optional field that, WHEN present, must be a string (``server``,
    ``api_url``, ``username``)."""
    if key in cfg and not isinstance(cfg[key], str):
        raise st.Usage(f"notify.toml: [{name}] {key} must be a string")


def _validate_priority(name: str, cfg: dict) -> None:
    """A ``priority`` map, if present, must be a table keyed by severity with int
    values — so a scalar (``priority = 5``), a typo'd severity key (silently
    ignored → the built-in default is used), or a non-int value all fail as clean
    config errors instead of an AttributeError / ValueError / silent-default at
    delivery time."""
    prio = cfg.get("priority")
    if prio is None:
        return
    if not isinstance(prio, dict):
        raise st.Usage(
            f"notify.toml: [{name}] priority must be a table "
            "(e.g. { info = 3, warning = 4, critical = 5 })"
        )
    for sev, value in prio.items():
        if sev not in st.SEVERITIES:  # a typo here is as silent as one in [severity]
            raise st.Usage(
                f"notify.toml: [{name}] priority has an unknown severity key {sev!r}; "
                f"valid: {', '.join(st.SEVERITIES)}"
            )
        if not _is_plain_int(value):
            raise st.Usage(f"notify.toml: [{name}] priority.{sev} must be an integer")


def _validate_required_keys(config: dict, adapters: list) -> None:
    """For each channel routed for THIS event, every field it reads is present
    (when required) and well-typed BEFORE any delivery — so a config-schema error
    is a clean exit 2 and can never reach delivery-time code where a preceding
    sibling channel has already succeeded and armed the cooldown. A missing secret
    *file* (a present, valid path) stays a delivery-time channel failure."""
    for name in adapters:
        cfg = config.get(name, {})
        if not isinstance(cfg, dict):
            raise st.Usage(f"notify.toml: [{name}] is not a table")
        if name == "ntfy":
            _require_str(name, "topic", cfg.get("topic"))
            # The topic is a URL path segment and the shared secret. Anything outside
            # ntfy's charset would silently redirect the POST ('#'/'?'), crash the
            # header/URL encoder (non-ASCII), or count as delivery to the wrong topic
            # — all of which arm the cooldown. Reject at config time.
            if not NTFY_TOPIC_RE.fullmatch(cfg["topic"]):
                raise st.Usage(
                    "notify.toml: [ntfy] topic must be 1-64 characters of [A-Za-z0-9_-] "
                    "(ntfy's topic charset)"
                )
            _optional_str(name, "server", cfg)
            token = cfg.get("token")  # Authorization header encodes latin-1 → require ASCII
            if token is not None and (not isinstance(token, str) or not token.isascii()):
                raise st.Usage("notify.toml: [ntfy] token must be an ASCII string")
            _validate_priority(name, cfg)
        elif name == "pushover":
            # Missing/wrong-typed KEY = config error (exit 2, before any side effect);
            # a missing secret FILE stays a delivery-time channel failure (_read_secret).
            _require_str(name, "token_file", cfg.get("token_file"))
            _require_str(name, "user_file", cfg.get("user_file"))
            _optional_str(name, "api_url", cfg)
            for key in ("retry", "expire"):  # sent as form fields; a wrong type 400s at delivery
                if key in cfg and not _is_plain_int(cfg[key]):
                    raise st.Usage(f"notify.toml: [pushover] {key} must be an integer")
            _validate_priority(name, cfg)
        elif name == "smtp":
            for key in ("host", "mail_from", "mail_to"):
                _require_str(name, key, cfg.get(key))
            _optional_str(name, "username", cfg)
            if cfg.get("username"):  # login is attempted only when username is set
                _require_str(name, "password_file", cfg.get("password_file"))
            if "starttls" in cfg and not isinstance(cfg["starttls"], bool):
                raise st.Usage("notify.toml: [smtp] starttls must be true or false")
            port = cfg.get("port", 587)
            if not _is_plain_int(port):
                raise st.Usage("notify.toml: [smtp] port must be an integer")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="notify.py",
        description="Deliver an escalation notice via routed channels (or one explicit adapter).",
    )
    parser.add_argument("--state-dir", help="state directory (or $%s)" % st.STATE_ENV)
    parser.add_argument(
        "--adapter",
        help="force one channel (outbox|stderr|ntfy|pushover|smtp); "
             "omit for severity-routed fan-out from notify.toml",
    )
    parser.add_argument("--rule", required=True)
    parser.add_argument("--severity", required=True, choices=st.SEVERITIES)
    parser.add_argument("--tier", required=True, type=int, help="escalation tier (0-4)")
    parser.add_argument(
        "--issue-draft", action="store_true",
        help="also write a tracker-ready draft to outbox/issues/ "
             "(the tier-3 action; not a delivery — records no cooldown)",
    )
    parser.add_argument("--incident-file", help="path referenced (never embedded) in the notice")
    parser.add_argument("--summary", help="one-line plain-prose summary")
    parser.add_argument("--now", help="ISO-8601 timestamp (default: now UTC)")
    return parser


def run(args: argparse.Namespace) -> int:
    state_dir = st.resolve_state_dir(args.state_dir)
    now = st.resolve_now(args.now)

    if not 0 <= args.tier <= 4:
        raise st.Usage(f"--tier must be 0-4, got {args.tier}")

    slug = st.slugify(args.rule)
    body = _body(args, st.iso(now))

    if args.adapter is not None:
        return _run_explicit(args, state_dir, now, slug, body)
    return _run_routed(args, state_dir, now, slug, body)


def _run_explicit(args: argparse.Namespace, state_dir: Path, now, slug: str, body: str) -> int:
    """Single explicit channel — today's shape, byte-compatible with LEO-338."""
    adapter = ADAPTERS.get(args.adapter)
    if adapter is None:
        raise st.Usage(
            f"unknown adapter {args.adapter!r}; available: {', '.join(sorted(ADAPTERS))}"
        )
    # A real channel still reads its notify.toml section (outbox/stderr need none;
    # an absent file yields {}, so a real adapter with no config fails required-key
    # validation just as before).
    config = (_load_config(state_dir) or {}) if args.adapter in CONFIGURED_ADAPTERS else {}
    _validate_required_keys(config, [args.adapter])

    # Validate the ledger BEFORE any side effect: a wrong-shape escalation.json
    # must fail cleanly here, not after the notice is already delivered and
    # journaled (which a retry would then duplicate).
    esc = st.load_escalation(state_dir)

    # Draft before delivery: it must exist even when the channel then fails
    # (exit 1 with the draft on disk beats exit 1 with nothing durable). A
    # failed-then-retried delivery writes a second, disambiguated draft —
    # harmless, and preferable to a success path with no draft.
    issue_draft = _write_issue_draft(state_dir, now, slug, args) if args.issue_draft else None

    target = adapter(state_dir, now, slug, body, args, config.get(args.adapter, {}))

    detail = {"adapter": args.adapter, "tier": args.tier, "target": target}
    if issue_draft:
        detail["issue_draft"] = issue_draft
    st.journal_append(state_dir, now, "notify", rule=args.rule, detail=detail)
    _record_delivery(esc, state_dir / "escalation.json", now, args.rule)
    result = {
        "adapter": args.adapter,
        "delivered": True,
        "tier": args.tier,
        "target": target,
    }
    if issue_draft:
        result["issue_draft"] = issue_draft
    st.emit(result)
    return 0


def _run_routed(args: argparse.Namespace, state_dir: Path, now, slug: str, body: str) -> int:
    """Severity-routed fan-out from notify.toml. All config validation precedes
    any delivery; the cooldown arms iff at least one channel succeeded."""
    config = _load_config(state_dir)
    if config is None:                       # no notify.toml → LEO-338 outbox fallback
        adapters = ["outbox"]
        config = {}
    else:
        _validate_config(config)
        adapters = _routed_adapters(config, args.severity)
        _validate_required_keys(config, adapters)

    # Ledger validation BEFORE any side effect (journaling included).
    esc = st.load_escalation(state_dir)

    # The draft precedes delivery (and survives an all-channels-failed exit 1);
    # it is written even for a deliberately-muted severity — the tier-3
    # deliverable is independent of which channels are routed.
    issue_draft = _write_issue_draft(state_dir, now, slug, args) if args.issue_draft else None

    if not adapters:  # empty list = a deliberate no-op for this severity
        detail = {"mode": "routed", "tier": args.tier, "channels": []}
        result = {"delivered": False, "tier": args.tier, "mode": "routed", "channels": []}
        if issue_draft:
            detail["issue_draft"] = issue_draft
            result["issue_draft"] = issue_draft
        st.journal_append(state_dir, now, "notify", rule=args.rule, detail=detail)
        st.emit(result)
        return 0

    channels: list[dict] = []
    for name in adapters:
        adapter = ADAPTERS[name]
        try:
            target = adapter(state_dir, now, slug, body, args, config.get(name, {}))
            channels.append({"adapter": name, "ok": True, "target": target})
        except st.Fatal as exc:  # per-channel isolation: one bad channel != total failure
            channels.append({"adapter": name, "ok": False, "error": str(exc)})
        except Exception as exc:
            # Defense in depth: adapters raise st.Fatal with sanitized reasons, but a
            # leaked raw exception could echo a secret (e.g. a URL with the topic)
            # into this journaled/emitted error field — record only its type.
            channels.append({"adapter": name, "ok": False, "error": f"unexpected {type(exc).__name__}"})

    any_ok = any(c["ok"] for c in channels)
    all_ok = all(c["ok"] for c in channels)

    detail = {"mode": "routed", "tier": args.tier, "channels": channels}
    if issue_draft:
        detail["issue_draft"] = issue_draft
    st.journal_append(state_dir, now, "notify", rule=args.rule, detail=detail)
    # A delivered notice spends the rule's cooldown and a daily-cap slot. Recording
    # it HERE — after delivery, and only when ≥1 channel succeeded — is what closes
    # the lost-notification gap: no delivery, no cooldown, so a redelivery
    # re-notifies instead of being suppressed. notify is the sole writer of
    # last_notified / notifications_today; the gate only reads them.
    if any_ok:
        _record_delivery(esc, state_dir / "escalation.json", now, args.rule)

    result = {
        "delivered": any_ok,
        "tier": args.tier,
        "mode": "routed",
        "channels": channels,
    }
    if issue_draft:
        result["issue_draft"] = issue_draft
    st.emit(result)
    return 0 if all_ok else 1


def _record_delivery(esc: dict, esc_path: Path, now, rule: str) -> None:
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

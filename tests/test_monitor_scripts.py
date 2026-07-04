"""Tests for the sensorwatch-monitor helper scripts.

The scripts' CLI contract (exit codes + stdout JSON) IS the interface the skill
consumes, so every case drives them through ``subprocess`` exactly as an agent
would — never by importing them. State lives in a ``tmp_path`` directory and
``--now`` is injected everywhere time matters, so nothing reads the wall clock
(mirroring the Rust CLI's clock-free determinism).

Event fixtures under ``tests/fixtures/events/`` are REAL ``watch --spool-dir``
output (committed bytes, not hand-typed): the fired + cleared events come from a
replayed PSU-sag log; the source-unavailable event from a live one-shot watch on
this (non-Windows) host, where the sensor source is always unavailable.

Runs identically on the Linux and Windows CI jobs — the scripts are pure,
stdlib-only, pathlib-portable file manipulation.
"""

import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
SCRIPTS = REPO / "skills" / "sensorwatch-monitor" / "scripts"
FIXTURES = Path(__file__).resolve().parent / "fixtures" / "events"

T0 = "2026-02-18T08:00:00-05:00"

# The sdist ships tests/ but not skills/ (MANIFEST.in, matching the usage skill),
# so downstream packagers run this suite from a tree without the scripts. Skip
# cleanly there; CI and local runs execute from the git checkout, where they exist.
pytestmark = pytest.mark.skipif(
    not SCRIPTS.is_dir(),
    reason="sensorwatch-monitor scripts absent (e.g. an sdist without skills/)",
)


# ---- helpers ----

def run_script(name, *args):
    # Decode as UTF-8 explicitly: the scripts force UTF-8 output, and a Windows
    # parent would otherwise decode with the ANSI code page and mojibake the
    # non-ASCII bootstrap header the summary echoes.
    return subprocess.run(
        [sys.executable, str(SCRIPTS / name), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )


def init_state(state, now=T0):
    result = run_script("init_state.py", "--state-dir", str(state), "--now", now)
    assert result.returncode == 0, result.stderr
    return result


def fixture_text(name):
    return (FIXTURES / f"{name}.json").read_text(encoding="utf-8")


def load_fixture(name):
    return json.loads(fixture_text(name))


# The frozen 14-key event contract (docs/agent-monitoring.md) — an independent
# copy so producer-side drift is caught here, not only at runtime.
CONTRACT_KEYS = [
    "schema_version", "seq", "id", "rule", "type", "severity", "state",
    "timestamp", "sensor", "reading", "value", "unit", "threshold",
    "samples_in_violation",
]


def _slug(rule):
    # Mirrors _state.slugify / the Rust watcher slug: ASCII-lowercase, keep
    # [a-z0-9._-], fold the rest to '-', truncate 50, no strip.
    out = []
    for ch in rule:
        c = chr(ord(ch) + 32) if "A" <= ch <= "Z" else ch
        out.append(c if ("a" <= c <= "z" or "0" <= c <= "9" or c in "._-") else "-")
    return "".join(out)[:50] or "rule"


def place_event(state, name):
    """Copy a fixture into spool/pending under its real spool filename."""
    event = load_fixture(name)
    dest = state / "spool" / "pending" / f"{event['seq']:010d}-{_slug(event['rule'])}.json"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(fixture_text(name), encoding="utf-8")
    return dest, event


def journal_text(state):
    return "".join(
        p.read_text(encoding="utf-8")
        for p in sorted((state / "journal").glob("journal-*.jsonl"))
    )


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def _notify(state, rule, tier=2, severity="critical", now=T0, adapter="outbox", summary=None):
    args = ["notify.py", "--state-dir", str(state), "--adapter", adapter, "--rule", rule,
            "--severity", severity, "--tier", str(tier), "--now", now]
    if summary is not None:
        args += ["--summary", summary]
    return run_script(*args)


def _synth_event(**over):
    """A valid 14-key event dict, overridable — for exercising code paths the three
    committed fixtures don't cover (arbitrary rule names, many distinct ids)."""
    event = {
        "schema_version": 1, "seq": 1, "id": "synth-1", "rule": "synth",
        "type": "threshold", "severity": "critical", "state": "fired",
        "timestamp": "2026-02-18T08:00:20-05:00", "sensor": "S", "reading": "R",
        "value": 1.0, "unit": "V", "threshold": 2.0, "samples_in_violation": 1,
    }
    event.update(over)
    return event


def _place_synth(state, event):
    dest = state / "spool" / "pending" / f"{event['seq']:010d}-{_slug(event['rule'])}.json"
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps(event, separators=(",", ":")), encoding="utf-8")
    return dest


def _write_open_incident(state, rule, severity="critical", opened="2026-02-18T08:00:00-05:00"):
    body = (
        f"# Incident: {rule}\n- rule: {rule}\n- severity: {severity}\n"
        f"- classification: incident\n- opened: {opened}\n"
        f"- snooze_until: 2026-02-18T14:00:00-05:00\n- events: 1\n- status: open\n"
    )
    (state / "incidents" / "open" / f"{_slug(rule)}.md").write_text(body, encoding="utf-8")


def _open(state, dest, classification="incident", now=T0):
    return run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
                      "--classification", classification, "--now", now)


def _heartbeat(state, kind, now=T0, blind_after=None):
    args = ["heartbeat.py", "--state-dir", str(state), "--kind", kind, "--now", now]
    if blind_after is not None:
        args += ["--blind-after", str(blind_after)]
    return run_script(*args)


# ---- init_state ----

def test_init_creates_tree_and_is_idempotent(tmp_path):
    state = tmp_path / "state"
    result = init_state(state)
    out = json.loads(result.stdout)
    assert out["in_git_worktree"] is False

    for rel in ("journal", "incidents/open", "incidents/closed",
                "spool/pending", "spool/acked", "outbox"):
        assert (state / rel).is_dir(), rel
    for name in ("bootstrap.md", "baseline.md", "cursor.json",
                 "heartbeat.json", "escalation.json"):
        assert (state / name).is_file(), name

    cursor = read_json(state / "cursor.json")
    assert cursor == {
        "schema_version": 1, "last_acked_seq": 0,
        "acked_ids_recent": [], "updated": T0,
    }
    assert '"action":"init"' in journal_text(state)

    # Re-init must not clobber human-curated files.
    (state / "bootstrap.md").write_text("CUSTOM HEADER\n", encoding="utf-8")
    result2 = init_state(state)
    assert (state / "bootstrap.md").read_text(encoding="utf-8") == "CUSTOM HEADER\n"
    assert "bootstrap.md" in json.loads(result2.stdout)["existed"]


def test_init_warns_inside_git_worktree(tmp_path):
    (tmp_path / ".git").mkdir()
    state = tmp_path / "nested" / "state"
    result = init_state(state)
    assert json.loads(result.stdout)["in_git_worktree"] is True
    assert "git work tree" in result.stderr


# ---- ack_event ----

def test_ack_happy_path_order(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, event = place_event(state, "fired-critical-threshold")

    result = run_script(
        "ack_event.py", "--state-dir", str(state),
        "--event-file", str(dest), "--note", "opened incident",
        "--now", "2026-02-18T08:00:26-05:00",
    )
    assert result.returncode == 0, result.stderr
    out = json.loads(result.stdout)
    assert out["deduped"] is False
    assert out["seq"] == 1 and out["last_acked_seq"] == 1

    # journal, cursor, move — all three present.
    assert '"action":"ack"' in journal_text(state)
    assert '"note":"opened incident"' in journal_text(state)
    cursor = read_json(state / "cursor.json")
    assert cursor["last_acked_seq"] == 1
    assert event["id"] in cursor["acked_ids_recent"]
    assert not dest.exists()
    assert (state / "spool" / "acked" / dest.name).exists()


def test_ack_is_idempotent(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")
    run_script("ack_event.py", "--state-dir", str(state),
               "--event-file", str(dest), "--now", T0)
    cursor_after_first = (state / "cursor.json").read_text(encoding="utf-8")

    # Redeliver the same event; the second ack is a no-op on the cursor.
    dest2, _ = place_event(state, "fired-critical-threshold")
    result = run_script("ack_event.py", "--state-dir", str(state),
                        "--event-file", str(dest2), "--now", T0)
    assert result.returncode == 0
    assert json.loads(result.stdout)["deduped"] is True
    assert (state / "cursor.json").read_text(encoding="utf-8") == cursor_after_first
    # The redundant pending duplicate is cleaned up (watch never deletes spool).
    assert not dest2.exists()


def test_ack_redelivered_lower_seq_keeps_max(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest_hi, _ = place_event(state, "cleared")               # seq 2
    run_script("ack_event.py", "--state-dir", str(state),
               "--event-file", str(dest_hi), "--now", T0)

    dest_lo, _ = place_event(state, "fired-critical-threshold")  # seq 1
    result = run_script("ack_event.py", "--state-dir", str(state),
                        "--event-file", str(dest_lo), "--now", T0)
    out = json.loads(result.stdout)
    assert out["deduped"] is False           # different id, still acked
    assert out["last_acked_seq"] == 2        # cursor never regresses
    assert read_json(state / "cursor.json")["last_acked_seq"] == 2


def test_ack_malformed_event_exits_2_untouched(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    bad = state / "spool" / "pending" / "0000000009-bad.json"
    bad.write_text('{"schema_version":1,"not":"an event"}', encoding="utf-8")
    cursor_before = (state / "cursor.json").read_text(encoding="utf-8")

    result = run_script("ack_event.py", "--state-dir", str(state),
                        "--event-file", str(bad), "--now", T0)
    assert result.returncode == 2
    assert bad.exists()                                      # not moved
    assert (state / "cursor.json").read_text(encoding="utf-8") == cursor_before
    assert '"action":"ack"' not in journal_text(state)


def test_ack_event_outside_pending_exits_2(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    stray = tmp_path / "elsewhere.json"
    stray.write_text(fixture_text("fired-critical-threshold"), encoding="utf-8")
    result = run_script("ack_event.py", "--state-dir", str(state),
                        "--event-file", str(stray), "--now", T0)
    assert result.returncode == 2


def test_ack_source_unavailable_event_null_fields(tmp_path):
    # The source-unavailable fixture has null sensor/reading/value/unit/threshold.
    state = tmp_path / "state"
    init_state(state)
    dest, event = place_event(state, "source-unavailable")
    result = run_script("ack_event.py", "--state-dir", str(state),
                        "--event-file", str(dest), "--now", T0)
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["seq"] == event["seq"]


# ---- open_incident ----

def test_open_incident_create_update_snooze(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")
    result = run_script(
        "open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
        "--classification", "incident", "--snooze", "6h",
        "--now", "2026-02-18T08:00:25-05:00",
    )
    out = json.loads(result.stdout)
    assert out["action"] == "open" and out["events"] == 1
    assert out["snooze_until"] == "2026-02-18T14:00:25-05:00"   # +6h arithmetic
    inc = state / "incidents" / "open" / "psu-12v-sag.md"
    assert inc.exists() and "psu-12v-sag-1" in inc.read_text(encoding="utf-8")

    # A second event for the same rule updates in place: append + count bump + re-snooze.
    dest2, _ = place_event(state, "cleared")
    result2 = run_script(
        "open_incident.py", "--state-dir", str(state), "--event-file", str(dest2),
        "--classification", "incident", "--now", "2026-02-18T09:00:00-05:00",
    )
    out2 = json.loads(result2.stdout)
    assert out2["action"] == "update" and out2["events"] == 2
    assert out2["snooze_until"] == "2026-02-18T15:00:00-05:00"
    text = inc.read_text(encoding="utf-8")
    assert "psu-12v-sag-2" in text and "- events: 2" in text


def test_open_incident_redelivery_is_idempotent(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")
    args = ["open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
            "--classification", "incident", "--now", T0]
    run_script(*args)
    result = run_script(*args)   # same event again (crash-before-ack redelivery)
    out = json.loads(result.stdout)
    assert out["action"] == "update-deduped" and out["events"] == 1
    inc = (state / "incidents" / "open" / "psu-12v-sag.md").read_text(encoding="utf-8")
    assert inc.count("psu-12v-sag-1 @") == 1   # not double-appended


def test_close_moves_open_to_closed(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")
    run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
               "--classification", "incident", "--now", T0)
    assert (state / "incidents" / "open" / "psu-12v-sag.md").exists()

    dclose, _ = place_event(state, "cleared")
    result = run_script("open_incident.py", "--state-dir", str(state),
                        "--event-file", str(dclose), "--close",
                        "--now", "2026-02-18T09:30:00-05:00")
    assert json.loads(result.stdout)["action"] == "close"
    assert not (state / "incidents" / "open" / "psu-12v-sag.md").exists()
    closed = list((state / "incidents" / "closed").glob("*.md"))
    assert len(closed) == 1
    body = closed[0].read_text(encoding="utf-8")
    assert "- status: closed" in body and "psu-12v-sag-2" in body
    assert "- events: 2" in body   # cleared event bumps the count (was 1 on open)


def test_benign_journals_without_incident_file(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")
    result = run_script("open_incident.py", "--state-dir", str(state),
                        "--event-file", str(dest),
                        "--classification", "benign", "--now", T0)
    out = json.loads(result.stdout)
    assert out["action"] == "benign" and out["incident_file"] is None
    assert not (state / "incidents" / "open" / "psu-12v-sag.md").exists()
    assert '"classification":"benign"' in journal_text(state)


# ---- escalation_gate ----

def _gate(state, rule, severity, gstate="fired", persistence=1, commit=False,
          now=T0, daily_cap=None, cooldown=None):
    args = ["escalation_gate.py", "--state-dir", str(state), "--rule", rule,
            "--severity", severity, "--state", gstate,
            "--persistence-events", str(persistence), "--now", now]
    if daily_cap is not None:
        args += ["--daily-cap", str(daily_cap)]
    if cooldown is not None:
        args += ["--cooldown-hours", str(cooldown)]
    if commit:
        args.append("--commit")
    result = run_script(*args)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_gate_tier_defaults(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    assert _gate(state, "r", "info", persistence=1)["tier"] == 0
    assert _gate(state, "r", "warning", persistence=1)["tier"] == 1
    assert _gate(state, "r", "warning", persistence=3)["tier"] == 2
    assert _gate(state, "r", "critical", persistence=1)["tier"] == 2
    assert _gate(state, "r", "critical", persistence=3)["tier"] == 3
    # Tiers 0-1 are local writes and always allowed.
    assert _gate(state, "r", "info")["decision"] == "allow"
    assert _gate(state, "r", "warning")["decision"] == "allow"


def test_gate_cooldown_suppresses(tmp_path):
    # The cooldown is armed by notify (on delivery), not by the gate — so a crash
    # before delivery can't suppress the redelivery.
    state = tmp_path / "state"
    init_state(state)
    assert _gate(state, "r1", "critical", now="2026-02-18T08:00:00-05:00")["decision"] == "allow"
    assert _notify(state, "r1", now="2026-02-18T08:00:00-05:00").returncode == 0
    later = _gate(state, "r1", "critical", now="2026-02-18T11:00:00-05:00")  # +3h
    assert later["decision"] == "suppress"


def test_gate_daily_cap_batches(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    _notify(state, "r1", now=T0)                          # notifications_today -> 1
    batched = _gate(state, "r2", "critical", daily_cap=1, now=T0)
    assert batched["decision"] == "batch"


def test_gate_commit_records_tier_not_cooldown(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    before = (state / "escalation.json").read_text(encoding="utf-8")
    _gate(state, "r1", "critical")                      # dry-run
    assert (state / "escalation.json").read_text(encoding="utf-8") == before
    assert '"action":"gate"' not in journal_text(state)

    _gate(state, "r1", "critical", commit=True)
    esc = read_json(state / "escalation.json")
    assert esc["per_rule"]["r1"]["tier"] == 2
    assert "last_notified" not in esc["per_rule"]["r1"]   # gate never arms cooldown
    assert esc["notifications_today"] == 0                # only notify bumps this
    assert '"action":"gate"' in journal_text(state)

    _notify(state, "r1", now=T0)                          # delivery arms cooldown + count
    esc2 = read_json(state / "escalation.json")
    assert esc2["notifications_today"] == 1
    assert "last_notified" in esc2["per_rule"]["r1"]


def test_gate_combination_tier_two_open_criticals(tmp_path):
    # The combination set is derived from incidents/open/, so an open critical
    # incident for r1 plus a critical fire on r2 => tier 4.
    state = tmp_path / "state"
    init_state(state)
    _write_open_incident(state, "r1", severity="critical")
    combo = _gate(state, "r2", "critical")
    assert combo["tier"] == 4
    # A single open critical (just the firing rule) stays tier 2.
    assert _gate(state, "r1", "critical")["tier"] == 2


def test_close_clears_combination_slot(tmp_path):
    # Closing an incident removes it from the combination set — no phantom tier-4
    # for a later unrelated critical (the ledger-desync bug).
    state = tmp_path / "state"
    init_state(state)
    dest, _ = place_event(state, "fired-critical-threshold")   # rule psu-12v-sag, critical
    run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
               "--classification", "incident", "--now", T0)
    assert _gate(state, "other-crit", "critical")["tier"] == 4  # psu + other = 2 open

    dclose, _ = place_event(state, "cleared")
    run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dclose),
               "--close", "--now", "2026-02-18T09:00:00-05:00")
    assert _gate(state, "other-crit", "critical")["tier"] == 2  # phantom gone


def test_naive_now_does_not_crash_gate(tmp_path):
    # A --now without an offset is assumed UTC; mixing it with an aware
    # last_notified must not raise an uncaught TypeError (empty stdout, exit 1).
    state = tmp_path / "state"
    init_state(state)
    assert _gate(state, "r", "critical", now="2026-02-18T08:00:00")["decision"] == "allow"
    assert _notify(state, "r", now="2026-02-18T08:00:00Z").returncode == 0   # aware (Z)
    later = _gate(state, "r", "critical", now="2026-02-18T09:00:00")         # naive, in cooldown
    assert later["decision"] == "suppress"


# ---- notify ----

def test_notify_outbox_and_stderr(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    result = run_script("notify.py", "--state-dir", str(state), "--adapter", "outbox",
                        "--rule", "psu-12v-sag", "--severity", "critical", "--tier", "2",
                        "--summary", "sag; see incident", "--now", T0)
    out = json.loads(result.stdout)
    assert out["adapter"] == "outbox" and out["delivered"] is True
    target = Path(out["target"])
    assert target.exists() and "psu-12v-sag" in target.read_text(encoding="utf-8")
    assert list((state / "outbox").glob("*.tmp")) == []      # atomic: no temp left
    assert '"action":"notify"' in journal_text(state)

    result2 = run_script("notify.py", "--state-dir", str(state), "--adapter", "stderr",
                         "--rule", "psu-12v-sag", "--severity", "critical", "--tier", "2",
                         "--now", T0)
    assert json.loads(result2.stdout)["target"] == "stderr"
    assert "sensorwatch monitor" in result2.stderr


def test_notify_unknown_adapter_exits_2(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    result = run_script("notify.py", "--state-dir", str(state), "--adapter", "carrier-pigeon",
                        "--rule", "r1", "--severity", "info", "--tier", "0", "--now", T0)
    assert result.returncode == 2


# ---- state_summary ----

def _write_incident(state, i):
    opened = f"2026-02-18T{i % 24:02d}:00:00-05:00"
    body = (
        f"# Incident: rule-{i}\n"
        f"- rule: rule-{i}\n"
        f"- severity: warning\n"
        f"- classification: anomaly\n"
        f"- opened: {opened}\n"
        f"- snooze_until: 2026-02-18T20:00:00-05:00\n"
        f"- events: 1\n"
        f"- status: open\n"
    )
    (state / "incidents" / "open" / f"rule-{i}.md").write_text(body, encoding="utf-8")


def test_summary_truncates_keeping_header(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    for i in range(80):
        _write_incident(state, i)
    result = run_script("state_summary.py", "--state-dir", str(state),
                        "--max-bytes", "4096", "--now", T0)
    assert result.returncode == 0, result.stderr
    assert len(result.stdout.encode("utf-8")) <= 4096          # hard cap honored
    assert "# sensorwatch monitor — bootstrap" in result.stdout  # header survives
    assert "omitted" in result.stdout                           # truncation happened


def test_summary_cap_blown_exits_1(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    # A bootstrap that busts the cap on its own — the summary floor cannot fit.
    (state / "bootstrap.md").write_text(("padding line\n" * 500), encoding="utf-8")
    result = run_script("state_summary.py", "--state-dir", str(state),
                        "--max-bytes", "4096", "--now", T0)
    assert result.returncode == 1
    assert "bootstrap.md" in result.stderr


def test_summary_reports_pending_and_incident(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    place_event(state, "fired-critical-threshold")             # seq 1 in pending
    dest2, _ = place_event(state, "cleared")                   # seq 2 in pending
    _ = dest2
    result = run_script("state_summary.py", "--state-dir", str(state),
                        "--max-bytes", "4096", "--now", T0)
    assert result.returncode == 0, result.stderr
    assert "pending=2" in result.stdout
    assert "lowest_pending_seq=1" in result.stdout


# ---- journal rotation ----

def test_journal_monthly_filename_from_now(tmp_path):
    state = tmp_path / "state"
    init_state(state, now=T0)                                  # Feb -> journal-2026-02
    dest, _ = place_event(state, "fired-critical-threshold")
    run_script("ack_event.py", "--state-dir", str(state), "--event-file", str(dest),
               "--now", "2026-03-05T10:00:00-05:00")           # March -> journal-2026-03
    assert (state / "journal" / "journal-2026-02.jsonl").exists()
    assert (state / "journal" / "journal-2026-03.jsonl").exists()


# ---- slug / incident collision (must match the Rust watcher slug) ----

def test_slug_keeps_dot_so_incidents_do_not_collide(tmp_path):
    # 'psu.12v' and 'psu-12v' are two legal, distinct rules; the Rust spool slug
    # keeps '.', so they must map to two distinct incident files (not one).
    state = tmp_path / "state"
    init_state(state)
    for rule in ("psu.12v", "psu-12v"):
        dest = _place_synth(state, _synth_event(rule=rule, id=f"{rule}-1"))
        r = run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
                       "--classification", "incident", "--now", T0)
        assert r.returncode == 0, r.stderr
    files = sorted(p.name for p in (state / "incidents" / "open").glob("*.md"))
    assert files == ["psu-12v.md", "psu.12v.md"]


# ---- incident line cap enforced on write ----

def test_incident_file_trimmed_to_line_cap(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    total = 99
    for i in range(1, total + 1):
        dest = _place_synth(state, _synth_event(
            rule="caprule", seq=i, id=f"caprule-{i}",
            timestamp=f"2026-02-18T08:{i % 60:02d}:00-05:00"))
        r = run_script("open_incident.py", "--state-dir", str(state), "--event-file", str(dest),
                       "--classification", "incident", "--now", f"2026-02-18T09:{i % 60:02d}:00-05:00")
        assert r.returncode == 0, r.stderr
    body = (state / "incidents" / "open" / "caprule.md").read_text(encoding="utf-8")
    assert len(body.splitlines()) <= 80          # INCIDENT_LINE_CAP enforced on write
    assert body.count("older events trimmed") == 1   # ONE marker, not accumulating
    assert f"- events: {total}" in body           # count stays cumulative, not line-limited
    assert f"caprule-{total} @" in body           # the newest event survives (dedup intact)
    assert body.count(" @ ") >= 5                 # real event lines remain, not all markers


# ---- frozen contract pin (catches producer-side drift on the Python side) ----

def test_fixtures_match_frozen_contract():
    for name in ("fired-critical-threshold", "cleared", "source-unavailable"):
        event = load_fixture(name)
        assert list(event.keys()) == CONTRACT_KEYS, name   # exact keys, exact order
        assert event["schema_version"] == 1, name


# ---- controlled fatals on corrupt ledgers (not raw tracebacks) ----

def test_corrupt_cursor_is_fatal(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    (state / "cursor.json").write_text(
        '{"schema_version":1,"last_acked_seq":0,"acked_ids_recent":"oops"}', encoding="utf-8")
    dest, _ = place_event(state, "fired-critical-threshold")
    r = run_script("ack_event.py", "--state-dir", str(state), "--event-file", str(dest), "--now", T0)
    assert r.returncode == 1
    assert "acked_ids_recent" in r.stderr


def test_corrupt_escalation_is_fatal(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    (state / "escalation.json").write_text(
        '{"schema_version":1,"per_rule":["not","objects"]}', encoding="utf-8")
    r = run_script("escalation_gate.py", "--state-dir", str(state), "--rule", "r",
                   "--severity", "critical", "--state", "fired", "--now", T0)
    assert r.returncode == 1
    assert "per_rule" in r.stderr


# ---- notify hardening ----

def test_notify_tier_must_be_int(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    r = run_script("notify.py", "--state-dir", str(state), "--adapter", "outbox", "--rule", "r",
                   "--severity", "info", "--tier", "not-a-number", "--now", T0)
    assert r.returncode == 2


def test_notify_summary_collapsed_to_one_line(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    r = _notify(state, "r", now=T0, summary="alpha\nbeta\n  gamma")
    assert r.returncode == 0, r.stderr
    body = Path(json.loads(r.stdout)["target"]).read_text(encoding="utf-8")
    assert "alpha beta gamma" in body


def test_notify_outbox_no_overwrite_same_second(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    for _ in range(3):
        assert _notify(state, "r", now=T0).returncode == 0   # same rule + same second
    assert len(list((state / "outbox").glob("*.md"))) == 3    # none overwritten


def test_notify_validates_ledger_before_delivery(tmp_path):
    # A wrong-shape escalation.json must fail BEFORE the adapter runs — no outbox
    # file, no journal line (else a retry duplicates the notice).
    state = tmp_path / "state"
    init_state(state)
    (state / "escalation.json").write_text(
        '{"schema_version":1,"per_rule":["bad"]}', encoding="utf-8")
    r = _notify(state, "r", now=T0)
    assert r.returncode == 1
    assert list((state / "outbox").glob("*.md")) == []
    assert '"action":"notify"' not in journal_text(state)


def test_notify_tier_out_of_range(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    r = run_script("notify.py", "--state-dir", str(state), "--adapter", "outbox", "--rule", "r",
                   "--severity", "critical", "--tier", "9", "--now", T0)
    assert r.returncode == 2


# ---- severity escalation feeds the combination tier ----

def test_incident_severity_ratchets_to_critical(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    inc = state / "incidents" / "open" / f"{_slug('A')}.md"
    _open(state, _place_synth(state, _synth_event(rule="A", id="A-1", seq=1, severity="warning")))
    assert "- severity: warning" in inc.read_text(encoding="utf-8")
    assert _gate(state, "B", "critical")["tier"] == 2   # A is warning => not combined

    _open(state, _place_synth(state, _synth_event(rule="A", id="A-2", seq=2, severity="critical")))
    assert "- severity: critical" in inc.read_text(encoding="utf-8")   # ratcheted up
    assert _gate(state, "B", "critical")["tier"] == 4   # now A counts => tier 4


# ---- single-pass template render (a brace in a field must not corrupt) ----

def test_render_rule_with_brace_not_re_substituted(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    rule = "weird-{events_block}"
    r = _open(state, _place_synth(state, _synth_event(rule=rule, id="weird-1")))
    assert r.returncode == 0, r.stderr
    body = (state / "incidents" / "open" / f"{_slug(rule)}.md").read_text(encoding="utf-8")
    assert f"- rule: {rule}" in body                    # literal value preserved
    assert body.count("weird-1 @") == 1                 # one real event line, not corrupted


# ---- gate: malformed state timestamp is fatal, not a usage error ----

def test_gate_malformed_last_notified_is_fatal(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    esc = read_json(state / "escalation.json")
    esc["per_rule"]["r"] = {"severity": "critical", "tier": 2, "last_notified": "not-a-timestamp"}
    (state / "escalation.json").write_text(json.dumps(esc), encoding="utf-8")
    r = run_script("escalation_gate.py", "--state-dir", str(state), "--rule", "r",
                   "--severity", "critical", "--state", "fired", "--now", T0)
    assert r.returncode == 1


# ---- heartbeat.py (liveness + maintenance updater) ----

def test_heartbeat_resets_failures(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    _heartbeat(state, "failure")
    r = _heartbeat(state, "heartbeat", now="2026-02-18T09:00:00-05:00")
    assert json.loads(r.stdout)["consecutive_watch_failures"] == 0
    hb = read_json(state / "heartbeat.json")
    assert hb["consecutive_watch_failures"] == 0
    assert hb["last_heartbeat"] == "2026-02-18T09:00:00-05:00"


def test_heartbeat_failure_marks_monitoring_blind_after_three(tmp_path):
    state = tmp_path / "state"
    init_state(state)
    assert json.loads(_heartbeat(state, "failure").stdout)["monitoring_blind"] is False
    assert json.loads(_heartbeat(state, "failure").stdout)["monitoring_blind"] is False
    third = json.loads(_heartbeat(state, "failure").stdout)
    assert third["consecutive_watch_failures"] == 3
    assert third["monitoring_blind"] is True


def test_heartbeat_maintenance_stamps_date_and_journals(tmp_path):
    state = tmp_path / "state"
    init_state(state, now=T0)
    r = _heartbeat(state, "maintenance", now="2026-03-01T00:05:00-05:00")
    assert json.loads(r.stdout)["last_maintenance_date"] == "2026-03-01"
    assert read_json(state / "heartbeat.json")["last_maintenance_date"] == "2026-03-01"
    assert '"action":"maintenance"' in journal_text(state)

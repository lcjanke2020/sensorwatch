# Worked example: run the agent monitor

A complete, copy-from starting point for standing up the **agentic hardware
monitor** — the deterministic `watch` loop plus the
[`sensorwatch-monitor`](../../skills/sensorwatch-monitor/SKILL.md) agent skill —
on the **free, zero-account path** (hosted [ntfy.sh](https://ntfy.sh) for
notifications).

This directory holds three files:

| File | What it is |
|------|------------|
| [`config.toml`](config.toml) | An annotated `[[rules]]` set exercising all five rule kinds (threshold, rate, stale, missing, source-unavailable). |
| [`notify.toml`](notify.toml) | Channel routing + transport config, ntfy-only by default. **An example — a real one is machine-local and never committed.** |
| [`PILOT_TEST_PLAN.md`](PILOT_TEST_PLAN.md) | An acceptance checklist to prove the monitor actually works before you rely on it. |

> **These thresholds are templates, not tuned values.** Every number in
> `config.toml` is a conservative starting point to adapt to your hardware. Work
> through [`PILOT_TEST_PLAN.md`](PILOT_TEST_PLAN.md) to validate and tune them —
> in particular to confirm a rule *actually fires*.

## Prerequisites

- **Windows + [HWiNFO64](https://www.hwinfo.com/)** with *Shared Memory Support*
  enabled (the sensor source). See the main [README](../../README.md).
- The **`sensorwatch` CLI** built or installed (`sensorwatch snapshot` should
  print live readings).
- For the agent loop: an agent runtime that can run a blocking process and act on
  its exit code, following the
  [`sensorwatch-monitor`](../../skills/sensorwatch-monitor/SKILL.md) skill.

## 1. Rules — `config.toml`

`watch` evaluates the `[[rules]]` array against the live sample stream. The
example covers all five kinds; each is annotated inline. Two things to do before
trusting it:

1. **Match your sensor names.** Matchers are case-insensitive substrings on the
   sensor/reading names. Discover yours with `sensorwatch snapshot` (add
   `--type Temperature` / `--match "+12V"` to filter) and edit the `sensor` /
   `reading` / `type` fields to match.
2. **Tune the thresholds** (the `TUNE:` callouts). The full key reference lives in
   the main [README → "Alert rules"](../../README.md#alert-rules-rules); this
   example does not restate it.

Validate the file parses (a bad rule exits `2`):

```sh
sensorwatch watch --config config.toml --timeout 1
```

## 2. Notifications — `notify.toml`

The default routes everything through **ntfy.sh**, which needs no account:
install the ntfy app, pick a long random topic, subscribe to it, and set the same
topic in `notify.toml`. Pushover (acknowledge-required paging) and SMTP (email)
are included as commented, optional add-ons.

> A **real** `notify.toml` is machine-local and **never committed** — it holds
> your topic (the shared secret) and points at `0600` secret files. Keep it in the
> monitor's state directory, not in a repo.

Notice bodies are reference-only prose (rule, severity, tier, and the
incident-file *path*) — never sensor readings — so nothing hardware-specific
transits a third-party push service.

## 3. Run the monitor

The deterministic watcher is the wake-up primitive: arm one blocking `watch`,
and **the exit code is the message**.

```sh
sensorwatch watch \
    --config config.toml \
    --timeout 1800 \
    --spool-dir <state-dir>/spool/pending
```

- **exit 10** — a rule fired; the event is on stdout and spooled. Triage it.
- **exit 0** — timeout, all quiet (a heartbeat). Do a light pass, re-arm.
- **exit 2** — config error. Stop and fix the rules (do not re-arm blindly).

An agent turns this into a durable, bounded monitoring loop — the wake-up state
machine, the state directory, and the escalation ladder are all in the
[`sensorwatch-monitor` skill](../../skills/sensorwatch-monitor/SKILL.md). The
underlying event contract and exit codes are specified in
[`docs/agent-monitoring.md`](../../docs/agent-monitoring.md) and the
[CLI README](../../rust/sensorwatch-cli/README.md#watch).

## 4. Validate it before you rely on it

A monitor you have not seen fire is a monitor you cannot trust. Work through
[`PILOT_TEST_PLAN.md`](PILOT_TEST_PLAN.md): prove a rule fires (deterministically,
via `--replay`), confirm a notification actually reaches you, and exercise the
crash → redelivery path. Only then promote it from a trial run to something you
depend on.

## See also

- [`sensorwatch-monitor` skill](../../skills/sensorwatch-monitor/SKILL.md) — the agent operating protocol.
- [Main README → Alert rules](../../README.md#alert-rules-rules) — the `[[rules]]` key reference.
- [`docs/agent-monitoring.md`](../../docs/agent-monitoring.md) — the event contract and design.

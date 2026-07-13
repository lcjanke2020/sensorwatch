# Roadmap

sensorwatch began as a small Windows PSU logger and is growing, deliberately,
into a **hardware observability toolkit with a first-class agent layer**. This
document is the extended version of the README's [Roadmap](README.md#roadmap)
section: where the project stands, where it is going, in what order, and which
questions are still open.

One constraint shapes the sequencing: **the project must be usable at every
intermediate stage.** Each milestone ships something you can run on its own —
nothing below depends on a later phase to be useful.

*Last updated: 2026-07-13.*

## Where the project is today

| Component | Status |
|-----------|--------|
| Python monitor + JSONL logger (`python -m sensorwatch`) | Shipped — [PyPI](https://pypi.org/project/sensorwatch/), prebuilt Windows wheels |
| Native C core (bounds-checked parser, opaque-handle ABI) | Shipped — [`include/sensorwatch/sensorwatch.h`](include/sensorwatch/sensorwatch.h), spec in [`docs/C_ABI.md`](docs/C_ABI.md) |
| Python binding (cffi, API mode) | Shipped — `sensorwatch.native` |
| C++ binding (header-only, C++17 RAII) | Shipped — [`include/sensorwatch/sensorwatch.hpp`](include/sensorwatch/sensorwatch.hpp) |
| Rust bindings (`-sys` crate + safe wrapper) | Shipped — [crates.io](https://crates.io/crates/sensorwatch), OIDC trusted publishing |
| Rust CLI — `snapshot` + `log` + `watch` + `report` subcommands | Shipped — [`rust/sensorwatch-cli`](rust/sensorwatch-cli/), repo-only binary `sensorwatch` |
| CMake `install()` / `find_package(sensorwatch CONFIG)` export | Shipped |
| Agent skill (portable Agent Skills bundle) | Shipped — [`skills/sensorwatch/`](skills/sensorwatch/) |
| Agent monitor skill (wake-up protocol + durable state dir) | Shipped — [`skills/sensorwatch-monitor/`](skills/sensorwatch-monitor/) |
| CI: Ubuntu + Windows, sanitizers, ABI/vendor drift gates, MSRV check | Shipped — [`ci.yml`](.github/workflows/ci.yml) |

The data source today is HWiNFO64's shared-memory feed on Windows. Everything
*builds and unit-tests* cross-platform (the parser is exercised against
synthetic buffers), but a live read requires Windows + HWiNFO64 — see
[Testing / CI scope](README.md#testing--ci-scope).

## Design principles

1. **Usable at every stage.** Each phase ends with a tool that stands alone.
2. **Deterministic before agentic.** Anything that can be a rule *is* a rule —
   thresholds, hysteresis, debounce, staleness live in config-driven native
   code. LLM judgment is reserved for interpretation, never for noticing.
3. **Bounded interfaces for LLM consumers.** Agent-facing output has hard size
   guarantees; an agent should never need to read raw history.
4. **Untrusted input.** The shared-memory region is parsed with full bounds
   validation and tested against synthetic adversarial buffers
   ([`SECURITY.md`](SECURITY.md)).
5. **Read-only.** sensorwatch observes hardware; it never controls it.
6. **Dependency-light.** Additions to the runtime dependency set need
   justification ([CONTRIBUTING](CONTRIBUTING.md#guidelines)).
7. **Docs travel with code.** Every change lands with its documentation — see
   [`PRE-MERGE-CHECKLIST.md`](PRE-MERGE-CHECKLIST.md).

## Phase 1 — a Rust CLI on the high-level API

The CLI moves from Python to Rust, built on the safe `sensorwatch` crate
(`Session` / `Snapshot` / `Reading`) rather than reimplementing the wire
format. The Python package stays in-tree as a **frozen reference
implementation** — it gathered the original PSU dataset, and its pure-Python
reader documents the shared-memory format end to end.

Each step ships independently:

1. **`snapshot`** — *shipped* ([`rust/sensorwatch-cli`](rust/sensorwatch-cli/))
   — one-shot live readings as JSON, with type and substring filters. *Usable
   outcome:* instant health checks and shell scripting, replacing the skill's
   bundled Python helper (kept as a no-toolchain fallback for now).
2. **`log`** — *shipped* ([`rust/sensorwatch-cli`](rust/sensorwatch-cli/))
   — the logger loop, byte-compatible with the Python logger's JSONL output
   (a Python-generated golden fixture is byte-compared in the tests) so
   existing analyses work unchanged over directories that mix old and new
   files. *Usable outcome:* a single static binary replaces the Python
   process for long-running capture.
3. **Declarative alert rules + deterministic engine** — *shipped*
   ([`rust/sensorwatch-cli`](rust/sensorwatch-cli/)) — a `[[rules]]` section
   in `config.toml`: thresholds with hysteresis and debounce, rate-of-change,
   stale-reading, missing-sensor, and source-unavailable detection. Evaluation
   is sample-count based and consumes timestamps from the data stream, so it is
   fully deterministic under **replay** of recorded logs — which also makes the
   whole engine developable and testable on non-Windows machines. *Usable
   outcome:* the rule engine is exercised end-to-end in CI via replay before
   any live wiring exists (the `watch` command below is where rules become
   user-visible).
4. **`watch`** — *shipped* ([`rust/sensorwatch-cli`](rust/sensorwatch-cli/))
   — the rules engine as a command. A blocking mode waits until a rule fires,
   emits one structured JSON event (with a monotonic, persisted sequence
   number), and exits with a distinct code; a follow mode streams fired and
   cleared events to daily files. The CLI-wide exit-code contract and the
   event schema land here, written up in
   [`docs/agent-monitoring.md`](docs/agent-monitoring.md). *Usable outcome:*
   deterministic hardware alerting with no agent involved — a shell script
   dispatching on the exit code is a complete alerting system.
5. **`report`** — *shipped* ([`rust/sensorwatch-cli`](rust/sensorwatch-cli/))
   — a bounded digest over logged history: per-reading window aggregates
   (recomputed over the window, not HWiNFO's lifetime numbers), rule violations
   re-derived by replaying the window through the same deterministic engine,
   sampling gaps, and a meta block that doubles as a one-call liveness check,
   all under a hard `--max-bytes` output cap with a `--top` selector and
   substring/type display filters. Pure file reading, so it runs on any
   platform. *Usable outcome:* one command answers "what happened in the last
   24 h" for humans and LLMs alike, on a fixed context budget.

Phase 1 closes with a documentation pass making the Rust CLI canonical and the
Python CLI explicitly legacy/reference.

## Phase 2 — the monitoring agent

With `watch` and `report` in place, an AI agent can *monitor* hardware over
days and weeks — woken by deterministic events plus a low-frequency heartbeat,
instead of burning cycles polling. The
[`sensorwatch-monitor`](skills/sensorwatch-monitor/SKILL.md) skill **ships the
operating protocol** (LEO-338); the real notification transport shipped in
LEO-339 (see below), and the unattended runtime around it is what remains in
progress:

- **Event-driven wake-ups — shipped.** The agent arms the blocking `watch`; the
  process exiting *is* the wake-up. A rule event means "triage this"; a timeout
  means "heartbeat — verify all quiet, re-arm."
- **Bounded context by construction — shipped.** Each wake-up consumes a ~1 KB
  event plus at most two size-capped `report` digests (one per heartbeat), and
  the hard context-budget rules forbid reading raw logs — the skill states them
  verbatim.
- **Durable state on disk, not in the context window — shipped.** The agent's
  memory is a small machine-local state directory: an acknowledgment cursor keyed
  to event sequence numbers (at-least-once handling, crash-safe), open-incident
  files with snooze semantics (a still-firing rule is not re-investigated every
  wake), a curated baseline of what "normal" looks like, and an escalation ledger
  with cooldowns so a fresh session can never re-alert. Any new session
  reconstructs the monitor from a few-kilobyte state summary. Stdlib-only helper
  scripts do every mechanical write.
- **Deterministic escalation ladder — shipped.** Journal → incident file →
  notification → issue-draft → critical-combination tier, driven by rule
  severity and persistence, with per-rule cooldowns and a global daily cap
  (batched digest beyond it). The issue tier currently delivers the same routed
  notification as tier 2 (a durable `outbox` draft exists only when explicitly
  forced); wiring it to a distinct action (an issue-draft file or a webhook) is a
  Phase 2 / Phase C follow-up from the pilot. Delivery goes through pluggable
  channels routed
  per severity from a machine-local `notify.toml`; LEO-339 ships real transports
  — **ntfy** (the zero-account default via hosted `ntfy.sh`), **Pushover**, and
  generic **SMTP** — with the `outbox`/`stderr` stubs kept as fallbacks.
- **Staged runtimes — in progress.** The interactive agent session runs the
  skill today (cheap to develop and tune against real hardware). The Windows
  wiring landed in LEO-340: an unattended supervisor — a small deterministic loop
  that re-runs `watch` and dispatches each exit to a fresh headless agent
  invocation, zero context growth, survives reboots — plus a dead-man's switch (a
  trivial scheduled task checking heartbeat-file age) that watches the watcher
  through an independent alert path. The **Phase 1 pilot (LEO-341) is
  complete**: a week-long interactive session (2026-07-05 → 07-13) monitored a
  real Windows machine (an AMD Ryzen 9 9950X, an MSI MEG Ai1600T PSU, and GPU
  temperatures), with deterministic rules on the PSU +12V rail, CPU and GPU
  temperatures, and sensor-feed liveness, and the dead-man's switch armed. Eight
  days of clean coverage held per-wake cost roughly constant; two attended fault
  drills — a synthetic escalation-ladder run and a real thermal event under load
  — both delivered a multi-channel alert. The run is written up in
  [`docs/pilot-field-report.md`](docs/pilot-field-report.md). It surfaced three
  defects, now queued as **Phase 2 / Phase C follow-ups**: (1) the logger drops
  samples under full CPU load (harden the sampler under contention; escalate on
  gap density so a starved logger becomes an alert); (2) arming `watch` one-shot
  per wake emits no cross-restart `cleared` event, so incidents don't auto-close
  (run the persistent `watch --follow`, and have the agent reconcile open
  incidents against a fresh `report` on each heartbeat); (3) the tier-3 issue
  rung has no distinct wired action — it collapses into the tier-2 notification
  plus a best-effort `outbox` draft (emit a real issue-draft artifact and/or a
  config-driven webhook). The headless supervisor graduation, and the
  generalizable worked **examples** + **`sensorwatch-monitor` skill
  refinements** (LEO-411), follow from here.

The protocol is deliberately harness-agnostic: it needs only "run a blocking
process; act on its exit," which any current agent runtime provides.

The architecture — *deterministic watcher → classified event → bounded digest
→ agent wake → durable state with an ack cursor* — is written up as a
standalone design document
([`docs/agent-monitoring.md`](docs/agent-monitoring.md), shipped with `watch`),
because it generalizes well beyond hardware: any "agent that keeps an eye on
something" (CI, PRs, queues) has the same shape.

## Phase 3 — the broader observability toolkit

Longer-horizon items, roughly ordered; several originate from the project's
earliest design discussions:

- **Source-adapter architecture.** Pluggable sensor sources behind the same C
  ABI — HWiNFO today; a UPS adapter (e.g. CyberPower via USB HID) as the
  proving ground, then AIDA64 and IPMI. Adapters emit the same snapshot shape,
  so every binding and tool above the ABI works unchanged.
- **Stable sensor identity.** A durable `source_id` + `sensor_path` (+
  optional user alias) per reading, with a schema version — so logs survive
  hardware renames and multi-source setups.
- **Per-reading quality flags.** `valid` / `stale` / `missing` as first-class
  data instead of inference, feeding the rule engine directly.
- **Optional localhost REST service.** Read-only live queries bound strictly
  to `127.0.0.1`, for dashboards and non-Python consumers. This is also the
  designated route should remote access ever be wanted — see the threat model
  in [`SECURITY.md`](SECURITY.md).
- **Headless HWiNFO startup.** Investigate driving HWiNFO64's shared-memory
  feed without an interactive sensors window, for service-style deployments.
- **Publishing the CLI crate.** The CLI starts repo-only (`publish = false`);
  publishing to crates.io for `cargo install` is a later, explicit step.

## Open questions

Design decisions we have deliberately left open, in case you'd like to weigh in
(issues and PRs welcome):

- **Binary naming.** The Rust binary wants the `sensorwatch` name, which the
  Python console script currently owns. The collision now exists in-tree — the
  repo-built Rust binary already takes the name, though nothing installs it
  onto `PATH` yet. Likely resolution: drop the Python entry point (the module
  stays runnable via `python -m sensorwatch`); the alternative is a distinct
  binary name. Decided with the Phase 1 docs handoff (LEO-342).
- **Notification transports.** *Decided (shipped in LEO-339).* The notify adapter
  routes channels per severity from a machine-local `notify.toml`. Built-in:
  **ntfy** (the zero-account default via hosted `ntfy.sh`; a long random topic is
  the shared secret), **Pushover** (the acknowledge-required upgrade path —
  emergency priority + a receipt), and generic **SMTP** (bring-your-own
  credentials). The dead-man's switch alerts through a **separate** ntfy watchdog
  topic so it shares no failure mode with the path it watches (wired in LEO-340).
  `outbox`/`stderr` remain as fallbacks.
- **Digest truncation semantics.** *Decided (shipped in `report`).* `--top`
  first caps reading rows to the largest relative movers plus anything in
  violation; if the JSON still overflows `--max-bytes`, detail is dropped
  worst-first — lowest-ranked reading row, then smallest gap (oldest on a tie),
  then oldest violation. Only the meta block is guaranteed to survive; a dropped
  early violation shows up as `truncated.violations_shown < violations_total`.
  Full order and rationale:
  [rust/sensorwatch-cli/README.md](rust/sensorwatch-cli/README.md#report).
- **Adaptive heartbeat.** Whether the watch timeout should vary by schedule
  (e.g. longer overnight) — and if so, deterministically in config rather than
  by agent judgment.
- **Rate rules and wall-clock time.** Rate-of-change rules are defined over
  sample windows for determinism; whether a wall-clock variant is needed for
  sparse/irregular sources (UPS events) is open until an adapter exists.
- **REST service scope.** Read-only snapshot + report endpoints seem
  sufficient; whether the event stream should also be exposed (server-sent
  events?) is undecided.

## Non-goals

- **Hardware control.** No fan curves, no clock changes, no power actions —
  sensorwatch is read-only by design, and the agent layer inherits that
  guarantee. Escalation to a human *is* the action.
- **Kernel drivers or sensor acquisition.** HWiNFO (and future adapters) own
  the hardware access; sensorwatch reads what they publish.
- **Network exposure by default.** No listeners today; the future REST service
  is opt-in and localhost-only.
- **A bespoke agent protocol server.** Agents consume sensorwatch through the
  shipped skills over the CLI and language APIs — not through a dedicated
  MCP-style server.

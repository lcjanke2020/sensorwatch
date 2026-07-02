# Roadmap

sensorwatch began as a small Windows PSU logger and is growing, deliberately,
into a **hardware observability toolkit with a first-class agent layer**. This
document is the extended version of the README's [Roadmap](README.md#roadmap)
section: where the project stands, where it is going, in what order, and which
questions are still open.

One constraint shapes the sequencing: **the project must be usable at every
intermediate stage.** Each milestone ships something you can run on its own —
nothing below depends on a later phase to be useful.

*Last updated: 2026-07-02.*

## Where the project is today

| Component | Status |
|-----------|--------|
| Python monitor + JSONL logger (`python -m sensorwatch`) | Shipped — [PyPI](https://pypi.org/project/sensorwatch/), prebuilt Windows wheels |
| Native C core (bounds-checked parser, opaque-handle ABI) | Shipped — [`include/sensorwatch/sensorwatch.h`](include/sensorwatch/sensorwatch.h), spec in [`docs/C_ABI.md`](docs/C_ABI.md) |
| Python binding (cffi, API mode) | Shipped — `sensorwatch.native` |
| C++ binding (header-only, C++17 RAII) | Shipped — [`include/sensorwatch/sensorwatch.hpp`](include/sensorwatch/sensorwatch.hpp) |
| Rust bindings (`-sys` crate + safe wrapper) | Shipped — [crates.io](https://crates.io/crates/sensorwatch), OIDC trusted publishing |
| Rust CLI — `snapshot` subcommand | Shipped — [`rust/sensorwatch-cli`](rust/sensorwatch-cli/), repo-only binary `sensorwatch` |
| CMake `install()` / `find_package(sensorwatch CONFIG)` export | Shipped |
| Agent skill (portable Agent Skills bundle) | Shipped — [`skills/sensorwatch/`](skills/sensorwatch/) |
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
2. **`log`** — the logger loop, byte-compatible with the Python logger's JSONL
   output so existing analyses work unchanged over directories that mix
   old and new files. *Usable outcome:* a single static binary replaces the
   Python process for long-running capture.
3. **Declarative alert rules + deterministic engine** — a `[[rules]]` section
   in `config.toml`: thresholds with hysteresis and debounce, rate-of-change,
   stale-reading, missing-sensor, and source-unavailable detection. Evaluation
   is sample-count based and consumes timestamps from the data stream, so it is
   fully deterministic under **replay** of recorded logs — which also makes the
   whole engine developable and testable on non-Windows machines. *Usable
   outcome:* the rule engine is exercised end-to-end in CI via replay before
   any live wiring exists.
4. **`watch`** — the rules engine as a command. A blocking mode waits until a
   rule fires, emits one structured JSON event (with a monotonic, persisted
   sequence number), and exits with a distinct code; a follow mode streams
   events to daily files. *Usable outcome:* deterministic hardware alerting
   with no agent involved — a shell script dispatching on the exit code is a
   complete alerting system.
5. **`report`** — a bounded digest over logged history: per-reading window
   aggregates, rule violations, sampling gaps, and a metadata block, under a
   hard output-size cap. *Usable outcome:* one command answers "what happened
   in the last 24 h" for humans and LLMs alike.

Phase 1 closes with a documentation pass making the Rust CLI canonical and the
Python CLI explicitly legacy/reference.

## Phase 2 — the monitoring agent

With `watch` and `report` in place, an AI agent can *monitor* hardware over
days and weeks — woken by deterministic events plus a low-frequency heartbeat,
instead of burning cycles polling. A second agent skill,
`sensorwatch-monitor`, will encode the operating protocol:

- **Event-driven wake-ups.** The agent arms the blocking `watch`; the process
  exiting *is* the wake-up. A rule event means "triage this"; a timeout means
  "heartbeat — verify all quiet, re-arm."
- **Bounded context by construction.** Each wake-up consumes a ~1 KB event
  plus one size-capped `report` digest. The protocol forbids reading raw logs.
- **Durable state on disk, not in the context window.** The agent's memory is
  a small state directory: an acknowledgment cursor keyed to event sequence
  numbers (at-least-once handling, crash-safe), open-incident files with
  snooze semantics (a still-firing rule is not re-investigated every wake),
  a curated baseline of what "normal" looks like, and an escalation ledger
  with cooldowns so a fresh session can never re-alert. Any new session
  reconstructs the monitor from a few kilobytes of state summary.
- **Deterministic escalation ladder.** Journal → incident file → notification
  → issue tracker, driven by rule severity and persistence, with per-rule
  cooldowns and a global daily cap. Notification delivery goes through a
  pluggable adapter (email first).
- **Staged runtimes.** First an interactive agent session (cheap to develop
  and tune against real hardware), then an unattended supervisor: a small
  deterministic loop that re-runs `watch` and dispatches each exit to a fresh
  headless agent invocation — zero context growth, survives reboots. A
  dead-man's switch (a trivial scheduled task checking heartbeat-file age)
  watches the watcher through an independent alert path.

The protocol is deliberately harness-agnostic: it needs only "run a blocking
process; act on its exit," which any current agent runtime provides.

The architecture — *deterministic watcher → classified event → bounded digest
→ agent wake → durable state with an ack cursor* — is written up as a
standalone design document (`docs/agent-monitoring.md`, landing with `watch`),
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
- **Notification transports.** The notify adapter ships with email first;
  which push channels (ntfy, Pushover, others) earn built-in adapters, and
  whether a "critical" tier warrants acknowledge-required semantics.
- **Digest truncation semantics.** `report` guarantees a size cap; the exact
  priority order of what gets dropped first as data grows deserves scrutiny
  once real multi-week logs exist.
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

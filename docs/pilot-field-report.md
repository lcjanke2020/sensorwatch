# Pilot field report: an agent ran the monitor for a week

This is a write-up of a live pilot in which an AI agent, driving the
[`sensorwatch-monitor`](../skills/sensorwatch-monitor/SKILL.md) skill, ran as the
always-on hardware monitor for a single workstation for about eight days —
2026-07-05 through 2026-07-13. The goal was not a demo but a soak test: does the
"deterministic before agentic" architecture actually hold up over hundreds of
wake-ups, and does the escalation path actually deliver an alert when something
is wrong?

It held up, and it delivered. It also surfaced three real defects — which is the
more useful result, and the reason this report exists. Everything below is drawn
from the pilot's own heartbeat log; host, network, and account specifics have
been scrubbed. The workstation is referred to as "the pilot host."

## Setup

The pilot host is a high-end Windows desktop feeding HWiNFO64's shared-memory
sensor stream. The configuration under test:

- **Logger:** the JSONL sampler, writing daily-rotated `sensors_*.jsonl`.
- **Watcher:** `watch` armed as a one-shot per wake-up (arm → block until a rule
  fires or the timeout elapses → dispatch on the exit code → re-arm), against a
  config of **10 rules** — PSU +12V sag, CPU and per-GPU temperature warn/critical
  bands with debounce and hysteresis, GPU memory-temp, and a sensor-staleness
  rule.
- **Agent:** on each wake it read the event (or the timeout heartbeat), pulled at
  most one bounded `report` digest, recorded state, and re-armed — under a hard
  ~4 KB context budget per wake.
- **Escalation ladder:** journal → incident file → notification → issue-draft,
  with a 6-hour per-rule cooldown and a daily notification cap.
- **Delivery:** three notification channels wired for critical events — an ntfy
  push topic, Pushover, and SMTP.

**A note on runtime.** The pilot ran as a *single long-running interactive agent
session*, not the headless per-wake supervisor that Phase 2 graduates to. What
kept each wake cheap was the skill's discipline, not process isolation: every
wake reads only the event (or heartbeat) plus at most one bounded `report`, all
durable memory lives in the on-disk state directory, and the session is
periodically compacted. That is exactly the discipline that makes the eventual
move to independent headless invocations safe — the pilot validated it under
real load before the supervisor is wired.

## The quiet baseline

For eight days the interesting thing was that nothing was interesting. The agent
woke roughly every 30 minutes; each quiet heartbeat cost about **2 KB of
context** — comfortably inside the 4 KB budget — and reported the same shape:

> all quiet — logger lag ~2s, 0 gaps, 0 violations / 10 rules

Two things worth calling out from the quiet stretch:

- **Per-wake cost stayed flat.** Each wake read only the event plus at most one
  bounded `report`, with all durable memory on disk — so a heartbeat late in the
  run cost about the same (~2 KB) as one early on, held there by periodic
  compaction rather than by unbounded context growth. This is the whole point of
  making the exit code the signal and the state directory the memory: it is what
  lets the same loop graduate from this interactive session to independent
  headless invocations without the context ballooning.
- **Maintenance ran unattended.** A nightly maintenance wake pruned old data,
  checked state-dir size caps, and re-verified the baseline — logged as
  `midnight maintenance — 0 prune, caps OK, baseline unchanged`.

A well-cooled machine at idle-to-moderate load simply does not trip
well-chosen thresholds. Which is exactly why the pilot could not end there: a
monitor that has never fired is a monitor you have no reason to trust. So the
last day was spent deliberately breaking things.

## Fault drill 1 — synthetic escalation-ladder validation

The first drill was deterministic and surgical: with the machine behaving
normally, tighten a rule's threshold below the current reading so it *must*
fire, and watch the ladder execute end to end. This isolates the escalation and
delivery machinery from the physics of actually heating a component.

What the ladder did, in order:

- **Tier 2 (notify)** fired and **delivered on all three channels** — ntfy,
  Pushover, and SMTP all received the critical alert.
- **Cooldown suppression worked:** the same rule firing again inside the 6-hour
  window was suppressed rather than re-paged — the anti-noise guarantee the
  ladder advertises.
- **Tier 4 (combination)** — two distinct criticals open at once — escalated and
  notified as designed.
- **Tier 3 (issue)** *recorded* an issue-draft artifact to the outbox. This is
  where the drill found defect (c), below.

**A bonus finding, for free.** Setting up the drill, I once lowered a threshold
below its own hysteresis `clear` value. The watcher **rejected the config and
exited `2`** — the usage-error code — rather than starting up in an
inconsistent state and hot-looping. An invalid rule set halts loudly instead of
failing open. That is the correct behavior, and I would not have thought to write
that specific test; the drill wrote it for me.

## Fault drill 2 — a real thermal event under load

The second drill used real physics. I ran [OCCT](https://www.ocbase.com/) to
drive the CPU to a sustained full-load thermal state, with the CPU
critical-temperature rule tightened to a threshold the real workload would
actually cross.

It crossed. The `cpu-temp-crit` rule fired on a **real** sustained CPU
temperature reading (~74 °C under the drill's tightened threshold), and the
critical alert was **delivered over all three channels**. A GPU warn-band rule
also tripped to tier 1 as the load spread. This is the end-to-end path that
matters: real hardware state → deterministic rule → agent triage → multi-channel
page, with no human in the loop until the phone buzzed.

Then I stopped OCCT, the temperature fell — and the second defect showed itself.

## Three defects the pilot found

Adversarial testing earns its keep by finding things, and the drills found three
things worth fixing. None are cosmetic; each teaches something about the design.

### (a) The logger drops samples under full CPU load

Under sustained 100% CPU, the sampling loop's cadence slipped and the recorded
log showed gaps — the same `gaps` flag that `report` already surfaces. The very
condition most likely to *cause* an incident (a pegged CPU) is the condition
that degrades the evidence you would use to diagnose it.

*What it teaches:* a monitor's data path has to be most robust exactly when the
system is least healthy. The fix direction is twofold — harden the sampler's
scheduling under contention, and, because some degradation is unavoidable, make
the monitor **escalate on gap density** so a starved logger becomes an alert
rather than a silent blind spot. *Status:* the escalation half shipped — the
heartbeat's reconcile step (`reconcile_incidents.py`) computes a `logger_health`
verdict from the digest's `gaps` and the skill escalates on `degraded`; the
sampler hardening itself is still queued.

### (b) Arm-per-wake emits no cross-restart "cleared" event

The watcher tracks fired/cleared transitions *within a single process*. The
pilot ran `watch` as a fresh one-shot per wake-up, so when the CPU cooled and
the rule recovered, the recovery happened in a *different* `watch` process than
the one that saw the fire — and no `cleared` event was emitted. The incident the
agent had opened did not auto-close.

*What it teaches:* the wake-up transport (one-shot `watch`, exit code as signal)
and the incident lifecycle (open on fire, close on clear) make different
assumptions about process identity, and the seam between them leaks. There are
two clean fixes, and the project wants both: run the persistent
`watch --follow`, which *does* emit native cleared events across the whole run
(the [replay demo](../examples/demo/) shows the full fire → clear lifecycle in a
single process); **and** teach the agent to reconcile open incidents against a
fresh `report` on each heartbeat and close the ones no longer in violation, so
the one-shot topology is self-healing too. *Status:* the reconcile path shipped
(`reconcile_incidents.py` — freshness-gated, closes only on a re-derived
`cleared` transition, conservative on absence of evidence).

### (c) Tier 3 is a label with no distinct action

The escalation ladder defines tier 3 as "file an issue," but with no issue
tracker wired in, tier 3 collapsed into the tier-2 notification and a
best-effort text draft dropped in the outbox. The ladder *said* it had four
distinct actions; it really had three plus a placeholder.

*What it teaches:* an escalation level is only real if it produces a distinct,
durable artifact. The fix keeps the design CLI/skill-native rather than reaching
for a heavyweight integration: at tier 3, emit a real issue-draft artifact (an
extension of the outbox pattern), so the level is a deliverable you can point
at, not a label. *Status:* shipped — `notify.py --issue-draft` writes a
tracker-ready draft to `outbox/issues/` in the same invocation as the
notification, recording the cooldown exactly once.

## What the pilot validates

- **The context-budget model works in practice, not just in theory.** Hundreds
  of wake-ups at a roughly constant ~2 KB per wake, with durable memory on disk
  and periodic compaction holding the line. That bounded-per-wake discipline is
  what will let the loop move from this interactive session to unattended
  headless invocations without the context growing.
- **Determinism paid off in testing.** Because rule evaluation is sample-count
  based and replayable, the synthetic drill could force exact firing conditions,
  and the config-error halt was catchable. The parts that need to be trustworthy
  are the parts that don't involve model judgment.
- **The delivery path is real.** Two independent drills — one synthetic, one a
  genuine thermal event — both reached a phone through three channels.
- **The defects are at the seams, and they are honest ones.** Every one of the
  three lives where two subsystems meet (sampler vs. load, one-shot transport vs.
  incident lifecycle, ladder vs. an unwired tracker). That is where real systems
  break, and finding them is the pilot's most valuable output. Defects (b) and
  (c) are fixed, and (a)'s detection half shipped (statuses above and on the
  [roadmap](../ROADMAP.md)); (a)'s sampler hardening remains queued.

The monitor spent eight days being boring and one day being useful. Both were the
point.

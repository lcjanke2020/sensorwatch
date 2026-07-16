# Monitor pilot test plan

A monitor you have never seen fire is a monitor you cannot trust. Before you rely
on this setup, run it as a **pilot** — a deliberate trial — and work through the
checks below. Each proves one link in the chain: *deterministic watcher → event →
triage → escalation → notification*, plus the failure paths (a dead watcher, a
crash mid-triage). They use only the CLI and the
[`sensorwatch-monitor`](../../skills/sensorwatch-monitor/SKILL.md) helper scripts,
and most run **without waiting for real hardware to misbehave**.

Every command below runs **from this directory** (`examples/monitor-setup/`), so
`config.toml` and fixture paths resolve as written and the helper scripts are
reached as `../../skills/sensorwatch-monitor/scripts/*.py`. `sensorwatch` means
the **Rust CLI** — invoke it by path, `../../rust/target/release/sensorwatch`
(`.exe` on Windows); the repo-built binary is not on `PATH` (and a stale Python
install of 0.2.0 or earlier may still own the bare name). `<state>` is
your monitor state directory — initialize it first with `init_state.py` (see the
[worked-example README](README.md#3-run-the-monitor)). Commands use **POSIX
shell** syntax (`\` line-continuation); run them in Git Bash or WSL, or adapt
them to PowerShell.

## 1. The config is valid

`watch` validates `[[rules]]` strictly and exits `2` on any bad rule.

```sh
sensorwatch watch --config config.toml --timeout 1
```

- [ ] Exit code is **0** (a clean 1 s heartbeat), not `2`. A `2` prints the exact
      rule problem — fix it and repeat.

## 2. Prove a rule actually fires

This is the check people skip and regret. Do it deterministically with
`--replay`: feed a synthetic log that trips a rule, no hardware required.

Create `fixture-12v-sag.jsonl` — three samples below the `psu-12v-sag` threshold
(three because `config.toml` sets `for_samples = 3` for that rule; if you tune
either, keep the two in sync):

```jsonl
{"timestamp":"2026-01-01T00:00:00.000000-00:00","sensors":[{"sensor":"PSU","reading":"+12V","type":"Voltage","value":11.4,"unit":"V"}]}
{"timestamp":"2026-01-01T00:00:10.000000-00:00","sensors":[{"sensor":"PSU","reading":"+12V","type":"Voltage","value":11.3,"unit":"V"}]}
{"timestamp":"2026-01-01T00:00:20.000000-00:00","sensors":[{"sensor":"PSU","reading":"+12V","type":"Voltage","value":11.4,"unit":"V"}]}
```

```sh
sensorwatch watch --config config.toml --replay fixture-12v-sag.jsonl \
    --rule psu-12v-sag --spool-dir <state>/spool/pending
```

- [ ] Exit code is **10**, one event JSON is printed, and a
      `<seq>-psu-12v-sag.json` file appears in `<state>/spool/pending/`.

> **On live hardware**, the equivalent is to *tighten* a rule until it trips: set
> a threshold just past your current idle reading (e.g. a CPU-temp warning a few
> degrees above idle), watch it fire once, then restore the real value. If a rule
> never fires in testing, it will never fire in production either.

## 3. A notification actually reaches you

With `notify.toml` in place (ntfy path), send a test notice and confirm it lands
on your phone/endpoint:

```sh
python ../../skills/sensorwatch-monitor/scripts/notify.py --state-dir <state> \
    --adapter ntfy --rule test --severity warning --tier 2 --summary "pilot test"
```

- [ ] The notice arrives on the subscribed ntfy topic (repeat with `--adapter
      pushover` / `smtp` for any other channels you routed).

## 4. A dead watcher is noticed (watcher-health)

If you run an out-of-band **dead-man's switch / supervisor** (a scheduled task
that alerts when the heartbeat file goes stale — see the skill's *Dead-man's
switch* section), prove it works: stop the monitor so `heartbeat.json` stops
updating, and wait.

- [ ] The independent staleness alert fires within its interval, through a channel
      that shares no failure mode with the watcher it guards. Acknowledge, restart
      the monitor, and confirm it goes quiet.

## 5. A crash mid-triage is recovered (redelivery + idempotency)

The spool gives **at-least-once** delivery; the cursor makes reprocessing
**idempotent**. Prove both by simulating a crash between opening an incident and
acknowledging it. (Reuse the spool file from step 2, or replay again.)

1. Open an incident on the pending event, then **stop** — do not ack:

   ```sh
   python ../../skills/sensorwatch-monitor/scripts/open_incident.py --state-dir <state> \
       --event-file <state>/spool/pending/<seq>-psu-12v-sag.json --classification incident
   ```

2. "Restart" and bootstrap — the unacked event is still in the spool:

   ```sh
   python ../../skills/sensorwatch-monitor/scripts/state_summary.py --state-dir <state>
   ```

   - [ ] Shows `pending=1` and one open incident. **`watch` never replays the
         spool — the surviving file, drained on bootstrap, is the redelivery.**

3. Reprocess and confirm idempotency:

   ```sh
   # Re-opening the same event dedups — no duplicate incident:
   python ../../skills/sensorwatch-monitor/scripts/open_incident.py --state-dir <state> \
       --event-file <state>/spool/pending/<seq>-psu-12v-sag.json --classification incident
   # -> "action":"update-deduped"

   # Ack drains it (the event file must still be in spool/pending/):
   python ../../skills/sensorwatch-monitor/scripts/ack_event.py --state-dir <state> \
       --event-file <state>/spool/pending/<seq>-psu-12v-sag.json
   ```

   - [ ] The re-open returns `"action":"update-deduped"` (exactly one incident,
         no double-count), and the ack moves the file to `spool/acked/` and
         advances the cursor — `pending` drains to 0.

## 6. Curate a baseline

After ~24 h with the **layer-1 logger running** (the `sensorwatch log` from the
worked-example README §3 — one-shot `watch` alone writes no `sensors_*.jsonl`, so
`report` would have no history), write `baseline.md` (what "normal" looks like)
from a digest, not by hand:

```sh
sensorwatch report --config config.toml --last 24h
```

- [ ] `baseline.md` records your idle **and** under-load ranges for the readings
      your rules watch. Triage compares events against this.

## 7. Steady state

- [ ] Over a multi-day run, each wake costs about the same (a bounded event + at
      most two `report` digests) — context does not grow with history.
- [ ] The once-a-day maintenance pass runs (prunes the acked spool, re-curates the
      baseline toward its cap).

---

## Ready to trust it when

- [ ] Config valid (`watch` accepts it).
- [ ] A rule **proven to fire** (replay or a tightened live threshold).
- [ ] A notification **proven to arrive** on every routed channel.
- [ ] A dead watcher **proven to alert** (if you run a dead-man's switch).
- [ ] A crash mid-triage **recovers idempotently** (surviving spool file drained on
      bootstrap; no duplicate incident).
- [ ] A curated `baseline.md`, and bounded per-wake cost over several days.

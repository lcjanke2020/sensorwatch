# Replay demo — watch a rule fire with no hardware

sensorwatch reads real hardware on Windows, but its **alerting engine is
platform-independent**: `watch --replay` runs the exact same rule evaluation
over a *recorded* log instead of a live sensor feed. That means you can see the
whole thing work — a critical alert firing, then clearing — on Linux, macOS, or
Windows, in one command, with no HWiNFO and no hardware.

![A terminal running the replay demo: one watch --replay command fires a critical psu-12v-sag event and exits 10; adding --follow records the full fired-then-cleared lifecycle](demo.gif)

This directory has everything you need:

| File | What it is |
|------|------------|
| [`sensors_demo.jsonl`](sensors_demo.jsonl) | A 4-sample recording of a PSU's +12V rail: nominal, then a two-sample sag, then recovery. |
| [`demo.toml`](demo.toml) | One `threshold` rule that fires CRITICAL when the rail sags below 11.6 V (commented). |

## Run it

Build the CLI once (from the repo root):

```sh
cargo build --release --manifest-path rust/Cargo.toml -p sensorwatch-cli
```

Then, from this directory:

```sh
cd examples/demo
../../rust/target/release/sensorwatch watch --config demo.toml --replay sensors_demo.jsonl
```

On Windows PowerShell, the built binary is `sensorwatch.exe` and paths use
backslashes:

```powershell
cd examples\demo
..\..\rust\target\release\sensorwatch.exe watch --config demo.toml --replay sensors_demo.jsonl
```

> No build step? Swap the binary for
> `cargo run --release --manifest-path rust/Cargo.toml -p sensorwatch-cli --`
> and run it from the repo root, pointing `--config` / `--replay` at
> `examples/demo/…`.

You'll see one JSON event and the process will exit **10** ("a rule fired"):

```json
{"schema_version":1,"seq":1,"id":"psu-12v-sag-1","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.4,"unit":"V","threshold":11.6,"samples_in_violation":2}
```

That single stdout event + exit code **is** the wake-up primitive the agent
monitor is built on: a supervisor arms `watch`, and the exit code tells it
whether to wake an agent (`10`), record a heartbeat (`0`), or back off
(`1`/`2`). See [`docs/agent-monitoring.md`](../../docs/agent-monitoring.md).

### See the full fire → clear lifecycle

One-shot mode exits the instant the first rule fires. Add `--follow` to run the
whole recording and record **both** the fire and the recovery to a daily event
file:

```sh
rm -rf logs   # reset the monotonic seq counter so this run starts at seq 1
../../rust/target/release/sensorwatch watch --config demo.toml --replay sensors_demo.jsonl --follow
cat logs/events_*.jsonl
```

```json
{"schema_version":1,"seq":1,...,"state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","value":11.4,...}
{"schema_version":1,...,"state":"cleared","timestamp":"2026-02-18T08:00:30.000000-05:00","value":11.9,...}
```

(`logs/` is git-ignored, so the demo leaves your tree clean.)

> **The GIF is generated from real output.**
> [`make_demo_gif.py`](make_demo_gif.py) runs these exact commands, captures
> their actual stdout and exit codes, and renders `demo.gif` from them — so the
> recording can't drift from what the CLI does. Regenerate it with
> `python make_demo_gif.py` (needs Pillow).

## What the demo is actually testing

The recording is only four samples, but it exercises the two guards that keep
alerting from being noisy — the reason the engine is a *deterministic rule
engine* and not an LLM eyeballing numbers:

1. **Debounce (`for_samples = 2`).** The rail dips to 11.5 V at `08:00:10` —
   already under the 11.6 V threshold — but the rule does **not** fire on that
   sample. It fires at `08:00:20`, the *second* consecutive sample in
   violation. `samples_in_violation:2` in the event records why. A single noisy
   dip would be ignored.
2. **Hysteresis (`clear = 11.8`).** After firing, the rule doesn't clear the
   moment the rail crosses back over 11.6 V. It clears at `08:00:30` when the
   rail reaches 11.9 V — above the separate 11.8 V clear line — so a reading
   hovering right at the threshold can't flap fired/cleared every sample.

Change a number in [`demo.toml`](demo.toml) or add a line to
[`sensors_demo.jsonl`](sensors_demo.jsonl) and re-run to see the engine react.

## Next steps

- **Analyze recorded history** instead of alerting on it:
  `sensorwatch report --last 24h` condenses a log directory into one
  size-bounded digest (see the [CLI README](../../rust/sensorwatch-cli/README.md#report)).
- **Run it for real** on Windows with HWiNFO64 — drop `--replay` and `watch`
  reads live sensors (see the top-level [README](../../README.md#usage)).
- **Make an agent the monitor** — the
  [`sensorwatch-monitor`](../../skills/sensorwatch-monitor/SKILL.md) skill turns
  this wake-up primitive into an always-on triage loop with an escalation
  ladder.

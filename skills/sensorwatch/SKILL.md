---
name: sensorwatch
description: >-
  Read live hardware sensor readings (temperatures, voltages, fan speeds,
  currents, power, clocks, usage) from a Windows PC with sensorwatch — query the
  current state through its Python/C/C++ API, run the CLI logger to collect
  history as JSON Lines, watch declarative alert rules and dispatch on the
  structured JSON events they emit, and analyze the logged data. Use when an
  agent needs the current hardware state, deterministic hardware alerting, or
  historical sensor trends, on a Windows machine running HWiNFO64 with Shared
  Memory Support enabled.
license: MIT
---

# Using sensorwatch

[sensorwatch](https://github.com/lcjanke2020/sensorwatch) is a lightweight
hardware-sensor monitor for Windows. It reads HWiNFO64's shared-memory feed and
exposes every sensor HWiNFO sees — temperatures, voltages, currents, power, fan
speeds, clocks, and usage. This skill covers the four things an agent typically
wants: **read the current state now**, **collect history**, **watch for alert
events**, and **analyze the logs**.

> **Read-only.** sensorwatch only *reads* hardware data and *writes* local log
> files — it never controls hardware and opens no network listeners. Do not make
> safety-critical decisions from raw sensor values (a single reading can be
> stale, missing, or `NaN`). See [`SECURITY.md`](../../SECURITY.md) §4.

## Prerequisites (read this first)

sensorwatch is **Windows-only** and needs HWiNFO64 supplying the data:

1. **Windows (x64)** with **Python 3.12+**.
2. **HWiNFO64 running** with **Shared Memory Support** enabled
   (Settings → Shared Memory Support) **and the sensors window open**. Without
   this, there is no data to read.
3. Install the package: `pip install sensorwatch` (prebuilt Windows wheels — no
   compiler needed), or from a source checkout `pip install -e .` / `uv sync`.

If a prerequisite is missing you'll see one of these — they are expected, not
crashes (see [Troubleshooting](#troubleshooting)). A `SensorwatchError` prints as
`[<code>] <message>` (from the C ABI); the symbolic `SW_ERR_*` name in parentheses
below is the conceptual `sw_error_t` code, **not** part of the printed text:

| What you'll see | Meaning |
|-----------------|---------|
| `[-4] Sensor source is not running or not enabled` (`SW_ERR_SOURCE_UNAVAILABLE`), or `read_sensors()` returns `None` | HWiNFO not running, shared memory disabled, or sensors window closed |
| `[-3] Backend is unavailable on this platform` (`SW_ERR_UNSUPPORTED_PLATFORM`) | Not running on Windows |
| `ImportError` for `sensorwatch._sw_cffi` | Native extension not built — install a wheel, or use the pure-Python reader below |

## Recipe 1 — Read the current state *now*

The fastest path to live readings is the native binding's `Session` →
`snapshot()`. A `Snapshot` is an immutable sequence of `Reading` objects.

```python
from sensorwatch.native import Session, SensorwatchError

try:
    with Session() as session:                # raises off-Windows or if HWiNFO is down
        with session.snapshot() as snapshot:  # immutable view of all readings at one instant
            print(len(snapshot), "readings from", snapshot.source)  # e.g. "... from HWiNFO"
            for r in snapshot:
                print(f"{r.sensor} / {r.reading} = {r.value} {r.unit} [{r.type.name}]")
except SensorwatchError as exc:
    print(f"sensorwatch unavailable: {exc}")   # e.g. [-4] Sensor source is not running or not enabled
```

Each `Reading` (a frozen dataclass) has: `source`, `sensor`, `reading`, `unit`,
`type` (a `ReadingType` enum), `value`, `minimum`, `maximum`, `average`.
`ReadingType` members: `NONE, TEMPERATURE, VOLTAGE, FAN, CURRENT, POWER, CLOCK,
USAGE, OTHER, UNKNOWN`.

**One-shot CLI.** The Rust CLI's `snapshot` subcommand prints the readings as a
JSON array — handy for a quick read or to pipe elsewhere. Build it once from
the repo's `rust/` directory, then:

```sh
cd rust && cargo build --release -p sensorwatch-cli
./target/release/sensorwatch snapshot                        # all readings, as JSON
./target/release/sensorwatch snapshot --type TEMPERATURE
./target/release/sensorwatch snapshot --match 12V --indent 0
```

It exits `0` after printing (possibly an empty array), `1` with a clear message
when sensorwatch/HWiNFO is unavailable, and `2` on a usage error (an unknown
`--type` or an `--indent` outside 0–16). Non-finite values are emitted as
`null` (valid JSON).

**Python fallback.** Without a Rust toolchain,
[`scripts/snapshot.py`](scripts/snapshot.py) prints the same JSON shape with
the same flags and exit codes (differences: it emits bare `NaN` for non-finite
values, which most JSON parsers reject, and it accepts any non-negative
`--indent`):

```sh
python skills/sensorwatch/scripts/snapshot.py --type TEMPERATURE
```

**Pure-Python fallback.** If the compiled native extension isn't available, the
`sensorwatch.hwinfo_shm` reader gets the same data with no compiled dependency
(it returns `None` instead of raising when HWiNFO is down):

```python
from sensorwatch.hwinfo_shm import read_sensors

readings = read_sensors()                 # list[SensorReading] | None
if readings is None:
    print("HWiNFO shared memory not available")
else:
    for r in readings:                    # fields: sensor_name, reading_name,
        print(r.sensor_name, r.reading_name, r.value, r.unit)   # sensor_type, value,
                                          # value_min/max/avg, unit
```

## Recipe 2 — Collect history (the CLI logger)

To capture readings over time, run the logger. It's a long-running process that
samples on an interval and appends one JSON object per sample to a daily file,
until you stop it with Ctrl+C. The primary logger is the Rust CLI's **`log`**
subcommand (alias: `run`) — a single static binary, byte-compatible with the
Python logger's output. Build it once as in Recipe 1, then:

```sh
./target/release/sensorwatch log                             # ./config.toml if present, else defaults
./target/release/sensorwatch log --config my.toml --verbose  # explicit config + per-sample debug output
./target/release/sensorwatch run                             # the same subcommand, under its alias
```

It writes `logs/sensors_YYYY-MM-DD.jsonl` (a new file each local day; old files
are pruned per `retention_days` on startup and at each rollover), warns once
and keeps retrying if HWiNFO's shared memory is unavailable, and shuts down
cleanly on Ctrl+C / Ctrl+Break / console close. Exit codes: `0` after a
signal-requested shutdown, `1` off-Windows or when startup fails (the log
directory cannot be prepared, or the shutdown signal handler cannot be
installed), `2` on usage errors. Config lookup: the `--config/-c` path, else
`config.toml` in the current directory, else built-in defaults.

**Python fallback.** Without a Rust toolchain, the frozen Python logger does
the same job (flags `--config/-c` and `--verbose/-v` only — it has no
subcommands; its default config lookup also checks next to the installed
package):

```sh
python -m sensorwatch
sensorwatch --config config.toml --verbose
```

**Mixing old and new files.** The Rust logger's records are byte-compatible
with the Python logger's, with three documented divergences — all
parse-identical for JSON consumers: unrecognized reading-type codes render as
a bare `"unknown"` (Python wrote `"unknown(<N>)"`), timestamps always carry
six fractional digits (pendulum omitted them at exactly zero microseconds),
and non-finite values are written as `null` (Python wrote bare `NaN`, which
most JSON parsers reject).

`config.toml` schema — shared by both loggers (every key is optional; defaults
shown; bad values warn and fall back to their default rather than crashing):

```toml
[general]
interval_seconds = 10     # seconds between samples (minimum 1)
log_dir = "logs"          # directory for the JSONL output
retention_days = 30       # delete logs older than this on startup/rollover (0 = keep all)

[sensors]
include = []              # case-insensitive substring patterns to capture (empty = ALL sensors)
exclude = []              # substring patterns to drop (applied after include)
```

Filtering is plain case-insensitive substring matching against the sensor name.
Example — capture only one PSU's sensors:

```toml
[sensors]
include = ["MEG Ai1600T"]
```

## Recipe 3 — Watch for alert events (deterministic alerting)

To be *notified* when hardware crosses a line — instead of polling readings
yourself — use the Rust CLI's **`watch`** subcommand. It evaluates declarative
`[[rules]]` in `config.toml` against live samples and emits a structured JSON
event when a rule fires. Detection is deterministic native code; an agent is
woken only once something has provably happened. **This is the wake-up
primitive an agent monitor arms** — the full five-layer architecture is in
[`docs/agent-monitoring.md`](../../docs/agent-monitoring.md).

> **Rust CLI only.** Unlike Recipes 1–2, `watch` has **no Python fallback** —
> the rule engine lives in the Rust CLI. Build it once as in Recipe 1.

Add rules to `config.toml`. The section is validated **strictly**: `watch`
exits `2` on any invalid rule (the `log` subcommand ignores it entirely):

```toml
[[rules]]
name = "psu-12v-sag"
kind = "threshold"       # threshold | rate | stale | missing | source-unavailable
sensor = "MEG Ai1600T"
reading = "+12V"
type = "Voltage"
metric = "value"         # value | min | max | avg
op = "<"                 # > | >= | < | <=
threshold = 11.6
clear = 11.8             # hysteresis re-arm level (omit = clears at threshold)
for_samples = 3          # consecutive violating samples before firing
severity = "critical"    # info | warning | critical
```

Then arm it. **One-shot** (default) blocks until the first firing rule, prints
one event, and exits `10`; a `--timeout` with no fire exits `0` (a heartbeat).
**Follow** runs until interrupted, appending fired *and* cleared events to
daily `events_YYYY-MM-DD.jsonl` files:

```sh
./target/release/sensorwatch watch --timeout 3600           # one-shot with a heartbeat deadline
./target/release/sensorwatch watch --spool-dir ./spool      # also drop each event as an atomic file
./target/release/sensorwatch watch --follow                 # stream events to daily files
./target/release/sensorwatch watch --replay logs/sensors_2026-02-18.jsonl   # test rules on recorded logs (any OS)
```

The **exit code is the signal** — arm `watch` and dispatch on how it exits:

| Code | Meaning | Agent action |
|------|---------|--------------|
| 10 | A rule fired (one-shot; JSON event on stdout) | Triage the event, then re-arm |
| 0 | Heartbeat timeout / replay exhausted | "All quiet" — re-arm |
| 2 | Invalid or zero rules, or an unknown `--rule` | Fix the config; do not re-arm blindly |
| 1 | Fatal (state/spool/source could not be prepared) | Surface the stderr message |
| 130 | Interrupted by a signal | Shutting down |

Each event is one compact, schema-versioned JSON line (~1 KB):

```json
{"schema_version":1,"seq":42,"id":"psu-12v-sag-42","rule":"psu-12v-sag","type":"threshold","severity":"critical","state":"fired","timestamp":"2026-02-18T08:00:20.000000-05:00","sensor":"MEG Ai1600T","reading":"+12V","value":11.4,"unit":"V","threshold":11.6,"samples_in_violation":2}
```

`seq` is monotonic and persisted (an agent's ack cursor keys off it, never wall
clock); `id` is `"{rule}-{seq}"`. Source loss is **not** an exit code — it
arrives as a `source-unavailable` event, so dispatch on event *content*. Off
Windows the live source only reports "unavailable" (there is no HWiNFO), so
only `source-unavailable` rules can fire there; `--replay` evaluates rules over
recorded logs on any platform.

## Recipe 4 — Review the logged history (the `report` digest)

**Agents never read the raw `sensors_*.jsonl` logs.** They are unbounded,
per-sample, and full of source-lifetime noise; parsing them by hand blows an
agent's context budget and invites subtly wrong aggregates. `sensorwatch report`
is the sanctioned alternative: one call condenses a window of history into a
single **bounded** JSON digest — window aggregates, re-derived rule violations,
sampling gaps, and a liveness meta block — capped at `--max-bytes`. Build the
CLI once as in Recipe 1, then:

```sh
./target/release/sensorwatch report                            # last 24 h, compact JSON
./target/release/sensorwatch report --last 6h --indent 2       # a 6 h window, pretty-printed
./target/release/sensorwatch report --match psu --type TEMPERATURE --last 6h   # focused triage
./target/release/sensorwatch report --since 2026-02-18 --until 2026-02-19       # an explicit date window
```

(Use the built Rust binary path, as in Recipes 1–3 — a bare `sensorwatch` on
PATH may resolve to the Python console script, which has no `report` subcommand.)

Flags: `--since`/`--until` (RFC 3339, local `YYYY-MM-DDTHH:MM:SS`, or a bare
`YYYY-MM-DD` — since = start of day, until = end; until defaults to now) or a
trailing `--last <24h|90m|7d|1d12h|SECONDS>` (default 24 h, conflicts with
`--since`); `--config/-c` and `--log-dir` (where the rules and logs come from);
`--max-bytes` (hard cap, default 8192) and `--top` (max reading rows, default
20); `--match`/`--type` (case-insensitive **display** filters — same vocabulary
as `snapshot`, and they never change the meta counts or the rule evaluation);
`--indent 0–16`.

**Digest anatomy** (`--indent 2`, abbreviated):

```json
{"schema_version":1,
 "meta":{"window":{"since":"2026-02-18T05:00:00Z","until":"2026-02-19T17:00:00Z"},
   "log_dir":"logs","files_scanned":2,"interval_seconds":10,"samples":8,
   "skipped_lines":0,"first_sample":"2026-02-18T08:00:00.000000-05:00",
   "last_sample":"2026-02-19T08:00:20.000000-05:00","series_total":2,"rules_evaluated":1,
   "truncated":{"readings_shown":2,"readings_total":2,"violations_shown":2,
     "violations_total":2,"gaps_shown":2,"gaps_total":2}},
 "violations":[/* frozen watch-event objects (Recipe 3's schema), chronological */],
 "gaps":[{"from":"…-05:00","to":"…-05:00","seconds":120}],
 "readings":[{"sensor":"MEG Ai1600T","reading":"+12V","type":"Voltage","unit":"V",
   "samples":8,"non_finite":0,"first":12.0,"last":12.25,"min":11.25,"max":12.5,
   "avg":11.9375,"delta":0.25,"in_violation":true}]}
```

- **`meta`** — the window, files scanned, per-window `samples`, `skipped_lines`
  (malformed lines the parser dropped), `series_total`, `rules_evaluated`, and
  `truncated` (what was shown vs found after the byte cap).
- **`readings`** — one row per `(sensor, reading)`, aggregated over the window
  itself. `non_finite` counts nulls/NaN/±inf (excluded from the math);
  `first/last/min/max/avg/delta` are over the finite values; `in_violation`
  flags a series that tripped a rule. HWiNFO's own per-record `min`/`max`/`avg`
  are source-lifetime numbers — wrong for a window — and are deliberately
  ignored.
- **`violations`** — the same frozen event objects `watch` emits (Recipe 3),
  re-derived by replaying the window through the identical deterministic engine.
  Their `seq`/`id` are **digest-local ordinals** for reference only — *not* an
  ack cursor.
- **`gaps`** — any pause longer than 3× `interval_seconds`: when the machine was
  off or the logger was down.

**Liveness in one call.** To answer "is the logger alive and how fresh is the
data?", read `meta`: a zero-sample digest (empty arrays, null `first_sample`/
`last_sample`, still exit `0`) means the logger is dead or the machine was off;
otherwise compare `meta.last_sample` against `meta.window.until` — a large lag,
or a trailing entry in `gaps`, means the feed has stalled.

**Aggregate-only by design.** The digest exposes per-window *aggregates*, not
individual samples — so per-sample questions ("at what minute did the GPU peak?",
"plot the +12V rail across yesterday") are **out of protocol on purpose**: the
whole point is a fixed, small context budget. Do not fall back to hand-parsing
the raw logs to answer them. A future `report` capability (a per-series
bucket/sparkline flag, or a sanctioned SQL surface) will widen this when needed.

For a full **human** offline analysis (efficiency study, Polars + DuckDB
queries, charts over a curated flat export), see
[`examples/psu-efficiency/`](../../examples/psu-efficiency/) — a worked example,
not the logger's output format.

## Other language surfaces

The same data is available natively for C/C++ consumers — link, don't duplicate:

- **C ABI** — `sw_session_open` → `sw_snapshot_take` → `sw_snapshot_get_*`
  accessors with `sw_error_t` return codes, in
  [`include/sensorwatch/sensorwatch.h`](../../include/sensorwatch/sensorwatch.h).
  Spec: [`docs/C_ABI.md`](../../docs/C_ABI.md); runnable example:
  [`examples/c/sw_dump.c`](../../examples/c/sw_dump.c).
- **C++ (header-only, C++17 RAII)** —
  [`include/sensorwatch/sensorwatch.hpp`](../../include/sensorwatch/sensorwatch.hpp):
  `sensorwatch::Session` / `Snapshot` / `Reading`, errors as `sensorwatch::Error`.

The Python `sensorwatch.native` binding (Recipe 1) is the same C core via cffi,
so prefer it from Python; reach for the C/C++ headers only when building native
consumers.

**Build & link the C core.** Both a static library and a shared library (DLL)
build from [`CMakeLists.txt`](../../CMakeLists.txt) — both on by default:

```sh
# Static library only (tests off → no cmocka network fetch)
cmake -B build -DSW_BUILD_TESTS=OFF -DSW_BUILD_SHARED=OFF
cmake --build build --target sensorwatch_static   # or --target sensorwatch for the DLL
```

Compile consumers of the **static** library with `-DSW_STATIC`; for the **DLL**,
define nothing (on Windows `SW_API` resolves to `dllimport`). From CMake, link the
namespaced targets — `sensorwatch::sensorwatch_static`, `sensorwatch::sensorwatch`,
or header-only `sensorwatch::hpp` — which propagate the right includes and defines.
Consume them either in-tree (`add_subdirectory` / `FetchContent`) or from an
installed tree via `cmake --install` + `find_package(sensorwatch CONFIG REQUIRED)`
(the same target names apply). Full details, toggles, and the linking rules:
[README → Building the native core](../../README.md#building-the-native-core-c).

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `[-4] Sensor source is not running or not enabled` (`SW_ERR_SOURCE_UNAVAILABLE`), or `read_sensors()` → `None` | HWiNFO not running, shared memory disabled, or sensors window closed | Start HWiNFO64, enable Settings → Shared Memory Support, open the sensors window |
| `[-3] Backend is unavailable on this platform` (`SW_ERR_UNSUPPORTED_PLATFORM`) | Not Windows | sensorwatch reads a Windows-only shared-memory source |
| `ImportError: sensorwatch._sw_cffi ... not built` | Native extension missing | `pip install sensorwatch` (prebuilt Windows wheel), or use the pure-Python `read_sensors()` (Recipe 1) |
| A reading's `value` is `NaN`, or its category is the catch-all | HWiNFO exposes some entries without a current value / known category | Skip `NaN` values; treat the catch-all category as uncategorized. **The spelling differs by surface:** the native API's `reading.type.name` is upper-case (`OTHER` / `UNKNOWN`), while the *Python* logger JSONL and `read_sensors()` use title-case `"Other"` and `"unknown(<N>)"` for unrecognized codes (there is no literal `"Unknown"`); the Rust CLI (both `snapshot` and `log`) and the Python snapshot helper use the same title-case labels with a bare `"unknown"`. |

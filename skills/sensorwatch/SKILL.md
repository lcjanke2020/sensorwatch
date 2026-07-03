---
name: sensorwatch
description: >-
  Read live hardware sensor readings (temperatures, voltages, fan speeds,
  currents, power, clocks, usage) from a Windows PC with sensorwatch — query the
  current state through its Python/C/C++ API, run the CLI logger to collect
  history as JSON Lines, and analyze the logged data. Use when an agent needs the
  current hardware state, or historical sensor trends, on a Windows machine
  running HWiNFO64 with Shared Memory Support enabled.
license: MIT
---

# Using sensorwatch

[sensorwatch](https://github.com/lcjanke2020/sensorwatch) is a lightweight
hardware-sensor monitor for Windows. It reads HWiNFO64's shared-memory feed and
exposes every sensor HWiNFO sees — temperatures, voltages, currents, power, fan
speeds, clocks, and usage. This skill covers the three things an agent typically
wants: **read the current state now**, **collect history**, and **analyze the
logs**.

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

## Recipe 3 — Analyze the logged data

Each log line is one sample: a `timestamp` plus a `sensors` array. This is the
**generic** shape the logger emits:

```json
{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [
  {"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}
]}
```

Read it with the stdlib — e.g. pull every temperature reading:

```python
import json

with open("logs/sensors_2026-02-18.jsonl", encoding="utf-8") as f:
    for line in f:
        record = json.loads(line)
        ts = record["timestamp"]
        for s in record["sensors"]:
            if s["type"] == "Temperature":
                print(ts, s["sensor"], s["reading"], s["value"], s["unit"])
```

For larger analyses, flatten the nested records into a tabular frame (one row per
reading per sample) and use Polars/DuckDB:

```python
import json, polars as pl

rows = []
with open("logs/sensors_2026-02-18.jsonl", encoding="utf-8") as f:
    for line in f:
        rec = json.loads(line)
        for s in rec["sensors"]:
            rows.append({"timestamp": rec["timestamp"], **s})
df = pl.DataFrame(rows)
```

A full worked analysis (efficiency study, Polars + DuckDB queries, charts) lives
in [`examples/psu-efficiency/`](../../examples/psu-efficiency/). **Note:** that
directory's published dataset is a curated *flat, PSU-specific* export (16
columns), **not** the generic nested format above — its README explains the
schema. Treat it as an analysis worked-example, not the logger's output format.

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

# sensorwatch

[![CI](https://github.com/lcjanke2020/sensorwatch/actions/workflows/ci.yml/badge.svg)](https://github.com/lcjanke2020/sensorwatch/actions/workflows/ci.yml)

A lightweight hardware sensor monitor for Windows. It reads [HWiNFO64](https://www.hwinfo.com/)'s
shared-memory sensor feed and logs readings as JSON Lines with daily file
rotation — a small, dependency-light background process you can leave running
and analyze later.

PSU efficiency monitoring is the first use case (see the
[worked example](examples/psu-efficiency/) below), but sensorwatch is
sensor-agnostic: it captures anything HWiNFO exposes — temperatures, voltages,
currents, power, fan speeds, clocks, and usage.

> **Flagship result:** a ~5.5-hour real-world capture shows the MSI MEG Ai1600T
> exceeds 80 PLUS Titanium efficiency at every measured load point (peak 94.5%,
> zero samples below 92%). Data, charts, and analysis:
> [`examples/psu-efficiency/`](examples/psu-efficiency/).

## Features

- **Reads HWiNFO64 shared memory** (`Global\HWiNFO_SENS_SM2`) directly via
  `ctypes` — no polling of HWiNFO's UI, no admin rights.
- **Sensor filtering** by case-insensitive substring include/exclude patterns.
- **JSON Lines output** with daily file rotation and configurable retention.
- **Graceful shutdown** on Ctrl+C / signals.
- **Light footprint** — a handful of stdlib modules plus `pendulum`.
- **Optional native binding** (`sensorwatch.native`) — a cffi wrapper over the
  bundled C core that reads the same data through the native parser (see
  [Native binding](#native-binding-cffi)).

## Requirements

- Windows (x64)
- Python 3.12+
- [HWiNFO64](https://www.hwinfo.com/) running with **Shared Memory Support**
  enabled (Settings → Shared Memory Support) and the sensors window open.

## Install

From PyPI — Windows wheels are prebuilt, so no compiler is needed:

```sh
pip install sensorwatch
```

From source — this builds the native cffi extension, so a C compiler is required
(MSVC on Windows; gcc/clang elsewhere):

```sh
git clone https://github.com/lcjanke2020/sensorwatch
cd sensorwatch
pip install -e .          # or: uv sync
```

## Usage

```sh
# Run with the bundled default config
python -m sensorwatch

# Or use the installed console script
sensorwatch --config config.toml --verbose
```

If HWiNFO64 is not running (or shared memory is disabled), sensorwatch logs a
warning and keeps trying — start HWiNFO and readings begin flowing.

## Running from WSL-2

sensorwatch is a Windows program, but you can launch it from a WSL-2 shell via
Windows interop — convenient if you prefer WSL-2's persistent SSH / terminal
multiplexer (tmux, WezTerm) sessions. You can't run it as a native Linux
process; you drive the Windows build from the WSL-2 side. See
[docs/running-from-wsl2.md](docs/running-from-wsl2.md).

## Output format

One JSON object per sample, written to `logs/sensors_YYYY-MM-DD.jsonl`:

```json
{"timestamp": "2026-02-18T08:17:48-05:00", "sensors": [
  {"sensor": "MEG Ai1600T", "reading": "+12V", "type": "Voltage", "value": 12.03, "min": 12.01, "max": 12.17, "avg": 12.06, "unit": "V"}
]}
```

## Configuration

`config.toml`:

| Key | Default | Description |
|-----|---------|-------------|
| `general.interval_seconds` | `10` | Seconds between samples |
| `general.log_dir` | `"logs"` | Directory for JSONL output |
| `general.retention_days` | `30` | Delete log files older than this on startup (`0` = keep all) |
| `sensors.include` | `[]` | Substring patterns to capture (empty = all sensors) |
| `sensors.exclude` | `[]` | Substring patterns to drop (applied after include) |

Example — capture only a specific PSU's sensors:

```toml
[sensors]
include = ["MEG Ai1600T"]
```

## Testing / CI scope

Continuous integration runs the unit tests on Ubuntu and Windows across Python
3.12 and 3.13. The tests cover the **parsing, configuration, and logging
logic** — in particular, the HWiNFO shared-memory parser is exercised against
**synthetic byte buffers** (`_parse_shared_memory()`), so the untrusted-header
bounds checks are validated without a live sensor source. The Python job also
builds the native cffi extension and runs the binding's non-live tests (the live
HWiNFO path is skipped, and `SW_ERR_UNSUPPORTED_PLATFORM` is asserted on Linux);
the C core is built and unit-tested separately with cmocka on both OSes.

CI does **not** — and cannot — exercise a real sensor read. That path requires
[HWiNFO64](https://www.hwinfo.com/) running on Windows with **Shared Memory
Support** enabled, and is verified manually. So a green CI badge means the logic
is sound, not that end-to-end sensor reading has been validated on your machine.

Run the tests locally:

```sh
uv sync
uv run pytest
```

## Building the native core (C)

Alongside the Python package, sensorwatch ships a small native C core that
implements the source-neutral C ABI in
[`include/sensorwatch/sensorwatch.h`](include/sensorwatch/sensorwatch.h) (see
[`docs/C_ABI.md`](docs/C_ABI.md)). It opens HWiNFO shared memory read-only,
copies-then-parses it with full bounds validation, and exposes immutable
snapshots — no third-party runtime dependencies beyond the C runtime and Windows
SDK. The Python package binds to this core via cffi — see
[Native binding](#native-binding-cffi).

Build the DLL + static library and run the cmocka unit tests with CMake:

```sh
cmake -B build -DSW_BUILD_TESTS=ON
cmake --build build
ctest --test-dir build --output-on-failure
```

MSVC is the primary toolchain; the parser core also builds with GCC/Clang
(including MinGW) for development and CI cross-checks. Useful options:

- `-DSW_ENABLE_ASAN=ON` — AddressSanitizer (plus UBSan on GCC/Clang).
- `-DSW_ENABLE_ANALYZE=ON` — MSVC `/analyze` static analysis (non-fatal).
- `-DSW_BUILD_EXAMPLES=ON` — build `sw_dump`, which prints a live snapshot (run it
  with HWiNFO64 running and Shared Memory Support enabled).

Like the Python suite, the C tests feed the parser **synthetic byte buffers** (no
live HWiNFO needed) and mirror the invariants in
[`tests/test_hwinfo_shm.py`](tests/test_hwinfo_shm.py); the Windows-only session
test mocks the Win32 calls.

## Native binding (cffi)

`sensorwatch.native` is a thin, safe Python wrapper over the bundled C core, built
with [cffi](https://cffi.readthedocs.io/) in API mode. The C sources are compiled
directly into a Python extension — there is no separate DLL to locate or load — so
it ships as an ordinary binary wheel and reads the same HWiNFO data as the
pure-Python reader, through the native parser.

```python
from sensorwatch.native import Session

with Session() as session:           # raises on non-Windows or if HWiNFO is down
    snapshot = session.snapshot()    # an immutable view of all readings
    print(len(snapshot), "readings from", snapshot.source)
    for r in snapshot:
        print(f"{r.sensor} / {r.reading} = {r.value} {r.unit} [{r.type.name}]")
```

Every native error surfaces as a `SensorwatchError` carrying the `sw_error_t` code
and the library's message — e.g. `SW_ERR_SOURCE_UNAVAILABLE` when HWiNFO isn't
running, `SW_ERR_UNSUPPORTED_PLATFORM` on non-Windows. `Session` and `Snapshot`
are context managers, and a `Snapshot` is an immutable sequence of `Reading`s
(`source`, `sensor`, `reading`, `unit`, `type`, `value`, `minimum`, `maximum`,
`average`). The pure-Python reader and the CLI are unchanged — the native binding
is an additional, optional API over the same data.

## Roadmap

sensorwatch starts as a Python monitor and grows toward a general hardware
observability toolkit:

- **Source-adapter architecture** — pluggable sensor sources behind one
  interface (HWiNFO today; UPS, AIDA64, and IPMI next) with stable sensor
  identities and per-reading quality flags.
- **Optional localhost REST service** for live queries (bound to `127.0.0.1`).
- **Native C core** — a dependency-free Windows DLL (plus static library)
  implementing the source-neutral C ABI in
  [`include/sensorwatch/sensorwatch.h`](include/sensorwatch/sensorwatch.h)
  ([`docs/C_ABI.md`](docs/C_ABI.md); standards in
  [`docs/C_CODING_STANDARDS.md`](docs/C_CODING_STANDARDS.md)). Built with CMake —
  see [Building the native core](#building-the-native-core-c). **Language bindings**
  over that core: a Python binding ships now (cffi — see
  [Native binding](#native-binding-cffi)); C++ and Rust are next.
- **Agent integration** via an MCP / skill layer so AI agents can query
  hardware state directly.

See [`SECURITY.md`](SECURITY.md) for the threat model covering these planned
components.

## Security

sensorwatch reads read-only hardware data and writes local log files; it opens
no network listeners in its current form. The full threat model — shared-memory
attack surface, the planned REST service and agent layer, supply-chain notes —
is in [`SECURITY.md`](SECURITY.md). Please report vulnerabilities privately (see
that document).

## Contributing

Contributions are welcome — see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

[MIT](LICENSE) © Leonard Janke

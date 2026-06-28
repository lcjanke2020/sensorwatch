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

## Requirements

- Windows (x64)
- Python 3.12+
- [HWiNFO64](https://www.hwinfo.com/) running with **Shared Memory Support**
  enabled (Settings → Shared Memory Support) and the sensors window open.

## Install

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
bounds checks are validated without a live sensor source.

CI does **not** — and cannot — exercise a real sensor read. That path requires
[HWiNFO64](https://www.hwinfo.com/) running on Windows with **Shared Memory
Support** enabled, and is verified manually. So a green CI badge means the logic
is sound, not that end-to-end sensor reading has been validated on your machine.

Run the tests locally:

```sh
uv sync
uv run pytest
```

## Roadmap

sensorwatch starts as a Python monitor and grows toward a general hardware
observability toolkit:

- **Source-adapter architecture** — pluggable sensor sources behind one
  interface (HWiNFO today; UPS, AIDA64, and IPMI next) with stable sensor
  identities and per-reading quality flags.
- **Optional localhost REST service** for live queries (bound to `127.0.0.1`).
- **Language bindings** over a shared core.
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

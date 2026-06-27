# MSI MEG Ai1600T PCIE5 — Efficiency Test Results (Feb 18, 2026)

## Overview

Real-world efficiency validation of the MSI MEG Ai1600T PCIE5 Titanium PSU
using our open-source sensor monitoring tool. Data was collected via HWiNFO64
shared memory at 10-second intervals over ~5.5 hours of mixed idle and stress
workloads.

**Result: The PSU exceeds 80 PLUS Titanium (115V) requirements at every
measured load point.**

## Test Setup

- **PSU**: MSI MEG Ai1600T PCIE5, ATX 3.1, 1600W Titanium
- **System**: Desktop workstation (Windows 11, x64)
- **Monitoring**: HWiNFO64 shared memory → Python `ctypes` reader → JSONL logs
- **Input voltage**: 115V (US residential)
- **Sampling interval**: 10 seconds
- **Duration**: ~5.5 hours (06:55 - 12:17 EST)
- **Total samples**: 2,070

### Stress tools used

Load was swept from idle to a ~1,048 W peak using **AIDA64 Extreme** and
**OCCT** stress tests across CPU, FPU, and combined power profiles to cover the
full power range.

## Key Findings

### Efficiency across load range

| Load Range | 12V Rail | Avg Efficiency | 80+ Titanium (115V) | Margin |
|-----------|----------|---------------|--------------------:|-------:|
| < 300W (idle) | ~185W | 92.9% | 90% @ 10% | +2.9% |
| 300-400W | ~280W | 94.0% | 92% @ 20% | +2.0% |
| 400-500W | ~380W | 94.5% | — | — |
| 500-600W | ~480W | 94.5% | — | — |
| 600-700W | ~580W | 94.4% | — | — |
| 700-800W | ~680W | 94.4% | 94% @ 50% | +0.4% |
| 800-900W | ~780W | 94.0% | — | — |
| 900-1000W | ~880W | 93.6% | — | — |
| 1000W+ | ~990W | 93.3% | — | — |

Peak efficiency: **94.5%** at 400-600W output (25-38% of rated capacity).

Zero samples below 92% efficiency across the entire dataset.

### Voltage regulation

All rails held well within ATX ±5% spec under all load conditions:

| Rail | Nominal | Measured Range | Deviation | Ripple |
|------|---------|---------------|-----------|--------|
| +12V | 12.000V | 12.016 - 12.172V | +0.13% to +1.43% | 156 mV |
| +5V | 5.000V | 4.953 - 4.977V | -0.94% to -0.47% | 24 mV |
| +3.3V | 3.300V | 3.344 - 3.367V | +1.33% to +2.04% | 23 mV |

### Thermal performance

| Metric | Idle (<300W) | Full Stress (>900W) |
|--------|-------------|-------------------|
| PSU Temperature | 25°C | 31-32°C |
| Fan Speed | ~1182 RPM | ~1184 RPM |
| Temperature delta | — | +7°C |

The fan did not ramp at any point during testing — speed remained flat at
~1185 RPM from idle through 1048W peak load. This highlights a known concern:
the Ai1600T lacks fan failure detection (confirmed by Cybenetics report). If
the fan failed, the PSU would continue operating with no thermal protection.
A monitor like sensorwatch could surface this condition by watching for fan
speed dropping to zero.

### Peak load moment

- **Time**: 08:17:48 EST
- **Power output**: 1,048W (65.5% of 1600W rated)
- **12V rail**: 12.031V @ 82.2A (989.6W)
- **Minor rails**: 62W combined (+5V + +3.3V)
- **Efficiency**: 93.2%
- **Temperature**: 31°C
- **Fan**: 1,190 RPM

### Consistency with Cybenetics certification

Our measured data aligns with the [Cybenetics lab report](https://www.cybenetics.com/evaluations/psus/2700/)
(certification #252700, issued January 9, 2025):

- Cybenetics rates the unit **Titanium** at both 115V and 230V
- Cybenetics average efficiency at 115V: 91.6% across 1,450+ load combinations
- Our operating points fall in the expected zones on Cybenetics' 2D efficiency
  heatmap (12V watts vs minor rails watts)
- Our idle cluster (~185W 12V, ~50W minor) maps to Cybenetics' >94% (red) zone
- Our stress cluster (~880W 12V, ~63W minor) maps to Cybenetics' 92-94% (black) zone

## Data Files

| File | Format | Size | Description |
|------|--------|------|-------------|
| `efficiency_test_2026_02_18.jsonl` | JSON Lines | 908 KB | One JSON object per sample, flat column names |
| `efficiency_test_2026_02_18.parquet` | Apache Parquet (Snappy) | 75 KB | Columnar format for Polars/DuckDB/pandas |

### Schema (16 columns)

| Column | Type | Unit |
|--------|------|------|
| `timestamp` | string (ISO 8601) | — |
| `psu_temperature_c` | float64 | °C |
| `voltage_3v3` | float64 | V |
| `voltage_5v` | float64 | V |
| `voltage_12v` | float64 | V |
| `current_3v3_a` | float64 | A |
| `current_5v_a` | float64 | A |
| `current_12v_a` | float64 | A |
| `power_3v3_w` | float64 | W |
| `power_5v_w` | float64 | W |
| `power_12v_w` | float64 | W |
| `power_sum_w` | float64 | W |
| `power_out_w` | float64 | W |
| `efficiency_pct` | float64 | % |
| `fan_speed_rpm` | float64 | RPM |
| `total_runtime_h` | float64 | hours |

### Quick start with DuckDB

```sql
SELECT
    power_out_w AS watts,
    efficiency_pct AS efficiency,
    psu_temperature_c AS temp_c,
    fan_speed_rpm AS fan
FROM read_parquet('efficiency_test_2026_02_18.parquet')
WHERE power_out_w > 800
ORDER BY power_out_w DESC
LIMIT 10;
```

### Quick start with Python + Polars

```python
import polars as pl

df = pl.read_parquet("efficiency_test_2026_02_18.parquet")

# Efficiency by 100W buckets
print(
    df.with_columns((pl.col("power_out_w") / 100).cast(pl.Int32) * 100)
    .rename({"power_out_w": "bucket_w"})
    .group_by("bucket_w")
    .agg(
        pl.col("efficiency_pct").mean().alias("avg_eff"),
        pl.col("efficiency_pct").min().alias("min_eff"),
        pl.col("efficiency_pct").max().alias("max_eff"),
        pl.len().alias("samples"),
    )
    .sort("bucket_w")
)
```

## Monitoring Tool

Data was collected using **sensorwatch**, our open-source hardware sensor monitor:
[github.com/lcjanke2020/sensorwatch](https://github.com/lcjanke2020/sensorwatch)

The tool reads HWiNFO64's shared memory interface (`Global\HWiNFO_SENS_SM2`)
via Python `ctypes` and logs sensor readings as JSON Lines with daily file
rotation. It runs as a lightweight background process (~0% CPU overhead).

## Charts

Charts generated from this dataset are in this directory:

- `efficiency_scatter.png` — 2D scatter (12V watts vs minor rails) colored by
  efficiency band, matching Cybenetics' heatmap axes
- `full_day_analysis.png` — 4-panel overview: efficiency curve, temperature vs
  load, fan speed vs load, and power draw time series
- `stress_test_chart.png` — Time series of the morning AIDA64 stress test

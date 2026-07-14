# Report digest fixtures

Real `sensorwatch report` output — committed bytes, not hand-typed — used by
[`../../test_monitor_scripts.py`](../../test_monitor_scripts.py) to drive
`reconcile_incidents.py` against the digest schema the Rust CLI actually emits
(so schema drift breaks these tests, not production).

All five were generated with the repo's public demo rule
([`examples/demo/demo.toml`](../../../examples/demo/demo.toml), `psu-12v-sag`:
`< 11.6`, clear `11.8`, `for_samples 2`) over the demo's public PSU golden
numbers; `--log-dir` was passed as a relative path so no machine paths land in
`meta.log_dir`.

| File | Log content | Window end | What it exercises |
|------|-------------|------------|-------------------|
| `psu-sag-recovered.json` | the demo sag log (`examples/demo/sensors_demo.jsonl` staged as `logs/sensors_2026-02-18.jsonl`) | `08:00:35` (5 s after the last sample) | fresh feed; latest transition for `psu-12v-sag` is `cleared` → auto-close |
| `psu-sag-still-firing.json` | same | `08:00:25` (between fire and clear) | fresh feed; latest transition is `fired` → no close |
| `psu-sag-stale-feed.json` | same | `09:00:00` (~59 min after the last sample) | freshness gate: a `cleared` transition is present but the feed is stale → `indeterminate` |
| `zero-samples.json` | `--log-dir no-such-dir` | `08:00:35` | zero-sample digest → freshness gate fails, `logger_health` degraded |
| `gappy-feed.json` | four normal samples with a 1190 s hole (08:00:10 → 08:20:00) | `08:20:15` | fresh feed, no transitions; single gap > 15 min → `logger_health` degraded |

To regenerate: stage the log content under a **relative** log dir, then

```
sensorwatch report --config examples/demo/demo.toml --log-dir <dir> \
    --since 2026-02-18T07:59:50-05:00 --until <window-end>-05:00 > <fixture>.json
```

The gappy log is the demo record with normal (≈12 V) values at
`08:00:00`, `08:00:10`, `08:20:00`, `08:20:10`.

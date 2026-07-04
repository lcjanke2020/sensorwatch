# Monitor event fixtures

Real `sensorwatch watch --spool-dir` output — committed bytes, not hand-typed —
used by [`../../test_monitor_scripts.py`](../../test_monitor_scripts.py) to drive
the `sensorwatch-monitor` helper scripts against the frozen 14-key event contract
([`docs/agent-monitoring.md`](../../../docs/agent-monitoring.md)).

| File | How it was generated |
|------|----------------------|
| `fired-critical-threshold.json` | Replaying a PSU +12V sag log through the canonical `psu-12v-sag` threshold rule (`< 11.6`, clear `11.8`, `for_samples 2`) — the fire on the 2nd violating sample. |
| `cleared.json` | The same replay's recovery sample, which clears the rule. |
| `source-unavailable.json` | A live one-shot `watch` with a `source-unavailable` rule on a non-Windows host, where the sensor source is always unavailable. |

All values are the repo's public PSU golden numbers (also in
`rust/sensorwatch-cli/tests/`); no machine-tuned thresholds or private data. To
regenerate, replay a sag/recover log with the PSU rule under `--follow
--spool-dir`, and run a live `watch` with a `source-unavailable` rule for the
third.

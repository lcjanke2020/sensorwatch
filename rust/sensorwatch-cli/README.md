# sensorwatch-cli

The sensorwatch command-line interface — a binary named `sensorwatch`, built on
the safe [`sensorwatch`](../sensorwatch) wrapper crate (`Session` / `Snapshot`
/ `Reading`) rather than reimplementing the wire format.

**Repo-only for now** (`publish = false`): build it from a checkout with
`cargo build --release -p sensorwatch-cli`. Publishing to crates.io for
`cargo install` is a later, explicit step — see the
[ROADMAP](../../ROADMAP.md#phase-3--the-broader-observability-toolkit).

## `snapshot`

One-shot live sensor readings as a JSON array on stdout:

```sh
sensorwatch snapshot                        # all readings
sensorwatch snapshot --type TEMPERATURE     # one reading type (case-insensitive)
sensorwatch snapshot --match 12V            # substring over sensor/reading names
sensorwatch snapshot --indent 0             # compact single line (default indent: 2)
```

Each element has the keys `source, sensor, reading, type, value, min, max,
avg, unit` — the same shape (and Title-case `type` labels) as the agent
skill's Python helper, which this subcommand replaces. Non-finite values
serialize as `null` (valid JSON; the Python helper printed bare `NaN`).

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Snapshot printed (possibly an empty array `[]`) |
| 1 | Sensor source unavailable (HWiNFO64 not running / shared memory disabled) or platform unsupported (not Windows) — message on stderr |
| 2 | Usage error (unknown flag or subcommand, invalid `--type`, negative `--indent`) |

A live read requires Windows with HWiNFO64's *Shared Memory Support* enabled;
everywhere else the binary builds and exits `1` with a clear message. Logging
goes to stderr under `RUST_LOG` (e.g. `RUST_LOG=debug`), never stdout.

## License

MIT — see [LICENSE](LICENSE).

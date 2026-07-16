# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the Python package
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Version numbers below track the **`sensorwatch` Python package** (published to
[PyPI](https://pypi.org/project/sensorwatch/)). The C core, the C++/Rust
bindings, the Rust CLI, and the agent skills live in the same repository and are
versioned independently — the Rust crates are published to
[crates.io](https://crates.io/crates/sensorwatch) at `0.1.0`. The full picture
is CONTRIBUTING's ["Version streams"](CONTRIBUTING.md#version-streams-what-version-is-sensorwatch)
section. Repository work that has not yet been folded into a tagged Python
release is listed under [Unreleased](#unreleased).

## [Unreleased]

_Repository work on `master` since the `0.3.0` Python tag is recorded here
until the next Python release picks it up._

### Added

- **`sensorwatch export`** (LEO-349) — new Rust CLI subcommand that streams a
  `--since`/`--until`/`--last` window of `sensors_*.jsonl` through the same
  bounded lenient replay parser as `report` and writes a flat
  one-row-per-reading-per-sample Apache Parquet file (Snappy), columns in file
  order: `timestamp` (TIMESTAMP micros, UTC), `sensor`, `reading`, `type`
  (STRING), `value` (nullable DOUBLE — absent/null/non-finite readings become
  SQL NULL), `unit` (STRING); HWiNFO's source-lifetime min/max/avg are
  deliberately excluded. An `--out` that names a selected input log is refused
  (usage error), so an export can never overwrite the history it reads. The
  sanctioned **deep-analysis** surface for per-sample SQL with DuckDB / Polars
  / pandas on the consumer side; `report` stays the first-line bounded digest.
  The usage skill gains a matching "deep analysis" recipe (Recipe 5),
  resolving Recipe 4's standing "sanctioned SQL surface" design note.

### Changed

- **Rust workspace MSRV: 1.82 → 1.85** (LEO-349) — required by the `parquet`
  crate (59.x, an edition-2024 dependency tree; built with default features
  off, so no arrow stack). The clap (`<4.5.58`) and toml (`<0.9`) caps in
  `sensorwatch-cli` are unchanged but now liftable in a future dedicated
  bump, since Rust 1.85 parses edition 2024.

## [0.3.0] - 2026-07-16

_Rolls up all repository work on `master` since `0.2.0` — the Rust CLI, the
C++/Rust bindings, and the agent skills — into a tagged Python release._

### Added

- **Release provenance** (LEO-416) — retroactive annotated tag `rust-v0.1.0` on
  the commit the crates.io `0.1.0` name-claim publish was made from
  (2026-07-01, predating the tag flow), a provenance note in CONTRIBUTING's
  Rust-releasing section, and a new CONTRIBUTING **"Version streams"** section
  documenting the three deliberately independent version streams (Python
  package `0.3.0` as of this release / C ABI draft `0.2.0` / Rust workspace
  `0.1.0` with unreleased API awaiting the next crate release) and what publishing the
  repo-only CLI crate would take (a `version` on its path-only dependency —
  the one `cargo package` blocker — plus flipping `publish = false`, plus
  in-crate copies of the two test resources `rust/sensorwatch-cli/src/e2e.rs`
  reaches outside the crate root for, so the packaged tests stay runnable).
  Registry badges landed earlier with the docs-truth pass (LEO-412).

- **clang-cl CI job + Windows sanitizer legs + blocking static analysis**
  (LEO-327) — native CI now builds and tests the C core with clang-cl (the
  coding standard's secondary Windows compiler) including a sanitizer pass,
  runs the MSVC leg's tests — including the Windows-only Win32 session-layer
  suite — under AddressSanitizer, compiles the cffi extension with ASan and
  runs the native-binding pytest suite under the preloaded runtime, and gates
  `src/*.c` with a blocking, version-pinned clang-tidy pass
  ([`.clang-tidy`](.clang-tidy)). One real finding fixed along the way:
  `sw_decode_field()` now sizes its output for the UTF-8 worst case provable
  locally, instead of leaning on the cp1252 decoder table's value range
  (output bytes unchanged).

- **`sw_snapshot_from_buffer()`** — new public C ABI entry point (ABI
  `0.1.0` → `0.2.0`) that parses a caller-supplied HWiNFO shared-memory image
  into a snapshot through the same validating parser as `sw_snapshot_take()`,
  with no session or live source. Exposed as `Snapshot::from_buffer` in the
  Rust binding and via the cffi layer in Python. Enables the populated-Snapshot
  accessor tests of every binding, plus a cross-language end-to-end test
  (synthetic buffer → snapshot → logger JSONL → watch engine → event JSON), to
  run on the Linux CI legs (LEO-415).
- **Rust CLI** (`rust/sensorwatch-cli`, binary `sensorwatch`) built on the safe
  Rust binding: `snapshot` (one-shot live readings as JSON), `log` (JSONL logger
  loop, byte-compatible with the Python logger), `watch` (declarative
  `[[rules]]` evaluation emitting structured JSON events for deterministic
  alerting), and `report` (size-bounded history digest for agent consumption).
- **Header-only C++17 binding** (`include/sensorwatch/sensorwatch.hpp`) — RAII
  wrappers over the C ABI.
- **Rust bindings** — the `sensorwatch-sys` FFI crate and the safe `sensorwatch`
  wrapper, published to crates.io (`0.1.0`) via OIDC trusted publishing, with a
  vendored C core and CI drift gates.
- **CMake packaging** — `install()` and `find_package(sensorwatch CONFIG)`
  export so C/C++ consumers can link the native core from an installed tree.
- **Agent skills** — `skills/sensorwatch/` (portable Agent Skills bundle teaching
  an agent to read state, run the logger, and analyze output) and
  `skills/sensorwatch-monitor/` (the always-on wake-up monitoring protocol: arm
  `watch`, dispatch on its exit code, triage, and record durable state, with a
  deterministic escalation ladder and cooldowns).
- **Monitor auto-close on recovery** — `reconcile_incidents.py` closes open
  incidents whose latest re-derived transition in a `report` digest is
  `cleared` (freshness-gated so a dead logger never looks like recovery), and
  reports a `logger_health` gap-density verdict the skill escalates on — both
  pilot follow-ups from the LEO-341 field report.
- **Tier-3 issue drafts** — `notify.py --issue-draft` writes a tracker-ready
  draft to `outbox/issues/` in the same invocation as the routed notification,
  recording the per-rule cooldown exactly once.
- **Fuzz harnesses + nightly CI** — a libFuzzer target over the C shared-memory
  parser (`sw_parse_buffer`, gated behind the `SW_BUILD_FUZZ` CMake option) and
  cargo-fuzz targets over the Rust JSONL replay parser (`parse_line` /
  `fixup_python_tokens`) — the C target under AddressSanitizer + UBSan, the Rust
  targets under AddressSanitizer (with Rust's debug-assertion + overflow checks) —
  run nightly by a new `fuzz.yml` workflow. Adds adversarial parser unit cases (32-bit
  `count × size` wrap, an oversized element, unterminated name/unit fields) with a
  committed seed corpus. The `sensorwatch-cli` crate gains a thin library target so
  the fuzz harness can reach the replay parser (`main` is now a shim over it).

### Changed

- **Docs handoff (LEO-342)** — the Rust CLI is the canonical `sensorwatch`
  interface and the Python package is frozen as the reference implementation;
  README, ROADMAP, CONTRIBUTING, and the agent skills updated accordingly
  (including Windows-native command forms for the live-read examples).

### Removed

- The **`sensorwatch` console-script entry point** — the Rust CLI binary owns
  the `sensorwatch` name now; the Python logger stays runnable via
  `python -m sensorwatch`. Installs of 0.2.0 or earlier keep their old console
  script until upgraded.
- The agent skill's bundled **`scripts/snapshot.py`** helper — the Rust CLI's
  `snapshot` subcommand is the sanctioned one-shot read (same JSON shape; emits
  valid-JSON `null` where the helper printed bare `NaN`).

### Fixed

- `sw_snapshot_take()` (and the parser behind `sw_snapshot_from_buffer()`) now
  validates the out-pointer first and sets `*out_snapshot` to `NULL` before any
  other argument check, so a NULL session/buffer alongside a valid out-pointer
  can no longer leave a stale handle behind — matching the documented
  "NULL on failure when possible" contract.

## [0.2.0] - 2026-06-29

### Added

- **Native, dependency-free C core** with a bounds-checked HWiNFO shared-memory
  parser behind an opaque-handle C ABI
  (`include/sensorwatch/sensorwatch.h`, spec in `docs/C_ABI.md`).
- **Python cffi (API-mode) binding** `sensorwatch.native` over the C core,
  reading the same data through the native parser. Prebuilt Windows wheels for
  CPython 3.12 and 3.13, plus a source distribution.

## [0.1.1] - 2026-06-28

### Added

- Test suite and cross-platform CI (Ubuntu + Windows).
- PyPI trusted-publishing (OIDC) release workflow.

## [0.1.0] - 2026-06-27

### Added

- Initial public release: a lightweight Windows HWiNFO64 shared-memory monitor
  that logs readings as JSON Lines with daily file rotation, case-insensitive
  substring include/exclude sensor filtering, configurable retention, and
  graceful shutdown on Ctrl+C / signals.

[Unreleased]: https://github.com/lcjanke2020/sensorwatch/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lcjanke2020/sensorwatch/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lcjanke2020/sensorwatch/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lcjanke2020/sensorwatch/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lcjanke2020/sensorwatch/releases/tag/v0.1.0

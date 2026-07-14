# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the Python package
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Version numbers below track the **`sensorwatch` Python package** (published to
[PyPI](https://pypi.org/project/sensorwatch/)). The C core, the C++/Rust
bindings, the Rust CLI, and the agent skills live in the same repository and are
versioned independently — the Rust crates are published to
[crates.io](https://crates.io/crates/sensorwatch) at `0.1.0`. Repository work
that has not yet been folded into a tagged Python release is listed under
[Unreleased](#unreleased).

## [Unreleased]

_Repository work on `master` since the `0.2.0` Python tag — the Rust CLI, the
C++/Rust bindings, and the agent skills — is recorded here until the next Python
release picks it up._

### Added

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

[Unreleased]: https://github.com/lcjanke2020/sensorwatch/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/lcjanke2020/sensorwatch/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lcjanke2020/sensorwatch/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lcjanke2020/sensorwatch/releases/tag/v0.1.0

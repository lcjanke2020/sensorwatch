# Security Analysis: sensorwatch

**Date**: 2026-06-30 (agent-skill addendum 2026-07-04: adds the
`sensorwatch-monitor` skill — stdlib helper scripts that write only inside a
machine-local state dir; no new listener, input class, or privilege, so the
threat model is unchanged. See §4.)
**Scope**: Windows hardware sensor monitoring toolkit reading HWiNFO64 shared
memory today through a Python package and CLI, a native C core with Python (cffi),
header-only C++, and Rust bindings, and read-only agent skills; with a planned
localhost REST service.

**Methodology**: Code review of the current Python implementation plus
architectural analysis of planned components. Risk levels are calibrated to what
actually matters for a single-user desktop monitoring tool, not a multi-tenant
cloud service.

---

## Current and Planned Attack Surface

**Current implementation**:

- Python package and CLI, plus a native C core with Python (cffi), header-only
  C++, and Rust bindings.
- Reads HWiNFO64 shared memory through `sensorwatch.hwinfo_shm` (read-only Win32
  file-mapping APIs via `ctypes`), or through `sensorwatch.native` (a cffi
  API-mode binding over the native C core).
- Writes local JSON Lines log files.
- Reads those JSON Lines files back as **untrusted input** (the Rust CLI's
  rule-engine replay source, shared by `watch --replay` and the `report`
  history digest): line length is bounded, malformed lines are counted and
  skipped rather than trusted, and the parser is exercised against synthetic
  adversarial lines in the test suite. `report` adds no new listener,
  privilege, or input class — it is the same bounded replay parser behind a
  read-only, size-capped digest.
- Opens no network listeners.
- The binary wheel ships a compiled cffi extension (`sensorwatch._sw_cffi`) with
  the C core statically linked in; Python imports it from the package directory,
  not via a name-based DLL search. A standalone `sensorwatch.dll` is built by
  CMake for C/C++ consumers but is not loaded by the Python package.
- Two read-only agent skills. `skills/sensorwatch/` is documentation plus a
  `snapshot.py` helper that calls the existing read-only API.
  `skills/sensorwatch-monitor/` adds the monitoring protocol plus stdlib-only
  helper scripts that read the watcher's own JSON event/spool files (validated
  against the frozen 14-key contract) and write durable state **only inside a
  machine-local state directory** — the same local-file-write class the logger
  already has, no new input source, listener, or privilege. Neither skill
  controls hardware; both treat sensor strings as untrusted display data.

**Planned components covered by this threat model**:

- Optional localhost REST service. (sensorwatch does not plan a separate MCP
  server: local agents use the shipped skill over the CLI/API, and any future
  remote, over-a-protocol access would be served by this REST service — see §4.)

Sections for planned components are design requirements, not currently shipped
attack surface.

---

## Table of Contents

1. [Shared Memory Attack Surface](#1-shared-memory-attack-surface)
2. [DLL Security](#2-dll-security)
3. [REST Service Risks](#3-rest-service-risks-planned)
4. [Agent Integration Security](#4-agent-integration-security)
5. [Data Sensitivity](#5-data-sensitivity)
6. [Supply Chain and Build Security](#6-supply-chain-and-build-security)
7. [Privilege Escalation](#7-privilege-escalation)
8. [Implementation-Specific Findings](#8-implementation-specific-findings)
9. [Summary and Prioritized Recommendations](#9-summary-and-prioritized-recommendations)

---

## 1. Shared Memory Attack Surface

### 1.1 Spoofed Shared Memory Region

**Risk Level**: LOW (but worth mitigating)

**Threat**: If HWiNFO64 is not running, a malicious process running as the same
user (or any user with `SeCreateGlobalPrivilege`) could create a shared memory
object named `Global\HWiNFO_SENS_SM2` and populate it with crafted data. Our
reader would then parse attacker-controlled bytes.

**Why it's low**: An attacker who can run arbitrary code as the same user already
owns the session. They could modify log files directly, hook the Python process,
or read the same sensor data themselves. Spoofing the shared memory to feed us
bad data usually does not give them anything they do not already have.

**Where it could matter**: If sensor data is used for automated decision-making
(for example, "shut down if temperature exceeds X"), spoofed data could trigger
or suppress those actions. For a logging tool, the realistic impact is corrupted
logs.

**Mitigations**:

- Validate the magic number (`HEADER_MAGIC = 0x53695748`). Implemented.
- Sanity-check header fields before using them as offsets/counts. Implemented;
  see §1.3 and §8.1.
- Optional defense in depth: query whether `HWiNFO64.exe` is actually running
  before trusting the shared memory. This is not foolproof because process names
  can be spoofed, but it can reduce accidental confusion.
- Do not make safety-critical decisions based solely on sensor data without
  independent verification.

### 1.2 Concurrent Modification (TOCTOU)

**Risk Level**: LOW

**Threat**: HWiNFO64 updates the shared memory region while sensorwatch is
reading it. HWiNFO's shared-memory interface does not expose a synchronization
protocol, so a read can combine data from adjacent poll cycles.

**Reality**: Torn reads on individual aligned `double` values are unlikely on
x86-64, but semantically mixed snapshots are possible: a value from one poll
cycle may be paired with a name or aggregate from another. For logging, a single
odd sample is not harmful and should be visible as an outlier.

**Mitigations**:

- The current Python reader copies the mapped region once into an immutable
  `bytes` buffer before parsing, so iteration is over a self-contained snapshot
  rather than live shared memory.
- Optional defense in depth: read `poll_time`/timestamp-like header fields before
  and after the copy if a future native implementation can do so cheaply; discard
  and retry if the producer changed data mid-read.
- Do not use instantaneous readings for safety-critical decisions without
  rate-based filtering or multi-sample confirmation.

### 1.3 Malformed Struct Data / Memory Corruption

**Risk Level**: MEDIUM -- this is the most important shared-memory risk.

**Threat**: Crafted or corrupted header data could cause a reader to:

- Read out of bounds because `offset + count * size` exceeds the mapped region.
- Spend excessive CPU/memory on unreasonable counts.
- Interpret arbitrary bytes as strings.
- Crash if native/ctypes code dereferences an invalid address.

**Current status**: Mitigated in the Python implementation.

`read_sensors()` opens `Global\HWiNFO_SENS_SM2` read-only. `_read_from_mapped()`
uses `VirtualQuery` to determine the mapped region size, caps the copied size at
`MAX_TOTAL_SIZE`, copies the region into an immutable `bytes` object once, and
then delegates to `_parse_shared_memory()`.

`_parse_shared_memory()` treats all header fields as untrusted input and validates
before parsing entries:

- minimum header size;
- magic number;
- minimum sensor/entry element sizes;
- maximum sensor/entry counts (`MAX_SENSOR_COUNT`, `MAX_ENTRY_COUNT`);
- section offsets not overlapping the header;
- computed section ends within the copied buffer;
- sensor and entry sections not overlapping each other.

Residual out-of-bounds parser reads are caught as `struct.error`, logged, and
reported as `None` rather than crashing the process.

**Requirements for future native code**:

- Keep the same copy-then-parse model; never expose raw shared-memory pointers
  across the ABI.
- Query or otherwise bound the mapped region size before copying.
- Treat the header as a packed wire layout: `last_update` is an `int64_t` at byte
  `0x0C` and is not 8-byte aligned. Read scalar fields by explicit byte offset
  (copying into a correctly-typed local) rather than overlaying a naturally-aligned
  C struct, which both mis-parses every field after the gap -- corrupting the very
  bounds the checks below rely on -- and risks unaligned-access UB that UBSan and
  strict-alignment targets reject. See `docs/C_CODING_STANDARDS.md`.
- Check size/count/offset multiplication using `size_t` and overflow-aware helper
  functions.
- Apply explicit maximums for total mapped size, sensor count, and entry count.
- Return explicit error codes for malformed data instead of conflating corruption
  with source unavailability.
- Run parser tests under sanitizers and fuzz malformed binary inputs.

### 1.4 Shared Memory Namespace Squatting

**Risk Level**: LOW

**Threat**: A non-admin process could potentially create a `Local\HWiNFO_SENS_SM2`
object that shadows the global one in certain contexts.

**Mitigation**: The current code explicitly uses the `Global\` prefix, which is
correct for HWiNFO's documented mapping.

---

## 2. DLL Security

The native C core is built from source (a Windows DLL plus a static library; see
"Building the native core" in the README). The shipped Python binding does **not**
load a separate DLL — it statically links the core into the `sensorwatch._sw_cffi`
extension (see §2.2). This section applies to the standalone `sensorwatch.dll` and
to any consumer, current or future, that loads it by name.

### 2.1 DLL Search Order Hijacking

**Risk Level**: MEDIUM for the standalone C DLL; not applicable to the shipped
Python binding

**Threat**: When an FFI caller loads a DLL by name rather than by full path,
Windows searches multiple directories. An attacker who can place a malicious DLL
in a searched directory can execute code in the caller process.

**The shipped Python and Rust bindings avoid this by construction**:
`sensorwatch.native` compiles the C core into its own extension module
(`SW_STATIC`), and the Rust `sensorwatch-sys` crate's `build.rs` compiles it into
the crate the same way — so neither loads a separate DLL and there is no name-based
search to hijack. The mitigations below apply to C/C++ consumers of the standalone
CMake-built `sensorwatch.dll`.

**Mitigations**:

- Always load by absolute path. In Python, resolve the DLL relative to the package
  installation and use that full path with `ctypes.CDLL`/`WinDLL`.
- Never load the sensorwatch DLL from the current working directory or an
  untrusted `PATH` entry.
- If the DLL loads dependencies, use `LoadLibraryEx`/safe DLL search settings so
  dependent libraries resolve from expected locations.
- Keep the native core dependency-free beyond the C runtime and Windows SDK.
- If distributing pre-built binaries, publish provenance and checksums; consider
  signing if the project reaches a distribution scale where that is practical.

### 2.2 Python DLL Use

**Risk Level**: LOW

The pure-Python reader (`sensorwatch.hwinfo_shm`) uses `kernel32` Win32 APIs
through `ctypes` and loads no third-party DLLs. The native binding
(`sensorwatch.native`) statically links the C core into the `sensorwatch._sw_cffi`
extension, which Python imports from the package directory; it likewise loads no
third-party DLL and performs no name-based DLL search. The search-order risk in
§2.1 therefore applies only to a consumer that loads the standalone
`sensorwatch.dll`.

---

## 3. REST Service Risks (planned)

The current implementation opens no network listener. This section applies only
if an optional REST service is added later.

### 3.1 Binding Address

**Risk Level**: HIGH if bound to `0.0.0.0`; LOW if bound to `127.0.0.1`.

**Threat**: Binding to `0.0.0.0` exposes sensor data to the local network. On
public Wi-Fi or shared LANs, that is an unnecessary information leak.

**Mitigation**: Bind to `127.0.0.1` by default. Network exposure must be explicit
opt-in configuration with a clear warning.

### 3.2 Browser-Origin Attacks

**Risk Level**: MEDIUM

**Threat**: A malicious web page could try to query `http://localhost:<port>` via
browser JavaScript. Same-origin policy helps, but CORS mistakes or DNS rebinding
can expose localhost services.

**Mitigations**:

- Do not emit permissive CORS headers by default.
- Validate the `Host` header and accept only loopback hosts such as `localhost`,
  `127.0.0.1`, and `[::1]`.
- If an API key is added, require it in a custom header rather than a query
  parameter. Browser simple cross-origin requests cannot send custom headers
  without a preflight, which should fail when CORS is not enabled.
- Keep the REST API read-only.

### 3.3 API Key Design

**Risk Level**: MEDIUM if a REST API grows beyond trivial local read-only access

**Recommendations**:

- Generate a cryptographically random key on first run if the feature is enabled.
- Store it in a per-user configuration location with owner-only permissions.
- Compare with constant-time comparison.
- Do not log the key.
- Treat the API key as defense in depth. The primary protections are loopback
  binding, Host validation, no CORS, and read-only endpoints.

### 3.4 Rate Limiting and Multi-User Machines

**Risk Level**: LOW

Basic in-memory rate limiting can protect the service from accidental local
request floods. On multi-user Windows systems, a per-user API key and per-user
port selection are reasonable best-effort mitigations, but sensorwatch should be
honest that it is designed primarily as a single-user desktop utility.

---

## 4. Agent Integration Security

sensorwatch's agent interface is the shipped read-only agent skills:
`skills/sensorwatch/` (guidance plus a `snapshot.py` helper over the existing
read-only CLI/API) and `skills/sensorwatch-monitor/` (the monitoring protocol
plus stdlib helper scripts that write durable state only inside a machine-local
state directory). Neither adds a network surface, and neither controls hardware —
escalation to a human *is* the action. There is deliberately no separate MCP
server: local agents use the skills, and any future remote, over-a-protocol
access would be served by the planned localhost REST service (§3), whose own
threat model then applies. The requirements below govern both shipped skills and
any agent-facing surface layered on top later.

### 4.1 Prompt Injection via Sensor Data

**Risk Level**: LOW (but worth designing around)

**Threat**: HWiNFO sensor names and user-defined labels are untrusted display
strings. If later passed to an AI agent, a spoofed sensor name could contain text
that looks like instructions.

**Mitigations**:

- Treat all sensor strings as untrusted display data.
- The current `_decode()` helper decodes HWiNFO strings as cp1252 with replacement
  and strips C0/C1 control characters. Future native code should emit sanitized
  UTF-8 at the ABI boundary.
- Agent-facing integrations should use structured output and explicitly tell the
  agent that sensor data is data, not instructions.
- Keep agent integration read-only: query, format, return. No write APIs.

### 4.2 Data Exfiltration via Agent

**Risk Level**: LOW

Sensor data is less sensitive than most files an authorized local coding agent can
already read. The agent integration should not add new network or write
capabilities; it should simply expose structured sensor readings.

### 4.3 Unintended Actions from Misinterpreted Queries

**Risk Level**: LOW

The strongest mitigation is architectural: no write API. A request like "what is
the CPU temperature?" should only read and format sensor state.

---

## 5. Data Sensitivity

### 5.1 Are Sensor Readings PII?

**Risk Level**: LOW

Hardware sensor readings (temperature, voltage, current, fan speed, clocks, and
usage) are not personally identifiable information under normal definitions. They
identify hardware state, not a person.

### 5.2 Side-Channel: Power Draw Revealing Activity

**Risk Level**: LOW (theoretical/academic)

Fine-grained power data can theoretically reveal usage patterns or workload type.
sensorwatch's default 10-second cadence and typical PSU-level readings are far
too coarse for practical cryptographic side-channel attacks. The realistic concern
is that shared logs include timestamps and can reveal when a machine was on.

**Mitigations**:

- Document that logs contain timestamps.
- Consider a future log-sanitization helper for bug reports.
- Do not log usernames, hostnames, IP addresses, process lists, or other machine
  context. Logs should contain timestamps and sensor readings only.

### 5.3 Hardware Fingerprinting

**Risk Level**: LOW (theoretical)

The set of sensor names and values can roughly fingerprint a hardware
configuration. This is comparable to a browser user-agent string in practical
risk. Users who want to share data anonymously should strip or generalize sensor
names before publishing logs.

---

## 6. Supply Chain and Build Security

### 6.1 Dependency Risk

**Risk Level**: LOW currently; MEDIUM for future compiled components

**Current state**:

- `sensorwatch.hwinfo_shm` uses Python stdlib modules (`ctypes`, `struct`, etc.).
- `sensorwatch.logger` uses `pendulum`, currently the only runtime third-party
  dependency.

**Concerns**:

- `pendulum` is well-known, but it includes native code in some distributions.
  Replacing it with stdlib `datetime` would simplify the trust chain.
- Future C/Rust/native artifacts increase supply-chain and build provenance
  requirements.

**Mitigations**:

- Keep runtime dependencies minimal.
- Keep `uv.lock` committed and review dependency changes.
- For future Rust components, use `cargo audit`/`cargo deny` and commit
  `Cargo.lock`.
- For future C components, avoid runtime dependencies beyond system libraries;
  keep test-only tools isolated from shipped artifacts.
- For native releases, publish build logs, checksums, and available artifact
  attestations.

### 6.2 Malicious Pull Requests

**Risk Level**: LOW if standard maintainer review is enforced

Users of a hardware monitoring tool may run high-value workstations, so even a
small project should treat supply-chain changes carefully.

**Mitigations**:

- Require maintainer review before merge.
- Run CI for tests and packaging checks.
- For compiled code, add sanitizer, static-analysis, and fuzzing gates before
  shipping native artifacts.
- Do not grant commit access to untrusted contributors.
- Keep core dependencies small.

### 6.3 Build Reproducibility

**Risk Level**: LOW currently

The Python package now ships compiled binary wheels (the cffi extension). Wheels
are built in CI with cibuildwheel and published via GitHub Actions OIDC trusted
publishing with PEP 740 attestations covering every uploaded file (wheels and the
sdist); `uv.lock` pins the build toolchain. The sdist stays buildable from source
for platforms without a prebuilt wheel.

The Rust crates (`sensorwatch-sys` + `sensorwatch`) publish to crates.io the same
tokenless way — GitHub Actions OIDC trusted publishing (`rust-lang/crates-io-auth-action`),
no stored `CARGO_REGISTRY_TOKEN`, gated on a reviewed `rust-v*` release and the
fmt/clippy/test suite. `sensorwatch-sys` vendors the C core (`vendor/`, a CI-enforced
verbatim mirror of `src/` + `include/`) so consumers build the same audited sources
from crates.io; `Cargo.lock` is committed. crates.io has no built-in PEP 740 / SLSA
attestation equivalent — build provenance (e.g. `cargo-auditable`, GitHub artifact
attestation) is a possible future addition, not yet wired.

---

## 7. Privilege Escalation

### 7.1 Reading Admin-Created Shared Memory

**Risk Level**: NONE

`OpenFileMappingW` with `FILE_MAP_READ` opens an existing kernel object with
read-only access. HWiNFO's DACL controls who can open it. Reading shared memory
does not grant elevated privileges.

### 7.2 Token/Handle Leaks

**Risk Level**: LOW

Leaking a mapping handle or view pointer wastes resources but does not provide a
privilege-escalation path. The current `read_sensors()` closes handles in a
`finally` block.

### 7.3 Could the Tool Be Used as a Privilege Escalation Vector?

**Risk Level**: NONE in the current implementation

The tool reads sensor data and writes log files. It does not run child processes,
accept commands, modify system state, communicate with privileged services, or
open network listeners.

---

## 8. Implementation-Specific Findings

### 8.1 Unbounded Shared Memory Reads

**Priority**: Previously HIGH; **Status**: Implemented / mitigated

Earlier versions needed stronger bounds validation before using shared-memory
header counts and offsets. The current implementation now mitigates this in
`_read_from_mapped()` and `_parse_shared_memory()`:

- `VirtualQuery` obtains the mapped region size.
- The region is copied once into immutable `bytes`, capped by `MAX_TOTAL_SIZE`.
- Header magic, element sizes, counts, section offsets, computed ends, and section
  overlap are validated before entry parsing.
- Parsing uses `struct.unpack_from` against the copied buffer.
- Malformed data is logged and returns `None` instead of crashing.

Tests in `tests/test_hwinfo_shm.py` cover valid synthetic buffers, bad magic,
truncated buffers, too-small element sizes, excessive counts, header-overlapping
sections, out-of-region sections, overlapping sections, invalid sensor indices,
blank user-name fallback, cp1252 decoding, and control-character stripping.

### 8.2 String Decoding from Untrusted Memory

**Priority**: Previously LOW; **Status**: Implemented / mitigated

`_decode()` treats HWiNFO string fields as fixed-width, null-terminated cp1252
byte arrays, decodes with replacement, and strips C0/C1 control characters.

Future native code should preserve the same security property while making the
public ABI more explicit: input strings are untrusted source data; output strings
crossing the C ABI should be sanitized UTF-8 and should never be interpreted as
commands or instructions.

### 8.3 Consider Replacing `pendulum` with `datetime`

**Priority**: LOW; **Status**: Open

`pendulum` is the only runtime third-party dependency. The logger's date/time
uses could likely be replaced with stdlib `datetime`, reducing supply-chain risk
and simplifying installation.

### 8.4 Log Directory Permissions

**Priority**: LOW; **Status**: Open / documented risk

The logger creates the configured log directory with inherited permissions. This
is appropriate for the default local `logs/` directory. If users configure a
shared directory, they are responsible for its permissions. A future hardening
change could warn when the log directory appears broadly writable/readable.

---

## 9. Summary and Prioritized Recommendations

### Current Implementation

| # | Finding | Section | Status |
|---|---------|---------|--------|
| 1 | Validate shared-memory counts, sizes, offsets, and bounds | 1.3, 8.1 | Done |
| 2 | Copy mapped region to immutable bytes before parsing | 1.2, 1.3, 8.1 | Done |
| 3 | Strip control characters from shared-memory strings | 4.1, 8.2 | Done |
| 4 | Keep shared-memory input untrusted in tests and docs | 1.3, 8.1 | Done / ongoing |
| 5 | Consider replacing `pendulum` with stdlib `datetime` | 6.1, 8.3 | Open |
| 6 | Document or check log-directory privacy expectations | 5.2, 8.4 | Open |

### Native C ABI / DLL and Python/C++/Rust bindings

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Avoid name-based DLL search in the Python and Rust bindings (static-link the core into the extension/crate) | 2.1, 2.2 | Done (cffi + Rust `-sys`) |
| 2 | Keep native core runtime dependency-free beyond system libraries | 2.1, 6.1 | Done |
| 3 | Preserve copy-then-parse model; expose immutable snapshots, not raw pointers | 1.3 | Done |
| 4 | Return explicit error codes for source unavailable vs corrupt data | 1.3 | Done |
| 5 | Run native parser tests under sanitizers and fuzzing | 1.3, 6.2 | ASan/UBSan done; fuzzing planned |
| 6 | Keep the C++ binding header-only (no compiled artifact, no ABI of its own) over the same `extern "C"` boundary | 2.1 | Done |
| 7 | Keep the Rust `-sys` FFI auditable: checked-in bindgen output with a CI drift check, no libclang at build time | 2.1 | Done |

### Planned REST Service

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Bind to `127.0.0.1` by default | 3.1 | Planned |
| 2 | Validate Host header | 3.2 | Planned |
| 3 | Do not emit permissive CORS headers by default | 3.2 | Planned |
| 4 | Keep endpoints read-only | 3.2 | Planned |
| 5 | Use custom-header API key only if needed | 3.3 | Planned |

### Agent Integration (skills shipped)

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Treat sensor strings as untrusted display data | 4.1 | Done (both skills) / ongoing |
| 2 | Use structured output for agent-facing data | 4.1 | Done (both skills) |
| 3 | Keep agent integration read-only (no hardware control; monitor writes only its state dir) | 4.1, 4.3 | Done (both skills) |

### Not Worth Doing for This Project

- Encrypting localhost REST traffic with TLS by default. This adds certificate
  complexity but does not meaningfully improve the single-user desktop threat
  model.
- Obfuscating sensor data. If a local process can query sensorwatch, it can
  likely query HWiNFO directly.
- Power-draw side-channel mitigations beyond documentation. At the default
  polling cadence and sensor granularity, this is not a practical attack.
- Multi-factor authentication for a localhost sensor reader.

---

## Threat Model Summary

| Attacker | Capability | Realistic? | Impact |
|----------|------------|------------|--------|
| Remote network attacker | Cannot reach current tool; future REST should bind loopback | Yes | None if defaults are correct |
| Malicious web page | Browser JavaScript can target localhost | Yes for future REST | Sensor data leak if Host/CORS are wrong |
| Local unprivileged process | Can spoof shared memory or read logs in accessible locations | Yes | Low; corrupted logs or local data access |
| Local admin/SYSTEM process | Full control | Yes, but out of scope | Game over |
| Malicious dependency | Code execution in process | Possible | High; mitigate by minimizing dependencies |
| Agent prompt injection | Crafted sensor names as data | Requires local control/spoofing | Very low; mitigate with sanitization and structured data |

**Bottom line**: sensorwatch is a single-user desktop tool reading read-only
hardware data. The most important practical mitigations are: (1) do not crash on
malformed shared memory, (2) keep future REST strictly local and browser-hardened,
and (3) minimize dependencies and native build complexity.

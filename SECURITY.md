# Security Analysis: sensorwatch

**Date**: 2026-06-29
**Scope**: Windows hardware sensor monitoring toolkit reading HWiNFO64 shared
memory today, with planned native C ABI/DLL, language bindings, localhost REST
service, CLI, and AI agent integration.

**Methodology**: Code review of the current Python implementation plus
architectural analysis of planned components. Risk levels are calibrated to what
actually matters for a single-user desktop monitoring tool, not a multi-tenant
cloud service.

---

## Current and Planned Attack Surface

**Current implementation**:

- Python package and CLI only.
- Reads HWiNFO64 shared memory through `sensorwatch.hwinfo_shm` using read-only
  Win32 file-mapping APIs via `ctypes`.
- Writes local JSON Lines log files.
- Opens no network listeners.
- Ships no native DLL/shared library.

**Planned components covered by this threat model**:

- Native C ABI / Windows DLL for language bindings.
- Python, C++, Rust, and other bindings over that ABI.
- Optional localhost REST service.
- Read-only agent integration through an MCP/skill layer.

Sections for planned components are design requirements, not currently shipped
attack surface.

---

## Table of Contents

1. [Shared Memory Attack Surface](#1-shared-memory-attack-surface)
2. [DLL Security](#2-dll-security-planned)
3. [REST Service Risks](#3-rest-service-risks-planned)
4. [Agent Integration Security](#4-agent-integration-security-planned)
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

## 2. DLL Security (planned)

The current package does not ship a DLL. This section applies to the planned
native C ABI / Windows DLL and to language bindings that load it.

### 2.1 DLL Search Order Hijacking

**Risk Level**: MEDIUM for the planned C DLL

**Threat**: When an FFI caller loads a DLL by name rather than by full path,
Windows searches multiple directories. An attacker who can place a malicious DLL
in a searched directory can execute code in the caller process.

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

### 2.2 Current Python DLL Use

**Risk Level**: LOW

The current Python implementation uses `kernel32` Win32 APIs through `ctypes` and
does not load third-party DLLs. Native DLL loading risks apply only after a
sensorwatch DLL exists.

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

## 4. Agent Integration Security (planned)

The current package does not ship an MCP server or agent skill. This section
applies to planned read-only agent integration.

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

For the Python package, source distribution and GitHub Actions trusted publishing
are sufficient for this project's scale. For future compiled components, provide
repeatable build instructions, build logs, checksums, and CI provenance.

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

### Planned Native C ABI / DLL

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Load DLLs by absolute path in bindings | 2.1 | Planned |
| 2 | Keep native core runtime dependency-free beyond system libraries | 2.1, 6.1 | Planned |
| 3 | Preserve copy-then-parse model; expose immutable snapshots, not raw pointers | 1.3 | Planned |
| 4 | Return explicit error codes for source unavailable vs corrupt data | 1.3 | Planned |
| 5 | Run native parser tests under sanitizers and fuzzing | 1.3, 6.2 | Planned |

### Planned REST Service

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Bind to `127.0.0.1` by default | 3.1 | Planned |
| 2 | Validate Host header | 3.2 | Planned |
| 3 | Do not emit permissive CORS headers by default | 3.2 | Planned |
| 4 | Keep endpoints read-only | 3.2 | Planned |
| 5 | Use custom-header API key only if needed | 3.3 | Planned |

### Planned Agent Integration

| # | Requirement | Section | Status |
|---|-------------|---------|--------|
| 1 | Treat sensor strings as untrusted display data | 4.1 | Planned / ongoing |
| 2 | Use structured output for agent-facing data | 4.1 | Planned |
| 3 | Keep agent integration read-only | 4.1, 4.3 | Planned |

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
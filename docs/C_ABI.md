# sensorwatch C ABI

**Status**: Implemented (ABI draft `0.1.0`). A native core now implements this
header: a Windows DLL plus a static library built with CMake, with cmocka unit
tests and an AddressSanitizer/UBSan gate (see "Building the native core" in the
[README](../README.md)). The ABI itself is still a **pre-1.0 draft** and may change
during review until the first release carries a stability commitment (see
[Evolution Policy](#evolution-policy)). A Python binding (cffi, API mode) now ships
over this ABI — see [Binding Notes → Python](#python); C++ and Rust bindings are
not provided yet.

This document defines the stable C ABI for the native sensorwatch core. The ABI is
designed to be wrapped by Python, C++, Rust, and other languages without exposing
Windows handles, HWiNFO raw structs, or raw shared-memory pointers.

The header lives in [`include/sensorwatch/sensorwatch.h`](../include/sensorwatch/sensorwatch.h)
and the implementation in [`src/`](../src). Implementation follows
[`docs/C_CODING_STANDARDS.md`](C_CODING_STANDARDS.md) and the security requirements
in [`SECURITY.md`](../SECURITY.md).

---

## Goals

- Provide one small, source-neutral C ABI for hardware sensor snapshots.
- Keep public handles opaque so internal layouts can evolve without breaking
  bindings.
- Return explicit error codes from fallible functions.
- Keep snapshots immutable and self-contained.
- Return public strings as sanitized UTF-8 through caller-owned buffers.
- Preserve enough reading metadata for stable binding behavior and future sensor
  identity work.
- Avoid committing to config parsing, logging, REST, or agent behavior in the
  native core.

## Non-goals

- No replacement of the current Python HWiNFO parser yet.
- No C++ or Rust bindings yet (a Python cffi binding now ships over the ABI).
- No prebuilt standalone DLL distribution or code signing yet (the Python binding
  ships as binary wheels that statically link the core into the extension).
- No fuzzing harness yet (planned; the parser is already under ASan/UBSan).
- No public exposure of HWiNFO shared-memory layout structs.
- No filesystem logging API in the core ABI.
- No network or REST API in the core ABI.
- No safety-critical control logic.

---

## Naming, Linkage, and Versioning

The public prefix is `sw_` / `SW_` rather than `hwi_`. HWiNFO is the first source,
but the roadmap includes UPS, AIDA64, IPMI, and other adapters. Public symbol names
should describe sensorwatch's source-neutral contract rather than the initial
backend.

The draft header uses ABI version `0.1.0`:

```c
#define SW_API_VERSION_MAJOR 0u
#define SW_API_VERSION_MINOR 1u
#define SW_API_VERSION_PATCH 0u
#define SW_API_VERSION \
    ((SW_API_VERSION_MAJOR * 10000u) + \
     (SW_API_VERSION_MINOR * 100u) + \
     SW_API_VERSION_PATCH)

uint32_t sw_api_version(void);
```

ABI major version `1` should begin only when the first implemented native library
is released with a compatibility commitment. Until then, the ABI may change during
review.

Public headers must be valid from C and C++:

- use `extern "C"` for C++ consumers;
- pin the C calling convention with an explicit `SW_CALL` macro (`__cdecl` on
  Windows, empty elsewhere) on every exported function, so the ABI stays stable
  even if a consumer's compiler default differs (e.g. MSVC `/Gz`); ABI functions
  are never `__stdcall` or `__fastcall`;
- pin the width of the public enums crossing the ABI (`sw_error_t`,
  `sw_reading_type_t`) with a `static_assert(sizeof(...) == 4)`, so the
  implementation-defined enum size cannot silently drift (e.g. under
  `-fshort-enums`) and break bindings that treat them as 32-bit;
- avoid Windows headers in the public ABI;
- use an export macro that supports DLL, static, and non-Windows builds.

The `SW_API` export macro is driven by two consumer-defined macros. Define
`SW_STATIC` when building or linking sensorwatch as a static library — `SW_API`
then expands to nothing (no import/export decoration). The shared-library build
itself defines `SW_BUILD_DLL` so `SW_API` exports symbols; a default Windows
consumer that defines neither imports them (`__declspec(dllimport)`), and on
non-Windows toolchains `SW_API` carries default ELF visibility.

---

## Supported Toolchains

The public header is written to compile cleanly as both C and C++ across MSVC,
Clang, and GCC, and its compile-time guards degrade safely on older language
modes (C99+/C++98+). That breadth is a convenience for binding authors, not an
open-ended support promise. The native core targets:

- **MSVC** (`cl` / `clang-cl`) on Windows — the primary, first-class toolchain,
  matching the Windows user-mode DLL the core ships as.
- **Clang and GCC**, including under MinGW — secondary toolchains for local
  development, bindings, and CI cross-checks.

Exotic compilers, ABI-altering build flags (e.g. `-fshort-enums`), and pre-C99 /
pre-C++98 language modes are out of scope: the header's `static_assert`s exist to
*fail loudly* under such configurations rather than to promise support for them.
Build bindings against one of the supported toolchains.

---

## Error Model

Every fallible public function returns `sw_error_t`. `SW_OK` is zero; errors are
negative. Callers should not use `errno` or `GetLastError()` as the ABI error
channel.

Initial error set:

| Error | Meaning |
|---|---|
| `SW_OK` | Success |
| `SW_ERR_NULL_POINTER` | Required pointer argument was null |
| `SW_ERR_INVALID_ARGUMENT` | Non-null argument was invalid |
| `SW_ERR_UNSUPPORTED_PLATFORM` | Backend is unavailable on this platform |
| `SW_ERR_SOURCE_UNAVAILABLE` | Sensor source is not running or not enabled |
| `SW_ERR_MAP_FAILED` | Source was found but could not be mapped/read |
| `SW_ERR_BAD_MAGIC` | Shared-memory magic/version marker did not match |
| `SW_ERR_CORRUPT_DATA` | Source data failed structural validation |
| `SW_ERR_OUT_OF_MEMORY` | Allocation failed |
| `SW_ERR_INDEX_OUT_OF_RANGE` | Snapshot index was outside entry count |
| `SW_ERR_BUFFER_TOO_SMALL` | Caller buffer was too small; required size is reported when possible |
| `SW_ERR_VERSION_MISMATCH` | Caller/library ABI expectations are incompatible |
| `SW_ERR_INTERNAL` | Unexpected library bug or invariant failure |

`sw_error_string(sw_error_t error)` returns static library-owned storage and must
never return `NULL`.

Bindings should translate these errors into the host language's normal exception
or result type while preserving the original `sw_error_t` value for diagnostics.

---

## Handles and Ownership

The ABI exposes two opaque handles:

```c
typedef struct sw_session sw_session_t;
typedef struct sw_snapshot sw_snapshot_t;
```

A session represents access to the configured sensor source. A snapshot is an
immutable, owned copy of one read of the source.

Lifecycle functions:

```c
sw_error_t sw_session_open(sw_session_t **out_session);
void sw_session_close(sw_session_t *session);

sw_error_t sw_snapshot_take(sw_session_t *session,
                            sw_snapshot_t **out_snapshot);
void sw_snapshot_free(sw_snapshot_t *snapshot);
```

Ownership rules:

- On success, `sw_session_open()` stores a non-null session in `*out_session`.
- On failure, `sw_session_open()` stores `NULL` in `*out_session` when possible.
- `sw_session_close(NULL)` is a no-op.
- On success, `sw_snapshot_take()` stores a non-null snapshot in `*out_snapshot`.
- On failure, `sw_snapshot_take()` stores `NULL` in `*out_snapshot` when possible.
- `sw_snapshot_free(NULL)` is a no-op.
- Handles must not be stack-allocated or freed by callers.

Future APIs that accept structs across the ABI must include `struct_size` as the
first field and be append-only within an ABI major version.

---

## Snapshot Model

The native implementation should preserve the current Python parser's security
shape:

1. Open the source read-only.
2. Determine or bound the mapped/readable size.
3. Copy source bytes into owned memory.
4. Validate header magic, sizes, counts, offsets, section bounds, and section
   overlap before parsing entries.
5. Parse entries into snapshot-owned data.
6. Expose only read-only query functions.

No raw shared-memory pointer, Win32 `HANDLE`, HWiNFO struct pointer, or internal
array pointer crosses the ABI.

Base query API:

```c
sw_error_t sw_snapshot_entry_count(const sw_snapshot_t *snapshot,
                                   uint32_t *out_count);

sw_error_t sw_snapshot_get_reading_type(const sw_snapshot_t *snapshot,
                                        uint32_t index,
                                        sw_reading_type_t *out_type);

sw_error_t sw_snapshot_get_value(const sw_snapshot_t *snapshot,
                                 uint32_t index,
                                 double *out_value);
```

Numeric aggregate accessors expose the same values currently returned by
`SensorReading`: current, minimum, maximum, and average.

---

## String and Buffer Conventions

Source strings are untrusted display data. HWiNFO currently stores fixed-width
cp1252 byte fields; future sources may use different encodings. The public ABI
normalizes strings to sanitized UTF-8.

String functions use caller-owned buffers:

```c
sw_error_t sw_snapshot_get_sensor_name(const sw_snapshot_t *snapshot,
                                       uint32_t index,
                                       char *buffer,
                                       size_t buffer_size,
                                       size_t *out_required);
```

Rules (`buffer_size` is the size of `buffer` in bytes):

- A `NULL` `snapshot` returns `SW_ERR_NULL_POINTER`; an out-of-range `index`
  returns `SW_ERR_INDEX_OUT_OF_RANGE`.
- **Length query** — `buffer == NULL && buffer_size == 0`. `out_required` **must**
  be non-`NULL`; the function stores the required byte count, including the
  terminating NUL (always `>= 1`), in `*out_required` and returns
  `SW_ERR_BUFFER_TOO_SMALL`. If `out_required == NULL` in this mode the size
  cannot be returned, so the call returns `SW_ERR_NULL_POINTER`.
- **Copy** — `buffer != NULL && buffer_size > 0`. If the value plus its NUL fits,
  it is copied, NUL-terminated, `*out_required` (when non-`NULL`) is set to the
  bytes written including the NUL, and the call returns `SW_OK`. If it does not
  fit, `buffer` is left as an empty NUL-terminated string (never a partial UTF-8
  sequence), `*out_required` (when non-`NULL`) is set to the full required size,
  and the call returns `SW_ERR_BUFFER_TOO_SMALL`.
- **Any other `(buffer, buffer_size)` combination** — `buffer == NULL` with
  `buffer_size > 0`, or `buffer != NULL` with `buffer_size == 0` — returns
  `SW_ERR_INVALID_ARGUMENT`.
- `out_required` may be `NULL` only in the copy form; it is required for length
  queries. Whenever `buffer != NULL && buffer_size > 0`, `buffer` is always
  NUL-terminated on return.
- Strings are sanitized UTF-8 display data — no C0/C1 control characters — and are
  display data, not commands or instructions.

Initial string accessors:

- `sw_snapshot_get_source_name()`
- `sw_snapshot_get_sensor_name()`
- `sw_snapshot_get_reading_name()`
- `sw_snapshot_get_unit()`

`sw_snapshot_get_sensor_name()`, `sw_snapshot_get_reading_name()`, and
`sw_snapshot_get_unit()` map onto the existing `SensorReading` fields
(`sensor_name`, `reading_name`, `unit`). `sw_snapshot_get_source_name()` is a
net-new, source-neutral concept (the backend/source identity, e.g. HWiNFO) with
no analog in today's single-source Python `SensorReading`; it is included now so
the multi-source roadmap does not require an ABI break later.

---

## Data Model

The public reading-type enum tracks the categories the current Python parser
exposes through `sensorwatch.hwinfo_shm.SENSOR_TYPES`. Values `1..8` map 1:1
(Temperature, Voltage, Fan, Current, Power, Clock, Usage, Other).
`SW_READING_TYPE_NONE` mirrors the source's explicit "None" category (HWiNFO
type `0`). A source type code outside the known range is reported as
`SW_READING_TYPE_UNKNOWN` — not silently folded into `NONE` or `OTHER` — so a
binding can distinguish "the source reported no specific type" from "this ABI
version does not recognize the source's category" (the Python reference renders
the latter as `unknown(N)`):

```c
typedef enum sw_reading_type {
    SW_READING_TYPE_NONE        = 0,   /* source "None" category (HWiNFO type 0) */
    SW_READING_TYPE_TEMPERATURE = 1,
    SW_READING_TYPE_VOLTAGE     = 2,
    SW_READING_TYPE_FAN         = 3,
    SW_READING_TYPE_CURRENT     = 4,
    SW_READING_TYPE_POWER       = 5,
    SW_READING_TYPE_CLOCK       = 6,
    SW_READING_TYPE_USAGE       = 7,
    SW_READING_TYPE_OTHER       = 8,
    SW_READING_TYPE_UNKNOWN     = 255  /* source code outside the known 0..8 range */
} sw_reading_type_t;
```

The ABI intentionally starts with accessors rather than a public reading struct.
That is more verbose for C callers, but it is friendlier to long-term ABI
stability because the library can add internal fields without changing public
struct layout.

Potential future additive APIs:

- stable source-neutral reading IDs;
- backend/source enumeration;
- source-specific HWiNFO IDs and instances;
- a diagnostic accessor for the raw source-specific type code behind
  `SW_READING_TYPE_UNKNOWN` (the Python reference keeps it as `unknown(N)`);
- quality flags;
- snapshot timestamps or producer poll counters;
- filtered snapshot creation.

These should be added without breaking the base snapshot/query functions.

---

## Thread Safety

- `sw_api_version()` and `sw_error_string()` are thread-safe.
- Different sessions may be used concurrently from different threads.
- The same session is session-bound: callers must synchronize concurrent use of
  one `sw_session_t`.
- Snapshots are immutable after creation. Query functions may be called
  concurrently on the same live snapshot.
- `sw_session_close()` and `sw_snapshot_free()` must not race with any other use
  of the same handle.

This contract keeps the native core simple and avoids hidden locks while still
allowing bindings to share completed snapshots safely.

---

## Security Requirements

Any implementation of this ABI must follow the threat model in `SECURITY.md`:

- Open HWiNFO shared memory read-only.
- Treat all source bytes and strings as untrusted input.
- Validate bounds before parsing.
- Cap memory use and entry counts.
- Avoid raw pointers across the ABI.
- Emit sanitized UTF-8 strings.
- Separate source-unavailable errors from corrupt-data errors.
- Keep the native core read-only and network-free.
- Do not make safety-critical decisions from sensor data.
- Add parser unit tests, sanitizer coverage, and fuzzing before shipping a native
  parser.

---

## Example C Usage

```c
#include "sensorwatch/sensorwatch.h"

#include <stdio.h>
#include <stdlib.h>

int main(void)
{
    sw_session_t *session = NULL;
    sw_snapshot_t *snapshot = NULL;
    uint32_t count = 0;

    sw_error_t err = sw_session_open(&session);
    if (err != SW_OK) {
        fprintf(stderr, "open failed: %s\n", sw_error_string(err));
        return 1;
    }

    err = sw_snapshot_take(session, &snapshot);
    if (err != SW_OK) {
        fprintf(stderr, "snapshot failed: %s\n", sw_error_string(err));
        sw_session_close(session);
        return 1;
    }

    err = sw_snapshot_entry_count(snapshot, &count);
    if (err == SW_OK) {
        printf("%u readings\n", (unsigned)count);
    }

    sw_snapshot_free(snapshot);
    sw_session_close(session);
    return err == SW_OK ? 0 : 1;
}
```

---

## Binding Notes

### Python

The shipped Python binding (`sensorwatch.native`) uses **cffi in API mode**: it
compiles the C sources in `src/` directly into the extension module
`sensorwatch._sw_cffi` (with `SW_STATIC`, so `SW_API` expands to nothing), rather
than loading a separate DLL. Compiling the stub against the real header makes
signature drift a build error, and linking the core into the extension sidesteps
the DLL search-order risk in [`SECURITY.md`](../SECURITY.md) §2.1 entirely — there
is no name-based DLL load. Sessions and snapshots are exposed as context managers,
and every non-`SW_OK` result becomes a `SensorwatchError` carrying the `sw_error_t`
code and `sw_error_string()` text. A consumer that instead loads the standalone
CMake-built `sensorwatch.dll` should still load it by absolute path relative to the
installed package, never from the current working directory or `PATH`.

### C++

C++ wrappers should use RAII for session and snapshot ownership. The C ABI remains
`extern "C"`; C++ exceptions must not cross the C boundary.

### Rust

Rust bindings can generate declarations with `bindgen` or maintain small manual
FFI declarations. Handles should be wrapped in `Drop` types. Query functions that
fill caller buffers can be exposed as safe `String`-returning methods after a
length query.

---

## Evolution Policy

Before ABI major version `1`, this draft may change in response to review. After
an implemented native library is released with ABI major `1`:

- Add functions, enum values, and optional capabilities in minor versions.
- Do not remove or change existing function signatures within a major version.
- Do not reorder existing enum values.
- Do not expose new required public struct fields without `struct_size` guards.
- Treat exported-symbol changes as review-blocking unless intentionally paired
  with a major version bump.

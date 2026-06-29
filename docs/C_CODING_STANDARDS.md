# C Coding Standards -- sensorwatch Native Core (planned)

**Audience**: Experienced developers returning to C after time in higher-level languages.
**Scope**: Windows user-mode DLL, read-only shared memory access, consumed via FFI
from Python, C++, and Rust.

**Status**: Planned native core. The current repository is Python-only; there is
no C source tree, CMake build, or shipped DLL yet. This document is normative for
future native contributions and implementation PRs, but its code snippets are
illustrative until promoted into a dedicated ABI specification or public header.
The ABI specification in [`C_ABI.md`](C_ABI.md) (with its declaration-only header
[`../include/sensorwatch/sensorwatch.h`](../include/sensorwatch/sensorwatch.h)) is
authoritative for public symbol names and function signatures; this document
remains the implementation and review standard for the native code behind that ABI.

---

## Table of Contents

1. [Review Checklist](#1-review-checklist)
2. [Error Handling Pattern](#2-error-handling-pattern)
3. [Memory Safety](#3-memory-safety)
4. [ABI Versioning and Stability](#4-abi-versioning-and-stability)
5. [Thread Safety](#5-thread-safety)
6. [API Design for FFI](#6-api-design-for-ffi)
7. [Coding Style](#7-coding-style)
8. [Build and Tooling](#8-build-and-tooling)
9. [Testing](#9-testing)
10. [Modern C Features](#10-modern-c-features)

---

## 1. Review Checklist

Use this as the compact PR-review version of the longer guidance below.

### MUST

- Target C17 and build cleanly with MSVC as the primary compiler.
- Keep the shipped native core dependency-free beyond the C standard library,
  the Windows SDK, and system libraries.
- Treat HWiNFO shared memory as untrusted binary input: validate magic, element
  sizes, counts, offsets, computed section bounds, and overlapping sections before
  parsing entries.
- Bound every count, allocation, copy, and offset calculation; perform size
  arithmetic in `size_t` and check for overflow before use.
- Copy mapped shared memory into owned memory before parsing; do not expose raw
  shared-memory pointers or raw HWiNFO structs across the ABI.
- Use explicit error codes for all fallible public APIs; do not use `errno`,
  `GetLastError()`, process termination, or uncaught access violations as the
  primary error channel.
- Validate public pointer parameters and document ownership for every output
  parameter.
- Keep public handles opaque and public strings caller-buffer-owned.
- Emit sanitized, NUL-terminated UTF-8 strings at the ABI boundary.
- Make snapshots immutable once created.
- Document thread-safety for every public function.
- Run unit tests for malformed buffers before shipping native parsing changes.

### SHOULD

- Use the `goto cleanup` pattern for functions that acquire multiple resources.
- Add SAL annotations on public APIs so MSVC `/analyze` can reason about buffers
  and nullability.
- Build with warnings as errors, `/sdl`, `/analyze`, AddressSanitizer, and a
  clang-cl sanitizer/static-analysis configuration in CI once native code exists.
- Fuzz the parser with mutated shared-memory blobs and run it under sanitizers.
- Maintain an exported-symbol snapshot or equivalent ABI-compatibility check once
  a native library is released.
- Mirror the existing Python parser invariants in C tests, especially the cases
  in `tests/test_hwinfo_shm.py`.

### MAY

- Add adapter-specific extension APIs after the source-neutral core ABI is stable.
- Add optional tools such as a shared-memory capture utility for test fixture
  generation.
- Use handle magic values as a debugging aid for common FFI mistakes, while not
  treating them as a security boundary.

---

## 2. Error Handling Pattern

### The Verdict: goto-cleanup Wins (Still)

The "validate parameters, acquire resources, do work, cleanup via goto" pattern
remains the consensus best practice for C error handling in 2024-2026. Nothing
better has emerged because nothing *can* emerge without language-level changes
(which C23 did not deliver; a `defer` proposal exists for C2Y but is not yet
standardized).

The pattern is explicitly endorsed by:

- The **Linux kernel coding style** guide, which calls goto "handy when a
  function exits from multiple locations and some common work such as cleanup
  has to be done."
- **SEI CERT C Coding Standard** rule MEM12-C: "Consider using a goto chain
  when leaving a function on error when using and releasing resources."
- Essentially every major C codebase: SQLite, OpenSSL, the Windows kernel, Git.

### Alternatives Considered and Rejected

| Alternative | Why Not |
|---|---|
| `do { ... } while(0)` with `break` | Works for simple cases but obscures intent. A loop that never loops is a lie. Nested resource acquisition becomes awkward. |
| Nested `if (success)` chains | The "arrow anti-pattern." Indentation depth grows with each resource. Unreadable at 3+ resources. |
| `__attribute__((cleanup))` / defer macros | GCC/Clang only. Not available on MSVC. Cannot use for a project that must build with MSVC. |
| Early-return with duplicate cleanup | Violates DRY. Every new resource adds cleanup to every exit path. Bugs guaranteed. |

### Recommended Pattern

Use a **single cleanup label** with **ordered resource tracking**. Initialize
all resource variables to their "not acquired" state at the top. The cleanup
block releases in reverse order, skipping anything not acquired.

```c
hwi_error_t hwi_session_open(hwi_session_t **out_session)
{
    hwi_error_t err = HWI_OK;
    HANDLE      map_handle = NULL;
    void       *view       = NULL;
    hwi_session_t *session = NULL;

    /* --- Validate parameters --- */
    if (out_session == NULL) {
        return HWI_ERR_NULL_POINTER;
    }
    *out_session = NULL;

    /* --- Acquire resources --- */
    map_handle = OpenFileMappingW(FILE_MAP_READ, FALSE,
                                  L"Global\\HWiNFO_SENS_SM2");
    if (map_handle == NULL) {
        err = HWI_ERR_SHM_NOT_AVAILABLE;
        goto cleanup;
    }

    view = MapViewOfFile(map_handle, FILE_MAP_READ, 0, 0, 0);
    if (view == NULL) {
        err = HWI_ERR_MAP_FAILED;
        goto cleanup;
    }

    /* --- Validate shared memory contents --- */
    /* In production, bound the mapping with VirtualQuery before this read
       (Section 3, Bounds Checking; full version in Appendix A); omitted here to
       keep the cleanup pattern in focus. memcpy is the unaligned-safe read --
       the view is a packed wire layout, not a naturally-aligned struct. */
    uint32_t magic = 0;
    memcpy(&magic, view, sizeof(magic));
    if (magic != HWI_HEADER_MAGIC) {
        err = HWI_ERR_BAD_MAGIC;
        goto cleanup;
    }

    /* --- Allocate session --- */
    session = calloc(1, sizeof(*session));
    if (session == NULL) {
        err = HWI_ERR_OUT_OF_MEMORY;
        goto cleanup;
    }
    session->map_handle = map_handle;
    session->view       = view;

    /* Success: transfer ownership, skip cleanup of transferred resources */
    *out_session = session;
    return HWI_OK;

cleanup:
    /* Release in reverse acquisition order; NULL checks make each safe */
    if (view != NULL)       { UnmapViewOfFile(view); }
    if (map_handle != NULL) { CloseHandle(map_handle); }
    /* session is not freed here because calloc failure means it's NULL,
       and success path returns early before reaching cleanup */
    return err;
}
```

### Label Naming

Follow the Linux kernel convention: name labels for **what they do** or **why
they exist**, not numbered labels.

```c
/* Good */
cleanup:
cleanup_unmap:
cleanup_close_handle:

/* Bad */
err1:
err2:
end:
```

For functions with only one cleanup block (the common case in this project),
just use `cleanup:`. Use a goto chain (`cleanup_unmap:`, `cleanup_close:`) only
when the function has many resources acquired in strict sequence and the chain
genuinely improves clarity.

### Rules for the Pattern

1. Initialize all resource variables at declaration (to `NULL`, `0`,
   `INVALID_HANDLE_VALUE`, etc.).
2. Validate all input parameters **before** acquiring any resources. Return
   immediately on bad input -- no goto needed.
3. The cleanup block releases resources in **reverse acquisition order**.
4. Each release is guarded by a NULL/validity check so it is safe to reach
   from any point.
5. The success path returns **before** the cleanup label, transferring
   ownership of resources to the caller.

---

## 3. Memory Safety

### Null Pointer Discipline

Every public API function validates pointer parameters before use:

```c
hwi_error_t hwi_sensor_get_name(const hwi_session_t *session,
                                 uint32_t index,
                                 const char **out_name)
{
    if (session == NULL || out_name == NULL) {
        return HWI_ERR_NULL_POINTER;
    }
    /* ... */
}
```

For internal functions, use assertions instead -- a NULL in an internal call is
a bug in our code, not a user error:

```c
#include <assert.h>

static void parse_header(const uint8_t *raw, hwi_header_t *out)
{
    assert(raw != NULL);
    assert(out != NULL);
    /* ... */
}
```

### Bounds Checking

The shared memory contains offset and count fields that we do not control. All
offset arithmetic must be bounds-checked against the mapped region size:

```c
/* Validate that the sensor array fits within the mapped region.
   Field names match the raw header struct in Section 10. */
size_t sensor_end = (size_t)header.sensor_section_offset
                  + (size_t)header.sensor_element_count
                  * (size_t)header.sensor_element_size;
if (sensor_end > mapped_size) {
    return HWI_ERR_CORRUPT_DATA;
}
```

Cast to `size_t` before multiplying. On a 64-bit build -- the default for the
shipped DLL -- a `uint32_t * uint32_t` product cannot overflow a 64-bit `size_t`,
so the comparison above is sufficient. A 32-bit build has a 32-bit `size_t`, where
`sensor_element_count * sensor_element_size` *can* wrap and silently pass the
bounds check. The
Review Checklist requires checking overflow before use, so do not rely on the
width of `size_t`; use overflow-checked arithmetic:

```c
#include <windows.h>   /* FAILED() lives in <winerror.h>, pulled in by <windows.h> */
#include <intsafe.h>   /* MSVC: SizeTMult / SizeTAdd -> S_OK or an overflow HRESULT */

size_t span = 0, sensor_end = 0;
if (FAILED(SizeTMult(header.sensor_element_count, header.sensor_element_size, &span)) ||
    FAILED(SizeTAdd(header.sensor_section_offset, span, &sensor_end)) ||
    sensor_end > mapped_size) {
    return HWI_ERR_CORRUPT_DATA;
}
```

(clang-cl: `__builtin_mul_overflow` / `__builtin_add_overflow` give the same
guarantee.) This mirrors the SECURITY.md §1.3 requirement to check
size/count/offset multiplication with overflow-aware helpers.

### Safe String Handling

Strings from shared memory are fixed-width, potentially unterminated byte
arrays. Always:

1. Bound the copy to the field width.
2. Force null-termination.
3. Use `strncpy` or `memcpy` + explicit terminator, never `strcpy`.

```c
/* Copy a fixed-width string from shared memory into a caller buffer */
static void copy_fixed_string(char *dest, size_t dest_size,
                               const char *src, size_t src_field_size)
{
    if (dest == NULL || dest_size == 0) {
        return;  /* nothing we can safely write; dest_size - 1 would wrap to SIZE_MAX */
    }
    size_t copy_len = (dest_size - 1 < src_field_size)
                    ? dest_size - 1
                    : src_field_size;
    memcpy(dest, src, copy_len);
    dest[copy_len] = '\0';

    /* Also terminate at first embedded NUL (HWiNFO pads with zeroes) */
    size_t actual_len = strnlen(dest, copy_len);
    dest[actual_len] = '\0';
}
```

### Pointer Arithmetic Safety

When walking the shared memory, always compute addresses using byte offsets
from the base pointer, never by incrementing typed pointers through
variable-size structs:

```c
/* Good: explicit byte offset */
const uint8_t *base = (const uint8_t *)view;
const uint8_t *sensor_i = base + header.sensor_section_offset
                         + (size_t)i * header.sensor_element_size;

/* Bad: assumes fixed struct size (distinct name only so both lines can
   coexist in one snippet -- the real mistake is the typed-pointer indexing) */
const hwi_sensor_raw_t *sensor_i_bad = &sensors[i];  /* wrong if element size varies */
```

### Packed Wire Structs and Unaligned Reads

The HWiNFO layout is an externally-defined **packed wire format**, not a layout
the C compiler is free to pad. The header's `last_update` is an `int64_t` at byte
`0x0C`, which is **not** 8-byte aligned. This has two consequences that bite
every binding author:

1. **A naturally-aligned `struct` mirror does not match the wire layout.** The
   compiler inserts 4 bytes of padding before `last_update` to align it, pushing
   it to `0x10`, shifting every following field by 4, and making `sizeof` 56 not
   48. To overlay a struct on the bytes (e.g. for a `static_assert` layout check
   or a single bulk copy), it must be packed: `#pragma pack(push, 1)` /
   `#pragma pack(pop)` on MSVC and clang-cl (equivalently
   `__attribute__((packed))` on GCC/Clang). See Section 10 for the asserted
   layout.
2. **Dereferencing an unaligned member is undefined behavior.** Even with a
   packed struct, taking `&header->last_update` and reading through that
   `int64_t *` is UB: the pointer is underaligned. It happens to work on x86-64,
   but UBSan (`-fsanitize=alignment`) flags it and strict-alignment targets fault.

The robust pattern -- and the one the Python reference uses (`struct.unpack_from`
at fixed offsets) -- is to read each scalar by byte offset into a correctly-typed
local with `memcpy`. The compiler lowers this to a plain (possibly unaligned)
load with no struct, no padding ambiguity, and no aliasing or alignment UB:

```c
/* Unaligned-safe scalar read from the packed wire layout */
static int64_t read_i64(const uint8_t *base, size_t offset)
{
    int64_t v;
    memcpy(&v, base + offset, sizeof(v));   /* never *(int64_t *)(base + offset) */
    return v;
}
```

Use a packed struct only for compile-time layout assertions or a verbatim copy;
read field values through offset-based `memcpy`, never through member
dereference of an unaligned pointer.

### Handle Validation (Optional Defense-in-Depth)

For extra protection against use-after-free or garbage pointers passed by FFI
callers, embed a magic number in the session struct:

```c
#define HWI_SESSION_MAGIC 0x48574953u  /* "HWIS" */

struct hwi_session {
    uint32_t magic;
    HANDLE   map_handle;
    void    *view;
    /* ... */
};

static bool session_is_valid(const hwi_session_t *s)
{
    if (s == NULL) return false;
    /* Reading s->magic could crash if s is truly garbage, but:
       - FFI callers passing wrong types is a programming error
       - This catches use-after-free if we zero magic on close */
    return s->magic == HWI_SESSION_MAGIC;
}
```

This is a pragmatic tradeoff: it catches common FFI mistakes (wrong handle
type, double-free) at the cost of a non-crashproof check. Good enough for a
user-mode library; do not pretend it is security.

---

## 4. ABI Versioning and Stability

### API Version Constant

Embed a version in the public header so callers can check compatibility at
both compile time and runtime. The authoritative public spelling is
`SW_API_VERSION` / `sw_api_version()` in [`C_ABI.md`](C_ABI.md) and the shipped
[`sensorwatch.h`](../include/sensorwatch/sensorwatch.h); the `hwi_`-prefixed form
below is an illustrative implementation sketch (see the Naming note in
[Section 7](#7-coding-style)) showing the macro/runtime-query pattern:

```c
/* Illustrative sketch -- public names are sw_/SW_; see C_ABI.md */
#define HWI_API_VERSION_MAJOR 1
#define HWI_API_VERSION_MINOR 0
#define HWI_API_VERSION_PATCH 0

/* Encode as a single integer for runtime comparison: 1.2.3 -> 10203 */
#define HWI_API_VERSION \
    (HWI_API_VERSION_MAJOR * 10000 + HWI_API_VERSION_MINOR * 100 + HWI_API_VERSION_PATCH)

/* Runtime query (returns the version the DLL was compiled with) */
HWI_API uint32_t hwi_version(void);
```

FFI callers should check at load time (using the public `sw_api_version()`):

```python
dll_version = lib.sw_api_version()
if dll_version // 10000 != EXPECTED_MAJOR:
    raise RuntimeError(f"ABI mismatch: expected major {EXPECTED_MAJOR}, got {dll_version}")
```

### Versioning Rules

Follow semantic versioning at the ABI level:

- **Major**: Breaking changes (removed functions, changed signatures, changed
  struct layouts visible to callers). Bindings must be updated.
- **Minor**: Additive changes (new functions, new error codes, new fields in
  opaque structs). Existing bindings continue to work.
- **Patch**: Bug fixes, documentation, internal-only changes.

### Struct Size Guards

For any struct that crosses the DLL boundary (currently none -- all handles are
opaque), embed a `struct_size` field so the DLL can detect version mismatches:

```c
typedef struct hwi_config {
    uint32_t struct_size;  /* Must be sizeof(hwi_config_t) */
    uint32_t poll_interval_ms;
    /* ... future fields appended here ... */
} hwi_config_t;

/* Caller initializes a value and passes its address: */
hwi_config_t cfg = { .struct_size = sizeof(cfg), .poll_interval_ms = 1000 };

/* DLL validates the caller-provided pointer: */
hwi_error_t hwi_configure(const hwi_config_t *cfg)
{
    if (cfg == NULL ||
        cfg->struct_size < offsetof(hwi_config_t, poll_interval_ms) + sizeof(uint32_t)) {
        return HWI_ERR_VERSION_MISMATCH;
    }
    /* ... safe to read fields up to cfg->struct_size ... */
    return HWI_OK;
}
```

This allows adding fields to the end of the struct without breaking older
callers -- the DLL uses `struct_size` to know which fields are present.

### Deprecation Policy

When deprecating a function:

1. Add `HWI_DEPRECATED` attribute (maps to `__declspec(deprecated)` on MSVC,
   `__attribute__((deprecated))` on GCC/Clang).
2. Document the replacement in the header comment.
3. Keep the deprecated function for at least one major version.
4. Remove in the next major version bump.

```c
#ifdef _MSC_VER
    #define HWI_DEPRECATED(msg) __declspec(deprecated(msg))
#else
    #define HWI_DEPRECATED(msg) __attribute__((deprecated(msg)))
#endif

HWI_DEPRECATED("Use hwi_snapshot_take() instead")
HWI_API hwi_error_t hwi_read_all(hwi_session_t *s, hwi_reading_t *buf, uint32_t max);
```

---

## 5. Thread Safety

### Contract Per API Function

Every public function must document its thread-safety guarantee using one of
three levels:

| Level | Meaning | Example |
|---|---|---|
| **Thread-safe** | May be called concurrently from any thread without external synchronization. | `hwi_version()`, `hwi_error_string()` |
| **Session-bound** | Safe to call concurrently on *different* sessions, but callers must not share a single session across threads without synchronization. | `hwi_snapshot_take()`, `hwi_sensor_count()` |
| **Single-threaded** | Must not be called concurrently, even on different sessions. | (None planned; avoid this if possible.) |

Document the level in the header comment for each function:

```c
/**
 * Take a snapshot of all sensor readings.
 *
 * Thread safety: session-bound. Safe to call from any thread, but
 * the same session must not be used concurrently without external
 * synchronization.
 */
HWI_API hwi_error_t hwi_snapshot_take(hwi_session_t *session,
                                       hwi_snapshot_t **out);

/**
 * Return the API version the DLL was compiled with.
 *
 * Thread safety: thread-safe. No state accessed.
 */
HWI_API uint32_t hwi_version(void);
```

### Design Rationale

The session-bound model is the practical choice for this library:

- **One session per thread** is the simplest correct usage pattern. Most
  callers (a Python monitor, a REST service) will use a single session on a
  single thread anyway.
- **Snapshots are immutable** once created, so they can be freely shared across
  threads after `hwi_snapshot_take()` returns.
- **No internal locks**: The library does not use mutexes or critical sections
  internally. This avoids deadlock risks, priority inversion, and the
  complexity of lock-ordering in a small library. If callers need concurrent
  access to a single session, they provide their own synchronization.

### The Global Function Table

The `hwi_platform_ops_t` function table (Section 9, Testing) is a global
mutable. In production, it is set once at initialization and never modified.
In tests, it is swapped before the test and restored after. This is safe
because tests are single-threaded. Document this constraint:

```c
/* hwi_platform is set once during initialization. It is NOT safe to modify
   after any session has been opened. Test code must swap it before opening
   sessions and restore it after closing all sessions. */
extern hwi_platform_ops_t hwi_platform;
```

---

## 6. API Design for FFI

### Guiding Principles

The C API is the **universal ABI surface**. Design it so that:

- Python `ctypes`/`cffi` can call it with no wrapper code beyond declarations.
- C++ can wrap it in RAII classes trivially.
- Rust `bindgen` can generate usable bindings automatically.

### Opaque Handle Pattern

Never expose struct internals in public headers. The public header declares an
incomplete type:

```c
/* hwi_monitor.h (public) */
typedef struct hwi_session hwi_session_t;

HWI_API hwi_error_t hwi_session_open(hwi_session_t **out);
HWI_API void        hwi_session_close(hwi_session_t *session);
```

The struct definition lives only in the `.c` file:

```c
/* hwi_session.c (private) */
struct hwi_session {
    uint32_t magic;
    HANDLE   map_handle;
    void    *view;
    size_t   mapped_size;
    /* parsed header cache */
    hwi_header_t header;
};
```

This provides:

- **Binary stability**: Internal layout changes do not break callers.
- **FFI simplicity**: Callers just pass `void *` or an opaque pointer.
- **Encapsulation**: No temptation to poke at internals.

### Error Handling: Return Codes, Not errno

Use an explicit error enum returned from every fallible function. Never rely on
`errno` or `GetLastError()` as the primary error channel -- FFI callers may not
have access to the correct thread-local errno.

```c
typedef enum hwi_error {
    HWI_OK                   = 0,
    HWI_ERR_NULL_POINTER     = -1,
    HWI_ERR_SHM_NOT_AVAILABLE = -2,
    HWI_ERR_MAP_FAILED       = -3,
    HWI_ERR_BAD_MAGIC        = -4,
    HWI_ERR_CORRUPT_DATA     = -5,
    HWI_ERR_OUT_OF_MEMORY    = -6,
    HWI_ERR_INDEX_OUT_OF_RANGE = -7,
    HWI_ERR_BUFFER_TOO_SMALL = -8,
} hwi_error_t;

/* Convert error code to human-readable string (always valid, never NULL) */
HWI_API const char *hwi_error_string(hwi_error_t err);
```

Provide `hwi_error_string()` so that FFI callers can produce useful messages
without maintaining their own error tables.

### Output Parameters

Return data through output parameters (pointer-to-pointer for handles,
pointer-to-value for scalars). The return value is always the error code:

```c
/* Scalar output */
HWI_API hwi_error_t hwi_sensor_count(const hwi_session_t *session,
                                      uint32_t *out_count);

/* String output: caller provides buffer and size */
HWI_API hwi_error_t hwi_sensor_get_name(const hwi_session_t *session,
                                         uint32_t index,
                                         char *buf, size_t buf_size,
                                         size_t *out_len);
```

For strings, use the "caller provides buffer" pattern:

- Caller passes `buf` and `buf_size`.
- Function writes at most `buf_size - 1` bytes and null-terminates.
- If `out_len` is not NULL, writes the required size (including terminator).
- Returns `HWI_ERR_BUFFER_TOO_SMALL` if buffer is insufficient.

This avoids the question of who owns allocated memory -- the caller always
owns the buffer.

### Snapshot/Iterator Pattern for Bulk Data

Rather than exposing raw shared memory pointers or requiring callers to
allocate arrays of unknown size, provide a **snapshot** model:

```c
/* Take a point-in-time snapshot of all sensor readings.
   The snapshot is an opaque, immutable, self-contained copy. */
HWI_API hwi_error_t hwi_snapshot_take(hwi_session_t *session,
                                       hwi_snapshot_t **out);
HWI_API void        hwi_snapshot_free(hwi_snapshot_t *snap);

/* Query the snapshot */
HWI_API hwi_error_t hwi_snapshot_entry_count(const hwi_snapshot_t *snap,
                                              uint32_t *out_count);
HWI_API hwi_error_t hwi_snapshot_get_value(const hwi_snapshot_t *snap,
                                            uint32_t index,
                                            double *out_value);
HWI_API hwi_error_t hwi_snapshot_get_name(const hwi_snapshot_t *snap,
                                           uint32_t index,
                                           char *buf, size_t buf_size,
                                           size_t *out_len);
```

Benefits:

- Snapshot is a single `memcpy` from shared memory -- fast, atomic-ish.
- Snapshot is immutable -- no TOCTOU races during iteration.
- Caller cannot accidentally read stale or partially-updated shared memory.
- Easy to wrap in any language: Python gets a context manager, C++ gets RAII,
  Rust gets `Drop`.

### DLL Export Macro

```c
/* hwi_export.h */
#ifndef HWI_EXPORT_H
#define HWI_EXPORT_H

#ifdef HWI_BUILD_DLL
    #define HWI_API __declspec(dllexport)
#else
    #define HWI_API __declspec(dllimport)
#endif

/* For static library builds, define HWI_STATIC to make HWI_API empty */
#ifdef HWI_STATIC
    #undef  HWI_API
    #define HWI_API
#endif

#endif /* HWI_EXPORT_H */
```

### Calling Convention

Pin the `__cdecl` calling convention explicitly rather than relying on the
compiler default. `__cdecl` is the C default and what Python `ctypes` expects,
but the MSVC default can be flipped by build flags (e.g. `/Gz` makes it
`__stdcall`), so a stable ABI annotates every exported function with an explicit
`SW_CALL` macro (`__cdecl` on Windows, empty elsewhere) -- see the public header
and [`C_ABI.md`](C_ABI.md). Do not use `__stdcall` -- it complicates name
decoration and provides no benefit for a modern DLL.

### C++ Compatibility

Wrap all public headers in `extern "C"`:

```c
#ifdef __cplusplus
extern "C" {
#endif

/* ... declarations ... */

#ifdef __cplusplus
}
#endif
```

---

## 7. Coding Style

### Naming Conventions

| Element | Convention | Example |
|---|---|---|
| Public functions | ABI prefix + snake_case | `sw_session_open` |
| Public types | ABI prefix + snake_case + `_t` | `sw_session_t` |
| Public enums/constants | ABI prefix + UPPER_SNAKE | `SW_ERR_NULL_POINTER` |
| Internal functions | `static` + snake_case (no prefix) | `static parse_header(...)` |
| Local variables | snake_case | `sensor_count` |
| Struct members | snake_case | `map_handle` |
| Macros | UPPER_SNAKE | `HWI_HEADER_MAGIC` |

The draft public ABI uses the source-neutral `sw_` / `SW_` prefix; see
[`C_ABI.md`](C_ABI.md). A short public prefix prevents symbol collisions when the
DLL is loaded alongside other libraries. The `hwi_` / `HWI_` symbols in this
document's worked examples are illustrative implementation sketches from the
HWiNFO-first design phase — internal parser names (`hwi_header_t`,
`HWI_HEADER_MAGIC`, etc.) may keep an adapter-specific prefix, but do **not**
introduce new *public* ABI symbols under an HWiNFO-specific prefix unless they are
explicitly adapter-specific extension APIs layered on the source-neutral core.

### Header Organization

The draft public ABI is intentionally a single header:

```
include/
  sensorwatch/
    sensorwatch.h    -- Primary public ABI header (users include this)
```

When native implementation work begins, split the internal code by responsibility
while keeping only the stable ABI in the public include directory:

```
src/
  sw_session.c       -- Session open/close, shared memory management
  sw_snapshot.c      -- Snapshot take/free/query
  sw_parse.c         -- Shared memory parsing (header, sensors, entries)
  sw_error.c         -- sw_error_string() implementation
  sw_internal.h      -- Internal shared declarations (struct definitions, etc.)
```

### Include Guards

Use traditional `#ifndef` guards, not `#pragma once`:

```c
#ifndef SENSORWATCH_SENSORWATCH_H
#define SENSORWATCH_SENSORWATCH_H

/* ... */

#endif /* SENSORWATCH_SENSORWATCH_H */
```

Rationale: `#pragma once` is supported by all compilers we care about (MSVC,
Clang, GCC), but it is not standardized and can have subtle issues with
network drives, symlinks, and junction points on Windows. The traditional
guard is universally portable and costs nothing.

### Const Correctness

- Mark all pointer parameters `const` unless the function modifies the
  pointed-to data.
- Mark local variables `const` when they are assigned once.
- Use `const` on function return types for string accessors (e.g.,
  `const char *hwi_error_string(...)`).

```c
/* The session is read-only for queries; mutable for open/close */
hwi_error_t hwi_sensor_count(const hwi_session_t *session, uint32_t *out_count);
```

### Integer Types

Use `<stdint.h>` types for all data that interacts with shared memory or
crosses the DLL boundary:

| Use | Type |
|---|---|
| Byte offsets, sizes | `size_t` |
| Shared memory fields | `uint32_t`, `int64_t`, `double` |
| Array indices in API | `uint32_t` |
| Booleans | `bool` (from `<stdbool.h>`, C99+) |
| Error codes | `hwi_error_t` (enum, int-sized) |

Do not use `int`, `long`, `unsigned` for data that crosses module boundaries.
Their sizes vary between platforms and compilers.

### Braces and Formatting

Use K&R style (opening brace on same line, except for function definitions):

```c
/* Function definition: brace on next line */
hwi_error_t hwi_session_open(hwi_session_t **out)
{
    if (out == NULL) {
        return HWI_ERR_NULL_POINTER;
    }

    for (uint32_t i = 0; i < count; i++) {
        /* ... */
    }
}
```

Always use braces, even for single-statement `if`/`for`/`while` bodies:

```c
/* Good */
if (handle == NULL) {
    return HWI_ERR_SHM_NOT_AVAILABLE;
}

/* Bad -- leads to bugs when adding a second statement */
if (handle == NULL)
    return HWI_ERR_SHM_NOT_AVAILABLE;
```

### Comments

- Use `/* ... */` block comments for documentation and explanations.
- Use `//` line comments for brief inline notes (C99+, universally supported).
- Every public function gets a doc comment in the header explaining parameters,
  return value, and ownership semantics.

---

## 8. Build and Tooling

### Compiler: MSVC (Primary) + clang-cl (Secondary)

| Compiler | Role |
|---|---|
| **MSVC** (cl.exe) | Primary. Ships with Visual Studio, produces native Windows DLLs, best debugger integration with WinDbg and Visual Studio. |
| **clang-cl** | Secondary. Drop-in MSVC-compatible driver with superior warnings and sanitizer support. Use for CI checks and sanitizer runs. |
| MinGW | Not recommended. Different CRT, different ABI for exceptions, different DLL export semantics. Not worth the compatibility risk for a Windows-only library. |

### Build System: CMake

CMake is the practical choice. It generates MSVC `.sln`/`.vcxproj` for Visual
Studio users and Ninja build files for command-line/CI builds. It is also the
system expected by consumers doing `find_package()` or `FetchContent`.

### Dependency Baseline

The core library has **zero external dependencies** beyond the C standard
library and the Windows SDK:

| Dependency | Source | Required? |
|---|---|---|
| C17 stdlib | Compiler | Yes (always) |
| Windows SDK (kernel32, etc.) | System | Yes (always) |
| cmocka | FetchContent | Test-only |
| clang-tidy, Cppcheck | System install | CI/dev-only |

This is a deliberate design constraint. The DLL that ships to users links only
against system libraries. No vcpkg, no Conan, no vendored third-party code in
the runtime dependency chain. This keeps CMake configuration simple, eliminates
supply chain risk for the shipped binary, and makes cross-compilation trivial.

Optional extras (future REST service, data export, etc.) live in separate
targets with their own dependencies, gated behind CMake options:

```cmake
option(HWI_BUILD_REST    "Build REST service (requires cJSON)" OFF)
option(HWI_BUILD_TESTS   "Build tests (fetches cmocka)"        ON)
option(HWI_BUILD_FUZZ    "Build fuzz targets"                   OFF)
```

This way `cmake -B build` with defaults builds just the core DLL and tests --
nothing else to configure, nothing else to download.

### Compiler Flags

#### MSVC (`cl.exe`)

```
/std:c17          # C17 standard (see Section 10 for rationale)
/W4               # Warning level 4 (high, but not /Wall which is too noisy)
/WX               # Warnings as errors
/sdl              # Security Development Lifecycle checks (buffer overrun, etc.)
/guard:cf         # Control Flow Guard
/analyze          # Static analysis (MSVC built-in, slower but catches real bugs)
/GS               # Buffer security checks (on by default)
/DYNAMICBASE      # ASLR
/NXCOMPAT         # DEP
```

#### clang-cl

```
/std:c17
/W4
/WX
-Wextra
-Wpedantic
-Wconversion       # Catches implicit narrowing (very valuable)
-Wshadow           # Catches variable shadowing
-Wformat=2         # Format string validation
```

### Static Analysis

Run **all three** in CI. They catch different classes of bugs:

| Tool | What It Catches | Integration |
|---|---|---|
| **MSVC /analyze** | Windows API misuse, buffer overruns, null dereference, annotation violations (SAL) | Built into cl.exe, add `/analyze` flag |
| **clang-tidy** | Modernization, cert-* checks, readability, bugprone-* patterns | Run separately via `clang-tidy --checks='...'` |
| **Cppcheck** | Memory leaks, uninitialized vars, MISRA violations, dead code | Run separately via `cppcheck --enable=all` |

### SAL Annotations (Microsoft Source Annotation Language)

MSVC's `/analyze` is dramatically more useful with SAL annotations. Annotate
public API functions:

```c
#include <sal.h>

HWI_API hwi_error_t hwi_sensor_get_name(
    _In_ const hwi_session_t *session,
    _In_ uint32_t index,
    _Out_writes_z_(buf_size) char *buf,
    _In_ size_t buf_size,
    _Out_opt_ size_t *out_len);
```

SAL annotations help `/analyze` find bugs like writing past buffer boundaries,
using output parameters before they are written, and passing NULL where not
allowed. They are ignored by non-MSVC compilers (wrap in a macro if needed).

### AddressSanitizer (ASan)

ASan is available on MSVC since Visual Studio 2019 16.9. Use it for all test
runs:

```
cl.exe /fsanitize=address /std:c17 /Zi /MD ...
```

Important MSVC ASan constraints:

- Use `/MD` (dynamic CRT), not `/MDd` (debug CRT). ASan is incompatible with
  debug CRT.
- Disable `/RTC` (runtime checks) -- incompatible with ASan.
- Disable incremental linking (`/INCREMENTAL:NO`).
- The ASan runtime DLL (`clang_rt.asan_dynamic-x86_64.dll`) must be in PATH at
  runtime.

UBSan (Undefined Behavior Sanitizer) has limited MSVC support. Use clang-cl
for UBSan runs:

```
clang-cl /fsanitize=undefined /std:c17 /Zi /MD ...
```

---

## 9. Testing

### Framework: cmocka

**cmocka** is the recommended testing framework for this project.

Rationale:

- Works with MSVC, Clang, GCC, MinGW.
- Built-in mocking support via `will_return()` / `mock()` -- essential for
  mocking Win32 API calls.
- Detects memory leaks when using `test_malloc()` / `test_free()`.
- Lightweight: one `.c` file, one `.h` file.
- Active development: the 2.0 line (cmocka 2.0.0, December 2025) added a C99
  requirement and TAP 14 support; pin a specific patch release (2.0.2).
- **CMake integration is trivial** via `FetchContent` -- no manual install, no
  vcpkg, no system package needed:

```cmake
include(FetchContent)
FetchContent_Declare(
    cmocka
    GIT_REPOSITORY https://gitlab.com/cmocka/cmocka.git
    GIT_TAG        cmocka-2.0.2
)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(WITH_EXAMPLES OFF CACHE BOOL "" FORCE)
FetchContent_MakeAvailable(cmocka)

# Then link test targets:
add_executable(test_parse tests/test_parse.c)
target_link_libraries(test_parse PRIVATE hwi_monitor cmocka::cmocka)
add_test(NAME test_parse COMMAND test_parse)
```

CMake downloads and builds cmocka as part of the first build. No extra steps
for contributors -- `cmake -B build && cmake --build build && ctest --test-dir build`
just works.

### Test Structure

```
tests/
  test_parse.c       -- Unit tests for shared memory parsing
  test_session.c     -- Integration tests for session lifecycle
  test_snapshot.c    -- Tests for snapshot take/query
  test_error.c       -- Tests for error codes and error_string
  mock_win32.c       -- Mock implementations of Win32 API calls
  mock_win32.h
  test_data/
    valid_shm.bin    -- Binary blob: valid HWiNFO shared memory snapshot
    bad_magic.bin    -- Binary blob: corrupted magic number
    truncated.bin    -- Binary blob: truncated data
```

### Mocking Shared Memory

The key challenge is testing without HWiNFO64 running. Strategy: **mock at the
Win32 API level**.

Wrap the Win32 calls behind an internal function-pointer table that can be
swapped in tests:

```c
/* hwi_internal.h */
typedef struct hwi_platform_ops {
    HANDLE (*open_file_mapping)(DWORD access, BOOL inherit, LPCWSTR name);
    void  *(*map_view_of_file)(HANDLE h, DWORD access,
                                DWORD off_hi, DWORD off_lo, SIZE_T bytes);
    SIZE_T (*virtual_query)(const void *addr, PMEMORY_BASIC_INFORMATION mbi,
                            SIZE_T mbi_size);
    BOOL   (*unmap_view_of_file)(const void *base);
    BOOL   (*close_handle)(HANDLE h);
} hwi_platform_ops_t;

/* Default: real Win32 calls */
extern hwi_platform_ops_t hwi_platform;

/* hwi_session.c */
hwi_platform_ops_t hwi_platform = {
    .open_file_mapping  = OpenFileMappingW,
    .map_view_of_file   = MapViewOfFile,
    .virtual_query      = VirtualQuery,
    .unmap_view_of_file = UnmapViewOfFile,
    .close_handle       = CloseHandle,
};
```

`virtual_query` is in the table for the same reason as the others: the
region-size bound (Section 3) -- "query or otherwise bound the mapped region
size before copying," required by SECURITY.md §1.3 -- can only be exercised in a
unit test if the size query is mockable. A mock returns a
`MEMORY_BASIC_INFORMATION` with a test-controlled `RegionSize`, letting tests
feed undersized or oversized regions and assert the parser rejects them.

In tests, replace with mocks that return pointers to `malloc`'d test data:

```c
/* test_parse.c */
#include <cmocka.h>

static HANDLE mock_open_file_mapping(DWORD access, BOOL inherit,
                                      LPCWSTR name)
{
    (void)access; (void)inherit; (void)name;
    return (HANDLE)mock();  /* return test-controlled handle */
}

static void *mock_map_view_of_file(HANDLE h, DWORD access,
                                    DWORD off_hi, DWORD off_lo,
                                    SIZE_T bytes)
{
    (void)h; (void)access; (void)off_hi; (void)off_lo; (void)bytes;
    return (void *)mock();  /* return pointer to test data blob */
}

static SIZE_T mock_virtual_query(const void *addr, PMEMORY_BASIC_INFORMATION mbi,
                                 SIZE_T mbi_size)
{
    (void)addr;
    /* Mirror VirtualQuery's failure contract: 0 (and no writes) on a bad
       buffer. */
    if (mbi == NULL || mbi_size < sizeof(*mbi)) {
        return 0;
    }
    /* Feed the region size via will_return so cases can drive the Section 3
       bound with an undersized or oversized mapping. Nonzero return = bytes
       written = success. */
    mbi->RegionSize = (SIZE_T)mock();
    return sizeof(*mbi);
}

/* No-op cleanup mocks: the tests drive the session with fake state
   ((HANDLE)0xDEAD, a heap/stack buffer), so unmap/close must not reach the
   real Win32 calls on the success path or the failure goto-cleanup path. */
static BOOL mock_unmap_view_of_file(const void *base)
{
    (void)base;
    return TRUE;
}

static BOOL mock_close_handle(HANDLE h)
{
    (void)h;
    return TRUE;
}

static void test_parse_valid_shm(void **state)
{
    (void)state;
    /* Load a captured binary blob of real HWiNFO shared memory */
    size_t data_size = 0;
    uint8_t *test_data = load_test_file("test_data/valid_shm.bin", &data_size);

    /* Configure mocks. virtual_query must be mocked too: hwi_session_open()
       bounds the mapping before reading the header, so the test controls the
       reported RegionSize rather than calling the real Win32 syscall on a
       heap pointer. */
    hwi_platform.open_file_mapping  = mock_open_file_mapping;
    hwi_platform.map_view_of_file   = mock_map_view_of_file;
    hwi_platform.virtual_query      = mock_virtual_query;
    hwi_platform.unmap_view_of_file = mock_unmap_view_of_file;
    hwi_platform.close_handle       = mock_close_handle;
    will_return(mock_open_file_mapping, (HANDLE)0xDEAD);
    will_return(mock_map_view_of_file, test_data);
    will_return(mock_virtual_query, (SIZE_T)data_size);  /* whole blob is "mapped" */

    /* Exercise */
    hwi_session_t *session = NULL;
    hwi_error_t err = hwi_session_open(&session);
    assert_int_equal(err, HWI_OK);
    assert_non_null(session);

    /* Verify sensor data */
    uint32_t count = 0;
    hwi_sensor_count(session, &count);
    assert_int_not_equal(count, 0);

    /* Cleanup */
    hwi_session_close(session);
    free(test_data);
}

/* The case that motivated adding virtual_query to the ops table: a mapping
   smaller than the header must be rejected, not parsed. */
static void test_open_rejects_undersized_region(void **state)
{
    (void)state;
    static uint8_t tiny[8] = {0};   /* < sizeof(hwi_header_raw_t) */

    hwi_platform.open_file_mapping  = mock_open_file_mapping;
    hwi_platform.map_view_of_file   = mock_map_view_of_file;
    hwi_platform.virtual_query      = mock_virtual_query;
    hwi_platform.unmap_view_of_file = mock_unmap_view_of_file;
    hwi_platform.close_handle       = mock_close_handle;
    will_return(mock_open_file_mapping, (HANDLE)0xDEAD);
    will_return(mock_map_view_of_file, tiny);
    will_return(mock_virtual_query, (SIZE_T)sizeof(tiny));

    hwi_session_t *session = NULL;
    hwi_error_t err = hwi_session_open(&session);
    assert_int_equal(err, HWI_ERR_CORRUPT_DATA);
    assert_null(session);
}
```

### Capturing Test Data

To create the binary test blobs, write a small utility that opens the real
HWiNFO shared memory and dumps it to a file:

```c
/* tools/capture_shm.c -- run once on a machine with HWiNFO running */
int main(void)
{
    HANDLE h = OpenFileMappingW(FILE_MAP_READ, FALSE,
                                 L"Global\\HWiNFO_SENS_SM2");
    void *view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, 0);

    /* Determine total size from header */
    /* ... parse header to find end of entry array ... */

    FILE *f = fopen("valid_shm.bin", "wb");
    fwrite(view, 1, total_size, f);
    fclose(f);

    UnmapViewOfFile(view);
    CloseHandle(h);
}
```

### Test Categories

| Category | What | When |
|---|---|---|
| Unit tests | Parsing logic with binary blobs, error code coverage, string handling | Every build |
| Integration tests | Real shared memory access (skip if HWiNFO not running) | Manual, on dev machine |
| Fuzz tests | Feed random/mutated binary blobs to parser, verify no crashes | CI nightly |
| Sanitizer tests | Full test suite under ASan + UBSan | Every CI build |
| ABI compatibility tests | Exported-symbol snapshots, header compile tests, and smoke tests from Python/C++/Rust bindings | Every ABI-affecting change after first native release |

### Fuzzing the Parser

The parser consumes untrusted structured binary data, so unit tests alone are not
enough once the C implementation exists. Add a fuzz target that accepts arbitrary
bytes, invokes the same parse routine used by snapshot acquisition, and treats any
crash, sanitizer finding, timeout, or unbounded allocation as a bug. Seed the
corpus with the synthetic fixtures mirrored from `tests/test_hwinfo_shm.py` and
any captured HWiNFO blobs that are safe to store in the repository.

Keep fuzz targets separate from the shipped library target and gate them behind a
CMake option such as `HWI_BUILD_FUZZ`. Run short fuzz/sanitizer jobs in CI for
parser changes and longer jobs on a schedule.

### ABI Compatibility Tests

After a native library is released, ABI stability needs automated checks in
addition to code review:

- Compile the public header from a tiny C translation unit with MSVC and clang-cl.
- Build and compare an exported-symbol list for the DLL.
- Load the DLL from Python `ctypes` and perform a version/error-string smoke test.
- Compile a minimal C++ RAII wrapper and a Rust `bindgen`/`libloading` smoke test
  when those bindings exist.
- Treat removed symbols, changed signatures, or changed public struct layouts as
  ABI-breaking unless the major version changes.

---

## 10. Modern C Features

### Target Standard: C17

**C17** (`/std:c17` in MSVC) is the recommended target.

Rationale:

- C17 is a "bug-fix" release of C11 -- same features, corrected wording.
- MSVC has full C17 support since VS 2019 16.8 (Windows SDK 10.0.20348.0+).
- C11/C17 gives us the important features (`static_assert`, `_Alignof`,
  `<stdbool.h>`, `<stdatomic.h>`, anonymous structs/unions).
- C23 (`/std:clatest` in MSVC) is **not** recommended yet because MSVC C23
  support is incomplete as of early 2026. GCC and Clang are much further ahead
  on C23 conformance.

### C11/C17 Features to Use

| Feature | Use For | Example |
|---|---|---|
| `static_assert` | Compile-time validation of struct sizes, offsets | `static_assert(sizeof(hwi_header_raw_t) == 48, "header size");` |
| `<stdbool.h>` | Boolean type in internal code | `bool is_valid = session_is_valid(s);` |
| `<stdint.h>` | Fixed-width integers | `uint32_t`, `int64_t`, `size_t` |
| `_Alignof` / `_Alignas` | Ensure struct alignment matches shared memory layout | `_Static_assert(_Alignof(hwi_header_raw_t) <= 8, "alignment");` |
| Anonymous structs/unions | Convenient nested data access | Sparingly; can confuse FFI tools |
| Designated initializers | Clear struct initialization | `hwi_platform_ops_t ops = { .open_file_mapping = mock_open };` |

### C23 Features to Adopt Later (When MSVC Catches Up)

These are worth adopting once MSVC `/std:c23` or `/std:clatest` reliably
supports them:

| Feature | Why We Want It |
|---|---|
| `nullptr` | Type-safe null pointer constant; avoids `(void *)0` vs `0` ambiguity |
| `typeof` / `typeof_unqual` | Reduces macro boilerplate, enables generic-ish utilities |
| `static_assert` without message | Shorter compile-time checks: `static_assert(sizeof(x) == 48);` |
| `[[nodiscard]]` attribute | Compiler warning if caller ignores return value (error codes!) |
| `[[maybe_unused]]` attribute | Suppresses warnings on intentionally unused parameters |
| `constexpr` variables | Named compile-time constants without `#define` |
| `bool` / `true` / `false` as keywords | No more `<stdbool.h>` include |
| `#embed` | Embed binary test data directly in test executables |
| Binary literal `0b...` | Readable bitmask constants |

### Features to Avoid

| Feature | Why |
|---|---|
| Variable-length arrays (VLAs) | Stack overflow risk, banned by MISRA, disabled by MSVC |
| `_Generic` | Clever but unreadable; better served by separate named functions |
| `<threads.h>` (C11) | Not implemented by MSVC; use Win32 threads or leave threading to callers |
| `<stdatomic.h>` (C11) | Not implemented by MSVC in C mode; use `InterlockedXxx` if atomics needed |
| Complex numbers (`_Complex`) | Irrelevant for this project |

### Practical static_assert Examples

```c
#include <assert.h>   /* static_assert: standard macro since C11/C17 (-> _Static_assert) */
#include <stdint.h>   /* uint32_t, int64_t */
#include <stddef.h>   /* offsetof */

/* Verify shared memory struct layout assumptions at compile time */
static_assert(sizeof(uint32_t) == 4, "uint32_t must be 4 bytes");
static_assert(sizeof(double) == 8, "double must be 8 bytes");

/* The header magic, as it appears in the mapping (bytes 'H' 'W' 'i' 'S').
   Must match the producer; the Python reference uses the same value. */
#define HWI_HEADER_MAGIC 0x53695748u

/* Verify our raw header struct matches the documented layout.
 *
 * This is a PACKED wire format: last_update is an int64 at byte 0x0C, which is
 * NOT 8-byte aligned. A naturally-aligned struct would pad before last_update
 * (pushing it to 0x10, sizeof to 56) and the asserts below would fail to
 * compile. #pragma pack(1) strips the padding so the struct mirrors the bytes.
 * See "Packed Wire Structs and Unaligned Reads" in Section 3 -- and note this
 * struct is for layout assertions and verbatim copies only; read field *values*
 * via offset-based memcpy, not by dereferencing unaligned members. */
#pragma pack(push, 1)
typedef struct hwi_header_raw {
    uint32_t magic;
    uint32_t version;
    uint32_t version2;
    int64_t  last_update;
    uint32_t sensor_section_offset;
    uint32_t sensor_element_size;
    uint32_t sensor_element_count;
    uint32_t entry_section_offset;
    uint32_t entry_element_size;
    uint32_t entry_element_count;
    uint32_t poll_time;
} hwi_header_raw_t;
#pragma pack(pop)

static_assert(sizeof(hwi_header_raw_t) == 48,
              "header struct size must match HWiNFO shared memory layout");

/* Verify field offsets (catches struct padding surprises) */
static_assert(offsetof(hwi_header_raw_t, last_update) == 0x0C,
              "last_update is an unaligned int64 at byte 0x0C");
static_assert(offsetof(hwi_header_raw_t, sensor_section_offset) == 0x14,
              "sensor_section_offset must be at byte 0x14");
static_assert(offsetof(hwi_header_raw_t, entry_section_offset) == 0x20,
              "entry_section_offset must be at byte 0x20");
```

---

## Appendix A: Complete Minimal Example

A condensed example showing all patterns working together. This is approximately
what `hwi_session.c` would look like:

```c
/* hwi_session.c */
#include "hwi_monitor.h"
#include "hwi_internal.h"

#include <assert.h>
#include <stdlib.h>
#include <string.h>

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

/* --- Platform operations (mockable for tests) --- */

hwi_platform_ops_t hwi_platform = {
    .open_file_mapping  = OpenFileMappingW,
    .map_view_of_file   = MapViewOfFile,
    .virtual_query      = VirtualQuery,
    .unmap_view_of_file = UnmapViewOfFile,
    .close_handle       = CloseHandle,
};

/* --- Session lifecycle --- */

hwi_error_t hwi_session_open(hwi_session_t **out)
{
    hwi_error_t    err         = HWI_OK;
    HANDLE         map_handle  = NULL;
    void          *view        = NULL;
    size_t         mapped_size = 0;
    hwi_session_t *session     = NULL;

    /* Validate parameters */
    if (out == NULL) {
        return HWI_ERR_NULL_POINTER;
    }
    *out = NULL;

    /* Open shared memory */
    map_handle = hwi_platform.open_file_mapping(
        FILE_MAP_READ, FALSE, L"Global\\HWiNFO_SENS_SM2");
    if (map_handle == NULL) {
        err = HWI_ERR_SHM_NOT_AVAILABLE;
        goto cleanup;
    }

    /* Map view */
    view = hwi_platform.map_view_of_file(
        map_handle, FILE_MAP_READ, 0, 0, 0);
    if (view == NULL) {
        err = HWI_ERR_MAP_FAILED;
        goto cleanup;
    }

    /* Bound the mapping before touching any field. An untrusted producer may
       map fewer bytes than a full header; without this, the magic read below
       could run off the end of the mapping. (Section 3, Bounds Checking.) */
    MEMORY_BASIC_INFORMATION mbi = {0};
    if (hwi_platform.virtual_query(view, &mbi, sizeof(mbi)) == 0 ||
        mbi.RegionSize < sizeof(hwi_header_raw_t)) {
        err = HWI_ERR_CORRUPT_DATA;
        goto cleanup;
    }
    mapped_size = mbi.RegionSize;

    /* Validate magic (now known to lie within the mapping). memcpy is the
       unaligned-safe read for a packed wire layout. */
    uint32_t magic = 0;
    memcpy(&magic, view, sizeof(magic));
    if (magic != HWI_HEADER_MAGIC) {
        err = HWI_ERR_BAD_MAGIC;
        goto cleanup;
    }

    /* Allocate and initialize session */
    session = calloc(1, sizeof(*session));
    if (session == NULL) {
        err = HWI_ERR_OUT_OF_MEMORY;
        goto cleanup;
    }
    session->magic       = HWI_SESSION_MAGIC;
    session->map_handle  = map_handle;
    session->view        = view;
    session->mapped_size = mapped_size;

    /* Cache the now-bounded header. Bulk entry parsing happens later in
       hwi_snapshot_take(), which copies the bounded region into owned memory
       and parses the copy -- the live view is never parsed past this validated
       header. (See "Snapshot/Iterator Pattern" and SECURITY.md §1.3.) */
    err = parse_header(view, &session->header);
    if (err != HWI_OK) {
        free(session);
        session = NULL;
        goto cleanup;
    }

    /* Success -- transfer ownership */
    *out = session;
    return HWI_OK;

cleanup:
    if (view != NULL) {
        hwi_platform.unmap_view_of_file(view);
    }
    if (map_handle != NULL) {
        hwi_platform.close_handle(map_handle);
    }
    return err;
}

void hwi_session_close(hwi_session_t *session)
{
    if (session == NULL) {
        return;
    }
    assert(session->magic == HWI_SESSION_MAGIC);

    if (session->view != NULL) {
        hwi_platform.unmap_view_of_file(session->view);
    }
    if (session->map_handle != NULL) {
        hwi_platform.close_handle(session->map_handle);
    }

    session->magic = 0;  /* Poison: catch use-after-free */
    free(session);
}
```

---

## Appendix B: Python ctypes Usage Example

What the consumer side looks like -- verifying the API is FFI-friendly:

```python
import ctypes
from ctypes import c_int, c_uint32, c_double, c_char_p, c_size_t, POINTER, byref

lib = ctypes.CDLL("hwi_monitor.dll")

# Error type
hwi_error_t = c_int

# Opaque handle
class hwi_session_t(ctypes.Structure):
    pass

session_ptr = POINTER(hwi_session_t)

# Bind functions
lib.hwi_session_open.restype = hwi_error_t
lib.hwi_session_open.argtypes = [POINTER(session_ptr)]

lib.hwi_session_close.restype = None
lib.hwi_session_close.argtypes = [session_ptr]

lib.hwi_sensor_count.restype = hwi_error_t
lib.hwi_sensor_count.argtypes = [session_ptr, POINTER(c_uint32)]

lib.hwi_error_string.restype = c_char_p
lib.hwi_error_string.argtypes = [hwi_error_t]

# Usage
session = session_ptr()
err = lib.hwi_session_open(byref(session))
if err != 0:
    msg = lib.hwi_error_string(err)
    raise RuntimeError(f"hwi_session_open failed: {msg.decode()}")

count = c_uint32()
lib.hwi_sensor_count(session, byref(count))
print(f"Found {count.value} sensors")

lib.hwi_session_close(session)
```

---

## Appendix C: Key References

- [Linux kernel coding style -- goto](https://docs.kernel.org/process/coding-style.html)
- [SEI CERT C: MEM12-C -- goto chain for resource cleanup](https://wiki.sei.cmu.edu/confluence/display/c/MEM12-C.+Consider+using+a+goto+chain+when+leaving+a+function+on+error+when+using+and+releasing+resources)
- [MSVC C11/C17 support](https://devblogs.microsoft.com/cppblog/c11-and-c17-standard-support-arriving-in-msvc/)
- [MSVC AddressSanitizer](https://learn.microsoft.com/en-us/cpp/sanitizers/asan)
- [Opaque Pointers and Objects in C](https://interrupt.memfault.com/blog/opaque-pointers)
- [cmocka -- unit testing framework for C](https://cmocka.org/)
- [C23 feature overview](https://lemire.me/blog/2024/01/21/c23-a-slightly-better-c/)
- [Cppcheck static analysis](https://cppcheck.sourceforge.io/)
- [MSVC /analyze](https://learn.microsoft.com/en-us/cpp/build/reference/analyze-code-analysis)
- [DLL export/import pattern](https://learn.microsoft.com/en-us/cpp/build/exporting-from-a-dll-using-declspec-dllexport)

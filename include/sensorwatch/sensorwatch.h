#ifndef SENSORWATCH_SENSORWATCH_H
#define SENSORWATCH_SENSORWATCH_H

/*
 * sensorwatch C ABI (draft 0.1.0).
 *
 * This header is the public C ABI for the native sensorwatch core, implemented in
 * src/ (a Windows DLL plus a static library; see docs/C_ABI.md and the README).
 * The ABI is a pre-1.0 draft and may still change until the first release carries
 * a stability commitment. A Python binding (cffi, API mode) ships over this ABI;
 * C++ and Rust bindings are not provided yet.
 *
 * Calling convention: every function is declared with SW_CALL, which pins the
 * platform C calling convention (`__cdecl` on Windows) so the ABI stays stable
 * even when a consumer's compiler default differs (e.g. MSVC `/Gz`). ABI
 * functions are never `__stdcall` or `__fastcall`.
 *
 * Enum width: sw_error_t and sw_reading_type_t are asserted to be 4 bytes so the
 * implementation-defined enum size cannot silently drift across compilers/flags
 * (e.g. `-fshort-enums`) and break the wire layout bindings depend on.
 */

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(SW_STATIC)
#  define SW_API
#elif defined(_WIN32) && defined(SW_BUILD_DLL)
#  define SW_API __declspec(dllexport)
#elif defined(_WIN32)
#  define SW_API __declspec(dllimport)
#elif defined(__GNUC__) || defined(__clang__)
#  define SW_API __attribute__((visibility("default")))
#else
#  define SW_API
#endif

/* Explicit calling convention: __cdecl on Windows, default elsewhere. */
#if defined(_WIN32)
#  define SW_CALL __cdecl
#else
#  define SW_CALL
#endif

/*
 * Portable compile-time assertion, active in every supported language mode
 * (C99+, C++98+) -- never a no-op, since these are ABI invariants. C11 / C++11
 * use the native static_assert (which surfaces `msg`); older modes fall back to
 * a negative-size-array typedef, still a hard compile error and -- being a
 * declaration -- it cleanly consumes the trailing `;` (no stray semicolon under
 * -Wpedantic). _MSVC_LANG is checked because MSVC reports __cplusplus as
 * 199711L unless /Zc:__cplusplus is set. Helper names avoid `__` so the header
 * stays clear of the reserved-identifier space (-Wreserved-identifier).
 */
#if (defined(__cplusplus) && __cplusplus >= 201103L) || \
    (defined(_MSVC_LANG) && _MSVC_LANG >= 201103L)
#  define SW_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#elif !defined(__cplusplus) && defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#  define SW_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#else
#  define SW_STATIC_ASSERT_CAT_(a, b) a##b
#  define SW_STATIC_ASSERT_CAT(a, b) SW_STATIC_ASSERT_CAT_(a, b)
#  define SW_STATIC_ASSERT(cond, msg) \
       typedef char SW_STATIC_ASSERT_CAT(sw_static_assert_, __LINE__)[(cond) ? 1 : -1]
#endif

#define SW_API_VERSION_MAJOR 0u
#define SW_API_VERSION_MINOR 1u
#define SW_API_VERSION_PATCH 0u
#define SW_API_VERSION \
    ((SW_API_VERSION_MAJOR * 10000u) + \
     (SW_API_VERSION_MINOR * 100u) + \
     SW_API_VERSION_PATCH)

typedef struct sw_session sw_session_t;
typedef struct sw_snapshot sw_snapshot_t;

typedef enum sw_error {
    SW_OK                         = 0,
    SW_ERR_NULL_POINTER           = -1,
    SW_ERR_INVALID_ARGUMENT       = -2,
    SW_ERR_UNSUPPORTED_PLATFORM   = -3,
    SW_ERR_SOURCE_UNAVAILABLE     = -4,
    SW_ERR_MAP_FAILED             = -5,
    SW_ERR_BAD_MAGIC              = -6,
    SW_ERR_CORRUPT_DATA           = -7,
    SW_ERR_OUT_OF_MEMORY          = -8,
    SW_ERR_INDEX_OUT_OF_RANGE     = -9,
    SW_ERR_BUFFER_TOO_SMALL       = -10,
    SW_ERR_VERSION_MISMATCH       = -11,
    SW_ERR_INTERNAL               = -12
} sw_error_t;

SW_STATIC_ASSERT(sizeof(sw_error_t) == 4,
                 "sw_error_t must be 4 bytes for a stable cross-compiler ABI");

/*
 * Source-neutral reading category.
 *
 * Values 1..8 map 1:1 onto the categories the current Python parser exposes via
 * sensorwatch.hwinfo_shm.SENSOR_TYPES (Temperature..Other). SW_READING_TYPE_NONE
 * mirrors the source's explicit "None" category (HWiNFO type 0). A source type
 * code that this ABI version does not recognize is reported as
 * SW_READING_TYPE_UNKNOWN rather than guessed at, so bindings can distinguish
 * "the source said none" from "newer/unknown source category".
 */
typedef enum sw_reading_type {
    SW_READING_TYPE_NONE          = 0,
    SW_READING_TYPE_TEMPERATURE   = 1,
    SW_READING_TYPE_VOLTAGE       = 2,
    SW_READING_TYPE_FAN           = 3,
    SW_READING_TYPE_CURRENT       = 4,
    SW_READING_TYPE_POWER         = 5,
    SW_READING_TYPE_CLOCK         = 6,
    SW_READING_TYPE_USAGE         = 7,
    SW_READING_TYPE_OTHER         = 8,
    SW_READING_TYPE_UNKNOWN       = 255
} sw_reading_type_t;

SW_STATIC_ASSERT(sizeof(sw_reading_type_t) == 4,
                 "sw_reading_type_t must be 4 bytes for a stable cross-compiler ABI");

/*
 * Return the ABI version encoded as MAJOR * 10000 + MINOR * 100 + PATCH.
 *
 * Thread safety: thread-safe.
 */
SW_API uint32_t SW_CALL sw_api_version(void);

/*
 * Return a static human-readable string for an error code.
 *
 * The returned pointer is owned by the library, remains valid for the life of
 * the process, and is never NULL.
 *
 * Thread safety: thread-safe.
 */
SW_API const char *SW_CALL sw_error_string(sw_error_t error);

/*
 * Open a sensorwatch session for the default sensor source.
 *
 * On success, writes a non-NULL session to out_session. On failure, writes NULL
 * to out_session when possible.
 *
 * Thread safety: safe to call concurrently for different output sessions.
 */
SW_API sw_error_t SW_CALL sw_session_open(sw_session_t **out_session);

/*
 * Close a session opened by sw_session_open(). Passing NULL is a no-op.
 *
 * Thread safety: must not race with any other use of the same session.
 */
SW_API void SW_CALL sw_session_close(sw_session_t *session);

/*
 * Take an immutable snapshot of all currently available sensor readings.
 *
 * On success, writes a non-NULL snapshot to out_snapshot. On failure, writes
 * NULL to out_snapshot when possible.
 *
 * Thread safety: session-bound. Callers must synchronize concurrent use of the
 * same session.
 */
SW_API sw_error_t SW_CALL sw_snapshot_take(sw_session_t *session,
                                           sw_snapshot_t **out_snapshot);

/*
 * Free a snapshot returned by sw_snapshot_take(). Passing NULL is a no-op.
 *
 * Thread safety: must not race with any other use of the same snapshot.
 */
SW_API void SW_CALL sw_snapshot_free(sw_snapshot_t *snapshot);

/*
 * Return the number of reading entries in a snapshot.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_entry_count(const sw_snapshot_t *snapshot,
                                                  uint32_t *out_count);

/*
 * Return the source-neutral reading type for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_reading_type(const sw_snapshot_t *snapshot,
                                                       uint32_t index,
                                                       sw_reading_type_t *out_type);

/*
 * Return the current value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_value(const sw_snapshot_t *snapshot,
                                                uint32_t index,
                                                double *out_value);

/*
 * Return the minimum value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_minimum(const sw_snapshot_t *snapshot,
                                                  uint32_t index,
                                                  double *out_value);

/*
 * Return the maximum value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_maximum(const sw_snapshot_t *snapshot,
                                                  uint32_t index,
                                                  double *out_value);

/*
 * Return the average value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_average(const sw_snapshot_t *snapshot,
                                                  uint32_t index,
                                                  double *out_value);

/*
 * Copy the source/backend name for a snapshot entry into a caller-owned buffer.
 *
 * Strings are UTF-8, sanitized as untrusted display data (no C0/C1 control
 * characters). This buffer/size/length contract is shared by every string
 * accessor below:
 *
 *   - snapshot == NULL                -> SW_ERR_NULL_POINTER.
 *   - index >= entry count            -> SW_ERR_INDEX_OUT_OF_RANGE.
 *   - Length query (buffer == NULL && buffer_size == 0): out_required MUST be
 *     non-NULL. The required byte count, including the terminating NUL (always
 *     >= 1), is stored in *out_required and the call returns
 *     SW_ERR_BUFFER_TOO_SMALL. If out_required == NULL in this mode there is no
 *     way to return the size, so the call returns SW_ERR_NULL_POINTER.
 *   - Copy (buffer != NULL && buffer_size > 0): if the value plus its NUL fits,
 *     it is copied, NUL-terminated, *out_required (when non-NULL) is set to the
 *     bytes written including the NUL, and the call returns SW_OK. If it does
 *     not fit, buffer is left as an empty NUL-terminated string (never a partial
 *     UTF-8 sequence), *out_required (when non-NULL) is set to the full required
 *     size, and the call returns SW_ERR_BUFFER_TOO_SMALL.
 *   - Any other (buffer, buffer_size) combination -- buffer == NULL with
 *     buffer_size > 0, or buffer != NULL with buffer_size == 0 -- returns
 *     SW_ERR_INVALID_ARGUMENT.
 *   - out_required may be NULL only in the copy form; it is required for length
 *     queries. Whenever buffer != NULL && buffer_size > 0, buffer is always
 *     NUL-terminated on return.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_source_name(const sw_snapshot_t *snapshot,
                                                      uint32_t index,
                                                      char *buffer,
                                                      size_t buffer_size,
                                                      size_t *out_required);

/*
 * Copy the sensor display name for a snapshot entry into a caller-owned buffer.
 * See sw_snapshot_get_source_name() for string/buffer rules.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_sensor_name(const sw_snapshot_t *snapshot,
                                                      uint32_t index,
                                                      char *buffer,
                                                      size_t buffer_size,
                                                      size_t *out_required);

/*
 * Copy the reading display name for a snapshot entry into a caller-owned buffer.
 * See sw_snapshot_get_source_name() for string/buffer rules.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_reading_name(const sw_snapshot_t *snapshot,
                                                       uint32_t index,
                                                       char *buffer,
                                                       size_t buffer_size,
                                                       size_t *out_required);

/*
 * Copy the unit string for a snapshot entry into a caller-owned buffer.
 * See sw_snapshot_get_source_name() for string/buffer rules.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t SW_CALL sw_snapshot_get_unit(const sw_snapshot_t *snapshot,
                                               uint32_t index,
                                               char *buffer,
                                               size_t buffer_size,
                                               size_t *out_required);

#ifdef __cplusplus
}
#endif

#endif /* SENSORWATCH_SENSORWATCH_H */

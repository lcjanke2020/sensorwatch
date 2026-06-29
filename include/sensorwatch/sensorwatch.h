#ifndef SENSORWATCH_SENSORWATCH_H
#define SENSORWATCH_SENSORWATCH_H

/*
 * sensorwatch C ABI draft.
 *
 * This is a declaration-only proposal for the future native core. The current
 * project does not yet ship a C implementation or DLL.
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

typedef enum sw_reading_type {
    SW_READING_TYPE_UNKNOWN       = 0,
    SW_READING_TYPE_TEMPERATURE   = 1,
    SW_READING_TYPE_VOLTAGE       = 2,
    SW_READING_TYPE_FAN           = 3,
    SW_READING_TYPE_CURRENT       = 4,
    SW_READING_TYPE_POWER         = 5,
    SW_READING_TYPE_CLOCK         = 6,
    SW_READING_TYPE_USAGE         = 7,
    SW_READING_TYPE_OTHER         = 8
} sw_reading_type_t;

/*
 * Return the ABI version encoded as MAJOR * 10000 + MINOR * 100 + PATCH.
 *
 * Thread safety: thread-safe.
 */
SW_API uint32_t sw_api_version(void);

/*
 * Return a static human-readable string for an error code.
 *
 * The returned pointer is owned by the library, remains valid for the life of
 * the process, and is never NULL.
 *
 * Thread safety: thread-safe.
 */
SW_API const char *sw_error_string(sw_error_t error);

/*
 * Open a sensorwatch session for the default sensor source.
 *
 * On success, writes a non-NULL session to out_session. On failure, writes NULL
 * to out_session when possible.
 *
 * Thread safety: safe to call concurrently for different output sessions.
 */
SW_API sw_error_t sw_session_open(sw_session_t **out_session);

/*
 * Close a session opened by sw_session_open(). Passing NULL is a no-op.
 *
 * Thread safety: must not race with any other use of the same session.
 */
SW_API void sw_session_close(sw_session_t *session);

/*
 * Take an immutable snapshot of all currently available sensor readings.
 *
 * On success, writes a non-NULL snapshot to out_snapshot. On failure, writes
 * NULL to out_snapshot when possible.
 *
 * Thread safety: session-bound. Callers must synchronize concurrent use of the
 * same session.
 */
SW_API sw_error_t sw_snapshot_take(sw_session_t *session,
                                   sw_snapshot_t **out_snapshot);

/*
 * Free a snapshot returned by sw_snapshot_take(). Passing NULL is a no-op.
 *
 * Thread safety: must not race with any other use of the same snapshot.
 */
SW_API void sw_snapshot_free(sw_snapshot_t *snapshot);

/*
 * Return the number of reading entries in a snapshot.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_entry_count(const sw_snapshot_t *snapshot,
                                          uint32_t *out_count);

/*
 * Return the source-neutral reading type for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_reading_type(const sw_snapshot_t *snapshot,
                                               uint32_t index,
                                               sw_reading_type_t *out_type);

/*
 * Return the current value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_value(const sw_snapshot_t *snapshot,
                                        uint32_t index,
                                        double *out_value);

/*
 * Return the minimum value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_minimum(const sw_snapshot_t *snapshot,
                                          uint32_t index,
                                          double *out_value);

/*
 * Return the maximum value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_maximum(const sw_snapshot_t *snapshot,
                                          uint32_t index,
                                          double *out_value);

/*
 * Return the average value for a snapshot entry.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_average(const sw_snapshot_t *snapshot,
                                          uint32_t index,
                                          double *out_value);

/*
 * Copy the source/backend name for a snapshot entry into a caller-owned buffer.
 *
 * Strings are UTF-8, NUL-terminated, and sanitized as untrusted display data.
 * out_required, when non-NULL, receives the required byte count including the
 * terminating NUL. Passing buffer == NULL and buffer_size == 0 is the length
 * query pattern and returns SW_ERR_BUFFER_TOO_SMALL when the string exists.
 *
 * Thread safety: thread-safe for a live immutable snapshot.
 */
SW_API sw_error_t sw_snapshot_get_source_name(const sw_snapshot_t *snapshot,
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
SW_API sw_error_t sw_snapshot_get_sensor_name(const sw_snapshot_t *snapshot,
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
SW_API sw_error_t sw_snapshot_get_reading_name(const sw_snapshot_t *snapshot,
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
SW_API sw_error_t sw_snapshot_get_unit(const sw_snapshot_t *snapshot,
                                       uint32_t index,
                                       char *buffer,
                                       size_t buffer_size,
                                       size_t *out_required);

#ifdef __cplusplus
}
#endif

#endif /* SENSORWATCH_SENSORWATCH_H */

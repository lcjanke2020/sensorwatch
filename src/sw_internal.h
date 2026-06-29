#ifndef SENSORWATCH_SW_INTERNAL_H
#define SENSORWATCH_SW_INTERNAL_H

/*
 * Internal declarations shared across the sensorwatch native core. Not part of
 * the public ABI -- none of this is exported. The public contract lives in
 * include/sensorwatch/sensorwatch.h; this header is the implementation's private
 * glue (structs, wire-format constants, helpers).
 *
 * Everything here is platform-independent: the pure parser (sw_parse.c) includes
 * this header and must NOT pull in <windows.h>. The Win32 session layer keeps its
 * platform types in src/sw_platform.h instead.
 */

#include "sensorwatch/sensorwatch.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* --- HWiNFO shared-memory wire format (mirrors sensorwatch/hwinfo_shm.py) --- */

/* Header magic: bytes 'H' 'W' 'i' 'S' read little-endian. */
#define SW_HEADER_MAGIC      0x53695748u
#define SW_HEADER_SIZE       48u

/* Header field byte offsets. */
#define SW_OFF_MAGIC         0x00u
#define SW_OFF_SENSOR_OFFSET 0x14u
#define SW_OFF_SENSOR_SIZE   0x18u
#define SW_OFF_SENSOR_COUNT  0x1Cu
#define SW_OFF_ENTRY_OFFSET  0x20u
#define SW_OFF_ENTRY_SIZE    0x24u
#define SW_OFF_ENTRY_COUNT   0x28u

/* Sensor element field offsets (id@0, instance@4). */
#define SW_SENSOR_OFF_NAME_ORIG 8u
#define SW_SENSOR_OFF_NAME_USER 136u
#define SW_NAME_FIELD_LEN       128u

/* Entry element field offsets (type@0, sensor_idx@4, id@8). */
#define SW_ENTRY_OFF_NAME_ORIG  12u
#define SW_ENTRY_OFF_NAME_USER  140u
#define SW_ENTRY_OFF_UNIT       268u
#define SW_UNIT_FIELD_LEN       16u
#define SW_ENTRY_OFF_VALUES     284u   /* value, min, max, avg -- 4 x double */

/* Minimum element sizes for the fields we actually read. */
#define SW_MIN_SENSOR_SIZE   264u
#define SW_MIN_ENTRY_SIZE    316u

/* Sanity caps for untrusted header fields (SECURITY.md section 1.3). */
#define SW_MAX_TOTAL_SIZE    (64u * 1024u * 1024u)
#define SW_MAX_SENSOR_COUNT  4096u
#define SW_MAX_ENTRY_COUNT   65536u

/* Defensive handle magics (docs/C_CODING_STANDARDS.md section 3). Poisoned on
   free to catch use-after-free; not a security boundary. */
#define SW_SESSION_MAGIC   0x53575345u  /* 'SWSE' */
#define SW_SNAPSHOT_MAGIC  0x5357534Eu  /* 'SWSN' */

/* The source/backend identity returned by sw_snapshot_get_source_name(). */
#define SW_SOURCE_NAME_HWINFO "HWiNFO"

/* --- Internal data model --- */

typedef struct sw_entry {
    sw_reading_type_t type;
    double value;
    double value_min;
    double value_max;
    double value_avg;
    char  *sensor_name;   /* owned, sanitized UTF-8 */
    char  *reading_name;  /* owned, sanitized UTF-8 */
    char  *unit;          /* owned, sanitized UTF-8 */
} sw_entry_t;

struct sw_snapshot {
    uint32_t    magic;
    uint32_t    entry_count;
    char       *source_name;  /* one owned copy of "HWiNFO" for the snapshot */
    sw_entry_t *entries;      /* owned array, entry_count long (NULL when 0) */
};

struct sw_session {
    uint32_t magic;
    void    *map_handle;   /* Win32 HANDLE (opaque here to stay <windows.h>-free) */
    void    *view;         /* MapViewOfFile result; copied per snapshot */
    size_t   mapped_size;  /* bounded via VirtualQuery */
};

/* --- Pure parser (sw_parse.c); no Win32, runs on every platform --- */

/*
 * Parse an HWiNFO shared-memory snapshot from an immutable byte buffer. Mirrors
 * sensorwatch.hwinfo_shm._parse_shared_memory: every header field is untrusted
 * and bounds-checked before use. Returns SW_OK and stores an owned snapshot in
 * *out_snapshot, or a specific sw_error_t (and *out_snapshot = NULL) on failure.
 */
sw_error_t sw_parse_buffer(const uint8_t *buf, size_t len,
                           sw_snapshot_t **out_snapshot);

/* Release a snapshot's owned memory (used by sw_snapshot_free and parser cleanup). */
void sw_snapshot_destroy(sw_snapshot_t *snapshot);

/* --- String helpers (sw_string.c) --- */

/*
 * Decode a fixed-width cp1252 field (NUL-terminated within field_len) into a
 * freshly malloc'd, sanitized, NUL-terminated UTF-8 string. Reproduces the
 * Python _decode(): cp1252 with replacement for the 5 undefined bytes, then C0/C1
 * control characters stripped. Returns NULL only on allocation failure.
 */
char *sw_decode_field(const uint8_t *field, size_t field_len);

/* Duplicate a NUL-terminated C string with malloc. NULL only on allocation failure. */
char *sw_dup_cstr(const char *s);

/*
 * Implement the public string-accessor buffer contract for an already-resolved
 * UTF-8 value (see the header comment on sw_snapshot_get_source_name). The
 * snapshot-NULL and index-range checks happen in the accessor before this is
 * called; this handles only the length-query / copy / invalid-combination logic.
 */
sw_error_t sw_copy_string_out(const char *value, char *buffer, size_t buffer_size,
                              size_t *out_required);

/* --- Overflow-safe size arithmetic (docs/C_CODING_STANDARDS.md section 3) --- */
/* Portable, header-free equivalents of intsafe's SizeTMult/SizeTAdd; correct on
   32-bit size_t too, so bounds checks never silently wrap. */

static inline bool sw_size_mul(size_t a, size_t b, size_t *out)
{
    if (a != 0 && b > SIZE_MAX / a) {
        return false;
    }
    *out = a * b;
    return true;
}

static inline bool sw_size_add(size_t a, size_t b, size_t *out)
{
    if (a > SIZE_MAX - b) {
        return false;
    }
    *out = a + b;
    return true;
}

#endif /* SENSORWATCH_SW_INTERNAL_H */

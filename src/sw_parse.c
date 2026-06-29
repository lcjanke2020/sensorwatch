/*
 * Pure HWiNFO shared-memory parser. No Win32 here -- this is the platform-free
 * core that sw_snapshot_take() feeds an owned copy of the mapping, and that the
 * cross-platform unit tests feed synthetic buffers. It mirrors the Python
 * reference sensorwatch.hwinfo_shm._parse_shared_memory() step for step, treating
 * every header field as untrusted input bounded before use (SECURITY.md 1.3).
 *
 * The wire format is little-endian. All supported targets (MSVC x64, MinGW x64,
 * Linux x64 CI) are little-endian, so scalars are read with a plain memcpy into a
 * correctly-typed local -- the unaligned-safe read for a packed wire layout
 * (docs/C_CODING_STANDARDS.md section 3); never a cast-and-deref.
 */

#include "sw_internal.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint32_t sw_read_u32(const uint8_t *buf, size_t off)
{
    uint32_t v;
    memcpy(&v, buf + off, sizeof(v));
    return v;
}

static double sw_read_f64(const uint8_t *buf, size_t off)
{
    double v;
    memcpy(&v, buf + off, sizeof(v));
    return v;
}

/* Map a raw source type code to the source-neutral enum. Values outside 0..8 are
   reported as UNKNOWN rather than guessed (C_ABI.md "Data Model"). */
static sw_reading_type_t sw_map_reading_type(uint32_t t)
{
    switch (t) {
    case 0u: return SW_READING_TYPE_NONE;
    case 1u: return SW_READING_TYPE_TEMPERATURE;
    case 2u: return SW_READING_TYPE_VOLTAGE;
    case 3u: return SW_READING_TYPE_FAN;
    case 4u: return SW_READING_TYPE_CURRENT;
    case 5u: return SW_READING_TYPE_POWER;
    case 6u: return SW_READING_TYPE_CLOCK;
    case 7u: return SW_READING_TYPE_USAGE;
    case 8u: return SW_READING_TYPE_OTHER;
    default: return SW_READING_TYPE_UNKNOWN;
    }
}

/* Synthetic fallback name for an entry whose sensor_idx is out of range, matching
   the Python reference's f"sensor_{sensor_idx}". NULL only on allocation failure. */
static char *sw_synthetic_sensor_name(uint32_t idx)
{
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "sensor_%u", (unsigned)idx);
    if (n < 0 || (size_t)n >= sizeof(tmp)) {
        return NULL;
    }
    return sw_dup_cstr(tmp);
}

sw_error_t sw_parse_buffer(const uint8_t *buf, size_t len,
                           sw_snapshot_t **out_snapshot)
{
    sw_error_t err = SW_ERR_INTERNAL;
    sw_snapshot_t *snap = NULL;
    char **sensor_names = NULL;
    uint32_t sensor_count = 0;

    if (buf == NULL || out_snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_snapshot = NULL;

    if (len < SW_HEADER_SIZE) {
        return SW_ERR_CORRUPT_DATA;
    }

    if (sw_read_u32(buf, SW_OFF_MAGIC) != SW_HEADER_MAGIC) {
        return SW_ERR_BAD_MAGIC;
    }

    uint32_t sensor_off  = sw_read_u32(buf, SW_OFF_SENSOR_OFFSET);
    uint32_t sensor_size = sw_read_u32(buf, SW_OFF_SENSOR_SIZE);
    uint32_t entry_off   = sw_read_u32(buf, SW_OFF_ENTRY_OFFSET);
    uint32_t entry_size  = sw_read_u32(buf, SW_OFF_ENTRY_SIZE);
    uint32_t entry_count = sw_read_u32(buf, SW_OFF_ENTRY_COUNT);
    sensor_count = sw_read_u32(buf, SW_OFF_SENSOR_COUNT);

    /* --- Validate untrusted header fields before using them as bounds (1.3) --- */
    if (sensor_size < SW_MIN_SENSOR_SIZE || entry_size < SW_MIN_ENTRY_SIZE) {
        return SW_ERR_CORRUPT_DATA;
    }
    if (sensor_count > SW_MAX_SENSOR_COUNT || entry_count > SW_MAX_ENTRY_COUNT) {
        return SW_ERR_CORRUPT_DATA;
    }
    if (sensor_off < SW_HEADER_SIZE || entry_off < SW_HEADER_SIZE) {
        /* A section starting inside the header would parse header bytes as data. */
        return SW_ERR_CORRUPT_DATA;
    }

    size_t sensor_span = 0, sensor_end = 0, entry_span = 0, entry_end = 0;
    if (!sw_size_mul((size_t)sensor_count, (size_t)sensor_size, &sensor_span) ||
        !sw_size_add((size_t)sensor_off, sensor_span, &sensor_end) ||
        !sw_size_mul((size_t)entry_count, (size_t)entry_size, &entry_span) ||
        !sw_size_add((size_t)entry_off, entry_span, &entry_end)) {
        return SW_ERR_CORRUPT_DATA;
    }
    if (sensor_end > len || entry_end > len) {
        return SW_ERR_CORRUPT_DATA;
    }
    /* The two arrays are disjoint in valid data; an overlap means a corrupt header
       would alias them. Reject rather than emit bogus readings. */
    if (sensor_off < entry_end && entry_off < sensor_end) {
        return SW_ERR_CORRUPT_DATA;
    }

    /* --- Build the sensor index -> display name table --- */
    if (sensor_count > 0) {
        sensor_names = (char **)calloc((size_t)sensor_count, sizeof(char *));
        if (sensor_names == NULL) {
            err = SW_ERR_OUT_OF_MEMORY;
            goto cleanup;
        }
        for (uint32_t i = 0; i < sensor_count; i++) {
            size_t base = (size_t)sensor_off + (size_t)i * (size_t)sensor_size;
            char *user = sw_decode_field(buf + base + SW_SENSOR_OFF_NAME_USER, SW_NAME_FIELD_LEN);
            char *orig = sw_decode_field(buf + base + SW_SENSOR_OFF_NAME_ORIG, SW_NAME_FIELD_LEN);
            if (user == NULL || orig == NULL) {
                free(user);
                free(orig);
                err = SW_ERR_OUT_OF_MEMORY;
                goto cleanup;
            }
            /* name_user or name_orig (Python: `name_user or name_orig`). */
            if (user[0] != '\0') {
                sensor_names[i] = user;
                free(orig);
            } else {
                sensor_names[i] = orig;
                free(user);
            }
        }
    }

    /* --- Allocate the snapshot and its source identity --- */
    snap = (sw_snapshot_t *)calloc(1, sizeof(*snap));
    if (snap == NULL) {
        err = SW_ERR_OUT_OF_MEMORY;
        goto cleanup;
    }
    snap->magic = SW_SNAPSHOT_MAGIC;
    snap->entry_count = entry_count;
    snap->source_name = sw_dup_cstr(SW_SOURCE_NAME_HWINFO);
    if (snap->source_name == NULL) {
        err = SW_ERR_OUT_OF_MEMORY;
        goto cleanup;
    }

    if (entry_count > 0) {
        snap->entries = (sw_entry_t *)calloc((size_t)entry_count, sizeof(sw_entry_t));
        if (snap->entries == NULL) {
            err = SW_ERR_OUT_OF_MEMORY;
            goto cleanup;
        }
    }

    /* --- Parse entries into snapshot-owned data --- */
    for (uint32_t i = 0; i < entry_count; i++) {
        size_t base = (size_t)entry_off + (size_t)i * (size_t)entry_size;

        uint32_t etype = sw_read_u32(buf, base + 0u);
        uint32_t sidx  = sw_read_u32(buf, base + 4u);

        char *rorig = sw_decode_field(buf + base + SW_ENTRY_OFF_NAME_ORIG, SW_NAME_FIELD_LEN);
        char *ruser = sw_decode_field(buf + base + SW_ENTRY_OFF_NAME_USER, SW_NAME_FIELD_LEN);
        char *unit  = sw_decode_field(buf + base + SW_ENTRY_OFF_UNIT, SW_UNIT_FIELD_LEN);
        if (rorig == NULL || ruser == NULL || unit == NULL) {
            free(rorig);
            free(ruser);
            free(unit);
            err = SW_ERR_OUT_OF_MEMORY;
            goto cleanup;
        }

        sw_entry_t *e = &snap->entries[i];
        e->type      = sw_map_reading_type(etype);
        e->value     = sw_read_f64(buf, base + SW_ENTRY_OFF_VALUES + 0u);
        e->value_min = sw_read_f64(buf, base + SW_ENTRY_OFF_VALUES + 8u);
        e->value_max = sw_read_f64(buf, base + SW_ENTRY_OFF_VALUES + 16u);
        e->value_avg = sw_read_f64(buf, base + SW_ENTRY_OFF_VALUES + 24u);
        e->unit      = unit;

        /* reading_user or reading_orig. */
        if (ruser[0] != '\0') {
            e->reading_name = ruser;
            free(rorig);
        } else {
            e->reading_name = rorig;
            free(ruser);
        }

        /* Resolve the sensor name. A valid index uses the table entry (even if it
           decoded to ""); an out-of-range index gets the synthetic fallback. */
        if (sidx < sensor_count) {
            e->sensor_name = sw_dup_cstr(sensor_names[sidx]);
        } else {
            e->sensor_name = sw_synthetic_sensor_name(sidx);
        }
        if (e->sensor_name == NULL) {
            err = SW_ERR_OUT_OF_MEMORY;
            goto cleanup;
        }
    }

    /* Table no longer needed; entries hold their own copies. */
    for (uint32_t i = 0; i < sensor_count; i++) {
        free(sensor_names[i]);
    }
    free(sensor_names);

    *out_snapshot = snap;
    return SW_OK;

cleanup:
    if (sensor_names != NULL) {
        for (uint32_t i = 0; i < sensor_count; i++) {
            free(sensor_names[i]);
        }
        free(sensor_names);
    }
    sw_snapshot_destroy(snap);  /* NULL-safe; frees any partially-built entries */
    return err;
}

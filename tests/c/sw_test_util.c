#include "sw_test_util.h"

#include "sw_internal.h"  /* SW_* wire-format constants and field offsets */

#include <stdlib.h>
#include <string.h>

static void put_u32(uint8_t *buf, size_t off, uint32_t v)
{
    memcpy(buf + off, &v, sizeof(v));
}

static void put_f64(uint8_t *buf, size_t off, double v)
{
    memcpy(buf + off, &v, sizeof(v));
}

/* Copy a C string into a fixed-width, zero-padded field (caller pre-zeroed it). */
static void pack_name(uint8_t *dst, size_t field_len, const char *src)
{
    if (src == NULL) {
        return;  /* leave the field as zeroes (empty string) */
    }
    size_t n = strlen(src);
    if (n > field_len - 1) {
        n = field_len - 1;  /* leave room for at least one terminating NUL */
    }
    memcpy(dst, src, n);
}

uint8_t *sw_test_build_buffer(const sw_test_sensor_t *sensors, uint32_t sensor_count,
                              const sw_test_entry_t *entries, uint32_t entry_count,
                              size_t *out_len)
{
    const uint32_t sensor_size = SW_MIN_SENSOR_SIZE;
    const uint32_t entry_size  = SW_MIN_ENTRY_SIZE;
    const uint32_t sensor_off  = SW_HEADER_SIZE;

    size_t sensor_region = (size_t)sensor_count * sensor_size;
    size_t entry_region  = (size_t)entry_count * entry_size;
    size_t entry_off     = (size_t)sensor_off + sensor_region;
    size_t total         = entry_off + entry_region;
    if (total < SW_HEADER_SIZE) {
        total = SW_HEADER_SIZE;
    }

    uint8_t *buf = (uint8_t *)calloc(1, total);
    if (buf == NULL) {
        return NULL;
    }

    put_u32(buf, SW_OFF_MAGIC,         SW_HEADER_MAGIC);
    put_u32(buf, SW_OFF_SENSOR_OFFSET, sensor_off);
    put_u32(buf, SW_OFF_SENSOR_SIZE,   sensor_size);
    put_u32(buf, SW_OFF_SENSOR_COUNT,  sensor_count);
    put_u32(buf, SW_OFF_ENTRY_OFFSET,  (uint32_t)entry_off);
    put_u32(buf, SW_OFF_ENTRY_SIZE,    entry_size);
    put_u32(buf, SW_OFF_ENTRY_COUNT,   entry_count);

    for (uint32_t i = 0; i < sensor_count; i++) {
        size_t base = (size_t)sensor_off + (size_t)i * sensor_size;
        put_u32(buf, base + 0u, i);  /* id */
        put_u32(buf, base + 4u, 0u); /* instance */
        pack_name(buf + base + SW_SENSOR_OFF_NAME_ORIG, SW_NAME_FIELD_LEN, sensors[i].name_orig);
        pack_name(buf + base + SW_SENSOR_OFF_NAME_USER, SW_NAME_FIELD_LEN, sensors[i].name_user);
    }

    for (uint32_t i = 0; i < entry_count; i++) {
        size_t base = entry_off + (size_t)i * entry_size;
        put_u32(buf, base + 0u, entries[i].type);
        put_u32(buf, base + 4u, entries[i].sensor_idx);
        put_u32(buf, base + 8u, i);  /* id */
        pack_name(buf + base + SW_ENTRY_OFF_NAME_ORIG, SW_NAME_FIELD_LEN, entries[i].reading_orig);
        pack_name(buf + base + SW_ENTRY_OFF_NAME_USER, SW_NAME_FIELD_LEN, entries[i].reading_user);
        pack_name(buf + base + SW_ENTRY_OFF_UNIT,      SW_UNIT_FIELD_LEN, entries[i].unit);
        double v = entries[i].value;
        put_f64(buf, base + SW_ENTRY_OFF_VALUES + 0u,  v);
        put_f64(buf, base + SW_ENTRY_OFF_VALUES + 8u,  v);
        put_f64(buf, base + SW_ENTRY_OFF_VALUES + 16u, v);
        put_f64(buf, base + SW_ENTRY_OFF_VALUES + 24u, v);
    }

    *out_len = total;
    return buf;
}

void sw_test_patch_u32(uint8_t *buf, size_t offset, uint32_t value)
{
    memcpy(buf + offset, &value, sizeof(value));
}

#ifndef SW_TEST_UTIL_H
#define SW_TEST_UTIL_H

/*
 * Synthetic HWiNFO shared-memory buffer builder -- the C counterpart of the
 * _build_buffer / _patch_u32 helpers in tests/test_hwinfo_shm.py. Lets the unit
 * tests exercise the parser with no Win32 and no live HWiNFO.
 */

#include <stddef.h>
#include <stdint.h>

typedef struct sw_test_sensor {
    const char *name_user;   /* written to the name_user field (NULL = empty) */
    const char *name_orig;   /* written to the name_original field (NULL = empty) */
} sw_test_sensor_t;

typedef struct sw_test_entry {
    uint32_t    type;
    uint32_t    sensor_idx;
    const char *reading_user;
    const char *reading_orig;
    const char *unit;
    double      value;       /* written to value / min / max / avg */
} sw_test_entry_t;

/*
 * Build a well-formed buffer. Returns a malloc'd buffer (caller frees) with its
 * length in *out_len, or NULL on allocation failure. Name fields are written as
 * raw bytes, so tests may embed cp1252 bytes (e.g. "\xB0" "C") or control
 * characters directly in the const char * fields.
 */
uint8_t *sw_test_build_buffer(const sw_test_sensor_t *sensors, uint32_t sensor_count,
                              const sw_test_entry_t *entries, uint32_t entry_count,
                              size_t *out_len);

/* Patch a little-endian uint32 at a byte offset (mirrors pytest's _patch_u32). */
void sw_test_patch_u32(uint8_t *buf, size_t offset, uint32_t value);

#endif /* SW_TEST_UTIL_H */

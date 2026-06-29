/*
 * Parser unit tests -- the C mirror of tests/test_hwinfo_shm.py. A well-formed
 * synthetic buffer must parse correctly; every malformed header must return a
 * specific error (and a NULL snapshot) instead of crashing. Cross-platform: these
 * drive the pure sw_parse_buffer() directly, no Win32.
 */

#include <stdarg.h>
#include <stddef.h>
#include <setjmp.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <cmocka.h>

#include "sw_internal.h"
#include "sw_test_util.h"

static uint8_t *valid_buffer(size_t *len)
{
    sw_test_sensor_t sensors[] = { { "MEG Ai1600T", NULL } };
    sw_test_entry_t  entries[] = { { 2u, 0u, "+12V", NULL, "V", 12.03 } };  /* type 2 = Voltage */
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, len);
    assert_non_null(buf);
    return buf;
}

static sw_snapshot_t *parse_ok(uint8_t *buf, size_t len)
{
    sw_snapshot_t *snap = NULL;
    assert_int_equal(sw_parse_buffer(buf, len, &snap), SW_OK);
    assert_non_null(snap);
    return snap;
}

static void expect_err(uint8_t *buf, size_t len, sw_error_t want)
{
    sw_snapshot_t *snap = (sw_snapshot_t *)0x1;  /* must be overwritten with NULL */
    assert_int_equal(sw_parse_buffer(buf, len, &snap), want);
    assert_null(snap);
}

/* --- Happy path --- */

static void test_valid_buffer_parses_reading(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_int_equal(snap->entry_count, 1u);
    const sw_entry_t *e = &snap->entries[0];
    assert_string_equal(snap->source_name, "HWiNFO");
    assert_string_equal(e->sensor_name, "MEG Ai1600T");
    assert_string_equal(e->reading_name, "+12V");
    assert_int_equal(e->type, SW_READING_TYPE_VOLTAGE);
    assert_string_equal(e->unit, "V");
    assert_true(e->value == 12.03);
    assert_true(e->value_min == 12.03);
    assert_true(e->value_max == 12.03);
    assert_true(e->value_avg == 12.03);

    sw_snapshot_free(snap);
    free(buf);
}

static void test_unit_decoded_as_cp1252(void **state)
{
    (void)state;
    /* HWiNFO writes "degC" as cp1252 byte 0xB0 ('degree') + 'C'. */
    sw_test_sensor_t sensors[] = { { "CPU", NULL } };
    sw_test_entry_t  entries[] = { { 1u, 0u, "Core", NULL, "\xB0" "C", 42.0 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_string_equal(snap->entries[0].unit, "\xC2\xB0" "C");  /* U+00B0 in UTF-8 */

    sw_snapshot_free(snap);
    free(buf);
}

static void test_invalid_sensor_idx_falls_back(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "MEG Ai1600T", NULL } };
    sw_test_entry_t  entries[] = { { 2u, 5u, "+12V", NULL, "V", 12.0 } };  /* idx 5, only 1 sensor */
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_string_equal(snap->entries[0].sensor_name, "sensor_5");

    sw_snapshot_free(snap);
    free(buf);
}

static void test_falls_back_to_original_names(void **state)
{
    (void)state;
    /* Blank user-customizable names -> use the original names. */
    sw_test_sensor_t sensors[] = { { NULL, "CPU [#0]" } };
    sw_test_entry_t  entries[] = { { 1u, 0u, NULL, "Core 0", "C", 30.0 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_string_equal(snap->entries[0].sensor_name, "CPU [#0]");
    assert_string_equal(snap->entries[0].reading_name, "Core 0");

    sw_snapshot_free(snap);
    free(buf);
}

static void test_control_chars_stripped(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "GPU\x01", NULL } };
    sw_test_entry_t  entries[] = { { 1u, 0u, "Hot\x1f" "Spot", NULL, "C", 55.0 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_string_equal(snap->entries[0].sensor_name, "GPU");
    assert_string_equal(snap->entries[0].reading_name, "HotSpot");

    sw_snapshot_free(snap);
    free(buf);
}

static void test_reading_type_none_and_unknown(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "S", NULL } };
    sw_test_entry_t  entries[] = {
        { 0u,  0u, "a", NULL, "", 1.0 },   /* type 0   -> NONE */
        { 99u, 0u, "b", NULL, "", 2.0 },   /* type 99  -> UNKNOWN */
    };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 2u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_int_equal(snap->entries[0].type, SW_READING_TYPE_NONE);
    assert_int_equal(snap->entries[1].type, SW_READING_TYPE_UNKNOWN);

    sw_snapshot_free(snap);
    free(buf);
}

static void test_zero_entries_is_valid_empty(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "S", NULL } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, NULL, 0u, &len);
    sw_snapshot_t *snap = parse_ok(buf, len);

    assert_int_equal(snap->entry_count, 0u);

    sw_snapshot_free(snap);
    free(buf);
}

/* --- Malformed headers all return a specific error and a NULL snapshot --- */

static void test_buffer_smaller_than_header(void **state)
{
    (void)state;
    uint8_t small[SW_HEADER_SIZE - 1];
    memset(small, 0, sizeof(small));
    expect_err(small, sizeof(small), SW_ERR_CORRUPT_DATA);
}

static void test_empty_buffer(void **state)
{
    (void)state;
    uint8_t dummy = 0;
    expect_err(&dummy, 0u, SW_ERR_CORRUPT_DATA);
}

static void test_null_buffer_is_null_pointer(void **state)
{
    (void)state;
    sw_snapshot_t *snap = (sw_snapshot_t *)0x1;
    assert_int_equal(sw_parse_buffer(NULL, 100u, &snap), SW_ERR_NULL_POINTER);
}

static void test_bad_magic(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_MAGIC, 0xDEADBEEFu);
    expect_err(buf, len, SW_ERR_BAD_MAGIC);
    free(buf);
}

static void test_sensor_size_below_minimum(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_SENSOR_SIZE, SW_MIN_SENSOR_SIZE - 1u);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_entry_size_below_minimum(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_ENTRY_SIZE, SW_MIN_ENTRY_SIZE - 1u);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_sensor_count_above_max(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_SENSOR_COUNT, SW_MAX_SENSOR_COUNT + 1u);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_entry_count_above_max(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_ENTRY_COUNT, SW_MAX_ENTRY_COUNT + 1u);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_section_offset_overlaps_header(void **state)
{
    (void)state;
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_SENSOR_OFFSET, SW_HEADER_SIZE - 1u);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_sections_exceed_region(void **state)
{
    (void)state;
    /* A count within the sanity cap but too large for the buffer must be caught by
       the region-bounds check, not by reading past the end. */
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_SENSOR_COUNT, SW_MAX_SENSOR_COUNT);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

static void test_overlapping_sections(void **state)
{
    (void)state;
    /* Point the entry section back into the sensor section: both stay within the
       buffer and past the header, so only the overlap guard catches this. */
    size_t len;
    uint8_t *buf = valid_buffer(&len);
    sw_test_patch_u32(buf, SW_OFF_ENTRY_OFFSET, SW_HEADER_SIZE);
    expect_err(buf, len, SW_ERR_CORRUPT_DATA);
    free(buf);
}

int main(void)
{
    const struct CMUnitTest tests[] = {
        cmocka_unit_test(test_valid_buffer_parses_reading),
        cmocka_unit_test(test_unit_decoded_as_cp1252),
        cmocka_unit_test(test_invalid_sensor_idx_falls_back),
        cmocka_unit_test(test_falls_back_to_original_names),
        cmocka_unit_test(test_control_chars_stripped),
        cmocka_unit_test(test_reading_type_none_and_unknown),
        cmocka_unit_test(test_zero_entries_is_valid_empty),
        cmocka_unit_test(test_buffer_smaller_than_header),
        cmocka_unit_test(test_empty_buffer),
        cmocka_unit_test(test_null_buffer_is_null_pointer),
        cmocka_unit_test(test_bad_magic),
        cmocka_unit_test(test_sensor_size_below_minimum),
        cmocka_unit_test(test_entry_size_below_minimum),
        cmocka_unit_test(test_sensor_count_above_max),
        cmocka_unit_test(test_entry_count_above_max),
        cmocka_unit_test(test_section_offset_overlaps_header),
        cmocka_unit_test(test_sections_exceed_region),
        cmocka_unit_test(test_overlapping_sections),
    };
    return cmocka_run_group_tests(tests, NULL, NULL);
}

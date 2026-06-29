/*
 * Snapshot accessor tests: the caller-buffer string contract (length query, exact
 * fit, too-small, invalid combinations, NULL/index validation) and the scalar /
 * enum accessors. Cross-platform: builds a snapshot via the pure parser.
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

/* One-entry snapshot: sensor "MEG Ai1600T", reading "+12V", unit "V", Voltage. */
static sw_snapshot_t *make_snapshot(void)
{
    sw_test_sensor_t sensors[] = { { "MEG Ai1600T", NULL } };
    sw_test_entry_t  entries[] = { { 2u, 0u, "+12V", NULL, "V", 12.5 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    assert_non_null(buf);
    sw_snapshot_t *snap = NULL;
    assert_int_equal(sw_parse_buffer(buf, len, &snap), SW_OK);
    assert_non_null(snap);
    free(buf);
    return snap;
}

/* --- Scalar / enum accessors --- */

static void test_entry_count(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    uint32_t count = 0;
    assert_int_equal(sw_snapshot_entry_count(snap, &count), SW_OK);
    assert_int_equal(count, 1u);
    sw_snapshot_free(snap);
}

static void test_value_accessors(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    double v = 0.0;
    assert_int_equal(sw_snapshot_get_value(snap, 0u, &v), SW_OK);
    assert_true(v == 12.5);
    assert_int_equal(sw_snapshot_get_minimum(snap, 0u, &v), SW_OK);
    assert_true(v == 12.5);
    assert_int_equal(sw_snapshot_get_maximum(snap, 0u, &v), SW_OK);
    assert_true(v == 12.5);
    assert_int_equal(sw_snapshot_get_average(snap, 0u, &v), SW_OK);
    assert_true(v == 12.5);

    sw_reading_type_t t = SW_READING_TYPE_UNKNOWN;
    assert_int_equal(sw_snapshot_get_reading_type(snap, 0u, &t), SW_OK);
    assert_int_equal(t, SW_READING_TYPE_VOLTAGE);
    sw_snapshot_free(snap);
}

static void test_scalar_null_and_range(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    uint32_t count = 0;
    double v = 0.0;

    assert_int_equal(sw_snapshot_entry_count(NULL, &count), SW_ERR_NULL_POINTER);
    assert_int_equal(sw_snapshot_entry_count(snap, NULL), SW_ERR_NULL_POINTER);
    assert_int_equal(sw_snapshot_get_value(snap, 0u, NULL), SW_ERR_NULL_POINTER);
    assert_int_equal(sw_snapshot_get_value(NULL, 0u, &v), SW_ERR_NULL_POINTER);
    assert_int_equal(sw_snapshot_get_value(snap, 1u, &v), SW_ERR_INDEX_OUT_OF_RANGE);

    sw_snapshot_free(snap);
}

/* --- String accessor buffer contract --- */

static void test_string_length_query(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    size_t required = 0;
    /* "MEG Ai1600T" is 11 bytes + NUL = 12. */
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, NULL, 0u, &required),
                     SW_ERR_BUFFER_TOO_SMALL);
    assert_int_equal(required, 12u);
    sw_snapshot_free(snap);
}

static void test_string_length_query_requires_out(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, NULL, 0u, NULL),
                     SW_ERR_NULL_POINTER);
    sw_snapshot_free(snap);
}

static void test_string_exact_fit(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[12];
    size_t required = 0;
    memset(buffer, 'x', sizeof(buffer));
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, buffer, sizeof(buffer), &required),
                     SW_OK);
    assert_string_equal(buffer, "MEG Ai1600T");
    assert_int_equal(required, 12u);
    sw_snapshot_free(snap);
}

static void test_string_copy_without_required(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[16] = {0};
    /* out_required may be NULL in the copy form. */
    assert_int_equal(sw_snapshot_get_unit(snap, 0u, buffer, sizeof(buffer), NULL), SW_OK);
    assert_string_equal(buffer, "V");
    sw_snapshot_free(snap);
}

static void test_string_too_small(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[4];
    size_t required = 0;
    memset(buffer, 'x', sizeof(buffer));
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, buffer, sizeof(buffer), &required),
                     SW_ERR_BUFFER_TOO_SMALL);
    assert_int_equal(buffer[0], '\0');  /* empty, never a partial sequence */
    assert_int_equal(required, 12u);
    sw_snapshot_free(snap);
}

static void test_string_invalid_combinations(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[8] = {0};
    size_t required = 0;
    /* buffer == NULL with size > 0 */
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, NULL, 8u, &required),
                     SW_ERR_INVALID_ARGUMENT);
    /* buffer != NULL with size == 0 */
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 0u, buffer, 0u, &required),
                     SW_ERR_INVALID_ARGUMENT);
    sw_snapshot_free(snap);
}

static void test_string_null_and_range(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[16] = {0};
    size_t required = 0;
    assert_int_equal(sw_snapshot_get_sensor_name(NULL, 0u, buffer, sizeof(buffer), &required),
                     SW_ERR_NULL_POINTER);
    assert_int_equal(sw_snapshot_get_sensor_name(snap, 1u, buffer, sizeof(buffer), &required),
                     SW_ERR_INDEX_OUT_OF_RANGE);
    sw_snapshot_free(snap);
}

static void test_source_name(void **state)
{
    (void)state;
    sw_snapshot_t *snap = make_snapshot();
    char buffer[16] = {0};
    assert_int_equal(sw_snapshot_get_source_name(snap, 0u, buffer, sizeof(buffer), NULL), SW_OK);
    assert_string_equal(buffer, "HWiNFO");
    sw_snapshot_free(snap);
}

int main(void)
{
    const struct CMUnitTest tests[] = {
        cmocka_unit_test(test_entry_count),
        cmocka_unit_test(test_value_accessors),
        cmocka_unit_test(test_scalar_null_and_range),
        cmocka_unit_test(test_string_length_query),
        cmocka_unit_test(test_string_length_query_requires_out),
        cmocka_unit_test(test_string_exact_fit),
        cmocka_unit_test(test_string_copy_without_required),
        cmocka_unit_test(test_string_too_small),
        cmocka_unit_test(test_string_invalid_combinations),
        cmocka_unit_test(test_string_null_and_range),
        cmocka_unit_test(test_source_name),
    };
    return cmocka_run_group_tests(tests, NULL, NULL);
}

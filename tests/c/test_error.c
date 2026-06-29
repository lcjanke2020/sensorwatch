/*
 * Version and error-string coverage. Cross-platform; no parser involved.
 */

#include <stdarg.h>
#include <stddef.h>
#include <setjmp.h>
#include <stdint.h>
#include <string.h>
#include <cmocka.h>

#include "sensorwatch/sensorwatch.h"

static void test_api_version(void **state)
{
    (void)state;
    /* 0.1.0 -> 0*10000 + 1*100 + 0 == 100. */
    assert_int_equal(sw_api_version(), SW_API_VERSION);
    assert_int_equal(sw_api_version(), 100u);
}

static void test_error_string_never_null(void **state)
{
    (void)state;
    const sw_error_t codes[] = {
        SW_OK, SW_ERR_NULL_POINTER, SW_ERR_INVALID_ARGUMENT,
        SW_ERR_UNSUPPORTED_PLATFORM, SW_ERR_SOURCE_UNAVAILABLE, SW_ERR_MAP_FAILED,
        SW_ERR_BAD_MAGIC, SW_ERR_CORRUPT_DATA, SW_ERR_OUT_OF_MEMORY,
        SW_ERR_INDEX_OUT_OF_RANGE, SW_ERR_BUFFER_TOO_SMALL, SW_ERR_VERSION_MISMATCH,
        SW_ERR_INTERNAL,
    };
    for (size_t i = 0; i < sizeof(codes) / sizeof(codes[0]); i++) {
        const char *msg = sw_error_string(codes[i]);
        assert_non_null(msg);
        assert_true(strlen(msg) > 0);
    }
    /* SW_OK has a distinct, sensible message. */
    assert_string_equal(sw_error_string(SW_OK), "Success");
}

static void test_error_string_out_of_range(void **state)
{
    (void)state;
    /* Values outside the defined enum must still return a non-NULL string. */
    assert_non_null(sw_error_string((sw_error_t)123));
    assert_non_null(sw_error_string((sw_error_t)-999));
    assert_string_equal(sw_error_string((sw_error_t)123), "Unknown error");
}

int main(void)
{
    const struct CMUnitTest tests[] = {
        cmocka_unit_test(test_api_version),
        cmocka_unit_test(test_error_string_never_null),
        cmocka_unit_test(test_error_string_out_of_range),
    };
    return cmocka_run_group_tests(tests, NULL, NULL);
}

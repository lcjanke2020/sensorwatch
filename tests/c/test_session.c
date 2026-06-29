/*
 * Win32 session-layer tests (Windows only). The kernel32 mapping calls are
 * swapped for mocks via the sw_platform table (docs/C_CODING_STANDARDS.md
 * section 9), so these run without HWiNFO and without touching real shared memory.
 */

#include <stdarg.h>
#include <stddef.h>
#include <setjmp.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <cmocka.h>

#include "sensorwatch/sensorwatch.h"
#include "sw_platform.h"   /* pulls in <windows.h>; Windows-only */
#include "sw_internal.h"
#include "sw_test_util.h"

static HANDLE WINAPI mock_open(DWORD access, BOOL inherit, LPCWSTR name)
{
    (void)access; (void)inherit; (void)name;
    return (HANDLE)(uintptr_t)mock();
}

static LPVOID WINAPI mock_map(HANDLE h, DWORD access, DWORD hi, DWORD lo, SIZE_T bytes)
{
    (void)h; (void)access; (void)hi; (void)lo; (void)bytes;
    return (LPVOID)(uintptr_t)mock();
}

static SIZE_T WINAPI mock_vq(LPCVOID addr, PMEMORY_BASIC_INFORMATION mbi, SIZE_T mbi_size)
{
    (void)addr;
    if (mbi == NULL || mbi_size < sizeof(*mbi)) {
        return 0;  /* mirror VirtualQuery's failure contract */
    }
    memset(mbi, 0, sizeof(*mbi));
    mbi->RegionSize = (SIZE_T)mock();
    return sizeof(*mbi);
}

static BOOL WINAPI mock_unmap(LPCVOID base) { (void)base; return TRUE; }
static BOOL WINAPI mock_close(HANDLE h)     { (void)h;    return TRUE; }

static void install_mocks(void)
{
    sw_platform.open_file_mapping  = mock_open;
    sw_platform.map_view_of_file   = mock_map;
    sw_platform.virtual_query      = mock_vq;
    sw_platform.unmap_view_of_file = mock_unmap;
    sw_platform.close_handle       = mock_close;
}

static void test_open_and_snapshot_valid(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "MEG Ai1600T", NULL } };
    sw_test_entry_t  entries[] = { { 2u, 0u, "+12V", NULL, "V", 12.03 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    assert_non_null(buf);

    install_mocks();
    will_return(mock_open, (uintptr_t)0xDEAD);
    will_return(mock_map, (uintptr_t)buf);
    will_return(mock_vq, (SIZE_T)len);

    sw_session_t *session = NULL;
    assert_int_equal(sw_session_open(&session), SW_OK);
    assert_non_null(session);

    sw_snapshot_t *snap = NULL;
    assert_int_equal(sw_snapshot_take(session, &snap), SW_OK);
    assert_non_null(snap);

    uint32_t count = 0;
    assert_int_equal(sw_snapshot_entry_count(snap, &count), SW_OK);
    assert_int_equal(count, 1u);

    sw_snapshot_free(snap);
    sw_session_close(session);
    free(buf);
}

static void test_open_source_unavailable(void **state)
{
    (void)state;
    install_mocks();
    will_return(mock_open, (uintptr_t)0);  /* OpenFileMappingW -> NULL */

    sw_session_t *session = (sw_session_t *)0x1;
    assert_int_equal(sw_session_open(&session), SW_ERR_SOURCE_UNAVAILABLE);
    assert_null(session);
}

static void test_open_rejects_undersized_region(void **state)
{
    (void)state;
    static uint8_t tiny[8] = {0};  /* < SW_HEADER_SIZE */

    install_mocks();
    will_return(mock_open, (uintptr_t)0xDEAD);
    will_return(mock_map, (uintptr_t)tiny);
    will_return(mock_vq, (SIZE_T)sizeof(tiny));

    sw_session_t *session = (sw_session_t *)0x1;
    assert_int_equal(sw_session_open(&session), SW_ERR_CORRUPT_DATA);
    assert_null(session);
}

static void test_open_bad_magic(void **state)
{
    (void)state;
    sw_test_sensor_t sensors[] = { { "S", NULL } };
    sw_test_entry_t  entries[] = { { 1u, 0u, "r", NULL, "C", 1.0 } };
    size_t len;
    uint8_t *buf = sw_test_build_buffer(sensors, 1u, entries, 1u, &len);
    assert_non_null(buf);
    sw_test_patch_u32(buf, SW_OFF_MAGIC, 0xDEADBEEFu);

    install_mocks();
    will_return(mock_open, (uintptr_t)0xDEAD);
    will_return(mock_map, (uintptr_t)buf);
    will_return(mock_vq, (SIZE_T)len);

    sw_session_t *session = (sw_session_t *)0x1;
    assert_int_equal(sw_session_open(&session), SW_ERR_BAD_MAGIC);
    assert_null(session);
    free(buf);
}

static void test_open_null_out(void **state)
{
    (void)state;
    assert_int_equal(sw_session_open(NULL), SW_ERR_NULL_POINTER);
}

int main(void)
{
    const struct CMUnitTest tests[] = {
        cmocka_unit_test(test_open_and_snapshot_valid),
        cmocka_unit_test(test_open_source_unavailable),
        cmocka_unit_test(test_open_rejects_undersized_region),
        cmocka_unit_test(test_open_bad_magic),
        cmocka_unit_test(test_open_null_out),
    };
    return cmocka_run_group_tests(tests, NULL, NULL);
}

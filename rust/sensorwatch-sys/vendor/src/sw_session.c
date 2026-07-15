/*
 * Session lifecycle and snapshot acquisition.
 *
 * On Windows this opens HWiNFO's shared memory read-only, bounds the mapping with
 * VirtualQuery, and -- on each snapshot -- copies the bounded region into owned
 * memory before handing it to the pure parser (never parsing the live view). This
 * is the copy-then-parse model required by SECURITY.md 1.3. The Win32 calls go
 * through the mockable sw_platform table.
 *
 * On every other platform the source is unavailable, so the lifecycle functions
 * report SW_ERR_UNSUPPORTED_PLATFORM and the library still builds and links (the
 * pure parser and its tests run everywhere).
 */

#include "sw_internal.h"

#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)

#include "sw_platform.h"

/*
 * Thin forwarders to the real kernel32 calls. The default ops table holds the
 * address of these (our own, non-import) functions rather than the Win32 symbols
 * directly: taking the address of a __declspec(dllimport) function in a static
 * initializer trips MSVC C4232 ("identity not guaranteed").
 */
static HANDLE WINAPI sw_real_open_file_mapping(DWORD access, BOOL inherit, LPCWSTR name)
{
    return OpenFileMappingW(access, inherit, name);
}

static LPVOID WINAPI sw_real_map_view_of_file(HANDLE h, DWORD access,
                                              DWORD hi, DWORD lo, SIZE_T bytes)
{
    return MapViewOfFile(h, access, hi, lo, bytes);
}

static SIZE_T WINAPI sw_real_virtual_query(LPCVOID addr, PMEMORY_BASIC_INFORMATION mbi,
                                           SIZE_T mbi_size)
{
    return VirtualQuery(addr, mbi, mbi_size);
}

static BOOL WINAPI sw_real_unmap_view_of_file(LPCVOID base)
{
    return UnmapViewOfFile(base);
}

static BOOL WINAPI sw_real_close_handle(HANDLE h)
{
    return CloseHandle(h);
}

sw_platform_ops_t sw_platform = {
    sw_real_open_file_mapping,
    sw_real_map_view_of_file,
    sw_real_virtual_query,
    sw_real_unmap_view_of_file,
    sw_real_close_handle,
};

/* HWiNFO's documented global mapping name (SECURITY.md 1.4: always Global\). */
#define SW_SHM_NAME L"Global\\HWiNFO_SENS_SM2"

SW_API sw_error_t SW_CALL sw_session_open(sw_session_t **out_session)
{
    sw_error_t     err        = SW_ERR_INTERNAL;
    HANDLE         map_handle = NULL;
    LPVOID         view       = NULL;
    sw_session_t  *session    = NULL;

    if (out_session == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_session = NULL;

    map_handle = sw_platform.open_file_mapping(FILE_MAP_READ, FALSE, SW_SHM_NAME);
    if (map_handle == NULL) {
        err = SW_ERR_SOURCE_UNAVAILABLE;  /* HWiNFO not running / SHM disabled */
        goto cleanup;
    }

    view = sw_platform.map_view_of_file(map_handle, FILE_MAP_READ, 0, 0, 0);
    if (view == NULL) {
        err = SW_ERR_MAP_FAILED;
        goto cleanup;
    }

    /* Bound the mapping before touching any field: a hostile producer may map
       fewer bytes than a full header. */
    MEMORY_BASIC_INFORMATION mbi;
    memset(&mbi, 0, sizeof(mbi));
    if (sw_platform.virtual_query(view, &mbi, sizeof(mbi)) == 0 ||
        mbi.RegionSize < SW_HEADER_SIZE) {
        err = SW_ERR_CORRUPT_DATA;
        goto cleanup;
    }

    /* Validate magic early (now known to lie within the mapping). memcpy is the
       unaligned-safe read for a packed wire layout. */
    uint32_t magic = 0;
    memcpy(&magic, view, sizeof(magic));
    if (magic != SW_HEADER_MAGIC) {
        err = SW_ERR_BAD_MAGIC;
        goto cleanup;
    }

    session = (sw_session_t *)calloc(1, sizeof(*session));
    if (session == NULL) {
        err = SW_ERR_OUT_OF_MEMORY;
        goto cleanup;
    }
    session->magic       = SW_SESSION_MAGIC;
    session->map_handle  = map_handle;
    session->view        = view;
    session->mapped_size = (size_t)mbi.RegionSize;

    *out_session = session;  /* transfer ownership */
    return SW_OK;

cleanup:
    if (view != NULL) {
        sw_platform.unmap_view_of_file(view);
    }
    if (map_handle != NULL) {
        sw_platform.close_handle(map_handle);
    }
    return err;
}

SW_API void SW_CALL sw_session_close(sw_session_t *session)
{
    if (session == NULL) {
        return;
    }
    if (session->view != NULL) {
        sw_platform.unmap_view_of_file(session->view);
    }
    if (session->map_handle != NULL) {
        sw_platform.close_handle((HANDLE)session->map_handle);
    }
    session->magic = 0;  /* poison the freed handle (debug breadcrumb; not read back) */
    free(session);
}

SW_API sw_error_t SW_CALL sw_snapshot_take(sw_session_t *session,
                                           sw_snapshot_t **out_snapshot)
{
    /* Out-pointer first, then poison, then the session check: every failure
       path must leave *out_snapshot NULL (the "NULL on failure when possible"
       contract), including a NULL session. */
    if (out_snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_snapshot = NULL;
    if (session == NULL) {
        return SW_ERR_NULL_POINTER;
    }

    size_t copy_size = session->mapped_size;
    if (copy_size > SW_MAX_TOTAL_SIZE) {
        copy_size = SW_MAX_TOTAL_SIZE;  /* cap worst-case work (SECURITY.md 1.3) */
    }
    if (copy_size < SW_HEADER_SIZE) {
        return SW_ERR_CORRUPT_DATA;
    }

    uint8_t *buf = (uint8_t *)malloc(copy_size);
    if (buf == NULL) {
        return SW_ERR_OUT_OF_MEMORY;
    }
    /* Single copy of the live mapping; the parser only ever sees this snapshot. */
    memcpy(buf, session->view, copy_size);

    sw_error_t err = sw_parse_buffer(buf, copy_size, out_snapshot);
    free(buf);
    return err;
}

#else  /* !_WIN32 */

SW_API sw_error_t SW_CALL sw_session_open(sw_session_t **out_session)
{
    /* Validate arguments before reporting platform support, so the NULL-pointer
       contract is identical across platforms (matches the Windows path). */
    if (out_session == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_session = NULL;
    return SW_ERR_UNSUPPORTED_PLATFORM;
}

SW_API void SW_CALL sw_session_close(sw_session_t *session)
{
    (void)session;
}

SW_API sw_error_t SW_CALL sw_snapshot_take(sw_session_t *session,
                                           sw_snapshot_t **out_snapshot)
{
    /* Same check order as the Windows path: out-pointer, poison, session. */
    if (out_snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_snapshot = NULL;
    if (session == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    return SW_ERR_UNSUPPORTED_PLATFORM;
}

#endif /* _WIN32 */

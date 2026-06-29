#ifndef SENSORWATCH_SW_PLATFORM_H
#define SENSORWATCH_SW_PLATFORM_H

/*
 * Win32 platform-operation indirection. The session layer calls the kernel32
 * mapping APIs through this table so unit tests can swap them for mocks
 * (docs/C_CODING_STANDARDS.md section 9). Windows-only: the pure parser and
 * non-Windows builds never include this header, keeping <windows.h> out of the
 * cross-platform core.
 */

#if defined(_WIN32)

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

typedef struct sw_platform_ops {
    HANDLE (WINAPI *open_file_mapping)(DWORD, BOOL, LPCWSTR);
    LPVOID (WINAPI *map_view_of_file)(HANDLE, DWORD, DWORD, DWORD, SIZE_T);
    SIZE_T (WINAPI *virtual_query)(LPCVOID, PMEMORY_BASIC_INFORMATION, SIZE_T);
    BOOL   (WINAPI *unmap_view_of_file)(LPCVOID);
    BOOL   (WINAPI *close_handle)(HANDLE);
} sw_platform_ops_t;

/*
 * Defined in sw_session.c, defaulting to the real Win32 calls. Tests swap members
 * before opening a session and restore them after. Process-global and mutated
 * only in single-threaded test setup (see the standards' note on the function
 * table), so it carries no internal locking.
 */
extern sw_platform_ops_t sw_platform;

#endif /* _WIN32 */

#endif /* SENSORWATCH_SW_PLATFORM_H */

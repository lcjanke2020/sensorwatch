/* Consumer smoke test: pure-C consumption of the installed shared library.
 * No SW_STATIC is defined, so on Windows SW_API resolves to dllimport and this
 * links against the installed import library. Building + linking is the check. */
#include <sensorwatch/sensorwatch.h>

#include <stdio.h>

int main(void) {
    /* Cast for %u: sw_api_version() returns uint32_t (matches examples/c/sw_dump.c). */
    printf("sensorwatch C consumer OK, ABI=%u\n", (unsigned)sw_api_version());
    return 0;
}

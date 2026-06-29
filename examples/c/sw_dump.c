/*
 * sw_dump -- tiny smoke/demo for the sensorwatch native core. Opens the default
 * source, takes one snapshot, and prints the readings. Uses only the public ABI,
 * so it doubles as a live end-to-end check (run it with HWiNFO64 running and
 * Shared Memory Support enabled). Build with -DSW_BUILD_EXAMPLES=ON.
 */

#include "sensorwatch/sensorwatch.h"

#include <stdio.h>

int main(void)
{
    sw_session_t *session = NULL;
    sw_error_t err = sw_session_open(&session);
    if (err != SW_OK) {
        fprintf(stderr, "sw_session_open: %s\n", sw_error_string(err));
        return 1;
    }

    sw_snapshot_t *snapshot = NULL;
    err = sw_snapshot_take(session, &snapshot);
    if (err != SW_OK) {
        fprintf(stderr, "sw_snapshot_take: %s\n", sw_error_string(err));
        sw_session_close(session);
        return 1;
    }

    uint32_t count = 0;
    sw_snapshot_entry_count(snapshot, &count);
    printf("sensorwatch ABI %u -- %u readings\n", (unsigned)sw_api_version(), (unsigned)count);

    uint32_t limit = (count < 20u) ? count : 20u;
    for (uint32_t i = 0; i < limit; i++) {
        /* The buffers are sized generously and these are zero-initialized, so a
           (very unlikely) too-small result just leaves an empty string. Accessor
           return codes are intentionally ignored here to keep the demo short --
           real consumers should check them and/or do a length query first. */
        char sensor[256] = {0};
        char reading[256] = {0};
        char unit[64] = {0};
        double value = 0.0;
        sw_reading_type_t type = SW_READING_TYPE_UNKNOWN;

        sw_snapshot_get_sensor_name(snapshot, i, sensor, sizeof(sensor), NULL);
        sw_snapshot_get_reading_name(snapshot, i, reading, sizeof(reading), NULL);
        sw_snapshot_get_unit(snapshot, i, unit, sizeof(unit), NULL);
        sw_snapshot_get_value(snapshot, i, &value);
        sw_snapshot_get_reading_type(snapshot, i, &type);

        printf("[%2u] %-28s %-24s %12.3f %-6s (type %d)\n",
               (unsigned)i, sensor, reading, value, unit, (int)type);
    }
    if (count > limit) {
        printf("... and %u more\n", (unsigned)(count - limit));
    }

    sw_snapshot_free(snapshot);
    sw_session_close(session);
    return 0;
}

/*
 * sw_dump -- tiny smoke/demo for the sensorwatch native core. Opens the default
 * source, takes one snapshot, and prints the readings. Uses only the public ABI,
 * so it doubles as a live end-to-end check (run it with HWiNFO64 running and
 * Shared Memory Support enabled). Build with -DSW_BUILD_EXAMPLES=ON.
 *
 * Every fallible ABI call is checked -- both because that is the project's coding
 * convention and so the example shows binding authors how to handle the error
 * codes (and the caller-buffer string contract) rather than ignoring them.
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
    err = sw_snapshot_entry_count(snapshot, &count);
    if (err != SW_OK) {
        fprintf(stderr, "sw_snapshot_entry_count: %s\n", sw_error_string(err));
        sw_snapshot_free(snapshot);
        sw_session_close(session);
        return 1;
    }

    printf("sensorwatch ABI %u -- %u readings\n", (unsigned)sw_api_version(), (unsigned)count);

    uint32_t limit = (count < 20u) ? count : 20u;
    for (uint32_t i = 0; i < limit; i++) {
        char sensor[256] = "";
        char reading[256] = "";
        char unit[64] = "";
        double value = 0.0;
        sw_reading_type_t type = SW_READING_TYPE_UNKNOWN;

        /* Scalars: a failure here would mean the row is unusable, so report and
           skip it rather than print a bogus value. */
        err = sw_snapshot_get_value(snapshot, i, &value);
        if (err == SW_OK) {
            err = sw_snapshot_get_reading_type(snapshot, i, &type);
        }
        if (err != SW_OK) {
            fprintf(stderr, "reading %u: %s\n", (unsigned)i, sw_error_string(err));
            continue;
        }

        /* Strings use caller-owned buffers; on any error show a visible marker
           instead of a silently-empty field. These buffers are comfortably larger
           than any real HWiNFO field, so SW_ERR_BUFFER_TOO_SMALL is not expected --
           a real consumer that can't assume that should length-query first. */
        err = sw_snapshot_get_sensor_name(snapshot, i, sensor, sizeof(sensor), NULL);
        if (err != SW_OK) {
            snprintf(sensor, sizeof(sensor), "<%s>", sw_error_string(err));
        }
        err = sw_snapshot_get_reading_name(snapshot, i, reading, sizeof(reading), NULL);
        if (err != SW_OK) {
            snprintf(reading, sizeof(reading), "<%s>", sw_error_string(err));
        }
        err = sw_snapshot_get_unit(snapshot, i, unit, sizeof(unit), NULL);
        if (err != SW_OK) {
            snprintf(unit, sizeof(unit), "<%s>", sw_error_string(err));
        }

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

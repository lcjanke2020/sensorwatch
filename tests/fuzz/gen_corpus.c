/*
 * One-shot generator for the fuzz_parse seed corpus.
 *
 * Reuses the exact synthetic-buffer builder the cmocka parser tests use
 * (tests/c/sw_test_util.*), so the seeds mirror the real HWiNFO wire layout and
 * stay in lockstep with the format constants in src/sw_internal.h. The emitted
 * files are committed under tests/fuzz/corpus/parse/; a fuzzer run mutates from
 * them but they are stable inputs, so regenerating is only needed if the layout
 * or the seed set changes. This is a dev tool, not a build target -- see
 * tests/fuzz/README.md for the compile/run line.
 *
 * The set spans both valid buffers (drive coverage into the parse body) and the
 * adversarial headers from tests/c/test_parse.c (drive the reject paths).
 */

#include "sw_test_util.h"
#include "sw_internal.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void write_file(const char *dir, const char *name,
                       const uint8_t *buf, size_t len)
{
    char path[512];
    int n = snprintf(path, sizeof(path), "%s/%s", dir, name);
    if (n < 0 || (size_t)n >= sizeof(path)) {
        fprintf(stderr, "path too long: %s/%s\n", dir, name);
        exit(1);
    }
    FILE *f = fopen(path, "wb");
    if (f == NULL) {
        perror(path);
        exit(1);
    }
    if (buf != NULL && len > 0) {
        fwrite(buf, 1, len, f);
    }
    fclose(f);
    printf("wrote %s (%zu bytes)\n", path, len);
}

int main(int argc, char **argv)
{
    const char *dir = (argc > 1) ? argv[1] : ".";
    size_t len;

    /* 1. Canonical valid single reading (== tests/c valid_buffer()). */
    {
        sw_test_sensor_t s[] = { { "MEG Ai1600T", NULL } };
        sw_test_entry_t  e[] = { { 2u, 0u, "+12V", NULL, "V", 12.03 } };
        uint8_t *b = sw_test_build_buffer(s, 1u, e, 1u, &len);
        write_file(dir, "valid_single.bin", b, len);
        free(b);
    }

    /* 2. Richer valid buffer: several reading types, a cp1252 unit (degC), an
       embedded control char, an original-name fallback, and an out-of-range
       sensor index -- pushes coverage through the decode + fallback branches. */
    {
        sw_test_sensor_t s[] = { { "CPU", NULL }, { NULL, "GPU [#0]" } };
        sw_test_entry_t  e[] = {
            { 1u, 0u, "Core\x01", NULL, "\xB0" "C", 42.5 },  /* temperature */
            { 2u, 1u, NULL, "VCore", "V", 1.23 },            /* voltage, orig fallback */
            { 5u, 9u, "Pkg", NULL, "W", 65.0 },              /* power, bad sensor idx */
        };
        uint8_t *b = sw_test_build_buffer(s, 2u, e, 3u, &len);
        write_file(dir, "valid_multi.bin", b, len);
        free(b);
    }

    /* 3. entry_count * entry_size overflows a 32-bit product to zero. */
    {
        sw_test_sensor_t s[] = { { "S", NULL } };
        sw_test_entry_t  e[] = { { 2u, 0u, "r", NULL, "V", 1.0 } };
        uint8_t *b = sw_test_build_buffer(s, 1u, e, 1u, &len);
        sw_test_patch_u32(b, SW_OFF_ENTRY_COUNT, 0x10000u);
        sw_test_patch_u32(b, SW_OFF_ENTRY_SIZE,  0x10000u);
        write_file(dir, "wrap_count_size.bin", b, len);
        free(b);
    }

    /* 4. A single element larger than the whole buffer (count one). */
    {
        sw_test_sensor_t s[] = { { "S", NULL } };
        sw_test_entry_t  e[] = { { 2u, 0u, "r", NULL, "V", 1.0 } };
        uint8_t *b = sw_test_build_buffer(s, 1u, e, 1u, &len);
        sw_test_patch_u32(b, SW_OFF_ENTRY_SIZE, 0x40000000u);
        write_file(dir, "oversize_one.bin", b, len);
        free(b);
    }

    /* 5. Name and unit fields with no NUL terminator within their width. */
    {
        sw_test_sensor_t s[] = { { "S", NULL } };
        sw_test_entry_t  e[] = { { 2u, 0u, "r", NULL, "V", 1.0 } };
        uint8_t *b = sw_test_build_buffer(s, 1u, e, 1u, &len);
        memset(b + SW_HEADER_SIZE + SW_SENSOR_OFF_NAME_USER, 'A', SW_NAME_FIELD_LEN);
        uint32_t eoff = 0;
        memcpy(&eoff, b + SW_OFF_ENTRY_OFFSET, sizeof(eoff));
        memset(b + (size_t)eoff + SW_ENTRY_OFF_NAME_USER, 'B', SW_NAME_FIELD_LEN);
        memset(b + (size_t)eoff + SW_ENTRY_OFF_UNIT,      'V', SW_UNIT_FIELD_LEN);
        write_file(dir, "unterminated_strings.bin", b, len);
        free(b);
    }

    /* 6. Valid layout, wrong magic -- the earliest reject path. */
    {
        sw_test_sensor_t s[] = { { "S", NULL } };
        sw_test_entry_t  e[] = { { 2u, 0u, "r", NULL, "V", 1.0 } };
        uint8_t *b = sw_test_build_buffer(s, 1u, e, 1u, &len);
        sw_test_patch_u32(b, SW_OFF_MAGIC, 0xDEADBEEFu);
        write_file(dir, "bad_magic.bin", b, len);
        free(b);
    }

    /* 7. Valid empty snapshot (zero entries). */
    {
        sw_test_sensor_t s[] = { { "S", NULL } };
        uint8_t *b = sw_test_build_buffer(s, 1u, NULL, 0u, &len);
        write_file(dir, "valid_empty.bin", b, len);
        free(b);
    }

    return 0;
}

/*
 * String handling at the ABI boundary.
 *
 * Source strings are untrusted, fixed-width cp1252 byte fields (SECURITY.md
 * sections 4.1 / 8.2). sw_decode_field reproduces the Python reference
 * sensorwatch.hwinfo_shm._decode() exactly: decode cp1252 (with replacement for
 * the 5 undefined bytes), then strip C0/C1 control characters, emitting sanitized
 * UTF-8. sw_copy_string_out implements the public caller-buffer contract.
 */

#include "sw_internal.h"

#include <stdlib.h>
#include <string.h>

/*
 * cp1252 high range (0x80..0x9F) -> Unicode code points. The 5 bytes that cp1252
 * leaves undefined (0x81, 0x8D, 0x8F, 0x90, 0x9D) map to U+FFFD, matching
 * Python's decode("cp1252", errors="replace"). 0x00..0x7F are ASCII and
 * 0xA0..0xFF are Latin-1 (code point == byte), handled inline below.
 */
static const uint32_t SW_CP1252_HIGH[32] = {
    0x20ACu, 0xFFFDu, 0x201Au, 0x0192u, 0x201Eu, 0x2026u, 0x2020u, 0x2021u,
    0x02C6u, 0x2030u, 0x0160u, 0x2039u, 0x0152u, 0xFFFDu, 0x017Du, 0xFFFDu,
    0xFFFDu, 0x2018u, 0x2019u, 0x201Cu, 0x201Du, 0x2022u, 0x2013u, 0x2014u,
    0x02DCu, 0x2122u, 0x0161u, 0x203Au, 0x0153u, 0xFFFDu, 0x017Eu, 0x0178u
};

static uint32_t sw_cp1252_codepoint(uint8_t b)
{
    if (b < 0x80u) {
        return (uint32_t)b;
    }
    if (b <= 0x9Fu) {
        return SW_CP1252_HIGH[b - 0x80u];
    }
    return (uint32_t)b;  /* 0xA0..0xFF -> identical Latin-1 code point */
}

/* Code points stripped post-decode: C0 controls, DEL, and C1 controls. Matches
   the Python regex [\x00-\x1f\x7f-\x9f]. */
static bool sw_is_control(uint32_t cp)
{
    return cp <= 0x1Fu || (cp >= 0x7Fu && cp <= 0x9Fu);
}

/* Encode one code point as UTF-8 into out (>= 4 bytes). Returns bytes written. */
static size_t sw_utf8_encode(uint32_t cp, char out[4])
{
    if (cp < 0x80u) {
        out[0] = (char)cp;
        return 1;
    }
    if (cp < 0x800u) {
        out[0] = (char)(0xC0u | (cp >> 6));
        out[1] = (char)(0x80u | (cp & 0x3Fu));
        return 2;
    }
    if (cp < 0x10000u) {
        out[0] = (char)(0xE0u | (cp >> 12));
        out[1] = (char)(0x80u | ((cp >> 6) & 0x3Fu));
        out[2] = (char)(0x80u | (cp & 0x3Fu));
        return 3;
    }
    out[0] = (char)(0xF0u | (cp >> 18));
    out[1] = (char)(0x80u | ((cp >> 12) & 0x3Fu));
    out[2] = (char)(0x80u | ((cp >> 6) & 0x3Fu));
    out[3] = (char)(0x80u | (cp & 0x3Fu));
    return 4;
}

char *sw_decode_field(const uint8_t *field, size_t field_len)
{
    /* Effective length: up to the first embedded NUL (HWiNFO pads with zeroes). */
    size_t eff = 0;
    while (eff < field_len && field[eff] != 0x00u) {
        eff++;
    }

    /* Worst case is 3 UTF-8 bytes per input byte (cp1252 never yields > U+FFFD),
       plus the terminator. Use overflow-checked arithmetic on principle. */
    size_t cap = 0;
    if (!sw_size_mul(eff, 3u, &cap) || !sw_size_add(cap, 1u, &cap)) {
        return NULL;
    }

    char *out = (char *)malloc(cap);
    if (out == NULL) {
        return NULL;
    }

    size_t pos = 0;
    for (size_t i = 0; i < eff; i++) {
        uint32_t cp = sw_cp1252_codepoint(field[i]);
        if (sw_is_control(cp)) {
            continue;  /* stripped */
        }
        char enc[4];
        size_t n = sw_utf8_encode(cp, enc);
        memcpy(out + pos, enc, n);
        pos += n;
    }
    out[pos] = '\0';
    return out;
}

char *sw_dup_cstr(const char *s)
{
    size_t n = strlen(s) + 1;
    char *out = (char *)malloc(n);
    if (out == NULL) {
        return NULL;
    }
    memcpy(out, s, n);
    return out;
}

sw_error_t sw_copy_string_out(const char *value, char *buffer, size_t buffer_size,
                              size_t *out_required)
{
    size_t required = strlen(value) + 1;

    /* Length query: buffer == NULL && buffer_size == 0. */
    if (buffer == NULL && buffer_size == 0) {
        if (out_required == NULL) {
            return SW_ERR_NULL_POINTER;
        }
        *out_required = required;
        return SW_ERR_BUFFER_TOO_SMALL;
    }

    /* Copy: buffer != NULL && buffer_size > 0. */
    if (buffer != NULL && buffer_size > 0) {
        if (required <= buffer_size) {
            memcpy(buffer, value, required);
            if (out_required != NULL) {
                *out_required = required;
            }
            return SW_OK;
        }
        /* Too small: leave an empty NUL-terminated string, never a partial UTF-8
           sequence; report the full required size. */
        buffer[0] = '\0';
        if (out_required != NULL) {
            *out_required = required;
        }
        return SW_ERR_BUFFER_TOO_SMALL;
    }

    /* buffer == NULL with size > 0, or buffer != NULL with size == 0. */
    return SW_ERR_INVALID_ARGUMENT;
}

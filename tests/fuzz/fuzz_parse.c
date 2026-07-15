/*
 * libFuzzer entry point for the pure HWiNFO shared-memory parser.
 *
 * sw_parse_buffer() is the "same parse routine used by snapshot acquisition"
 * (docs/C_CODING_STANDARDS.md "Fuzzing the Parser"): sw_snapshot_take() feeds it
 * an owned copy of the live mapping, and here libFuzzer feeds it arbitrary bytes.
 * Every header field is untrusted input the parser must bound before use
 * (SECURITY.md 1.3), so any crash, sanitizer finding, timeout, or unbounded
 * allocation this surfaces is a bug.
 *
 * Built only under -DSW_BUILD_FUZZ=ON (clang, libFuzzer + ASan/UBSan; see
 * CMakeLists.txt). Links sensorwatch_static so it reaches the internal parser
 * directly, exactly as the cmocka tests do. The corpus is seeded from the same
 * synthetic buffers those tests use plus the adversarial cases in
 * tests/c/test_parse.c (see tests/fuzz/README.md).
 */

#include "sw_internal.h"  /* sw_parse_buffer (internal, not exported) */

#include <stddef.h>
#include <stdint.h>

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
    sw_snapshot_t *snapshot = NULL;

    /* The return code is intentionally ignored: a specific error on malformed
       input is correct behavior, not a finding. What the sanitizers police is
       *how* the parser reaches that verdict -- no out-of-bounds read of the
       untrusted buffer, no overflow in the size math, no leak on any path. */
    (void)sw_parse_buffer(data, size, &snapshot);

    /* NULL-safe (sw_snapshot_free -> sw_snapshot_destroy): on error snapshot is
       NULL; on success this frees it so ASan's leak check stays clean. */
    sw_snapshot_free(snapshot);
    return 0;
}

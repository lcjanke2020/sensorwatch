"""cffi out-of-line API-mode build script for the sensorwatch native core.

This compiles the C core in ``src/`` *directly into* the extension module
``sensorwatch._sw_cffi`` (rather than loading a separate ``sensorwatch.dll``).
Defining ``SW_STATIC`` makes the header's ``SW_API`` expand to nothing, so the
public ``sw_*`` functions link straight into the extension and the internal
helpers stay extension-local. This single-artifact shape sidesteps the DLL
search-order risk discussed in ``SECURITY.md`` §2.1.

API mode (vs. ABI/ctypes) means cffi compiles a stub against the *real* header
``include/sensorwatch/sensorwatch.h``, so any drift between the hand-curated
``cdef`` below and the shipped prototypes becomes a **compile error** at wheel
build time.

The ``cdef`` is curated by hand because cffi's parser cannot ingest the full
header (it carries ``#include``s and the ``SW_STATIC_ASSERT`` / ``SW_API`` /
``SW_CALL`` macros). Keep it in lockstep with the public header; the API-mode
compile is the guard that the signatures still match. ``SW_API`` / ``SW_CALL``
are intentionally omitted here — API mode uses the header's real prototypes
(including the pinned ``__cdecl``) via the ``#include`` in ``set_source``.

Build directly with::

    python sensorwatch/_native_build.py

(run from the repo root, inside a shell that has the C compiler on PATH — on
this project's Windows box that means the VS Developer Shell). Normally it runs
automatically via setuptools' ``cffi_modules`` hook (see ``setup.py``).
"""

from __future__ import annotations

from cffi import FFI

ffibuilder = FFI()

# Curated declarations — must stay in sync with include/sensorwatch/sensorwatch.h.
# API mode compiles this against the real header, so a mismatch fails the build.
ffibuilder.cdef(
    r"""
    typedef enum {
        SW_OK                       = 0,
        SW_ERR_NULL_POINTER         = -1,
        SW_ERR_INVALID_ARGUMENT     = -2,
        SW_ERR_UNSUPPORTED_PLATFORM = -3,
        SW_ERR_SOURCE_UNAVAILABLE   = -4,
        SW_ERR_MAP_FAILED           = -5,
        SW_ERR_BAD_MAGIC            = -6,
        SW_ERR_CORRUPT_DATA         = -7,
        SW_ERR_OUT_OF_MEMORY        = -8,
        SW_ERR_INDEX_OUT_OF_RANGE   = -9,
        SW_ERR_BUFFER_TOO_SMALL     = -10,
        SW_ERR_VERSION_MISMATCH     = -11,
        SW_ERR_INTERNAL             = -12
    } sw_error_t;

    typedef enum {
        SW_READING_TYPE_NONE        = 0,
        SW_READING_TYPE_TEMPERATURE = 1,
        SW_READING_TYPE_VOLTAGE     = 2,
        SW_READING_TYPE_FAN         = 3,
        SW_READING_TYPE_CURRENT     = 4,
        SW_READING_TYPE_POWER       = 5,
        SW_READING_TYPE_CLOCK       = 6,
        SW_READING_TYPE_USAGE       = 7,
        SW_READING_TYPE_OTHER       = 8,
        SW_READING_TYPE_UNKNOWN     = 255
    } sw_reading_type_t;

    typedef struct sw_session sw_session_t;
    typedef struct sw_snapshot sw_snapshot_t;

    uint32_t sw_api_version(void);
    const char *sw_error_string(sw_error_t error);

    sw_error_t sw_session_open(sw_session_t **out_session);
    void sw_session_close(sw_session_t *session);

    sw_error_t sw_snapshot_take(sw_session_t *session, sw_snapshot_t **out_snapshot);
    void sw_snapshot_free(sw_snapshot_t *snapshot);

    sw_error_t sw_snapshot_entry_count(const sw_snapshot_t *snapshot, uint32_t *out_count);
    sw_error_t sw_snapshot_get_reading_type(const sw_snapshot_t *snapshot, uint32_t index, sw_reading_type_t *out_type);
    sw_error_t sw_snapshot_get_value(const sw_snapshot_t *snapshot, uint32_t index, double *out_value);
    sw_error_t sw_snapshot_get_minimum(const sw_snapshot_t *snapshot, uint32_t index, double *out_value);
    sw_error_t sw_snapshot_get_maximum(const sw_snapshot_t *snapshot, uint32_t index, double *out_value);
    sw_error_t sw_snapshot_get_average(const sw_snapshot_t *snapshot, uint32_t index, double *out_value);

    sw_error_t sw_snapshot_get_source_name(const sw_snapshot_t *snapshot, uint32_t index, char *buffer, size_t buffer_size, size_t *out_required);
    sw_error_t sw_snapshot_get_sensor_name(const sw_snapshot_t *snapshot, uint32_t index, char *buffer, size_t buffer_size, size_t *out_required);
    sw_error_t sw_snapshot_get_reading_name(const sw_snapshot_t *snapshot, uint32_t index, char *buffer, size_t buffer_size, size_t *out_required);
    sw_error_t sw_snapshot_get_unit(const sw_snapshot_t *snapshot, uint32_t index, char *buffer, size_t buffer_size, size_t *out_required);
    """
)

ffibuilder.set_source(
    "sensorwatch._sw_cffi",
    '#include "sensorwatch/sensorwatch.h"',
    sources=[
        "src/sw_error.c",
        "src/sw_string.c",
        "src/sw_parse.c",
        "src/sw_snapshot.c",
        "src/sw_session.c",
    ],
    include_dirs=["include", "src"],
    # SW_STATIC -> SW_API expands to nothing: the sw_* functions are compiled
    # straight into this extension, no import/export decoration, no separate DLL.
    define_macros=[("SW_STATIC", None)],
)


if __name__ == "__main__":
    ffibuilder.compile(verbose=True)

"""Low-level glue for the native cffi binding.

Everything in this module touches the compiled extension ``sensorwatch._sw_cffi``
directly: importing ``ffi`` / ``lib``, the load-time ABI-version guard, the
``SensorwatchError`` exception, the ``ReadingType`` enum, and the helpers that
translate ``sw_error_t`` return codes into exceptions and the caller-buffer
string contract into Python ``str``. The Pythonic surface
(:class:`~sensorwatch.native.Session` / :class:`~sensorwatch.native.Snapshot`)
lives in :mod:`sensorwatch.native` and builds on these.
"""

from __future__ import annotations

import enum

try:
    from sensorwatch._sw_cffi import ffi, lib
except ImportError as exc:  # pragma: no cover - exercised only without the build
    raise ImportError(
        "The sensorwatch native extension (sensorwatch._sw_cffi) is not built. "
        "Install a sensorwatch wheel, or build it from source with a C compiler "
        "(e.g. `python sensorwatch/_native_build.py` from the repo root)."
    ) from exc


# The ABI version this binding was written against. The C ABI encodes its version
# as MAJOR * 10000 + MINOR * 100 + PATCH (see SW_API_VERSION). API mode already
# makes signature drift a build error; the load-time guard below catches a
# *runtime* mismatch (e.g. a stale compiled extension paired with newer Python
# wrappers). Pre-1.0 the MINOR is the breaking-change axis, so the guard pins
# major.minor (a patch bump stays compatible); from 1.0 on, only the major gates.
EXPECTED_ABI_MAJOR = 0
EXPECTED_ABI_MINOR = 1

# Defensive upper bound for a single ABI string (see _query_string). The ABI's
# name fields are bounded, sanitized UTF-8 (a few hundred bytes at most); 64 KiB
# is far above any legitimate value, so anything larger signals a fault.
_MAX_STRING_BYTES = 64 * 1024


class SensorwatchError(Exception):
    """A native ``sw_error_t`` surfaced as a Python exception.

    Carries the numeric :attr:`code` (the ``sw_error_t`` value) and, by default,
    the library's own ``sw_error_string`` text for that code. ``SW_OK`` is never
    raised.
    """

    def __init__(self, code: int, message: str | None = None) -> None:
        self.code = int(code)
        if message is None:
            message = ffi.string(lib.sw_error_string(self.code)).decode(
                "utf-8", errors="replace"
            )
        self.message = message
        super().__init__(f"[{self.code}] {message}")


class ReadingType(enum.IntEnum):
    """Source-neutral reading category, mirroring ``sw_reading_type_t``."""

    NONE = 0
    TEMPERATURE = 1
    VOLTAGE = 2
    FAN = 3
    CURRENT = 4
    POWER = 5
    CLOCK = 6
    USAGE = 7
    OTHER = 8
    UNKNOWN = 255

    @classmethod
    def _missing_(cls, value):
        # The C core already maps unrecognized source categories to UNKNOWN; this
        # keeps any unexpected value inside the enum (never a bare ValueError out
        # of Snapshot.__getitem__) should the ABI grow a category this binding
        # predates.
        return cls.UNKNOWN


def _check(err: int) -> None:
    """Raise :class:`SensorwatchError` for any non-``SW_OK`` return code."""
    if err != lib.SW_OK:
        raise SensorwatchError(err)


def _require_compatible_abi() -> None:
    """Fail fast at import if the loaded extension's ABI is incompatible."""
    version = lib.sw_api_version()
    major = version // 10000
    minor = (version // 100) % 100
    # Pre-1.0 a minor bump is breaking, so it must match; from 1.0 on only the
    # major gates compatibility.
    compatible = major == EXPECTED_ABI_MAJOR and (major >= 1 or minor == EXPECTED_ABI_MINOR)
    if not compatible:
        patch = version % 100
        # Raised at import time, so use ImportError (not SensorwatchError): callers
        # that guard the optional native import with `except ImportError` — the same
        # way they'd catch a missing extension — then fall back cleanly.
        raise ImportError(
            f"sensorwatch native ABI {major}.{minor}.{patch} is incompatible with "
            f"this binding (expected {EXPECTED_ABI_MAJOR}.{EXPECTED_ABI_MINOR}.x). "
            f"Reinstall a matching sensorwatch build."
        )


def _query_string(accessor, snapshot, index: int) -> str:
    """Read one string field via the ABI's length-query-then-copy contract.

    ``accessor`` is one of the ``sw_snapshot_get_*_name`` / ``sw_snapshot_get_unit``
    functions. A first call with ``(NULL, 0, &required)`` reports the byte count
    (including the NUL) and returns ``SW_ERR_BUFFER_TOO_SMALL``; a second call
    fills a buffer of that size. Strings are already sanitized UTF-8 at the ABI
    boundary (see docs/C_ABI.md "String and Buffer Conventions").
    """
    required = ffi.new("size_t *")
    err = accessor(snapshot, index, ffi.NULL, 0, required)
    if err != lib.SW_ERR_BUFFER_TOO_SMALL:
        # Any other code is a real error (NULL snapshot, index out of range, ...).
        # The length query never legitimately returns SW_OK.
        _check(err)
        raise SensorwatchError(
            lib.SW_ERR_INTERNAL,
            "string length query unexpectedly succeeded without a buffer",
        )

    size = required[0]
    # Defensive cap (SECURITY.md treats sensor data as untrusted): ABI strings are
    # bounded, sanitized UTF-8, so an unreasonable required size signals a fault
    # rather than a value worth allocating for.
    if size > _MAX_STRING_BYTES:
        raise SensorwatchError(
            lib.SW_ERR_CORRUPT_DATA,
            f"native string length {size} exceeds the {_MAX_STRING_BYTES}-byte cap",
        )
    buffer = ffi.new("char[]", size)
    _check(accessor(snapshot, index, buffer, size, required))
    return ffi.string(buffer).decode("utf-8", errors="replace")


_require_compatible_abi()

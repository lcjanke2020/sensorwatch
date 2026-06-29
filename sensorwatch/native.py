"""Pythonic bindings over the native sensorwatch C ABI.

This is a thin, safe wrapper around the compiled ``sensorwatch._sw_cffi``
extension (cffi, API mode), which links the native C core in ``src/`` directly
into the extension. It mirrors the pure-Python :mod:`sensorwatch.hwinfo_shm`
reader but goes through the C parser.

Usage::

    from sensorwatch.native import Session

    with Session() as session:
        snapshot = session.snapshot()
        print(len(snapshot), "readings from", snapshot.source)
        for reading in snapshot:
            print(reading.sensor, reading.reading, reading.value, reading.unit)

Every non-success native return code is raised as
:class:`~sensorwatch._native.SensorwatchError` (carrying the ``sw_error_t`` value
and the library's error text). On non-Windows platforms the sensor source is
unavailable, so opening a :class:`Session` raises ``SW_ERR_UNSUPPORTED_PLATFORM``
rather than crashing; when HWiNFO is not running it raises
``SW_ERR_SOURCE_UNAVAILABLE``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterator

from sensorwatch._native import (
    ReadingType,
    SensorwatchError,
    _check,
    _query_string,
    ffi,
    lib,
)

__all__ = ["Session", "Snapshot", "Reading", "ReadingType", "SensorwatchError"]


@dataclass(frozen=True)
class Reading:
    """One immutable sensor reading from a :class:`Snapshot`.

    Mirrors :class:`sensorwatch.hwinfo_shm.SensorReading`, plus the
    source-neutral :attr:`source` and a typed :attr:`type` enum.
    """

    source: str
    sensor: str
    reading: str
    unit: str
    type: ReadingType
    value: float
    minimum: float
    maximum: float
    average: float


class Snapshot:
    """An immutable view of all sensor readings captured at one instant.

    Acts as an immutable sequence: ``len(snapshot)`` is the reading count and
    ``snapshot[i]`` (with negative indexing) returns a :class:`Reading`. Use as a
    context manager, or call :meth:`close`, to release the native snapshot
    promptly; it is also freed on garbage collection. A snapshot stays valid
    after its parent :class:`Session` is closed (it owns its own copy of the
    data).
    """

    def __init__(self, ptr) -> None:
        self._ptr = ptr
        count = ffi.new("uint32_t *")
        _check(lib.sw_snapshot_entry_count(ptr, count))
        self._count = int(count[0])

    def _require_open(self):
        if self._ptr is None:
            raise SensorwatchError(
                lib.SW_ERR_INVALID_ARGUMENT, "snapshot is closed"
            )
        return self._ptr

    def __len__(self) -> int:
        return self._count

    @property
    def source(self) -> str:
        """The source/backend identity (e.g. ``"HWiNFO"``)."""
        ptr = self._require_open()
        if self._count == 0:
            return ""
        return _query_string(lib.sw_snapshot_get_source_name, ptr, 0)

    def __getitem__(self, index: int) -> Reading:
        if not isinstance(index, int):
            raise TypeError(
                f"Snapshot indices must be integers, not {type(index).__name__}"
            )
        ptr = self._require_open()
        if index < 0:
            index += self._count
        if not 0 <= index < self._count:
            raise IndexError("snapshot reading index out of range")

        reading_type = ffi.new("sw_reading_type_t *")
        _check(lib.sw_snapshot_get_reading_type(ptr, index, reading_type))
        value = ffi.new("double *")
        _check(lib.sw_snapshot_get_value(ptr, index, value))
        minimum = ffi.new("double *")
        _check(lib.sw_snapshot_get_minimum(ptr, index, minimum))
        maximum = ffi.new("double *")
        _check(lib.sw_snapshot_get_maximum(ptr, index, maximum))
        average = ffi.new("double *")
        _check(lib.sw_snapshot_get_average(ptr, index, average))

        return Reading(
            source=_query_string(lib.sw_snapshot_get_source_name, ptr, index),
            sensor=_query_string(lib.sw_snapshot_get_sensor_name, ptr, index),
            reading=_query_string(lib.sw_snapshot_get_reading_name, ptr, index),
            unit=_query_string(lib.sw_snapshot_get_unit, ptr, index),
            type=ReadingType(reading_type[0]),
            value=value[0],
            minimum=minimum[0],
            maximum=maximum[0],
            average=average[0],
        )

    def __iter__(self) -> Iterator[Reading]:
        for i in range(self._count):
            yield self[i]

    def close(self) -> None:
        """Free the native snapshot. Idempotent."""
        if self._ptr is not None:
            lib.sw_snapshot_free(self._ptr)
            self._ptr = None

    def __enter__(self) -> "Snapshot":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self) -> None:  # pragma: no cover - GC timing dependent
        try:
            self.close()
        except Exception:
            pass


class Session:
    """A connection to the default sensor source (HWiNFO on Windows).

    Use as a context manager, or call :meth:`close`, to release native
    resources; it is also closed on garbage collection. Opening a session on a
    non-Windows platform raises ``SW_ERR_UNSUPPORTED_PLATFORM``; when HWiNFO is
    not running (or shared memory is disabled) it raises
    ``SW_ERR_SOURCE_UNAVAILABLE``.
    """

    def __init__(self) -> None:
        # Set before _check so a failed open leaves a clean, closeable object
        # (close()/__del__ are no-ops) rather than an AttributeError on _ptr.
        self._ptr = None
        out = ffi.new("sw_session_t **")
        _check(lib.sw_session_open(out))
        self._ptr = out[0]

    def snapshot(self) -> Snapshot:
        """Capture an immutable :class:`Snapshot` of the current readings."""
        if self._ptr is None:
            raise SensorwatchError(
                lib.SW_ERR_INVALID_ARGUMENT, "session is closed"
            )
        out = ffi.new("sw_snapshot_t **")
        _check(lib.sw_snapshot_take(self._ptr, out))
        return Snapshot(out[0])

    def close(self) -> None:
        """Close the session. Idempotent."""
        if self._ptr is not None:
            lib.sw_session_close(self._ptr)
            self._ptr = None

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self) -> None:  # pragma: no cover - GC timing dependent
        try:
            self.close()
        except Exception:
            pass

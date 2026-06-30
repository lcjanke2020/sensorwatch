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
:class:`~sensorwatch.native.SensorwatchError` (carrying the ``sw_error_t`` value
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
    data). Once the snapshot itself is closed, every accessor (``len()``,
    :attr:`source`, indexing) raises :class:`SensorwatchError`.
    """

    def __init__(self, ptr) -> None:
        self._ptr = ptr
        count = ffi.new("uint32_t *")
        _check(lib.sw_snapshot_entry_count(ptr, count))
        self._count = int(count[0])
        self._source = None  # snapshot-global; queried once and cached on access

    def _require_open(self):
        if self._ptr is None:
            raise SensorwatchError(
                lib.SW_ERR_INVALID_ARGUMENT, "snapshot is closed"
            )
        return self._ptr

    def __len__(self) -> int:
        self._require_open()
        return self._count

    @property
    def source(self) -> str:
        """The source/backend identity (e.g. ``"HWiNFO"``); shared by all readings.

        Empty for a zero-entry snapshot (the ABI's source accessor is index-gated,
        so there is no index to query). Requires the snapshot to be open on every
        access — including the cached path — so the closed-state guard stays
        consistent with the other accessors.
        """
        ptr = self._require_open()
        if self._source is None:
            self._source = (
                "" if self._count == 0
                else _query_string(lib.sw_snapshot_get_source_name, ptr, 0)
            )
        return self._source

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
            source=self.source,  # snapshot-global; cached, not re-queried per entry
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
        # Check eagerly so iterating a closed snapshot raises even when it is
        # empty (an empty range would otherwise skip the per-item guard).
        self._require_open()
        return (self[i] for i in range(self._count))

    def close(self) -> None:
        """Free the native snapshot. Idempotent.

        Clears the handle before freeing it. Like the C ABI (``sw_snapshot_free``
        "must not race with any other use of the same snapshot"), this is not safe
        to call concurrently with other operations on the same snapshot —
        synchronize externally if you share one across threads.
        """
        ptr, self._ptr = self._ptr, None  # poison-then-free
        if ptr is not None:
            lib.sw_snapshot_free(ptr)

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
        """Close the session. Idempotent.

        Clears the handle before closing it. Like the C ABI (``sw_session_close``
        "must not race with any other use of the same session"), this is not safe
        to call concurrently with other operations on the same session —
        synchronize externally if you share one across threads.
        """
        ptr, self._ptr = self._ptr, None  # poison-then-close
        if ptr is not None:
            lib.sw_session_close(ptr)

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __del__(self) -> None:  # pragma: no cover - GC timing dependent
        try:
            self.close()
        except Exception:
            pass

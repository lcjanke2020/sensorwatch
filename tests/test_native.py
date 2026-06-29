"""Tests for the native cffi binding (sensorwatch.native).

These exercise the compiled extension ``sensorwatch._sw_cffi`` and its Pythonic
wrappers. The module-level import requires the extension to be built; CI builds
it before running pytest. Tests that need a live sensor source are skipped when
HWiNFO is unavailable, and the Windows-only / non-Windows-only paths guard on
``sys.platform`` so the same suite runs on both CI legs.
"""

from __future__ import annotations

import sys

import pytest

from sensorwatch import native
from sensorwatch._native import ffi, lib
from sensorwatch.hwinfo_shm import SENSOR_TYPES, read_sensors
from sensorwatch.native import ReadingType, SensorwatchError, Session, Snapshot

IS_WINDOWS = sys.platform == "win32"

# Map the typed enum back to the pure-Python reader's string category.
_TYPE_TO_STR = {ReadingType(code): name for code, name in SENSOR_TYPES.items()}


def _type_str(rt: ReadingType) -> str:
    return _TYPE_TO_STR.get(rt, f"unknown({int(rt)})")


# --- Import + ABI version smoke -------------------------------------------------

def test_abi_version_matches_expected():
    from sensorwatch._native import EXPECTED_ABI_MAJOR, EXPECTED_ABI_MINOR
    version = lib.sw_api_version()
    assert version // 10000 == EXPECTED_ABI_MAJOR
    assert (version // 100) % 100 == EXPECTED_ABI_MINOR  # pinned pre-1.0 (0.1.x)


def test_reading_type_enum_matches_abi():
    assert ReadingType.NONE == 0
    assert ReadingType.TEMPERATURE == 1
    assert ReadingType.OTHER == 8
    assert ReadingType.UNKNOWN == 255
    assert ReadingType(lib.SW_READING_TYPE_FAN) is ReadingType.FAN
    # An out-of-enum code maps to UNKNOWN rather than raising ValueError.
    assert ReadingType(9999) is ReadingType.UNKNOWN


def test_public_surface():
    assert set(native.__all__) == {
        "Session", "Snapshot", "Reading", "ReadingType", "SensorwatchError",
    }


# --- Error translation ----------------------------------------------------------

def test_error_is_raised_for_null_pointer():
    """A non-SW_OK return code becomes a SensorwatchError (deterministic, any OS)."""
    count = ffi.new("uint32_t *")
    err = lib.sw_snapshot_entry_count(ffi.NULL, count)
    assert err == lib.SW_ERR_NULL_POINTER
    with pytest.raises(SensorwatchError) as excinfo:
        from sensorwatch._native import _check
        _check(err)
    assert excinfo.value.code == lib.SW_ERR_NULL_POINTER


def test_sensorwatch_error_carries_code_and_message():
    err = SensorwatchError(lib.SW_ERR_SOURCE_UNAVAILABLE)
    assert err.code == lib.SW_ERR_SOURCE_UNAVAILABLE
    assert err.message  # library-provided text, non-empty
    assert str(err.code) in str(err)


def test_sensorwatch_error_custom_message():
    err = SensorwatchError(lib.SW_ERR_VERSION_MISMATCH, "custom text")
    assert err.message == "custom text"
    assert "custom text" in str(err)


@pytest.mark.skipif(IS_WINDOWS, reason="non-Windows platform path")
def test_session_open_unsupported_on_non_windows():
    with pytest.raises(SensorwatchError) as excinfo:
        Session()
    assert excinfo.value.code == lib.SW_ERR_UNSUPPORTED_PLATFORM


@pytest.mark.skipif(not IS_WINDOWS, reason="Windows-only source-availability mapping")
def test_session_open_maps_source_unavailable_when_hwinfo_absent():
    """When HWiNFO is not running, opening a session maps to SOURCE_UNAVAILABLE.

    A no-op when HWiNFO *is* running (the live path is covered separately).
    """
    try:
        Session().close()
    except SensorwatchError as exc:
        assert exc.code == lib.SW_ERR_SOURCE_UNAVAILABLE


# --- Windows-only live integration ---------------------------------------------

def _live_session_or_skip() -> Session:
    if not IS_WINDOWS:
        pytest.skip("live source is Windows-only")
    try:
        return Session()
    except SensorwatchError as exc:
        if exc.code == lib.SW_ERR_SOURCE_UNAVAILABLE:
            pytest.skip("HWiNFO not running / shared memory disabled")
        raise


def test_live_snapshot_shape_matches_reference():
    """A live snapshot matches the pure-Python reader's shape, field for field."""
    reference = read_sensors()
    if reference is None:
        pytest.skip("read_sensors() returned None (HWiNFO unavailable)")

    with _live_session_or_skip() as session:
        snapshot = session.snapshot()
        assert snapshot.source == "HWiNFO"
        assert len(snapshot) == len(reference)

        native_shape = [
            (r.sensor, r.reading, r.unit, _type_str(r.type)) for r in snapshot
        ]
        reference_shape = [
            (p.sensor_name, p.reading_name, p.unit, p.sensor_type) for p in reference
        ]
        assert native_shape == reference_shape


def test_live_snapshot_reading_fields_are_typed():
    with _live_session_or_skip() as session:
        snapshot = session.snapshot()
        if len(snapshot) == 0:
            pytest.skip("no readings available")
        reading = snapshot[0]
        assert isinstance(reading.type, ReadingType)
        assert isinstance(reading.value, float)
        assert isinstance(reading.minimum, float)
        assert isinstance(reading.maximum, float)
        assert isinstance(reading.average, float)
        assert isinstance(reading.sensor, str)
        assert isinstance(reading.unit, str)


def test_live_snapshot_indexing_and_bounds():
    with _live_session_or_skip() as session:
        snapshot = session.snapshot()
        n = len(snapshot)
        if n == 0:
            pytest.skip("no readings available")
        assert snapshot[-1] == snapshot[n - 1]  # negative indexing
        with pytest.raises(IndexError):
            _ = snapshot[n]
        with pytest.raises(TypeError):
            _ = snapshot["nope"]


def test_closed_session_and_snapshot_are_guarded():
    with _live_session_or_skip() as session:
        snapshot = session.snapshot()
        snapshot.close()
        snapshot.close()  # idempotent
        with pytest.raises(SensorwatchError):
            _ = snapshot.source
    # session closed by context manager
    with pytest.raises(SensorwatchError):
        session.snapshot()


def test_snapshot_outlives_session():
    """A snapshot owns its data, so it stays usable after the session closes."""
    session = _live_session_or_skip()
    snapshot = session.snapshot()
    session.close()
    # Still readable.
    assert isinstance(len(snapshot), int)
    if len(snapshot) > 0:
        _ = snapshot[0]
    snapshot.close()

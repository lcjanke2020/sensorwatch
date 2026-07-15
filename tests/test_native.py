"""Tests for the native cffi binding (sensorwatch.native).

These exercise the compiled extension ``sensorwatch._sw_cffi`` and its Pythonic
wrappers. The module-level import requires the extension to be built; CI builds
it before running pytest. Tests that need a live sensor source are skipped when
HWiNFO is unavailable, and the Windows-only / non-Windows-only paths guard on
``sys.platform`` so the same suite runs on both CI legs. The populated-Snapshot
accessor surface is additionally covered cross-platform via synthetic buffers
parsed through ``sw_snapshot_from_buffer`` (ABI 0.2.0).
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from sensorwatch import native
from sensorwatch._native import _check, ffi, lib
from sensorwatch.hwinfo_shm import SENSOR_TYPES, _parse_shared_memory, read_sensors
from sensorwatch.native import ReadingType, SensorwatchError, Session, Snapshot

from test_hwinfo_shm import _build_buffer  # same-directory synthetic-buffer builder

IS_WINDOWS = sys.platform == "win32"

# Map the typed enum back to the pure-Python reader's string category.
_TYPE_TO_STR = {ReadingType(code): name for code, name in SENSOR_TYPES.items()}


def _type_str(rt: ReadingType) -> str:
    return _TYPE_TO_STR.get(rt, f"unknown({int(rt)})")


def _norm_type(label: str) -> str:
    # The C ABI normalizes any unrecognized source code to UNKNOWN ("unknown(255)"),
    # while the pure-Python reader preserves the raw code ("unknown(<N>)"). Collapse
    # both so the shape comparison doesn't false-fail on an unrecognized live type.
    return "unknown" if label.startswith("unknown") else label


# --- Import + ABI version smoke -------------------------------------------------

def test_abi_version_matches_expected():
    from sensorwatch._native import EXPECTED_ABI_MAJOR, EXPECTED_ABI_MINOR
    version = lib.sw_api_version()
    assert version // 10000 == EXPECTED_ABI_MAJOR
    assert (version // 100) % 100 == EXPECTED_ABI_MINOR  # pinned pre-1.0 (0.2.x)


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
        if len(snapshot):  # an empty snapshot has no source identity to report
            assert snapshot.source == "HWiNFO"

        native_shape = [
            (r.sensor, r.reading, r.unit, _norm_type(_type_str(r.type)))
            for r in snapshot
        ]
        reference_shape = [
            (p.sensor_name, p.reading_name, p.unit, _norm_type(p.sensor_type))
            for p in reference
        ]
        # read_sensors() and snapshot() are two independent live reads; HWiNFO may
        # add/remove a sensor between them. Treat that rare TOCTOU drift as a skip,
        # and compare order-independently (the captures are distinct).
        if len(native_shape) != len(reference_shape):
            pytest.skip("sensor set changed between the two live reads (TOCTOU)")
        assert sorted(native_shape) == sorted(reference_shape)


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
        # Touch the snapshot first so the source cache is populated; the
        # closed-state guard must still fire on the cached path.
        _ = snapshot.source
        if len(snapshot):
            _ = snapshot[0]
        snapshot.close()
        snapshot.close()  # idempotent
        with pytest.raises(SensorwatchError):
            _ = snapshot.source
        with pytest.raises(SensorwatchError):
            _ = len(snapshot)
        with pytest.raises(SensorwatchError):
            _ = snapshot[0]
        with pytest.raises(SensorwatchError):
            list(snapshot)  # iteration is guarded even for an empty snapshot
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


# --- Cross-platform synthetic-snapshot coverage ----------------------------------
#
# sw_snapshot_from_buffer parses a caller-supplied buffer with the same validating
# parser the live path uses, so the populated accessor surface (the bodies of the
# live tests above) runs on every CI leg. The buffers come from the same builder
# the pure-Python parser tests use, which also makes these a native-vs-pure
# differential oracle over identical bytes.

_SYNTH_SENSORS = ["MEG Ai1600T"]
_SYNTH_ENTRIES = [
    (2, 0, "+12V", "V", 12.03),        # Voltage
    (1, 0, "CPU Package", "C", 41.5),  # Temperature
    (3, 0, "Pump", "RPM", 1450.0),     # Fan
]


def _snapshot_from_buffer(data: bytes) -> Snapshot:
    out = ffi.new("sw_snapshot_t **")
    cbuf = ffi.from_buffer(data)  # zero-copy view; the parser copies out what it keeps
    _check(lib.sw_snapshot_from_buffer(ffi.cast("const uint8_t *", cbuf), len(data), out))
    return Snapshot(out[0])


def _synthetic_buffer() -> bytes:
    return _build_buffer(sensors=_SYNTH_SENSORS, entries=_SYNTH_ENTRIES)


def test_synthetic_snapshot_accessor_surface():
    snapshot = _snapshot_from_buffer(_synthetic_buffer())
    try:
        assert len(snapshot) == 3
        assert snapshot.source == "HWiNFO"

        first = snapshot[0]
        assert first.sensor == "MEG Ai1600T"
        assert first.reading == "+12V"
        assert first.unit == "V"
        assert first.type is ReadingType.VOLTAGE
        assert first.value == 12.03
        assert first.minimum == first.maximum == first.average == 12.03

        readings = list(snapshot)
        assert len(readings) == 3
        assert [r.type for r in readings] == [
            ReadingType.VOLTAGE,
            ReadingType.TEMPERATURE,
            ReadingType.FAN,
        ]
    finally:
        snapshot.close()


def test_synthetic_snapshot_reading_fields_are_typed():
    snapshot = _snapshot_from_buffer(_synthetic_buffer())
    try:
        reading = snapshot[0]
        assert isinstance(reading.type, ReadingType)
        assert isinstance(reading.value, float)
        assert isinstance(reading.minimum, float)
        assert isinstance(reading.maximum, float)
        assert isinstance(reading.average, float)
        assert isinstance(reading.sensor, str)
        assert isinstance(reading.unit, str)
    finally:
        snapshot.close()


def test_synthetic_snapshot_indexing_and_bounds():
    snapshot = _snapshot_from_buffer(_synthetic_buffer())
    try:
        n = len(snapshot)
        assert snapshot[-1] == snapshot[n - 1]  # negative indexing
        with pytest.raises(IndexError):
            _ = snapshot[n]
        with pytest.raises(TypeError):
            _ = snapshot["nope"]
    finally:
        snapshot.close()


def test_closed_synthetic_snapshot_is_guarded():
    snapshot = _snapshot_from_buffer(_synthetic_buffer())
    # Touch the snapshot first so the source cache is populated; the closed-state
    # guard must still fire on the cached path.
    _ = snapshot.source
    _ = snapshot[0]
    snapshot.close()
    snapshot.close()  # idempotent
    with pytest.raises(SensorwatchError):
        _ = snapshot.source
    with pytest.raises(SensorwatchError):
        _ = len(snapshot)
    with pytest.raises(SensorwatchError):
        _ = snapshot[0]
    with pytest.raises(SensorwatchError):
        list(snapshot)


def test_synthetic_snapshot_matches_pure_python_parser():
    """Differential oracle: the same bytes through the C parser (via the cffi
    binding) and the pure-Python reference parser yield identical readings."""
    buf = _synthetic_buffer()
    reference = _parse_shared_memory(buf)
    assert reference is not None
    snapshot = _snapshot_from_buffer(buf)
    try:
        native_rows = [
            (r.sensor, r.reading, r.unit, _norm_type(_type_str(r.type)),
             r.value, r.minimum, r.maximum, r.average)
            for r in snapshot
        ]
        reference_rows = [
            (p.sensor_name, p.reading_name, p.unit, _norm_type(p.sensor_type),
             p.value, p.value_min, p.value_max, p.value_avg)
            for p in reference
        ]
        # Same buffer, same order: exact equality, not just shape.
        assert native_rows == reference_rows
    finally:
        snapshot.close()


def test_corpus_seed_parses_identically_in_both_parsers():
    """Anchor both parsers on a committed fixture: the fuzz seed corpus is built by
    the C test-util builder, so these bytes are shared across the language suites
    (the C cmocka tests and the Rust CLI e2e consume the same files)."""
    seed = Path(__file__).parent / "fuzz" / "corpus" / "parse" / "valid_multi.bin"
    data = seed.read_bytes()
    reference = _parse_shared_memory(data)
    assert reference  # a valid seed parses to at least one reading
    snapshot = _snapshot_from_buffer(data)
    try:
        assert len(snapshot) == len(reference)
        for r, p in zip(snapshot, reference):
            assert (r.sensor, r.reading, r.unit) == (p.sensor_name, p.reading_name, p.unit)
            assert (r.value, r.minimum, r.maximum, r.average) == (
                p.value, p.value_min, p.value_max, p.value_avg,
            )
    finally:
        snapshot.close()


def test_from_buffer_rejects_malformed():
    out = ffi.new("sw_snapshot_t **")

    short = b"\x00" * 47  # shorter than a header
    cbuf = ffi.from_buffer(short)
    assert lib.sw_snapshot_from_buffer(
        ffi.cast("const uint8_t *", cbuf), len(short), out
    ) == lib.SW_ERR_CORRUPT_DATA
    assert out[0] == ffi.NULL

    bad = b"\x00" * 48  # header-sized, wrong magic
    cbuf = ffi.from_buffer(bad)
    assert lib.sw_snapshot_from_buffer(
        ffi.cast("const uint8_t *", cbuf), len(bad), out
    ) == lib.SW_ERR_BAD_MAGIC
    assert out[0] == ffi.NULL

    assert lib.sw_snapshot_from_buffer(ffi.NULL, 1, out) == lib.SW_ERR_NULL_POINTER

"""Tests for sensorwatch.hwinfo_shm._parse_shared_memory.

The HWiNFO shared-memory header is untrusted input (SECURITY.md 1.3 / 8.1).
These tests feed synthetic byte buffers — no Win32, no live HWiNFO — and assert
that a well-formed buffer parses correctly while malformed headers return None
(or fall back safely) instead of crashing.

Buffer layout helpers below mirror the offsets documented in hwinfo_shm.py.
"""

import struct

from sensorwatch.hwinfo_shm import (
    HEADER_MAGIC,
    HEADER_SIZE,
    MAX_ENTRY_COUNT,
    MAX_SENSOR_COUNT,
    MIN_ENTRY_SIZE,
    MIN_SENSOR_SIZE,
    _parse_shared_memory,
)

# Header field offsets (see hwinfo_shm.py header layout comment).
OFF_MAGIC = 0x00
OFF_SENSOR_OFF = 0x14
OFF_SENSOR_SIZE = 0x18
OFF_SENSOR_COUNT = 0x1C
OFF_ENTRY_OFF = 0x20
OFF_ENTRY_SIZE = 0x24
OFF_ENTRY_COUNT = 0x28


def _pack_name(s: str, length: int) -> bytes:
    """Encode ``s`` as a fixed-width, NUL-terminated cp1252 field."""
    raw = s.encode("cp1252")[: length - 1]  # leave room for a terminator
    return raw + b"\x00" * (length - len(raw))


def _build_buffer(sensors, entries):
    """Build a well-formed HWiNFO buffer.

    ``sensors``: list of display names (placed in the name_user field).
    ``entries``: list of ``(type, sensor_idx, reading_name, unit, value)``.

    Corruption tests start from this valid buffer and patch a single header
    field via :func:`_patch_u32`.
    """
    sensor_size = MIN_SENSOR_SIZE
    entry_size = MIN_ENTRY_SIZE
    sensor_off = HEADER_SIZE

    sensor_region = bytearray(len(sensors) * sensor_size)
    for i, name in enumerate(sensors):
        base = i * sensor_size
        struct.pack_into("<I", sensor_region, base + 0, i)   # id
        struct.pack_into("<I", sensor_region, base + 4, 0)   # instance
        sensor_region[base + 8: base + 136] = _pack_name("", 128)        # name_original
        sensor_region[base + 136: base + 264] = _pack_name(name, 128)    # name_user

    entry_region = bytearray(len(entries) * entry_size)
    for i, (etype, sidx, reading, unit, value) in enumerate(entries):
        base = i * entry_size
        struct.pack_into("<I", entry_region, base + 0, etype)
        struct.pack_into("<I", entry_region, base + 4, sidx)
        struct.pack_into("<I", entry_region, base + 8, i)    # id
        entry_region[base + 12: base + 140] = _pack_name("", 128)        # name_original
        entry_region[base + 140: base + 268] = _pack_name(reading, 128)  # name_user
        entry_region[base + 268: base + 284] = _pack_name(unit, 16)      # unit
        struct.pack_into("<dddd", entry_region, base + 284, value, value, value, value)

    entry_off = sensor_off + len(sensor_region)
    total = entry_off + len(entry_region)
    buf = bytearray(max(total, HEADER_SIZE))

    struct.pack_into("<I", buf, OFF_MAGIC, HEADER_MAGIC)
    struct.pack_into("<I", buf, OFF_SENSOR_OFF, sensor_off)
    struct.pack_into("<I", buf, OFF_SENSOR_SIZE, sensor_size)
    struct.pack_into("<I", buf, OFF_SENSOR_COUNT, len(sensors))
    struct.pack_into("<I", buf, OFF_ENTRY_OFF, entry_off)
    struct.pack_into("<I", buf, OFF_ENTRY_SIZE, entry_size)
    struct.pack_into("<I", buf, OFF_ENTRY_COUNT, len(entries))

    buf[sensor_off: sensor_off + len(sensor_region)] = sensor_region
    buf[entry_off: entry_off + len(entry_region)] = entry_region
    return bytes(buf)


def _valid_buffer():
    return _build_buffer(
        sensors=["MEG Ai1600T"],
        entries=[(2, 0, "+12V", "V", 12.03)],  # type 2 == Voltage
    )


def _patch_u32(buf: bytes, offset: int, value: int) -> bytes:
    b = bytearray(buf)
    struct.pack_into("<I", b, offset, value)
    return bytes(b)


# --- Happy path -------------------------------------------------------------

def test_valid_buffer_parses_reading():
    readings = _parse_shared_memory(_valid_buffer())
    assert readings is not None
    assert len(readings) == 1
    r = readings[0]
    assert r.sensor_name == "MEG Ai1600T"
    assert r.reading_name == "+12V"
    assert r.sensor_type == "Voltage"
    assert r.unit == "V"
    assert r.value == 12.03
    assert r.value_min == 12.03
    assert r.value_max == 12.03
    assert r.value_avg == 12.03


def test_unit_decoded_as_cp1252():
    # HWiNFO writes "°C" as the single cp1252 byte 0xB0 followed by 'C'.
    readings = _parse_shared_memory(_build_buffer(["CPU"], [(1, 0, "Core", "°C", 42.0)]))
    assert readings is not None
    assert readings[0].unit == "°C"


def test_invalid_sensor_idx_falls_back_to_synthetic_name():
    # Only sensor index 0 exists; an entry referencing index 5 must not crash.
    readings = _parse_shared_memory(_build_buffer(["MEG Ai1600T"], [(2, 5, "+12V", "V", 12.0)]))
    assert readings is not None
    assert len(readings) == 1
    assert readings[0].sensor_name == "sensor_5"


# --- Malformed headers all return None safely -------------------------------

def test_buffer_smaller_than_header_returns_none():
    assert _parse_shared_memory(b"\x00" * (HEADER_SIZE - 1)) is None


def test_empty_buffer_returns_none():
    assert _parse_shared_memory(b"") is None


def test_bad_magic_returns_none():
    assert _parse_shared_memory(_patch_u32(_valid_buffer(), OFF_MAGIC, 0xDEADBEEF)) is None


def test_sensor_size_below_minimum_returns_none():
    buf = _patch_u32(_valid_buffer(), OFF_SENSOR_SIZE, MIN_SENSOR_SIZE - 1)
    assert _parse_shared_memory(buf) is None


def test_entry_size_below_minimum_returns_none():
    buf = _patch_u32(_valid_buffer(), OFF_ENTRY_SIZE, MIN_ENTRY_SIZE - 1)
    assert _parse_shared_memory(buf) is None


def test_sensor_count_above_max_returns_none():
    buf = _patch_u32(_valid_buffer(), OFF_SENSOR_COUNT, MAX_SENSOR_COUNT + 1)
    assert _parse_shared_memory(buf) is None


def test_entry_count_above_max_returns_none():
    buf = _patch_u32(_valid_buffer(), OFF_ENTRY_COUNT, MAX_ENTRY_COUNT + 1)
    assert _parse_shared_memory(buf) is None


def test_section_offset_overlapping_header_returns_none():
    buf = _patch_u32(_valid_buffer(), OFF_SENSOR_OFF, HEADER_SIZE - 1)
    assert _parse_shared_memory(buf) is None


def test_sections_exceeding_region_returns_none():
    # A count within the sanity cap but too large for the buffer must be caught
    # by the region-bounds check, not by reading past the end.
    buf = _patch_u32(_valid_buffer(), OFF_SENSOR_COUNT, MAX_SENSOR_COUNT)
    assert _parse_shared_memory(buf) is None

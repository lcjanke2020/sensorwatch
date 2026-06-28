"""HWiNFO64 shared memory reader.

Reads sensor data from the HWiNFO64 shared memory interface
(Global\\HWiNFO_SENS_SM2). Requires HWiNFO64 running with
"Shared Memory Support" enabled in settings and the sensors window open.

The whole mapped region is copied into an immutable ``bytes`` buffer once (sized
via ``VirtualQuery``) and parsed with ``struct.unpack_from``. Header fields are
untrusted input, so they are bounds-checked before use and any out-of-range
offset surfaces as a caught ``struct.error`` (logged, returns ``None``) rather
than an unrecoverable access violation. See SECURITY.md sections 1.3 and 8.1.

References:
  - Shared memory layout: https://gist.github.com/namazso/0c37be5a53863954c8c8279f66cfb1cc
  - HWiNFO forum: https://www.hwinfo.com/forum/threads/shared-memory-support.18/
"""

from __future__ import annotations

import ctypes
import logging
import re
import struct
import sys
from dataclasses import dataclass

log = logging.getLogger(__name__)

# --- Win32 constants ---
FILE_MAP_READ = 0x0004

# --- Shared memory constants ---
SHM_NAME = "Global\\HWiNFO_SENS_SM2"

# --- Sensor type enum ---
SENSOR_TYPES = {
    0: "None",
    1: "Temperature",
    2: "Voltage",
    3: "Fan",
    4: "Current",
    5: "Power",
    6: "Clock",
    7: "Usage",
    8: "Other",
}

# --- Header field offsets (verified against HWiNFO v8.x) ---
# Header is 48 bytes:
#   0x00: magic (uint32)        — 0x53695748
#   0x04: version (uint32)
#   0x08: version2 (uint32)
#   0x0C: last_update (int64)
#   0x14: sensor_section_offset (uint32)
#   0x18: sensor_element_size (uint32)
#   0x1C: sensor_element_count (uint32)
#   0x20: entry_section_offset (uint32)
#   0x24: entry_element_size (uint32)
#   0x28: entry_element_count (uint32)
#   0x2C: poll_time (uint32)    — polling period in ms
HEADER_SIZE = 48
HEADER_MAGIC = 0x53695748

# Sensor element layout (actual size varies by version, read from header):
#   0x00: id (uint32)
#   0x04: instance (uint32)
#   0x08: name_original (char[128])
#   0x88: name_user (char[128])
# Entry element layout (actual size varies by version, read from header):
#   0x000: type (uint32)          — SensorType enum
#   0x004: sensor_index (uint32)  — index into sensor array
#   0x008: id (uint32)
#   0x00C: name_original (char[128])
#   0x08C: name_user (char[128])
#   0x10C: unit (char[16])
#   0x11C: value (double)
#   0x124: value_min (double)
#   0x12C: value_max (double)
#   0x134: value_avg (double)

# Minimum struct sizes we require for parsing (fields we actually read)
MIN_SENSOR_SIZE = 264   # id(4) + instance(4) + name_orig(128) + name_user(128)
MIN_ENTRY_SIZE = 316    # type(4) + idx(4) + id(4) + name_orig(128) + name_user(128) + unit(16) + 4×double(32)

# Sanity caps for the untrusted header fields (see SECURITY.md §1.3). HWiNFO
# typically maps ~1-4 MB with a few hundred sensors and a few thousand entries;
# these ceilings are generous but bound worst-case work on a corrupt header.
MAX_TOTAL_SIZE = 64 * 1024 * 1024
MAX_SENSOR_COUNT = 4096
MAX_ENTRY_COUNT = 65536

# Strip C0/C1 control characters from decoded strings — defensive, since these
# strings may later flow into the planned REST/agent layers (SECURITY.md §8.2).
_CONTROL_CHARS = re.compile(r"[\x00-\x1f\x7f-\x9f]")


@dataclass
class SensorReading:
    sensor_name: str
    reading_name: str
    sensor_type: str
    value: float
    value_min: float
    value_max: float
    value_avg: float
    unit: str

    def to_dict(self) -> dict:
        return {
            "sensor": self.sensor_name,
            "reading": self.reading_name,
            "type": self.sensor_type,
            "value": self.value,
            "min": self.value_min,
            "max": self.value_max,
            "avg": self.value_avg,
            "unit": self.unit,
        }


def _decode(raw: bytes, offset: int, length: int) -> str:
    """Decode a null-terminated string from a raw byte buffer, stripping controls.

    HWiNFO writes these fields in the Windows ANSI code page (cp1252), not UTF-8 —
    e.g. the temperature unit "°C" is the single byte 0xB0. cp1252 never raises and
    maps 0xB0 -> "°"; the handful of undefined bytes fall back to the replacement
    character.
    """
    s = raw[offset:offset + length].split(b"\x00")[0].decode("cp1252", errors="replace")
    return _CONTROL_CHARS.sub("", s)


# Win32 bindings are set up only on Windows so the module imports cleanly
# elsewhere (read_sensors() then fails fast with a clear message).
if sys.platform == "win32":
    import ctypes.wintypes

    class MEMORY_BASIC_INFORMATION(ctypes.Structure):
        # 64-bit-safe layout: explicit alignment padding around the SIZE_T field.
        _fields_ = [
            ("BaseAddress", ctypes.c_void_p),
            ("AllocationBase", ctypes.c_void_p),
            ("AllocationProtect", ctypes.wintypes.DWORD),
            ("__alignment1", ctypes.wintypes.DWORD),
            ("RegionSize", ctypes.c_size_t),
            ("State", ctypes.wintypes.DWORD),
            ("Protect", ctypes.wintypes.DWORD),
            ("Type", ctypes.wintypes.DWORD),
            ("__alignment2", ctypes.wintypes.DWORD),
        ]

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _kernel32.OpenFileMappingW.restype = ctypes.wintypes.HANDLE
    _kernel32.OpenFileMappingW.argtypes = [
        ctypes.wintypes.DWORD, ctypes.wintypes.BOOL, ctypes.wintypes.LPCWSTR,
    ]
    _kernel32.MapViewOfFile.restype = ctypes.c_void_p
    _kernel32.MapViewOfFile.argtypes = [
        ctypes.wintypes.HANDLE, ctypes.wintypes.DWORD,
        ctypes.wintypes.DWORD, ctypes.wintypes.DWORD, ctypes.c_size_t,
    ]
    _kernel32.UnmapViewOfFile.argtypes = [ctypes.c_void_p]
    _kernel32.UnmapViewOfFile.restype = ctypes.wintypes.BOOL
    _kernel32.CloseHandle.argtypes = [ctypes.wintypes.HANDLE]
    _kernel32.CloseHandle.restype = ctypes.wintypes.BOOL
    _kernel32.VirtualQuery.restype = ctypes.c_size_t
    _kernel32.VirtualQuery.argtypes = [
        ctypes.c_void_p, ctypes.POINTER(MEMORY_BASIC_INFORMATION), ctypes.c_size_t,
    ]
else:  # pragma: no cover - exercised only on non-Windows imports
    MEMORY_BASIC_INFORMATION = None
    _kernel32 = None


def _mapped_region_size(ptr: int) -> int:
    """Return the committed size of the memory region at ``ptr`` (0 on failure)."""
    mbi = MEMORY_BASIC_INFORMATION()
    if _kernel32.VirtualQuery(ptr, ctypes.byref(mbi), ctypes.sizeof(mbi)) == 0:
        return 0
    return int(mbi.RegionSize)


def read_sensors() -> list[SensorReading] | None:
    """Read all sensor entries from HWiNFO64 shared memory.

    Returns a list of SensorReading objects, or None if the shared memory
    is not available (HWiNFO64 not running, shared memory not enabled,
    sensors window not open, or a malformed mapping).
    """
    if _kernel32 is None:
        log.error("sensorwatch's HWiNFO reader requires Windows (kernel32 unavailable).")
        return None

    handle = _kernel32.OpenFileMappingW(FILE_MAP_READ, False, SHM_NAME)
    if not handle:
        return None

    ptr = _kernel32.MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0)
    if not ptr:
        log.warning("MapViewOfFile failed (error %d)", ctypes.get_last_error())
        _kernel32.CloseHandle(handle)
        return None

    try:
        return _read_from_mapped(ptr)
    except Exception:
        log.exception("Error reading HWiNFO shared memory")
        return None
    finally:
        _kernel32.UnmapViewOfFile(ptr)
        _kernel32.CloseHandle(handle)


def _read_from_mapped(ptr: int) -> list[SensorReading] | None:
    """Copy the mapped region into an immutable ``bytes`` buffer and parse it.

    The copy is sized via ``VirtualQuery`` so untrusted header offsets can never
    read past the mapping. Parsing the buffer is delegated to the pure,
    Win32-free :func:`_parse_shared_memory` helper.
    """
    region_size = _mapped_region_size(ptr)
    if region_size < HEADER_SIZE:
        log.warning("Mapped region too small (%d bytes)", region_size)
        return None

    # Copy the region once; every read below is bounds-checked by struct/bytes.
    read_size = min(region_size, MAX_TOTAL_SIZE)
    buf = bytes((ctypes.c_char * read_size).from_address(ptr))
    return _parse_shared_memory(buf)


def _parse_shared_memory(buf: bytes) -> list[SensorReading] | None:
    """Parse an HWiNFO shared-memory snapshot from an immutable byte buffer.

    ``buf`` is an in-memory copy of the mapped region. Header fields are
    untrusted, so they are bounds-checked against ``len(buf)`` before use and any
    residual out-of-range access surfaces as a caught ``struct.error`` (logged,
    returns ``None``) instead of crashing. This is the pure core of the reader:
    :func:`read_sensors` supplies ``buf`` from the live Win32 mapping, while the
    tests feed synthetic buffers directly. See SECURITY.md sections 1.3 and 8.1.
    """
    if len(buf) < HEADER_SIZE:
        log.warning("Buffer too small (%d bytes)", len(buf))
        return None

    try:
        magic = struct.unpack_from("<I", buf, 0x00)[0]
        if magic != HEADER_MAGIC:
            log.warning("Bad magic: 0x%08X (expected 0x%08X)", magic, HEADER_MAGIC)
            return None

        sensor_off = struct.unpack_from("<I", buf, 0x14)[0]
        sensor_size = struct.unpack_from("<I", buf, 0x18)[0]
        sensor_count = struct.unpack_from("<I", buf, 0x1C)[0]
        entry_off = struct.unpack_from("<I", buf, 0x20)[0]
        entry_size = struct.unpack_from("<I", buf, 0x24)[0]
        entry_count = struct.unpack_from("<I", buf, 0x28)[0]

        # --- Validate untrusted header fields before using them as bounds (§1.3) ---
        if sensor_size < MIN_SENSOR_SIZE or entry_size < MIN_ENTRY_SIZE:
            log.warning("Element size too small (sensor=%d, entry=%d) — incompatible version?",
                        sensor_size, entry_size)
            return None
        if sensor_count > MAX_SENSOR_COUNT or entry_count > MAX_ENTRY_COUNT:
            log.warning("Unreasonable counts (sensors=%d, entries=%d)", sensor_count, entry_count)
            return None

        if sensor_off < HEADER_SIZE or entry_off < HEADER_SIZE:
            # An offset inside the header would otherwise parse header bytes as data.
            log.warning("Section offset overlaps header (sensor_off=%d, entry_off=%d, header=%d)",
                        sensor_off, entry_off, HEADER_SIZE)
            return None

        sensor_end = sensor_off + sensor_count * sensor_size
        entry_end = entry_off + entry_count * entry_size
        if sensor_end > len(buf) or entry_end > len(buf):
            log.warning("Header sections exceed mapped region (sensor_end=%d, entry_end=%d, region=%d)",
                        sensor_end, entry_end, len(buf))
            return None

        # The sensor and entry arrays are disjoint regions in valid data. An
        # overlap means a corrupt header would have the two arrays aliasing each
        # other's bytes; reject rather than emit semantically bogus readings.
        if sensor_off < entry_end and entry_off < sensor_end:
            log.warning("Header sections overlap (sensor=[%d,%d), entry=[%d,%d))",
                        sensor_off, sensor_end, entry_off, entry_end)
            return None

        # Build sensor name lookup: index -> display name
        sensor_names: dict[int, str] = {}
        for i in range(sensor_count):
            base = sensor_off + i * sensor_size
            name_user = _decode(buf, base + 136, 128)
            name_orig = _decode(buf, base + 8, 128)
            sensor_names[i] = name_user or name_orig

        # Read all entries
        readings: list[SensorReading] = []
        for i in range(entry_count):
            base = entry_off + i * entry_size
            etype, sensor_idx, _ = struct.unpack_from("<III", buf, base)
            reading_orig = _decode(buf, base + 12, 128)
            reading_user = _decode(buf, base + 140, 128)
            unit = _decode(buf, base + 268, 16)
            value, value_min, value_max, value_avg = struct.unpack_from("<dddd", buf, base + 284)

            if not (0 <= sensor_idx < sensor_count):
                log.warning("Entry %d has invalid sensor_idx %d (count=%d)", i, sensor_idx, sensor_count)
                sensor_name = f"sensor_{sensor_idx}"
            else:
                sensor_name = sensor_names.get(sensor_idx, f"sensor_{sensor_idx}")

            readings.append(SensorReading(
                sensor_name=sensor_name,
                reading_name=reading_user or reading_orig,
                sensor_type=SENSOR_TYPES.get(etype, f"unknown({etype})"),
                value=value,
                value_min=value_min,
                value_max=value_max,
                value_avg=value_avg,
                unit=unit,
            ))
    except struct.error as exc:
        # Defensive backstop: the bounds checks above should prevent this, but a
        # corrupt header must never crash the caller. Reaching here means input
        # slipped past validation (untrusted/incompatible producer), not an
        # internal bug — log a warning, not a full traceback. See SECURITY.md §1.3.
        log.warning("Malformed HWiNFO shared memory buffer: %s", exc)
        return None

    log.debug("Read %d sensors, %d entries from HWiNFO shared memory", sensor_count, entry_count)
    return readings

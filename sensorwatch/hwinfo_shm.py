"""HWiNFO64 shared memory reader.

Reads sensor data from the HWiNFO64 shared memory interface
(Global\\HWiNFO_SENS_SM2). Requires HWiNFO64 running with
"Shared Memory Support" enabled in settings and the sensors window open.

References:
  - Shared memory layout: https://gist.github.com/namazso/0c37be5a53863954c8c8279f66cfb1cc
  - HWiNFO forum: https://www.hwinfo.com/forum/threads/shared-memory-support.18/
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes
import logging
import struct
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
#   ... (remainder is padding/reserved in newer versions)

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
#   ... (remainder is padding/reserved in newer versions)


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
    """Decode a null-terminated string from a raw byte buffer."""
    return raw[offset:offset + length].split(b"\x00")[0].decode("utf-8", errors="replace")


# Minimum struct sizes we require for parsing (fields we actually read)
MIN_SENSOR_SIZE = 264   # id(4) + instance(4) + name_orig(128) + name_user(128)
MIN_ENTRY_SIZE = 316    # type(4) + idx(4) + id(4) + name_orig(128) + name_user(128) + unit(16) + 4×double(32)

# Configure Win32 function signatures for 64-bit pointer safety
_kernel32 = ctypes.windll.kernel32
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
_kernel32.CloseHandle.argtypes = [ctypes.wintypes.HANDLE]


def read_sensors() -> list[SensorReading] | None:
    """Read all sensor entries from HWiNFO64 shared memory.

    Returns a list of SensorReading objects, or None if the shared memory
    is not available (HWiNFO64 not running, shared memory not enabled,
    or sensors window not open).
    """
    handle = _kernel32.OpenFileMappingW(FILE_MAP_READ, False, SHM_NAME)
    if not handle:
        return None

    ptr = _kernel32.MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0)
    if not ptr:
        log.warning("MapViewOfFile failed (error %d)", ctypes.GetLastError())
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
    """Parse the mapped shared memory and return all sensor readings."""
    # Read and validate header
    header_raw = bytes((ctypes.c_char * HEADER_SIZE).from_address(ptr))

    magic = struct.unpack_from("<I", header_raw, 0x00)[0]
    if magic != HEADER_MAGIC:
        log.warning("Bad magic: 0x%08X (expected 0x%08X)", magic, HEADER_MAGIC)
        return None

    sensor_off = struct.unpack_from("<I", header_raw, 0x14)[0]
    sensor_size = struct.unpack_from("<I", header_raw, 0x18)[0]
    sensor_count = struct.unpack_from("<I", header_raw, 0x1C)[0]
    entry_off = struct.unpack_from("<I", header_raw, 0x20)[0]
    entry_size = struct.unpack_from("<I", header_raw, 0x24)[0]
    entry_count = struct.unpack_from("<I", header_raw, 0x28)[0]

    if sensor_size < MIN_SENSOR_SIZE:
        log.warning("Sensor element size %d < expected %d — incompatible version?", sensor_size, MIN_SENSOR_SIZE)
        return None
    if entry_size < MIN_ENTRY_SIZE:
        log.warning("Entry element size %d < expected %d — incompatible version?", entry_size, MIN_ENTRY_SIZE)
        return None

    # Build sensor name lookup: index -> display name
    sensor_names: dict[int, str] = {}
    for i in range(sensor_count):
        addr = ptr + sensor_off + i * sensor_size
        raw = bytes((ctypes.c_char * min(sensor_size, 264)).from_address(addr))
        name_user = _decode(raw, 136, 128)
        name_orig = _decode(raw, 8, 128)
        sensor_names[i] = name_user or name_orig

    # Read all entries
    readings: list[SensorReading] = []
    for i in range(entry_count):
        addr = ptr + entry_off + i * entry_size
        raw = bytes((ctypes.c_char * min(entry_size, 316)).from_address(addr))

        etype, sensor_idx, _ = struct.unpack_from("<III", raw, 0)
        reading_orig = _decode(raw, 12, 128)
        reading_user = _decode(raw, 140, 128)
        unit = _decode(raw, 268, 16)
        value, value_min, value_max, value_avg = struct.unpack_from("<dddd", raw, 284)

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

    log.debug("Read %d sensors, %d entries from HWiNFO shared memory", sensor_count, entry_count)
    return readings

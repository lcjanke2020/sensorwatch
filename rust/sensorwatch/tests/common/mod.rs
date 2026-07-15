//! Synthetic HWiNFO shared-memory buffer builder — the Rust counterpart of the C
//! test util (`tests/c/sw_test_util.c`) and Python's `_build_buffer`
//! (`tests/test_hwinfo_shm.py`). Builds well-formed little-endian buffers from the
//! wire-format layout in `src/sw_internal.h`; tests that need malformed input
//! patch the returned bytes afterwards.
//!
//! Names/units are packed as raw bytes into fixed-width zero-padded fields the
//! parser decodes as cp1252 — keep test strings ASCII so they round-trip
//! byte-for-byte through that decode.

pub const HEADER_SIZE: usize = 48;

const HEADER_MAGIC: u32 = 0x5369_5748; // bytes 'H' 'W' 'i' 'S' read little-endian

const OFF_MAGIC: usize = 0x00;
const OFF_SENSOR_OFFSET: usize = 0x14;
const OFF_SENSOR_SIZE: usize = 0x18;
const OFF_SENSOR_COUNT: usize = 0x1C;
const OFF_ENTRY_OFFSET: usize = 0x20;
const OFF_ENTRY_SIZE: usize = 0x24;
const OFF_ENTRY_COUNT: usize = 0x28;

const SENSOR_OFF_NAME_ORIG: usize = 8;
const SENSOR_OFF_NAME_USER: usize = 136;
const NAME_FIELD_LEN: usize = 128;

const ENTRY_OFF_NAME_ORIG: usize = 12;
const ENTRY_OFF_NAME_USER: usize = 140;
const ENTRY_OFF_UNIT: usize = 268;
const UNIT_FIELD_LEN: usize = 16;
const ENTRY_OFF_VALUES: usize = 284;

const MIN_SENSOR_SIZE: usize = 264;
const MIN_ENTRY_SIZE: usize = 316;

pub struct Sensor<'a> {
    pub name_user: Option<&'a str>,
    pub name_orig: Option<&'a str>,
}

impl<'a> Sensor<'a> {
    pub fn named(name_user: &'a str) -> Self {
        Sensor {
            name_user: Some(name_user),
            name_orig: None,
        }
    }
}

pub struct Entry<'a> {
    pub type_code: u32,
    pub sensor_idx: u32,
    pub reading_user: Option<&'a str>,
    pub reading_orig: Option<&'a str>,
    pub unit: Option<&'a str>,
    pub value: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub average: f64,
}

impl<'a> Entry<'a> {
    /// An entry whose four statistics all equal `value` (mirrors the C builder).
    pub fn flat(
        type_code: u32,
        sensor_idx: u32,
        reading_user: &'a str,
        unit: &'a str,
        value: f64,
    ) -> Self {
        Entry {
            type_code,
            sensor_idx,
            reading_user: Some(reading_user),
            reading_orig: None,
            unit: Some(unit),
            value,
            minimum: value,
            maximum: value,
            average: value,
        }
    }
}

/// Build a well-formed buffer: 48-byte header, then `sensors.len()` sensor
/// elements of the minimum size, then `entries.len()` entry elements of the
/// minimum size — the same shape the C and Python builders produce.
pub fn build_buffer(sensors: &[Sensor<'_>], entries: &[Entry<'_>]) -> Vec<u8> {
    let sensor_off = HEADER_SIZE;
    let entry_off = sensor_off + sensors.len() * MIN_SENSOR_SIZE;
    let total = (entry_off + entries.len() * MIN_ENTRY_SIZE).max(HEADER_SIZE);
    let mut buf = vec![0u8; total];

    put_u32(&mut buf, OFF_MAGIC, HEADER_MAGIC);
    put_u32(&mut buf, OFF_SENSOR_OFFSET, sensor_off as u32);
    put_u32(&mut buf, OFF_SENSOR_SIZE, MIN_SENSOR_SIZE as u32);
    put_u32(&mut buf, OFF_SENSOR_COUNT, sensors.len() as u32);
    put_u32(&mut buf, OFF_ENTRY_OFFSET, entry_off as u32);
    put_u32(&mut buf, OFF_ENTRY_SIZE, MIN_ENTRY_SIZE as u32);
    put_u32(&mut buf, OFF_ENTRY_COUNT, entries.len() as u32);

    for (i, sensor) in sensors.iter().enumerate() {
        let base = sensor_off + i * MIN_SENSOR_SIZE;
        put_u32(&mut buf, base, i as u32); // id
        put_u32(&mut buf, base + 4, 0); // instance
        pack_name(
            &mut buf,
            base + SENSOR_OFF_NAME_ORIG,
            NAME_FIELD_LEN,
            sensor.name_orig,
        );
        pack_name(
            &mut buf,
            base + SENSOR_OFF_NAME_USER,
            NAME_FIELD_LEN,
            sensor.name_user,
        );
    }

    for (i, entry) in entries.iter().enumerate() {
        let base = entry_off + i * MIN_ENTRY_SIZE;
        put_u32(&mut buf, base, entry.type_code);
        put_u32(&mut buf, base + 4, entry.sensor_idx);
        put_u32(&mut buf, base + 8, i as u32); // id
        pack_name(
            &mut buf,
            base + ENTRY_OFF_NAME_ORIG,
            NAME_FIELD_LEN,
            entry.reading_orig,
        );
        pack_name(
            &mut buf,
            base + ENTRY_OFF_NAME_USER,
            NAME_FIELD_LEN,
            entry.reading_user,
        );
        pack_name(&mut buf, base + ENTRY_OFF_UNIT, UNIT_FIELD_LEN, entry.unit);
        put_f64(&mut buf, base + ENTRY_OFF_VALUES, entry.value);
        put_f64(&mut buf, base + ENTRY_OFF_VALUES + 8, entry.minimum);
        put_f64(&mut buf, base + ENTRY_OFF_VALUES + 16, entry.maximum);
        put_f64(&mut buf, base + ENTRY_OFF_VALUES + 24, entry.average);
    }

    buf
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f64(buf: &mut [u8], off: usize, v: f64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Copy a string into a fixed-width, zero-padded field, truncating to leave at
/// least one terminating NUL. `None` leaves the field all zeroes (empty string).
fn pack_name(buf: &mut [u8], off: usize, field_len: usize, s: Option<&str>) {
    if let Some(s) = s {
        let bytes = s.as_bytes();
        let n = bytes.len().min(field_len - 1);
        buf[off..off + n].copy_from_slice(&bytes[..n]);
    }
}

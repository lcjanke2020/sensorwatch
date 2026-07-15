//! Integration tests for the safe `sensorwatch` binding.
//!
//! The error-translation, ReadingType-folding, ABI-version, and synthetic-snapshot
//! checks run on every platform (`Snapshot::from_buffer` needs no session, so the
//! populated accessor surface is proven cross-platform). The live-snapshot path is
//! Windows-only and self-skips when no sensor source is running; on non-Windows
//! the platform test asserts `Session::new()` reports `UnsupportedPlatform`.

use sensorwatch::{
    abi_version, check_abi_compatibility, sys, Error, Reading, ReadingType, Session, Snapshot,
};

mod common;
use common::{build_buffer, Entry, Sensor};

// Thread-safety markers, asserted at compile time. The owned value types are
// Send + Sync; an immutable Snapshot is Send + Sync (the ABI documents its queries
// as safe to call concurrently on a live snapshot); a Session is Send but not Sync
// (the ABI requires synchronizing concurrent use of one session).
fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn thread_safety_markers() {
    assert_send_sync::<Reading>();
    assert_send_sync::<Error>();
    assert_send_sync::<ReadingType>();
    assert_send_sync::<Snapshot>();
    assert_send::<Session>();
}

#[test]
fn reading_equality_is_nan_reflexive() {
    // The C core copies raw doubles, so a Reading may carry NaN. Equality must stay
    // reflexive (mirrors the C++ binding); a derived PartialEq would make NaN != NaN
    // and could make the live-snapshot assert_eq! checks flaky.
    let a = Reading {
        source: "HWiNFO".to_string(),
        sensor: "S".to_string(),
        reading: "R".to_string(),
        unit: String::new(),
        kind: ReadingType::Temperature,
        value: f64::NAN,
        minimum: f64::NAN,
        maximum: 0.0,
        average: 1.0,
    };
    let b = a.clone();
    assert_eq!(a, b); // a NaN-carrying Reading equals its clone...
    assert_eq!(a, a.clone());

    let mut c = a.clone();
    c.value = 2.0; // ...but a finite value differs from a NaN one
    assert_ne!(a, c);
}

#[test]
fn error_round_trips_through_code() {
    // Every named ABI code maps to a variant and back to the same code.
    let codes = [
        sys::SW_ERR_NULL_POINTER,
        sys::SW_ERR_INVALID_ARGUMENT,
        sys::SW_ERR_UNSUPPORTED_PLATFORM,
        sys::SW_ERR_SOURCE_UNAVAILABLE,
        sys::SW_ERR_MAP_FAILED,
        sys::SW_ERR_BAD_MAGIC,
        sys::SW_ERR_CORRUPT_DATA,
        sys::SW_ERR_OUT_OF_MEMORY,
        sys::SW_ERR_INDEX_OUT_OF_RANGE,
        sys::SW_ERR_BUFFER_TOO_SMALL,
        sys::SW_ERR_VERSION_MISMATCH,
        sys::SW_ERR_INTERNAL,
    ];
    for code in codes {
        let err = Error::from_code(code).expect("non-OK code must map to Some(Error)");
        assert_eq!(err.code(), code, "code round-trip mismatch");
        // Display is never empty and never panics.
        assert!(!err.to_string().is_empty());
    }

    // SW_OK is not an error.
    assert_eq!(Error::from_code(sys::SW_OK), None);

    // A specific mapping the wrapper documents.
    assert_eq!(
        Error::from_code(sys::SW_ERR_SOURCE_UNAVAILABLE),
        Some(Error::SourceUnavailable)
    );
}

#[test]
fn unknown_error_code_is_carried_verbatim() {
    // A code with no named variant is preserved in Other(..) rather than lost.
    let unknown: sys::sw_error_t = -9999;
    assert_eq!(Error::from_code(unknown), Some(Error::Other(unknown)));
    assert_eq!(Error::Other(unknown).code(), unknown);
}

#[test]
fn reading_type_folds_unknown() {
    assert_eq!(
        ReadingType::from_raw(sys::SW_READING_TYPE_NONE),
        ReadingType::None
    );
    assert_eq!(
        ReadingType::from_raw(sys::SW_READING_TYPE_TEMPERATURE),
        ReadingType::Temperature
    );
    assert_eq!(
        ReadingType::from_raw(sys::SW_READING_TYPE_OTHER),
        ReadingType::Other
    );
    assert_eq!(
        ReadingType::from_raw(sys::SW_READING_TYPE_UNKNOWN),
        ReadingType::Unknown
    );
    // Codes this binding does not name fold to Unknown, never an out-of-enum value.
    assert_eq!(ReadingType::from_raw(9), ReadingType::Unknown);
    assert_eq!(ReadingType::from_raw(200), ReadingType::Unknown);
}

#[test]
fn abi_version_matches_header() {
    // The statically linked core reports the version this crate was built against.
    let runtime = abi_version();
    assert_eq!(runtime / 10_000, sys::SW_API_VERSION_MAJOR);
    assert_eq!((runtime / 100) % 100, sys::SW_API_VERSION_MINOR);
    // The compatibility guard therefore passes against the linked core.
    assert!(check_abi_compatibility().is_ok());
}

// --- Synthetic-snapshot coverage (cross-platform; the port of the live-test body,
// with deterministic expected values a live source can never promise) ---

#[test]
fn synthetic_snapshot_accessor_surface() {
    let buf = build_buffer(
        &[Sensor::named("MEG Ai1600T")],
        &[
            Entry::flat(2, 0, "+12V", "V", 12.03),
            Entry {
                type_code: 1,
                sensor_idx: 0,
                reading_user: Some("CPU Package"),
                reading_orig: None,
                unit: Some("C"),
                value: 41.5,
                minimum: 39.0,
                maximum: 88.0,
                average: 52.25,
            },
        ],
    );
    let snapshot = Snapshot::from_buffer(&buf).expect("parse synthetic buffer");
    // The snapshot deep-copies: the input bytes can go away immediately.
    drop(buf);

    assert_eq!(snapshot.len(), 2);
    assert!(!snapshot.is_empty());
    assert_eq!(snapshot.source(), "HWiNFO");

    let first = snapshot.get(0).expect("first reading");
    assert_eq!(first.source, "HWiNFO");
    assert_eq!(first.sensor, "MEG Ai1600T");
    assert_eq!(first.reading, "+12V");
    assert_eq!(first.unit, "V");
    assert_eq!(first.kind, ReadingType::Voltage);
    assert_eq!(first.value, 12.03);
    assert_eq!(first.minimum, 12.03);
    assert_eq!(first.maximum, 12.03);
    assert_eq!(first.average, 12.03);
    // get is deterministic for the same immutable snapshot.
    assert_eq!(first, snapshot.get(0).expect("first reading again"));

    // The four statistics are read from their own fields, not copies of value.
    let second = snapshot.get(1).expect("second reading");
    assert_eq!(second.sensor, "MEG Ai1600T");
    assert_eq!(second.reading, "CPU Package");
    assert_eq!(second.kind, ReadingType::Temperature);
    assert_eq!(second.value, 41.5);
    assert_eq!(second.minimum, 39.0);
    assert_eq!(second.maximum, 88.0);
    assert_eq!(second.average, 52.25);

    // Iteration visits exactly len() readings, all sharing the snapshot source.
    let mut iterated = 0usize;
    for reading in &snapshot {
        let reading = reading.expect("reading during iteration");
        assert_eq!(reading.source, snapshot.source());
        iterated += 1;
    }
    assert_eq!(iterated, snapshot.len());

    // to_vec() materializes the same set.
    let all = snapshot.to_vec().expect("materialize all readings");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], first);
    assert_eq!(all[1], second);

    // Past-the-end index is a clean IndexOutOfRange, not a panic or UB.
    assert_eq!(snapshot.get(snapshot.len()), Err(Error::IndexOutOfRange));

    // Readings own their strings, so they remain valid after the snapshot is dropped.
    drop(snapshot);
    assert_eq!(all[0].sensor, "MEG Ai1600T");
}

#[test]
fn synthetic_snapshot_empty_buffer_shape() {
    // A valid image with zero sensors and zero entries parses to an empty
    // snapshot with the documented zero-entry properties.
    let buf = build_buffer(&[], &[]);
    let snapshot = Snapshot::from_buffer(&buf).expect("parse empty synthetic buffer");
    assert_eq!(snapshot.len(), 0);
    assert!(snapshot.is_empty());
    assert_eq!(snapshot.source(), ""); // source is index-gated; empty when no entries
    assert_eq!(snapshot.get(0), Err(Error::IndexOutOfRange));
    assert_eq!(snapshot.to_vec().expect("empty to_vec"), Vec::new());
}

#[test]
fn from_buffer_rejects_invalid_input() {
    // Empty slice: length-checked before any read.
    let err = Snapshot::from_buffer(&[]).expect_err("empty must fail");
    assert_eq!(err, Error::CorruptData);

    // Shorter than a header.
    let err = Snapshot::from_buffer(&[0u8; 47]).expect_err("truncated must fail");
    assert_eq!(err, Error::CorruptData);

    // Header-sized but the magic is wrong.
    let err = Snapshot::from_buffer(&[0u8; 48]).expect_err("bad magic must fail");
    assert_eq!(err, Error::BadMagic);

    // A valid buffer with its magic corrupted after the fact.
    let mut buf = build_buffer(&[Sensor::named("S")], &[Entry::flat(2, 0, "R", "V", 1.0)]);
    buf[0] ^= 0xFF;
    let err = Snapshot::from_buffer(&buf).expect_err("corrupted magic must fail");
    assert_eq!(err, Error::BadMagic);
}

#[cfg(not(windows))]
#[test]
fn session_unsupported_off_windows() {
    // On non-Windows the source is unavailable, but the crate still links and the
    // error surfaces as UnsupportedPlatform (not a link failure or panic).
    match Session::new() {
        Err(Error::UnsupportedPlatform) => {}
        Err(other) => panic!("expected UnsupportedPlatform, got {other:?}"),
        Ok(_) => panic!("Session::new() must fail on a non-Windows platform"),
    }
}

#[cfg(windows)]
#[test]
fn live_snapshot_shape() {
    // The only legitimate skip is a genuinely absent source, reported at session
    // open. Once the session opens, every later error is a real failure.
    let mut session = match Session::new() {
        Ok(session) => session,
        Err(Error::SourceUnavailable) => {
            eprintln!("[test] SKIP live snapshot (HWiNFO not running)");
            return;
        }
        Err(other) => panic!("unexpected error opening session: {other} ({other:?})"),
    };

    let snapshot = session.snapshot().expect("snapshot from an open session");
    eprintln!(
        "[test] live snapshot: {} readings from {:?}",
        snapshot.len(),
        snapshot.source()
    );

    if snapshot.is_empty() {
        eprintln!("[test] zero readings; skipping per-entry checks");
        return;
    }

    // source is snapshot-wide and non-empty; get(0) agrees with the shared source.
    assert!(!snapshot.source().is_empty());
    let first = snapshot.get(0).expect("first reading");
    assert_eq!(first.source, snapshot.source());
    // get is deterministic for the same immutable snapshot.
    assert_eq!(first, snapshot.get(0).expect("first reading again"));

    // Iteration visits exactly len() readings.
    let mut iterated = 0usize;
    for reading in &snapshot {
        let reading = reading.expect("reading during iteration");
        assert_eq!(reading.source, snapshot.source());
        iterated += 1;
    }
    assert_eq!(iterated, snapshot.len());

    // to_vec() materializes the same set, and the owned Readings outlive the snapshot.
    let all = snapshot.to_vec().expect("materialize all readings");
    assert_eq!(all.len(), snapshot.len());
    assert_eq!(all[0], first);

    // Past-the-end index is a clean IndexOutOfRange, not a panic or UB.
    assert_eq!(snapshot.get(snapshot.len()), Err(Error::IndexOutOfRange));

    // Readings own their strings, so they remain valid after the snapshot is dropped.
    drop(snapshot);
    assert!(!all[0].source.is_empty());
}

//! Integration tests for the safe `sensorwatch` binding.
//!
//! The error-translation, ReadingType-folding, and ABI-version checks run on every
//! platform (they touch only the pure translation layer and the always-available
//! `sw_api_version` / `sw_error_string`). The live-snapshot path is Windows-only
//! and self-skips when no sensor source is running; on non-Windows the platform
//! test asserts `Session::new()` reports `UnsupportedPlatform`.

use sensorwatch::{
    abi_version, check_abi_compatibility, sys, Error, Reading, ReadingType, Session, Snapshot,
};

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

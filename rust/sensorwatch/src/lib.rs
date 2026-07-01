//! Safe, idiomatic Rust bindings over the sensorwatch native C ABI.
//!
//! This crate is a thin RAII layer over [`sensorwatch_sys`] (the raw `-sys` FFI),
//! mirroring the shipped Python (`sensorwatch.native`) and header-only C++
//! (`sensorwatch.hpp`) bindings. The C core is compiled straight into the `-sys`
//! crate, so there is no separate DLL to locate or load.
//!
//! - [`Session`] and [`Snapshot`] are move-only handles closed/freed by [`Drop`].
//!   Rust's ownership makes the "closed exactly once, never double-freed" property
//!   automatic — there is no moved-from state to guard.
//! - A [`Snapshot`] yields [`Reading`] values (`source`, `sensor`, `reading`,
//!   `unit`, `kind`, `value`, `minimum`, `maximum`, `average`) via [`Snapshot::get`],
//!   iteration (`for r in &snapshot { let r = r?; .. }`), and [`Snapshot::to_vec`].
//! - Every non-`SW_OK` result becomes an [`Error`] carrying the `sw_error_t`
//!   [`code`](Error::code) and the library's `sw_error_string()` text.
//!
//! # Platform support
//!
//! The sensor source (HWiNFO shared memory) is Windows-only. The crate still
//! *builds and links* everywhere; on non-Windows [`Session::new`] returns
//! [`Error::UnsupportedPlatform`] rather than failing to link or panicking.
//!
//! # Example
//!
//! ```no_run
//! use sensorwatch::Session;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut session = Session::new()?; // Err off Windows, or if HWiNFO is down
//! let snapshot = session.snapshot()?; // an immutable view of all readings
//! println!("{} readings from {}", snapshot.len(), snapshot.source());
//! for reading in &snapshot {
//!     let r = reading?;
//!     println!("{} / {} = {} {} [{:?}]", r.sensor, r.reading, r.value, r.unit, r.kind);
//! }
//! # Ok(())
//! # }
//! ```

#![warn(rust_2018_idioms)]

use std::ffi::{c_char, CStr};
use std::fmt;

/// Re-export of the raw FFI crate, for coverage this safe wrapper does not expose.
pub use sensorwatch_sys as sys;

/// A specialized [`Result`](std::result::Result) for sensorwatch operations.
pub type Result<T> = std::result::Result<T, Error>;

// Defensive upper bound on a single ABI string (matches the Python/C++ bindings'
// 64 KiB cap). ABI strings are bounded, sanitized UTF-8, so a larger required
// length signals a fault rather than a value worth allocating for.
const MAX_STRING_BYTES: usize = 64 * 1024;

/// A source-neutral reading category, mirroring `sw_reading_type_t`.
///
/// [`Unknown`](ReadingType::Unknown) is the ABI's sentinel for a source category
/// this binding version does not recognize; [`from_raw`](ReadingType::from_raw)
/// also folds any value the running ABI might add ahead of this binding into it,
/// so a raw code never escapes as an out-of-enum value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReadingType {
    /// The source's explicit "None" category (HWiNFO type 0).
    None,
    /// Temperature.
    Temperature,
    /// Voltage.
    Voltage,
    /// Fan speed.
    Fan,
    /// Electric current.
    Current,
    /// Power.
    Power,
    /// Clock frequency.
    Clock,
    /// Usage / load.
    Usage,
    /// Any other categorized reading.
    Other,
    /// A source category outside the range this binding version recognizes.
    Unknown,
}

impl ReadingType {
    /// Map a raw `sw_reading_type_t` onto a [`ReadingType`], folding any
    /// unrecognized value to [`Unknown`](ReadingType::Unknown).
    #[must_use]
    pub fn from_raw(raw: sys::sw_reading_type_t) -> ReadingType {
        match raw {
            sys::SW_READING_TYPE_NONE => ReadingType::None,
            sys::SW_READING_TYPE_TEMPERATURE => ReadingType::Temperature,
            sys::SW_READING_TYPE_VOLTAGE => ReadingType::Voltage,
            sys::SW_READING_TYPE_FAN => ReadingType::Fan,
            sys::SW_READING_TYPE_CURRENT => ReadingType::Current,
            sys::SW_READING_TYPE_POWER => ReadingType::Power,
            sys::SW_READING_TYPE_CLOCK => ReadingType::Clock,
            sys::SW_READING_TYPE_USAGE => ReadingType::Usage,
            sys::SW_READING_TYPE_OTHER => ReadingType::Other,
            _ => ReadingType::Unknown,
        }
    }
}

impl From<sys::sw_reading_type_t> for ReadingType {
    fn from(raw: sys::sw_reading_type_t) -> ReadingType {
        ReadingType::from_raw(raw)
    }
}

/// A native `sw_error_t` surfaced as a Rust error.
///
/// Named variants cover the codes the ABI defines today; a code this binding
/// version does not name is carried in [`Other`](Error::Other). The numeric code
/// is always available via [`code`](Error::code), and [`Display`](fmt::Display)
/// uses the library's own `sw_error_string()` text (with a friendlier message for
/// the two cases a caller most often hits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A required pointer argument was null (`SW_ERR_NULL_POINTER`).
    NullPointer,
    /// A non-null argument was invalid (`SW_ERR_INVALID_ARGUMENT`).
    InvalidArgument,
    /// The backend is unavailable on this platform (`SW_ERR_UNSUPPORTED_PLATFORM`).
    UnsupportedPlatform,
    /// The sensor source is not running or not enabled (`SW_ERR_SOURCE_UNAVAILABLE`).
    SourceUnavailable,
    /// The source was found but could not be mapped/read (`SW_ERR_MAP_FAILED`).
    MapFailed,
    /// The shared-memory magic/version marker did not match (`SW_ERR_BAD_MAGIC`).
    BadMagic,
    /// Source data failed structural validation (`SW_ERR_CORRUPT_DATA`).
    CorruptData,
    /// An allocation failed (`SW_ERR_OUT_OF_MEMORY`).
    OutOfMemory,
    /// A snapshot index was outside the entry count (`SW_ERR_INDEX_OUT_OF_RANGE`).
    IndexOutOfRange,
    /// A caller buffer was too small (`SW_ERR_BUFFER_TOO_SMALL`).
    BufferTooSmall,
    /// Caller/library ABI expectations are incompatible (`SW_ERR_VERSION_MISMATCH`).
    VersionMismatch,
    /// An unexpected library bug or invariant failure (`SW_ERR_INTERNAL`).
    Internal,
    /// An `sw_error_t` code this binding version has no named variant for.
    Other(sys::sw_error_t),
}

impl Error {
    /// Build an [`Error`] from a raw `sw_error_t`, returning `None` for `SW_OK`.
    #[must_use]
    pub fn from_code(code: sys::sw_error_t) -> Option<Error> {
        Some(match code {
            sys::SW_OK => return None,
            sys::SW_ERR_NULL_POINTER => Error::NullPointer,
            sys::SW_ERR_INVALID_ARGUMENT => Error::InvalidArgument,
            sys::SW_ERR_UNSUPPORTED_PLATFORM => Error::UnsupportedPlatform,
            sys::SW_ERR_SOURCE_UNAVAILABLE => Error::SourceUnavailable,
            sys::SW_ERR_MAP_FAILED => Error::MapFailed,
            sys::SW_ERR_BAD_MAGIC => Error::BadMagic,
            sys::SW_ERR_CORRUPT_DATA => Error::CorruptData,
            sys::SW_ERR_OUT_OF_MEMORY => Error::OutOfMemory,
            sys::SW_ERR_INDEX_OUT_OF_RANGE => Error::IndexOutOfRange,
            sys::SW_ERR_BUFFER_TOO_SMALL => Error::BufferTooSmall,
            sys::SW_ERR_VERSION_MISMATCH => Error::VersionMismatch,
            sys::SW_ERR_INTERNAL => Error::Internal,
            other => Error::Other(other),
        })
    }

    /// The raw `sw_error_t` value behind this error.
    #[must_use]
    pub fn code(self) -> sys::sw_error_t {
        match self {
            Error::NullPointer => sys::SW_ERR_NULL_POINTER,
            Error::InvalidArgument => sys::SW_ERR_INVALID_ARGUMENT,
            Error::UnsupportedPlatform => sys::SW_ERR_UNSUPPORTED_PLATFORM,
            Error::SourceUnavailable => sys::SW_ERR_SOURCE_UNAVAILABLE,
            Error::MapFailed => sys::SW_ERR_MAP_FAILED,
            Error::BadMagic => sys::SW_ERR_BAD_MAGIC,
            Error::CorruptData => sys::SW_ERR_CORRUPT_DATA,
            Error::OutOfMemory => sys::SW_ERR_OUT_OF_MEMORY,
            Error::IndexOutOfRange => sys::SW_ERR_INDEX_OUT_OF_RANGE,
            Error::BufferTooSmall => sys::SW_ERR_BUFFER_TOO_SMALL,
            Error::VersionMismatch => sys::SW_ERR_VERSION_MISMATCH,
            Error::Internal => sys::SW_ERR_INTERNAL,
            Error::Other(code) => code,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::SourceUnavailable => f.write_str(
                "sensor source unavailable — is HWiNFO running with shared memory enabled?",
            ),
            Error::UnsupportedPlatform => f.write_str(
                "sensorwatch is only supported on Windows (the HWiNFO shared-memory source is unavailable here)",
            ),
            other => f.write_str(error_string(other.code())),
        }
    }
}

impl std::error::Error for Error {}

/// The library-owned, process-lifetime message for an `sw_error_t`.
fn error_string(code: sys::sw_error_t) -> &'static str {
    // SAFETY: sw_error_string returns a static, never-null, NUL-terminated C string
    // that stays valid for the life of the process (ABI contract). The messages are
    // ASCII, so to_str() succeeds; the fallback only guards against a future change.
    let ptr = unsafe { sys::sw_error_string(code) };
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .unwrap_or("sensorwatch error")
}

/// Turn a raw `sw_error_t` into `Ok(())` for `SW_OK`, or `Err(Error)` otherwise.
fn check(code: sys::sw_error_t) -> Result<()> {
    match Error::from_code(code) {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

/// The ABI version of the linked C core, encoded as `MAJOR*10000 + MINOR*100 + PATCH`.
#[must_use]
pub fn abi_version() -> u32 {
    // SAFETY: sw_api_version has no preconditions and is thread-safe.
    unsafe { sys::sw_api_version() }
}

/// Verify the linked core's ABI is compatible with the version this crate was built
/// against, returning [`Error::VersionMismatch`] otherwise.
///
/// Pre-1.0 a minor bump is breaking, so `major.minor` must match; from 1.0 on only
/// the major gates compatibility (mirrors the Python/C++ bindings' load-time guard).
/// [`Session::new`] calls this before opening; a static link is already pinned at
/// link time, so it matters most to a future DLL-backed build.
pub fn check_abi_compatibility() -> Result<()> {
    let runtime = abi_version();
    let major = runtime / 10_000;
    let minor = (runtime / 100) % 100;
    let compatible =
        major == sys::SW_API_VERSION_MAJOR && (major >= 1 || minor == sys::SW_API_VERSION_MINOR);
    if compatible {
        Ok(())
    } else {
        Err(Error::VersionMismatch)
    }
}

/// One reading entry: a snapshot-independent value copy.
///
/// The strings are owned, so a `Reading` outlives the [`Snapshot`] it came from.
/// Fields and order mirror the Python/C++ bindings' reading shape (the ABI's `type`
/// is spelled `kind` here, since `type` is a Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The source/backend identity shared by every reading (e.g. `"HWiNFO"`).
    pub source: String,
    /// The sensor display name.
    pub sensor: String,
    /// The reading display name.
    pub reading: String,
    /// The unit string (may be empty).
    pub unit: String,
    /// The source-neutral reading category (the ABI's `type`).
    pub kind: ReadingType,
    /// The current value.
    pub value: f64,
    /// The minimum value observed by the source.
    pub minimum: f64,
    /// The maximum value observed by the source.
    pub maximum: f64,
    /// The average value observed by the source.
    pub average: f64,
}

// Pointers to the scalar / string snapshot accessors, so build_reading can share
// one code path across the four numeric aggregates and the string fields.
type ScalarAccessor =
    unsafe extern "C" fn(*const sys::sw_snapshot_t, u32, *mut f64) -> sys::sw_error_t;
type StringAccessor = unsafe extern "C" fn(
    *const sys::sw_snapshot_t,
    u32,
    *mut c_char,
    usize,
    *mut usize,
) -> sys::sw_error_t;

/// Read one string field via the ABI's length-query-then-copy contract.
fn query_string(
    accessor: StringAccessor,
    snapshot: *const sys::sw_snapshot_t,
    index: u32,
) -> Result<String> {
    // Length query: (NULL, 0, &needed) reports the byte count incl. the NUL (>= 1)
    // and returns SW_ERR_BUFFER_TOO_SMALL. Any other code is a real error (NULL
    // snapshot, index out of range, ...); a length query never returns SW_OK.
    let mut needed: usize = 0;
    let code = unsafe { accessor(snapshot, index, std::ptr::null_mut(), 0, &mut needed) };
    if code != sys::SW_ERR_BUFFER_TOO_SMALL {
        check(code)?;
        return Err(Error::Internal); // unreachable: SW_OK from a length query breaks the contract
    }
    if needed > MAX_STRING_BYTES {
        return Err(Error::CorruptData);
    }
    let mut buffer = vec![0u8; needed];
    check(unsafe {
        accessor(
            snapshot,
            index,
            buffer.as_mut_ptr().cast::<c_char>(),
            buffer.len(),
            &mut needed,
        )
    })?;
    // `needed` now counts bytes written including the trailing NUL (>= 1); drop it.
    buffer.truncate(needed.saturating_sub(1));
    String::from_utf8(buffer).map_err(|_| Error::CorruptData)
}

/// An immutable snapshot of all readings, owning its own data copy.
///
/// Move-only (no `Clone`), freed by [`Drop`]. All accessors take `&self`; the type
/// is not `Sync`, so the ABI's "queries are safe on a live snapshot" holds without
/// exposing it to unsynchronized cross-thread use. Extract [`Reading`]s (which are
/// owned and `Send`) to move data across threads.
pub struct Snapshot {
    ptr: *mut sys::sw_snapshot_t,
    len: u32,
    source: String,
}

impl Snapshot {
    /// Adopt a snapshot returned by `sw_snapshot_take`.
    ///
    /// # Safety
    /// `ptr` must be a non-null snapshot from `sw_snapshot_take` whose ownership is
    /// transferred to the returned `Snapshot` (freed once on drop).
    unsafe fn from_raw(ptr: *mut sys::sw_snapshot_t) -> Result<Snapshot> {
        let mut count: u32 = 0;
        if let Some(err) = Error::from_code(sys::sw_snapshot_entry_count(ptr, &mut count)) {
            sys::sw_snapshot_free(ptr); // do not leak on a failed construction
            return Err(err);
        }
        // The source name is snapshot-wide; query it once, eagerly. Empty for a
        // zero-entry snapshot, where the index-gated source accessor has no entry
        // to read.
        let source = if count > 0 {
            match query_string(sys::sw_snapshot_get_source_name, ptr, 0) {
                Ok(source) => source,
                Err(err) => {
                    sys::sw_snapshot_free(ptr); // still no leak if the source query fails
                    return Err(err);
                }
            }
        } else {
            String::new()
        };
        Ok(Snapshot {
            ptr,
            len: count,
            source,
        })
    }

    /// The number of reading entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the snapshot has no readings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The source/backend identity (e.g. `"HWiNFO"`), shared by every reading.
    /// Empty for a zero-entry snapshot.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Build the [`Reading`] at `index`, or [`Error::IndexOutOfRange`] if out of bounds.
    pub fn get(&self, index: usize) -> Result<Reading> {
        let index = u32::try_from(index).map_err(|_| Error::IndexOutOfRange)?;
        self.build_reading(index)
    }

    /// Iterate the readings, each yielded as a `Result<Reading>`.
    #[must_use]
    pub fn iter(&self) -> Readings<'_> {
        Readings {
            snapshot: self,
            index: 0,
        }
    }

    /// Materialize all readings at once, short-circuiting on the first error.
    pub fn to_vec(&self) -> Result<Vec<Reading>> {
        self.iter().collect()
    }

    fn scalar(&self, index: u32, accessor: ScalarAccessor) -> Result<f64> {
        let mut out = 0.0_f64;
        // SAFETY: live snapshot ptr; the accessor validates `index` and writes `out`.
        check(unsafe { accessor(self.ptr, index, &mut out) })?;
        Ok(out)
    }

    fn build_reading(&self, index: u32) -> Result<Reading> {
        // Scalars first: they validate `index` without allocating, so an
        // out-of-range index errors (SW_ERR_INDEX_OUT_OF_RANGE) before any string
        // buffer is allocated.
        let mut raw_type: sys::sw_reading_type_t = sys::SW_READING_TYPE_UNKNOWN;
        // SAFETY: live snapshot ptr; accessor validates `index` and writes the type.
        check(unsafe { sys::sw_snapshot_get_reading_type(self.ptr, index, &mut raw_type) })?;
        let kind = ReadingType::from_raw(raw_type);
        let value = self.scalar(index, sys::sw_snapshot_get_value)?;
        let minimum = self.scalar(index, sys::sw_snapshot_get_minimum)?;
        let maximum = self.scalar(index, sys::sw_snapshot_get_maximum)?;
        let average = self.scalar(index, sys::sw_snapshot_get_average)?;
        let sensor = query_string(sys::sw_snapshot_get_sensor_name, self.ptr, index)?;
        let reading = query_string(sys::sw_snapshot_get_reading_name, self.ptr, index)?;
        let unit = query_string(sys::sw_snapshot_get_unit, self.ptr, index)?;
        Ok(Reading {
            source: self.source.clone(),
            sensor,
            reading,
            unit,
            kind,
            value,
            minimum,
            maximum,
            average,
        })
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: ptr came from sw_snapshot_take; Rust's move semantics guarantee a
        // single owner, so it is freed exactly once. sw_snapshot_free(NULL) is a
        // no-op regardless.
        unsafe { sys::sw_snapshot_free(self.ptr) }
    }
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("len", &self.len)
            .field("source", &self.source)
            .finish()
    }
}

/// Iterator over a [`Snapshot`]'s readings, yielding each as a `Result<Reading>`.
///
/// Building a reading can fail (e.g. an allocation failure), so items are
/// `Result<Reading>`. In-range indices on a live snapshot succeed in practice; the
/// `Result` keeps the rare failure honest instead of panicking.
pub struct Readings<'a> {
    snapshot: &'a Snapshot,
    index: u32,
}

impl Iterator for Readings<'_> {
    type Item = Result<Reading>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.snapshot.len {
            return None;
        }
        let index = self.index;
        self.index += 1;
        Some(self.snapshot.build_reading(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.snapshot.len - self.index) as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Readings<'_> {}

impl<'a> IntoIterator for &'a Snapshot {
    type Item = Result<Reading>;
    type IntoIter = Readings<'a>;

    fn into_iter(self) -> Readings<'a> {
        self.iter()
    }
}

/// An open sensorwatch session for the default sensor source.
///
/// Move-only, closed by [`Drop`]. [`new`](Session::new) verifies the ABI and opens
/// the source, returning [`Error::UnsupportedPlatform`] off Windows or
/// [`Error::SourceUnavailable`] when the source is not running.
pub struct Session {
    ptr: *mut sys::sw_session_t,
}

impl Session {
    /// Open a session for the default sensor source.
    ///
    /// Errors with [`Error::VersionMismatch`] if the linked core's ABI is
    /// incompatible, [`Error::UnsupportedPlatform`] on non-Windows, or
    /// [`Error::SourceUnavailable`] if the source (HWiNFO) is not running.
    pub fn new() -> Result<Session> {
        // Verify the linked core's ABI before opening (mirrors the Python/C++
        // bindings). For a static link this is already pinned; it future-proofs a
        // DLL-backed build whose loaded core could drift from this crate.
        check_abi_compatibility()?;
        let mut ptr: *mut sys::sw_session_t = std::ptr::null_mut();
        // SAFETY: out-param; on SW_OK the C writes a non-null owned session,
        // otherwise NULL. We take ownership of the session on success.
        check(unsafe { sys::sw_session_open(&mut ptr) })?;
        Ok(Session { ptr })
    }

    /// Take an immutable snapshot of all currently available readings.
    ///
    /// Takes `&mut self`: the ABI requires callers to synchronize concurrent use of
    /// one session, and exclusive access is Rust's encoding of that requirement.
    pub fn snapshot(&mut self) -> Result<Snapshot> {
        let mut ptr: *mut sys::sw_snapshot_t = std::ptr::null_mut();
        // SAFETY: valid session ptr; out-param receives a non-null owned snapshot on SW_OK.
        check(unsafe { sys::sw_snapshot_take(self.ptr, &mut ptr) })?;
        // SAFETY: on SW_OK, ptr is a non-null snapshot whose ownership we now hold.
        unsafe { Snapshot::from_raw(ptr) }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: ptr came from sw_session_open; Rust's move semantics guarantee a
        // single owner, so it is closed exactly once. sw_session_close(NULL) is a
        // no-op regardless.
        unsafe { sys::sw_session_close(self.ptr) }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

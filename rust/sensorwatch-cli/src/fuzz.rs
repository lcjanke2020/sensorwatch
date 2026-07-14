//! Narrow entry points for the cargo-fuzz targets in `fuzz/`.
//!
//! Each wraps a private replay parser and discards the result: the fuzzer hunts
//! for panics, out-of-bounds, unbounded work, and timeouts, not return values.
//! Keeping the surface here lets the parsers stay `pub(crate)` instead of fully
//! public. Both take arbitrary bytes — the replay path must survive invalid
//! UTF-8 and any byte sequence a log file can contain.

/// Fuzz the full JSONL record parser: strict `serde_json` → Python-token fixup
/// fallback → timestamp parse → type-label normalization. This is the untrusted
/// surface behind `watch --replay` and `report`.
pub fn parse_line(data: &[u8]) {
    let _ = crate::replay::parse_line(data);
}

/// Fuzz the Python-token fixup pass in isolation — the byte-wise in-string
/// tracker that rewrites bare `NaN`/`Infinity`/`-Infinity` to `null` outside
/// strings. Its output feeds `serde_json`, so a boundary bug here is exactly the
/// interesting class.
pub fn fixup_python_tokens(data: &[u8]) {
    let _ = crate::replay::fixup_python_tokens(data);
}

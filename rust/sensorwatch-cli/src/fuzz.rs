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
/// tracker that rewrites bare `NaN`/`Infinity`/`-Infinity` to `null` outside JSON
/// strings. Its output feeds `serde_json`, so the interesting bug is string-
/// boundary corruption, which crash-only fuzzing can't see. Two semantic
/// invariants make a boundary regression fail the run:
///
/// 1. **Never touch already-valid JSON.** Bare `NaN`/`Infinity`/`-Infinity` are
///    not legal JSON literals, so they appear as bare tokens only in *invalid*
///    input; inside strings they must be preserved. Hence any input that parses
///    as strict JSON must pass through unchanged (`None`). A fixup that rewrote a
///    token inside a string would make some valid-JSON input return `Some`.
/// 2. **The rewrite is complete (idempotent).** A second pass over the output has
///    nothing left to change.
pub fn fixup_python_tokens(data: &[u8]) {
    let out = crate::replay::fixup_python_tokens(data);

    if serde_json::from_slice::<serde_json::Value>(data).is_ok() {
        assert!(
            out.is_none(),
            "fixup rewrote already-valid JSON (string-boundary corruption)",
        );
    }
    if let Some(rewritten) = out {
        assert!(
            crate::replay::fixup_python_tokens(&rewritten).is_none(),
            "fixup is not idempotent",
        );
    }
}

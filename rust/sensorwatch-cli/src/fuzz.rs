//! Narrow entry points for the cargo-fuzz targets in `fuzz/`.
//!
//! [`parse_line`] discards its result — the fuzzer hunts for panics, out-of-
//! bounds, unbounded work, and timeouts. [`fixup_python_tokens`] additionally
//! asserts semantic invariants (see its docs), so a string-boundary regression
//! fails the run, not just a crash. Keeping the surface here lets the parsers stay
//! `pub(crate)` instead of fully public. Both take arbitrary bytes — the replay
//! path must survive invalid UTF-8 and any byte sequence a log file can contain.

/// Fuzz the full JSONL record parser: strict `serde_json` → Python-token fixup
/// fallback → timestamp parse → type-label normalization. This is the untrusted
/// surface behind `watch --replay` and `report`.
pub fn parse_line(data: &[u8]) {
    let _ = crate::replay::parse_line(data);
}

/// Fuzz the Python-token fixup pass in isolation — the byte-wise in-string
/// tracker that rewrites bare `NaN`/`Infinity`/`-Infinity` to `null` outside JSON
/// strings. Its output feeds `serde_json`, so the interesting bug is string-
/// boundary corruption, which crash-only fuzzing can't see. Three semantic checks
/// make a boundary regression fail the run:
///
/// 1. **Never touch already-valid JSON.** Bare `NaN`/`Infinity`/`-Infinity` are
///    not legal JSON literals, so they appear as bare tokens only in *invalid*
///    input; inside strings they must be preserved. Any input that parses as
///    strict JSON must therefore pass through unchanged (`None`).
/// 2. **The rewrite is complete (idempotent).** A second pass over the output has
///    nothing left to change.
/// 3. **Metamorphic string-preservation.** Invariants 1–2 both miss the production
///    shape where the input *needs* fixup yet also carries in-string tokens (a bare
///    token defeats 1; rewriting the in-string token too still leaves an idempotent
///    result, defeating 2). So embed the arbitrary bytes in a JSON string field,
///    append a separate bare `NaN` to force the rewrite path, and assert the string
///    field decodes back byte-for-byte after the fixup and a re-parse.
pub fn fixup_python_tokens(data: &[u8]) {
    let out = crate::replay::fixup_python_tokens(data);

    // (1) Already-valid JSON is never rewritten.
    if serde_json::from_slice::<serde_json::Value>(data).is_ok() {
        assert!(
            out.is_none(),
            "fixup rewrote already-valid JSON (string-boundary corruption)",
        );
    }
    // (2) The rewrite is idempotent.
    if let Some(rewritten) = &out {
        assert!(
            crate::replay::fixup_python_tokens(rewritten).is_none(),
            "fixup is not idempotent",
        );
    }

    // (3) Metamorphic: a JSON string field must survive the fixup unchanged even
    // when a sibling bare token forces the rewrite path. `serde_json` escapes the
    // (lossily-decoded) bytes into a valid string literal; the trailing bare `NaN`
    // is outside every string, so a correct fixup rewrites only it.
    let text = String::from_utf8_lossy(data);
    let field = serde_json::to_string(text.as_ref()).expect("a string always serializes");
    let framed = format!(r#"{{"s": {field}, "v": NaN}}"#);
    match crate::replay::fixup_python_tokens(framed.as_bytes()) {
        Some(fixed) => {
            let value: serde_json::Value =
                serde_json::from_slice(&fixed).expect("fixed output must be valid JSON");
            assert_eq!(
                value.get("s").and_then(|v| v.as_str()),
                Some(text.as_ref()),
                "fixup corrupted a JSON string field",
            );
        }
        None => panic!("fixup left the bare NaN token in place"),
    }
}

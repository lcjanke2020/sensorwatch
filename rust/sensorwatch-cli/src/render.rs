//! The shared compact/pretty JSON renderer.
//!
//! `snapshot` and the `report` digest both serialize with the same
//! compact-or-`PrettyFormatter::with_indent` pattern. Factoring it here keeps
//! their byte output identical by construction — the guarantee snapshot's frozen
//! byte-lock tests and the digest's exact-bytes tests both depend on — instead
//! of two hand-copied renderers that could drift.

use serde::Serialize;

/// Serialize `value` to a JSON string: compact for `indent == 0`, else pretty
/// with an `indent`-space indentation unit. Byte-for-byte what a bare
/// `serde_json::to_string` (compact) or a `PrettyFormatter::with_indent`
/// serializer (pretty) produces, so callers pinned to frozen output can share
/// it. The caller chooses how to handle the (practically unreachable)
/// serialization error — `snapshot` propagates it with `?`, the digest
/// `.expect()`s it.
pub(crate) fn to_json_string<T: Serialize>(value: &T, indent: u32) -> serde_json::Result<String> {
    if indent == 0 {
        return serde_json::to_string(value);
    }
    let indent_unit = vec![b' '; indent as usize];
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent_unit);
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    value.serialize(&mut serializer)?;
    Ok(String::from_utf8(out).expect("serde_json output is UTF-8"))
}

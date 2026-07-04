//! Shared numeric limits for parsing external input.
//!
//! Single-sourced here so the enforcement site ([`crate::replay`]) and the
//! integration fixture that probes the boundary (`tests/report_cli.rs`, via a
//! `#[path]` include) can never drift apart: if the cap moves, both move.

/// Upper bound for one JSONL line. A full HWiNFO record is ~100 KB, so this
/// is >20k readings of headroom; anything larger is discarded to the next
/// newline WITHOUT being buffered.
pub(crate) const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

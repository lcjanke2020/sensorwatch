#![no_main]
//! Fuzz the full JSONL replay-record parser — the untrusted-input surface behind
//! `watch --replay` and `report`. Arbitrary bytes in; the harness catches any
//! panic, out-of-bounds, unbounded work, or timeout.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sensorwatch_cli::fuzz::parse_line(data);
});

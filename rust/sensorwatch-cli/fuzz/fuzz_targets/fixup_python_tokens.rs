#![no_main]
//! Fuzz the Python-token fixup pass in isolation — the byte-wise in-string
//! tracker that rewrites bare `NaN`/`Infinity`/`-Infinity` to `null` outside JSON
//! strings. Its output feeds `serde_json`, so a string-boundary bug here is the
//! interesting class.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    sensorwatch_cli::fuzz::fixup_python_tokens(data);
});

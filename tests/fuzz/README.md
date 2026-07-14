# Fuzz targets

Fuzz harnesses for sensorwatch's two untrusted-input parsers. Both run any
crash, sanitizer finding, timeout, or unbounded allocation as a bug
(`docs/C_CODING_STANDARDS.md` §"Fuzzing the Parser", `SECURITY.md` §1.3). They
are exercised nightly by `.github/workflows/fuzz.yml`; this note is for running
them by hand.

## C parser — `fuzz_parse` (libFuzzer)

`fuzz_parse.c` feeds arbitrary bytes to `sw_parse_buffer()`, the same pure parser
`sw_snapshot_take()` runs over the live HWiNFO mapping. Built behind the
`SW_BUILD_FUZZ` CMake option, which requires **clang** (libFuzzer) and implies
ASan + UBSan.

```sh
cmake -B build-fuzz -DSW_BUILD_FUZZ=ON -DCMAKE_C_COMPILER=clang -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build-fuzz --target fuzz_parse -j

# libFuzzer writes newly discovered inputs to its FIRST corpus dir, so fuzz into
# a scratch dir and pass tests/fuzz/corpus/parse as read-only seed input -- that
# keeps the committed seeds curated.
mkdir -p /tmp/sw-fuzz-parse
./build-fuzz/fuzz_parse /tmp/sw-fuzz-parse tests/fuzz/corpus/parse                    # until a crash / Ctrl-C
./build-fuzz/fuzz_parse -max_total_time=300 /tmp/sw-fuzz-parse tests/fuzz/corpus/parse  # bounded (CI uses this)
```

### Seed corpus

`corpus/parse/` holds committed seeds generated from the same synthetic-buffer
builder the cmocka parser tests use (`tests/c/sw_test_util.*`), so they mirror the
HWiNFO wire layout: valid single/multi buffers plus the adversarial headers from
`tests/c/test_parse.c` (32-bit `count*size` wrap, an oversized single element,
unterminated name/unit fields, bad magic, empty). Regenerate after changing the
layout or the seed set:

```sh
clang -I src -I include -I tests/c tests/fuzz/gen_corpus.c tests/c/sw_test_util.c -o /tmp/gen_corpus
/tmp/gen_corpus tests/fuzz/corpus/parse
```

## Rust replay parser — `cargo-fuzz`

`parse_line` / `fixup_python_tokens` (`rust/sensorwatch-cli/src/replay.rs`) parse
arbitrary JSONL log files fed to `watch --replay` / `report`. The cargo-fuzz crate
lives at `rust/sensorwatch-cli/fuzz/` and needs a **nightly** toolchain.

```sh
cd rust/sensorwatch-cli
cargo +nightly fuzz run parse_line -- -max_total_time=300
cargo +nightly fuzz run fixup_python_tokens -- -max_total_time=300
```

## Mutation self-test

The harness's value is that it catches a regression in the bounds checks. To
demonstrate: weaken a guard in `src/sw_parse.c` (e.g. delete the
`sensor_end / entry_end > len` check), rebuild `fuzz_parse`, and run it over
`corpus/parse` — ASan reports a heap-buffer-overflow within seconds. Revert the
change afterward.

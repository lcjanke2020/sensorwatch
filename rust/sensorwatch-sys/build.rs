// Build script for sensorwatch-sys.
//
// Compiles the sensorwatch C core (vendored under vendor/) straight into this -sys
// crate as a static archive, with SW_STATIC defined so SW_API is undecorated. This is
// the "static library into the crate" approach: a single self-contained artifact, no
// separate DLL to locate at runtime — which sidesteps the DLL search-order risk in
// SECURITY.md §2.1 entirely, exactly as the Python cffi binding (which likewise
// compiles src/*.c into its extension) and the C++/CMake static path do.
//
// The C is *vendored* (vendor/src, vendor/include) rather than read from the repo's
// canonical ../../src + ../../include, because `cargo publish` only packages files
// beneath the crate directory: a published sensorwatch-sys must carry its own copy of
// the sources to build on a consumer machine. The vendored tree is a verbatim mirror
// of the canonical core, kept in lockstep by the CI `vendor-sync` job
// (git diff --exit-code); building always uses vendor/, in-repo and published alike.
//
// The C is compiled on EVERY platform, not gated behind cfg(windows). The core is
// portable: its session layer returns SW_ERR_UNSUPPORTED_PLATFORM off Windows
// (src/sw_session.c), just as the Python and C++ bindings rely on. Compiling it
// everywhere is what lets the safe `sensorwatch` crate *link* and return
// Error::UnsupportedPlatform on non-Windows, rather than failing to link — the
// behavior the acceptance criteria require.
//
// No bindgen / libclang here: the FFI declarations are checked in (src/bindings.rs)
// and regenerated out-of-band, so this build needs only a C compiler.

use std::path::PathBuf;

// Must stay in sync with SW_SOURCES in the top-level CMakeLists.txt. These five
// translation units are the whole C core; the header-only pieces (sw_internal.h,
// sw_platform.h) are pulled in via the include paths below.
const SOURCES: &[&str] = &[
    "sw_error.c",
    "sw_string.c",
    "sw_parse.c",
    "sw_snapshot.c",
    "sw_session.c",
];

fn main() {
    // The vendored C core lives under <crate>/vendor (see the module comment); this
    // is what makes the published crate self-contained.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let vendor_dir = manifest_dir.join("vendor");
    let src_dir = vendor_dir.join("src");
    let include_dir = vendor_dir.join("include");

    let mut build = cc::Build::new();
    build
        .include(&include_dir) // public header: sensorwatch/sensorwatch.h
        .include(&src_dir) // internal headers: sw_internal.h, sw_platform.h
        // SW_STATIC => SW_API expands to nothing (no dllimport/dllexport); the core
        // is linked directly into this crate's rlib.
        .define("SW_STATIC", None);
    for file in SOURCES {
        build.file(src_dir.join(file));
    }
    // cc matches the Rust target's CRT (e.g. MSVC /MD) automatically, so the C
    // objects link cleanly against the Rust std it is compiled into.
    build.compile("sensorwatch");

    // Rebuild whenever any part of the C core or the public/internal headers change.
    for file in SOURCES {
        println!("cargo:rerun-if-changed={}", src_dir.join(file).display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        include_dir.join("sensorwatch/sensorwatch.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("sw_internal.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        src_dir.join("sw_platform.h").display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}

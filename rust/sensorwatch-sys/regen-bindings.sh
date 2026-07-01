#!/usr/bin/env bash
#
# Regenerate src/bindings.rs from the public C ABI header.
#
# The generated file is CHECKED IN so that building sensorwatch-sys never needs
# libclang. This script is the single source of truth for how it is produced, run
# both locally and by the CI `bindgen-drift` job (which then `git diff --exit-code`s
# the result — so a header change that isn't reflected in the committed bindings, or
# a hand-edit of bindings.rs, fails the build).
#
# Reproducibility depends on pinning BOTH tools; the CI job installs exactly these:
#   * bindgen  0.72.1   (cargo install bindgen-cli --version 0.72.1 --locked)
#   * libclang 18.1.1   (pip/uv install libclang==18.1.1; point LIBCLANG_PATH at the
#                        directory containing libclang.{dll,so,dylib})
#
# --target pins the data model to the Windows/x86_64 ABI the core actually ships
# for, so the output is identical regardless of the host OS running bindgen.
#
# Usage:
#   LIBCLANG_PATH=/path/to/libclang/dir ./regen-bindings.sh
set -euo pipefail

bindgen="${BINDGEN:-bindgen}"

# On Windows/MSYS, keep the shell from rewriting our arguments as paths (a `//`
# or `/`-leading value would otherwise be mangled). A no-op on Linux, so local
# (Git Bash) and CI stay byte-identical. bindings.rs is left as *pure* bindgen
# output — the "do not edit / how to regenerate" banner lives in src/lib.rs — so
# there are no custom --raw-lines to differ across platforms.
export MSYS2_ARG_CONV_EXCL='*'

# Run from the crate dir and use paths relative to it. Relative paths carry no
# drive prefix, so they work whether the (possibly native-Windows) libclang sees
# them as `C:\...` or a shell hands them over as `/c/...`.
cd "$(dirname "${BASH_SOURCE[0]}")"
mkdir -p src

"$bindgen" \
  wrapper.h \
  --output src/bindings.rs \
  --allowlist-item '^(sw_|SW_).*' \
  --default-enum-style consts \
  --no-prepend-enum-name \
  --use-core \
  --merge-extern-blocks \
  --no-layout-tests \
  --no-doc-comments \
  -- \
  -I vendor/include \
  --target=x86_64-pc-windows-msvc

# Line endings: bindgen may emit CRLF on Windows and LF on Linux, but the file is
# marked `text eol=lf` in .gitattributes, so git stores and compares it as LF on
# every platform. That keeps the CI bindgen-drift `git diff` line-ending-stable
# without a fragile in-place normalization step here.

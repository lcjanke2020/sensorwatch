# sensorwatch-sys

Raw FFI declarations for the [sensorwatch](https://github.com/lcjanke2020/sensorwatch)
native C ABI (`sw_*` / `SW_*`, see `include/sensorwatch/sensorwatch.h`).

**Most users want the safe wrapper, [`sensorwatch`](https://crates.io/crates/sensorwatch),
not this crate directly.**

- The C core is **vendored** in this crate (`vendor/`) and compiled straight in by
  `build.rs` (with `SW_STATIC`), so there is no separate DLL to locate at runtime and
  no external native library to install. Building needs only a C compiler.
- The FFI declarations are pre-generated with `bindgen` and **checked in**
  (`src/bindings.rs`), so building never requires libclang. Regeneration is an
  out-of-band maintainer step (`regen-bindings.sh`), guarded by a CI drift check.
- `links = "sensorwatch"` ensures at most one copy of the C core is linked into any
  final artifact.

## Platform support

The core compiles on every platform; its session layer returns
`SW_ERR_UNSUPPORTED_PLATFORM` off Windows. Reading live sensors currently requires
Windows with HWiNFO's shared-memory support enabled.

## License

MIT — see [LICENSE](LICENSE).

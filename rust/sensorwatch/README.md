# sensorwatch

Safe, idiomatic Rust bindings for reading hardware sensors through the
[sensorwatch](https://github.com/lcjanke2020/sensorwatch) native core
(Windows / HWiNFO shared memory).

This is the safe wrapper; it pulls in the raw FFI crate
[`sensorwatch-sys`](https://crates.io/crates/sensorwatch-sys) and compiles the C
core straight in, so there is no DLL to locate and building needs only a C
compiler (never libclang).

```rust
use sensorwatch::Session;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new()?;   // Err off Windows, or if HWiNFO is down
    let snapshot = session.snapshot()?;  // an immutable view of all readings
    println!("{} readings from {}", snapshot.len(), snapshot.source());
    for reading in &snapshot {
        let r = reading?;
        println!("{} / {} = {} {} [{:?}]", r.sensor, r.reading, r.value, r.unit, r.kind);
    }
    Ok(())
}
```

`Session` and `Snapshot` are move-only handles freed by `Drop` — Rust's ownership
makes the close/free exactly-once, never-double-free property automatic. Every
native (`sw_error_t`) failure surfaces as an `Error` carrying the `code()` and
message (e.g. `Error::UnsupportedPlatform` off Windows, `Error::SourceUnavailable`
when HWiNFO isn't running).

## Platform support

The core reads sensors on **Windows** (via HWiNFO's shared memory today). On other
platforms every session call returns `Error::UnsupportedPlatform` — the crate still
builds and links everywhere, so it is safe to depend on in cross-platform code.

## License

MIT — see [LICENSE](LICENSE). Part of the
[sensorwatch](https://github.com/lcjanke2020/sensorwatch) project.

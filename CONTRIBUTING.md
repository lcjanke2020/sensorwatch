# Contributing to sensorwatch

Thanks for your interest! sensorwatch is an open-source hardware sensor monitor
for Windows. Contributions — bug reports, fixes, sensor-source adapters, docs —
are welcome.

## Getting started

```sh
git clone https://github.com/lcjanke2020/sensorwatch
cd sensorwatch
uv sync          # or: pip install -e .
python -m sensorwatch --verbose
```

You'll need Windows and [HWiNFO64](https://www.hwinfo.com/) with Shared Memory
Support enabled to exercise the live reader. The code is structured so the
parsing logic in `sensorwatch/hwinfo_shm.py` can be reasoned about without a
running HWiNFO instance.

## Guidelines

- **Keep dependencies minimal.** The core aims to stay close to the standard
  library; new runtime dependencies should be well justified.
- **Match the existing style.** Type hints, small focused functions, and clear
  log messages.
- **Don't make safety-critical decisions on raw sensor data.** See
  [`SECURITY.md`](SECURITY.md) — shared-memory input is treated as untrusted.
- **One logical change per pull request**, with a clear description of what and
  why.

## Reporting bugs

Open an issue with your OS/Python/HWiNFO versions, your `config.toml`, and a
sample of the log output or the error you saw.

## Security

Please report security issues privately rather than in a public issue — see
[`SECURITY.md`](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).

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

## Running the tests

```sh
uv sync
uv run pytest
```

The suite is platform-independent: it covers the config, logging, and
shared-memory parsing logic, feeding the parser synthetic byte buffers rather
than reading a live sensor. CI runs it on Ubuntu and Windows across Python 3.12
and 3.13 (see the [Testing / CI scope](README.md#testing--ci-scope) note).

## Guidelines

- **Keep dependencies minimal.** The core aims to stay close to the standard
  library; new runtime dependencies should be well justified.
- **Match the existing style.** Type hints, small focused functions, and clear
  log messages.
- **Don't make safety-critical decisions on raw sensor data.** See
  [`SECURITY.md`](SECURITY.md) — shared-memory input is treated as untrusted.
- **One logical change per pull request**, with a clear description of what and
  why.

## Releasing

_(Maintainers.)_ Releases publish to [PyPI](https://pypi.org/project/sensorwatch/)
automatically via GitHub Actions OIDC **trusted publishing** — no stored token,
no manual upload. Publishing the GitHub Release fires
[`.github/workflows/publish.yml`](.github/workflows/publish.yml), which builds,
runs the test gate, and uploads with [PEP 740](https://peps.python.org/pep-0740/)
attestations.

```sh
# 1. Bump the version FIRST. PyPI refuses to overwrite an existing version —
#    there is no delete-and-reupload. Keep both in sync:
#      - pyproject.toml        [project] version
#      - sensorwatch/__init__.py  __version__
git commit -am "release: vX.Y.Z" && git push

# 2. Cut the GitHub Release (creates the tag and fires publish.yml):
gh release create vX.Y.Z --generate-notes
```

One-time setup before the first release: on PyPI, add a **pending** trusted
publisher for this repo (owner `lcjanke2020`, repo `sensorwatch`, workflow
`publish.yml`, environment `pypi`). A TestPyPI dry-run first is recommended.

## Reporting bugs

Open an issue with your OS/Python/HWiNFO versions, your `config.toml`, and a
sample of the log output or the error you saw.

## Security

Please report security issues privately rather than in a public issue — see
[`SECURITY.md`](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).

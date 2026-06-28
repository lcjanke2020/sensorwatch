# Running from WSL-2

sensorwatch is a Windows program: it reads HWiNFO64's shared-memory feed
(`Global\HWiNFO_SENS_SM2`), which is a Windows named object. Even so, you may
prefer to *drive* it from a WSL-2 shell — WSL-2 has nicer support for persistent
SSH sessions and terminal multiplexers (tmux, WezTerm) than a bare Windows
console, so a long-running capture can be launched from a session that survives
disconnects.

That works, but only one way: by launching the **Windows** build of sensorwatch
from WSL-2 via interop — *not* as a native Linux process.

## Why you can't run it as a native WSL-2 (Linux) process

Two independent reasons:

1. **A platform guard exits immediately.** `python -m sensorwatch` checks
   `sys.platform` and exits on anything other than Windows.
2. **The data source is unreachable anyway.** HWiNFO publishes sensors into a
   Windows named shared-memory section in the Windows NT object namespace, opened
   via `kernel32` (`OpenFileMappingW` / `MapViewOfFile`). WSL-2 is a separate
   lightweight VM with its own Linux kernel; it does not share that namespace, so
   a Linux process cannot open the mapping even with the guard removed. (One level
   deeper: HWiNFO itself only has the data via a Windows ring-0 driver that the
   WSL-2 VM never sees.)

## How to run it from a WSL-2 shell (Windows interop)

The trick is to invoke a **Windows** Python interpreter from WSL-2. WSL interop
runs it as a native Windows process (`sys.platform == "win32"`), so it reaches
HWiNFO normally — your WSL-2 terminal is just the launcher.

### Prerequisites

- A **Windows** checkout of sensorwatch with a **Windows** Python environment. A
  `uv` venv works well, *provided its base interpreter is a Windows CPython*
  (`.venv\Scripts\python.exe`). A venv created by Linux `uv` inside WSL points at
  a Linux interpreter and will hit the guard above — it won't work. The quickest
  tell: a Windows venv has `Scripts\python.exe`; a Linux one has `bin/python`.
- HWiNFO64 running with **Shared Memory Support** enabled (and the sensors window
  open).
- WSL interop enabled (the default).

### Launch

From your WSL-2 shell, call the Windows interpreter by path. Run from a **real
Windows working directory** (under `/mnt/c/...`) so relative paths like the
`logs/` output directory resolve predictably — launching from a
`\\wsl.localhost\...` UNC path makes Windows fall back to `C:\Windows` as the
working directory.

```sh
# Adjust the path to your Windows checkout
cd /mnt/c/Users/<you>/path/to/sensorwatch

# Module form
.venv/Scripts/python.exe -m sensorwatch --config config.toml --verbose

# ...or the installed console script
.venv/Scripts/sensorwatch.exe --config config.toml --verbose
```

Readings are written under `log_dir` on the Windows filesystem. Stop the capture
with Ctrl-C; each record is flushed as it is written, so stopping never truncates
the log.

### Creating the Windows venv

If you don't already have one, create it with a **Windows** Python — for example
run `uv sync` from a normal Windows terminal in the checkout, or invoke the
Windows build of `uv`/`python` from WSL. The only thing that matters is that the
resulting venv's base interpreter is a Windows CPython (see the prerequisite
above).

## Developing and testing in WSL-2 (native — no Windows needed)

Only the *live capture* needs Windows. Parsing, configuration, and logging are
platform-agnostic; the shared-memory parser is exercised against synthetic byte
buffers, and CI runs on Linux. So you can develop and run the full test suite
natively in WSL-2:

```sh
uv sync
uv run pytest
```

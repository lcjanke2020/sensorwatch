"""setuptools shim for the cffi extension build.

All package metadata lives in ``pyproject.toml`` (PEP 621); this file exists only
to pass cffi's ``cffi_modules`` keyword to ``setup()``. cffi registers that
keyword via a setuptools entry point and, at build time, compiles the
``ffibuilder`` from ``sensorwatch/_native_build.py`` into the extension module
``sensorwatch._sw_cffi`` — which also makes the resulting wheel platform-specific
(non-pure) automatically.
"""

from setuptools import setup

setup(cffi_modules=["sensorwatch/_native_build.py:ffibuilder"])

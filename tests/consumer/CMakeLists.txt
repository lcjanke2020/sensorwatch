# Standalone consumer smoke test for the installed sensorwatch package.
#
# This is NOT part of the main build (it is not add_subdirectory'd). It is
# configured on its own against an install prefix to prove that
# `cmake --install` + `find_package(sensorwatch CONFIG)` actually works for a
# downstream project. Build-only: the targets are compiled and linked but never
# run, so no HWiNFO source and no DLL-on-PATH handling are needed.
#
# Run it (see the consumer-smoke CI jobs) with:
#   cmake -S tests/consumer -B build-consumer -DCMAKE_PREFIX_PATH=<install-prefix>
#   cmake --build build-consumer
# C is the only hard requirement: this project must also configure in a pure-C
# environment (the package supports C-only consumers), so C++ is enabled optionally
# below rather than in project().
cmake_minimum_required(VERSION 3.21)
project(sensorwatch_consumer LANGUAGES C)

# The `0.1` also exercises sensorwatchConfigVersion.cmake (SameMinorVersion).
find_package(sensorwatch 0.1 CONFIG REQUIRED)

# Each consumer is gated on the target it links, so the smoke test adapts to
# whatever the package was installed with (SW_BUILD_SHARED / SW_BUILD_STATIC).
# _sw_consumers collects what actually gets built so we can fail loudly rather than
# silently pass having consumed nothing (e.g. shared+C++ both absent).
set(_sw_consumers "")

# Pure-C consumer of the shared library: proves the C header install plus the
# Windows import library (ARCHIVE) / ELF .so link and the dllimport branch of SW_API.
if(TARGET sensorwatch::sensorwatch)
    add_executable(consumer_c main.c)
    target_link_libraries(consumer_c PRIVATE sensorwatch::sensorwatch)
    list(APPEND _sw_consumers consumer_c)
endif()

# Pure-C consumer of the static library: proves the static ARCHIVE install and that
# its PUBLIC SW_STATIC define arrives (undecorated SW_API). Same main.c, compiled a
# second way, so the static core is exercised even with no C++ compiler present.
if(TARGET sensorwatch::sensorwatch_static)
    add_executable(consumer_c_static main.c)
    target_link_libraries(consumer_c_static PRIVATE sensorwatch::sensorwatch_static)
    list(APPEND _sw_consumers consumer_c_static)
endif()

# C++ consumer of the header-only binding: proves the header install resolves
# (#include <sensorwatch/sensorwatch.hpp>) and cxx_std_17 propagates from
# sensorwatch::hpp. C++ is optional (mirrors the package's own check_language(CXX)
# guard) so this project still configures with no C++ compiler -- the "pure-C
# consumers work" acceptance criterion. Links whichever C core is present (the
# header-only target supplies no ABI implementation): prefer the static lib (no DLL
# runtime dependency), fall back to the shared lib when static was not installed.
include(CheckLanguage)
check_language(CXX)
if(NOT CMAKE_CXX_COMPILER)
    message(STATUS "No C++ compiler found; skipping consumer_hpp (pure-C consumption)")
elseif(TARGET sensorwatch::sensorwatch_static)
    enable_language(CXX)
    add_executable(consumer_hpp main.cpp)
    target_link_libraries(consumer_hpp PRIVATE sensorwatch::hpp sensorwatch::sensorwatch_static)
    list(APPEND _sw_consumers consumer_hpp)
elseif(TARGET sensorwatch::sensorwatch)
    enable_language(CXX)
    add_executable(consumer_hpp main.cpp)
    target_link_libraries(consumer_hpp PRIVATE sensorwatch::hpp sensorwatch::sensorwatch)
    list(APPEND _sw_consumers consumer_hpp)
else()
    # hpp is header-only; with no C core exported there is nothing for it to link.
    message(STATUS "No C core (static/shared) exported; skipping consumer_hpp")
endif()

# Guard against a vacuous pass: if the installed package exported no consumable C
# library for this toolchain, the smoke test built nothing and proved nothing.
if(NOT _sw_consumers)
    message(FATAL_ERROR
        "sensorwatch consumer smoke test built no targets -- the installed package "
        "exported no consumable library reachable from this toolchain.")
endif()
message(STATUS "sensorwatch consumer smoke test targets: ${_sw_consumers}")

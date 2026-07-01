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
cmake_minimum_required(VERSION 3.21)
project(sensorwatch_consumer LANGUAGES C CXX)

# The `0.1` also exercises sensorwatchConfigVersion.cmake (SameMinorVersion).
find_package(sensorwatch 0.1 CONFIG REQUIRED)

# C++ consumer of the header-only binding over the static core. Proves the header
# install resolves (#include <sensorwatch/sensorwatch.hpp>), that cxx_std_17
# propagates from the exported sensorwatch::hpp, and that the static lib's PUBLIC
# SW_STATIC define arrives (undecorated SW_API). Links the static core, so there is
# no DLL runtime dependency.
add_executable(consumer_hpp main.cpp)
target_link_libraries(consumer_hpp PRIVATE sensorwatch::hpp sensorwatch::sensorwatch_static)

# Pure-C consumer of the shared library. Proves the C header install plus the
# Windows import library (ARCHIVE) / ELF .so link and the dllimport branch of
# SW_API. Guarded so the smoke test still works if the package was installed with
# -DSW_BUILD_SHARED=OFF.
if(TARGET sensorwatch::sensorwatch)
    add_executable(consumer_c main.c)
    target_link_libraries(consumer_c PRIVATE sensorwatch::sensorwatch)
endif()

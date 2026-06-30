cmake_minimum_required(VERSION 3.20)

# VERSION tracks the C ABI draft (matches SW_API_VERSION 0.1.0 in the public
# header), deliberately independent of the Python package version in pyproject.toml.
project(sensorwatch
    VERSION 0.1.0
    DESCRIPTION "sensorwatch native C core (HWiNFO shared-memory reader)"
    LANGUAGES C)

# This builds the native C library only. The Python package is built separately
# by hatchling (pyproject.toml) and ignores this file; build/ is git-ignored.

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# Default to a config with debug info but no MSVC /RTC (RTC is incompatible with
# ASan). Single-config generators only; multi-config (VS) ignore this.
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE RelWithDebInfo CACHE STRING "Build type" FORCE)
endif()

# Co-locate the DLL and example/test exes so a freshly built sw_dump.exe finds
# sensorwatch.dll without install/PATH fiddling on Windows.
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}")

option(SW_BUILD_SHARED   "Build the sensorwatch shared library (DLL)"        ON)
option(SW_BUILD_STATIC   "Build the sensorwatch static library"             ON)
option(SW_BUILD_TESTS    "Build the cmocka unit tests"                      ON)
option(SW_BUILD_EXAMPLES "Build example programs (sw_dump)"                 OFF)
option(SW_ENABLE_ASAN    "Build with AddressSanitizer (+UBSan on gcc/clang)" OFF)
option(SW_ENABLE_ANALYZE "Enable MSVC /analyze static analysis (non-fatal)" OFF)
option(SW_BUILD_FUZZ     "Build fuzz targets (follow-up; not yet provided)" OFF)

# --- Sanitizers: applied globally (before any targets) so the sanitizer runtime
#     is consistent across our code and cmocka. ---
if(SW_ENABLE_ASAN)
    if(MSVC)
        add_compile_options(/fsanitize=address /Zi)
        add_link_options(/INCREMENTAL:NO)
    else()
        # -fno-sanitize-recover makes a UBSan finding abort (and fail ctest) rather
        # than just print and continue.
        add_compile_options(-fsanitize=address,undefined -fno-sanitize-recover=all
                            -fno-omit-frame-pointer -g)
        add_link_options(-fsanitize=address,undefined)
    endif()
endif()

# --- Per-target warning / security flags. Applied only to our own targets so a
#     third-party dependency (cmocka) is not held to our -Werror. ---
function(sw_set_target_flags target)
    if(MSVC)
        target_compile_options(${target} PRIVATE /W4 /WX /sdl /guard:cf)
        if(SW_ENABLE_ANALYZE)
            # /analyze:WX- keeps analysis findings non-fatal so analyzer-version
            # differences (local VS2026 vs CI VS2022) don't break the build gate.
            target_compile_options(${target} PRIVATE /analyze /analyze:WX-)
        endif()
    else()
        target_compile_options(${target} PRIVATE
            -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror)
    endif()
endfunction()

set(SW_SOURCES
    src/sw_error.c
    src/sw_string.c
    src/sw_parse.c
    src/sw_snapshot.c
    src/sw_session.c)

# Shared library (the shipped DLL). SW_BUILD_DLL drives SW_API -> dllexport.
if(SW_BUILD_SHARED)
    add_library(sensorwatch SHARED ${SW_SOURCES})
    target_include_directories(sensorwatch PUBLIC include PRIVATE src)
    target_compile_definitions(sensorwatch PRIVATE SW_BUILD_DLL)
    set_target_properties(sensorwatch PROPERTIES C_VISIBILITY_PRESET hidden)
    sw_set_target_flags(sensorwatch)
endif()

# Static library. Tests link this so they can reach internal symbols (the pure
# parser, decode helpers) and compile against an undecorated ABI (SW_STATIC).
if(SW_BUILD_STATIC OR SW_BUILD_TESTS)
    add_library(sensorwatch_static STATIC ${SW_SOURCES})
    target_include_directories(sensorwatch_static PUBLIC include PRIVATE src)
    target_compile_definitions(sensorwatch_static PUBLIC SW_STATIC)
    set_target_properties(sensorwatch_static PROPERTIES POSITION_INDEPENDENT_CODE ON)
    sw_set_target_flags(sensorwatch_static)
endif()

# Header-only C++ binding (include/sensorwatch/sensorwatch.hpp). An INTERFACE
# target carrying only the include directory: consumers link this plus one of the
# C libraries (sensorwatch_static with SW_STATIC, or the DLL). It ships no compiled
# artifact of its own, so it is always available regardless of SW_BUILD_* toggles.
add_library(sensorwatch_hpp INTERFACE)
target_include_directories(sensorwatch_hpp INTERFACE include)
add_library(sensorwatch::hpp ALIAS sensorwatch_hpp)

# --- Tests (cmocka via FetchContent) ---
if(SW_BUILD_TESTS)
    enable_testing()

    include(FetchContent)
    FetchContent_Declare(cmocka
        GIT_REPOSITORY https://gitlab.com/cmocka/cmocka.git
        GIT_TAG        cmocka-2.0.2)
    set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
    set(WITH_EXAMPLES     OFF CACHE BOOL "" FORCE)
    set(WITH_CMOCKERY_SUPPORT OFF CACHE BOOL "" FORCE)
    set(UNIT_TESTING      OFF CACHE BOOL "" FORCE)
    FetchContent_MakeAvailable(cmocka)

    add_library(sw_testutil STATIC tests/c/sw_test_util.c)
    target_include_directories(sw_testutil PUBLIC tests/c PRIVATE src include)
    target_compile_definitions(sw_testutil PRIVATE SW_STATIC)

    # Cross-platform tests: the pure parser, accessors, and error/version surface.
    set(SW_XPLAT_TESTS test_parse test_snapshot test_error)
    foreach(t ${SW_XPLAT_TESTS})
        add_executable(${t} tests/c/${t}.c)
        target_include_directories(${t} PRIVATE src include tests/c)
        target_link_libraries(${t} PRIVATE sensorwatch_static sw_testutil cmocka)
        sw_set_target_flags(${t})
        add_test(NAME ${t} COMMAND ${t})
    endforeach()

    # Windows-only: the Win32 session layer with mocked platform ops.
    if(WIN32)
        add_executable(test_session tests/c/test_session.c)
        target_include_directories(test_session PRIVATE src include tests/c)
        target_link_libraries(test_session PRIVATE sensorwatch_static sw_testutil cmocka)
        sw_set_target_flags(test_session)
        add_test(NAME test_session COMMAND test_session)
    endif()

    # C++ ABI header compile check: the C TUs already exercise the header as C, so
    # this confirms include/sensorwatch/sensorwatch.h is also valid C++ (extern "C",
    # the static_asserts) for C++ consumers. Building the target is the check.
    # Skipped when no C++ compiler is present, so a pure-C toolchain can still build
    # and run the C tests. The C++ standard is set on this target only, not globally.
    include(CheckLanguage)
    check_language(CXX)
    if(CMAKE_CXX_COMPILER)
        enable_language(CXX)
        add_library(sw_abi_header_cxx STATIC tests/c/abi_header_check.cpp)
        target_include_directories(sw_abi_header_cxx PRIVATE include)
        target_compile_definitions(sw_abi_header_cxx PRIVATE SW_STATIC)
        set_target_properties(sw_abi_header_cxx PROPERTIES
            CXX_STANDARD 11
            CXX_STANDARD_REQUIRED ON)

        # C++17 RAII wrapper test. Builds and runs the header-only binding against
        # the static core: links sensorwatch_static (PUBLIC SW_STATIC, so SW_API is
        # undecorated) plus the header-only INTERFACE target. The live-snapshot path
        # is Windows-only and self-skips without a sensor source; the non-Windows
        # build asserts Session construction throws SW_ERR_UNSUPPORTED_PLATFORM. Held
        # to the same /W4 /WX (MSVC) and -Wall -Wextra -Wconversion -Werror (gcc)
        # gate as the C targets via sw_set_target_flags.
        add_executable(test_cpp_wrapper tests/cpp/test_wrapper.cpp)
        target_link_libraries(test_cpp_wrapper PRIVATE sensorwatch_static sensorwatch_hpp)
        set_target_properties(test_cpp_wrapper PROPERTIES
            CXX_STANDARD 17
            CXX_STANDARD_REQUIRED ON)
        sw_set_target_flags(test_cpp_wrapper)
        add_test(NAME test_cpp_wrapper COMMAND test_cpp_wrapper)
    else()
        message(STATUS "No C++ compiler found; skipping the C++ ABI header-compile check")
    endif()
endif()

# --- Example / smoke program (opt-in) ---
if(SW_BUILD_EXAMPLES)
    if(NOT SW_BUILD_SHARED)
        message(FATAL_ERROR "SW_BUILD_EXAMPLES requires SW_BUILD_SHARED=ON")
    endif()
    add_executable(sw_dump examples/c/sw_dump.c)
    target_link_libraries(sw_dump PRIVATE sensorwatch)
    sw_set_target_flags(sw_dump)
endif()

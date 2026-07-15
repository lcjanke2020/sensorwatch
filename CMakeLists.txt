cmake_minimum_required(VERSION 3.21)  # PROJECT_IS_TOP_LEVEL (used by SW_INSTALL) needs >= 3.21

# VERSION tracks the C ABI draft (matches SW_API_VERSION 0.2.0 in the public
# header), deliberately independent of the Python package version in pyproject.toml.
project(sensorwatch
    VERSION 0.2.0
    DESCRIPTION "sensorwatch native C core (HWiNFO shared-memory reader)"
    LANGUAGES C)

# This builds the native C library only. The Python package is built separately
# by hatchling (pyproject.toml) and ignores this file; build/ is git-ignored.

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

# clang-cl reports MSVC=TRUE (it drives the MSVC ABI/CRT) but has its own flag
# surface: no /sdl, no /analyze, gcc-style -f/-W flags accepted, and sanitizer
# runtimes that must be named at link time explicitly -- the objects' embedded
# /DEFAULTLIB directives are not usable when CMake+Ninja invokes lld-link
# directly (see the SW_ENABLE_ASAN branch below). Detect it once so the flag
# blocks below can branch. CI configures both C and CXX as clang-cl, so the
# C-side check governs.
if(CMAKE_C_COMPILER_ID STREQUAL "Clang" AND CMAKE_C_SIMULATE_ID STREQUAL "MSVC")
    set(SW_CLANG_CL TRUE)
else()
    set(SW_CLANG_CL FALSE)
endif()

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
option(SW_ENABLE_ASAN    "Build with AddressSanitizer (+UBSan on gcc/clang; UBSan-trap on clang-cl)" OFF)
option(SW_ENABLE_ANALYZE "Enable MSVC /analyze static analysis (non-fatal; cl.exe only)" OFF)
option(SW_BUILD_FUZZ     "Build libFuzzer targets (clang only; implies ASan+UBSan)" OFF)

# Install / package-export rules (install(), find_package(sensorwatch CONFIG)).
# Default ON for a top-level build, OFF when consumed via add_subdirectory so an
# outer project's `cmake --install` does not also drop sensorwatch's config.
option(SW_INSTALL "Generate install() + find_package(sensorwatch CONFIG) rules" ${PROJECT_IS_TOP_LEVEL})

# GNUInstallDirs gives CMAKE_INSTALL_{LIB,BIN,INCLUDE}DIR; included before the
# targets below because their $<INSTALL_INTERFACE:> uses CMAKE_INSTALL_INCLUDEDIR.
include(GNUInstallDirs)

# Public include interface shared by all consumable targets: $<BUILD_INTERFACE:>
# keeps the absolute source path out of the exported interface (install(EXPORT)
# rejects source-tree paths); $<INSTALL_INTERFACE:> points installed consumers at
# <prefix>/include. Declared once so the three targets stay consistent.
set(SW_PUBLIC_INCLUDES
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>)

# --- Sanitizers: applied globally (before any targets) so the sanitizer runtime
#     is consistent across our code and cmocka. ---
if(SW_CLANG_CL AND SW_ENABLE_ANALYZE)
    message(STATUS "SW_ENABLE_ANALYZE (MSVC /analyze) has no effect under clang-cl")
endif()

if(SW_ENABLE_ASAN)
    if(SW_CLANG_CL)
        # UBSan runs in trap mode: UB becomes an illegal-instruction abort (no
        # runtime to link, inherently non-recoverable), which fails ctest just
        # like -fno-sanitize-recover. The full ubsan_standalone runtime is not
        # wired for direct lld-link linking here. -fno-omit-frame-pointer is
        # not in clang-cl's accepted -f set and needs the /clang: passthrough.
        add_compile_options(-fsanitize=address
                            -fsanitize=undefined -fsanitize-trap=undefined
                            /clang:-fno-omit-frame-pointer /Zi)
        # clang-cl's objects do not carry usable ASan-runtime /DEFAULTLIB
        # directives when the linker is invoked directly (CMake+Ninja calls
        # lld-link, not the compiler driver), so every image fails with
        # undefined __asan_* symbols unless the runtime is named explicitly.
        # Resolve the /MD dynamic-runtime pair from the compiler's resource
        # dir. Exactly one layout is selected -- per-target lib/<triple> first,
        # then legacy lib/windows/*-x86_64 -- and the pair must be complete:
        # a half-present pair or a both-layouts mix would configure fine and
        # then fail (or double-link) at link time.
        execute_process(COMMAND ${CMAKE_C_COMPILER} /clang:-print-resource-dir
                        OUTPUT_VARIABLE SW_CLANG_RESOURCE_DIR
                        OUTPUT_STRIP_TRAILING_WHITESPACE
                        COMMAND_ERROR_IS_FATAL ANY)
        set(SW_RT_PERTARGET "${SW_CLANG_RESOURCE_DIR}/lib/x86_64-pc-windows-msvc")
        set(SW_RT_LEGACY    "${SW_CLANG_RESOURCE_DIR}/lib/windows")
        if(EXISTS "${SW_RT_PERTARGET}/clang_rt.asan_dynamic.lib" AND
           EXISTS "${SW_RT_PERTARGET}/clang_rt.asan_dynamic_runtime_thunk.lib")
            set(SW_ASAN_RT_LIBS
                "${SW_RT_PERTARGET}/clang_rt.asan_dynamic.lib"
                "${SW_RT_PERTARGET}/clang_rt.asan_dynamic_runtime_thunk.lib")
        elseif(EXISTS "${SW_RT_LEGACY}/clang_rt.asan_dynamic-x86_64.lib" AND
               EXISTS "${SW_RT_LEGACY}/clang_rt.asan_dynamic_runtime_thunk-x86_64.lib")
            set(SW_ASAN_RT_LIBS
                "${SW_RT_LEGACY}/clang_rt.asan_dynamic-x86_64.lib"
                "${SW_RT_LEGACY}/clang_rt.asan_dynamic_runtime_thunk-x86_64.lib")
        else()
            message(FATAL_ERROR
                "SW_ENABLE_ASAN with clang-cl: no complete clang_rt.asan_dynamic "
                "+ runtime_thunk pair under ${SW_RT_PERTARGET} or "
                "${SW_RT_LEGACY} -- cannot link the ASan runtime.")
        endif()
        add_link_options(/INCREMENTAL:NO ${SW_ASAN_RT_LIBS})
    elseif(MSVC)
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

# --- Fuzzing: libFuzzer coverage instrumentation + sanitizers, applied globally
#     (before any targets) so the code under test is instrumented. clang-only:
#     -fsanitize=fuzzer is not a gcc flag. Self-enables ASan+UBSan, so a fuzz
#     build needs only -DSW_BUILD_FUZZ=ON (not also -DSW_ENABLE_ASAN). ---
if(SW_BUILD_FUZZ)
    if(NOT CMAKE_C_COMPILER_ID MATCHES "Clang" OR SW_CLANG_CL)
        message(FATAL_ERROR
            "SW_BUILD_FUZZ requires clang with libFuzzer; gcc and clang-cl on the "
            "windows-msvc target are not supported. Re-configure with "
            "-DCMAKE_C_COMPILER=clang (Linux). Current C compiler: "
            "'${CMAKE_C_COMPILER_ID}'.")
    endif()
    # fuzzer-no-link instruments every TU for coverage without pulling libFuzzer's
    # main() into the libraries; the harness target adds -fsanitize=fuzzer to link it.
    add_compile_options(-fsanitize=fuzzer-no-link,address,undefined
                        -fno-sanitize-recover=all -fno-omit-frame-pointer -g)
    add_link_options(-fsanitize=address,undefined)
endif()

# --- Per-target warning / security flags. Applied only to our own targets so a
#     third-party dependency (cmocka) is not held to our -Werror. ---
function(sw_set_target_flags target)
    if(SW_CLANG_CL)
        # /W4 (maps to clang -Wall -Wextra), /WX, /guard:cf are supported by
        # clang-cl; /sdl and /analyze are cl.exe-only and would trip
        # -Wunknown-argument under /WX. The extra -W flags are the clang-cl set
        # docs/C_CODING_STANDARDS.md section 8 asks for (held below the gcc
        # branch's -Wpedantic until the Win32 TUs have burned in under clang).
        target_compile_options(${target} PRIVATE
            /W4 /WX /guard:cf -Wconversion -Wshadow -Wformat=2)
    elseif(MSVC)
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
    target_include_directories(sensorwatch PUBLIC ${SW_PUBLIC_INCLUDES} PRIVATE src)
    target_compile_definitions(sensorwatch PRIVATE SW_BUILD_DLL)
    set_target_properties(sensorwatch PROPERTIES C_VISIBILITY_PRESET hidden)
    # In-tree alias matching the exported name, so add_subdirectory and
    # find_package consumers write the same target_link_libraries().
    add_library(sensorwatch::sensorwatch ALIAS sensorwatch)
    sw_set_target_flags(sensorwatch)
endif()

# Static library. Tests link this so they can reach internal symbols (the pure
# parser, decode helpers) and compile against an undecorated ABI (SW_STATIC); the
# fuzz target (below) links it too, so SW_BUILD_FUZZ must pull it in even when
# SW_BUILD_STATIC and SW_BUILD_TESTS are off.
if(SW_BUILD_STATIC OR SW_BUILD_TESTS OR SW_BUILD_FUZZ)
    add_library(sensorwatch_static STATIC ${SW_SOURCES})
    target_include_directories(sensorwatch_static PUBLIC ${SW_PUBLIC_INCLUDES} PRIVATE src)
    # PUBLIC so it propagates through the export: a consumer linking the static
    # lib compiles with SW_STATIC (undecorated SW_API, no dllimport).
    target_compile_definitions(sensorwatch_static PUBLIC SW_STATIC)
    set_target_properties(sensorwatch_static PROPERTIES POSITION_INDEPENDENT_CODE ON)
    add_library(sensorwatch::sensorwatch_static ALIAS sensorwatch_static)
    sw_set_target_flags(sensorwatch_static)
endif()

# --- Fuzz target (libFuzzer over the pure parser). Built only with
#     -DSW_BUILD_FUZZ=ON, which also enables the coverage + sanitizer
#     instrumentation above. Links the static core to reach the internal
#     sw_parse_buffer() directly, the same way the cmocka tests do. Kept out of
#     sw_set_target_flags (the harness is a libFuzzer TU, not shipped code). ---
if(SW_BUILD_FUZZ)
    add_executable(fuzz_parse tests/fuzz/fuzz_parse.c)
    target_include_directories(fuzz_parse PRIVATE src include)
    target_link_libraries(fuzz_parse PRIVATE sensorwatch_static)
    target_link_options(fuzz_parse PRIVATE -fsanitize=fuzzer)
endif()

# Header-only C++ binding (include/sensorwatch/sensorwatch.hpp). An INTERFACE
# target carrying only the include directory: consumers link this plus one of the
# C libraries (sensorwatch_static with SW_STATIC, or the DLL). It ships no compiled
# artifact of its own, so it is always available regardless of SW_BUILD_* toggles.
add_library(sensorwatch_hpp INTERFACE)
target_include_directories(sensorwatch_hpp INTERFACE ${SW_PUBLIC_INCLUDES})
# Export as sensorwatch::hpp (not sensorwatch::sensorwatch_hpp) to match the alias.
set_target_properties(sensorwatch_hpp PROPERTIES EXPORT_NAME hpp)
add_library(sensorwatch::hpp ALIAS sensorwatch_hpp)

# Propagate the C++17 floor to consumers via the target's usage requirements, so
# linking sensorwatch::hpp under an older standard is a clear error -- but only when
# a C++ compiler is available. The project is declared LANGUAGES C, and requesting
# cxx_std_17 requires CXX to be enabled, so on a pure-C toolchain (or with
# -DSW_BUILD_TESTS=OFF) this would otherwise fail to configure. enable_language(CXX)
# here is a no-op if CXX is already enabled (e.g. by the test block below). This
# feature is absent from the exported target only when the *installing* toolchain
# had no C++ compiler; the header itself #errors below C++17 regardless, so the
# floor is enforced for every consumer independent of install provenance.
include(CheckLanguage)
check_language(CXX)
if(CMAKE_CXX_COMPILER)
    enable_language(CXX)
    target_compile_features(sensorwatch_hpp INTERFACE cxx_std_17)
endif()

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
        # undecorated) plus the header-only INTERFACE target, plus sw_testutil for
        # the synthetic buffers behind the cross-platform populated-snapshot checks
        # (sw_snapshot_from_buffer). The live-snapshot path is Windows-only and
        # self-skips without a sensor source; the non-Windows build asserts Session
        # construction throws SW_ERR_UNSUPPORTED_PLATFORM. Held to the same /W4 /WX
        # (MSVC) and -Wall -Wextra -Wconversion -Werror (gcc) gate as the C targets
        # via sw_set_target_flags.
        add_executable(test_cpp_wrapper tests/cpp/test_wrapper.cpp)
        target_link_libraries(test_cpp_wrapper PRIVATE sensorwatch_static sensorwatch_hpp sw_testutil)
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

# --- Install + find_package(sensorwatch CONFIG) export ---
# Lets downstream projects consume an installed tree (find_package) instead of
# only in-tree add_subdirectory/FetchContent. Exported targets, all under the
# sensorwatch:: namespace to match the in-tree aliases:
#   sensorwatch::sensorwatch         - shared library (DLL) + import lib
#   sensorwatch::sensorwatch_static  - static library (carries SW_STATIC)
#   sensorwatch::hpp                 - header-only C++17 RAII binding
#
# Exporting sensorwatch::hpp alone (no C core) would ship a header-only C++ binding
# with no ABI implementation to link, so a consumer linking it hits undefined sw_*
# references. When SW_INSTALL is on but neither C library is enabled, warn and skip
# the export rather than produce that broken package -- a warning, not a fatal, so a
# test-only/header-dev configure (SW_BUILD_TESTS=ON with both cores off) still works.
if(SW_INSTALL AND NOT SW_BUILD_SHARED AND NOT SW_BUILD_STATIC)
    message(WARNING
        "SW_INSTALL is ON but neither SW_BUILD_SHARED nor SW_BUILD_STATIC is enabled; "
        "skipping install/export (a package exporting only sensorwatch::hpp has no ABI "
        "implementation to link). Enable a C core, or set SW_INSTALL=OFF to silence this.")
elseif(SW_INSTALL)
    include(CMakePackageConfigHelpers)

    # Only export the consumable targets that are actually enabled. sensorwatch_static
    # is keyed on the SW_BUILD_STATIC option, not "was it built" -- it is also built
    # for the tests (SW_BUILD_STATIC OR SW_BUILD_TESTS) but is only *shipped* when the
    # option is on. The header-only interface target is always installable.
    set(_sw_install_targets sensorwatch_hpp)
    if(SW_BUILD_SHARED)
        list(APPEND _sw_install_targets sensorwatch)
    endif()
    if(SW_BUILD_STATIC)
        list(APPEND _sw_install_targets sensorwatch_static)
    endif()

    install(TARGETS ${_sw_install_targets}
        EXPORT  sensorwatchTargets
        ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}   # static lib + Windows import .lib
        LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}   # .so / .dylib
        RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})  # .dll
        # No INCLUDES DESTINATION: the include dir is already carried by the
        # $<INSTALL_INTERFACE:> genexes on the targets above.

    # Public headers: sensorwatch.h (C ABI) + sensorwatch.hpp (C++ binding).
    install(DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/include/sensorwatch
        DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})

    set(SW_INSTALL_CMAKEDIR ${CMAKE_INSTALL_LIBDIR}/cmake/sensorwatch)

    configure_package_config_file(
        ${CMAKE_CURRENT_SOURCE_DIR}/cmake/sensorwatchConfig.cmake.in
        ${CMAKE_CURRENT_BINARY_DIR}/sensorwatchConfig.cmake
        INSTALL_DESTINATION ${SW_INSTALL_CMAKEDIR})

    # SameMinorVersion: pre-1.0 the C ABI may change on a minor bump, so major.minor
    # must match (mirrors check_abi_compatibility() in sensorwatch.hpp). Switch to
    # SameMajorVersion at 1.0.
    write_basic_package_version_file(
        ${CMAKE_CURRENT_BINARY_DIR}/sensorwatchConfigVersion.cmake
        VERSION ${PROJECT_VERSION}
        COMPATIBILITY SameMinorVersion)

    install(EXPORT sensorwatchTargets
        FILE      sensorwatchTargets.cmake
        NAMESPACE sensorwatch::
        DESTINATION ${SW_INSTALL_CMAKEDIR})

    install(FILES
        ${CMAKE_CURRENT_BINARY_DIR}/sensorwatchConfig.cmake
        ${CMAKE_CURRENT_BINARY_DIR}/sensorwatchConfigVersion.cmake
        DESTINATION ${SW_INSTALL_CMAKEDIR})
endif()

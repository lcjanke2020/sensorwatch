// Consumer smoke test: the header-only C++ binding over the installed static core.
// Building + linking is the check; it is not run in CI. check_abi_compatibility()
// links against the core (sw_api_version) and forces a C++17 compile of the header.
#include <sensorwatch/sensorwatch.hpp>

#include <cstdio>

int main() {
    sensorwatch::check_abi_compatibility();  // throws sensorwatch::Error on mismatch
    // Cast for %u: sw_api_version() returns uint32_t (matches examples/c/sw_dump.c).
    std::printf("sensorwatch C++ consumer OK, ABI=%u\n", static_cast<unsigned>(sw_api_version()));
    return 0;
}

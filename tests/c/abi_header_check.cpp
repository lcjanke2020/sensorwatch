// Compile-only check that the public C ABI header is valid C++ as well as C:
// it must parse under a C++ compiler (extern "C" linkage block, the enum-width
// static_asserts). Building this translation unit is the test; it is never linked
// into a shipped artifact.

#include "sensorwatch/sensorwatch.h"

// Reference a couple of ABI symbols so the include is not optimized away and the
// declarations are actually parsed in C++ mode.
unsigned int sw_abi_header_cxx_probe(void)
{
    return SW_API_VERSION + static_cast<unsigned int>(SW_OK);
}

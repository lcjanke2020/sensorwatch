/*
 * Version and error-string accessors. Both are thread-safe and access no shared
 * state (C_ABI.md "Thread Safety"). sw_error_string never returns NULL.
 */

#include "sensorwatch/sensorwatch.h"

SW_API uint32_t SW_CALL sw_api_version(void)
{
    return SW_API_VERSION;
}

SW_API const char *SW_CALL sw_error_string(sw_error_t error)
{
    switch (error) {
    case SW_OK:                       return "Success";
    case SW_ERR_NULL_POINTER:         return "Required pointer argument was null";
    case SW_ERR_INVALID_ARGUMENT:     return "Non-null argument was invalid";
    case SW_ERR_UNSUPPORTED_PLATFORM: return "Backend is unavailable on this platform";
    case SW_ERR_SOURCE_UNAVAILABLE:   return "Sensor source is not running or not enabled";
    case SW_ERR_MAP_FAILED:           return "Source was found but could not be mapped or read";
    case SW_ERR_BAD_MAGIC:            return "Shared-memory magic/version marker did not match";
    case SW_ERR_CORRUPT_DATA:         return "Source data failed structural validation";
    case SW_ERR_OUT_OF_MEMORY:        return "Allocation failed";
    case SW_ERR_INDEX_OUT_OF_RANGE:   return "Snapshot index was outside entry count";
    case SW_ERR_BUFFER_TOO_SMALL:     return "Caller buffer was too small";
    case SW_ERR_VERSION_MISMATCH:     return "Caller/library ABI expectations are incompatible";
    case SW_ERR_INTERNAL:             return "Unexpected library bug or invariant failure";
    }

    /* Any value outside the defined enum (e.g. a corrupted cast). Never NULL. */
    return "Unknown error";
}

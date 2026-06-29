/*
 * Snapshot lifetime and read-only query accessors. A snapshot is immutable after
 * creation, so every accessor here is thread-safe for a live snapshot (C_ABI.md
 * "Thread Safety"). Scalar/enum accessors validate the snapshot pointer, the
 * out-pointer, and the index; string accessors validate the snapshot and index,
 * then defer the buffer logic to sw_copy_string_out().
 */

#include "sw_internal.h"

#include <stdlib.h>

void sw_snapshot_destroy(sw_snapshot_t *snapshot)
{
    if (snapshot == NULL) {
        return;
    }
    if (snapshot->entries != NULL) {
        /* entries is always calloc'd before any field is set, so each pointer is
           either NULL or owned -- the C6001 "uninitialized memory" that MSVC
           /analyze infers from local-only reasoning cannot actually occur, and
           free(NULL) is well-defined. */
#if defined(_MSC_VER)
#  pragma warning(push)
#  pragma warning(disable : 6001)
#endif
        for (uint32_t i = 0; i < snapshot->entry_count; i++) {
            free(snapshot->entries[i].sensor_name);
            free(snapshot->entries[i].reading_name);
            free(snapshot->entries[i].unit);
        }
#if defined(_MSC_VER)
#  pragma warning(pop)
#endif
        free(snapshot->entries);
    }
    free(snapshot->source_name);
    snapshot->magic = 0;  /* poison */
    free(snapshot);
}

SW_API void SW_CALL sw_snapshot_free(sw_snapshot_t *snapshot)
{
    sw_snapshot_destroy(snapshot);
}

SW_API sw_error_t SW_CALL sw_snapshot_entry_count(const sw_snapshot_t *snapshot,
                                                  uint32_t *out_count)
{
    if (snapshot == NULL || out_count == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    *out_count = snapshot->entry_count;
    return SW_OK;
}

SW_API sw_error_t SW_CALL sw_snapshot_get_reading_type(const sw_snapshot_t *snapshot,
                                                       uint32_t index,
                                                       sw_reading_type_t *out_type)
{
    if (snapshot == NULL || out_type == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    if (index >= snapshot->entry_count) {
        return SW_ERR_INDEX_OUT_OF_RANGE;
    }
    *out_type = snapshot->entries[index].type;
    return SW_OK;
}

/* The four numeric accessors share one body; only the struct field differs. */
#define SW_DEFINE_VALUE_ACCESSOR(fn_name, field)                                \
    SW_API sw_error_t SW_CALL fn_name(const sw_snapshot_t *snapshot,            \
                                      uint32_t index, double *out_value)        \
    {                                                                          \
        if (snapshot == NULL || out_value == NULL) {                           \
            return SW_ERR_NULL_POINTER;                                        \
        }                                                                      \
        if (index >= snapshot->entry_count) {                                  \
            return SW_ERR_INDEX_OUT_OF_RANGE;                                  \
        }                                                                      \
        *out_value = snapshot->entries[index].field;                           \
        return SW_OK;                                                          \
    }

SW_DEFINE_VALUE_ACCESSOR(sw_snapshot_get_value,   value)
SW_DEFINE_VALUE_ACCESSOR(sw_snapshot_get_minimum, value_min)
SW_DEFINE_VALUE_ACCESSOR(sw_snapshot_get_maximum, value_max)
SW_DEFINE_VALUE_ACCESSOR(sw_snapshot_get_average, value_avg)

#undef SW_DEFINE_VALUE_ACCESSOR

/* String accessors. source_name is snapshot-wide (one identity for all entries)
   but is still index-validated to honor the documented contract. */

SW_API sw_error_t SW_CALL sw_snapshot_get_source_name(const sw_snapshot_t *snapshot,
                                                      uint32_t index,
                                                      char *buffer,
                                                      size_t buffer_size,
                                                      size_t *out_required)
{
    if (snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    if (index >= snapshot->entry_count) {
        return SW_ERR_INDEX_OUT_OF_RANGE;
    }
    return sw_copy_string_out(snapshot->source_name, buffer, buffer_size, out_required);
}

SW_API sw_error_t SW_CALL sw_snapshot_get_sensor_name(const sw_snapshot_t *snapshot,
                                                      uint32_t index,
                                                      char *buffer,
                                                      size_t buffer_size,
                                                      size_t *out_required)
{
    if (snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    if (index >= snapshot->entry_count) {
        return SW_ERR_INDEX_OUT_OF_RANGE;
    }
    return sw_copy_string_out(snapshot->entries[index].sensor_name,
                              buffer, buffer_size, out_required);
}

SW_API sw_error_t SW_CALL sw_snapshot_get_reading_name(const sw_snapshot_t *snapshot,
                                                       uint32_t index,
                                                       char *buffer,
                                                       size_t buffer_size,
                                                       size_t *out_required)
{
    if (snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    if (index >= snapshot->entry_count) {
        return SW_ERR_INDEX_OUT_OF_RANGE;
    }
    return sw_copy_string_out(snapshot->entries[index].reading_name,
                              buffer, buffer_size, out_required);
}

SW_API sw_error_t SW_CALL sw_snapshot_get_unit(const sw_snapshot_t *snapshot,
                                               uint32_t index,
                                               char *buffer,
                                               size_t buffer_size,
                                               size_t *out_required)
{
    if (snapshot == NULL) {
        return SW_ERR_NULL_POINTER;
    }
    if (index >= snapshot->entry_count) {
        return SW_ERR_INDEX_OUT_OF_RANGE;
    }
    return sw_copy_string_out(snapshot->entries[index].unit,
                              buffer, buffer_size, out_required);
}

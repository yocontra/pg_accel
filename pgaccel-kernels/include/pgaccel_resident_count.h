/*
 * pgaccel_resident_count.h - resident device-side grouped COUNT(*) ABI.
 *
 * The input keys must already be accessible from the active SYCL device. Group
 * assignment and count accumulation stay on the device; only the compact
 * result is copied into the opaque host-owned result state.
 */

#ifndef PGACCEL_RESIDENT_COUNT_H
#define PGACCEL_RESIDENT_COUNT_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct pgaccel_agg_state pgaccel_agg_state;

/*
 * Count equal int64 keys using a bounded, row-parallel device hash table.
 *
 * `max_distinct_hint` is a conservative upper bound for the number of groups.
 * Zero or a value above `row_count` is normalized to `row_count`. The function
 * returns PGACCEL_UNSUPPORTED when the actual group count exceeds the bound or
 * the input cannot be represented by the device table. Every non-OK status
 * leaves `out_state` NULL.
 *
 * Group order is not part of the ABI contract.
 */
pgaccel_status pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
    int64_t* group_keys, size_t row_count, size_t max_distinct_hint,
    pgaccel_agg_state** out_state);

/* Compatibility wrapper for existing resident callers. */
pgaccel_agg_state* pgaccel_hash_count_i64_device_hash_execute_bounded(
    int64_t* group_keys, size_t row_count, size_t max_distinct_hint);

size_t pgaccel_agg_group_count(const pgaccel_agg_state* state);
const void* pgaccel_agg_get_group_keys(const pgaccel_agg_state* state);
const double* pgaccel_agg_get_results(const pgaccel_agg_state* state, size_t agg_idx);
const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state);
void pgaccel_agg_free(pgaccel_agg_state* state);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_RESIDENT_COUNT_H */

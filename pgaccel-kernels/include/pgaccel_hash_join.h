/*
 * pgaccel_hash_join.h - resident count-only hash join API.
 *
 * GpuHashJoin has a deliberately narrow selected-path implementation:
 * INT32/INT64 equality keys only. Other key types return unsupported/null;
 * there is no CPU hash join fallback.
 * NULL keys are excluded from the build side (SQL: NULL = NULL is not TRUE).
 *
 * General row-emitting hash joins are a structural decline. The retained
 * operation borrows resident build/probe buffers and emits only a final
 * match count, so no row-proportional data crosses the device boundary.
 */

#ifndef PGACCEL_HASH_JOIN_H
#define PGACCEL_HASH_JOIN_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Key type tags. */

typedef enum {
  PGACCEL_KEY_INT32 = 0,
  PGACCEL_KEY_INT64 = 1,
} pgaccel_key_type;

/* Hash table handle. */

/// Opaque handle to a resident count-only GPU hash table.
typedef struct pgaccel_hash_table pgaccel_hash_table;

/* Build API. */

/// Build a count-only hash table from already device-resident inner keys.
///
/// The returned table borrows `device_keys` and `device_null_mask`; both must
/// remain allocated and immutable until `pgaccel_hash_join_free`. NULL keys
/// are excluded under SQL equality semantics. Only INT32 and INT64 are
/// supported.
pgaccel_hash_table* pgaccel_hash_join_build_device_count(const void* device_keys,
                                                         const uint8_t* device_null_mask,
                                                         size_t count, pgaccel_key_type key_type);

/// Free table metadata. Borrowed key and null buffers remain caller-owned.
void pgaccel_hash_join_free(pgaccel_hash_table* ht);

/* Probe API. */

/// Count matches for already device-resident outer keys without materializing
/// row pairs. `device_outer_null_mask[i] != 0` excludes that outer row.
/// Returns PGACCEL_UNSUPPORTED instead of wrapping an unrepresentable count.
pgaccel_status pgaccel_hash_join_count_device(const pgaccel_hash_table* ht,
                                              const void* device_outer_keys,
                                              const uint8_t* device_outer_null_mask,
                                              size_t outer_count,
                                              size_t* match_count /* output: actual matches */
);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_HASH_JOIN_H */

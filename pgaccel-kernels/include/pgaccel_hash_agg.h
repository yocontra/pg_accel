/*
 * pgaccel_hash_agg.h — GPU hash aggregation types and API.
 *
 * Implements grouped aggregation with per-group accumulators.
 * Supports SUM, MIN, MAX, COUNT, AVG (decomposed to SUM+COUNT).
 *
 * Integer accumulators use wider types to prevent overflow:
 *   int32 → int64, int64 → int64 (saturating).
 */

#ifndef PGACCEL_HASH_AGG_H
#define PGACCEL_HASH_AGG_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Aggregate function tags ────────────────────────────────────── */

typedef enum {
  PGACCEL_AGG_SUM = 0,
  PGACCEL_AGG_MIN = 1,
  PGACCEL_AGG_MAX = 2,
  PGACCEL_AGG_COUNT = 3,
} pgaccel_agg_func;

/* ── Aggregate column descriptor ────────────────────────────────── */

typedef struct {
  pgaccel_agg_func func;
  size_t col_idx; /* Column index in batch, or SIZE_MAX for COUNT(*) */
} pgaccel_agg_col;

/* ── Grouped aggregation result ─────────────────────────────────── */

/// Opaque handle to grouped aggregation state.
typedef struct pgaccel_agg_state pgaccel_agg_state;

/* ── API ────────────────────────────────────────────────────────── */

/// Perform grouped aggregation on a columnar batch.
///
/// `group_keys` is a contiguous array of group key values (one per row).
/// `group_null_mask[i] == 1` means row i has a NULL group key.
/// `agg_cols` describes each aggregate to compute.
///
/// Returns aggregation state, or NULL on failure.
/// Use pgaccel_agg_get_results to extract results.
pgaccel_agg_state*
pgaccel_hash_agg_execute(const void* group_keys, const uint8_t* group_null_mask, size_t row_count,
                         int key_type,                      /* pgaccel_key_type */
                         const void* const* value_cols,     /* [num_aggs] column data arrays */
                         const uint8_t* const* value_nulls, /* [num_aggs] null masks */
                         const int* value_types,            /* [num_aggs] pgaccel_val_tag */
                         const pgaccel_agg_col* agg_cols, size_t num_aggs);

/// Get the number of groups in the result.
size_t pgaccel_agg_group_count(const pgaccel_agg_state* state);

/// Get the group keys as a contiguous array.
///
/// Returns pointer to internal storage (valid until state is freed).
const void* pgaccel_agg_get_group_keys(const pgaccel_agg_state* state);

/// Get aggregate results for one aggregate column.
///
/// Returns pointer to array of `group_count` f64 values.
const double* pgaccel_agg_get_results(const pgaccel_agg_state* state, size_t agg_idx);

/// Get the count per group (for COUNT aggregates or AVG denominator).
const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state);

/// Free aggregation state.
void pgaccel_agg_free(pgaccel_agg_state* state);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_HASH_AGG_H */

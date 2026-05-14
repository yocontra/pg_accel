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
  /* Partial-mode-only funcs. These are NOT accepted by
   * pgaccel_hash_agg_execute (the finalize-mode entry point) — only by
   * pgaccel_hash_agg_execute_partial below, which emits per-group
   * transition states matching PG's float8_avg_accum / float8_accum
   * shapes:
   *   PGACCEL_AGG_AVG    → [N, sum]            (float8[2])
   *   PGACCEL_AGG_STDDEV → [N, sum, sum_sq]    (float8[3])  — Σx², not Σ(x-μ)²
   *   PGACCEL_AGG_VAR    → [N, sum, sum_sq]    (float8[3])  — same as STDDEV
   * The host emit path converts sum_sq (Σx²) to Sxx = Σx² − Σx²/N. */
  PGACCEL_AGG_AVG = 4,
  PGACCEL_AGG_STDDEV = 5,
  PGACCEL_AGG_VAR = 6,
} pgaccel_agg_func;

/* Partial-mode lane width per agg func (number of f64 lanes per group
 * the partial-mode kernel writes into out_partials). Mirrors
 * PgaccelAggFunc::partial_width on the Rust side. */
static inline size_t pgaccel_agg_partial_width(pgaccel_agg_func f) {
  switch (f) {
    case PGACCEL_AGG_SUM:
    case PGACCEL_AGG_MIN:
    case PGACCEL_AGG_MAX:
    case PGACCEL_AGG_COUNT:
      return 1;
    case PGACCEL_AGG_AVG:
      return 2;
    case PGACCEL_AGG_STDDEV:
    case PGACCEL_AGG_VAR:
      return 3;
    default:
      return 0;
  }
}

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

/// Diagnostic/test entry point: force the sort-based grouped aggregation
/// path and never fall back to the unsorted hash path.
///
/// On backends where the sort-based hashagg path is quarantined (currently
/// Metal/AdaptiveCpp), returns PGACCEL_UNSUPPORTED and leaves `out_state`
/// NULL so callers can decline the pg_accel plan cleanly.
pgaccel_status pgaccel_hash_agg_execute_sort_based(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, pgaccel_agg_state** out_state);

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

/// Perform grouped aggregation in **partial** mode — emits per-group
/// transition states ready for PG's combine functions (Phase 3B).
///
/// Same shape as `pgaccel_hash_agg_execute` for SUM / MIN / MAX / COUNT
/// (1 f64 per group, identical to finalize mode). For PGACCEL_AGG_AVG
/// emits 2 f64s per group (`[N, sum]` matching float8_avg_accum). For
/// PGACCEL_AGG_STDDEV / PGACCEL_AGG_VAR emits 3 f64s per group
/// (`[N, sum, sum_sq]` — host-side converts to Sxx = sum_sq − sum²/N).
///
/// Per-group counts in `pgaccel_agg_get_counts` are total **rows** per
/// group (NULLs included). Per-agg per-group **non-null** counts live in
/// the partial output's first lane for AVG / STDDEV / VAR (= the `N` PG
/// expects).
///
/// Use `pgaccel_agg_get_partial_results` to read per-agg results and
/// `pgaccel_agg_partial_width` (above) to interpret lane shape.
///
/// Returns aggregation state, or NULL on failure.
pgaccel_agg_state*
pgaccel_hash_agg_execute_partial(const void* group_keys, const uint8_t* group_null_mask,
                                 size_t row_count, int key_type, const void* const* value_cols,
                                 const uint8_t* const* value_nulls, const int* value_types,
                                 const pgaccel_agg_col* agg_cols, size_t num_aggs);

/// Get the partial-mode aggregate results for one aggregate column.
///
/// Returns a pointer to `group_count * partial_width(func)` f64 values
/// laid out as `[g0_lane0, g0_lane1, ..., g1_lane0, ...]` (group-major).
/// Returns NULL if `state` is NULL or `agg_idx` is out of bounds.
/// Returns NULL when the state was built in finalize mode.
const double* pgaccel_agg_get_partial_results(const pgaccel_agg_state* state, size_t agg_idx);

/// Get the partial-mode lane width for one aggregate column.
///
/// 1 for SUM/MIN/MAX/COUNT, 2 for AVG, 3 for STDDEV/VAR. Returns 0 when the
/// state has no partial buffer for `agg_idx`.
size_t pgaccel_agg_get_partial_width(const pgaccel_agg_state* state, size_t agg_idx);

/// Get the count per group (for COUNT aggregates or AVG denominator).
const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state);

/// Free aggregation state.
void pgaccel_agg_free(pgaccel_agg_state* state);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_HASH_AGG_H */

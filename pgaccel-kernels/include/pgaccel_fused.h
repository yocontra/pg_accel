#ifndef PGACCEL_FUSED_H
#define PGACCEL_FUSED_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Aggregation operations for fused kernels */
typedef enum {
  PGACCEL_FUSED_SUM = 0,
  PGACCEL_FUSED_MIN = 1,
  PGACCEL_FUSED_MAX = 2,
  PGACCEL_FUSED_COUNT = 3,
} pgaccel_fused_agg_op;

/* Comparison operators for filter predicates */
typedef enum {
  PGACCEL_CMP_EQ = 0, /* == */
  PGACCEL_CMP_NE = 1, /* != */
  PGACCEL_CMP_LT = 2, /* <  */
  PGACCEL_CMP_LE = 3, /* <= */
  PGACCEL_CMP_GT = 4, /* >  */
  PGACCEL_CMP_GE = 5, /* >= */
} pgaccel_cmp_op;

/// Fused filter + single-column reduce.
///
/// Evaluates `filter_col[i] <cmp_op> filter_val` for each row,
/// then applies `agg_op` on `agg_col[i]` for matching rows.
/// Single GPU kernel launch — one memory pass instead of three.
///
/// Returns result in `*out_result`. For COUNT, agg_col may be NULL.
pgaccel_status pgaccel_fused_filter_reduce_f32(
    const float* filter_col,     /* [count] column to filter on */
    pgaccel_cmp_op cmp_op,       /* comparison operator */
    float filter_val,            /* scalar to compare against */
    const float* agg_col,        /* [count] column to aggregate (NULL for COUNT) */
    pgaccel_fused_agg_op agg_op, /* aggregation operation */
    size_t count,                /* number of rows */
    double* out_result           /* scalar output */
);

/// Fused filter + multi-column reduce.
///
/// Same filter, but aggregates multiple columns in one pass.
/// `agg_cols[j]` is the j-th column, `agg_ops[j]` its operation.
/// Results written to `out_results[j]`.
pgaccel_status pgaccel_fused_filter_multi_reduce_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* const* agg_cols,                      /* [num_aggs][count] */
    const pgaccel_fused_agg_op* agg_ops,               /* [num_aggs] */
    size_t num_aggs, size_t count, double* out_results /* [num_aggs] output */
);

/// Fused filter + COUNT(*) — no value column needed.
///
/// Returns count of rows matching the predicate.
pgaccel_status pgaccel_fused_filter_count_f32(const float* filter_col, pgaccel_cmp_op cmp_op,
                                              float filter_val, size_t count, int64_t* out_count);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FUSED_H */

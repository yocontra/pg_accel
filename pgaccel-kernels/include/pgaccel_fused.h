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
  PGACCEL_CMP_ALWAYS_TRUE = 6,
} pgaccel_cmp_op;

typedef struct {
  int op;
  const float* data;
} pgaccel_reduce_col;

/// Fused filter + multi-column reduce.
///
/// Same filter, but aggregates multiple columns in one GPU path.
/// `cols[j].data` is the j-th column and `cols[j].op` its operation.
/// Results are written to `out_results[j]`; the number of passing rows is
/// written to `out_pass_count`.
pgaccel_status pgaccel_fused_filter_multi_reduce_f32(const float* filter_col, size_t count,
                                                     int cmp_op, float filter_val,
                                                     const pgaccel_reduce_col* cols,
                                                     size_t num_cols, float* out_results,
                                                     size_t* out_pass_count);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FUSED_H */

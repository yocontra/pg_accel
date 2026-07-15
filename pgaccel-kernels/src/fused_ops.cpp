// fused_ops.cpp - ABI quarantine for the legacy fused multi-reduce lane.

#include "pgaccel_fused.h"

extern "C" pgaccel_status
pgaccel_fused_filter_multi_reduce_f32(const float* filter_col, size_t count, int cmp_op_raw,
                                      float filter_val, const pgaccel_reduce_col* cols,
                                      size_t num_cols, float* out_results, size_t* out_pass_count) {
  (void)count;
  (void)filter_val;
  (void)cols;

  if (out_results == nullptr || out_pass_count == nullptr)
    return PGACCEL_ERROR;
  if (cmp_op_raw < PGACCEL_CMP_EQ || cmp_op_raw > PGACCEL_CMP_ALWAYS_TRUE)
    return PGACCEL_ERROR;
  if (cmp_op_raw != PGACCEL_CMP_ALWAYS_TRUE && filter_col == nullptr)
    return PGACCEL_ERROR;
  if (num_cols != 0 && cols == nullptr)
    return PGACCEL_ERROR;

  // This export has no resident executor caller. Keep the ABI visible, but do
  // not claim acceleration or publish CPU identities until a registered
  // device-resident lane owns it.
  return PGACCEL_UNSUPPORTED;
}

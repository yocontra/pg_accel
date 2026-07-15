// fused_ops.cpp - ABI quarantine for the legacy fused multi-reduce lane.

#include "pgaccel_fused.h"

namespace {

constexpr size_t GPU_FUSED_THRESHOLD = 8192;

bool valid_reduce_columns(const pgaccel_reduce_col* cols, size_t num_cols) {
  for (size_t j = 0; j < num_cols; ++j) {
    if (cols[j].op < PGACCEL_FUSED_SUM || cols[j].op > PGACCEL_FUSED_COUNT)
      return false;
    if (cols[j].op != PGACCEL_FUSED_COUNT && cols[j].data == nullptr)
      return false;
  }
  return true;
}

}  // namespace

extern "C" pgaccel_status
pgaccel_fused_filter_multi_reduce_f32(const float* filter_col, size_t count, int cmp_op_raw,
                                      float filter_val, const pgaccel_reduce_col* cols,
                                      size_t num_cols, float* out_results, size_t* out_pass_count) {
  (void)filter_val;

  if (out_results == nullptr || out_pass_count == nullptr)
    return PGACCEL_ERROR;
  if (cmp_op_raw < PGACCEL_CMP_EQ || cmp_op_raw > PGACCEL_CMP_ALWAYS_TRUE)
    return PGACCEL_ERROR;
  if (cmp_op_raw != PGACCEL_CMP_ALWAYS_TRUE && filter_col == nullptr)
    return PGACCEL_ERROR;
  if (num_cols == 0) {
    *out_pass_count = 0;
    return PGACCEL_OK;
  }
  if (cols == nullptr)
    return PGACCEL_ERROR;

  if (count == 0) {
    for (size_t j = 0; j < num_cols; ++j)
      out_results[j] = 0.0f;
    *out_pass_count = 0;
    return PGACCEL_OK;
  }

  // Small inputs historically declined before inspecting per-column metadata.
  if (count < GPU_FUSED_THRESHOLD)
    return PGACCEL_UNSUPPORTED;
  if (!valid_reduce_columns(cols, num_cols))
    return PGACCEL_ERROR;

  // This export has no resident executor caller. Keep the ABI visible, but do
  // not claim acceleration until a registered device-resident lane owns it.
  return PGACCEL_UNSUPPORTED;
}

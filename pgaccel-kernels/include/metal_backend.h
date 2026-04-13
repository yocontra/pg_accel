// metal_backend.h — Direct Metal API backend for pg_accel.
//
// Provides the same kernel dispatch semantics as the SYCL path but using
// native Metal API + pre-compiled binary archives. Works after fork()
// because binary archives bypass MTLCompilerService.

#ifndef PGACCEL_METAL_BACKEND_H
#define PGACCEL_METAL_BACKEND_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    METAL_OK = 0,
    METAL_ERROR = -1,
    METAL_ERROR_NO_DEVICE = -5,
    METAL_ERROR_INIT = -6,
    METAL_ERROR_OOM = -3,
} metal_status;

typedef struct {
    char device_name[128];
    char backend_name[64];
    uint32_t compute_units;
    size_t max_alloc_bytes;
    bool has_fp64;
    bool is_unified_memory;
} metal_device_info;

/// Initialize the Metal backend. Safe to call after fork() if parent
/// never touched Metal. Thread-safe (uses std::call_once).
metal_status metal_init(void);

/// Check if Metal backend is initialized.
bool metal_is_initialized(void);

/// Get device info (valid after metal_init).
metal_device_info metal_get_device_info(void);

/// Shutdown Metal backend. Releases all resources.
void metal_shutdown(void);

// ── Reduce kernels ────────────────────────────────────────────────

metal_status metal_reduce_sum_f32(const float* data, size_t count, float* result);
metal_status metal_reduce_min_f32(const float* data, size_t count, float* result);
metal_status metal_reduce_max_f32(const float* data, size_t count, float* result);
metal_status metal_reduce_sum_i64(const int64_t* data, size_t count, int64_t* result);
metal_status metal_reduce_count(const uint8_t* mask, size_t count, size_t* result);

metal_status metal_reduce_multi_f32(
    const float* data, size_t count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_count);

metal_status metal_reduce_multi_i64(
    const int64_t* data, size_t count,
    int64_t* out_sum, int64_t* out_min, int64_t* out_max, int64_t* out_count);

// ── Window kernels ────────────────────────────────────────────────

metal_status metal_window_row_number(
    const uint8_t* partition_starts, size_t count, int64_t* results);

metal_status metal_window_lag(
    const uint8_t* partition_starts,
    const double* values, const uint8_t* null_mask,
    size_t count, int offset, double default_val,
    double* results, uint8_t* result_nulls);

metal_status metal_window_lead(
    const uint8_t* partition_starts,
    const double* values, const uint8_t* null_mask,
    size_t count, int offset, double default_val,
    double* results, uint8_t* result_nulls);

// ── Sort kernels ──────────────────────────────────────────────────
// Keys are pre-converted to sortable uint on the CPU side.
// Internally selects bitonic (<65K) or radix (>=65K).

metal_status metal_sort_kv_u32(uint32_t* keys, uint32_t* indices, size_t count);
metal_status metal_sort_kv_u64(uint64_t* keys, uint32_t* indices, size_t count);

// ── H3 kernels ───────────────────────────────────────────────────

metal_status metal_h3_get_resolution(
    const uint64_t* cells, size_t count, int32_t* results);

metal_status metal_h3_cell_to_parent(
    const uint64_t* cells, size_t count, int parent_res, uint64_t* parents);

metal_status metal_h3_grid_distance(
    const uint64_t* cells_a, const uint64_t* cells_b,
    size_t count, int32_t* distances);

metal_status metal_h3_lat_lng_to_cell(
    const double* lats, const double* lngs,
    size_t count, int resolution,
    uint64_t* cell_ids, uint8_t* valid);

// ── BBox kernels ─────────────────────────────────────────────────

metal_status metal_bbox_intersects_f32(
    const float* boxes_a, size_t count_a,
    const float* boxes_b, size_t count_b,
    uint8_t* result, size_t* hit_count);

// ── Fused filter+reduce kernels ──────────────────────────────────

metal_status metal_fused_filter_reduce_f32(
    const float* filter_col, uint32_t cmp_op, float filter_val,
    const float* agg_col, uint32_t agg_op, size_t count,
    double* out_result);

metal_status metal_fused_filter_count_f32(
    const float* filter_col, uint32_t cmp_op, float filter_val,
    size_t count, int64_t* out_count);

#ifdef __cplusplus
}
#endif

#endif // PGACCEL_METAL_BACKEND_H

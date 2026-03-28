#ifndef PGACCEL_FFI_H
#define PGACCEL_FFI_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    PGACCEL_OK = 0,
    PGACCEL_ERROR = -1,
    PGACCEL_UNSUPPORTED = -2,
    PGACCEL_OOM = -3,
    PGACCEL_TIMEOUT = -4,
} pgaccel_status;

typedef struct {
    char device_name[128];
    char backend_name[64];
    uint32_t compute_units;
    size_t max_alloc_bytes;
    bool has_fp64;
    bool has_atomic64;
    bool is_unified_memory;
} pgaccel_device_info;

typedef struct {
    bool has_fp64;
    bool has_atomic64;
    bool has_ooo_queue;
    bool is_unified_memory;
    size_t max_alloc_bytes;
    uint32_t compute_units;
    char backend_name[64];
} pgaccel_platform_caps;

pgaccel_status pgaccel_init(void);
pgaccel_status pgaccel_shutdown(void);
pgaccel_device_info pgaccel_get_device_info(void);
pgaccel_platform_caps pgaccel_get_caps(void);

/* ── Platform Capability Convenience Predicates ───────────────────── */

bool pgaccel_fp64_available(void);
bool pgaccel_unified_memory(void);
bool pgaccel_ooo_queue_available(void);


/* ── Memory Pool (USM arena allocator) ────────────────────────────── */

void*  pgaccel_alloc(size_t bytes);
void   pgaccel_free(void* ptr);
void   pgaccel_pool_reset(void);
size_t pgaccel_pool_bytes_used(void);
void   pgaccel_prefetch(void* ptr, size_t bytes);

/* ── Bounding Box Overlap ──────────────────────────────────────────── */

/*
 * Bulk bbox intersection: tests every (a[i], b[j]) pair.
 * Each box is 4 consecutive values: xmin, ymin, xmax, ymax.
 * result must point to count_a * count_b bytes (1 = intersects, 0 = not).
 * hit_count receives the total number of intersecting pairs.
 */

/* fp32 path — exact for PostGIS BOX2DF, works on all platforms */
pgaccel_status pgaccel_bbox_intersects_bulk_f32(
    const float* boxes_a,
    size_t count_a,
    const float* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
);

/* fp64 path — PG native box type, requires fp64 hardware support */
pgaccel_status pgaccel_bbox_intersects_bulk_f64(
    const double* boxes_a,
    size_t count_a,
    const double* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
);

/* ── Sort Kernels ─────────────────────────────────────────────────── */

pgaccel_status pgaccel_sort_f32(float* data, size_t count);
pgaccel_status pgaccel_sort_f64(double* data, size_t count);
pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count);
pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count);

/*
 * Key-value sort: sorts keys[] and permutes indices[] to match.
 * Stable for equal keys (preserves original row order).
 */
pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices, size_t count);
pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices, size_t count);

/* ── Reduce Kernels ──────────────────────────────────────────────── */

/* fp32 reductions — all platforms */
pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count, float* result);

/* fp64 reductions — CUDA/ROCm/Level Zero only, returns UNSUPPORTED on Metal */
pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count, double* result);

/* i64 sum — all platforms */
pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count, int64_t* result);

/* Count nonzero bytes in mask (popcount) — all platforms */
pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count, size_t* result);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FFI_H */

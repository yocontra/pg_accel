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

#ifdef __cplusplus
}
#endif

#endif // PGACCEL_METAL_BACKEND_H

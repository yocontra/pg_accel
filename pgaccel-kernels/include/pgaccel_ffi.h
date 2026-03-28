#ifndef PGACCEL_FFI_H
#define PGACCEL_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    PGACCEL_OK = 0,
    PGACCEL_ERROR_INIT = 1,
    PGACCEL_ERROR_NO_DEVICE = 2,
    PGACCEL_ERROR_OOM = 3,
    PGACCEL_ERROR_TIMEOUT = 4,
    PGACCEL_ERROR_UNSUPPORTED = 5,
} pgaccel_status;

typedef struct {
    char device_name[256];
    char backend_name[64];
    int has_fp64;
    int has_atomic64;
    int has_ooo_queue;
    int is_unified_memory;
    size_t max_alloc_bytes;
    uint32_t compute_units;
} pgaccel_device_info;

typedef struct {
    int has_fp64;
    int has_atomic64;
    int has_ooo_queue;
    int is_unified_memory;
    size_t max_alloc_bytes;
    uint32_t compute_units;
    char backend_name[64];
} pgaccel_platform_caps;

pgaccel_status pgaccel_init(void);
pgaccel_status pgaccel_shutdown(void);
pgaccel_device_info pgaccel_get_device_info(void);
pgaccel_platform_caps pgaccel_get_caps(void);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FFI_H */

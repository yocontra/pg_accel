#include "pgaccel_ffi.h"
#include <cstring>
#include <cstdio>

static bool g_initialized = false;

extern "C" pgaccel_status pgaccel_init(void) {
    if (g_initialized) return PGACCEL_OK;
    g_initialized = true;
    return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_shutdown(void) {
    g_initialized = false;
    return PGACCEL_OK;
}

extern "C" pgaccel_device_info pgaccel_get_device_info(void) {
    pgaccel_device_info info = {};
    std::strncpy(info.device_name, "CPU Fallback", sizeof(info.device_name) - 1);
    std::strncpy(info.backend_name, "cpu", sizeof(info.backend_name) - 1);
    info.has_fp64 = 1;
    info.has_atomic64 = 1;
    info.has_ooo_queue = 0;
    info.is_unified_memory = 1;
    info.max_alloc_bytes = 0;
    info.compute_units = 0;
    return info;
}

extern "C" pgaccel_platform_caps pgaccel_get_caps(void) {
    pgaccel_platform_caps caps = {};
    caps.has_fp64 = 1;
    caps.has_atomic64 = 1;
    caps.has_ooo_queue = 0;
    caps.is_unified_memory = 1;
    caps.max_alloc_bytes = 0;
    caps.compute_units = 0;
    std::strncpy(caps.backend_name, "cpu", sizeof(caps.backend_name) - 1);
    return caps;
}

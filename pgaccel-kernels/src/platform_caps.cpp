// platform_caps.cpp — Platform capability queries.
//
// The actual capability detection is performed in device_manager.cpp during
// pgaccel_init(). This translation unit provides helpers that kernel code can
// use to branch on platform capabilities without pulling in pgaccel_ffi.h.
//
// Currently all state lives in device_manager.cpp (the pgaccel_platform_caps
// struct). This file exists as the designated home for any future capability
// logic that grows beyond what fits in device_manager — e.g., runtime feature
// negotiation, capability-based kernel dispatch tables, or platform quirk
// workarounds.

#include "pgaccel_ffi.h"

// Re-export: callers in the kernel library can use pgaccel_get_caps() directly.
// This file is intentionally minimal — capability population happens at init
// time in device_manager.cpp, and the result is a read-only struct thereafter.
//
// Future additions that belong here:
//   - pgaccel_supports_kernel(kernel_id) — check if platform can run a kernel
//   - pgaccel_preferred_workgroup_size(kernel_id) — platform-tuned sizes
//   - pgaccel_fp64_available() — convenience predicate
//   - Quirk tables for specific GPU models / driver versions

extern "C" bool pgaccel_fp64_available(void) {
    pgaccel_platform_caps caps = pgaccel_get_caps();
    return caps.has_fp64;
}

extern "C" bool pgaccel_unified_memory(void) {
    pgaccel_platform_caps caps = pgaccel_get_caps();
    return caps.is_unified_memory;
}

extern "C" bool pgaccel_ooo_queue_available(void) {
    pgaccel_platform_caps caps = pgaccel_get_caps();
    return caps.has_ooo_queue;
}

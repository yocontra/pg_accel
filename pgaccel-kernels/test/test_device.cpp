#include "pgaccel_ffi.h"
#include <cstdio>

int main() {
    pgaccel_status status = pgaccel_init();
    if (status != PGACCEL_OK) {
        fprintf(stderr, "pgaccel_init failed: %d\n", status);
        return 1;
    }

    pgaccel_device_info info = pgaccel_get_device_info();
    pgaccel_platform_caps caps = pgaccel_get_caps();

    printf("Device: %s\n", info.device_name);
    printf("Backend: %s\n", info.backend_name);
    printf("FP64: %s\n", caps.has_fp64 ? "yes" : "no");
    printf("Atomic64: %s\n", caps.has_atomic64 ? "yes" : "no");
    printf("OOQ: %s\n", caps.has_ooo_queue ? "yes" : "no");
    printf("Unified Memory: %s\n", caps.is_unified_memory ? "yes" : "no");
    printf("Compute Units: %u\n", caps.compute_units);

    pgaccel_shutdown();
    return 0;
}

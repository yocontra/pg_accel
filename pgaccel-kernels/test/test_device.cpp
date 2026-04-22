#include <cstdio>

#include "pgaccel_ffi.h"

int main() {
  pgaccel_status status = pgaccel_init();
  if (status != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed: %d\n", status);
    return 1;
  }

  pgaccel_device_info info = pgaccel_get_device_info();
  pgaccel_platform_caps caps = pgaccel_get_caps();

  printf("=== Device Info ===\n");
  printf("  Device:          %s\n", info.device_name);
  printf("  Backend:         %s\n", info.backend_name);
  printf("  Compute Units:   %u\n", info.compute_units);
  printf("  Max Alloc:       %zu bytes\n", info.max_alloc_bytes);
  printf("  FP64:            %s\n", info.has_native_fp64 ? "yes" : "no");
  printf("  Atomic64:        %s\n", info.has_atomic64 ? "yes" : "no");
  printf("  Unified Memory:  %s\n", info.is_unified_memory ? "yes" : "no");

  printf("\n=== Platform Caps ===\n");
  printf("  Backend:         %s\n", caps.backend_name);
  printf("  FP64:            %s\n", caps.has_native_fp64 ? "yes" : "no");
  printf("  Atomic64:        %s\n", caps.has_atomic64 ? "yes" : "no");
  printf("  OOO Queue:       %s\n", caps.has_ooo_queue ? "yes" : "no");
  printf("  Unified Memory:  %s\n", caps.is_unified_memory ? "yes" : "no");
  printf("  Compute Units:   %u\n", caps.compute_units);
  printf("  Max Alloc:       %zu bytes\n", caps.max_alloc_bytes);

  printf("\n=== Convenience Predicates ===\n");
  printf("  fp64_available:  %s\n", pgaccel_fp64_available() ? "yes" : "no");
  printf("  unified_memory:  %s\n", pgaccel_unified_memory() ? "yes" : "no");
  printf("  ooo_queue:       %s\n", pgaccel_ooo_queue_available() ? "yes" : "no");

  // Verify consistency between device_info and caps.
  bool consistent = true;
  if (info.has_native_fp64 != caps.has_native_fp64) {
    fprintf(stderr, "MISMATCH: has_native_fp64 differs between info and caps\n");
    consistent = false;
  }
  if (info.has_atomic64 != caps.has_atomic64) {
    fprintf(stderr, "MISMATCH: has_atomic64 differs between info and caps\n");
    consistent = false;
  }
  if (info.is_unified_memory != caps.is_unified_memory) {
    fprintf(stderr, "MISMATCH: is_unified_memory differs between info and caps\n");
    consistent = false;
  }
  if (info.compute_units != caps.compute_units) {
    fprintf(stderr, "MISMATCH: compute_units differs between info and caps\n");
    consistent = false;
  }

  if (consistent) {
    printf("\nAll fields consistent between device_info and caps.\n");
  } else {
    fprintf(stderr, "\nInconsistencies detected!\n");
    pgaccel_shutdown();
    return 1;
  }

  status = pgaccel_shutdown();
  if (status != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_shutdown failed: %d\n", status);
    return 1;
  }

  printf("Shutdown OK.\n");
  return 0;
}

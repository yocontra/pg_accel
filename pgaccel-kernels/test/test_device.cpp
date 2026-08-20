#include <sycl/sycl.hpp>

#include <cstdio>

#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;
extern sycl::queue* g_ooo_queue;

#if defined(PGACCEL_TEST_HOOKS)
extern "C" void pgacceltest_fail_before_ooo_queue_once(void);
extern "C" unsigned pgacceltest_unpublished_queue_count(void);
extern "C" bool pgacceltest_grouped_agg_cleanup_exception_is_caught(void);
extern "C" bool pgacceltest_grouped_agg_helper_semantics(void);
#endif

int main() {
#if defined(PGACCEL_TEST_HOOKS)
  pgacceltest_fail_before_ooo_queue_once();
  if (pgaccel_init() != PGACCEL_ERROR) {
    fprintf(stderr, "injected second-queue construction failure did not fail initialization\n");
    return 1;
  }
  if (g_queue != nullptr || g_ooo_queue != nullptr || pgacceltest_unpublished_queue_count() != 0) {
    fprintf(stderr, "failed initialization published or leaked a queue\n");
    return 1;
  }
  pgaccel_device_info failed_info = pgaccel_get_device_info();
  pgaccel_platform_caps failed_caps = pgaccel_get_caps();
  if (failed_info.device_name[0] != '\0' || failed_info.backend_name[0] != '\0' ||
      failed_caps.backend_name[0] != '\0') {
    fprintf(stderr, "failed initialization published device metadata\n");
    return 1;
  }
  if (!pgacceltest_grouped_agg_cleanup_exception_is_caught()) {
    fprintf(stderr, "noexcept scratch cleanup did not catch the injected failure\n");
    return 1;
  }
  if (!pgacceltest_grouped_agg_helper_semantics()) {
    fprintf(stderr, "grouped aggregate host helper semantics regressed\n");
    return 1;
  }
#endif

  pgaccel_status status = pgaccel_init();
  if (status != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed: %d\n", status);
    return 1;
  }

  if (g_queue == nullptr || g_ooo_queue == nullptr) {
    fprintf(stderr, "successful initialization did not publish both queues\n");
    pgaccel_shutdown();
    return 1;
  }
  sycl::queue* const initial_queue = g_queue;
  sycl::queue* const initial_ooo_queue = g_ooo_queue;
  if (pgaccel_init() != PGACCEL_OK || g_queue != initial_queue ||
      g_ooo_queue != initial_ooo_queue) {
    fprintf(stderr, "idempotent initialization replaced the published queue pair\n");
    pgaccel_shutdown();
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

  printf("\n=== Platform Caps ===\n");
  printf("  Backend:         %s\n", caps.backend_name);
  printf("  FP64:            %s\n", caps.has_native_fp64 ? "yes" : "no");
  printf("  Atomic64:        %s\n", caps.has_atomic64 ? "yes" : "no");
  printf("  OOO Queue:       %s\n", caps.has_ooo_queue ? "yes" : "no");
  printf("  Compute Units:   %u\n", caps.compute_units);
  printf("  Max Alloc:       %zu bytes\n", caps.max_alloc_bytes);

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
  if (g_queue != nullptr || g_ooo_queue != nullptr) {
    fprintf(stderr, "shutdown did not clear the published queue pair\n");
    return 1;
  }
#if defined(PGACCEL_TEST_HOOKS)
  if (pgacceltest_unpublished_queue_count() != 0) {
    fprintf(stderr, "shutdown left queue publication ownership outstanding\n");
    return 1;
  }
#endif

  printf("Shutdown OK.\n");
  return 0;
}

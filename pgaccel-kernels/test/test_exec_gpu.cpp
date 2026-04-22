/// Test that a fork()+exec()'d process can use GPU.
/// This binary is designed to be exec'd from a forked child.
/// Usage: test_exec_gpu
///   - Initializes SYCL/Metal GPU
///   - Allocates shared memory
///   - Runs a simple parallel_for kernel
///   - Verifies the GPU actually wrote data
///   - Exits 0 on success, 1 on failure

#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "pgaccel_ffi.h"

int main() {
  fprintf(stderr, "test_exec_gpu: initializing GPU...\n");
  pgaccel_status st = pgaccel_init();
  if (st != PGACCEL_OK) {
    fprintf(stderr, "test_exec_gpu: pgaccel_init failed: %d\n", st);
    return 1;
  }

  pgaccel_device_info info = pgaccel_get_device_info();
  fprintf(stderr, "test_exec_gpu: device=%s, CUs=%u\n", info.device_name, info.compute_units);

  // Test sort: 100K floats via GPU bitonic sort.
  const size_t N = 100000;
  float* keys = new float[N];
  uint32_t* indices = new uint32_t[N];

  for (size_t i = 0; i < N; i++) {
    keys[i] = static_cast<float>(rand()) / static_cast<float>(RAND_MAX);
    indices[i] = static_cast<uint32_t>(i);
  }

  fprintf(stderr, "test_exec_gpu: pre-sort keys[0..5] = %f %f %f %f %f\n", keys[0], keys[1],
          keys[2], keys[3], keys[4]);

  st = pgaccel_sort_kv_f32(keys, indices, N);
  if (st != PGACCEL_OK) {
    fprintf(stderr, "test_exec_gpu: sort failed: %d\n", st);
    delete[] keys;
    delete[] indices;
    return 1;
  }

  fprintf(stderr, "test_exec_gpu: post-sort keys[0..5] = %f %f %f %f %f\n", keys[0], keys[1],
          keys[2], keys[3], keys[4]);

  // Verify sorted
  bool sorted = true;
  for (size_t i = 1; i < N; i++) {
    if (keys[i] < keys[i - 1]) {
      fprintf(stderr, "test_exec_gpu: NOT SORTED at i=%zu: %f > %f\n", i, keys[i - 1], keys[i]);
      sorted = false;
      break;
    }
  }

  uint64_t gpu_count = pgaccel_gpu_exec_count();
  fprintf(stderr, "test_exec_gpu: gpu_exec_count=%llu, sorted=%s\n", (unsigned long long)gpu_count,
          sorted ? "YES" : "NO");

  delete[] keys;
  delete[] indices;
  pgaccel_shutdown();

  if (!sorted) {
    fprintf(stderr, "test_exec_gpu: FAIL — data not sorted\n");
    return 1;
  }
  if (gpu_count == 0) {
    fprintf(stderr, "test_exec_gpu: FAIL — gpu_exec_count is 0\n");
    return 1;
  }

  fprintf(stderr, "test_exec_gpu: PASS\n");
  return 0;
}

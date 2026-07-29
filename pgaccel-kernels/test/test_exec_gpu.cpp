// Fork/exec GPU smoke using the retained resident count-join operation.

#include <cstdint>
#include <cstdio>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_join.h"

int main() {
  std::fprintf(stderr, "test_exec_gpu: initializing GPU...\n");
  pgaccel_status status = pgaccel_init();
  if (status != PGACCEL_OK) {
    std::fprintf(stderr, "test_exec_gpu: pgaccel_init failed: %d\n", status);
    return 1;
  }

  const pgaccel_device_info info = pgaccel_get_device_info();
  std::fprintf(stderr, "test_exec_gpu: device=%s, CUs=%u\n", info.device_name, info.compute_units);

  constexpr size_t kRows = 100000;
  constexpr size_t kDomain = 257;
  std::vector<int32_t> keys(kRows);
  std::vector<size_t> frequencies(kDomain, 0);
  for (size_t row = 0; row < kRows; ++row) {
    keys[row] = static_cast<int32_t>((row * 37 + 11) % kDomain);
    ++frequencies[static_cast<size_t>(keys[row])];
  }
  size_t expected = 0;
  for (size_t frequency : frequencies)
    expected += frequency * frequency;

  void* device_keys = nullptr;
  status = pgaccel_expr_device_alloc_copy(keys.data(), keys.size() * sizeof(keys[0]), &device_keys);
  if (status != PGACCEL_OK || device_keys == nullptr) {
    std::fprintf(stderr, "test_exec_gpu: resident key allocation failed: %d\n", status);
    pgaccel_shutdown();
    return 1;
  }

  pgaccel_reset_gpu_exec_count();
  pgaccel_hash_table* table =
      pgaccel_hash_join_build_device_count(device_keys, nullptr, keys.size(), PGACCEL_KEY_INT32);
  if (table == nullptr) {
    std::fprintf(stderr, "test_exec_gpu: resident hash build failed\n");
    pgaccel_expr_device_free(device_keys);
    pgaccel_shutdown();
    return 1;
  }

  size_t actual = 0;
  status = pgaccel_hash_join_count_device(table, device_keys, nullptr, keys.size(), &actual);
  const uint64_t gpu_count = pgaccel_gpu_exec_count();
  pgaccel_hash_join_free(table);
  pgaccel_expr_device_free(device_keys);
  pgaccel_shutdown();

  std::fprintf(stderr, "test_exec_gpu: status=%d count=%zu expected=%zu gpu_exec_count=%llu\n",
               status, actual, expected, static_cast<unsigned long long>(gpu_count));
  if (status != PGACCEL_OK || actual != expected || gpu_count < 2) {
    std::fprintf(stderr, "test_exec_gpu: FAIL\n");
    return 1;
  }
  std::fprintf(stderr, "test_exec_gpu: PASS\n");
  return 0;
}

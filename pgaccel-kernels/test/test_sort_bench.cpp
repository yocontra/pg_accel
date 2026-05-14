// test_sort_bench.cpp
//
// Correctness + perf harness for the GPU sort radix paths.
// Used during radix-sort development to verify that the Metal-safe radix
// implementation produces sorted output and is faster than bitonic on
// million-row keys.
//
// Usage: ./test_sort_bench [n]
//
//   n = dataset size (default 1_000_000)

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <random>
#include <vector>

#include "pgaccel_ffi.h"

using clk = std::chrono::steady_clock;
using dur_ms = std::chrono::duration<double, std::milli>;

static int g_fail = 0;

template <typename T>
static bool pg_less_value(T a, T b) {
  const bool a_nan = (a != a);
  const bool b_nan = (b != b);
  if (a_nan)
    return false;
  if (b_nan)
    return true;
  return a < b;
}

template <typename T>
static bool pg_equal_value(T a, T b) {
  const bool a_nan = (a != a);
  const bool b_nan = (b != b);
  return (a_nan && b_nan) || (!a_nan && !b_nan && a == b);
}

static uint64_t value_bits(float v) {
  uint32_t bits = 0;
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

static uint64_t value_bits(double v) {
  uint64_t bits = 0;
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

static float f32_from_bits(uint32_t bits) {
  float v = 0.0f;
  std::memcpy(&v, &bits, sizeof(v));
  return v;
}

static double f64_from_bits(uint64_t bits) {
  double v = 0.0;
  std::memcpy(&v, &bits, sizeof(v));
  return v;
}

static float special_f32(size_t i) {
  switch (i % 16) {
    case 0:
      return -0.0f;
    case 1:
      return 0.0f;
    case 2:
      return -std::numeric_limits<float>::infinity();
    case 3:
      return std::numeric_limits<float>::infinity();
    case 4:
      return f32_from_bits(0x7fc00001u);
    case 5:
      return f32_from_bits(0x7fc12345u);
    case 6:
    case 7:
      return -17.5f;
    case 8:
    case 9:
      return 42.25f;
    default:
      return static_cast<float>(static_cast<int>(i % 97) - 48);
  }
}

static double special_f64(size_t i) {
  switch (i % 16) {
    case 0:
      return -0.0;
    case 1:
      return 0.0;
    case 2:
      return -std::numeric_limits<double>::infinity();
    case 3:
      return std::numeric_limits<double>::infinity();
    case 4:
      return f64_from_bits(0x7ff8000000000001ULL);
    case 5:
      return f64_from_bits(0x7ff8000000012345ULL);
    case 6:
    case 7:
      return -17.5;
    case 8:
    case 9:
      return 42.25;
    default:
      return static_cast<double>(static_cast<int>(i % 97) - 48);
  }
}

static void check_sorted_i32(const int32_t* data, size_t n, const char* tag) {
  for (size_t i = 1; i < n; ++i) {
    if (data[i] < data[i - 1]) {
      fprintf(stderr, "[%s] FAIL: not sorted at i=%zu (%d < %d)\n", tag, i, data[i], data[i - 1]);
      ++g_fail;
      return;
    }
  }
  printf("[%s] sorted OK (n=%zu)\n", tag, n);
}

static void check_sorted_i64(const int64_t* data, size_t n, const char* tag) {
  for (size_t i = 1; i < n; ++i) {
    if (data[i] < data[i - 1]) {
      fprintf(stderr, "[%s] FAIL: not sorted at i=%zu (%lld < %lld)\n", tag, i, (long long)data[i],
              (long long)data[i - 1]);
      ++g_fail;
      return;
    }
  }
  printf("[%s] sorted OK (n=%zu)\n", tag, n);
}

static void check_kv_sorted_i32(const int32_t* keys, const uint32_t* indices,
                                const int32_t* original, size_t n, const char* tag) {
  for (size_t i = 1; i < n; ++i) {
    if (keys[i] < keys[i - 1]) {
      fprintf(stderr, "[%s] FAIL keys not sorted at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
  }
  // Check indices reference correct original values.
  for (size_t i = 0; i < n; ++i) {
    if (original[indices[i]] != keys[i]) {
      fprintf(stderr,
              "[%s] FAIL index/key mismatch at i=%zu "
              "(idx=%u orig=%d key=%d)\n",
              tag, i, indices[i], original[indices[i]], keys[i]);
      ++g_fail;
      return;
    }
  }
  printf("[%s] kv-sorted OK (n=%zu)\n", tag, n);
}

template <typename T>
static void check_scalar_float_specials(const char* tag, pgaccel_status (*sort_fn)(T*, size_t),
                                        T (*make_value)(size_t)) {
  constexpr size_t n = 65536;
  std::vector<T> data(n);
  for (size_t i = 0; i < n; ++i)
    data[i] = make_value(i);

  std::vector<T> original(data);
  std::vector<uint32_t> order(n);
  for (size_t i = 0; i < n; ++i)
    order[i] = static_cast<uint32_t>(i);
  std::stable_sort(order.begin(), order.end(),
                   [&](uint32_t a, uint32_t b) { return pg_less_value(original[a], original[b]); });

  pgaccel_status st = sort_fn(data.data(), n);
  if (st != PGACCEL_OK) {
    fprintf(stderr, "[%s] FAIL: status=%d\n", tag, (int)st);
    ++g_fail;
    return;
  }

  for (size_t i = 1; i < n; ++i) {
    if (pg_less_value(data[i], data[i - 1])) {
      fprintf(stderr, "[%s] FAIL: not sorted at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
  }
  for (size_t i = 0; i < n; ++i) {
    if (value_bits(data[i]) != value_bits(original[order[i]])) {
      fprintf(stderr, "[%s] FAIL: value bits changed at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
  }
  printf("[%s] scalar specials OK (n=%zu)\n", tag, n);
}

template <typename T>
static void check_kv_float_specials(const char* tag,
                                    pgaccel_status (*sort_fn)(T*, uint32_t*, size_t),
                                    T (*make_value)(size_t)) {
  constexpr size_t n = 65536;
  std::vector<T> keys(n);
  std::vector<uint32_t> indices(n);
  for (size_t i = 0; i < n; ++i) {
    keys[i] = make_value(i);
    indices[i] = static_cast<uint32_t>(i);
  }

  std::vector<T> original(keys);
  pgaccel_status st = sort_fn(keys.data(), indices.data(), n);
  if (st != PGACCEL_OK) {
    fprintf(stderr, "[%s] FAIL: status=%d\n", tag, (int)st);
    ++g_fail;
    return;
  }

  for (size_t i = 1; i < n; ++i) {
    if (pg_less_value(keys[i], keys[i - 1])) {
      fprintf(stderr, "[%s] FAIL: keys not sorted at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
    if (pg_equal_value(keys[i], keys[i - 1]) && indices[i] < indices[i - 1]) {
      fprintf(stderr, "[%s] FAIL: stable order violated at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
  }
  for (size_t i = 0; i < n; ++i) {
    if (indices[i] >= n || value_bits(keys[i]) != value_bits(original[indices[i]])) {
      fprintf(stderr, "[%s] FAIL: index/key payload mismatch at i=%zu\n", tag, i);
      ++g_fail;
      return;
    }
  }
  printf("[%s] kv specials OK (n=%zu)\n", tag, n);
}

int main(int argc, char** argv) {
  size_t n = 1'000'000;
  if (argc > 1)
    n = static_cast<size_t>(std::atoll(argv[1]));

  printf("== Sort benchmark (n=%zu) ==\n", n);

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed — CPU-only run\n");
  }

  auto info = pgaccel_get_device_info();
  printf("device=%s backend=%s CUs=%u\n", info.device_name, info.backend_name, info.compute_units);

  printf("\n== Floating-point radix edge cases ==\n");
  check_scalar_float_specials<float>("float32", pgaccel_sort_f32, special_f32);
  check_kv_float_specials<float>("float32-kv", pgaccel_sort_kv_f32, special_f32);
  check_scalar_float_specials<double>("float64", pgaccel_sort_f64, special_f64);
  check_kv_float_specials<double>("float64-kv", pgaccel_sort_kv_f64, special_f64);

  std::mt19937 rng(0xC0FFEE);
  std::uniform_int_distribution<int32_t> dist32(INT32_MIN, INT32_MAX);
  std::uniform_int_distribution<int64_t> dist64(INT64_MIN, INT64_MAX);

  // --- int32 ---------------------------------------------------------
  {
    std::vector<int32_t> data(n);
    for (size_t i = 0; i < n; ++i)
      data[i] = dist32(rng);
    std::vector<int32_t> expected(data);
    std::sort(expected.begin(), expected.end());

    // Warm-up kernel cache.
    std::vector<int32_t> warm(4096);
    for (size_t i = 0; i < warm.size(); ++i)
      warm[i] = dist32(rng);
    pgaccel_sort_i32(warm.data(), warm.size());

    auto t0 = clk::now();
    pgaccel_status st = pgaccel_sort_i32(data.data(), n);
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();
    printf("int32  sort status=%d time=%.2f ms  (%.1f M/s)\n", (int)st, ms,
           n / (ms / 1000.0) / 1e6);
    check_sorted_i32(data.data(), n, "int32");
    if (data != expected) {
      fprintf(stderr, "[int32] FAIL: result differs from std::sort\n");
      ++g_fail;
    }
  }

  // --- int64 ---------------------------------------------------------
  {
    std::vector<int64_t> data(n);
    for (size_t i = 0; i < n; ++i)
      data[i] = dist64(rng);
    std::vector<int64_t> expected(data);
    std::sort(expected.begin(), expected.end());

    std::vector<int64_t> warm(4096);
    for (size_t i = 0; i < warm.size(); ++i)
      warm[i] = dist64(rng);
    pgaccel_sort_i64(warm.data(), warm.size());

    auto t0 = clk::now();
    pgaccel_status st = pgaccel_sort_i64(data.data(), n);
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();
    printf("int64  sort status=%d time=%.2f ms  (%.1f M/s)\n", (int)st, ms,
           n / (ms / 1000.0) / 1e6);
    check_sorted_i64(data.data(), n, "int64");
    if (data != expected) {
      fprintf(stderr, "[int64] FAIL: result differs from std::sort\n");
      ++g_fail;
    }
  }

  // --- int32 key-value -----------------------------------------------
  {
    std::vector<int32_t> keys(n);
    std::vector<uint32_t> indices(n);
    for (size_t i = 0; i < n; ++i) {
      keys[i] = dist32(rng);
      indices[i] = static_cast<uint32_t>(i);
    }
    std::vector<int32_t> original(keys);

    auto t0 = clk::now();
    pgaccel_status st = pgaccel_sort_kv_i32(keys.data(), indices.data(), n);
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();
    printf("int32  kv-sort status=%d time=%.2f ms  (%.1f M/s)\n", (int)st, ms,
           n / (ms / 1000.0) / 1e6);
    check_kv_sorted_i32(keys.data(), indices.data(), original.data(), n, "int32-kv");
  }

  // --- int64 key-value -----------------------------------------------
  {
    std::vector<int64_t> keys(n);
    std::vector<uint32_t> indices(n);
    for (size_t i = 0; i < n; ++i) {
      keys[i] = dist64(rng);
      indices[i] = static_cast<uint32_t>(i);
    }

    auto t0 = clk::now();
    pgaccel_status st = pgaccel_sort_kv_i64(keys.data(), indices.data(), n);
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();
    printf("int64  kv-sort status=%d time=%.2f ms  (%.1f M/s)\n", (int)st, ms,
           n / (ms / 1000.0) / 1e6);
    for (size_t i = 1; i < n; ++i) {
      if (keys[i] < keys[i - 1]) {
        fprintf(stderr, "[int64-kv] FAIL at i=%zu\n", i);
        ++g_fail;
        break;
      }
    }
  }

  printf("\n== Host baseline (std::sort) ==\n");
  {
    std::vector<int32_t> data(n);
    for (size_t i = 0; i < n; ++i)
      data[i] = dist32(rng);
    auto t0 = clk::now();
    std::sort(data.begin(), data.end());
    auto t1 = clk::now();
    printf("std::sort int32 n=%zu  %.2f ms\n", n, dur_ms(t1 - t0).count());
  }
  {
    std::vector<int64_t> data(n);
    for (size_t i = 0; i < n; ++i)
      data[i] = dist64(rng);
    auto t0 = clk::now();
    std::sort(data.begin(), data.end());
    auto t1 = clk::now();
    printf("std::sort int64 n=%zu  %.2f ms\n", n, dur_ms(t1 - t0).count());
  }

  pgaccel_shutdown();

  if (g_fail) {
    fprintf(stderr, "\n== %d FAILURES ==\n", g_fail);
    return 1;
  }
  printf("\n== ALL PASSED ==\n");
  return 0;
}

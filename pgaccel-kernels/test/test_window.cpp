// Standalone correctness tests for the window-function SYCL kernels in
// pgaccel-kernels/src/window.cpp. Closes the kernel-level coverage gap
// for `pgaccel_window_row_number`, `pgaccel_window_count`,
// `pgaccel_window_sum`, `pgaccel_window_rank`, `pgaccel_window_dense_rank`
// — none of these had any standalone test coverage before this file.
//
// Each kernel gates on `count >= GPU_WINDOW_THRESHOLD == 65536` and
// returns PGACCEL_ERROR_NO_DEVICE below that. On Metal, the legacy
// non-segmented count/sum/rank kernels also return NO_DEVICE at the
// threshold because one large partition can trip the command-buffer
// interactivity watchdog; production planning declines that path until
// segmented prefix scans replace it.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_window.h"

static int g_pass = 0;
static int g_fail = 0;

static bool is_metal_backend() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") == 0;
}

#define ASSERT_STATUS_OK(desc, status)                                                \
  do {                                                                                \
    if ((status) == PGACCEL_OK) {                                                     \
      g_pass++;                                                                       \
    } else {                                                                          \
      fprintf(stderr, "FAIL: %s — status %d (expected OK)\n", (desc), (int)(status)); \
      g_fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define ASSERT_EQ_I64(desc, actual, expected)                                              \
  do {                                                                                     \
    if ((actual) == (expected)) {                                                          \
      g_pass++;                                                                            \
    } else {                                                                               \
      fprintf(stderr, "FAIL: %s — got %lld, expected %lld\n", (desc), (long long)(actual), \
              (long long)(expected));                                                      \
      g_fail++;                                                                            \
    }                                                                                      \
  } while (0)

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
  } while (0)

// ---------------------------------------------------------------------------
// Test: window_row_number
// ---------------------------------------------------------------------------
//
// partition_starts[i] is 1 iff row i is the first row of a new partition;
// row_number = (i - prev_start) + 1.
//
// Layout: 65536 rows, two partitions of 32768 each. Each partition's
// row_number sequence runs 1..32768.
static void test_window_row_number() {
  printf("--- test_window_row_number ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  starts[N / 2] = 1;

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_row_number(starts.data(), N, results.data());
  ASSERT_STATUS_OK("window_row_number 65536 status", s);

  bool ok = true;
  for (size_t i = 0; i < N / 2; i++) {
    if (results[i] != static_cast<int64_t>(i + 1)) {
      fprintf(stderr, "  partition 0 row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)(i + 1));
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_row_number partition 0 sequence 1..32768", ok);

  ok = true;
  for (size_t i = 0; i < N / 2; i++) {
    int64_t expect = static_cast<int64_t>(i + 1);
    if (results[N / 2 + i] != expect) {
      fprintf(stderr, "  partition 1 row %zu got %lld, expected %lld\n", i,
              (long long)results[N / 2 + i], (long long)expect);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_row_number partition 1 sequence 1..32768", ok);
}

// ---------------------------------------------------------------------------
// Test: window_count
// ---------------------------------------------------------------------------
//
// Cumulative count of non-null values within partition. With null_mask
// pattern null=1 every other row, count should advance only on non-null.
static void test_window_count() {
  printf("--- test_window_count ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;

  std::vector<uint8_t> nulls(N, 0);
  for (size_t i = 0; i < N; i++)
    nulls[i] = static_cast<uint8_t>(i & 1);

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_count(starts.data(), nulls.data(), N, results.data());
  if (is_metal_backend()) {
    ASSERT_TRUE("window_count Metal non-segmented path returns NO_DEVICE",
                s == PGACCEL_ERROR_NO_DEVICE);
    return;
  }
  ASSERT_STATUS_OK("window_count 65536 status", s);

  // Cumulative-count semantics (Postgres): count[i] = number of non-null
  // values in [partition_start..=i]. With null=1 every odd i, the count
  // increments only at even i's, but reads the same at the next odd i.
  //   i=0 (not null): count = 1
  //   i=1 (null):     count = 1 (carries forward)
  //   i=2 (not null): count = 2
  //   i=3 (null):     count = 2
  //   ...
  // → count[i] = (i / 2) + 1 with integer division.
  bool ok = true;
  for (size_t i = 0; i < N; i++) {
    int64_t expect = static_cast<int64_t>((i / 2) + 1);
    if (results[i] != expect) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)expect);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_count alternating-null cumulative count", ok);

  for (size_t i = 0; i < N; i++)
    nulls[i] = 0;
  s = pgaccel_window_count(starts.data(), nulls.data(), N, results.data());
  ASSERT_STATUS_OK("window_count all-non-null status", s);
  ASSERT_EQ_I64("window_count last row counts every row", results[N - 1], static_cast<int64_t>(N));
}

// ---------------------------------------------------------------------------
// Test: window_sum
// ---------------------------------------------------------------------------
//
// Cumulative sum within partition. Inputs all 1.0 → results are 1, 2, 3,...
static void test_window_sum() {
  printf("--- test_window_sum ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  starts[N / 2] = 1;

  std::vector<double> values(N, 1.0);
  std::vector<double> results(N, -1.0);

  pgaccel_status s = pgaccel_window_sum(starts.data(), values.data(), nullptr, N, results.data());
  if (is_metal_backend()) {
    ASSERT_TRUE("window_sum Metal non-segmented path returns NO_DEVICE",
                s == PGACCEL_ERROR_NO_DEVICE);
    return;
  }
  ASSERT_STATUS_OK("window_sum 65536 status", s);

  ASSERT_TRUE("window_sum partition 0 last == 32768.0", results[N / 2 - 1] == 32768.0);
  ASSERT_TRUE("window_sum partition 1 first == 1.0", results[N / 2] == 1.0);
  ASSERT_TRUE("window_sum partition 1 last == 32768.0", results[N - 1] == 32768.0);
}

// ---------------------------------------------------------------------------
// Test: window_rank
// ---------------------------------------------------------------------------
//
// `rank` = 1 + number of strictly-less-than rows in partition.
// Layout: single partition, sort_keys = floor(i/4) so every 4-row group
// shares a key. Expected ranks: 1, 1, 1, 1, 5, 5, 5, 5, 9, 9, 9, 9, ...
static void test_window_rank() {
  printf("--- test_window_rank ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;

  std::vector<double> sort_keys(N);
  for (size_t i = 0; i < N; i++)
    sort_keys[i] = static_cast<double>(i / 4);

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_rank(starts.data(), sort_keys.data(), N, results.data());
  if (is_metal_backend()) {
    ASSERT_TRUE("window_rank Metal non-segmented path returns NO_DEVICE",
                s == PGACCEL_ERROR_NO_DEVICE);
    return;
  }
  ASSERT_STATUS_OK("window_rank 65536 status", s);

  bool ok = true;
  for (size_t i = 0; i < N; i++) {
    int64_t expect = static_cast<int64_t>((i / 4) * 4 + 1);
    if (results[i] != expect) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)expect);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_rank tied-by-4 sequence 1,1,1,1,5,5,5,5,...", ok);
}

// ---------------------------------------------------------------------------
// Test: window_dense_rank
// ---------------------------------------------------------------------------
//
// Same layout as window_rank but no gap on ties: 1, 1, 1, 1, 2, 2, 2, 2,...
static void test_window_dense_rank() {
  printf("--- test_window_dense_rank ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;

  std::vector<double> sort_keys(N);
  for (size_t i = 0; i < N; i++)
    sort_keys[i] = static_cast<double>(i / 4);

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_dense_rank(starts.data(), sort_keys.data(), N, results.data());
  if (is_metal_backend()) {
    ASSERT_TRUE("window_dense_rank Metal non-segmented path returns NO_DEVICE",
                s == PGACCEL_ERROR_NO_DEVICE);
    return;
  }
  ASSERT_STATUS_OK("window_dense_rank 65536 status", s);

  bool ok = true;
  for (size_t i = 0; i < N; i++) {
    int64_t expect = static_cast<int64_t>((i / 4) + 1);
    if (results[i] != expect) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)expect);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_dense_rank tied-by-4 sequence 1,1,1,1,2,2,2,2,...", ok);
}

// ---------------------------------------------------------------------------
// Test: below-threshold gating
// ---------------------------------------------------------------------------
//
// All 5 kernels return PGACCEL_ERROR_NO_DEVICE for count < 65536; the
// dispatcher uses that to fall back to the host implementation. Confirm
// the gate fires.
static void test_below_threshold_gates() {
  printf("--- test_below_threshold_gates ---\n");

  constexpr size_t N = 1024;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  std::vector<int64_t> i64_results(N, 0);
  std::vector<double> d_results(N, 0.0);
  std::vector<double> d_values(N, 1.0);

  pgaccel_status s;
  s = pgaccel_window_row_number(starts.data(), N, i64_results.data());
  ASSERT_TRUE("row_number N=1024 returns NO_DEVICE", s == PGACCEL_ERROR_NO_DEVICE);

  s = pgaccel_window_count(starts.data(), nullptr, N, i64_results.data());
  ASSERT_TRUE("count N=1024 returns NO_DEVICE", s == PGACCEL_ERROR_NO_DEVICE);

  s = pgaccel_window_sum(starts.data(), d_values.data(), nullptr, N, d_results.data());
  ASSERT_TRUE("sum N=1024 returns NO_DEVICE", s == PGACCEL_ERROR_NO_DEVICE);

  s = pgaccel_window_rank(starts.data(), d_values.data(), N, i64_results.data());
  ASSERT_TRUE("rank N=1024 returns NO_DEVICE", s == PGACCEL_ERROR_NO_DEVICE);

  s = pgaccel_window_dense_rank(starts.data(), d_values.data(), N, i64_results.data());
  ASSERT_TRUE("dense_rank N=1024 returns NO_DEVICE", s == PGACCEL_ERROR_NO_DEVICE);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel window kernel tests ===\n\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run window tests\n");
    return 1;
  }

  test_below_threshold_gates();
  test_window_row_number();
  test_window_count();
  test_window_sum();
  test_window_rank();
  test_window_dense_rank();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

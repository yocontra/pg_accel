// Standalone correctness tests for the window-function SYCL kernels.
//
// Each kernel gates on `count >= GPU_WINDOW_THRESHOLD == 65536` and
// returns PGACCEL_UNSUPPORTED below that. At and above the threshold every
// supported backend, including Metal, dispatches device boundary discovery
// followed by a parallel or segmented device result pass.
//
// Status-honesty note (2026-07 kernel-safety pass): these decline gates
// used to return PGACCEL_ERROR_NO_DEVICE, conflating "planner should not
// use this path" with "the machine has no GPU". The gating behavior under
// test is identical; only the status code was corrected. NO_DEVICE is now
// reserved for an actually-missing device, PGACCEL_ERROR for a kernel that
// failed, PGACCEL_UNSUPPORTED for policy declines like these.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_window.h"

static int g_pass = 0;
static int g_fail = 0;

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
// Includes an implicit first partition (starts[0] == 0), adjacent singleton
// partitions, ordinary partitions, and a singleton final partition.
static void test_window_row_number() {
  printf("--- test_window_row_number ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[1] = 1;
  starts[2] = 1;
  starts[17] = 1;
  starts[18] = 1;
  starts[N / 2] = 1;
  starts[N - 1] = 1;

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_row_number(starts.data(), N, results.data());
  ASSERT_STATUS_OK("window_row_number 65536 status", s);

  bool ok = true;
  size_t partition_start = 0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0)
      partition_start = i;
    const int64_t expected = static_cast<int64_t>(i - partition_start + 1);
    if (results[i] != expected) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)expected);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_row_number honors implicit, adjacent, and final partitions", ok);
}

// ---------------------------------------------------------------------------
// Test: window_count
// ---------------------------------------------------------------------------
//
// Cumulative count of non-null values over singleton, all-null, and ordinary
// partitions. A second dispatch covers COUNT(*) with a null mask omitted.
static void test_window_count() {
  printf("--- test_window_count ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  starts[1] = 1;
  starts[9] = 1;
  starts[21] = 1;
  starts[N / 3] = 1;
  starts[N / 3 + 64] = 1;
  starts[N - 1] = 1;

  std::vector<uint8_t> nulls(N, 0);
  for (size_t i = 0; i < N; ++i)
    nulls[i] = static_cast<uint8_t>(i & 1);
  for (size_t i = 9; i < 21; ++i)
    nulls[i] = 1;

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_count(starts.data(), nulls.data(), N, results.data());
  ASSERT_STATUS_OK("window_count 65536 status", s);

  bool ok = true;
  int64_t running = 0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0)
      running = 0;
    if (nulls[i] == 0)
      ++running;
    if (results[i] != running) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)running);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_count segmented mixed/all-null cumulative count", ok);
  ASSERT_EQ_I64("window_count all-null partition stays zero", results[20], 0);

  s = pgaccel_window_count(starts.data(), nullptr, N, results.data());
  ASSERT_STATUS_OK("window_count COUNT(*) status", s);
  ok = true;
  size_t partition_start = 0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0)
      partition_start = i;
    const int64_t expected = static_cast<int64_t>(i - partition_start + 1);
    if (results[i] != expected) {
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_count COUNT(*) resets at every device boundary", ok);
}

// ---------------------------------------------------------------------------
// Test: window_sum
// ---------------------------------------------------------------------------
//
// Cumulative Kahan sum over ordinary, singleton, and all-null partitions.
static void test_window_sum() {
  printf("--- test_window_sum ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  starts[1] = 1;
  starts[7] = 1;
  starts[15] = 1;
  starts[N / 2] = 1;
  starts[N - 1] = 1;

  std::vector<double> values(N);
  std::vector<uint8_t> nulls(N, 0);
  for (size_t i = 0; i < N; ++i) {
    values[i] = static_cast<double>(static_cast<int>(i % 11) - 5) * 0.25;
    nulls[i] = static_cast<uint8_t>((i % 13) == 0);
  }
  for (size_t i = 7; i < 15; ++i)
    nulls[i] = 1;
  std::vector<double> results(N, -1.0);

  pgaccel_status s =
      pgaccel_window_sum(starts.data(), values.data(), nulls.data(), N, results.data());
  ASSERT_STATUS_OK("window_sum 65536 status", s);

  bool ok = true;
  double running_sum = 0.0;
  double compensation = 0.0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0) {
      running_sum = 0.0;
      compensation = 0.0;
    }
    if (nulls[i] == 0) {
      const double adjusted = values[i] - compensation;
      const double next = running_sum + adjusted;
      compensation = (next - running_sum) - adjusted;
      running_sum = next;
    }
    if (results[i] != running_sum) {
      fprintf(stderr, "  row %zu got %.17g, expected %.17g\n", i, results[i], running_sum);
      ok = false;
      break;
    }
  }
  ASSERT_TRUE("window_sum segmented values match Kahan reference", ok);
  ASSERT_TRUE("window_sum all-null partition stays zero", results[14] == 0.0);
  ASSERT_TRUE("window_sum singleton final partition resets", results[N - 1] == values[N - 1]);
}

// ---------------------------------------------------------------------------
// Test: window_rank
// ---------------------------------------------------------------------------
//
static bool peer_equal(double left, double right) {
  const bool both_nan = left != left && right != right;
  return both_nan || left == right;
}

static void make_rank_case(std::vector<uint8_t>& starts, std::vector<double>& keys) {
  const size_t count = starts.size();
  starts[0] = 1;
  starts[1] = 1;
  starts[2] = 1;
  starts[257] = 1;
  starts[count / 2] = 1;
  starts[count - 1] = 1;

  size_t partition_start = 0;
  for (size_t i = 0; i < count; ++i) {
    if (starts[i] != 0)
      partition_start = i;
    keys[i] = static_cast<double>((i - partition_start) / 4);
  }

  const double nan = std::numeric_limits<double>::quiet_NaN();
  keys[0] = nan;
  keys[1] = nan;
  for (size_t i = 253; i < 257; ++i)
    keys[i] = nan;
  for (size_t i = count / 2 - 4; i < count / 2; ++i)
    keys[i] = nan;
  for (size_t i = count - 5; i < count - 1; ++i)
    keys[i] = nan;
  keys[count - 1] = nan;
}

// Multi-partition RANK with adjacent singletons, ties, and NaN peer groups.
static void test_window_rank() {
  printf("--- test_window_rank ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  std::vector<double> sort_keys(N, 0.0);
  make_rank_case(starts, sort_keys);

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_rank(starts.data(), sort_keys.data(), N, results.data());
  ASSERT_STATUS_OK("window_rank 65536 status", s);

  bool ok = true;
  int64_t row_number = 0;
  int64_t rank = 1;
  bool first = true;
  double previous = 0.0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0) {
      row_number = 0;
      rank = 1;
      first = true;
    }
    ++row_number;
    if (first) {
      first = false;
    } else if (!peer_equal(previous, sort_keys[i])) {
      rank = row_number;
    }
    if (results[i] != rank) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)rank);
      ok = false;
      break;
    }
    previous = sort_keys[i];
  }
  ASSERT_TRUE("window_rank segmented ties and NaN peers", ok);
}

// ---------------------------------------------------------------------------
// Test: window_dense_rank
// ---------------------------------------------------------------------------
//
// Same adversarial layout as RANK, with no gaps between peer groups.
static void test_window_dense_rank() {
  printf("--- test_window_dense_rank ---\n");

  constexpr size_t N = 65536;
  std::vector<uint8_t> starts(N, 0);
  std::vector<double> sort_keys(N, 0.0);
  make_rank_case(starts, sort_keys);

  std::vector<int64_t> results(N, -1);
  pgaccel_status s = pgaccel_window_dense_rank(starts.data(), sort_keys.data(), N, results.data());
  ASSERT_STATUS_OK("window_dense_rank 65536 status", s);

  bool ok = true;
  int64_t dense_rank = 0;
  bool first = true;
  double previous = 0.0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0) {
      dense_rank = 0;
      first = true;
    }
    if (first || !peer_equal(previous, sort_keys[i])) {
      ++dense_rank;
      first = false;
    }
    if (results[i] != dense_rank) {
      fprintf(stderr, "  row %zu got %lld, expected %lld\n", i, (long long)results[i],
              (long long)dense_rank);
      ok = false;
      break;
    }
    previous = sort_keys[i];
  }
  ASSERT_TRUE("window_dense_rank segmented ties and NaN peers", ok);
}

static void make_offset_case(std::vector<uint8_t>& starts, std::vector<double>& values,
                             std::vector<uint8_t>& nulls) {
  const size_t count = starts.size();
  starts[0] = 1;
  starts[1] = 1;
  starts[4] = 1;
  starts[19] = 1;
  starts[20] = 1;
  starts[count / 2] = 1;
  starts[count - 1] = 1;
  for (size_t i = 0; i < count; ++i) {
    values[i] = static_cast<double>(i) * 0.5;
    nulls[i] = static_cast<uint8_t>((i % 17) == 0);
  }
  for (size_t i = 4; i < 19; ++i)
    nulls[i] = 1;
}

// LAG offsets 0, an in-range/cross-boundary offset, and an offset larger than
// every partition. NULL source rows preserve the output null marker while an
// out-of-partition lookup yields the non-null default.
static void test_window_lag() {
  printf("--- test_window_lag ---\n");

  constexpr size_t N = 65536;
  constexpr double DEFAULT_VALUE = -77.25;
  std::vector<uint8_t> starts(N, 0);
  std::vector<double> values(N, 0.0);
  std::vector<uint8_t> nulls(N, 0);
  make_offset_case(starts, values, nulls);

  std::vector<size_t> partition_start(N, 0);
  size_t start = 0;
  for (size_t i = 0; i < N; ++i) {
    if (starts[i] != 0)
      start = i;
    partition_start[i] = start;
  }

  std::vector<double> results(N, 0.0);
  std::vector<uint8_t> result_nulls(N, 0xff);
  for (const int offset : {0, 3, static_cast<int>(N)}) {
    pgaccel_status status =
        pgaccel_window_lag(starts.data(), values.data(), nulls.data(), N, offset, DEFAULT_VALUE,
                           results.data(), result_nulls.data());
    ASSERT_STATUS_OK("window_lag adversarial offset status", status);

    bool ok = status == PGACCEL_OK;
    const size_t distance = static_cast<size_t>(offset);
    for (size_t i = 0; ok && i < N; ++i) {
      double expected_value = DEFAULT_VALUE;
      uint8_t expected_null = 0;
      if (distance <= i - partition_start[i]) {
        const size_t target = i - distance;
        if (nulls[target] != 0) {
          expected_null = 1;
        } else {
          expected_value = values[target];
        }
      }
      if (results[i] != expected_value || result_nulls[i] != expected_null) {
        fprintf(stderr, "  lag offset %d row %zu got (%.17g,%u), expected (%.17g,%u)\n", offset, i,
                results[i], (unsigned)result_nulls[i], expected_value, (unsigned)expected_null);
        ok = false;
      }
    }
    ASSERT_TRUE("window_lag values/defaults/nulls stay within partitions", ok);
  }
}

static void test_window_lead() {
  printf("--- test_window_lead ---\n");

  constexpr size_t N = 65536;
  constexpr double DEFAULT_VALUE = 91.5;
  std::vector<uint8_t> starts(N, 0);
  std::vector<double> values(N, 0.0);
  std::vector<uint8_t> nulls(N, 0);
  make_offset_case(starts, values, nulls);

  std::vector<size_t> partition_end(N, N - 1);
  size_t end = N - 1;
  for (size_t remaining = N; remaining > 0; --remaining) {
    const size_t i = remaining - 1;
    if (i + 1 < N && starts[i + 1] != 0)
      end = i;
    partition_end[i] = end;
  }

  std::vector<double> results(N, 0.0);
  std::vector<uint8_t> result_nulls(N, 0xff);
  for (const int offset : {0, 3, static_cast<int>(N)}) {
    pgaccel_status status =
        pgaccel_window_lead(starts.data(), values.data(), nulls.data(), N, offset, DEFAULT_VALUE,
                            results.data(), result_nulls.data());
    ASSERT_STATUS_OK("window_lead adversarial offset status", status);

    bool ok = status == PGACCEL_OK;
    const size_t distance = static_cast<size_t>(offset);
    for (size_t i = 0; ok && i < N; ++i) {
      double expected_value = DEFAULT_VALUE;
      uint8_t expected_null = 0;
      if (distance <= partition_end[i] - i) {
        const size_t target = i + distance;
        if (nulls[target] != 0) {
          expected_null = 1;
        } else {
          expected_value = values[target];
        }
      }
      if (results[i] != expected_value || result_nulls[i] != expected_null) {
        fprintf(stderr, "  lead offset %d row %zu got (%.17g,%u), expected (%.17g,%u)\n", offset, i,
                results[i], (unsigned)result_nulls[i], expected_value, (unsigned)expected_null);
        ok = false;
      }
    }
    ASSERT_TRUE("window_lead values/defaults/nulls stay within partitions", ok);
  }
}

// ---------------------------------------------------------------------------
// Test: below-threshold gating
// ---------------------------------------------------------------------------
//
// All seven kernels return PGACCEL_UNSUPPORTED for count < 65536; the
// dispatcher uses that to fall back to the host implementation. Confirm
// the gate fires (and reports itself as a decline, not a missing device).
static void test_below_threshold_gates() {
  printf("--- test_below_threshold_gates ---\n");

  constexpr size_t N = 1024;
  std::vector<uint8_t> starts(N, 0);
  starts[0] = 1;
  std::vector<int64_t> i64_results(N, 0);
  std::vector<double> d_results(N, 0.0);
  std::vector<double> d_values(N, 1.0);
  std::vector<uint8_t> result_nulls(N, 0);

  pgaccel_status s;
  s = pgaccel_window_row_number(starts.data(), N, i64_results.data());
  ASSERT_TRUE("row_number N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_count(starts.data(), nullptr, N, i64_results.data());
  ASSERT_TRUE("count N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_sum(starts.data(), d_values.data(), nullptr, N, d_results.data());
  ASSERT_TRUE("sum N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_rank(starts.data(), d_values.data(), N, i64_results.data());
  ASSERT_TRUE("rank N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_dense_rank(starts.data(), d_values.data(), N, i64_results.data());
  ASSERT_TRUE("dense_rank N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_lag(starts.data(), d_values.data(), nullptr, N, 1, 0.0, d_results.data(),
                         result_nulls.data());
  ASSERT_TRUE("lag N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);

  s = pgaccel_window_lead(starts.data(), d_values.data(), nullptr, N, 1, 0.0, d_results.data(),
                          result_nulls.data());
  ASSERT_TRUE("lead N=1024 declines with UNSUPPORTED", s == PGACCEL_UNSUPPORTED);
}

static void test_empty_identities() {
  printf("--- test_empty_identities ---\n");

  uint8_t starts = 1;
  uint8_t null_mask = 0;
  uint8_t result_null = 0x7f;
  double value = 4.5;
  double double_result = 123.0;
  int64_t integer_result = 456;

  ASSERT_STATUS_OK("empty row_number", pgaccel_window_row_number(&starts, 0, &integer_result));
  ASSERT_STATUS_OK("empty rank", pgaccel_window_rank(&starts, &value, 0, &integer_result));
  ASSERT_STATUS_OK("empty dense_rank",
                   pgaccel_window_dense_rank(&starts, &value, 0, &integer_result));
  ASSERT_STATUS_OK("empty sum", pgaccel_window_sum(&starts, &value, &null_mask, 0, &double_result));
  ASSERT_STATUS_OK("empty count", pgaccel_window_count(&starts, &null_mask, 0, &integer_result));
  ASSERT_STATUS_OK("empty lag", pgaccel_window_lag(&starts, &value, &null_mask, 0, 1, -1.0,
                                                   &double_result, &result_null));
  ASSERT_STATUS_OK("empty lead", pgaccel_window_lead(&starts, &value, &null_mask, 0, 1, -1.0,
                                                     &double_result, &result_null));
  ASSERT_EQ_I64("empty integer output unchanged", integer_result, 456);
  ASSERT_TRUE("empty double output unchanged", double_result == 123.0);
  ASSERT_TRUE("empty null output unchanged", result_null == 0x7f);
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
  test_empty_identities();
  test_window_row_number();
  test_window_count();
  test_window_sum();
  test_window_rank();
  test_window_dense_rank();
  test_window_lag();
  test_window_lead();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

// Standalone correctness tests for the **partial-mode** GPU hash-agg
// path (Phase 3B). For each (func, value-type) pair we drive
// pgaccel_hash_agg_execute_partial against a synthetic batch and check
// that the per-group transition state matches the reference value
// computed on the host:
//
//   PGACCEL_AGG_SUM    -> single f64 per group, == Σ values
//   PGACCEL_AGG_COUNT  -> single f64 per group, == row count (or
//                         non-null count for COUNT(col))
//   PGACCEL_AGG_MIN    -> single f64 per group, == min values
//   PGACCEL_AGG_MAX    -> single f64 per group, == max values
//   PGACCEL_AGG_AVG    -> [N, sum] per group; matches PG's
//                         float8_avg_accum transtype shape
//   PGACCEL_AGG_STDDEV -> [N, sum, sum_sq] per group; sum_sq is Σx²,
//                         host converts to Sxx = sum_sq - sum²/N at
//                         emit time (matches Float8StatsEmitter)
//
// Supported backends exercise the GPU sort/group path at multiple row counts.
// Metal is quarantined and must return PGACCEL_UNSUPPORTED before dispatch.
//
// Per CLAUDE.md anti-cheat ban #1, every assertion runs against a
// real kernel dispatch and the comparison is exact (or 1e-6 relative
// for f64 arithmetic) — no early-out on group_count == 0, no
// loosened tolerances.

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"
#include "pgaccel_hash_join.h"

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
  } while (0)

#define ASSERT_EQ_SZ(desc, actual, expected)                                          \
  do {                                                                                \
    if ((actual) == (expected)) {                                                     \
      g_pass++;                                                                       \
    } else {                                                                          \
      fprintf(stderr, "FAIL: %s — got %zu, expected %zu\n", (desc), (size_t)(actual), \
              (size_t)(expected));                                                    \
      g_fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define ASSERT_NEAR(desc, actual, expected, tol)                                             \
  do {                                                                                       \
    double a = (actual);                                                                     \
    double e = (expected);                                                                   \
    double d = std::abs(a - e);                                                              \
    double m = std::abs(e) > 1.0 ? std::abs(e) : 1.0;                                        \
    if (d <= (tol) * m) {                                                                    \
      g_pass++;                                                                              \
    } else {                                                                                 \
      fprintf(stderr, "FAIL: %s — got %.6g, expected %.6g (delta %.3e)\n", (desc), a, e, d); \
      g_fail++;                                                                              \
    }                                                                                        \
  } while (0)

// ---------------------------------------------------------------------------
// Helper: build per-group reference (sum, sum_sq, count, min, max) over
// the input row stream.
// ---------------------------------------------------------------------------
struct GroupRef {
  double sum;
  double sum_sq;
  double min_v;
  double max_v;
  int64_t non_null_count;
  int64_t total_rows;
};

static bool is_metal_backend() {
  return std::strcmp(pgaccel_get_caps().backend_name, "metal") == 0;
}

static void test_partial_checked_admission() {
  printf("--- test_partial_checked_admission ---\n");

  std::vector<int64_t> keys = {1, 2, 1, 2};
  std::vector<double> values = {1.0, 2.0, 3.0, 4.0};
  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {nullptr};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_AVG, 0}};

  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_hash_agg_execute_partial_checked(
      keys.data(), nullptr, keys.size(), PGACCEL_KEY_INT64, val_arrays, val_null_arrays, val_types,
      agg_cols, 1, &state);

  if (is_metal_backend()) {
    ASSERT_TRUE("Metal partial hash_agg returns UNSUPPORTED", status == PGACCEL_UNSUPPORTED);
    ASSERT_TRUE("Metal partial hash_agg clears output state", state == nullptr);
    ASSERT_EQ_SZ("Metal partial hash_agg launches no GPU kernels", pgaccel_gpu_exec_count(),
                 (uint64_t)0);
    return;
  }

  ASSERT_TRUE("partial checked hash_agg status OK", status == PGACCEL_OK);
  ASSERT_TRUE("partial checked hash_agg state non-null", state != nullptr);
  ASSERT_TRUE("partial checked hash_agg launched GPU kernels", pgaccel_gpu_exec_count() > 0);
  if (state != nullptr)
    pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// SUM(int64) on float-typed value column — small-N agg_hash_partial path.
// ---------------------------------------------------------------------------
static void test_partial_sum_int64_small_n() {
  printf("--- test_partial_sum_int64_small_n ---\n");

  constexpr size_t ROWS_PER_GROUP = 250;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;  // 1000 — well below SORT_AGG_MIN_ROWS

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  GroupRef refs[NUM_GROUPS] = {{0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0}};

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid * 100 + 7);
    values[i] = static_cast<double>(i + 1);
    refs[gid].sum += values[i];
    refs[gid].sum_sq += values[i] * values[i];
    refs[gid].non_null_count += 1;
    refs[gid].total_rows += 1;
    if (values[i] < refs[gid].min_v)
      refs[gid].min_v = values[i];
    if (values[i] > refs[gid].max_v)
      refs[gid].max_v = values[i];
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("partial SUM small-N state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("partial SUM small-N group_count == 4", pgaccel_agg_group_count(state), NUM_GROUPS);
  ASSERT_EQ_SZ("partial SUM lane width == 1", pgaccel_agg_get_partial_width(state, 0), (size_t)1);

  const double* parts = pgaccel_agg_get_partial_results(state, 0);
  ASSERT_TRUE("partial SUM small-N partial buffer non-null", parts != nullptr);
  if (!parts) {
    pgaccel_agg_free(state);
    return;
  }

  // Match each output group to its expected sum by key.
  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    int64_t k = keys_out[g];
    size_t expected_gid = (k - 7) / 100;
    if (expected_gid < NUM_GROUPS) {
      ASSERT_NEAR("partial SUM small-N per-group sum", parts[g], refs[expected_gid].sum, 1e-9);
    }
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// AVG(float8) — large-N path — verify [N, sum] per group.
// ---------------------------------------------------------------------------
static void test_partial_avg_large_n() {
  printf("--- test_partial_avg_large_n ---\n");

  constexpr size_t ROWS_PER_GROUP = 30000;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;  // 120k — large-N dispatch path

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  GroupRef refs[NUM_GROUPS] = {{0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0}};

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid * 100 + 7);
    values[i] = 1.0 + 0.5 * static_cast<double>(i % 17);
    refs[gid].sum += values[i];
    refs[gid].non_null_count += 1;
    refs[gid].total_rows += 1;
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_AVG, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("partial AVG large-N state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("partial AVG large-N group_count == 4", pgaccel_agg_group_count(state), NUM_GROUPS);
  ASSERT_EQ_SZ("partial AVG lane width == 2", pgaccel_agg_get_partial_width(state, 0), (size_t)2);

  const double* parts = pgaccel_agg_get_partial_results(state, 0);
  ASSERT_TRUE("partial AVG large-N partial buffer non-null", parts != nullptr);
  if (!parts) {
    pgaccel_agg_free(state);
    return;
  }

  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    int64_t k = keys_out[g];
    size_t expected_gid = (k - 7) / 100;
    if (expected_gid < NUM_GROUPS) {
      double n_lane = parts[g * 2 + 0];
      double sum_lane = parts[g * 2 + 1];
      ASSERT_NEAR("partial AVG large-N N lane", n_lane,
                  static_cast<double>(refs[expected_gid].non_null_count), 0.0);
      ASSERT_NEAR("partial AVG large-N sum lane", sum_lane, refs[expected_gid].sum, 1e-9);
    }
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// STDDEV(float8) with NULL values mixed in — small-N (hash path).
// ---------------------------------------------------------------------------
static void test_partial_stddev_with_nulls_small_n() {
  printf("--- test_partial_stddev_with_nulls_small_n ---\n");

  constexpr size_t ROWS_PER_GROUP = 250;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;  // 1000

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  GroupRef refs[NUM_GROUPS] = {{0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0},
                               {0.0, 0.0, 1e30, -1e30, 0, 0}};

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid * 100 + 7);
    values[i] = static_cast<double>(i + 1) * 0.25;
    // NULL out every 7th row to exercise the non-null-count path.
    bool is_null = (i % 7 == 0);
    val_nulls[i] = is_null ? 1u : 0u;
    refs[gid].total_rows += 1;
    if (!is_null) {
      refs[gid].sum += values[i];
      refs[gid].sum_sq += values[i] * values[i];
      refs[gid].non_null_count += 1;
    }
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_STDDEV, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("partial STDDEV w/ nulls state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("partial STDDEV w/ nulls group_count == 4", pgaccel_agg_group_count(state),
               NUM_GROUPS);
  ASSERT_EQ_SZ("partial STDDEV lane width == 3", pgaccel_agg_get_partial_width(state, 0),
               (size_t)3);

  const double* parts = pgaccel_agg_get_partial_results(state, 0);
  ASSERT_TRUE("partial STDDEV partial buffer non-null", parts != nullptr);
  if (!parts) {
    pgaccel_agg_free(state);
    return;
  }

  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    int64_t k = keys_out[g];
    size_t expected_gid = (k - 7) / 100;
    if (expected_gid < NUM_GROUPS) {
      double n_lane = parts[g * 3 + 0];
      double sum_lane = parts[g * 3 + 1];
      double sum_sq_lane = parts[g * 3 + 2];
      ASSERT_NEAR("partial STDDEV N lane (non-null count)", n_lane,
                  static_cast<double>(refs[expected_gid].non_null_count), 0.0);
      ASSERT_NEAR("partial STDDEV sum lane", sum_lane, refs[expected_gid].sum, 1e-9);
      ASSERT_NEAR("partial STDDEV sum_sq lane", sum_sq_lane, refs[expected_gid].sum_sq, 1e-9);
    }
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// MIN/MAX over int32-typed values (multi-agg dispatch).
// ---------------------------------------------------------------------------
static void test_partial_min_max_int32() {
  printf("--- test_partial_min_max_int32 ---\n");

  constexpr size_t ROWS_PER_GROUP = 200;
  constexpr size_t NUM_GROUPS = 3;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;  // 600

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<int32_t> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  GroupRef refs[NUM_GROUPS] = {
      {0.0, 0.0, 1e30, -1e30, 0, 0}, {0.0, 0.0, 1e30, -1e30, 0, 0}, {0.0, 0.0, 1e30, -1e30, 0, 0}};

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid + 1);
    values[i] = static_cast<int32_t>(i + 1);
    double v = static_cast<double>(values[i]);
    if (v < refs[gid].min_v)
      refs[gid].min_v = v;
    if (v > refs[gid].max_v)
      refs[gid].max_v = v;
  }

  const void* val_arrays[2] = {values.data(), values.data()};
  const uint8_t* val_null_arrays[2] = {val_nulls.data(), val_nulls.data()};
  int val_types[2] = {PGACCEL_VAL_INT32, PGACCEL_VAL_INT32};
  pgaccel_agg_col agg_cols[2] = {{PGACCEL_AGG_MIN, 0}, {PGACCEL_AGG_MAX, 1}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 2);
  ASSERT_TRUE("partial MIN/MAX int32 state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("partial MIN/MAX group_count == 3", pgaccel_agg_group_count(state), NUM_GROUPS);
  ASSERT_EQ_SZ("partial MIN lane width == 1", pgaccel_agg_get_partial_width(state, 0), (size_t)1);
  ASSERT_EQ_SZ("partial MAX lane width == 1", pgaccel_agg_get_partial_width(state, 1), (size_t)1);

  const double* min_parts = pgaccel_agg_get_partial_results(state, 0);
  const double* max_parts = pgaccel_agg_get_partial_results(state, 1);
  ASSERT_TRUE("partial MIN buffer non-null", min_parts != nullptr);
  ASSERT_TRUE("partial MAX buffer non-null", max_parts != nullptr);
  if (!min_parts || !max_parts) {
    pgaccel_agg_free(state);
    return;
  }

  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    int64_t k = keys_out[g];
    size_t expected_gid = static_cast<size_t>(k) - 1;
    if (expected_gid < NUM_GROUPS) {
      ASSERT_NEAR("partial MIN per-group min", min_parts[g], refs[expected_gid].min_v, 0.0);
      ASSERT_NEAR("partial MAX per-group max", max_parts[g], refs[expected_gid].max_v, 0.0);
    }
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// COUNT(*) — width=1, non-null counts == row counts.
// ---------------------------------------------------------------------------
static void test_partial_count_star() {
  printf("--- test_partial_count_star ---\n");

  constexpr size_t ROWS_PER_GROUP = 137;
  constexpr size_t NUM_GROUPS = 5;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  // Dummy value column (kernel ignores when col_idx == SIZE_MAX for COUNT(*)).
  std::vector<double> values(N, 0.0);
  std::vector<uint8_t> val_nulls(N, 0);

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid * 17 + 1);
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_COUNT, SIZE_MAX}};  // COUNT(*)

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("partial COUNT(*) state non-null", state != nullptr);
  if (!state) {
    return;
  }
  ASSERT_EQ_SZ("partial COUNT(*) group_count == 5", pgaccel_agg_group_count(state), NUM_GROUPS);
  ASSERT_EQ_SZ("partial COUNT lane width == 1", pgaccel_agg_get_partial_width(state, 0), (size_t)1);

  const double* parts = pgaccel_agg_get_partial_results(state, 0);
  ASSERT_TRUE("partial COUNT(*) buffer non-null", parts != nullptr);
  if (!parts) {
    pgaccel_agg_free(state);
    return;
  }

  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    ASSERT_NEAR("partial COUNT(*) per-group count", parts[g], static_cast<double>(ROWS_PER_GROUP),
                0.0);
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Bool-typed AVG path (PGACCEL_VAL_BOOL → 0.0/1.0). Verifies the type
// dispatch in device_read_value_flat reaches the bool branch.
// ---------------------------------------------------------------------------
static void test_partial_avg_bool() {
  printf("--- test_partial_avg_bool ---\n");

  constexpr size_t ROWS_PER_GROUP = 1000;
  constexpr size_t NUM_GROUPS = 2;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;

  std::vector<int32_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<bool> values(N);  // bool column
  std::vector<uint8_t> val_nulls(N, 0);
  double sum_per_group[NUM_GROUPS] = {0.0, 0.0};
  int64_t count_per_group[NUM_GROUPS] = {0, 0};

  for (size_t i = 0; i < N; ++i) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int32_t>(gid + 1);
    values[i] = (i % 3 != 0);  // 2/3 true, 1/3 false
    sum_per_group[gid] += values[i] ? 1.0 : 0.0;
    count_per_group[gid] += 1;
  }

  // std::vector<bool> is bitpacked, so build a raw bool buffer for the kernel.
  std::vector<uint8_t> raw_bools(N);
  for (size_t i = 0; i < N; ++i)
    raw_bools[i] = values[i] ? 1u : 0u;

  const void* val_arrays[1] = {raw_bools.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_BOOL};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_AVG, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute_partial(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT32,
                                       val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("partial AVG bool state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("partial AVG bool group_count == 2", pgaccel_agg_group_count(state), NUM_GROUPS);

  const double* parts = pgaccel_agg_get_partial_results(state, 0);
  ASSERT_TRUE("partial AVG bool buffer non-null", parts != nullptr);
  if (!parts) {
    pgaccel_agg_free(state);
    return;
  }

  const auto* keys_out = static_cast<const int32_t*>(pgaccel_agg_get_group_keys(state));
  for (size_t g = 0; g < NUM_GROUPS; ++g) {
    int32_t k = keys_out[g];
    size_t expected_gid = static_cast<size_t>(k) - 1;
    if (expected_gid < NUM_GROUPS) {
      double n_lane = parts[g * 2 + 0];
      double sum_lane = parts[g * 2 + 1];
      ASSERT_NEAR("partial AVG bool N lane", n_lane,
                  static_cast<double>(count_per_group[expected_gid]), 0.0);
      ASSERT_NEAR("partial AVG bool sum lane", sum_lane, sum_per_group[expected_gid], 1e-9);
    }
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel hash_agg PARTIAL-MODE correctness matrix (Phase 3B) ===\n\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run partial-mode tests\n");
    return 1;
  }

  test_partial_checked_admission();
  if (is_metal_backend()) {
    printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
  }

  test_partial_sum_int64_small_n();
  test_partial_avg_large_n();
  test_partial_stddev_with_nulls_small_n();
  test_partial_min_max_int32();
  test_partial_count_star();
  test_partial_avg_bool();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

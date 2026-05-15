// Standalone correctness tests for hash-agg group-key paths in
// pgaccel-kernels/src/hash_agg.cpp. These cover wide UUID/INET keys,
// FLOAT64 key normalization, and NULL/sentinel collision handling.

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
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

#define ASSERT_EQ_INT(desc, actual, expected)                                                      \
  do {                                                                                             \
    if ((actual) == (expected)) {                                                                  \
      g_pass++;                                                                                    \
    } else {                                                                                       \
      fprintf(stderr, "FAIL: %s — got %d, expected %d\n", (desc), (int)(actual), (int)(expected)); \
      g_fail++;                                                                                    \
    }                                                                                              \
  } while (0)

// ---------------------------------------------------------------------------
// Test: UUID key group-by + SUM
// ---------------------------------------------------------------------------
//
// 4 distinct 16-byte UUIDs replicated 25k times each → 100k rows total.
// Per group SUM should equal the value-sum of that group's rows.
static void test_hash_agg_uuid_keys() {
  printf("--- test_hash_agg_uuid_keys ---\n");

  constexpr size_t ROWS_PER_GROUP = 25000;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;  // 100000

  // 4 distinct UUIDs (host byte order, 16 bytes each).
  uint8_t group_uuids[NUM_GROUPS][16] = {
      {0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
       0x0f},
      {0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
       0x1f},
      {0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xaa, 0xbb, 0xbb, 0xcc, 0xcc, 0xdd,
       0xdd},
      {0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde,
       0xf0},
  };

  // Build group_keys buffer: N rows × 16 bytes. Interleave so each
  // UUID's rows aren't contiguous (forces the kernel to actually
  // group, not just chunk-sum).
  std::vector<uint8_t> keys(N * 16);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  double expected_sum_per_group[NUM_GROUPS] = {0.0, 0.0, 0.0, 0.0};

  for (size_t i = 0; i < N; i++) {
    size_t gid = i % NUM_GROUPS;
    std::memcpy(keys.data() + i * 16, group_uuids[gid], 16);
    values[i] = static_cast<double>(i + 1);  // distinct values to catch any group-mixing bug
    expected_sum_per_group[gid] += values[i];
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), N, PGACCEL_KEY_UUID, val_arrays,
                               val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("UUID hash_agg returned non-null state", state != nullptr);
  if (!state) {
    return;
  }

  size_t ngroups = pgaccel_agg_group_count(state);
  ASSERT_EQ_SZ("UUID hash_agg group_count == 4", ngroups, NUM_GROUPS);

  if (ngroups == NUM_GROUPS) {
    const auto* keys_out = static_cast<const uint8_t*>(pgaccel_agg_get_group_keys(state));
    const double* sums = pgaccel_agg_get_results(state, 0);

    // Match each output group back to its expected UUID via memcmp.
    bool all_groups_matched = true;
    bool sums_correct = true;
    for (size_t g = 0; g < NUM_GROUPS; g++) {
      const uint8_t* out_uuid = keys_out + g * 16;
      bool matched = false;
      for (size_t expected_g = 0; expected_g < NUM_GROUPS; expected_g++) {
        if (std::memcmp(out_uuid, group_uuids[expected_g], 16) == 0) {
          matched = true;
          double expected = expected_sum_per_group[expected_g];
          if (std::abs(sums[g] - expected) > 1e-3) {
            fprintf(stderr, "  group %zu: sum %.3f != expected %.3f\n", g, sums[g], expected);
            sums_correct = false;
          }
          break;
        }
      }
      if (!matched) {
        fprintf(stderr, "  group %zu: output UUID didn't match any expected\n", g);
        all_groups_matched = false;
      }
    }
    ASSERT_TRUE("UUID hash_agg every output group matches an expected UUID", all_groups_matched);
    ASSERT_TRUE("UUID hash_agg per-group sums match expected", sums_correct);
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Test: INET key group-by + SUM
// ---------------------------------------------------------------------------
//
// Canonical INET key layout (per pgaccel_hash_join.h docs):
//   byte 0      = family (PGSQL_AF_INET=2 or PGSQL_AF_INET6=3)
//   byte 1      = bits (netmask, 0-128)
//   bytes 2-17  = ipaddr (16 bytes; IPv4 zero-padded after first 4 bytes)
//   bytes 18-23 = zero padding
//
// Mix IPv4 and IPv6 addresses to exercise both family branches.
static void test_hash_agg_inet_keys() {
  printf("--- test_hash_agg_inet_keys ---\n");

  constexpr size_t ROWS_PER_GROUP = 25000;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;
  constexpr size_t INET_SIZE = 24;

  // Build 4 canonical INET keys.
  // Group 0: IPv4 192.168.1.1/32
  // Group 1: IPv4 10.0.0.5/32
  // Group 2: IPv6 ::1/128
  // Group 3: IPv6 fe80::1/128
  uint8_t group_inets[NUM_GROUPS][INET_SIZE] = {};

  // Group 0: 192.168.1.1/32
  group_inets[0][0] = 2;   // PGSQL_AF_INET
  group_inets[0][1] = 32;  // /32
  group_inets[0][2] = 192;
  group_inets[0][3] = 168;
  group_inets[0][4] = 1;
  group_inets[0][5] = 1;
  // bytes 6-23: zero (ipaddr zero-padded after the first 4; bytes 18-23 padding)

  // Group 1: 10.0.0.5/32
  group_inets[1][0] = 2;
  group_inets[1][1] = 32;
  group_inets[1][2] = 10;
  group_inets[1][3] = 0;
  group_inets[1][4] = 0;
  group_inets[1][5] = 5;

  // Group 2: ::1/128
  group_inets[2][0] = 3;    // PGSQL_AF_INET6
  group_inets[2][1] = 128;  // /128
  group_inets[2][17] = 1;   // last byte of 16-byte ipaddr = 1

  // Group 3: fe80::1/128
  group_inets[3][0] = 3;
  group_inets[3][1] = 128;
  group_inets[3][2] = 0xfe;
  group_inets[3][3] = 0x80;
  group_inets[3][17] = 1;

  std::vector<uint8_t> keys(N * INET_SIZE, 0);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  double expected_sum_per_group[NUM_GROUPS] = {0.0, 0.0, 0.0, 0.0};

  for (size_t i = 0; i < N; i++) {
    size_t gid = i % NUM_GROUPS;
    std::memcpy(keys.data() + i * INET_SIZE, group_inets[gid], INET_SIZE);
    values[i] = static_cast<double>(i + 1);
    expected_sum_per_group[gid] += values[i];
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INET, val_arrays,
                               val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("INET hash_agg returned non-null state", state != nullptr);
  if (!state) {
    return;
  }

  size_t ngroups = pgaccel_agg_group_count(state);
  ASSERT_EQ_SZ("INET hash_agg group_count == 4", ngroups, NUM_GROUPS);

  if (ngroups == NUM_GROUPS) {
    const auto* keys_out = static_cast<const uint8_t*>(pgaccel_agg_get_group_keys(state));
    const double* sums = pgaccel_agg_get_results(state, 0);

    bool all_groups_matched = true;
    bool sums_correct = true;
    for (size_t g = 0; g < NUM_GROUPS; g++) {
      const uint8_t* out_inet = keys_out + g * INET_SIZE;
      bool matched = false;
      for (size_t expected_g = 0; expected_g < NUM_GROUPS; expected_g++) {
        if (std::memcmp(out_inet, group_inets[expected_g], INET_SIZE) == 0) {
          matched = true;
          double expected = expected_sum_per_group[expected_g];
          if (std::abs(sums[g] - expected) > 1e-3) {
            fprintf(stderr, "  group %zu: sum %.3f != expected %.3f\n", g, sums[g], expected);
            sums_correct = false;
          }
          break;
        }
      }
      if (!matched) {
        fprintf(stderr, "  group %zu: output INET didn't match any expected\n", g);
        all_groups_matched = false;
      }
    }
    ASSERT_TRUE("INET hash_agg every output group matches an expected INET", all_groups_matched);
    ASSERT_TRUE("INET hash_agg per-group sums match expected", sums_correct);
  }

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Test: INT64 key sanity (regression baseline — the path that already works)
// ---------------------------------------------------------------------------
//
// 100k rows, 4 distinct int64 keys. Confirms the sort-based path
// produces the same shape of output as UUID/INET so any failures above
// can be triangulated against this known-good baseline.
static void test_hash_agg_int64_baseline() {
  printf("--- test_hash_agg_int64_baseline ---\n");

  constexpr size_t ROWS_PER_GROUP = 25000;
  constexpr size_t NUM_GROUPS = 4;
  constexpr size_t N = ROWS_PER_GROUP * NUM_GROUPS;

  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N);
  std::vector<uint8_t> val_nulls(N, 0);
  double expected_sum_per_group[NUM_GROUPS] = {0.0, 0.0, 0.0, 0.0};

  for (size_t i = 0; i < N; i++) {
    size_t gid = i % NUM_GROUPS;
    keys[i] = static_cast<int64_t>(gid * 100 + 7);  // 7, 107, 207, 307
    values[i] = static_cast<double>(i + 1);
    expected_sum_per_group[gid] += values[i];
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64, val_arrays,
                               val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("INT64 hash_agg baseline state non-null", state != nullptr);
  if (!state) {
    return;
  }

  size_t ngroups = pgaccel_agg_group_count(state);
  ASSERT_EQ_SZ("INT64 hash_agg baseline group_count == 4", ngroups, NUM_GROUPS);
  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Test: 1M-row INT32 key sort-based hashagg repro + SUM/COUNT
// ---------------------------------------------------------------------------
//
// Forces the sort-based path directly so Metal/AdaptiveCpp can prove it
// declines cleanly before unsafe argument-buffer setup, while supported
// backends must produce correct grouped SUM and COUNT output.
static void test_sort_based_hash_agg_int32_1m_sum_count() {
  printf("--- test_sort_based_hash_agg_int32_1m_sum_count ---\n");

  constexpr size_t N = 1000000;
  constexpr size_t NUM_GROUPS = 8192;
  constexpr int32_t KEY_BASE = -4096;

  std::vector<int32_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<int32_t> values(N);
  std::vector<int64_t> expected_sums(NUM_GROUPS, 0);
  std::vector<int64_t> expected_counts(NUM_GROUPS, 0);

  for (size_t i = 0; i < N; ++i) {
    const size_t gid = (i * 8191 + 17) & (NUM_GROUPS - 1);
    keys[i] = static_cast<int32_t>(KEY_BASE + static_cast<int32_t>(gid));
    values[i] = static_cast<int32_t>(i % 97) - 48;
    expected_sums[gid] += values[i];
    expected_counts[gid] += 1;
  }

  const void* val_arrays[2] = {values.data(), nullptr};
  const uint8_t* val_null_arrays[2] = {nullptr, nullptr};
  int val_types[2] = {PGACCEL_VAL_INT32, PGACCEL_VAL_INT32};
  pgaccel_agg_col agg_cols[2] = {{PGACCEL_AGG_SUM, 0}, {PGACCEL_AGG_COUNT, SIZE_MAX}};

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status st = pgaccel_hash_agg_execute_sort_based(
      keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT32, val_arrays, val_null_arrays, val_types,
      agg_cols, 2, &state);

  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (std::strcmp(caps.backend_name, "metal") == 0) {
    ASSERT_EQ_INT("Metal sort-based hashagg returns UNSUPPORTED", st, PGACCEL_UNSUPPORTED);
    ASSERT_TRUE("Metal sort-based hashagg leaves state null", state == nullptr);
    ASSERT_EQ_SZ("Metal unsupported path launches no GPU kernels", pgaccel_gpu_exec_count(),
                 (uint64_t)0);
    return;
  }

  ASSERT_EQ_INT("sort-based hashagg status OK", st, PGACCEL_OK);
  ASSERT_TRUE("sort-based hashagg returned non-null state", state != nullptr);
  ASSERT_TRUE("sort-based hashagg launched GPU kernels", pgaccel_gpu_exec_count() > 0);
  if (st != PGACCEL_OK || state == nullptr) {
    if (state != nullptr)
      pgaccel_agg_free(state);
    return;
  }

  const size_t ngroups = pgaccel_agg_group_count(state);
  ASSERT_EQ_SZ("sort-based hashagg group_count == 8192", ngroups, NUM_GROUPS);

  const auto* keys_out = static_cast<const int32_t*>(pgaccel_agg_get_group_keys(state));
  const double* sums = pgaccel_agg_get_results(state, 0);
  const double* counts = pgaccel_agg_get_results(state, 1);
  const int64_t* row_counts = pgaccel_agg_get_counts(state);
  ASSERT_TRUE("sort-based hashagg keys/results non-null",
              keys_out != nullptr && sums != nullptr && counts != nullptr && row_counts != nullptr);

  bool all_groups_seen = true;
  bool sums_correct = true;
  bool counts_correct = true;
  std::vector<uint8_t> seen(NUM_GROUPS, 0);

  if (keys_out != nullptr && sums != nullptr && counts != nullptr && row_counts != nullptr &&
      ngroups == NUM_GROUPS) {
    for (size_t g = 0; g < ngroups; ++g) {
      const int64_t gid_signed = static_cast<int64_t>(keys_out[g]) - KEY_BASE;
      if (gid_signed < 0 || gid_signed >= static_cast<int64_t>(NUM_GROUPS)) {
        fprintf(stderr, "  output group %zu has unexpected key %d\n", g, keys_out[g]);
        all_groups_seen = false;
        continue;
      }
      const size_t gid = static_cast<size_t>(gid_signed);
      seen[gid] = 1;
      if (sums[g] != static_cast<double>(expected_sums[gid])) {
        fprintf(stderr, "  key %d: sum %.0f != expected %lld\n", keys_out[g], sums[g],
                (long long)expected_sums[gid]);
        sums_correct = false;
      }
      if (counts[g] != static_cast<double>(expected_counts[gid]) ||
          row_counts[g] != expected_counts[gid]) {
        fprintf(stderr, "  key %d: count %.0f / row_count %lld != expected %lld\n", keys_out[g],
                counts[g], (long long)row_counts[g], (long long)expected_counts[gid]);
        counts_correct = false;
      }
    }

    for (size_t gid = 0; gid < NUM_GROUPS; ++gid) {
      if (!seen[gid]) {
        fprintf(stderr, "  missing output group for key %d\n",
                static_cast<int32_t>(KEY_BASE + static_cast<int32_t>(gid)));
        all_groups_seen = false;
        break;
      }
    }
  }

  ASSERT_TRUE("sort-based hashagg saw every expected group", all_groups_seen);
  ASSERT_TRUE("sort-based hashagg SUM results match expected", sums_correct);
  ASSERT_TRUE("sort-based hashagg COUNT results match expected", counts_correct);

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Opt-in diagnostic: Metal radix-sort works, sort-based hashagg remains gated
// ---------------------------------------------------------------------------
//
// Enable with PGACCEL_HASHAGG_METAL_DIAG=1. This keeps the normal suite from
// adding another large Metal JIT lane, but gives a direct repro that the
// unsupported behavior is an explicit prelaunch sort-based-hashagg gate rather
// than a hidden radix-sort regression.
static void test_metal_sort_based_hashagg_unsupported_diagnostic() {
  if (std::getenv("PGACCEL_HASHAGG_METAL_DIAG") == nullptr) {
    return;
  }

  printf("--- test_metal_sort_based_hashagg_unsupported_diagnostic ---\n");

  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (std::strcmp(caps.backend_name, "metal") != 0) {
    printf("  skipped: backend=%s\n", caps.backend_name);
    return;
  }

  constexpr size_t N = 131072;
  constexpr size_t NUM_GROUPS = 1024;
  constexpr int32_t KEY_BASE = -512;

  std::vector<int32_t> sort_keys(N);
  std::vector<int32_t> agg_keys(N);
  std::vector<uint32_t> indices(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<int32_t> values(N, 1);

  for (size_t i = 0; i < N; ++i) {
    const size_t gid = (i * 1021 + 13) & (NUM_GROUPS - 1);
    const int32_t key = static_cast<int32_t>(KEY_BASE + static_cast<int32_t>(gid));
    sort_keys[i] = key;
    agg_keys[i] = key;
    indices[i] = static_cast<uint32_t>(i);
  }

  pgaccel_reset_gpu_exec_count();
  const pgaccel_status sort_st = pgaccel_sort_kv_i32(sort_keys.data(), indices.data(), N);
  ASSERT_EQ_INT("Metal radix kv-sort diagnostic status OK", sort_st, PGACCEL_OK);
  ASSERT_TRUE("Metal radix kv-sort diagnostic launched GPU kernels", pgaccel_gpu_exec_count() > 0);

  bool sorted = (sort_st == PGACCEL_OK);
  for (size_t i = 1; sorted && i < N; ++i) {
    if (sort_keys[i] < sort_keys[i - 1]) {
      sorted = false;
    }
  }
  ASSERT_TRUE("Metal radix kv-sort diagnostic output sorted", sorted);

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {nullptr};
  int val_types[1] = {PGACCEL_VAL_INT32};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_COUNT, SIZE_MAX}};

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status hashagg_st = pgaccel_hash_agg_execute_sort_based(
      agg_keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT32, val_arrays, val_null_arrays,
      val_types, agg_cols, 1, &state);

  ASSERT_EQ_INT("Metal sort-based hashagg diagnostic returns UNSUPPORTED", hashagg_st,
                PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("Metal sort-based hashagg diagnostic leaves state null", state == nullptr);
  ASSERT_EQ_SZ("Metal sort-based hashagg diagnostic launches no GPU kernels",
               pgaccel_gpu_exec_count(), (uint64_t)0);

  if (state != nullptr) {
    pgaccel_agg_free(state);
  }
}

static void test_hash_agg_f64_zero_nan_normalization() {
  printf("--- test_hash_agg_f64_zero_nan_normalization ---\n");

  uint64_t nan_payload_bits = 0x7ff8000000001234ULL;
  double nan_payload;
  std::memcpy(&nan_payload, &nan_payload_bits, sizeof(nan_payload));

  std::vector<double> keys = {-0.0, 0.0, std::numeric_limits<double>::quiet_NaN(), nan_payload};
  std::vector<uint8_t> key_nulls(keys.size(), 0);
  std::vector<double> values = {1.0, 2.0, 3.0, 4.0};
  std::vector<uint8_t> val_nulls(keys.size(), 0);

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), keys.size(), PGACCEL_KEY_FLOAT64,
                               val_arrays, val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("FLOAT64 normalization state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("FLOAT64 normalization group_count == 2", pgaccel_agg_group_count(state), (size_t)2);
  const auto* keys_out = static_cast<const double*>(pgaccel_agg_get_group_keys(state));
  const double* sums = pgaccel_agg_get_results(state, 0);
  bool saw_zero = false;
  bool saw_nan = false;
  for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
    if (keys_out[g] != keys_out[g]) {
      saw_nan = true;
      ASSERT_TRUE("NaN payloads grouped together", std::abs(sums[g] - 7.0) < 1e-9);
    } else if (keys_out[g] == 0.0) {
      saw_zero = true;
      ASSERT_TRUE("-0.0 and +0.0 grouped together", std::abs(sums[g] - 3.0) < 1e-9);
    }
  }
  ASSERT_TRUE("FLOAT64 normalization saw zero group", saw_zero);
  ASSERT_TRUE("FLOAT64 normalization saw NaN group", saw_nan);
  pgaccel_agg_free(state);
}

static void test_hash_agg_int64_null_sentinel_collision() {
  printf("--- test_hash_agg_int64_null_sentinel_collision ---\n");

  constexpr size_t N = 100000;
  std::vector<int64_t> keys(N);
  std::vector<uint8_t> key_nulls(N, 0);
  std::vector<double> values(N, 1.0);
  std::vector<uint8_t> val_nulls(N, 0);
  double expected_null = 0.0;
  double expected_max = 0.0;
  double expected_seven = 0.0;

  for (size_t i = 0; i < N; ++i) {
    switch (i % 3) {
      case 0:
        keys[i] = 42;
        key_nulls[i] = 1;
        expected_null += 1.0;
        break;
      case 1:
        keys[i] = std::numeric_limits<int64_t>::max();
        expected_max += 1.0;
        break;
      default:
        keys[i] = 7;
        expected_seven += 1.0;
        break;
    }
  }

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), N, PGACCEL_KEY_INT64, val_arrays,
                               val_null_arrays, val_types, agg_cols, 1);
  ASSERT_TRUE("INT64 null/sentinel collision state non-null", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("INT64 null/sentinel collision group_count == 3", pgaccel_agg_group_count(state),
               (size_t)3);
  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const double* sums = pgaccel_agg_get_results(state, 0);
  bool saw_null = false;
  bool saw_max = false;
  bool saw_seven = false;
  for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
    if (keys_out[g] == 0 && std::abs(sums[g] - expected_null) < 1e-9) {
      saw_null = true;
    } else if (keys_out[g] == std::numeric_limits<int64_t>::max()) {
      saw_max = true;
      ASSERT_TRUE("INT64 max sentinel-real group sum", std::abs(sums[g] - expected_max) < 1e-9);
    } else if (keys_out[g] == 7) {
      saw_seven = true;
      ASSERT_TRUE("INT64 ordinary group sum", std::abs(sums[g] - expected_seven) < 1e-9);
    }
  }
  ASSERT_TRUE("INT64 null/sentinel saw NULL group", saw_null);
  ASSERT_TRUE("INT64 null/sentinel saw max group", saw_max);
  ASSERT_TRUE("INT64 null/sentinel saw ordinary group", saw_seven);

  pgaccel_agg_free(state);
}

static void test_hash_agg_invalid_inputs_return_null() {
  printf("--- test_hash_agg_invalid_inputs_return_null ---\n");

  std::vector<int64_t> keys = {1, 2, 3, 4};
  std::vector<uint8_t> key_nulls(keys.size(), 0);
  std::vector<double> values(keys.size(), 1.0);
  std::vector<uint8_t> val_nulls(keys.size(), 0);

  const void* val_arrays[1] = {values.data()};
  const uint8_t* val_null_arrays[1] = {val_nulls.data()};
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col sum_cols[1] = {{PGACCEL_AGG_SUM, 0}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(nullptr, key_nulls.data(), keys.size(), PGACCEL_KEY_INT64,
                               val_arrays, val_null_arrays, val_types, sum_cols, 1);
  ASSERT_TRUE("NULL group_keys rejected", state == nullptr);

  state = pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), keys.size(), PGACCEL_KEY_INT64,
                                   nullptr, val_null_arrays, val_types, sum_cols, 1);
  ASSERT_TRUE("NULL value_cols rejected for SUM", state == nullptr);

  pgaccel_agg_col bad_cols[1] = {{static_cast<pgaccel_agg_func>(99), 0}};
  state = pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), keys.size(), PGACCEL_KEY_INT64,
                                   val_arrays, val_null_arrays, val_types, bad_cols, 1);
  ASSERT_TRUE("unknown aggregate func rejected", state == nullptr);

  pgaccel_agg_col avg_cols[1] = {{PGACCEL_AGG_AVG, 0}};
  state = pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), keys.size(), PGACCEL_KEY_INT64,
                                   val_arrays, val_null_arrays, val_types, avg_cols, 1);
  ASSERT_TRUE("finalize-mode AVG rejected", state == nullptr);
}

static void test_hash_agg_count_star_without_value_cols() {
  printf("--- test_hash_agg_count_star_without_value_cols ---\n");

  std::vector<int64_t> keys = {10, 20, 10, 30, 20, 20};
  std::vector<uint8_t> key_nulls(keys.size(), 0);
  int val_types[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_COUNT, SIZE_MAX}};

  pgaccel_agg_state* state =
      pgaccel_hash_agg_execute(keys.data(), key_nulls.data(), keys.size(), PGACCEL_KEY_INT64,
                               nullptr, nullptr, val_types, agg_cols, 1);
  ASSERT_TRUE("COUNT(*) accepts NULL value_cols", state != nullptr);
  if (!state) {
    return;
  }

  ASSERT_EQ_SZ("COUNT(*) no value cols group_count == 3", pgaccel_agg_group_count(state),
               (size_t)3);
  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const double* counts = pgaccel_agg_get_results(state, 0);
  bool saw_10 = false;
  bool saw_20 = false;
  bool saw_30 = false;
  for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
    if (keys_out[g] == 10) {
      saw_10 = true;
      ASSERT_TRUE("COUNT(*) key=10 count", std::abs(counts[g] - 2.0) < 1e-9);
    } else if (keys_out[g] == 20) {
      saw_20 = true;
      ASSERT_TRUE("COUNT(*) key=20 count", std::abs(counts[g] - 3.0) < 1e-9);
    } else if (keys_out[g] == 30) {
      saw_30 = true;
      ASSERT_TRUE("COUNT(*) key=30 count", std::abs(counts[g] - 1.0) < 1e-9);
    }
  }
  ASSERT_TRUE("COUNT(*) no value cols saw key=10", saw_10);
  ASSERT_TRUE("COUNT(*) no value cols saw key=20", saw_20);
  ASSERT_TRUE("COUNT(*) no value cols saw key=30", saw_30);

  pgaccel_agg_free(state);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel hash_agg key-type tests ===\n\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run hash_agg tests\n");
    return 1;
  }

  test_hash_agg_invalid_inputs_return_null();
  test_hash_agg_count_star_without_value_cols();
  test_hash_agg_int64_baseline();
  test_sort_based_hash_agg_int32_1m_sum_count();
  test_metal_sort_based_hashagg_unsupported_diagnostic();
  test_hash_agg_f64_zero_nan_normalization();
  test_hash_agg_int64_null_sentinel_collision();
  test_hash_agg_uuid_keys();
  test_hash_agg_inet_keys();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

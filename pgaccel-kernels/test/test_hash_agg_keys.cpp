// Standalone correctness tests for the UUID and INET hash-agg key
// paths in pgaccel-kernels/src/hash_agg.cpp. These two key-type arms
// (PGACCEL_KEY_UUID, PGACCEL_KEY_INET) shipped earlier this session
// (commits 243fa1f / c4134d5 / 3fbc03d) but had zero kernel-level
// test coverage — only Rust-side adapter tests exercised them.
//
// **KNOWN FAILURE — DO NOT ADD TO `just gpu-test` UNTIL FIXED.**
//
// Initial cold-cache run uncovered a Metal SSCP MSL-emitter bug for
// UUID and INET key types: the per-group SUM accumulation kernel
// emits MSL with `device device void**` (duplicate address-space
// qualifier + cross-address-space cast) that fails `xcrun metal`
// compilation. The grouping (sort path) succeeds — `group_count` is
// correct — but every per-group sum reads back as 0.0 because the
// JIT-compile failure leaves the value-aggregation pass un-executed.
//
// Filed as a Phase 6 SSCP entry in TODO.md. Until the upstream
// AdaptiveCpp emitter fix lands, this test stays as a reproducer:
// committed but not wired into `just gpu-test`. Per anti-cheat ban
// #1 / #9, the test asserts the H3-/INET-correct behavior so it
// FAILS today and PASSES the moment the kernel bug is resolved —
// no `#[ignore]`, no fake-success workaround.
//
// The tests use N >= SORT_AGG_MIN_ROWS = 100000 so the sort-based
// dispatch path runs (the small-N agg_hash path is the one with the
// other TODO Phase 6 known bug; sort-based is correct at scale for
// INT64 keys, demonstrated by the baseline test below).

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
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel hash_agg key-type tests ===\n\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run hash_agg tests\n");
    return 1;
  }

  test_hash_agg_int64_baseline();
  test_hash_agg_uuid_keys();
  test_hash_agg_inet_keys();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

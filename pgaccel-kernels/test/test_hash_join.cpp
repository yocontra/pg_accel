// Standalone correctness tests for the narrow selected GPU hash-join path.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "pgaccel_ffi.h"
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

#define ASSERT_EQ_SZ(desc, actual, expected)                                           \
  do {                                                                                 \
    if ((actual) == (expected)) {                                                      \
      g_pass++;                                                                        \
    } else {                                                                           \
      fprintf(stderr, "FAIL: %s -- got %zu, expected %zu\n", (desc), (size_t)(actual), \
              (size_t)(expected));                                                     \
      g_fail++;                                                                        \
    }                                                                                  \
  } while (0)

#define ASSERT_EQ_STATUS(desc, actual, expected)                                         \
  do {                                                                                   \
    if ((actual) == (expected)) {                                                        \
      g_pass++;                                                                          \
    } else {                                                                             \
      fprintf(stderr, "FAIL: %s -- got status %d, expected %d\n", (desc), (int)(actual), \
              (int)(expected));                                                          \
      g_fail++;                                                                          \
    }                                                                                    \
  } while (0)

static std::vector<std::pair<uint32_t, uint32_t>> collect_pairs(const uint32_t* pairs,
                                                                size_t count) {
  std::vector<std::pair<uint32_t, uint32_t>> out;
  out.reserve(count);
  for (size_t i = 0; i < count; ++i) {
    out.emplace_back(pairs[i * 2], pairs[i * 2 + 1]);
  }
  std::sort(out.begin(), out.end());
  return out;
}

static void test_int32_duplicates_nulls_and_dispatch_counter() {
  printf("--- test_int32_duplicates_nulls_and_dispatch_counter ---\n");

  std::vector<int32_t> build_keys = {1, 2, 3, 2, 0, 4};
  std::vector<uint8_t> build_nulls = {0, 0, 0, 0, 1, 0};
  std::vector<uint32_t> indices = {0, 1, 2, 3, 4, 5};

  pgaccel_reset_gpu_exec_count();
  pgaccel_hash_table* ht = pgaccel_hash_join_build(
      build_keys.data(), build_nulls.data(), indices.data(), build_keys.size(), PGACCEL_KEY_INT32);
  ASSERT_TRUE("INT32 hash join build returns a table", ht != nullptr);
  if (ht == nullptr) {
    return;
  }

  std::vector<int32_t> probe_keys = {2, 4, 9, 2};
  std::vector<uint8_t> probe_nulls = {0, 0, 0, 1};
  std::vector<uint32_t> pairs(8 * 2, 0);
  size_t match_count = 0;
  pgaccel_status st = pgaccel_hash_join_probe(ht, probe_keys.data(), probe_nulls.data(),
                                              probe_keys.size(), pairs.data(), 8, &match_count);
  ASSERT_EQ_STATUS("INT32 probe status", st, PGACCEL_OK);
  ASSERT_EQ_SZ("INT32 probe match count", match_count, 3);

  auto got = collect_pairs(pairs.data(), match_count);
  std::vector<std::pair<uint32_t, uint32_t>> expected = {{0, 1}, {0, 3}, {1, 5}};
  ASSERT_TRUE("INT32 probe pairs match expected duplicate/null semantics", got == expected);
  ASSERT_TRUE("INT32 build+probe launched GPU kernels", pgaccel_gpu_exec_count() >= 2);

  pgaccel_hash_join_free(ht);
}

static void test_int64_probe() {
  printf("--- test_int64_probe ---\n");

  std::vector<int64_t> build_keys = {10, -20, 30};
  std::vector<uint8_t> build_nulls = {0, 0, 0};
  std::vector<uint32_t> indices = {7, 8, 9};
  pgaccel_hash_table* ht = pgaccel_hash_join_build(
      build_keys.data(), build_nulls.data(), indices.data(), build_keys.size(), PGACCEL_KEY_INT64);
  ASSERT_TRUE("INT64 hash join build returns a table", ht != nullptr);
  if (ht == nullptr) {
    return;
  }

  std::vector<int64_t> probe_keys = {-20, 99, 10};
  std::vector<uint8_t> probe_nulls = {0, 0, 0};
  std::vector<uint32_t> pairs(4 * 2, 0);
  size_t match_count = 0;
  pgaccel_status st = pgaccel_hash_join_probe(ht, probe_keys.data(), probe_nulls.data(),
                                              probe_keys.size(), pairs.data(), 4, &match_count);
  ASSERT_EQ_STATUS("INT64 probe status", st, PGACCEL_OK);
  ASSERT_EQ_SZ("INT64 probe match count", match_count, 2);

  auto got = collect_pairs(pairs.data(), match_count);
  std::vector<std::pair<uint32_t, uint32_t>> expected = {{0, 8}, {2, 7}};
  ASSERT_TRUE("INT64 probe pairs match expected indices", got == expected);

  pgaccel_hash_join_free(ht);
}

static void test_count_only_probe_avoids_pair_output() {
  printf("--- test_count_only_probe_avoids_pair_output ---\n");

  std::vector<int32_t> build_keys = {5, 5, 8, 9};
  std::vector<uint8_t> build_nulls = {0, 0, 0, 0};
  std::vector<uint32_t> indices = {0, 1, 2, 3};
  pgaccel_hash_table* ht = pgaccel_hash_join_build(
      build_keys.data(), build_nulls.data(), indices.data(), build_keys.size(), PGACCEL_KEY_INT32);
  ASSERT_TRUE("count-only build returns a table", ht != nullptr);
  if (ht == nullptr) {
    return;
  }

  std::vector<int32_t> probe_keys = {5, 8, 7, 5, 0};
  std::vector<uint8_t> probe_nulls = {0, 0, 0, 1, 1};
  size_t match_count = 0;
  pgaccel_status st = pgaccel_hash_join_count(ht, probe_keys.data(), probe_nulls.data(),
                                              probe_keys.size(), &match_count);
  ASSERT_EQ_STATUS("count-only probe status", st, PGACCEL_OK);
  ASSERT_EQ_SZ("count-only probe match count", match_count, 3);

  pgaccel_hash_join_free(ht);
}

static void test_unsupported_float64_build_fails_closed() {
  printf("--- test_unsupported_float64_build_fails_closed ---\n");

  std::vector<double> build_keys = {1.0, 2.0};
  std::vector<uint8_t> build_nulls = {0, 0};
  std::vector<uint32_t> indices = {0, 1};
  pgaccel_hash_table* ht =
      pgaccel_hash_join_build(build_keys.data(), build_nulls.data(), indices.data(),
                              build_keys.size(), PGACCEL_KEY_FLOAT64);
  ASSERT_TRUE("FLOAT64 hash join build is unsupported", ht == nullptr);
}

static void test_duplicate_overflow_is_guarded() {
  printf("--- test_duplicate_overflow_is_guarded ---\n");

  std::vector<int32_t> build_keys = {7, 7, 7, 7, 7};
  std::vector<uint8_t> build_nulls(build_keys.size(), 0);
  std::vector<uint32_t> indices = {0, 1, 2, 3, 4};
  pgaccel_hash_table* ht = pgaccel_hash_join_build(
      build_keys.data(), build_nulls.data(), indices.data(), build_keys.size(), PGACCEL_KEY_INT32);
  ASSERT_TRUE("overflow test build returns a table", ht != nullptr);
  if (ht == nullptr) {
    return;
  }

  int32_t probe_key = 7;
  uint8_t probe_null = 0;
  std::vector<uint32_t> pairs(4 * 2, 0);
  size_t match_count = 0;
  pgaccel_status st =
      pgaccel_hash_join_probe(ht, &probe_key, &probe_null, 1, pairs.data(), 4, &match_count);
  ASSERT_EQ_STATUS("duplicate overflow returns unsupported", st, PGACCEL_UNSUPPORTED);
  ASSERT_EQ_SZ("duplicate overflow reports full match count", match_count, 5);

  pgaccel_hash_join_free(ht);
}

int main() {
  printf("=== pgaccel hash_join selected-kernel tests ===\n\n");

  pgaccel_status init = pgaccel_init();
  if (init != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init failed with status %d\n", (int)init);
    return 1;
  }

  test_int32_duplicates_nulls_and_dispatch_counter();
  test_int64_probe();
  test_count_only_probe_avoids_pair_output();
  test_unsupported_float64_build_fails_closed();
  test_duplicate_overflow_is_guarded();

  printf("\nPASS=%d FAIL=%d\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

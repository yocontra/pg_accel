// Standalone correctness tests for the narrow selected GPU hash-join path.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "pgaccel_expr.h"
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

static uint64_t test_hash64(uint64_t k) {
  k ^= k >> 33;
  k *= 0xff51afd7ed558ccdULL;
  k ^= k >> 33;
  k *= 0xc4ceb9fe1a85ec53ULL;
  k ^= k >> 33;
  return k;
}

static std::vector<int32_t> colliding_int32_keys(size_t count, size_t table_capacity) {
  std::vector<int32_t> keys;
  keys.reserve(count);
  const uint64_t mask = table_capacity - 1;
  const uint64_t target_bucket = test_hash64(0) & mask;
  for (uint32_t candidate = 0; keys.size() < count; ++candidate) {
    if ((test_hash64(static_cast<uint64_t>(candidate)) & mask) == target_bucket) {
      keys.push_back(static_cast<int32_t>(candidate));
    }
  }
  return keys;
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
  ASSERT_TRUE("INT32 build+probe launched expected GPU kernels", pgaccel_gpu_exec_count() >= 2);

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

static void test_device_count_table_rejects_pair_probe() {
  printf("--- test_device_count_table_rejects_pair_probe ---\n");

  std::vector<int32_t> build_keys = {5, 5, 8};
  std::vector<uint8_t> build_nulls = {0, 0, 0};
  void* device_keys = nullptr;
  void* device_nulls = nullptr;
  pgaccel_status st = pgaccel_expr_device_alloc_copy(
      build_keys.data(), build_keys.size() * sizeof(int32_t), &device_keys);
  ASSERT_EQ_STATUS("device-count keys allocation", st, PGACCEL_OK);
  st = pgaccel_expr_device_alloc_copy(build_nulls.data(), build_nulls.size() * sizeof(uint8_t),
                                      &device_nulls);
  ASSERT_EQ_STATUS("device-count null allocation", st, PGACCEL_OK);
  if (device_keys == nullptr || device_nulls == nullptr) {
    pgaccel_expr_device_free(device_keys);
    pgaccel_expr_device_free(device_nulls);
    return;
  }

  pgaccel_hash_table* ht = pgaccel_hash_join_build_device_count(
      device_keys, static_cast<const uint8_t*>(device_nulls), build_keys.size(), PGACCEL_KEY_INT32);
  ASSERT_TRUE("device-count build returns a table", ht != nullptr);
  if (ht == nullptr) {
    pgaccel_expr_device_free(device_keys);
    pgaccel_expr_device_free(device_nulls);
    return;
  }

  size_t match_count = 0;
  st = pgaccel_hash_join_count_device(ht, device_keys, static_cast<const uint8_t*>(device_nulls),
                                      build_keys.size(), &match_count);
  ASSERT_EQ_STATUS("device-count table count status", st, PGACCEL_OK);
  ASSERT_EQ_SZ("device-count table count result", match_count, 5);

  int32_t probe_key = 5;
  uint8_t probe_null = 0;
  uint32_t pairs[4] = {};
  match_count = 99;
  st = pgaccel_hash_join_probe(ht, &probe_key, &probe_null, 1, pairs, 2, &match_count);
  ASSERT_EQ_STATUS("device-count table rejects materializing probe", st, PGACCEL_UNSUPPORTED);
  ASSERT_EQ_SZ("device-count rejected probe resets match count", match_count, 0);

  pgaccel_hash_join_free(ht);
  pgaccel_expr_device_free(device_keys);
  pgaccel_expr_device_free(device_nulls);
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

static void test_adversarial_collisions_duplicates_nulls_and_device_build() {
  printf("--- test_adversarial_collisions_duplicates_nulls_and_device_build ---\n");

  constexpr size_t distinct_keys = 16;
  constexpr size_t repetitions = 64;
  constexpr size_t row_count = distinct_keys * repetitions;
  constexpr size_t table_capacity = row_count * 2;
  const std::vector<int32_t> collision_keys = colliding_int32_keys(distinct_keys, table_capacity);

  std::vector<int32_t> build_keys;
  std::vector<uint8_t> build_nulls;
  std::vector<uint32_t> indices;
  std::vector<std::vector<uint32_t>> expected_indices(distinct_keys);
  build_keys.reserve(row_count);
  build_nulls.reserve(row_count);
  indices.reserve(row_count);

  for (size_t repetition = 0; repetition < repetitions; ++repetition) {
    for (size_t key_index = 0; key_index < distinct_keys; ++key_index) {
      const size_t row = build_keys.size();
      const uint8_t is_null = (row % 17 == 0 || (repetition == 31 && key_index == 7)) ? 1 : 0;
      const uint32_t index = static_cast<uint32_t>(100000 + row * 7);
      build_keys.push_back(collision_keys[key_index]);
      build_nulls.push_back(is_null);
      indices.push_back(index);
      if (is_null == 0) {
        expected_indices[key_index].push_back(index);
      }
    }
  }

  pgaccel_hash_table* ht = pgaccel_hash_join_build(build_keys.data(), build_nulls.data(),
                                                   indices.data(), row_count, PGACCEL_KEY_INT32);
  ASSERT_TRUE("collision-heavy host-input build returns a table", ht != nullptr);
  if (ht == nullptr) {
    return;
  }

  size_t expected_matches = 0;
  std::vector<std::pair<uint32_t, uint32_t>> expected_pairs;
  for (size_t key_index = 0; key_index < distinct_keys; ++key_index) {
    expected_matches += expected_indices[key_index].size();
    for (uint32_t index : expected_indices[key_index]) {
      expected_pairs.emplace_back(static_cast<uint32_t>(key_index), index);
    }
  }
  std::sort(expected_pairs.begin(), expected_pairs.end());

  std::vector<uint32_t> pairs(expected_matches * 2, 0);
  size_t match_count = 0;
  pgaccel_status st =
      pgaccel_hash_join_probe(ht, collision_keys.data(), nullptr, collision_keys.size(),
                              pairs.data(), expected_matches, &match_count);
  ASSERT_EQ_STATUS("collision-heavy pair probe status", st, PGACCEL_OK);
  ASSERT_EQ_SZ("collision-heavy pair probe match count", match_count, expected_matches);
  ASSERT_TRUE("collision-heavy pair probe preserves caller indices",
              collect_pairs(pairs.data(), match_count) == expected_pairs);
  pgaccel_hash_join_free(ht);

  void* device_keys = nullptr;
  void* device_nulls = nullptr;
  st = pgaccel_expr_device_alloc_copy(build_keys.data(), build_keys.size() * sizeof(int32_t),
                                      &device_keys);
  ASSERT_EQ_STATUS("collision-heavy device keys allocation", st, PGACCEL_OK);
  st = pgaccel_expr_device_alloc_copy(build_nulls.data(), build_nulls.size() * sizeof(uint8_t),
                                      &device_nulls);
  ASSERT_EQ_STATUS("collision-heavy device null allocation", st, PGACCEL_OK);
  if (device_keys == nullptr || device_nulls == nullptr) {
    pgaccel_expr_device_free(device_keys);
    pgaccel_expr_device_free(device_nulls);
    return;
  }

  ht = pgaccel_hash_join_build_device_count(device_keys, static_cast<const uint8_t*>(device_nulls),
                                            row_count, PGACCEL_KEY_INT32);
  ASSERT_TRUE("collision-heavy device-input build returns a table", ht != nullptr);
  if (ht != nullptr) {
    size_t expected_count = 0;
    for (const auto& key_indices : expected_indices) {
      expected_count += key_indices.size() * key_indices.size();
    }
    match_count = 0;
    st = pgaccel_hash_join_count_device(ht, device_keys, static_cast<const uint8_t*>(device_nulls),
                                        row_count, &match_count);
    ASSERT_EQ_STATUS("collision-heavy device count status", st, PGACCEL_OK);
    ASSERT_EQ_SZ("collision-heavy device count result", match_count, expected_count);
    pgaccel_hash_join_free(ht);
  }

  pgaccel_expr_device_free(device_keys);
  pgaccel_expr_device_free(device_nulls);
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
  test_device_count_table_rejects_pair_probe();
  test_unsupported_float64_build_fails_closed();
  test_duplicate_overflow_is_guarded();
  test_adversarial_collisions_duplicates_nulls_and_device_build();

  printf("\nPASS=%d FAIL=%d\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

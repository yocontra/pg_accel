/*
 * Retained target name for release-baseline continuity. The retired partial
 * hash-aggregation surface is replaced by stress coverage for resident COUNT.
 */

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <unordered_map>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_resident_count.h"

static int g_checks = 0;
static int g_failures = 0;

#define CHECK(label, condition)                  \
  do {                                           \
    ++g_checks;                                  \
    if (!(condition)) {                          \
      ++g_failures;                              \
      std::fprintf(stderr, "FAIL: %s\n", label); \
    }                                            \
  } while (0)

static uint64_t hash64(uint64_t value) {
  value ^= value >> 33;
  value *= 0xff51afd7ed558ccdULL;
  value ^= value >> 33;
  value *= 0xc4ceb9fe1a85ec53ULL;
  value ^= value >> 33;
  return value;
}

static bool matches(const pgaccel_agg_state* state,
                    const std::unordered_map<int64_t, int64_t>& expected) {
  if (state == nullptr || pgaccel_agg_group_count(state) != expected.size())
    return false;
  const auto* keys = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const int64_t* counts = pgaccel_agg_get_counts(state);
  if (keys == nullptr || counts == nullptr)
    return false;

  std::unordered_map<int64_t, int64_t> actual;
  for (size_t group = 0; group < pgaccel_agg_group_count(state); ++group) {
    if (!actual.emplace(keys[group], counts[group]).second)
      return false;
  }
  return actual == expected;
}

static pgaccel_agg_state* execute(const std::vector<int64_t>& keys, size_t hint,
                                  pgaccel_status* status) {
  void* device_keys = nullptr;
  *status = pgaccel_expr_device_alloc_copy(keys.data(), keys.size() * sizeof(int64_t), &device_keys);
  if (*status != PGACCEL_OK || device_keys == nullptr)
    return nullptr;

  pgaccel_agg_state* state = nullptr;
  *status = pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
      static_cast<int64_t*>(device_keys), keys.size(), hint, &state);
  pgaccel_expr_device_free(device_keys);
  return state;
}

static void test_collision_chain() {
  std::printf("--- resident count collision chain ---\n");
  constexpr size_t kGroups = 12;
  constexpr uint64_t kTableMask = 31;
  std::vector<int64_t> colliding_keys;
  for (int64_t candidate = -1000000; colliding_keys.size() < kGroups; ++candidate) {
    if ((hash64(static_cast<uint64_t>(candidate)) & kTableMask) == 7)
      colliding_keys.push_back(candidate);
  }

  std::vector<int64_t> rows;
  std::unordered_map<int64_t, int64_t> expected;
  for (size_t group = 0; group < colliding_keys.size(); ++group) {
    const int64_t repetitions = static_cast<int64_t>(group + 1);
    for (int64_t repeat = 0; repeat < repetitions; ++repeat)
      rows.push_back(colliding_keys[group]);
    expected[colliding_keys[group]] = repetitions;
  }
  std::reverse(rows.begin(), rows.end());

  pgaccel_status status = PGACCEL_ERROR;
  pgaccel_agg_state* state = execute(rows, kGroups, &status);
  CHECK("collision chain status", status == PGACCEL_OK);
  CHECK("collision chain exact counts", matches(state, expected));
  pgaccel_agg_free(state);
}

static void test_state_owns_compact_result() {
  std::printf("--- resident count result ownership ---\n");
  std::vector<int64_t> rows(262144);
  std::unordered_map<int64_t, int64_t> expected;
  for (size_t row = 0; row < rows.size(); ++row) {
    rows[row] = static_cast<int64_t>((row * 29) % 17) - 8;
    ++expected[rows[row]];
  }

  pgaccel_status status = PGACCEL_ERROR;
  pgaccel_agg_state* state = execute(rows, expected.size(), &status);
  CHECK("owned result status", status == PGACCEL_OK);
  CHECK("state remains valid after resident input free", matches(state, expected));
  pgaccel_agg_free(state);
}

static void test_permutation_invariance_without_order_contract() {
  std::printf("--- resident count permutation invariance ---\n");
  std::vector<int64_t> rows;
  std::unordered_map<int64_t, int64_t> expected;
  for (int64_t key = -20; key <= 20; ++key) {
    for (int64_t repeat = 0; repeat < 3 + (key & 7); ++repeat) {
      rows.push_back(key);
      ++expected[key];
    }
  }

  for (size_t run = 0; run < 5; ++run) {
    std::rotate(rows.begin(), rows.begin() + static_cast<std::ptrdiff_t>(17 + run), rows.end());
    pgaccel_status status = PGACCEL_ERROR;
    pgaccel_agg_state* state = execute(rows, expected.size(), &status);
    CHECK("permutation status", status == PGACCEL_OK);
    CHECK("permutation key/count map", matches(state, expected));
    pgaccel_agg_free(state);
  }
}

static void test_single_group_capacity_boundary() {
  std::printf("--- resident count single-group capacity ---\n");
  std::vector<int64_t> rows(131072, INT64_C(-9223372036854770000));
  const std::unordered_map<int64_t, int64_t> expected = {
      {rows.front(), static_cast<int64_t>(rows.size())}};
  pgaccel_status status = PGACCEL_ERROR;
  pgaccel_agg_state* state = execute(rows, 1, &status);
  CHECK("single-group status", status == PGACCEL_OK);
  CHECK("single-group count", matches(state, expected));
  pgaccel_agg_free(state);
}

int main() {
  std::printf("=== resident grouped COUNT stress tests ===\n");
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "FATAL: pgaccel_init failed\n");
    return 1;
  }

  test_collision_chain();
  test_state_owns_compact_result();
  test_permutation_invariance_without_order_contract();
  test_single_group_capacity_boundary();

  pgaccel_shutdown();
  std::printf("resident grouped COUNT stress: %d checks, %d failures\n", g_checks, g_failures);
  return g_failures == 0 ? 0 : 1;
}

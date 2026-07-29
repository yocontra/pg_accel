/* Resident int64 grouped COUNT semantic and boundary matrix. */

#include <cstdint>
#include <cstdio>
#include <limits>
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

struct DeviceKeys {
  int64_t* ptr = nullptr;

  explicit DeviceKeys(const std::vector<int64_t>& keys) {
    void* allocation = nullptr;
    const pgaccel_status status =
        pgaccel_expr_device_alloc_copy(keys.data(), keys.size() * sizeof(int64_t), &allocation);
    CHECK("resident key allocation status", status == PGACCEL_OK);
    CHECK("resident key allocation pointer", allocation != nullptr);
    ptr = static_cast<int64_t*>(allocation);
  }

  ~DeviceKeys() {
    if (ptr != nullptr)
      pgaccel_expr_device_free(ptr);
  }

  DeviceKeys(const DeviceKeys&) = delete;
  DeviceKeys& operator=(const DeviceKeys&) = delete;
};

static bool state_matches(const pgaccel_agg_state* state,
                          const std::unordered_map<int64_t, int64_t>& expected) {
  if (state == nullptr || pgaccel_agg_group_count(state) != expected.size())
    return false;

  const auto* keys = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const int64_t* counts = pgaccel_agg_get_counts(state);
  const double* results = pgaccel_agg_get_results(state, 0);
  if (keys == nullptr || counts == nullptr || results == nullptr ||
      pgaccel_agg_get_results(state, 1) != nullptr) {
    return false;
  }

  std::unordered_map<int64_t, size_t> seen;
  int64_t total = 0;
  for (size_t group = 0; group < pgaccel_agg_group_count(state); ++group) {
    const auto expected_it = expected.find(keys[group]);
    if (expected_it == expected.end() || counts[group] != expected_it->second ||
        results[group] != static_cast<double>(expected_it->second)) {
      return false;
    }
    ++seen[keys[group]];
    total += counts[group];
  }

  int64_t expected_total = 0;
  for (const auto& [key, count] : expected) {
    if (seen[key] != 1)
      return false;
    expected_total += count;
  }
  return total == expected_total;
}

static void test_invalid_and_empty_contract() {
  std::printf("--- resident count invalid/empty contract ---\n");
  pgaccel_reset_gpu_exec_count();

  CHECK("null out-state rejected",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 1, 1, nullptr) ==
            PGACCEL_INVALID_ARGUMENT);

  auto* sentinel = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  CHECK("null keys rejected",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 1, 1, &sentinel) ==
            PGACCEL_INVALID_ARGUMENT);
  CHECK("null keys clear output", sentinel == nullptr);

  sentinel = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  CHECK("empty input accepted",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 0, 0, &sentinel) ==
            PGACCEL_OK);
  CHECK("empty input returns state", sentinel != nullptr);
  CHECK("empty state has zero groups", pgaccel_agg_group_count(sentinel) == 0);
  CHECK("empty state has no key buffer", pgaccel_agg_get_group_keys(sentinel) == nullptr);
  CHECK("empty state has no count buffer", pgaccel_agg_get_counts(sentinel) == nullptr);
  CHECK("empty state has no result buffer", pgaccel_agg_get_results(sentinel, 0) == nullptr);
  pgaccel_agg_free(sentinel);
  pgaccel_agg_free(nullptr);

  int64_t dummy = 0;
  sentinel = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  const size_t too_many_rows =
      static_cast<size_t>(std::numeric_limits<uint32_t>::max()) + size_t{1};
  CHECK("unrepresentable row count declines",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
            &dummy, too_many_rows, 1, &sentinel) == PGACCEL_UNSUPPORTED);
  CHECK("unrepresentable row count clears output", sentinel == nullptr);
  CHECK("invalid/empty paths do not dispatch", pgaccel_gpu_exec_count() == 0);
}

static void test_duplicate_and_extreme_keys() {
  std::printf("--- resident count duplicate/extreme keys ---\n");
  constexpr size_t kRows = 65536;
  constexpr size_t kOrdinaryGroups = 61;
  std::vector<int64_t> keys(kRows);
  std::unordered_map<int64_t, int64_t> expected;
  for (size_t row = 0; row < kRows; ++row) {
    int64_t key = static_cast<int64_t>((row * 37 + 11) % kOrdinaryGroups) - 30;
    if (row % 4093 == 0)
      key = std::numeric_limits<int64_t>::min();
    else if (row % 4091 == 0)
      key = std::numeric_limits<int64_t>::max();
    keys[row] = key;
    ++expected[key];
  }

  DeviceKeys device(keys);
  if (device.ptr == nullptr)
    return;

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status =
      pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
          device.ptr, keys.size(), expected.size(), &state);
  CHECK("duplicate/extreme status", status == PGACCEL_OK);
  CHECK("duplicate/extreme state", state != nullptr);
  CHECK("duplicate/extreme launches build and compact", pgaccel_gpu_exec_count() == 2);
  CHECK("duplicate/extreme map matches", state_matches(state, expected));
  pgaccel_agg_free(state);
}

static void test_high_cardinality_and_hint_normalization() {
  std::printf("--- resident count high cardinality/hints ---\n");
  constexpr size_t kRows = 8192;
  std::vector<int64_t> keys(kRows);
  std::unordered_map<int64_t, int64_t> expected;
  for (size_t row = 0; row < kRows; ++row) {
    keys[row] = static_cast<int64_t>(row) * 0x100000001LL - 900000000000LL;
    expected[keys[row]] = 1;
  }

  for (const size_t hint : {size_t{0}, kRows, kRows + 17}) {
    DeviceKeys device(keys);
    if (device.ptr == nullptr)
      return;
    pgaccel_agg_state* state = nullptr;
    const pgaccel_status status =
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
            device.ptr, keys.size(), hint, &state);
    CHECK("high-cardinality normalized hint status", status == PGACCEL_OK);
    CHECK("high-cardinality normalized hint map", state_matches(state, expected));
    pgaccel_agg_free(state);
  }

  DeviceKeys device(keys);
  if (device.ptr == nullptr)
    return;
  pgaccel_agg_state* wrapper_state =
      pgaccel_hash_count_i64_device_hash_execute_bounded(device.ptr, keys.size(), keys.size());
  CHECK("compatibility wrapper state", wrapper_state != nullptr);
  CHECK("compatibility wrapper map", state_matches(wrapper_state, expected));
  pgaccel_agg_free(wrapper_state);
}

static void test_underestimated_bound_declines() {
  std::printf("--- resident count underestimated bound ---\n");
  std::vector<int64_t> keys;
  for (int64_t key = -4; key < 4; ++key) {
    keys.push_back(key);
    keys.push_back(key);
  }
  DeviceKeys device(keys);
  if (device.ptr == nullptr)
    return;

  auto* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status =
      pgaccel_hash_count_i64_device_hash_execute_bounded_checked(device.ptr, keys.size(), 4,
                                                                 &state);
  CHECK("underestimated bound declines", status == PGACCEL_UNSUPPORTED);
  CHECK("underestimated bound publishes no state", state == nullptr);
  CHECK("underestimated bound still used device grouping", pgaccel_gpu_exec_count() == 2);
}

int main() {
  std::printf("=== resident grouped COUNT key tests ===\n");
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "FATAL: pgaccel_init failed\n");
    return 1;
  }

  test_invalid_and_empty_contract();
  test_duplicate_and_extreme_keys();
  test_high_cardinality_and_hint_normalization();
  test_underestimated_bound_declines();

  pgaccel_shutdown();
  std::printf("resident grouped COUNT keys: %d checks, %d failures\n", g_checks, g_failures);
  return g_failures == 0 ? 0 : 1;
}

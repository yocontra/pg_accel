#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <map>
#include <stdexcept>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"

namespace {

int g_pass = 0;
int g_fail = 0;

void check_impl(bool condition, const char* expression, const char* file, int line) {
  if (condition) {
    ++g_pass;
    return;
  }
  std::fprintf(stderr, "FAIL: %s:%d: %s\n", file, line, expression);
  ++g_fail;
}

#define CHECK(condition) check_impl((condition), #condition, __FILE__, __LINE__)

void check_status_impl(pgaccel_status actual, pgaccel_status expected, const char* expression,
                       const char* file, int line) {
  if (actual == expected) {
    ++g_pass;
    return;
  }
  std::fprintf(stderr, "FAIL: %s:%d: %s returned %d, expected %d\n", file, line, expression,
               static_cast<int>(actual), static_cast<int>(expected));
  ++g_fail;
}

#define CHECK_STATUS(expression, expected) \
  check_status_impl((expression), (expected), #expression, __FILE__, __LINE__)

template <typename T>
class SharedArray {
 public:
  SharedArray() = default;
  explicit SharedArray(size_t count) : count_(count) {
    if (count == 0)
      return;
    void* allocation = nullptr;
    if (pgaccel_expr_shared_alloc(count * sizeof(T), &allocation) != PGACCEL_OK ||
        allocation == nullptr)
      throw std::runtime_error("shared USM allocation failed");
    values_ = static_cast<T*>(allocation);
  }
  explicit SharedArray(const std::vector<T>& values) : SharedArray(values.size()) {
    std::copy(values.begin(), values.end(), values_);
  }
  SharedArray(const SharedArray&) = delete;
  SharedArray& operator=(const SharedArray&) = delete;
  ~SharedArray() { pgaccel_expr_shared_free(values_); }

  T* data() { return values_; }
  const T* data() const { return values_; }
  T& operator[](size_t index) { return values_[index]; }
  const T& operator[](size_t index) const { return values_[index]; }
  size_t size() const { return count_; }

 private:
  T* values_ = nullptr;
  size_t count_ = 0;
};

class SharedWorkspace {
 public:
  SharedWorkspace(size_t bytes, size_t alignment)
      : allocation_(bytes + (alignment == 0 ? 0 : alignment - 1)) {
    const uintptr_t raw = reinterpret_cast<uintptr_t>(allocation_.data());
    const uintptr_t aligned = (raw + alignment - 1) & ~(static_cast<uintptr_t>(alignment) - 1);
    pointer_ = reinterpret_cast<void*>(aligned);
  }

  void* data() const { return pointer_; }

 private:
  SharedArray<uint8_t> allocation_;
  void* pointer_ = nullptr;
};

pgaccel_grouped_agg_filter disabled_filter() {
  pgaccel_grouped_agg_filter filter = {};
  filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
  filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  return filter;
}

pgaccel_grouped_agg_desc hash_desc(const uint64_t* keys, const uint8_t* nulls, size_t row_count,
                                   size_t group_capacity) {
  pgaccel_grouped_agg_desc desc = {};
  desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  desc.size_bytes = sizeof(desc);
  desc.row_count = row_count;
  desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_HASH;
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  desc.key_count = 1;
  desc.group_capacity = group_capacity;
  desc.keys[0].values.values = keys;
  desc.keys[0].values.nulls = nulls;
  desc.keys[0].values.type = PGACCEL_VAL_INT64;
  desc.keys[0].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
  desc.keys[0].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
  desc.measure_count = 1;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[0].state_bytes = sizeof(uint64_t);
  desc.where_filter = disabled_filter();
  for (auto& filter : desc.measure_filters)
    filter = disabled_filter();
  return desc;
}

uint64_t make_h3_cell(int base_cell, int resolution, const int* digits) {
  uint64_t cell = UINT64_C(1) << 59;
  cell |= static_cast<uint64_t>(resolution & 0xf) << 52;
  cell |= static_cast<uint64_t>(base_cell & 0x7f) << 45;
  for (int r = 1; r <= 15; ++r) {
    const int shift = (15 - r) * 3;
    cell |= static_cast<uint64_t>(r <= resolution ? digits[r - 1] & 7 : 7) << shift;
  }
  return cell;
}

uint64_t h3_parent_for_test(uint64_t cell, int resolution) {
  cell = (cell & ~(UINT64_C(0xf) << 52)) | (static_cast<uint64_t>(resolution) << 52);
  for (int r = resolution + 1; r <= 15; ++r)
    cell |= UINT64_C(7) << ((15 - r) * 3);
  return cell;
}

pgaccel_grouped_agg_workspace_req query_workspace(const pgaccel_grouped_agg_desc& desc,
                                                  pgaccel_status* status = nullptr) {
  pgaccel_grouped_agg_workspace_req req = {};
  req.abi_version = PGACCEL_OLAP_ABI_VERSION;
  req.size_bytes = sizeof(req);
  const pgaccel_status result = pgaccel_grouped_agg_workspace_requirements(&desc, &req);
  if (status != nullptr)
    *status = result;
  return req;
}

struct HashOutput {
  explicit HashOutput(size_t capacity, bool nullable)
      : keys(capacity, kKeySentinel), nulls(nullable ? capacity : 0, uint8_t{0xa5}),
        counts(capacity, kCountSentinel) {
    out.abi_version = PGACCEL_OLAP_ABI_VERSION;
    out.size_bytes = sizeof(out);
    out.group_capacity = capacity;
    out.output_space = PGACCEL_MEM_SPACE_HOST;
    out.keys[0].values = keys.data();
    out.keys[0].nulls = nullable ? nulls.data() : nullptr;
    out.keys[0].type = PGACCEL_VAL_INT64;
    out.measures[0].count = counts.data();
  }

  bool untouched() const {
    return out.emitted_group_count == 0 && out.selected_count == 0 && out.uncertain_count == 0 &&
           std::all_of(keys.begin(), keys.end(),
                       [](uint64_t value) { return value == kKeySentinel; }) &&
           std::all_of(nulls.begin(), nulls.end(), [](uint8_t value) { return value == 0xa5; }) &&
           std::all_of(counts.begin(), counts.end(),
                       [](uint64_t value) { return value == kCountSentinel; });
  }

  static constexpr uint64_t kKeySentinel = UINT64_C(0xfedcba9876543210);
  static constexpr uint64_t kCountSentinel = UINT64_C(0xcafef00ddeadbeef);
  pgaccel_grouped_agg_out out = {};
  std::vector<uint64_t> keys;
  std::vector<uint8_t> nulls;
  std::vector<uint64_t> counts;
};

pgaccel_status execute_with_workspace(const pgaccel_grouped_agg_desc& original,
                                      const pgaccel_grouped_agg_workspace_req& req,
                                      SharedWorkspace& workspace, pgaccel_grouped_agg_out* out,
                                      int32_t* detail) {
  pgaccel_grouped_agg_desc desc = original;
  desc.scratch = workspace.data();
  desc.scratch_bytes = req.bytes;
  desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
  desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
  return pgaccel_grouped_agg_execute_ex(&desc, out, detail);
}

struct ExpectedGroup {
  bool is_null;
  uint64_t key;
  uint64_t count;

  bool operator==(const ExpectedGroup& other) const {
    return is_null == other.is_null && key == other.key && count == other.count;
  }
};

std::vector<ExpectedGroup> oracle(const std::vector<uint64_t>& keys,
                                  const std::vector<uint8_t>* nulls) {
  std::map<uint64_t, uint64_t> counts;
  uint64_t null_count = 0;
  for (size_t row = 0; row < keys.size(); ++row) {
    if (nulls != nullptr && (*nulls)[row] != 0)
      ++null_count;
    else
      ++counts[keys[row]];
  }
  std::vector<ExpectedGroup> expected;
  if (null_count != 0)
    expected.push_back({true, 0, null_count});
  for (const auto& [key, count] : counts)
    expected.push_back({false, key, count});
  return expected;
}

std::vector<ExpectedGroup> actual_groups(const HashOutput& output) {
  std::vector<ExpectedGroup> actual;
  for (size_t i = 0; i < output.out.emitted_group_count; ++i) {
    const bool is_null = !output.nulls.empty() && output.nulls[i] != 0;
    actual.push_back({is_null, output.keys[i], output.counts[i]});
  }
  return actual;
}

void check_success(const HashOutput& output, const std::vector<ExpectedGroup>& expected,
                   size_t selected) {
  CHECK(output.out.emitted_group_count == expected.size());
  CHECK(output.out.selected_count == selected);
  CHECK(output.out.uncertain_count == 0);
  const std::vector<ExpectedGroup> actual = actual_groups(output);
  if (actual != expected) {
    std::fprintf(stderr, "group mismatch: actual=%zu expected=%zu\n", actual.size(),
                 expected.size());
    const size_t limit = std::max(actual.size(), expected.size());
    for (size_t i = 0; i < limit; ++i) {
      if (i < actual.size())
        std::fprintf(stderr, "  actual[%zu] null=%d key=%llu count=%llu\n", i,
                     actual[i].is_null ? 1 : 0, static_cast<unsigned long long>(actual[i].key),
                     static_cast<unsigned long long>(actual[i].count));
      if (i < expected.size())
        std::fprintf(stderr, "  expect[%zu] null=%d key=%llu count=%llu\n", i,
                     expected[i].is_null ? 1 : 0, static_cast<unsigned long long>(expected[i].key),
                     static_cast<unsigned long long>(expected[i].count));
    }
  }
  CHECK(actual == expected);
  for (size_t i = 0; i < output.out.emitted_group_count; ++i) {
    if (!output.nulls.empty())
      CHECK(output.nulls[i] <= 1);
    if (!output.nulls.empty() && output.nulls[i] != 0)
      CHECK(output.keys[i] == 0);
  }
}

uint64_t hash_for_test(uint64_t value) {
  value += UINT64_C(0x9e3779b97f4a7c15);
  value = (value ^ (value >> 30)) * UINT64_C(0xbf58476d1ce4e5b9);
  value = (value ^ (value >> 27)) * UINT64_C(0x94d049bb133111eb);
  return value ^ (value >> 31);
}

std::vector<uint64_t> colliding_keys(size_t slot_count, size_t count) {
  std::vector<uint64_t> result;
  const uint64_t target = hash_for_test(0) & (slot_count - 1);
  for (uint64_t candidate = 0; result.size() < count; ++candidate) {
    if ((hash_for_test(candidate) & (slot_count - 1)) == target)
      result.push_back(candidate);
  }
  return result;
}

void test_duplicates_nulls_high_bits_collisions_and_warm_workspace() {
  std::printf("--- hash duplicates/nulls/high-bits/collisions/warm workspace ---\n");
  const std::vector<uint64_t> collisions = colliding_keys(32, 5);
  std::vector<uint64_t> host_keys = {
      collisions[0],
      collisions[1],
      collisions[0],
      UINT64_C(0x8000000000000000),
      UINT64_MAX,
      UINT64_MAX,
      17,
      collisions[2],
      collisions[3],
      collisions[4],
      UINT64_C(0x8000000000000000),
      99,
  };
  std::vector<uint8_t> host_nulls = {0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
  SharedArray<uint64_t> keys(host_keys);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nulls.data(), keys.size(), 16);
  pgaccel_status query_status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req req = query_workspace(desc, &query_status);
  CHECK_STATUS(query_status, PGACCEL_OK);
  CHECK(req.bytes > 0);
  CHECK(req.alignment >= 8);
  SharedWorkspace workspace(req.bytes, req.alignment);
  const std::vector<ExpectedGroup> expected = oracle(host_keys, &host_nulls);

  std::vector<ExpectedGroup> first;
  for (size_t iteration = 0; iteration < 12; ++iteration) {
    HashOutput output(desc.group_capacity, true);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    check_success(output, expected, host_keys.size());
    if (iteration == 0)
      first = actual_groups(output);
    else
      CHECK(actual_groups(output) == first);
  }

  std::vector<uint64_t> second_keys(host_keys.size(), UINT64_C(0xffff000000000001));
  second_keys[3] = 2;
  second_keys[7] = 2;
  std::vector<uint8_t> second_nulls(host_keys.size(), 0);
  second_nulls[1] = 1;
  second_nulls[9] = 1;
  for (size_t i = 0; i < second_keys.size(); ++i) {
    keys[i] = second_keys[i];
    nulls[i] = second_nulls[i];
  }
  HashOutput reset_output(desc.group_capacity, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &reset_output.out, &detail),
               PGACCEL_OK);
  check_success(reset_output, oracle(second_keys, &second_nulls), second_keys.size());
}

void test_empty_all_null_and_capacity_boundaries() {
  std::printf("--- hash empty/all-null/capacity boundaries ---\n");
  {
    pgaccel_grouped_agg_desc desc = hash_desc(nullptr, nullptr, 0, 4);
    const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(desc.group_capacity, false);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    check_success(output, {}, 0);
  }
  {
    std::vector<uint64_t> host_keys(32, UINT64_MAX);
    std::vector<uint8_t> host_nulls(32, 1);
    SharedArray<uint64_t> keys(host_keys);
    SharedArray<uint8_t> nulls(host_nulls);
    pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nulls.data(), keys.size(), 1);
    const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(desc.group_capacity, true);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    check_success(output, {{true, 0, host_keys.size()}}, host_keys.size());
  }
  {
    std::vector<uint64_t> exact_keys = colliding_keys(16, 8);
    SharedArray<uint64_t> keys(exact_keys);
    pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nullptr, keys.size(), 8);
    const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(desc.group_capacity, false);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    check_success(output, oracle(exact_keys, nullptr), exact_keys.size());
  }
  {
    std::vector<uint64_t> overflow_keys = colliding_keys(16, 9);
    SharedArray<uint64_t> keys(overflow_keys);
    pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nullptr, keys.size(), 8);
    const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(desc.group_capacity, false);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                 PGACCEL_UNSUPPORTED);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    CHECK(output.untouched());
  }
  {
    SharedArray<uint64_t> key(std::vector<uint64_t>({1}));
    pgaccel_grouped_agg_desc desc =
        hash_desc(key.data(), nullptr, 1, std::numeric_limits<size_t>::max());
    pgaccel_status status = PGACCEL_OK;
    query_workspace(desc, &status);
    CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
  }
}

void test_fixed_seed_differential() {
  std::printf("--- hash fixed-seed differential ---\n");
  constexpr size_t rows = 4096;
  std::vector<uint64_t> host_keys(rows);
  std::vector<uint8_t> host_nulls(rows);
  uint64_t random = UINT64_C(0x72b9a4e36d0c15f1);
  for (size_t row = 0; row < rows; ++row) {
    random ^= random << 13;
    random ^= random >> 7;
    random ^= random << 17;
    host_keys[row] = (random % 47) | ((row % 19 == 0) ? (UINT64_C(1) << 63) : 0);
    host_nulls[row] = row % 31 == 0 ? 1 : 0;
  }
  SharedArray<uint64_t> keys(host_keys);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nulls.data(), rows, 128);
  const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(desc.group_capacity, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
  check_success(output, oracle(host_keys, &host_nulls), rows);
}

void test_million_row_hot_groups() {
  std::printf("--- hash million-row hot groups ---\n");
  constexpr size_t rows = 1'000'000;
  constexpr size_t h3_resolution_zero_capacity = 123;
  std::vector<uint64_t> host_keys(rows, UINT64_C(0x08001fffffffffff));
  std::vector<uint8_t> host_nulls(rows);
  for (size_t row = 0; row < rows; ++row)
    host_nulls[row] = row % 97 == 0 ? 1 : 0;

  SharedArray<uint64_t> keys(host_keys);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc =
      hash_desc(keys.data(), nulls.data(), rows, h3_resolution_zero_capacity);
  const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(desc.group_capacity, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  check_success(output, oracle(host_keys, &host_nulls), rows);
}

void test_fused_h3_parent_exactness_and_validation() {
  std::printf("--- fused H3 parent hash exactness/validation ---\n");
  const int a[15] = {2, 3, 4, 5, 6};
  const int b[15] = {2, 3, 1, 1, 1};
  const int c[15] = {2, 4, 1, 2, 3};
  const int d[15] = {5, 1, 2, 3, 4};
  std::vector<uint64_t> host_cells = {
      make_h3_cell(10, 5, a), make_h3_cell(10, 5, b), make_h3_cell(10, 5, c),
      make_h3_cell(10, 5, d), make_h3_cell(10, 5, a), UINT64_MAX,
  };
  std::vector<uint8_t> host_nulls = {0, 0, 0, 0, 0, 1};
  std::vector<uint64_t> expected_keys(host_cells.size());
  for (size_t row = 0; row < host_cells.size(); ++row)
    expected_keys[row] = host_nulls[row] == 0 ? h3_parent_for_test(host_cells[row], 2) : 0;

  SharedArray<uint64_t> cells(host_cells);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = hash_desc(cells.data(), nulls.data(), cells.size(), 8);
  desc.keys[0].flags = PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT;
  desc.keys[0]._pad0 = 2;
  pgaccel_status status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req req = query_workspace(desc, &status);
  CHECK_STATUS(status, PGACCEL_OK);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(desc.group_capacity, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  check_success(output, oracle(expected_keys, &host_nulls), host_cells.size());
  CHECK(std::equal(host_cells.begin(), host_cells.end(), cells.data()));
  CHECK(std::equal(host_nulls.begin(), host_nulls.end(), nulls.data()));

  cells[0] = 0;
  HashOutput malformed(desc.group_capacity, true);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &malformed.out, &detail),
               PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(malformed.untouched());
  cells[0] = host_cells[0];

  const int coarse_digits[15] = {2};
  cells[0] = make_h3_cell(10, 1, coarse_digits);
  HashOutput coarse(desc.group_capacity, true);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &coarse.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(coarse.untouched());
  cells[0] = host_cells[0];

  pgaccel_grouped_agg_desc invalid = desc;
  invalid.keys[0].flags = UINT32_C(1) << 31;
  query_workspace(invalid, &status);
  CHECK_STATUS(status, PGACCEL_ERROR);
  invalid = desc;
  invalid.keys[0]._pad0 = 16;
  query_workspace(invalid, &status);
  CHECK_STATUS(status, PGACCEL_ERROR);
  invalid = desc;
  invalid.keys[0].flags = 0;
  query_workspace(invalid, &status);
  CHECK_STATUS(status, PGACCEL_ERROR);
  invalid = desc;
  invalid.keys[0].values.type = PGACCEL_VAL_INT32;
  query_workspace(invalid, &status);
  CHECK_STATUS(status, PGACCEL_ERROR);
}

void test_hard_failure_and_unsupported_shapes() {
  std::printf("--- hash hard failure and unsupported shapes ---\n");
  std::vector<uint64_t> host_keys = {1, 2, 1, 3};
  std::vector<uint8_t> bad_nulls = {0, 2, 0, 0};
  SharedArray<uint64_t> keys(host_keys);
  SharedArray<uint8_t> nulls(bad_nulls);
  pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nulls.data(), keys.size(), 4);
  const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(desc.group_capacity, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(output.untouched());

  pgaccel_status status = PGACCEL_OK;
  pgaccel_grouped_agg_desc unsupported = desc;
  unsupported.keys[0].values.type = PGACCEL_VAL_INT32;
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_UNSUPPORTED);

  SharedArray<int8_t> mask(std::vector<int8_t>(host_keys.size(), PGACCEL_EXPR_TRUE));
  unsupported = desc;
  unsupported.keys[0].values.nulls = nullptr;
  unsupported.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  unsupported.where_filter.mask = mask.data();
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_UNSUPPORTED);

  unsupported = desc;
  unsupported.keys[0].values.nulls = nullptr;
  unsupported.measures[1] = unsupported.measures[0];
  unsupported.measure_count = 2;
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_UNSUPPORTED);

  pgaccel_grouped_agg_desc invalid = desc;
  invalid.keys[0].values.nulls = nullptr;
  invalid.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  query_workspace(invalid, &status);
  CHECK_STATUS(status, PGACCEL_ERROR);
}

}  // namespace

int main() {
  try {
    CHECK_STATUS(pgaccel_init(), PGACCEL_OK);
    test_duplicates_nulls_high_bits_collisions_and_warm_workspace();
    test_empty_all_null_and_capacity_boundaries();
    test_fixed_seed_differential();
    test_million_row_hot_groups();
    test_fused_h3_parent_exactness_and_validation();
    test_hard_failure_and_unsupported_shapes();
    CHECK_STATUS(pgaccel_shutdown(), PGACCEL_OK);
  } catch (const std::exception& error) {
    std::fprintf(stderr, "FAIL: unexpected test exception: %s\n", error.what());
    ++g_fail;
  }
  std::printf("test_grouped_agg_hash: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

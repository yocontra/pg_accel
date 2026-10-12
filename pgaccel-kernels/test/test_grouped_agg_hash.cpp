#include <sys/mman.h>
#include <unistd.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
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
  explicit HashOutput(size_t capacity, bool nullable, bool sum = false, bool minimum = false,
                      bool maximum = false)
      : keys(capacity, kKeySentinel), nulls(nullable ? capacity : 0, uint8_t{0xa5}),
        counts(capacity, kCountSentinel), sums(sum ? capacity : 0, kCountSentinel),
        nonnulls(sum || minimum || maximum ? capacity : 0, kCountSentinel),
        mins(minimum ? capacity : 0, kCountSentinel), maxs(maximum ? capacity : 0, kCountSentinel) {
    out.abi_version = PGACCEL_OLAP_ABI_VERSION;
    out.size_bytes = sizeof(out);
    out.group_capacity = capacity;
    out.output_space = PGACCEL_MEM_SPACE_HOST;
    out.keys[0].values = keys.data();
    out.keys[0].nulls = nullable ? nulls.data() : nullptr;
    out.keys[0].type = PGACCEL_VAL_INT64;
    out.measures[0].count = counts.data();
    out.measures[0].sum = sum ? sums.data() : nullptr;
    out.measures[0].nonnull_count = sum || minimum || maximum ? nonnulls.data() : nullptr;
    out.measures[0].min = minimum ? mins.data() : nullptr;
    out.measures[0].max = maximum ? maxs.data() : nullptr;
  }

  bool untouched() const {
    return out.emitted_group_count == 0 && out.selected_count == 0 && out.uncertain_count == 0 &&
           std::all_of(keys.begin(), keys.end(),
                       [](uint64_t value) { return value == kKeySentinel; }) &&
           std::all_of(nulls.begin(), nulls.end(), [](uint8_t value) { return value == 0xa5; }) &&
           std::all_of(counts.begin(), counts.end(),
                       [](uint64_t value) { return value == kCountSentinel; }) &&
           std::all_of(sums.begin(), sums.end(),
                       [](uint64_t value) { return value == kCountSentinel; }) &&
           std::all_of(nonnulls.begin(), nonnulls.end(),
                       [](uint64_t value) { return value == kCountSentinel; }) &&
           std::all_of(mins.begin(), mins.end(),
                       [](uint64_t value) { return value == kCountSentinel; }) &&
           std::all_of(maxs.begin(), maxs.end(),
                       [](uint64_t value) { return value == kCountSentinel; });
  }

  static constexpr uint64_t kKeySentinel = UINT64_C(0xfedcba9876543210);
  static constexpr uint64_t kCountSentinel = UINT64_C(0xcafef00ddeadbeef);
  pgaccel_grouped_agg_out out = {};
  std::vector<uint64_t> keys;
  std::vector<uint8_t> nulls;
  std::vector<uint64_t> counts;
  std::vector<uint64_t> sums;
  std::vector<uint64_t> nonnulls;
  std::vector<uint64_t> mins;
  std::vector<uint64_t> maxs;
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
    const size_t limit = std::min(size_t{32}, std::max(actual.size(), expected.size()));
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
  bool canonical_nulls = true;
  for (size_t i = 0; i < output.out.emitted_group_count; ++i) {
    if (!output.nulls.empty() &&
        (output.nulls[i] > 1 || (output.nulls[i] != 0 && output.keys[i] != 0)))
      canonical_nulls = false;
  }
  CHECK(canonical_nulls);
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

struct CountColumnType {
  int32_t type;
  uint32_t width;
};

constexpr CountColumnType kCountColumnTypes[] = {
    {PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1},      {PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4},
    {PGACCEL_GROUPED_AGG_PHYSICAL_INT64, 8},     {PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4},
    {PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8},   {PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 4},
    {PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 8},
};

void set_count_column(pgaccel_grouped_agg_desc* desc, CountColumnType type, const void* values,
                      const uint8_t* nulls) {
  auto& measure = desc->measures[0];
  measure.op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
  measure.value.values = values;
  measure.value.nulls = nulls;
  measure.value.physical_type = type.type;
  measure.value.element_bytes = type.width;
}

void set_sql_filter(pgaccel_grouped_agg_desc* desc, const int8_t* mask) {
  desc->measure_filters[0] = disabled_filter();
  desc->measure_filters[0].kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc->measure_filters[0].mask = mask;
}

void set_sql_where(pgaccel_grouped_agg_desc* desc, const int8_t* mask) {
  desc->where_filter = disabled_filter();
  desc->where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc->where_filter.mask = mask;
}

void test_hash_where_metadata() {
  const long page_size = sysconf(_SC_PAGESIZE);
  CHECK(page_size > 0);
  if (page_size <= 0)
    return;
  void* page = mmap(nullptr, static_cast<size_t>(page_size), PROT_NONE,
                    MAP_PRIVATE | MAP_ANON, -1, 0);
  CHECK(page != MAP_FAILED);
  if (page == MAP_FAILED)
    return;
  for (size_t rows : {size_t{0}, size_t{1}, size_t{1'000'017}, size_t{UINT32_MAX}}) {
    for (size_t capacity : {size_t{1}, size_t{17}, size_t{16'385}}) {
      for (auto key_type : {PGACCEL_VAL_INT32, PGACCEL_VAL_INT64}) {
        for (unsigned state = 0; state < 9; ++state) {
          auto desc = hash_desc(rows ? static_cast<const uint64_t*>(page) : nullptr,
                                static_cast<const uint8_t*>(page), rows, capacity);
          desc.keys[0].values.type = key_type;
          if (state != 0) {
            set_count_column(&desc, kCountColumnTypes[1], rows ? page : nullptr,
                             static_cast<const uint8_t*>(page));
            desc.measures[0].agg_mask |= ((state - 1) & 1 ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                                        ((state - 1) & 2 ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                                        ((state - 1) & 4 ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
          }
          pgaccel_status status = PGACCEL_ERROR;
          const auto base = query_workspace(desc, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          set_sql_where(&desc, static_cast<const int8_t*>(page));
          const auto where = query_workspace(desc, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          CHECK(where.bytes == base.bytes + 256 * sizeof(uint32_t));
          CHECK(where.alignment == base.alignment);
          set_sql_filter(&desc, static_cast<const int8_t*>(page));
          CHECK(query_workspace(desc, &status).bytes == where.bytes);
          CHECK_STATUS(status, PGACCEL_OK);
          int32_t mode = 0;
          CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
          CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH);
          for (unsigned malformed = 0; malformed < 5; ++malformed) {
            auto bad = desc;
            switch (malformed) {
              case 0: bad.where_filter.mask = nullptr; break;
              case 1: bad.where_filter.flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL; break;
              case 2: bad.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_RECHECK; break;
              case 3: bad.where_filter._pad0 = 1; break;
              default: bad.where_filter.predicate_measure_slot = 1; break;
            }
            query_workspace(bad, &status);
            CHECK(status != PGACCEL_OK);
          }
        }
      }
    }
  }
  CHECK(munmap(page, static_cast<size_t>(page_size)) == 0);
}

void test_hash_where_values(size_t rows, size_t groups, unsigned state, bool filtered,
                           bool narrow, bool reject_all) {
  std::vector<uint64_t> host_keys(rows);
  std::vector<int32_t> host_narrow(rows), host_values(rows);
  std::vector<uint8_t> host_nulls(rows), host_value_nulls(rows);
  std::vector<int8_t> host_where(rows), host_filter(rows);
  for (size_t row = 0; row < rows; ++row) {
    const size_t group = row % groups;
    host_narrow[row] = static_cast<int32_t>(group) - static_cast<int32_t>(groups / 2);
    host_keys[row] = static_cast<uint64_t>(static_cast<int64_t>(host_narrow[row]));
    host_nulls[row] = group == 0;
    host_values[row] = row % 2 ? INT32_MIN : INT32_MAX;
    host_value_nulls[row] = group == 1 || row % 7 == 0;
    // One key exists only in rejected input; another survives WHERE but has
    // every measure filtered out. UNKNOWN must behave differently in each slot.
    host_where[row] = reject_all || group == 2 ? PGACCEL_EXPR_UNCERTAIN
                       : row % 5 == 0 ? PGACCEL_EXPR_FALSE : PGACCEL_EXPR_TRUE;
    host_filter[row] = group == 3 ? PGACCEL_EXPR_FALSE
                        : row % 11 == 0 ? PGACCEL_EXPR_UNCERTAIN : PGACCEL_EXPR_TRUE;
  }
  SharedArray<uint64_t> keys(host_keys);
  SharedArray<int32_t> narrow_keys(host_narrow), values(host_values);
  SharedArray<uint8_t> nulls(host_nulls), value_nulls(host_value_nulls);
  // Nonnull mask descriptors remain canonical for the empty-input case.
  SharedArray<int8_t> where(std::max(rows, size_t{1})), filter(std::max(rows, size_t{1}));
  std::copy(host_where.begin(), host_where.end(), where.data());
  std::copy(host_filter.begin(), host_filter.end(), filter.data());
  auto desc = hash_desc(keys.data(), nulls.data(), rows, groups);
  if (narrow) {
    desc.keys[0].values.values = narrow_keys.data();
    desc.keys[0].values.type = PGACCEL_VAL_INT32;
  }
  const unsigned lanes = state == 0 ? 0 : state - 1;
  if (state != 0) {
    set_count_column(&desc, kCountColumnTypes[1], values.data(), value_nulls.data());
    desc.measures[0].agg_mask |= (lanes & 1 ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                                (lanes & 2 ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                                (lanes & 4 ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
  }
  set_sql_where(&desc, where.data());
  if (filtered)
    set_sql_filter(&desc, filter.data());
  // Empty SharedArray inputs have no NULL sidecar. The ABI requires output
  // nullability to match the actual descriptor, even when no groups emit.
  const bool nullable_key = desc.keys[0].values.nulls != nullptr;
  struct State {
    uint64_t count = 0;
    int64_t sum = 0;
    int64_t min = INT64_MAX;
    int64_t max = INT64_MIN;
  };
  std::map<std::pair<bool, uint64_t>, State> expected;
  uint64_t selected = 0;
  for (size_t row = 0; row < rows; ++row) {
    if (host_where[row] != PGACCEL_EXPR_TRUE)
      continue;
    ++selected;
    auto& result = expected[{host_nulls[row] == 0, host_nulls[row] ? 0 : host_keys[row]}];
    if ((filtered && host_filter[row] != PGACCEL_EXPR_TRUE) ||
        (state != 0 && host_value_nulls[row]))
      continue;
    ++result.count;
    result.sum += host_values[row];
    result.min = std::min(result.min, static_cast<int64_t>(host_values[row]));
    result.max = std::max(result.max, static_cast<int64_t>(host_values[row]));
  }
  const auto req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  if (groups > 16'384 && !reject_all)
    CHECK(expected.size() > 16'384);  // Actually exercise parallel compact sorting.
  auto verify = [&](const HashOutput& output) {
    CHECK(output.out.emitted_group_count == expected.size());
    CHECK(output.out.selected_count == selected);
    CHECK(output.out.uncertain_count == 0);
    size_t index = 0;
    for (const auto& [key, result] : expected) {
      CHECK(output.keys[index] == key.second);
      CHECK(output.nulls[index] == static_cast<uint8_t>(!key.first));
      CHECK(output.counts[index] == result.count);
      if (lanes)
        CHECK(output.nonnulls[index] == result.count);
      if (lanes & 1)
        CHECK(static_cast<int64_t>(output.sums[index]) == result.sum);
      if (lanes & 2)
        CHECK(static_cast<int64_t>(output.mins[index]) == (result.count ? result.min : 0));
      if (lanes & 4)
        CHECK(static_cast<int64_t>(output.maxs[index]) == (result.count ? result.max : 0));
      ++index;
    }
  };
  int32_t detail = 0;
  if (rows == 0) {
    HashOutput mismatched(groups, !nullable_key, lanes & 1, lanes & 2, lanes & 4);
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &mismatched.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(mismatched.untouched());
  }
  for (unsigned repeat = 0; repeat < 2; ++repeat) {
    HashOutput output(groups, nullable_key, lanes & 1, lanes & 2, lanes & 4);
    const auto status = execute_with_workspace(desc, req, workspace, &output.out, &detail);
    if (status != PGACCEL_OK)
      std::fprintf(stderr,
                   "WHERE case: rows=%zu groups=%zu state=%u filtered=%d narrow=%d reject_all=%d "
                   "repeat=%u detail=%d\n",
                   rows, groups, state, filtered, narrow, reject_all, repeat, detail);
    CHECK_STATUS(status, PGACCEL_OK);
    verify(output);
  }
  if (rows != 0) {
    // A late malformed WHERE byte must suppress all output; the same scratch
    // must reset counters and recover on the next execution.
    where[rows - 1] = 2;
    HashOutput failed(groups, true, lanes & 1, lanes & 2, lanes & 4);
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(failed.untouched());
    where[rows - 1] = host_where[rows - 1];
    HashOutput retry(groups, true, lanes & 1, lanes & 2, lanes & 4);
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retry.out, &detail), PGACCEL_OK);
    verify(retry);
  }
}

void test_hash_where() {
  std::printf("--- hash WHERE states, independent FILTER, empty input and failure barriers ---\n");
  for (unsigned state = 0; state < 9; ++state)
    for (bool filtered : {false, true})
      for (bool narrow : {false, true}) {
        test_hash_where_values(0, 17, state, filtered, narrow, false);
        for (bool reject_all : {false, true})
          test_hash_where_values(51, 17, state, filtered, narrow, reject_all);
      }
  for (size_t rows : {size_t{1}, size_t{1'000'017}})
    test_hash_where_values(rows, 17, 8, true, false, false);
  test_hash_where_values(81'956, 20'489, 8, true, true, false);
}

void test_hash_where_order_and_failures() {
  SharedArray<uint64_t> keys(std::vector<uint64_t>{77, 88, 9, 9});
  SharedArray<uint8_t> key_nulls(std::vector<uint8_t>{0, 2, 0, 0});
  SharedArray<uint8_t> value_nulls(std::vector<uint8_t>{2, 2, 0, 1});
  SharedArray<int32_t> values(std::vector<int32_t>{INT32_MIN, INT32_MAX, 7, 5});
  SharedArray<int8_t> where(std::vector<int8_t>{PGACCEL_EXPR_FALSE, PGACCEL_EXPR_UNCERTAIN,
                                              PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE});
  SharedArray<int8_t> filter(std::vector<int8_t>{2, 2, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE});
  auto desc = hash_desc(keys.data(), key_nulls.data(), 4, 1);
  set_count_column(&desc, kCountColumnTypes[1], values.data(), value_nulls.data());
  desc.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                              PGACCEL_GROUPED_AGG_LANE_MAX;
  set_sql_where(&desc, where.data());
  set_sql_filter(&desc, filter.data());
  const auto req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  int32_t detail = 0;
  auto success = [&]() {
    HashOutput output(1, true, true, true, true);
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    CHECK(output.out.emitted_group_count == 1);
    CHECK(output.out.selected_count == 2);
    CHECK(output.keys[0] == 9 && output.nulls[0] == 0);
    CHECK(output.counts[0] == 1 && output.nonnulls[0] == 1);
    CHECK(output.sums[0] == 7 && output.mins[0] == 7 && output.maxs[0] == 7);
  };
  success();
  for (size_t row : {size_t{0}, size_t{1}}) {
    const int8_t saved = where[row];
    where[row] = PGACCEL_EXPR_TRUE;
    HashOutput failed(1, true, true, true, true);
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(failed.untouched());
    where[row] = saved;
    success();
  }
  // WHERE (unlike aggregate FILTER) can make a formerly over-capacity shape fit.
  key_nulls[1] = 0;
  value_nulls[0] = value_nulls[1] = 0;
  filter[0] = filter[1] = PGACCEL_EXPR_FALSE;
  where[0] = where[1] = PGACCEL_EXPR_TRUE;
  HashOutput capacity(1, true, true, true, true);
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &capacity.out, &detail), PGACCEL_UNSUPPORTED);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  CHECK(capacity.untouched());
  where[0] = where[1] = PGACCEL_EXPR_FALSE;
  success();

  // An output that overlaps even an entirely rejected mask is still forbidden.
  HashOutput aliased(1, true, true, true, true);
  auto alias_desc = desc;
  alias_desc.where_filter.mask = reinterpret_cast<const int8_t*>(aliased.keys.data());
  CHECK_STATUS(execute_with_workspace(alias_desc, req, workspace, &aliased.out, &detail), PGACCEL_ERROR);
  CHECK(aliased.untouched());

  const int digits[3] = {2, 4, 1};
  SharedArray<uint64_t> cells(std::vector<uint64_t>{0, make_h3_cell(20, 3, digits)});
  SharedArray<int8_t> h3_where(std::vector<int8_t>{PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE});
  auto h3 = hash_desc(cells.data(), nullptr, 2, 1);
  h3.keys[0].flags = PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT;
  h3.keys[0]._pad0 = 1;
  set_sql_where(&h3, h3_where.data());
  const auto h3_req = query_workspace(h3);
  SharedWorkspace h3_workspace(h3_req.bytes, h3_req.alignment);
  HashOutput h3_output(1, false);
  CHECK_STATUS(execute_with_workspace(h3, h3_req, h3_workspace, &h3_output.out, &detail), PGACCEL_OK);
  CHECK(h3_output.out.selected_count == 1 && h3_output.out.emitted_group_count == 1);
  CHECK(h3_output.keys[0] == h3_parent_for_test(cells[1], 1) && h3_output.counts[0] == 1);
  h3_where[0] = PGACCEL_EXPR_TRUE;
  HashOutput invalid_h3(1, false);
  CHECK_STATUS(execute_with_workspace(h3, h3_req, h3_workspace, &invalid_h3.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(invalid_h3.untouched());
}

void test_bool_mask_metadata() {
  CHECK_STATUS(pgaccel_bool_sql_mask(nullptr, nullptr, 0, nullptr), PGACCEL_OK);
  alignas(4) uint8_t bytes[16] = {};
  CHECK_STATUS(pgaccel_bool_sql_mask(nullptr, nullptr, 1, reinterpret_cast<int8_t*>(bytes)),
               PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(bytes, nullptr, 1, nullptr), PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(bytes, nullptr, 65537, reinterpret_cast<int8_t*>(bytes + 8)),
               PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(bytes, nullptr, 1, reinterpret_cast<int8_t*>(bytes + 9)),
               PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(bytes, nullptr, 8, reinterpret_cast<int8_t*>(bytes + 4)),
               PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(bytes, bytes + 8, 4, reinterpret_cast<int8_t*>(bytes + 8)),
               PGACCEL_ERROR);
  CHECK_STATUS(pgaccel_bool_sql_mask(reinterpret_cast<const uint8_t*>(UINTPTR_MAX - 1), nullptr, 4,
                                     reinterpret_cast<int8_t*>(bytes)),
               PGACCEL_ERROR);
}

void test_bool_mask_device() {
  std::printf("--- Boolean/NULL SQL mask exhaustive byte pairs and exact output spans ---\n");
  std::vector<uint8_t> host_values(65536), host_nulls(65536);
  for (size_t row = 0; row < host_values.size(); ++row) {
    host_values[row] = static_cast<uint8_t>(row % 256);
    host_nulls[row] = static_cast<uint8_t>(row / 256);
  }
  SharedArray<uint8_t> values(host_values), nulls(host_nulls);
  for (size_t rows : {size_t{1}, size_t{3}, size_t{4}, size_t{5}, size_t{65535}, size_t{65536}}) {
    for (bool nullable : {false, true}) {
      SharedArray<int8_t> output(std::vector<int8_t>(rows + 8, 99));
      CHECK_STATUS(pgaccel_bool_sql_mask(values.data(), nullable ? nulls.data() : nullptr, rows,
                                         output.data() + 4),
                   PGACCEL_OK);
      for (size_t row = 0; row < rows; ++row) {
        const uint8_t flag = nullable ? host_nulls[row] : 0;
        const int8_t expected = flag > 1                ? 2
                                : flag == 1             ? PGACCEL_EXPR_UNCERTAIN
                                : host_values[row] > 1  ? 2
                                : host_values[row] == 1 ? PGACCEL_EXPR_TRUE
                                                        : PGACCEL_EXPR_FALSE;
        CHECK(output[row + 4] == expected);
      }
      for (size_t guard = 0; guard < 4; ++guard) {
        CHECK(output[guard] == 99);
        CHECK(output[rows + 4 + guard] == 99);
      }
    }
  }
}

void test_filtered_count_metadata_contract() {
  std::printf("--- hash FILTER metadata contracts (protected row storage) ---\n");
  const long page_size = sysconf(_SC_PAGESIZE);
  CHECK(page_size > 0);
  if (page_size <= 0)
    return;
  void* page =
      mmap(nullptr, static_cast<size_t>(page_size), PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
  CHECK(page != MAP_FAILED);
  if (page == MAP_FAILED)
    return;
  for (size_t rows : {size_t{0}, size_t{17}, size_t{1'000'000}}) {
    for (size_t capacity : {size_t{1}, size_t{17}, size_t{4097}}) {
      // The last iteration is COUNT(*); the others exercise every column type.
      for (size_t type = 0; type <= std::size(kCountColumnTypes); ++type) {
        auto desc = hash_desc(rows == 0 ? nullptr : static_cast<const uint64_t*>(page), nullptr,
                              rows, capacity);
        if (type < std::size(kCountColumnTypes))
          set_count_column(&desc, kCountColumnTypes[type], rows == 0 ? nullptr : page,
                           static_cast<const uint8_t*>(page));
        pgaccel_status status = PGACCEL_ERROR;
        const auto baseline = query_workspace(desc, &status);
        CHECK_STATUS(status, PGACCEL_OK);
        set_sql_filter(&desc, static_cast<const int8_t*>(page));
        const auto req = query_workspace(desc, &status);
        CHECK_STATUS(status, PGACCEL_OK);
        CHECK(req.bytes == baseline.bytes);
        CHECK(req.alignment == baseline.alignment);
        auto narrow = desc;
        narrow.keys[0].values.type = PGACCEL_VAL_INT32;
        const auto narrow_req = query_workspace(narrow, &status);
        CHECK_STATUS(status, PGACCEL_OK);
        CHECK(narrow_req.bytes == req.bytes);
        int32_t mode = 0;
        CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
        CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH);

        auto rejected = desc;
        rejected.measure_filters[0].kind = PGACCEL_GROUPED_AGG_FILTER_RECHECK;
        query_workspace(rejected, &status);
        CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
        rejected = desc;
        rejected.measure_filters[0].mask = nullptr;
        query_workspace(rejected, &status);
        CHECK_STATUS(status, PGACCEL_ERROR);
        rejected = desc;
        rejected.measure_filters[0].flags = UINT32_C(0x80000000);
        query_workspace(rejected, &status);
        CHECK_STATUS(status, PGACCEL_ERROR);
        rejected = desc;
        rejected.where_filter = desc.measure_filters[0];
        CHECK(query_workspace(rejected, &status).bytes == req.bytes + 256 * sizeof(uint32_t));
        CHECK_STATUS(status, PGACCEL_OK);
        rejected = desc;
        rejected.execution_flags =
            PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        query_workspace(rejected, &status);
        CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
        if (type < std::size(kCountColumnTypes) &&
            kCountColumnTypes[type].type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32) {
          // This NULL-accepting scalar predicate is valid for the dense INT4
          // lane, but HASH admits only the canonical SQL-mask-only form.
          rejected = desc;
          rejected.measure_filters[0].flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
          query_workspace(rejected, &status);
          CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
        }
      }
    }
  }
  CHECK(munmap(page, static_cast<size_t>(page_size)) == 0);
}

void test_count_column_metadata_contract() {
  std::printf("--- hash COUNT(column) metadata contracts (no device initialization) ---\n");
  // Protected storage catches even a single accidental row/NULL payload read.
  // Metadata-only ABI queries must not need device registration or dereference
  // these pointers, regardless of the declared row count.
  const long page_size = sysconf(_SC_PAGESIZE);
  CHECK(page_size > 0);
  if (page_size <= 0)
    return;
  void* page =
      mmap(nullptr, static_cast<size_t>(page_size), PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
  CHECK(page != MAP_FAILED);
  if (page == MAP_FAILED)
    return;
  const auto* marker = static_cast<const uint64_t*>(page);
  const auto* null_marker = static_cast<const uint8_t*>(page);
  for (const auto type : kCountColumnTypes) {
    for (size_t rows : {size_t{0}, size_t{17}, size_t{1'000'000}}) {
      for (size_t capacity : {size_t{1}, size_t{17}, size_t{4097}}) {
        for (bool nullable : {false, true}) {
          auto desc = hash_desc(rows == 0 ? nullptr : marker, nullptr, rows, capacity);
          pgaccel_status status = PGACCEL_ERROR;
          const auto baseline = query_workspace(desc, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          set_count_column(&desc, type, rows == 0 ? nullptr : marker,
                           nullable ? null_marker : nullptr);
          const auto req = query_workspace(desc, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          CHECK(req.bytes == baseline.bytes);
          CHECK(req.alignment == baseline.alignment);
          int32_t mode = 0;
          CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
          CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH);

          auto invalid = desc;
          invalid.measures[0].value.element_bytes = 3;
          query_workspace(invalid, &status);
          CHECK_STATUS(status, PGACCEL_ERROR);
          if (rows != 0) {
            invalid = desc;
            invalid.measures[0].value.values = nullptr;
            query_workspace(invalid, &status);
            CHECK_STATUS(status, PGACCEL_ERROR);
          }
          invalid = desc;
          invalid.execution_flags =
              PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
          query_workspace(invalid, &status);
          CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
        }
      }
    }
  }
  auto unsupported = hash_desc(marker, nullptr, 1, 1);
  set_count_column(&unsupported, kCountColumnTypes[1], marker, nullptr);
  unsupported.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
  pgaccel_status status = PGACCEL_OK;
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_OK);
  set_count_column(&unsupported, kCountColumnTypes[2], marker, nullptr);
  unsupported.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
  unsupported.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  unsupported.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  unsupported.where_filter.mask = reinterpret_cast<const int8_t*>(null_marker);
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_OK);
  CHECK(munmap(page, static_cast<size_t>(page_size)) == 0);
}

std::vector<ExpectedGroup> count_column_oracle(const std::vector<uint64_t>& keys,
                                               const std::vector<uint8_t>& key_nulls,
                                               const std::vector<uint8_t>* value_nulls,
                                               const std::vector<int8_t>* filter = nullptr) {
  std::map<std::pair<bool, uint64_t>, uint64_t> groups;
  for (size_t row = 0; row < keys.size(); ++row) {
    const bool not_null = key_nulls[row] == 0;
    // false sorts the SQL NULL key first, independently of unsigned key bits.
    auto& count = groups[{not_null, not_null ? keys[row] : 0}];
    count += (filter == nullptr || (*filter)[row] == PGACCEL_EXPR_TRUE) &&
             (value_nulls == nullptr || (*value_nulls)[row] == 0);
  }
  std::vector<ExpectedGroup> expected;
  for (const auto& [key, count] : groups)
    expected.push_back({!key.first, key.second, count});
  return expected;
}

void test_hash_sum_metadata() {
  const long page_size = sysconf(_SC_PAGESIZE);
  CHECK(page_size > 0);
  void* page =
      mmap(nullptr, static_cast<size_t>(page_size), PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
  CHECK(page != MAP_FAILED);
  if (page == MAP_FAILED)
    return;
  for (size_t rows : {size_t{0}, size_t{1}, size_t{1'000'017}, size_t{UINT32_MAX}}) {
    for (size_t capacity : {size_t{1}, size_t{17}, size_t{16'385}}) {
      auto desc =
          hash_desc(rows ? static_cast<const uint64_t*>(page) : nullptr, nullptr, rows, capacity);
      set_count_column(&desc, kCountColumnTypes[1], rows ? page : nullptr,
                       static_cast<const uint8_t*>(page));
      pgaccel_status status = PGACCEL_ERROR;
      const auto count_req = query_workspace(desc, &status);
      CHECK_STATUS(status, PGACCEL_OK);
      desc.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
      const auto sum_req = query_workspace(desc, &status);
      CHECK_STATUS(status, PGACCEL_OK);
      CHECK(sum_req.bytes >= count_req.bytes + capacity * sizeof(uint64_t));
      for (unsigned states = 1; states < 8; ++states) {
        auto integer = desc;
        integer.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT |
                                       ((states & 1) ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                                       ((states & 2) ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                                       ((states & 4) ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
        const auto req = query_workspace(integer, &status);
        CHECK_STATUS(status, PGACCEL_OK);
        CHECK(req.bytes > count_req.bytes);
        auto narrow = integer;
        narrow.keys[0].values.type = PGACCEL_VAL_INT32;
        const auto narrow_req = query_workspace(narrow, &status);
        CHECK_STATUS(status, PGACCEL_OK);
        CHECK(narrow_req.bytes == req.bytes);
        CHECK(narrow_req.alignment == req.alignment);
        narrow.keys[0].flags = PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT;
        query_workspace(narrow, &status);
        CHECK_STATUS(status, PGACCEL_ERROR);
        auto masked = integer;
        set_sql_filter(&masked, static_cast<const int8_t*>(page));
        CHECK(query_workspace(masked, &status).bytes == req.bytes);
        CHECK_STATUS(status, PGACCEL_OK);
        integer.measures[0].agg_mask &= ~PGACCEL_GROUPED_AGG_LANE_COUNT;
        query_workspace(integer, &status);
        CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
      }
      auto filtered = desc;
      set_sql_filter(&filtered, static_cast<const int8_t*>(page));
      const auto filtered_req = query_workspace(filtered, &status);
      CHECK_STATUS(status, PGACCEL_OK);
      CHECK(filtered_req.bytes == sum_req.bytes);
      for (const auto type : kCountColumnTypes) {
        if (type.type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32)
          continue;
        auto rejected = desc;
        set_count_column(&rejected, type, rows ? page : nullptr, nullptr);
        query_workspace(rejected, &status);
        CHECK_STATUS(status, (type.type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32 ||
                              type.type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64)
                                 ? PGACCEL_ERROR
                                 : PGACCEL_UNSUPPORTED);
      }
      auto rejected = desc;
      rejected.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_SUM;
      query_workspace(rejected, &status);
      CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
      rejected = desc;
      rejected.execution_flags =
          PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      query_workspace(rejected, &status);
      CHECK_STATUS(status, PGACCEL_UNSUPPORTED);
    }
  }
  CHECK(munmap(page, static_cast<size_t>(page_size)) == 0);
}

void test_hash_integer_sums() {
  for (size_t groups : {size_t{17}, size_t{16'385}}) {
    for (unsigned value_pattern : {0u, 1u, 2u}) {
      const size_t rows = groups == 17 ? 1'000'017 : groups * 3 + 1;
      std::vector<uint64_t> host_keys(rows);
      std::vector<uint8_t> host_key_nulls(rows), host_nulls(rows);
      std::vector<int32_t> host_values(rows);
      std::vector<int8_t> host_mask(rows);
      for (size_t row = 0; row < rows; ++row) {
        const size_t group = row % groups;
        host_keys[row] = (UINT64_C(1) << 63) | static_cast<uint64_t>(groups - group);
        host_key_nulls[row] = group == 0;
        host_values[row] = value_pattern == 1
                               ? INT32_MAX
                               : (value_pattern == 2 || row % 2 ? INT32_MIN : INT32_MAX);
        host_nulls[row] = group == 1 || row % 7 == 0;
        host_mask[row] = group == 2 ? PGACCEL_EXPR_FALSE
                                    : (row % 5 == 0 ? PGACCEL_EXPR_UNCERTAIN : PGACCEL_EXPR_TRUE);
      }
      SharedArray<uint64_t> keys(host_keys);
      SharedArray<uint8_t> key_nulls(host_key_nulls), nulls(host_nulls);
      SharedArray<int32_t> values(host_values);
      SharedArray<int8_t> mask(host_mask);
      for (bool filtered : {false, true}) {
        auto desc = hash_desc(keys.data(), key_nulls.data(), rows, groups);
        set_count_column(&desc, kCountColumnTypes[1], values.data(), nulls.data());
        desc.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
        if (filtered)
          set_sql_filter(&desc, mask.data());
        std::map<std::pair<bool, uint64_t>, std::pair<int64_t, uint64_t>> expected;
        for (size_t row = 0; row < rows; ++row) {
          auto& state =
              expected[{host_key_nulls[row] == 0, host_key_nulls[row] ? 0 : host_keys[row]}];
          if (!host_nulls[row] && (!filtered || host_mask[row] == PGACCEL_EXPR_TRUE)) {
            state.first += host_values[row];
            ++state.second;
          }
        }
        const auto req = query_workspace(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        auto verify = [&](const HashOutput& out) {
          CHECK(out.out.emitted_group_count == expected.size());
          CHECK(out.out.selected_count == rows);
          CHECK(out.out.uncertain_count == 0);
          size_t index = 0;
          for (const auto& [key, state] : expected) {
            CHECK(out.keys[index] == key.second);
            CHECK(out.nulls[index] == static_cast<uint8_t>(!key.first));
            CHECK(out.counts[index] == state.second);
            CHECK(out.nonnulls[index] == state.second);
            CHECK(static_cast<int64_t>(out.sums[index]) == state.first);
            ++index;
          }
        };
        int32_t detail = -1;
        for (unsigned attempt = 0; attempt < 2; ++attempt) {
          HashOutput output(groups, true, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                       PGACCEL_OK);
          CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
          verify(output);
        }
        // A late evaluated NULL corruption poisons every output lane; retry
        // must reset both count and sum buckets, including carry/high words.
        const size_t corrupt = rows - 1;
        nulls[corrupt] = 2;
        if (filtered)
          mask[corrupt] = PGACCEL_EXPR_TRUE;
        HashOutput failed(groups, true, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail),
                     PGACCEL_ERROR);
        CHECK(failed.untouched());
        nulls[corrupt] = host_nulls[corrupt];
        mask[corrupt] = host_mask[corrupt];
        HashOutput retry(groups, true, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retry.out, &detail), PGACCEL_OK);
        verify(retry);
        auto small = desc;
        small.group_capacity = groups - 1;
        const auto small_req = query_workspace(small);
        HashOutput too_small(groups - 1, true, true);
        CHECK_STATUS(execute_with_workspace(small, small_req, workspace, &too_small.out, &detail),
                     PGACCEL_UNSUPPORTED);
        CHECK(too_small.untouched());
        auto host_input = desc;
        host_input.measures[0].value.values = host_values.data();
        HashOutput rejected(groups, true, true);
        CHECK_STATUS(execute_with_workspace(host_input, req, workspace, &rejected.out, &detail),
                     PGACCEL_ERROR);
        CHECK(rejected.untouched());
        if (filtered) {
          // Rejected rows do not inspect corrupt argument NULL bytes; the same
          // corruption must fail if the mask changes to TRUE.
          nulls[2] = 2;
          HashOutput suppressed(groups, true, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &suppressed.out, &detail),
                       PGACCEL_OK);
          verify(suppressed);
          nulls[2] = host_nulls[2];
          mask[rows - 1] = 2;
          HashOutput bad_mask(groups, true, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &bad_mask.out, &detail),
                       PGACCEL_ERROR);
          CHECK(bad_mask.untouched());
          mask[rows - 1] = host_mask[rows - 1];
        }
        HashOutput aliased(groups, true, true);
        aliased.out.measures[0].sum = aliased.counts.data();
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &aliased.out, &detail),
                     PGACCEL_ERROR);
        CHECK(aliased.untouched());
      }
    }
  }
  auto empty = hash_desc(nullptr, nullptr, 0, 1);
  set_count_column(&empty, kCountColumnTypes[1], nullptr, nullptr);
  empty.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
  const auto req = query_workspace(empty);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(1, false, true);
  int32_t detail = -1;
  CHECK_STATUS(execute_with_workspace(empty, req, workspace, &output.out, &detail), PGACCEL_OK);
  CHECK(output.untouched());
}

void test_hash_integer_extrema() {
  std::printf("--- hash INT32 extrema: subsets, sort alignment, NULLs and failure barriers ---\n");
  struct State {
    int64_t sum = 0;
    int32_t minimum = INT32_MAX;
    int32_t maximum = INT32_MIN;
    uint64_t count = 0;
  };
  for (bool narrow_key : {false, true}) {
    for (size_t groups : {size_t{17}, size_t{16'385}}) {
      const size_t rows = groups == 17 ? 1'000'017 : groups * 5;
      std::vector<uint64_t> host_keys(rows);
      std::vector<int32_t> host_narrow_keys(rows);
      std::vector<uint8_t> host_key_nulls(rows), host_nulls(rows);
      std::vector<int32_t> host_values(rows);
      std::vector<int8_t> host_mask(rows);
      for (size_t row = 0; row < rows; ++row) {
        const size_t group = row % groups;
        host_keys[row] = UINT64_MAX - group;
        host_narrow_keys[row] =
            group == 1   ? INT32_MIN
            : group == 2 ? INT32_MAX
                         : static_cast<int32_t>(group) - static_cast<int32_t>(groups / 2);
        if (narrow_key)
          host_keys[row] = static_cast<uint64_t>(static_cast<int64_t>(host_narrow_keys[row]));
        host_key_nulls[row] = group == 0;
        host_nulls[row] = group == 1 || row % 11 == 0;
        // Distinct per-group extrema catch lost key/state alignment in sorting;
        // separate groups retain both signed endpoints and all-NULL behavior.
        host_values[row] = group % 5 == 0 ? INT32_MIN
                           : group % 5 == 1
                               ? INT32_MAX
                               : static_cast<int32_t>(group * 37 + (row / groups) % 5) - 400000;
        host_mask[row] = group == 2     ? PGACCEL_EXPR_FALSE
                         : row % 7 == 0 ? PGACCEL_EXPR_UNCERTAIN
                                        : PGACCEL_EXPR_TRUE;
      }
      SharedArray<uint64_t> keys(host_keys);
      SharedArray<int32_t> narrow_keys(host_narrow_keys);
      SharedArray<uint8_t> key_nulls(host_key_nulls), nulls(host_nulls);
      SharedArray<int32_t> values(host_values);
      SharedArray<int8_t> masks(host_mask);
      for (bool filtered : {false, true}) {
        std::map<std::pair<bool, uint64_t>, State> expected;
        for (size_t row = 0; row < rows; ++row) {
          auto& state =
              expected[{host_key_nulls[row] == 0, host_key_nulls[row] ? 0 : host_keys[row]}];
          if (!host_nulls[row] && (!filtered || host_mask[row] == PGACCEL_EXPR_TRUE)) {
            ++state.count;
            state.sum += host_values[row];
            state.minimum = std::min(state.minimum, host_values[row]);
            state.maximum = std::max(state.maximum, host_values[row]);
          }
        }
        for (unsigned states = 1; states < 8; ++states) {
          const bool sum = (states & 1) != 0;
          const bool minimum = (states & 2) != 0;
          const bool maximum = (states & 4) != 0;
          auto desc = hash_desc(keys.data(), key_nulls.data(), rows, groups);
          if (narrow_key) {
            desc.keys[0].values.values = narrow_keys.data();
            desc.keys[0].values.type = PGACCEL_VAL_INT32;
          }
          set_count_column(&desc, kCountColumnTypes[1], values.data(), nulls.data());
          desc.measures[0].agg_mask |= (sum ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                                       (minimum ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                                       (maximum ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
          if (filtered)
            set_sql_filter(&desc, masks.data());
          pgaccel_status status = PGACCEL_ERROR;
          const auto req = query_workspace(desc, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          SharedWorkspace workspace(req.bytes, req.alignment);
          auto verify = [&](const HashOutput& output) {
            CHECK(output.out.emitted_group_count == expected.size());
            CHECK(output.out.selected_count == rows);
            size_t index = 0;
            for (const auto& [key, state] : expected) {
              CHECK(output.keys[index] == key.second);
              CHECK(output.nulls[index] == static_cast<uint8_t>(!key.first));
              CHECK(output.counts[index] == state.count);
              CHECK(output.nonnulls[index] == state.count);
              if (sum)
                CHECK(static_cast<int64_t>(output.sums[index]) == state.sum);
              if (minimum)
                CHECK(static_cast<int64_t>(output.mins[index]) ==
                      (state.count ? state.minimum : 0));
              if (maximum)
                CHECK(static_cast<int64_t>(output.maxs[index]) ==
                      (state.count ? state.maximum : 0));
              ++index;
            }
          };
          int32_t detail = -1;
          for (unsigned attempt = 0; attempt < 2; ++attempt) {
            HashOutput output(groups, true, sum, minimum, maximum);
            CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                         PGACCEL_OK);
            CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
            verify(output);
          }
          const size_t corrupt = rows - 1;
          nulls[corrupt] = 2;
          masks[corrupt] = PGACCEL_EXPR_TRUE;
          HashOutput failed(groups, true, sum, minimum, maximum);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail),
                       PGACCEL_ERROR);
          CHECK(failed.untouched());
          nulls[corrupt] = host_nulls[corrupt];
          masks[corrupt] = host_mask[corrupt];
          HashOutput retry(groups, true, sum, minimum, maximum);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retry.out, &detail),
                       PGACCEL_OK);
          verify(retry);
          HashOutput alias(groups, true, sum, minimum, maximum);
          if (sum)
            alias.out.measures[0].sum = alias.counts.data();
          else if (minimum)
            alias.out.measures[0].min = alias.counts.data();
          else
            alias.out.measures[0].max = alias.counts.data();
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &alias.out, &detail),
                       PGACCEL_ERROR);
          CHECK(alias.untouched());
          auto small = desc;
          small.group_capacity = groups - 1;
          const auto small_req = query_workspace(small, &status);
          CHECK_STATUS(status, PGACCEL_OK);
          HashOutput overflow(groups - 1, true, sum, minimum, maximum);
          CHECK_STATUS(execute_with_workspace(small, small_req, workspace, &overflow.out, &detail),
                       PGACCEL_UNSUPPORTED);
          CHECK(overflow.untouched());
        }
      }
    }
  }
}

void test_hash_empty_extrema() {
  for (unsigned states = 2; states < 8; ++states) {
    const bool sum = (states & 1) != 0;
    const bool minimum = (states & 2) != 0;
    const bool maximum = (states & 4) != 0;
    auto desc = hash_desc(nullptr, nullptr, 0, 1);
    set_count_column(&desc, kCountColumnTypes[1], nullptr, nullptr);
    desc.measures[0].agg_mask |= (sum ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                                 (minimum ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                                 (maximum ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
    pgaccel_status status = PGACCEL_ERROR;
    const auto req = query_workspace(desc, &status);
    CHECK_STATUS(status, PGACCEL_OK);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(1, false, sum, minimum, maximum);
    int32_t detail = -1;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    CHECK(output.untouched());
  }
}

void test_narrow_key_counts() {
  for (size_t groups : {size_t{17}, size_t{16'385}}) {
    const size_t rows = groups * 5;
    std::vector<int32_t> host_keys(rows);
    std::vector<uint64_t> wide_keys(rows);
    std::vector<uint8_t> host_nulls(rows), host_value_nulls(rows);
    std::vector<int8_t> host_mask(rows);
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = group == 1 ? INT32_MIN
                       : group == 2
                           ? INT32_MAX
                           : static_cast<int32_t>(group) - static_cast<int32_t>(groups / 2);
      wide_keys[row] = static_cast<uint64_t>(static_cast<int64_t>(host_keys[row]));
      host_nulls[row] = group == 0;
      host_value_nulls[row] = row % 3 == 0 || group == 3;
      host_mask[row] = group == 4     ? PGACCEL_EXPR_FALSE
                       : row % 7 == 0 ? PGACCEL_EXPR_UNCERTAIN
                                      : PGACCEL_EXPR_TRUE;
    }
    SharedArray<int32_t> keys(host_keys);
    SharedArray<uint8_t> nulls(host_nulls), value_nulls(host_value_nulls);
    SharedArray<int8_t> mask(host_mask);
    for (bool column : {false, true}) {
      for (bool filtered : {false, true}) {
        auto desc = hash_desc(nullptr, nulls.data(), rows, groups);
        desc.keys[0].values.values = keys.data();
        desc.keys[0].values.type = PGACCEL_VAL_INT32;
        if (column)
          set_count_column(&desc, kCountColumnTypes[1], keys.data(), value_nulls.data());
        if (filtered)
          set_sql_filter(&desc, mask.data());
        const auto expected =
            count_column_oracle(wide_keys, host_nulls, column ? &host_value_nulls : nullptr,
                                filtered ? &host_mask : nullptr);
        const auto req = query_workspace(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        int32_t detail = -1;
        for (unsigned attempt = 0; attempt < 2; ++attempt) {
          HashOutput output(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                       PGACCEL_OK);
          check_success(output, expected, rows);
        }
        HashOutput wrong_width(groups, true);
        wrong_width.out.keys[0].type = PGACCEL_VAL_INT32;
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &wrong_width.out, &detail),
                     PGACCEL_ERROR);
        CHECK(wrong_width.untouched());
        nulls[rows - 1] = 2;
        HashOutput bad_null(groups, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &bad_null.out, &detail),
                     PGACCEL_ERROR);
        CHECK(bad_null.untouched());
        nulls[rows - 1] = host_nulls[rows - 1];
        HashOutput retry(groups, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retry.out, &detail), PGACCEL_OK);
        check_success(retry, expected, rows);
      }
    }
  }
}

void test_filtered_counts() {
  std::printf("--- hash FILTER preserves zero-count groups, NULLs, failures and retries ---\n");
  // The parallel compact/merge threshold is 16,384, not 4,096. Cross it with
  // actual emitted cardinality as well as capacity so the device fast path
  // cannot silently reduce both fixtures to the serial sorter.
  for (size_t groups : {size_t{17}, size_t{16'385}}) {
    const size_t rows = groups * 5;
    std::vector<uint64_t> host_keys(rows);
    std::vector<uint8_t> host_nulls(rows), value_flags(rows);
    for (size_t row = 0; row < rows; ++row) {
      host_keys[row] = UINT64_C(0x8000000000000000) + row % groups;
      host_nulls[row] = row % groups == 0;
      value_flags[row] = row % 3 == 0 || row % groups == 2;
    }
    SharedArray<uint64_t> keys(host_keys);
    SharedArray<uint8_t> nulls(host_nulls), value_nulls(value_flags);
    SharedArray<uint64_t> values(std::vector<uint64_t>(rows, UINT64_MAX));
    for (size_t type = 0; type <= std::size(kCountColumnTypes); ++type) {
      const bool column = type < std::size(kCountColumnTypes);
      for (unsigned pattern = 0; pattern < 4; ++pattern) {
        std::vector<int8_t> flags(rows);
        for (size_t row = 0; row < rows; ++row) {
          flags[row] = pattern == 0        ? PGACCEL_EXPR_TRUE
                       : pattern == 1      ? PGACCEL_EXPR_FALSE
                       : pattern == 2      ? PGACCEL_EXPR_UNCERTAIN
                       : row % groups == 1 ? PGACCEL_EXPR_FALSE
                       : row % 3 == 0      ? PGACCEL_EXPR_TRUE
                       : row % 3 == 1      ? PGACCEL_EXPR_FALSE
                                           : PGACCEL_EXPR_UNCERTAIN;
        }
        SharedArray<int8_t> mask(flags);
        auto desc = hash_desc(keys.data(), nulls.data(), rows, groups);
        if (column)
          set_count_column(&desc, kCountColumnTypes[type], values.data(),
                           pattern == 0 ? nullptr : value_nulls.data());
        set_sql_filter(&desc, mask.data());
        const auto req = query_workspace(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        const auto expected = count_column_oracle(
            host_keys, host_nulls, column && pattern != 0 ? &value_flags : nullptr, &flags);
        int32_t detail = -1;
        for (unsigned warm = 0; warm < 2; ++warm) {
          HashOutput output(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                       PGACCEL_OK);
          CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
          check_success(output, expected, rows);
        }

        if (pattern == 1) {
          // FILTER cannot reduce the number of groups or rescue short capacity.
          auto short_desc = desc;
          short_desc.group_capacity = groups - 1;
          const auto short_req = query_workspace(short_desc);
          HashOutput failed(groups - 1, true);
          CHECK_STATUS(
              execute_with_workspace(short_desc, short_req, workspace, &failed.out, &detail),
              PGACCEL_UNSUPPORTED);
          CHECK(failed.untouched());
        }

        if (pattern == 0) {
          // A valid mask value is not enough: the lane must be device-visible
          // and disjoint from the output. Neither rejection may publish data.
          auto invalid_pointer = desc;
          invalid_pointer.measure_filters[0].mask = flags.data();
          HashOutput host_mask(groups, true);
          CHECK_STATUS(
              execute_with_workspace(invalid_pointer, req, workspace, &host_mask.out, &detail),
              PGACCEL_ERROR);
          CHECK(host_mask.untouched());
          HashOutput overlapping(groups, true);
          invalid_pointer.measure_filters[0].mask =
              reinterpret_cast<const int8_t*>(overlapping.counts.data());
          CHECK_STATUS(
              execute_with_workspace(invalid_pointer, req, workspace, &overlapping.out, &detail),
              PGACCEL_ERROR);
          CHECK(overlapping.untouched());
        }

        // Noncanonical masks poison publication even when the measure is NULL.
        for (int8_t bad : {int8_t{2}, int8_t{-2}, int8_t{127}, int8_t{-128}}) {
          mask[0] = bad;
          HashOutput failed(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail),
                       PGACCEL_ERROR);
          CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
          CHECK(failed.untouched());
        }
        mask[0] = flags[0];
        if (column && pattern == 1) {
          // SQL FILTER runs before the argument: rejected measure bytes are
          // not read, but turning the filter true must expose the same error.
          value_nulls[0] = 2;
          HashOutput rejected(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &rejected.out, &detail),
                       PGACCEL_OK);
          check_success(rejected, expected, rows);
          mask[0] = PGACCEL_EXPR_TRUE;
          HashOutput failed(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail),
                       PGACCEL_ERROR);
          CHECK(failed.untouched());
          value_nulls[0] = value_flags[0];
          mask[0] = flags[0];
        }
        HashOutput retry(groups, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retry.out, &detail), PGACCEL_OK);
        check_success(retry, expected, rows);
        CHECK(std::equal(flags.begin(), flags.end(), mask.data()));
      }
    }
  }
  // Empty inputs still require canonical metadata but never read the mask.
  SharedArray<int8_t> empty_mask(std::vector<int8_t>{2});
  auto empty = hash_desc(nullptr, nullptr, 0, 1);
  set_sql_filter(&empty, empty_mask.data());
  const auto req = query_workspace(empty);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(1, false);
  int32_t detail = -1;
  CHECK_STATUS(execute_with_workspace(empty, req, workspace, &output.out, &detail), PGACCEL_OK);
  check_success(output, {}, 0);
}

void test_nullable_count_columns() {
  std::printf("--- hash COUNT(column) exact NULL groups, types, merge sort, and failures ---\n");
  for (const auto type : kCountColumnTypes) {
    auto empty = hash_desc(nullptr, nullptr, 0, 1);
    set_count_column(&empty, type, nullptr, nullptr);
    const auto req = query_workspace(empty);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(1, false);
    int32_t detail = -1;
    CHECK_STATUS(execute_with_workspace(empty, req, workspace, &output.out, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    check_success(output, {}, 0);
  }
  for (size_t groups : {size_t{17}, size_t{4097}}) {
    const size_t rows = groups * 3;
    std::vector<uint64_t> host_keys(rows);
    std::vector<uint8_t> host_key_nulls(rows);
    for (size_t row = 0; row < rows; ++row) {
      host_keys[row] = UINT64_C(0x8000000000000000) + row % groups;
      host_key_nulls[row] = row % groups == 0 ? 1 : 0;
    }
    SharedArray<uint64_t> keys(host_keys);
    SharedArray<uint8_t> key_nulls(host_key_nulls);
    // Payload bits are immaterial to COUNT, even NaNs and large integers.
    SharedArray<uint64_t> values(std::vector<uint64_t>(rows, UINT64_MAX));
    for (const auto type : kCountColumnTypes) {
      for (unsigned pattern = 0; pattern < 4; ++pattern) {
        std::vector<uint8_t> flags(rows);
        for (size_t row = 0; row < rows; ++row)
          flags[row] =
              pattern == 1 || (pattern == 2 && row % 2 == 0) || (pattern == 3 && row % groups <= 2);
        SharedArray<uint8_t> nulls(flags);
        auto desc = hash_desc(keys.data(), key_nulls.data(), rows, groups);
        set_count_column(&desc, type, values.data(), pattern == 0 ? nullptr : nulls.data());
        const auto req = query_workspace(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        HashOutput output(groups, true);
        int32_t detail = -1;
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail),
                     PGACCEL_OK);
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
        check_success(
            output, count_column_oracle(host_keys, host_key_nulls, pattern == 0 ? nullptr : &flags),
            rows);

        if (pattern == 1) {
          auto too_small = desc;
          too_small.group_capacity = groups - 1;
          const auto small_req = query_workspace(too_small);
          HashOutput failed(groups - 1, true);
          CHECK_STATUS(
              execute_with_workspace(too_small, small_req, workspace, &failed.out, &detail),
              PGACCEL_UNSUPPORTED);
          CHECK(failed.untouched());
        }

        // Invalid NULL bytes must poison publication, even for all-zero counts.
        if (pattern != 0) {
          nulls[rows / 2] = 2;
          HashOutput failed(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &failed.out, &detail),
                       PGACCEL_ERROR);
          CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
          CHECK(failed.untouched());
          nulls[rows / 2] = flags[rows / 2];
          HashOutput retried(groups, true);
          CHECK_STATUS(execute_with_workspace(desc, req, workspace, &retried.out, &detail),
                       PGACCEL_OK);
          check_success(retried, count_column_oracle(host_keys, host_key_nulls, &flags), rows);
        }
      }
    }
  }
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

void test_h3_planner_workspace_budget() {
  // Match Rust's compact H3 COUNT capacity policy. Probe power-of-two
  // boundaries because the hash owner/count table rounds up independently.
  SharedArray<uint64_t> key(std::vector<uint64_t>({1}));
  for (size_t capacity :
       {size_t{1}, size_t{123}, size_t{1025}, size_t{65537}, size_t{500000}, size_t{524289},
        size_t{1000000}, size_t{1'048'577}, size_t{12'748'986}, size_t{16'777'216}}) {
    auto desc = hash_desc(key.data(), nullptr, 1, capacity);
    pgaccel_status status = PGACCEL_ERROR;
    const auto req = query_workspace(desc, &status);
    CHECK_STATUS(status, PGACCEL_OK);
    // Output: one uint64 key, one null byte, one uint64 COUNT, plus a
    // conservative size_t group-code lane. Fixed overhead is budgeted too.
    const size_t output_bytes = capacity * (8 + 1 + 8 + sizeof(size_t));
    CHECK(req.bytes + output_bytes <= capacity * 128 + 65536);
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
  constexpr size_t rows = 1'000'017;  // Cross the per-launch row bound.
  std::vector<uint64_t> host_keys(rows, UINT64_C(0x08001fffffffffff));
  std::vector<uint8_t> host_nulls(rows);
  for (size_t row = 0; row < rows; ++row)
    host_nulls[row] = row % 97 == 0 ? 1 : 0;

  SharedArray<uint64_t> keys(host_keys);
  SharedArray<uint8_t> nulls(host_nulls);
  std::vector<int8_t> flags(rows, PGACCEL_EXPR_FALSE);
  // Only the second launch contributes: a missing offset or an accidentally
  // repeated first-launch mask would return zero for this fixture.
  std::fill(flags.begin() + 1'000'000, flags.end(), PGACCEL_EXPR_TRUE);
  SharedArray<int8_t> mask(flags);
  for (size_t capacity : {size_t{123}, size_t{1'000'000}}) {
    pgaccel_grouped_agg_desc desc = hash_desc(keys.data(), nulls.data(), rows, capacity);
    const pgaccel_grouped_agg_workspace_req req = query_workspace(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    HashOutput output(desc.group_capacity, true);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    check_success(output, oracle(host_keys, &host_nulls), rows);
    for (bool column : {false, true}) {
      auto filtered = desc;
      if (column)
        set_count_column(&filtered, kCountColumnTypes[2], keys.data(), nulls.data());
      set_sql_filter(&filtered, mask.data());
      const auto filtered_req = query_workspace(filtered);
      CHECK(filtered_req.bytes == req.bytes);
      HashOutput filtered_output(capacity, true);
      CHECK_STATUS(
          execute_with_workspace(filtered, filtered_req, workspace, &filtered_output.out, &detail),
          PGACCEL_OK);
      check_success(
          filtered_output,
          count_column_oracle(host_keys, host_nulls, column ? &host_nulls : nullptr, &flags), rows);
      mask[rows - 1] = 2;
      HashOutput late_failure(capacity, true);
      CHECK_STATUS(
          execute_with_workspace(filtered, filtered_req, workspace, &late_failure.out, &detail),
          PGACCEL_ERROR);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
      CHECK(late_failure.untouched());
      mask[rows - 1] = flags[rows - 1];
    }
  }
}

void test_parallel_compaction_and_merge_sort() {
  std::printf("--- parallel hash compaction/sort, non-power-of-two cardinalities ---\n");
  for (size_t distinct : {size_t{16'383}, size_t{16'385}, size_t{32'771}, size_t{1'048'583}}) {
    const size_t rows = distinct * 2 + 3;
    std::vector<uint64_t> host_keys(rows);
    std::vector<uint8_t> host_nulls(rows);
    for (size_t row = 0; row < rows; ++row) {
      // Reverse order with keys on both sides of the signed boundary.
      const uint64_t value = distinct - 1 - row % distinct;
      host_keys[row] = value * 2 | ((value % 2) << 63);
      host_nulls[row] = row % distinct == 0 ? 1 : 0;
    }
    SharedArray<uint64_t> keys(host_keys);
    SharedArray<uint8_t> nulls(host_nulls);
    for (bool nullable : {false, true}) {
      std::printf("parallel case: groups=%zu nullable=%d\n", distinct, nullable);
      std::fflush(stdout);
      auto desc = hash_desc(keys.data(), nullable ? nulls.data() : nullptr, rows, distinct);
      const auto req = query_workspace(desc);
      SharedWorkspace workspace(req.bytes, req.alignment);
      HashOutput output(distinct, nullable);
      int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
      CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      check_success(output, oracle(host_keys, nullable ? &host_nulls : nullptr), rows);
      // Reusing scratch must not retain old merge lanes or occupied slots.
      HashOutput repeat(distinct, nullable);
      CHECK_STATUS(execute_with_workspace(desc, req, workspace, &repeat.out, &detail), PGACCEL_OK);
      check_success(repeat, actual_groups(output), rows);
      if (distinct == 32'771 && nullable) {
        auto small = desc;
        small.group_capacity = 16'385;
        const auto small_req = query_workspace(small);
        SharedWorkspace small_workspace(small_req.bytes, small_req.alignment);
        HashOutput overflow(small.group_capacity, true);
        CHECK_STATUS(
            execute_with_workspace(small, small_req, small_workspace, &overflow.out, &detail),
            PGACCEL_UNSUPPORTED);
        CHECK(overflow.untouched());
        nulls[rows - 1] = 2;
        HashOutput invalid(distinct, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &invalid.out, &detail),
                     PGACCEL_ERROR);
        CHECK(invalid.untouched());
        nulls[rows - 1] = 0;
        HashOutput recovered(distinct, true);
        CHECK_STATUS(execute_with_workspace(desc, req, workspace, &recovered.out, &detail),
                     PGACCEL_OK);
        check_success(recovered, oracle(host_keys, &host_nulls), rows);
      }
    }
  }
}

void test_large_capacity_full_cardinality() {
  std::printf("--- hash qualification ceiling: 16Mi distinct groups including NULL ---\n");
  std::fflush(stdout);
  constexpr size_t rows = 16 * 1024 * 1024;
  SharedArray<uint64_t> keys(rows);
  SharedArray<uint8_t> nulls(rows);
  for (size_t row = 0; row < rows; ++row) {
    keys[row] = UINT64_MAX - row;
    nulls[row] = row == 0 ? 1 : 0;
  }
  auto desc = hash_desc(keys.data(), nulls.data(), rows, rows);
  const auto req = query_workspace(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  HashOutput output(rows, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_with_workspace(desc, req, workspace, &output.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  CHECK(output.out.emitted_group_count == rows);
  CHECK(output.out.selected_count == rows);
  bool exact = output.nulls[0] == 1 && output.keys[0] == 0 && output.counts[0] == 1;
  for (size_t group = 1; group < rows; ++group)
    exact = exact && output.nulls[group] == 0 && output.counts[group] == 1 &&
            output.keys[group] == UINT64_MAX - rows + group;
  CHECK(exact);
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

  const std::vector<uint8_t> measure_flags = {1, 0, 1, 0, 1, 1};
  SharedArray<uint8_t> measure_nulls(measure_flags);
  auto counted = desc;
  set_count_column(&counted, kCountColumnTypes[2], cells.data(), measure_nulls.data());
  const auto counted_req = query_workspace(counted, &status);
  CHECK_STATUS(status, PGACCEL_OK);
  CHECK(counted_req.bytes == req.bytes);
  HashOutput column_output(desc.group_capacity, true);
  CHECK_STATUS(execute_with_workspace(counted, counted_req, workspace, &column_output.out, &detail),
               PGACCEL_OK);
  check_success(column_output, count_column_oracle(expected_keys, host_nulls, &measure_flags),
                host_cells.size());
  const std::vector<int8_t> filter_flags = {0, 1, -1, 0, 1, -1};
  SharedArray<int8_t> filter_mask(filter_flags);
  SharedArray<int32_t> sum_values(std::vector<int32_t>(host_cells.size(), 7));
  for (bool filtered : {false, true}) {
    auto summed = desc;
    set_count_column(&summed, kCountColumnTypes[1], sum_values.data(), measure_nulls.data());
    summed.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
    if (filtered)
      set_sql_filter(&summed, filter_mask.data());
    const auto sum_req = query_workspace(summed);
    SharedWorkspace sum_workspace(sum_req.bytes, sum_req.alignment);
    HashOutput sum_output(summed.group_capacity, true, true);
    CHECK_STATUS(execute_with_workspace(summed, sum_req, sum_workspace, &sum_output.out, &detail),
                 PGACCEL_OK);
    const auto expected = count_column_oracle(expected_keys, host_nulls, &measure_flags,
                                              filtered ? &filter_flags : nullptr);
    check_success(sum_output, expected, host_cells.size());
    for (size_t i = 0; i < expected.size(); ++i) {
      CHECK(sum_output.sums[i] == expected[i].count * 7);
      CHECK(sum_output.nonnulls[i] == expected[i].count);
    }
    cells[0] = 0;  // NULL/rejected measure cannot suppress H3 grouping errors.
    HashOutput failed_sum(summed.group_capacity, true, true);
    CHECK_STATUS(execute_with_workspace(summed, sum_req, sum_workspace, &failed_sum.out, &detail),
                 PGACCEL_ERROR);
    CHECK(failed_sum.untouched());
    cells[0] = host_cells[0];
  }
  for (bool column : {false, true}) {
    auto filtered = column ? counted : desc;
    set_sql_filter(&filtered, filter_mask.data());
    const auto filtered_req = query_workspace(filtered);
    CHECK(filtered_req.bytes == req.bytes);
    HashOutput filtered_output(desc.group_capacity, true);
    CHECK_STATUS(
        execute_with_workspace(filtered, filtered_req, workspace, &filtered_output.out, &detail),
        PGACCEL_OK);
    check_success(filtered_output,
                  count_column_oracle(expected_keys, host_nulls, column ? &measure_flags : nullptr,
                                      &filter_flags),
                  host_cells.size());
    // FILTER rejects row zero, but the grouping expression still must run.
    cells[0] = 0;
    HashOutput failed(desc.group_capacity, true);
    CHECK_STATUS(execute_with_workspace(filtered, filtered_req, workspace, &failed.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(failed.untouched());
    cells[0] = host_cells[0];
  }
  // A NULL measure does not suppress evaluation/validation of its group key.
  cells[0] = 0;
  HashOutput invalid_counted(desc.group_capacity, true);
  CHECK_STATUS(
      execute_with_workspace(counted, counted_req, workspace, &invalid_counted.out, &detail),
      PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(invalid_counted.untouched());
  cells[0] = host_cells[0];

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
  unsupported.keys[0].values.type = PGACCEL_VAL_FLOAT64;
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_UNSUPPORTED);

  SharedArray<int8_t> mask(std::vector<int8_t>(host_keys.size(), PGACCEL_EXPR_TRUE));
  unsupported = desc;
  unsupported.keys[0].values.nulls = nullptr;
  unsupported.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  unsupported.where_filter.mask = mask.data();
  query_workspace(unsupported, &status);
  CHECK_STATUS(status, PGACCEL_OK);

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

int main(int argc, char** argv) {
  test_bool_mask_metadata();
  test_count_column_metadata_contract();
  test_filtered_count_metadata_contract();
  test_hash_sum_metadata();
  test_hash_where_metadata();
  if (argc == 2 && std::strcmp(argv[1], "--contract-only") == 0) {
    std::printf("hash metadata contracts: %d passed, %d failed\n", g_pass, g_fail);
    return g_fail == 0 ? 0 : 1;
  }
  try {
    CHECK_STATUS(pgaccel_init(), PGACCEL_OK);
    test_bool_mask_device();
    test_nullable_count_columns();
    test_filtered_counts();
    test_hash_where();
    test_hash_where_order_and_failures();
    test_hash_integer_sums();
    test_hash_integer_extrema();
    test_hash_empty_extrema();
    test_narrow_key_counts();
    test_duplicates_nulls_high_bits_collisions_and_warm_workspace();
    test_empty_all_null_and_capacity_boundaries();
    test_h3_planner_workspace_budget();
    test_fixed_seed_differential();
    test_million_row_hot_groups();
    test_parallel_compaction_and_merge_sort();
    test_large_capacity_full_cardinality();
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

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <string>
#include <utility>
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
    const pgaccel_status status = pgaccel_expr_shared_alloc(count * sizeof(T), &allocation);
    if (status != PGACCEL_OK || allocation == nullptr)
      throw std::runtime_error("shared USM allocation failed");
    ptr_ = static_cast<T*>(allocation);
  }

  SharedArray(std::initializer_list<T> values) : SharedArray(values.size()) {
    std::copy(values.begin(), values.end(), ptr_);
  }

  explicit SharedArray(const std::vector<T>& values) : SharedArray(values.size()) {
    std::copy(values.begin(), values.end(), ptr_);
  }

  SharedArray(const SharedArray&) = delete;
  SharedArray& operator=(const SharedArray&) = delete;

  SharedArray(SharedArray&& other) noexcept : ptr_(other.ptr_), count_(other.count_) {
    other.ptr_ = nullptr;
    other.count_ = 0;
  }

  SharedArray& operator=(SharedArray&& other) noexcept {
    if (this == &other)
      return *this;
    pgaccel_expr_shared_free(ptr_);
    ptr_ = other.ptr_;
    count_ = other.count_;
    other.ptr_ = nullptr;
    other.count_ = 0;
    return *this;
  }

  ~SharedArray() { pgaccel_expr_shared_free(ptr_); }

  T* data() { return ptr_; }
  const T* data() const { return ptr_; }
  size_t size() const { return count_; }
  T& operator[](size_t index) { return ptr_[index]; }
  const T& operator[](size_t index) const { return ptr_[index]; }

 private:
  T* ptr_ = nullptr;
  size_t count_ = 0;
};

class SharedWorkspace {
 public:
  SharedWorkspace(size_t bytes, size_t alignment)
      : allocation_(bytes + (alignment > 0 ? alignment - 1 : 0)) {
    std::fill(allocation_.data(), allocation_.data() + allocation_.size(), uint8_t{0});
    const uintptr_t raw = reinterpret_cast<uintptr_t>(allocation_.data());
    const uintptr_t aligned = (raw + alignment - 1) & ~(static_cast<uintptr_t>(alignment) - 1);
    ptr_ = reinterpret_cast<void*>(aligned);
  }

  void* data() const { return ptr_; }

 private:
  SharedArray<uint8_t> allocation_;
  void* ptr_ = nullptr;
};

pgaccel_expr_usm_col i32_col(const int32_t* values, const uint8_t* nulls = nullptr) {
  pgaccel_expr_usm_col col = {};
  col.values = values;
  col.nulls = nulls;
  col.type = PGACCEL_VAL_INT32;
  return col;
}

pgaccel_grouped_agg_filter disabled_filter() {
  pgaccel_grouped_agg_filter filter = {};
  filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
  filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  return filter;
}

pgaccel_grouped_agg_desc base_desc(size_t row_count) {
  pgaccel_grouped_agg_desc desc = {};
  desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  desc.size_bytes = sizeof(desc);
  desc.row_count = row_count;
  desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  desc.group_capacity = 1;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.where_filter = disabled_filter();
  for (auto& filter : desc.measure_filters)
    filter = disabled_filter();
  return desc;
}

void set_count_star(pgaccel_grouped_agg_desc& desc, uint32_t slot) {
  desc.measures[slot] = {};
  desc.measures[slot].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  desc.measures[slot].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[slot].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[slot].state_bytes = sizeof(int64_t);
  desc.measure_count = std::max(desc.measure_count, slot + 1);
}

void set_i32_view(pgaccel_grouped_agg_measure_col& col, const int32_t* values,
                  const uint8_t* nulls = nullptr) {
  col = {};
  col.values = values;
  col.nulls = nulls;
  col.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
  col.element_bytes = sizeof(int32_t);
}

void set_i64_view(pgaccel_grouped_agg_measure_col& col, const int64_t* values,
                  const uint8_t* nulls = nullptr) {
  col = {};
  col.values = values;
  col.nulls = nulls;
  col.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
  col.element_bytes = sizeof(int64_t);
}

void set_f64_view(pgaccel_grouped_agg_measure_col& col, const double* values,
                  const uint8_t* nulls = nullptr) {
  col = {};
  col.values = values;
  col.nulls = nulls;
  col.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64;
  col.element_bytes = sizeof(double);
}

void set_count_only_view(pgaccel_grouped_agg_desc& desc, uint32_t slot, const void* values,
                         const uint8_t* nulls, int32_t physical_type, uint32_t element_bytes,
                         int32_t accumulator_kind) {
  desc.measures[slot] = {};
  desc.measures[slot].value.values = values;
  desc.measures[slot].value.nulls = nulls;
  desc.measures[slot].value.physical_type = physical_type;
  desc.measures[slot].value.element_bytes = element_bytes;
  desc.measures[slot].op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
  desc.measures[slot].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[slot].accumulator_kind = accumulator_kind;
  desc.measures[slot].state_bytes = sizeof(uint64_t);
  desc.measure_count = std::max(desc.measure_count, slot + 1);
}

void finish_i64_measure(pgaccel_grouped_agg_desc& desc, uint32_t slot, int32_t op, uint32_t mask) {
  desc.measures[slot].op = op;
  desc.measures[slot].agg_mask = mask;
  desc.measures[slot].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[slot].state_bytes = sizeof(int64_t);
  desc.measure_count = std::max(desc.measure_count, slot + 1);
}

void finish_f64_measure(pgaccel_grouped_agg_desc& desc, uint32_t slot, int32_t op, uint32_t mask) {
  desc.measures[slot].op = op;
  desc.measures[slot].agg_mask = mask;
  desc.measures[slot].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_F64;
  desc.measures[slot].state_bytes = sizeof(double);
  desc.measure_count = std::max(desc.measure_count, slot + 1);
}

void set_fact_key(pgaccel_grouped_agg_desc& desc, uint32_t slot, const int32_t* values,
                  const uint8_t* nulls, int32_t code_min, uint32_t cardinality,
                  int32_t null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE) {
  auto& key = desc.keys[slot];
  key = {};
  key.values = i32_col(values, nulls);
  key.source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
  key.code_min = code_min;
  key.cardinality = cardinality;
  key.null_code = null_code;
  desc.key_count = std::max(desc.key_count, slot + 1);
  desc.group_capacity *= cardinality;
}

void set_dim(pgaccel_grouped_agg_desc& desc, uint32_t slot, const int32_t* fact_key,
             const uint8_t* fact_nulls, int32_t key_min, uint32_t key_count,
             const uint8_t* match_by_key = nullptr, const uint64_t* multiplicity_by_key = nullptr) {
  auto& dim = desc.dims[slot];
  dim = {};
  dim.fact_key = i32_col(fact_key, fact_nulls);
  dim.match_by_key = match_by_key;
  dim.multiplicity_by_key = multiplicity_by_key;
  dim.key_min = key_min;
  dim.key_count = key_count;
  desc.dim_count = std::max(desc.dim_count, slot + 1);
}

void set_dim_key(pgaccel_grouped_agg_desc& desc, uint32_t slot, uint32_t dim_slot,
                 const int32_t* lookup_by_key, int32_t code_min, uint32_t cardinality,
                 int32_t null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE) {
  auto& key = desc.keys[slot];
  key = {};
  key.lookup_by_key = lookup_by_key;
  key.source = PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0 + static_cast<int32_t>(dim_slot);
  key.code_min = code_min;
  key.cardinality = cardinality;
  key.null_code = null_code;
  desc.key_count = std::max(desc.key_count, slot + 1);
  desc.group_capacity *= cardinality;
}

pgaccel_val val_i64(int64_t value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_INT64;
  result.data.i64 = value;
  return result;
}

pgaccel_val val_bool(bool value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_BOOL;
  result.data.b = value;
  return result;
}

pgaccel_val val_f32(float value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_FLOAT32;
  result.data.f32 = value;
  return result;
}

pgaccel_val val_date(int32_t value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_DATE;
  result.data.i32 = value;
  return result;
}

pgaccel_val val_timestamp(int64_t value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_TIMESTAMP;
  result.data.i64 = value;
  return result;
}

struct MeasureOutputStorage {
  std::vector<uint64_t> sum;
  std::vector<uint64_t> min;
  std::vector<uint64_t> max;
  std::vector<uint64_t> sumsq;
  std::vector<uint64_t> count;
  std::vector<uint64_t> nonnull;
  std::vector<uint64_t> rhs_sum;
  std::vector<uint64_t> rhs_count;
  std::vector<uint64_t> rhs_nonnull;
};

class OutputStorage {
 public:
  explicit OutputStorage(const pgaccel_grouped_agg_desc& desc, bool include_group_codes = false,
                         bool include_dense_keys = false) {
    const size_t capacity = desc.group_capacity;
    out.abi_version = PGACCEL_OLAP_ABI_VERSION;
    out.size_bytes = sizeof(out);
    out.group_capacity = capacity;
    out.output_space = PGACCEL_MEM_SPACE_HOST;

    if (desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE) {
      active.assign(capacity, 0xa5);
      out.active_groups = active.data();
    }
    if (include_group_codes) {
      group_codes.assign(capacity, std::numeric_limits<size_t>::max());
      out.group_codes = group_codes.data();
    }

    const bool materialize_keys =
        desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT || include_dense_keys;
    if (materialize_keys) {
      for (uint32_t i = 0; i < desc.key_count; ++i) {
        key_values[i].assign(capacity, std::numeric_limits<int32_t>::min());
        out.keys[i].values = key_values[i].data();
        out.keys[i].type = PGACCEL_VAL_INT32;
        if (desc.keys[i].null_code != PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE) {
          key_nulls[i].assign(capacity, 0xa5);
          out.keys[i].nulls = key_nulls[i].data();
        }
      }
    }

    for (uint32_t i = 0; i < desc.measure_count; ++i) {
      const uint32_t mask = desc.measures[i].agg_mask;
      auto& storage = measures[i];
      auto& lane = out.measures[i];
      if (mask & PGACCEL_GROUPED_AGG_LANE_SUM) {
        storage.sum.assign(capacity, kSentinel);
        lane.sum = storage.sum.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_MIN) {
        storage.min.assign(capacity, kSentinel);
        lane.min = storage.min.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_MAX) {
        storage.max.assign(capacity, kSentinel);
        lane.max = storage.max.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_SUMSQ) {
        storage.sumsq.assign(capacity, kSentinel);
        lane.sumsq = storage.sumsq.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_COUNT) {
        storage.count.assign(capacity, kSentinel);
        lane.count = storage.count.data();
      }
      const bool value_state =
          (mask & (PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                   PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_SUMSQ)) != 0;
      const bool count_column = desc.measures[i].op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR &&
                                (mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0;
      if (value_state || count_column) {
        storage.nonnull.assign(capacity, kSentinel);
        lane.nonnull_count = storage.nonnull.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_RHS_SUM) {
        storage.rhs_sum.assign(capacity, kSentinel);
        storage.rhs_nonnull.assign(capacity, kSentinel);
        lane.rhs_sum = storage.rhs_sum.data();
        lane.rhs_nonnull_count = storage.rhs_nonnull.data();
      }
      if (mask & PGACCEL_GROUPED_AGG_LANE_RHS_COUNT) {
        storage.rhs_count.assign(capacity, kSentinel);
        lane.rhs_count = storage.rhs_count.data();
      }
    }
  }

  int64_t i64(const std::vector<uint64_t>& lane, size_t group) const {
    int64_t value = 0;
    std::memcpy(&value, &lane[group], sizeof(value));
    return value;
  }

  double f64(const std::vector<uint64_t>& lane, size_t group) const {
    double value = 0.0;
    std::memcpy(&value, &lane[group], sizeof(value));
    return value;
  }

  static constexpr uint64_t kSentinel = UINT64_C(0xdedec0decafef00d);

  pgaccel_grouped_agg_out out = {};
  std::vector<size_t> group_codes;
  std::vector<uint8_t> active;
  std::array<std::vector<int32_t>, PGACCEL_GROUPED_AGG_MAX_KEYS> key_values;
  std::array<std::vector<uint8_t>, PGACCEL_GROUPED_AGG_MAX_KEYS> key_nulls;
  std::array<MeasureOutputStorage, PGACCEL_GROUPED_AGG_MAX_MEASURES> measures;
};

pgaccel_grouped_agg_workspace_req workspace_req(const pgaccel_grouped_agg_desc& desc,
                                                pgaccel_status* status = nullptr) {
  pgaccel_grouped_agg_workspace_req req = {};
  req.abi_version = PGACCEL_OLAP_ABI_VERSION;
  req.size_bytes = sizeof(req);
  const pgaccel_status result = pgaccel_grouped_agg_workspace_requirements(&desc, &req);
  if (status != nullptr)
    *status = result;
  return req;
}

pgaccel_status execute_external(const pgaccel_grouped_agg_desc& original,
                                pgaccel_grouped_agg_out* out) {
  pgaccel_status query_status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req req = workspace_req(original, &query_status);
  if (query_status != PGACCEL_OK)
    return query_status;
  SharedWorkspace workspace(req.bytes, req.alignment);
  pgaccel_grouped_agg_desc desc = original;
  desc.scratch = workspace.data();
  desc.scratch_bytes = req.bytes;
  desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
  desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
  return pgaccel_grouped_agg_execute(&desc, out);
}

pgaccel_status execute_external_ex(const pgaccel_grouped_agg_desc& original,
                                   pgaccel_grouped_agg_out* out, int32_t* detail) {
  pgaccel_status query_status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req req = workspace_req(original, &query_status);
  if (query_status != PGACCEL_OK)
    return query_status;
  SharedWorkspace workspace(req.bytes, req.alignment);
  pgaccel_grouped_agg_desc desc = original;
  desc.scratch = workspace.data();
  desc.scratch_bytes = req.bytes;
  desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
  desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
  return pgaccel_grouped_agg_execute_ex(&desc, out, detail);
}

pgaccel_status execute_in_workspace(const pgaccel_grouped_agg_desc& original,
                                    const pgaccel_grouped_agg_workspace_req& req,
                                    const SharedWorkspace& workspace, pgaccel_grouped_agg_out* out,
                                    int32_t* detail) {
  pgaccel_grouped_agg_desc desc = original;
  desc.scratch = workspace.data();
  desc.scratch_bytes = req.bytes;
  desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
  desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
  return pgaccel_grouped_agg_execute_ex(&desc, out, detail);
}

const void* advance_bytes(const void* pointer, size_t rows, size_t width) {
  return pointer == nullptr ? nullptr : static_cast<const uint8_t*>(pointer) + rows * width;
}

pgaccel_grouped_agg_desc row_slice(const pgaccel_grouped_agg_desc& original, size_t first_row,
                                   size_t row_count) {
  pgaccel_grouped_agg_desc desc = original;
  desc.row_count = row_count;
  for (size_t i = 0; i < desc.key_count; ++i) {
    pgaccel_grouped_agg_key& key = desc.keys[i];
    if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
      key.values.values = advance_bytes(key.values.values, first_row, sizeof(int32_t));
      key.values.nulls = static_cast<const uint8_t*>(
          advance_bytes(key.values.nulls, first_row, sizeof(uint8_t)));
    }
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    pgaccel_grouped_agg_measure& measure = desc.measures[i];
    measure.value.values =
        advance_bytes(measure.value.values, first_row, measure.value.element_bytes);
    measure.value.nulls = static_cast<const uint8_t*>(
        advance_bytes(measure.value.nulls, first_row, sizeof(uint8_t)));
    measure.rhs.values = advance_bytes(measure.rhs.values, first_row, measure.rhs.element_bytes);
    measure.rhs.nulls = static_cast<const uint8_t*>(
        advance_bytes(measure.rhs.nulls, first_row, sizeof(uint8_t)));
    desc.measure_filters[i].mask = static_cast<const int8_t*>(
        advance_bytes(desc.measure_filters[i].mask, first_row, sizeof(int8_t)));
  }
  desc.where_filter.mask = static_cast<const int8_t*>(
      advance_bytes(desc.where_filter.mask, first_row, sizeof(int8_t)));
  for (size_t i = 0; i < desc.dim_count; ++i) {
    pgaccel_grouped_agg_dim& dim = desc.dims[i];
    dim.fact_key.values = advance_bytes(dim.fact_key.values, first_row, sizeof(int32_t));
    dim.fact_key.nulls = static_cast<const uint8_t*>(
        advance_bytes(dim.fact_key.nulls, first_row, sizeof(uint8_t)));
  }
  return desc;
}

void check_outputs_equal(const OutputStorage& actual, const OutputStorage& expected,
                         uint32_t measure_count) {
  CHECK(actual.out.emitted_group_count == expected.out.emitted_group_count);
  CHECK(actual.out.selected_count == expected.out.selected_count);
  CHECK(actual.out.uncertain_count == expected.out.uncertain_count);
  CHECK(actual.active == expected.active);
  CHECK(actual.group_codes == expected.group_codes);
  CHECK(actual.key_values == expected.key_values);
  CHECK(actual.key_nulls == expected.key_nulls);
  for (size_t i = 0; i < measure_count; ++i) {
    CHECK(actual.measures[i].sum == expected.measures[i].sum);
    CHECK(actual.measures[i].min == expected.measures[i].min);
    CHECK(actual.measures[i].max == expected.measures[i].max);
    CHECK(actual.measures[i].sumsq == expected.measures[i].sumsq);
    CHECK(actual.measures[i].count == expected.measures[i].count);
    CHECK(actual.measures[i].nonnull == expected.measures[i].nonnull);
    CHECK(actual.measures[i].rhs_sum == expected.measures[i].rhs_sum);
    CHECK(actual.measures[i].rhs_count == expected.measures[i].rhs_count);
    CHECK(actual.measures[i].rhs_nonnull == expected.measures[i].rhs_nonnull);
  }
}

void check_i64_lane(const OutputStorage& output, const std::vector<uint64_t>& lane,
                    std::initializer_list<int64_t> expected) {
  CHECK(lane.size() == expected.size());
  size_t index = 0;
  for (const int64_t value : expected) {
    CHECK(output.i64(lane, index) == value);
    ++index;
  }
}

void check_u64_lane(const std::vector<uint64_t>& lane, std::initializer_list<uint64_t> expected) {
  CHECK(lane.size() == expected.size());
  size_t index = 0;
  for (const uint64_t value : expected) {
    CHECK(lane[index] == value);
    ++index;
  }
}

void test_workspace_and_descriptor_validation() {
  std::printf("--- workspace and descriptor validation ---\n");
  pgaccel_grouped_agg_desc desc = base_desc(0);
  set_count_star(desc, 0);

  pgaccel_status status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req first = workspace_req(desc, &status);
  CHECK(status == PGACCEL_OK);
  CHECK(first.bytes > 0);
  CHECK(first.alignment > 0);
  CHECK((first.alignment & (first.alignment - 1)) == 0);
  CHECK(first.space == PGACCEL_MEM_SPACE_SHARED_USM || first.space == PGACCEL_MEM_SPACE_DEVICE);
  CHECK(first.flags == 0);

  pgaccel_grouped_agg_desc ignored_scratch = desc;
  ignored_scratch.scratch = reinterpret_cast<void*>(static_cast<uintptr_t>(0x123));
  ignored_scratch.scratch_bytes = 3;
  ignored_scratch.scratch_space = 999;
  ignored_scratch.scratch_alignment = 3;
  const pgaccel_grouped_agg_workspace_req second = workspace_req(ignored_scratch, &status);
  CHECK(status == PGACCEL_OK);
  CHECK(first.bytes == second.bytes);
  CHECK(first.alignment == second.alignment);
  CHECK(first.space == second.space);

  pgaccel_grouped_agg_workspace_req bad_req = {};
  bad_req.abi_version = PGACCEL_OLAP_ABI_VERSION;
  bad_req.size_bytes = sizeof(bad_req);
  bad_req.bytes = 1;
  CHECK_STATUS(pgaccel_grouped_agg_workspace_requirements(&desc, &bad_req), PGACCEL_ERROR);

  pgaccel_grouped_agg_desc malformed = desc;
  malformed.abi_version++;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.group_capacity = 2;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.keys[2].flags = 1;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.measure_filters[3].value_cmp_opcode = 0;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_OK);
}

void test_dense_chunk_lifecycle_equivalence() {
  std::printf("--- dense chunk lifecycle equivalence ---\n");
  constexpr size_t rows = 7;
  SharedArray<int32_t> keys({0, 1, 0, 2, 1, 2, 0});
  SharedArray<uint8_t> key_nulls({0, 0, 1, 0, 0, 0, 0});
  SharedArray<int64_t> values({10, -4, 99, 8, 6, 3, -2});
  SharedArray<uint8_t> value_nulls({0, 0, 1, 0, 0, 1, 0});
  SharedArray<int8_t> where_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                  PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE,
                                  PGACCEL_EXPR_UNCERTAIN, PGACCEL_EXPR_TRUE});
  SharedArray<int8_t> measure_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE, PGACCEL_EXPR_UNCERTAIN,
                                    PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE});
  SharedArray<int32_t> dim_keys({10, 11, 10, 11, 10, 11, 10});
  SharedArray<uint8_t> dim_nulls({0, 0, 0, 0, 0, 0, 1});
  SharedArray<uint8_t> dim_match({1, 1});
  SharedArray<uint64_t> dim_multiplicity({2, 3});

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, 4, 3);
  set_dim(desc, 0, dim_keys.data(), dim_nulls.data(), 10, 2, dim_match.data(),
          dim_multiplicity.data());
  set_i64_view(desc.measures[0].value, values.data(), value_nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT |
                         PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = where_mask.data();
  desc.measure_filters[0].kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.measure_filters[0].mask = measure_mask.data();

  OutputStorage reference(desc, true, true);
  CHECK_STATUS(execute_external(desc, &reference.out), PGACCEL_OK);
  CHECK(reference.out.selected_count == 9);
  CHECK(reference.out.uncertain_count == 0);

  const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
  CHECK(req.bytes > 0);

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    for (size_t row = 0; row < rows; ++row) {
      pgaccel_grouped_agg_desc chunk = row_slice(desc, row, 1);
      chunk.execution_flags = row == 0
                                  ? PGACCEL_GROUPED_AGG_EXEC_RESET |
                                        PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
                                  : PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
      CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      if (row == 2) {
        pgaccel_grouped_agg_desc empty = row_slice(desc, 0, 0);
        empty.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        CHECK_STATUS(execute_in_workspace(empty, req, workspace, nullptr, &detail), PGACCEL_OK);
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      }
    }
    pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage output(desc, true, true);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &output.out, &detail),
                 PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    check_outputs_equal(output, reference, desc.measure_count);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    const std::array<size_t, 3> chunks = {3, 3, 1};
    size_t first_row = 0;
    OutputStorage output(desc, true, true);
    for (size_t index = 0; index < chunks.size(); ++index) {
      pgaccel_grouped_agg_desc chunk = row_slice(desc, first_row, chunks[index]);
      chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      if (index == 0)
        chunk.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
      if (index + 1 == chunks.size())
        chunk.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
      int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
      CHECK_STATUS(execute_in_workspace(chunk, req, workspace,
                                        index + 1 == chunks.size() ? &output.out : nullptr,
                                        &detail),
                   PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      first_row += chunks[index];
    }
    CHECK(first_row == rows);
    check_outputs_equal(output, reference, desc.measure_count);
  }

  {
    pgaccel_grouped_agg_desc empty_desc = row_slice(desc, 0, 0);
    empty_desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage empty_reference(empty_desc, true, true);
    CHECK_STATUS(execute_external(empty_desc, &empty_reference.out), PGACCEL_OK);

    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc reset = empty_desc;
    reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(reset, req, workspace, nullptr, &detail), PGACCEL_OK);
    pgaccel_grouped_agg_desc empty_accumulate = empty_desc;
    empty_accumulate.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    CHECK_STATUS(execute_in_workspace(empty_accumulate, req, workspace, nullptr, &detail),
                 PGACCEL_OK);
    pgaccel_grouped_agg_desc finalize = empty_desc;
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage output(empty_desc, true, true);
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &output.out, &detail),
                 PGACCEL_OK);
    check_outputs_equal(output, empty_reference, desc.measure_count);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, rows);
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage ignored(desc, true, true);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, &ignored.out, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc reset_chunk = row_slice(desc, 4, 2);
    reset_chunk.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    CHECK_STATUS(execute_in_workspace(reset_chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
    pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage reset_output(desc, true, true);
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &reset_output.out, &detail),
                 PGACCEL_OK);

    pgaccel_grouped_agg_desc reset_reference_desc = row_slice(desc, 4, 2);
    reset_reference_desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage reset_reference(desc, true, true);
    CHECK_STATUS(execute_external(reset_reference_desc, &reset_reference.out), PGACCEL_OK);
    check_outputs_equal(reset_output, reset_reference, desc.measure_count);
  }
}

void test_dense_chunk_lifecycle_fail_closed() {
  std::printf("--- dense chunk lifecycle fail closed ---\n");
  SharedArray<int32_t> keys({0, 1, 0});
  SharedArray<int64_t> values({4, 5, 6});
  SharedArray<int8_t> masks({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE});
  SharedArray<int8_t> invalid_mask({2});

  pgaccel_grouped_agg_desc desc = base_desc(3);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, 2);
  set_i64_view(desc.measures[0].value, values.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT |
                         PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = masks.data();
  const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc accumulate = row_slice(desc, 0, 1);
    accumulate.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    OutputStorage unexpected(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(accumulate, req, workspace, &unexpected.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(unexpected.measures[0].sum[0] == OutputStorage::kSentinel);

    pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage uninitialized(desc);
    detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &uninitialized.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc drifted = row_slice(desc, 0, 0);
    drifted.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    drifted.keys[0].cardinality = 1;
    drifted.group_capacity = 1;
    OutputStorage drifted_output(drifted);
    CHECK_STATUS(execute_in_workspace(drifted, req, workspace, &drifted_output.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc drifted = row_slice(desc, 0, 0);
    drifted.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    drifted.measures[0].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
    drifted.measures[0].value.element_bytes = sizeof(int32_t);
    OutputStorage drifted_output(drifted);
    CHECK_STATUS(execute_in_workspace(drifted, req, workspace, &drifted_output.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc drifted = row_slice(desc, 0, 0);
    drifted.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    drifted.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    OutputStorage drifted_output(drifted);
    CHECK_STATUS(execute_in_workspace(drifted, req, workspace, &drifted_output.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc drifted = row_slice(desc, 0, 0);
    drifted.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    drifted.keys[0].code_min = -1;
    OutputStorage drifted_output(desc);
    CHECK_STATUS(execute_in_workspace(drifted, req, workspace, &drifted_output.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

    pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage poisoned(desc);
    detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &poisoned.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

    pgaccel_grouped_agg_desc reset = row_slice(desc, 0, 3);
    reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage recovered(desc);
    CHECK_STATUS(execute_in_workspace(reset, req, workspace, &recovered.out, &detail),
                 PGACCEL_OK);
    OutputStorage reference(desc);
    CHECK_STATUS(execute_external(desc, &reference.out), PGACCEL_OK);
    check_outputs_equal(recovered, reference, desc.measure_count);

    OutputStorage reused(desc);
    detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &reused.out, &detail),
                 PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedWorkspace workspace(req.bytes, req.alignment);
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

    pgaccel_grouped_agg_desc invalid = row_slice(desc, 1, 1);
    invalid.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    invalid.where_filter.mask = invalid_mask.data();
    CHECK_STATUS(execute_in_workspace(invalid, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

    pgaccel_grouped_agg_desc next = row_slice(desc, 2, 1);
    next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(next, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

    pgaccel_grouped_agg_desc reset = row_slice(desc, 2, 1);
    reset.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    CHECK_STATUS(execute_in_workspace(reset, req, workspace, nullptr, &detail), PGACCEL_OK);
  }
}

void test_empty_ungrouped_active() {
  std::printf("--- empty ungrouped active semantics ---\n");
  pgaccel_grouped_agg_desc desc = base_desc(0);
  set_count_star(desc, 0);
  OutputStorage output(desc, true);
  pgaccel_reset_gpu_exec_count();
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(pgaccel_gpu_exec_count() > 0);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.out.selected_count == 0);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.active[0] == 1);
  CHECK(output.group_codes[0] == 0);
  CHECK(output.measures[0].count[0] == 0);
}

void test_i64_four_measure_lanes() {
  std::printf("--- dense I64 four-measure lanes ---\n");
  SharedArray<int64_t> values0({5, -2, 7, 9, 1});
  SharedArray<uint8_t> nulls0({0, 0, 0, 1, 0});
  SharedArray<int32_t> lhs1({2, -3, 4, 5, 6});
  SharedArray<int32_t> rhs1({10, 2, -1, 7, 0});
  SharedArray<uint8_t> rhs1_nulls({0, 0, 0, 1, 0});
  SharedArray<int64_t> lhs2({100, 50, -10, 8, 1});
  SharedArray<int64_t> rhs2({1, 70, -20, 3, 2});
  SharedArray<uint8_t> lhs2_nulls({0, 0, 0, 0, 1});
  SharedArray<uint8_t> rhs2_nulls({0, 0, 1, 0, 0});

  pgaccel_grouped_agg_desc desc = base_desc(5);
  set_i64_view(desc.measures[0].value, values0.data(), nulls0.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);

  set_i32_view(desc.measures[1].value, lhs1.data());
  set_i32_view(desc.measures[1].rhs, rhs1.data(), rhs1_nulls.data());
  finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_MUL,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);

  set_i64_view(desc.measures[2].value, lhs2.data(), lhs2_nulls.data());
  set_i64_view(desc.measures[2].rhs, rhs2.data(), rhs2_nulls.data());
  finish_i64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_count_star(desc, 3);

  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 5);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.active[0] == 1);

  CHECK(output.i64(output.measures[0].sum, 0) == 11);
  CHECK(output.i64(output.measures[0].min, 0) == -2);
  CHECK(output.i64(output.measures[0].max, 0) == 7);
  CHECK(output.measures[0].count[0] == 4);
  CHECK(output.measures[0].nonnull[0] == 4);

  CHECK(output.i64(output.measures[1].sum, 0) == 10);
  CHECK(output.measures[1].count[0] == 4);
  CHECK(output.measures[1].nonnull[0] == 4);

  CHECK(output.i64(output.measures[2].sum, 0) == 84);
  CHECK(output.i64(output.measures[2].min, 0) == -20);
  CHECK(output.i64(output.measures[2].max, 0) == 99);
  CHECK(output.measures[2].count[0] == 3);
  CHECK(output.measures[2].nonnull[0] == 3);
  CHECK(output.measures[3].count[0] == 5);
}

void test_f64_stats_pair_and_nan_ordering() {
  std::printf("--- dense F64 stats pair and NaN ordering ---\n");
  const double nan = std::numeric_limits<double>::quiet_NaN();
  SharedArray<int32_t> groups({10, 10, 11, 11, 12, 12});
  SharedArray<double> primary({1.5, 2.5, -1.0, 4.0, nan, 16.0});
  SharedArray<uint8_t> primary_nulls({0, 1, 0, 0, 0, 0});
  SharedArray<double> rhs({10.0, 20.0, 30.0, 40.0, 50.0, 60.0});
  SharedArray<uint8_t> rhs_nulls({0, 0, 1, 0, 0, 0});

  pgaccel_grouped_agg_desc desc = base_desc(6);
  set_fact_key(desc, 0, groups.data(), nullptr, 10, 3);
  set_f64_view(desc.measures[0].value, primary.data(), primary_nulls.data());
  set_f64_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
  finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                     PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN);

  OutputStorage output(desc, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 6);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 3);
  CHECK(output.active == std::vector<uint8_t>({1, 1, 1}));
  CHECK(output.group_codes == std::vector<size_t>({0, 1, 2}));

  CHECK(output.f64(output.measures[0].sum, 0) == 1.5);
  CHECK(output.f64(output.measures[0].min, 0) == 1.5);
  CHECK(output.f64(output.measures[0].max, 0) == 1.5);
  CHECK(output.f64(output.measures[0].sumsq, 0) == 2.25);
  CHECK(output.measures[0].count[0] == 1);
  CHECK(output.measures[0].nonnull[0] == 1);
  CHECK(output.f64(output.measures[0].rhs_sum, 0) == 30.0);
  CHECK(output.measures[0].rhs_count[0] == 2);
  CHECK(output.measures[0].rhs_nonnull[0] == 2);

  CHECK(output.f64(output.measures[0].sum, 1) == 3.0);
  CHECK(output.f64(output.measures[0].min, 1) == -1.0);
  CHECK(output.f64(output.measures[0].max, 1) == 4.0);
  CHECK(output.f64(output.measures[0].sumsq, 1) == 17.0);
  CHECK(output.measures[0].count[1] == 2);
  CHECK(output.measures[0].nonnull[1] == 2);
  CHECK(output.f64(output.measures[0].rhs_sum, 1) == 40.0);
  CHECK(output.measures[0].rhs_count[1] == 1);
  CHECK(output.measures[0].rhs_nonnull[1] == 1);

  CHECK(std::isnan(output.f64(output.measures[0].sum, 2)));
  CHECK(output.f64(output.measures[0].min, 2) == 16.0);
  CHECK(std::isnan(output.f64(output.measures[0].max, 2)));
  CHECK(std::isnan(output.f64(output.measures[0].sumsq, 2)));
  CHECK(output.measures[0].count[2] == 2);
  CHECK(output.measures[0].nonnull[2] == 2);
  CHECK(output.f64(output.measures[0].rhs_sum, 2) == 110.0);
  CHECK(output.measures[0].rhs_count[2] == 2);
  CHECK(output.measures[0].rhs_nonnull[2] == 2);
}

void test_global_and_measure_filters() {
  std::printf("--- global and per-measure filters ---\n");
  SharedArray<int64_t> a({1, 2, 3, 4, 7, 8});
  SharedArray<int64_t> b({10, 20, 30, 40, 70, 80});
  SharedArray<int8_t> where_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_UNCERTAIN,
                                  PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE});
  SharedArray<int8_t> measure1_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                     PGACCEL_EXPR_UNCERTAIN, PGACCEL_EXPR_TRUE,
                                     PGACCEL_EXPR_FALSE});

  pgaccel_grouped_agg_desc desc = base_desc(6);
  set_i64_view(desc.measures[0].value, a.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_i64_view(desc.measures[1].value, b.data());
  finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);

  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  desc.where_filter.predicate_measure_slot = 0;
  desc.where_filter.predicate_range_count = 1;
  desc.where_filter.predicate_lo[0] = val_i64(2);
  desc.where_filter.predicate_hi[0] = val_i64(8);
  desc.where_filter.mask = where_mask.data();

  desc.measure_filters[0].predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  desc.measure_filters[0].predicate_measure_slot = 0;
  desc.measure_filters[0].predicate_range_count = 2;
  desc.measure_filters[0].predicate_lo[0] = val_i64(2);
  desc.measure_filters[0].predicate_hi[0] = val_i64(2);
  desc.measure_filters[0].predicate_lo[1] = val_i64(8);
  desc.measure_filters[0].predicate_hi[1] = val_i64(8);

  desc.measure_filters[1].kind = PGACCEL_GROUPED_AGG_FILTER_RECHECK;
  desc.measure_filters[1].mask = measure1_mask.data();

  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 3);
  CHECK(output.out.uncertain_count == 1);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.active[0] == 1);
  CHECK(output.i64(output.measures[0].sum, 0) == 10);
  CHECK(output.measures[0].count[0] == 2);
  CHECK(output.measures[0].nonnull[0] == 2);
  CHECK(output.i64(output.measures[1].sum, 0) == 20);
  CHECK(output.measures[1].count[0] == 1);
  CHECK(output.measures[1].nonnull[0] == 1);
}

void test_predicate_only_physical_count() {
  std::printf("--- predicate-only physical COUNT lanes ---\n");

  {
    SharedArray<uint8_t> values({0, 1, 1});
    SharedArray<uint8_t> nulls({0, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(3);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        sizeof(uint8_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_bool(true);
    desc.where_filter.predicate_hi[0] = val_bool(true);

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 1);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.measures[0].count[0] == 1);
    CHECK(output.measures[0].nonnull[0] == 1);
  }

  {
    SharedArray<float> values({-1.0F, 1.0F, 2.0F});
    SharedArray<uint8_t> nulls({0, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(3);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
                        sizeof(float), PGACCEL_GROUPED_AGG_ACCUM_F64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_f32(0.0F);
    desc.where_filter.predicate_hi[0] = val_f32(2.0F);

    pgaccel_grouped_agg_desc unsupported = desc;
    unsupported.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
    pgaccel_status unsupported_status = PGACCEL_ERROR;
    workspace_req(unsupported, &unsupported_status);
    CHECK(unsupported_status == PGACCEL_UNSUPPORTED);

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 1);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.measures[0].count[0] == 1);
    CHECK(output.measures[0].nonnull[0] == 1);
  }

  {
    SharedArray<int32_t> values({0, 10, 20});
    SharedArray<uint8_t> nulls({0, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(3);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                        sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_date(5);
    desc.where_filter.predicate_hi[0] = val_date(20);

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 1);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.measures[0].count[0] == 1);
    CHECK(output.measures[0].nonnull[0] == 1);
  }

  {
    SharedArray<int64_t> values({0, 10, 20});
    SharedArray<uint8_t> nulls({0, 1, 0});
    pgaccel_grouped_agg_desc desc = base_desc(3);
    set_count_only_view(desc, 0, values.data(), nulls.data(),
                        PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, sizeof(int64_t),
                        PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_timestamp(5);
    desc.where_filter.predicate_hi[0] = val_timestamp(25);

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 1);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.measures[0].count[0] == 1);
    CHECK(output.measures[0].nonnull[0] == 1);
  }
}

void test_four_dimensions_and_multiplicity() {
  std::printf("--- four dimensions and multiplicity ---\n");
  SharedArray<int32_t> d0_key({0, 1, 0, 1, 0, 1});
  SharedArray<int32_t> d1_key({0, 1, 0, 1, 2, 2});
  SharedArray<int32_t> d2_key({0, 0, 1, 1, 0, 1});
  SharedArray<int32_t> d3_key({0, 1, 0, 1, 0, 1});
  SharedArray<int32_t> d0_group({10, 11});
  SharedArray<uint8_t> d1_match({1, 1, 0});
  SharedArray<uint64_t> d1_mult({2, 3, 5});
  SharedArray<uint64_t> d2_mult({4, 2});
  SharedArray<uint8_t> d3_match({1, 0});
  SharedArray<uint64_t> d3_mult({1, 7});
  SharedArray<int64_t> values({5, 7, 11, 13, 17, 19});

  pgaccel_grouped_agg_desc desc = base_desc(6);
  set_dim(desc, 0, d0_key.data(), nullptr, 0, 2);
  set_dim(desc, 1, d1_key.data(), nullptr, 0, 3, d1_match.data(), d1_mult.data());
  set_dim(desc, 2, d2_key.data(), nullptr, 0, 2, nullptr, d2_mult.data());
  set_dim(desc, 3, d3_key.data(), nullptr, 0, 2, d3_match.data(), d3_mult.data());
  set_dim_key(desc, 0, 0, d0_group.data(), 10, 2);
  set_i64_view(desc.measures[0].value, values.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);

  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 12);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.active == std::vector<uint8_t>({1, 0}));
  check_i64_lane(output, output.measures[0].sum, {84, 0});
  CHECK(output.i64(output.measures[0].min, 0) == 5);
  CHECK(output.i64(output.measures[0].max, 0) == 11);
  check_u64_lane(output.measures[0].count, {12, 0});
  check_u64_lane(output.measures[0].nonnull, {12, 0});
}

void test_mixed_radix_compact_and_keyed_empty() {
  std::printf("--- mixed radix compact and keyed empty ---\n");
  SharedArray<int32_t> key0({1, 1, 2, 2, 1});
  SharedArray<int32_t> key1({5, 6, 5, 6, 5});

  pgaccel_grouped_agg_desc desc = base_desc(5);
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  set_fact_key(desc, 0, key0.data(), nullptr, 1, 3);
  set_fact_key(desc, 1, key1.data(), nullptr, 5, 2);
  set_count_star(desc, 0);

  OutputStorage output(desc, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 5);
  CHECK(output.out.emitted_group_count == 4);
  CHECK(output.out.active_groups == nullptr);
  CHECK(std::vector<size_t>(output.group_codes.begin(), output.group_codes.begin() + 4) ==
        std::vector<size_t>({0, 1, 2, 3}));
  CHECK(std::vector<int32_t>(output.key_values[0].begin(), output.key_values[0].begin() + 4) ==
        std::vector<int32_t>({1, 1, 2, 2}));
  CHECK(std::vector<int32_t>(output.key_values[1].begin(), output.key_values[1].begin() + 4) ==
        std::vector<int32_t>({5, 6, 5, 6}));
  CHECK(std::vector<uint64_t>(output.measures[0].count.begin(),
                              output.measures[0].count.begin() + 4) ==
        std::vector<uint64_t>({2, 1, 1, 1}));

  pgaccel_grouped_agg_desc empty = base_desc(0);
  set_fact_key(empty, 0, nullptr, nullptr, 0, 2);
  set_count_star(empty, 0);
  OutputStorage empty_output(empty);
  CHECK_STATUS(execute_external(empty, &empty_output.out), PGACCEL_OK);
  CHECK(empty_output.out.emitted_group_count == 0);
  CHECK(empty_output.active == std::vector<uint8_t>({0, 0}));
  check_u64_lane(empty_output.measures[0].count, {0, 0});

  SharedArray<int32_t> nullable_key({0, 99, 1});
  SharedArray<uint8_t> nullable_key_nulls({0, 1, 0});
  pgaccel_grouped_agg_desc nullable = base_desc(3);
  nullable.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  set_fact_key(nullable, 0, nullable_key.data(), nullable_key_nulls.data(), 0, 3, 2);
  set_count_star(nullable, 0);
  OutputStorage nullable_output(nullable, true);
  CHECK_STATUS(execute_external(nullable, &nullable_output.out), PGACCEL_OK);
  CHECK(nullable_output.out.emitted_group_count == 3);
  CHECK(nullable_output.key_values[0] == std::vector<int32_t>({0, 1, 2}));
  CHECK(nullable_output.key_nulls[0] == std::vector<uint8_t>({0, 0, 1}));
  CHECK(nullable_output.measures[0].count == std::vector<uint64_t>({1, 1, 1}));
}

void test_device_publication_contract() {
  std::printf("--- device publication variable-width/no-write contract ---\n");
  SharedArray<int32_t> keys({3, 1, 6, 3});
  SharedArray<uint8_t> nulls({0, 0, 1, 0});
  pgaccel_grouped_agg_desc desc = base_desc(keys.size());
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  set_fact_key(desc, 0, keys.data(), nulls.data(), 0, 8, 7);
  set_count_star(desc, 0);

  OutputStorage output(desc, true);
  pgaccel_reset_gpu_exec_count();
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(pgaccel_gpu_exec_count() > 0);
  CHECK(output.out.emitted_group_count == 3);
  CHECK(output.out.selected_count == keys.size());
  CHECK(std::vector<int32_t>(output.key_values[0].begin(), output.key_values[0].begin() + 3) ==
        std::vector<int32_t>({1, 3, 7}));
  CHECK(std::vector<uint8_t>(output.key_nulls[0].begin(), output.key_nulls[0].begin() + 3) ==
        std::vector<uint8_t>({0, 0, 1}));
  CHECK(std::vector<uint64_t>(output.measures[0].count.begin(),
                              output.measures[0].count.begin() + 3) ==
        std::vector<uint64_t>({1, 2, 1}));
  for (size_t group = 3; group < desc.group_capacity; ++group) {
    CHECK(output.key_values[0][group] == std::numeric_limits<int32_t>::min());
    CHECK(output.key_nulls[0][group] == 0xa5);
    CHECK(output.measures[0].count[group] == OutputStorage::kSentinel);
  }

  pgaccel_grouped_agg_desc invalid = desc;
  ++invalid.abi_version;
  OutputStorage invalid_output(desc, true);
  pgaccel_reset_gpu_exec_count();
  CHECK_STATUS(execute_external(invalid, &invalid_output.out), PGACCEL_ERROR);
  CHECK(pgaccel_gpu_exec_count() == 0);
  CHECK(invalid_output.measures[0].count[0] == OutputStorage::kSentinel);

  nulls[1] = 2;
  OutputStorage device_failure(desc, true);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  pgaccel_reset_gpu_exec_count();
  CHECK_STATUS(execute_external_ex(desc, &device_failure.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(pgaccel_gpu_exec_count() > 0);
  CHECK(device_failure.out.emitted_group_count == 0);
  CHECK(device_failure.measures[0].count[0] == OutputStorage::kSentinel);
}

void test_group_activity_ignores_measure_validity() {
  std::printf("--- group activity ignores measure validity ---\n");
  SharedArray<int32_t> keys({0, 1});
  SharedArray<int64_t> values({7, 9});
  SharedArray<uint8_t> value_nulls({1, 1});
  pgaccel_grouped_agg_desc desc = base_desc(2);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, 2);
  set_i64_view(desc.measures[0].value, values.data(), value_nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.active == std::vector<uint8_t>({1, 1}));
  CHECK(output.out.emitted_group_count == 2);
  check_i64_lane(output, output.measures[0].sum, {0, 0});
  check_u64_lane(output.measures[0].count, {0, 0});
  check_u64_lane(output.measures[0].nonnull, {0, 0});
}

void test_integer_expression_overflow_semantics() {
  std::printf("--- integer expression overflow semantics ---\n");

  {
    SharedArray<int32_t> lhs({46340});
    SharedArray<int32_t> rhs({46340});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL,
                       PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                           PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.i64(output.measures[0].sum, 0) == INT64_C(2147395600));
    CHECK(output.i64(output.measures[0].min, 0) == INT64_C(2147395600));
    CHECK(output.i64(output.measures[0].max, 0) == INT64_C(2147395600));
    CHECK(output.measures[0].count[0] == 1);
  }

  {
    SharedArray<int32_t> lhs(
        {std::numeric_limits<int32_t>::max(), std::numeric_limits<int32_t>::min()});
    SharedArray<int32_t> rhs({0, 0});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                       PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                           PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.i64(output.measures[0].sum, 0) == -1);
    CHECK(output.i64(output.measures[0].min, 0) == std::numeric_limits<int32_t>::min());
    CHECK(output.i64(output.measures[0].max, 0) == std::numeric_limits<int32_t>::max());
    CHECK(output.measures[0].count[0] == 2);
  }

  constexpr std::array<uint32_t, 4> lanes = {
      PGACCEL_GROUPED_AGG_LANE_SUM,
      PGACCEL_GROUPED_AGG_LANE_MIN,
      PGACCEL_GROUPED_AGG_LANE_MAX,
      PGACCEL_GROUPED_AGG_LANE_COUNT,
  };
  for (const uint32_t lane : lanes) {
    SharedArray<int32_t> lhs({46341});
    SharedArray<int32_t> rhs({46341});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, lane);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }
  for (const uint32_t lane : lanes) {
    SharedArray<int32_t> lhs({std::numeric_limits<int32_t>::min()});
    SharedArray<int32_t> rhs({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB, lane);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }

  {
    SharedArray<int64_t> lhs({std::numeric_limits<int64_t>::min()});
    SharedArray<int64_t> rhs({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, lhs.data());
    set_i64_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB, PGACCEL_GROUPED_AGG_LANE_COUNT);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }
}

void test_error_and_unsupported_statuses() {
  std::printf("--- error and unsupported statuses ---\n");

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, nullptr), PGACCEL_ERROR);
  }

  {
    SharedArray<int32_t> keys({0, 2});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, 2);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(output.measures[0].count[0] == OutputStorage::kSentinel);
    CHECK(output.measures[0].count[1] == OutputStorage::kSentinel);
  }

  {
    SharedArray<int8_t> mask({PGACCEL_EXPR_TRUE, 2});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_count_star(desc, 0);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = mask.data();
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(output.measures[0].count[0] == OutputStorage::kSentinel);
  }

  {
    SharedArray<int64_t> values({std::numeric_limits<int64_t>::max(), 1});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_i64_view(desc.measures[0].value, values.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    CHECK(output.measures[0].sum[0] == OutputStorage::kSentinel);
  }

  {
    SharedArray<int32_t> fact0({0});
    SharedArray<int32_t> fact1({0});
    SharedArray<uint64_t> mult0({std::numeric_limits<uint64_t>::max()});
    SharedArray<uint64_t> mult1({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, fact0.data(), nullptr, 0, 1, nullptr, mult0.data());
    set_dim(desc, 1, fact1.data(), nullptr, 0, 1, nullptr, mult1.data());
    set_count_star(desc, 0);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    CHECK(output.measures[0].count[0] == OutputStorage::kSentinel);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    CHECK(req.bytes > 0);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes - 1;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    CHECK_STATUS(pgaccel_grouped_agg_execute(&desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.active_groups = reinterpret_cast<uint8_t*>(output.out.measures[0].count);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    SharedArray<int64_t> values({7});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, values.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    output.out.measures[0].nonnull_count = nullptr;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    SharedArray<int64_t> lhs({2});
    SharedArray<int64_t> rhs({3});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, lhs.data());
    set_i64_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    pgaccel_status status = PGACCEL_ERROR;
    workspace_req(desc, &status);
    CHECK(status == PGACCEL_UNSUPPORTED);
  }

  {
    SharedArray<int64_t> hash_key({42});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_HASH;
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    desc.group_capacity = 4;
    desc.key_count = 1;
    desc.keys[0].values.values = hash_key.data();
    desc.keys[0].values.type = PGACCEL_VAL_INT64;
    desc.keys[0].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
    desc.keys[0].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
    set_count_star(desc, 0);
    pgaccel_status status = PGACCEL_ERROR;
    workspace_req(desc, &status);
    CHECK(status == PGACCEL_OK);

    // Hash/H3 owners are current-call row indexes, so their reusable chunk
    // lifecycle is intentionally rejected until owner identity is detached
    // from the input slice.
    desc.execution_flags =
        PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    workspace_req(desc, &status);
    CHECK(status == PGACCEL_UNSUPPORTED);
  }
}

uint64_t next_random(uint64_t& state) {
  state ^= state << 13;
  state ^= state >> 7;
  state ^= state << 17;
  return state;
}

void test_fixed_seed_mixed_radix_fuzz() {
  std::printf("--- fixed-seed mixed-radix fuzz ---\n");
  uint64_t random_state = UINT64_C(0x3d6f9a20b14c57e1);
  constexpr std::array<uint32_t, 3> radices = {3, 3, 2};

  for (uint32_t iteration = 0; iteration < 12; ++iteration) {
    const uint32_t key_count = iteration % 4;
    const size_t rows = 17 + iteration;
    std::vector<int64_t> values(rows);
    std::vector<uint8_t> value_nulls(rows);
    std::vector<int8_t> masks(rows);
    std::array<std::vector<int32_t>, 3> key_values;
    std::array<std::vector<uint8_t>, 3> key_nulls;
    for (uint32_t key = 0; key < key_count; ++key) {
      key_values[key].resize(rows);
      key_nulls[key].resize(rows);
    }

    for (size_t row = 0; row < rows; ++row) {
      values[row] = static_cast<int64_t>(next_random(random_state) % 101) - 50;
      value_nulls[row] = (next_random(random_state) % 5 == 0) ? 1 : 0;
      masks[row] = (next_random(random_state) % 4 == 0) ? PGACCEL_EXPR_FALSE : PGACCEL_EXPR_TRUE;
      for (uint32_t key = 0; key < key_count; ++key) {
        const bool is_null = next_random(random_state) % 7 == 0;
        key_nulls[key][row] = is_null ? 1 : 0;
        key_values[key][row] = static_cast<int32_t>(next_random(random_state) % (radices[key] - 1));
      }
    }

    SharedArray<int64_t> values_usm(values);
    SharedArray<uint8_t> value_nulls_usm(value_nulls);
    SharedArray<int8_t> masks_usm(masks);
    std::array<SharedArray<int32_t>, 3> key_values_usm;
    std::array<SharedArray<uint8_t>, 3> key_nulls_usm;

    pgaccel_grouped_agg_desc desc = base_desc(rows);
    for (uint32_t key = 0; key < key_count; ++key) {
      key_values_usm[key] = SharedArray<int32_t>(key_values[key]);
      key_nulls_usm[key] = SharedArray<uint8_t>(key_nulls[key]);
      set_fact_key(desc, key, key_values_usm[key].data(), key_nulls_usm[key].data(), 0,
                   radices[key], static_cast<int32_t>(radices[key] - 1));
    }
    set_i64_view(desc.measures[0].value, values_usm.data(), value_nulls_usm.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                       PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = masks_usm.data();

    std::vector<uint8_t> expected_active(desc.group_capacity, 0);
    std::vector<int64_t> expected_sum(desc.group_capacity, 0);
    std::vector<uint64_t> expected_count(desc.group_capacity, 0);
    uint64_t expected_selected = 0;
    for (size_t row = 0; row < rows; ++row) {
      if (masks[row] != PGACCEL_EXPR_TRUE)
        continue;
      size_t group = 0;
      for (uint32_t key = 0; key < key_count; ++key) {
        const int32_t code = key_nulls[key][row] != 0 ? static_cast<int32_t>(radices[key] - 1)
                                                      : key_values[key][row];
        group = group * radices[key] + static_cast<size_t>(code);
      }
      expected_active[group] = 1;
      ++expected_selected;
      if (value_nulls[row] == 0) {
        expected_sum[group] += values[row];
        ++expected_count[group];
      }
    }
    if (key_count == 0)
      expected_active[0] = 1;

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == expected_selected);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.active == expected_active);
    CHECK(output.out.emitted_group_count ==
          static_cast<size_t>(std::count(expected_active.begin(), expected_active.end(), 1)));
    for (size_t group = 0; group < desc.group_capacity; ++group) {
      CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
      CHECK(output.measures[0].count[group] == expected_count[group]);
      CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    }
  }
}

void test_dense_full_lane_reference() {
  std::printf("--- dense full-lane reference ---\n");
  constexpr size_t rows = 8;
  constexpr size_t groups_count = 3;
  SharedArray<int32_t> groups({10, 11, 10, 12, 10, 11, 12, 12});
  SharedArray<double> values({1.0, 2.5, 3.0, 4.0, 5.0, 6.5, 8.0, 16.0});
  constexpr std::array<double, groups_count> expected_sum = {9.0, 9.0, 28.0};
  constexpr std::array<double, groups_count> expected_min = {1.0, 2.5, 4.0};
  constexpr std::array<double, groups_count> expected_max = {5.0, 6.5, 16.0};
  constexpr std::array<uint64_t, groups_count> expected_count = {3, 2, 3};

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, groups.data(), nullptr, 10, groups_count);
  set_f64_view(desc.measures[0].value, values.data());
  finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups_count);
  for (size_t group = 0; group < groups_count; ++group) {
    CHECK(output.f64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.f64(output.measures[0].min, group) == expected_min[group]);
    CHECK(output.f64(output.measures[0].max, group) == expected_max[group]);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(output.active[group] == 1);
  }
}

void test_filtered_product_reference() {
  std::printf("--- filtered product reference ---\n");
  SharedArray<int32_t> orderdate({19930001, 19930002, 19930003, 19930004, 19940101, 19940102,
                                  19940103, 19940104, 19940040, 19940041, 19940042, 19940043});
  SharedArray<int32_t> discount({1, 2, 3, 4, 4, 5, 6, 7, 5, 6, 7, 8});
  SharedArray<int32_t> quantity({10, 20, 24, 25, 26, 30, 35, 36, 26, 31, 35, 40});
  SharedArray<int32_t> extendedprice(
      {100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200});
  constexpr size_t rows = 12;

  std::vector<int8_t> selection(rows, PGACCEL_EXPR_FALSE);
  for (size_t row = 0; row < rows; ++row) {
    if (orderdate[row] >= 19930001 && orderdate[row] <= 19930099 && discount[row] >= 1 &&
        discount[row] <= 3 && quantity[row] >= 0 && quantity[row] <= 24)
      selection[row] = PGACCEL_EXPR_TRUE;
  }
  SharedArray<int8_t> selection_usm(selection);

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_i32_view(desc.measures[0].value, extendedprice.data());
  set_i32_view(desc.measures[0].rhs, discount.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = selection_usm.data();
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.i64(output.measures[0].sum, 0) == 1400);
  CHECK(output.out.selected_count == 3);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.measures[0].nonnull[0] == 3);
  CHECK(output.active[0] == 1);
}

void test_two_key_dimension_reference() {
  std::printf("--- two-key dimension reference ---\n");
  constexpr size_t rows = 8;
  constexpr size_t group_count = 8;
  SharedArray<int32_t> orderdate({100, 101, 102, 103, 100, 101, 102, 103});
  SharedArray<int32_t> partkey({1, 2, 3, 4, 2, 3, 4, 1});
  SharedArray<int32_t> suppkey({1, 1, 1, 1, 2, 2, 2, 2});
  SharedArray<int32_t> revenue({10, 20, 30, 40, 50, 60, 70, 80});
  SharedArray<int32_t> date_year({1992, 1992, 1993, 1993});
  SharedArray<int32_t> part_brand({-1, 0, 1, 2, 3});
  SharedArray<uint8_t> part_match({0, 1, 1, 0, 1});
  SharedArray<uint8_t> supplier_match({0, 1, 0});

  constexpr std::array<int64_t, group_count> expected_sum = {10, 20, 0, 0, 0, 0, 0, 40};
  constexpr std::array<uint64_t, group_count> expected_count = {1, 1, 0, 0, 0, 0, 0, 1};

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, orderdate.data(), nullptr, 100, date_year.size());
  set_dim(desc, 1, partkey.data(), nullptr, 0, part_brand.size(), part_match.data());
  set_dim(desc, 2, suppkey.data(), nullptr, 0, supplier_match.size(), supplier_match.data());
  set_dim_key(desc, 0, 0, date_year.data(), 1992, 2);
  set_dim_key(desc, 1, 1, part_brand.data(), 0, 4);
  set_i32_view(desc.measures[0].value, revenue.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 3);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 3);
  for (size_t group = 0; group < group_count; ++group) {
    CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(output.active[group] == (expected_count[group] != 0 ? 1 : 0));
  }
}

void test_three_key_dimension_reference() {
  std::printf("--- three-key dimension reference ---\n");
  constexpr size_t rows = 8;
  constexpr size_t group_count = 8;
  SharedArray<int32_t> orderdate({100, 101, 102, 103, 100, 101, 102, 103});
  SharedArray<int32_t> custkey({1, 2, 1, 2, 3, 1, 2, 1});
  SharedArray<int32_t> suppkey({1, 1, 2, 2, 1, 2, 1, 3});
  SharedArray<int32_t> revenue({10, 20, 30, 40, 50, 60, 70, 80});
  SharedArray<int32_t> date_year({1992, 1992, 1993, 1993});
  SharedArray<uint8_t> date_match({1, 1, 1, 0});
  SharedArray<int32_t> customer_code({-1, 0, 1, 0});
  SharedArray<uint8_t> customer_match({0, 1, 1, 0});
  SharedArray<int32_t> supplier_code({-1, 0, 1, 0});
  SharedArray<uint8_t> supplier_match({0, 1, 1, 0});

  constexpr std::array<int64_t, group_count> expected_sum = {10, 60, 20, 0, 0, 30, 70, 0};
  constexpr std::array<uint64_t, group_count> expected_count = {1, 1, 1, 0, 0, 1, 1, 0};

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, orderdate.data(), nullptr, 100, date_year.size(), date_match.data());
  set_dim(desc, 1, custkey.data(), nullptr, 0, customer_code.size(), customer_match.data());
  set_dim(desc, 2, suppkey.data(), nullptr, 0, supplier_code.size(), supplier_match.data());
  set_dim_key(desc, 0, 0, date_year.data(), 1992, 2);
  set_dim_key(desc, 1, 1, customer_code.data(), 0, 2);
  set_dim_key(desc, 2, 2, supplier_code.data(), 0, 2);
  set_i32_view(desc.measures[0].value, revenue.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 5);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 5);
  for (size_t group = 0; group < group_count; ++group) {
    CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(output.active[group] == (expected_count[group] != 0 ? 1 : 0));
  }
}

void test_dimension_subtraction_reference() {
  std::printf("--- dimension subtraction reference ---\n");
  constexpr size_t rows = 8;
  constexpr size_t group_count = 8;
  SharedArray<int32_t> orderdate({100, 101, 102, 103, 100, 101, 102, 102});
  SharedArray<int32_t> custkey({1, 2, 1, 2, 1, 1, 2, 1});
  SharedArray<int32_t> suppkey({1, 1, 2, 2, 1, 2, 1, 2});
  SharedArray<int32_t> partkey({1, 1, 2, 2, 3, 1, 2, 2});
  SharedArray<int32_t> revenue({100, 50, 30, 40, 80, 10, 70, 60});
  SharedArray<int32_t> supplycost({40, 70, 10, 5, 10, 30, 20, 50});
  SharedArray<int32_t> date_year({1992, 1992, 1993, 1993});
  SharedArray<uint8_t> date_match({1, 1, 1, 0});
  SharedArray<int32_t> customer_code({-1, 0, 1});
  SharedArray<uint8_t> customer_match({0, 1, 1});
  SharedArray<int32_t> supplier_code({-1, 0, 1});
  SharedArray<uint8_t> supplier_match({0, 1, 1});
  SharedArray<int32_t> part_code({-1, 0, 1, 0});
  SharedArray<uint8_t> part_match({0, 1, 1, 0});
  constexpr std::array<int64_t, group_count> expected_sum = {40, 0, -20, 0, 0, 50, 0, 30};
  constexpr std::array<uint64_t, group_count> expected_count = {2, 0, 1, 0, 0, 1, 0, 2};

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, orderdate.data(), nullptr, 100, date_year.size(), date_match.data());
  set_dim(desc, 1, custkey.data(), nullptr, 0, customer_code.size(), customer_match.data());
  set_dim(desc, 2, suppkey.data(), nullptr, 0, supplier_code.size(), supplier_match.data());
  set_dim(desc, 3, partkey.data(), nullptr, 0, part_code.size(), part_match.data());
  set_dim_key(desc, 0, 0, date_year.data(), 1992, 2);
  set_dim_key(desc, 1, 2, supplier_code.data(), 0, 2);
  set_dim_key(desc, 2, 3, part_code.data(), 0, 2);
  set_i32_view(desc.measures[0].value, revenue.data());
  set_i32_view(desc.measures[0].rhs, supplycost.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 6);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 4);
  for (size_t group = 0; group < group_count; ++group) {
    CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(output.active[group] == (expected_count[group] != 0 ? 1 : 0));
  }
}

}  // namespace

int main() {
  try {
    const pgaccel_status init_status = pgaccel_init();
    CHECK(init_status == PGACCEL_OK);
    if (init_status != PGACCEL_OK)
      return 1;

    const pgaccel_device_info info = pgaccel_get_device_info();
    CHECK(std::string(info.backend_name).find("Metal") != std::string::npos ||
          std::string(info.backend_name).find("metal") != std::string::npos);

    test_workspace_and_descriptor_validation();
    test_dense_chunk_lifecycle_equivalence();
    test_dense_chunk_lifecycle_fail_closed();
    test_empty_ungrouped_active();
    test_i64_four_measure_lanes();
    test_f64_stats_pair_and_nan_ordering();
    test_global_and_measure_filters();
    test_predicate_only_physical_count();
    test_four_dimensions_and_multiplicity();
    test_mixed_radix_compact_and_keyed_empty();
    test_device_publication_contract();
    test_group_activity_ignores_measure_validity();
    test_integer_expression_overflow_semantics();
    test_error_and_unsupported_statuses();
    test_fixed_seed_mixed_radix_fuzz();
    test_dense_full_lane_reference();
    test_filtered_product_reference();
    test_two_key_dimension_reference();
    test_three_key_dimension_reference();
    test_dimension_subtraction_reference();

    CHECK_STATUS(pgaccel_shutdown(), PGACCEL_OK);
  } catch (const std::exception& exception) {
    std::fprintf(stderr, "FAIL: unexpected test infrastructure exception: %s\n", exception.what());
    ++g_fail;
  }

  std::printf("test_grouped_agg: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

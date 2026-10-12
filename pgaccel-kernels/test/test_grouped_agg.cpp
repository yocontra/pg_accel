#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <numeric>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"
#include "pgaccel_scalar.hpp"

extern "C" void pgacceltest_grouped_agg_fail_stage_once(unsigned stage);
extern "C" unsigned pgacceltest_grouped_agg_active_scratch_owners(void);
extern "C" bool pgacceltest_grouped_agg_seed_integer_prefix(
    const pgaccel_grouped_agg_desc*, void*, uint64_t, int64_t);

namespace {

static_assert(sizeof(pgaccel_scalar_input) == 24);
static_assert(offsetof(pgaccel_scalar_input, values) == 0);
static_assert(offsetof(pgaccel_scalar_input, nulls) == 8);
static_assert(offsetof(pgaccel_scalar_input, physical_type) == 16);
static_assert(offsetof(pgaccel_scalar_input, reserved) == 20);
static_assert(sizeof(pgaccel_scalar_view) == 40);
static_assert(offsetof(pgaccel_scalar_view, version) == 0);
static_assert(offsetof(pgaccel_scalar_view, word_count) == 4);
static_assert(offsetof(pgaccel_scalar_view, input_count) == 8);
static_assert(offsetof(pgaccel_scalar_view, reserved) == 12);
static_assert(offsetof(pgaccel_scalar_view, row_count) == 16);
static_assert(offsetof(pgaccel_scalar_view, words) == 24);
static_assert(offsetof(pgaccel_scalar_view, inputs) == 32);

static_assert(sizeof(pgaccel_numeric_row) == 16);
static_assert(offsetof(pgaccel_numeric_row, ndigits) == 8);
static_assert(offsetof(pgaccel_numeric_row, weight) == 10);
static_assert(offsetof(pgaccel_numeric_row, sign) == 12);
static_assert(offsetof(pgaccel_numeric_row, dscale) == 14);
static_assert(sizeof(pgaccel_numeric_view) == 48);
static_assert(offsetof(pgaccel_numeric_view, digit_count) == 16);
static_assert(offsetof(pgaccel_numeric_view, row_count) == 24);
static_assert(offsetof(pgaccel_numeric_view, min_weight) == 32);
static_assert(offsetof(pgaccel_numeric_view, max_weight) == 36);
static_assert(offsetof(pgaccel_numeric_view, max_ndigits) == 40);
static_assert(offsetof(pgaccel_numeric_view, reserved) == 44);
static_assert(sizeof(pgaccel_numeric_sum) == 16);
static_assert(offsetof(pgaccel_numeric_sum, ndigits) == 4);
static_assert(offsetof(pgaccel_numeric_sum, sign) == 8);
static_assert(offsetof(pgaccel_numeric_sum, dscale) == 12);

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

pgaccel_val val_i32(int32_t value) {
  pgaccel_val result = {};
  result.tag = PGACCEL_VAL_INT32;
  result.data.i32 = value;
  return result;
}

void set_i32_value_range(pgaccel_grouped_agg_desc& desc, uint32_t measure_slot, int32_t lo,
                         int32_t hi) {
  desc.where_filter = disabled_filter();
  desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  desc.where_filter.predicate_measure_slot = static_cast<int32_t>(measure_slot);
  desc.where_filter.predicate_range_count = 1;
  desc.where_filter.predicate_lo[0] = val_i32(lo);
  desc.where_filter.predicate_hi[0] = val_i32(hi);
}

void set_i32_measure_value_range(pgaccel_grouped_agg_desc& desc, uint32_t filter_slot,
                                 uint32_t measure_slot, int32_t lo, int32_t hi) {
  desc.measure_filters[filter_slot] = disabled_filter();
  desc.measure_filters[filter_slot].predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  desc.measure_filters[filter_slot].predicate_measure_slot = static_cast<int32_t>(measure_slot);
  desc.measure_filters[filter_slot].predicate_range_count = 1;
  desc.measure_filters[filter_slot].predicate_lo[0] = val_i32(lo);
  desc.measure_filters[filter_slot].predicate_hi[0] = val_i32(hi);
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
        storage.sum.assign(capacity * desc.measures[i].state_bytes / sizeof(uint64_t), kSentinel);
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
      key.values.nulls =
          static_cast<const uint8_t*>(advance_bytes(key.values.nulls, first_row, sizeof(uint8_t)));
    }
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    pgaccel_grouped_agg_measure& measure = desc.measures[i];
    measure.value.values =
        advance_bytes(measure.value.values, first_row, measure.value.element_bytes);
    measure.value.nulls =
        static_cast<const uint8_t*>(advance_bytes(measure.value.nulls, first_row, sizeof(uint8_t)));
    measure.rhs.values = advance_bytes(measure.rhs.values, first_row, measure.rhs.element_bytes);
    measure.rhs.nulls =
        static_cast<const uint8_t*>(advance_bytes(measure.rhs.nulls, first_row, sizeof(uint8_t)));
    desc.measure_filters[i].mask = static_cast<const int8_t*>(
        advance_bytes(desc.measure_filters[i].mask, first_row, sizeof(int8_t)));
  }
  desc.where_filter.mask =
      static_cast<const int8_t*>(advance_bytes(desc.where_filter.mask, first_row, sizeof(int8_t)));
  for (size_t i = 0; i < desc.dim_count; ++i) {
    pgaccel_grouped_agg_dim& dim = desc.dims[i];
    dim.fact_key.values = advance_bytes(dim.fact_key.values, first_row, sizeof(int32_t));
    dim.fact_key.nulls =
        static_cast<const uint8_t*>(advance_bytes(dim.fact_key.nulls, first_row, sizeof(uint8_t)));
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

void check_device_invalid(const pgaccel_grouped_agg_desc& desc) {
  OutputStorage output(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
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
  set_fact_key(malformed, 0, nullptr, nullptr, 0, 1);
  malformed.keys[0].flags = 1;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  set_fact_key(malformed, 0, nullptr, nullptr, 0, 0);
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  set_fact_key(malformed, 0, nullptr, nullptr, 0, 1);
  malformed.keys[0].values.type = PGACCEL_VAL_INT64;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.key_count = 1;
  malformed.keys[0].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0;
  malformed.keys[0].cardinality = 1;
  malformed.keys[0].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.group_capacity = 0;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_HASH;
  malformed.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  malformed.group_capacity = 0;
  malformed.key_count = 1;
  malformed.keys[0].values.type = PGACCEL_VAL_INT64;
  malformed.keys[0].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
  malformed.keys[0].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.measures[1].flags = 1;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.measure_filters[0].kind = 99;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.dim_count = 1;
  malformed.dims[0].fact_key.type = PGACCEL_VAL_INT32;
  malformed.dims[0].flags = 1;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);

  malformed = desc;
  malformed.dims[1].flags = 1;
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
                                  PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_UNCERTAIN,
                                  PGACCEL_EXPR_TRUE});
  SharedArray<int8_t> measure_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE, PGACCEL_EXPR_UNCERTAIN, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE});
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
      chunk.execution_flags =
          row == 0 ? PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
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
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &output.out, &detail), PGACCEL_OK);
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
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &output.out, &detail), PGACCEL_OK);
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
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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
    CHECK_STATUS(execute_in_workspace(reset, req, workspace, &recovered.out, &detail), PGACCEL_OK);
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
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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
    reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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

void test_parallel_dense_count_star_lifecycle() {
  std::printf("--- parallel dense COUNT(*) lifecycle ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 17;
  constexpr int32_t code_min = -8;
  constexpr int32_t null_code = 8;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_nulls(rows);
  std::array<uint64_t, groups> expected{};
  for (size_t row = 0; row < rows; ++row) {
    const bool is_null = row % 113 == 0;
    host_nulls[row] = is_null ? 1 : 0;
    host_keys[row] = is_null ? INT32_MAX : code_min + static_cast<int32_t>(row % 16);
    const size_t group = is_null ? groups - 1 : row % 16;
    ++expected[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nulls.data(), code_min, groups, null_code);
  set_count_star(desc, 0);

  OutputStorage one_shot(desc, true, true);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  CHECK(one_shot.out.selected_count == rows);
  CHECK(one_shot.out.uncertain_count == 0);
  CHECK(one_shot.out.emitted_group_count == groups);
  CHECK(one_shot.active == std::vector<uint8_t>(groups, 1));
  for (size_t group = 0; group < groups; ++group) {
    CHECK(one_shot.group_codes[group] == group);
    CHECK(one_shot.key_values[0][group] == code_min + static_cast<int32_t>(group));
    CHECK(one_shot.key_nulls[0][group] == (group + 1 == groups ? 1 : 0));
    CHECK(one_shot.measures[0].count[group] == expected[group]);
  }

  OutputStorage timed(desc, true, true);
  const auto timed_start = std::chrono::steady_clock::now();
  CHECK_STATUS(execute_external(desc, &timed.out), PGACCEL_OK);
  const auto timed_end = std::chrono::steady_clock::now();
  const double timed_ms =
      std::chrono::duration<double, std::milli>(timed_end - timed_start).count();
  std::printf("parallel dense COUNT(*) %zu rows/%zu groups: %.3f ms\n", rows, groups, timed_ms);
  check_outputs_equal(timed, one_shot, desc.measure_count);

  const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  constexpr size_t first_rows = 100003;
  pgaccel_grouped_agg_desc first = row_slice(desc, 0, first_rows);
  first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  pgaccel_grouped_agg_desc second = row_slice(desc, first_rows, rows - first_rows);
  second.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_OK);
  pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  check_outputs_equal(chunked, one_shot, desc.measure_count);

  nulls[7] = 2;
  pgaccel_grouped_agg_desc invalid = row_slice(desc, 7, 1);
  invalid.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_in_workspace(invalid, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  pgaccel_grouped_agg_desc poisoned = row_slice(desc, 8, 1);
  poisoned.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_in_workspace(poisoned, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

  nulls[7] = 0;
  OutputStorage recovered(desc, true, true);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &recovered.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  check_outputs_equal(recovered, one_shot, desc.measure_count);

  keys[19] = 99;
  OutputStorage invalid_code(desc);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid_code.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(invalid_code.measures[0].count[0] == OutputStorage::kSentinel);
}

void test_parallel_dense_int2_count_column() {
  std::printf("--- parallel dense nullable INT2 column COUNT ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 3;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_key_nulls(rows);
  std::vector<int32_t> host_values(rows);
  std::vector<uint8_t> host_value_nulls(rows);
  std::array<uint64_t, groups> expected_selected{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const bool null_key = row % 19 == 0;
    const size_t group = null_key ? 2 : row % 2;
    host_keys[row] = null_key ? INT32_MAX : static_cast<int32_t>(group);
    host_key_nulls[row] = null_key ? 1 : 0;
    host_values[row] = row % 2 == 0 ? INT16_MIN : INT16_MAX;
    const bool null_value = row % 11 == 0 || group == 2;
    host_value_nulls[row] = null_value ? 1 : 0;
    ++expected_selected[group];
    if (!null_value)
      ++expected_count[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> key_nulls(host_key_nulls);
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> value_nulls(host_value_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, 2);
  set_count_only_view(desc, 0, values.data(), value_nulls.data(),
                      PGACCEL_GROUPED_AGG_PHYSICAL_INT32, sizeof(int32_t),
                      PGACCEL_GROUPED_AGG_ACCUM_I64);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.active[group] == 1);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(expected_selected[group] != 0);
  }
  CHECK(output.measures[0].count[2] == 0);

  value_nulls[31] = 2;
  OutputStorage invalid(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_parallel_dense_int8_count_column() {
  std::printf("--- parallel dense nullable INT8 column COUNT ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 3;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_key_nulls(rows);
  std::vector<int64_t> host_values(rows);
  std::vector<uint8_t> host_value_nulls(rows);
  std::array<uint64_t, groups> expected_selected{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const bool null_key = row % 19 == 0;
    const size_t group = null_key ? 2 : row % 2;
    host_keys[row] = null_key ? INT32_MAX : static_cast<int32_t>(group);
    host_key_nulls[row] = null_key ? 1 : 0;
    host_values[row] = row % 2 == 0 ? INT64_MIN : INT64_MAX;
    const bool null_value = row % 11 == 0 || group == 2;
    host_value_nulls[row] = null_value ? 1 : 0;
    ++expected_selected[group];
    if (!null_value)
      ++expected_count[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> key_nulls(host_key_nulls);
  SharedArray<int64_t> values(host_values);
  SharedArray<uint8_t> value_nulls(host_value_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, 2);
  set_count_only_view(desc, 0, values.data(), value_nulls.data(),
                      PGACCEL_GROUPED_AGG_PHYSICAL_INT64, sizeof(int64_t),
                      PGACCEL_GROUPED_AGG_ACCUM_I64);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.active[group] == 1);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(expected_selected[group] != 0);
  }
  CHECK(output.measures[0].count[2] == 0);

  value_nulls[37] = 255;
  OutputStorage invalid(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_parallel_dense_date_count_column() {
  std::printf("--- parallel dense nullable DATE column COUNT ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 3;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_key_nulls(rows);
  std::vector<int32_t> host_values(rows);
  std::vector<uint8_t> host_value_nulls(rows);
  std::array<uint64_t, groups> expected_selected{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const bool null_key = row % 19 == 0;
    const size_t group = null_key ? 2 : row % 2;
    host_keys[row] = null_key ? INT32_MAX : static_cast<int32_t>(group);
    host_key_nulls[row] = null_key ? 1 : 0;
    host_values[row] = row % 2 == 0 ? INT32_MIN : INT32_MAX;
    const bool null_value = row % 11 == 0 || group == 2;
    host_value_nulls[row] = null_value ? 1 : 0;
    ++expected_selected[group];
    if (!null_value)
      ++expected_count[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> key_nulls(host_key_nulls);
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> value_nulls(host_value_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, 2);
  set_count_only_view(desc, 0, values.data(), value_nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                      sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.active[group] == 1);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(expected_selected[group] != 0);
  }
  CHECK(output.measures[0].count[2] == 0);

  value_nulls[41] = 7;
  OutputStorage invalid(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_parallel_dense_timestamp_count_column() {
  std::printf("--- parallel dense nullable TIMESTAMP column COUNT ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 3;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_key_nulls(rows);
  std::vector<int64_t> host_values(rows);
  std::vector<uint8_t> host_value_nulls(rows);
  std::array<uint64_t, groups> expected_selected{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const bool null_key = row % 19 == 0;
    const size_t group = null_key ? 2 : row % 2;
    host_keys[row] = null_key ? INT32_MAX : static_cast<int32_t>(group);
    host_key_nulls[row] = null_key ? 1 : 0;
    host_values[row] = row % 2 == 0 ? INT64_MIN : INT64_MAX;
    const bool null_value = row % 11 == 0 || group == 2;
    host_value_nulls[row] = null_value ? 1 : 0;
    ++expected_selected[group];
    if (!null_value)
      ++expected_count[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> key_nulls(host_key_nulls);
  SharedArray<int64_t> values(host_values);
  SharedArray<uint8_t> value_nulls(host_value_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, 2);
  set_count_only_view(desc, 0, values.data(), value_nulls.data(),
                      PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, sizeof(int64_t),
                      PGACCEL_GROUPED_AGG_ACCUM_I64);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.active[group] == 1);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(expected_selected[group] != 0);
  }
  CHECK(output.measures[0].count[2] == 0);

  value_nulls[43] = 3;
  OutputStorage invalid(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

template <typename T>
void run_parallel_dense_float_count_column(int32_t physical_type) {
  constexpr size_t rows = 262144;
  constexpr size_t groups = 3;
  std::vector<int32_t> host_keys(rows);
  std::vector<uint8_t> host_key_nulls(rows);
  std::vector<T> host_values(rows);
  std::vector<uint8_t> host_value_nulls(rows);
  std::array<uint64_t, groups> expected_selected{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const bool null_key = row % 19 == 0;
    const size_t group = null_key ? 2 : row % 2;
    host_keys[row] = null_key ? INT32_MAX : static_cast<int32_t>(group);
    host_key_nulls[row] = null_key ? 1 : 0;
    switch (row % 5) {
      case 0:
        host_values[row] = std::numeric_limits<T>::quiet_NaN();
        break;
      case 1:
        host_values[row] = std::numeric_limits<T>::infinity();
        break;
      case 2:
        host_values[row] = -std::numeric_limits<T>::infinity();
        break;
      case 3:
        host_values[row] = T{0};
        break;
      default:
        host_values[row] = -T{0};
        break;
    }
    const bool null_value = row % 11 == 0 || group == 2;
    host_value_nulls[row] = null_value ? 1 : 0;
    ++expected_selected[group];
    if (!null_value)
      ++expected_count[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<uint8_t> key_nulls(host_key_nulls);
  SharedArray<T> values(host_values);
  SharedArray<uint8_t> value_nulls(host_value_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, 2);
  set_count_only_view(desc, 0, values.data(), value_nulls.data(), physical_type, sizeof(T),
                      PGACCEL_GROUPED_AGG_ACCUM_I64);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.active[group] == 1);
    CHECK(output.measures[0].count[group] == expected_count[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(expected_selected[group] != 0);
  }
  CHECK(output.measures[0].count[2] == 0);

  value_nulls[47] = 5;
  OutputStorage invalid(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_parallel_dense_float_count_columns() {
  std::printf("--- parallel dense nullable FLOAT column COUNT ---\n");
  run_parallel_dense_float_count_column<float>(PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32);
  run_parallel_dense_float_count_column<double>(PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64);
}

void test_parallel_dense_integer_phase2_shape() {
  std::printf("--- parallel dense Phase2 integer lanes ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t groups = 17;
  std::vector<int32_t> host_keys(rows);
  std::vector<int32_t> host_values(rows);
  std::vector<uint8_t> host_nulls(rows);
  std::array<int64_t, groups> expected_sum{};
  std::array<int64_t, groups> expected_min{};
  std::array<int64_t, groups> expected_max{};
  std::array<uint64_t, groups> expected_nonnull{};
  std::array<uint64_t, groups> expected_count{};
  expected_min.fill(INT64_MAX);
  expected_max.fill(INT64_MIN);
  for (size_t row = 0; row < rows; ++row) {
    const size_t group = row % groups;
    host_keys[row] = static_cast<int32_t>(group);
    host_nulls[row] = row % 127 == 0 ? 1 : 0;
    const int32_t value =
        row % 257 == 0 ? INT32_MAX
                       : (row % 263 == 0 ? INT32_MIN : static_cast<int32_t>(row % 997) - 498);
    host_values[row] = value;
    ++expected_count[group];
    if (host_nulls[row] != 0)
      continue;
    expected_sum[group] += value;
    expected_min[group] = std::min(expected_min[group], static_cast<int64_t>(value));
    expected_max[group] = std::max(expected_max[group], static_cast<int64_t>(value));
    ++expected_nonnull[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  set_i32_view(desc.measures[0].value, values.data(), nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX);
  set_count_star(desc, 1);

  OutputStorage one_shot(desc, true, true);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  CHECK(one_shot.out.selected_count == rows);
  CHECK(one_shot.out.uncertain_count == 0);
  CHECK(one_shot.out.emitted_group_count == groups);
  CHECK(one_shot.active == std::vector<uint8_t>(groups, 1));
  for (size_t group = 0; group < groups; ++group) {
    CHECK(one_shot.i64(one_shot.measures[0].sum, group) == expected_sum[group]);
    CHECK(one_shot.i64(one_shot.measures[0].min, group) == expected_min[group]);
    CHECK(one_shot.i64(one_shot.measures[0].max, group) == expected_max[group]);
    CHECK(one_shot.measures[0].nonnull[group] == expected_nonnull[group]);
    CHECK(one_shot.measures[1].count[group] == expected_count[group]);
  }

  OutputStorage timed(desc, true, true);
  const auto timed_start = std::chrono::steady_clock::now();
  CHECK_STATUS(execute_external(desc, &timed.out), PGACCEL_OK);
  const auto timed_end = std::chrono::steady_clock::now();
  const double timed_ms =
      std::chrono::duration<double, std::milli>(timed_end - timed_start).count();
  std::printf("parallel dense SUM/MIN/MAX+COUNT(*) %zu rows/%zu groups: %.3f ms\n", rows, groups,
              timed_ms);
  check_outputs_equal(timed, one_shot, desc.measure_count);

  // One-shot extrema now uses atomic scratch. A reusable multi-batch session
  // must request its partial workspace using the session execution flags.
  auto session = desc;
  session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req req = workspace_req(session);
  SharedWorkspace workspace(req.bytes, req.alignment);
  constexpr std::array<size_t, 3> chunk_rows = {65537, 100003, rows - 165540};
  size_t first_row = 0;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  for (size_t index = 0; index < chunk_rows.size(); ++index) {
    pgaccel_grouped_agg_desc chunk = row_slice(desc, first_row, chunk_rows[index]);
    chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    if (index == 0)
      chunk.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
    CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    first_row += chunk_rows[index];
  }
  pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  check_outputs_equal(chunked, one_shot, desc.measure_count);

  nulls[31] = 2;
  OutputStorage invalid_null(desc);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &invalid_null.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(invalid_null.measures[0].sum[0] == OutputStorage::kSentinel);
  nulls[31] = host_nulls[31];

  keys[41] = 99;
  pgaccel_grouped_agg_desc invalid_key = row_slice(desc, 41, 1);
  invalid_key.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_in_workspace(invalid_key, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  pgaccel_grouped_agg_desc poisoned = row_slice(desc, 42, 1);
  poisoned.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(poisoned, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  keys[41] = host_keys[41];
  OutputStorage recovered(desc, true, true);
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &recovered.out, &detail), PGACCEL_OK);
  check_outputs_equal(recovered, one_shot, desc.measure_count);
}

void test_parallel_dense_global_integer_filters(bool with_mask, size_t groups = 1,
                                                bool with_ranges = false, bool rhs_filter = false) {
  std::printf("--- global exact integer SUM/COUNT with independent filters ---\n");
  constexpr size_t rows = 2053;
  std::vector<int32_t> host_values(rows);
  std::vector<uint8_t> host_nulls(rows);
  std::vector<int8_t> host_mask(rows);
  std::vector<int32_t> host_predicates(rows);
  std::vector<uint8_t> host_predicate_nulls(rows);
  int64_t negative_sum = 0;
  int64_t negative_min = 0, negative_max = 0;
  uint64_t negative_count = 0, positive_count = 0, selected = 0;
  for (size_t row = 0; row < rows; ++row) {
    host_values[row] = row % 2 == 0 ? INT32_MIN : INT32_MAX;
    host_nulls[row] = row % 11 == 0;
    host_mask[row] = row % 3 == 0 ? -1 : row % 3 == 1 ? 0 : 1;
    host_predicates[row] = static_cast<int32_t>(row % 7) - 3;
    host_predicate_nulls[row] = row % 13 == 0;
    if (with_ranges && (host_predicate_nulls[row] != 0 ||
                        !((host_predicates[row] >= -2 && host_predicates[row] <= -1) ||
                          (host_predicates[row] >= 1 && host_predicates[row] <= 2))))
      continue;
    if (with_mask && host_mask[row] != 1)
      continue;
    ++selected;
    if (host_nulls[row] == 0) {
      if ((!rhs_filter && host_values[row] < 0) ||
          (rhs_filter && !host_predicate_nulls[row] && host_predicates[row] < 0)) {
        negative_sum += host_values[row];
        if (negative_count == 0 || host_values[row] < negative_min)
          negative_min = host_values[row];
        if (negative_count == 0 || host_values[row] > negative_max)
          negative_max = host_values[row];
        ++negative_count;
      }
      if ((!rhs_filter && host_values[row] > 0) ||
          (rhs_filter && !host_predicate_nulls[row] && host_predicates[row] > 0)) {
        ++positive_count;
      }
    }
  }
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> nulls(host_nulls);
  SharedArray<int8_t> mask(host_mask);
  SharedArray<int32_t> predicates(host_predicates);
  SharedArray<uint8_t> predicate_nulls(host_predicate_nulls);
  SharedArray<int32_t> keys(std::vector<int32_t>(rows, 0));
  auto desc = base_desc(rows);
  // Extra dictionary capacity exercises hierarchical (3) and atomic (17)
  // dispatch, while all accepted rows belong to group zero.
  if (groups > 1)
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  if (with_mask) {
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = mask.data();
  }
  set_i32_view(desc.measures[0].value, values.data(), nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT |
                         PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
  set_i32_view(desc.measures[1].value, values.data(), nulls.data());
  finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_count_star(desc, 2);
  set_i32_measure_value_range(desc, 0, 0, INT32_MIN, -1);
  set_i32_measure_value_range(desc, 1, 0, 1, INT32_MAX);
  if (rhs_filter) {
    for (size_t slot = 0; slot < 2; ++slot) {
      set_i32_view(desc.measures[slot].rhs, predicates.data(), predicate_nulls.data());
      desc.measures[slot].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
      desc.measure_filters[slot].predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
      desc.measure_filters[slot].predicate_measure_slot = static_cast<int32_t>(slot);
    }
  }
  if (with_ranges) {
    set_i32_view(desc.measures[1].value, predicates.data(), predicate_nulls.data());
    desc.where_filter = desc.measure_filters[1];
    desc.where_filter.predicate_measure_slot = 1;
    desc.where_filter.predicate_range_count = 2;
    desc.where_filter.predicate_lo[0] = val_i32(-2);
    desc.where_filter.predicate_hi[0] = val_i32(-1);
    desc.where_filter.predicate_lo[1] = val_i32(1);
    desc.where_filter.predicate_hi[1] = val_i32(2);
    if (with_mask) {
      desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
      desc.where_filter.mask = mask.data();
    }
  }
  int32_t mode = -1;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
  CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
  if (rhs_filter) {
    auto invalid = desc;
    invalid.measure_filters[0].predicate_measure_slot = 1;
    CHECK(pgaccel_grouped_agg_kernel_mode(&invalid, &mode) != PGACCEL_OK);
    invalid = desc;
    invalid.measures[0].flags = 0;
    CHECK(pgaccel_grouped_agg_kernel_mode(&invalid, &mode) != PGACCEL_OK);
  }
  OutputStorage expected(desc, true, true);
  CHECK_STATUS(execute_external(desc, &expected.out), PGACCEL_OK);
  CHECK(expected.out.emitted_group_count == 1 && expected.out.selected_count == selected);
  CHECK(expected.active[0] == 1);
  CHECK(expected.i64(expected.measures[0].sum, 0) == negative_sum);
  CHECK(expected.i64(expected.measures[0].min, 0) == negative_min);
  CHECK(expected.i64(expected.measures[0].max, 0) == negative_max);
  CHECK(expected.measures[0].count[0] == negative_count);
  CHECK(expected.measures[0].nonnull[0] == negative_count);
  CHECK(expected.measures[1].count[0] == positive_count);
  CHECK(expected.measures[2].count[0] == selected);
  auto session = desc;
  session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const auto req = workspace_req(session);
  SharedWorkspace workspace(req.bytes, req.alignment);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  for (size_t first = 0; first < rows; first += 997) {
    auto chunk = row_slice(session, first, std::min(size_t{997}, rows - first));
    if (first == 0)
      chunk.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
    CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
  }
  auto final = row_slice(session, 0, 0);
  final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(final, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  check_outputs_equal(chunked, expected, 3);
  final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  OutputStorage empty(desc, true, true);
  CHECK_STATUS(execute_in_workspace(final, req, workspace, &empty.out, &detail), PGACCEL_OK);
  CHECK(empty.out.emitted_group_count == (groups == 1 ? 1 : 0) && empty.out.selected_count == 0);
  CHECK(empty.active[0] == (groups == 1 ? 1 : 0) && empty.measures[0].nonnull[0] == 0);
  CHECK(empty.measures[0].count[0] == 0 && empty.measures[1].count[0] == 0);
  CHECK(empty.measures[2].count[0] == 0);
  if (with_mask) {
    mask[0] = 2;
    OutputStorage invalid(desc, true, true);
    CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    for (size_t row = 0; row < rows; ++row)
      mask[row] = -1;
    OutputStorage rejected(desc, true, true);
    CHECK_STATUS(execute_external(desc, &rejected.out), PGACCEL_OK);
    CHECK(rejected.out.selected_count == 0);
    CHECK(rejected.out.emitted_group_count == (groups == 1 ? 1 : 0));
    CHECK(rejected.measures[0].nonnull[0] == 0 && rejected.measures[2].count[0] == 0);
  }
}

void test_parallel_dense_integer_without_count() {
  std::printf("--- parallel dense integer SUM/non-NULL state without COUNT(*) ---\n");
  constexpr size_t rows = 131077;
  // Three groups use hierarchical reduction; seventeen use atomic one-shot
  // execution and ordinary partial reduction for chunked execution.
  for (size_t groups : {size_t{3}, size_t{17}}) {
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_values(rows);
    std::vector<uint8_t> host_nulls(rows);
    std::vector<int64_t> sums(groups, 0);
    std::vector<uint64_t> nonnull(groups, 0);
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = static_cast<int32_t>(group);
      host_values[row] = row % 2 == 0 ? INT32_MAX : INT32_MIN;
      host_nulls[row] = group == groups - 1 || row % 11 == 0 ? 1 : 0;
      if (host_nulls[row] == 0) {
        sums[group] += host_values[row];
        ++nonnull[group];
      }
    }
    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> values(host_values);
    SharedArray<uint8_t> nulls(host_nulls);
    auto desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(desc.measures[0].value, values.data(), nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    CHECK(desc.measure_count == 1);
    OutputStorage expected(desc, true, true);
    CHECK_STATUS(execute_external(desc, &expected.out), PGACCEL_OK);
    CHECK(expected.out.selected_count == rows);
    CHECK(expected.out.emitted_group_count == groups);
    for (size_t group = 0; group < groups; ++group) {
      CHECK(expected.active[group] == 1);
      CHECK(expected.i64(expected.measures[0].sum, group) == sums[group]);
      CHECK(expected.measures[0].nonnull[group] == nonnull[group]);
    }

    // A non-one-shot workspace reserves partials, not atomic scratch.
    auto session = desc;
    session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    const auto req = workspace_req(session);
    SharedWorkspace workspace(req.bytes, req.alignment);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    constexpr size_t split = 65539;
    auto first = row_slice(session, 0, split);
    first.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
    auto second = row_slice(session, split, rows - split);
    CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_OK);
    auto final = row_slice(session, 0, 0);
    final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage chunked(desc, true, true);
    CHECK_STATUS(execute_in_workspace(final, req, workspace, &chunked.out, &detail), PGACCEL_OK);
    check_outputs_equal(chunked, expected, 1);

    auto empty = row_slice(session, 0, 0);
    empty.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage cleared(desc, true, true);
    CHECK_STATUS(execute_in_workspace(empty, req, workspace, &cleared.out, &detail), PGACCEL_OK);
    CHECK(cleared.out.selected_count == 0);
    CHECK(cleared.out.emitted_group_count == 0);
    CHECK(cleared.active == std::vector<uint8_t>(groups, 0));
  }
}

void test_parallel_dense_multiple_integer_measures(bool extrema = false) {
  std::printf("--- parallel dense multiple independent integer SUM/AVG states ---\n");
  constexpr size_t rows = 65543;
  for (size_t groups : {size_t{3}, size_t{17}}) {
    std::vector<int32_t> host_keys(rows);
    std::array<std::vector<int32_t>, 4> host_values;
    std::array<std::vector<uint8_t>, 4> host_nulls;
    std::array<std::vector<int64_t>, 4> sums;
    std::array<std::vector<int64_t>, 4> mins, maxs;
    std::array<std::vector<uint64_t>, 4> nonnull;
    for (size_t slot = 0; slot < 4; ++slot) {
      host_values[slot].resize(rows);
      host_nulls[slot].resize(rows);
      sums[slot].resize(groups);
      mins[slot].resize(groups);
      maxs[slot].resize(groups);
      nonnull[slot].resize(groups);
      for (size_t row = 0; row < rows; ++row) {
        const size_t group = row % groups;
        host_keys[row] = static_cast<int32_t>(group);
        host_values[slot][row] = (row + slot) % 2 == 0 ? INT32_MAX : INT32_MIN;
        // Lane zero has an all-NULL group; later lanes still contribute there.
        // Lane two is statically non-NULL, while lane three is entirely NULL.
        host_nulls[slot][row] =
            slot == 3 || (slot != 2 && (group == slot || row % (11 + slot) == 0)) ? 1 : 0;
        if (host_nulls[slot][row] == 0) {
          if (nonnull[slot][group] == 0 || host_values[slot][row] < mins[slot][group])
            mins[slot][group] = host_values[slot][row];
          if (nonnull[slot][group] == 0 || host_values[slot][row] > maxs[slot][group])
            maxs[slot][group] = host_values[slot][row];
          sums[slot][group] += host_values[slot][row];
          ++nonnull[slot][group];
        }
      }
    }
    SharedArray<int32_t> keys(host_keys);
    std::array<SharedArray<int32_t>, 4> values = {
        SharedArray<int32_t>(host_values[0]), SharedArray<int32_t>(host_values[1]),
        SharedArray<int32_t>(host_values[2]), SharedArray<int32_t>(host_values[3])};
    std::array<SharedArray<uint8_t>, 4> nulls = {
        SharedArray<uint8_t>(host_nulls[0]), SharedArray<uint8_t>(host_nulls[1]),
        SharedArray<uint8_t>(host_nulls[2]), SharedArray<uint8_t>(host_nulls[3])};
    for (size_t lanes : {size_t{2}, size_t{3}, size_t{4}}) {
      for (bool with_count : {false, true}) {
        if (lanes == 4 && with_count)
          continue;
        auto desc = base_desc(rows);
        set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
        for (size_t slot = 0; slot < lanes; ++slot) {
          set_i32_view(desc.measures[slot].value, values[slot].data(),
                       slot == 2 ? nullptr : nulls[slot].data());
          finish_i64_measure(
              desc, slot, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
              extrema ? (slot == 0   ? PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN
                         : slot == 1 ? PGACCEL_GROUPED_AGG_LANE_MAX
                                     : PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX)
                      : PGACCEL_GROUPED_AGG_LANE_SUM);
        }
        if (with_count)
          finish_i64_measure(desc, lanes, PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
                             PGACCEL_GROUPED_AGG_LANE_COUNT);
        int32_t mode = 0;
        CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
        CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
        OutputStorage expected(desc, true, true);
        CHECK_STATUS(execute_external(desc, &expected.out), PGACCEL_OK);
        CHECK(expected.out.selected_count == rows);
        CHECK(expected.out.emitted_group_count == groups);
        for (size_t group = 0; group < groups; ++group) {
          CHECK(expected.active[group] == 1);
          for (size_t slot = 0; slot < lanes; ++slot) {
            if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_SUM)
              CHECK(expected.i64(expected.measures[slot].sum, group) == sums[slot][group]);
            if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MIN)
              CHECK(expected.i64(expected.measures[slot].min, group) == mins[slot][group]);
            if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MAX)
              CHECK(expected.i64(expected.measures[slot].max, group) == maxs[slot][group]);
            CHECK(expected.measures[slot].nonnull[group] == nonnull[slot][group]);
          }
          if (with_count)
            CHECK(expected.measures[lanes].count[group] == rows / groups + (group < rows % groups));
        }

        auto session = desc;
        session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&session, &mode), PGACCEL_OK);
        CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
        const auto req = workspace_req(session);
        SharedWorkspace workspace(req.bytes, req.alignment);
        int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
        constexpr size_t split = 32771;
        auto first = row_slice(session, 0, split);
        first.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
        CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
        auto second = row_slice(session, split, rows - split);
        CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_OK);
        auto final = row_slice(session, 0, 0);
        final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
        OutputStorage chunked(desc, true, true);
        CHECK_STATUS(execute_in_workspace(final, req, workspace, &chunked.out, &detail),
                     PGACCEL_OK);
        check_outputs_equal(chunked, expected, desc.measure_count);

        // A bad NULL byte in a later lane poisons the whole operation; no
        // partially successful first-lane results may become visible.
        nulls[1][42] = 7;
        OutputStorage invalid(desc, true, true);
        CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
        for (size_t slot = 0; slot < lanes; ++slot) {
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_SUM)
            CHECK(invalid.measures[slot].sum ==
                  std::vector<uint64_t>(groups, OutputStorage::kSentinel));
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MIN)
            CHECK(invalid.measures[slot].min ==
                  std::vector<uint64_t>(groups, OutputStorage::kSentinel));
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MAX)
            CHECK(invalid.measures[slot].max ==
                  std::vector<uint64_t>(groups, OutputStorage::kSentinel));
        }
        first = row_slice(session, 0, split);
        first.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
        CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_ERROR);
        nulls[1][42] = host_nulls[1][42];
        CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_ERROR);
        CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);

        auto empty = row_slice(session, 0, 0);
        empty.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
        OutputStorage cleared(desc, true, true);
        CHECK_STATUS(execute_in_workspace(empty, req, workspace, &cleared.out, &detail),
                     PGACCEL_OK);
        CHECK(cleared.out.selected_count == 0);
        CHECK(cleared.out.emitted_group_count == 0);
        for (size_t slot = 0; slot < lanes; ++slot) {
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_SUM)
            CHECK(cleared.measures[slot].sum == std::vector<uint64_t>(groups, 0));
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MIN)
            CHECK(cleared.measures[slot].min == std::vector<uint64_t>(groups, 0));
          if (desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_MAX)
            CHECK(cleared.measures[slot].max == std::vector<uint64_t>(groups, 0));
          CHECK(cleared.measures[slot].nonnull == std::vector<uint64_t>(groups, 0));
        }
      }
    }
  }
}

void test_parallel_dense_independent_measure_filters() {
  std::printf("--- parallel independent aggregate ranges and predicate NULLs ---\n");
  constexpr size_t rows = 65543;
  for (size_t groups : {size_t{3}, size_t{17}}) {
    std::vector<int32_t> host_keys(rows), host_a(rows), host_b(rows);
    std::vector<uint8_t> host_an(rows), host_bn(rows);
    std::array<std::vector<int64_t>, 2> sums = {std::vector<int64_t>(groups),
                                                std::vector<int64_t>(groups)};
    std::array<std::vector<uint64_t>, 2> nonnull = {std::vector<uint64_t>(groups),
                                                    std::vector<uint64_t>(groups)};
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = static_cast<int32_t>(group);
      host_a[row] = row % 2 == 0 ? INT32_MAX : INT32_MIN;
      host_b[row] = static_cast<int32_t>(row % 203) - 101;
      host_an[row] = row % 11 == 0 ? 1 : 0;
      host_bn[row] = group == groups - 1 || row % 13 == 0 ? 1 : 0;
      // Two disjoint intervals on b filter a; a has its own independent NULLs.
      if (!host_an[row] && !host_bn[row] && (host_b[row] == -100 || host_b[row] >= 100)) {
        sums[0][group] += host_a[row];
        ++nonnull[0][group];
      }
      if (!host_bn[row] && host_b[row] >= 0) {
        sums[1][group] += host_b[row];
        ++nonnull[1][group];
      }
    }
    SharedArray<int32_t> keys(host_keys), a(host_a), b(host_b);
    SharedArray<uint8_t> an(host_an), bn(host_bn);
    for (uint32_t first_mask :
         {uint32_t{PGACCEL_GROUPED_AGG_LANE_SUM}, uint32_t{PGACCEL_GROUPED_AGG_LANE_COUNT},
          uint32_t{PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT}}) {
      for (uint32_t second_mask :
           {uint32_t{PGACCEL_GROUPED_AGG_LANE_SUM}, uint32_t{PGACCEL_GROUPED_AGG_LANE_COUNT},
            uint32_t{PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT}}) {
        auto desc = base_desc(rows);
        set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
        set_i32_view(desc.measures[0].value, a.data(), an.data());
        set_i32_view(desc.measures[1].value, b.data(), bn.data());
        finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, first_mask);
        finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, second_mask);
        finish_i64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
                           PGACCEL_GROUPED_AGG_LANE_COUNT);
        set_i32_measure_value_range(desc, 0, 1, -100, -100);
        desc.measure_filters[0].predicate_range_count = 2;
        desc.measure_filters[0].predicate_lo[1] = val_i32(100);
        desc.measure_filters[0].predicate_hi[1] = val_i32(INT32_MAX);
        set_i32_measure_value_range(desc, 1, 1, 0, INT32_MAX);
        int32_t mode = 0;
        CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
        CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
        OutputStorage expected(desc, true, true);
        CHECK_STATUS(execute_external(desc, &expected.out), PGACCEL_OK);
        CHECK(expected.out.selected_count == rows);
        CHECK(expected.out.emitted_group_count == groups);
        for (size_t group = 0; group < groups; ++group) {
          CHECK(expected.active[group] == 1);
          CHECK(expected.measures[2].count[group] == rows / groups + (group < rows % groups));
          for (size_t slot = 0; slot < 2; ++slot) {
            if ((desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_SUM) != 0)
              CHECK(expected.i64(expected.measures[slot].sum, group) == sums[slot][group]);
            if ((desc.measures[slot].agg_mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0)
              CHECK(expected.measures[slot].count[group] == nonnull[slot][group]);
            CHECK(expected.measures[slot].nonnull[group] == nonnull[slot][group]);
          }
        }
        auto session = desc;
        session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        const auto req = workspace_req(session);
        SharedWorkspace workspace(req.bytes, req.alignment);
        int32_t detail = 0;
        auto first = row_slice(session, 0, 32771);
        first.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
        CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
        auto second = row_slice(session, 32771, rows - 32771);
        CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_OK);
        auto final = row_slice(session, 0, 0);
        final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
        OutputStorage chunked(desc, true, true);
        CHECK_STATUS(execute_in_workspace(final, req, workspace, &chunked.out, &detail),
                     PGACCEL_OK);
        check_outputs_equal(chunked, expected, desc.measure_count);
        // Invalid predicate NULL metadata fails even where the value itself is NULL.
        an[44] = 1;
        bn[44] = 9;
        OutputStorage invalid(desc);
        CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
        if ((first_mask & PGACCEL_GROUPED_AGG_LANE_SUM) != 0)
          CHECK(invalid.measures[0].sum == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
        if ((first_mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0)
          CHECK(invalid.measures[0].count ==
                std::vector<uint64_t>(groups, OutputStorage::kSentinel));
        an[44] = host_an[44];
        bn[44] = host_bn[44];
      }
    }
  }
}

void test_parallel_dense_count_only_states() {
  std::printf("--- parallel independent count-only states without SUM allocation ---\n");
  constexpr size_t rows = 65543;
  for (size_t groups : {size_t{3}, size_t{17}, size_t{1024}}) {
    std::vector<int32_t> host_keys(rows), host_values(rows, INT32_MAX);
    for (size_t row = 0; row < rows; ++row)
      host_keys[row] = static_cast<int32_t>(row % groups);
    SharedArray<int32_t> keys(host_keys), values(host_values);
    for (size_t lanes : {size_t{2}, size_t{4}}) {
      auto desc = base_desc(rows);
      set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
      for (size_t slot = 0; slot < lanes; ++slot) {
        set_i32_view(desc.measures[slot].value, values.data(), nullptr);
        finish_i64_measure(desc, slot, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                           PGACCEL_GROUPED_AGG_LANE_COUNT);
      }
      const bool count_star = lanes == 2 && groups != 3;
      if (count_star)
        set_count_star(desc, lanes);
      int32_t mode = 0;
      CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
      CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
      OutputStorage expected(desc);
      CHECK_STATUS(execute_external(desc, &expected.out), PGACCEL_OK);
      CHECK(expected.out.selected_count == rows);
      CHECK(expected.out.emitted_group_count == groups);
      if (count_star) {
        for (size_t group = 0; group < groups; ++group)
          CHECK(expected.measures[lanes].count[group] == rows / groups + (group < rows % groups));
      }
      for (size_t slot = 0; slot < lanes; ++slot) {
        CHECK(expected.measures[slot].sum.empty());
        for (size_t group = 0; group < groups; ++group) {
          CHECK(expected.measures[slot].count[group] == rows / groups + (group < rows % groups));
          CHECK(expected.measures[slot].nonnull[group] == expected.measures[slot].count[group]);
        }
      }
      auto session = desc;
      session.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      const auto req = workspace_req(session);
      SharedWorkspace workspace(req.bytes, req.alignment);
      int32_t detail = 0;
      auto first = row_slice(session, 0, 32771);
      first.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
      CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
      auto second = row_slice(session, 32771, rows - 32771);
      CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_OK);
      auto final = row_slice(session, 0, 0);
      final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
      OutputStorage chunked(desc);
      CHECK_STATUS(execute_in_workspace(final, req, workspace, &chunked.out, &detail), PGACCEL_OK);
      check_outputs_equal(chunked, expected, desc.measure_count);
      auto empty = row_slice(session, 0, 0);
      empty.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
      OutputStorage reset(desc);
      CHECK_STATUS(execute_in_workspace(empty, req, workspace, &reset.out, &detail), PGACCEL_OK);
      CHECK(reset.out.selected_count == 0);
      CHECK(reset.out.emitted_group_count == 0);
      for (size_t slot = 0; slot < lanes; ++slot)
        CHECK(reset.measures[slot].count == std::vector<uint64_t>(groups, 0));
    }
  }
}

void test_parallel_dense_integer_product_sum_shape() {
  std::printf("--- parallel dense int4 product SUM+COUNT(*) ---\n");
  constexpr size_t rows = 263157;
  constexpr size_t groups = 128;
  std::vector<int32_t> host_keys(rows);
  std::vector<int32_t> host_lhs(rows);
  std::vector<int32_t> host_rhs(rows);
  std::vector<uint8_t> host_lhs_nulls(rows);
  std::vector<uint8_t> host_rhs_nulls(rows);
  std::array<int64_t, groups> expected_sum{};
  std::array<uint64_t, groups> expected_nonnull{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const size_t group = row % groups;
    host_keys[row] = static_cast<int32_t>(group);
    host_lhs[row] = 1 + static_cast<int32_t>(row % 997);
    host_rhs[row] = 1 + static_cast<int32_t>(row % 49);
    // Keep one group entirely NULL while also covering lhs-only, rhs-only,
    // and jointly NULL rows in the remaining groups.
    host_lhs_nulls[row] = group + 1 == groups || row % 127 == 0 ? 1 : 0;
    host_rhs_nulls[row] = row % 131 == 0 ? 1 : 0;
    ++expected_count[group];
    if (host_lhs_nulls[row] != 0 || host_rhs_nulls[row] != 0)
      continue;
    expected_sum[group] += static_cast<int64_t>(host_lhs[row]) * host_rhs[row];
    ++expected_nonnull[group];
  }

  SharedArray<int32_t> keys(host_keys);
  SharedArray<int32_t> lhs(host_lhs);
  SharedArray<int32_t> rhs(host_rhs);
  SharedArray<uint8_t> lhs_nulls(host_lhs_nulls);
  SharedArray<uint8_t> rhs_nulls(host_rhs_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
  set_i32_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
  set_count_star(desc, 1);

  OutputStorage one_shot(desc, true, true);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  CHECK(one_shot.out.selected_count == rows);
  CHECK(one_shot.out.uncertain_count == 0);
  CHECK(one_shot.out.emitted_group_count == groups);
  CHECK(one_shot.active == std::vector<uint8_t>(groups, 1));
  for (size_t group = 0; group < groups; ++group) {
    CHECK(one_shot.i64(one_shot.measures[0].sum, group) == expected_sum[group]);
    CHECK(one_shot.measures[0].nonnull[group] == expected_nonnull[group]);
    CHECK(one_shot.measures[1].count[group] == expected_count[group]);
  }
  CHECK(expected_count[groups - 1] > 0);
  CHECK(one_shot.measures[0].nonnull[groups - 1] == 0);

  OutputStorage timed(desc, true, true);
  const auto timed_start = std::chrono::steady_clock::now();
  CHECK_STATUS(execute_external(desc, &timed.out), PGACCEL_OK);
  const auto timed_end = std::chrono::steady_clock::now();
  const double timed_ms =
      std::chrono::duration<double, std::milli>(timed_end - timed_start).count();
  std::printf("parallel dense int4 MUL/SUM+COUNT(*) %zu rows/%zu groups: %.3f ms\n", rows, groups,
              timed_ms);
  check_outputs_equal(timed, one_shot, desc.measure_count);

  lhs[31] = 46341;
  rhs[31] = 46341;
  lhs_nulls[31] = 0;
  rhs_nulls[31] = 0;
  OutputStorage atomic_overflow(desc, true, true);
  int32_t atomic_detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_external_ex(desc, &atomic_overflow.out, &atomic_detail), PGACCEL_ERROR);
  CHECK(atomic_detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  lhs[31] = host_lhs[31];
  rhs[31] = host_rhs[31];
  lhs_nulls[31] = host_lhs_nulls[31];
  rhs_nulls[31] = host_rhs_nulls[31];

  const pgaccel_grouped_agg_workspace_req one_shot_req = workspace_req(desc);
  pgaccel_grouped_agg_desc session_shape = desc;
  session_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req req = workspace_req(session_shape);
  CHECK(one_shot_req.bytes < req.bytes);
  SharedWorkspace workspace(req.bytes, req.alignment);
  constexpr std::array<size_t, 3> chunk_rows = {65537, 100003, rows - 165540};
  size_t first_row = 0;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  for (size_t index = 0; index < chunk_rows.size(); ++index) {
    pgaccel_grouped_agg_desc chunk = row_slice(desc, first_row, chunk_rows[index]);
    chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    if (index == 0)
      chunk.execution_flags |= PGACCEL_GROUPED_AGG_EXEC_RESET;
    CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
    first_row += chunk_rows[index];
  }
  pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  check_outputs_equal(chunked, one_shot, desc.measure_count);

  lhs[31] = 46340;
  rhs[31] = 46340;
  lhs_nulls[31] = 0;
  rhs_nulls[31] = 0;
  pgaccel_grouped_agg_desc exact_product = row_slice(desc, 31, 1);
  exact_product.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(exact_product, req, workspace, nullptr, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);

  lhs[31] = 46341;
  rhs[31] = 46341;
  pgaccel_grouped_agg_desc overflow = row_slice(desc, 31, 1);
  overflow.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(execute_in_workspace(overflow, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  pgaccel_grouped_agg_desc poisoned = row_slice(desc, 32, 1);
  poisoned.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(poisoned, req, workspace, nullptr, &detail), PGACCEL_ERROR);

  lhs[31] = host_lhs[31];
  rhs[31] = host_rhs[31];
  lhs_nulls[31] = host_lhs_nulls[31];
  rhs_nulls[31] = host_rhs_nulls[31];
  OutputStorage recovered(desc, true, true);
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &recovered.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  check_outputs_equal(recovered, one_shot, desc.measure_count);

  // The session's logical plan may exceed the partial budget even though its
  // bounded executor slices qualify. A workspace sized from the large shape
  // must still admit the 256K lifecycle call.
  pgaccel_grouped_agg_desc large_plan = desc;
  large_plan.row_count = 1'300'000;
  pgaccel_grouped_agg_desc large_workspace_shape = large_plan;
  large_workspace_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req large_req = workspace_req(large_workspace_shape);
  pgaccel_grouped_agg_desc bounded = row_slice(desc, 0, 256'000);
  bounded.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  SharedWorkspace large_workspace(large_req.bytes, large_req.alignment);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(bounded, large_req, large_workspace, nullptr, &detail),
               PGACCEL_OK);
  pgaccel_grouped_agg_desc bounded_finalize = row_slice(large_plan, 0, 0);
  bounded_finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage bounded_output(desc, true, true);
  CHECK_STATUS(execute_in_workspace(bounded_finalize, large_req, large_workspace,
                                    &bounded_output.out, &detail),
               PGACCEL_OK);
  OutputStorage bounded_expected(desc, true, true);
  pgaccel_grouped_agg_desc bounded_one_shot = row_slice(desc, 0, 256'000);
  CHECK_STATUS(execute_external(bounded_one_shot, &bounded_expected.out), PGACCEL_OK);
  check_outputs_equal(bounded_output, bounded_expected, desc.measure_count);

  // Eligibility is non-monotone at the partial budget: 1,000 groups makes a
  // 256K allocation shape serial, while a 100K tail is parallel. Reservation
  // must cover the shorter tail rather than the selected mode of the maximum.
  pgaccel_grouped_agg_desc wide_allocation = bounded_one_shot;
  wide_allocation.group_capacity = 1'000;
  wide_allocation.keys[0].cardinality = 1'000;
  pgaccel_grouped_agg_desc wide_workspace_shape = wide_allocation;
  wide_workspace_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req wide_req = workspace_req(wide_workspace_shape);
  SharedWorkspace wide_workspace(wide_req.bytes, wide_req.alignment);
  pgaccel_grouped_agg_desc wide_tail = row_slice(wide_allocation, 0, 100'000);
  wide_tail.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(wide_tail, wide_req, wide_workspace, nullptr, &detail),
               PGACCEL_OK);
  pgaccel_grouped_agg_desc wide_finalize = row_slice(wide_allocation, 0, 0);
  wide_finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage wide_output(wide_allocation, true, true);
  CHECK_STATUS(
      execute_in_workspace(wide_finalize, wide_req, wide_workspace, &wide_output.out, &detail),
      PGACCEL_OK);
  OutputStorage wide_expected(wide_allocation, true, true);
  pgaccel_grouped_agg_desc wide_one_shot = row_slice(wide_allocation, 0, 100'000);
  CHECK_STATUS(execute_external(wide_one_shot, &wide_expected.out), PGACCEL_OK);
  check_outputs_equal(wide_output, wide_expected, wide_allocation.measure_count);
}

void test_parallel_dense_integer_measure_range_filter() {
  std::printf("--- parallel dense aggregate-local INT4 range FILTER ---\n");
  constexpr size_t rows = 4097;
  for (const size_t groups : {size_t{4}, size_t{256}}) {
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_values(rows);
    std::vector<uint8_t> host_nulls(rows);
    std::vector<int64_t> expected_sum(groups, 0);
    std::vector<uint64_t> expected_nonnull(groups, 0);
    std::vector<uint64_t> expected_count(groups, 0);
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = static_cast<int32_t>(group);
      host_values[row] = group + 1 == groups ? 900 : static_cast<int32_t>(row % 1001);
      host_nulls[row] = row % 29 == 0 ? 1 : 0;
      ++expected_count[group];
      if (host_nulls[row] == 0 && host_values[row] >= 200 && host_values[row] <= 800) {
        expected_sum[group] += host_values[row];
        ++expected_nonnull[group];
      }
    }

    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> values(host_values);
    SharedArray<uint8_t> nulls(host_nulls);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(desc.measures[0].value, values.data(), nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_measure_value_range(desc, 0, 0, 200, 800);

    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    OutputStorage output(desc, true, true);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == rows);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.out.emitted_group_count == groups);
    for (size_t group = 0; group < groups; ++group) {
      CHECK(output.active[group] == 1);
      CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
      CHECK(output.measures[0].nonnull[group] == expected_nonnull[group]);
      CHECK(output.measures[1].count[group] == expected_count[group]);
    }
    CHECK(expected_count[groups - 1] != 0);
    CHECK(output.measures[0].nonnull[groups - 1] == 0);

    if (groups == 256) {
      // A lifecycle call cannot use the one-shot atomic path. Exercise the
      // chunked specialization as well, including aggregate-filter rejection,
      // invalid-null fail-closed behavior, and reset recovery.
      pgaccel_grouped_agg_desc session = desc;
      session.execution_flags =
          PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      const pgaccel_grouped_agg_workspace_req req = workspace_req(session);
      SharedWorkspace workspace(req.bytes, req.alignment);
      int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
      CHECK_STATUS(execute_in_workspace(session, req, workspace, nullptr, &detail), PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);

      pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
      finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
      OutputStorage chunked(desc, true, true);
      CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail),
                   PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      check_outputs_equal(chunked, output, desc.measure_count);

      nulls[31] = 2;
      detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
      CHECK_STATUS(execute_in_workspace(session, req, workspace, nullptr, &detail), PGACCEL_ERROR);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
      nulls[31] = host_nulls[31];

      CHECK_STATUS(execute_in_workspace(session, req, workspace, nullptr, &detail), PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      OutputStorage recovered(desc, true, true);
      CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &recovered.out, &detail),
                   PGACCEL_OK);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
      check_outputs_equal(recovered, output, desc.measure_count);

      // The one-shot atomic specialization must also fail closed when a fact
      // key maps outside the declared dense radix.
      const int32_t saved_key = keys[31];
      keys[31] = static_cast<int32_t>(groups);
      OutputStorage invalid_key(desc);
      detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
      CHECK_STATUS(execute_external_ex(desc, &invalid_key.out, &detail), PGACCEL_ERROR);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
      keys[31] = saved_key;
    }

    nulls[31] = 2;
    OutputStorage invalid(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    nulls[31] = host_nulls[31];
  }
}

void test_parallel_dense_sum_mask_unique_dimension_shape() {
  std::printf("--- parallel dense SUM+COUNT(*) mask/two unique dimensions ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t dim0_rows = 1000;
  constexpr size_t dim1_rows = 257;
  constexpr size_t key0_groups = 5;
  constexpr size_t key1_groups = 7;
  constexpr size_t groups = key0_groups * key1_groups;
  std::vector<int32_t> host_fact_key0(rows);
  std::vector<int32_t> host_fact_key1(rows);
  std::vector<int32_t> host_values(rows);
  std::vector<int8_t> host_mask(rows);
  std::vector<int32_t> host_group_lookup0(dim0_rows);
  std::vector<int32_t> host_group_lookup1(dim1_rows);
  std::vector<uint8_t> host_matches0(dim0_rows, 1);
  std::vector<uint8_t> host_matches1(dim1_rows, 1);
  std::vector<uint64_t> host_multiplicity0(dim0_rows, 1);
  std::array<int64_t, groups> expected_sum{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t dim = 0; dim < dim0_rows; ++dim) {
    host_group_lookup0[dim] = static_cast<int32_t>((dim * 3 + 1) % key0_groups);
    if (dim % 97 == 0)
      host_matches0[dim] = 0;
  }
  for (size_t dim = 0; dim < dim1_rows; ++dim) {
    host_group_lookup1[dim] = static_cast<int32_t>((dim * 5 + 2) % key1_groups);
    if (dim % 43 == 0)
      host_matches1[dim] = 0;
  }
  size_t expected_selected = 0;
  size_t rejected_by_dim0_only = 0;
  size_t rejected_by_dim1_only = 0;
  for (size_t row = 0; row < rows; ++row) {
    const size_t dim0 = row % dim0_rows;
    const size_t dim1 = (row * 37 + 11) % dim1_rows;
    host_fact_key0[row] = static_cast<int32_t>(dim0);
    host_fact_key1[row] = static_cast<int32_t>(dim1);
    host_values[row] = 1 + static_cast<int32_t>(row % 1000);
    host_mask[row] = row % 10 == 0 ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
    if (host_mask[row] != PGACCEL_EXPR_TRUE)
      continue;
    const bool dim0_matches = host_matches0[dim0] != 0;
    const bool dim1_matches = host_matches1[dim1] != 0;
    if (!dim0_matches || !dim1_matches) {
      rejected_by_dim0_only += !dim0_matches && dim1_matches ? 1 : 0;
      rejected_by_dim1_only += dim0_matches && !dim1_matches ? 1 : 0;
      continue;
    }
    const size_t group = static_cast<size_t>(host_group_lookup0[dim0]) * key1_groups +
                         static_cast<size_t>(host_group_lookup1[dim1]);
    expected_sum[group] += host_values[row];
    ++expected_count[group];
    ++expected_selected;
  }
  CHECK(rejected_by_dim0_only != 0);
  CHECK(rejected_by_dim1_only != 0);

  SharedArray<int32_t> fact_key0(host_fact_key0);
  SharedArray<int32_t> fact_key1(host_fact_key1);
  SharedArray<int32_t> values(host_values);
  SharedArray<int8_t> mask(host_mask);
  SharedArray<int32_t> group_lookup0(host_group_lookup0);
  SharedArray<int32_t> group_lookup1(host_group_lookup1);
  SharedArray<uint8_t> matches0(host_matches0);
  SharedArray<uint8_t> matches1(host_matches1);
  SharedArray<uint64_t> multiplicity0(host_multiplicity0);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, fact_key0.data(), nullptr, 0, dim0_rows, matches0.data());
  set_dim(desc, 1, fact_key1.data(), nullptr, 0, dim1_rows, matches1.data());
  set_dim_key(desc, 0, 0, group_lookup0.data(), 0, key0_groups);
  set_dim_key(desc, 1, 1, group_lookup1.data(), 0, key1_groups);
  set_i32_view(desc.measures[0].value, values.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
  set_count_star(desc, 1);
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = mask.data();

  const pgaccel_grouped_agg_workspace_req one_shot_req = workspace_req(desc);
  pgaccel_grouped_agg_desc serial_one_shot_shape = desc;
  serial_one_shot_shape.dims[0].multiplicity_by_key = multiplicity0.data();
  const pgaccel_grouped_agg_workspace_req serial_one_shot_req =
      workspace_req(serial_one_shot_shape);
  CHECK(one_shot_req.alignment >= alignof(uint64_t));
  CHECK(one_shot_req.alignment % alignof(uint64_t) == 0);
  const auto add_atomic_words = [&](size_t bytes) {
    const size_t alignment_mask = one_shot_req.alignment - 1;
    return ((bytes + alignment_mask) & ~alignment_mask) + groups * sizeof(uint64_t);
  };
  CHECK(one_shot_req.bytes == add_atomic_words(add_atomic_words(serial_one_shot_req.bytes)));
  pgaccel_grouped_agg_desc session_shape = desc;
  session_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req parallel_req = workspace_req(session_shape);
  pgaccel_grouped_agg_desc serial_session_shape = session_shape;
  serial_session_shape.dims[0].multiplicity_by_key = multiplicity0.data();
  const pgaccel_grouped_agg_workspace_req serial_req = workspace_req(serial_session_shape);
  CHECK(one_shot_req.bytes < parallel_req.bytes);
  CHECK(parallel_req.bytes > serial_req.bytes);

  // The release SSBM date x part sentinel resolves to at most 350 groups.
  // Its one-million-row shape must remain eligible for both compact atomic
  // one-shot execution and the ordered partial lifecycle path.
  pgaccel_grouped_agg_desc release_shape = desc;
  release_shape.row_count = 1'000'000;
  release_shape.group_capacity = 350;
  release_shape.keys[0].cardinality = 50;
  const pgaccel_grouped_agg_workspace_req release_one_shot_req = workspace_req(release_shape);
  pgaccel_grouped_agg_desc release_session_shape = release_shape;
  release_session_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req release_parallel_req =
      workspace_req(release_session_shape);
  pgaccel_grouped_agg_desc release_serial_session_shape = release_session_shape;
  release_serial_session_shape.dims[0].multiplicity_by_key = multiplicity0.data();
  const pgaccel_grouped_agg_workspace_req release_serial_req =
      workspace_req(release_serial_session_shape);
  constexpr size_t release_chunks = (1'000'000 + 1023) / 1024;
  constexpr size_t expected_partial_bytes = 350 * release_chunks * 56;
  CHECK(release_one_shot_req.bytes < release_parallel_req.bytes);
  CHECK(release_parallel_req.bytes >= release_serial_req.bytes + expected_partial_bytes);

  // Atomic one-shot execution owns no partial array, so it remains eligible
  // above the ordered lifecycle budget. The same shape queried for a session
  // still reserves only the bounded partial layout.
  pgaccel_grouped_agg_desc high_capacity_shape = release_shape;
  high_capacity_shape.group_capacity = 700;
  high_capacity_shape.keys[0].cardinality = 100;
  constexpr size_t high_capacity_partial_bytes = 700 * release_chunks * 56;
  CHECK(high_capacity_partial_bytes > 32 * 1024 * 1024);
  pgaccel_status high_capacity_status = PGACCEL_ERROR;
  const pgaccel_grouped_agg_workspace_req high_capacity_one_shot_req =
      workspace_req(high_capacity_shape, &high_capacity_status);
  CHECK(high_capacity_status == PGACCEL_OK);
  pgaccel_grouped_agg_desc high_capacity_session_shape = high_capacity_shape;
  high_capacity_session_shape.execution_flags =
      PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const pgaccel_grouped_agg_workspace_req high_capacity_session_req =
      workspace_req(high_capacity_session_shape, &high_capacity_status);
  CHECK(high_capacity_status == PGACCEL_OK);
  CHECK(high_capacity_one_shot_req.bytes < high_capacity_session_req.bytes);

  OutputStorage one_shot(desc, true, true);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  CHECK(one_shot.out.selected_count == expected_selected);
  CHECK(one_shot.out.uncertain_count == 0);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(one_shot.group_codes[group] == group);
    CHECK(one_shot.key_values[0][group] == static_cast<int32_t>(group / key1_groups));
    CHECK(one_shot.key_values[1][group] == static_cast<int32_t>(group % key1_groups));
    CHECK(one_shot.i64(one_shot.measures[0].sum, group) == expected_sum[group]);
    CHECK(one_shot.measures[0].nonnull[group] == expected_count[group]);
    CHECK(one_shot.measures[1].count[group] == expected_count[group]);
    CHECK(one_shot.active[group] == (expected_count[group] != 0 ? 1 : 0));
  }

  OutputStorage timed(desc, true, true);
  const auto timed_start = std::chrono::steady_clock::now();
  CHECK_STATUS(execute_external(desc, &timed.out), PGACCEL_OK);
  const double timed_ms =
      std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - timed_start)
          .count();
  std::printf("row-parallel dense SUM+COUNT(*) %zu rows/%zu groups: %.3f ms\n", rows, groups,
              timed_ms);
  check_outputs_equal(timed, one_shot, desc.measure_count);

  SharedWorkspace workspace(parallel_req.bytes, parallel_req.alignment);
  constexpr size_t first_rows = 100003;
  pgaccel_grouped_agg_desc first = row_slice(desc, 0, first_rows);
  first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(first, parallel_req, workspace, nullptr, &detail), PGACCEL_OK);
  pgaccel_grouped_agg_desc second = row_slice(desc, first_rows, rows - first_rows);
  second.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(second, parallel_req, workspace, nullptr, &detail), PGACCEL_OK);
  pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(finalize, parallel_req, workspace, &chunked.out, &detail),
               PGACCEL_OK);
  check_outputs_equal(chunked, one_shot, desc.measure_count);
}

void test_parallel_dense_count_unique_dimension_shape() {
  std::printf("--- parallel dense COUNT(*) unique dimension ---\n");
  constexpr size_t rows = 262144;
  constexpr size_t dim_rows = 10000;
  std::vector<int32_t> host_fact_keys(rows);
  std::vector<uint8_t> host_matches(dim_rows, 1);
  std::vector<uint64_t> host_multiplicity(dim_rows, 1);
  for (size_t dim = 0; dim < dim_rows; ++dim) {
    if (dim % 101 == 0)
      host_matches[dim] = 0;
  }
  size_t expected = 0;
  for (size_t row = 0; row < rows; ++row) {
    const size_t dim = row % dim_rows;
    host_fact_keys[row] = static_cast<int32_t>(dim);
    expected += host_matches[dim] != 0 ? 1 : 0;
  }

  SharedArray<int32_t> fact_keys(host_fact_keys);
  SharedArray<uint8_t> matches(host_matches);
  SharedArray<uint64_t> multiplicity(host_multiplicity);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, fact_keys.data(), nullptr, 0, dim_rows, matches.data());
  set_count_star(desc, 0);

  const pgaccel_grouped_agg_workspace_req parallel_req = workspace_req(desc);
  pgaccel_grouped_agg_desc weighted_desc = desc;
  weighted_desc.dims[0].multiplicity_by_key = multiplicity.data();
  const pgaccel_grouped_agg_workspace_req weighted_req = workspace_req(weighted_desc);
  CHECK(weighted_req.bytes > parallel_req.bytes);

  int32_t weighted_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&weighted_desc, &weighted_mode), PGACCEL_OK);
  CHECK(weighted_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);

  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == expected);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.measures[0].count[0] == expected);
  CHECK(output.active[0] == 1);

  OutputStorage weighted(weighted_desc, true, true);
  CHECK_STATUS(execute_external(weighted_desc, &weighted.out), PGACCEL_OK);
  check_outputs_equal(weighted, output, desc.measure_count);
}

void test_parallel_dense_weighted_global_count_shape() {
  std::printf("--- parallel dense weighted global COUNT(*) ---\n");
  constexpr size_t rows = 4097;
  std::vector<int32_t> host_dim0_keys(rows);
  std::vector<uint8_t> host_dim0_nulls(rows, 0);
  std::vector<int32_t> host_dim1_keys(rows);
  std::vector<int32_t> host_group_keys(rows);
  std::vector<int32_t> host_values(rows, 1);
  const std::vector<uint8_t> host_dim0_matches = {1, 1, 0, 1};
  const std::vector<uint64_t> host_dim0_multiplicity = {2, 3, 5, 1};
  const std::vector<uint64_t> host_dim1_multiplicity = {4, 0, 2};
  uint64_t expected = 0;
  for (size_t row = 0; row < rows; ++row) {
    host_dim0_keys[row] = static_cast<int32_t>(row % 5);
    host_dim0_nulls[row] = row % 97 == 0 ? 1 : 0;
    host_dim1_keys[row] = 10 + static_cast<int32_t>(row % 3);
    host_group_keys[row] = static_cast<int32_t>(row % 2);
    if (host_dim0_nulls[row] != 0 || host_dim0_keys[row] >= 4 ||
        host_dim0_matches[host_dim0_keys[row]] == 0)
      continue;
    const uint64_t weight = host_dim0_multiplicity[host_dim0_keys[row]] *
                            host_dim1_multiplicity[host_dim1_keys[row] - 10];
    expected += weight;
  }

  SharedArray<int32_t> dim0_keys(host_dim0_keys);
  SharedArray<uint8_t> dim0_nulls(host_dim0_nulls);
  SharedArray<int32_t> dim1_keys(host_dim1_keys);
  SharedArray<int32_t> group_keys(host_group_keys);
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> dim0_matches(host_dim0_matches);
  SharedArray<uint64_t> dim0_multiplicity(host_dim0_multiplicity);
  SharedArray<uint64_t> dim1_multiplicity(host_dim1_multiplicity);

  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_dim(desc, 0, dim0_keys.data(), dim0_nulls.data(), 0, 4, dim0_matches.data(),
          dim0_multiplicity.data());
  set_dim(desc, 1, dim1_keys.data(), nullptr, 10, 3, nullptr, dim1_multiplicity.data());
  set_count_star(desc, 0);

  int32_t kernel_mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);

  OutputStorage one_shot(desc, true, true);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  CHECK(one_shot.out.selected_count == expected);
  CHECK(one_shot.out.uncertain_count == 0);
  CHECK(one_shot.out.emitted_group_count == 1);
  CHECK(one_shot.measures[0].count[0] == expected);
  CHECK(one_shot.active[0] == 1);

  const pgaccel_grouped_agg_workspace_req request = workspace_req(desc);
  SharedWorkspace workspace(request.bytes, request.alignment);
  constexpr size_t first_rows = 2048;
  pgaccel_grouped_agg_desc first = row_slice(desc, 0, first_rows);
  first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  CHECK_STATUS(execute_in_workspace(first, request, workspace, nullptr, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  pgaccel_grouped_agg_desc second = row_slice(desc, first_rows, rows - first_rows);
  second.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(second, request, workspace, nullptr, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc, true, true);
  CHECK_STATUS(execute_in_workspace(finalize, request, workspace, &chunked.out, &detail),
               PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  check_outputs_equal(chunked, one_shot, desc.measure_count);

  pgaccel_grouped_agg_desc grouped = desc;
  set_fact_key(grouped, 0, group_keys.data(), nullptr, 0, 2);
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&grouped, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC);

  pgaccel_grouped_agg_desc typed_count = desc;
  set_count_only_view(typed_count, 0, values.data(), nullptr, PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                      sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&typed_count, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC);

  SharedArray<int8_t> sql_mask(std::vector<int8_t>(rows, PGACCEL_EXPR_TRUE));
  pgaccel_grouped_agg_desc filtered = desc;
  filtered.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  filtered.where_filter.mask = sql_mask.data();
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&filtered, &kernel_mode), PGACCEL_OK);
  CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC);
}

void test_parallel_dense_sum_atomic_low_word_carry() {
  std::printf("--- row-parallel dense SUM low-word carry ---\n");
  constexpr size_t rows = 65536;
  constexpr size_t groups = 17;
  std::vector<int32_t> host_keys(rows);
  std::vector<int32_t> host_values(rows, INT32_MAX);
  std::array<int64_t, groups> expected_sum{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const size_t group = row % groups;
    host_keys[row] = static_cast<int32_t>(group);
    expected_sum[group] += host_values[row];
    ++expected_count[group];
  }
  for (const int64_t sum : expected_sum)
    CHECK(sum > static_cast<int64_t>(UINT32_MAX));

  SharedArray<int32_t> keys(host_keys);
  SharedArray<int32_t> values(host_values);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  set_i32_view(desc.measures[0].value, values.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
  set_count_star(desc, 1);

  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.group_codes[group] == group);
    CHECK(output.key_values[0][group] == static_cast<int32_t>(group));
    CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.measures[0].nonnull[group] == expected_count[group]);
    CHECK(output.measures[1].count[group] == expected_count[group]);
    CHECK(output.active[group] == 1);
  }
}

void test_parallel_dense_sum_atomic_negative_nullable_publication() {
  std::printf("--- row-parallel dense negative nullable SUM publication ---\n");
  constexpr size_t rows = 65539;
  constexpr size_t groups = 17;
  std::vector<int32_t> host_keys(rows);
  std::vector<int32_t> host_values(rows, -INT32_MAX);
  std::vector<uint8_t> host_nulls(rows);
  std::array<int64_t, groups> expected_sum{};
  std::array<uint64_t, groups> expected_nonnull{};
  std::array<uint64_t, groups> expected_count{};
  for (size_t row = 0; row < rows; ++row) {
    const size_t group = (row * 7 + 3) % groups;
    host_keys[row] = static_cast<int32_t>(group);
    host_nulls[row] = row % 19 == 0 ? 1 : 0;
    ++expected_count[group];
    if (host_nulls[row] == 0) {
      expected_sum[group] += host_values[row];
      ++expected_nonnull[group];
    }
  }
  for (const int64_t sum : expected_sum)
    CHECK(sum < -static_cast<int64_t>(UINT32_MAX));

  SharedArray<int32_t> keys(host_keys);
  SharedArray<int32_t> values(host_values);
  SharedArray<uint8_t> nulls(host_nulls);
  pgaccel_grouped_agg_desc desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  set_i32_view(desc.measures[0].value, values.data(), nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
  set_count_star(desc, 1);

  const pgaccel_grouped_agg_workspace_req nullable_req = workspace_req(desc);
  pgaccel_grouped_agg_desc nonnull_desc = desc;
  nonnull_desc.measures[0].value.nulls = nullptr;
  const pgaccel_grouped_agg_workspace_req nonnull_req = workspace_req(nonnull_desc);
  const size_t mask = nullable_req.alignment - 1;
  const size_t expected_nullable_bytes =
      ((nonnull_req.bytes + mask) & ~mask) + groups * sizeof(uint64_t);
  CHECK(nullable_req.alignment == nonnull_req.alignment);
  CHECK(nullable_req.bytes == expected_nullable_bytes);

  OutputStorage output(desc, true, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    CHECK(output.group_codes[group] == group);
    CHECK(output.key_values[0][group] == static_cast<int32_t>(group));
    CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
    CHECK(output.measures[0].nonnull[group] == expected_nonnull[group]);
    CHECK(output.measures[1].count[group] == expected_count[group]);
    CHECK(output.active[group] == 1);
  }
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

  SharedArray<double> both_nan({nan, nan});
  pgaccel_grouped_agg_desc both_nan_desc = base_desc(2);
  set_f64_view(both_nan_desc.measures[0].value, both_nan.data());
  finish_f64_measure(both_nan_desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                     PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
  OutputStorage both_nan_output(both_nan_desc);
  CHECK_STATUS(execute_external(both_nan_desc, &both_nan_output.out), PGACCEL_OK);
  CHECK(std::isnan(both_nan_output.f64(both_nan_output.measures[0].min, 0)));
  CHECK(std::isnan(both_nan_output.f64(both_nan_output.measures[0].max, 0)));
}

void test_i64_stats_pair_and_f64_binary_measures() {
  std::printf("--- I64 stats pair and F64 binary measures ---\n");
  SharedArray<int64_t> stats_values({2, -3, 5, 7});
  SharedArray<uint8_t> stats_nulls({0, 0, 1, 0});
  SharedArray<int64_t> stats_rhs({10, 20, 30, 40});
  SharedArray<uint8_t> stats_rhs_nulls({0, 1, 0, 0});
  SharedArray<double> mul_values({2.0, -3.0, 4.0, 5.0});
  SharedArray<double> mul_rhs({10.0, 2.0, -1.0, 0.5});
  SharedArray<double> sub_values({100.0, 50.0, -10.0, 8.0});
  SharedArray<double> sub_rhs({1.0, 70.0, -20.0, 3.0});

  pgaccel_grouped_agg_desc desc = base_desc(4);
  set_i64_view(desc.measures[0].value, stats_values.data(), stats_nulls.data());
  set_i64_view(desc.measures[0].rhs, stats_rhs.data(), stats_rhs_nulls.data());
  finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT |
                         PGACCEL_GROUPED_AGG_LANE_RHS_SUM | PGACCEL_GROUPED_AGG_LANE_RHS_COUNT);

  set_f64_view(desc.measures[1].value, mul_values.data());
  set_f64_view(desc.measures[1].rhs, mul_rhs.data());
  finish_f64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_MUL,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_SUMSQ |
                         PGACCEL_GROUPED_AGG_LANE_COUNT);

  set_f64_view(desc.measures[2].value, sub_values.data());
  set_f64_view(desc.measures[2].rhs, sub_rhs.data());
  finish_f64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_SUMSQ |
                         PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_count_star(desc, 3);

  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 4);
  CHECK(output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 1);

  CHECK(output.i64(output.measures[0].sum, 0) == 6);
  CHECK(output.i64(output.measures[0].min, 0) == -3);
  CHECK(output.i64(output.measures[0].max, 0) == 7);
  CHECK(output.measures[0].count[0] == 3);
  CHECK(output.measures[0].nonnull[0] == 3);
  CHECK(output.i64(output.measures[0].rhs_sum, 0) == 80);
  CHECK(output.measures[0].rhs_count[0] == 3);
  CHECK(output.measures[0].rhs_nonnull[0] == 3);

  CHECK(output.f64(output.measures[1].sum, 0) == 12.5);
  CHECK(output.f64(output.measures[1].min, 0) == -6.0);
  CHECK(output.f64(output.measures[1].max, 0) == 20.0);
  CHECK(output.f64(output.measures[1].sumsq, 0) == 458.25);
  CHECK(output.measures[1].count[0] == 4);
  CHECK(output.measures[1].nonnull[0] == 4);

  CHECK(output.f64(output.measures[2].sum, 0) == 94.0);
  CHECK(output.f64(output.measures[2].min, 0) == -20.0);
  CHECK(output.f64(output.measures[2].max, 0) == 99.0);
  CHECK(output.f64(output.measures[2].sumsq, 0) == 10326.0);
  CHECK(output.measures[2].count[0] == 4);
  CHECK(output.measures[2].nonnull[0] == 4);
  CHECK(output.measures[3].count[0] == 4);
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
    SharedArray<int32_t> values({0, 5, 9});
    pgaccel_grouped_agg_desc desc = base_desc(3);
    set_count_only_view(desc, 0, values.data(), nullptr, PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                        sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_i32(1);
    desc.where_filter.predicate_hi[0] = val_i32(8);

    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 1);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.measures[0].count[0] == 1);
    CHECK(output.measures[0].nonnull[0] == 1);
  }

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

void test_ordered_physical_min_max_count() {
  std::printf("--- ordered physical MIN/MAX/COUNT lanes ---\n");
  constexpr uint32_t lanes =
      PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT;

  {
    SharedArray<uint8_t> values({1, 0, 1});
    SharedArray<uint8_t> nulls({0, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(values.size());
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        sizeof(uint8_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.measures[0].agg_mask = lanes;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.i64(output.measures[0].min, 0) == 0);
    CHECK(output.i64(output.measures[0].max, 0) == 1);
    CHECK(output.measures[0].count[0] == 2);
    CHECK(output.measures[0].nonnull[0] == 2);
  }

  {
    SharedArray<float> values({-std::numeric_limits<float>::infinity(),
                               std::numeric_limits<float>::max(),
                               std::numeric_limits<float>::quiet_NaN(), 0.0F});
    SharedArray<uint8_t> nulls({0, 0, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(values.size());
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
                        sizeof(float), PGACCEL_GROUPED_AGG_ACCUM_F64);
    desc.measures[0].agg_mask = lanes;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(std::isinf(output.f64(output.measures[0].min, 0)) &&
          output.f64(output.measures[0].min, 0) < 0.0);
    CHECK(std::isnan(output.f64(output.measures[0].max, 0)));
    CHECK(output.measures[0].count[0] == 3);
    CHECK(output.measures[0].nonnull[0] == 3);
  }

  {
    SharedArray<int32_t> values(
        {std::numeric_limits<int32_t>::min(), 0, std::numeric_limits<int32_t>::max()});
    SharedArray<uint8_t> nulls({0, 1, 0});
    pgaccel_grouped_agg_desc desc = base_desc(values.size());
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                        sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.measures[0].agg_mask = lanes;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.i64(output.measures[0].min, 0) == std::numeric_limits<int32_t>::min());
    CHECK(output.i64(output.measures[0].max, 0) == std::numeric_limits<int32_t>::max());
    CHECK(output.measures[0].count[0] == 2);
    CHECK(output.measures[0].nonnull[0] == 2);
  }

  {
    SharedArray<int64_t> values(
        {std::numeric_limits<int64_t>::min(), 0, std::numeric_limits<int64_t>::max()});
    SharedArray<uint8_t> nulls({0, 1, 0});
    pgaccel_grouped_agg_desc desc = base_desc(values.size());
    set_count_only_view(desc, 0, values.data(), nulls.data(),
                        PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, sizeof(int64_t),
                        PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.measures[0].agg_mask = lanes;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.i64(output.measures[0].min, 0) == std::numeric_limits<int64_t>::min());
    CHECK(output.i64(output.measures[0].max, 0) == std::numeric_limits<int64_t>::max());
    CHECK(output.measures[0].count[0] == 2);
    CHECK(output.measures[0].nonnull[0] == 2);
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
  CHECK(output.out.uncertain_count == 0);
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
  CHECK(device_failure.out.selected_count == 0);
  CHECK(device_failure.out.uncertain_count == 0);
  CHECK(device_failure.measures[0].count[0] == OutputStorage::kSentinel);
}

void test_injected_lifecycle_failures_are_transactional_and_reusable() {
  std::printf("--- grouped allocation/copy/wait/materialization failure lifecycle ---\n");
  constexpr unsigned kOwnedAllocation = 1;
  constexpr unsigned kPublishedCopy = 2;
  constexpr unsigned kPublishedWait = 3;
  constexpr unsigned kOutputMaterialization = 4;

  SharedArray<int32_t> keys({0, 0, 0, 0});
  pgaccel_grouped_agg_desc desc = base_desc(keys.size());
  set_fact_key(desc, 0, keys.data(), nullptr, 0, 1);
  set_count_star(desc, 0);

  const auto assert_untouched = [](const OutputStorage& output) {
    CHECK(output.out.emitted_group_count == 0);
    CHECK(output.out.selected_count == 0);
    CHECK(output.out.uncertain_count == 0);
    CHECK(output.active == std::vector<uint8_t>({0xa5}));
    CHECK(output.measures[0].count == std::vector<uint64_t>({OutputStorage::kSentinel}));
  };

  for (const unsigned stage : {kPublishedCopy, kPublishedWait, kOutputMaterialization}) {
    OutputStorage failed(desc);
    pgacceltest_grouped_agg_fail_stage_once(stage);
    CHECK_STATUS(execute_external(desc, &failed.out), PGACCEL_ERROR);
    assert_untouched(failed);
    CHECK(pgacceltest_grouped_agg_active_scratch_owners() == 0);

    OutputStorage recovered(desc);
    CHECK_STATUS(execute_external(desc, &recovered.out), PGACCEL_OK);
    CHECK(recovered.out.emitted_group_count == 1);
    CHECK(recovered.out.selected_count == keys.size());
    CHECK(recovered.measures[0].count == std::vector<uint64_t>({keys.size()}));
    CHECK(pgacceltest_grouped_agg_active_scratch_owners() == 0);
  }

  // A null scratch descriptor exercises the native API's owned-workspace
  // allocation and ScratchOwner cleanup path rather than the test wrapper's
  // externally supplied shared workspace.
  OutputStorage allocation_failed(desc);
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  pgacceltest_grouped_agg_fail_stage_once(kOwnedAllocation);
  CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &allocation_failed.out, &detail),
               PGACCEL_ERROR);
  assert_untouched(allocation_failed);
  CHECK(pgacceltest_grouped_agg_active_scratch_owners() == 0);

  OutputStorage owned_recovered(desc);
  detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &owned_recovered.out, &detail), PGACCEL_OK);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  CHECK(owned_recovered.out.emitted_group_count == 1);
  CHECK(owned_recovered.out.selected_count == keys.size());
  CHECK(owned_recovered.measures[0].count == std::vector<uint64_t>({keys.size()}));
  CHECK(pgacceltest_grouped_agg_active_scratch_owners() == 0);
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

void test_generic_device_error_matrix() {
  std::printf("--- generic device error and rejection matrix ---\n");

  {
    SharedArray<uint8_t> values({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_count_only_view(desc, 0, values.data(), nullptr, PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        sizeof(uint8_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_bool(false);
    desc.where_filter.predicate_hi[0] = val_bool(true);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> values({7});
    SharedArray<uint8_t> nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                        sizeof(int32_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    desc.where_filter.predicate_measure_slot = 0;
    desc.where_filter.predicate_range_count = 1;
    desc.where_filter.predicate_lo[0] = val_i32(0);
    desc.where_filter.predicate_hi[0] = val_i32(10);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({2});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 0);
    CHECK(output.f64(output.measures[0].sum, 0) == 0.0);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint8_t> dim_nulls({2});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), dim_nulls.data(), 0, 1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint8_t> dim_match({2});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, dim_match.data());
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint64_t> multiplicity({0});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, multiplicity.data());
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 0);
    CHECK(output.f64(output.measures[0].sum, 0) == 0.0);
  }

  {
    SharedArray<int32_t> keys({0});
    SharedArray<uint8_t> key_nulls({2});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, 2, 1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint64_t> multiplicity({2});
    SharedArray<int32_t> lookup({0});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, multiplicity.data());
    set_dim_key(desc, 0, 0, lookup.data(), 0, 1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> keys({1});
    SharedArray<double> values({1.0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, 1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<double> values({1.0});
    SharedArray<int8_t> mask({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_f64_view(desc.measures[0].value, values.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    desc.measure_filters[0].kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.measure_filters[0].mask = mask.data();
    check_device_invalid(desc);
  }

  {
    SharedArray<int64_t> values({1});
    SharedArray<uint8_t> nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, values.data(), nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int64_t> lhs({1});
    SharedArray<int64_t> rhs({2});
    SharedArray<uint8_t> rhs_nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, lhs.data());
    set_i64_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<double> values({1.0});
    SharedArray<uint8_t> nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_f64_view(desc.measures[0].value, values.data(), nulls.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<double> lhs({1.0});
    SharedArray<double> rhs({2.0});
    SharedArray<uint8_t> rhs_nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_f64_view(desc.measures[0].value, lhs.data());
    set_f64_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_SUB, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int64_t> lhs({1});
    SharedArray<int64_t> rhs({2});
    SharedArray<uint8_t> rhs_nulls({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, lhs.data());
    set_i64_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                       PGACCEL_GROUPED_AGG_LANE_RHS_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint64_t> multiplicity({2});
    SharedArray<int64_t> lhs({1});
    SharedArray<int64_t> rhs({std::numeric_limits<int64_t>::max()});
    SharedArray<uint8_t> lhs_nulls({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, multiplicity.data());
    set_i64_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i64_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                       PGACCEL_GROUPED_AGG_LANE_RHS_SUM);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }
}

void test_specialized_dense_branch_matrix() {
  std::printf("--- specialized dense branch matrix ---\n");

  SharedArray<int32_t> specialization_dim_fact({0, 1, 2, 0, 1, 2});
  SharedArray<uint8_t> specialization_dim_match({1, 0, 1});
  SharedArray<int8_t> specialization_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE,
                                           PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                           PGACCEL_EXPR_TRUE});
  SharedArray<int32_t> specialization_values({1, 2, 3, 4, 5, 6});
  SharedArray<int32_t> specialization_rhs({2, 2, 2, 2, 2, 2});
  SharedArray<uint8_t> specialization_bools({0, 1, 0, 1, 0, 1});
  SharedArray<uint8_t> specialization_bool_nulls({0, 1, 0, 0, 1, 0});

  for (const bool membership : {false, true}) {
    for (const bool sql_mask : {false, true}) {
      pgaccel_grouped_agg_desc desc = base_desc(6);
      if (membership) {
        set_dim(desc, 0, specialization_dim_fact.data(), nullptr, 0,
                specialization_dim_match.size(), specialization_dim_match.data());
      }
      if (sql_mask) {
        desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.where_filter.mask = specialization_mask.data();
      }
      set_count_star(desc, 0);
      OutputStorage output(desc);
      CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
      const uint64_t expected = membership ? 4 : (sql_mask ? 5 : 6);
      CHECK(output.out.selected_count == expected);
      CHECK(output.measures[0].count[0] == expected);
    }
  }

  for (const bool membership : {false, true}) {
    for (const bool sql_mask : {false, true}) {
      pgaccel_grouped_agg_desc desc = base_desc(6);
      if (membership) {
        set_dim(desc, 0, specialization_dim_fact.data(), nullptr, 0,
                specialization_dim_match.size(), specialization_dim_match.data());
      }
      if (sql_mask) {
        desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.where_filter.mask = specialization_mask.data();
      }
      set_count_only_view(desc, 0, specialization_bools.data(), specialization_bool_nulls.data(),
                          PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, sizeof(uint8_t),
                          PGACCEL_GROUPED_AGG_ACCUM_I64);
      int32_t kernel_mode = 0;
      CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
      CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
      OutputStorage output(desc);
      CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
      const uint64_t expected_selected = membership ? 4 : (sql_mask ? 5 : 6);
      const uint64_t expected_count = 4;
      CHECK(output.out.selected_count == expected_selected);
      CHECK(output.measures[0].count[0] == expected_count);
      CHECK(output.measures[0].nonnull[0] == expected_count);
      CHECK(output.active[0] == 1);
    }
  }

  {
    SharedArray<int32_t> keys({0, 0, 1, 1});
    SharedArray<uint8_t> values({0, 1, 0, 1});
    SharedArray<uint8_t> nulls({1, 1, 0, 1});
    pgaccel_grouped_agg_desc desc = base_desc(4);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, 3);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        sizeof(uint8_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 4);
    CHECK(output.active[0] == 1);
    CHECK(output.measures[0].count[0] == 0);
    CHECK(output.measures[0].nonnull[0] == 0);
    CHECK(output.active[1] == 1);
    CHECK(output.measures[0].count[1] == 1);
    CHECK(output.measures[0].nonnull[1] == 1);
    CHECK(output.active[2] == 0);
  }

  for (const bool membership : {false, true}) {
    for (const bool sql_mask : {false, true}) {
      for (const bool multiply : {false, true}) {
        pgaccel_grouped_agg_desc desc = base_desc(6);
        if (membership) {
          set_dim(desc, 0, specialization_dim_fact.data(), nullptr, 0,
                  specialization_dim_match.size(), specialization_dim_match.data());
        }
        if (sql_mask) {
          desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
          desc.where_filter.mask = specialization_mask.data();
        }
        set_i32_view(desc.measures[0].value, specialization_values.data());
        if (multiply)
          set_i32_view(desc.measures[0].rhs, specialization_rhs.data());
        finish_i64_measure(desc, 0,
                           multiply ? PGACCEL_GROUPED_AGG_MEASURE_MUL
                                    : PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                           PGACCEL_GROUPED_AGG_LANE_SUM);
        set_count_star(desc, 1);
        OutputStorage output(desc);
        CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
        const uint64_t expected_count = membership ? 4 : (sql_mask ? 5 : 6);
        int64_t expected_sum = membership ? 14 : (sql_mask ? 19 : 21);
        if (multiply)
          expected_sum *= 2;
        CHECK(output.out.selected_count == expected_count);
        CHECK(output.measures[1].count[0] == expected_count);
        CHECK(output.i64(output.measures[0].sum, 0) == expected_sum);
      }
    }
  }

  {
    constexpr size_t rows = 4097;
    constexpr size_t groups = 4;
    std::vector<int32_t> host_keys(rows);
    std::vector<uint8_t> host_key_nulls(rows);
    std::vector<int32_t> host_lhs(rows);
    std::vector<uint8_t> host_lhs_nulls(rows);
    std::vector<int32_t> host_rhs(rows);
    std::vector<uint8_t> host_rhs_nulls(rows);
    std::array<int64_t, groups> expected_sum{};
    std::array<uint64_t, groups> expected_count{};
    std::array<uint64_t, groups> expected_nonnull{};
    uint64_t expected_selected = 0;
    for (size_t row = 0; row < rows; ++row) {
      const bool null_key = row % 11 == 0;
      const size_t group = null_key ? groups - 1 : row % (groups - 1);
      host_keys[row] = static_cast<int32_t>(row % (groups - 1));
      host_key_nulls[row] = null_key ? 1 : 0;
      host_lhs[row] = static_cast<int32_t>(row % 1001);
      host_lhs_nulls[row] = row % 13 == 0 ? 1 : 0;
      host_rhs[row] = static_cast<int32_t>(row % 9) - 4;
      host_rhs_nulls[row] = row % 17 == 0 ? 1 : 0;
      if (host_lhs_nulls[row] == 0 && host_lhs[row] >= 200 && host_lhs[row] <= 800) {
        ++expected_selected;
        ++expected_count[group];
        if (host_rhs_nulls[row] == 0) {
          expected_sum[group] += static_cast<int64_t>(host_lhs[row]) * host_rhs[row];
          ++expected_nonnull[group];
        }
      }
    }

    SharedArray<int32_t> keys(host_keys);
    SharedArray<uint8_t> key_nulls(host_key_nulls);
    SharedArray<int32_t> lhs(host_lhs);
    SharedArray<uint8_t> lhs_nulls(host_lhs_nulls);
    SharedArray<int32_t> rhs(host_rhs);
    SharedArray<uint8_t> rhs_nulls(host_rhs_nulls);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, groups, groups - 1);
    set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i32_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_value_range(desc, 0, 200, 800);
    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == expected_selected);
    for (size_t group = 0; group < groups; ++group) {
      CHECK(output.active[group] == (expected_count[group] != 0 ? 1 : 0));
      CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
      CHECK(output.measures[0].nonnull[group] == expected_nonnull[group]);
      CHECK(output.measures[1].count[group] == expected_count[group]);
    }
  }

  {
    constexpr size_t rows = 4097;
    constexpr size_t groups = 256;
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_lhs(rows);
    std::vector<uint8_t> host_lhs_nulls(rows);
    std::vector<int32_t> host_rhs(rows);
    std::vector<uint8_t> host_rhs_nulls(rows);
    std::array<int64_t, groups> expected_sum{};
    std::array<uint64_t, groups> expected_count{};
    std::array<uint64_t, groups> expected_nonnull{};
    uint64_t expected_selected = 0;
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = static_cast<int32_t>(group);
      host_lhs[row] = 198 + static_cast<int32_t>(row % 605);
      host_lhs_nulls[row] = row % 31 == 0 ? 1 : 0;
      host_rhs[row] = static_cast<int32_t>(row % 7) - 3;
      host_rhs_nulls[row] = row % 37 == 0 ? 1 : 0;
      if (host_lhs_nulls[row] == 0 && host_lhs[row] >= 200 && host_lhs[row] <= 800) {
        ++expected_selected;
        ++expected_count[group];
        if (host_rhs_nulls[row] == 0) {
          expected_sum[group] += static_cast<int64_t>(host_lhs[row]) * host_rhs[row];
          ++expected_nonnull[group];
        }
      }
    }

    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> lhs(host_lhs);
    SharedArray<uint8_t> lhs_nulls(host_lhs_nulls);
    SharedArray<int32_t> rhs(host_rhs);
    SharedArray<uint8_t> rhs_nulls(host_rhs_nulls);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i32_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_value_range(desc, 0, 200, 800);
    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == expected_selected);
    for (size_t group = 0; group < groups; ++group) {
      CHECK(output.active[group] == (expected_count[group] != 0 ? 1 : 0));
      CHECK(output.i64(output.measures[0].sum, group) == expected_sum[group]);
      CHECK(output.measures[0].nonnull[group] == expected_nonnull[group]);
      CHECK(output.measures[1].count[group] == expected_count[group]);
    }
  }

  {
    constexpr size_t rows = 2048;
    constexpr size_t groups = 256;
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_lhs(rows, 400);
    std::vector<uint8_t> host_lhs_nulls(rows, 0);
    std::vector<int32_t> host_rhs(rows, 2);
    for (size_t row = 0; row < rows; ++row)
      host_keys[row] = static_cast<int32_t>(row % groups);
    host_lhs_nulls[1024] = 2;
    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> lhs(host_lhs);
    SharedArray<uint8_t> lhs_nulls(host_lhs_nulls);
    SharedArray<int32_t> rhs(host_rhs);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_value_range(desc, 0, 200, 800);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(output.out.selected_count == 0);
    CHECK(output.active == std::vector<uint8_t>(groups, 0xa5));
    CHECK(output.measures[0].sum == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
    CHECK(output.measures[1].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
  }

  for (const size_t groups : {size_t{4}, size_t{256}}) {
    constexpr size_t rows = 4097;
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_lhs(rows, 800);
    std::vector<int32_t> host_rhs(rows, 1);
    for (size_t row = 0; row < rows; ++row)
      host_keys[row] = static_cast<int32_t>(row % groups);
    host_rhs[rows / 2] = 3'000'000;
    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> lhs(host_lhs);
    SharedArray<int32_t> rhs(host_rhs);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_value_range(desc, 0, 200, 800);
    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    CHECK(output.out.selected_count == 0);
    CHECK(output.active == std::vector<uint8_t>(groups, 0xa5));
    CHECK(output.measures[0].sum == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
    CHECK(output.measures[1].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
  }

  {
    SharedArray<int32_t> lhs({-100, 200, 800, 900});
    SharedArray<int32_t> rhs({1, 1, 1, 1});
    pgaccel_grouped_agg_desc desc = base_desc(lhs.size());
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    set_i32_value_range(desc, 0, INT32_MIN, 800);
    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 3);
    CHECK(output.i64(output.measures[0].sum, 0) == 900);
    CHECK(output.measures[1].count[0] == 3);

    set_i32_value_range(desc, 0, 200, 200);
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);

    set_i32_value_range(desc, 0, 200, 800);
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
  }

  {
    constexpr size_t rows = 4097;
    constexpr size_t groups = 4;
    std::vector<int32_t> host_keys(rows);
    std::vector<int32_t> host_values(rows);
    std::vector<int8_t> host_mask(rows);
    std::vector<uint8_t> host_bool_values(rows);
    std::vector<uint8_t> host_bool_nulls(rows);
    std::array<int64_t, groups> expected_sum{};
    std::array<uint64_t, groups> expected_count{};
    std::array<uint64_t, groups> expected_bool_count{};
    for (size_t row = 0; row < rows; ++row) {
      const size_t group = row % groups;
      host_keys[row] = static_cast<int32_t>(group);
      host_values[row] = static_cast<int32_t>(row % 101) - 50;
      host_bool_values[row] = static_cast<uint8_t>(row % 2);
      host_bool_nulls[row] = row % 5 == 0 ? 1 : 0;
      host_mask[row] = row % 7 == 0 ? PGACCEL_EXPR_FALSE : PGACCEL_EXPR_TRUE;
      if (host_mask[row] == PGACCEL_EXPR_TRUE) {
        expected_sum[group] += host_values[row];
        ++expected_count[group];
        if (host_bool_nulls[row] == 0)
          ++expected_bool_count[group];
      }
    }
    SharedArray<int32_t> keys(host_keys);
    SharedArray<int32_t> values(host_values);
    SharedArray<int8_t> mask(host_mask);
    SharedArray<uint8_t> bool_values(host_bool_values);
    SharedArray<uint8_t> bool_nulls(host_bool_nulls);

    pgaccel_grouped_agg_desc count_desc = base_desc(rows);
    set_fact_key(count_desc, 0, keys.data(), nullptr, 0, groups);
    set_count_star(count_desc, 0);
    count_desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    count_desc.where_filter.mask = mask.data();
    OutputStorage count_output(count_desc);
    CHECK_STATUS(execute_external(count_desc, &count_output.out), PGACCEL_OK);
    for (size_t group = 0; group < groups; ++group)
      CHECK(count_output.measures[0].count[group] == expected_count[group]);

    pgaccel_grouped_agg_desc bool_count_desc = base_desc(rows);
    set_fact_key(bool_count_desc, 0, keys.data(), nullptr, 0, groups);
    set_count_only_view(bool_count_desc, 0, bool_values.data(), bool_nulls.data(),
                        PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, sizeof(uint8_t),
                        PGACCEL_GROUPED_AGG_ACCUM_I64);
    bool_count_desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    bool_count_desc.where_filter.mask = mask.data();
    int32_t bool_kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&bool_count_desc, &bool_kernel_mode), PGACCEL_OK);
    CHECK(bool_kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
    OutputStorage bool_count_output(bool_count_desc);
    CHECK_STATUS(execute_external(bool_count_desc, &bool_count_output.out), PGACCEL_OK);
    CHECK(bool_count_output.out.selected_count ==
          std::accumulate(expected_count.begin(), expected_count.end(), uint64_t{0}));
    for (size_t group = 0; group < groups; ++group) {
      CHECK(bool_count_output.measures[0].count[group] == expected_bool_count[group]);
      CHECK(bool_count_output.measures[0].nonnull[group] == expected_bool_count[group]);
      CHECK(bool_count_output.active[group] == (expected_count[group] != 0 ? 1 : 0));
    }

    pgaccel_grouped_agg_desc integer_desc = base_desc(rows);
    set_fact_key(integer_desc, 0, keys.data(), nullptr, 0, groups);
    set_i32_view(integer_desc.measures[0].value, values.data());
    finish_i64_measure(integer_desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                       PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(integer_desc, 1);
    integer_desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    integer_desc.where_filter.mask = mask.data();
    OutputStorage integer_output(integer_desc);
    CHECK_STATUS(execute_external(integer_desc, &integer_output.out), PGACCEL_OK);
    for (size_t group = 0; group < groups; ++group) {
      CHECK(integer_output.i64(integer_output.measures[0].sum, group) == expected_sum[group]);
      CHECK(integer_output.measures[1].count[group] == expected_count[group]);
    }
  }

  {
    constexpr size_t rows = 2048;
    std::vector<uint8_t> host_values(rows, 1);
    std::vector<uint8_t> host_nulls(rows, 0);
    host_nulls[1024] = 2;
    SharedArray<uint8_t> values(host_values);
    SharedArray<uint8_t> nulls(host_nulls);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_count_only_view(desc, 0, values.data(), nulls.data(), PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        sizeof(uint8_t), PGACCEL_GROUPED_AGG_ACCUM_I64);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0, 0, 4, 0, 1, 2, 2, 2, 2, 2});
    SharedArray<uint8_t> dim_nulls({2, 1, 0, 0, 0, 0, 0, 0, 0, 0});
    SharedArray<uint8_t> dim_match({2, 0, 1});
    SharedArray<int32_t> keys({0, 0, 0, 0, 0, 0, 2, 0, 0, 0});
    SharedArray<uint8_t> key_nulls({0, 0, 0, 0, 0, 1, 0, 0, 0, 0});
    SharedArray<int8_t> where_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE, 2, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE});

    pgaccel_grouped_agg_desc desc = base_desc(10);
    set_dim(desc, 0, dim_fact.data(), dim_nulls.data(), 0, dim_match.size(), dim_match.data());
    set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, 2, 1);
    set_count_star(desc, 0);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = where_mask.data();
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> keys({0, 2, 0, 0, 0, 0, 0, 0});
    SharedArray<uint8_t> key_nulls({2, 0, 0, 0, 0, 0, 0, 0});
    SharedArray<int32_t> lhs({1, 1, std::numeric_limits<int32_t>::max(), 1, 1, 1, 1, 3});
    SharedArray<int32_t> rhs({1, 1, 2, 1, 1, 1, 1, 4});
    SharedArray<uint8_t> lhs_nulls({0, 0, 0, 2, 0, 0, 0, 0});
    SharedArray<uint8_t> rhs_nulls({0, 0, 0, 0, 2, 0, 0, 0});
    SharedArray<int8_t> where_mask({PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE,
                                    PGACCEL_EXPR_TRUE, PGACCEL_EXPR_TRUE, 2, PGACCEL_EXPR_FALSE,
                                    PGACCEL_EXPR_TRUE});

    pgaccel_grouped_agg_desc desc = base_desc(8);
    set_fact_key(desc, 0, keys.data(), key_nulls.data(), 0, 2, 1);
    set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i32_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = where_mask.data();
    check_device_invalid(desc);
  }

  {
    constexpr size_t rows = 2048;
    std::vector<int32_t> host_lhs(rows, 3);
    std::vector<int32_t> host_rhs(rows, 4);
    std::vector<uint8_t> host_lhs_nulls(rows, 0);
    std::vector<uint8_t> host_rhs_nulls(rows, 0);
    std::vector<int8_t> host_where_mask(rows, PGACCEL_EXPR_TRUE);
    host_where_mask[0] = PGACCEL_EXPR_FALSE;
    host_lhs_nulls[1] = 1;
    host_rhs_nulls[2] = 1;
    host_where_mask[4] = 2;
    host_rhs_nulls[1024] = 2;

    SharedArray<int32_t> lhs(host_lhs);
    SharedArray<int32_t> rhs(host_rhs);
    SharedArray<uint8_t> lhs_nulls(host_lhs_nulls);
    SharedArray<uint8_t> rhs_nulls(host_rhs_nulls);
    SharedArray<int8_t> where_mask(host_where_mask);
    pgaccel_grouped_agg_desc desc = base_desc(rows);
    set_i32_view(desc.measures[0].value, lhs.data(), lhs_nulls.data());
    set_i32_view(desc.measures[0].rhs, rhs.data(), rhs_nulls.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    set_count_star(desc, 1);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = where_mask.data();
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;

    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }
}

void test_weighted_overflow_branch_matrix() {
  std::printf("--- weighted overflow branch matrix ---\n");

  {
    SharedArray<int32_t> dim_fact({0});
    SharedArray<uint64_t> multiplicity({std::numeric_limits<uint64_t>::max()});
    SharedArray<int64_t> values({-2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, multiplicity.data());
    set_i64_view(desc.measures[0].value, values.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }

  {
    SharedArray<int32_t> dim_fact({0, 0});
    SharedArray<uint64_t> multiplicity({std::numeric_limits<uint64_t>::max()});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, multiplicity.data());
    set_count_star(desc, 0);
    int32_t kernel_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
    CHECK(kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  }

  {
    SharedArray<int64_t> lhs({2});
    SharedArray<int64_t> rhs({3});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i64_view(desc.measures[0].value, lhs.data());
    set_i64_view(desc.measures[0].rhs, rhs.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                       PGACCEL_GROUPED_AGG_LANE_RHS_COUNT);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.measures[0].rhs_count[0] == 1);
  }
}

void test_compact_full_lane_copy() {
  std::printf("--- compact full-lane state copy ---\n");
  SharedArray<int32_t> keys({1});
  SharedArray<double> values({2.0});
  SharedArray<double> rhs({3.0});

  pgaccel_grouped_agg_desc desc = base_desc(1);
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  set_fact_key(desc, 0, keys.data(), nullptr, 0, 2);
  set_f64_view(desc.measures[0].value, values.data());
  set_f64_view(desc.measures[0].rhs, rhs.data());
  finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                     PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN);

  OutputStorage output(desc, true);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.emitted_group_count == 1);
  CHECK(output.out.selected_count == 1);
  CHECK(output.group_codes[0] == 1);
  CHECK(output.key_values[0][0] == 1);
  CHECK(output.f64(output.measures[0].sum, 0) == 2.0);
  CHECK(output.f64(output.measures[0].min, 0) == 2.0);
  CHECK(output.f64(output.measures[0].max, 0) == 2.0);
  CHECK(output.f64(output.measures[0].sumsq, 0) == 4.0);
  CHECK(output.measures[0].count[0] == 1);
  CHECK(output.measures[0].nonnull[0] == 1);
  CHECK(output.f64(output.measures[0].rhs_sum, 0) == 3.0);
  CHECK(output.measures[0].rhs_count[0] == 1);
  CHECK(output.measures[0].rhs_nonnull[0] == 1);
}

void test_output_descriptor_validation() {
  std::printf("--- grouped output descriptor validation ---\n");

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.flags = 1;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.active_groups = nullptr;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.active_groups = reinterpret_cast<uint8_t*>(output.out.measures[0].count);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    set_fact_key(desc, 0, nullptr, nullptr, 0, 1);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.keys[0].values = nullptr;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    set_fact_key(desc, 0, nullptr, nullptr, 0, 1);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.keys[0].type = PGACCEL_VAL_INT64;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.keys[1].flags = 1;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.measures[0].count = nullptr;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.measures[1].count = output.out.measures[0].count;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_HASH;
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    desc.group_capacity = 4;
    desc.key_count = 1;
    desc.keys[0].values.type = PGACCEL_VAL_INT64;
    desc.keys[0].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
    desc.keys[0].null_code = PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
    set_count_star(desc, 0);
    OutputStorage output(desc, true);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }
}

void test_pointer_span_validation() {
  std::printf("--- grouped pointer span validation ---\n");
  const uintptr_t invalid_address = std::numeric_limits<uintptr_t>::max();
  const auto* invalid_i8 = reinterpret_cast<const int8_t*>(invalid_address);
  const auto* invalid_u8 = reinterpret_cast<const uint8_t*>(invalid_address);
  const auto* invalid_i32 = reinterpret_cast<const int32_t*>(invalid_address);

  {
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_fact_key(desc, 0, invalid_i32, nullptr, 0, 1);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> keys({0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_fact_key(desc, 0, keys.data(), invalid_u8, 0, 2, 1);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1);
    set_dim_key(desc, 0, 0, invalid_i32, 0, 1);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, invalid_i32);
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> values({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, values.data(), invalid_u8);
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> lhs({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, invalid_i32);
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> lhs({1});
    SharedArray<int32_t> rhs({2});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, lhs.data());
    set_i32_view(desc.measures[0].rhs, rhs.data(), invalid_u8);
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_LANE_SUM);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> values({1});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, values.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    desc.measure_filters[0].kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.measure_filters[0].mask = invalid_i8;
    check_device_invalid(desc);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_count_star(desc, 0);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = invalid_i8;
    check_device_invalid(desc);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, invalid_i32, nullptr, 0, 1);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), invalid_u8, 0, 1);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, invalid_u8);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    SharedArray<int32_t> dim_fact({0});
    const auto* invalid_u64 = reinterpret_cast<const uint64_t*>(invalid_address);
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_dim(desc, 0, dim_fact.data(), nullptr, 0, 1, nullptr, invalid_u64);
    set_count_star(desc, 0);
    check_device_invalid(desc);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc, true);
    output.out.group_codes = reinterpret_cast<size_t*>(invalid_address);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.active_groups = reinterpret_cast<uint8_t*>(invalid_address);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    set_fact_key(desc, 0, nullptr, nullptr, 0, 1);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.keys[0].values = reinterpret_cast<void*>(invalid_address);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    set_fact_key(desc, 0, nullptr, nullptr, 0, 2, 1);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.keys[0].nulls = reinterpret_cast<uint8_t*>(invalid_address);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_f64_view(desc.measures[0].value, nullptr);
    set_f64_view(desc.measures[0].rhs, nullptr);
    finish_f64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR,
                       PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN);
    for (size_t index = 0; index < 9; ++index) {
      OutputStorage output(desc);
      pgaccel_grouped_agg_measure_out& measure = output.out.measures[0];
      switch (index) {
        case 0:
          measure.sum = reinterpret_cast<void*>(invalid_address);
          break;
        case 1:
          measure.min = reinterpret_cast<void*>(invalid_address);
          break;
        case 2:
          measure.max = reinterpret_cast<void*>(invalid_address);
          break;
        case 3:
          measure.sumsq = reinterpret_cast<void*>(invalid_address);
          break;
        case 4:
          measure.count = reinterpret_cast<uint64_t*>(invalid_address);
          break;
        case 5:
          measure.nonnull_count = reinterpret_cast<uint64_t*>(invalid_address);
          break;
        case 6:
          measure.rhs_sum = reinterpret_cast<void*>(invalid_address);
          break;
        case 7:
          measure.rhs_count = reinterpret_cast<uint64_t*>(invalid_address);
          break;
        case 8:
          measure.rhs_nonnull_count = reinterpret_cast<uint64_t*>(invalid_address);
          break;
      }
      CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
    }
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    const uintptr_t aligned_invalid =
        invalid_address & ~(static_cast<uintptr_t>(req.alignment) - 1);
    desc.scratch = reinterpret_cast<void*>(aligned_invalid);
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    SharedArray<int32_t> placeholder({7});
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, placeholder.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.measures[0].value.values = workspace.data();
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    int32_t host_value = 7;
    pgaccel_grouped_agg_desc desc = base_desc(1);
    set_i32_view(desc.measures[0].value, &host_value);
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(execute_external_ex(desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    OutputStorage output(desc);
    output.out.output_space = PGACCEL_MEM_SPACE_SHARED_USM;
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    SharedArray<uint8_t> shared_active(1);
    OutputStorage output(desc);
    output.out.active_groups = shared_active.data();
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_ERROR);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    SharedArray<uint8_t> active(1);
    SharedArray<uint64_t> count(1);
    active[0] = 0xa5;
    count[0] = OutputStorage::kSentinel;
    pgaccel_grouped_agg_out output = {};
    output.abi_version = PGACCEL_OLAP_ABI_VERSION;
    output.size_bytes = sizeof(output);
    output.group_capacity = 1;
    output.output_space = PGACCEL_MEM_SPACE_SHARED_USM;
    output.active_groups = active.data();
    output.measures[0].count = count.data();
    CHECK_STATUS(execute_external(desc, &output), PGACCEL_OK);
    CHECK(output.emitted_group_count == 1);
    CHECK(output.selected_count == 0);
    CHECK(active[0] == 1);
    CHECK(count[0] == 0);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    alignas(64) static std::array<uint8_t, 65536> host_workspace{};
    CHECK(req.bytes <= host_workspace.size());
    desc.scratch = host_workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  }
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

  {
    SharedArray<int64_t> values({std::numeric_limits<int64_t>::max(), 1});
    pgaccel_grouped_agg_desc desc = base_desc(2);
    set_i64_view(desc.measures[0].value, values.data());
    finish_i64_measure(desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    pgaccel_grouped_agg_desc first = row_slice(desc, 0, 1);
    first.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    CHECK_STATUS(execute_in_workspace(first, req, workspace, nullptr, &detail), PGACCEL_OK);
    pgaccel_grouped_agg_desc second = row_slice(desc, 1, 1);
    second.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    CHECK_STATUS(execute_in_workspace(second, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    pgaccel_grouped_agg_desc finalize = row_slice(desc, 0, 0);
    finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage poisoned(desc);
    CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &poisoned.out, &detail),
                 PGACCEL_ERROR);
    CHECK(poisoned.measures[0].sum[0] == OutputStorage::kSentinel);

    pgaccel_grouped_agg_desc reset = row_slice(desc, 1, 1);
    reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    OutputStorage recovered(desc);
    CHECK_STATUS(execute_in_workspace(reset, req, workspace, &recovered.out, &detail), PGACCEL_OK);
    CHECK(recovered.i64(recovered.measures[0].sum, 0) == 1);
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
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    output.out.active_groups = reinterpret_cast<uint8_t*>(&desc.execution_flags);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    pgaccel_reset_gpu_exec_count();
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(pgaccel_gpu_exec_count() == 0);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    output.out.active_groups = reinterpret_cast<uint8_t*>(&output.out.measures[0].count);
    int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    pgaccel_reset_gpu_exec_count();
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(pgaccel_gpu_exec_count() == 0);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    int32_t* detail = reinterpret_cast<int32_t*>(&output.out.flags);
    pgaccel_reset_gpu_exec_count();
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, detail), PGACCEL_ERROR);
    CHECK(*detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(pgaccel_gpu_exec_count() == 0);
  }

  {
    pgaccel_grouped_agg_desc desc = base_desc(0);
    set_count_star(desc, 0);
    const pgaccel_grouped_agg_workspace_req req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    desc.scratch = workspace.data();
    desc.scratch_bytes = req.bytes;
    desc.scratch_space = PGACCEL_MEM_SPACE_SHARED_USM;
    desc.scratch_alignment = static_cast<uint32_t>(req.alignment);
    OutputStorage output(desc);
    int32_t* detail = reinterpret_cast<int32_t*>(&desc._pad0);
    pgaccel_reset_gpu_exec_count();
    CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &output.out, detail), PGACCEL_ERROR);
    CHECK(*detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(pgaccel_gpu_exec_count() == 0);
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
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
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

// Independent host fixtures use PostgreSQL's base-10000 wire digit convention.
// Payloads reside in USM; only the descriptor view itself is ordinary host memory.
class NumericFixture {
 public:
  NumericFixture(const std::vector<pgaccel_numeric_row>& headers,
                 const std::vector<uint16_t>& payload, int32_t min_weight, int32_t max_weight,
                 uint32_t max_ndigits, const std::vector<uint8_t>& nulls = {})
      : rows(headers), digits(payload), null_flags(nulls) {
    view = {rows.data(), digits.data(), digits.size(), rows.size(),
            min_weight,  max_weight,    max_ndigits,   0};
  }

  void bind(pgaccel_grouped_agg_desc& desc, uint32_t slot, bool count = true) {
    auto& measure = desc.measures[slot];
    measure = {};
    measure.op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
    measure.accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
    measure.agg_mask = PGACCEL_GROUPED_AGG_LANE_SUM | (count ? PGACCEL_GROUPED_AGG_LANE_COUNT : 0);
    measure.state_bytes = sizeof(pgaccel_numeric_sum) +
                          ((2 * (view.max_weight - view.min_weight + 5) + 7) & ~size_t{7});
    measure.value.values = &view;
    measure.value.nulls = null_flags.data();
    measure.value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
    measure.value.flags = PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW;
    measure.value.element_bytes = sizeof(view);
    desc.measure_count = std::max(desc.measure_count, slot + 1);
  }

  SharedArray<pgaccel_numeric_row> rows;
  SharedArray<uint16_t> digits;
  SharedArray<uint8_t> null_flags;
  pgaccel_numeric_view view = {};
};

void check_numeric(const pgaccel_grouped_agg_desc& desc, const OutputStorage& output, size_t slot,
                   size_t group, int32_t weight, uint32_t sign, uint32_t scale,
                   const std::vector<uint16_t>& expected, uint64_t count) {
  const auto* header = reinterpret_cast<const pgaccel_numeric_sum*>(
      output.measures[slot].sum.data() + group * desc.measures[slot].state_bytes / 8);
  CHECK(header->weight == weight);
  CHECK(header->sign == sign);
  CHECK(header->dscale == scale);
  CHECK(header->ndigits == expected.size());
  const auto* digits = reinterpret_cast<const uint16_t*>(header + 1);
  if (header->ndigits == expected.size())
    CHECK(std::equal(expected.begin(), expected.end(), digits));
  CHECK(output.measures[slot].nonnull[group] == count);
  if (!output.measures[slot].count.empty())
    CHECK(output.measures[slot].count[group] == count);
}

void test_numeric_exact_arithmetic() {
  // 9999.9999 + .0001 - 1 = 9999.0000; cancellation of a 41-digit
  // integer leaves a tiny negative fraction that cannot survive float casts.
  NumericFixture values({{0, 2, 0, 0, 4},
                         {2, 1, -1, 0, 4},
                         {3, 1, 0, 0x4000, 0},
                         {4, 1, 10, 0, 0},
                         {5, 1, 10, 0x4000, 0},
                         {6, 1, -2, 0x4000, 8},
                         {7, 0, 0, 0, 7},
                         {7, 1, 0, 0, 0}},
                        {9999, 9999, 1, 1, 1, 1, 1, 42}, -2, 10, 2, {0, 0, 0, 0, 0, 0, 0, 1});
  SharedArray<int32_t> keys({0, 0, 0, 1, 1, 1, 2, 3});
  for (uint32_t measures = 1; measures <= 4; ++measures) {
    auto desc = base_desc(values.rows.size());
    set_fact_key(desc, 0, keys.data(), nullptr, 0, 5);
    for (uint32_t slot = 0; slot < measures; ++slot)
      values.bind(desc, slot, slot % 2 == 0);
    if (measures < 4)
      set_count_star(desc, measures);
    OutputStorage output(desc, true, true);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.out.selected_count == 8);
    CHECK(output.out.emitted_group_count == 4);
    CHECK(output.active == std::vector<uint8_t>({1, 1, 1, 1, 0}));
    for (uint32_t slot = 0; slot < measures; ++slot) {
      check_numeric(desc, output, slot, 0, 0, 0, 4, {9999}, 3);
      check_numeric(desc, output, slot, 1, -2, 0x4000, 8, {1}, 3);
      check_numeric(desc, output, slot, 2, 0, 0, 7, {}, 1);
      check_numeric(desc, output, slot, 3, 0, 0, 0, {}, 0);
      check_numeric(desc, output, slot, 4, 0, 0, 0, {}, 0);
    }
    if (measures < 4)
      CHECK(output.measures[measures].count == std::vector<uint64_t>({3, 3, 1, 1, 0}));
  }
  // Every row cancels exactly; the greatest finite display scale survives.
  NumericFixture cancel({{0, 1, 20, 0, 0}, {1, 1, 20, 0x4000, 12}}, {9999, 9999}, 20, 20, 1);
  auto desc = base_desc(2);
  cancel.bind(desc, 0);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  check_numeric(desc, output, 0, 0, 0, 0, 12, {}, 2);
}

void test_numeric_specials_and_extremes() {
  NumericFixture specials({{0, 0, 0, 0xd000, 32},
                           {0, 0, 0, 0xf000, 32},
                           {0, 0, 0, 0xd000, 32},
                           {0, 0, 0, 0xf000, 32},
                           {0, 0, 0, 0xc000, 0},
                           {0, 1, 0, 0, 4}},
                          {1}, 0, 0, 1);
  SharedArray<int32_t> keys({0, 0, 1, 2, 3, 3});
  auto desc = base_desc(6);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, 4);
  specials.bind(desc, 0);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  check_numeric(desc, output, 0, 0, 0, 0xc000, 0, {}, 2);
  check_numeric(desc, output, 0, 1, 0, 0xd000, 32, {}, 1);
  check_numeric(desc, output, 0, 2, 0, 0xf000, 32, {}, 1);
  check_numeric(desc, output, 0, 3, 0, 0xc000, 0, {}, 2);

  // Full PostgreSQL numeric precision: 131072 integer + 16383 fractional
  // digits. Last base digit ends in zero because scale is not divisible by 4.
  std::vector<uint16_t> huge_digits(36864, 9999);
  huge_digits.back() = 9990;
  NumericFixture huge({{0, 36864, 32767, 0, 16383}}, huge_digits, -4096, 32767, 36864);
  desc = base_desc(1);
  huge.bind(desc, 0);
  OutputStorage huge_output(desc);
  CHECK_STATUS(execute_external(desc, &huge_output.out), PGACCEL_OK);
  check_numeric(desc, huge_output, 0, 0, 32767, 0, 16383, huge_digits, 1);

  NumericFixture empty({}, {}, 0, 0, 0);
  desc = base_desc(0);
  empty.bind(desc, 0);
  OutputStorage empty_output(desc);
  CHECK_STATUS(execute_external(desc, &empty_output.out), PGACCEL_OK);
  CHECK(empty_output.active[0] == 1);
  CHECK(empty_output.out.emitted_group_count == 1);
  check_numeric(desc, empty_output, 0, 0, 0, 0, 0, {}, 0);
}

void test_numeric_lifecycle_and_invalid() {
  NumericFixture values({{0, 1, 0, 0, 0}, {1, 1, 0, 0, 0}}, {9999, 2}, 0, 0, 1);
  auto desc = base_desc(2);
  values.bind(desc, 0);
  OutputStorage one_shot(desc);
  CHECK_STATUS(execute_external(desc, &one_shot.out), PGACCEL_OK);
  check_numeric(desc, one_shot, 0, 0, 1, 0, 0, {1, 1}, 2);
  auto req = workspace_req(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  OutputStorage chunked(desc);
  int32_t detail = 0;
  values.view.row_count = desc.row_count = 0;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  CHECK(chunked.measures[0].sum[0] == OutputStorage::kSentinel);
  desc.row_count = 1;
  values.view.row_count = 1;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  values.view.rows = values.rows.data() + 1;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  values.view.row_count = desc.row_count = 0;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  CHECK(chunked.measures[0].sum == one_shot.measures[0].sum);
  CHECK(chunked.measures[0].count == one_shot.measures[0].count);
  CHECK(chunked.out.selected_count == 2);

  // Same byte footprint is insufficient: changed digit exponents must not
  // reinterpret an existing workspace's buckets at finalize.
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  values.view.min_weight = values.view.max_weight = 1;
  OutputStorage stale(desc);
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &stale.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  CHECK(stale.measures[0].sum[0] == OutputStorage::kSentinel);
  values.view.min_weight = values.view.max_weight = 0;

  values.view.rows = values.rows.data();
  values.view.row_count = desc.row_count = 2;
  const auto original = values.rows[0];
  for (int invalid = 0; invalid < 7; ++invalid) {
    values.rows[0] = original;
    values.digits[0] = 9999;
    switch (invalid) {
      case 0:
        values.digits[0] = 10000;
        break;
      case 1:
        values.rows[0].digit_offset = UINT64_MAX;
        break;
      case 2:
        values.rows[0].ndigits = 2;
        break;
      case 3:
        values.rows[0].weight = 1;
        break;
      case 4:
        values.rows[0].sign = 0x8000;
        break;
      case 5:
        values.rows[0].dscale = 16384;
        break;
      case 6:
        values.rows[0].sign = 0xc000;
        break;
    }
    OutputStorage failed(desc);
    const auto sentinel = failed.measures[0].sum;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(failed.measures[0].sum == sentinel);
    CHECK(failed.active[0] == 0xa5);
    values.view.row_count = desc.row_count = 0;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
    CHECK(failed.measures[0].sum == sentinel);
    values.rows[0] = original;
    values.digits[0] = 9999;
    values.view.row_count = desc.row_count = 2;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, &failed.out, &detail), PGACCEL_OK);
    CHECK(failed.measures[0].sum == one_shot.measures[0].sum);
  }
}

void test_numeric_overflow_and_admission() {
  NumericFixture values({{0, 1, 32767, 0, 0}, {1, 1, 32767, 0, 0}, {2, 1, 32767, 0x4000, 0}},
                        {9999, 9999, 9999}, 32767, 32767, 1);
  auto desc = base_desc(3);
  values.bind(desc, 0);
  OutputStorage canceled(desc);
  CHECK_STATUS(execute_external(desc, &canceled.out), PGACCEL_OK);
  check_numeric(desc, canceled, 0, 0, 32767, 0, 0, {9999}, 3);

  values.view.row_count = desc.row_count = 2;
  OutputStorage overflow(desc);
  const auto untouched = overflow.measures[0].sum;
  int32_t detail = 0;
  CHECK_STATUS(execute_external_ex(desc, &overflow.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  CHECK(overflow.measures[0].sum == untouched);
  CHECK(overflow.measures[0].nonnull[0] == OutputStorage::kSentinel);

  values.view.row_count = desc.row_count = 3;
  values.rows[2] = {2, 0, 0, 0xd000, 32};
  OutputStorage infinite(desc);
  CHECK_STATUS(execute_external(desc, &infinite.out), PGACCEL_OK);
  check_numeric(desc, infinite, 0, 0, 0, 0xd000, 32, {}, 3);

  int32_t mode = 0;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
  CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC);
  auto malformed = desc;
  malformed.measures[0].state_bytes -= 8;
  pgaccel_status status = PGACCEL_OK;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_ERROR);
  malformed = desc;
  malformed.measures[0].value.flags = 0;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);
  malformed = desc;
  malformed.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  workspace_req(malformed, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);
  values.view.reserved = 1;
  workspace_req(desc, &status);
  CHECK(status == PGACCEL_ERROR);
  values.view.reserved = 0;
  values.view.min_weight = -4097;
  workspace_req(desc, &status);
  CHECK(status == PGACCEL_ERROR);
}

void test_numeric_where_filters() {
  NumericFixture decimal({{0, 1, 0, 0, 0},
                          {0, 0, 0, 0xc000, 0},
                          {1, 1, 32767, 0, 0},
                          {2, 1, 0, 0, 0},
                          {3, 1, 32767, 0, 0}},
                         {1, 9999, 4, 9999}, 0, 32767, 1);
  SharedArray<int32_t> groups({0, 0, 1, 1, 2});
  SharedArray<int32_t> predicates({1, 1, 0, 2, 0});
  SharedArray<uint8_t> nulls({0, 1, 0, 0, 0});
  auto desc = base_desc(5);
  set_fact_key(desc, 0, groups.data(), nullptr, 0, 3);
  decimal.bind(desc, 0);
  finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_i32_view(desc.measures[1].value, predicates.data(), nulls.data());
  set_count_star(desc, 2);
  desc.where_filter.predicate_measure_slot = 1;
  desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  desc.where_filter.predicate_range_count = 2;
  desc.where_filter.predicate_lo[0] = desc.where_filter.predicate_hi[0] = val_i32(1);
  desc.where_filter.predicate_lo[1] = desc.where_filter.predicate_hi[1] = val_i32(2);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 2 && output.out.uncertain_count == 0);
  CHECK(output.out.emitted_group_count == 2);
  CHECK(output.active == std::vector<uint8_t>({1, 1, 0}));
  check_numeric(desc, output, 0, 0, 0, 0, 0, {1}, 1);
  check_numeric(desc, output, 0, 1, 0, 0, 0, {4}, 1);
  CHECK(output.measures[2].count == std::vector<uint64_t>({1, 1, 0}));
  const auto req = workspace_req(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  int32_t detail = 0;
  desc.row_count = decimal.view.row_count = 0;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  for (size_t row = 0; row < 5; ++row) {
    desc.row_count = decimal.view.row_count = 1;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    desc.keys[0].values.values = groups.data() + row;
    decimal.view.rows = decimal.rows.data() + row;
    set_i32_view(desc.measures[1].value, predicates.data() + row, nulls.data() + row);
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  }
  desc.row_count = decimal.view.row_count = 0;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  OutputStorage chunked(desc);
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, &chunked.out, &detail), PGACCEL_OK);
  CHECK(chunked.out.selected_count == 2 && chunked.out.emitted_group_count == 2);
  CHECK(chunked.measures[0].sum == output.measures[0].sum);
  desc.row_count = decimal.view.row_count = 5;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.keys[0].values.values = groups.data();
  decimal.view.rows = decimal.rows.data();
  set_i32_view(desc.measures[1].value, predicates.data(), nulls.data());
  // SQL mask UNKNOWN rejects, unlike spatial uncertainty that requires an
  // exact recheck outside this family. Invalid mask bytes fail closed.
  SharedArray<int8_t> mask({1, 0, -1, 1, -1});
  // Both predicates must apply: the mask removes row zero, while ranges
  // independently exclude NULL/NaN and overflowing decimal input rows.
  SharedArray<int8_t> combined_mask({0, 1, 1, 1, 1});
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = combined_mask.data();
  OutputStorage combined(desc);
  CHECK_STATUS(execute_external(desc, &combined.out), PGACCEL_OK);
  CHECK(combined.out.selected_count == 1 && combined.out.emitted_group_count == 1);
  CHECK(combined.active == std::vector<uint8_t>({0, 1, 0}));
  check_numeric(desc, combined, 0, 1, 0, 0, 0, {4}, 1);
  desc.where_filter = base_desc(0).where_filter;
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  desc.where_filter.mask = mask.data();
  OutputStorage masked(desc);
  CHECK_STATUS(execute_external(desc, &masked.out), PGACCEL_OK);
  CHECK(masked.measures[0].sum == output.measures[0].sum);
  CHECK(masked.out.selected_count == 2 && masked.out.uncertain_count == 0);
  mask[0] = 2;
  OutputStorage invalid(desc);
  CHECK_STATUS(execute_external(desc, &invalid.out), PGACCEL_ERROR);
  for (size_t row = 0; row < 5; ++row)
    mask[row] = -1;
  OutputStorage rejected(desc);
  CHECK_STATUS(execute_external(desc, &rejected.out), PGACCEL_OK);
  CHECK(rejected.out.selected_count == 0 && rejected.out.emitted_group_count == 0);
  desc.key_count = 0;
  desc.keys[0] = {};
  desc.group_capacity = 1;
  OutputStorage global(desc);
  CHECK_STATUS(execute_external(desc, &global.out), PGACCEL_OK);
  CHECK(global.out.selected_count == 0 && global.out.emitted_group_count == 1);
  CHECK(global.measures[0].nonnull[0] == 0 && global.measures[2].count[0] == 0);
}

void test_numeric_mixed_integer_states() {
  NumericFixture decimal(
      {{0, 1, 0, 0, 0}, {1, 1, 0, 0, 0}, {2, 1, 0, 0, 0}, {3, 1, 0, 0, 0}, {4, 1, 0, 0, 0}},
      {1, 2, 3, 4, 5}, 0, 0, 1);
  SharedArray<int32_t> groups({0, 0, 1, 1, 2});
  SharedArray<int32_t> integers({INT32_MIN, INT32_MAX, -17, 20, 99});
  SharedArray<uint8_t> nulls({0, 0, 0, 0, 1});
  SharedArray<int32_t> predicates({1, 1, 0, 1, 1});
  auto desc = base_desc(5);
  set_fact_key(desc, 0, groups.data(), nullptr, 0, 3);
  for (size_t slot = 0; slot < 2; ++slot) {
    finish_i64_measure(desc, slot, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                       PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
    set_i32_view(desc.measures[slot].value, integers.data(), nulls.data());
  }
  desc.measures[1].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
  set_i32_view(desc.measures[1].rhs, predicates.data(), nullptr);
  auto& filter = desc.measure_filters[1];
  filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
  filter.predicate_measure_slot = 1;
  filter.predicate_range_count = 1;
  filter.predicate_lo[0] = val_i32(1);
  filter.predicate_hi[0] = val_i32(1);
  decimal.bind(desc, 2);
  set_count_star(desc, 3);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.measures[0].sum == std::vector<uint64_t>({uint64_t(-1), 3, 0}));
  CHECK(output.measures[1].sum == std::vector<uint64_t>({uint64_t(-1), 20, 0}));
  CHECK(output.measures[0].count == std::vector<uint64_t>({2, 2, 0}));
  CHECK(output.measures[1].count == std::vector<uint64_t>({2, 1, 0}));
  CHECK(output.measures[3].count == std::vector<uint64_t>({2, 2, 1}));
  check_numeric(desc, output, 2, 0, 0, 0, 0, {3}, 2);
  check_numeric(desc, output, 2, 1, 0, 0, 0, {7}, 2);
  int32_t mode = -1;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
  CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC);
  const auto req = workspace_req(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  int32_t detail = 0;
  for (int repeat = 0; repeat < 2; ++repeat) {
    desc.row_count = decimal.view.row_count = 0;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
    for (size_t row = 0; row < 5; ++row) {
      desc.row_count = decimal.view.row_count = 1;
      desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      desc.keys[0].values.values = groups.data() + row;
      decimal.view.rows = decimal.rows.data() + row;
      for (size_t slot = 0; slot < 2; ++slot)
        set_i32_view(desc.measures[slot].value, integers.data() + row, nulls.data() + row);
      set_i32_view(desc.measures[1].rhs, predicates.data() + row, nullptr);
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
    }
    desc.row_count = decimal.view.row_count = 0;
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    OutputStorage chunked(desc);
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, &chunked.out, &detail), PGACCEL_OK);
    for (size_t slot = 0; slot < 4; ++slot) {
      CHECK(chunked.measures[slot].sum == output.measures[slot].sum);
      CHECK(chunked.measures[slot].count == output.measures[slot].count);
    }
  }
  // Forecast only: payload spans are not accessed. Exact prefix-safe row
  // bounds apply to the entire lifecycle, not merely each dispatch chunk.
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.row_count = uint64_t{INT64_MAX} / (uint64_t{1} << 31);
  decimal.view.row_count = desc.row_count;
  pgaccel_status status = PGACCEL_ERROR;
  workspace_req(desc, &status);
  CHECK(status == PGACCEL_OK);
  ++desc.row_count;
  decimal.view.row_count = desc.row_count;
  workspace_req(desc, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);
}

void set_scalar_measure(pgaccel_grouped_agg_desc& desc, uint32_t slot,
                        const pgaccel_scalar_view& view, uint32_t lanes) {
  auto& col = desc.measures[slot].value;
  col = {};
  col.values = &view;
  col.physical_type = view.words[1];
  col.element_bytes = sizeof(view);
  col.flags = PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1;
  finish_i64_measure(desc, slot, PGACCEL_GROUPED_AGG_MEASURE_SCALAR, lanes);
}

void test_fused_scalar_programs() {
  namespace s = pgaccel_scalar;
  std::printf("--- fused scalar CASE/COALESCE and aggregate lifecycle ---\n");
  // CASE WHEN flag THEN COALESCE(a, 7) + 2 ELSE NULL END.
  const std::vector<int32_t> words{s::MAGIC, s::INT4, 2, s::INT4, s::BOOL, 0, 1,      51,
                                   8,        0,       0, 54,      5,       1, 7,      1,
                                   2,        10,      0, 50,      9,       3, s::INT4};
  constexpr size_t rows = 4097;
  std::vector<int32_t> host(rows), host_keys(rows);
  std::vector<uint8_t> host_nulls(rows), host_bool(rows);
  std::vector<int8_t> host_mask(rows);
  for (size_t row = 0; row < rows; ++row) {
    host[row] = int32_t(row % 31) - 15;
    host_nulls[row] = row % 5 == 0;
    host_bool[row] = row % 3 != 0;
    host_mask[row] = row % 7 != 0;
  }
  SharedArray<int32_t> values(host), keys(rows);
  SharedArray<uint8_t> nulls(host_nulls), flags(host_bool);
  SharedArray<int8_t> mask(host_mask);
  const pgaccel_scalar_input inputs[] = {{values.data(), nulls.data(), s::INT4, 0},
                                         {flags.data(), nullptr, s::BOOL, 0}};
  const pgaccel_scalar_view view{1, uint32_t(words.size()), 2, 0, rows, words.data(), inputs};
  const uint32_t lanes = PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                         PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT;
  for (size_t groups : {size_t{1}, size_t{3}, size_t{8}, size_t{9}, size_t{10}, size_t{17},
                        size_t{18}, size_t{19}, size_t{33}, size_t{36}, size_t{37}, size_t{73},
                        size_t{74}}) {
    for (size_t row = 0; row < rows; ++row)
      keys[row] = int32_t(row % groups);
    auto desc = base_desc(rows);
    if (groups > 1)
      set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
    set_scalar_measure(desc, 0, view, lanes);
    set_scalar_measure(desc, 1, view, PGACCEL_GROUPED_AGG_LANE_COUNT);
    set_count_star(desc, 2);
    desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    desc.where_filter.mask = mask.data();
    OutputStorage reference(desc);
    CHECK_STATUS(execute_external(desc, &reference.out), PGACCEL_OK);
    for (size_t group = 0; group < groups; ++group) {
      int64_t sum = 0, lo = INT64_MAX, hi = INT64_MIN;
      uint64_t count = 0, selected = 0;
      for (size_t row = group; row < rows; row += groups) {
        if (!host_mask[row])
          continue;
        ++selected;
        if (!host_bool[row])
          continue;
        const int64_t value = (host_nulls[row] ? 7 : host[row]) + 2;
        sum += value;
        lo = std::min(lo, value);
        hi = std::max(hi, value);
        ++count;
      }
      CHECK(reference.i64(reference.measures[0].sum, group) == sum);
      if (count) {
        CHECK(reference.i64(reference.measures[0].min, group) == lo);
        CHECK(reference.i64(reference.measures[0].max, group) == hi);
      }
      CHECK(reference.measures[0].count[group] == count);
      CHECK(reference.measures[0].nonnull[group] == count);
      CHECK(reference.measures[1].count[group] == count);
      CHECK(reference.measures[2].count[group] == selected);
    }
    // Low-cardinality hierarchical and wider atomic bounded reductions.
    // The 4097-row fixture straddles ordered-partial tile boundaries too.
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    const auto req = workspace_req(desc);
    SharedWorkspace workspace(req.bytes, req.alignment);
    int32_t detail = 0;
    for (size_t offset : {size_t{0}, size_t{2048}}) {
      auto chunk = desc;
      chunk.row_count = offset == 0 ? 2048 : rows - offset;
      if (offset)
        chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      const pgaccel_scalar_input sliced_inputs[] = {
          {values.data() + offset, nulls.data() + offset, s::INT4, 0},
          {flags.data() + offset, nullptr, s::BOOL, 0}};
      auto sliced = view;
      sliced.inputs = sliced_inputs;
      sliced.row_count = rows - offset;
      for (size_t slot = 0; slot < 2; ++slot)
        chunk.measures[slot].value.values = &sliced;
      if (groups > 1)
        chunk.keys[0].values.values = keys.data() + offset;
      chunk.where_filter.mask = mask.data() + offset;
      CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
    }
    desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    desc.row_count = 0;
    OutputStorage finalized(desc);
    CHECK_STATUS(execute_in_workspace(desc, req, workspace, &finalized.out, &detail), PGACCEL_OK);
    check_outputs_equal(finalized, reference, desc.measure_count);
  }
}

void test_atomic_bounded_ordered_prefix_fallback() {
  namespace s = pgaccel_scalar;
  std::printf("--- bounded atomic batches retain ordered overflow fallback ---\n");
  SharedArray<int32_t> values({10, -10}), keys({0, 0});
  const int32_t words[] = {s::MAGIC, s::INT4, 1, s::INT4, 0, 0};
  const pgaccel_scalar_input input{values.data(), nullptr, s::INT4, 0};
  pgaccel_scalar_view view{1, uint32_t(std::size(words)), 1, 0, 1'000'001, words, &input};
  auto maximum = base_desc(1'000'001);
  set_fact_key(maximum, 0, keys.data(), nullptr, 0, 33);
  set_scalar_measure(maximum, 0, view,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_count_star(maximum, 1);
  maximum.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  // The just-over-one-shot maximum must reserve the atomic scratch needed by
  // shorter chunks even though it cannot use atomics for its own row count.
  const auto req = workspace_req(maximum);
  auto shorter = maximum;
  shorter.row_count = 1'000'000;
  const auto shorter_req = workspace_req(shorter);
  CHECK(req.bytes >= shorter_req.bytes);
  SharedWorkspace workspace(req.bytes, req.alignment);
  auto desc = maximum;
  desc.row_count = view.row_count = 2;
  constexpr uint64_t prior_rows = uint64_t{INT64_MAX} / (uint64_t{1} << 31) + 1;
  for (bool negative : {false, true}) {
    const int64_t initial = negative ? INT64_MIN + 5 : INT64_MAX - 5;
    for (bool overflow : {false, true}) {
      const int32_t toward_limit = negative ? -10 : 10;
      values[0] = overflow ? toward_limit : -toward_limit;
      values[1] = -values[0];
      desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
      desc.row_count = 0;
      int32_t detail = 0;
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
      CHECK(pgacceltest_grouped_agg_seed_integer_prefix(&desc, workspace.data(), prior_rows, initial));
      desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      desc.row_count = 2;
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail),
                   overflow ? PGACCEL_ERROR : PGACCEL_OK);
      if (overflow)
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
      auto final = desc;
      final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
      final.row_count = 0;
      OutputStorage output(final);
      CHECK_STATUS(execute_in_workspace(final, req, workspace, &output.out, &detail),
                   overflow ? PGACCEL_ERROR : PGACCEL_OK);
      if (overflow) {
        CHECK(output.measures[1].count == std::vector<uint64_t>(33, OutputStorage::kSentinel));
      } else {
        CHECK(output.i64(output.measures[0].sum, 0) == initial);
        CHECK(output.measures[0].count[0] == prior_rows + 2);
        CHECK(output.measures[1].count[0] == prior_rows + 2);
      }
    }
  }
}

void test_scalar_date_results_and_errors() {
  namespace s = pgaccel_scalar;
  SharedArray<int32_t> dates({s::DATE_MIN, s::DATE_END - 1, INT32_MIN, INT32_MAX, 0, 0});
  SharedArray<int32_t> days({0, 0, INT32_MAX, INT32_MIN, 1, 1});
  SharedArray<uint8_t> nulls({0, 0, 0, 0, 1, 0});
  SharedArray<int32_t> keys({0, 0, 0, 0, 0, 0});
  const int32_t words[] = {s::MAGIC, s::DATE, 2, s::DATE, s::INT4, 0, 0, 0, 1, 70, 0};
  const pgaccel_scalar_input inputs[] = {{dates.data(), nulls.data(), s::DATE, 0},
                                         {days.data(), nullptr, s::INT4, 0}};
  const pgaccel_scalar_view view{1, uint32_t(std::size(words)), 2, 0, 6, words, inputs};
  for (size_t groups : {size_t{1}, size_t{9}, size_t{33}, size_t{74}}) {
    for (bool bounded : {false, true}) {
      auto desc = base_desc(6);
      if (groups > 1)
        set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
      set_scalar_measure(desc, 0, view, PGACCEL_GROUPED_AGG_LANE_COUNT |
                            PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
      set_count_star(desc, 1);
      if (bounded)
        desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
      const auto req = workspace_req(desc);
      SharedWorkspace workspace(req.bytes, req.alignment);
      int32_t detail = 0;
      OutputStorage output(desc);
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, bounded ? nullptr : &output.out, &detail), PGACCEL_OK);
      if (bounded) {
        auto final = desc;
        final.row_count = 0;
        final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
        CHECK_STATUS(execute_in_workspace(final, req, workspace, &output.out, &detail), PGACCEL_OK);
      }
      CHECK(output.measures[0].count[0] == 5 && output.measures[1].count[0] == 6);
      CHECK(output.measures[0].min[0] == INT32_MIN && output.measures[0].max[0] == INT32_MAX);
      // Finite upper overflow must poison publication, including COUNT-only.
      days[1] = 1;
      OutputStorage failed(desc);
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, bounded ? nullptr : &failed.out, &detail), PGACCEL_ERROR);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DATE_OUT_OF_RANGE);
      CHECK(failed.measures[1].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
      days[1] = 0;
      // Reset after the error must restore a usable session.
      OutputStorage recovered(desc);
      CHECK_STATUS(execute_in_workspace(desc, req, workspace, bounded ? nullptr : &recovered.out, &detail), PGACCEL_OK);
      if (bounded) {
        auto final = desc;
        final.row_count = 0;
        final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
        CHECK_STATUS(execute_in_workspace(final, req, workspace, &recovered.out, &detail), PGACCEL_OK);
      }
      CHECK(recovered.measures[0].count[0] == 5 && recovered.measures[1].count[0] == 6);
      desc.measures[0].agg_mask |= PGACCEL_GROUPED_AGG_LANE_SUM;
      pgaccel_status status = PGACCEL_OK;
      workspace_req(desc, &status);
      CHECK(status == PGACCEL_ERROR);
    }
  }
}

void test_scalar_where_predicates() {
  namespace s = pgaccel_scalar;
  SharedArray<uint8_t> accepted({1, 0, 1, 1}), nulls({0, 0, 1, 0});
  SharedArray<int32_t> numerator({10, 20, 30, 40}), denominator({2, 0, 0, 4});
  SharedArray<int32_t> keys({0, 1, 2, 0});
  const int32_t bool_words[] = {s::MAGIC, s::BOOL, 1, s::BOOL, 0, 0};
  const pgaccel_scalar_input bool_input{accepted.data(), nulls.data(), s::BOOL, 0};
  const pgaccel_scalar_view predicate{
      1, uint32_t(std::size(bool_words)), 1, 0, 4, bool_words, &bool_input};
  const int32_t value_words[] = {s::MAGIC, s::INT4, 2, s::INT4, s::INT4, 0, 0, 0, 1, 13, 0};
  const pgaccel_scalar_input value_inputs[] = {{numerator.data(), nullptr, s::INT4, 0},
                                               {denominator.data(), nullptr, s::INT4, 0}};
  const pgaccel_scalar_view value{
      1, uint32_t(std::size(value_words)), 2, 0, 4, value_words, value_inputs};
  for (size_t groups : {size_t{1}, size_t{9}, size_t{33}, size_t{74}}) {
    for (bool bounded : {false, true}) {
      for (size_t carrier_slot : {size_t{0}, size_t{1}}) {
        const size_t value_slot = 1 - carrier_slot;
        auto desc = base_desc(4);
        if (groups > 1)
          set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
        set_scalar_measure(desc, value_slot, value,
                           PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
        set_scalar_measure(desc, carrier_slot, predicate, PGACCEL_GROUPED_AGG_LANE_COUNT);
        set_count_star(desc, 2);
        desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
        desc.where_filter.predicate_measure_slot = carrier_slot;
        if (bounded)
          desc.execution_flags =
              PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        const auto req = workspace_req(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        OutputStorage output(desc);
        int32_t detail = 0;
        CHECK_STATUS(
            execute_in_workspace(desc, req, workspace, bounded ? nullptr : &output.out, &detail),
            PGACCEL_OK);
        if (bounded) {
          auto next = desc;
          next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
          CHECK_STATUS(execute_in_workspace(next, req, workspace, nullptr, &detail), PGACCEL_OK);
          auto final = desc;
          final.row_count = 0;
          final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
          CHECK_STATUS(execute_in_workspace(final, req, workspace, &output.out, &detail),
                       PGACCEL_OK);
        }
        const uint64_t batches = bounded ? 2 : 1;
        CHECK(output.measures[value_slot].sum[0] == 15 * batches &&
              output.measures[value_slot].count[0] == 2 * batches);
        CHECK(output.measures[carrier_slot].count[0] == 2 * batches);
        CHECK(output.measures[2].count[0] == 2 * batches);
        for (size_t group = 1; group < groups; ++group)
          CHECK(output.active[group] == 0);  // FALSE/UNKNOWN cannot create groups.

        // WHERE itself may throw, with the same typed error as an argument.
        const int32_t bad_words[] = {
            s::MAGIC, s::BOOL, 2, s::INT4, s::INT4, 0,  0, 0,
            1,        13,      0, 1,       0,       24, 0};  // numerator/denominator > 0
        const pgaccel_scalar_view bad{
            1, uint32_t(std::size(bad_words)), 2, 0, 4, bad_words, value_inputs};
        set_scalar_measure(desc, carrier_slot, bad, PGACCEL_GROUPED_AGG_LANE_COUNT);
        const auto bad_req = workspace_req(desc);
        SharedWorkspace bad_workspace(bad_req.bytes, bad_req.alignment);
        OutputStorage failed(desc);
        CHECK_STATUS(execute_in_workspace(desc, bad_req, bad_workspace,
                                          bounded ? nullptr : &failed.out, &detail),
                     PGACCEL_ERROR);
        CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO);
        CHECK(failed.measures[2].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));

        // A scalar selector is not a general measure FILTER or an untyped input.
        auto invalid = desc;
        invalid.measure_filters[0] = invalid.where_filter;
        pgaccel_status status = PGACCEL_OK;
        workspace_req(invalid, &status);
        CHECK(status == PGACCEL_ERROR);
        invalid = desc;
        invalid.where_filter.predicate_measure_slot = value_slot;  // INT4, not Boolean
        workspace_req(invalid, &status);
        CHECK(status == PGACCEL_ERROR);
        invalid = desc;
        invalid.where_filter.flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
        workspace_req(invalid, &status);
        CHECK(status == PGACCEL_ERROR);
      }
    }
  }

  // The only value worker cannot be elided, with or without COUNT(*).
  for (bool count_star : {false, true}) {
    auto desc = base_desc(4);
    set_scalar_measure(desc, 0, predicate, PGACCEL_GROUPED_AGG_LANE_COUNT);
    if (count_star)
      set_count_star(desc, 1);
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.measures[0].count[0] == 2);
    if (count_star)
      CHECK(output.measures[1].count[0] == 2);
  }

  // Three values, arbitrary carrier position, no COUNT(*). Other FILTERs
  // can reject every aggregate input without removing selected-row activity.
  for (size_t carrier_slot : {size_t{0}, size_t{1}, size_t{2}}) {
    auto desc = base_desc(4);
    for (size_t slot = 0; slot < 3; ++slot) {
      set_scalar_measure(desc, slot, slot == carrier_slot ? predicate : value,
                         PGACCEL_GROUPED_AGG_LANE_COUNT);
      if (slot != carrier_slot) {
        desc.measures[slot].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
        set_i32_view(desc.measures[slot].rhs, numerator.data());
        auto& filter = desc.measure_filters[slot];
        filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
        filter.predicate_measure_slot = slot;
        filter.predicate_range_count = 1;
        filter.predicate_lo[0] = filter.predicate_hi[0] = val_i32(999);
      }
    }
    desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
    desc.where_filter.predicate_measure_slot = carrier_slot;
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    CHECK(output.active[0] == 1);
    for (size_t slot = 0; slot < 3; ++slot)
      CHECK(output.measures[slot].count[0] == (slot == carrier_slot ? 2 : 0));
  }
}

void test_scalar_where_mask_position() {
  namespace s = pgaccel_scalar;
  SharedArray<int32_t> numerator({10, 20, 30, 40}), denominator({2, 0, 0, 4});
  SharedArray<int32_t> keys({0, 1, 2, 0});
  SharedArray<int8_t> mask({1, 0, -1, 1});
  const int32_t mask_first[] = {s::MAGIC, s::BOOL, 2, s::INT4, s::INT4, 8,  0, 51, 8, 0, 0, 0,
                                1,        13,      0, 1,       0,       24, 0, 50, 9, 2, 0};
  const int32_t scalar_first[] = {s::MAGIC, s::BOOL, 2, s::INT4, s::INT4, 0, 0, 0,  1, 13, 0, 1,
                                  0,        24,      0, 51,      8,       8, 0, 50, 9, 2,  0};
  const pgaccel_scalar_input inputs[] = {{numerator.data(), nullptr, s::INT4, 0},
                                         {denominator.data(), nullptr, s::INT4, 0}};
  const int32_t value_words[] = {s::MAGIC, s::INT4, 1, s::INT4, 0, 0};
  const pgaccel_scalar_view value{1, 6, 1, 0, 4, value_words, inputs};
  for (size_t groups : {size_t{1}, size_t{9}, size_t{33}, size_t{74}}) {
    for (size_t carrier : {size_t{0}, size_t{1}}) {
      for (bool bounded : {false, true}) {
        for (bool mask_before : {false, true}) {
          const pgaccel_scalar_view predicate{1, uint32_t(std::size(mask_first)),         2,     0,
                                              4, mask_before ? mask_first : scalar_first, inputs};
          auto desc = base_desc(4);
          if (groups > 1)
            set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
          set_scalar_measure(desc, carrier, predicate, PGACCEL_GROUPED_AGG_LANE_COUNT);
          set_scalar_measure(desc, 1 - carrier, value,
                             PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
          desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
          desc.where_filter.mask = mask.data();
          desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
          desc.where_filter.predicate_measure_slot = carrier;
          if (bounded)
            desc.execution_flags =
                PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
          const auto req = workspace_req(desc);
          SharedWorkspace workspace(req.bytes, req.alignment);
          OutputStorage output(desc);
          int32_t detail = 0;
          CHECK_STATUS(
              execute_in_workspace(desc, req, workspace, bounded ? nullptr : &output.out, &detail),
              mask_before ? PGACCEL_OK : PGACCEL_ERROR);
          if (!mask_before) {
            CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO);
            CHECK(output.measures[carrier].count ==
                  std::vector<uint64_t>(groups, OutputStorage::kSentinel));
            continue;
          }
          if (bounded) {
            auto next = desc;
            next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
            CHECK_STATUS(execute_in_workspace(next, req, workspace, nullptr, &detail), PGACCEL_OK);
            next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
            next.row_count = 0;
            CHECK_STATUS(execute_in_workspace(next, req, workspace, &output.out, &detail),
                         PGACCEL_OK);
          }
          const uint64_t batches = bounded ? 2 : 1;
          CHECK(output.measures[carrier].count[0] == 2 * batches);
          CHECK(output.measures[1 - carrier].sum[0] == 50 * batches);
          for (size_t group = 1; group < groups; ++group)
            CHECK(output.active[group] == 0);
          auto invalid = desc;
          invalid.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
          invalid.where_filter.mask = nullptr;
          pgaccel_status status = PGACCEL_OK;
          workspace_req(invalid, &status);
          CHECK(status == PGACCEL_ERROR);
          invalid = desc;
          invalid.where_filter.predicate_measure_slot = 1 - carrier;
          workspace_req(invalid, &status);
          CHECK(status == PGACCEL_ERROR);
        }
      }
    }
  }
}

void test_scalar_where_ordered_carrier() {
  namespace s = pgaccel_scalar;
  // Above the atomic batch cap, with enough groups to require the ordered
  // non-hierarchical reducer. Predicate and value NULLs are independent.
  constexpr size_t rows = 1'000'001, groups = 74;
  SharedArray<uint8_t> flags(rows), predicate_nulls(rows), value_nulls(rows);
  SharedArray<int8_t> sql_mask(rows);
  SharedArray<int32_t> values(rows), keys(rows);
  std::array<uint64_t, groups> selected{}, present{};
  std::array<int64_t, groups> sums{};
  uint64_t selected_total = 0;
  for (size_t row = 0; row < rows; ++row) {
    keys[row] = row % groups;
    flags[row] = row % 3 == 0;
    predicate_nulls[row] = row % 7 == 0;
    sql_mask[row] = predicate_nulls[row] ? -1 : static_cast<int8_t>(flags[row]);
    value_nulls[row] = row % 11 == 0;
    values[row] = static_cast<int32_t>(row % 17) - 8;
    if (flags[row] && !predicate_nulls[row]) {
      ++selected[keys[row]];
      ++selected_total;
      if (!value_nulls[row]) {
        ++present[keys[row]];
        sums[keys[row]] += values[row];
      }
    }
  }
  const int32_t predicate_words[] = {s::MAGIC, s::BOOL, 1, s::BOOL, 0, 0};
  const int32_t mask_words[] = {s::MAGIC, s::BOOL, 1, s::BOOL, 8, 0};
  const int32_t value_words[] = {s::MAGIC, s::INT4, 1, s::INT4, 0, 0};
  const pgaccel_scalar_input predicate_input{flags.data(), predicate_nulls.data(), s::BOOL, 0};
  const pgaccel_scalar_input value_input{values.data(), value_nulls.data(), s::INT4, 0};
  pgaccel_scalar_view predicate{1, 6, 1, 0, rows, predicate_words, &predicate_input};
  const pgaccel_scalar_view value{1, 6, 1, 0, rows, value_words, &value_input};
  for (bool masked : {false, true}) {
    predicate.words = masked ? mask_words : predicate_words;
    for (size_t carrier : {size_t{0}, size_t{1}}) {
      for (bool bounded : {false, true}) {
        auto desc = base_desc(rows);
        set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
        set_scalar_measure(desc, carrier, predicate, PGACCEL_GROUPED_AGG_LANE_COUNT);
        set_scalar_measure(desc, 1 - carrier, value,
                           PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
        desc.where_filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
        desc.where_filter.predicate_measure_slot = carrier;
        if (masked) {
          desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
          desc.where_filter.mask = sql_mask.data();
        }
        if (bounded)
          desc.execution_flags =
              PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        const auto req = workspace_req(desc);
        SharedWorkspace workspace(req.bytes, req.alignment);
        OutputStorage output(desc);
        int32_t detail = 0;
        CHECK_STATUS(
            execute_in_workspace(desc, req, workspace, bounded ? nullptr : &output.out, &detail),
            PGACCEL_OK);
        if (bounded) {
          auto next = desc;
          next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
          CHECK_STATUS(execute_in_workspace(next, req, workspace, nullptr, &detail), PGACCEL_OK);
          next.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
          next.row_count = 0;
          CHECK_STATUS(execute_in_workspace(next, req, workspace, &output.out, &detail),
                       PGACCEL_OK);
        }
        const uint64_t batches = bounded ? 2 : 1;
        CHECK(output.out.selected_count == selected_total * batches);
        for (size_t group = 0; group < groups; ++group) {
          CHECK(output.measures[carrier].count[group] == selected[group] * batches);
          CHECK(output.measures[1 - carrier].count[group] == present[group] * batches);
          CHECK(output.i64(output.measures[1 - carrier].sum, group) ==
                sums[group] * static_cast<int64_t>(batches));
        }
      }
    }
  }
}

void test_fused_scalar_errors_and_validation() {
  namespace s = pgaccel_scalar;
  SharedArray<int32_t> numerator({10, 20}), denominator({2, 0}), key({0, 0});
  SharedArray<int8_t> mask({1, 0});
  std::vector<int32_t> words{s::MAGIC, s::INT4, 2, s::INT4, s::INT4, 0, 0, 0, 1, 13, 0};
  pgaccel_scalar_input inputs[] = {{numerator.data(), nullptr, s::INT4, 0},
                                   {denominator.data(), nullptr, s::INT4, 0}};
  pgaccel_scalar_view view{1, uint32_t(words.size()), 2, 0, 2, words.data(), inputs};
  for (size_t groups : {size_t{1}, size_t{9}, size_t{17}, size_t{33}, size_t{73}, size_t{74}}) {
    auto desc = base_desc(2);
    if (groups > 1)
      set_fact_key(desc, 0, key.data(), nullptr, 0, groups);
    set_scalar_measure(desc, 0, view, PGACCEL_GROUPED_AGG_LANE_COUNT);
    set_count_star(desc, 1);
    for (bool where : {false, true}) {
      auto& filter = where ? desc.where_filter : desc.measure_filters[0];
      filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
      filter.mask = mask.data();
      OutputStorage output(desc);
      if (where) {
        CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
        CHECK(output.measures[0].count[0] == 1);
        CHECK(output.measures[1].count[0] == 1);
      } else {
        // Measure SQL-mask producers are not part of the qualified grammar.
        CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_UNSUPPORTED);
      }
      filter = disabled_filter();
    }
    // Independent raw RHS range is evaluated before the scalar program.
    desc.measures[0].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
    set_i32_view(desc.measures[0].rhs, denominator.data());
    auto& filter = desc.measure_filters[0];
    filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
    filter.predicate_measure_slot = 0;
    filter.predicate_range_count = 1;
    filter.predicate_lo[0] = filter.predicate_hi[0] = val_i32(2);
    OutputStorage filtered(desc);
    CHECK_STATUS(execute_external(desc, &filtered.out), PGACCEL_OK);
    CHECK(filtered.measures[0].count[0] == 1);
    filter = disabled_filter();
    desc.measures[0].flags = 0;
    desc.measures[0].rhs = {};
    int32_t detail = 0;
    OutputStorage failed(desc);
    CHECK_STATUS(execute_external_ex(desc, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO);
    CHECK(failed.measures[1].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
    numerator[1] = INT32_MIN;
    denominator[1] = -1;
    CHECK_STATUS(execute_external_ex(desc, &failed.out, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    numerator[1] = 20;
    denominator[1] = 0;
    // The bounded path must poison on an error, publish nothing, and recover
    // only after RESET. Sparse groups leave most lanes without rows.
    auto bounded = desc;
    bounded.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    int32_t bounded_mode = 0;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&bounded, &bounded_mode), PGACCEL_OK);
    CHECK(bounded_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
    const auto req = workspace_req(bounded);
    SharedWorkspace workspace(req.bytes, req.alignment);
    CHECK_STATUS(execute_in_workspace(bounded, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO);
    auto final = bounded;
    final.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
    final.row_count = 0;
    OutputStorage unpublished(final);
    CHECK_STATUS(execute_in_workspace(final, req, workspace, &unpublished.out, &detail), PGACCEL_ERROR);
    CHECK(unpublished.measures[1].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));
    numerator[1] = INT32_MIN;
    denominator[1] = -1;
    CHECK_STATUS(execute_in_workspace(bounded, req, workspace, nullptr, &detail), PGACCEL_ERROR);
    CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
    numerator[1] = 20;
    denominator[1] = 2;
    CHECK_STATUS(execute_in_workspace(bounded, req, workspace, nullptr, &detail), PGACCEL_OK);
    OutputStorage recovered(final);
    CHECK_STATUS(execute_in_workspace(final, req, workspace, &recovered.out, &detail), PGACCEL_OK);
    CHECK(recovered.measures[0].count[0] == 2);
    CHECK(recovered.measures[1].count[0] == 2);
    denominator[1] = 0;
    // Reject malformed metadata at workspace sizing, before device execution.
    const auto reject = [&] {
      pgaccel_status status;
      workspace_req(desc, &status);
      CHECK(status == PGACCEL_ERROR);
    };
    view.version = 2;
    reject();
    view.version = 1;
    view.row_count = 1;
    reject();
    view.row_count = 2;
    inputs[1].physical_type = s::BOOL;
    reject();
    inputs[1].physical_type = s::INT4;
    inputs[1].reserved = 1;
    reject();
    inputs[1].reserved = 0;
    words.back() = 1;
    reject();
    words.back() = 0;
  }
  // Literal NULL and BOOL COUNT must not use the raw-column non-NULL shortcut.
  for (int32_t type : {s::INT4, s::BOOL}) {
    for (bool is_null : {false, true}) {
      const int32_t constant[] = {s::MAGIC, type, 0, is_null ? 3 : (type == s::BOOL ? 2 : 1),
                                  is_null ? type : 1};
      const pgaccel_scalar_view literal{1, 5, 0, 0, 2, constant, nullptr};
      for (size_t groups : {size_t{1}, size_t{17}}) {
        auto desc = base_desc(2);
        if (groups > 1)
          set_fact_key(desc, 0, key.data(), nullptr, 0, groups);
        set_scalar_measure(desc, 0, literal, PGACCEL_GROUPED_AGG_LANE_COUNT);
        OutputStorage output(desc);
        CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
        CHECK(output.measures[0].count[0] == (is_null ? 0u : 2u));
      }
    }
  }
}

void test_fused_scalar_binding_and_recovery() {
  namespace s = pgaccel_scalar;
  SharedArray<int32_t> values({10, 20});
  SharedArray<uint8_t> nulls({0, 0});
  // Exercise maximum input metadata and all four states without a fixed-list
  // overflow in alias validation. Unused inputs must still be typed and USM.
  std::vector<int32_t> words{s::MAGIC, s::INT4, 64};
  words.insert(words.end(), 64, s::INT4);
  words.insert(words.end(), {0, 63});
  std::array<pgaccel_scalar_input, 64> inputs;
  inputs.fill({values.data(), nulls.data(), s::INT4, 0});
  pgaccel_scalar_view view{1, uint32_t(words.size()), 64, 0, 2, words.data(), inputs.data()};
  auto desc = base_desc(2);
  for (size_t slot = 0; slot < 4; ++slot)
    set_scalar_measure(desc, slot, view, PGACCEL_GROUPED_AGG_LANE_SUM);
  OutputStorage result(desc);
  CHECK_STATUS(execute_external(desc, &result.out), PGACCEL_OK);
  for (size_t slot = 0; slot < 4; ++slot)
    CHECK(result.i64(result.measures[slot].sum, 0) == 30);
  // Null scratch owns device-USM workspace: covers staged program copies.
  OutputStorage owned(desc);
  int32_t detail = 0;
  CHECK_STATUS(pgaccel_grouped_agg_execute_ex(&desc, &owned.out, &detail), PGACCEL_OK);
  check_outputs_equal(owned, result, 4);
  CHECK(pgacceltest_grouped_agg_active_scratch_owners() == 0);
  const int32_t host_values[] = {10, 20};
  inputs[0].values = host_values;
  OutputStorage invalid(desc);
  CHECK_STATUS(execute_external(desc, &invalid.out), PGACCEL_ERROR);
  inputs[0].values = values.data();
  nulls[0] = 2;
  CHECK_STATUS(execute_external_ex(desc, &invalid.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
  nulls[0] = 0;
  auto unsupported = desc;
  unsupported.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
  pgaccel_status status;
  workspace_req(unsupported, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);
  // No generic serial fallback when the ordered partial budget cannot fit.
  auto over_budget = desc;
  over_budget.row_count = 1'000'001;
  view.row_count = over_budget.row_count;
  set_fact_key(over_budget, 0, values.data(), nullptr, 0, 65536);
  workspace_req(over_budget, &status);  // metadata only; never reads row buffers
  CHECK(status == PGACCEL_UNSUPPORTED);
  view.row_count = desc.row_count;
  unsupported = desc;
  set_dim(unsupported, 0, values.data(), nullptr, 0, 21);
  workspace_req(unsupported, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);

  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  const auto req = workspace_req(desc);
  SharedWorkspace workspace(req.bytes, req.alignment);
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  // Same program length/type, different instruction: fingerprint must reject.
  words[words.size() - 2] = 1;
  auto finalize = desc;
  finalize.row_count = 0;
  finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &invalid.out, &detail),
               PGACCEL_ERROR);
  CHECK(invalid.measures[0].sum[0] == OutputStorage::kSentinel);
  words[words.size() - 2] = 0;
  // Explicit RESET recovers the failed lifecycle.
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  OutputStorage recovered(desc);
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &recovered.out, &detail), PGACCEL_OK);
  check_outputs_equal(recovered, result, 4);

  // A device expression failure must also poison accumulation until RESET.
  nulls[0] = 2;
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  nulls[0] = 0;
  auto accumulate = desc;
  accumulate.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  CHECK_STATUS(execute_in_workspace(accumulate, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_OK);
  OutputStorage reset_result(desc);
  CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &reset_result.out, &detail),
               PGACCEL_OK);
  check_outputs_equal(reset_result, result, 4);

  // Input/metadata/output may never alias mutable workspace.
  inputs[0].values = workspace.data();
  CHECK_STATUS(execute_in_workspace(desc, req, workspace, nullptr, &detail), PGACCEL_ERROR);
  inputs[0].values = values.data();
  OutputStorage alias(desc);
  alias.out.measures[0].sum = values.data();
  CHECK_STATUS(execute_external(desc, &alias.out), PGACCEL_ERROR);
  CHECK(values[0] == 10 && values[1] == 20);

  auto empty = desc;
  empty.row_count = 0;
  empty.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  OutputStorage empty_result(empty);
  CHECK_STATUS(execute_external(empty, &empty_result.out), PGACCEL_OK);
  CHECK(empty_result.active[0] == 1);
  CHECK(empty_result.measures[0].nonnull[0] == 0);

  // Lazy errors and noncanonical Boolean values in a fused COUNT expression.
  SharedArray<uint8_t> branches({0, 0});
  const int32_t lazy_words[] = {s::MAGIC, s::INT4, 1, s::BOOL, 0, 0,  51, 6, 1,
                                1,        1,       0, 13,      0, 50, 7,  1, 42};
  const pgaccel_scalar_input branch_input{branches.data(), nullptr, s::BOOL, 0};
  const pgaccel_scalar_view lazy{
      1, uint32_t(std::size(lazy_words)), 1, 0, 2, lazy_words, &branch_input};
  auto conditional = base_desc(2);
  set_scalar_measure(conditional, 0, lazy, PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage lazy_result(conditional);
  CHECK_STATUS(execute_external(conditional, &lazy_result.out), PGACCEL_OK);
  CHECK(lazy_result.measures[0].count[0] == 2);
  branches[0] = 1;
  OutputStorage lazy_failed(conditional);
  CHECK_STATUS(execute_external_ex(conditional, &lazy_failed.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO);
  branches[0] = 2;
  CHECK_STATUS(execute_external_ex(conditional, &lazy_failed.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_fused_integer_measure_order() {
  std::printf("--- fused integer row ownership and independent measures ---\n");
  constexpr size_t rows = 129;
  constexpr size_t groups = 17;
  std::array<std::vector<int32_t>, 3> host_values;
  std::array<std::vector<uint8_t>, 3> host_nulls;
  std::vector<int32_t> host_keys(rows);
  std::vector<int8_t> host_mask(rows);
  for (size_t col = 0; col < 3; ++col) {
    host_values[col].resize(rows);
    host_nulls[col].resize(rows);
    for (size_t row = 0; row < rows; ++row) {
      host_values[col][row] = int32_t((row * (col + 1)) % 23) - 11;
      host_nulls[col][row] = row % (col + 3) == 0;
      host_keys[row] = int32_t(row % groups);
      host_mask[row] = row % 11 != 0;
    }
  }
  SharedArray<int32_t> a(host_values[0]), b(host_values[1]), c(host_values[2]), keys(host_keys);
  SharedArray<uint8_t> an(host_nulls[0]), bn(host_nulls[1]), cn(host_nulls[2]);
  SharedArray<int8_t> mask(host_mask);
  const int32_t* values[] = {a.data(), b.data(), c.data()};
  const uint8_t* nulls[] = {an.data(), bn.data(), cn.data()};
  const int32_t ops[] = {PGACCEL_GROUPED_AGG_MEASURE_MUL, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                         PGACCEL_GROUPED_AGG_MEASURE_ADD};
  std::array<size_t, 3> order = {0, 1, 2};
  do {
    for (bool masked : {false, true}) {
      auto desc = base_desc(rows);
      set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
      for (size_t slot = 0; slot < 3; ++slot) {
        const size_t semantic = order[slot];
        set_i32_view(desc.measures[slot].value, values[semantic], nulls[semantic]);
        set_i32_view(desc.measures[slot].rhs, values[(semantic + 1) % 3],
                     nulls[(semantic + 1) % 3]);
        finish_i64_measure(desc, slot, ops[semantic],
                           PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT |
                               PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX);
        if (semantic != 1) {
          auto& filter = desc.measure_filters[slot];
          filter.predicate_measure_slot = int32_t(slot);
          filter.predicate_range_count = 1;
          filter.predicate_lo[0] = val_i32(-3);
          filter.predicate_hi[0] = val_i32(3);
        }
      }
      set_count_star(desc, 3);
      if (masked) {
        desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.where_filter.mask = mask.data();
      }
      OutputStorage output(desc);
      CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
      uint64_t total = 0;
      for (size_t group = 0; group < groups; ++group) {
        uint64_t selected = 0;
        for (size_t row = group; row < rows; row += groups)
          selected += !masked || host_mask[row];
        total += selected;
        CHECK(output.measures[3].count[group] == selected);
        CHECK(output.active[group] == uint8_t(selected != 0));
        for (size_t slot = 0; slot < 3; ++slot) {
          const size_t semantic = order[slot], rhs = (semantic + 1) % 3;
          int64_t sum = 0, lo = INT64_MAX, hi = INT64_MIN;
          uint64_t nonnull = 0;
          for (size_t row = group; row < rows; row += groups) {
            if ((masked && !host_mask[row]) || host_nulls[semantic][row] || host_nulls[rhs][row])
              continue;
            const int64_t lhs_value = host_values[semantic][row], rhs_value = host_values[rhs][row];
            if (semantic != 1 && (lhs_value < -3 || lhs_value > 3))
              continue;
            const int64_t value = semantic == 0   ? lhs_value * rhs_value
                                  : semantic == 1 ? lhs_value - rhs_value
                                                  : lhs_value + rhs_value;
            sum += value;
            lo = std::min(lo, value);
            hi = std::max(hi, value);
            ++nonnull;
          }
          CHECK(output.i64(output.measures[slot].sum, group) == sum);
          CHECK(output.measures[slot].count[group] == nonnull);
          CHECK(output.measures[slot].nonnull[group] == nonnull);
          if (nonnull) {
            CHECK(output.i64(output.measures[slot].min, group) == lo);
            CHECK(output.i64(output.measures[slot].max, group) == hi);
          }
        }
      }
      CHECK(output.out.selected_count == total);
    }
  } while (std::next_permutation(order.begin(), order.end()));

  // A NULL first state and rejecting second FILTER must not suppress an
  // overflow in a later COUNT expression or publish any partial output.
  SharedArray<int32_t> endpoint({INT32_MAX}), one({1}), key({0});
  SharedArray<uint8_t> absent({1});
  auto failed = base_desc(1);
  set_fact_key(failed, 0, key.data(), nullptr, 0, groups);
  set_i32_view(failed.measures[0].value, one.data(), absent.data());
  finish_i64_measure(failed, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_i32_view(failed.measures[1].value, one.data());
  finish_i64_measure(failed, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_SUM);
  failed.measure_filters[1].predicate_measure_slot = 1;
  failed.measure_filters[1].predicate_range_count = 1;
  failed.measure_filters[1].predicate_lo[0] = failed.measure_filters[1].predicate_hi[0] =
      val_i32(0);
  set_i32_view(failed.measures[2].value, endpoint.data());
  set_i32_view(failed.measures[2].rhs, one.data());
  finish_i64_measure(failed, 2, PGACCEL_GROUPED_AGG_MEASURE_ADD, PGACCEL_GROUPED_AGG_LANE_COUNT);
  set_count_star(failed, 3);
  OutputStorage unpublished(failed);
  int32_t detail = 0;
  CHECK_STATUS(execute_external_ex(failed, &unpublished.out, &detail), PGACCEL_ERROR);
  CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
  CHECK(unpublished.measures[3].count == std::vector<uint64_t>(groups, OutputStorage::kSentinel));

  // Maximum one-shot skew: every row contends on the same group. Counts fit
  // one word, but SUM must still carry across words and remain exact. Follow
  // with one extra row to exercise the unchanged bounded reducer at the cap.
  for (size_t count_rows : {size_t{1'000'000}, size_t{1'000'001}}) {
    SharedArray<int32_t> skew_values(std::vector<int32_t>(count_rows, INT32_MAX));
    SharedArray<int32_t> skew_keys(std::vector<int32_t>(count_rows, 0));
    std::vector<uint8_t> host_skew_nulls(count_rows);
    for (size_t row = 0; row < count_rows; ++row)
      host_skew_nulls[row] = row % 2;
    SharedArray<uint8_t> skew_nulls(host_skew_nulls);
    auto skew = base_desc(count_rows);
    set_fact_key(skew, 0, skew_keys.data(), nullptr, 0, groups);
    set_i32_view(skew.measures[0].value, skew_values.data(), skew_nulls.data());
    finish_i64_measure(skew, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                       PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
    set_i32_view(skew.measures[1].value, skew_values.data());
    finish_i64_measure(skew, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN, PGACCEL_GROUPED_AGG_LANE_COUNT);
    set_count_star(skew, 2);
    OutputStorage skew_output(skew);
    CHECK_STATUS(execute_external(skew, &skew_output.out), PGACCEL_OK);
    const size_t present = (count_rows + 1) / 2;
    CHECK(skew_output.out.selected_count == count_rows);
    CHECK(skew_output.measures[2].count[0] == count_rows);
    CHECK(skew_output.measures[1].count[0] == count_rows);
    CHECK(skew_output.measures[0].count[0] == present);
    CHECK(skew_output.i64(skew_output.measures[0].sum, 0) == int64_t(present) * INT32_MAX);
    for (size_t group = 1; group < groups; ++group) {
      CHECK(skew_output.active[group] == 0);
      CHECK(skew_output.measures[2].count[group] == 0);
      CHECK(skew_output.measures[0].count[group] == 0);
    }
  }
}

void test_parallel_computed_integer_measures() {
  std::printf("--- checked computed INT4 measures ---\n");
  SharedArray<int32_t> a({-12, 1000, -1000, 17, 0, 100, -200, 99, 13});
  SharedArray<int32_t> b({2, -7, 9, 0, 3, 10, -10, 8, 5});
  SharedArray<uint8_t> an({0, 1, 0, 0, 0, 0, 0, 0, 0});
  SharedArray<uint8_t> bn({0, 0, 1, 0, 0, 0, 0, 0, 0});
  SharedArray<int32_t> keys({0, 0, 1, 1, 2, 2, 0, 1, 2});
  SharedArray<int32_t> p({0, 0, 1, 0, 2, 3, 2, 1, 0});
  SharedArray<uint8_t> pn({0, 0, 0, 1, 0, 0, 0, 0, 0});
  for (size_t capacity : {size_t{1}, size_t{4}, size_t{17}}) {
    for (int32_t op : {PGACCEL_GROUPED_AGG_MEASURE_ADD, PGACCEL_GROUPED_AGG_MEASURE_SUB,
                       PGACCEL_GROUPED_AGG_MEASURE_MUL}) {
      for (unsigned lanes = 1; lanes < 16; ++lanes) {
        const uint32_t mask = ((lanes & 1) ? PGACCEL_GROUPED_AGG_LANE_SUM : 0) |
                              ((lanes & 2) ? PGACCEL_GROUPED_AGG_LANE_COUNT : 0) |
                              ((lanes & 4) ? PGACCEL_GROUPED_AGG_LANE_MIN : 0) |
                              ((lanes & 8) ? PGACCEL_GROUPED_AGG_LANE_MAX : 0);
        for (bool restricted : {false, true}) {
          auto desc = base_desc(a.size());
          if (capacity != 1)
            set_fact_key(desc, 0, keys.data(), nullptr, 0, capacity);
          set_i32_view(desc.measures[0].value, a.data(), an.data());
          set_i32_view(desc.measures[0].rhs, b.data(), bn.data());
          finish_i64_measure(desc, 0, op, mask);
          set_i32_view(desc.measures[1].value, b.data(), bn.data());
          set_i32_view(desc.measures[1].rhs, a.data(), an.data());
          finish_i64_measure(desc, 1,
                             op == PGACCEL_GROUPED_AGG_MEASURE_ADD
                                 ? PGACCEL_GROUPED_AGG_MEASURE_MUL
                                 : PGACCEL_GROUPED_AGG_MEASURE_ADD,
                             PGACCEL_GROUPED_AGG_LANE_COUNT);
          set_i32_view(desc.measures[2].value, p.data(), pn.data());
          finish_i64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                             PGACCEL_GROUPED_AGG_LANE_COUNT);
          set_count_star(desc, 3);
          for (size_t slot = 0; slot < 2; ++slot) {
            auto& filter = desc.measure_filters[slot];
            filter.predicate_measure_slot = 2;
            filter.predicate_range_count = 1;
            filter.predicate_lo[0] = val_i32(slot == 0 ? 0 : 2);
            filter.predicate_hi[0] = val_i32(slot == 0 ? 1 : 3);
            filter.flags = slot == 0 ? PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL : 0;
          }
          if (restricted) {
            desc.where_filter.flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
            desc.where_filter.predicate_measure_slot = 2;
            desc.where_filter.predicate_range_count = 1;
            desc.where_filter.predicate_lo[0] = desc.where_filter.predicate_hi[0] = val_i32(0);
          }
          int32_t mode = -1;
          CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
          CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER);
          OutputStorage output(desc);
          CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
          uint64_t total_selected = 0;
          for (size_t group = 0; group < capacity; ++group) {
            int64_t sum = 0, lo = INT64_MAX, hi = INT64_MIN;
            uint64_t selected = 0, nonnull = 0, second_count = 0, predicate_count = 0;
            for (size_t row = 0; row < a.size(); ++row) {
              if ((capacity != 1 && size_t(keys[row]) != group) ||
                  (restricted && !pn[row] && p[row] != 0))
                continue;
              ++selected;
              predicate_count += !pn[row];
              if (an[row] || bn[row])
                continue;
              second_count += !pn[row] && p[row] >= 2 && p[row] <= 3;
              if (!pn[row] && (p[row] < 0 || p[row] > 1))
                continue;
              const int64_t value = op == PGACCEL_GROUPED_AGG_MEASURE_ADD ? int64_t(a[row]) + b[row]
                                    : op == PGACCEL_GROUPED_AGG_MEASURE_SUB
                                        ? int64_t(a[row]) - b[row]
                                        : int64_t(a[row]) * b[row];
              sum += value;
              lo = std::min(lo, value);
              hi = std::max(hi, value);
              ++nonnull;
            }
            total_selected += selected;
            CHECK(output.active[group] == uint8_t(capacity == 1 || selected != 0));
            CHECK(output.measures[0].nonnull[group] == nonnull);
            if (mask & PGACCEL_GROUPED_AGG_LANE_SUM)
              CHECK(output.i64(output.measures[0].sum, group) == sum);
            if (mask & PGACCEL_GROUPED_AGG_LANE_COUNT)
              CHECK(output.measures[0].count[group] == nonnull);
            if (nonnull && (mask & PGACCEL_GROUPED_AGG_LANE_MIN))
              CHECK(output.i64(output.measures[0].min, group) == lo);
            if (nonnull && (mask & PGACCEL_GROUPED_AGG_LANE_MAX))
              CHECK(output.i64(output.measures[0].max, group) == hi);
            CHECK(output.measures[1].count[group] == second_count);
            CHECK(output.measures[2].count[group] == predicate_count);
            CHECK(output.measures[3].count[group] == selected);
          }
          CHECK(output.out.selected_count == total_selected);
          if (lanes == 15) {
            auto lifecycle = desc;
            lifecycle.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
            const auto req = workspace_req(lifecycle);
            SharedWorkspace workspace(req.bytes, req.alignment);
            int32_t detail = 0;
            auto reset = row_slice(desc, 0, 0);
            reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
            CHECK_STATUS(execute_in_workspace(reset, req, workspace, nullptr, &detail), PGACCEL_OK);
            for (size_t row = 0; row < a.size(); ++row) {
              auto chunk = row_slice(desc, row, 1);
              chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
              CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail),
                           PGACCEL_OK);
            }
            auto finish = row_slice(desc, 0, 0);
            finish.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
            OutputStorage chunked(finish);
            CHECK_STATUS(execute_in_workspace(finish, req, workspace, &chunked.out, &detail),
                         PGACCEL_OK);
            check_outputs_equal(output, chunked, desc.measure_count);
          }
        }
      }
      SharedArray<int32_t> bad_a({op == PGACCEL_GROUPED_AGG_MEASURE_ADD ? INT32_MAX : INT32_MIN});
      SharedArray<int32_t> bad_b({op == PGACCEL_GROUPED_AGG_MEASURE_MUL ? -1 : 1});
      SharedArray<int32_t> key({0});
      SharedArray<uint8_t> rhs_null({1});
      auto bad = base_desc(1);
      if (capacity != 1)
        set_fact_key(bad, 0, key.data(), nullptr, 0, capacity);
      set_i32_view(bad.measures[0].value, bad_a.data());
      set_i32_view(bad.measures[0].rhs, bad_b.data());
      finish_i64_measure(bad, 0, op, PGACCEL_GROUPED_AGG_LANE_COUNT);
      set_count_star(bad, 1);
      OutputStorage failed(bad);
      int32_t detail = 0;
      CHECK_STATUS(execute_external_ex(bad, &failed.out, &detail), PGACCEL_ERROR);
      CHECK(detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
      CHECK(failed.measures[0].count == std::vector<uint64_t>(capacity, OutputStorage::kSentinel));
      bad.measures[0].rhs.nulls = rhs_null.data();
      OutputStorage nullable(bad);
      CHECK_STATUS(execute_external(bad, &nullable.out), PGACCEL_OK);
      CHECK(nullable.measures[0].count[0] == 0 && nullable.measures[1].count[0] == 1);
      bad.measures[0].rhs.nulls = nullptr;
      bad.measure_filters[0].predicate_range_count = 1;
      bad.measure_filters[0].predicate_lo[0] = bad.measure_filters[0].predicate_hi[0] = val_i32(0);
      OutputStorage rejected(bad);
      CHECK_STATUS(execute_external(bad, &rejected.out), PGACCEL_OK);
      CHECK(rejected.measures[0].count[0] == 0 && rejected.measures[1].count[0] == 1);
    }
  }
}

void test_null_accepting_ranges() {
  SharedArray<int32_t> values({10, -20, INT32_MIN, INT32_MAX, 7, 8, 9, 10});
  SharedArray<int64_t> wide_values({10, -20, INT32_MIN, INT32_MAX, 7, 8, 9, 10});
  SharedArray<uint8_t> value_nulls({0, 1, 0, 0, 1, 0, 0, 0});
  SharedArray<int32_t> groups({0, 0, 1, 1, 2, 2, 3, 3});
  SharedArray<int32_t> predicates({0, 1, 0, 2, 1, 2, 0, 3});
  SharedArray<uint8_t> predicate_nulls({1, 0, 1, 0, 1, 0, 0, 0});
  for (bool decimal : {false, true}) {
    for (size_t capacity : {size_t{1}, size_t{4}, size_t{17}}) {
      for (bool null_only : {false, true}) {
        for (int where_mode = 0; where_mode < 3; ++where_mode) {
          auto desc = base_desc(values.size());
          if (capacity != 1)
            set_fact_key(desc, 0, groups.data(), nullptr, 0, capacity);
          finish_i64_measure(
              desc, 0, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
              PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT |
                  (decimal ? 0 : PGACCEL_GROUPED_AGG_LANE_MIN | PGACCEL_GROUPED_AGG_LANE_MAX));
          set_i32_view(desc.measures[0].value, values.data(), value_nulls.data());
          if (decimal) {
            set_i64_view(desc.measures[0].value, wide_values.data(), value_nulls.data());
            desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
            desc.measures[0].state_bytes = PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES;
          }
          // Filtered COUNT(*) uses a count-only column with no value NULL
          // sidecar, while its independent RHS keeps predicate NULL semantics.
          finish_i64_measure(desc, 1, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                             PGACCEL_GROUPED_AGG_LANE_COUNT);
          set_i32_view(desc.measures[1].value, predicates.data(), nullptr);
          for (size_t slot = 0; slot < 2; ++slot) {
            desc.measures[slot].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
            set_i32_view(desc.measures[slot].rhs, predicates.data(), predicate_nulls.data());
            auto& filter = desc.measure_filters[slot];
            filter.flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
            filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
            filter.predicate_measure_slot = slot;
            if (!null_only) {
              filter.predicate_range_count = 1;
              filter.predicate_lo[0] = val_i32(1);
              filter.predicate_hi[0] = val_i32(2);
            }
          }
          finish_i64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                             PGACCEL_GROUPED_AGG_LANE_COUNT);
          set_i32_view(desc.measures[2].value, predicates.data(), predicate_nulls.data());
          set_count_star(desc, 3);
          if (where_mode != 0) {
            desc.where_filter.flags = PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
            desc.where_filter.predicate_measure_slot = 2;
            if (where_mode == 2) {
              desc.where_filter.predicate_range_count = 1;
              desc.where_filter.predicate_lo[0] = desc.where_filter.predicate_hi[0] = val_i32(0);
            }
          }
          OutputStorage output(desc);
          CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
          int32_t mode = -1;
          CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
          CHECK(mode == (decimal ? PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC
                                 : PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER));
          for (size_t group = 0; group < capacity; ++group) {
            int64_t sum = 0, lo = INT64_MAX, hi = INT64_MIN;
            uint64_t selected = 0, counted = 0, nonnull = 0, present_predicates = 0;
            for (size_t row = 0; row < values.size(); ++row) {
              if ((capacity != 1 && size_t(groups[row]) != group) ||
                  (where_mode != 0 && !predicate_nulls[row] &&
                   (where_mode == 1 || predicates[row] != 0)))
                continue;
              ++selected;
              present_predicates += !predicate_nulls[row];
              if (!predicate_nulls[row] &&
                  (null_only || predicates[row] < 1 || predicates[row] > 2))
                continue;
              ++counted;
              if (value_nulls[row])
                continue;
              ++nonnull;
              sum += values[row];
              lo = std::min(lo, int64_t(values[row]));
              hi = std::max(hi, int64_t(values[row]));
            }
            CHECK(output.active[group] == uint8_t(capacity == 1 || selected != 0));
            CHECK(output.measures[1].count[group] == counted);
            CHECK(output.measures[2].count[group] == present_predicates);
            CHECK(output.measures[3].count[group] == selected);
            if (decimal) {
              auto magnitude = sum < 0 ? -sum : sum;
              std::vector<uint16_t> digits;
              while (magnitude) {
                digits.push_back(magnitude % 10000);
                magnitude /= 10000;
              }
              const int32_t weight = digits.empty() ? 0 : int32_t(digits.size() - 1);
              std::reverse(digits.begin(), digits.end());
              while (!digits.empty() && digits.back() == 0)
                digits.pop_back();
              check_numeric(desc, output, 0, group, weight, sum < 0 ? 0x4000 : 0, 0, digits,
                            nonnull);
            } else {
              CHECK(output.measures[0].sum[group] == uint64_t(sum));
              CHECK(output.measures[0].count[group] == nonnull);
              if (nonnull) {
                CHECK(output.measures[0].min[group] == uint64_t(lo));
                CHECK(output.measures[0].max[group] == uint64_t(hi));
              }
            }
          }
          auto lifecycle = desc;
          lifecycle.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
          const auto req = workspace_req(lifecycle);
          SharedWorkspace workspace(req.bytes, req.alignment);
          int32_t detail = 0;
          auto reset = row_slice(desc, 0, 0);
          reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
          CHECK_STATUS(execute_in_workspace(reset, req, workspace, nullptr, &detail), PGACCEL_OK);
          for (size_t row = 0; row < values.size(); ++row) {
            auto chunk = row_slice(desc, row, 1);
            chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
            CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
          }
          auto finalize = row_slice(desc, 0, 0);
          finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
          OutputStorage chunked(finalize);
          CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail),
                       PGACCEL_OK);
          check_outputs_equal(chunked, output, 4);
          auto bad = desc;
          bad.measure_filters[0].flags = 2;
          pgaccel_status status = PGACCEL_OK;
          workspace_req(bad, &status);
          CHECK_STATUS(status, PGACCEL_ERROR);
        }
      }
    }
  }
}

void test_bigint_numeric_states() {
  // The independent host oracle uses 128 bits; device input stays raw INT64
  // and must survive sums beyond INT64, including abs(INT64_MIN).
  const std::vector<int64_t> host = {
      INT64_MAX, INT64_MAX, INT64_MIN, INT64_MIN, INT64_MAX, INT64_MIN, 123, -456, 0, 0, -10000, 0};
  SharedArray<int64_t> values(host);
  SharedArray<int32_t> groups({0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5});
  SharedArray<uint8_t> nulls({0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0});
  SharedArray<int32_t> predicates({1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0});
  NumericFixture decimal(std::vector<pgaccel_numeric_row>(host.size(), {0, 1, 0, 0, 0}), {7}, 0, 0,
                         1);
  for (bool grouped : {false, true}) {
    auto desc = base_desc(host.size());
    if (grouped)
      set_fact_key(desc, 0, groups.data(), nullptr, 0, 6);
    for (size_t slot = 0; slot < 2; ++slot) {
      finish_i64_measure(desc, slot, PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                         PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_COUNT);
      auto& measure = desc.measures[slot];
      set_i64_view(measure.value, values.data(), nulls.data());
      measure.accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
      measure.state_bytes = PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES;
    }
    desc.measures[1].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
    set_i32_view(desc.measures[1].rhs, predicates.data(), nullptr);
    auto& filter = desc.measure_filters[1];
    filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
    filter.predicate_measure_slot = 1;
    filter.predicate_range_count = 1;
    filter.predicate_lo[0] = filter.predicate_hi[0] = val_i32(1);
    decimal.view.rows = decimal.rows.data();
    decimal.view.row_count = host.size();
    decimal.bind(desc, 2);
    set_count_star(desc, 3);
    OutputStorage output(desc);
    CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
    int32_t mode = -1;
    CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
    CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC);
    for (size_t group = 0; group < (grouped ? 6u : 1u); ++group) {
      for (size_t slot = 0; slot < 2; ++slot) {
        __int128 sum = 0;
        uint64_t count = 0;
        for (size_t row = 0; row < host.size(); ++row) {
          if ((grouped && size_t(groups[row]) != group) || nulls[row] ||
              (slot == 1 && predicates[row] != 1))
            continue;
          sum += host[row];
          ++count;
        }
        const uint32_t sign = sum < 0 ? 0x4000 : 0;
        auto magnitude = sum < 0 ? -sum : sum;
        std::vector<uint16_t> digits;
        while (magnitude) {
          digits.push_back(uint16_t(magnitude % 10000));
          magnitude /= 10000;
        }
        const int32_t weight = digits.empty() ? 0 : int32_t(digits.size() - 1);
        std::reverse(digits.begin(), digits.end());
        while (!digits.empty() && digits.back() == 0)
          digits.pop_back();
        check_numeric(desc, output, slot, group, weight, sign, 0, digits, count);
      }
      check_numeric(desc, output, 2, group, 0, 0, 0, {uint16_t(grouped ? 14 : 84)},
                    grouped ? 2 : 12);
    }
    auto lifecycle = desc;
    lifecycle.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    const auto req = workspace_req(lifecycle);
    SharedWorkspace workspace(req.bytes, req.alignment);
    int32_t detail = 0;
    for (int repeat = 0; repeat < 2; ++repeat) {
      auto reset = row_slice(desc, 0, 0);
      decimal.view.row_count = 0;
      reset.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET;
      CHECK_STATUS(execute_in_workspace(reset, req, workspace, nullptr, &detail), PGACCEL_OK);
      for (size_t row = 0; row < host.size(); ++row) {
        auto chunk = row_slice(desc, row, 1);
        // Packed decimals carry a host metadata header; row_slice advances
        // raw buffers, so restore that header and slice its rows explicitly.
        chunk.measures[2].value.values = &decimal.view;
        decimal.view.rows = decimal.rows.data() + row;
        decimal.view.row_count = 1;
        chunk.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        CHECK_STATUS(execute_in_workspace(chunk, req, workspace, nullptr, &detail), PGACCEL_OK);
      }
      auto finalize = row_slice(desc, 0, 0);
      decimal.view.row_count = 0;
      finalize.execution_flags = PGACCEL_GROUPED_AGG_EXEC_FINALIZE;
      OutputStorage chunked(finalize);
      CHECK_STATUS(execute_in_workspace(finalize, req, workspace, &chunked.out, &detail),
                   PGACCEL_OK);
      for (size_t slot = 0; slot < 4; ++slot) {
        CHECK(chunked.measures[slot].sum == output.measures[slot].sum);
        CHECK(chunked.measures[slot].count == output.measures[slot].count);
        CHECK(chunked.measures[slot].nonnull == output.measures[slot].nonnull);
      }
    }
    decimal.view.rows = decimal.rows.data();
    decimal.view.row_count = host.size();
    auto malformed = desc;
    malformed.measures[0].state_bytes = 8;
    pgaccel_status status = PGACCEL_ERROR;
    workspace_req(malformed, &status);
    CHECK_STATUS(status, PGACCEL_ERROR);
  }
}

void test_numeric_independent_counts() {
  // The first two values overflow NUMERIC SUM together. COUNT must only test
  // NULLness, including NaN/Infinity, and never read/accumulate these digits.
  NumericFixture huge({{0, 1, 32767, 0, 0},
                       {1, 1, 32767, 0, 0},
                       {0, 0, 0, 0xc000, 0},
                       {0, 0, 0, 0xd000, 0},
                       {0, 1, 32767, 0, 0}},
                      {9999, 9999}, 32767, 32767, 1, {0, 0, 0, 0, 1});
  NumericFixture small(
      {{0, 1, 0, 0, 0}, {1, 1, 0, 0, 0}, {2, 1, 0, 0, 0}, {3, 1, 0, 0, 0}, {4, 1, 0, 0, 0}},
      {1, 2, 3, 4, 5}, 0, 0, 1);
  SharedArray<int32_t> groups({0, 0, 1, 1, 2});
  SharedArray<int32_t> predicates({1, 0, 1, 1, 1});
  auto desc = base_desc(5);
  set_fact_key(desc, 0, groups.data(), nullptr, 0, 3);
  for (size_t slot = 0; slot < 2; ++slot) {
    huge.bind(desc, slot);
    desc.measures[slot].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
    desc.measures[slot].state_bytes = sizeof(uint64_t);
  }
  desc.measures[1].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
  set_i32_view(desc.measures[1].rhs, predicates.data(), nullptr);
  auto& filter = desc.measure_filters[1];
  filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
  filter.predicate_measure_slot = 1;
  filter.predicate_range_count = 1;
  filter.predicate_lo[0] = val_i32(1);
  filter.predicate_hi[0] = val_i32(1);
  small.bind(desc, 2);
  set_count_star(desc, 3);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.measures[0].count == std::vector<uint64_t>({2, 2, 0}));
  CHECK(output.measures[1].count == std::vector<uint64_t>({1, 2, 0}));
  CHECK(output.measures[0].sum.empty());
  CHECK(output.measures[1].sum.empty());
  check_numeric(desc, output, 2, 0, 0, 0, 0, {3}, 2);
  check_numeric(desc, output, 2, 1, 0, 0, 0, {7}, 2);
  check_numeric(desc, output, 2, 2, 0, 0, 0, {5}, 1);
  CHECK(output.measures[3].count == std::vector<uint64_t>({2, 2, 1}));
  // COUNT-only execution needs no SUM workspace, even with full digit bounds.
  desc.measure_count = 2;
  desc.measures[2] = {};
  desc.measures[3] = {};
  OutputStorage counts(desc);
  CHECK_STATUS(execute_external(desc, &counts.out), PGACCEL_OK);
  CHECK(counts.measures[0].count == output.measures[0].count);
  CHECK(counts.measures[1].count == output.measures[1].count);
  pgaccel_status status = PGACCEL_ERROR;
  const auto narrow = workspace_req(desc, &status);
  CHECK(status == PGACCEL_OK && narrow.bytes > 0);
  huge.view.min_weight = -4096;
  huge.view.max_weight = 32767;
  huge.view.max_ndigits = 36864;
  const auto wide = workspace_req(desc, &status);
  CHECK(status == PGACCEL_OK && wide.bytes > 0);
  CHECK(wide.bytes == narrow.bytes);
  OutputStorage wide_counts(desc);
  CHECK_STATUS(execute_external(desc, &wide_counts.out), PGACCEL_OK);
  CHECK(wide_counts.measures[0].count == output.measures[0].count);
  int32_t mode = -1;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
  CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC);
  huge.null_flags[0] = 2;
  OutputStorage invalid_counts(desc);
  CHECK_STATUS(execute_external(desc, &invalid_counts.out), PGACCEL_ERROR);
}

void test_numeric_independent_filters() {
  NumericFixture values({{0, 1, 0, 0, 0},
                         {1, 1, 0, 0, 0},
                         {0, 0, 0, 0xc000, 0},
                         {2, 1, 0, 0, 0},
                         {3, 1, 0, 0, 0},
                         {4, 1, 0, 0, 0},
                         {0, 0, 0, 0xd000, 0},
                         {5, 1, 0, 0, 0}},
                        {1, 2, 4, 5, 6, 8}, 0, 0, 1, {0, 0, 0, 0, 1, 0, 0, 0});
  SharedArray<int32_t> groups({0, 0, 0, 1, 1, 2, 2, 3});
  SharedArray<int32_t> first({1, 2, 0, 1, 2, 1, 0, 0});
  SharedArray<int32_t> second({-1, 1, 0, 1, 1, 1, 0, 0});
  SharedArray<uint8_t> first_nulls({0, 0, 0, 1, 0, 0, 0, 0});
  auto desc = base_desc(8);
  set_fact_key(desc, 0, groups.data(), nullptr, 0, 4);
  for (uint32_t slot = 0; slot < 2; ++slot) {
    values.bind(desc, slot);
    desc.measures[slot].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
    set_i32_view(desc.measures[slot].rhs, slot == 0 ? first.data() : second.data(),
                 slot == 0 ? first_nulls.data() : nullptr);
    auto& filter = desc.measure_filters[slot];
    filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
    filter.predicate_measure_slot = slot;
    filter.predicate_range_count = 1;
    filter.predicate_lo[0] = val_i32(1);
    filter.predicate_hi[0] = val_i32(1);
  }
  finish_i64_measure(desc, 2, PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
                     PGACCEL_GROUPED_AGG_LANE_COUNT);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == 8);
  CHECK(output.out.emitted_group_count == 4);
  check_numeric(desc, output, 0, 0, 0, 0, 0, {1}, 1);
  check_numeric(desc, output, 1, 0, 0, 0, 0, {2}, 1);
  check_numeric(desc, output, 0, 1, 0, 0, 0, {}, 0);
  check_numeric(desc, output, 1, 1, 0, 0, 0, {4}, 1);
  for (size_t slot = 0; slot < 2; ++slot) {
    check_numeric(desc, output, slot, 2, 0, 0, 0, {6}, 1);
    check_numeric(desc, output, slot, 3, 0, 0, 0, {}, 0);
  }
  CHECK(output.measures[2].count == std::vector<uint64_t>({3, 2, 2, 1}));
  int32_t mode = -1;
  CHECK_STATUS(pgaccel_grouped_agg_kernel_mode(&desc, &mode), PGACCEL_OK);
  CHECK(mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC);
  {
    auto mixed = desc;
    SharedArray<int32_t> predicates({INT32_MIN, 1, 0, 2, 1, 1, 0, INT32_MAX});
    set_i32_view(mixed.measures[1].rhs, predicates.data(), nullptr);
    auto& filter = mixed.measure_filters[1];
    filter.predicate_range_count = 3;
    filter.predicate_lo[0] = val_i32(INT32_MIN);
    filter.predicate_hi[0] = val_i32(INT32_MIN);
    filter.predicate_lo[1] = val_i32(1);
    filter.predicate_hi[1] = val_i32(2);
    filter.predicate_lo[2] = val_i32(INT32_MAX);
    filter.predicate_hi[2] = val_i32(INT32_MAX);
    OutputStorage mixed_output(mixed);
    CHECK_STATUS(execute_external(mixed, &mixed_output.out), PGACCEL_OK);
    check_numeric(mixed, mixed_output, 1, 0, 0, 0, 0, {3}, 2);
    check_numeric(mixed, mixed_output, 1, 1, 0, 0, 0, {4}, 1);
    check_numeric(mixed, mixed_output, 1, 2, 0, 0, 0, {6}, 1);
    check_numeric(mixed, mixed_output, 1, 3, 0, 0, 0, {8}, 1);
    CHECK(mixed_output.measures[2].count == std::vector<uint64_t>({3, 2, 2, 1}));
  }
  pgaccel_status status = PGACCEL_OK;
  auto invalid = desc;
  invalid.measures[0].flags = 0;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_ERROR);
  invalid = desc;
  invalid.measure_filters[0].predicate_measure_slot = 1;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_UNSUPPORTED);
  invalid = desc;
  invalid.measures[0].flags = 2;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_ERROR);
  invalid = desc;
  invalid.measures[0].rhs.values = nullptr;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_ERROR);
  invalid = desc;
  invalid.measures[0].rhs.element_bytes = 8;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_ERROR);
  invalid = desc;
  invalid.measure_filters[0].predicate_range_count = 0;
  workspace_req(invalid, &status);
  CHECK(status != PGACCEL_OK);
  invalid = desc;
  invalid.measures[2].flags = PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
  workspace_req(invalid, &status);
  CHECK(status == PGACCEL_ERROR);
  first_nulls[0] = 2;
  OutputStorage invalid_output(desc);
  CHECK_STATUS(execute_external(desc, &invalid_output.out), PGACCEL_ERROR);
}

void test_numeric_large_reference() {
  // A deterministic integer oracle at scale 4 exercises contended sharded
  // buckets, positive/negative carries, NULLs, and multiple launch chunks.
  constexpr size_t rows = 1000017;
  constexpr size_t groups = 17;
  std::vector<pgaccel_numeric_row> headers;
  std::vector<uint16_t> digits;
  std::vector<uint8_t> nulls;
  std::vector<int32_t> codes;
  std::array<int64_t, groups> sums{};
  std::array<uint64_t, groups> counts{};
  uint64_t random = 0x876543210abcdefULL;
  for (size_t row = 0; row < rows; ++row) {
    random = random * 6364136223846793005ULL + 1442695040888963407ULL;
    const int64_t value = static_cast<int64_t>((random >> 16) % 199999999) - 99999999;
    const uint64_t magnitude = value < 0 ? -value : value;
    const size_t group = row % groups;
    const bool is_null = row % 23 == 0;
    headers.push_back({digits.size(), 2, 0, static_cast<uint16_t>(value < 0 ? 0x4000 : 0), 4});
    digits.push_back(static_cast<uint16_t>(magnitude / 10000));
    digits.push_back(static_cast<uint16_t>(magnitude % 10000));
    codes.push_back(static_cast<int32_t>(group));
    nulls.push_back(is_null);
    if (!is_null) {
      sums[group] += value;
      ++counts[group];
    }
  }
  NumericFixture values(headers, digits, -1, 0, 2, nulls);
  SharedArray<int32_t> keys(codes);
  auto desc = base_desc(rows);
  set_fact_key(desc, 0, keys.data(), nullptr, 0, groups);
  values.bind(desc, 0);
  set_count_star(desc, 1);
  OutputStorage output(desc);
  CHECK_STATUS(execute_external(desc, &output.out), PGACCEL_OK);
  CHECK(output.out.selected_count == rows);
  CHECK(output.out.emitted_group_count == groups);
  for (size_t group = 0; group < groups; ++group) {
    uint64_t magnitude = sums[group] < 0 ? -sums[group] : sums[group];
    std::vector<uint16_t> expected;
    while (magnitude != 0) {
      expected.push_back(static_cast<uint16_t>(magnitude % 10000));
      magnitude /= 10000;
    }
    const int32_t weight = expected.empty() ? 0 : static_cast<int32_t>(expected.size()) - 2;
    std::reverse(expected.begin(), expected.end());
    while (!expected.empty() && expected.back() == 0)
      expected.pop_back();
    check_numeric(desc, output, 0, group, weight, sums[group] < 0 ? 0x4000 : 0, 4, expected,
                  counts[group]);
    CHECK(output.measures[1].count[group] == rows / groups + (group < rows % groups ? 1 : 0));
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

    pgaccel_reset_grouped_agg_telemetry();
    test_workspace_and_descriptor_validation();
    test_dense_chunk_lifecycle_equivalence();
    test_dense_chunk_lifecycle_fail_closed();
    test_empty_ungrouped_active();
    test_parallel_dense_count_star_lifecycle();
    test_parallel_dense_int2_count_column();
    test_parallel_dense_int8_count_column();
    test_parallel_dense_date_count_column();
    test_parallel_dense_timestamp_count_column();
    test_parallel_dense_float_count_columns();
    test_parallel_dense_integer_phase2_shape();
    test_parallel_dense_integer_without_count();
    test_parallel_dense_global_integer_filters(false);
    test_parallel_dense_global_integer_filters(true);
    test_parallel_dense_global_integer_filters(true, 3);
    test_parallel_dense_global_integer_filters(true, 17);
    test_parallel_dense_global_integer_filters(false, 1, true);
    test_parallel_dense_global_integer_filters(false, 3, true);
    test_parallel_dense_global_integer_filters(false, 17, true);
    test_parallel_dense_global_integer_filters(true, 1, true);
    test_parallel_dense_global_integer_filters(true, 3, true);
    test_parallel_dense_global_integer_filters(true, 17, true);
    test_parallel_dense_global_integer_filters(false, 1, false, true);
    test_parallel_dense_global_integer_filters(true, 3, false, true);
    test_parallel_dense_global_integer_filters(true, 17, false, true);
    test_parallel_dense_multiple_integer_measures();
    test_parallel_dense_multiple_integer_measures(true);
    test_parallel_dense_independent_measure_filters();
    test_parallel_dense_count_only_states();
    test_numeric_exact_arithmetic();
    test_numeric_specials_and_extremes();
    test_numeric_lifecycle_and_invalid();
    test_numeric_overflow_and_admission();
    test_numeric_independent_filters();
    test_numeric_independent_counts();
    test_numeric_mixed_integer_states();
    test_bigint_numeric_states();
    test_null_accepting_ranges();
    test_parallel_computed_integer_measures();
    test_fused_integer_measure_order();
    test_fused_scalar_programs();
    test_atomic_bounded_ordered_prefix_fallback();
    test_fused_scalar_errors_and_validation();
    test_scalar_date_results_and_errors();
    test_scalar_where_predicates();
    test_scalar_where_mask_position();
    test_scalar_where_ordered_carrier();
    test_fused_scalar_binding_and_recovery();
    test_numeric_where_filters();
    test_numeric_large_reference();
    test_parallel_dense_integer_product_sum_shape();
    test_parallel_dense_integer_measure_range_filter();
    test_parallel_dense_sum_mask_unique_dimension_shape();
    test_parallel_dense_count_unique_dimension_shape();
    test_parallel_dense_weighted_global_count_shape();
    test_parallel_dense_sum_atomic_low_word_carry();
    test_parallel_dense_sum_atomic_negative_nullable_publication();
    test_i64_four_measure_lanes();
    test_f64_stats_pair_and_nan_ordering();
    test_i64_stats_pair_and_f64_binary_measures();
    test_global_and_measure_filters();
    test_predicate_only_physical_count();
    test_ordered_physical_min_max_count();
    test_four_dimensions_and_multiplicity();
    test_mixed_radix_compact_and_keyed_empty();
    test_device_publication_contract();
    test_injected_lifecycle_failures_are_transactional_and_reusable();
    // The owned-workspace recovery probe intentionally exercises device-USM
    // copy/wait telemetry.  The aggregate assertions below describe the
    // ordinary externally-owned shared-USM suite, so start that sample fresh.
    pgaccel_reset_grouped_agg_telemetry();
    test_group_activity_ignores_measure_validity();
    test_generic_device_error_matrix();
    test_specialized_dense_branch_matrix();
    test_weighted_overflow_branch_matrix();
    test_compact_full_lane_copy();
    test_output_descriptor_validation();
    test_pointer_span_validation();
    test_integer_expression_overflow_semantics();
    test_error_and_unsupported_statuses();
    test_fixed_seed_mixed_radix_fuzz();
    test_dense_full_lane_reference();
    test_filtered_product_reference();
    test_two_key_dimension_reference();
    test_three_key_dimension_reference();
    test_dimension_subtraction_reference();

    const uint64_t launches = pgaccel_grouped_agg_transition_launch_count();
    CHECK(launches > 0);
    CHECK(pgaccel_grouped_agg_queue_wait_count() == launches);
    CHECK(pgaccel_grouped_agg_queue_wait_ns() > 0);
    CHECK(pgaccel_grouped_agg_output_bytes() > 0);
    CHECK(pgaccel_grouped_agg_shared_copy_calls() > 0);
    CHECK(pgaccel_grouped_agg_device_copy_calls() == 0);

    CHECK_STATUS(pgaccel_shutdown(), PGACCEL_OK);
  } catch (const std::exception& exception) {
    std::fprintf(stderr, "FAIL: unexpected test infrastructure exception: %s\n", exception.what());
    ++g_fail;
  }

  std::printf("test_grouped_agg: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

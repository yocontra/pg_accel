/*
 * Generic dense grouped aggregation for the frozen OLAP descriptor ABI.
 *
 * Phase 4B intentionally uses one GPU work-item.  This preserves descriptor
 * row order (and therefore deterministic floating-point results) while the
 * generic surface is brought up.  No row is interpreted or aggregated on the
 * host; the host validates the ABI and publishes completed scratch state.
 */

#include <sycl/sycl.hpp>

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <new>
#include <type_traits>

#include "pgaccel_olap.h"
#include "pgaccel_queue.h"

namespace {

constexpr size_t kWorkspaceAlignment = 8;
constexpr size_t kNoOffset = std::numeric_limits<size_t>::max();
constexpr uint16_t kOpEq = PGACCEL_EXPR_OP_EQ;
constexpr uint16_t kOpNe = PGACCEL_EXPR_OP_NE;
constexpr uint16_t kOpLt = PGACCEL_EXPR_OP_LT;
constexpr uint16_t kOpLe = PGACCEL_EXPR_OP_LE;
constexpr uint16_t kOpGt = PGACCEL_EXPR_OP_GT;
constexpr uint16_t kOpGe = PGACCEL_EXPR_OP_GE;
constexpr uint16_t kOpAlways = PGACCEL_EXPR_OP_ALWAYS_TRUE;

template <typename T>
bool bytewise_zero(const T& value) {
  static_assert(std::is_trivially_copyable_v<T>);
  const T zero{};
  return std::memcmp(&value, &zero, sizeof(T)) == 0;
}

bool canonical_null(const pgaccel_val& value) {
  return bytewise_zero(value);
}

bool canonical_disabled_filter(const pgaccel_grouped_agg_filter& filter) {
  if (filter.kind != PGACCEL_GROUPED_AGG_FILTER_NONE || filter.predicate_source != 0 ||
      filter.predicate_measure_slot != 0 || filter.predicate_range_count != 0 ||
      filter.value_cmp_opcode != kOpAlways || filter._pad0 != 0 || filter.flags != 0 ||
      filter.mask != nullptr || !canonical_null(filter.value_cmp_const))
    return false;
  for (size_t i = 0; i < PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES; ++i) {
    if (!canonical_null(filter.predicate_lo[i]) || !canonical_null(filter.predicate_hi[i]))
      return false;
  }
  return true;
}

bool checked_add_size(size_t lhs, size_t rhs, size_t* out) {
  if (lhs > std::numeric_limits<size_t>::max() - rhs)
    return false;
  *out = lhs + rhs;
  return true;
}

bool checked_mul_size(size_t lhs, size_t rhs, size_t* out) {
  if (lhs != 0 && rhs > std::numeric_limits<size_t>::max() / lhs)
    return false;
  *out = lhs * rhs;
  return true;
}

bool checked_span(const void* ptr, size_t count, size_t width, uintptr_t* begin, uintptr_t* end) {
  if (ptr == nullptr || count == 0 || width == 0)
    return false;
  size_t bytes = 0;
  if (!checked_mul_size(count, width, &bytes))
    return false;
  const uintptr_t start = reinterpret_cast<uintptr_t>(ptr);
  if (start > std::numeric_limits<uintptr_t>::max() - bytes)
    return false;
  *begin = start;
  *end = start + bytes;
  return true;
}

struct Span {
  uintptr_t begin;
  uintptr_t end;
};

template <typename T, size_t Capacity>
struct FixedList {
  T values[Capacity]{};
  size_t count = 0;

  bool push(const T& value) {
    if (count == Capacity)
      return false;
    values[count++] = value;
    return true;
  }
};

using SpanList = FixedList<Span, 64>;
using PointerList = FixedList<const void*, 64>;

bool add_span(SpanList* spans, const void* ptr, size_t count, size_t width) {
  if (ptr == nullptr || count == 0)
    return true;
  uintptr_t begin = 0;
  uintptr_t end = 0;
  if (!checked_span(ptr, count, width, &begin, &end))
    return false;
  return spans->push({begin, end});
}

bool spans_overlap(const Span& lhs, const Span& rhs) {
  return lhs.begin < rhs.end && rhs.begin < lhs.end;
}

struct DeviceMeasureBuffers {
  uint64_t* sum = nullptr;
  uint64_t* min = nullptr;
  uint64_t* max = nullptr;
  uint64_t* sumsq = nullptr;
  uint64_t* count = nullptr;
  uint64_t* nonnull = nullptr;
  uint64_t* rhs_sum = nullptr;
  uint64_t* rhs_count = nullptr;
  uint64_t* rhs_nonnull = nullptr;
};

struct DeviceMeta {
  size_t emitted;
  uint64_t selected;
  uint64_t uncertain;
  int32_t error;
  uint32_t _pad;
};

struct KernelParams {
  size_t row_count;
  size_t group_capacity;
  uint32_t key_count;
  uint32_t measure_count;
  uint32_t dim_count;
  int32_t output_mode;
  pgaccel_grouped_agg_key keys[PGACCEL_GROUPED_AGG_MAX_KEYS];
  pgaccel_grouped_agg_measure measures[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  pgaccel_grouped_agg_filter where_filter;
  pgaccel_grouped_agg_filter measure_filters[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  pgaccel_grouped_agg_dim dims[PGACCEL_GROUPED_AGG_MAX_DIMS];
  uint8_t* active;
  DeviceMeasureBuffers buffers[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  size_t* staged_group_codes;
  int32_t* staged_key_values[PGACCEL_GROUPED_AGG_MAX_KEYS];
  uint8_t* staged_key_nulls[PGACCEL_GROUPED_AGG_MAX_KEYS];
  DeviceMeta* meta;
};

struct MeasureLayout {
  size_t sum = kNoOffset;
  size_t min = kNoOffset;
  size_t max = kNoOffset;
  size_t sumsq = kNoOffset;
  size_t count = kNoOffset;
  size_t nonnull = kNoOffset;
  size_t rhs_sum = kNoOffset;
  size_t rhs_count = kNoOffset;
  size_t rhs_nonnull = kNoOffset;
};

struct WorkspaceLayout {
  size_t bytes = 0;
  size_t params = kNoOffset;
  size_t active = kNoOffset;
  MeasureLayout measures[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  size_t staged_group_codes = kNoOffset;
  size_t staged_key_values[PGACCEL_GROUPED_AGG_MAX_KEYS] = {kNoOffset, kNoOffset, kNoOffset};
  size_t staged_key_nulls[PGACCEL_GROUPED_AGG_MAX_KEYS] = {kNoOffset, kNoOffset, kNoOffset};
  size_t meta = kNoOffset;
};

class ArenaSizer {
 public:
  template <typename T>
  bool add(size_t count, size_t* offset) {
    const size_t alignment = alignof(T) > kWorkspaceAlignment ? alignof(T) : kWorkspaceAlignment;
    size_t aligned = 0;
    const size_t mask = alignment - 1;
    if (!checked_add_size(size_, mask, &aligned))
      return false;
    aligned &= ~mask;
    size_t bytes = 0;
    if (!checked_mul_size(count, sizeof(T), &bytes) || !checked_add_size(aligned, bytes, &size_))
      return false;
    *offset = aligned;
    return true;
  }

  size_t size() const { return size_; }

 private:
  size_t size_ = 0;
};

bool key_nullable(const pgaccel_grouped_agg_desc& desc, const pgaccel_grouped_agg_key& key) {
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX ||
      key.source != PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT)
    return key.null_code != PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
  return key.values.nulls != nullptr;
}

bool known_val_tag(int32_t tag) {
  return tag >= PGACCEL_VAL_BOOL && tag <= PGACCEL_VAL_TIMESTAMP;
}

size_t val_tag_width(int32_t tag) {
  switch (tag) {
    case PGACCEL_VAL_BOOL:
      return 1;
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_FLOAT32:
    case PGACCEL_VAL_DATE:
      return 4;
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_FLOAT64:
    case PGACCEL_VAL_TIMESTAMP:
      return 8;
    default:
      return 0;
  }
}

int32_t materialized_key_type(const pgaccel_grouped_agg_desc& desc,
                              const pgaccel_grouped_agg_key& key) {
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH &&
      key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT)
    return key.values.type;
  return PGACCEL_VAL_INT32;
}

bool make_layout(const pgaccel_grouped_agg_desc& desc, WorkspaceLayout* layout) {
  ArenaSizer arena;
  if (!arena.add<KernelParams>(1, &layout->params) ||
      !arena.add<uint8_t>(desc.group_capacity, &layout->active))
    return false;

  for (size_t i = 0; i < desc.measure_count; ++i) {
    const uint32_t mask = desc.measures[i].agg_mask;
    MeasureLayout& ml = layout->measures[i];
    if ((mask & PGACCEL_GROUPED_AGG_LANE_SUM) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.sum))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_MIN) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.min))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_MAX) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.max))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_SUMSQ) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.sumsq))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.count))
      return false;
    if (desc.measures[i].op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR &&
        (mask & (PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                 PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_SUMSQ |
                 PGACCEL_GROUPED_AGG_LANE_COUNT)) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.nonnull))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_RHS_SUM) != 0 &&
        (!arena.add<uint64_t>(desc.group_capacity, &ml.rhs_sum) ||
         !arena.add<uint64_t>(desc.group_capacity, &ml.rhs_nonnull)))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_RHS_COUNT) != 0 &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.rhs_count))
      return false;
    if ((mask & PGACCEL_GROUPED_AGG_LANE_RHS_COUNT) != 0 && ml.rhs_nonnull == kNoOffset &&
        !arena.add<uint64_t>(desc.group_capacity, &ml.rhs_nonnull))
      return false;
  }

  if (!arena.add<size_t>(desc.group_capacity, &layout->staged_group_codes))
    return false;
  for (size_t i = 0; i < desc.key_count; ++i) {
    if (!arena.add<int32_t>(desc.group_capacity, &layout->staged_key_values[i]))
      return false;
    if (key_nullable(desc, desc.keys[i]) &&
        !arena.add<uint8_t>(desc.group_capacity, &layout->staged_key_nulls[i]))
      return false;
  }
  if (!arena.add<DeviceMeta>(1, &layout->meta))
    return false;
  layout->bytes = arena.size();
  return layout->bytes != 0;
}

bool known_physical(int32_t type) {
  return type >= PGACCEL_GROUPED_AGG_PHYSICAL_BOOL && type <= PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL;
}

bool physical_shape_valid(const pgaccel_grouped_agg_measure_col& col) {
  if (!known_physical(col.physical_type) || col.flags != 0)
    return false;
  switch (col.physical_type) {
    case PGACCEL_GROUPED_AGG_PHYSICAL_BOOL:
      return col.element_bytes == 1 && col.scale == 0;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT32:
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32:
    case PGACCEL_GROUPED_AGG_PHYSICAL_DATE:
      return col.element_bytes == 4 && col.scale == 0;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT64:
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64:
    case PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP:
      return col.element_bytes == 8 && col.scale == 0;
    case PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC:
    case PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL:
      return col.element_bytes != 0;
    default:
      return false;
  }
}

int32_t physical_val_tag(int32_t type) {
  switch (type) {
    case PGACCEL_GROUPED_AGG_PHYSICAL_BOOL:
      return PGACCEL_VAL_BOOL;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT32:
      return PGACCEL_VAL_INT32;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT64:
      return PGACCEL_VAL_INT64;
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32:
      return PGACCEL_VAL_FLOAT32;
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64:
      return PGACCEL_VAL_FLOAT64;
    case PGACCEL_GROUPED_AGG_PHYSICAL_DATE:
      return PGACCEL_VAL_DATE;
    case PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP:
      return PGACCEL_VAL_TIMESTAMP;
    default:
      return -1;
  }
}

bool host_float_nan(const pgaccel_val& value) {
  if (value.tag == PGACCEL_VAL_FLOAT32) {
    uint32_t bits = 0;
    std::memcpy(&bits, &value.data.f32, sizeof(bits));
    return (bits & 0x7f800000u) == 0x7f800000u && (bits & 0x007fffffu) != 0;
  }
  if (value.tag == PGACCEL_VAL_FLOAT64) {
    uint64_t bits = 0;
    std::memcpy(&bits, &value.data.f64, sizeof(bits));
    return (bits & UINT64_C(0x7ff0000000000000)) == UINT64_C(0x7ff0000000000000) &&
           (bits & UINT64_C(0x000fffffffffffff)) != 0;
  }
  return false;
}

int host_compare_val(const pgaccel_val& lhs, const pgaccel_val& rhs) {
  switch (lhs.tag) {
    case PGACCEL_VAL_BOOL:
      return lhs.data.b == rhs.data.b ? 0 : (lhs.data.b ? 1 : -1);
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return lhs.data.i32 == rhs.data.i32 ? 0 : (lhs.data.i32 < rhs.data.i32 ? -1 : 1);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return lhs.data.i64 == rhs.data.i64 ? 0 : (lhs.data.i64 < rhs.data.i64 ? -1 : 1);
    case PGACCEL_VAL_FLOAT32:
      return lhs.data.f32 == rhs.data.f32 ? 0 : (lhs.data.f32 < rhs.data.f32 ? -1 : 1);
    case PGACCEL_VAL_FLOAT64:
      return lhs.data.f64 == rhs.data.f64 ? 0 : (lhs.data.f64 < rhs.data.f64 ? -1 : 1);
    default:
      return 0;
  }
}

struct Validation {
  bool supported = true;
  bool dim_used[PGACCEL_GROUPED_AGG_MAX_DIMS] = {};
};

bool validate_measure_col(const pgaccel_grouped_agg_measure_col& col, size_t row_count,
                          bool required) {
  if (!required)
    return bytewise_zero(col);
  return physical_shape_valid(col) && (row_count == 0 || col.values != nullptr);
}

bool validate_measure(const pgaccel_grouped_agg_measure& measure, size_t row_count,
                      Validation* validation) {
  if (measure.flags != 0 || measure._pad0 != 0 || measure.agg_mask == 0 ||
      (measure.agg_mask & ~PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN) != 0 ||
      ((measure.agg_mask & PGACCEL_GROUPED_AGG_LANE_SUMSQ) != 0 &&
       (measure.agg_mask & PGACCEL_GROUPED_AGG_LANE_SUM) == 0) ||
      measure.op < PGACCEL_GROUPED_AGG_MEASURE_COLUMN ||
      measure.op > PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR)
    return false;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR) {
    return measure.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT &&
           measure.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64 &&
           measure.state_bytes == sizeof(int64_t) && bytewise_zero(measure.value) &&
           bytewise_zero(measure.rhs);
  }

  const bool rhs_required = measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
                            measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB ||
                            measure.op == PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR;
  if (!validate_measure_col(measure.value, row_count, true) ||
      !validate_measure_col(measure.rhs, row_count, rhs_required))
    return false;
  if ((measure.agg_mask &
       (PGACCEL_GROUPED_AGG_LANE_RHS_SUM | PGACCEL_GROUPED_AGG_LANE_RHS_COUNT)) != 0 &&
      measure.op != PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR)
    return false;

  switch (measure.accumulator_kind) {
    case PGACCEL_GROUPED_AGG_ACCUM_I64:
      if (measure.state_bytes != 8)
        return false;
      if (!((measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 ||
             measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT64 ||
             measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_BOOL ||
             measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_DATE ||
             measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP) &&
            (!rhs_required || measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 ||
             measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT64 ||
             measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_BOOL ||
             measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_DATE ||
             measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP)))
        return false;
      if (measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
          measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT64)
        validation->supported = false;
      if (rhs_required && measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
          measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT64)
        validation->supported = false;
      if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL &&
          (measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32 ||
           measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32))
        validation->supported = false;
      if ((measure.agg_mask & PGACCEL_GROUPED_AGG_LANE_SUMSQ) != 0)
        validation->supported = false;
      break;
    case PGACCEL_GROUPED_AGG_ACCUM_F64:
      if (measure.state_bytes != 8)
        return false;
      if (!((measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32 ||
             measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64) &&
            (!rhs_required || measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32 ||
             measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64)))
        return false;
      if (measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64)
        validation->supported = false;
      if (rhs_required && measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64)
        validation->supported = false;
      break;
    case PGACCEL_GROUPED_AGG_ACCUM_NUMERIC:
      if (measure.state_bytes == 0 ||
          measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC ||
          (rhs_required && measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC))
        return false;
      validation->supported = false;
      break;
    case PGACCEL_GROUPED_AGG_ACCUM_INTERVAL:
      if (measure.state_bytes == 0 ||
          measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL ||
          (rhs_required && measure.rhs.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL))
        return false;
      validation->supported = false;
      break;
    default:
      return false;
  }
  return true;
}

bool validate_filter(const pgaccel_grouped_agg_filter& filter,
                     const pgaccel_grouped_agg_desc& desc) {
  if (filter.kind < PGACCEL_GROUPED_AGG_FILTER_NONE ||
      filter.kind > PGACCEL_GROUPED_AGG_FILTER_RECHECK || filter._pad0 != 0 || filter.flags != 0 ||
      filter.predicate_range_count < 0 ||
      filter.predicate_range_count > static_cast<int32_t>(PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES))
    return false;
  if ((filter.kind == PGACCEL_GROUPED_AGG_FILTER_NONE) != (filter.mask == nullptr))
    return false;
  if (!(filter.value_cmp_opcode == kOpEq || filter.value_cmp_opcode == kOpNe ||
        filter.value_cmp_opcode == kOpLt || filter.value_cmp_opcode == kOpLe ||
        filter.value_cmp_opcode == kOpGt || filter.value_cmp_opcode == kOpGe ||
        filter.value_cmp_opcode == kOpAlways))
    return false;

  const bool scalar = filter.predicate_range_count != 0 || filter.value_cmp_opcode != kOpAlways;
  if (!scalar) {
    if (filter.predicate_source != 0 || filter.predicate_measure_slot != 0 ||
        !canonical_null(filter.value_cmp_const))
      return false;
    for (size_t i = 0; i < PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES; ++i) {
      if (!canonical_null(filter.predicate_lo[i]) || !canonical_null(filter.predicate_hi[i]))
        return false;
    }
    return true;
  }

  if (filter.predicate_source < PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE ||
      filter.predicate_source > PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS ||
      filter.predicate_measure_slot < 0 ||
      static_cast<uint32_t>(filter.predicate_measure_slot) >= desc.measure_count)
    return false;
  const pgaccel_grouped_agg_measure& measure = desc.measures[filter.predicate_measure_slot];
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR ||
      (filter.predicate_source == PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS &&
       measure.op != PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR))
    return false;
  const pgaccel_grouped_agg_measure_col& col =
      filter.predicate_source == PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE ? measure.value
                                                                       : measure.rhs;
  const int32_t tag = physical_val_tag(col.physical_type);
  if (tag < 0)
    return false;
  for (int32_t i = 0; i < filter.predicate_range_count; ++i) {
    const pgaccel_val& lo = filter.predicate_lo[i];
    const pgaccel_val& hi = filter.predicate_hi[i];
    if (lo.tag != tag || hi.tag != tag || host_float_nan(lo) || host_float_nan(hi) ||
        host_compare_val(lo, hi) > 0)
      return false;
  }
  for (int32_t i = filter.predicate_range_count;
       i < static_cast<int32_t>(PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES); ++i) {
    if (!canonical_null(filter.predicate_lo[i]) || !canonical_null(filter.predicate_hi[i]))
      return false;
  }
  if (filter.value_cmp_opcode == kOpAlways) {
    if (!canonical_null(filter.value_cmp_const))
      return false;
  } else if (filter.value_cmp_const.tag != tag) {
    return false;
  }
  return true;
}

pgaccel_status validate_desc(const pgaccel_grouped_agg_desc* desc, Validation* validation,
                             WorkspaceLayout* layout) {
  if (desc == nullptr || desc->abi_version != PGACCEL_OLAP_ABI_VERSION ||
      desc->size_bytes != sizeof(*desc) || desc->flags != 0 || desc->_pad0 != 0 ||
      desc->_pad1 != 0 || desc->_pad2 != 0 || desc->key_count > PGACCEL_GROUPED_AGG_MAX_KEYS ||
      desc->measure_count == 0 || desc->measure_count > PGACCEL_GROUPED_AGG_MAX_MEASURES ||
      desc->dim_count > PGACCEL_GROUPED_AGG_MAX_DIMS ||
      desc->grouping_mode < PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX ||
      desc->grouping_mode > PGACCEL_GROUPED_AGG_GROUPING_HASH ||
      desc->output_mode < PGACCEL_GROUPED_AGG_OUTPUT_DENSE ||
      desc->output_mode > PGACCEL_GROUPED_AGG_OUTPUT_COMPACT ||
      (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH &&
       desc->output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE) ||
      (desc->execution_flags & ~PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN) != 0 ||
      desc->execution_flags == 0 ||
      ((desc->execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) == 0 && desc->row_count != 0))
    return PGACCEL_ERROR;

  if (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH)
    validation->supported = false;

  size_t expected_capacity = 1;
  for (size_t i = 0; i < desc->key_count; ++i) {
    const pgaccel_grouped_agg_key& key = desc->keys[i];
    if (key.flags != 0 || key._pad0 != 0 || key.source < PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT ||
        key.source > PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM3)
      return PGACCEL_ERROR;
    if (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX) {
      const int64_t end = static_cast<int64_t>(key.code_min) + key.cardinality;
      if (key.cardinality == 0 || end > static_cast<int64_t>(INT32_MAX) + 1 ||
          end <= static_cast<int64_t>(INT32_MIN) ||
          (key.null_code != PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE &&
           (static_cast<int64_t>(key.null_code) < key.code_min ||
            static_cast<int64_t>(key.null_code) >= end)) ||
          !checked_mul_size(expected_capacity, key.cardinality, &expected_capacity))
        return PGACCEL_ERROR;
    }
    if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
      const bool valid_type = desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
                                  ? key.values.type == PGACCEL_VAL_INT32
                                  : known_val_tag(key.values.type);
      if (!valid_type || key.lookup_by_key != nullptr ||
          (desc->row_count != 0 && key.values.values == nullptr) ||
          (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX &&
           key.null_code == PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE && key.values.nulls != nullptr))
        return PGACCEL_ERROR;
    } else {
      const size_t dim = static_cast<size_t>(key.source - PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0);
      if (dim >= desc->dim_count || !bytewise_zero(key.values) ||
          (desc->dims[dim].key_count != 0 && key.lookup_by_key == nullptr))
        return PGACCEL_ERROR;
      validation->dim_used[dim] = true;
    }
  }
  for (size_t i = desc->key_count; i < PGACCEL_GROUPED_AGG_MAX_KEYS; ++i) {
    if (!bytewise_zero(desc->keys[i]))
      return PGACCEL_ERROR;
  }
  if (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX &&
      desc->group_capacity != expected_capacity)
    return PGACCEL_ERROR;
  if (desc->group_capacity == 0)
    return PGACCEL_ERROR;

  for (size_t i = 0; i < desc->measure_count; ++i) {
    if (!validate_measure(desc->measures[i], desc->row_count, validation))
      return PGACCEL_ERROR;
  }
  for (size_t i = desc->measure_count; i < PGACCEL_GROUPED_AGG_MAX_MEASURES; ++i) {
    if (!bytewise_zero(desc->measures[i]))
      return PGACCEL_ERROR;
  }

  if (!validate_filter(desc->where_filter, *desc))
    return PGACCEL_ERROR;
  for (size_t i = 0; i < desc->measure_count; ++i) {
    if (!validate_filter(desc->measure_filters[i], *desc))
      return PGACCEL_ERROR;
  }
  for (size_t i = desc->measure_count; i < PGACCEL_GROUPED_AGG_MAX_MEASURES; ++i) {
    if (!canonical_disabled_filter(desc->measure_filters[i]))
      return PGACCEL_ERROR;
  }

  for (size_t i = 0; i < desc->dim_count; ++i) {
    const pgaccel_grouped_agg_dim& dim = desc->dims[i];
    const int64_t end = static_cast<int64_t>(dim.key_min) + dim.key_count;
    if (dim.flags != 0 || dim._pad0 != 0 || dim.fact_key.type != PGACCEL_VAL_INT32 ||
        (desc->row_count != 0 && dim.fact_key.values == nullptr) ||
        end > static_cast<int64_t>(INT32_MAX) + 1)
      return PGACCEL_ERROR;
  }
  for (size_t i = desc->dim_count; i < PGACCEL_GROUPED_AGG_MAX_DIMS; ++i) {
    if (!bytewise_zero(desc->dims[i]))
      return PGACCEL_ERROR;
  }

  if (!make_layout(*desc, layout))
    return PGACCEL_ERROR;
  if (desc->execution_flags != PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN)
    validation->supported = false;
  return validation->supported ? PGACCEL_OK : PGACCEL_UNSUPPORTED;
}

inline bool add_u64(uint64_t lhs, uint64_t rhs, uint64_t* out) {
  if (lhs > UINT64_MAX - rhs)
    return false;
  *out = lhs + rhs;
  return true;
}

inline bool mul_u64(uint64_t lhs, uint64_t rhs, uint64_t* out) {
  constexpr uint64_t mask = UINT64_C(0xffffffff);
  const uint64_t lhs_lo = lhs & mask;
  const uint64_t lhs_hi = lhs >> 32;
  const uint64_t rhs_lo = rhs & mask;
  const uint64_t rhs_hi = rhs >> 32;
  const uint64_t product_lo = lhs_lo * rhs_lo;
  const uint64_t cross_lhs = lhs_lo * rhs_hi;
  const uint64_t cross_rhs = lhs_hi * rhs_lo;
  const uint64_t product_hi = lhs_hi * rhs_hi;
  const uint64_t middle = (product_lo >> 32) + (cross_lhs & mask) + (cross_rhs & mask);
  if (product_hi != 0 || (cross_lhs >> 32) != 0 || (cross_rhs >> 32) != 0 || (middle >> 32) != 0)
    return false;
  *out = (product_lo & mask) | ((middle & mask) << 32);
  return true;
}

inline bool add_i64(int64_t lhs, int64_t rhs, int64_t* out) {
  if ((rhs > 0 && lhs > INT64_MAX - rhs) || (rhs < 0 && lhs < INT64_MIN - rhs))
    return false;
  *out = lhs + rhs;
  return true;
}

inline bool sub_i64(int64_t lhs, int64_t rhs, int64_t* out) {
  if ((rhs < 0 && lhs > INT64_MAX + rhs) || (rhs > 0 && lhs < INT64_MIN + rhs))
    return false;
  *out = lhs - rhs;
  return true;
}

inline bool weight_i64(int64_t value, uint64_t weight, int64_t* out) {
  if (value == 0 || weight == 0) {
    *out = 0;
    return true;
  }
  if (value > 0) {
    if (weight > static_cast<uint64_t>(INT64_MAX) / static_cast<uint64_t>(value))
      return false;
    *out = value * static_cast<int64_t>(weight);
    return true;
  }
  const uint64_t magnitude = static_cast<uint64_t>(-(value + 1)) + 1;
  const uint64_t negative_limit = UINT64_C(1) << 63;
  if (weight > negative_limit / magnitude)
    return false;
  const uint64_t product = magnitude * weight;
  *out = product == negative_limit ? INT64_MIN : -static_cast<int64_t>(product);
  return true;
}

inline bool is_nan_f32(float value) {
  const uint32_t bits = sycl::bit_cast<uint32_t>(value);
  return (bits & 0x7f800000u) == 0x7f800000u && (bits & 0x007fffffu) != 0;
}

inline bool is_nan_f64(double value) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(value);
  return (bits & UINT64_C(0x7ff0000000000000)) == UINT64_C(0x7ff0000000000000) &&
         (bits & UINT64_C(0x000fffffffffffff)) != 0;
}

template <typename T>
inline bool pg_compare(T lhs, uint16_t opcode, T rhs, bool lhs_nan, bool rhs_nan) {
  switch (opcode) {
    case kOpEq:
      return lhs_nan ? rhs_nan : (!rhs_nan && lhs == rhs);
    case kOpNe:
      return lhs_nan ? !rhs_nan : (rhs_nan || lhs != rhs);
    case kOpLt:
      return !lhs_nan && (rhs_nan || lhs < rhs);
    case kOpLe:
      return lhs_nan ? rhs_nan : (rhs_nan || lhs <= rhs);
    case kOpGt:
      return !rhs_nan && (lhs_nan || lhs > rhs);
    case kOpGe:
      return rhs_nan ? lhs_nan : (lhs_nan || lhs >= rhs);
    default:
      return true;
  }
}

inline int pg_order_f64(double lhs, double rhs) {
  const bool lhs_nan = is_nan_f64(lhs);
  const bool rhs_nan = is_nan_f64(rhs);
  if (lhs_nan)
    return rhs_nan ? 0 : 1;
  if (rhs_nan)
    return -1;
  return lhs < rhs ? -1 : (lhs > rhs ? 1 : 0);
}

inline bool null_at(const uint8_t* nulls, size_t row, bool* is_null) {
  if (nulls == nullptr) {
    *is_null = false;
    return true;
  }
  const uint8_t byte = nulls[row];
  if (byte > 1)
    return false;
  *is_null = byte != 0;
  return true;
}

inline bool load_predicate_value(const pgaccel_grouped_agg_measure_col& col, size_t row,
                                 pgaccel_val* value, bool* is_null) {
  if (!null_at(col.nulls, row, is_null))
    return false;
  *value = {};
  if (*is_null)
    return true;
  switch (col.physical_type) {
    case PGACCEL_GROUPED_AGG_PHYSICAL_BOOL:
      if (static_cast<const uint8_t*>(col.values)[row] > 1)
        return false;
      value->tag = PGACCEL_VAL_BOOL;
      value->data.b = static_cast<const uint8_t*>(col.values)[row] != 0;
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT32:
      value->tag = PGACCEL_VAL_INT32;
      value->data.i32 = static_cast<const int32_t*>(col.values)[row];
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_INT64:
      value->tag = PGACCEL_VAL_INT64;
      value->data.i64 = static_cast<const int64_t*>(col.values)[row];
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32:
      value->tag = PGACCEL_VAL_FLOAT32;
      value->data.f32 = static_cast<const float*>(col.values)[row];
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64:
      value->tag = PGACCEL_VAL_FLOAT64;
      value->data.f64 = static_cast<const double*>(col.values)[row];
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_DATE:
      value->tag = PGACCEL_VAL_DATE;
      value->data.i32 = static_cast<const int32_t*>(col.values)[row];
      return true;
    case PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP:
      value->tag = PGACCEL_VAL_TIMESTAMP;
      value->data.i64 = static_cast<const int64_t*>(col.values)[row];
      return true;
    default:
      return false;
  }
}

inline bool compare_val(const pgaccel_val& lhs, uint16_t opcode, const pgaccel_val& rhs) {
  switch (lhs.tag) {
    case PGACCEL_VAL_BOOL:
      return pg_compare<int32_t>(lhs.data.b ? 1 : 0, opcode, rhs.data.b ? 1 : 0, false, false);
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return pg_compare<int32_t>(lhs.data.i32, opcode, rhs.data.i32, false, false);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return pg_compare<int64_t>(lhs.data.i64, opcode, rhs.data.i64, false, false);
    case PGACCEL_VAL_FLOAT32:
      return pg_compare<float>(lhs.data.f32, opcode, rhs.data.f32, is_nan_f32(lhs.data.f32),
                               is_nan_f32(rhs.data.f32));
    case PGACCEL_VAL_FLOAT64:
      return pg_compare<double>(lhs.data.f64, opcode, rhs.data.f64, is_nan_f64(lhs.data.f64),
                                is_nan_f64(rhs.data.f64));
    default:
      return false;
  }
}

enum class FilterResult : int32_t { Reject = 0, Accept = 1, Uncertain = 2, Error = 3 };

inline FilterResult evaluate_filter(const pgaccel_grouped_agg_filter& filter,
                                    const KernelParams& params, size_t row) {
  bool mask_uncertain = false;
  if (filter.mask != nullptr) {
    const int8_t mask = filter.mask[row];
    if (mask != PGACCEL_EXPR_TRUE && mask != PGACCEL_EXPR_FALSE && mask != PGACCEL_EXPR_UNCERTAIN)
      return FilterResult::Error;
    if (mask == PGACCEL_EXPR_FALSE)
      return FilterResult::Reject;
    if (mask == PGACCEL_EXPR_UNCERTAIN) {
      if (filter.kind != PGACCEL_GROUPED_AGG_FILTER_RECHECK)
        return FilterResult::Reject;
      mask_uncertain = true;
    }
  }
  const bool scalar = filter.predicate_range_count != 0 || filter.value_cmp_opcode != kOpAlways;
  if (!scalar)
    return mask_uncertain ? FilterResult::Uncertain : FilterResult::Accept;
  const pgaccel_grouped_agg_measure& measure = params.measures[filter.predicate_measure_slot];
  const pgaccel_grouped_agg_measure_col& col =
      filter.predicate_source == PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE ? measure.value
                                                                       : measure.rhs;
  pgaccel_val value{};
  bool is_null = false;
  if (!load_predicate_value(col, row, &value, &is_null))
    return FilterResult::Error;
  if (is_null)
    return FilterResult::Reject;
  if (filter.predicate_range_count != 0) {
    bool in_range = false;
    for (int32_t i = 0; i < filter.predicate_range_count; ++i) {
      if (compare_val(value, kOpGe, filter.predicate_lo[i]) &&
          compare_val(value, kOpLe, filter.predicate_hi[i])) {
        in_range = true;
        break;
      }
    }
    if (!in_range)
      return FilterResult::Reject;
  }
  if (filter.value_cmp_opcode != kOpAlways &&
      !compare_val(value, filter.value_cmp_opcode, filter.value_cmp_const))
    return FilterResult::Reject;
  return mask_uncertain ? FilterResult::Uncertain : FilterResult::Accept;
}

inline bool load_i64(const pgaccel_grouped_agg_measure_col& col, size_t row, int64_t* value) {
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32) {
    *value = static_cast<const int32_t*>(col.values)[row];
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT64) {
    *value = static_cast<const int64_t*>(col.values)[row];
    return true;
  }
  return false;
}

inline bool load_f64(const pgaccel_grouped_agg_measure_col& col, size_t row, double* value) {
  if (col.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64)
    return false;
  *value = static_cast<const double*>(col.values)[row];
  return true;
}

inline bool accumulate_count(uint64_t* counts, size_t group, uint64_t weight) {
  if (counts == nullptr)
    return true;
  return add_u64(counts[group], weight, &counts[group]);
}

inline bool accumulate_i64(const pgaccel_grouped_agg_measure& measure,
                           const DeviceMeasureBuffers& buffers, size_t row, size_t group,
                           uint64_t weight) {
  bool lhs_null = false;
  bool rhs_null = false;
  if (!null_at(measure.value.nulls, row, &lhs_null))
    return false;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
      measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!null_at(measure.rhs.nulls, row, &rhs_null))
      return false;
  }
  if (lhs_null || rhs_null)
    return true;
  int64_t value = 0;
  int64_t rhs = 0;
  if (!load_i64(measure.value, row, &value))
    return false;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL) {
    if (!load_i64(measure.rhs, row, &rhs))
      return false;
    value = static_cast<int64_t>(static_cast<int32_t>(value)) *
            static_cast<int64_t>(static_cast<int32_t>(rhs));
  } else if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!load_i64(measure.rhs, row, &rhs) || !sub_i64(value, rhs, &value))
      return false;
  }
  if (!accumulate_count(buffers.count, group, weight) ||
      !accumulate_count(buffers.nonnull, group, weight))
    return false;
  if (buffers.sum != nullptr) {
    int64_t weighted = 0;
    int64_t current = sycl::bit_cast<int64_t>(buffers.sum[group]);
    if (!weight_i64(value, weight, &weighted) || !add_i64(current, weighted, &current))
      return false;
    buffers.sum[group] = sycl::bit_cast<uint64_t>(current);
  }
  const uint64_t valid = buffers.nonnull == nullptr ? 0 : buffers.nonnull[group];
  if (buffers.min != nullptr) {
    const int64_t current = sycl::bit_cast<int64_t>(buffers.min[group]);
    if (valid == weight || value < current)
      buffers.min[group] = sycl::bit_cast<uint64_t>(value);
  }
  if (buffers.max != nullptr) {
    const int64_t current = sycl::bit_cast<int64_t>(buffers.max[group]);
    if (valid == weight || value > current)
      buffers.max[group] = sycl::bit_cast<uint64_t>(value);
  }
  return true;
}

inline bool accumulate_f64(const pgaccel_grouped_agg_measure& measure,
                           const DeviceMeasureBuffers& buffers, size_t row, size_t group,
                           uint64_t weight) {
  bool lhs_null = false;
  bool rhs_null = false;
  if (!null_at(measure.value.nulls, row, &lhs_null))
    return false;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
      measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!null_at(measure.rhs.nulls, row, &rhs_null))
      return false;
  }
  if (lhs_null || rhs_null)
    return true;
  double value = 0;
  double rhs = 0;
  if (!load_f64(measure.value, row, &value))
    return false;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL) {
    if (!load_f64(measure.rhs, row, &rhs))
      return false;
    value *= rhs;
  } else if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!load_f64(measure.rhs, row, &rhs))
      return false;
    value -= rhs;
  }
  if (!accumulate_count(buffers.count, group, weight) ||
      !accumulate_count(buffers.nonnull, group, weight))
    return false;
  const double weighted = value * static_cast<double>(weight);
  if (buffers.sum != nullptr) {
    const double current = sycl::bit_cast<double>(buffers.sum[group]);
    buffers.sum[group] = sycl::bit_cast<uint64_t>(current + weighted);
  }
  if (buffers.sumsq != nullptr) {
    const double current = sycl::bit_cast<double>(buffers.sumsq[group]);
    buffers.sumsq[group] =
        sycl::bit_cast<uint64_t>(current + value * value * static_cast<double>(weight));
  }
  const uint64_t valid = buffers.nonnull == nullptr ? 0 : buffers.nonnull[group];
  if (buffers.min != nullptr) {
    const double current = sycl::bit_cast<double>(buffers.min[group]);
    if (valid == weight || pg_order_f64(value, current) < 0)
      buffers.min[group] = sycl::bit_cast<uint64_t>(value);
  }
  if (buffers.max != nullptr) {
    const double current = sycl::bit_cast<double>(buffers.max[group]);
    if (valid == weight || pg_order_f64(value, current) > 0)
      buffers.max[group] = sycl::bit_cast<uint64_t>(value);
  }
  return true;
}

inline bool accumulate_rhs(const pgaccel_grouped_agg_measure& measure,
                           const DeviceMeasureBuffers& buffers, size_t row, size_t group,
                           uint64_t weight) {
  if (measure.op != PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR)
    return true;
  bool is_null = false;
  if (!null_at(measure.rhs.nulls, row, &is_null))
    return false;
  if (is_null)
    return true;
  if (!accumulate_count(buffers.rhs_count, group, weight) ||
      !accumulate_count(buffers.rhs_nonnull, group, weight))
    return false;
  if (buffers.rhs_sum == nullptr)
    return true;
  if (measure.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64) {
    int64_t value = 0;
    int64_t weighted = 0;
    int64_t current = sycl::bit_cast<int64_t>(buffers.rhs_sum[group]);
    if (!load_i64(measure.rhs, row, &value) || !weight_i64(value, weight, &weighted) ||
        !add_i64(current, weighted, &current))
      return false;
    buffers.rhs_sum[group] = sycl::bit_cast<uint64_t>(current);
    return true;
  }
  double value = 0;
  if (!load_f64(measure.rhs, row, &value))
    return false;
  const double current = sycl::bit_cast<double>(buffers.rhs_sum[group]);
  buffers.rhs_sum[group] = sycl::bit_cast<uint64_t>(current + value * static_cast<double>(weight));
  return true;
}

inline void copy_group_state(DeviceMeasureBuffers& buffers, size_t dst, size_t src) {
  if (buffers.sum != nullptr)
    buffers.sum[dst] = buffers.sum[src];
  if (buffers.min != nullptr)
    buffers.min[dst] = buffers.min[src];
  if (buffers.max != nullptr)
    buffers.max[dst] = buffers.max[src];
  if (buffers.sumsq != nullptr)
    buffers.sumsq[dst] = buffers.sumsq[src];
  if (buffers.count != nullptr)
    buffers.count[dst] = buffers.count[src];
  if (buffers.nonnull != nullptr)
    buffers.nonnull[dst] = buffers.nonnull[src];
  if (buffers.rhs_sum != nullptr)
    buffers.rhs_sum[dst] = buffers.rhs_sum[src];
  if (buffers.rhs_count != nullptr)
    buffers.rhs_count[dst] = buffers.rhs_count[src];
  if (buffers.rhs_nonnull != nullptr)
    buffers.rhs_nonnull[dst] = buffers.rhs_nonnull[src];
}

inline void stage_keys(KernelParams& params, size_t dense_group, size_t output_group) {
  params.staged_group_codes[output_group] = dense_group;
  size_t remainder = dense_group;
  for (size_t reverse = params.key_count; reverse != 0; --reverse) {
    const size_t key_index = reverse - 1;
    const pgaccel_grouped_agg_key& key = params.keys[key_index];
    const size_t digit = remainder % key.cardinality;
    remainder /= key.cardinality;
    const int32_t code = static_cast<int32_t>(static_cast<int64_t>(key.code_min) + digit);
    params.staged_key_values[key_index][output_group] = code;
    if (params.staged_key_nulls[key_index] != nullptr)
      params.staged_key_nulls[key_index][output_group] = code == key.null_code ? 1 : 0;
  }
}

inline void run_dense_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  meta = {};
  for (size_t group = 0; group < params.group_capacity; ++group) {
    params.active[group] = 0;
    for (size_t m = 0; m < params.measure_count; ++m) {
      DeviceMeasureBuffers& buffers = params.buffers[m];
      if (buffers.sum != nullptr)
        buffers.sum[group] = 0;
      if (buffers.min != nullptr)
        buffers.min[group] = 0;
      if (buffers.max != nullptr)
        buffers.max[group] = 0;
      if (buffers.sumsq != nullptr)
        buffers.sumsq[group] = 0;
      if (buffers.count != nullptr)
        buffers.count[group] = 0;
      if (buffers.nonnull != nullptr)
        buffers.nonnull[group] = 0;
      if (buffers.rhs_sum != nullptr)
        buffers.rhs_sum[group] = 0;
      if (buffers.rhs_count != nullptr)
        buffers.rhs_count[group] = 0;
      if (buffers.rhs_nonnull != nullptr)
        buffers.rhs_nonnull[group] = 0;
    }
  }
  if (params.key_count == 0)
    params.active[0] = 1;

  for (size_t row = 0; row < params.row_count && meta.error == 0; ++row) {
    size_t dim_indexes[PGACCEL_GROUPED_AGG_MAX_DIMS] = {};
    uint64_t weight = 1;
    bool rejected = false;
    for (size_t d = 0; d < params.dim_count; ++d) {
      const pgaccel_grouped_agg_dim& dim = params.dims[d];
      bool is_null = false;
      if (!null_at(dim.fact_key.nulls, row, &is_null)) {
        meta.error = 1;
        break;
      }
      if (is_null) {
        rejected = true;
        break;
      }
      const int32_t raw = static_cast<const int32_t*>(dim.fact_key.values)[row];
      const int64_t digit = static_cast<int64_t>(raw) - dim.key_min;
      if (digit < 0 || static_cast<uint64_t>(digit) >= dim.key_count) {
        rejected = true;
        break;
      }
      dim_indexes[d] = static_cast<size_t>(digit);
      if (dim.match_by_key != nullptr) {
        const uint8_t match = dim.match_by_key[digit];
        if (match > 1) {
          meta.error = 1;
          break;
        }
        if (match == 0) {
          rejected = true;
          break;
        }
      }
      const uint64_t multiplicity =
          dim.multiplicity_by_key == nullptr ? 1 : dim.multiplicity_by_key[digit];
      if (multiplicity == 0) {
        rejected = true;
        break;
      }
      if (!mul_u64(weight, multiplicity, &weight)) {
        meta.error = 1;
        break;
      }
    }
    if (meta.error != 0 || rejected)
      continue;

    size_t group = 0;
    for (size_t k = 0; k < params.key_count; ++k) {
      const pgaccel_grouped_agg_key& key = params.keys[k];
      int32_t raw = 0;
      if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
        bool is_null = false;
        if (!null_at(key.values.nulls, row, &is_null)) {
          meta.error = 1;
          break;
        }
        raw = is_null ? key.null_code : static_cast<const int32_t*>(key.values.values)[row];
      } else {
        const size_t dim = static_cast<size_t>(key.source - PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0);
        const pgaccel_grouped_agg_dim& source_dim = params.dims[dim];
        if (source_dim.multiplicity_by_key != nullptr &&
            source_dim.multiplicity_by_key[dim_indexes[dim]] != 1) {
          meta.error = 1;
          break;
        }
        raw = key.lookup_by_key[dim_indexes[dim]];
      }
      const int64_t digit = static_cast<int64_t>(raw) - key.code_min;
      if (digit < 0 || static_cast<uint64_t>(digit) >= key.cardinality) {
        meta.error = 1;
        break;
      }
      // Descriptor validation proves the complete radix product fits size_t.
      group = group * key.cardinality + static_cast<size_t>(digit);
    }
    if (meta.error != 0)
      break;

    const FilterResult where = evaluate_filter(params.where_filter, params, row);
    if (where == FilterResult::Error) {
      meta.error = 1;
      break;
    }
    if (where == FilterResult::Uncertain) {
      if (!add_u64(meta.uncertain, 1, &meta.uncertain))
        meta.error = 1;
      continue;
    }
    if (where == FilterResult::Reject)
      continue;
    if (!add_u64(meta.selected, weight, &meta.selected)) {
      meta.error = 1;
      break;
    }
    params.active[group] = 1;

    bool row_uncertain = false;
    for (size_t m = 0; m < params.measure_count; ++m) {
      const FilterResult filter = evaluate_filter(params.measure_filters[m], params, row);
      if (filter == FilterResult::Error) {
        meta.error = 1;
        break;
      }
      if (filter == FilterResult::Uncertain) {
        row_uncertain = true;
        continue;
      }
      if (filter == FilterResult::Reject)
        continue;
      const pgaccel_grouped_agg_measure& measure = params.measures[m];
      DeviceMeasureBuffers& buffers = params.buffers[m];
      if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR) {
        if (!accumulate_count(buffers.count, group, weight))
          meta.error = 1;
        continue;
      }
      const uint32_t primary_mask = PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                                    PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT |
                                    PGACCEL_GROUPED_AGG_LANE_SUMSQ;
      bool ok = true;
      if ((measure.agg_mask & primary_mask) != 0) {
        ok = measure.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64
                 ? accumulate_i64(measure, buffers, row, group, weight)
                 : accumulate_f64(measure, buffers, row, group, weight);
      }
      if (ok && (measure.agg_mask &
                 (PGACCEL_GROUPED_AGG_LANE_RHS_SUM | PGACCEL_GROUPED_AGG_LANE_RHS_COUNT)) != 0)
        ok = accumulate_rhs(measure, buffers, row, group, weight);
      if (!ok) {
        meta.error = 1;
        break;
      }
    }
    if (row_uncertain && !add_u64(meta.uncertain, 1, &meta.uncertain))
      meta.error = 1;
  }

  if (meta.error != 0)
    return;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    if (params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE)
      stage_keys(params, group, group);
    if (params.active[group] == 0)
      continue;
    const size_t output_group =
        params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT ? meta.emitted : group;
    if (params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT && output_group != group) {
      for (size_t m = 0; m < params.measure_count; ++m)
        copy_group_state(params.buffers[m], output_group, group);
    }
    if (params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT)
      stage_keys(params, group, output_group);
    ++meta.emitted;
  }
}

template <typename T>
T* arena_ptr(void* base, size_t offset) {
  return offset == kNoOffset ? nullptr : reinterpret_cast<T*>(static_cast<uint8_t*>(base) + offset);
}

void bind_params(const pgaccel_grouped_agg_desc& desc, const WorkspaceLayout& layout, void* scratch,
                 KernelParams* params) {
  *params = {};
  params->row_count = desc.row_count;
  params->group_capacity = desc.group_capacity;
  params->key_count = desc.key_count;
  params->measure_count = desc.measure_count;
  params->dim_count = desc.dim_count;
  params->output_mode = desc.output_mode;
  std::memcpy(params->keys, desc.keys, sizeof(params->keys));
  std::memcpy(params->measures, desc.measures, sizeof(params->measures));
  params->where_filter = desc.where_filter;
  std::memcpy(params->measure_filters, desc.measure_filters, sizeof(params->measure_filters));
  std::memcpy(params->dims, desc.dims, sizeof(params->dims));
  params->active = arena_ptr<uint8_t>(scratch, layout.active);
  params->staged_group_codes = arena_ptr<size_t>(scratch, layout.staged_group_codes);
  params->meta = arena_ptr<DeviceMeta>(scratch, layout.meta);
  for (size_t k = 0; k < desc.key_count; ++k) {
    params->staged_key_values[k] = arena_ptr<int32_t>(scratch, layout.staged_key_values[k]);
    params->staged_key_nulls[k] = arena_ptr<uint8_t>(scratch, layout.staged_key_nulls[k]);
  }
  for (size_t m = 0; m < desc.measure_count; ++m) {
    const MeasureLayout& ml = layout.measures[m];
    DeviceMeasureBuffers& buffers = params->buffers[m];
    buffers.sum = arena_ptr<uint64_t>(scratch, ml.sum);
    buffers.min = arena_ptr<uint64_t>(scratch, ml.min);
    buffers.max = arena_ptr<uint64_t>(scratch, ml.max);
    buffers.sumsq = arena_ptr<uint64_t>(scratch, ml.sumsq);
    buffers.count = arena_ptr<uint64_t>(scratch, ml.count);
    buffers.nonnull = arena_ptr<uint64_t>(scratch, ml.nonnull);
    buffers.rhs_sum = arena_ptr<uint64_t>(scratch, ml.rhs_sum);
    buffers.rhs_count = arena_ptr<uint64_t>(scratch, ml.rhs_count);
    buffers.rhs_nonnull = arena_ptr<uint64_t>(scratch, ml.rhs_nonnull);
  }
}

bool validate_measure_out(const pgaccel_grouped_agg_measure& measure,
                          const pgaccel_grouped_agg_measure_out& output) {
  const uint32_t mask = measure.agg_mask;
  const bool value_state =
      (mask & (PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
               PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_SUMSQ)) != 0;
  const bool nonnull_valid = value_state ? output.nonnull_count != nullptr
                                         : (output.nonnull_count == nullptr ||
                                            (measure.op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR &&
                                             (mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0));
  const bool rhs_nonnull_valid = (mask & PGACCEL_GROUPED_AGG_LANE_RHS_SUM) != 0
                                     ? output.rhs_nonnull_count != nullptr
                                     : (output.rhs_nonnull_count == nullptr ||
                                        (mask & PGACCEL_GROUPED_AGG_LANE_RHS_COUNT) != 0);
  return (output.sum != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_SUM) != 0) &&
         (output.min != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_MIN) != 0) &&
         (output.max != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_MAX) != 0) &&
         (output.sumsq != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_SUMSQ) != 0) &&
         (output.count != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_COUNT) != 0) &&
         nonnull_valid &&
         (output.rhs_sum != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_RHS_SUM) != 0) &&
         (output.rhs_count != nullptr) == ((mask & PGACCEL_GROUPED_AGG_LANE_RHS_COUNT) != 0) &&
         rhs_nonnull_valid;
}

bool validate_out(const pgaccel_grouped_agg_desc& desc, const pgaccel_grouped_agg_out* out) {
  if (out == nullptr || out->abi_version != PGACCEL_OLAP_ABI_VERSION ||
      out->size_bytes != sizeof(*out) || out->group_capacity != desc.group_capacity ||
      (out->output_space != PGACCEL_MEM_SPACE_HOST &&
       out->output_space != PGACCEL_MEM_SPACE_SHARED_USM) ||
      out->flags != 0 || out->emitted_group_count != 0 || out->selected_count != 0 ||
      out->uncertain_count != 0)
    return false;
  if (desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE) {
    if (out->active_groups == nullptr)
      return false;
  } else if (out->active_groups != nullptr) {
    return false;
  }
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH && out->group_codes != nullptr)
    return false;

  for (size_t i = 0; i < desc.key_count; ++i) {
    const pgaccel_grouped_agg_key_out& key_out = out->keys[i];
    const bool required = desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    if (key_out.values == nullptr) {
      if (required || !bytewise_zero(key_out))
        return false;
      continue;
    }
    if (key_out.type != materialized_key_type(desc, desc.keys[i]) || key_out.flags != 0 ||
        (key_out.nulls != nullptr) != key_nullable(desc, desc.keys[i]))
      return false;
  }
  for (size_t i = desc.key_count; i < PGACCEL_GROUPED_AGG_MAX_KEYS; ++i) {
    if (!bytewise_zero(out->keys[i]))
      return false;
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    if (!validate_measure_out(desc.measures[i], out->measures[i]))
      return false;
  }
  for (size_t i = desc.measure_count; i < PGACCEL_GROUPED_AGG_MAX_MEASURES; ++i) {
    if (!bytewise_zero(out->measures[i]))
      return false;
  }
  return true;
}

bool collect_input_spans(const pgaccel_grouped_agg_desc& desc, SpanList* spans) {
  for (size_t i = 0; i < desc.key_count; ++i) {
    const pgaccel_grouped_agg_key& key = desc.keys[i];
    if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
      if (!add_span(spans, key.values.values, desc.row_count, val_tag_width(key.values.type)) ||
          !add_span(spans, key.values.nulls, desc.row_count, sizeof(uint8_t)))
        return false;
    } else {
      const size_t dim = static_cast<size_t>(key.source - PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0);
      if (!add_span(spans, key.lookup_by_key, desc.dims[dim].key_count, sizeof(int32_t)))
        return false;
    }
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    const pgaccel_grouped_agg_measure& measure = desc.measures[i];
    if (measure.op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR &&
        (!add_span(spans, measure.value.values, desc.row_count, measure.value.element_bytes) ||
         !add_span(spans, measure.value.nulls, desc.row_count, sizeof(uint8_t))))
      return false;
    if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
        measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB ||
        measure.op == PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR) {
      if (!add_span(spans, measure.rhs.values, desc.row_count, measure.rhs.element_bytes) ||
          !add_span(spans, measure.rhs.nulls, desc.row_count, sizeof(uint8_t)))
        return false;
    }
    if (!add_span(spans, desc.measure_filters[i].mask, desc.row_count, sizeof(int8_t)))
      return false;
  }
  if (!add_span(spans, desc.where_filter.mask, desc.row_count, sizeof(int8_t)))
    return false;
  for (size_t i = 0; i < desc.dim_count; ++i) {
    const pgaccel_grouped_agg_dim& dim = desc.dims[i];
    if (!add_span(spans, dim.fact_key.values, desc.row_count, sizeof(int32_t)) ||
        !add_span(spans, dim.fact_key.nulls, desc.row_count, sizeof(uint8_t)) ||
        !add_span(spans, dim.match_by_key, dim.key_count, sizeof(uint8_t)) ||
        !add_span(spans, dim.multiplicity_by_key, dim.key_count, sizeof(uint64_t)))
      return false;
  }
  return true;
}

bool collect_output_spans(const pgaccel_grouped_agg_desc& desc, const pgaccel_grouped_agg_out& out,
                          SpanList* spans) {
  const size_t capacity = out.group_capacity;
  if (!add_span(spans, out.group_codes, capacity, sizeof(size_t)) ||
      !add_span(spans, out.active_groups, capacity, sizeof(uint8_t)))
    return false;
  for (size_t i = 0; i < desc.key_count; ++i) {
    if (!add_span(spans, out.keys[i].values, capacity,
                  val_tag_width(materialized_key_type(desc, desc.keys[i]))) ||
        !add_span(spans, out.keys[i].nulls, capacity, sizeof(uint8_t)))
      return false;
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    const pgaccel_grouped_agg_measure_out& measure = out.measures[i];
    const size_t state_bytes = desc.measures[i].state_bytes;
    if (!add_span(spans, measure.sum, capacity, state_bytes) ||
        !add_span(spans, measure.min, capacity, state_bytes) ||
        !add_span(spans, measure.max, capacity, state_bytes) ||
        !add_span(spans, measure.sumsq, capacity, state_bytes) ||
        !add_span(spans, measure.count, capacity, sizeof(uint64_t)) ||
        !add_span(spans, measure.nonnull_count, capacity, sizeof(uint64_t)) ||
        !add_span(spans, measure.rhs_sum, capacity, state_bytes) ||
        !add_span(spans, measure.rhs_count, capacity, sizeof(uint64_t)) ||
        !add_span(spans, measure.rhs_nonnull_count, capacity, sizeof(uint64_t)))
      return false;
  }
  return true;
}

bool validate_aliases(const pgaccel_grouped_agg_desc& desc, const pgaccel_grouped_agg_out& out) {
  SpanList inputs;
  SpanList outputs;
  if (!collect_input_spans(desc, &inputs) || !add_span(&inputs, &desc, 1, sizeof(desc)) ||
      !add_span(&inputs, &out, 1, sizeof(out)) || !collect_output_spans(desc, out, &outputs))
    return false;
  if (desc.scratch != nullptr) {
    uintptr_t begin = 0;
    uintptr_t end = 0;
    if (!checked_span(desc.scratch, 1, desc.scratch_bytes, &begin, &end))
      return false;
    const Span scratch{begin, end};
    for (size_t i = 0; i < inputs.count; ++i) {
      if (spans_overlap(scratch, inputs.values[i]))
        return false;
    }
    if (!inputs.push(scratch))
      return false;
  }
  for (size_t i = 0; i < outputs.count; ++i) {
    for (size_t j = i + 1; j < outputs.count; ++j) {
      if (spans_overlap(outputs.values[i], outputs.values[j]))
        return false;
    }
    for (size_t j = 0; j < inputs.count; ++j) {
      if (spans_overlap(outputs.values[i], inputs.values[j]))
        return false;
    }
  }
  return true;
}

bool device_accessible(sycl::queue& queue, const void* ptr) {
  if (ptr == nullptr)
    return true;
  const sycl::usm::alloc type = sycl::get_pointer_type(ptr, queue.get_context());
  return type == sycl::usm::alloc::device || type == sycl::usm::alloc::shared;
}

bool validate_input_usm(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc) {
  PointerList pointers;
  for (size_t i = 0; i < desc.key_count; ++i) {
    const pgaccel_grouped_agg_key& key = desc.keys[i];
    if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
      if (!pointers.push(key.values.values) || !pointers.push(key.values.nulls))
        return false;
    } else {
      if (!pointers.push(key.lookup_by_key))
        return false;
    }
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    const pgaccel_grouped_agg_measure& measure = desc.measures[i];
    if (measure.op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR) {
      if (!pointers.push(measure.value.values) || !pointers.push(measure.value.nulls))
        return false;
    }
    if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
        measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB ||
        measure.op == PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR) {
      if (!pointers.push(measure.rhs.values) || !pointers.push(measure.rhs.nulls))
        return false;
    }
    if (!pointers.push(desc.measure_filters[i].mask))
      return false;
  }
  if (!pointers.push(desc.where_filter.mask))
    return false;
  for (size_t i = 0; i < desc.dim_count; ++i) {
    if (!pointers.push(desc.dims[i].fact_key.values) ||
        !pointers.push(desc.dims[i].fact_key.nulls) || !pointers.push(desc.dims[i].match_by_key) ||
        !pointers.push(desc.dims[i].multiplicity_by_key))
      return false;
  }
  for (size_t i = 0; i < pointers.count; ++i) {
    if (!device_accessible(queue, pointers.values[i]))
      return false;
  }
  return true;
}

bool validate_output_usm(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                         const pgaccel_grouped_agg_out& out) {
  PointerList pointers;
  if (!pointers.push(out.group_codes) || !pointers.push(out.active_groups))
    return false;
  for (size_t i = 0; i < desc.key_count; ++i) {
    if (!pointers.push(out.keys[i].values) || !pointers.push(out.keys[i].nulls))
      return false;
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    const pgaccel_grouped_agg_measure_out& measure = out.measures[i];
    if (!pointers.push(measure.sum) || !pointers.push(measure.min) || !pointers.push(measure.max) ||
        !pointers.push(measure.sumsq) || !pointers.push(measure.count) ||
        !pointers.push(measure.nonnull_count) || !pointers.push(measure.rhs_sum) ||
        !pointers.push(measure.rhs_count) || !pointers.push(measure.rhs_nonnull_count))
      return false;
  }
  for (size_t i = 0; i < pointers.count; ++i) {
    const void* pointer = pointers.values[i];
    if (pointer == nullptr)
      continue;
    const sycl::usm::alloc actual = sycl::get_pointer_type(pointer, queue.get_context());
    if (out.output_space == PGACCEL_MEM_SPACE_SHARED_USM) {
      if (actual != sycl::usm::alloc::shared)
        return false;
    } else if (actual == sycl::usm::alloc::device || actual == sycl::usm::alloc::shared) {
      return false;
    }
  }
  return true;
}

bool validate_scratch_shape(const pgaccel_grouped_agg_desc& desc, const WorkspaceLayout& layout) {
  if (desc.scratch == nullptr)
    return desc.scratch_bytes == 0 && desc.scratch_space == PGACCEL_MEM_SPACE_HOST &&
           desc.scratch_alignment == 0 &&
           desc.execution_flags == PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  if ((desc.scratch_space != PGACCEL_MEM_SPACE_SHARED_USM &&
       desc.scratch_space != PGACCEL_MEM_SPACE_DEVICE) ||
      desc.scratch_bytes < layout.bytes || desc.scratch_alignment < kWorkspaceAlignment ||
      (desc.scratch_alignment & (desc.scratch_alignment - 1)) != 0 ||
      (reinterpret_cast<uintptr_t>(desc.scratch) & (desc.scratch_alignment - 1)) != 0)
    return false;
  return true;
}

bool validate_scratch_usm(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc) {
  if (desc.scratch == nullptr)
    return true;
  const sycl::usm::alloc actual = sycl::get_pointer_type(desc.scratch, queue.get_context());
  return (desc.scratch_space == PGACCEL_MEM_SPACE_SHARED_USM &&
          actual == sycl::usm::alloc::shared) ||
         (desc.scratch_space == PGACCEL_MEM_SPACE_DEVICE && actual == sycl::usm::alloc::device);
}

void enqueue_copy(sycl::queue& queue, void* dst, const void* src, size_t count, size_t width) {
  if (dst != nullptr && count != 0)
    queue.memcpy(dst, src, count * width);
}

void publish_output(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                    const WorkspaceLayout& layout, void* scratch, const DeviceMeta& meta,
                    pgaccel_grouped_agg_out* out) {
  const size_t count =
      desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT ? meta.emitted : desc.group_capacity;
  if (desc.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE)
    enqueue_copy(queue, out->active_groups, arena_ptr<uint8_t>(scratch, layout.active),
                 desc.group_capacity, sizeof(uint8_t));
  enqueue_copy(queue, out->group_codes, arena_ptr<size_t>(scratch, layout.staged_group_codes),
               count, sizeof(size_t));
  for (size_t k = 0; k < desc.key_count; ++k) {
    enqueue_copy(queue, out->keys[k].values,
                 arena_ptr<int32_t>(scratch, layout.staged_key_values[k]), count, sizeof(int32_t));
    enqueue_copy(queue, out->keys[k].nulls, arena_ptr<uint8_t>(scratch, layout.staged_key_nulls[k]),
                 count, sizeof(uint8_t));
  }
  for (size_t m = 0; m < desc.measure_count; ++m) {
    const MeasureLayout& ml = layout.measures[m];
    pgaccel_grouped_agg_measure_out& output = out->measures[m];
    enqueue_copy(queue, output.sum, arena_ptr<uint64_t>(scratch, ml.sum), count, sizeof(uint64_t));
    enqueue_copy(queue, output.min, arena_ptr<uint64_t>(scratch, ml.min), count, sizeof(uint64_t));
    enqueue_copy(queue, output.max, arena_ptr<uint64_t>(scratch, ml.max), count, sizeof(uint64_t));
    enqueue_copy(queue, output.sumsq, arena_ptr<uint64_t>(scratch, ml.sumsq), count,
                 sizeof(uint64_t));
    enqueue_copy(queue, output.count, arena_ptr<uint64_t>(scratch, ml.count), count,
                 sizeof(uint64_t));
    enqueue_copy(queue, output.nonnull_count, arena_ptr<uint64_t>(scratch, ml.nonnull), count,
                 sizeof(uint64_t));
    enqueue_copy(queue, output.rhs_sum, arena_ptr<uint64_t>(scratch, ml.rhs_sum), count,
                 sizeof(uint64_t));
    enqueue_copy(queue, output.rhs_count, arena_ptr<uint64_t>(scratch, ml.rhs_count), count,
                 sizeof(uint64_t));
    enqueue_copy(queue, output.rhs_nonnull_count, arena_ptr<uint64_t>(scratch, ml.rhs_nonnull),
                 count, sizeof(uint64_t));
  }
  queue.wait_and_throw();
  out->emitted_group_count = meta.emitted;
  out->selected_count = meta.selected;
  out->uncertain_count = meta.uncertain;
}

class GroupedAggDenseKernel;

class ScratchOwner {
 public:
  ScratchOwner() = default;
  ScratchOwner(sycl::queue* queue, void* pointer) : queue_(queue), pointer_(pointer) {}
  ScratchOwner(const ScratchOwner&) = delete;
  ScratchOwner& operator=(const ScratchOwner&) = delete;
  ~ScratchOwner() {
    if (pointer_ != nullptr)
      sycl::free(pointer_, *queue_);
  }

 private:
  sycl::queue* queue_ = nullptr;
  void* pointer_ = nullptr;
};

}  // namespace

extern "C" pgaccel_status
pgaccel_grouped_agg_workspace_requirements(const pgaccel_grouped_agg_desc* desc,
                                           pgaccel_grouped_agg_workspace_req* out) {
  if (out == nullptr || out->abi_version != PGACCEL_OLAP_ABI_VERSION ||
      out->size_bytes != sizeof(*out) || out->bytes != 0 || out->alignment != 0 ||
      out->space != 0 || out->flags != 0)
    return PGACCEL_ERROR;
  Validation validation;
  WorkspaceLayout layout;
  const pgaccel_status status = validate_desc(desc, &validation, &layout);
  if (status != PGACCEL_OK)
    return status;
  out->bytes = layout.bytes;
  out->alignment = kWorkspaceAlignment;
  out->space = PGACCEL_MEM_SPACE_SHARED_USM;
  out->flags = 0;
  return status;
}

extern "C" pgaccel_status pgaccel_grouped_agg_execute(const pgaccel_grouped_agg_desc* desc,
                                                      pgaccel_grouped_agg_out* out) {
  Validation validation;
  WorkspaceLayout layout;
  const pgaccel_status descriptor_status = validate_desc(desc, &validation, &layout);
  if (descriptor_status == PGACCEL_ERROR)
    return descriptor_status;
  if (!validate_scratch_shape(*desc, layout))
    return PGACCEL_ERROR;
  if ((desc->execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) != 0) {
    if (!validate_out(*desc, out) || !validate_aliases(*desc, *out))
      return PGACCEL_ERROR;
  }
  if (descriptor_status == PGACCEL_UNSUPPORTED)
    return descriptor_status;

  try {
    sycl::queue& queue = pgaccel_require_queue();
    if (!validate_input_usm(queue, *desc) || !validate_scratch_usm(queue, *desc) ||
        !validate_output_usm(queue, *desc, *out))
      return PGACCEL_ERROR;

    void* scratch = desc->scratch;
    if (scratch == nullptr) {
      scratch = sycl::aligned_alloc_device(kWorkspaceAlignment, layout.bytes, queue);
      if (scratch == nullptr)
        return PGACCEL_OOM;
    }
    ScratchOwner owner(desc->scratch == nullptr ? &queue : nullptr,
                       desc->scratch == nullptr ? scratch : nullptr);

    KernelParams host_params;
    bind_params(*desc, layout, scratch, &host_params);
    KernelParams* device_params = arena_ptr<KernelParams>(scratch, layout.params);
    queue.memcpy(device_params, &host_params, sizeof(host_params));
    queue.single_task<GroupedAggDenseKernel>([=]() { run_dense_kernel(device_params); });
    queue.wait_and_throw();
    pgaccel_record_gpu_exec();

    DeviceMeta meta{};
    queue.memcpy(&meta, arena_ptr<DeviceMeta>(scratch, layout.meta), sizeof(meta)).wait_and_throw();
    if (meta.error != 0)
      return PGACCEL_ERROR;
    publish_output(queue, *desc, layout, scratch, meta, out);
    return PGACCEL_OK;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::bad_alloc&) {
    return PGACCEL_OOM;
  } catch (const std::exception& error) {
    return pgaccel_kernel_failure("pgaccel_grouped_agg_execute", &error);
  } catch (...) {
    return pgaccel_kernel_failure("pgaccel_grouped_agg_execute", nullptr);
  }
}

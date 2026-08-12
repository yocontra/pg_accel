/*
 * Generic dense grouped aggregation for the frozen OLAP descriptor ABI.
 *
 * Order-sensitive general aggregates retain the deterministic serial device
 * path. Ordinary dense COUNT(*) and exact int4 SUM+COUNT(*) shapes, including
 * SQL masks and unique resident dimensions, use parallel row classification
 * and checked per-group state commits.
 * No row is interpreted or aggregated on the host; the device publishes a
 * completion record and the host only performs its awaited copybacks.
 */

#include <sycl/sycl.hpp>

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <limits>
#include <new>
#include <stdexcept>
#include <type_traits>

#include "pgaccel_olap.h"
#include "pgaccel_queue.h"

#include "h3_exact_device.hpp"

namespace {

constexpr size_t kWorkspaceAlignment = 8;
constexpr size_t kNoOffset = std::numeric_limits<size_t>::max();
constexpr uint32_t kHashEmptyOwner = UINT32_MAX;
constexpr size_t kHashMaxRows = UINT32_MAX;
constexpr size_t kHashMaxGroupCapacity = size_t{1} << 30;
constexpr size_t kDenseIntegerChunkRows = 1024;
constexpr size_t kDenseIntegerAtomicMaxRows = 1'000'000;
constexpr size_t kDenseReductionWorkgroupSize = 32;
constexpr size_t kDenseReductionItemsPerWorkItem =
    kDenseIntegerChunkRows / kDenseReductionWorkgroupSize;
constexpr size_t kDenseHierarchicalMaxGroups = 8;
static_assert(kDenseIntegerChunkRows % kDenseReductionWorkgroupSize == 0);
// A one-million-row date x part grouping with 350 dense groups needs about
// 19 MiB of deterministic per-chunk partials. Keep enough bounded headroom
// for that release shape while retaining a hard fallback for high cardinality.
constexpr size_t kDenseIntegerMaxPartialBytes = 32 * 1024 * 1024;
constexpr uint32_t kFailureInvalid = 1u << 0;
constexpr uint32_t kFailureNumericOverflow = 1u << 1;
constexpr uint32_t kFailureCapacity = 1u << 2;
constexpr uint64_t kLifecycleCookie = UINT64_C(0x7067616363656c32);
constexpr uint32_t kLifecycleActive = 1;
constexpr uint32_t kLifecycleFinalized = 2;
constexpr uint32_t kLifecycleFailed = 3;
constexpr size_t kPublishKeyLaneCount = 2;
constexpr size_t kPublishMeasureLaneCount = 9;
constexpr size_t kPublishDetail = 0;
constexpr size_t kPublishActive = 1;
constexpr size_t kPublishGroupCodes = 2;
constexpr size_t kPublishKeys = 3;
constexpr size_t kPublishMeasures =
    kPublishKeys + PGACCEL_GROUPED_AGG_MAX_KEYS * kPublishKeyLaneCount;
constexpr size_t kPublishEmitted =
    kPublishMeasures + PGACCEL_GROUPED_AGG_MAX_MEASURES * kPublishMeasureLaneCount;
constexpr size_t kPublishSelected = kPublishEmitted + 1;
constexpr size_t kPublishUncertain = kPublishSelected + 1;
constexpr size_t kPublishCommandCount = kPublishUncertain + 1;
static_assert(kPublishCommandCount <= 64);
constexpr uint64_t publish_bit(size_t slot) {
  return UINT64_C(1) << slot;
}
constexpr uint16_t kOpEq = PGACCEL_EXPR_OP_EQ;
constexpr uint16_t kOpNe = PGACCEL_EXPR_OP_NE;
constexpr uint16_t kOpLt = PGACCEL_EXPR_OP_LT;
constexpr uint16_t kOpLe = PGACCEL_EXPR_OP_LE;
constexpr uint16_t kOpGt = PGACCEL_EXPR_OP_GT;
constexpr uint16_t kOpGe = PGACCEL_EXPR_OP_GE;
constexpr uint16_t kOpAlways = PGACCEL_EXPR_OP_ALWAYS_TRUE;

thread_local uint64_t g_grouped_transition_launches = 0;
thread_local uint64_t g_grouped_queue_waits = 0;
thread_local uint64_t g_grouped_queue_wait_ns = 0;
thread_local uint64_t g_grouped_output_bytes = 0;
thread_local uint64_t g_grouped_shared_copy_calls = 0;
thread_local uint64_t g_grouped_device_copy_calls = 0;

void saturating_add(uint64_t* const target, uint64_t value) {
  *target = value > std::numeric_limits<uint64_t>::max() - *target
                ? std::numeric_limits<uint64_t>::max()
                : *target + value;
}

using GroupedWaitStarted = std::chrono::steady_clock::time_point;

void record_grouped_wait(const GroupedWaitStarted started) {
  const auto elapsed = std::chrono::steady_clock::now() - started;
  const auto ns = std::chrono::duration_cast<std::chrono::nanoseconds>(elapsed).count();
  saturating_add(&g_grouped_queue_waits, 1);
  saturating_add(&g_grouped_queue_wait_ns, ns < 0 ? 0 : static_cast<uint64_t>(ns));
}

template <typename Awaitable>
void wait_grouped(Awaitable& awaitable) {
  const auto started = std::chrono::steady_clock::now();
  try {
    awaitable.wait_and_throw();
  } catch (...) {
    record_grouped_wait(started);
    throw;
  }
  record_grouped_wait(started);
}

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

bool exact_sql_mask_filter(const pgaccel_grouped_agg_filter& filter) {
  if (filter.kind != PGACCEL_GROUPED_AGG_FILTER_SQL || filter.predicate_source != 0 ||
      filter.predicate_measure_slot != 0 || filter.predicate_range_count != 0 ||
      filter.value_cmp_opcode != kOpAlways || filter._pad0 != 0 || filter.flags != 0 ||
      filter.mask == nullptr || !canonical_null(filter.value_cmp_const))
    return false;
  for (size_t i = 0; i < PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES; ++i) {
    if (!canonical_null(filter.predicate_lo[i]) || !canonical_null(filter.predicate_hi[i]))
      return false;
  }
  return true;
}

bool parallel_dense_where_filter(const pgaccel_grouped_agg_filter& filter) {
  return canonical_disabled_filter(filter) || exact_sql_mask_filter(filter);
}

bool exact_dense_integer_range_filter(const pgaccel_grouped_agg_filter& filter) {
  if (filter.kind != PGACCEL_GROUPED_AGG_FILTER_NONE || filter.mask != nullptr ||
      filter.predicate_source != PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE ||
      filter.predicate_measure_slot != 0 || filter.predicate_range_count != 1 ||
      filter.value_cmp_opcode != kOpAlways || filter._pad0 != 0 || filter.flags != 0 ||
      !canonical_null(filter.value_cmp_const) || filter.predicate_lo[0].tag != PGACCEL_VAL_INT32 ||
      filter.predicate_hi[0].tag != PGACCEL_VAL_INT32)
    return false;
  const int32_t lo = filter.predicate_lo[0].data.i32;
  const int32_t hi = filter.predicate_hi[0].data.i32;
  if (lo == INT32_MIN || hi == INT32_MAX || lo >= hi)
    return false;
  for (size_t i = 1; i < PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES; ++i) {
    if (!canonical_null(filter.predicate_lo[i]) || !canonical_null(filter.predicate_hi[i]))
      return false;
  }
  return true;
}

bool parallel_dense_integer_where_filter(const pgaccel_grouped_agg_filter& filter) {
  return parallel_dense_where_filter(filter) || exact_dense_integer_range_filter(filter);
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

struct alignas(8) AtomicU64Words {
  uint32_t low;
  uint32_t high;
};
static_assert(sizeof(AtomicU64Words) == sizeof(uint64_t));
static_assert(alignof(AtomicU64Words) == alignof(uint64_t));
static_assert(offsetof(AtomicU64Words, low) == 0);
static_assert(offsetof(AtomicU64Words, high) == sizeof(uint32_t));
static_assert(std::is_standard_layout_v<AtomicU64Words>);

struct DeviceMeta {
  size_t emitted;
  uint64_t selected;
  uint64_t uncertain;
  uint64_t lifecycle_cookie;
  uint64_t shape_fingerprint;
  uint32_t failure_flags;
  uint32_t lifecycle_state;
};

struct DenseIntegerPartial {
  int64_t sum;
  int64_t prefix_min;
  int64_t prefix_max;
  int64_t min;
  int64_t max;
  uint32_t rows;
  uint32_t nonnull;
  uint32_t failure_flags;
  uint32_t _pad0;
};
static_assert(sizeof(DenseIntegerPartial) == 56);

struct DeviceCopyCommand {
  size_t source_offset;
  size_t bytes;
};

struct DeviceCompletion {
  int32_t status;
  int32_t detail;
  size_t emitted;
  uint64_t selected;
  uint64_t uncertain;
  DeviceCopyCommand commands[kPublishCommandCount];
};

static_assert(std::is_standard_layout_v<DeviceCompletion>);
static_assert(offsetof(DeviceCompletion, detail) == sizeof(decltype(DeviceCompletion::status)));
static_assert(std::is_same_v<decltype(DeviceCompletion::emitted),
                             decltype(pgaccel_grouped_agg_out::emitted_group_count)>);
static_assert(sizeof(decltype(DeviceCompletion::emitted)) ==
              sizeof(decltype(pgaccel_grouped_agg_out::emitted_group_count)));

void record_grouped_output_bytes(const DeviceCompletion& completion) {
  for (const DeviceCopyCommand& command : completion.commands)
    saturating_add(&g_grouped_output_bytes, static_cast<uint64_t>(command.bytes));
}

struct DeviceOutputPresence {
  uint8_t detail;
  uint8_t group_codes;
  uint8_t active_groups;
  uint8_t key_values[PGACCEL_GROUPED_AGG_MAX_KEYS];
  uint8_t key_nulls[PGACCEL_GROUPED_AGG_MAX_KEYS];
  uint8_t measure_lanes[PGACCEL_GROUPED_AGG_MAX_MEASURES][kPublishMeasureLaneCount];
  uint8_t emitted;
  uint8_t selected;
  uint8_t uncertain;
};

struct DeviceSourceOffsets {
  size_t completion;
  size_t active;
  size_t group_codes;
  size_t key_values[PGACCEL_GROUPED_AGG_MAX_KEYS];
  size_t key_nulls[PGACCEL_GROUPED_AGG_MAX_KEYS];
  size_t measure_lanes[PGACCEL_GROUPED_AGG_MAX_MEASURES][kPublishMeasureLaneCount];
};

struct DevicePublishParams {
  size_t group_capacity;
  uint32_t execution_flags;
  uint32_t key_count;
  uint32_t measure_count;
  int32_t output_mode;
  int32_t key_types[PGACCEL_GROUPED_AGG_MAX_KEYS];
  size_t measure_state_bytes[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  DeviceMeta* meta;
  DeviceCompletion* completion;
  DeviceOutputPresence output;
  DeviceSourceOffsets source_offsets;
};

static_assert(kFailureInvalid == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
static_assert(kFailureNumericOverflow == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW);
static_assert(PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE == 0);
constexpr int32_t PGACCEL_GROUPED_AGG_DETAIL_INVALID = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;

struct KernelParams {
  size_t row_count;
  size_t group_capacity;
  uint64_t shape_fingerprint;
  uint32_t key_count;
  uint32_t measure_count;
  uint32_t dim_count;
  uint32_t execution_flags;
  int32_t grouping_mode;
  int32_t output_mode;
  size_t hash_slot_count;
  pgaccel_grouped_agg_key keys[PGACCEL_GROUPED_AGG_MAX_KEYS];
  pgaccel_grouped_agg_measure measures[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  pgaccel_grouped_agg_filter where_filter;
  pgaccel_grouped_agg_filter measure_filters[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  pgaccel_grouped_agg_dim dims[PGACCEL_GROUPED_AGG_MAX_DIMS];
  uint8_t* active;
  DeviceMeasureBuffers buffers[PGACCEL_GROUPED_AGG_MAX_MEASURES];
  size_t* staged_group_codes;
  uint8_t* staged_key_values[PGACCEL_GROUPED_AGG_MAX_KEYS];
  uint8_t* staged_key_nulls[PGACCEL_GROUPED_AGG_MAX_KEYS];
  uint32_t* hash_owners;
  uint32_t* hash_counts;
  uint32_t* hash_group_count;
  uint32_t* dense_chunk_counts;
  uint32_t* dense_selected_counts;
  size_t dense_count_chunk_count;
  AtomicU64Words* dense_integer_atomic_sum;
  AtomicU64Words* dense_integer_atomic_nonnull;
  AtomicU64Words* dense_integer_atomic_count;
  DenseIntegerPartial* dense_integer_partials;
  size_t dense_integer_chunk_count;
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
  size_t hash_slot_count = 0;
  size_t hash_owners = kNoOffset;
  size_t hash_counts = kNoOffset;
  size_t hash_group_count = kNoOffset;
  size_t dense_chunk_counts = kNoOffset;
  size_t dense_selected_counts = kNoOffset;
  size_t dense_count_chunk_count = 0;
  size_t dense_count_partial_count = 0;
  bool dense_count_hierarchical = false;
  size_t dense_integer_atomic_sum = kNoOffset;
  size_t dense_integer_atomic_nonnull = kNoOffset;
  size_t dense_integer_atomic_count = kNoOffset;
  size_t dense_integer_partials = kNoOffset;
  size_t dense_integer_chunk_count = 0;
  size_t dense_integer_partial_count = 0;
  bool dense_integer_parallel = false;
  bool dense_integer_hierarchical = false;
  bool dense_integer_atomic_one_shot = false;
  size_t meta = kNoOffset;
  size_t publish_params = kNoOffset;
  size_t completion = kNoOffset;
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

bool parallel_dense_count_column_shape(const pgaccel_grouped_agg_desc& desc) {
  const pgaccel_grouped_agg_measure& count = desc.measures[0];
  return count.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
         count.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT &&
         count.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64 &&
         count.state_bytes == sizeof(int64_t) &&
         ((count.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_BOOL &&
           count.value.element_bytes == sizeof(uint8_t)) ||
          (count.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
           count.value.element_bytes == sizeof(int32_t)) ||
          (count.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT64 &&
           count.value.element_bytes == sizeof(int64_t)));
}

bool parallel_dense_count_shape(const pgaccel_grouped_agg_desc& desc) {
  if (desc.grouping_mode != PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX ||
      desc.output_mode != PGACCEL_GROUPED_AGG_OUTPUT_DENSE || desc.measure_count != 1 ||
      !parallel_dense_where_filter(desc.where_filter) ||
      !canonical_disabled_filter(desc.measure_filters[0]))
    return false;
  const pgaccel_grouped_agg_measure& count = desc.measures[0];
  const bool count_star = count.op == PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR &&
                          count.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT &&
                          count.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64 &&
                          count.state_bytes == sizeof(int64_t);
  if (!count_star && !parallel_dense_count_column_shape(desc))
    return false;
  for (size_t dim = 0; dim < desc.dim_count; ++dim) {
    if (desc.dims[dim].multiplicity_by_key != nullptr)
      return false;
  }
  return true;
}

bool parallel_dense_integer_structure(const pgaccel_grouped_agg_desc& desc) {
  const bool measure_range = exact_dense_integer_range_filter(desc.measure_filters[0]);
  if (desc.grouping_mode != PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX ||
      desc.output_mode != PGACCEL_GROUPED_AGG_OUTPUT_DENSE || desc.group_capacity == 0 ||
      desc.measure_count != 2 || !parallel_dense_integer_where_filter(desc.where_filter) ||
      (!canonical_disabled_filter(desc.measure_filters[0]) && !measure_range) ||
      !canonical_disabled_filter(desc.measure_filters[1]))
    return false;
  const pgaccel_grouped_agg_measure& value = desc.measures[0];
  const pgaccel_grouped_agg_measure& count = desc.measures[1];
  const bool direct_integer_stats =
      value.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
      (value.agg_mask == PGACCEL_GROUPED_AGG_LANE_SUM ||
       value.agg_mask == (PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                          PGACCEL_GROUPED_AGG_LANE_MAX));
  const bool integer_product_sum = value.op == PGACCEL_GROUPED_AGG_MEASURE_MUL &&
                                   value.agg_mask == PGACCEL_GROUPED_AGG_LANE_SUM &&
                                   value.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
                                   value.rhs.element_bytes == sizeof(int32_t);
  if ((!direct_integer_stats && !integer_product_sum) ||
      (exact_dense_integer_range_filter(desc.where_filter) &&
       (!integer_product_sum || desc.key_count != 1 || desc.dim_count != 0)) ||
      (measure_range && (!direct_integer_stats || value.agg_mask != PGACCEL_GROUPED_AGG_LANE_SUM ||
                         !canonical_disabled_filter(desc.where_filter) || desc.key_count != 1 ||
                         desc.dim_count != 0)) ||
      value.accumulator_kind != PGACCEL_GROUPED_AGG_ACCUM_I64 ||
      value.state_bytes != sizeof(int64_t) ||
      value.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32 ||
      value.value.element_bytes != sizeof(int32_t) ||
      count.op != PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR ||
      count.agg_mask != PGACCEL_GROUPED_AGG_LANE_COUNT ||
      count.accumulator_kind != PGACCEL_GROUPED_AGG_ACCUM_I64 ||
      count.state_bytes != sizeof(int64_t))
    return false;
  for (size_t dim = 0; dim < desc.dim_count; ++dim) {
    if (desc.dims[dim].multiplicity_by_key != nullptr)
      return false;
  }
  return true;
}

bool dense_integer_partial_shape(const pgaccel_grouped_agg_desc& desc, size_t* chunk_count,
                                 size_t* partial_count, size_t* allocation_count) {
  const size_t chunks = desc.row_count / kDenseIntegerChunkRows +
                        (desc.row_count % kDenseIntegerChunkRows != 0 ? 1 : 0);
  if (desc.group_capacity == 0)
    return false;
  const size_t max_partials = kDenseIntegerMaxPartialBytes / sizeof(DenseIntegerPartial);
  const size_t max_chunks = max_partials / desc.group_capacity;
  const size_t allocation_chunks = std::min(chunks, max_chunks);
  if (!checked_mul_size(desc.group_capacity, allocation_chunks, allocation_count))
    return false;
  *chunk_count = chunks;
  if (chunks > max_chunks) {
    *partial_count = 0;
    return false;
  }
  return checked_mul_size(desc.group_capacity, chunks, partial_count);
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

bool hash_slot_capacity(size_t group_capacity, size_t* slot_count) {
  size_t required = 0;
  if (!checked_mul_size(group_capacity, 2, &required))
    return false;
  size_t slots = 1;
  while (slots < required) {
    if (slots > std::numeric_limits<size_t>::max() / 2)
      return false;
    slots *= 2;
  }
  *slot_count = slots;
  return true;
}

bool make_layout(const pgaccel_grouped_agg_desc& desc, WorkspaceLayout* layout) {
  ArenaSizer arena;
  // Completion stays at the exact typed workspace base. Besides making its
  // device lineage explicit, this keeps the lifecycle result independent of
  // the descriptor's shape-specific lanes.
  if (!arena.add<DeviceCompletion>(1, &layout->completion) || layout->completion != 0 ||
      !arena.add<KernelParams>(1, &layout->params) || !arena.add<DeviceMeta>(1, &layout->meta) ||
      !arena.add<DevicePublishParams>(1, &layout->publish_params) ||
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
  if (parallel_dense_count_shape(desc)) {
    layout->dense_count_chunk_count = desc.row_count / kDenseIntegerChunkRows +
                                      (desc.row_count % kDenseIntegerChunkRows != 0 ? 1 : 0);
    layout->dense_count_hierarchical =
        desc.group_capacity <= kDenseHierarchicalMaxGroups && layout->dense_count_chunk_count != 0;
    if (layout->dense_count_hierarchical) {
      if (!checked_mul_size(desc.group_capacity, layout->dense_count_chunk_count,
                            &layout->dense_count_partial_count) ||
          !arena.add<uint32_t>(layout->dense_count_partial_count, &layout->dense_chunk_counts) ||
          (parallel_dense_count_column_shape(desc) &&
           !arena.add<uint32_t>(layout->dense_count_partial_count, &layout->dense_selected_counts)))
        return false;
    } else if (!arena.add<uint32_t>(desc.group_capacity, &layout->dense_chunk_counts) ||
               (parallel_dense_count_column_shape(desc) &&
                !arena.add<uint32_t>(desc.group_capacity, &layout->dense_selected_counts))) {
      return false;
    }
  }
  for (size_t i = 0; i < desc.key_count; ++i) {
    const size_t key_width = val_tag_width(materialized_key_type(desc, desc.keys[i]));
    if ((key_width == sizeof(uint32_t) &&
         !arena.add<uint32_t>(desc.group_capacity, &layout->staged_key_values[i])) ||
        (key_width == sizeof(uint64_t) &&
         !arena.add<uint64_t>(desc.group_capacity, &layout->staged_key_values[i])) ||
        (key_width != sizeof(uint32_t) && key_width != sizeof(uint64_t)))
      return false;
    if (key_nullable(desc, desc.keys[i]) &&
        !arena.add<uint8_t>(desc.group_capacity, &layout->staged_key_nulls[i]))
      return false;
  }
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH) {
    if (!hash_slot_capacity(desc.group_capacity, &layout->hash_slot_count) ||
        !arena.add<uint32_t>(layout->hash_slot_count, &layout->hash_owners) ||
        !arena.add<uint32_t>(layout->hash_slot_count, &layout->hash_counts) ||
        !arena.add<uint32_t>(1, &layout->hash_group_count))
      return false;
  }
  if (parallel_dense_integer_structure(desc)) {
    size_t allocation_count = 0;
    layout->dense_integer_atomic_one_shot =
        desc.execution_flags == PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN &&
        desc.row_count <= kDenseIntegerAtomicMaxRows &&
        desc.measures[0].agg_mask == PGACCEL_GROUPED_AGG_LANE_SUM &&
        desc.group_capacity > kDenseHierarchicalMaxGroups;
    layout->dense_integer_parallel =
        dense_integer_partial_shape(desc, &layout->dense_integer_chunk_count,
                                    &layout->dense_integer_partial_count, &allocation_count);
    layout->dense_integer_hierarchical =
        layout->dense_integer_parallel && desc.group_capacity <= kDenseHierarchicalMaxGroups;
    if (layout->dense_integer_atomic_one_shot) {
      const pgaccel_grouped_agg_measure& measure = desc.measures[0];
      const bool statically_nonnull =
          measure.value.nulls == nullptr &&
          (measure.op != PGACCEL_GROUPED_AGG_MEASURE_MUL || measure.rhs.nulls == nullptr) &&
          !exact_dense_integer_range_filter(desc.measure_filters[0]);
      if (!arena.add<AtomicU64Words>(desc.group_capacity, &layout->dense_integer_atomic_sum) ||
          (!statically_nonnull &&
           !arena.add<AtomicU64Words>(desc.group_capacity,
                                      &layout->dense_integer_atomic_nonnull)) ||
          !arena.add<AtomicU64Words>(desc.group_capacity, &layout->dense_integer_atomic_count))
        return false;
    } else {
      // Reserve the largest partial layout reachable by any shorter
      // lifecycle chunk. Parallel eligibility becomes false after the
      // bounded partial budget, so sizing only from the current row count
      // would undersize a shared session.
      if (allocation_count != 0 &&
          !arena.add<DenseIntegerPartial>(allocation_count, &layout->dense_integer_partials))
        return false;
    }
  }
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
  const bool count_only_column = measure.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
                                 measure.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT;
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
      {
        const bool ordered_column =
            measure.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
            (measure.agg_mask & ~(PGACCEL_GROUPED_AGG_LANE_COUNT | PGACCEL_GROUPED_AGG_LANE_MIN |
                                  PGACCEL_GROUPED_AGG_LANE_MAX)) == 0;
        if (measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
            measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_INT64 &&
            !count_only_column && !ordered_column)
          validation->supported = false;
      }
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
      {
        const bool ordered_column =
            measure.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
            (measure.agg_mask & ~(PGACCEL_GROUPED_AGG_LANE_COUNT | PGACCEL_GROUPED_AGG_LANE_MIN |
                                  PGACCEL_GROUPED_AGG_LANE_MAX)) == 0;
        if (measure.value.physical_type != PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64 &&
            !count_only_column && !ordered_column)
          validation->supported = false;
      }
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

void fingerprint_mix(uint64_t* fingerprint, uint64_t value) {
  *fingerprint ^= value + UINT64_C(0x9e3779b97f4a7c15) + (*fingerprint << 6) + (*fingerprint >> 2);
}

void fingerprint_pointer(uint64_t* fingerprint, const void* pointer) {
  fingerprint_mix(fingerprint, static_cast<uint64_t>(reinterpret_cast<uintptr_t>(pointer)));
}

void fingerprint_value(uint64_t* fingerprint, const pgaccel_val& value) {
  fingerprint_mix(fingerprint, static_cast<uint64_t>(value.tag));
  uint64_t payload = 0;
  switch (value.tag) {
    case PGACCEL_VAL_BOOL:
      payload = value.data.b ? 1 : 0;
      break;
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      payload = static_cast<uint32_t>(value.data.i32);
      break;
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      std::memcpy(&payload, &value.data.i64, sizeof(payload));
      break;
    case PGACCEL_VAL_FLOAT32: {
      uint32_t bits = 0;
      std::memcpy(&bits, &value.data.f32, sizeof(bits));
      payload = bits;
      break;
    }
    case PGACCEL_VAL_FLOAT64:
      std::memcpy(&payload, &value.data.f64, sizeof(payload));
      break;
    default:
      break;
  }
  fingerprint_mix(fingerprint, payload);
}

void fingerprint_filter(uint64_t* fingerprint, const pgaccel_grouped_agg_filter& filter) {
  fingerprint_mix(fingerprint, static_cast<uint64_t>(filter.kind));
  fingerprint_mix(fingerprint, static_cast<uint64_t>(filter.predicate_source));
  fingerprint_mix(fingerprint, static_cast<uint64_t>(filter.predicate_measure_slot));
  fingerprint_mix(fingerprint, static_cast<uint64_t>(filter.predicate_range_count));
  fingerprint_mix(fingerprint, filter.value_cmp_opcode);
  fingerprint_mix(fingerprint, filter.flags);
  fingerprint_mix(fingerprint, filter.mask == nullptr ? 0 : 1);
  fingerprint_value(fingerprint, filter.value_cmp_const);
  for (size_t i = 0; i < PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES; ++i) {
    fingerprint_value(fingerprint, filter.predicate_lo[i]);
    fingerprint_value(fingerprint, filter.predicate_hi[i]);
  }
}

uint64_t descriptor_shape_fingerprint(const pgaccel_grouped_agg_desc& desc) {
  uint64_t fingerprint = UINT64_C(0x243f6a8885a308d3);
  fingerprint_mix(&fingerprint, desc.abi_version);
  fingerprint_mix(&fingerprint, desc.size_bytes);
  fingerprint_mix(&fingerprint, static_cast<uint64_t>(desc.grouping_mode));
  fingerprint_mix(&fingerprint, static_cast<uint64_t>(desc.output_mode));
  fingerprint_mix(&fingerprint, desc.key_count);
  fingerprint_mix(&fingerprint, desc.group_capacity);
  fingerprint_mix(&fingerprint, desc.measure_count);
  fingerprint_mix(&fingerprint, desc.flags);
  fingerprint_mix(&fingerprint, desc.dim_count);
  for (size_t i = 0; i < desc.key_count; ++i) {
    const pgaccel_grouped_agg_key& key = desc.keys[i];
    fingerprint_mix(&fingerprint, static_cast<uint64_t>(key.values.type));
    fingerprint_mix(&fingerprint, key.values.values == nullptr ? 0 : 1);
    fingerprint_mix(&fingerprint, key.values.nulls == nullptr ? 0 : 1);
    fingerprint_pointer(&fingerprint, key.lookup_by_key);
    fingerprint_mix(&fingerprint, static_cast<uint64_t>(key.source));
    fingerprint_mix(&fingerprint, static_cast<uint32_t>(key.code_min));
    fingerprint_mix(&fingerprint, key.cardinality);
    fingerprint_mix(&fingerprint, static_cast<uint32_t>(key.null_code));
    fingerprint_mix(&fingerprint, key.flags);
  }
  for (size_t i = 0; i < desc.measure_count; ++i) {
    const pgaccel_grouped_agg_measure& measure = desc.measures[i];
    const pgaccel_grouped_agg_measure_col* columns[] = {&measure.value, &measure.rhs};
    for (const pgaccel_grouped_agg_measure_col* column : columns) {
      fingerprint_mix(&fingerprint, column->values == nullptr ? 0 : 1);
      fingerprint_mix(&fingerprint, column->nulls == nullptr ? 0 : 1);
      fingerprint_mix(&fingerprint, static_cast<uint64_t>(column->physical_type));
      fingerprint_mix(&fingerprint, column->element_bytes);
      fingerprint_mix(&fingerprint, static_cast<uint32_t>(column->scale));
      fingerprint_mix(&fingerprint, column->flags);
    }
    fingerprint_mix(&fingerprint, static_cast<uint64_t>(measure.op));
    fingerprint_mix(&fingerprint, measure.agg_mask);
    fingerprint_mix(&fingerprint, static_cast<uint64_t>(measure.accumulator_kind));
    fingerprint_mix(&fingerprint, measure.state_bytes);
    fingerprint_mix(&fingerprint, measure.flags);
    fingerprint_filter(&fingerprint, desc.measure_filters[i]);
  }
  fingerprint_filter(&fingerprint, desc.where_filter);
  for (size_t i = 0; i < desc.dim_count; ++i) {
    const pgaccel_grouped_agg_dim& dim = desc.dims[i];
    fingerprint_mix(&fingerprint, static_cast<uint64_t>(dim.fact_key.type));
    fingerprint_mix(&fingerprint, dim.fact_key.values == nullptr ? 0 : 1);
    fingerprint_mix(&fingerprint, dim.fact_key.nulls == nullptr ? 0 : 1);
    fingerprint_pointer(&fingerprint, dim.match_by_key);
    fingerprint_pointer(&fingerprint, dim.multiplicity_by_key);
    fingerprint_mix(&fingerprint, static_cast<uint32_t>(dim.key_min));
    fingerprint_mix(&fingerprint, dim.key_count);
    fingerprint_mix(&fingerprint, dim.flags);
  }
  return fingerprint;
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

  size_t expected_capacity = 1;
  for (size_t i = 0; i < desc->key_count; ++i) {
    const pgaccel_grouped_agg_key& key = desc->keys[i];
    if ((key.flags & ~PGACCEL_GROUPED_AGG_KEY_FLAG_ALL_KNOWN) != 0 ||
        key.source < PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT ||
        key.source > PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM3)
      return PGACCEL_ERROR;
    const bool h3_parent = (key.flags & PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT) != 0;
    if (h3_parent) {
      if (desc->grouping_mode != PGACCEL_GROUPED_AGG_GROUPING_HASH || desc->key_count != 1 ||
          key.source != PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT ||
          key.values.type != PGACCEL_VAL_INT64 || key.lookup_by_key != nullptr ||
          key._pad0 > pgaccel_h3_exact::MAX_H3_RES)
        return PGACCEL_ERROR;
    } else if (key._pad0 != 0) {
      return PGACCEL_ERROR;
    }
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

  if (desc->grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH) {
    const bool minimal_hash_slice =
        desc->key_count == 1 && desc->keys[0].source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT &&
        desc->keys[0].values.type == PGACCEL_VAL_INT64 && desc->measure_count == 1 &&
        desc->measures[0].op == PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR && desc->dim_count == 0 &&
        desc->row_count <= kHashMaxRows && desc->group_capacity <= kHashMaxGroupCapacity &&
        canonical_disabled_filter(desc->where_filter) &&
        canonical_disabled_filter(desc->measure_filters[0]);
    // Hash owners identify rows in the current input pointer range. Reusing
    // them across chunks would dereference owners against a different range,
    // so hash/H3 remains deliberately one-shot until that state is redesigned.
    if (!minimal_hash_slice || desc->execution_flags != PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN)
      validation->supported = false;
  }

  if (!validation->supported)
    return PGACCEL_UNSUPPORTED;
  return make_layout(*desc, layout) ? PGACCEL_OK : PGACCEL_ERROR;
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
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_BOOL) {
    const uint8_t raw = static_cast<const uint8_t*>(col.values)[row];
    if (raw > 1)
      return false;
    *value = raw;
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32) {
    *value = static_cast<const int32_t*>(col.values)[row];
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT64) {
    *value = static_cast<const int64_t*>(col.values)[row];
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_DATE) {
    *value = static_cast<const int32_t*>(col.values)[row];
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP) {
    *value = static_cast<const int64_t*>(col.values)[row];
    return true;
  }
  return false;
}

inline bool load_f64(const pgaccel_grouped_agg_measure_col& col, size_t row, double* value) {
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32) {
    *value = static_cast<const float*>(col.values)[row];
    return true;
  }
  if (col.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64) {
    *value = static_cast<const double*>(col.values)[row];
    return true;
  }
  return false;
}

inline pgaccel_grouped_agg_device_error accumulate_count(uint64_t* counts, size_t group,
                                                         uint64_t weight) {
  if (counts == nullptr)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  return add_u64(counts[group], weight, &counts[group])
             ? PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE
             : PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
}

inline pgaccel_grouped_agg_device_error accumulate_i64(const pgaccel_grouped_agg_measure& measure,
                                                       const DeviceMeasureBuffers& buffers,
                                                       size_t row, size_t group, uint64_t weight) {
  bool lhs_null = false;
  bool rhs_null = false;
  if (!null_at(measure.value.nulls, row, &lhs_null))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
      measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!null_at(measure.rhs.nulls, row, &rhs_null))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  }
  if (lhs_null || rhs_null)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
      measure.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT) {
    const auto count_error = accumulate_count(buffers.count, group, weight);
    return count_error == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE
               ? accumulate_count(buffers.nonnull, group, weight)
               : count_error;
  }
  int64_t value = 0;
  int64_t rhs = 0;
  if (!load_i64(measure.value, row, &value))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL) {
    if (!load_i64(measure.rhs, row, &rhs))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    value = static_cast<int64_t>(static_cast<int32_t>(value)) *
            static_cast<int64_t>(static_cast<int32_t>(rhs));
  } else if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!load_i64(measure.rhs, row, &rhs))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    if (!sub_i64(value, rhs, &value))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
  }
  if ((measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
       measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) &&
      measure.value.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
      measure.rhs.physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_INT32 &&
      (value < std::numeric_limits<int32_t>::min() || value > std::numeric_limits<int32_t>::max()))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
  const auto count_error = accumulate_count(buffers.count, group, weight);
  if (count_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return count_error;
  const auto nonnull_error = accumulate_count(buffers.nonnull, group, weight);
  if (nonnull_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return nonnull_error;
  if (buffers.sum != nullptr) {
    int64_t weighted = 0;
    int64_t current = sycl::bit_cast<int64_t>(buffers.sum[group]);
    if (!weight_i64(value, weight, &weighted) || !add_i64(current, weighted, &current))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
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
  return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
}

inline pgaccel_grouped_agg_device_error accumulate_f64(const pgaccel_grouped_agg_measure& measure,
                                                       const DeviceMeasureBuffers& buffers,
                                                       size_t row, size_t group, uint64_t weight) {
  bool lhs_null = false;
  bool rhs_null = false;
  if (!null_at(measure.value.nulls, row, &lhs_null))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL ||
      measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!null_at(measure.rhs.nulls, row, &rhs_null))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  }
  if (lhs_null || rhs_null)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_COLUMN &&
      measure.agg_mask == PGACCEL_GROUPED_AGG_LANE_COUNT) {
    const auto count_error = accumulate_count(buffers.count, group, weight);
    return count_error == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE
               ? accumulate_count(buffers.nonnull, group, weight)
               : count_error;
  }
  double value = 0;
  double rhs = 0;
  if (!load_f64(measure.value, row, &value))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_MUL) {
    if (!load_f64(measure.rhs, row, &rhs))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    value *= rhs;
  } else if (measure.op == PGACCEL_GROUPED_AGG_MEASURE_SUB) {
    if (!load_f64(measure.rhs, row, &rhs))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    value -= rhs;
  }
  const auto count_error = accumulate_count(buffers.count, group, weight);
  if (count_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return count_error;
  const auto nonnull_error = accumulate_count(buffers.nonnull, group, weight);
  if (nonnull_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return nonnull_error;
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
  return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
}

inline pgaccel_grouped_agg_device_error accumulate_rhs(const pgaccel_grouped_agg_measure& measure,
                                                       const DeviceMeasureBuffers& buffers,
                                                       size_t row, size_t group, uint64_t weight) {
  if (measure.op != PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  bool is_null = false;
  if (!null_at(measure.rhs.nulls, row, &is_null))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  if (is_null)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  const auto count_error = accumulate_count(buffers.rhs_count, group, weight);
  if (count_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return count_error;
  const auto nonnull_error = accumulate_count(buffers.rhs_nonnull, group, weight);
  if (nonnull_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE)
    return nonnull_error;
  if (buffers.rhs_sum == nullptr)
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  if (measure.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64) {
    int64_t value = 0;
    int64_t weighted = 0;
    int64_t current = sycl::bit_cast<int64_t>(buffers.rhs_sum[group]);
    if (!load_i64(measure.rhs, row, &value))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
    if (!weight_i64(value, weight, &weighted) || !add_i64(current, weighted, &current))
      return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
    buffers.rhs_sum[group] = sycl::bit_cast<uint64_t>(current);
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  }
  double value = 0;
  if (!load_f64(measure.rhs, row, &value))
    return PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  const double current = sycl::bit_cast<double>(buffers.rhs_sum[group]);
  buffers.rhs_sum[group] = sycl::bit_cast<uint64_t>(current + value * static_cast<double>(weight));
  return PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
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
    reinterpret_cast<int32_t*>(params.staged_key_values[key_index])[output_group] = code;
    if (params.staged_key_nulls[key_index] != nullptr)
      params.staged_key_nulls[key_index][output_group] = code == key.null_code ? 1 : 0;
  }
}

template <typename T>
using DeviceAtomic = sycl::atomic_ref<T, sycl::memory_order::relaxed, sycl::memory_scope::device,
                                      sycl::access::address_space::global_space>;

inline void record_failure(DeviceMeta& meta, uint32_t failure) {
  DeviceAtomic<uint32_t> failures(meta.failure_flags);
  failures.fetch_or(failure);
}

enum class DenseRowResult : int32_t { Accept = 0, Reject = 1, Error = 2 };

inline DenseRowResult dense_group_for_row(const KernelParams& params, size_t row,
                                          size_t* group_out) {
  size_t dim_indexes[PGACCEL_GROUPED_AGG_MAX_DIMS] = {};
  for (size_t dim_index = 0; dim_index < params.dim_count; ++dim_index) {
    const pgaccel_grouped_agg_dim& dim = params.dims[dim_index];
    if (dim.multiplicity_by_key != nullptr)
      return DenseRowResult::Error;
    bool is_null = false;
    if (!null_at(dim.fact_key.nulls, row, &is_null))
      return DenseRowResult::Error;
    if (is_null)
      return DenseRowResult::Reject;
    const int32_t raw = static_cast<const int32_t*>(dim.fact_key.values)[row];
    const int64_t digit = static_cast<int64_t>(raw) - dim.key_min;
    if (digit < 0 || static_cast<uint64_t>(digit) >= dim.key_count)
      return DenseRowResult::Reject;
    dim_indexes[dim_index] = static_cast<size_t>(digit);
    if (dim.match_by_key != nullptr) {
      const uint8_t match = dim.match_by_key[digit];
      if (match > 1)
        return DenseRowResult::Error;
      if (match == 0)
        return DenseRowResult::Reject;
    }
  }

  size_t group = 0;
  for (size_t key_index = 0; key_index < params.key_count; ++key_index) {
    const pgaccel_grouped_agg_key& key = params.keys[key_index];
    int32_t raw = 0;
    if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
      bool is_null = false;
      if (!null_at(key.values.nulls, row, &is_null))
        return DenseRowResult::Error;
      raw = is_null ? key.null_code : static_cast<const int32_t*>(key.values.values)[row];
    } else {
      const size_t dim_index =
          static_cast<size_t>(key.source - PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0);
      if (dim_index >= params.dim_count || key.lookup_by_key == nullptr)
        return DenseRowResult::Error;
      raw = key.lookup_by_key[dim_indexes[dim_index]];
    }
    const int64_t digit = static_cast<int64_t>(raw) - key.code_min;
    if (digit < 0 || static_cast<uint64_t>(digit) >= key.cardinality)
      return DenseRowResult::Error;
    group = group * key.cardinality + static_cast<size_t>(digit);
  }
  *group_out = group;
  return DenseRowResult::Accept;
}

template <bool HasMembership>
inline DenseRowResult specialized_dense_group_for_row(const KernelParams& params, size_t row,
                                                      size_t* group_out) {
  if constexpr (HasMembership) {
    return dense_group_for_row(params, row, group_out);
  } else {
    size_t group = 0;
    for (size_t key_index = 0; key_index < params.key_count; ++key_index) {
      const pgaccel_grouped_agg_key& key = params.keys[key_index];
      bool is_null = false;
      if (!null_at(key.values.nulls, row, &is_null))
        return DenseRowResult::Error;
      const int32_t raw =
          is_null ? key.null_code : static_cast<const int32_t*>(key.values.values)[row];
      const int64_t digit = static_cast<int64_t>(raw) - key.code_min;
      if (digit < 0 || static_cast<uint64_t>(digit) >= key.cardinality)
        return DenseRowResult::Error;
      group = group * key.cardinality + static_cast<size_t>(digit);
    }
    *group_out = group;
    return DenseRowResult::Accept;
  }
}

template <bool HasSqlMask>
inline FilterResult specialized_dense_where_filter(const KernelParams& params, size_t row) {
  if constexpr (!HasSqlMask) {
    return FilterResult::Accept;
  } else {
    const int8_t mask = params.where_filter.mask[row];
    if (mask == PGACCEL_EXPR_TRUE)
      return FilterResult::Accept;
    if (mask == PGACCEL_EXPR_FALSE || mask == PGACCEL_EXPR_UNCERTAIN)
      return FilterResult::Reject;
    return FilterResult::Error;
  }
}

template <bool HasSqlMask, bool HasScalarRange>
inline FilterResult specialized_dense_integer_where_filter(const KernelParams& params, size_t row,
                                                           int32_t* range_value) {
  static_assert(!(HasSqlMask && HasScalarRange));
  if constexpr (HasScalarRange) {
    const pgaccel_grouped_agg_measure_col& col = params.measures[0].value;
    bool is_null = false;
    if (!null_at(col.nulls, row, &is_null))
      return FilterResult::Error;
    if (is_null)
      return FilterResult::Reject;
    const int32_t value = static_cast<const int32_t*>(col.values)[row];
    *range_value = value;
    const pgaccel_grouped_agg_filter& filter = params.where_filter;
    return value >= filter.predicate_lo[0].data.i32 && value <= filter.predicate_hi[0].data.i32
               ? FilterResult::Accept
               : FilterResult::Reject;
  } else {
    return specialized_dense_where_filter<HasSqlMask>(params, row);
  }
}

template <bool HasMeasureRange>
inline FilterResult specialized_dense_integer_measure_filter(const KernelParams& params, size_t row,
                                                             int32_t* range_value) {
  if constexpr (!HasMeasureRange) {
    return FilterResult::Accept;
  } else {
    const pgaccel_grouped_agg_measure_col& col = params.measures[0].value;
    bool is_null = false;
    if (!null_at(col.nulls, row, &is_null))
      return FilterResult::Error;
    if (is_null)
      return FilterResult::Reject;
    const int32_t value = static_cast<const int32_t*>(col.values)[row];
    *range_value = value;
    const pgaccel_grouped_agg_filter& filter = params.measure_filters[0];
    return value >= filter.predicate_lo[0].data.i32 && value <= filter.predicate_hi[0].data.i32
               ? FilterResult::Accept
               : FilterResult::Reject;
  }
}

inline bool atomic_increment_u32(uint32_t* value) {
  DeviceAtomic<uint32_t> count(*value);
  // Hash validation caps row_count at UINT32_MAX, and each row increments
  // exactly one slot once, so a slot can reach UINT32_MAX but cannot wrap.
  // A CAS retry loop turns hot groups into severe device-wide contention and
  // can trip Metal's command-buffer watchdog at the one-million-row H3 scale.
  return count.fetch_add(1) != UINT32_MAX;
}

inline uint64_t hash_u64_key(uint64_t value, bool is_null) {
  uint64_t mixed = is_null ? UINT64_C(0x6a09e667f3bcc909) : value;
  mixed += UINT64_C(0x9e3779b97f4a7c15);
  mixed = (mixed ^ (mixed >> 30)) * UINT64_C(0xbf58476d1ce4e5b9);
  mixed = (mixed ^ (mixed >> 27)) * UINT64_C(0x94d049bb133111eb);
  return mixed ^ (mixed >> 31);
}

inline bool load_hash_key(const pgaccel_grouped_agg_key& key, size_t row, bool* is_null,
                          uint64_t* value) {
  if (!null_at(key.values.nulls, row, is_null))
    return false;
  if (*is_null) {
    *value = 0;
    return true;
  }
  const uint64_t raw = static_cast<const uint64_t*>(key.values.values)[row];
  if ((key.flags & PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT) == 0) {
    *value = raw;
    return true;
  }
  if (!pgaccel_h3_exact::h3_is_valid_cell(raw))
    return false;
  *value = pgaccel_h3_exact::h3_cell_to_parent(raw, static_cast<int>(key._pad0));
  return *value != 0;
}

inline void run_hash_row(KernelParams* params_ptr, size_t row) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const pgaccel_grouped_agg_key& key = params.keys[0];
  bool row_null = false;
  uint64_t row_key = 0;
  if (!load_hash_key(key, row, &row_null, &row_key)) {
    record_failure(meta, kFailureInvalid);
    return;
  }
  const size_t mask = params.hash_slot_count - 1;
  const size_t start = static_cast<size_t>(hash_u64_key(row_key, row_null)) & mask;

  size_t count_slot = params.hash_slot_count;
  for (size_t probe = 0; probe < params.hash_slot_count; ++probe) {
    const size_t slot = (start + probe) & mask;
    DeviceAtomic<uint32_t> owner(params.hash_owners[slot]);
    uint32_t expected = kHashEmptyOwner;
    if (owner.compare_exchange_strong(expected, static_cast<uint32_t>(row))) {
      DeviceAtomic<uint32_t> emitted(*params.hash_group_count);
      const uint32_t ordinal = emitted.fetch_add(1);
      if (ordinal >= params.group_capacity)
        record_failure(meta, kFailureCapacity);
      count_slot = slot;
      break;
    }

    const uint32_t owner_row = expected;
    if (owner_row >= params.row_count) {
      record_failure(meta, kFailureInvalid);
      return;
    }
    bool owner_null = false;
    uint64_t owner_key = 0;
    if (!load_hash_key(key, static_cast<size_t>(owner_row), &owner_null, &owner_key)) {
      record_failure(meta, kFailureInvalid);
      return;
    }
    if (row_null != owner_null)
      continue;
    if (row_null || owner_key == row_key) {
      count_slot = slot;
      break;
    }
  }

  if (count_slot == params.hash_slot_count) {
    record_failure(meta, kFailureCapacity);
    return;
  }
  if (!atomic_increment_u32(&params.hash_counts[count_slot]))
    record_failure(meta, kFailureNumericOverflow);
}

inline bool hash_tuple_less(const uint64_t* keys, const uint8_t* nulls, size_t lhs, size_t rhs) {
  const bool lhs_null = nulls != nullptr && nulls[lhs] != 0;
  const bool rhs_null = nulls != nullptr && nulls[rhs] != 0;
  if (lhs_null != rhs_null)
    return lhs_null;
  return keys[lhs] < keys[rhs];
}

inline void swap_hash_tuple(uint64_t* keys, uint8_t* nulls, uint64_t* counts, size_t lhs,
                            size_t rhs) {
  const uint64_t key = keys[lhs];
  keys[lhs] = keys[rhs];
  keys[rhs] = key;
  if (nulls != nullptr) {
    const uint8_t is_null = nulls[lhs];
    nulls[lhs] = nulls[rhs];
    nulls[rhs] = is_null;
  }
  const uint64_t count = counts[lhs];
  counts[lhs] = counts[rhs];
  counts[rhs] = count;
}

inline void sift_hash_heap(uint64_t* keys, uint8_t* nulls, uint64_t* counts, size_t root,
                           size_t heap_size) {
  for (;;) {
    if (root >= heap_size / 2)
      return;
    size_t child = root * 2 + 1;
    if (child + 1 < heap_size && hash_tuple_less(keys, nulls, child, child + 1))
      ++child;
    if (!hash_tuple_less(keys, nulls, root, child))
      return;
    swap_hash_tuple(keys, nulls, counts, root, child);
    root = child;
  }
}

inline void sort_hash_output(uint64_t* keys, uint8_t* nulls, uint64_t* counts, size_t count) {
  if (count < 2)
    return;
  for (size_t root = count / 2; root != 0; --root)
    sift_hash_heap(keys, nulls, counts, root - 1, count);
  for (size_t heap_size = count; heap_size > 1; --heap_size) {
    swap_hash_tuple(keys, nulls, counts, 0, heap_size - 1);
    sift_hash_heap(keys, nulls, counts, 0, heap_size - 1);
  }
}

inline void run_hash_compact_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  meta.selected = params.row_count;
  if (meta.failure_flags != 0)
    return;
  uint64_t* staged_keys = reinterpret_cast<uint64_t*>(params.staged_key_values[0]);
  uint8_t* staged_nulls = params.staged_key_nulls[0];
  uint64_t* staged_counts = params.buffers[0].count;
  const pgaccel_grouped_agg_key& key = params.keys[0];
  const size_t emitted = *params.hash_group_count;
  meta.emitted = emitted;
  size_t output = 0;

  for (size_t slot = 0; slot < params.hash_slot_count; ++slot) {
    const uint32_t owner = params.hash_owners[slot];
    if (owner == kHashEmptyOwner)
      continue;
    if (owner >= params.row_count || output >= params.group_capacity) {
      meta.failure_flags |= kFailureInvalid;
      return;
    }
    bool is_null = false;
    uint64_t output_key = 0;
    if (!load_hash_key(key, static_cast<size_t>(owner), &is_null, &output_key)) {
      meta.failure_flags |= kFailureInvalid;
      return;
    }
    if (is_null && staged_nulls == nullptr) {
      meta.failure_flags |= kFailureInvalid;
      return;
    }
    staged_keys[output] = is_null ? 0 : output_key;
    if (staged_nulls != nullptr)
      staged_nulls[output] = is_null ? 1 : 0;
    staged_counts[output] = static_cast<uint64_t>(params.hash_counts[slot]);
    ++output;
  }
  if (output != emitted) {
    meta.failure_flags |= kFailureInvalid;
    return;
  }
  sort_hash_output(staged_keys, staged_nulls, staged_counts, output);
}

inline void run_hash_init_kernel(KernelParams* params_ptr) {
  *params_ptr->meta = {};
}

inline void run_dense_count_prepare_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const bool reset = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_RESET) != 0;

  if (reset) {
    meta = {};
    meta.lifecycle_cookie = kLifecycleCookie;
    meta.shape_fingerprint = params.shape_fingerprint;
    meta.lifecycle_state = kLifecycleActive;
    for (size_t group = 0; group < params.group_capacity; ++group) {
      params.active[group] = 0;
      params.buffers[0].count[group] = 0;
      if (params.buffers[0].nonnull != nullptr)
        params.buffers[0].nonnull[group] = 0;
    }
    if (params.key_count == 0)
      params.active[0] = 1;
    return;
  }

  if (meta.lifecycle_cookie != kLifecycleCookie ||
      meta.shape_fingerprint != params.shape_fingerprint ||
      meta.lifecycle_state != kLifecycleActive || meta.failure_flags != 0) {
    meta.failure_flags |= kFailureInvalid;
    meta.lifecycle_state = kLifecycleFailed;
  }
}

template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
inline void run_dense_count_row(KernelParams* params_ptr, size_t row) {
  KernelParams& params = *params_ptr;
  size_t group = 0;
  switch (specialized_dense_group_for_row<HasMembership>(params, row, &group)) {
    case DenseRowResult::Reject:
      return;
    case DenseRowResult::Error:
      record_failure(*params.meta, kFailureInvalid);
      return;
    case DenseRowResult::Accept:
      break;
  }
  const FilterResult filter = specialized_dense_where_filter<HasSqlMask>(params, row);
  if (filter == FilterResult::Error || filter == FilterResult::Uncertain) {
    record_failure(*params.meta, kFailureInvalid);
    return;
  }
  if (filter == FilterResult::Reject)
    return;

  bool is_null = false;
  if constexpr (CountNonNull) {
    if (!null_at(params.measures[0].value.nulls, row, &is_null)) {
      record_failure(*params.meta, kFailureInvalid);
      return;
    }
    DeviceAtomic<uint32_t> selected(params.dense_selected_counts[group]);
    if (selected.fetch_add(1) == UINT32_MAX)
      record_failure(*params.meta, kFailureNumericOverflow);
    if (is_null)
      return;
  }
  DeviceAtomic<uint32_t> count(params.dense_chunk_counts[group]);
  if (count.fetch_add(1) == UINT32_MAX)
    record_failure(*params.meta, kFailureNumericOverflow);
}

template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
inline void run_dense_count_workgroup(KernelParams* params_ptr, sycl::nd_item<1> item,
                                      const sycl::local_accessor<uint32_t, 1>& local_counts) {
  KernelParams& params = *params_ptr;
  const size_t local_id = item.get_local_linear_id();
  const size_t workgroup = item.get_group_linear_id();
  const size_t local_base = local_id * params.group_capacity;
  const size_t selected_base = kDenseReductionWorkgroupSize * params.group_capacity + local_base;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    local_counts[local_base + group] = 0;
    if constexpr (CountNonNull)
      local_counts[selected_base + group] = 0;
  }

  const size_t first_row =
      workgroup * kDenseIntegerChunkRows + local_id * kDenseReductionItemsPerWorkItem;
  const size_t end_row = std::min(first_row + kDenseReductionItemsPerWorkItem, params.row_count);
  for (size_t row = first_row; row < end_row; ++row) {
    size_t group = 0;
    const DenseRowResult grouped =
        specialized_dense_group_for_row<HasMembership>(params, row, &group);
    if (grouped == DenseRowResult::Error) {
      record_failure(*params.meta, kFailureInvalid);
      continue;
    }
    if (grouped == DenseRowResult::Reject)
      continue;
    const FilterResult filter = specialized_dense_where_filter<HasSqlMask>(params, row);
    if (filter == FilterResult::Error || filter == FilterResult::Uncertain) {
      record_failure(*params.meta, kFailureInvalid);
      continue;
    }
    if (filter != FilterResult::Accept)
      continue;
    if constexpr (CountNonNull) {
      bool is_null = false;
      if (!null_at(params.measures[0].value.nulls, row, &is_null)) {
        record_failure(*params.meta, kFailureInvalid);
        continue;
      }
      ++local_counts[selected_base + group];
      if (is_null)
        continue;
    }
    ++local_counts[local_base + group];
  }

  item.barrier(sycl::access::fence_space::local_space);
  for (size_t stride = 1; stride < kDenseReductionWorkgroupSize; stride *= 2) {
    if (local_id % (stride * 2) == 0) {
      const size_t rhs_base = (local_id + stride) * params.group_capacity;
      const size_t rhs_selected_base =
          kDenseReductionWorkgroupSize * params.group_capacity + rhs_base;
      for (size_t group = 0; group < params.group_capacity; ++group) {
        local_counts[local_base + group] += local_counts[rhs_base + group];
        if constexpr (CountNonNull)
          local_counts[selected_base + group] += local_counts[rhs_selected_base + group];
      }
    }
    item.barrier(sycl::access::fence_space::local_space);
  }
  if (local_id == 0) {
    for (size_t group = 0; group < params.group_capacity; ++group) {
      params.dense_chunk_counts[group * params.dense_count_chunk_count + workgroup] =
          local_counts[group];
      if constexpr (CountNonNull) {
        params.dense_selected_counts[group * params.dense_count_chunk_count + workgroup] =
            local_counts[kDenseReductionWorkgroupSize * params.group_capacity + group];
      }
    }
  }
}

inline void run_dense_count_commit_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const bool accumulate = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) != 0;
  const bool finalize = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) != 0;

  if (meta.failure_flags != 0) {
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }
  if (accumulate) {
    uint64_t selected = 0;
    for (size_t group = 0; group < params.group_capacity; ++group) {
      uint64_t chunk_count = 0;
      uint64_t chunk_selected = 0;
      if (params.dense_count_chunk_count == 0) {
        chunk_count = params.dense_chunk_counts[group];
        chunk_selected = params.dense_selected_counts == nullptr
                             ? chunk_count
                             : params.dense_selected_counts[group];
      } else {
        const size_t first = group * params.dense_count_chunk_count;
        for (size_t chunk = 0; chunk < params.dense_count_chunk_count; ++chunk) {
          chunk_count += params.dense_chunk_counts[first + chunk];
          chunk_selected += params.dense_selected_counts == nullptr
                                ? params.dense_chunk_counts[first + chunk]
                                : params.dense_selected_counts[first + chunk];
        }
      }
      if (!add_u64(selected, chunk_selected, &selected)) {
        meta.failure_flags = kFailureNumericOverflow;
        meta.lifecycle_state = kLifecycleFailed;
        return;
      }
      if (chunk_selected == 0)
        continue;
      uint64_t next = 0;
      if (!add_u64(params.buffers[0].count[group], chunk_count, &next)) {
        meta.failure_flags = kFailureNumericOverflow;
        meta.lifecycle_state = kLifecycleFailed;
        return;
      }
      params.buffers[0].count[group] = next;
      if (params.buffers[0].nonnull != nullptr) {
        if (!add_u64(params.buffers[0].nonnull[group], chunk_count, &next)) {
          meta.failure_flags = kFailureNumericOverflow;
          meta.lifecycle_state = kLifecycleFailed;
          return;
        }
        params.buffers[0].nonnull[group] = next;
      }
      params.active[group] = 1;
    }
    if (!add_u64(meta.selected, selected, &meta.selected)) {
      meta.failure_flags = kFailureNumericOverflow;
      meta.lifecycle_state = kLifecycleFailed;
      return;
    }
  }
  if (!finalize)
    return;

  meta.emitted = 0;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    stage_keys(params, group, group);
    if (params.active[group] != 0)
      ++meta.emitted;
  }
  meta.lifecycle_state = kLifecycleFinalized;
}

inline void run_dense_integer_prepare_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const bool reset = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_RESET) != 0;

  if (reset) {
    meta = {};
    meta.lifecycle_cookie = kLifecycleCookie;
    meta.shape_fingerprint = params.shape_fingerprint;
    meta.lifecycle_state = kLifecycleActive;
    for (size_t group = 0; group < params.group_capacity; ++group) {
      params.active[group] = 0;
      for (size_t measure = 0; measure < params.measure_count; ++measure) {
        DeviceMeasureBuffers& buffers = params.buffers[measure];
        if (buffers.sum != nullptr)
          buffers.sum[group] = 0;
        if (buffers.min != nullptr)
          buffers.min[group] = 0;
        if (buffers.max != nullptr)
          buffers.max[group] = 0;
        if (buffers.count != nullptr)
          buffers.count[group] = 0;
        if (buffers.nonnull != nullptr)
          buffers.nonnull[group] = 0;
      }
      if (params.dense_integer_atomic_sum != nullptr) {
        params.dense_integer_atomic_sum[group] = {};
        params.dense_integer_atomic_count[group] = {};
        if (params.dense_integer_atomic_nonnull != nullptr)
          params.dense_integer_atomic_nonnull[group] = {};
      }
    }
    if (params.key_count == 0)
      params.active[0] = 1;
    return;
  }

  if (meta.lifecycle_cookie != kLifecycleCookie ||
      meta.shape_fingerprint != params.shape_fingerprint ||
      meta.lifecycle_state != kLifecycleActive || meta.failure_flags != 0) {
    meta.failure_flags |= kFailureInvalid;
    meta.lifecycle_state = kLifecycleFailed;
  }
}

inline void atomic_add_u64(AtomicU64Words& target, uint64_t increment) {
  // AdaptiveCpp's Metal path does not provide a stable 64-bit fetch-add.
  // Adding the low word first and then its carry plus the high word is an
  // exact modular 64-bit sum because both 32-bit additions are commutative.
  const uint32_t low_increment = static_cast<uint32_t>(increment);
  DeviceAtomic<uint32_t> low(target.low);
  const uint32_t old_low = low.fetch_add(low_increment);
  const uint32_t carry = old_low > UINT32_MAX - low_increment ? 1 : 0;
  DeviceAtomic<uint32_t> high(target.high);
  high.fetch_add(static_cast<uint32_t>(increment >> 32) + carry);
}

inline uint64_t atomic_u64_value(const AtomicU64Words& value) {
  return static_cast<uint64_t>(value.low) | (static_cast<uint64_t>(value.high) << 32);
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
inline void run_dense_integer_atomic_row(KernelParams* params_ptr, size_t row) {
  KernelParams& params = *params_ptr;
  size_t group = 0;
  switch (specialized_dense_group_for_row<HasMembership>(params, row, &group)) {
    case DenseRowResult::Reject:
      return;
    case DenseRowResult::Error:
      record_failure(*params.meta, kFailureInvalid);
      return;
    case DenseRowResult::Accept:
      break;
  }
  int32_t range_value = 0;
  const FilterResult filter =
      specialized_dense_integer_where_filter<HasSqlMask, HasScalarRange>(params, row, &range_value);
  if (filter == FilterResult::Error || filter == FilterResult::Uncertain) {
    record_failure(*params.meta, kFailureInvalid);
    return;
  }
  if (filter == FilterResult::Reject)
    return;

  atomic_add_u64(params.dense_integer_atomic_count[group], 1);

  int32_t measure_range_value = 0;
  const FilterResult measure_filter =
      specialized_dense_integer_measure_filter<HasMeasureRange>(params, row, &measure_range_value);
  if (measure_filter == FilterResult::Error || measure_filter == FilterResult::Uncertain) {
    record_failure(*params.meta, kFailureInvalid);
    return;
  }
  if (measure_filter == FilterResult::Reject)
    return;

  const pgaccel_grouped_agg_measure& measure = params.measures[0];
  bool lhs_null = false;
  if constexpr (!HasScalarRange && !HasMeasureRange) {
    if (!null_at(measure.value.nulls, row, &lhs_null)) {
      record_failure(*params.meta, kFailureInvalid);
      return;
    }
  }
  bool rhs_null = false;
  if constexpr (Multiply) {
    if (!null_at(measure.rhs.nulls, row, &rhs_null)) {
      record_failure(*params.meta, kFailureInvalid);
      return;
    }
  }
  if (lhs_null || rhs_null)
    return;

  int64_t value = HasScalarRange    ? range_value
                  : HasMeasureRange ? measure_range_value
                                    : static_cast<const int32_t*>(measure.value.values)[row];
  if constexpr (Multiply) {
    const int64_t rhs = static_cast<const int32_t*>(measure.rhs.values)[row];
    value *= rhs;
    if (value < std::numeric_limits<int32_t>::min() ||
        value > std::numeric_limits<int32_t>::max()) {
      record_failure(*params.meta, kFailureNumericOverflow);
      return;
    }
  }

  // This path is capped at one million rows. Every accepted term is int4, so
  // its absolute SUM is below INT64_MAX and modular atomic addition is exact.
  atomic_add_u64(params.dense_integer_atomic_sum[group], sycl::bit_cast<uint64_t>(value));
  const bool statically_nonnull = !HasMeasureRange && measure.value.nulls == nullptr &&
                                  (!Multiply || measure.rhs.nulls == nullptr);
  if (!statically_nonnull)
    atomic_add_u64(params.dense_integer_atomic_nonnull[group], 1);
}

inline void run_dense_integer_atomic_commit_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  if (meta.failure_flags != 0) {
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }

  const pgaccel_grouped_agg_measure& measure = params.measures[0];
  const bool statically_nonnull =
      measure.value.nulls == nullptr &&
      (measure.op != PGACCEL_GROUPED_AGG_MEASURE_MUL || measure.rhs.nulls == nullptr) &&
      params.measure_filters[0].predicate_range_count == 0;
  uint64_t selected = 0;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    const uint64_t count = atomic_u64_value(params.dense_integer_atomic_count[group]);
    params.buffers[0].sum[group] = atomic_u64_value(params.dense_integer_atomic_sum[group]);
    params.buffers[0].nonnull[group] =
        statically_nonnull ? count : atomic_u64_value(params.dense_integer_atomic_nonnull[group]);
    params.buffers[1].count[group] = count;
    if (!add_u64(selected, count, &selected)) {
      meta.failure_flags = kFailureNumericOverflow;
      meta.lifecycle_state = kLifecycleFailed;
      return;
    }
    if (count != 0)
      params.active[group] = 1;
  }
  if (!add_u64(meta.selected, selected, &meta.selected)) {
    meta.failure_flags = kFailureNumericOverflow;
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }

  meta.emitted = 0;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    stage_keys(params, group, group);
    if (params.active[group] != 0)
      ++meta.emitted;
  }
  meta.lifecycle_state = kLifecycleFinalized;
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
inline void run_dense_integer_chunk(KernelParams* params_ptr, size_t chunk) {
  KernelParams& params = *params_ptr;
  const size_t chunk_count = params.dense_integer_chunk_count;
  const size_t first_row = chunk * kDenseIntegerChunkRows;
  const size_t remaining = params.row_count - first_row;
  const size_t chunk_rows = remaining < kDenseIntegerChunkRows ? remaining : kDenseIntegerChunkRows;

  for (size_t offset = 0; offset < chunk_rows; ++offset) {
    const size_t row = first_row + offset;
    size_t row_group = 0;
    switch (specialized_dense_group_for_row<HasMembership>(params, row, &row_group)) {
      case DenseRowResult::Reject:
        continue;
      case DenseRowResult::Error:
        record_failure(*params.meta, kFailureInvalid);
        return;
      case DenseRowResult::Accept:
        break;
    }
    int32_t range_value = 0;
    const FilterResult filter = specialized_dense_integer_where_filter<HasSqlMask, HasScalarRange>(
        params, row, &range_value);
    if (filter == FilterResult::Error || filter == FilterResult::Uncertain) {
      record_failure(*params.meta, kFailureInvalid);
      return;
    }
    if (filter == FilterResult::Reject)
      continue;
    DenseIntegerPartial& partial = params.dense_integer_partials[row_group * chunk_count + chunk];
    ++partial.rows;

    int32_t measure_range_value = 0;
    const FilterResult measure_filter = specialized_dense_integer_measure_filter<HasMeasureRange>(
        params, row, &measure_range_value);
    if (measure_filter == FilterResult::Error || measure_filter == FilterResult::Uncertain) {
      record_failure(*params.meta, kFailureInvalid);
      return;
    }
    if (measure_filter == FilterResult::Reject)
      continue;

    const pgaccel_grouped_agg_measure& measure = params.measures[0];
    bool lhs_null = false;
    if constexpr (!HasScalarRange && !HasMeasureRange) {
      if (!null_at(measure.value.nulls, row, &lhs_null)) {
        record_failure(*params.meta, kFailureInvalid);
        return;
      }
    }
    bool rhs_null = false;
    if constexpr (Multiply) {
      if (!null_at(measure.rhs.nulls, row, &rhs_null)) {
        record_failure(*params.meta, kFailureInvalid);
        return;
      }
    }
    if (lhs_null || rhs_null)
      continue;
    int64_t value = HasScalarRange    ? range_value
                    : HasMeasureRange ? measure_range_value
                                      : static_cast<const int32_t*>(measure.value.values)[row];
    if constexpr (Multiply) {
      const int64_t rhs = static_cast<const int32_t*>(measure.rhs.values)[row];
      value *= rhs;
      // PostgreSQL evaluates int4 * int4 before widening the SUM transition.
      if (value < std::numeric_limits<int32_t>::min() ||
          value > std::numeric_limits<int32_t>::max()) {
        record_failure(*params.meta, kFailureNumericOverflow);
        return;
      }
    }
    int64_t next_sum = 0;
    if (!add_i64(partial.sum, value, &next_sum)) {
      record_failure(*params.meta, kFailureNumericOverflow);
      return;
    }
    partial.sum = next_sum;
    if (partial.sum < partial.prefix_min)
      partial.prefix_min = partial.sum;
    if (partial.sum > partial.prefix_max)
      partial.prefix_max = partial.sum;
    if (partial.nonnull == 0 || value < partial.min)
      partial.min = value;
    if (partial.nonnull == 0 || value > partial.max)
      partial.max = value;
    ++partial.nonnull;
  }
}

inline void merge_ordered_dense_integer_partial(DenseIntegerPartial* left,
                                                const DenseIntegerPartial& right) {
  left->failure_flags |= right.failure_flags;
  if (left->rows > UINT32_MAX - right.rows || left->nonnull > UINT32_MAX - right.nonnull) {
    left->failure_flags |= kFailureNumericOverflow;
    return;
  }
  left->rows += right.rows;
  if (right.nonnull == 0)
    return;

  const int64_t left_sum = left->sum;
  int64_t shifted_prefix_min = 0;
  int64_t shifted_prefix_max = 0;
  int64_t combined_sum = 0;
  if (!add_i64(left_sum, right.prefix_min, &shifted_prefix_min) ||
      !add_i64(left_sum, right.prefix_max, &shifted_prefix_max) ||
      !add_i64(left_sum, right.sum, &combined_sum)) {
    left->failure_flags |= kFailureNumericOverflow;
    return;
  }
  left->prefix_min = std::min(left->prefix_min, shifted_prefix_min);
  left->prefix_max = std::max(left->prefix_max, shifted_prefix_max);
  if (left->nonnull == 0 || right.min < left->min)
    left->min = right.min;
  if (left->nonnull == 0 || right.max > left->max)
    left->max = right.max;
  left->sum = combined_sum;
  left->nonnull += right.nonnull;
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
inline void
run_dense_integer_workgroup(KernelParams* params_ptr, sycl::nd_item<1> item,
                            const sycl::local_accessor<DenseIntegerPartial, 1>& local_partials) {
  KernelParams& params = *params_ptr;
  const size_t local_id = item.get_local_linear_id();
  const size_t workgroup = item.get_group_linear_id();
  const size_t local_base = local_id * params.group_capacity;
  for (size_t group = 0; group < params.group_capacity; ++group)
    local_partials[local_base + group] = {};

  const size_t first_row =
      workgroup * kDenseIntegerChunkRows + local_id * kDenseReductionItemsPerWorkItem;
  const size_t end_row = std::min(first_row + kDenseReductionItemsPerWorkItem, params.row_count);
  for (size_t row = first_row; row < end_row; ++row) {
    size_t group = 0;
    const DenseRowResult grouped =
        specialized_dense_group_for_row<HasMembership>(params, row, &group);
    if (grouped == DenseRowResult::Error) {
      record_failure(*params.meta, kFailureInvalid);
      continue;
    }
    if (grouped == DenseRowResult::Reject)
      continue;
    int32_t range_value = 0;
    const FilterResult filter = specialized_dense_integer_where_filter<HasSqlMask, HasScalarRange>(
        params, row, &range_value);
    if (filter == FilterResult::Error || filter == FilterResult::Uncertain) {
      record_failure(*params.meta, kFailureInvalid);
      continue;
    }
    if (filter == FilterResult::Reject)
      continue;

    DenseIntegerPartial& partial = local_partials[local_base + group];
    ++partial.rows;
    int32_t measure_range_value = 0;
    const FilterResult measure_filter = specialized_dense_integer_measure_filter<HasMeasureRange>(
        params, row, &measure_range_value);
    if (measure_filter == FilterResult::Error || measure_filter == FilterResult::Uncertain) {
      partial.failure_flags |= kFailureInvalid;
      continue;
    }
    if (measure_filter == FilterResult::Reject)
      continue;
    const pgaccel_grouped_agg_measure& measure = params.measures[0];
    bool lhs_null = false;
    if constexpr (!HasScalarRange && !HasMeasureRange) {
      if (!null_at(measure.value.nulls, row, &lhs_null)) {
        partial.failure_flags |= kFailureInvalid;
        continue;
      }
    }
    bool rhs_null = false;
    if constexpr (Multiply) {
      if (!null_at(measure.rhs.nulls, row, &rhs_null)) {
        partial.failure_flags |= kFailureInvalid;
        continue;
      }
    }
    if (lhs_null || rhs_null)
      continue;

    int64_t value = HasScalarRange    ? range_value
                    : HasMeasureRange ? measure_range_value
                                      : static_cast<const int32_t*>(measure.value.values)[row];
    if constexpr (Multiply) {
      const int64_t rhs = static_cast<const int32_t*>(measure.rhs.values)[row];
      value *= rhs;
      if (value < std::numeric_limits<int32_t>::min() ||
          value > std::numeric_limits<int32_t>::max()) {
        partial.failure_flags |= kFailureNumericOverflow;
        continue;
      }
    }
    int64_t next_sum = 0;
    if (!add_i64(partial.sum, value, &next_sum)) {
      partial.failure_flags |= kFailureNumericOverflow;
      continue;
    }
    partial.sum = next_sum;
    partial.prefix_min = std::min(partial.prefix_min, partial.sum);
    partial.prefix_max = std::max(partial.prefix_max, partial.sum);
    if (partial.nonnull == 0 || value < partial.min)
      partial.min = value;
    if (partial.nonnull == 0 || value > partial.max)
      partial.max = value;
    ++partial.nonnull;
  }

  item.barrier(sycl::access::fence_space::local_space);
  for (size_t stride = 1; stride < kDenseReductionWorkgroupSize; stride *= 2) {
    if (local_id % (stride * 2) == 0) {
      const size_t rhs_base = (local_id + stride) * params.group_capacity;
      for (size_t group = 0; group < params.group_capacity; ++group) {
        merge_ordered_dense_integer_partial(&local_partials[local_base + group],
                                            local_partials[rhs_base + group]);
      }
    }
    item.barrier(sycl::access::fence_space::local_space);
  }
  if (local_id == 0) {
    for (size_t group = 0; group < params.group_capacity; ++group) {
      params.dense_integer_partials[group * params.dense_integer_chunk_count + workgroup] =
          local_partials[group];
    }
  }
}

inline void run_dense_integer_reduce(KernelParams* params_ptr, size_t group) {
  KernelParams& params = *params_ptr;
  DeviceMeasureBuffers& value_buffers = params.buffers[0];
  DeviceMeasureBuffers& count_buffers = params.buffers[1];
  int64_t sum = sycl::bit_cast<int64_t>(value_buffers.sum[group]);
  const bool tracks_min = value_buffers.min != nullptr;
  const bool tracks_max = value_buffers.max != nullptr;
  int64_t min = tracks_min ? sycl::bit_cast<int64_t>(value_buffers.min[group]) : 0;
  int64_t max = tracks_max ? sycl::bit_cast<int64_t>(value_buffers.max[group]) : 0;
  uint64_t nonnull = value_buffers.nonnull[group];
  uint64_t count = count_buffers.count[group];
  bool active = params.active[group] != 0;
  const size_t first_partial = group * params.dense_integer_chunk_count;

  for (size_t chunk = 0; chunk < params.dense_integer_chunk_count; ++chunk) {
    const DenseIntegerPartial& partial = params.dense_integer_partials[first_partial + chunk];
    if (partial.failure_flags != 0) {
      record_failure(*params.meta, partial.failure_flags);
      return;
    }
    active |= partial.rows != 0;
    uint64_t next_count = 0;
    if (!add_u64(count, partial.rows, &next_count)) {
      record_failure(*params.meta, kFailureNumericOverflow);
      return;
    }
    count = next_count;
    if (partial.nonnull == 0)
      continue;

    int64_t ignored = 0;
    int64_t next_sum = 0;
    uint64_t next_nonnull = 0;
    if (!add_i64(sum, partial.prefix_min, &ignored) ||
        !add_i64(sum, partial.prefix_max, &ignored) || !add_i64(sum, partial.sum, &next_sum) ||
        !add_u64(nonnull, partial.nonnull, &next_nonnull)) {
      record_failure(*params.meta, kFailureNumericOverflow);
      return;
    }
    sum = next_sum;
    if (tracks_min && (nonnull == 0 || partial.min < min))
      min = partial.min;
    if (tracks_max && (nonnull == 0 || partial.max > max))
      max = partial.max;
    nonnull = next_nonnull;
  }

  value_buffers.sum[group] = sycl::bit_cast<uint64_t>(sum);
  if (tracks_min)
    value_buffers.min[group] = sycl::bit_cast<uint64_t>(min);
  if (tracks_max)
    value_buffers.max[group] = sycl::bit_cast<uint64_t>(max);
  value_buffers.nonnull[group] = nonnull;
  count_buffers.count[group] = count;
  params.active[group] = active ? 1 : 0;
}

inline void run_dense_integer_commit_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const bool accumulate = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) != 0;
  const bool finalize = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) != 0;

  if (meta.failure_flags != 0) {
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }
  if (accumulate) {
    uint64_t selected = 0;
    const size_t partial_count = params.group_capacity * params.dense_integer_chunk_count;
    for (size_t partial = 0; partial < partial_count; ++partial) {
      if (!add_u64(selected, params.dense_integer_partials[partial].rows, &selected)) {
        meta.failure_flags = kFailureNumericOverflow;
        meta.lifecycle_state = kLifecycleFailed;
        return;
      }
    }
    if (!add_u64(meta.selected, selected, &meta.selected)) {
      meta.failure_flags = kFailureNumericOverflow;
      meta.lifecycle_state = kLifecycleFailed;
      return;
    }
  }
  if (!finalize)
    return;

  meta.emitted = 0;
  for (size_t group = 0; group < params.group_capacity; ++group) {
    stage_keys(params, group, group);
    if (params.active[group] != 0)
      ++meta.emitted;
  }
  meta.lifecycle_state = kLifecycleFinalized;
}

inline void run_dense_kernel(KernelParams* params_ptr) {
  KernelParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  const bool reset = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_RESET) != 0;
  const bool accumulate = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) != 0;
  const bool finalize = (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) != 0;

  if (reset) {
    meta = {};
    meta.lifecycle_cookie = kLifecycleCookie;
    meta.shape_fingerprint = params.shape_fingerprint;
    meta.lifecycle_state = kLifecycleActive;
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
  } else if (meta.lifecycle_cookie != kLifecycleCookie ||
             meta.shape_fingerprint != params.shape_fingerprint ||
             meta.lifecycle_state != kLifecycleActive || meta.failure_flags != 0) {
    meta.failure_flags |= kFailureInvalid;
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }

  for (size_t row = 0; accumulate && row < params.row_count && meta.failure_flags == 0; ++row) {
    size_t dim_indexes[PGACCEL_GROUPED_AGG_MAX_DIMS] = {};
    uint64_t weight = 1;
    bool rejected = false;
    for (size_t d = 0; d < params.dim_count; ++d) {
      const pgaccel_grouped_agg_dim& dim = params.dims[d];
      bool is_null = false;
      if (!null_at(dim.fact_key.nulls, row, &is_null)) {
        meta.failure_flags = kFailureInvalid;
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
          meta.failure_flags = kFailureInvalid;
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
        meta.failure_flags = kFailureNumericOverflow;
        break;
      }
    }
    if (meta.failure_flags != 0 || rejected)
      continue;

    size_t group = 0;
    for (size_t k = 0; k < params.key_count; ++k) {
      const pgaccel_grouped_agg_key& key = params.keys[k];
      int32_t raw = 0;
      if (key.source == PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT) {
        bool is_null = false;
        if (!null_at(key.values.nulls, row, &is_null)) {
          meta.failure_flags = kFailureInvalid;
          break;
        }
        raw = is_null ? key.null_code : static_cast<const int32_t*>(key.values.values)[row];
      } else {
        const size_t dim = static_cast<size_t>(key.source - PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0);
        const pgaccel_grouped_agg_dim& source_dim = params.dims[dim];
        if (source_dim.multiplicity_by_key != nullptr &&
            source_dim.multiplicity_by_key[dim_indexes[dim]] != 1) {
          meta.failure_flags = kFailureInvalid;
          break;
        }
        raw = key.lookup_by_key[dim_indexes[dim]];
      }
      const int64_t digit = static_cast<int64_t>(raw) - key.code_min;
      if (digit < 0 || static_cast<uint64_t>(digit) >= key.cardinality) {
        meta.failure_flags = kFailureInvalid;
        break;
      }
      // Descriptor validation proves the complete radix product fits size_t.
      group = group * key.cardinality + static_cast<size_t>(digit);
    }
    if (meta.failure_flags != 0)
      break;

    const FilterResult where = evaluate_filter(params.where_filter, params, row);
    if (where == FilterResult::Error) {
      meta.failure_flags = kFailureInvalid;
      break;
    }
    if (where == FilterResult::Uncertain) {
      if (!add_u64(meta.uncertain, 1, &meta.uncertain))
        meta.failure_flags = kFailureNumericOverflow;
      continue;
    }
    if (where == FilterResult::Reject)
      continue;
    if (!add_u64(meta.selected, weight, &meta.selected)) {
      meta.failure_flags = kFailureNumericOverflow;
      break;
    }
    params.active[group] = 1;

    bool row_uncertain = false;
    for (size_t m = 0; m < params.measure_count; ++m) {
      const FilterResult filter = evaluate_filter(params.measure_filters[m], params, row);
      if (filter == FilterResult::Error) {
        meta.failure_flags = kFailureInvalid;
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
        meta.failure_flags = accumulate_count(buffers.count, group, weight);
        continue;
      }
      const uint32_t primary_mask = PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_MIN |
                                    PGACCEL_GROUPED_AGG_LANE_MAX | PGACCEL_GROUPED_AGG_LANE_COUNT |
                                    PGACCEL_GROUPED_AGG_LANE_SUMSQ;
      auto measure_error = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
      if ((measure.agg_mask & primary_mask) != 0) {
        measure_error = measure.accumulator_kind == PGACCEL_GROUPED_AGG_ACCUM_I64
                            ? accumulate_i64(measure, buffers, row, group, weight)
                            : accumulate_f64(measure, buffers, row, group, weight);
      }
      if (measure_error == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE &&
          (measure.agg_mask &
           (PGACCEL_GROUPED_AGG_LANE_RHS_SUM | PGACCEL_GROUPED_AGG_LANE_RHS_COUNT)) != 0)
        measure_error = accumulate_rhs(measure, buffers, row, group, weight);
      if (measure_error != PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE) {
        meta.failure_flags = measure_error;
        break;
      }
    }
    if (row_uncertain && !add_u64(meta.uncertain, 1, &meta.uncertain))
      meta.failure_flags = kFailureNumericOverflow;
  }

  if (meta.failure_flags != 0) {
    meta.lifecycle_state = kLifecycleFailed;
    return;
  }
  if (!finalize)
    return;
  meta.emitted = 0;
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
  meta.lifecycle_state = kLifecycleFinalized;
}

inline void set_completion_copy(DeviceCompletion& completion, size_t slot, size_t source_offset,
                                bool destination_present, size_t count, size_t width) {
  if (source_offset == kNoOffset || !destination_present || count == 0)
    return;
  DeviceCopyCommand& command = completion.commands[slot];
  command.source_offset = source_offset;
  command.bytes = count * width;
}

inline DeviceCompletion build_completion(DevicePublishParams* params_ptr) {
  DevicePublishParams& params = *params_ptr;
  DeviceMeta& meta = *params.meta;
  DeviceCompletion completion{};

  const uint32_t known_failures = kFailureInvalid | kFailureNumericOverflow | kFailureCapacity;
  if ((meta.failure_flags & ~known_failures) != 0 || (meta.failure_flags & kFailureInvalid) != 0) {
    completion.status = PGACCEL_ERROR;
    completion.detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  } else if ((meta.failure_flags & kFailureNumericOverflow) != 0) {
    completion.status = PGACCEL_ERROR;
    completion.detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW;
  } else if ((meta.failure_flags & kFailureCapacity) != 0) {
    completion.status = PGACCEL_UNSUPPORTED;
    completion.detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  } else {
    completion.status = PGACCEL_OK;
    completion.detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  }

  set_completion_copy(completion, kPublishDetail,
                      params.source_offsets.completion + offsetof(DeviceCompletion, detail),
                      params.output.detail, 1, sizeof(completion.detail));
  if (completion.status != PGACCEL_OK ||
      (params.execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) == 0)
    return completion;

  completion.emitted = meta.emitted;
  completion.selected = meta.selected;
  completion.uncertain = meta.uncertain;
  const size_t count = params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
                           ? completion.emitted
                           : params.group_capacity;
  if (params.output_mode == PGACCEL_GROUPED_AGG_OUTPUT_DENSE) {
    set_completion_copy(completion, kPublishActive, params.source_offsets.active,
                        params.output.active_groups, params.group_capacity, sizeof(uint8_t));
  }
  set_completion_copy(completion, kPublishGroupCodes, params.source_offsets.group_codes,
                      params.output.group_codes, count, sizeof(size_t));

  for (size_t key = 0; key < params.key_count; ++key) {
    const size_t slot = kPublishKeys + key * kPublishKeyLaneCount;
    const size_t width = val_tag_width(params.key_types[key]);
    set_completion_copy(completion, slot, params.source_offsets.key_values[key],
                        params.output.key_values[key], count, width);
    set_completion_copy(completion, slot + 1, params.source_offsets.key_nulls[key],
                        params.output.key_nulls[key], count, sizeof(uint8_t));
  }

  for (size_t measure = 0; measure < params.measure_count; ++measure) {
    const size_t slot = kPublishMeasures + measure * kPublishMeasureLaneCount;
    const uint8_t* destinations = params.output.measure_lanes[measure];
    const size_t state_bytes = params.measure_state_bytes[measure];
    const size_t* offsets = params.source_offsets.measure_lanes[measure];
    set_completion_copy(completion, slot, offsets[0], destinations[0], count, state_bytes);
    set_completion_copy(completion, slot + 1, offsets[1], destinations[1], count, state_bytes);
    set_completion_copy(completion, slot + 2, offsets[2], destinations[2], count, state_bytes);
    set_completion_copy(completion, slot + 3, offsets[3], destinations[3], count, state_bytes);
    set_completion_copy(completion, slot + 4, offsets[4], destinations[4], count, sizeof(uint64_t));
    set_completion_copy(completion, slot + 5, offsets[5], destinations[5], count, sizeof(uint64_t));
    set_completion_copy(completion, slot + 6, offsets[6], destinations[6], count, state_bytes);
    set_completion_copy(completion, slot + 7, offsets[7], destinations[7], count, sizeof(uint64_t));
    set_completion_copy(completion, slot + 8, offsets[8], destinations[8], count, sizeof(uint64_t));
  }

  set_completion_copy(completion, kPublishEmitted,
                      params.source_offsets.completion + offsetof(DeviceCompletion, emitted),
                      params.output.emitted, 1, sizeof(completion.emitted));
  set_completion_copy(completion, kPublishSelected,
                      params.source_offsets.completion + offsetof(DeviceCompletion, selected),
                      params.output.selected, 1, sizeof(completion.selected));
  set_completion_copy(completion, kPublishUncertain,
                      params.source_offsets.completion + offsetof(DeviceCompletion, uncertain),
                      params.output.uncertain, 1, sizeof(completion.uncertain));
  return completion;
}

template <typename T>
T* arena_ptr(void* base, size_t offset) {
  return offset == kNoOffset ? nullptr : reinterpret_cast<T*>(static_cast<uint8_t*>(base) + offset);
}

void bind_params(const pgaccel_grouped_agg_desc& desc, const WorkspaceLayout& layout,
                 void* const scratch, KernelParams* params) {
  *params = {};
  params->row_count = desc.row_count;
  params->group_capacity = desc.group_capacity;
  params->shape_fingerprint = descriptor_shape_fingerprint(desc);
  params->key_count = desc.key_count;
  params->measure_count = desc.measure_count;
  params->dim_count = desc.dim_count;
  params->execution_flags = desc.execution_flags;
  params->grouping_mode = desc.grouping_mode;
  params->output_mode = desc.output_mode;
  params->hash_slot_count = layout.hash_slot_count;
  std::memcpy(params->keys, desc.keys, sizeof(params->keys));
  std::memcpy(params->measures, desc.measures, sizeof(params->measures));
  params->where_filter = desc.where_filter;
  std::memcpy(params->measure_filters, desc.measure_filters, sizeof(params->measure_filters));
  std::memcpy(params->dims, desc.dims, sizeof(params->dims));
  params->active = arena_ptr<uint8_t>(scratch, layout.active);
  params->staged_group_codes = arena_ptr<size_t>(scratch, layout.staged_group_codes);
  params->hash_owners = arena_ptr<uint32_t>(scratch, layout.hash_owners);
  params->hash_counts = arena_ptr<uint32_t>(scratch, layout.hash_counts);
  params->hash_group_count = arena_ptr<uint32_t>(scratch, layout.hash_group_count);
  params->dense_chunk_counts = arena_ptr<uint32_t>(scratch, layout.dense_chunk_counts);
  params->dense_selected_counts = arena_ptr<uint32_t>(scratch, layout.dense_selected_counts);
  params->dense_count_chunk_count =
      layout.dense_count_hierarchical ? layout.dense_count_chunk_count : 0;
  params->dense_integer_atomic_sum =
      arena_ptr<AtomicU64Words>(scratch, layout.dense_integer_atomic_sum);
  params->dense_integer_atomic_nonnull =
      arena_ptr<AtomicU64Words>(scratch, layout.dense_integer_atomic_nonnull);
  params->dense_integer_atomic_count =
      arena_ptr<AtomicU64Words>(scratch, layout.dense_integer_atomic_count);
  params->dense_integer_partials =
      arena_ptr<DenseIntegerPartial>(scratch, layout.dense_integer_partials);
  params->dense_integer_chunk_count = layout.dense_integer_chunk_count;
  params->meta = arena_ptr<DeviceMeta>(scratch, layout.meta);
  for (size_t k = 0; k < desc.key_count; ++k) {
    params->staged_key_values[k] = arena_ptr<uint8_t>(scratch, layout.staged_key_values[k]);
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

void bind_publish_params(const pgaccel_grouped_agg_desc& desc, const WorkspaceLayout& layout,
                         void* const scratch, uint64_t output_mask, DevicePublishParams* params) {
  *params = {};
  params->group_capacity = desc.group_capacity;
  params->execution_flags = desc.execution_flags;
  params->key_count = desc.key_count;
  params->measure_count = desc.measure_count;
  params->output_mode = desc.output_mode;
  params->meta = arena_ptr<DeviceMeta>(scratch, layout.meta);
  params->completion = static_cast<DeviceCompletion*>(scratch);
  params->source_offsets.completion = layout.completion;
  params->source_offsets.active = layout.active;
  params->source_offsets.group_codes = layout.staged_group_codes;
  params->output.detail = (output_mask & publish_bit(kPublishDetail)) != 0;
  params->output.active_groups = (output_mask & publish_bit(kPublishActive)) != 0;
  params->output.group_codes = (output_mask & publish_bit(kPublishGroupCodes)) != 0;
  params->output.emitted = (output_mask & publish_bit(kPublishEmitted)) != 0;
  params->output.selected = (output_mask & publish_bit(kPublishSelected)) != 0;
  params->output.uncertain = (output_mask & publish_bit(kPublishUncertain)) != 0;
  for (size_t key = 0; key < PGACCEL_GROUPED_AGG_MAX_KEYS; ++key) {
    params->output.key_values[key] =
        (output_mask & publish_bit(kPublishKeys + key * kPublishKeyLaneCount)) != 0;
    params->output.key_nulls[key] =
        (output_mask & publish_bit(kPublishKeys + key * kPublishKeyLaneCount + 1)) != 0;
  }
  for (size_t measure = 0; measure < PGACCEL_GROUPED_AGG_MAX_MEASURES; ++measure) {
    for (size_t lane = 0; lane < kPublishMeasureLaneCount; ++lane) {
      params->output.measure_lanes[measure][lane] =
          (output_mask &
           publish_bit(kPublishMeasures + measure * kPublishMeasureLaneCount + lane)) != 0;
    }
  }
  for (size_t key = 0; key < desc.key_count; ++key) {
    params->key_types[key] = materialized_key_type(desc, desc.keys[key]);
    params->source_offsets.key_values[key] = layout.staged_key_values[key];
    params->source_offsets.key_nulls[key] = layout.staged_key_nulls[key];
  }
  for (size_t measure = 0; measure < desc.measure_count; ++measure) {
    params->measure_state_bytes[measure] = desc.measures[measure].state_bytes;
    const MeasureLayout& ml = layout.measures[measure];
    size_t* offsets = params->source_offsets.measure_lanes[measure];
    offsets[0] = ml.sum;
    offsets[1] = ml.min;
    offsets[2] = ml.max;
    offsets[3] = ml.sumsq;
    offsets[4] = ml.count;
    offsets[5] = ml.nonnull;
    offsets[6] = ml.rhs_sum;
    offsets[7] = ml.rhs_count;
    offsets[8] = ml.rhs_nonnull;
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

bool add_control_bytes(SpanList* spans, const void* control, size_t begin, size_t end) {
  if (begin > end)
    return false;
  if (begin == end)
    return true;
  return add_span(spans, static_cast<const uint8_t*>(control) + begin, end - begin,
                  sizeof(uint8_t));
}

bool validate_aliases(const pgaccel_grouped_agg_desc& desc, const pgaccel_grouped_agg_out& out,
                      const pgaccel_grouped_agg_desc* const descriptor_control,
                      pgaccel_grouped_agg_out* const output_control, int32_t* const detail) {
  SpanList inputs;
  SpanList outputs;
  constexpr size_t emitted_begin = offsetof(pgaccel_grouped_agg_out, emitted_group_count);
  constexpr size_t emitted_end = emitted_begin + sizeof(output_control->emitted_group_count);
  constexpr size_t selected_begin = offsetof(pgaccel_grouped_agg_out, selected_count);
  constexpr size_t selected_end = selected_begin + sizeof(output_control->selected_count);
  constexpr size_t uncertain_begin = offsetof(pgaccel_grouped_agg_out, uncertain_count);
  constexpr size_t uncertain_end = uncertain_begin + sizeof(output_control->uncertain_count);
  if (descriptor_control == nullptr || output_control == nullptr || detail == nullptr ||
      !collect_input_spans(desc, &inputs) ||
      !add_span(&inputs, descriptor_control, 1, sizeof(*descriptor_control)) ||
      !add_control_bytes(&inputs, output_control, 0, emitted_begin) ||
      !add_control_bytes(&inputs, output_control, emitted_end, selected_begin) ||
      !add_control_bytes(&inputs, output_control, selected_end, uncertain_begin) ||
      !add_control_bytes(&inputs, output_control, uncertain_end, sizeof(*output_control)) ||
      !collect_output_spans(desc, out, &outputs) ||
      !add_span(&outputs, &output_control->emitted_group_count, 1,
                sizeof(output_control->emitted_group_count)) ||
      !add_span(&outputs, &output_control->selected_count, 1,
                sizeof(output_control->selected_count)) ||
      !add_span(&outputs, &output_control->uncertain_count, 1,
                sizeof(output_control->uncertain_count)) ||
      !add_span(&outputs, detail, 1, sizeof(*detail)))
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

pgaccel_grouped_agg_out snapshot_output_contract(const pgaccel_grouped_agg_out& out) {
  return out;
}

bool same_queue_device(sycl::queue& queue, const void* ptr) {
  try {
    return sycl::get_pointer_device(ptr, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

bool device_accessible(sycl::queue& queue, const void* ptr) {
  if (ptr == nullptr)
    return true;
  const sycl::usm::alloc type = sycl::get_pointer_type(ptr, queue.get_context());
  return (type == sycl::usm::alloc::device || type == sycl::usm::alloc::shared) &&
         same_queue_device(queue, ptr);
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
      if (actual != sycl::usm::alloc::shared || !same_queue_device(queue, pointer))
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
  const bool expected_space =
      (desc.scratch_space == PGACCEL_MEM_SPACE_SHARED_USM && actual == sycl::usm::alloc::shared) ||
      (desc.scratch_space == PGACCEL_MEM_SPACE_DEVICE && actual == sycl::usm::alloc::device);
  return expected_space && same_queue_device(queue, desc.scratch);
}

class GroupedAggDenseKernel;
class GroupedAggDenseCountPrepareKernel;
template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
class GroupedAggDenseCountRowsKernel;
template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
class GroupedAggDenseCountWorkgroupKernel;
class GroupedAggDenseCountCommitKernel;
class GroupedAggDenseIntegerPrepareKernel;
template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
class GroupedAggDenseIntegerAtomicRowsKernel;
class GroupedAggDenseIntegerAtomicCommitKernel;
template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
class GroupedAggDenseIntegerChunksKernel;
template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
class GroupedAggDenseIntegerWorkgroupKernel;
class GroupedAggDenseIntegerReduceKernel;
class GroupedAggDenseIntegerCommitKernel;
class GroupedAggHashKernel;
class GroupedAggHashInitKernel;
class GroupedAggHashCompactKernel;
class GroupedAggCompletionKernel;

#if defined(PGACCEL_TEST_HOOKS)
static std::atomic<bool> g_test_throw_scratch_cleanup{false};
static std::atomic<unsigned> g_test_caught_scratch_cleanup{0};
static std::atomic<unsigned> g_test_failure_stage{0};
static std::atomic<unsigned> g_test_active_scratch_owners{0};

enum TestFailureStage : unsigned {
  kTestFailureNone = 0,
  kTestFailureOwnedAllocation = 1,
  kTestFailurePublishedCopy = 2,
  kTestFailurePublishedWait = 3,
  kTestFailureOutputMaterialization = 4,
};

void test_fail_stage(unsigned stage) {
  unsigned expected = stage;
  if (g_test_failure_stage.compare_exchange_strong(expected, kTestFailureNone,
                                                   std::memory_order_acq_rel))
    throw std::runtime_error("injected grouped aggregate lifecycle failure");
}

extern "C" void pgacceltest_grouped_agg_fail_stage_once(unsigned stage) {
  g_test_failure_stage.store(stage, std::memory_order_release);
}

extern "C" unsigned pgacceltest_grouped_agg_active_scratch_owners(void) {
  return g_test_active_scratch_owners.load(std::memory_order_acquire);
}
#endif

class ScratchOwner {
 public:
  ScratchOwner() = default;
  ScratchOwner(sycl::queue* queue, void* pointer) : queue_(queue), pointer_(pointer) {
#if defined(PGACCEL_TEST_HOOKS)
    if (pointer_ != nullptr)
      g_test_active_scratch_owners.fetch_add(1, std::memory_order_relaxed);
#endif
  }
  ScratchOwner(const ScratchOwner&) = delete;
  ScratchOwner& operator=(const ScratchOwner&) = delete;
  ~ScratchOwner() noexcept {
    if (pointer_ == nullptr)
      return;
    try {
#if defined(PGACCEL_TEST_HOOKS)
      if (g_test_throw_scratch_cleanup.exchange(false, std::memory_order_acq_rel))
        throw std::runtime_error("injected scratch cleanup failure");
#endif
      sycl::free(pointer_, *queue_);
    } catch (const std::exception& error) {
#if defined(PGACCEL_TEST_HOOKS)
      g_test_caught_scratch_cleanup.fetch_add(1, std::memory_order_relaxed);
#endif
      std::fprintf(stderr, "pgaccel: grouped aggregate scratch cleanup failed: %s\n", error.what());
    } catch (...) {
#if defined(PGACCEL_TEST_HOOKS)
      g_test_caught_scratch_cleanup.fetch_add(1, std::memory_order_relaxed);
#endif
      std::fprintf(stderr,
                   "pgaccel: grouped aggregate scratch cleanup failed: unknown C++ exception\n");
    }
#if defined(PGACCEL_TEST_HOOKS)
    g_test_active_scratch_owners.fetch_sub(1, std::memory_order_relaxed);
#endif
  }

 private:
  sycl::queue* queue_ = nullptr;
  void* pointer_ = nullptr;
};

static_assert(std::is_nothrow_destructible_v<ScratchOwner>);

template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
void submit_dense_count_rows(sycl::queue& queue, size_t row_count,
                             KernelParams* const device_params) {
  queue.parallel_for<GroupedAggDenseCountRowsKernel<HasMembership, HasSqlMask, CountNonNull>>(
      sycl::range<1>(row_count), [=](sycl::id<1> id) {
        run_dense_count_row<HasMembership, HasSqlMask, CountNonNull>(device_params, id[0]);
      });
}

template <bool HasMembership, bool HasSqlMask, bool CountNonNull>
void submit_dense_count_workgroups(sycl::queue& queue, size_t group_capacity,
                                   size_t workgroup_count, KernelParams* const device_params) {
  queue.submit([&](sycl::handler& handler) {
    sycl::local_accessor<uint32_t, 1> local_counts(
        sycl::range<1>((CountNonNull ? 2 : 1) * kDenseReductionWorkgroupSize * group_capacity),
        handler);
    handler
        .parallel_for<GroupedAggDenseCountWorkgroupKernel<HasMembership, HasSqlMask, CountNonNull>>(
            sycl::nd_range<1>(sycl::range<1>(workgroup_count * kDenseReductionWorkgroupSize),
                              sycl::range<1>(kDenseReductionWorkgroupSize)),
            [=](sycl::nd_item<1> item) {
              run_dense_count_workgroup<HasMembership, HasSqlMask, CountNonNull>(
                  device_params, item, local_counts);
            });
  });
}

void submit_dense_count_specialization(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                                       const WorkspaceLayout& layout,
                                       KernelParams* const device_params) {
  const bool membership = desc.dim_count != 0;
  const bool sql_mask = desc.where_filter.kind == PGACCEL_GROUPED_AGG_FILTER_SQL;
#define PGACCEL_SUBMIT_DENSE_COUNT(MEMBERSHIP, MASK, NONNULL)                                   \
  do {                                                                                          \
    if (layout.dense_count_hierarchical)                                                        \
      submit_dense_count_workgroups<MEMBERSHIP, MASK, NONNULL>(                                 \
          queue, desc.group_capacity, layout.dense_count_chunk_count, device_params);           \
    else                                                                                        \
      submit_dense_count_rows<MEMBERSHIP, MASK, NONNULL>(queue, desc.row_count, device_params); \
  } while (false)
  const bool count_nonnull = parallel_dense_count_column_shape(desc);
  if (membership && sql_mask && count_nonnull)
    PGACCEL_SUBMIT_DENSE_COUNT(true, true, true);
  else if (membership && sql_mask)
    PGACCEL_SUBMIT_DENSE_COUNT(true, true, false);
  else if (membership && count_nonnull)
    PGACCEL_SUBMIT_DENSE_COUNT(true, false, true);
  else if (membership)
    PGACCEL_SUBMIT_DENSE_COUNT(true, false, false);
  else if (sql_mask && count_nonnull)
    PGACCEL_SUBMIT_DENSE_COUNT(false, true, true);
  else if (sql_mask)
    PGACCEL_SUBMIT_DENSE_COUNT(false, true, false);
  else if (count_nonnull)
    PGACCEL_SUBMIT_DENSE_COUNT(false, false, true);
  else
    PGACCEL_SUBMIT_DENSE_COUNT(false, false, false);
#undef PGACCEL_SUBMIT_DENSE_COUNT
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
void submit_dense_integer_atomic_rows(sycl::queue& queue, size_t row_count,
                                      KernelParams* const device_params) {
  queue.parallel_for<GroupedAggDenseIntegerAtomicRowsKernel<
      HasMembership, HasSqlMask, HasScalarRange, Multiply, HasMeasureRange>>(
      sycl::range<1>(row_count), [=](sycl::id<1> id) {
        run_dense_integer_atomic_row<HasMembership, HasSqlMask, HasScalarRange, Multiply,
                                     HasMeasureRange>(device_params, id[0]);
      });
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
void submit_dense_integer_chunks(sycl::queue& queue, size_t chunk_count,
                                 KernelParams* const device_params) {
  queue.parallel_for<GroupedAggDenseIntegerChunksKernel<HasMembership, HasSqlMask, HasScalarRange,
                                                        Multiply, HasMeasureRange>>(
      sycl::range<1>(chunk_count), [=](sycl::id<1> id) {
        run_dense_integer_chunk<HasMembership, HasSqlMask, HasScalarRange, Multiply,
                                HasMeasureRange>(device_params, id[0]);
      });
}

template <bool HasMembership, bool HasSqlMask, bool HasScalarRange, bool Multiply,
          bool HasMeasureRange>
void submit_dense_integer_workgroups(sycl::queue& queue, size_t group_capacity,
                                     size_t workgroup_count, KernelParams* const device_params) {
  queue.submit([&](sycl::handler& handler) {
    sycl::local_accessor<DenseIntegerPartial, 1> local_partials(
        sycl::range<1>(kDenseReductionWorkgroupSize * group_capacity), handler);
    handler.parallel_for<GroupedAggDenseIntegerWorkgroupKernel<
        HasMembership, HasSqlMask, HasScalarRange, Multiply, HasMeasureRange>>(
        sycl::nd_range<1>(sycl::range<1>(workgroup_count * kDenseReductionWorkgroupSize),
                          sycl::range<1>(kDenseReductionWorkgroupSize)),
        [=](sycl::nd_item<1> item) {
          run_dense_integer_workgroup<HasMembership, HasSqlMask, HasScalarRange, Multiply,
                                      HasMeasureRange>(device_params, item, local_partials);
        });
  });
}

template <bool Atomic>
void submit_dense_integer_specialization(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                                         size_t launch_count, bool hierarchical,
                                         KernelParams* const device_params) {
  const bool membership = desc.dim_count != 0;
  const bool sql_mask = desc.where_filter.kind == PGACCEL_GROUPED_AGG_FILTER_SQL;
  const bool scalar_range = exact_dense_integer_range_filter(desc.where_filter);
  const bool measure_range = exact_dense_integer_range_filter(desc.measure_filters[0]);
  const bool multiply = desc.measures[0].op == PGACCEL_GROUPED_AGG_MEASURE_MUL;
#define PGACCEL_SUBMIT_DENSE_INTEGER(MEMBERSHIP, MASK, RANGE, MULTIPLY, MEASURE_RANGE)    \
  do {                                                                                    \
    if constexpr (Atomic) {                                                               \
      submit_dense_integer_atomic_rows<MEMBERSHIP, MASK, RANGE, MULTIPLY, MEASURE_RANGE>( \
          queue, launch_count, device_params);                                            \
    } else if (hierarchical) {                                                            \
      submit_dense_integer_workgroups<MEMBERSHIP, MASK, RANGE, MULTIPLY, MEASURE_RANGE>(  \
          queue, desc.group_capacity, launch_count, device_params);                       \
    } else {                                                                              \
      submit_dense_integer_chunks<MEMBERSHIP, MASK, RANGE, MULTIPLY, MEASURE_RANGE>(      \
          queue, launch_count, device_params);                                            \
    }                                                                                     \
  } while (false)
  if (measure_range)
    PGACCEL_SUBMIT_DENSE_INTEGER(false, false, false, false, true);
  else if (scalar_range)
    PGACCEL_SUBMIT_DENSE_INTEGER(false, false, true, true, false);
  else if (membership && sql_mask && multiply)
    PGACCEL_SUBMIT_DENSE_INTEGER(true, true, false, true, false);
  else if (membership && sql_mask)
    PGACCEL_SUBMIT_DENSE_INTEGER(true, true, false, false, false);
  else if (membership && multiply)
    PGACCEL_SUBMIT_DENSE_INTEGER(true, false, false, true, false);
  else if (membership)
    PGACCEL_SUBMIT_DENSE_INTEGER(true, false, false, false, false);
  else if (sql_mask && multiply)
    PGACCEL_SUBMIT_DENSE_INTEGER(false, true, false, true, false);
  else if (sql_mask)
    PGACCEL_SUBMIT_DENSE_INTEGER(false, true, false, false, false);
  else if (multiply)
    PGACCEL_SUBMIT_DENSE_INTEGER(false, false, false, true, false);
  else
    PGACCEL_SUBMIT_DENSE_INTEGER(false, false, false, false, false);
#undef PGACCEL_SUBMIT_DENSE_INTEGER
}

void submit_grouped_agg_mode(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                             const WorkspaceLayout& layout, const KernelParams& host_params,
                             KernelParams* const device_params) {
  const bool parallel_dense_count =
      parallel_dense_count_shape(desc) && desc.row_count <= UINT32_MAX;
  const bool parallel_dense_integer = layout.dense_integer_parallel;
  const bool atomic_dense_integer_one_shot = layout.dense_integer_atomic_one_shot;
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH) {
    queue.single_task<GroupedAggHashInitKernel>([=]() { run_hash_init_kernel(device_params); });
    queue.fill(host_params.hash_owners, kHashEmptyOwner, layout.hash_slot_count);
    queue.fill(host_params.hash_counts, uint32_t{0}, layout.hash_slot_count);
    queue.fill(host_params.hash_group_count, uint32_t{0}, size_t{1});
    if (desc.row_count != 0) {
      queue.parallel_for<GroupedAggHashKernel>(sycl::range<1>(desc.row_count), [=](sycl::id<1> id) {
        run_hash_row(device_params, id[0]);
      });
    }
    queue.single_task<GroupedAggHashCompactKernel>(
        [=]() { run_hash_compact_kernel(device_params); });
  } else if (parallel_dense_count) {
    queue.single_task<GroupedAggDenseCountPrepareKernel>(
        [=]() { run_dense_count_prepare_kernel(device_params); });
    if ((desc.execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) != 0) {
      const size_t partial_count =
          layout.dense_count_hierarchical ? layout.dense_count_partial_count : desc.group_capacity;
      queue.fill(host_params.dense_chunk_counts, uint32_t{0}, partial_count);
      if (host_params.dense_selected_counts != nullptr)
        queue.fill(host_params.dense_selected_counts, uint32_t{0}, partial_count);
      if (desc.row_count != 0) {
        submit_dense_count_specialization(queue, desc, layout, device_params);
      }
    }
    queue.single_task<GroupedAggDenseCountCommitKernel>(
        [=]() { run_dense_count_commit_kernel(device_params); });
  } else if (atomic_dense_integer_one_shot) {
    queue.single_task<GroupedAggDenseIntegerPrepareKernel>(
        [=]() { run_dense_integer_prepare_kernel(device_params); });
    if (desc.row_count != 0) {
      submit_dense_integer_specialization<true>(queue, desc, desc.row_count, false, device_params);
    }
    queue.single_task<GroupedAggDenseIntegerAtomicCommitKernel>(
        [=]() { run_dense_integer_atomic_commit_kernel(device_params); });
  } else if (parallel_dense_integer) {
    queue.single_task<GroupedAggDenseIntegerPrepareKernel>(
        [=]() { run_dense_integer_prepare_kernel(device_params); });
    if ((desc.execution_flags & PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE) != 0 &&
        layout.dense_integer_partial_count != 0) {
      queue.fill(host_params.dense_integer_partials, DenseIntegerPartial{},
                 layout.dense_integer_partial_count);
      submit_dense_integer_specialization<false>(queue, desc, layout.dense_integer_chunk_count,
                                                 layout.dense_integer_hierarchical, device_params);
      queue.parallel_for<GroupedAggDenseIntegerReduceKernel>(
          sycl::range<1>(desc.group_capacity),
          [=](sycl::id<1> id) { run_dense_integer_reduce(device_params, id[0]); });
    }
    queue.single_task<GroupedAggDenseIntegerCommitKernel>(
        [=]() { run_dense_integer_commit_kernel(device_params); });
  } else {
    queue.single_task<GroupedAggDenseKernel>([=]() { run_dense_kernel(device_params); });
  }
}

int32_t grouped_agg_kernel_mode(const pgaccel_grouped_agg_desc& desc,
                                const WorkspaceLayout& layout) {
  if (desc.grouping_mode == PGACCEL_GROUPED_AGG_GROUPING_HASH)
    return PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH;
  if (parallel_dense_count_shape(desc) && desc.row_count <= UINT32_MAX)
    return PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT;
  if (layout.dense_integer_atomic_one_shot || layout.dense_integer_parallel)
    return PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER;
  return PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC;
}

pgaccel_status launch_grouped_agg_device(sycl::queue& queue, const pgaccel_grouped_agg_desc& desc,
                                         uint64_t output_mask, void* const device_workspace,
                                         const WorkspaceLayout& layout,
                                         DeviceCompletion* const device_completion) {
  KernelParams host_params;
  bind_params(desc, layout, device_workspace, &host_params);
  DevicePublishParams host_publish_params;
  bind_publish_params(desc, layout, device_workspace, output_mask, &host_publish_params);
  auto* const device_params =
      reinterpret_cast<KernelParams*>(static_cast<uint8_t*>(device_workspace) + layout.params);
  auto* const device_publish_params = reinterpret_cast<DevicePublishParams*>(
      static_cast<uint8_t*>(device_workspace) + layout.publish_params);
  if (desc.scratch_space == PGACCEL_MEM_SPACE_SHARED_USM) {
    *device_params = host_params;
    *device_publish_params = host_publish_params;
  } else {
    auto params_copy = queue.memcpy(device_params, &host_params, sizeof(host_params));
    wait_grouped(params_copy);
    auto publish_params_copy =
        queue.memcpy(device_publish_params, &host_publish_params, sizeof(host_publish_params));
    wait_grouped(publish_params_copy);
  }

  submit_grouped_agg_mode(queue, desc, layout, host_params, device_params);
  // The process-wide execution queue is in-order, so completion observes all
  // preceding prepare, accumulate, and commit kernels before it publishes.
  queue.single_task<GroupedAggCompletionKernel>([=]() {
    const DeviceCompletion published = build_completion(device_publish_params);
    device_completion->status = published.status;
    device_completion->detail = published.detail;
    device_completion->emitted = published.emitted;
    device_completion->selected = published.selected;
    device_completion->uncertain = published.uncertain;
    for (size_t slot = 0; slot < kPublishCommandCount; ++slot)
      device_completion->commands[slot] = published.commands[slot];
  });
  saturating_add(&g_grouped_transition_launches, 1);
  const auto started = std::chrono::steady_clock::now();
  try {
    queue.wait_and_throw();
  } catch (...) {
    record_grouped_wait(started);
    throw;
  }
  record_grouped_wait(started);
  return PGACCEL_OK;
}

// The completion kernel is already awaited by launch_grouped_agg_device().
// Shared-USM workspaces are therefore coherent and host-readable here: copy
// their published lanes directly instead of submitting another queue memcpy
// graph and waiting a second time. Device-only workspaces retain the exact
// queued-copy behavior through the same small facade.
class PublishedCopyQueue {
 public:
  PublishedCopyQueue(sycl::queue& queue, const void* const workspace)
      : queue_(queue), shared_(sycl::get_pointer_type(workspace, queue.get_context()) ==
                               sycl::usm::alloc::shared) {}

  void memcpy(void* const destination, const void* const source, size_t bytes) {
#if defined(PGACCEL_TEST_HOOKS)
    test_fail_stage(kTestFailurePublishedCopy);
#endif
    if (shared_) {
      std::memcpy(destination, source, bytes);
      saturating_add(&g_grouped_shared_copy_calls, 1);
      return;
    }
    queue_.memcpy(destination, source, bytes);
    saturating_add(&g_grouped_device_copy_calls, 1);
  }

  void wait_and_throw() {
#if defined(PGACCEL_TEST_HOOKS)
    test_fail_stage(kTestFailurePublishedWait);
#endif
    if (shared_)
      return;
    const auto started = std::chrono::steady_clock::now();
    try {
      queue_.wait_and_throw();
    } catch (...) {
      record_grouped_wait(started);
      throw;
    }
    record_grouped_wait(started);
  }

 private:
  sycl::queue& queue_;
  bool shared_;
};

pgaccel_status execute_grouped_agg_finalize(sycl::queue& device_queue,
                                            const pgaccel_grouped_agg_desc& desc,
                                            pgaccel_grouped_agg_out* out, int32_t* detail,
                                            void* const device_workspace,
                                            const WorkspaceLayout& layout) {
  size_t* const group_codes_output = out->group_codes;
  uint8_t* const active_groups_output = out->active_groups;
  void* const key0_values_output = out->keys[0].values;
  uint8_t* const key0_nulls_output = out->keys[0].nulls;
  void* const key1_values_output = out->keys[1].values;
  uint8_t* const key1_nulls_output = out->keys[1].nulls;
  void* const key2_values_output = out->keys[2].values;
  uint8_t* const key2_nulls_output = out->keys[2].nulls;
  void* const measure0_sum_output = out->measures[0].sum;
  void* const measure0_min_output = out->measures[0].min;
  void* const measure0_max_output = out->measures[0].max;
  void* const measure0_sumsq_output = out->measures[0].sumsq;
  uint64_t* const measure0_count_output = out->measures[0].count;
  uint64_t* const measure0_nonnull_output = out->measures[0].nonnull_count;
  void* const measure0_rhs_sum_output = out->measures[0].rhs_sum;
  uint64_t* const measure0_rhs_count_output = out->measures[0].rhs_count;
  uint64_t* const measure0_rhs_nonnull_output = out->measures[0].rhs_nonnull_count;
  void* const measure1_sum_output = out->measures[1].sum;
  void* const measure1_min_output = out->measures[1].min;
  void* const measure1_max_output = out->measures[1].max;
  void* const measure1_sumsq_output = out->measures[1].sumsq;
  uint64_t* const measure1_count_output = out->measures[1].count;
  uint64_t* const measure1_nonnull_output = out->measures[1].nonnull_count;
  void* const measure1_rhs_sum_output = out->measures[1].rhs_sum;
  uint64_t* const measure1_rhs_count_output = out->measures[1].rhs_count;
  uint64_t* const measure1_rhs_nonnull_output = out->measures[1].rhs_nonnull_count;
  void* const measure2_sum_output = out->measures[2].sum;
  void* const measure2_min_output = out->measures[2].min;
  void* const measure2_max_output = out->measures[2].max;
  void* const measure2_sumsq_output = out->measures[2].sumsq;
  uint64_t* const measure2_count_output = out->measures[2].count;
  uint64_t* const measure2_nonnull_output = out->measures[2].nonnull_count;
  void* const measure2_rhs_sum_output = out->measures[2].rhs_sum;
  uint64_t* const measure2_rhs_count_output = out->measures[2].rhs_count;
  uint64_t* const measure2_rhs_nonnull_output = out->measures[2].rhs_nonnull_count;
  void* const measure3_sum_output = out->measures[3].sum;
  void* const measure3_min_output = out->measures[3].min;
  void* const measure3_max_output = out->measures[3].max;
  void* const measure3_sumsq_output = out->measures[3].sumsq;
  uint64_t* const measure3_count_output = out->measures[3].count;
  uint64_t* const measure3_nonnull_output = out->measures[3].nonnull_count;
  void* const measure3_rhs_sum_output = out->measures[3].rhs_sum;
  uint64_t* const measure3_rhs_count_output = out->measures[3].rhs_count;
  uint64_t* const measure3_rhs_nonnull_output = out->measures[3].rhs_nonnull_count;

  const uint64_t output_mask =
      publish_bit(kPublishDetail) | publish_bit(kPublishEmitted) | publish_bit(kPublishSelected) |
      publish_bit(kPublishUncertain) |
      publish_bit(kPublishActive) * static_cast<uint64_t>(active_groups_output != nullptr) |
      publish_bit(kPublishGroupCodes) * static_cast<uint64_t>(group_codes_output != nullptr) |
      publish_bit(kPublishKeys) * static_cast<uint64_t>(key0_values_output != nullptr) |
      publish_bit(kPublishKeys + 1) * static_cast<uint64_t>(key0_nulls_output != nullptr) |
      publish_bit(kPublishKeys + kPublishKeyLaneCount) *
          static_cast<uint64_t>(key1_values_output != nullptr) |
      publish_bit(kPublishKeys + kPublishKeyLaneCount + 1) *
          static_cast<uint64_t>(key1_nulls_output != nullptr) |
      publish_bit(kPublishKeys + 2 * kPublishKeyLaneCount) *
          static_cast<uint64_t>(key2_values_output != nullptr) |
      publish_bit(kPublishKeys + 2 * kPublishKeyLaneCount + 1) *
          static_cast<uint64_t>(key2_nulls_output != nullptr) |
      publish_bit(kPublishMeasures) * static_cast<uint64_t>(measure0_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 1) * static_cast<uint64_t>(measure0_min_output != nullptr) |
      publish_bit(kPublishMeasures + 2) * static_cast<uint64_t>(measure0_max_output != nullptr) |
      publish_bit(kPublishMeasures + 3) * static_cast<uint64_t>(measure0_sumsq_output != nullptr) |
      publish_bit(kPublishMeasures + 4) * static_cast<uint64_t>(measure0_count_output != nullptr) |
      publish_bit(kPublishMeasures + 5) *
          static_cast<uint64_t>(measure0_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 6) *
          static_cast<uint64_t>(measure0_rhs_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 7) *
          static_cast<uint64_t>(measure0_rhs_count_output != nullptr) |
      publish_bit(kPublishMeasures + 8) *
          static_cast<uint64_t>(measure0_rhs_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount) *
          static_cast<uint64_t>(measure1_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 1) *
          static_cast<uint64_t>(measure1_min_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 2) *
          static_cast<uint64_t>(measure1_max_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 3) *
          static_cast<uint64_t>(measure1_sumsq_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 4) *
          static_cast<uint64_t>(measure1_count_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 5) *
          static_cast<uint64_t>(measure1_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 6) *
          static_cast<uint64_t>(measure1_rhs_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 7) *
          static_cast<uint64_t>(measure1_rhs_count_output != nullptr) |
      publish_bit(kPublishMeasures + 1 * kPublishMeasureLaneCount + 8) *
          static_cast<uint64_t>(measure1_rhs_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount) *
          static_cast<uint64_t>(measure2_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 1) *
          static_cast<uint64_t>(measure2_min_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 2) *
          static_cast<uint64_t>(measure2_max_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 3) *
          static_cast<uint64_t>(measure2_sumsq_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 4) *
          static_cast<uint64_t>(measure2_count_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 5) *
          static_cast<uint64_t>(measure2_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 6) *
          static_cast<uint64_t>(measure2_rhs_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 7) *
          static_cast<uint64_t>(measure2_rhs_count_output != nullptr) |
      publish_bit(kPublishMeasures + 2 * kPublishMeasureLaneCount + 8) *
          static_cast<uint64_t>(measure2_rhs_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount) *
          static_cast<uint64_t>(measure3_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 1) *
          static_cast<uint64_t>(measure3_min_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 2) *
          static_cast<uint64_t>(measure3_max_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 3) *
          static_cast<uint64_t>(measure3_sumsq_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 4) *
          static_cast<uint64_t>(measure3_count_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 5) *
          static_cast<uint64_t>(measure3_nonnull_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 6) *
          static_cast<uint64_t>(measure3_rhs_sum_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 7) *
          static_cast<uint64_t>(measure3_rhs_count_output != nullptr) |
      publish_bit(kPublishMeasures + 3 * kPublishMeasureLaneCount + 8) *
          static_cast<uint64_t>(measure3_rhs_nonnull_output != nullptr);
  auto* const device_completion = static_cast<DeviceCompletion*>(device_workspace);
  const pgaccel_status launch_status = launch_grouped_agg_device(
      device_queue, desc, output_mask, device_workspace, layout, device_completion);
  if (launch_status != PGACCEL_OK)
    return launch_status;

  PublishedCopyQueue queue(device_queue, device_workspace);
  const auto* const workspace_bytes = static_cast<const uint8_t*>(device_workspace);
  const auto* const completion_bytes = reinterpret_cast<const uint8_t*>(device_completion);
  DeviceCompletion completion{};
  queue.memcpy(&completion, device_completion, sizeof(completion));
  queue.wait_and_throw();
  record_grouped_output_bytes(completion);
  pgaccel_record_gpu_exec();

#if defined(PGACCEL_TEST_HOOKS)
  // No caller-visible output has been touched yet.  Injecting here proves
  // that a materialization failure cannot publish a plausible partial row.
  test_fail_stage(kTestFailureOutputMaterialization);
#endif

  queue.memcpy(detail, completion_bytes + sizeof(completion.status), sizeof(completion.detail));
  queue.memcpy(&out->emitted_group_count, &device_completion->emitted,
               sizeof(out->emitted_group_count));
  {
    {
      const DeviceCopyCommand& command = completion.commands[kPublishActive];
      if (command.bytes != 0 && active_groups_output != nullptr)
        queue.memcpy(active_groups_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishGroupCodes];
      if (command.bytes != 0 && group_codes_output != nullptr)
        queue.memcpy(group_codes_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishKeys];
      if (command.bytes != 0 && key0_values_output != nullptr)
        queue.memcpy(key0_values_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishKeys + 1];
      if (command.bytes != 0 && key0_nulls_output != nullptr)
        queue.memcpy(key0_nulls_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishKeys + kPublishKeyLaneCount];
      if (command.bytes != 0 && key1_values_output != nullptr)
        queue.memcpy(key1_values_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishKeys + kPublishKeyLaneCount + 1];
      if (command.bytes != 0 && key1_nulls_output != nullptr)
        queue.memcpy(key1_nulls_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishKeys + 2 * kPublishKeyLaneCount];
      if (command.bytes != 0 && key2_values_output != nullptr)
        queue.memcpy(key2_values_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishKeys + 2 * kPublishKeyLaneCount + 1];
      if (command.bytes != 0 && key2_nulls_output != nullptr)
        queue.memcpy(key2_nulls_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures];
      if (command.bytes != 0 && measure0_sum_output != nullptr)
        queue.memcpy(measure0_sum_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 1];
      if (command.bytes != 0 && measure0_min_output != nullptr)
        queue.memcpy(measure0_min_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 2];
      if (command.bytes != 0 && measure0_max_output != nullptr)
        queue.memcpy(measure0_max_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 3];
      if (command.bytes != 0 && measure0_sumsq_output != nullptr)
        queue.memcpy(measure0_sumsq_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 4];
      if (command.bytes != 0 && measure0_count_output != nullptr)
        queue.memcpy(measure0_count_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 5];
      if (command.bytes != 0 && measure0_nonnull_output != nullptr)
        queue.memcpy(measure0_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 6];
      if (command.bytes != 0 && measure0_rhs_sum_output != nullptr)
        queue.memcpy(measure0_rhs_sum_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 7];
      if (command.bytes != 0 && measure0_rhs_count_output != nullptr)
        queue.memcpy(measure0_rhs_count_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishMeasures + 8];
      if (command.bytes != 0 && measure0_rhs_nonnull_output != nullptr)
        queue.memcpy(measure0_rhs_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount];
      if (command.bytes != 0 && measure1_sum_output != nullptr)
        queue.memcpy(measure1_sum_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 1];
      if (command.bytes != 0 && measure1_min_output != nullptr)
        queue.memcpy(measure1_min_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 2];
      if (command.bytes != 0 && measure1_max_output != nullptr)
        queue.memcpy(measure1_max_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 3];
      if (command.bytes != 0 && measure1_sumsq_output != nullptr)
        queue.memcpy(measure1_sumsq_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 4];
      if (command.bytes != 0 && measure1_count_output != nullptr)
        queue.memcpy(measure1_count_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 5];
      if (command.bytes != 0 && measure1_nonnull_output != nullptr)
        queue.memcpy(measure1_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 6];
      if (command.bytes != 0 && measure1_rhs_sum_output != nullptr)
        queue.memcpy(measure1_rhs_sum_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 7];
      if (command.bytes != 0 && measure1_rhs_count_output != nullptr)
        queue.memcpy(measure1_rhs_count_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + kPublishMeasureLaneCount + 8];
      if (command.bytes != 0 && measure1_rhs_nonnull_output != nullptr)
        queue.memcpy(measure1_rhs_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount];
      if (command.bytes != 0 && measure2_sum_output != nullptr)
        queue.memcpy(measure2_sum_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 1];
      if (command.bytes != 0 && measure2_min_output != nullptr)
        queue.memcpy(measure2_min_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 2];
      if (command.bytes != 0 && measure2_max_output != nullptr)
        queue.memcpy(measure2_max_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 3];
      if (command.bytes != 0 && measure2_sumsq_output != nullptr)
        queue.memcpy(measure2_sumsq_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 4];
      if (command.bytes != 0 && measure2_count_output != nullptr)
        queue.memcpy(measure2_count_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 5];
      if (command.bytes != 0 && measure2_nonnull_output != nullptr)
        queue.memcpy(measure2_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 6];
      if (command.bytes != 0 && measure2_rhs_sum_output != nullptr)
        queue.memcpy(measure2_rhs_sum_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 7];
      if (command.bytes != 0 && measure2_rhs_count_output != nullptr)
        queue.memcpy(measure2_rhs_count_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 2 * kPublishMeasureLaneCount + 8];
      if (command.bytes != 0 && measure2_rhs_nonnull_output != nullptr)
        queue.memcpy(measure2_rhs_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount];
      if (command.bytes != 0 && measure3_sum_output != nullptr)
        queue.memcpy(measure3_sum_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 1];
      if (command.bytes != 0 && measure3_min_output != nullptr)
        queue.memcpy(measure3_min_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 2];
      if (command.bytes != 0 && measure3_max_output != nullptr)
        queue.memcpy(measure3_max_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 3];
      if (command.bytes != 0 && measure3_sumsq_output != nullptr)
        queue.memcpy(measure3_sumsq_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 4];
      if (command.bytes != 0 && measure3_count_output != nullptr)
        queue.memcpy(measure3_count_output, workspace_bytes + command.source_offset, command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 5];
      if (command.bytes != 0 && measure3_nonnull_output != nullptr)
        queue.memcpy(measure3_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 6];
      if (command.bytes != 0 && measure3_rhs_sum_output != nullptr)
        queue.memcpy(measure3_rhs_sum_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 7];
      if (command.bytes != 0 && measure3_rhs_count_output != nullptr)
        queue.memcpy(measure3_rhs_count_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command =
          completion.commands[kPublishMeasures + 3 * kPublishMeasureLaneCount + 8];
      if (command.bytes != 0 && measure3_rhs_nonnull_output != nullptr)
        queue.memcpy(measure3_rhs_nonnull_output, workspace_bytes + command.source_offset,
                     command.bytes);
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishSelected];
      if (command.bytes != 0)
        queue.memcpy(&out->selected_count, &device_completion->selected,
                     sizeof(out->selected_count));
    }
    {
      const DeviceCopyCommand& command = completion.commands[kPublishUncertain];
      if (command.bytes != 0)
        queue.memcpy(&out->uncertain_count, &device_completion->uncertain,
                     sizeof(out->uncertain_count));
    }
  }
  queue.wait_and_throw();
  return static_cast<pgaccel_status>(completion.status);
}

pgaccel_status execute_grouped_agg_accumulate(sycl::queue& device_queue,
                                              const pgaccel_grouped_agg_desc& desc, int32_t* detail,
                                              void* const device_workspace,
                                              const WorkspaceLayout& layout) {
  auto* const device_completion = static_cast<DeviceCompletion*>(device_workspace);
  const pgaccel_status launch_status = launch_grouped_agg_device(
      device_queue, desc, publish_bit(kPublishDetail), device_workspace, layout, device_completion);
  if (launch_status != PGACCEL_OK)
    return launch_status;

  PublishedCopyQueue queue(device_queue, device_workspace);
  const auto* const completion_bytes = reinterpret_cast<const uint8_t*>(device_completion);
  DeviceCompletion completion{};
  queue.memcpy(&completion, device_completion, sizeof(completion));
  queue.wait_and_throw();
  record_grouped_output_bytes(completion);
  pgaccel_record_gpu_exec();
  queue.memcpy(detail, completion_bytes + sizeof(completion.status), sizeof(completion.detail));
  queue.wait_and_throw();
  return static_cast<pgaccel_status>(completion.status);
}

}  // namespace

extern "C" uint64_t pgaccel_grouped_agg_transition_launch_count(void) {
  return g_grouped_transition_launches;
}

extern "C" uint64_t pgaccel_grouped_agg_queue_wait_count(void) {
  return g_grouped_queue_waits;
}

extern "C" uint64_t pgaccel_grouped_agg_queue_wait_ns(void) {
  return g_grouped_queue_wait_ns;
}

extern "C" uint64_t pgaccel_grouped_agg_output_bytes(void) {
  return g_grouped_output_bytes;
}

extern "C" uint64_t pgaccel_grouped_agg_shared_copy_calls(void) {
  return g_grouped_shared_copy_calls;
}

extern "C" uint64_t pgaccel_grouped_agg_device_copy_calls(void) {
  return g_grouped_device_copy_calls;
}

extern "C" void pgaccel_reset_grouped_agg_telemetry(void) {
  g_grouped_transition_launches = 0;
  g_grouped_queue_waits = 0;
  g_grouped_queue_wait_ns = 0;
  g_grouped_output_bytes = 0;
  g_grouped_shared_copy_calls = 0;
  g_grouped_device_copy_calls = 0;
}

#if defined(PGACCEL_TEST_HOOKS)
extern "C" bool pgacceltest_grouped_agg_cleanup_exception_is_caught(void) {
  const unsigned caught_before = g_test_caught_scratch_cleanup.load(std::memory_order_relaxed);
  g_test_throw_scratch_cleanup.store(true, std::memory_order_release);
  { ScratchOwner owner(nullptr, reinterpret_cast<void*>(uintptr_t{1})); }
  return !g_test_throw_scratch_cleanup.load(std::memory_order_acquire) &&
         g_test_caught_scratch_cleanup.load(std::memory_order_relaxed) == caught_before + 1;
}

extern "C" bool pgacceltest_grouped_agg_partial_merge_semantics(void) {
  const int64_t near_max = std::numeric_limits<int64_t>::max() - 5;
  DenseIntegerPartial left{};
  left.sum = near_max;
  left.prefix_max = near_max;
  left.min = near_max;
  left.max = near_max;
  left.rows = 1;
  left.nonnull = 1;

  DenseIntegerPartial safe_suffix{};
  safe_suffix.sum = 0;
  safe_suffix.prefix_min = -10;
  safe_suffix.prefix_max = 0;
  safe_suffix.min = -10;
  safe_suffix.max = 10;
  safe_suffix.rows = 2;
  safe_suffix.nonnull = 2;
  DenseIntegerPartial safe = left;
  merge_ordered_dense_integer_partial(&safe, safe_suffix);
  if (safe.failure_flags != 0 || safe.sum != near_max || safe.rows != 3 || safe.nonnull != 3 ||
      safe.min != -10 || safe.max != near_max)
    return false;

  DenseIntegerPartial overflowing_suffix = safe_suffix;
  overflowing_suffix.prefix_min = 0;
  overflowing_suffix.prefix_max = 10;
  DenseIntegerPartial overflow = left;
  merge_ordered_dense_integer_partial(&overflow, overflowing_suffix);
  return (overflow.failure_flags & kFailureNumericOverflow) != 0;
}
#endif

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

extern "C" pgaccel_status pgaccel_grouped_agg_kernel_mode(const pgaccel_grouped_agg_desc* desc,
                                                          int32_t* out_mode) {
  if (out_mode == nullptr)
    return PGACCEL_ERROR;
  *out_mode = 0;
  Validation validation;
  WorkspaceLayout layout;
  const pgaccel_status status = validate_desc(desc, &validation, &layout);
  if (status != PGACCEL_OK)
    return status;
  *out_mode = grouped_agg_kernel_mode(*desc, layout);
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_grouped_agg_execute_ex(const pgaccel_grouped_agg_desc* desc,
                                                         pgaccel_grouped_agg_out* out,
                                                         int32_t* detail) {
  if (detail == nullptr)
    return PGACCEL_ERROR;
  Validation validation;
  WorkspaceLayout layout;
  const pgaccel_status descriptor_status = validate_desc(desc, &validation, &layout);
  if (descriptor_status == PGACCEL_ERROR) {
    *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
    return PGACCEL_ERROR;
  }
  if (descriptor_status == PGACCEL_UNSUPPORTED) {
    *detail = 0;
    return PGACCEL_UNSUPPORTED;
  }
  const pgaccel_grouped_agg_desc descriptor_contract = *desc;
  void* const scratch_output = desc->scratch;
  if (scratch_output != descriptor_contract.scratch ||
      !validate_scratch_shape(descriptor_contract, layout)) {
    *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
    return PGACCEL_ERROR;
  }
  const bool finalize =
      (descriptor_contract.execution_flags & PGACCEL_GROUPED_AGG_EXEC_FINALIZE) != 0;
  if (!finalize && scratch_output == nullptr) {
    *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
    return PGACCEL_ERROR;
  }
  pgaccel_grouped_agg_out output_contract{};
  if (finalize) {
    if (!validate_out(descriptor_contract, out)) {
      *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
      return PGACCEL_ERROR;
    }
    output_contract = snapshot_output_contract(*out);
    if (!validate_aliases(descriptor_contract, output_contract, desc, out, detail)) {
      *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
      return PGACCEL_ERROR;
    }
  } else {
    if (out != nullptr ||
        !validate_aliases(descriptor_contract, output_contract, desc, &output_contract, detail)) {
      *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
      return PGACCEL_ERROR;
    }
  }
  try {
    sycl::queue& queue = pgaccel_require_queue();
    if (!validate_input_usm(queue, descriptor_contract) ||
        !validate_scratch_usm(queue, descriptor_contract) ||
        (finalize && !validate_output_usm(queue, descriptor_contract, output_contract))) {
      *detail = PGACCEL_GROUPED_AGG_DETAIL_INVALID;
      return PGACCEL_ERROR;
    }

    if (!finalize)
      return execute_grouped_agg_accumulate(queue, descriptor_contract, detail, scratch_output,
                                            layout);

    if (descriptor_contract.scratch != nullptr)
      return execute_grouped_agg_finalize(queue, descriptor_contract, out, detail,
                                          descriptor_contract.scratch, layout);

#if defined(PGACCEL_TEST_HOOKS)
    test_fail_stage(kTestFailureOwnedAllocation);
#endif
    void* owned_workspace = sycl::aligned_alloc_device(kWorkspaceAlignment, layout.bytes, queue);
    if (owned_workspace == nullptr) {
      *detail = 0;
      return PGACCEL_OOM;
    }
    ScratchOwner owner(&queue, owned_workspace);
    return execute_grouped_agg_finalize(queue, descriptor_contract, out, detail, owned_workspace,
                                        layout);
  } catch (const pgaccel_no_device_error&) {
    *detail = 0;
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::bad_alloc&) {
    *detail = 0;
    return PGACCEL_OOM;
  } catch (const std::exception& error) {
    *detail = 0;
    return pgaccel_kernel_failure("pgaccel_grouped_agg_execute_ex", &error);
  } catch (...) {
    *detail = 0;
    return pgaccel_kernel_failure("pgaccel_grouped_agg_execute_ex", nullptr);
  }
}

extern "C" pgaccel_status pgaccel_grouped_agg_execute(const pgaccel_grouped_agg_desc* desc,
                                                      pgaccel_grouped_agg_out* out) {
  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  return pgaccel_grouped_agg_execute_ex(desc, out, &detail);
}

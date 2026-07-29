#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <numeric>
#include <utility>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"
#include "pgaccel_resident_count.h"

static int g_checks = 0;
static int g_failures = 0;

#define CHECK(desc, condition)                  \
  do {                                          \
    ++g_checks;                                 \
    if (!(condition)) {                         \
      ++g_failures;                             \
      std::fprintf(stderr, "FAIL: %s\n", desc); \
    }                                           \
  } while (0)

static bool near(double actual, double expected, double tolerance = 1.0e-6) {
  return std::abs(actual - expected) <= tolerance * std::max(1.0, std::abs(expected));
}

static void test_resident_count_matrix() {
  constexpr size_t kRows = 32768;
  constexpr size_t kGroups = 32;
  std::vector<int64_t> keys(kRows);
  std::array<int64_t, kGroups> expected{};
  for (size_t row = 0; row < kRows; ++row) {
    const size_t group = (row * 13 + row / 19 + 7) % kGroups;
    keys[row] = static_cast<int64_t>(group) - 16;
    ++expected[group];
  }

  void* device_keys = nullptr;
  CHECK("resident count matrix allocation",
        pgaccel_expr_device_alloc_copy(keys.data(), keys.size() * sizeof(int64_t), &device_keys) ==
            PGACCEL_OK);
  CHECK("resident count matrix device pointer", device_keys != nullptr);
  if (device_keys == nullptr)
    return;

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
      static_cast<int64_t*>(device_keys), keys.size(), kGroups, &state);
  const uint64_t dispatches = pgaccel_gpu_exec_count();
  pgaccel_expr_device_free(device_keys);

  CHECK("resident count matrix status", status == PGACCEL_OK);
  CHECK("resident count matrix state", state != nullptr);
  CHECK("resident count matrix dispatch", dispatches == 2);
  if (state == nullptr)
    return;

  CHECK("resident count matrix group count", pgaccel_agg_group_count(state) == kGroups);
  const auto* out_keys = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const int64_t* counts = pgaccel_agg_get_counts(state);
  const double* results = pgaccel_agg_get_results(state, 0);
  std::array<uint8_t, kGroups> seen{};
  bool correct = out_keys != nullptr && counts != nullptr && results != nullptr;
  for (size_t group = 0; group < pgaccel_agg_group_count(state) && correct; ++group) {
    const int64_t key_index = out_keys[group] + 16;
    if (key_index < 0 || key_index >= static_cast<int64_t>(kGroups)) {
      correct = false;
      break;
    }
    const size_t index = static_cast<size_t>(key_index);
    correct = seen[index] == 0 && counts[group] == expected[index] &&
              results[group] == static_cast<double>(expected[index]);
    seen[index] = 1;
  }
  correct =
      correct && std::all_of(seen.begin(), seen.end(), [](uint8_t value) { return value != 0; });
  CHECK("resident count matrix unordered key/count map", correct);
  pgaccel_agg_free(state);
}

static void test_resident_count_boundaries() {
  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  CHECK("resident count rejects missing output",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 0, 0, nullptr) ==
            PGACCEL_INVALID_ARGUMENT);
  CHECK("resident count empty state status",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 0, 0, &state) ==
            PGACCEL_OK);
  CHECK("resident count empty state exists", state != nullptr);
  CHECK("resident count empty group count", pgaccel_agg_group_count(state) == 0);
  CHECK("resident count empty keys", pgaccel_agg_get_group_keys(state) == nullptr);
  CHECK("resident count empty counts", pgaccel_agg_get_counts(state) == nullptr);
  CHECK("resident count empty results", pgaccel_agg_get_results(state, 0) == nullptr);
  CHECK("resident count wrong result lane", pgaccel_agg_get_results(state, 1) == nullptr);
  pgaccel_agg_free(state);
  pgaccel_agg_free(nullptr);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  CHECK("resident count rejects null nonempty keys",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 1, 1, &state) ==
            PGACCEL_INVALID_ARGUMENT);
  CHECK("resident count clears state after invalid input", state == nullptr);
  CHECK("resident count rejects unaddressable row count",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
            reinterpret_cast<int64_t*>(uintptr_t{1}),
            static_cast<size_t>(std::numeric_limits<uint32_t>::max()) + 1, 1,
            &state) == PGACCEL_UNSUPPORTED);
  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  CHECK("resident count rejects an unrepresentable table capacity",
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
            reinterpret_cast<int64_t*>(uintptr_t{1}),
            static_cast<size_t>(std::numeric_limits<uint32_t>::max()),
            static_cast<size_t>(std::numeric_limits<uint32_t>::max()),
            &state) == PGACCEL_UNSUPPORTED);
  CHECK("resident count capacity rejection clears state", state == nullptr);

  const std::array<int64_t, 3> distinct = {11, 22, 33};
  void* device_keys = nullptr;
  CHECK("resident count bounded fixture allocation",
        pgaccel_expr_device_alloc_copy(distinct.data(), sizeof(distinct), &device_keys) ==
            PGACCEL_OK);
  if (device_keys != nullptr) {
    state = nullptr;
    pgaccel_reset_gpu_exec_count();
    CHECK("resident count enforces distinct bound",
          pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
              static_cast<int64_t*>(device_keys), distinct.size(), 1, &state) ==
              PGACCEL_UNSUPPORTED);
    CHECK("resident count bound failure has no partial state", state == nullptr);
    CHECK("resident count bound failure dispatches", pgaccel_gpu_exec_count() == 1);
    CHECK("resident count legacy wrapper fails closed",
          pgaccel_hash_count_i64_device_hash_execute_bounded(static_cast<int64_t*>(device_keys),
                                                             distinct.size(), 1) == nullptr);
    pgaccel_expr_device_free(device_keys);
  }
}

template <typename T>
static void test_spatial_predicate_width(bool use_fp64, const char* label) {
  const T nan = std::numeric_limits<T>::quiet_NaN();
  const std::array<T, 10> ring = {T(0), T(0), T(2), T(0), T(2), T(2), T(0), T(2), T(0), T(0)};
  const std::array<T, 8> points = {T(1), T(1), T(3), T(1), T(0), T(1), nan, T(0)};
  std::array<int8_t, 4> point_results = {9, 9, 9, 9};
  CHECK(label, pgaccel_point_in_ring_bulk(points.data(), 4, ring.data(), 5, use_fp64,
                                          point_results.data()) == PGACCEL_OK);
  CHECK("point-in-ring inside/outside/edge/nonfinite",
        (point_results == std::array<int8_t, 4>{1, -1, 0, 0}));

  const std::array<T, 8> open_ring = {T(0), T(0), T(2), T(0), T(2), T(2), T(0), T(2)};
  point_results.fill(9);
  CHECK("point-in-ring rejects open ring semantically",
        pgaccel_point_in_ring_bulk(points.data(), 4, open_ring.data(), 4, use_fp64,
                                   point_results.data()) == PGACCEL_OK);
  CHECK("point-in-ring open ring is uncertain",
        std::all_of(point_results.begin(), point_results.end(),
                    [](int8_t result) { return result == 0; }));

  const std::array<T, 12> duplicate_edge_ring = {
      T(0), T(0), T(2), T(0), T(2), T(0), T(2), T(2), T(0), T(2), T(0), T(0),
  };
  const std::array<T, 2> duplicate_edge_point = {T(2), T(0)};
  int8_t duplicate_edge_result = 9;
  CHECK("point-in-ring duplicate edge status",
        pgaccel_point_in_ring_bulk(duplicate_edge_point.data(), 1, duplicate_edge_ring.data(), 6,
                                   use_fp64, &duplicate_edge_result) == PGACCEL_OK);
  CHECK("point-in-ring duplicate edge is uncertain", duplicate_edge_result == 0);

  const std::array<T, 8> sphere_a = {T(0), T(0), T(0), T(0), T(0), T(0), nan, T(0)};
  const std::array<T, 8> sphere_b = {T(0), T(0), T(1), T(0), T(180), T(0), T(1), T(1)};
  std::array<T, 4> distances = {T(-1), T(-1), T(-1), T(-1)};
  std::array<uint8_t, 4> uncertain = {9, 9, 9, 9};
  CHECK("sphere boundary matrix status",
        pgaccel_sphere_distance_bulk(sphere_a.data(), sphere_b.data(), 4, use_fp64,
                                     distances.data(), uncertain.data()) == PGACCEL_OK);
  CHECK("sphere boundary uncertainty", (uncertain == std::array<uint8_t, 4>{1, 0, 1, 1}));
  CHECK("sphere boundary finite distance", near(static_cast<double>(distances[1]), 111195.0, 5e-4));

  const std::array<T, 24> seg_a = {
      T(0), T(0), T(2), T(2), T(0), T(0), T(2), T(0), T(1), T(1), T(1), T(1),
      T(0), T(0), T(2), T(0), T(0), T(0), T(2), T(0), nan,  T(0), T(2), T(2),
  };
  const std::array<T, 24> seg_b = {
      T(0), T(2), T(2), T(0), T(0), T(1), T(2), T(1), T(0), T(0), T(2), T(2),
      T(1), T(1), T(1), T(1), T(1), T(0), T(3), T(0), T(0), T(2), T(2), T(0),
  };
  std::array<int8_t, 6> segment_results = {9, 9, 9, 9, 9, 9};
  CHECK("segment boundary matrix status",
        pgaccel_segment_intersects_bulk(seg_a.data(), seg_b.data(), 6, use_fp64,
                                        segment_results.data()) == PGACCEL_OK);
  CHECK("segment crossing/disjoint/degenerate/collinear/nonfinite",
        (segment_results == std::array<int8_t, 6>{1, -1, 0, 0, 0, 0}));

  const std::array<T, 12> csr_coords = {T(7), T(9), T(0), T(0), T(3), T(4),
                                        T(0), T(0), T(4), T(0), T(4), T(3)};
  const std::array<uint32_t, 5> offsets = {0, 0, 2, 6, 12};
  std::array<T, 4> areas = {T(-1), T(-1), T(-1), T(-1)};
  CHECK("area degeneracy matrix status",
        pgaccel_st_area_bulk(csr_coords.data(), offsets.data(), 4, use_fp64, areas.data()) ==
            PGACCEL_OK);
  CHECK("area degeneracy matrix results",
        near(areas[0], 0) && near(areas[1], 0) && near(areas[2], 0) && near(areas[3], 6));

  std::array<T, 4> open_lengths = {T(-1), T(-1), T(-1), T(-1)};
  std::array<T, 4> closed_lengths = {T(-1), T(-1), T(-1), T(-1)};
  CHECK("open length degeneracy matrix status",
        pgaccel_st_length_bulk(csr_coords.data(), offsets.data(), 4, use_fp64, false,
                               open_lengths.data()) == PGACCEL_OK);
  CHECK("closed length degeneracy matrix status",
        pgaccel_st_length_bulk(csr_coords.data(), offsets.data(), 4, use_fp64, true,
                               closed_lengths.data()) == PGACCEL_OK);
  CHECK("open length degeneracy matrix results",
        near(open_lengths[0], 0) && near(open_lengths[1], 0) && near(open_lengths[2], 5) &&
            near(open_lengths[3], 7));
  CHECK("closed length degeneracy matrix results",
        near(closed_lengths[0], 0) && near(closed_lengths[1], 0) && near(closed_lengths[2], 10) &&
            near(closed_lengths[3], 12));
}

static void test_spatial_contract_boundaries() {
  int8_t result = 9;
  float scalar = 0;
  uint8_t uncertain = 9;
  const uint32_t offsets[] = {0, 0};
  CHECK("point-in-ring empty accepts nulls",
        pgaccel_point_in_ring_bulk(nullptr, 0, nullptr, 0, false, nullptr) == PGACCEL_OK);
  CHECK("point-in-ring rejects null points",
        pgaccel_point_in_ring_bulk(nullptr, 1, &scalar, 1, false, &result) == PGACCEL_ERROR_INIT);
  CHECK("sphere empty accepts nulls",
        pgaccel_sphere_distance_bulk(nullptr, nullptr, 0, false, nullptr, nullptr) == PGACCEL_OK);
  CHECK("sphere rejects null output",
        pgaccel_sphere_distance_bulk(&scalar, &scalar, 1, false, nullptr, &uncertain) ==
            PGACCEL_ERROR_INIT);
  CHECK("segment empty accepts nulls",
        pgaccel_segment_intersects_bulk(nullptr, nullptr, 0, false, nullptr) == PGACCEL_OK);
  CHECK("segment rejects null output",
        pgaccel_segment_intersects_bulk(&scalar, &scalar, 1, false, nullptr) == PGACCEL_ERROR_INIT);
  CHECK("area empty accepts nulls",
        pgaccel_st_area_bulk(nullptr, nullptr, 0, false, nullptr) == PGACCEL_OK);
  CHECK("area rejects null coordinates",
        pgaccel_st_area_bulk(nullptr, offsets, 1, false, &scalar) == PGACCEL_ERROR_INIT);
  CHECK("length empty accepts nulls",
        pgaccel_st_length_bulk(nullptr, nullptr, 0, false, false, nullptr) == PGACCEL_OK);
  CHECK("length rejects null coordinates",
        pgaccel_st_length_bulk(nullptr, offsets, 1, false, false, &scalar) == PGACCEL_ERROR_INIT);
}

static pgaccel_grouped_agg_filter disabled_group_filter() {
  pgaccel_grouped_agg_filter filter{};
  filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
  filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  return filter;
}

static pgaccel_grouped_agg_desc grouped_count_star_desc() {
  pgaccel_grouped_agg_desc desc{};
  desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  desc.size_bytes = sizeof(desc);
  desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  desc.group_capacity = 1;
  desc.measure_count = 1;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[0].state_bytes = sizeof(int64_t);
  desc.where_filter = disabled_group_filter();
  for (pgaccel_grouped_agg_filter& filter : desc.measure_filters)
    filter = disabled_group_filter();
  return desc;
}

static pgaccel_status grouped_workspace_status(const pgaccel_grouped_agg_desc& desc) {
  pgaccel_grouped_agg_workspace_req request{};
  request.abi_version = PGACCEL_OLAP_ABI_VERSION;
  request.size_bytes = sizeof(request);
  return pgaccel_grouped_agg_workspace_requirements(&desc, &request);
}

static void set_grouped_column(pgaccel_grouped_agg_desc* desc, int32_t physical_type,
                               uint32_t element_bytes, int32_t accumulator_kind,
                               uint32_t aggregate_mask = PGACCEL_GROUPED_AGG_LANE_COUNT) {
  desc->measures[0] = {};
  desc->measures[0].value.physical_type = physical_type;
  desc->measures[0].value.element_bytes = element_bytes;
  desc->measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
  desc->measures[0].agg_mask = aggregate_mask;
  desc->measures[0].accumulator_kind = accumulator_kind;
  desc->measures[0].state_bytes = sizeof(uint64_t);
}

static pgaccel_val bool_value(bool value) {
  pgaccel_val result{};
  result.tag = PGACCEL_VAL_BOOL;
  result.data.b = value;
  return result;
}

static pgaccel_val i32_value(int32_t value) {
  pgaccel_val result{};
  result.tag = PGACCEL_VAL_INT32;
  result.data.i32 = value;
  return result;
}

static pgaccel_val f64_value(double value) {
  pgaccel_val result{};
  result.tag = PGACCEL_VAL_FLOAT64;
  result.data.f64 = value;
  return result;
}

static pgaccel_grouped_agg_filter range_filter(pgaccel_val low, pgaccel_val high) {
  pgaccel_grouped_agg_filter filter{};
  filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
  filter.predicate_measure_slot = 0;
  filter.predicate_range_count = 1;
  filter.predicate_lo[0] = low;
  filter.predicate_hi[0] = high;
  filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  filter.mask = reinterpret_cast<const int8_t*>(uintptr_t{1});
  return filter;
}

static void test_grouped_descriptor_semantics() {
  pgaccel_grouped_agg_desc desc = grouped_count_star_desc();
  CHECK("grouped baseline descriptor", grouped_workspace_status(desc) == PGACCEL_OK);

  const std::array<std::pair<int32_t, uint32_t>, 5> count_only_types = {{
      {PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1},
      {PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4},
      {PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 4},
      {PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8},
      {PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 8},
  }};
  for (const auto& [physical_type, element_bytes] : count_only_types) {
    desc = grouped_count_star_desc();
    set_grouped_column(&desc, physical_type, element_bytes,
                       physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32 ||
                               physical_type == PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64
                           ? PGACCEL_GROUPED_AGG_ACCUM_F64
                           : PGACCEL_GROUPED_AGG_ACCUM_I64);
    CHECK("grouped count-only physical type accepted",
          grouped_workspace_status(desc) == PGACCEL_OK);
  }

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC, 16,
                     PGACCEL_GROUPED_AGG_ACCUM_NUMERIC, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].state_bytes = 32;
  CHECK("grouped numeric shape is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);
  desc.measures[0].state_bytes = 0;
  CHECK("grouped numeric requires state bytes", grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL, 16,
                     PGACCEL_GROUPED_AGG_ACCUM_INTERVAL, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].state_bytes = 24;
  CHECK("grouped interval shape is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);
  desc.measures[0].value.element_bytes = 0;
  CHECK("grouped interval requires physical width",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1, PGACCEL_GROUPED_AGG_ACCUM_I64,
                     PGACCEL_GROUPED_AGG_LANE_SUM);
  CHECK("grouped boolean SUM is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT64, 8, PGACCEL_GROUPED_AGG_ACCUM_I64,
                     PGACCEL_GROUPED_AGG_LANE_SUM | PGACCEL_GROUPED_AGG_LANE_SUMSQ);
  CHECK("grouped integer SUMSQ is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);
  desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_SUMSQ;
  CHECK("grouped SUMSQ requires SUM", grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4, PGACCEL_GROUPED_AGG_ACCUM_F64,
                     PGACCEL_GROUPED_AGG_LANE_SUM);
  CHECK("grouped float32 SUM is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);
  desc.measures[0].state_bytes = 4;
  CHECK("grouped f64 accumulator requires eight bytes",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4, PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(bool_value(false), bool_value(true));
  CHECK("grouped typed filter rejects mismatched bool/int source",
        grouped_workspace_status(desc) == PGACCEL_ERROR);
  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1, PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(bool_value(false), bool_value(true));
  CHECK("grouped boolean range filter", grouped_workspace_status(desc) == PGACCEL_OK);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4, PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(i32_value(9), i32_value(2));
  CHECK("grouped descending integer range rejected",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8, PGACCEL_GROUPED_AGG_ACCUM_F64);
  desc.where_filter = range_filter(f64_value(-1.5), f64_value(2.5));
  CHECK("grouped float64 range filter", grouped_workspace_status(desc) == PGACCEL_OK);
  desc.where_filter.predicate_lo[0] = f64_value(std::numeric_limits<double>::quiet_NaN());
  CHECK("grouped NaN range bound rejected", grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  desc.where_filter.predicate_source = 1;
  CHECK("grouped disabled filter requires canonical source",
        grouped_workspace_status(desc) == PGACCEL_ERROR);
  desc = grouped_count_star_desc();
  desc.where_filter.predicate_lo[3] = i32_value(1);
  CHECK("grouped disabled filter requires null tail ranges",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64, PGACCEL_GROUPED_AGG_LANE_RHS_SUM);
  CHECK("grouped RHS lanes require stats-pair operation",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].state_bytes = 4;
  CHECK("grouped i64 accumulator requires eight bytes",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC, 16,
                     PGACCEL_GROUPED_AGG_ACCUM_I64, PGACCEL_GROUPED_AGG_LANE_SUM);
  CHECK("grouped i64 accumulator rejects numeric physical input",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR;
  desc.measures[0].rhs.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_BOOL;
  desc.measures[0].rhs.element_bytes = 1;
  CHECK("grouped integer stats-pair boolean RHS is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_F64, PGACCEL_GROUPED_AGG_LANE_SUM);
  CHECK("grouped f64 accumulator rejects integer physical input",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8,
                     PGACCEL_GROUPED_AGG_ACCUM_F64, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_SUB;
  desc.measures[0].rhs.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32;
  desc.measures[0].rhs.element_bytes = 4;
  CHECK("grouped f64 binary float32 RHS is explicit unsupported",
        grouped_workspace_status(desc) == PGACCEL_UNSUPPORTED);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL, 16,
                     PGACCEL_GROUPED_AGG_ACCUM_INTERVAL, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].state_bytes = 16;
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_SUB;
  desc.measures[0].rhs.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
  desc.measures[0].rhs.element_bytes = 4;
  CHECK("grouped interval accumulator rejects integer RHS",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4, 99,
                     PGACCEL_GROUPED_AGG_LANE_SUM);
  CHECK("grouped rejects unknown accumulator kind", grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  desc.where_filter.kind = 99;
  CHECK("grouped rejects unknown filter kind", grouped_workspace_status(desc) == PGACCEL_ERROR);
  desc = grouped_count_star_desc();
  desc.where_filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
  CHECK("grouped SQL filter requires a mask", grouped_workspace_status(desc) == PGACCEL_ERROR);
  desc = grouped_count_star_desc();
  desc.where_filter.value_cmp_opcode = 999;
  CHECK("grouped rejects unknown comparison opcode",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  desc.where_filter.predicate_source = 99;
  CHECK("grouped scalar filter rejects unknown predicate source",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  CHECK("grouped scalar filter rejects count-star source",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC, 16,
                     PGACCEL_GROUPED_AGG_ACCUM_NUMERIC, PGACCEL_GROUPED_AGG_LANE_SUM);
  desc.measures[0].state_bytes = 16;
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  CHECK("grouped scalar filter rejects non-materializable physical type",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  desc.where_filter.predicate_lo[2] = i32_value(1);
  CHECK("grouped scalar filter requires canonical unused ranges",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  desc.where_filter.value_cmp_const = i32_value(1);
  CHECK("grouped always comparison requires canonical constant",
        grouped_workspace_status(desc) == PGACCEL_ERROR);

  desc = grouped_count_star_desc();
  set_grouped_column(&desc, PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4,
                     PGACCEL_GROUPED_AGG_ACCUM_I64);
  desc.where_filter = range_filter(i32_value(0), i32_value(1));
  desc.where_filter.value_cmp_opcode = PGACCEL_EXPR_OP_EQ;
  desc.where_filter.value_cmp_const = bool_value(true);
  CHECK("grouped typed comparison rejects mismatched constant",
        grouped_workspace_status(desc) == PGACCEL_ERROR);
}

static void test_grouped_full_publication_matrix() {
  constexpr size_t kRows = 4;
  constexpr size_t kCapacity = 12;
  const std::array<int32_t, kRows> key0 = {0, 0, 1, 1};
  const std::array<int32_t, kRows> key1 = {0, 1, 0, 1};
  const std::array<int32_t, kRows> key2 = {0, 0, 0, 0};
  const std::array<uint8_t, kRows> key1_nulls = {0, 0, 0, 1};
  const std::array<uint8_t, kRows> key2_nulls = {0, 0, 1, 0};
  const std::array<double, kRows> values = {1.0, 2.0, 3.0, 4.0};
  const std::array<double, kRows> rhs = {10.0, 20.0, 30.0, 40.0};

  std::vector<void*> allocations;
  auto device_copy = [&](const void* source, size_t bytes) {
    void* pointer = nullptr;
    const pgaccel_status status = pgaccel_expr_device_alloc_copy(source, bytes, &pointer);
    CHECK("grouped publication device allocation", status == PGACCEL_OK && pointer != nullptr);
    if (pointer != nullptr)
      allocations.push_back(pointer);
    return pointer;
  };
  auto release = [&]() {
    for (void* pointer : allocations)
      pgaccel_expr_device_free(pointer);
  };

  std::array<void*, 3> key_values = {
      device_copy(key0.data(), sizeof(key0)),
      device_copy(key1.data(), sizeof(key1)),
      device_copy(key2.data(), sizeof(key2)),
  };
  std::array<void*, 3> key_nulls = {
      nullptr,
      device_copy(key1_nulls.data(), sizeof(key1_nulls)),
      device_copy(key2_nulls.data(), sizeof(key2_nulls)),
  };
  std::array<void*, PGACCEL_GROUPED_AGG_MAX_MEASURES> measure_values{};
  std::array<void*, PGACCEL_GROUPED_AGG_MAX_MEASURES> measure_rhs{};
  for (size_t measure = 0; measure < PGACCEL_GROUPED_AGG_MAX_MEASURES; ++measure) {
    measure_values[measure] = device_copy(values.data(), sizeof(values));
    measure_rhs[measure] = device_copy(rhs.data(), sizeof(rhs));
  }
  const std::array<int8_t, kRows> true_mask = {
      PGACCEL_EXPR_TRUE,
      PGACCEL_EXPR_TRUE,
      PGACCEL_EXPR_TRUE,
      PGACCEL_EXPR_TRUE,
  };
  std::array<void*, PGACCEL_GROUPED_AGG_MAX_MEASURES + 1> filter_masks{};
  for (void*& mask : filter_masks)
    mask = device_copy(true_mask.data(), sizeof(true_mask));
  const bool allocated = std::all_of(key_values.begin(), key_values.end(),
                                     [](void* pointer) { return pointer != nullptr; }) &&
                         key_nulls[1] != nullptr && key_nulls[2] != nullptr &&
                         std::all_of(measure_values.begin(), measure_values.end(),
                                     [](void* pointer) { return pointer != nullptr; }) &&
                         std::all_of(measure_rhs.begin(), measure_rhs.end(),
                                     [](void* pointer) { return pointer != nullptr; }) &&
                         std::all_of(filter_masks.begin(), filter_masks.end(),
                                     [](void* pointer) { return pointer != nullptr; });
  if (!allocated) {
    release();
    return;
  }

  pgaccel_grouped_agg_desc desc = grouped_count_star_desc();
  desc.row_count = kRows;
  desc.key_count = 3;
  desc.group_capacity = kCapacity;
  const std::array<uint32_t, 3> cardinalities = {2, 3, 2};
  const std::array<int32_t, 3> null_codes = {
      PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE,
      2,
      1,
  };
  for (size_t key = 0; key < desc.key_count; ++key) {
    desc.keys[key].values.values = key_values[key];
    desc.keys[key].values.nulls = static_cast<const uint8_t*>(key_nulls[key]);
    desc.keys[key].values.type = PGACCEL_VAL_INT32;
    desc.keys[key].source = PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
    desc.keys[key].code_min = 0;
    desc.keys[key].cardinality = cardinalities[key];
    desc.keys[key].null_code = null_codes[key];
  }
  desc.measure_count = PGACCEL_GROUPED_AGG_MAX_MEASURES;
  for (size_t measure = 0; measure < desc.measure_count; ++measure) {
    desc.measures[measure] = {};
    desc.measures[measure].value.values = measure_values[measure];
    desc.measures[measure].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64;
    desc.measures[measure].value.element_bytes = sizeof(double);
    desc.measures[measure].rhs.values = measure_rhs[measure];
    desc.measures[measure].rhs.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64;
    desc.measures[measure].rhs.element_bytes = sizeof(double);
    desc.measures[measure].op = PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR;
    desc.measures[measure].agg_mask = PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN;
    desc.measures[measure].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_F64;
    desc.measures[measure].state_bytes = sizeof(double);
  }
  desc.where_filter = range_filter(f64_value(-100.0), f64_value(100.0));
  desc.where_filter.mask = static_cast<const int8_t*>(filter_masks[0]);
  const std::array<uint16_t, PGACCEL_GROUPED_AGG_MAX_MEASURES> compare_ops = {
      PGACCEL_EXPR_OP_EQ,
      PGACCEL_EXPR_OP_NE,
      PGACCEL_EXPR_OP_LT,
      PGACCEL_EXPR_OP_GT,
  };
  const std::array<double, PGACCEL_GROUPED_AGG_MAX_MEASURES> compare_constants = {
      2.0,
      999.0,
      999.0,
      -999.0,
  };
  for (size_t measure = 0; measure < desc.measure_count; ++measure) {
    pgaccel_grouped_agg_filter& filter = desc.measure_filters[measure];
    filter = {};
    filter.kind = PGACCEL_GROUPED_AGG_FILTER_SQL;
    filter.predicate_source = PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    filter.predicate_measure_slot = static_cast<int32_t>(measure);
    filter.value_cmp_opcode = compare_ops[measure];
    filter.value_cmp_const = f64_value(compare_constants[measure]);
    filter.mask = static_cast<const int8_t*>(filter_masks[measure + 1]);
  }

  pgaccel_grouped_agg_out output{};
  output.abi_version = PGACCEL_OLAP_ABI_VERSION;
  output.size_bytes = sizeof(output);
  output.group_capacity = kCapacity;
  output.output_space = PGACCEL_MEM_SPACE_HOST;
  std::vector<size_t> group_codes(kCapacity, std::numeric_limits<size_t>::max());
  std::vector<uint8_t> active(kCapacity, 0);
  output.group_codes = group_codes.data();
  output.active_groups = active.data();

  std::array<std::vector<int32_t>, 3> output_keys;
  std::array<std::vector<uint8_t>, 3> output_key_nulls;
  for (size_t key = 0; key < desc.key_count; ++key) {
    output_keys[key].assign(kCapacity, -999);
    output_key_nulls[key].assign(kCapacity, 9);
    output.keys[key].values = output_keys[key].data();
    output.keys[key].nulls = key == 0 ? nullptr : output_key_nulls[key].data();
    output.keys[key].type = PGACCEL_VAL_INT32;
  }

  std::array<std::vector<double>, PGACCEL_GROUPED_AGG_MAX_MEASURES> sums;
  std::array<std::vector<double>, PGACCEL_GROUPED_AGG_MAX_MEASURES> mins;
  std::array<std::vector<double>, PGACCEL_GROUPED_AGG_MAX_MEASURES> maxes;
  std::array<std::vector<double>, PGACCEL_GROUPED_AGG_MAX_MEASURES> sumsqs;
  std::array<std::vector<double>, PGACCEL_GROUPED_AGG_MAX_MEASURES> rhs_sums;
  std::array<std::vector<uint64_t>, PGACCEL_GROUPED_AGG_MAX_MEASURES> counts;
  std::array<std::vector<uint64_t>, PGACCEL_GROUPED_AGG_MAX_MEASURES> nonnulls;
  std::array<std::vector<uint64_t>, PGACCEL_GROUPED_AGG_MAX_MEASURES> rhs_counts;
  std::array<std::vector<uint64_t>, PGACCEL_GROUPED_AGG_MAX_MEASURES> rhs_nonnulls;
  for (size_t measure = 0; measure < desc.measure_count; ++measure) {
    sums[measure].assign(kCapacity, -999);
    mins[measure].assign(kCapacity, -999);
    maxes[measure].assign(kCapacity, -999);
    sumsqs[measure].assign(kCapacity, -999);
    rhs_sums[measure].assign(kCapacity, -999);
    counts[measure].assign(kCapacity, UINT64_MAX);
    nonnulls[measure].assign(kCapacity, UINT64_MAX);
    rhs_counts[measure].assign(kCapacity, UINT64_MAX);
    rhs_nonnulls[measure].assign(kCapacity, UINT64_MAX);
    output.measures[measure].sum = sums[measure].data();
    output.measures[measure].min = mins[measure].data();
    output.measures[measure].max = maxes[measure].data();
    output.measures[measure].sumsq = sumsqs[measure].data();
    output.measures[measure].count = counts[measure].data();
    output.measures[measure].nonnull_count = nonnulls[measure].data();
    output.measures[measure].rhs_sum = rhs_sums[measure].data();
    output.measures[measure].rhs_count = rhs_counts[measure].data();
    output.measures[measure].rhs_nonnull_count = rhs_nonnulls[measure].data();
  }

  int32_t detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_grouped_agg_execute_ex(&desc, &output, &detail);
  CHECK("grouped full publication status", status == PGACCEL_OK);
  CHECK("grouped full publication detail", detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);
  CHECK("grouped full publication selected rows", output.selected_count == kRows);
  CHECK("grouped full publication emitted groups", output.emitted_group_count == kRows);
  CHECK("grouped full publication dispatch", pgaccel_gpu_exec_count() > 0);
  CHECK("grouped full publication active groups",
        std::count(active.begin(), active.end(), uint8_t{1}) == kRows);
  const std::array<uint64_t, PGACCEL_GROUPED_AGG_MAX_MEASURES> expected_counts = {1, 4, 4, 4};
  for (size_t measure = 0; measure < desc.measure_count; ++measure) {
    const uint64_t total_count =
        std::accumulate(counts[measure].begin(), counts[measure].end(), uint64_t{0});
    const uint64_t total_rhs_count =
        std::accumulate(rhs_counts[measure].begin(), rhs_counts[measure].end(), uint64_t{0});
    CHECK("grouped full publication count lane", total_count == expected_counts[measure]);
    CHECK("grouped full publication rhs count lane", total_rhs_count == expected_counts[measure]);
  }
  release();
}

static void test_spatial_numeric_matrix() {
  const float area_f32_coords[] = {0, 0, 4, 0, 4, 3, 0, 0, 0, 0, 2, 0, 0, 2, 0, 0};
  const double area_f64_coords[] = {0, 0, 4, 0, 4, 3, 0, 0, 0, 0, 2, 0, 0, 2, 0, 0};
  const uint32_t offsets[] = {0, 8, 16};
  float areas_f32[2] = {-1, -1};
  double areas_f64[2] = {-1, -1};
  pgaccel_reset_gpu_exec_count();
  CHECK("area f32 status",
        pgaccel_st_area_bulk(area_f32_coords, offsets, 2, false, areas_f32) == PGACCEL_OK);
  CHECK("area f64 status",
        pgaccel_st_area_bulk(area_f64_coords, offsets, 2, true, areas_f64) == PGACCEL_OK);
  CHECK("area f32 results", near(areas_f32[0], 6.0) && near(areas_f32[1], 2.0));
  CHECK("area f64 results", near(areas_f64[0], 6.0, 1.0e-12) && near(areas_f64[1], 2.0, 1.0e-12));

  float lengths_f32[2] = {-1, -1};
  double lengths_f64[2] = {-1, -1};
  CHECK("length f32 status", pgaccel_st_length_bulk(area_f32_coords, offsets, 2, false, false,
                                                    lengths_f32) == PGACCEL_OK);
  CHECK("length f64 status",
        pgaccel_st_length_bulk(area_f64_coords, offsets, 2, true, true, lengths_f64) == PGACCEL_OK);
  CHECK("length f32 results", near(lengths_f32[0], 12.0) && near(lengths_f32[1], 6.8284271));
  CHECK("length f64 results",
        near(lengths_f64[0], 12.0, 1.0e-12) && near(lengths_f64[1], 6.82842712474619, 1.0e-12));

  const float sphere_a_f32[] = {0, 0, 0, 0, 0, 0};
  const float sphere_b_f32[] = {0, 0, 1, 0, 180, 0};
  const double sphere_a_f64[] = {0, 0, 0, 0, 0, 0};
  const double sphere_b_f64[] = {0, 0, 1, 0, 180, 0};
  float sphere_dist_f32[3] = {-1, -1, -1};
  double sphere_dist_f64[3] = {-1, -1, -1};
  uint8_t sphere_unc_f32[3] = {9, 9, 9};
  uint8_t sphere_unc_f64[3] = {9, 9, 9};
  CHECK("sphere f32 status",
        pgaccel_sphere_distance_bulk(sphere_a_f32, sphere_b_f32, 3, false, sphere_dist_f32,
                                     sphere_unc_f32) == PGACCEL_OK);
  CHECK("sphere f64 status",
        pgaccel_sphere_distance_bulk(sphere_a_f64, sphere_b_f64, 3, true, sphere_dist_f64,
                                     sphere_unc_f64) == PGACCEL_OK);
  CHECK("sphere f32 results",
        near(sphere_dist_f32[0], 0.0) && near(sphere_dist_f32[1], 111194.93, 2.0e-4) &&
            sphere_unc_f32[0] == 1 && sphere_unc_f32[1] == 0 && sphere_unc_f32[2] == 1);
  CHECK("sphere f64 results", near(sphere_dist_f64[0], 0.0, 1.0e-12) &&
                                  near(sphere_dist_f64[1], 111195.0802335329, 1.0e-6) &&
                                  sphere_unc_f64[0] == 1 && sphere_unc_f64[1] == 0 &&
                                  sphere_unc_f64[2] == 1);

  const float seg_a_f32[] = {0, 0, 2, 2, 0, 0, 2, 0, 0, 0, 1, 0};
  const float seg_b_f32[] = {0, 2, 2, 0, 0, 1, 2, 1, 1, 0, 2, 1};
  const double seg_a_f64[] = {0, 0, 2, 2, 0, 0, 2, 0, 0, 0, 1, 0};
  const double seg_b_f64[] = {0, 2, 2, 0, 0, 1, 2, 1, 1, 0, 2, 1};
  int8_t seg_f32[3] = {9, 9, 9};
  int8_t seg_f64[3] = {9, 9, 9};
  CHECK("segments f32 status",
        pgaccel_segment_intersects_bulk(seg_a_f32, seg_b_f32, 3, false, seg_f32) == PGACCEL_OK);
  CHECK("segments f64 status",
        pgaccel_segment_intersects_bulk(seg_a_f64, seg_b_f64, 3, true, seg_f64) == PGACCEL_OK);
  CHECK("segments f32 tri-state", seg_f32[0] == 1 && seg_f32[1] == -1 && seg_f32[2] == 0);
  CHECK("segments f64 tri-state", seg_f64[0] == 1 && seg_f64[1] == -1 && seg_f64[2] == 0);
  CHECK("spatial numeric matrix dispatched", pgaccel_gpu_exec_count() >= 8);
}

static void test_polygon_distance_matrix() {
  const float a[] = {0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0};
  const float b[] = {3, 0, 4, 0, 4, 1, 3, 1, 3, 0, 1, 0, 2, 0, 2, 1, 1, 1, 1, 0};
  const uint32_t offsets[] = {0, 10, 20};
  float distances[2] = {-1, -1};
  uint8_t uncertain[2] = {9, 9};
  pgaccel_reset_gpu_exec_count();
  CHECK("polygon distance status",
        pgaccel_st_distance_polygon_polygon_bulk(a, offsets, b, offsets, 2, distances, uncertain) ==
            PGACCEL_OK);
  CHECK("polygon distance disjoint", near(distances[0], 2.0) && uncertain[0] == 0);
  CHECK("polygon distance touching", near(distances[1], 0.0) && uncertain[1] == 1);
  CHECK("polygon distance dispatched", pgaccel_gpu_exec_count() > 0);

  const float degenerate_a[] = {
      0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 1,
  };
  const float degenerate_b[] = {
      0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 3, 0, 4, 0, 4, 1, 3, 1, 3, 0,
  };
  const uint32_t degenerate_offsets_a[] = {0, 4, 14};
  const uint32_t degenerate_offsets_b[] = {0, 10, 20};
  distances[0] = distances[1] = -1;
  uncertain[0] = uncertain[1] = 9;
  CHECK("polygon distance degeneracy status",
        pgaccel_st_distance_polygon_polygon_bulk(degenerate_a, degenerate_offsets_a, degenerate_b,
                                                 degenerate_offsets_b, 2, distances,
                                                 uncertain) == PGACCEL_OK);
  CHECK("polygon distance degeneracy results", near(distances[0], 0.0) && uncertain[0] == 1 &&
                                                   near(distances[1], 2.0) && uncertain[1] == 0);
}

static pgaccel_geometry geometry(pgaccel_geom_type type, const float* bbox, const float* coords,
                                 size_t count) {
  return {type, bbox, coords, count, nullptr, 0};
}

static void test_algorithmic_predicate_matrix() {
  const float point_a[] = {0, 0};
  const float point_b[] = {0.5f, 0.5f};
  const float point_far[] = {10, 10};
  const float point_a_bbox[] = {0, 0, 0, 0};
  const float point_b_bbox[] = {0.5f, 0.5f, 0.5f, 0.5f};
  const float point_far_bbox[] = {10, 10, 10, 10};
  const float line_a[] = {0, 0, 2, 2};
  const float line_b[] = {0, 2, 2, 0};
  const float common_bbox[] = {0, 0, 2, 2};
  const float square[] = {0, 0, 2, 0, 2, 2, 0, 2, 0, 0};
  const float square_rotated[] = {2, 2, 0, 2, 0, 0, 2, 0, 2, 2};
  const float changed[] = {0, 0, 2, 0, 1.5f, 2, 0, 2, 0, 0};

  std::array<pgaccel_geometry, 8> a = {
      geometry(PGACCEL_GEOM_POINT, point_a_bbox, point_a, 1),
      geometry(PGACCEL_GEOM_POINT, point_a_bbox, point_a, 1),
      geometry(PGACCEL_GEOM_POINT, common_bbox, point_a, 1),
      geometry(PGACCEL_GEOM_LINESTRING, common_bbox, line_a, 2),
      geometry(PGACCEL_GEOM_POLYGON, common_bbox, square, 5),
      geometry(PGACCEL_GEOM_POLYGON, common_bbox, square, 5),
      geometry(PGACCEL_GEOM_POINT, common_bbox, point_b, 1),
      geometry(PGACCEL_GEOM_UNKNOWN, common_bbox, point_a, 1),
  };
  std::array<pgaccel_geometry, 8> b = {
      geometry(PGACCEL_GEOM_POINT, point_a_bbox, point_a, 1),
      geometry(PGACCEL_GEOM_POINT, point_far_bbox, point_far, 1),
      geometry(PGACCEL_GEOM_POINT, common_bbox, point_b, 1),
      geometry(PGACCEL_GEOM_LINESTRING, common_bbox, line_b, 2),
      geometry(PGACCEL_GEOM_POLYGON, common_bbox, square_rotated, 5),
      geometry(PGACCEL_GEOM_POLYGON, common_bbox, changed, 5),
      geometry(PGACCEL_GEOM_POLYGON, common_bbox, square, 5),
      geometry(PGACCEL_GEOM_POINT, point_a_bbox, point_a, 1),
  };

  const std::array<int8_t, 8> equals_expected = {1, -1, -1, 0, 1, 0, -1, 0};
  const std::array<int8_t, 8> touches_expected = {-1, -1, -1, 0, -1, 0, 0, 0};
  const std::array<int8_t, 8> crosses_expected = {-1, -1, -1, 0, -1, 0, 0, 0};
  const std::array<int8_t, 8> overlaps_expected = {-1, -1, 0, 0, -1, 0, -1, 0};
  std::array<int8_t, 8> results{};

  pgaccel_reset_gpu_exec_count();
  CHECK("equals matrix status",
        pgaccel_st_equals_bulk(a.data(), b.data(), a.size(), results.data()) == PGACCEL_OK);
  CHECK("equals matrix results", results == equals_expected);
  results.fill(9);
  CHECK("touches matrix status",
        pgaccel_st_touches_bulk(a.data(), b.data(), a.size(), results.data()) == PGACCEL_OK);
  CHECK("touches matrix results", results == touches_expected);
  results.fill(9);
  CHECK("crosses matrix status",
        pgaccel_st_crosses_bulk(a.data(), b.data(), a.size(), results.data()) == PGACCEL_OK);
  CHECK("crosses matrix results", results == crosses_expected);
  results.fill(9);
  CHECK("overlaps matrix status",
        pgaccel_st_overlaps_bulk(a.data(), b.data(), a.size(), results.data()) == PGACCEL_OK);
  CHECK("overlaps matrix results", results == overlaps_expected);
  CHECK("algorithmic predicate matrix dispatched four kernels", pgaccel_gpu_exec_count() == 4);
}

static void test_algorithmic_ring_boundaries() {
  int8_t result = 9;
  CHECK("algorithmic equals empty accepts nulls",
        pgaccel_st_equals_bulk(nullptr, nullptr, 0, nullptr) == PGACCEL_OK);
  CHECK("algorithmic touches empty accepts nulls",
        pgaccel_st_touches_bulk(nullptr, nullptr, 0, nullptr) == PGACCEL_OK);
  CHECK("algorithmic crosses empty accepts nulls",
        pgaccel_st_crosses_bulk(nullptr, nullptr, 0, nullptr) == PGACCEL_OK);
  CHECK("algorithmic overlaps empty accepts nulls",
        pgaccel_st_overlaps_bulk(nullptr, nullptr, 0, nullptr) == PGACCEL_OK);
  CHECK("algorithmic equals rejects null geometry",
        pgaccel_st_equals_bulk(nullptr, reinterpret_cast<const pgaccel_geometry*>(uintptr_t{1}), 1,
                               &result) == PGACCEL_ERROR_INIT);

  std::vector<float> long_ring(34 * 2);
  constexpr double kPi = 3.14159265358979323846264338327950288;
  for (size_t vertex = 0; vertex < 34; ++vertex) {
    const double angle = 2.0 * kPi * static_cast<double>(vertex) / 33.0;
    long_ring[vertex * 2] = static_cast<float>(std::cos(angle));
    long_ring[vertex * 2 + 1] = static_cast<float>(std::sin(angle));
  }
  long_ring[66] = long_ring[0];
  long_ring[67] = long_ring[1];

  const float bbox[] = {-1, -1, 1, 1};
  const float triangle[] = {0, 0, 1, 0, 0, 1};
  const float square[] = {0, 0, 1, 0, 1, 1, 0, 1};
  const uint32_t one_ring[] = {0};
  const uint32_t two_rings[] = {0, 3};
  const uint32_t shifted_ring[] = {1};

  std::array<pgaccel_geometry, 6> a = {
      geometry(PGACCEL_GEOM_POLYGON, bbox, long_ring.data(), 34),
      geometry(PGACCEL_GEOM_POLYGON, bbox, nullptr, 0),
      geometry(PGACCEL_GEOM_POLYGON, bbox, triangle, 3),
      geometry(PGACCEL_GEOM_POLYGON, bbox, triangle, 3),
      {PGACCEL_GEOM_POLYGON, bbox, square, 4, two_rings, 2},
      {PGACCEL_GEOM_POLYGON, bbox, square, 4, shifted_ring, 1},
  };
  std::array<pgaccel_geometry, 6> b = {
      geometry(PGACCEL_GEOM_POLYGON, bbox, long_ring.data(), 34),
      geometry(PGACCEL_GEOM_POLYGON, bbox, nullptr, 0),
      geometry(PGACCEL_GEOM_POLYGON, bbox, triangle, 3),
      {PGACCEL_GEOM_POLYGON, bbox, square, 4, one_ring, 1},
      {PGACCEL_GEOM_POLYGON, bbox, square, 4, two_rings, 2},
      {PGACCEL_GEOM_POLYGON, bbox, square, 4, one_ring, 1},
  };
  std::array<int8_t, 6> results{};
  CHECK("algorithmic ring boundary status",
        pgaccel_st_equals_bulk(a.data(), b.data(), a.size(), results.data()) == PGACCEL_OK);
  CHECK("algorithmic ring comparability boundaries",
        (results == std::array<int8_t, 6>{0, 0, 1, 0, 0, 0}));
}

int main() {
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "FATAL: pgaccel_init failed\n");
    return 1;
  }

  test_resident_count_matrix();
  test_resident_count_boundaries();
  test_spatial_contract_boundaries();
  test_spatial_predicate_width<float>(false, "spatial predicate fp32 matrix status");
  test_spatial_predicate_width<double>(true, "spatial predicate fp64 matrix status");
  test_grouped_descriptor_semantics();
  test_grouped_full_publication_matrix();
  test_spatial_numeric_matrix();
  test_polygon_distance_matrix();
  test_algorithmic_predicate_matrix();
  test_algorithmic_ring_boundaries();

  CHECK("runtime shutdown", pgaccel_shutdown() == PGACCEL_OK);
  std::printf("resident-count/spatial matrix: %d checks, %d failures\n", g_checks, g_failures);
  return g_failures == 0 ? 0 : 1;
}

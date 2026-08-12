#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"
#include "pgaccel_resident_count.h"

namespace {

int g_pass = 0;
int g_fail = 0;

void check_status(const char* label, pgaccel_status actual, pgaccel_status expected) {
  if (actual == expected) {
    ++g_pass;
    return;
  }
  std::fprintf(stderr, "FAIL: %s returned %d, expected %d\n", label, static_cast<int>(actual),
               static_cast<int>(expected));
  ++g_fail;
}

void check_value(const char* label, bool condition) {
  if (condition) {
    ++g_pass;
    return;
  }
  std::fprintf(stderr, "FAIL: %s\n", label);
  ++g_fail;
}

void check_unavailable(const char* label, pgaccel_status actual) {
  check_value(label, actual == PGACCEL_ERROR || actual == PGACCEL_ERROR_NO_DEVICE);
}

pgaccel_spatial_resident_request empty_resident_request(pgaccel_spatial_predicate predicate) {
  pgaccel_spatial_resident_request request{};
  request.abi_version = PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION;
  request.predicate = predicate;
  return request;
}

pgaccel_spatial_resident_request nonempty_resident_request(int8_t* result) {
  pgaccel_spatial_resident_request request =
      empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.count = 1;
  request.max_referenced_bytes = 1024;
  request.left.view.row_count = 1;
  request.right.view.row_count = 1;
  request.predicate_results = result;
  request.predicate_results_bytes = sizeof(*result);
  request.output_capacity = 1;
  return request;
}

pgaccel_grouped_agg_filter disabled_grouped_filter() {
  pgaccel_grouped_agg_filter filter{};
  filter.kind = PGACCEL_GROUPED_AGG_FILTER_NONE;
  filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  return filter;
}

pgaccel_grouped_agg_desc count_star_desc() {
  pgaccel_grouped_agg_desc desc{};
  desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  desc.size_bytes = sizeof(desc);
  desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
  desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  desc.group_capacity = 1;
  desc.measure_count = 1;
  desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  desc.where_filter = disabled_grouped_filter();
  for (auto& filter : desc.measure_filters)
    filter = disabled_grouped_filter();
  desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  desc.measures[0].state_bytes = sizeof(int64_t);
  return desc;
}

pgaccel_grouped_agg_workspace_req empty_workspace_req() {
  pgaccel_grouped_agg_workspace_req request{};
  request.abi_version = PGACCEL_OLAP_ABI_VERSION;
  request.size_bytes = sizeof(request);
  return request;
}

void test_spatial_predicate_pointer_contracts() {
  float coords_f32[8]{};
  double coords_f64[8]{};
  uint32_t offsets[2] = {0, 4};
  float out_f32 = 0.0f;
  double out_f64 = 0.0;
  int8_t predicate = 0;
  uint8_t uncertain = 0;

  check_status("area empty", pgaccel_st_area_bulk(nullptr, nullptr, 0, false, nullptr), PGACCEL_OK);
  check_status("area null coords", pgaccel_st_area_bulk(nullptr, offsets, 1, false, &out_f32),
               PGACCEL_ERROR_INIT);
  check_status("area null offsets", pgaccel_st_area_bulk(coords_f32, nullptr, 1, false, &out_f32),
               PGACCEL_ERROR_INIT);
  check_status("area null output", pgaccel_st_area_bulk(coords_f32, offsets, 1, false, nullptr),
               PGACCEL_ERROR_INIT);

  check_status("length empty", pgaccel_st_length_bulk(nullptr, nullptr, 0, false, false, nullptr),
               PGACCEL_OK);
  check_status("length null coords",
               pgaccel_st_length_bulk(nullptr, offsets, 1, false, false, &out_f32),
               PGACCEL_ERROR_INIT);
  check_status("length null offsets",
               pgaccel_st_length_bulk(coords_f32, nullptr, 1, false, false, &out_f32),
               PGACCEL_ERROR_INIT);
  check_status("length null output",
               pgaccel_st_length_bulk(coords_f32, offsets, 1, false, false, nullptr),
               PGACCEL_ERROR_INIT);

  check_status("point-in-ring null points",
               pgaccel_point_in_ring_bulk(nullptr, 1, coords_f64, 4, true, &predicate),
               PGACCEL_ERROR_INIT);
  check_status("point-in-ring null ring",
               pgaccel_point_in_ring_bulk(coords_f64, 1, nullptr, 4, true, &predicate),
               PGACCEL_ERROR_INIT);
  check_status("point-in-ring null output",
               pgaccel_point_in_ring_bulk(coords_f64, 1, coords_f64, 4, true, nullptr),
               PGACCEL_ERROR_INIT);

  check_status("sphere-distance null left",
               pgaccel_sphere_distance_bulk(nullptr, coords_f64, 1, true, &out_f64, &uncertain),
               PGACCEL_ERROR_INIT);
  check_status("sphere-distance null right",
               pgaccel_sphere_distance_bulk(coords_f64, nullptr, 1, true, &out_f64, &uncertain),
               PGACCEL_ERROR_INIT);
  check_status("sphere-distance null distance",
               pgaccel_sphere_distance_bulk(coords_f64, coords_f64, 1, true, nullptr, &uncertain),
               PGACCEL_ERROR_INIT);
  check_status("sphere-distance null uncertainty",
               pgaccel_sphere_distance_bulk(coords_f64, coords_f64, 1, true, &out_f64, nullptr),
               PGACCEL_ERROR_INIT);

  check_status("segment null left",
               pgaccel_segment_intersects_bulk(nullptr, coords_f64, 1, true, &predicate),
               PGACCEL_ERROR_INIT);
  check_status("segment null right",
               pgaccel_segment_intersects_bulk(coords_f64, nullptr, 1, true, &predicate),
               PGACCEL_ERROR_INIT);
  check_status("segment null output",
               pgaccel_segment_intersects_bulk(coords_f64, coords_f64, 1, true, nullptr),
               PGACCEL_ERROR_INIT);

  check_status("polygon distance empty",
               pgaccel_st_distance_polygon_polygon_bulk(nullptr, nullptr, nullptr, nullptr, 0,
                                                        nullptr, nullptr),
               PGACCEL_OK);
  check_status("polygon distance null right coordinates",
               pgaccel_st_distance_polygon_polygon_bulk(coords_f32, offsets, nullptr, offsets, 1,
                                                        &out_f32, &uncertain),
               PGACCEL_ERROR_INIT);
  check_status("polygon distance null uncertainty",
               pgaccel_st_distance_polygon_polygon_bulk(coords_f32, offsets, coords_f32, offsets, 1,
                                                        &out_f32, nullptr),
               PGACCEL_ERROR_INIT);

  pgaccel_geometry geometry{};
  check_status("algorithmic predicate empty", pgaccel_st_equals_bulk(nullptr, nullptr, 0, nullptr),
               PGACCEL_OK);
  check_status("algorithmic predicate null left",
               pgaccel_st_equals_bulk(nullptr, &geometry, 1, &predicate), PGACCEL_ERROR_INIT);
  check_status("algorithmic predicate null right",
               pgaccel_st_equals_bulk(&geometry, nullptr, 1, &predicate), PGACCEL_ERROR_INIT);
  check_status("algorithmic predicate null output",
               pgaccel_st_equals_bulk(&geometry, &geometry, 1, nullptr), PGACCEL_ERROR_INIT);

  check_status("point-in-polygon empty",
               pgaccel_point_in_polygon_bulk(nullptr, 0, nullptr, nullptr, 0, nullptr, 0, nullptr),
               PGACCEL_OK);
  check_status(
      "point-in-polygon null points",
      pgaccel_point_in_polygon_bulk(nullptr, 1, coords_f32, coords_f32, 4, nullptr, 0, &predicate),
      PGACCEL_ERROR);
  check_status(
      "point-in-polygon null polygon",
      pgaccel_point_in_polygon_bulk(coords_f32, 1, coords_f32, nullptr, 4, nullptr, 0, &predicate),
      PGACCEL_ERROR);
  check_status(
      "point-in-polygon null bbox",
      pgaccel_point_in_polygon_bulk(coords_f32, 1, nullptr, coords_f32, 4, nullptr, 0, &predicate),
      PGACCEL_ERROR);
  check_status(
      "point-in-polygon null output",
      pgaccel_point_in_polygon_bulk(coords_f32, 1, coords_f32, coords_f32, 4, nullptr, 0, nullptr),
      PGACCEL_ERROR);
}

void test_resident_spatial_contracts() {
  int32_t detail = 99;
  pgaccel_spatial_resident_request request =
      empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);

  check_status("resident launch null detail",
               pgaccel_spatial_eval_resident_launch(&request, nullptr, nullptr),
               PGACCEL_INVALID_ARGUMENT);
  check_status("resident launch null request",
               pgaccel_spatial_eval_resident_launch(nullptr, nullptr, &detail),
               PGACCEL_INVALID_ARGUMENT);
  check_value("resident null request detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);

  detail = 99;
  check_status("resident empty launch",
               pgaccel_spatial_eval_resident_launch(&request, nullptr, &detail), PGACCEL_OK);
  check_value("resident empty launch detail", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_DISTANCE);
  detail = 99;
  check_status("resident empty distance launch",
               pgaccel_spatial_eval_resident_launch(&request, nullptr, &detail), PGACCEL_OK);
  check_value("resident empty distance detail", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  auto expect_bad_request = [&](const char* label, const pgaccel_spatial_resident_request& bad) {
    detail = 99;
    check_status(label, pgaccel_spatial_eval_resident_launch(&bad, nullptr, &detail),
                 PGACCEL_INVALID_ARGUMENT);
    check_value("malformed resident detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
  };

  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.abi_version++;
  expect_bad_request("resident wrong ABI", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.flags = 1;
  expect_bad_request("resident nonzero flags", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.pad = 1;
  expect_bad_request("resident nonzero padding", request);
  request = empty_resident_request(static_cast<pgaccel_spatial_predicate>(-1));
  expect_bad_request("resident predicate below domain", request);
  request = empty_resident_request(static_cast<pgaccel_spatial_predicate>(
      static_cast<int32_t>(PGACCEL_SPATIAL_PREDICATE_DISTANCE) + 1));
  expect_bad_request("resident predicate above domain", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.count = static_cast<size_t>(PGACCEL_SPATIAL_MAX_CHUNK_ROWS) + 1;
  request.output_capacity = request.count;
  expect_bad_request("resident count above chunk limit", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_DWITHIN);
  request.distance_threshold = -1.0;
  expect_bad_request("resident negative DWithin threshold", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_DWITHIN);
  request.distance_threshold = std::numeric_limits<double>::quiet_NaN();
  expect_bad_request("resident non-finite DWithin threshold", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.distance_threshold = 1.0;
  expect_bad_request("resident threshold on non-distance predicate", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_DISTANCE);
  request.distance_threshold = 1.0;
  expect_bad_request("resident threshold on distance predicate", request);

  double distance = 0.0;
  uint8_t distance_uncertain = 0;
  int8_t predicate_result = 0;
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.distances = &distance;
  expect_bad_request("resident predicate with distance output", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_INTERSECTS);
  request.distance_uncertain = &distance_uncertain;
  expect_bad_request("resident predicate with uncertainty output", request);
  request = empty_resident_request(PGACCEL_SPATIAL_PREDICATE_DISTANCE);
  request.predicate_results = &predicate_result;
  expect_bad_request("resident distance with predicate output", request);

  request = nonempty_resident_request(&predicate_result);
  request.left.row_stride = 2;
  expect_bad_request("resident invalid left range", request);
  request = nonempty_resident_request(&predicate_result);
  request.right.row_stride = 2;
  expect_bad_request("resident invalid right range", request);
  request = nonempty_resident_request(&predicate_result);
  request.output_capacity = 0;
  expect_bad_request("resident insufficient output capacity", request);
  request = nonempty_resident_request(&predicate_result);
  request.predicate_results_bytes = 0;
  expect_bad_request("resident short predicate output", request);
  request = nonempty_resident_request(&predicate_result);
  request.left.first_row = 1;
  expect_bad_request("resident invalid left first row", request);
  request = nonempty_resident_request(&predicate_result);
  request.right.first_row = 1;
  expect_bad_request("resident invalid right first row", request);

  check_status("workspace finish null detail", pgaccel_spatial_workspace_finish(nullptr, nullptr),
               PGACCEL_INVALID_ARGUMENT);

  check_status("legacy resident null detail", pgaccel_spatial_eval_resident_ex(&request, nullptr),
               PGACCEL_INVALID_ARGUMENT);
  detail = 99;
  check_status("legacy resident null request", pgaccel_spatial_eval_resident_ex(nullptr, &detail),
               PGACCEL_INVALID_ARGUMENT);
  check_value("legacy null request detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
}

void test_recheck_contracts() {
  int32_t detail = 99;
  pgaccel_spatial_recheck_compact_request compact{};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;

  check_status("compact null detail",
               pgaccel_spatial_recheck_compact_launch(&compact, nullptr, nullptr),
               PGACCEL_INVALID_ARGUMENT);
  check_status("compact null request",
               pgaccel_spatial_recheck_compact_launch(nullptr, nullptr, &detail),
               PGACCEL_INVALID_ARGUMENT);
  check_value("compact null request detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
  detail = 99;
  check_status("compact empty", pgaccel_spatial_recheck_compact_launch(&compact, nullptr, &detail),
               PGACCEL_OK);
  check_value("compact empty detail", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  int8_t tri_state = 0;
  int8_t final_mask = 0;
  uint64_t uncertain_index = 0;
  uint64_t uncertain_count = 0;
  auto expect_bad_compact = [&](const char* label,
                                const pgaccel_spatial_recheck_compact_request& bad) {
    detail = 99;
    check_status(label, pgaccel_spatial_recheck_compact_launch(&bad, nullptr, &detail),
                 PGACCEL_INVALID_ARGUMENT);
    check_value("malformed compact detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
  };
  compact.tri_state = &tri_state;
  expect_bad_compact("compact empty with tri-state pointer", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.tri_state_bytes = 1;
  expect_bad_compact("compact empty with tri-state bytes", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.final_mask = &final_mask;
  expect_bad_compact("compact empty with final mask", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.uncertain_indices = &uncertain_index;
  expect_bad_compact("compact empty with uncertain indices", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.uncertain_count = &uncertain_count;
  expect_bad_compact("compact empty with uncertain count", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION + 1;
  expect_bad_compact("compact wrong ABI", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.flags = 1;
  expect_bad_compact("compact nonzero flags", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.row_count = 1;
  expect_bad_compact("compact mismatched uncertain capacity", compact);
  compact = {};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.row_count = static_cast<size_t>(PGACCEL_SPATIAL_MAX_CHUNK_ROWS) + 1;
  compact.uncertain_capacity = compact.row_count;
  expect_bad_compact("compact count above chunk limit", compact);

  pgaccel_spatial_recheck_patch_request patch{};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  check_status("patch null detail", pgaccel_spatial_recheck_patch_launch(&patch, nullptr, nullptr),
               PGACCEL_INVALID_ARGUMENT);
  detail = 99;
  check_status("patch null request",
               pgaccel_spatial_recheck_patch_launch(nullptr, nullptr, &detail),
               PGACCEL_INVALID_ARGUMENT);
  check_value("patch null request detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
  detail = 99;
  check_status("patch empty", pgaccel_spatial_recheck_patch_launch(&patch, nullptr, &detail),
               PGACCEL_OK);
  check_value("patch empty detail", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  uint64_t patch_index = 0;
  int8_t patch_result = 1;
  auto expect_bad_patch = [&](const char* label, const pgaccel_spatial_recheck_patch_request& bad) {
    detail = 99;
    check_status(label, pgaccel_spatial_recheck_patch_launch(&bad, nullptr, &detail),
                 PGACCEL_INVALID_ARGUMENT);
    check_value("malformed patch detail", detail == PGACCEL_SPATIAL_DETAIL_CONTRACT);
  };
  patch.indices = &patch_index;
  expect_bad_patch("patch empty with indices", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.results = &patch_result;
  expect_bad_patch("patch empty with results", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.final_mask = &final_mask;
  expect_bad_patch("patch empty with final mask", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION + 1;
  expect_bad_patch("patch wrong ABI", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.flags = 1;
  expect_bad_patch("patch nonzero flags", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.patch_count = 1;
  expect_bad_patch("patch count exceeds row count", patch);
  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.row_count = static_cast<size_t>(PGACCEL_SPATIAL_MAX_CHUNK_ROWS) + 1;
  expect_bad_patch("patch row count above chunk limit", patch);

  patch = {};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.row_count = 1;
  detail = 99;
  check_status("zero-patch nonempty domain",
               pgaccel_spatial_recheck_patch_launch(&patch, nullptr, &detail), PGACCEL_OK);
  check_value("zero-patch detail", detail == PGACCEL_SPATIAL_DETAIL_NONE);
}

void test_deprecated_spatial_contract() {
  size_t true_count = 9;
  size_t false_count = 9;
  size_t uncertain_count = 9;
  check_status("deprecated intersects null true count",
               pgaccel_spatial_intersects(nullptr, 0, nullptr, 0, nullptr, nullptr, nullptr,
                                          &false_count, nullptr, &uncertain_count),
               PGACCEL_ERROR);
  check_status("deprecated intersects null false count",
               pgaccel_spatial_intersects(nullptr, 0, nullptr, 0, nullptr, &true_count, nullptr,
                                          nullptr, nullptr, &uncertain_count),
               PGACCEL_ERROR);
  check_status("deprecated intersects null uncertain count",
               pgaccel_spatial_intersects(nullptr, 0, nullptr, 0, nullptr, &true_count, nullptr,
                                          &false_count, nullptr, nullptr),
               PGACCEL_ERROR);

  true_count = false_count = uncertain_count = 9;
  check_status("deprecated intersects empty left",
               pgaccel_spatial_intersects(nullptr, 0, nullptr, 1, nullptr, &true_count, nullptr,
                                          &false_count, nullptr, &uncertain_count),
               PGACCEL_OK);
  check_value("deprecated empty left clears counts",
              true_count == 0 && false_count == 0 && uncertain_count == 0);
  true_count = false_count = uncertain_count = 9;
  check_status("deprecated intersects empty right",
               pgaccel_spatial_intersects(nullptr, 1, nullptr, 0, nullptr, &true_count, nullptr,
                                          &false_count, nullptr, &uncertain_count),
               PGACCEL_OK);
  check_value("deprecated empty right clears counts",
              true_count == 0 && false_count == 0 && uncertain_count == 0);
}

void test_grouped_aggregate_host_contracts() {
  pgaccel_grouped_agg_desc desc = count_star_desc();
  pgaccel_grouped_agg_workspace_req request = empty_workspace_req();
  check_status("grouped workspace requirements",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_OK);
  check_value("grouped workspace result",
              request.bytes > 0 && request.alignment > 0 && request.flags == 0);

  int32_t kernel_mode = 0;
  check_status("grouped count physical mode",
               pgaccel_grouped_agg_kernel_mode(&desc, &kernel_mode), PGACCEL_OK);
  check_value("grouped count physical mode value",
              kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);
  check_status("grouped physical mode null output",
               pgaccel_grouped_agg_kernel_mode(&desc, nullptr), PGACCEL_ERROR);

  pgaccel_grouped_agg_desc int8_count = desc;
  int8_count.measures[0] = {};
  int8_count.measures[0].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
  int8_count.measures[0].value.element_bytes = sizeof(int64_t);
  int8_count.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
  int8_count.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  int8_count.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  int8_count.measures[0].state_bytes = sizeof(int64_t);
  kernel_mode = 0;
  check_status("grouped int8 count physical mode",
               pgaccel_grouped_agg_kernel_mode(&int8_count, &kernel_mode), PGACCEL_OK);
  check_value("grouped int8 count physical mode value",
              kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT);

  pgaccel_grouped_agg_desc serial = int8_count;
  serial.measures[0].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_DATE;
  serial.measures[0].value.element_bytes = sizeof(int32_t);
  kernel_mode = 0;
  check_status("grouped serial physical mode",
               pgaccel_grouped_agg_kernel_mode(&serial, &kernel_mode), PGACCEL_OK);
  check_value("grouped serial physical mode value",
              kernel_mode == PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC);

  check_status("grouped workspace null output",
               pgaccel_grouped_agg_workspace_requirements(&desc, nullptr), PGACCEL_ERROR);
  request = empty_workspace_req();
  request.abi_version++;
  check_status("grouped workspace wrong ABI",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_ERROR);
  request = empty_workspace_req();
  request.size_bytes--;
  check_status("grouped workspace wrong size",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_ERROR);
  request = empty_workspace_req();
  request.alignment = 1;
  check_status("grouped workspace noncanonical alignment",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_ERROR);
  request = empty_workspace_req();
  request.space = PGACCEL_MEM_SPACE_DEVICE;
  check_status("grouped workspace noncanonical space",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_ERROR);
  request = empty_workspace_req();
  request.flags = 1;
  check_status("grouped workspace noncanonical flags",
               pgaccel_grouped_agg_workspace_requirements(&desc, &request), PGACCEL_ERROR);

  int32_t detail = 99;
  check_status("grouped execute invalid descriptor",
               pgaccel_grouped_agg_execute_ex(nullptr, nullptr, &detail), PGACCEL_ERROR);
  check_value("grouped invalid descriptor detail",
              detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);

  pgaccel_grouped_agg_desc unsupported = desc;
  unsupported.measures[0] = {};
  unsupported.measures[0].value.physical_type = PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
  unsupported.measures[0].value.element_bytes = 16;
  unsupported.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
  unsupported.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  unsupported.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
  unsupported.measures[0].state_bytes = 16;
  detail = 99;
  kernel_mode = 99;
  check_status("grouped unsupported physical mode",
               pgaccel_grouped_agg_kernel_mode(&unsupported, &kernel_mode),
               PGACCEL_UNSUPPORTED);
  check_value("grouped unsupported physical mode clears output", kernel_mode == 0);
  check_status("grouped execute reserved numeric capability",
               pgaccel_grouped_agg_execute_ex(&unsupported, nullptr, &detail), PGACCEL_UNSUPPORTED);
  check_value("grouped unsupported detail", detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE);

  pgaccel_grouped_agg_desc stateful = desc;
  stateful.execution_flags = PGACCEL_GROUPED_AGG_EXEC_RESET | PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
  detail = 99;
  check_status("grouped stateful execute requires workspace",
               pgaccel_grouped_agg_execute_ex(&stateful, nullptr, &detail), PGACCEL_ERROR);
  check_value("grouped missing workspace detail",
              detail == PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID);
}

void test_resident_count_host_contracts() {
  check_status("resident count missing output",
               pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 0, 0, nullptr),
               PGACCEL_INVALID_ARGUMENT);

  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  check_status("resident count missing nonempty keys",
               pgaccel_hash_count_i64_device_hash_execute_bounded_checked(nullptr, 1, 1, &state),
               PGACCEL_INVALID_ARGUMENT);
  check_value("resident count invalid input clears output", state == nullptr);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  check_status("resident count unaddressable row count",
               pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
                   reinterpret_cast<int64_t*>(uintptr_t{1}),
                   static_cast<size_t>(std::numeric_limits<uint32_t>::max()) + 1, 1, &state),
               PGACCEL_UNSUPPORTED);
  check_value("resident count unsupported input clears output", state == nullptr);

  state = pgaccel_hash_count_i64_device_hash_execute_bounded(nullptr, 0, 0);
  check_value("resident count empty wrapper state", state != nullptr);
  check_value("resident count empty group count", pgaccel_agg_group_count(state) == 0);
  check_value("resident count empty keys", pgaccel_agg_get_group_keys(state) == nullptr);
  check_value("resident count empty results", pgaccel_agg_get_results(state, 0) == nullptr);
  check_value("resident count empty counts", pgaccel_agg_get_counts(state) == nullptr);
  pgaccel_agg_free(state);

  check_value("resident count null group count", pgaccel_agg_group_count(nullptr) == 0);
  check_value("resident count null keys", pgaccel_agg_get_group_keys(nullptr) == nullptr);
  check_value("resident count null results", pgaccel_agg_get_results(nullptr, 0) == nullptr);
  check_value("resident count null counts", pgaccel_agg_get_counts(nullptr) == nullptr);
  pgaccel_agg_free(nullptr);
}

void test_workspace_and_bbox_host_contracts() {
  void* workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_status(
      "workspace missing output",
      pgaccel_grouped_agg_workspace_alloc(0, alignof(void*), PGACCEL_MEM_SPACE_SHARED_USM, nullptr),
      PGACCEL_ERROR);
  check_status("workspace zero alignment",
               pgaccel_grouped_agg_workspace_alloc(1, 0, PGACCEL_MEM_SPACE_SHARED_USM, &workspace),
               PGACCEL_ERROR);
  check_value("workspace invalid alignment clears output", workspace == nullptr);
  workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_status("workspace non-power-of-two alignment",
               pgaccel_grouped_agg_workspace_alloc(1, 3, PGACCEL_MEM_SPACE_SHARED_USM, &workspace),
               PGACCEL_ERROR);
  check_value("workspace non-power-of-two clears output", workspace == nullptr);
  workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_status(
      "workspace rejects host space",
      pgaccel_grouped_agg_workspace_alloc(1, alignof(void*), PGACCEL_MEM_SPACE_HOST, &workspace),
      PGACCEL_ERROR);
  check_value("workspace invalid space clears output", workspace == nullptr);

  workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_status("zero-byte shared workspace",
               pgaccel_grouped_agg_workspace_alloc(0, alignof(void*), PGACCEL_MEM_SPACE_SHARED_USM,
                                                   &workspace),
               PGACCEL_OK);
  check_value("zero-byte shared workspace is null", workspace == nullptr);
  workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_status(
      "zero-byte device workspace",
      pgaccel_grouped_agg_workspace_alloc(0, alignof(void*), PGACCEL_MEM_SPACE_DEVICE, &workspace),
      PGACCEL_OK);
  check_value("zero-byte device workspace is null", workspace == nullptr);
  pgaccel_grouped_agg_workspace_free(nullptr);

  check_status("empty f32 bbox without hit count",
               pgaccel_bbox_intersects_bulk_f32(nullptr, 0, nullptr, 0, nullptr, nullptr),
               PGACCEL_OK);

  const double box[4] = {0.0, 0.0, 1.0, 1.0};
  size_t hit_count = 99;
  check_status("empty-right f64 bbox",
               pgaccel_bbox_intersects_bulk_f64(box, 1, nullptr, 0, nullptr, &hit_count),
               PGACCEL_OK);
  check_value("empty-right f64 bbox clears hit count", hit_count == 0);
  check_status("empty f64 bbox without hit count",
               pgaccel_bbox_intersects_bulk_f64(nullptr, 0, nullptr, 0, nullptr, nullptr),
               PGACCEL_OK);
}

int test_no_device_contracts() {
  const pgaccel_status init_status = pgaccel_init();
  check_unavailable("no-device initialization fails closed", init_status);
  if (init_status == PGACCEL_OK) {
    pgaccel_shutdown();
    return 1;
  }

  const pgaccel_device_info device = pgaccel_get_device_info();
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  check_value("failed init leaves device info empty",
              device.device_name[0] == '\0' && device.backend_name[0] == '\0' &&
                  device.compute_units == 0 && device.max_alloc_bytes == 0);
  check_value("failed init leaves capabilities empty",
              caps.backend_name[0] == '\0' && caps.compute_units == 0 && caps.max_alloc_bytes == 0);

  uint8_t source = 7;
  uint8_t destination = 0;
  void* allocation = reinterpret_cast<void*>(uintptr_t{1});
  check_unavailable("shared expression allocation without device",
                    pgaccel_expr_shared_alloc(1, &allocation));
  check_value("shared expression failure clears output", allocation == nullptr);
  allocation = reinterpret_cast<void*>(uintptr_t{1});
  check_unavailable("device expression allocation without device",
                    pgaccel_expr_device_alloc(1, &allocation));
  check_value("device expression failure clears output", allocation == nullptr);
  allocation = reinterpret_cast<void*>(uintptr_t{1});
  check_unavailable("resident expression copy allocation without device",
                    pgaccel_expr_device_alloc_copy(&source, sizeof(source), &allocation));
  check_value("resident expression copy failure clears output", allocation == nullptr);
  check_unavailable("expression copy from host without device",
                    pgaccel_expr_device_copy_from_host(&destination, &source, sizeof(source)));
  check_unavailable("expression copy to host without device",
                    pgaccel_expr_device_copy_to_host(&destination, &source, sizeof(source)));
  pgaccel_expr_shared_free(reinterpret_cast<void*>(uintptr_t{1}));
  pgaccel_expr_device_free(reinterpret_cast<void*>(uintptr_t{1}));

  void* workspace = reinterpret_cast<void*>(uintptr_t{1});
  check_unavailable("grouped workspace allocation without device",
                    pgaccel_grouped_agg_workspace_alloc(1, alignof(void*),
                                                        PGACCEL_MEM_SPACE_SHARED_USM, &workspace));
  check_value("grouped workspace failure clears output", workspace == nullptr);

  int32_t detail = 99;
  int8_t predicate_result = 0;
  pgaccel_spatial_resident_request resident = nonempty_resident_request(&predicate_result);
  check_status("resident launch without device",
               pgaccel_spatial_eval_resident_launch(&resident, nullptr, &detail),
               PGACCEL_ERROR_NO_DEVICE);
  check_value("resident no-device detail remains clear", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  detail = 99;
  check_status("workspace finish without device",
               pgaccel_spatial_workspace_finish(nullptr, &detail), PGACCEL_ERROR_NO_DEVICE);
  check_value("workspace finish no-device detail remains clear",
              detail == PGACCEL_SPATIAL_DETAIL_NONE);

  int8_t tri_state = 0;
  int8_t final_mask = 0;
  uint64_t uncertain_index = 0;
  uint64_t uncertain_count = 0;
  pgaccel_spatial_recheck_compact_request compact{};
  compact.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  compact.tri_state = &tri_state;
  compact.tri_state_bytes = sizeof(tri_state);
  compact.final_mask = &final_mask;
  compact.final_mask_bytes = sizeof(final_mask);
  compact.uncertain_indices = &uncertain_index;
  compact.uncertain_indices_bytes = sizeof(uncertain_index);
  compact.uncertain_count = &uncertain_count;
  compact.uncertain_count_bytes = sizeof(uncertain_count);
  compact.row_count = 1;
  compact.uncertain_capacity = 1;
  detail = 99;
  check_status("compact launch without device",
               pgaccel_spatial_recheck_compact_launch(&compact, nullptr, &detail),
               PGACCEL_ERROR_NO_DEVICE);
  check_value("compact no-device detail remains clear", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  uint64_t patch_index = 0;
  int8_t patch_result = 1;
  pgaccel_spatial_recheck_patch_request patch{};
  patch.abi_version = PGACCEL_SPATIAL_RECHECK_ABI_VERSION;
  patch.indices = &patch_index;
  patch.indices_bytes = sizeof(patch_index);
  patch.results = &patch_result;
  patch.results_bytes = sizeof(patch_result);
  patch.final_mask = &final_mask;
  patch.final_mask_bytes = sizeof(final_mask);
  patch.row_count = 1;
  patch.patch_count = 1;
  detail = 99;
  check_status("patch launch without device",
               pgaccel_spatial_recheck_patch_launch(&patch, nullptr, &detail),
               PGACCEL_ERROR_NO_DEVICE);
  check_value("patch no-device detail remains clear", detail == PGACCEL_SPATIAL_DETAIL_NONE);

  check_status("failed runtime shutdown", pgaccel_shutdown(), PGACCEL_OK);
  check_status("idempotent failed runtime shutdown", pgaccel_shutdown(), PGACCEL_OK);
  std::printf("host API no-device contracts: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL: fork no-device host contracts: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_HOST_API_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL: exec no-device host contracts: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL: no-device host contract child status=%d errno=%d\n", status, errno);
    return false;
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  if (std::getenv("PGACCEL_HOST_API_NO_DEVICE") != nullptr)
    return test_no_device_contracts();

  check_value("no-device host contract child",
              argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]));
  test_spatial_predicate_pointer_contracts();
  test_resident_spatial_contracts();
  test_recheck_contracts();
  test_deprecated_spatial_contract();
  test_grouped_aggregate_host_contracts();
  test_resident_count_host_contracts();
  test_workspace_and_bbox_host_contracts();

  std::printf("host API contracts: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}

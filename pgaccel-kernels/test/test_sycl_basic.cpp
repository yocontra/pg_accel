// Basic SYCL runtime diagnostics used by the Metal backend smoke chain.
//
// Raw host pointers are intentionally not a supported kernel contract. Metal on
// Apple Silicon can silently read zeros from raw host pointers, so production
// kernels must stage inputs through SYCL allocations.
#include <sycl/sycl.hpp>

#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <stdexcept>

#include "pgaccel_ffi.h"
#include "pgaccel_olap.h"
#include "pgaccel_queue.h"
#include "pgaccel_resident_count.h"

extern sycl::queue* g_queue;

namespace {

bool nearly_eq(float a, float b) {
  return std::fabs(a - b) < 1e-5f;
}

bool assert_float(const char* name, float got, float expected) {
  if (!nearly_eq(got, expected)) {
    std::fprintf(stderr, "%s: got %.6f, expected %.6f\n", name, got, expected);
    return false;
  }
  return true;
}

bool assert_true(const char* name, bool condition) {
  if (!condition)
    std::fprintf(stderr, "%s: failed\n", name);
  return condition;
}

bool test_no_device_public_api_matrix() {
  bool ok = true;
  const auto no_device = [&](const char* name, pgaccel_status status) {
    ok &= assert_true(name, status == PGACCEL_ERROR_NO_DEVICE);
  };

  float f32[] = {1.0F, 2.0F, 3.0F, 4.0F, 0.0F, 0.0F, 1.0F, 1.0F};
  double f64[] = {1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 1.0, 1.0};
  int16_t i16[] = {1, 2};
  int32_t i32[] = {1, 2};
  int64_t i64[] = {1, 2};
  uint8_t bytes[] = {1, 0};
  uint8_t byte_out = 0;
  float f32_out = 0.0F;
  double f64_out = 0.0;
  int16_t i16_out = 0;
  int32_t i32_out = 0;
  int64_t i64_out = 0;
  size_t size_out = 0;
  uint64_t u64_out = 0;

  no_device("bbox f32 no device",
            pgaccel_bbox_intersects_bulk_f32(f32, 1, f32 + 4, 1, &byte_out, &size_out));
  no_device("bbox f64 no device",
            pgaccel_bbox_intersects_bulk_f64(f64, 1, f64 + 4, 1, &byte_out, &size_out));

  pgaccel_agg_state* resident_count_state = nullptr;
  no_device("resident count no device",
            pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
                i64, 1, 1, &resident_count_state));
  ok &= assert_true("resident count no-device state remains null", resident_count_state == nullptr);

  void* grouped_workspace = reinterpret_cast<void*>(uintptr_t{1});
  ok &= assert_true(
      "grouped workspace allocation propagates initialization failure",
      pgaccel_grouped_agg_workspace_alloc(64, 8, PGACCEL_MEM_SPACE_SHARED_USM,
                                          &grouped_workspace) != PGACCEL_OK);
  ok &= assert_true("grouped workspace no-device output remains null",
                    grouped_workspace == nullptr);
  pgaccel_grouped_agg_workspace_free(reinterpret_cast<void*>(uintptr_t{1}));

  no_device("sum f32 no device", pgaccel_reduce_sum_f32(f32, 2, &f32_out));
  no_device("min f32 no device", pgaccel_reduce_min_f32(f32, 2, &f32_out));
  no_device("max f32 no device", pgaccel_reduce_max_f32(f32, 2, &f32_out));
  no_device("sum f64 no device", pgaccel_reduce_sum_f64(f64, 2, &f64_out));
  no_device("min f64 no device", pgaccel_reduce_min_f64(f64, 2, &f64_out));
  no_device("max f64 no device", pgaccel_reduce_max_f64(f64, 2, &f64_out));
  no_device("sum i64 no device", pgaccel_reduce_sum_i64(i64, 2, &i64_out));
  no_device("min i64 no device", pgaccel_reduce_min_i64(i64, 2, &i64_out));
  no_device("max i64 no device", pgaccel_reduce_max_i64(i64, 2, &i64_out));
  no_device("count no device", pgaccel_reduce_count(bytes, 2, &size_out));

  int64_t aggregate_count = 0;
  no_device("multi f32 no device",
            pgaccel_reduce_multi_f32(f32, 2, &f32_out, &f32_out, &f32_out, &aggregate_count));
  no_device("multi f64 no device",
            pgaccel_reduce_multi_f64(f64, 2, &f64_out, &f64_out, &f64_out, &aggregate_count));
  no_device("multi i64 no device",
            pgaccel_reduce_multi_i64(i64, 2, &i64_out, &i64_out, &i64_out, &aggregate_count));
  no_device("masked multi f32 no device",
            pgaccel_reduce_multi_masked_f32(f32, bytes, bytes, 2, &f32_out, &f32_out, &f32_out,
                                            &aggregate_count));
  no_device("masked multi f64 no device",
            pgaccel_reduce_multi_masked_f64(f64, bytes, bytes, 2, &f64_out, &f64_out, &f64_out,
                                            &aggregate_count));
  no_device("masked multi i64 no device",
            pgaccel_reduce_multi_masked_i64(i64, bytes, bytes, 2, &i64_out, &i64_out, &i64_out,
                                            &aggregate_count));
  no_device("sum square f32 no device", pgaccel_reduce_sum_sq_f32(f32, 2, &f64_out));
  no_device("sum square f64 no device", pgaccel_reduce_sum_sq_f64(f64, 2, &f64_out));
  no_device("stats f32 no device",
            pgaccel_reduce_stats_f32(f32, 2, &u64_out, &f64_out, &f64_out));
  no_device("stats f64 no device",
            pgaccel_reduce_stats_f64(f64, 2, &u64_out, &f64_out, &f64_out));
  no_device("bool and no device", pgaccel_reduce_bool_and(bytes, 2, &byte_out));
  no_device("bool or no device", pgaccel_reduce_bool_or(bytes, 2, &byte_out));
  no_device("bit and i16 no device", pgaccel_reduce_bit_and_i16(i16, 2, &i16_out));
  no_device("bit and i32 no device", pgaccel_reduce_bit_and_i32(i32, 2, &i32_out));
  no_device("bit and i64 no device", pgaccel_reduce_bit_and_i64(i64, 2, &i64_out));
  no_device("bit or i16 no device", pgaccel_reduce_bit_or_i16(i16, 2, &i16_out));
  no_device("bit or i32 no device", pgaccel_reduce_bit_or_i32(i32, 2, &i32_out));
  no_device("bit or i64 no device", pgaccel_reduce_bit_or_i64(i64, 2, &i64_out));
  no_device("bit xor i16 no device", pgaccel_reduce_bit_xor_i16(i16, 2, &i16_out));
  no_device("bit xor i32 no device", pgaccel_reduce_bit_xor_i32(i32, 2, &i32_out));
  no_device("bit xor i64 no device", pgaccel_reduce_bit_xor_i64(i64, 2, &i64_out));

  uint32_t offsets[] = {0, 2};
  int8_t predicate_out = 0;
  no_device("point in ring no device",
            pgaccel_point_in_ring_bulk(f32, 1, f32 + 4, 2, false, &predicate_out));
  no_device("sphere distance f32 no device",
            pgaccel_sphere_distance_bulk(f32, f32 + 4, 1, false, &f32_out, &byte_out));
  no_device("sphere distance f64 no device",
            pgaccel_sphere_distance_bulk(f64, f64 + 4, 1, true, &f64_out, &byte_out));
  no_device("segment intersection no device",
            pgaccel_segment_intersects_bulk(f32, f32 + 4, 1, false, &predicate_out));
  no_device("area no device", pgaccel_st_area_bulk(f32, offsets, 1, false, &f32_out));
  no_device("length no device",
            pgaccel_st_length_bulk(f32, offsets, 1, false, false, &f32_out));
  no_device("polygon distance no device",
            pgaccel_st_distance_polygon_polygon_bulk(f32, offsets, f32 + 4, offsets, 1, &f32_out,
                                                      &byte_out));

  const pgaccel_geometry point_geometry = {
      PGACCEL_GEOM_POINT, f32, f32 + 4, 1, nullptr, 0,
  };
  no_device("equals no device",
            pgaccel_st_equals_bulk(&point_geometry, &point_geometry, 1, &predicate_out));
  no_device("touches no device",
            pgaccel_st_touches_bulk(&point_geometry, &point_geometry, 1, &predicate_out));
  no_device("crosses no device",
            pgaccel_st_crosses_bulk(&point_geometry, &point_geometry, 1, &predicate_out));
  no_device("overlaps no device",
            pgaccel_st_overlaps_bulk(&point_geometry, &point_geometry, 1, &predicate_out));
  no_device("pairwise intersects no device",
            pgaccel_spatial_intersects_pairwise(&point_geometry, &point_geometry, 1,
                                                &predicate_out));

  uint64_t cell = UINT64_C(0x8029fffffffffff);
  uint64_t cell_out = 0;
  int32_t h3_i32_out = 0;
  no_device("h3 resolution no device",
            pgaccel_h3_get_resolution_bulk(&cell, 1, &h3_i32_out));
  no_device("h3 base cell no device", pgaccel_h3_get_base_cell_bulk(&cell, 1, &h3_i32_out));
  no_device("h3 validity no device", pgaccel_h3_is_valid_cell_bulk(&cell, 1, &byte_out));
  no_device("h3 pentagon no device", pgaccel_h3_is_pentagon_bulk(&cell, 1, &byte_out));
  no_device("h3 class no device", pgaccel_h3_is_res_class_iii_bulk(&cell, 1, &byte_out));
  no_device("h3 parent no device", pgaccel_h3_cell_to_parent_bulk(&cell, 1, 0, &cell_out));
  no_device("h3 center child no device",
            pgaccel_h3_cell_to_center_child_bulk(&cell, 1, 1, &cell_out));
  no_device("h3 distance no device",
            pgaccel_h3_grid_distance_bulk(&cell, &cell, 1, &h3_i32_out));
  no_device("h3 lat/lng no device",
            pgaccel_h3_lat_lng_to_cell_bulk(f64, f64 + 2, 1, 0, true, &cell_out, &byte_out));

  pgaccel_grouped_agg_desc grouped_desc = {};
  grouped_desc.abi_version = PGACCEL_OLAP_ABI_VERSION;
  grouped_desc.size_bytes = sizeof(grouped_desc);
  grouped_desc.grouping_mode = PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
  grouped_desc.output_mode = PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
  grouped_desc.group_capacity = 1;
  grouped_desc.measure_count = 1;
  grouped_desc.execution_flags = PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
  grouped_desc.measures[0].op = PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
  grouped_desc.measures[0].agg_mask = PGACCEL_GROUPED_AGG_LANE_COUNT;
  grouped_desc.measures[0].accumulator_kind = PGACCEL_GROUPED_AGG_ACCUM_I64;
  grouped_desc.measures[0].state_bytes = sizeof(int64_t);
  grouped_desc.where_filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;
  for (auto& filter : grouped_desc.measure_filters)
    filter.value_cmp_opcode = PGACCEL_EXPR_OP_ALWAYS_TRUE;

  pgaccel_grouped_agg_out grouped_out = {};
  grouped_out.abi_version = PGACCEL_OLAP_ABI_VERSION;
  grouped_out.size_bytes = sizeof(grouped_out);
  grouped_out.group_capacity = 1;
  grouped_out.output_space = PGACCEL_MEM_SPACE_HOST;
  grouped_out.active_groups = &byte_out;
  grouped_out.measures[0].count = &u64_out;
  int32_t grouped_detail = PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
  no_device("grouped aggregate no device",
            pgaccel_grouped_agg_execute_ex(&grouped_desc, &grouped_out, &grouped_detail));

  uint8_t raster_pixels[] = {1};
  uint64_t raster_band_offsets[] = {0, 1};
  pgaccel_resident_raster_row raster_row = {};
  raster_row.width = 1;
  raster_row.height = 1;
  raster_row.band_count = 1;
  pgaccel_resident_raster_band raster_band = {};
  raster_band.pixel_type = PGACCEL_RESIDENT_RASTER_UINT8;
  pgaccel_resident_raster_reclass_rule raster_rule = {1, 7};
  uint64_t raster_output_offsets[] = {0, 1};
  uint8_t raster_output[] = {0};
  uint8_t raster_actions[] = {0};
  pgaccel_resident_raster_validation_scratch raster_validation = {};
  pgaccel_raster_reclass_resident_request raster_request = {};
  raster_request.abi_version = PGACCEL_RESIDENT_RASTER_ABI_VERSION;
  raster_request.input.abi_version = PGACCEL_RESIDENT_RASTER_ABI_VERSION;
  raster_request.input.pixels = raster_pixels;
  raster_request.input.pixels_bytes = sizeof(raster_pixels);
  raster_request.input.band_offsets = raster_band_offsets;
  raster_request.input.band_offsets_bytes = sizeof(raster_band_offsets);
  raster_request.input.rows = &raster_row;
  raster_request.input.rows_bytes = sizeof(raster_row);
  raster_request.input.bands = &raster_band;
  raster_request.input.bands_bytes = sizeof(raster_band);
  raster_request.input.row_count = 1;
  raster_request.input.band_count = 1;
  raster_request.count = 1;
  raster_request.output_pixel_type = PGACCEL_RESIDENT_RASTER_UINT8;
  raster_request.rules = &raster_rule;
  raster_request.rules_bytes = sizeof(raster_rule);
  raster_request.rule_count = 1;
  raster_request.output_offsets = raster_output_offsets;
  raster_request.output_offsets_bytes = sizeof(raster_output_offsets);
  raster_request.output_pixels = raster_output;
  raster_request.output_pixels_bytes = sizeof(raster_output);
  raster_request.row_actions = raster_actions;
  raster_request.row_actions_bytes = sizeof(raster_actions);
  raster_request.validation_scratch = &raster_validation;
  raster_request.validation_scratch_bytes = sizeof(raster_validation);
  raster_request.max_total_pixels = 1;
  raster_request.max_chunk_pixels = 1;
  int32_t raster_detail = PGACCEL_RASTER_DETAIL_NONE;
  no_device("raster resident no device",
            pgaccel_raster_reclass_resident_ex(&raster_request, &raster_detail));
  return ok;
}

bool test_no_device_queue_paths() {
  bool ok = true;
  ok &= assert_true("out-of-order queue reports no device", pgaccel_get_ooo_queue() == nullptr);

  bool reported_no_device = false;
  try {
    (void)pgaccel_require_queue();
  } catch (const pgaccel_no_device_error& error) {
    reported_no_device = std::strstr(error.what(), "queue unavailable") != nullptr;
  } catch (...) {
  }
  ok &= assert_true("required queue throws no-device error", reported_no_device);

  const std::runtime_error error("synthetic kernel failure");
  ok &= assert_true("std exception maps to kernel failure",
                    pgaccel_kernel_failure("test_sycl_basic", &error) == PGACCEL_ERROR);
  ok &= assert_true("unknown exception maps to kernel failure",
                    pgaccel_kernel_failure("test_sycl_basic", nullptr) == PGACCEL_ERROR);
  ok &= test_no_device_public_api_matrix();
  ok &= assert_true("no-device shutdown succeeds", pgaccel_shutdown() == PGACCEL_OK);
  return ok;
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "fork no-device queue test failed: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  return waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

}  // namespace

int main(int argc, char** argv) {
  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr)
    return test_no_device_queue_paths() ? 0 : 1;

  bool ok = true;
  ok &= assert_true("no-device queue child",
                    argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]));

  pgaccel_init();

  sycl::queue* q = g_queue;
  if (q == nullptr) {
    std::fprintf(stderr, "No SYCL queue\n");
    return 1;
  }
  ok &= assert_true("required queue returns the process queue", &pgaccel_require_queue() == q);

  {
    float* out = sycl::malloc_shared<float>(1, *q);
    if (out == nullptr)
      return 1;
    *out = -1.0f;
    q->submit([&](sycl::handler& h) { h.single_task([=]() { *out = 42.0f; }); }).wait();
    std::printf("Test 1 (single_task write): %.0f (expected 42)\n", *out);
    ok &= assert_float("single_task shared write", *out, 42.0f);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* out = sycl::malloc_shared<float>(N, *q);
    if (out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      out[i] = -1.0f;
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N),
                      [=](sycl::id<1> i) { out[i] = static_cast<float>(i[0]) + 1.0f; });
     }).wait();
    std::printf("Test 2 (parallel_for write):");
    for (size_t i = 0; i < N; ++i) {
      std::printf(" %.0f", out[i]);
      ok &= assert_float("parallel_for shared write", out[i], static_cast<float>(i + 1));
    }
    std::printf(" (expected 1 2 3 4 5 6 7 8)\n");
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(N, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i) {
      in[i] = 10.0f;
      out[i] = -1.0f;
    }
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) { out[i] = in[i] * 2.0f; });
     }).wait();
    std::printf("Test 3 (read+write shared):");
    for (size_t i = 0; i < N; ++i) {
      std::printf(" %.0f", out[i]);
      ok &= assert_float("parallel_for shared read/write", out[i], 20.0f);
    }
    std::printf(" (expected 20 20 20 20 20 20 20 20)\n");
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* out = sycl::malloc_shared<float>(1, *q);
    float* in = sycl::malloc_shared<float>(N, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      in[i] = 1.0f;
    *out = -1.0f;
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<float, 1> lm(N, h);
       h.parallel_for(sycl::nd_range<1>(N, N), [=](sycl::nd_item<1> item) {
         const size_t lid = item.get_local_id(0);
         lm[lid] = in[lid];
         item.barrier(sycl::access::fence_space::local_space);
         for (size_t s = N / 2; s > 0; s >>= 1) {
           if (lid < s)
             lm[lid] += lm[lid + s];
           item.barrier(sycl::access::fence_space::local_space);
         }
         if (lid == 0)
           *out = lm[0];
       });
     }).wait();
    std::printf("Test 5 (nd_range tree reduce): %.0f (expected 8)\n", *out);
    ok &= assert_float("nd_range tree reduce", *out, 8.0f);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(1, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      in[i] = 1.0f;
    *out = 0.0f;
    q->submit([&](sycl::handler& h) {
       auto red = sycl::reduction(out, sycl::plus<float>());
       h.parallel_for(sycl::range<1>(N), red, [=](sycl::id<1> i, auto& sum) { sum += in[i]; });
     }).wait();
    std::printf("Test 6 (sycl::reduction): %.0f (expected 8)\n", *out);
    ok &= assert_float("sycl::reduction", *out, 8.0f);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  pgaccel_shutdown();
  return ok ? 0 : 1;
}

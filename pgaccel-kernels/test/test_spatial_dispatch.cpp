#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <iterator>
#include <limits>
#include <vector>

#include "pgaccel_ffi.h"

/* ----------------------------------------------------------------
 * Helpers
 * ---------------------------------------------------------------- */

static int g_tests_passed = 0;
static int g_tests_failed = 0;

#define ASSERT_EQ(a, b)                                                                            \
  do {                                                                                             \
    if ((a) != (b)) {                                                                              \
      fprintf(stderr, "  FAIL %s:%d: %s (%zu) != %s (%zu)\n", __FILE__, __LINE__, #a, (size_t)(a), \
              #b, (size_t)(b));                                                                    \
      g_tests_failed++;                                                                            \
      return;                                                                                      \
    }                                                                                              \
  } while (0)

#define TEST(name)             \
  static void test_##name();   \
  static void run_##name() {   \
    printf("  %-50s ", #name); \
    test_##name();             \
    printf("OK\n");            \
    g_tests_passed++;          \
  }                            \
  static void test_##name()

/* Allocate output buffers large enough for any test. */
struct DispatchResult {
  std::vector<uint32_t> dt;  /* definite_true pairs */
  std::vector<uint32_t> df;  /* definite_false pairs */
  std::vector<uint32_t> unc; /* uncertain pairs */
  size_t dt_count = 0;
  size_t df_count = 0;
  size_t unc_count = 0;

  explicit DispatchResult(size_t max_pairs)
      : dt(max_pairs * 2), df(max_pairs * 2), unc(max_pairs * 2) {}
};

/* Check that a pair (i,j) appears in the given pair array. */
static bool has_pair(const uint32_t* pairs, size_t count, uint32_t i, uint32_t j) {
  for (size_t k = 0; k < count; ++k) {
    if (pairs[k * 2] == i && pairs[k * 2 + 1] == j) {
      return true;
    }
  }
  return false;
}

/* Test-only adapter: expand tiny fixture cross-products into the new linear
 * pairwise ABI, then retain the old pair-bucket assertions below. Production
 * callers never allocate this shape. */
static pgaccel_status
test_spatial_cross_product(const pgaccel_geometry* geoms_a, size_t count_a,
                           const pgaccel_geometry* geoms_b, size_t count_b,
                           uint32_t* definite_true_pairs, size_t* definite_true_count,
                           uint32_t* definite_false_pairs, size_t* definite_false_count,
                           uint32_t* uncertain_pairs, size_t* uncertain_count) {
  *definite_true_count = 0;
  *definite_false_count = 0;
  *uncertain_count = 0;
  if (count_a == 0 || count_b == 0)
    return PGACCEL_OK;
  if (count_a > std::numeric_limits<size_t>::max() / count_b)
    return PGACCEL_ERROR;

  const size_t count = count_a * count_b;
  std::vector<pgaccel_geometry> pair_a;
  std::vector<pgaccel_geometry> pair_b;
  pair_a.reserve(count);
  pair_b.reserve(count);
  for (size_t i = 0; i < count_a; ++i) {
    for (size_t j = 0; j < count_b; ++j) {
      pair_a.push_back(geoms_a[i]);
      pair_b.push_back(geoms_b[j]);
    }
  }

  std::vector<int8_t> results(count, 99);
  const pgaccel_status status =
      pgaccel_spatial_intersects_pairwise(pair_a.data(), pair_b.data(), count, results.data());
  if (status != PGACCEL_OK)
    return status;

  for (size_t k = 0; k < count; ++k) {
    const uint32_t i = static_cast<uint32_t>(k / count_b);
    const uint32_t j = static_cast<uint32_t>(k % count_b);
    uint32_t* pairs = nullptr;
    size_t* pair_count = nullptr;
    if (results[k] == 1) {
      pairs = definite_true_pairs;
      pair_count = definite_true_count;
    } else if (results[k] == -1) {
      pairs = definite_false_pairs;
      pair_count = definite_false_count;
    } else {
      pairs = uncertain_pairs;
      pair_count = uncertain_count;
    }
    pairs[*pair_count * 2] = i;
    pairs[*pair_count * 2 + 1] = j;
    ++*pair_count;
  }
  return PGACCEL_OK;
}

struct PipCounts {
  size_t inside = 0;
  size_t outside = 0;
  size_t uncertain = 0;
  size_t untouched = 0;
  size_t other = 0;
};

static PipCounts count_pip_results(const std::vector<int8_t>& results) {
  PipCounts counts;
  for (int8_t result : results) {
    if (result == 1) {
      counts.inside++;
    } else if (result == -1) {
      counts.outside++;
    } else if (result == 0) {
      counts.uncertain++;
    } else if (result == 99) {
      counts.untouched++;
    } else {
      counts.other++;
    }
  }
  return counts;
}

static void fill_selectivity_points(std::vector<float>& points, size_t inside_count) {
  const size_t point_count = points.size() / 2;
  const size_t outside_count = point_count - inside_count;
  const size_t in_bbox_outside_count = outside_count / 2;

  for (size_t i = 0; i < point_count; ++i) {
    if (i < inside_count) {
      points[i * 2] = (i & 1) ? 0.25f : -0.20f;
      points[i * 2 + 1] = (i & 1) ? 0.10f : -0.15f;
    } else if (i < inside_count + in_bbox_outside_count) {
      points[i * 2] = 0.95f;
      points[i * 2 + 1] = 0.95f;
    } else {
      points[i * 2] = 1.50f;
      points[i * 2 + 1] = 1.50f;
    }
  }
}

static std::vector<float> make_regular_ring(size_t unique_vertices, float radius) {
  constexpr double kPi = 3.14159265358979323846264338327950288;
  std::vector<float> ring((unique_vertices + 1) * 2);
  for (size_t i = 0; i < unique_vertices; ++i) {
    const double angle = 2.0 * kPi * static_cast<double>(i) / static_cast<double>(unique_vertices);
    ring[i * 2] = static_cast<float>(radius * std::cos(angle));
    ring[i * 2 + 1] = static_cast<float>(radius * std::sin(angle));
  }
  ring[unique_vertices * 2] = ring[0];
  ring[unique_vertices * 2 + 1] = ring[1];
  return ring;
}

/* ----------------------------------------------------------------
 * Test: empty inputs
 * ---------------------------------------------------------------- */
TEST(empty_inputs) {
  DispatchResult r(1);
  pgaccel_status s;

  /* Both empty */
  s = test_spatial_cross_product(nullptr, 0, nullptr, 0, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_EQ(s, PGACCEL_OK);
  ASSERT_EQ(r.dt_count, 0u);
  ASSERT_EQ(r.df_count, 0u);
  ASSERT_EQ(r.unc_count, 0u);

  /* One side empty */
  float bbox[4] = {0, 0, 1, 1};
  float coords[2] = {0.5f, 0.5f};
  pgaccel_geometry g = {};
  g.type = PGACCEL_GEOM_POINT;
  g.bbox = bbox;
  g.coords = coords;
  g.coord_count = 1;

  s = test_spatial_cross_product(&g, 1, nullptr, 0, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_EQ(s, PGACCEL_OK);
  ASSERT_EQ(r.dt_count, 0u);
}

TEST(legacy_cross_product_declines) {
  float bbox[] = {0, 0, 0, 0};
  float coords[] = {0, 0};
  pgaccel_geometry point = {PGACCEL_GEOM_POINT, bbox, coords, 1, nullptr, 0};
  DispatchResult r(1);

  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status =
      pgaccel_spatial_intersects(&point, 1, &point, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_EQ(status, PGACCEL_UNSUPPORTED);
  ASSERT_EQ(r.dt_count, 0u);
  ASSERT_EQ(r.df_count, 0u);
  ASSERT_EQ(r.unc_count, 0u);
  ASSERT_EQ(pgaccel_gpu_exec_count(), 0u);
}

TEST(pairwise_recheck_boundaries) {
  float equal_bbox[] = {1.0f, 1.0f, 1.0f, 1.0f};
  float equal_coords[] = {1.0f, 1.0f};

  float near_a_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};
  float near_a_coords[] = {0.0f, 0.0f};
  float near_b_bbox[] = {5.0e-8f, 0.0f, 5.0e-8f, 0.0f};
  float near_b_coords[] = {5.0e-8f, 0.0f};

  float outside_bbox[] = {-5.0e-8f, 0.5f, -5.0e-8f, 0.5f};
  float outside_coords[] = {-5.0e-8f, 0.5f};
  float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float poly_coords[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  uint32_t rings[] = {0};

  pgaccel_geometry a[] = {
      {PGACCEL_GEOM_POINT, equal_bbox, equal_coords, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, near_a_bbox, near_a_coords, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, outside_bbox, outside_coords, 1, nullptr, 0},
  };
  pgaccel_geometry b[] = {
      {PGACCEL_GEOM_POINT, equal_bbox, equal_coords, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, near_b_bbox, near_b_coords, 1, nullptr, 0},
      {PGACCEL_GEOM_POLYGON, poly_bbox, poly_coords, 5, rings, 1},
  };
  int8_t results[] = {99, 99, 99};

  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_spatial_intersects_pairwise(a, b, 3, results);
  ASSERT_EQ(status, PGACCEL_OK);
  ASSERT_EQ(results[0], 0);
  ASSERT_EQ(results[1], 0);
  ASSERT_EQ(results[2], 0);
  ASSERT_EQ(pgaccel_gpu_exec_count(), 1u);
}

TEST(malformed_pairwise_rings_decline_before_dispatch) {
  float point_bbox[] = {0.5f, 0.5f, 0.5f, 0.5f};
  float point_coords[] = {0.5f, 0.5f};
  float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float poly_coords[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  uint32_t one_ring_offset[] = {0};

  pgaccel_geometry point = {PGACCEL_GEOM_POINT, point_bbox, point_coords, 1, nullptr, 0};
  pgaccel_geometry malformed = {PGACCEL_GEOM_POLYGON, poly_bbox,
                                poly_coords,          5,
                                one_ring_offset,      std::numeric_limits<size_t>::max()};
  int8_t result = 99;

  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_spatial_intersects_pairwise(&point, &malformed, 1, &result);
  ASSERT_EQ(status, PGACCEL_ERROR);
  ASSERT_EQ(result, 99);
  ASSERT_EQ(pgaccel_gpu_exec_count(), 0u);
}

TEST(pairwise_contract_and_geometry_boundaries) {
  float bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float point_coords[] = {0.5f, 0.5f};
  float line_coords[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float poly_coords[] = {0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 0.2f, 0.2f, 0.0f, 0.0f};
  uint32_t offset_zero[] = {0};
  uint32_t offset_one[] = {1};
  uint32_t duplicate_offsets[] = {0, 0};
  int8_t result = 99;

  pgaccel_geometry valid_point = {PGACCEL_GEOM_POINT, bbox, point_coords, 1, nullptr, 0};
  std::vector<pgaccel_geometry> invalid = {
      {PGACCEL_GEOM_POINT, bbox, nullptr, 1, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, bbox, line_coords, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, bbox, point_coords, 1, offset_zero, 1},
      {PGACCEL_GEOM_POLYGON, bbox, poly_coords, 3, duplicate_offsets, 2},
      {PGACCEL_GEOM_POLYGON, bbox, poly_coords, 4, offset_one, 1},
      {PGACCEL_GEOM_POLYGON, bbox, poly_coords, 6, duplicate_offsets, 2},
  };

  pgaccel_reset_gpu_exec_count();
  for (const pgaccel_geometry& malformed : invalid) {
    result = 99;
    ASSERT_EQ(pgaccel_spatial_intersects_pairwise(&valid_point, &malformed, 1, &result),
              PGACCEL_ERROR);
    ASSERT_EQ(result, 99);
  }
  ASSERT_EQ(pgaccel_gpu_exec_count(), 0u);

  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(nullptr, nullptr, 0, nullptr), PGACCEL_OK);
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(nullptr, &valid_point, 1, &result), PGACCEL_ERROR);
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(&valid_point, nullptr, 1, &result), PGACCEL_ERROR);
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(&valid_point, &valid_point, 1, nullptr),
            PGACCEL_ERROR);

  pgaccel_geometry no_bbox_a = {PGACCEL_GEOM_POINT, nullptr, point_coords, 1, nullptr, 0};
  float other_coords[] = {0.75f, 0.75f};
  pgaccel_geometry no_bbox_b = {PGACCEL_GEOM_POINT, nullptr, other_coords, 1, nullptr, 0};
  result = 99;
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(&no_bbox_a, &no_bbox_b, 1, &result), PGACCEL_OK);
  ASSERT_EQ(result, -1);
}

/* ----------------------------------------------------------------
 * Test: 4 points vs 1 polygon
 *
 * Polygon: unit square (0,0)-(1,0)-(1,1)-(0,1)-(0,0)
 * Point 0: (0.5, 0.5) — inside → DEFINITE_TRUE
 * Point 1: (0.2, 0.8) — inside → DEFINITE_TRUE
 * Point 2: (2.0, 2.0) — outside bbox → DEFINITE_FALSE
 * Point 3: (0.0, 0.5) — on edge → UNCERTAIN for exact PG recheck
 * ---------------------------------------------------------------- */
TEST(points_vs_polygon) {
  /* Polygon: unit square */
  float poly_coords[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};
  uint32_t ring_offsets[] = {0};

  pgaccel_geometry polygon = {};
  polygon.type = PGACCEL_GEOM_POLYGON;
  polygon.bbox = poly_bbox;
  polygon.coords = poly_coords;
  polygon.coord_count = 5;
  polygon.ring_offsets = ring_offsets;
  polygon.ring_count = 1;

  /* 4 points */
  float pt0[] = {0.5f, 0.5f};
  float pt1[] = {0.2f, 0.8f};
  float pt2[] = {2.0f, 2.0f};
  float pt3[] = {0.0f, 0.5f};

  float bb0[] = {0.5f, 0.5f, 0.5f, 0.5f};
  float bb1[] = {0.2f, 0.8f, 0.2f, 0.8f};
  float bb2[] = {2.0f, 2.0f, 2.0f, 2.0f};
  float bb3[] = {0.0f, 0.5f, 0.0f, 0.5f};

  pgaccel_geometry points[4] = {};
  points[0] = {PGACCEL_GEOM_POINT, bb0, pt0, 1, nullptr, 0};
  points[1] = {PGACCEL_GEOM_POINT, bb1, pt1, 1, nullptr, 0};
  points[2] = {PGACCEL_GEOM_POINT, bb2, pt2, 1, nullptr, 0};
  points[3] = {PGACCEL_GEOM_POINT, bb3, pt3, 1, nullptr, 0};

  DispatchResult r(4);

  pgaccel_status s =
      test_spatial_cross_product(points, 4, &polygon, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);

  /* All 4 pairs accounted for. */
  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 4u);

  /* Point 0 (0.5,0.5) inside → definite true */
  assert(has_pair(r.dt.data(), r.dt_count, 0, 0));

  /* Point 1 (0.2,0.8) inside → definite true */
  assert(has_pair(r.dt.data(), r.dt_count, 1, 0));

  /* Point 2 (2.0,2.0) outside bbox → definite false */
  assert(has_pair(r.df.data(), r.df_count, 2, 0));

  /* Point 3 (0.0,0.5) on edge → uncertain. */
  assert(has_pair(r.unc.data(), r.unc_count, 3, 0));
}

/* ----------------------------------------------------------------
 * Test: all bbox misses → everything definite_false
 * ---------------------------------------------------------------- */
TEST(all_bbox_miss) {
  float bb_a[] = {0, 0, 1, 1};
  float bb_b[] = {10, 10, 11, 11};
  float coords_a[] = {0.5f, 0.5f};
  float coords_b[] = {10.5f, 10.5f};

  pgaccel_geometry a = {PGACCEL_GEOM_POINT, bb_a, coords_a, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb_b, coords_b, 1, nullptr, 0};

  DispatchResult r(1);

  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);
  ASSERT_EQ(r.dt_count, 0u);
  ASSERT_EQ(r.df_count, 1u);
  ASSERT_EQ(r.unc_count, 0u);
}

/* ----------------------------------------------------------------
 * Test: point vs point (equal and not-equal)
 * ---------------------------------------------------------------- */
TEST(point_vs_point) {
  float bb1[] = {1, 1, 1, 1};
  float bb2[] = {1, 1, 1, 1};
  float bb3[] = {2, 2, 2, 2};
  float c1[] = {1.0f, 1.0f};
  float c2[] = {1.0f, 1.0f};
  float c3[] = {2.0f, 2.0f};

  pgaccel_geometry pts[2] = {
      {PGACCEL_GEOM_POINT, bb1, c1, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, bb3, c3, 1, nullptr, 0},
  };

  pgaccel_geometry target = {PGACCEL_GEOM_POINT, bb2, c2, 1, nullptr, 0};

  DispatchResult r(2);

  pgaccel_status s =
      test_spatial_cross_product(pts, 2, &target, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);

  /* f32-equal points may differ before extraction, so PG must recheck. */
  assert(has_pair(r.unc.data(), r.unc_count, 0, 0));

  /* pt[1] bbox misses target → definite false */
  assert(has_pair(r.df.data(), r.df_count, 1, 0));

  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 2u);
}

/* ----------------------------------------------------------------
 * Test: line vs line (crossing and parallel)
 * ---------------------------------------------------------------- */
TEST(line_vs_line) {
  /* Line A: (0,0)-(2,2) — diagonal */
  float ca[] = {0, 0, 2, 2};
  float bba[] = {0, 0, 2, 2};

  /* Line B: (0,2)-(2,0) — crosses A */
  float cb[] = {0, 2, 2, 0};
  float bbb[] = {0, 0, 2, 2};

  /* Line C: (3,3)-(4,4) — parallel, no overlap bbox */
  float cc[] = {3, 3, 4, 4};
  float bbc[] = {3, 3, 4, 4};

  pgaccel_geometry lines_a[1] = {
      {PGACCEL_GEOM_LINESTRING, bba, ca, 2, nullptr, 0},
  };
  pgaccel_geometry lines_b[2] = {
      {PGACCEL_GEOM_LINESTRING, bbb, cb, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, bbc, cc, 2, nullptr, 0},
  };

  DispatchResult r(2);

  pgaccel_status s =
      test_spatial_cross_product(lines_a, 1, lines_b, 2, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);

  /* A vs B: crossing → definite true */
  assert(has_pair(r.dt.data(), r.dt_count, 0, 0));

  /* A vs C: bbox miss → definite false */
  assert(has_pair(r.df.data(), r.df_count, 0, 1));

  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 2u);
}

TEST(pairwise_reverse_polygon_and_line_boundaries) {
  float common_bbox[] = {0.0f, 0.0f, 2.0f, 2.0f};
  float square[] = {0.0f, 0.0f, 2.0f, 0.0f, 2.0f, 2.0f, 0.0f, 2.0f, 0.0f, 0.0f};
  uint32_t rings[] = {0};
  float inside[] = {1.0f, 1.0f};
  float edge[] = {0.0f, 1.0f};
  float outside[] = {3.0f, 1.0f};
  float inside_bbox[] = {1.0f, 1.0f, 1.0f, 1.0f};
  float edge_bbox[] = {0.0f, 1.0f, 0.0f, 1.0f};
  float outside_bbox[] = {3.0f, 1.0f, 3.0f, 1.0f};

  pgaccel_geometry polygon = {PGACCEL_GEOM_POLYGON, common_bbox, square, 5, rings, 1};
  pgaccel_geometry points[] = {
      {PGACCEL_GEOM_POINT, inside_bbox, inside, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, edge_bbox, edge, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, outside_bbox, outside, 1, nullptr, 0},
  };
  pgaccel_geometry polygons[] = {polygon, polygon, polygon};
  int8_t reverse_results[] = {99, 99, 99};
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(polygons, points, 3, reverse_results), PGACCEL_OK);
  ASSERT_EQ(reverse_results[0], 1);
  ASSERT_EQ(reverse_results[1], 0);
  ASSERT_EQ(reverse_results[2], -1);

  float base_line[] = {0.0f, 0.0f, 2.0f, 0.0f};
  float parallel_line[] = {0.0f, 1.0f, 2.0f, 1.0f};
  float endpoint_line[] = {2.0f, 0.0f, 2.0f, 2.0f};
  float collinear_line[] = {1.0f, 0.0f, 3.0f, 0.0f};
  float zero_line[] = {1.0f, 1.0f, 1.0f, 1.0f};
  pgaccel_geometry line_a[] = {
      {PGACCEL_GEOM_LINESTRING, common_bbox, base_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, base_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, base_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, zero_line, 2, nullptr, 0},
  };
  pgaccel_geometry line_b[] = {
      {PGACCEL_GEOM_LINESTRING, common_bbox, parallel_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, endpoint_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, collinear_line, 2, nullptr, 0},
      {PGACCEL_GEOM_LINESTRING, common_bbox, base_line, 2, nullptr, 0},
  };
  int8_t line_results[] = {99, 99, 99, 99};
  ASSERT_EQ(pgaccel_spatial_intersects_pairwise(line_a, line_b, 4, line_results), PGACCEL_OK);
  ASSERT_EQ(line_results[0], -1);
  ASSERT_EQ(line_results[1], 0);
  ASSERT_EQ(line_results[2], 0);
  ASSERT_EQ(line_results[3], 0);
}

/* ----------------------------------------------------------------
 * Test: unknown geometry type → uncertain
 * ---------------------------------------------------------------- */
TEST(unknown_geom_type) {
  float unknown_bb[] = {100, 100, 101, 101};
  float point_bb[] = {0, 0, 1, 1};
  float c[] = {0.5f, 0.5f};

  pgaccel_geometry a = {PGACCEL_GEOM_UNKNOWN, unknown_bb, c, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, point_bb, c, 1, nullptr, 0};

  DispatchResult r(1);

  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);
  /* Unknown types stay uncertain even when their synthetic bbox is disjoint. */
  ASSERT_EQ(r.unc_count, 1u);
  ASSERT_EQ(r.dt_count, 0u);
}

/* ----------------------------------------------------------------
 * Test: total partitioning — all bbox survivors accounted for
 * ---------------------------------------------------------------- */
TEST(total_partitioning) {
  /* 3 points vs 2 points, all with same bbox → 6 pairs survive bbox.
   * Each gets dispatched to point_vs_point. */
  float bb[] = {0, 0, 0, 0};
  float c0[] = {0, 0};
  float c1[] = {0, 0};
  float c2[] = {0, 0};
  float c3[] = {0, 0};
  float c4[] = {1, 1};

  pgaccel_geometry ga[3] = {
      {PGACCEL_GEOM_POINT, bb, c0, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, bb, c1, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, bb, c2, 1, nullptr, 0},
  };
  pgaccel_geometry gb[2] = {
      {PGACCEL_GEOM_POINT, bb, c3, 1, nullptr, 0},
      {PGACCEL_GEOM_POINT, bb, c4, 1, nullptr, 0},
  };

  DispatchResult r(6);

  pgaccel_status s = test_spatial_cross_product(ga, 3, gb, 2, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);
  /* All 6 pairs must be accounted for. */
  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 6u);

  /* Rounded-equal coordinates require exact PG recheck. */
  ASSERT_EQ(r.unc_count, 3u);
  /* Each of the 3 a-points does NOT match gb[1] (different coords). */
  ASSERT_EQ(r.df_count, 3u);
}

/* ----------------------------------------------------------------
 * Regression: 100K point-in-polygon selectivity sweep, simple kernel.
 *
 * The 2026-05 Phase-1 release gate exposed Metal/AdaptiveCpp crashes
 * around 100K spatial selectivity fixtures. This keeps a deterministic
 * standalone repro in the dispatch test and verifies that each point
 * forms exactly one accounted point/polygon pair.
 * ---------------------------------------------------------------- */
TEST(point_in_polygon_bulk_simple_100k_selectivity_sweep) {
  constexpr size_t point_count = 100000;

  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  float diamond[] = {
      0.0f, 1.0f, 1.0f, 0.0f, 0.0f, -1.0f, -1.0f, 0.0f, 0.0f, 1.0f,
  };
  const size_t selectivities[] = {1, 10, 50, 90};

  ASSERT_EQ(pgaccel_init(), PGACCEL_OK);

  for (size_t pct : selectivities) {
    const size_t expected_inside = point_count * pct / 100;
    std::vector<float> points(point_count * 2);
    fill_selectivity_points(points, expected_inside);

    std::vector<int8_t> results(point_count, 99);
    pgaccel_reset_gpu_exec_count();
    const uint64_t before = pgaccel_gpu_exec_count();

    pgaccel_status s = pgaccel_point_in_polygon_bulk(points.data(), point_count, bbox, diamond, 5,
                                                     nullptr, 0, results.data());

    ASSERT_EQ(s, PGACCEL_OK);
    const uint64_t after = pgaccel_gpu_exec_count();
    ASSERT_EQ(after, before + 1);

    const PipCounts counts = count_pip_results(results);
    ASSERT_EQ(counts.inside + counts.outside + counts.uncertain, point_count);
    ASSERT_EQ(counts.inside, expected_inside);
    ASSERT_EQ(counts.outside, point_count - expected_inside);
    ASSERT_EQ(counts.uncertain, 0u);
    ASSERT_EQ(counts.untouched, 0u);
    ASSERT_EQ(counts.other, 0u);
  }
}

/* ----------------------------------------------------------------
 * Regression: 100K point-in-polygon, 1024+ vertex cooperative kernel.
 *
 * A 1025-coordinate closed ring crosses the cooperative dispatch
 * threshold. The high-selectivity point set keeps most points in the
 * polygon bbox so the cooperative kernel is exercised at the crash scale.
 * ---------------------------------------------------------------- */
TEST(point_in_polygon_bulk_coop_1024v_100k) {
  constexpr size_t point_count = 100000;
  constexpr size_t unique_vertices = 1024;
  constexpr size_t expected_inside = point_count * 90 / 100;

  std::vector<float> ring = make_regular_ring(unique_vertices, 1.0f);
  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  uint32_t rings[] = {0};

  std::vector<float> points(point_count * 2);
  fill_selectivity_points(points, expected_inside);

  std::vector<int8_t> results(point_count, 99);
  ASSERT_EQ(pgaccel_init(), PGACCEL_OK);
  pgaccel_reset_gpu_exec_count();
  const uint64_t before = pgaccel_gpu_exec_count();

  pgaccel_status s = pgaccel_point_in_polygon_bulk(points.data(), point_count, bbox, ring.data(),
                                                   unique_vertices + 1, rings, 1, results.data());

  ASSERT_EQ(s, PGACCEL_OK);
  const uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_EQ(after, before + 1);

  const PipCounts counts = count_pip_results(results);
  ASSERT_EQ(counts.inside + counts.outside + counts.uncertain, point_count);
  ASSERT_EQ(counts.inside, expected_inside);
  ASSERT_EQ(counts.outside, point_count - expected_inside);
  ASSERT_EQ(counts.uncertain, 0u);
  ASSERT_EQ(counts.untouched, 0u);
  ASSERT_EQ(counts.other, 0u);
}

TEST(point_in_polygon_ring_and_hole_boundaries) {
  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  float small_polygon[] = {
      -1.0f,  -1.0f,  1.0f,  -1.0f,  1.0f,  1.0f,  -1.0f,  1.0f,  -1.0f,  -1.0f,
      -0.25f, -0.25f, 0.25f, -0.25f, 0.25f, 0.25f, -0.25f, 0.25f, -0.25f, -0.25f,
  };
  uint32_t small_rings[] = {0, 5};
  float points[] = {0.0f, 0.0f, 0.75f, 0.0f, 2.0f, 0.0f, 0.25f, 0.0f, 1.0f, 0.0f};
  int8_t simple_results[] = {99, 99, 99, 99, 99};
  ASSERT_EQ(pgaccel_point_in_polygon_bulk(points, 5, bbox, small_polygon, 10, small_rings, 2,
                                          simple_results),
            PGACCEL_OK);
  ASSERT_EQ(simple_results[0], -1);
  ASSERT_EQ(simple_results[1], 1);
  ASSERT_EQ(simple_results[2], -1);
  ASSERT_EQ(simple_results[3], 0);
  ASSERT_EQ(simple_results[4], 0);

  constexpr size_t outer_vertices = 1024;
  std::vector<float> cooperative = make_regular_ring(outer_vertices, 1.0f);
  cooperative.insert(cooperative.end(), std::begin(small_polygon) + 10, std::end(small_polygon));
  uint32_t cooperative_rings[] = {0, outer_vertices + 1};
  int8_t cooperative_results[] = {99, 99, 99, 99, 99};
  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ(pgaccel_point_in_polygon_bulk(points, 5, bbox, cooperative.data(),
                                          cooperative.size() / 2, cooperative_rings, 2,
                                          cooperative_results),
            PGACCEL_OK);
  ASSERT_EQ(cooperative_results[0], -1);
  ASSERT_EQ(cooperative_results[1], 1);
  ASSERT_EQ(cooperative_results[2], -1);
  ASSERT_EQ(cooperative_results[3], 0);
  ASSERT_EQ(cooperative_results[4], 0);
  ASSERT_EQ(pgaccel_gpu_exec_count(), 1u);
}

/* ----------------------------------------------------------------
 * Test: unsupported geometry-type pairs must be UNCERTAIN, never
 * silently DEFINITE_TRUE or DEFINITE_FALSE from the predicate layer.
 *
 * These pairs are UNSUPPORTED in device_pairwise_intersects and correspond
 * to missing kernels:
 *   - Point × LineString            — no pgaccel_point_on_linestring_bulk
 *   - LineString × Polygon          — no pgaccel_linestring_polygon_intersects_bulk
 *   - Polygon × Polygon             — no pgaccel_polygon_polygon_intersects_bulk
 *
 * All bboxes overlap so the Layer-1 bbox filter cannot prune them;
 * every pair must therefore reach Layer-2 and land in UNCERTAIN.
 * Future kernels that close one of these gaps must add an explicit device
 * branch; never remove the UNCERTAIN result silently.
 * ---------------------------------------------------------------- */
TEST(unsupported_pairs_are_uncertain) {
  /* Shared bbox that every geometry below overlaps. */
  float bb[] = {0.0f, 0.0f, 2.0f, 2.0f};

  float pt_coords[] = {1.0f, 1.0f};
  float line_coords[] = {0.0f, 0.0f, 2.0f, 2.0f};
  float poly_coords[] = {0.0f, 0.0f, 2.0f, 0.0f, 2.0f, 2.0f, 0.0f, 2.0f, 0.0f, 0.0f};
  uint32_t poly_rings[] = {0};

  pgaccel_geometry pt = {PGACCEL_GEOM_POINT, bb, pt_coords, 1, nullptr, 0};
  pgaccel_geometry line = {PGACCEL_GEOM_LINESTRING, bb, line_coords, 2, nullptr, 0};
  pgaccel_geometry poly = {PGACCEL_GEOM_POLYGON, bb, poly_coords, 5, poly_rings, 1};

  /* Point × LineString (both orders). */
  {
    DispatchResult r(1);
    pgaccel_status s =
        test_spatial_cross_product(&pt, 1, &line, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }
  {
    DispatchResult r(1);
    pgaccel_status s =
        test_spatial_cross_product(&line, 1, &pt, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }

  /* LineString × Polygon (both orders). */
  {
    DispatchResult r(1);
    pgaccel_status s =
        test_spatial_cross_product(&line, 1, &poly, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }
  {
    DispatchResult r(1);
    pgaccel_status s =
        test_spatial_cross_product(&poly, 1, &line, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }

  /* Polygon × Polygon. */
  {
    DispatchResult r(1);
    pgaccel_status s =
        test_spatial_cross_product(&poly, 1, &poly, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }
}

/* ----------------------------------------------------------------
 * main
 * ---------------------------------------------------------------- */
int main() {
  printf("spatial_dispatch tests\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed\n");
    return 1;
  }

  run_empty_inputs();
  run_legacy_cross_product_declines();
  run_pairwise_recheck_boundaries();
  run_malformed_pairwise_rings_decline_before_dispatch();
  run_pairwise_contract_and_geometry_boundaries();
  run_points_vs_polygon();
  run_all_bbox_miss();
  run_point_vs_point();
  run_line_vs_line();
  run_pairwise_reverse_polygon_and_line_boundaries();
  run_unknown_geom_type();
  run_total_partitioning();
  run_point_in_polygon_bulk_simple_100k_selectivity_sweep();
  run_point_in_polygon_bulk_coop_1024v_100k();
  run_point_in_polygon_ring_and_hole_boundaries();
  run_unsupported_pairs_are_uncertain();

  if (pgaccel_shutdown() != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_shutdown failed\n");
    g_tests_failed++;
  }
  printf("\n%d passed, %d failed\n", g_tests_passed, g_tests_failed);
  return g_tests_failed > 0 ? 1 : 0;
}

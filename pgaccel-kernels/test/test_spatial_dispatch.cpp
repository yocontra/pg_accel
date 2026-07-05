#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstring>
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
  s = pgaccel_spatial_intersects(nullptr, 0, nullptr, 0, r.dt.data(), &r.dt_count, r.df.data(),
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

  s = pgaccel_spatial_intersects(&g, 1, nullptr, 0, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_EQ(s, PGACCEL_OK);
  ASSERT_EQ(r.dt_count, 0u);
}

/* ----------------------------------------------------------------
 * Test: 4 points vs 1 polygon
 *
 * Polygon: unit square (0,0)-(1,0)-(1,1)-(0,1)-(0,0)
 * Point 0: (0.5, 0.5) — inside → DEFINITE_TRUE
 * Point 1: (0.2, 0.8) — inside → DEFINITE_TRUE
 * Point 2: (2.0, 2.0) — outside bbox → DEFINITE_FALSE
 * Point 3: (0.0, 0.5) — on edge → either TRUE or UNCERTAIN is acceptable
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
      pgaccel_spatial_intersects(points, 4, &polygon, 1, r.dt.data(), &r.dt_count, r.df.data(),
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

  /* Point 3 (0.0,0.5) on edge → TRUE or UNCERTAIN (not false) */
  assert(has_pair(r.dt.data(), r.dt_count, 3, 0) || has_pair(r.unc.data(), r.unc_count, 3, 0));
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

  pgaccel_status s = pgaccel_spatial_intersects(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
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
      pgaccel_spatial_intersects(pts, 2, &target, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);

  /* pt[0] == target → definite true */
  assert(has_pair(r.dt.data(), r.dt_count, 0, 0));

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
      pgaccel_spatial_intersects(lines_a, 1, lines_b, 2, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);

  /* A vs B: crossing → definite true */
  assert(has_pair(r.dt.data(), r.dt_count, 0, 0));

  /* A vs C: bbox miss → definite false */
  assert(has_pair(r.df.data(), r.df_count, 0, 1));

  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 2u);
}

/* ----------------------------------------------------------------
 * Test: unknown geometry type → uncertain
 * ---------------------------------------------------------------- */
TEST(unknown_geom_type) {
  float bb[] = {0, 0, 1, 1};
  float c[] = {0.5f, 0.5f};

  pgaccel_geometry a = {PGACCEL_GEOM_UNKNOWN, bb, c, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};

  DispatchResult r(1);

  pgaccel_status s = pgaccel_spatial_intersects(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);
  /* Bbox overlaps, but unknown type → uncertain */
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

  pgaccel_status s = pgaccel_spatial_intersects(ga, 3, gb, 2, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);

  ASSERT_EQ(s, PGACCEL_OK);
  /* All 6 pairs must be accounted for. */
  ASSERT_EQ(r.dt_count + r.df_count + r.unc_count, 6u);

  /* Each of the 3 a-points matches gb[0] (same coords). */
  ASSERT_EQ(r.dt_count, 3u);
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

/* ----------------------------------------------------------------
 * Test: unsupported geometry-type pairs must be UNCERTAIN, never
 * silently DEFINITE_TRUE or DEFINITE_FALSE from the predicate layer.
 *
 * These pairs are listed as UNSUPPORTED in the evaluate_predicate
 * comment in spatial_dispatch.cpp and correspond to missing kernels:
 *   - Point × LineString            — no pgaccel_point_on_linestring_bulk
 *   - LineString × Polygon          — no pgaccel_linestring_polygon_intersects_bulk
 *   - Polygon × Polygon             — no pgaccel_polygon_polygon_intersects_bulk
 *
 * All bboxes overlap so the Layer-1 bbox filter cannot prune them;
 * every pair must therefore reach Layer-2 and land in UNCERTAIN.
 * Future kernels that close one of these gaps must add a scalar check
 * helper and an explicit branch in evaluate_predicate — never remove
 * the UNCERTAIN fallback silently.
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
        pgaccel_spatial_intersects(&pt, 1, &line, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }
  {
    DispatchResult r(1);
    pgaccel_status s =
        pgaccel_spatial_intersects(&line, 1, &pt, 1, r.dt.data(), &r.dt_count, r.df.data(),
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
        pgaccel_spatial_intersects(&line, 1, &poly, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                   &r.df_count, r.unc.data(), &r.unc_count);
    ASSERT_EQ(s, PGACCEL_OK);
    ASSERT_EQ(r.unc_count, 1u);
    ASSERT_EQ(r.dt_count, 0u);
    ASSERT_EQ(r.df_count, 0u);
  }
  {
    DispatchResult r(1);
    pgaccel_status s =
        pgaccel_spatial_intersects(&poly, 1, &line, 1, r.dt.data(), &r.dt_count, r.df.data(),
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
        pgaccel_spatial_intersects(&poly, 1, &poly, 1, r.dt.data(), &r.dt_count, r.df.data(),
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

  run_empty_inputs();
  run_points_vs_polygon();
  run_all_bbox_miss();
  run_point_vs_point();
  run_line_vs_line();
  run_unknown_geom_type();
  run_total_partitioning();
  run_point_in_polygon_bulk_simple_100k_selectivity_sweep();
  run_point_in_polygon_bulk_coop_1024v_100k();
  run_unsupported_pairs_are_uncertain();

  printf("\n%d passed, %d failed\n", g_tests_passed, g_tests_failed);
  return g_tests_failed > 0 ? 1 : 0;
}

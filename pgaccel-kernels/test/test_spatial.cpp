#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <random>
#include <vector>

#include "pgaccel_ffi.h"

// ---------------------------------------------------------------------------
// Minimal test harness
// ---------------------------------------------------------------------------

static int g_tests_run = 0;
static int g_tests_passed = 0;
static int g_tests_failed = 0;

#define ASSERT_EQ(desc, got, expected)                                                            \
  do {                                                                                            \
    g_tests_run++;                                                                                \
    if ((got) == (expected)) {                                                                    \
      g_tests_passed++;                                                                           \
    } else {                                                                                      \
      g_tests_failed++;                                                                           \
      fprintf(stderr, "  FAIL: %s — got %d, expected %d\n", (desc), (int)(got), (int)(expected)); \
    }                                                                                             \
  } while (0)

#define ASSERT_NEAR(desc, got, expected, tol)                                          \
  do {                                                                                 \
    g_tests_run++;                                                                     \
    if (std::abs((got) - (expected)) <= (tol)) {                                       \
      g_tests_passed++;                                                                \
    } else {                                                                           \
      g_tests_failed++;                                                                \
      fprintf(stderr, "  FAIL: %s — got %.6f, expected %.6f\n", (desc), (double)(got), \
              (double)(expected));                                                     \
    }                                                                                  \
  } while (0)

// ---------------------------------------------------------------------------
// point_in_ring tests
// ---------------------------------------------------------------------------

// Unit square: (0,0) (1,0) (1,1) (0,1) (0,0)
static const double square_ring[] = {
    0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0,
};
static const size_t square_verts = 5;

static void test_point_in_ring_basic() {
  printf("--- point_in_ring: basic ---\n");

  // Point clearly inside
  {
    double pt[] = {0.5, 0.5};
    int8_t result = 0;
    pgaccel_status s = pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("status OK", s, PGACCEL_OK);
    ASSERT_EQ("(0.5,0.5) inside unit square", result, 1);
  }

  // Point clearly outside
  {
    double pt[] = {5.0, 5.0};
    int8_t result = 0;
    pgaccel_status s = pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("status OK", s, PGACCEL_OK);
    ASSERT_EQ("(5,5) outside unit square", result, -1);
  }

  // Point outside, negative coords
  {
    double pt[] = {-1.0, -1.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("(-1,-1) outside unit square", result, -1);
  }

  // Multiple points in one call
  {
    double pts[] = {0.5, 0.5, 5.0, 5.0, 0.25, 0.75};
    int8_t results[3] = {};
    pgaccel_point_in_ring_bulk(pts, 3, square_ring, square_verts, true, results);
    ASSERT_EQ("batch[0] inside", results[0], 1);
    ASSERT_EQ("batch[1] outside", results[1], -1);
    ASSERT_EQ("batch[2] inside", results[2], 1);
  }
}

static void test_point_in_ring_edge_cases() {
  printf("--- point_in_ring: edge cases ---\n");

  // Point on vertex — should be UNCERTAIN
  {
    double pt[] = {0.0, 0.0};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("point on vertex -> uncertain", result, 0);
  }

  // Point on edge — should be UNCERTAIN
  {
    double pt[] = {0.5, 0.0};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("point on edge -> uncertain", result, 0);
  }

  // Degenerate ring (< 4 vertices)
  {
    double degenerate[] = {0.0, 0.0, 1.0, 1.0, 0.0, 0.0};
    double pt[] = {0.5, 0.5};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, degenerate, 3, true, &result);
    ASSERT_EQ("degenerate ring -> uncertain", result, 0);
  }

  // NaN coordinate
  {
    double nan_val = std::numeric_limits<double>::quiet_NaN();
    double pt[] = {nan_val, 0.5};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("NaN point -> uncertain", result, 0);
  }

  // Inf coordinate
  {
    double inf_val = std::numeric_limits<double>::infinity();
    double pt[] = {inf_val, 0.5};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("Inf point -> uncertain", result, 0);
  }

  // Zero-count call
  {
    pgaccel_status s =
        pgaccel_point_in_ring_bulk(nullptr, 0, square_ring, square_verts, true, nullptr);
    ASSERT_EQ("zero count OK", s, PGACCEL_OK);
  }

  // Null pointers
  {
    int8_t result;
    pgaccel_status s =
        pgaccel_point_in_ring_bulk(nullptr, 1, square_ring, square_verts, true, &result);
    ASSERT_EQ("null points -> error", s, PGACCEL_ERROR_INIT);
  }
}

static void test_point_in_ring_fp32() {
  printf("--- point_in_ring: fp32 ---\n");

  float ring[] = {0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f};

  // Clearly inside
  {
    float pt[] = {0.5f, 0.5f};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 5, false, &result);
    ASSERT_EQ("fp32 (0.5,0.5) inside", result, 1);
  }

  // Clearly outside
  {
    float pt[] = {5.0f, 5.0f};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 5, false, &result);
    ASSERT_EQ("fp32 (5,5) outside", result, -1);
  }

  // Near edge — fp32 epsilon is wider so more things go uncertain
  {
    float pt[] = {0.5f, 1e-6f};
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 5, false, &result);
    ASSERT_EQ("fp32 near edge -> uncertain", result, 0);
  }
}

// ---------------------------------------------------------------------------
// sphere_distance tests
// ---------------------------------------------------------------------------

static void test_sphere_distance_basic() {
  printf("--- sphere_distance: basic ---\n");

  // fp32 + fp64 paths both supported as of Agent 2A's split (Phase A).
  // Templated `sphere_distance_bulk_sycl<T>` was replaced with two
  // non-templated kernels (`_f32` / `_f64`) — see spatial_predicates.cpp
  // for the Metal-SSCP template-instantiation hang background.

  // New York to London (known ~5570 km)
  {
    float a[] = {-74.006f, 40.7128f};
    float b[] = {-0.1278f, 51.5074f};
    float dist = 0;
    uint8_t unc = 1;
    pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("status OK", s, PGACCEL_OK);
    ASSERT_EQ("NYC-London definite", unc, 0);
    ASSERT_NEAR("NYC-London ~5570km", dist / 1000.0f, 5570.0f, 50.0f);
  }

  // Same point — should be uncertain (very close)
  {
    float a[] = {0.0f, 0.0f};
    float b[] = {0.0f, 0.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("same point -> uncertain", unc, 1);
  }

  // Equator distance: 1 degree longitude at equator ~ 111.195 km
  {
    float a[] = {0.0f, 0.0f};
    float b[] = {1.0f, 0.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("equator 1deg definite", unc, 0);
    ASSERT_NEAR("equator 1deg ~111km", dist / 1000.0f, 111.195f, 1.0f);
  }
}

static void test_sphere_distance_edge_cases() {
  printf("--- sphere_distance: edge cases ---\n");

  // Antipodal points (0,0) to (180,0)
  {
    float a[] = {0.0f, 0.0f};
    float b[] = {180.0f, 0.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("antipodal -> uncertain", unc, 1);
  }

  // NaN input
  {
    float nan_val = std::numeric_limits<float>::quiet_NaN();
    float a[] = {nan_val, 0.0f};
    float b[] = {0.0f, 0.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("NaN -> uncertain", unc, 1);
  }

  // Null pointers
  {
    pgaccel_status s = pgaccel_sphere_distance_bulk(nullptr, nullptr, 1, false, nullptr, nullptr);
    ASSERT_EQ("null -> error", s, PGACCEL_ERROR_INIT);
  }

  // Zero count
  {
    pgaccel_status s = pgaccel_sphere_distance_bulk(nullptr, nullptr, 0, false, nullptr, nullptr);
    ASSERT_EQ("zero count OK", s, PGACCEL_OK);
  }

  // fp64 path now lives — split from templated kernel to avoid the
  // Metal SSCP template-instantiation hang (Agent 2A task 1). NYC->London
  // distance ≈ 5570 km via Haversine. Definite (not uncertain) because
  // the points are >> 1 mm apart.
  {
    double a[] = {-74.006, 40.7128};
    double b[] = {-0.1278, 51.5074};
    double dist = 0;
    uint8_t unc = 1;
    pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
    ASSERT_EQ("fp64 status OK", s, PGACCEL_OK);
    ASSERT_EQ("fp64 NYC-London definite", unc, 0);
    ASSERT_NEAR("fp64 NYC-London ~5570km", dist / 1000.0, 5570.0, 50.0);
  }
}

static void test_sphere_distance_fp32() {
  printf("--- sphere_distance: fp32 ---\n");

  // NYC to London in fp32
  {
    float a[] = {-74.006f, 40.7128f};
    float b[] = {-0.1278f, 51.5074f};
    float dist = 0;
    uint8_t unc = 1;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("fp32 NYC-London definite", unc, 0);
    ASSERT_NEAR("fp32 NYC-London ~5570km", dist / 1000.0f, 5570.0f, 50.0f);
  }

  // Points <1m apart — fp32 should be uncertain
  {
    float a[] = {0.0f, 0.0f};
    float b[] = {0.000001f, 0.0f};  // ~0.11 m at equator
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("fp32 very close -> uncertain", unc, 1);
  }
}

// fp64 path tests — exercises the split non-templated `_f64` kernel that
// replaced the templated form (Agent 2A task 1). The fp64 kernel is the
// one that used to return PGACCEL_ERROR_NO_DEVICE; these assertions
// confirm it now returns real distances.
static void test_sphere_distance_fp64() {
  printf("--- sphere_distance: fp64 (post-split) ---\n");

  // Multiple-pair batch: NYC->London, Berlin->Paris, antipodal sentinel.
  {
    double a[] = {
        -74.006, 40.7128,  // NYC
        13.405,  52.52,    // Berlin
        0.0,     0.0,      // antipodal pair
    };
    double b[] = {
        -0.1278, 51.5074,  // London
        2.3522,  48.8566,  // Paris
        180.0,   0.0,      // antipode
    };
    double dist[3] = {-1.0, -1.0, -1.0};
    uint8_t unc[3] = {0, 0, 0};
    pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 3, true, dist, unc);
    ASSERT_EQ("fp64 batch status OK", s, PGACCEL_OK);
    // NYC -> London ≈ 5570 km
    ASSERT_NEAR("fp64 batch[0] NYC-London km", dist[0] / 1000.0, 5570.0, 50.0);
    // Berlin -> Paris ≈ 878 km
    ASSERT_NEAR("fp64 batch[1] Berlin-Paris km", dist[1] / 1000.0, 878.0, 20.0);
    // Antipodal -> uncertain flag set, distance returned as 0.
    ASSERT_EQ("fp64 batch[2] antipodal -> uncertain", unc[2], 1);
  }

  // Very close points → should flag uncertain on fp64 too (1 mm threshold).
  {
    double a[] = {0.0, 0.0};
    double b[] = {1e-10, 0.0};  // sub-mm separation
    double dist = -1.0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
    ASSERT_EQ("fp64 sub-mm -> uncertain", unc, 1);
  }
}

// ---------------------------------------------------------------------------
// segment_intersects tests
// ---------------------------------------------------------------------------

static void test_segment_intersects_basic() {
  printf("--- segment_intersects: basic ---\n");

  // Classic X intersection
  {
    double a[] = {0.0, 0.0, 1.0, 1.0};  // (0,0)-(1,1)
    double b[] = {0.0, 1.0, 1.0, 0.0};  // (0,1)-(1,0)
    int8_t result = 0;
    pgaccel_status s = pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("status OK", s, PGACCEL_OK);
    ASSERT_EQ("X intersection -> 1", result, 1);
  }

  // Parallel segments — no intersection
  {
    double a[] = {0.0, 0.0, 1.0, 0.0};  // horizontal at y=0
    double b[] = {0.0, 1.0, 1.0, 1.0};  // horizontal at y=1
    int8_t result = 0;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("parallel -> -1", result, -1);
  }

  // Non-intersecting (well separated, no collinearity)
  {
    double a[] = {0.0, 0.0, 1.0, 0.5};
    double b[] = {3.0, 0.0, 4.0, 1.0};
    int8_t result = 0;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("disjoint -> -1", result, -1);
  }

  // Multiple segment pairs
  {
    double a[] = {
        0.0, 0.0, 1.0, 1.0,  // crosses
        0.0, 0.0, 1.0, 0.0,  // parallel
    };
    double b[] = {
        0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0,
    };
    int8_t results[2] = {};
    pgaccel_segment_intersects_bulk(a, b, 2, true, results);
    ASSERT_EQ("batch[0] intersects", results[0], 1);
    ASSERT_EQ("batch[1] no intersect", results[1], -1);
  }
}

static void test_segment_intersects_edge_cases() {
  printf("--- segment_intersects: edge cases ---\n");

  // Endpoint touching — should be UNCERTAIN (cross product near zero)
  {
    double a[] = {0.0, 0.0, 1.0, 0.0};
    double b[] = {1.0, 0.0, 2.0, 1.0};  // shares endpoint (1,0)
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("endpoint touch -> uncertain", result, 0);
  }

  // Collinear overlapping — should be UNCERTAIN
  {
    double a[] = {0.0, 0.0, 2.0, 0.0};
    double b[] = {1.0, 0.0, 3.0, 0.0};
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("collinear overlap -> uncertain", result, 0);
  }

  // Zero-length segment
  {
    double a[] = {0.0, 0.0, 0.0, 0.0};
    double b[] = {0.0, 1.0, 1.0, 0.0};
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("zero-length seg -> uncertain", result, 0);
  }

  // NaN
  {
    double nan_val = std::numeric_limits<double>::quiet_NaN();
    double a[] = {nan_val, 0.0, 1.0, 1.0};
    double b[] = {0.0, 1.0, 1.0, 0.0};
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("NaN coord -> uncertain", result, 0);
  }

  // Null pointers
  {
    int8_t result;
    pgaccel_status s = pgaccel_segment_intersects_bulk(nullptr, nullptr, 1, true, &result);
    ASSERT_EQ("null -> error", s, PGACCEL_ERROR_INIT);
  }
}

static void test_segment_intersects_fp32() {
  printf("--- segment_intersects: fp32 ---\n");

  // X intersection
  {
    float a[] = {0.0f, 0.0f, 1.0f, 1.0f};
    float b[] = {0.0f, 1.0f, 1.0f, 0.0f};
    int8_t result = 0;
    pgaccel_segment_intersects_bulk(a, b, 1, false, &result);
    ASSERT_EQ("fp32 X intersect", result, 1);
  }

  // No intersection
  {
    float a[] = {0.0f, 0.0f, 1.0f, 0.0f};
    float b[] = {0.0f, 1.0f, 1.0f, 1.0f};
    int8_t result = 0;
    pgaccel_segment_intersects_bulk(a, b, 1, false, &result);
    ASSERT_EQ("fp32 parallel no intersect", result, -1);
  }
}

// ---------------------------------------------------------------------------
// Triangle test (non-axis-aligned polygon)
// ---------------------------------------------------------------------------

static void test_point_in_ring_triangle() {
  printf("--- point_in_ring: triangle ---\n");

  // Triangle: (0,0) (4,0) (2,3) (0,0)
  double tri[] = {0.0, 0.0, 4.0, 0.0, 2.0, 3.0, 0.0, 0.0};

  // Centroid (2, 1) — inside
  {
    double pt[] = {2.0, 1.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, tri, 4, true, &result);
    ASSERT_EQ("triangle centroid inside", result, 1);
  }

  // Well outside
  {
    double pt[] = {10.0, 10.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, tri, 4, true, &result);
    ASSERT_EQ("triangle outside", result, -1);
  }

  // Below triangle
  {
    double pt[] = {2.0, -1.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, tri, 4, true, &result);
    ASSERT_EQ("triangle below", result, -1);
  }
}

// ---------------------------------------------------------------------------
// fp64 bulk recheck coverage (W5 fp64-unlock plan)
//
// Exercises the ST_Contains/ST_Intersects fp64 recheck path at
// 1k / 64k / 256k / 1M. The test scatters random points in/around a
// unit square ring and asserts: status=OK, every reported 1/-1 is
// correct (the 0=uncertain points are tallied but not verified since
// a recheck would be CPU-side PG work). fp64 tolerance: 0 ULP is
// unreasonable for "PIP across many vertices" — the soft-fp64 u35
// reduction contract applies; this test uses inside/outside regions
// well away from the boundary so even a few-ULP cross-product drift
// can't flip the answer.
// ---------------------------------------------------------------------------
static void test_point_in_ring_fp64_bulk() {
  printf("--- point_in_ring: fp64 bulk (1k/64k/256k/1M) ---\n");

  for (size_t N : {(size_t)1024, (size_t)65536, (size_t)262144, (size_t)1048576}) {
    std::mt19937_64 rng(0xBEEFULL ^ N);
    // Points in [-2, 3] × [-2, 3] — covers inside square [0,1]² and
    // outside region. Uses a deterministic seed.
    std::uniform_real_distribution<double> d(-2.0, 3.0);
    std::vector<double> pts(N * 2);
    for (size_t i = 0; i < N; ++i) {
      pts[2 * i] = d(rng);
      pts[2 * i + 1] = d(rng);
    }
    std::vector<int8_t> results(N, 99);
    pgaccel_status s = pgaccel_point_in_ring_bulk(pts.data(), N, square_ring, square_verts,
                                                  /*use_fp64=*/true, results.data());
    char buf[96];
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu status OK", N);
    ASSERT_EQ(buf, s, PGACCEL_OK);

    // Verify every DEFINITE result is correct. Uncertain (0) is a
    // legitimate soft-boundary result; count but don't check them.
    // Boundary-safe margin: inside  ⇒ all coords in (margin, 1-margin).
    //                       outside ⇒ at least one coord outside
    //                                   [-margin, 1+margin].
    const double margin = 0.01;
    size_t wrong = 0, uncertain = 0, inside_def = 0, outside_def = 0;
    for (size_t i = 0; i < N; ++i) {
      const double x = pts[2 * i];
      const double y = pts[2 * i + 1];
      const bool strict_inside =
          (x > margin) && (x < 1.0 - margin) && (y > margin) && (y < 1.0 - margin);
      const bool strict_outside =
          (x < -margin) || (x > 1.0 + margin) || (y < -margin) || (y > 1.0 + margin);
      if (results[i] == 1) {
        inside_def++;
        if (!strict_inside && !(x >= 0.0 && x <= 1.0 && y >= 0.0 && y <= 1.0)) {
          wrong++;
          if (wrong <= 5)
            fprintf(stderr, "  fp64 bulk: inside-claim but point outside unit square: (%f, %f)\n",
                    x, y);
        }
      } else if (results[i] == -1) {
        outside_def++;
        if (strict_inside) {
          wrong++;
          if (wrong <= 5)
            fprintf(stderr, "  fp64 bulk: outside-claim but point strictly inside: (%f, %f)\n", x,
                    y);
        }
      } else if (results[i] == 0) {
        uncertain++;
      } else {
        wrong++;
      }
    }
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu classifications (in=%zu out=%zu unc=%zu)", N,
             inside_def, outside_def, uncertain);
    ASSERT_EQ(buf, wrong, 0);
  }
}

// ---------------------------------------------------------------------------
// fp64 GPU dispatch coverage — proves the SYCL kernel actually runs.
//
// Before the Phase 1 promotion, the fp64 branch of pgaccel_point_in_ring_bulk
// was a host scalar loop (spatial_predicates.cpp:210-221). This test wraps a
// known-correct fp64 batch in pgaccel_reset_gpu_exec_count() /
// pgaccel_gpu_exec_count() and asserts the counter ticks upward — i.e. the
// kernel reached pgaccel_record_gpu_exec(), which only fires after a
// successful SYCL dispatch.
// ---------------------------------------------------------------------------
static void test_point_in_ring_fp64_gpu_dispatch() {
  printf("--- point_in_ring: fp64 GPU dispatch counter ---\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "  SKIP: no SYCL device available for fp64 GPU dispatch test\n");
    return;
  }

  const size_t N = 4096;
  std::mt19937_64 rng(0xC0FFEEULL);
  std::uniform_real_distribution<double> d(-2.0, 3.0);
  std::vector<double> pts(N * 2);
  for (size_t i = 0; i < N; ++i) {
    pts[2 * i] = d(rng);
    pts[2 * i + 1] = d(rng);
  }
  std::vector<int8_t> results(N, 99);

  pgaccel_reset_gpu_exec_count();
  uint64_t before = pgaccel_gpu_exec_count();

  pgaccel_status s = pgaccel_point_in_ring_bulk(pts.data(), N, square_ring, square_verts,
                                                /*use_fp64=*/true, results.data());
  ASSERT_EQ("fp64 GPU dispatch status OK", s, PGACCEL_OK);

  uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_EQ("fp64 GPU dispatch counter advanced", (int)(after > before), 1);

  size_t still_placeholder = 0;
  for (size_t i = 0; i < N; ++i) {
    if (results[i] == 99)
      ++still_placeholder;
  }
  ASSERT_EQ("fp64 GPU dispatch wrote all results", still_placeholder, (size_t)0);
}

// ---------------------------------------------------------------------------
// point_in_polygon_bulk tests
// ---------------------------------------------------------------------------

struct PipCounts {
  size_t inside = 0;
  size_t outside = 0;
  size_t uncertain = 0;
  size_t untouched = 0;
};

static PipCounts count_pip_results(const std::vector<int8_t>& results) {
  PipCounts counts;
  for (int8_t r : results) {
    if (r == 1) {
      counts.inside++;
    } else if (r == -1) {
      counts.outside++;
    } else if (r == 0) {
      counts.uncertain++;
    } else {
      counts.untouched++;
    }
  }
  return counts;
}

static std::vector<float> make_regular_ring(size_t unique_vertices, float radius) {
  constexpr float two_pi = 6.28318530717958647692f;
  std::vector<float> ring((unique_vertices + 1) * 2);
  for (size_t i = 0; i < unique_vertices; ++i) {
    const float angle = two_pi * static_cast<float>(i) / static_cast<float>(unique_vertices);
    ring[i * 2] = radius * std::cos(angle);
    ring[i * 2 + 1] = radius * std::sin(angle);
  }
  ring[unique_vertices * 2] = ring[0];
  ring[unique_vertices * 2 + 1] = ring[1];
  return ring;
}

static void test_point_in_polygon_bulk_simple_path() {
  printf("--- point_in_polygon_bulk: simple kernel path ---\n");

  float bbox[] = {0.0f, 0.0f, 10.0f, 10.0f};
  float poly[] = {
      0.0f, 0.0f, 10.0f, 0.0f, 10.0f, 10.0f, 0.0f, 10.0f, 0.0f, 0.0f,
  };
  uint32_t rings[] = {0};
  float pts[] = {
      5.0f,  5.0f,   // inside, survives bbox and simple PIP
      20.0f, 20.0f,  // bbox reject
      2.0f,  8.0f,   // inside, survives bbox and simple PIP
      5.0f,  10.0f,  // boundary edge: ST_Intersects = true
      0.0f,  0.0f,   // boundary vertex: ST_Intersects = true
  };
  int8_t results[] = {99, 99, 99, 99, 99};

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts, 5, bbox, poly, 5, rings, 1, results);

  ASSERT_EQ("simple PIP status OK", s, PGACCEL_OK);
  ASSERT_EQ("simple PIP inside[0]", results[0], 1);
  ASSERT_EQ("simple PIP bbox outside", results[1], -1);
  ASSERT_EQ("simple PIP inside[2]", results[2], 1);
  ASSERT_EQ("simple PIP boundary edge", results[3], 1);
  ASSERT_EQ("simple PIP boundary vertex", results[4], 1);
}

static void test_point_in_polygon_bulk_simple_hole_boundary() {
  printf("--- point_in_polygon_bulk: simple kernel hole boundary ---\n");

  float bbox[] = {0.0f, 0.0f, 10.0f, 10.0f};
  float polygon[] = {
      0.0f, 0.0f, 10.0f, 0.0f, 10.0f, 10.0f, 0.0f, 10.0f, 0.0f, 0.0f,
      4.0f, 4.0f, 6.0f, 4.0f,  6.0f,  6.0f,  4.0f, 6.0f,  4.0f, 4.0f,
  };
  uint32_t rings[] = {0, 5};
  float pts[] = {
      5.0f, 4.0f,  // interior-ring boundary: ST_Intersects = true
      5.0f, 5.0f,  // inside the hole: no intersection
      2.0f, 2.0f,  // inside polygon shell
  };
  int8_t results[] = {99, 99, 99};

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts, 3, bbox, polygon, 10, rings, 2, results);

  ASSERT_EQ("simple hole-boundary PIP status OK", s, PGACCEL_OK);
  ASSERT_EQ("simple PIP hole boundary", results[0], 1);
  ASSERT_EQ("simple PIP inside hole", results[1], -1);
  ASSERT_EQ("simple PIP shell interior", results[2], 1);
}

static void test_point_in_polygon_bulk_simple_slab_large_batch() {
  printf("--- point_in_polygon_bulk: simple slab large batch ---\n");

  constexpr size_t point_count = 100000;
  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  float diamond[] = {
      0.0f, 1.0f, 1.0f, 0.0f, 0.0f, -1.0f, -1.0f, 0.0f, 0.0f, 1.0f,
  };
  uint32_t rings[] = {0};

  std::vector<float> pts(point_count * 2);
  for (size_t i = 0; i < point_count; ++i) {
    switch (i % 5) {
      case 0:
        pts[i * 2] = 0.0f;
        pts[i * 2 + 1] = 0.0f;
        break;
      case 1:
        pts[i * 2] = 0.40f;
        pts[i * 2 + 1] = 0.20f;
        break;
      case 2:
        pts[i * 2] = 0.90f;
        pts[i * 2 + 1] = 0.90f;
        break;
      case 3:
        pts[i * 2] = -0.90f;
        pts[i * 2 + 1] = -0.90f;
        break;
      default:
        pts[i * 2] = 2.0f;
        pts[i * 2 + 1] = 2.0f;
        break;
    }
  }

  std::vector<int8_t> results(point_count, 99);
  pgaccel_reset_gpu_exec_count();
  const uint64_t before = pgaccel_gpu_exec_count();

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts.data(), point_count, bbox, diamond, 5, rings,
                                                   1, results.data());

  ASSERT_EQ("simple slab status OK", s, PGACCEL_OK);
  const uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_EQ("simple slab dispatched one GPU kernel", (int)(after == before + 1), 1);

  const PipCounts counts = count_pip_results(results);
  ASSERT_EQ("simple slab inside count", counts.inside, (size_t)40000);
  ASSERT_EQ("simple slab outside count", counts.outside, (size_t)60000);
  ASSERT_EQ("simple slab uncertain count", counts.uncertain, (size_t)0);
  ASSERT_EQ("simple slab wrote every result", counts.untouched, (size_t)0);
}

static void test_point_in_polygon_bulk_coop_path() {
  printf("--- point_in_polygon_bulk: cooperative kernel path ---\n");

  constexpr size_t unique_vertices = 1024;
  std::vector<float> ring = make_regular_ring(unique_vertices, 1.0f);

  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  uint32_t rings[] = {0};
  const float edge_mid_x = (ring[0] + ring[2]) * 0.5f;
  const float edge_mid_y = (ring[1] + ring[3]) * 0.5f;
  float pts[] = {
      0.0f,   0.0f,  // inside
      0.9f,   0.9f,  // inside bbox, outside polygon
      2.0f,   0.0f,  // bbox reject
      -0.25f, 0.5f,  // inside
      edge_mid_x,
      edge_mid_y,  // boundary edge: ST_Intersects = true
      ring[0],
      ring[1],  // boundary vertex: ST_Intersects = true
  };
  int8_t results[] = {99, 99, 99, 99, 99, 99};

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts, 6, bbox, ring.data(), unique_vertices + 1,
                                                   rings, 1, results);

  ASSERT_EQ("coop PIP status OK", s, PGACCEL_OK);
  ASSERT_EQ("coop PIP inside[0]", results[0], 1);
  ASSERT_EQ("coop PIP in-bbox outside", results[1], -1);
  ASSERT_EQ("coop PIP bbox outside", results[2], -1);
  ASSERT_EQ("coop PIP inside[3]", results[3], 1);
  ASSERT_EQ("coop PIP boundary edge", results[4], 1);
  ASSERT_EQ("coop PIP boundary vertex", results[5], 1);
}

static void test_point_in_polygon_bulk_coop_hole_boundary() {
  printf("--- point_in_polygon_bulk: cooperative kernel hole boundary ---\n");

  constexpr size_t unique_vertices = 1024;
  std::vector<float> polygon = make_regular_ring(unique_vertices, 1.0f);
  const size_t hole_offset = polygon.size() / 2;
  const float hole[] = {
      -0.15f, -0.15f, 0.15f, -0.15f, 0.15f, 0.15f, -0.15f, 0.15f, -0.15f, -0.15f,
  };
  polygon.insert(polygon.end(), hole, hole + sizeof(hole) / sizeof(hole[0]));

  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  uint32_t rings[] = {0, static_cast<uint32_t>(hole_offset)};
  float pts[] = {
      0.0f,  -0.15f,  // interior-ring boundary: ST_Intersects = true
      0.0f,  0.0f,    // inside the hole: no intersection
      0.50f, 0.00f,   // inside polygon shell
  };
  int8_t results[] = {99, 99, 99};

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts, 3, bbox, polygon.data(),
                                                   polygon.size() / 2, rings, 2, results);

  ASSERT_EQ("coop hole-boundary PIP status OK", s, PGACCEL_OK);
  ASSERT_EQ("coop PIP hole boundary", results[0], 1);
  ASSERT_EQ("coop PIP inside hole", results[1], -1);
  ASSERT_EQ("coop PIP shell interior", results[2], 1);
}

static void test_point_in_polygon_bulk_coop_slab_large_batch_with_hole() {
  printf("--- point_in_polygon_bulk: cooperative slab large batch with hole ---\n");

  constexpr size_t point_count = 100000;
  constexpr size_t unique_vertices = 2048;

  std::vector<float> polygon = make_regular_ring(unique_vertices, 1.0f);
  const size_t hole_offset = polygon.size() / 2;
  const float hole[] = {
      -0.15f, -0.15f, 0.15f, -0.15f, 0.15f, 0.15f, -0.15f, 0.15f, -0.15f, -0.15f,
  };
  polygon.insert(polygon.end(), hole, hole + sizeof(hole) / sizeof(hole[0]));

  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  uint32_t rings[] = {0, static_cast<uint32_t>(hole_offset)};

  std::vector<float> pts(point_count * 2);
  for (size_t i = 0; i < point_count; ++i) {
    switch (i % 5) {
      case 0:
        pts[i * 2] = 0.50f;
        pts[i * 2 + 1] = 0.00f;
        break;
      case 1:
        pts[i * 2] = -0.50f;
        pts[i * 2 + 1] = 0.25f;
        break;
      case 2:
        pts[i * 2] = 0.00f;
        pts[i * 2 + 1] = 0.00f;
        break;
      case 3:
        pts[i * 2] = 0.95f;
        pts[i * 2 + 1] = 0.95f;
        break;
      default:
        pts[i * 2] = 1.50f;
        pts[i * 2 + 1] = 0.00f;
        break;
    }
  }

  std::vector<int8_t> results(point_count, 99);
  pgaccel_reset_gpu_exec_count();
  const uint64_t before = pgaccel_gpu_exec_count();

  pgaccel_status s = pgaccel_point_in_polygon_bulk(pts.data(), point_count, bbox, polygon.data(),
                                                   polygon.size() / 2, rings, 2, results.data());

  ASSERT_EQ("coop slab status OK", s, PGACCEL_OK);
  const uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_EQ("coop slab dispatched one GPU kernel", (int)(after == before + 1), 1);

  const PipCounts counts = count_pip_results(results);
  ASSERT_EQ("coop slab inside count", counts.inside, (size_t)40000);
  ASSERT_EQ("coop slab outside count", counts.outside, (size_t)60000);
  ASSERT_EQ("coop slab uncertain count", counts.uncertain, (size_t)0);
  ASSERT_EQ("coop slab wrote every result", counts.untouched, (size_t)0);
}

// ---------------------------------------------------------------------------
// st_area_bulk tests — Shoelace formula on flat [x,y,x,y,...] CSR layout
// ---------------------------------------------------------------------------
//
// Kernel signature (`pgaccel_st_area_bulk`): single-ring polygons only;
// dispatcher must short-circuit multi-ring / hole shapes to UNCERTAIN
// before invoking. Result returned in coordinate-system units squared.
// Both fp32 and fp64 paths are SYCL kernels.

static void test_st_area_bulk_basic_fp32() {
  printf("--- st_area_bulk: basic fp32 ---\n");

  // Three single-ring polygons packed CSR-style:
  //   row 0: unit square at origin  → area = 1
  //   row 1: 3-4 right triangle     → area = 6
  //   row 2: 10×10 square           → area = 100
  const float coords[] = {
      // Unit square (5 verts, last == first to close ring is OK but
      // not required by Shoelace; kernel handles either)
      0.0f,
      0.0f,
      1.0f,
      0.0f,
      1.0f,
      1.0f,
      0.0f,
      1.0f,
      // 3-4-5 triangle
      0.0f,
      0.0f,
      3.0f,
      0.0f,
      0.0f,
      4.0f,
      // 10×10 square
      0.0f,
      0.0f,
      10.0f,
      0.0f,
      10.0f,
      10.0f,
      0.0f,
      10.0f,
  };
  // CSR offsets count COORDINATES (not vertices): unit square has
  // 4 verts × 2 = 8 coords; triangle 3 × 2 = 6; large square 4 × 2 = 8.
  const uint32_t offsets[] = {0, 8, 14, 22};
  float areas[3] = {-1.0f, -1.0f, -1.0f};
  pgaccel_status s = pgaccel_st_area_bulk(coords, offsets, 3, false, areas);
  ASSERT_EQ("st_area_bulk fp32 status OK", s, PGACCEL_OK);
  ASSERT_NEAR("st_area_bulk unit square = 1", areas[0], 1.0f, 1e-5f);
  ASSERT_NEAR("st_area_bulk 3-4 triangle = 6", areas[1], 6.0f, 1e-5f);
  ASSERT_NEAR("st_area_bulk 10×10 square = 100", areas[2], 100.0f, 1e-3f);
}

static void test_st_area_bulk_orientation_fp32() {
  printf("--- st_area_bulk: orientation invariance ---\n");

  // Same triangle, CCW vs CW order — Shoelace returns |area|, so both
  // should give the same magnitude.
  const float coords[] = {
      // CCW: (0,0) → (3,0) → (0,4)
      0.0f,
      0.0f,
      3.0f,
      0.0f,
      0.0f,
      4.0f,
      // CW: (0,0) → (0,4) → (3,0)
      0.0f,
      0.0f,
      0.0f,
      4.0f,
      3.0f,
      0.0f,
  };
  const uint32_t offsets[] = {0, 6, 12};
  float areas[2] = {-1.0f, -1.0f};
  pgaccel_status s = pgaccel_st_area_bulk(coords, offsets, 2, false, areas);
  ASSERT_EQ("st_area_bulk orientation status OK", s, PGACCEL_OK);
  ASSERT_NEAR("st_area_bulk CCW triangle = 6", areas[0], 6.0f, 1e-5f);
  ASSERT_NEAR("st_area_bulk CW triangle = 6 (|.|)", areas[1], 6.0f, 1e-5f);
}

static void test_st_area_bulk_empty() {
  printf("--- st_area_bulk: empty input ---\n");
  // row_count = 0 → OK no-op (per FFI contract).
  pgaccel_status s = pgaccel_st_area_bulk(nullptr, nullptr, 0, false, nullptr);
  ASSERT_EQ("st_area_bulk empty input is OK", s, PGACCEL_OK);
}

// ---------------------------------------------------------------------------
// st_length_bulk tests — Euclidean edge-length sum (fp32 only today)
// ---------------------------------------------------------------------------

static void test_st_length_bulk_closed_ring() {
  printf("--- st_length_bulk: closed ring (Polygon perimeter) ---\n");

  // Unit square closed: 4 edges × 1 = 4
  // 3-4-5 right triangle closed: 3 + 4 + 5 = 12
  const float coords[] = {
      // Unit square
      0.0f,
      0.0f,
      1.0f,
      0.0f,
      1.0f,
      1.0f,
      0.0f,
      1.0f,
      // 3-4-5 triangle: (0,0) (3,0) (0,4) — closed adds wrap-around 5
      0.0f,
      0.0f,
      3.0f,
      0.0f,
      0.0f,
      4.0f,
  };
  const uint32_t offsets[] = {0, 8, 14};
  float lengths[2] = {-1.0f, -1.0f};
  pgaccel_status s = pgaccel_st_length_bulk(coords, offsets, 2, false, true, lengths);
  ASSERT_EQ("st_length_bulk closed status OK", s, PGACCEL_OK);
  ASSERT_NEAR("unit-square perimeter = 4", lengths[0], 4.0f, 1e-5f);
  ASSERT_NEAR("3-4-5 triangle perimeter = 12", lengths[1], 12.0f, 1e-4f);
}

static void test_st_length_bulk_open_path() {
  printf("--- st_length_bulk: open path (LineString) ---\n");

  // Open path (closed_ring = false):
  //   (0,0) → (3,0) → (0,4): two edges of length 3 and 5 → total 8
  //   (0,0) → (3,0): single edge of length 3
  const float coords[] = {
      0.0f, 0.0f, 3.0f, 0.0f, 0.0f, 4.0f, 0.0f, 0.0f, 3.0f, 0.0f,
  };
  const uint32_t offsets[] = {0, 6, 10};
  float lengths[2] = {-1.0f, -1.0f};
  pgaccel_status s = pgaccel_st_length_bulk(coords, offsets, 2, false, false, lengths);
  ASSERT_EQ("st_length_bulk open status OK", s, PGACCEL_OK);
  // Two edges: (0,0)→(3,0) length 3; (3,0)→(0,4) length 5. Total 8.
  ASSERT_NEAR("open 3-edge path length = 8", lengths[0], 8.0f, 1e-4f);
  ASSERT_NEAR("open 1-edge path length = 3", lengths[1], 3.0f, 1e-5f);
}

static void test_st_length_bulk_degenerate() {
  printf("--- st_length_bulk: degenerate inputs ---\n");

  // Single-vertex path → length 0 (vertex_count < 2 sentinel).
  // Two coincident vertices → length 0.
  const float coords[] = {
      // Single vertex
      5.0f,
      5.0f,
      // Two coincident vertices
      2.0f,
      2.0f,
      2.0f,
      2.0f,
  };
  const uint32_t offsets[] = {0, 2, 6};
  float lengths[2] = {-1.0f, -1.0f};
  pgaccel_status s = pgaccel_st_length_bulk(coords, offsets, 2, false, false, lengths);
  ASSERT_EQ("st_length_bulk degenerate status OK", s, PGACCEL_OK);
  ASSERT_NEAR("single vertex → length 0", lengths[0], 0.0f, 1e-6f);
  ASSERT_NEAR("coincident vertices → length 0", lengths[1], 0.0f, 1e-6f);
}

static void test_st_length_bulk_fp64() {
  printf("--- st_length_bulk: fp64 (post-split) ---\n");

  // row 0: open path (3,0) -> (0,4): length sqrt(9+16) = 5
  // row 1: open path (0,0)->(3,0)->(0,4)->(0,0): 3 + 5 + 4 = 12
  const double coords[] = {
      // row 0
      3.0,
      0.0,
      0.0,
      4.0,
      // row 1
      0.0,
      0.0,
      3.0,
      0.0,
      0.0,
      4.0,
      0.0,
      0.0,
  };
  const uint32_t offsets[] = {0, 4, 12};

  // Open mode test
  double lengths[2] = {-1.0, -1.0};
  pgaccel_status s = pgaccel_st_length_bulk(coords, offsets, 2, true, false, lengths);
  ASSERT_EQ("fp64 open path status OK", s, PGACCEL_OK);
  ASSERT_NEAR("fp64 open row 0 length = 5", lengths[0], 5.0, 1e-9);
  ASSERT_NEAR("fp64 open row 1 length = 12", lengths[1], 12.0, 1e-9);

  // Closed-ring mode: row 0 is 2 verts so wrap = 5 + 5 = 10;
  // row 1 wraps (0,0)->(0,0) = +0, total still 12.
  double lengths2[2] = {-1.0, -1.0};
  pgaccel_status s2 = pgaccel_st_length_bulk(coords, offsets, 2, true, true, lengths2);
  ASSERT_EQ("fp64 closed status OK", s2, PGACCEL_OK);
  ASSERT_NEAR("fp64 closed row 0 = 10 (wrap)", lengths2[0], 10.0, 1e-9);
  ASSERT_NEAR("fp64 closed row 1 = 12", lengths2[1], 12.0, 1e-9);
}

// ---------------------------------------------------------------------------
// st_distance_polygon_polygon tests
// ---------------------------------------------------------------------------

static void test_st_distance_polygon_polygon_disjoint() {
  printf("--- st_distance_polygon_polygon: disjoint pair ---\n");
  // Polygon A: unit square at origin (0,0)-(1,1). Closed ring:
  //   (0,0)(1,0)(1,1)(0,1)(0,0)  -> 5 verts
  // Polygon B: unit square offset by (3,0): (3,0)-(4,1).
  //   (3,0)(4,0)(4,1)(3,1)(3,0)  -> 5 verts
  // Boundary distance between right edge of A (x=1) and left edge of B (x=3)
  // = 2.0. Boundaries do not touch → uncertain = 0 (definite).
  const float coords_a[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  const float coords_b[] = {
      3.0f, 0.0f, 4.0f, 0.0f, 4.0f, 1.0f, 3.0f, 1.0f, 3.0f, 0.0f,
  };
  const uint32_t off_a[] = {0, 10};
  const uint32_t off_b[] = {0, 10};
  float dist[1] = {-1.0f};
  uint8_t unc[1] = {1};
  pgaccel_status s =
      pgaccel_st_distance_polygon_polygon_bulk(coords_a, off_a, coords_b, off_b, 1, dist, unc);
  ASSERT_EQ("status OK", s, PGACCEL_OK);
  ASSERT_EQ("disjoint definite", unc[0], 0);
  ASSERT_NEAR("disjoint distance = 2", dist[0], 2.0f, 1e-4f);
}

static void test_st_distance_polygon_polygon_touching() {
  printf("--- st_distance_polygon_polygon: touching boundaries ---\n");
  // Two unit squares that share an edge at x=1. Right edge of A coincides
  // with left edge of B. Boundary-distance = 0; mark as UNCERTAIN so PG
  // re-checks whether they intersect / contain.
  const float coords_a[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  const float coords_b[] = {
      1.0f, 0.0f, 2.0f, 0.0f, 2.0f, 1.0f, 1.0f, 1.0f, 1.0f, 0.0f,
  };
  const uint32_t off_a[] = {0, 10};
  const uint32_t off_b[] = {0, 10};
  float dist[1] = {-1.0f};
  uint8_t unc[1] = {0};
  pgaccel_st_distance_polygon_polygon_bulk(coords_a, off_a, coords_b, off_b, 1, dist, unc);
  ASSERT_EQ("touching → UNCERTAIN flag set", unc[0], 1);
}

static void test_st_distance_polygon_polygon_degenerate() {
  printf("--- st_distance_polygon_polygon: degenerate row -> UNCERTAIN ---\n");
  // Row 0 has < 3 verts (4 floats); kernel should mark UNCERTAIN.
  const float coords_a[] = {0.0f, 0.0f, 1.0f, 1.0f};
  const float coords_b[] = {
      3.0f, 0.0f, 4.0f, 0.0f, 4.0f, 1.0f, 3.0f, 1.0f, 3.0f, 0.0f,
  };
  const uint32_t off_a[] = {0, 4};
  const uint32_t off_b[] = {0, 10};
  float dist[1] = {-1.0f};
  uint8_t unc[1] = {0};
  pgaccel_st_distance_polygon_polygon_bulk(coords_a, off_a, coords_b, off_b, 1, dist, unc);
  ASSERT_EQ("degenerate → UNCERTAIN", unc[0], 1);
}

// ---------------------------------------------------------------------------
// Algorithmic predicate tests (st_equals / _touches / _crosses / _overlaps).
//
// All four kernels classify pairs into DEFINITE TRUE (1), DEFINITE FALSE
// (-1), or UNCERTAIN (0). The host-side fast-path covers identical
// Point/Point coords, identical Polygon/Polygon ring vertex sets, and
// disjoint bboxes. Everything else routes to UNCERTAIN.
// ---------------------------------------------------------------------------

// Helper: build a Point geometry descriptor from two stored coords.
static pgaccel_geometry make_point_geom(const float* coords, const float* bbox) {
  pgaccel_geometry g;
  g.type = PGACCEL_GEOM_POINT;
  g.bbox = bbox;
  g.coords = coords;
  g.coord_count = 1;
  g.ring_offsets = nullptr;
  g.ring_count = 0;
  return g;
}

// Helper: build a Polygon geometry descriptor.
static pgaccel_geometry make_polygon_geom(const float* coords, size_t coord_count,
                                          const float* bbox) {
  pgaccel_geometry g;
  g.type = PGACCEL_GEOM_POLYGON;
  g.bbox = bbox;
  g.coords = coords;
  g.coord_count = coord_count;
  g.ring_offsets = nullptr;
  g.ring_count = 0;
  return g;
}

static void test_st_equals_bulk() {
  printf("--- st_equals_bulk ---\n");
  // Point/Point identical → DEFINITE TRUE
  // Point/Point disjoint  → DEFINITE FALSE
  // Polygon/Polygon identical → DEFINITE TRUE
  // Cross-dim Point/Polygon → DEFINITE FALSE (different dims)
  const float pt_a_xy[] = {1.0f, 1.0f};
  const float pt_a_bbox[] = {1.0f, 1.0f, 1.0f, 1.0f};
  const float pt_b_xy[] = {1.0f, 1.0f};
  const float pt_b_bbox[] = {1.0f, 1.0f, 1.0f, 1.0f};
  const float pt_c_xy[] = {99.0f, 99.0f};
  const float pt_c_bbox[] = {99.0f, 99.0f, 99.0f, 99.0f};

  // Unit square ring (CCW, closed)
  const float poly_xy[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  const float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};
  // Different polygon (offset)
  const float poly2_xy[] = {
      10.0f, 10.0f, 11.0f, 10.0f, 11.0f, 11.0f, 10.0f, 11.0f, 10.0f, 10.0f,
  };
  const float poly2_bbox[] = {10.0f, 10.0f, 11.0f, 11.0f};

  pgaccel_geometry g_a[4];
  pgaccel_geometry g_b[4];
  g_a[0] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[0] = make_point_geom(pt_b_xy, pt_b_bbox);  // identical points
  g_a[1] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[1] = make_point_geom(pt_c_xy, pt_c_bbox);  // disjoint points
  g_a[2] = make_polygon_geom(poly_xy, 5, poly_bbox);
  g_b[2] = make_polygon_geom(poly_xy, 5, poly_bbox);  // identical polygons
  g_a[3] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[3] = make_polygon_geom(poly2_xy, 5, poly2_bbox);  // mixed dim, disjoint

  int8_t results[4] = {99, 99, 99, 99};
  pgaccel_status s = pgaccel_st_equals_bulk(g_a, g_b, 4, results);
  ASSERT_EQ("status OK", s, PGACCEL_OK);
  ASSERT_EQ("Point=Point identical → 1", results[0], 1);
  ASSERT_EQ("Point=Point disjoint → -1", results[1], -1);
  ASSERT_EQ("Polygon=Polygon identical → 1", results[2], 1);
  ASSERT_EQ("Point/Polygon mixed-dim → -1", results[3], -1);
}

static void test_st_touches_bulk() {
  printf("--- st_touches_bulk ---\n");
  // Disjoint bboxes → DEFINITE FALSE
  // Identical points → DEFINITE FALSE (interiors overlap)
  // Anything more complex → UNCERTAIN
  const float pt_a_xy[] = {0.0f, 0.0f};
  const float pt_a_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};
  const float pt_far_xy[] = {99.0f, 99.0f};
  const float pt_far_bbox[] = {99.0f, 99.0f, 99.0f, 99.0f};
  const float pt_eq_xy[] = {0.0f, 0.0f};
  const float pt_eq_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};

  pgaccel_geometry g_a[2];
  pgaccel_geometry g_b[2];
  g_a[0] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[0] = make_point_geom(pt_far_xy, pt_far_bbox);  // disjoint
  g_a[1] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[1] = make_point_geom(pt_eq_xy, pt_eq_bbox);  // identical

  int8_t results[2] = {99, 99};
  pgaccel_status s = pgaccel_st_touches_bulk(g_a, g_b, 2, results);
  ASSERT_EQ("status OK", s, PGACCEL_OK);
  ASSERT_EQ("Disjoint bbox → -1", results[0], -1);
  ASSERT_EQ("Identical points (interiors overlap) → -1", results[1], -1);
}

static void test_st_crosses_bulk() {
  printf("--- st_crosses_bulk ---\n");
  // Disjoint bboxes → DEFINITE FALSE
  // Identical points → DEFINITE FALSE
  const float pt_a_xy[] = {0.0f, 0.0f};
  const float pt_a_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};
  const float pt_far_xy[] = {99.0f, 99.0f};
  const float pt_far_bbox[] = {99.0f, 99.0f, 99.0f, 99.0f};

  pgaccel_geometry g_a[2];
  pgaccel_geometry g_b[2];
  g_a[0] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[0] = make_point_geom(pt_far_xy, pt_far_bbox);
  g_a[1] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[1] = make_point_geom(pt_a_xy, pt_a_bbox);

  int8_t results[2] = {99, 99};
  pgaccel_status s = pgaccel_st_crosses_bulk(g_a, g_b, 2, results);
  ASSERT_EQ("status OK", s, PGACCEL_OK);
  ASSERT_EQ("Disjoint bbox → -1", results[0], -1);
  ASSERT_EQ("Identical points → -1", results[1], -1);
}

static void test_st_overlaps_bulk() {
  printf("--- st_overlaps_bulk ---\n");
  // Disjoint bboxes → DEFINITE FALSE
  // Different types → DEFINITE FALSE (same-dim required)
  // Identical polygons → DEFINITE FALSE (intersection equals input)
  const float pt_a_xy[] = {0.0f, 0.0f};
  const float pt_a_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};
  const float pt_far_xy[] = {99.0f, 99.0f};
  const float pt_far_bbox[] = {99.0f, 99.0f, 99.0f, 99.0f};

  const float poly_xy[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  const float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};

  pgaccel_geometry g_a[3];
  pgaccel_geometry g_b[3];
  g_a[0] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[0] = make_point_geom(pt_far_xy, pt_far_bbox);  // disjoint
  g_a[1] = make_point_geom(pt_a_xy, pt_a_bbox);
  g_b[1] = make_polygon_geom(poly_xy, 5, poly_bbox);  // mixed dim
  g_a[2] = make_polygon_geom(poly_xy, 5, poly_bbox);
  g_b[2] = make_polygon_geom(poly_xy, 5, poly_bbox);  // identical polygons

  int8_t results[3] = {99, 99, 99};
  pgaccel_status s = pgaccel_st_overlaps_bulk(g_a, g_b, 3, results);
  ASSERT_EQ("status OK", s, PGACCEL_OK);
  ASSERT_EQ("Disjoint bbox → -1", results[0], -1);
  ASSERT_EQ("Mixed dim → -1", results[1], -1);
  ASSERT_EQ("Identical polygons → -1", results[2], -1);
}

static void test_algorithmic_predicate_slab_dispatch() {
  printf("--- algorithmic predicates: slab GPU dispatch ---\n");

  const float pt_xy[] = {0.0f, 0.0f};
  const float pt_bbox[] = {0.0f, 0.0f, 0.0f, 0.0f};
  const float far_xy[] = {99.0f, 99.0f};
  const float far_bbox[] = {99.0f, 99.0f, 99.0f, 99.0f};
  const float poly_xy[] = {
      0.0f, 0.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f,
  };
  const float poly_bbox[] = {0.0f, 0.0f, 1.0f, 1.0f};

  pgaccel_geometry g_a[3];
  pgaccel_geometry g_b[3];
  g_a[0] = make_point_geom(pt_xy, pt_bbox);
  g_b[0] = make_point_geom(pt_xy, pt_bbox);
  g_a[1] = make_point_geom(pt_xy, pt_bbox);
  g_b[1] = make_point_geom(far_xy, far_bbox);
  g_a[2] = make_polygon_geom(poly_xy, 5, poly_bbox);
  g_b[2] = make_polygon_geom(poly_xy, 5, poly_bbox);

  pgaccel_reset_gpu_exec_count();
  uint64_t before = pgaccel_gpu_exec_count();

  int8_t equals[3] = {99, 99, 99};
  pgaccel_status s = pgaccel_st_equals_bulk(g_a, g_b, 3, equals);
  ASSERT_EQ("equals slab status OK", s, PGACCEL_OK);
  ASSERT_EQ("equals identical point", equals[0], 1);
  ASSERT_EQ("equals disjoint point", equals[1], -1);
  ASSERT_EQ("equals identical polygon", equals[2], 1);

  int8_t touches[3] = {99, 99, 99};
  s = pgaccel_st_touches_bulk(g_a, g_b, 3, touches);
  ASSERT_EQ("touches slab status OK", s, PGACCEL_OK);
  ASSERT_EQ("touches identical point", touches[0], -1);
  ASSERT_EQ("touches disjoint point", touches[1], -1);
  ASSERT_EQ("touches identical polygon", touches[2], -1);

  int8_t crosses[3] = {99, 99, 99};
  s = pgaccel_st_crosses_bulk(g_a, g_b, 3, crosses);
  ASSERT_EQ("crosses slab status OK", s, PGACCEL_OK);
  ASSERT_EQ("crosses identical point", crosses[0], -1);
  ASSERT_EQ("crosses disjoint point", crosses[1], -1);
  ASSERT_EQ("crosses identical polygon", crosses[2], -1);

  int8_t overlaps[3] = {99, 99, 99};
  s = pgaccel_st_overlaps_bulk(g_a, g_b, 3, overlaps);
  ASSERT_EQ("overlaps slab status OK", s, PGACCEL_OK);
  ASSERT_EQ("overlaps identical point", overlaps[0], -1);
  ASSERT_EQ("overlaps disjoint point", overlaps[1], -1);
  ASSERT_EQ("overlaps identical polygon", overlaps[2], -1);

  uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_EQ("four algorithmic predicate kernels dispatched", (int)(after == before + 4), 1);
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

int main() {
  printf("=== pgaccel spatial predicate tests ===\n\n");

  // pgaccel_point_in_ring_bulk fp64 is a SYCL kernel (Phase 1 promotion);
  // it requires an initialized SYCL queue. Init is idempotent across the
  // process lifetime, so calling here is safe.
  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run spatial tests\n");
    return 1;
  }

  test_point_in_ring_basic();
  test_point_in_ring_edge_cases();
  test_point_in_ring_fp32();
  test_point_in_ring_triangle();
  test_point_in_ring_fp64_bulk();
  test_point_in_ring_fp64_gpu_dispatch();
  test_point_in_polygon_bulk_simple_path();
  test_point_in_polygon_bulk_simple_hole_boundary();
  test_point_in_polygon_bulk_simple_slab_large_batch();
  test_point_in_polygon_bulk_coop_path();
  test_point_in_polygon_bulk_coop_hole_boundary();
  test_point_in_polygon_bulk_coop_slab_large_batch_with_hole();

  test_sphere_distance_basic();
  test_sphere_distance_edge_cases();
  test_sphere_distance_fp32();
  test_sphere_distance_fp64();

  test_segment_intersects_basic();
  test_segment_intersects_edge_cases();
  test_segment_intersects_fp32();

  test_st_area_bulk_basic_fp32();
  test_st_area_bulk_orientation_fp32();
  test_st_area_bulk_empty();

  test_st_length_bulk_closed_ring();
  test_st_length_bulk_open_path();
  test_st_length_bulk_degenerate();
  test_st_length_bulk_fp64();

  test_st_distance_polygon_polygon_disjoint();
  test_st_distance_polygon_polygon_touching();
  test_st_distance_polygon_polygon_degenerate();

  test_st_equals_bulk();
  test_st_touches_bulk();
  test_st_crosses_bulk();
  test_st_overlaps_bulk();
  test_algorithmic_predicate_slab_dispatch();

  printf("\n=== Results: %d/%d passed", g_tests_passed, g_tests_run);
  if (g_tests_failed > 0) {
    printf(", %d FAILED", g_tests_failed);
  }
  printf(" ===\n");

  return g_tests_failed > 0 ? 1 : 0;
}

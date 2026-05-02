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

  // fp32 path is the supported acceleration today; fp64 currently returns
  // PGACCEL_ERROR_NO_DEVICE because instantiating the soft-fp64 SYCL kernel
  // hangs Metal SSCP JIT. See sphere_distance_bulk_sycl<T> notes in
  // spatial_predicates.cpp and TODO Phase 7 (soft-fp64 trig coverage).

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

  // fp64 path currently returns NO_DEVICE; caller routes to PG recheck.
  {
    double a[] = {-74.006, 40.7128};
    double b[] = {-0.1278, 51.5074};
    double dist = 0;
    uint8_t unc = 0;
    pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
    ASSERT_EQ("fp64 -> NO_DEVICE (deferred)", s, PGACCEL_ERROR_NO_DEVICE);
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

  test_sphere_distance_basic();
  test_sphere_distance_edge_cases();
  test_sphere_distance_fp32();

  test_segment_intersects_basic();
  test_segment_intersects_edge_cases();
  test_segment_intersects_fp32();

  printf("\n=== Results: %d/%d passed", g_tests_passed, g_tests_run);
  if (g_tests_failed > 0) {
    printf(", %d FAILED", g_tests_failed);
  }
  printf(" ===\n");

  return g_tests_failed > 0 ? 1 : 0;
}

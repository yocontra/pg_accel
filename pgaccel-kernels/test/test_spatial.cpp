#include "pgaccel_ffi.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>

// ---------------------------------------------------------------------------
// Minimal test harness
// ---------------------------------------------------------------------------

static int g_tests_run    = 0;
static int g_tests_passed = 0;
static int g_tests_failed = 0;

#define ASSERT_EQ(desc, got, expected) do {                         \
    g_tests_run++;                                                  \
    if ((got) == (expected)) {                                      \
        g_tests_passed++;                                           \
    } else {                                                        \
        g_tests_failed++;                                           \
        fprintf(stderr, "  FAIL: %s — got %d, expected %d\n",      \
                (desc), (int)(got), (int)(expected));               \
    }                                                               \
} while (0)

#define ASSERT_NEAR(desc, got, expected, tol) do {                  \
    g_tests_run++;                                                  \
    if (std::abs((got) - (expected)) <= (tol)) {                    \
        g_tests_passed++;                                           \
    } else {                                                        \
        g_tests_failed++;                                           \
        fprintf(stderr, "  FAIL: %s — got %.6f, expected %.6f\n",  \
                (desc), (double)(got), (double)(expected));         \
    }                                                               \
} while (0)

// ---------------------------------------------------------------------------
// point_in_ring tests
// ---------------------------------------------------------------------------

// Unit square: (0,0) (1,0) (1,1) (0,1) (0,0)
static const double square_ring[] = {
    0.0, 0.0,
    1.0, 0.0,
    1.0, 1.0,
    0.0, 1.0,
    0.0, 0.0,
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
        pgaccel_status s = pgaccel_point_in_ring_bulk(nullptr, 0, square_ring, square_verts, true, nullptr);
        ASSERT_EQ("zero count OK", s, PGACCEL_OK);
    }

    // Null pointers
    {
        int8_t result;
        pgaccel_status s = pgaccel_point_in_ring_bulk(nullptr, 1, square_ring, square_verts, true, &result);
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

    // New York to London (known ~5570 km)
    {
        double a[] = {-74.006, 40.7128};  // NYC
        double b[] = {-0.1278, 51.5074};  // London
        double dist = 0;
        uint8_t unc = 1;
        pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
        ASSERT_EQ("status OK", s, PGACCEL_OK);
        ASSERT_EQ("NYC-London definite", unc, 0);
        // Haversine gives ~5570 km, allow 50 km tolerance
        ASSERT_NEAR("NYC-London ~5570km", dist / 1000.0, 5570.0, 50.0);
    }

    // Same point — should be uncertain (very close)
    {
        double a[] = {0.0, 0.0};
        double b[] = {0.0, 0.0};
        double dist = 0;
        uint8_t unc = 0;
        pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
        ASSERT_EQ("same point -> uncertain", unc, 1);
    }

    // Equator distance: 1 degree longitude at equator ~ 111.195 km
    {
        double a[] = {0.0, 0.0};
        double b[] = {1.0, 0.0};
        double dist = 0;
        uint8_t unc = 0;
        pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
        ASSERT_EQ("equator 1deg definite", unc, 0);
        ASSERT_NEAR("equator 1deg ~111km", dist / 1000.0, 111.195, 1.0);
    }
}

static void test_sphere_distance_edge_cases() {
    printf("--- sphere_distance: edge cases ---\n");

    // Antipodal points (0,0) to (180,0)
    {
        double a[] = {0.0, 0.0};
        double b[] = {180.0, 0.0};
        double dist = 0;
        uint8_t unc = 0;
        pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
        ASSERT_EQ("antipodal -> uncertain", unc, 1);
    }

    // NaN input
    {
        double nan_val = std::numeric_limits<double>::quiet_NaN();
        double a[] = {nan_val, 0.0};
        double b[] = {0.0, 0.0};
        double dist = 0;
        uint8_t unc = 0;
        pgaccel_sphere_distance_bulk(a, b, 1, true, &dist, &unc);
        ASSERT_EQ("NaN -> uncertain", unc, 1);
    }

    // Null pointers
    {
        pgaccel_status s = pgaccel_sphere_distance_bulk(nullptr, nullptr, 1, true, nullptr, nullptr);
        ASSERT_EQ("null -> error", s, PGACCEL_ERROR_INIT);
    }

    // Zero count
    {
        pgaccel_status s = pgaccel_sphere_distance_bulk(nullptr, nullptr, 0, true, nullptr, nullptr);
        ASSERT_EQ("zero count OK", s, PGACCEL_OK);
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
            0.0, 1.0, 1.0, 0.0,
            0.0, 1.0, 1.0, 1.0,
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
// main
// ---------------------------------------------------------------------------

int main() {
    printf("=== pgaccel spatial predicate tests ===\n\n");

    test_point_in_ring_basic();
    test_point_in_ring_edge_cases();
    test_point_in_ring_fp32();
    test_point_in_ring_triangle();

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

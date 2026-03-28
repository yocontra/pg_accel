#include "pgaccel_ffi.h"

#include <cmath>
#include <cstdlib>
#include <cstring>
#include <vector>

/* ----------------------------------------------------------------
 * Layer 2 — CPU fallback predicates
 *
 * These inline implementations handle the geometric tests on the
 * CPU.  They will be replaced by bulk GPU kernel calls once
 * spatial_predicates.cpp lands.
 *
 * Return values:
 *    1  = DEFINITE_TRUE   (geometries definitely intersect)
 *   -1  = DEFINITE_FALSE  (geometries definitely do not intersect)
 *    0  = UNCERTAIN       (need CPU recheck on Rust side)
 * ---------------------------------------------------------------- */

static constexpr float EPSILON = 1.0e-7f;

/* Point-in-ring via ray casting.
 * ring_coords: flat x,y pairs for a single ring.
 * ring_len:    number of coordinate pairs in the ring.
 * Returns true if point (px,py) is inside the ring. */
static bool point_in_ring(float px, float py,
                          const float* ring_coords, size_t ring_len)
{
    bool inside = false;
    for (size_t i = 0, j = ring_len - 1; i < ring_len; j = i++) {
        float xi = ring_coords[i * 2];
        float yi = ring_coords[i * 2 + 1];
        float xj = ring_coords[j * 2];
        float yj = ring_coords[j * 2 + 1];

        /* Check if the point lies exactly on a vertex. */
        if (std::fabs(px - xi) < EPSILON && std::fabs(py - yi) < EPSILON) {
            return true; /* on vertex — treat as inside */
        }

        bool crosses = ((yi > py) != (yj > py)) &&
                       (px < (xj - xi) * (py - yi) / (yj - yi) + xi);
        if (crosses) {
            inside = !inside;
        }
    }
    return inside;
}

/* Point-in-polygon with holes.
 * Outer ring must contain the point AND no hole ring may contain it. */
static int8_t point_in_polygon_check(const float* pt_coords,
                                     const float* poly_coords,
                                     size_t poly_coord_count,
                                     const uint32_t* ring_offsets,
                                     size_t ring_count)
{
    float px = pt_coords[0];
    float py = pt_coords[1];

    if (ring_count == 0 || ring_offsets == nullptr) {
        /* Treat the whole coord array as one ring. */
        bool inside = point_in_ring(px, py, poly_coords, poly_coord_count);
        return inside ? 1 : -1;
    }

    /* Outer ring is ring 0.  Compute its length. */
    size_t outer_start = ring_offsets[0];
    size_t outer_end = (ring_count > 1) ? ring_offsets[1] : poly_coord_count;
    size_t outer_len = outer_end - outer_start;

    if (!point_in_ring(px, py, poly_coords + outer_start * 2, outer_len)) {
        return -1; /* outside outer ring */
    }

    /* Check hole rings. */
    for (size_t r = 1; r < ring_count; ++r) {
        size_t start = ring_offsets[r];
        size_t end = (r + 1 < ring_count) ? ring_offsets[r + 1] : poly_coord_count;
        size_t len = end - start;
        if (point_in_ring(px, py, poly_coords + start * 2, len)) {
            return -1; /* inside a hole */
        }
    }

    return 1; /* inside polygon, not in any hole */
}

/* 2D cross product of vectors (b-a) and (c-a). */
static float cross2d(float ax, float ay, float bx, float by, float cx, float cy)
{
    return (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
}

/* Test if segments (p1-p2) and (p3-p4) intersect. */
static int8_t segments_intersect(float p1x, float p1y, float p2x, float p2y,
                                 float p3x, float p3y, float p4x, float p4y)
{
    float d1 = cross2d(p3x, p3y, p4x, p4y, p1x, p1y);
    float d2 = cross2d(p3x, p3y, p4x, p4y, p2x, p2y);
    float d3 = cross2d(p1x, p1y, p2x, p2y, p3x, p3y);
    float d4 = cross2d(p1x, p1y, p2x, p2y, p4x, p4y);

    if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) &&
        ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
        return 1; /* proper intersection */
    }

    /* Collinear / endpoint touch — uncertain, let CPU recheck. */
    if (std::fabs(d1) < EPSILON || std::fabs(d2) < EPSILON ||
        std::fabs(d3) < EPSILON || std::fabs(d4) < EPSILON) {
        return 0; /* UNCERTAIN */
    }

    return -1; /* no intersection */
}

/* Check if any segment of linestring A intersects any segment of linestring B. */
static int8_t linestring_intersect_check(const pgaccel_geometry& a,
                                         const pgaccel_geometry& b)
{
    if (a.coord_count < 2 || b.coord_count < 2) {
        return -1; /* degenerate linestrings cannot intersect as lines */
    }

    for (size_t i = 0; i + 1 < a.coord_count; ++i) {
        float ax1 = a.coords[i * 2];
        float ay1 = a.coords[i * 2 + 1];
        float ax2 = a.coords[(i + 1) * 2];
        float ay2 = a.coords[(i + 1) * 2 + 1];

        for (size_t j = 0; j + 1 < b.coord_count; ++j) {
            float bx1 = b.coords[j * 2];
            float by1 = b.coords[j * 2 + 1];
            float bx2 = b.coords[(j + 1) * 2];
            float by2 = b.coords[(j + 1) * 2 + 1];

            int8_t r = segments_intersect(ax1, ay1, ax2, ay2,
                                          bx1, by1, bx2, by2);
            if (r == 1) return 1;   /* definite intersection found */
            if (r == 0) return 0;   /* uncertain — bail to CPU */
        }
    }

    return -1; /* no segment pair intersects */
}

/* Point vs point: equal within epsilon. */
static int8_t points_equal_check(const float* a, const float* b)
{
    if (std::fabs(a[0] - b[0]) < EPSILON && std::fabs(a[1] - b[1]) < EPSILON) {
        return 1; /* coincident */
    }
    return -1;
}

/* Top-level predicate dispatch for a single pair. */
static int8_t evaluate_predicate(const pgaccel_geometry& a,
                                 const pgaccel_geometry& b)
{
    /* Point vs Polygon */
    if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POLYGON) {
        return point_in_polygon_check(a.coords, b.coords, b.coord_count,
                                      b.ring_offsets, b.ring_count);
    }
    /* Polygon vs Point (reverse) */
    if (a.type == PGACCEL_GEOM_POLYGON && b.type == PGACCEL_GEOM_POINT) {
        return point_in_polygon_check(b.coords, a.coords, a.coord_count,
                                      a.ring_offsets, a.ring_count);
    }
    /* Linestring vs Linestring */
    if (a.type == PGACCEL_GEOM_LINESTRING && b.type == PGACCEL_GEOM_LINESTRING) {
        return linestring_intersect_check(a, b);
    }
    /* Point vs Point */
    if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POINT) {
        return points_equal_check(a.coords, b.coords);
    }
    /* Unknown or unsupported combination — UNCERTAIN */
    return 0;
}

/* ================================================================
 * pgaccel_spatial_intersects — three-layer spatial dispatch
 * ================================================================ */
extern "C" pgaccel_status pgaccel_spatial_intersects(
    const pgaccel_geometry* geoms_a,
    size_t count_a,
    const pgaccel_geometry* geoms_b,
    size_t count_b,
    uint32_t* definite_true_pairs,
    size_t* definite_true_count,
    uint32_t* definite_false_pairs,
    size_t* definite_false_count,
    uint32_t* uncertain_pairs,
    size_t* uncertain_count)
{
    *definite_true_count = 0;
    *definite_false_count = 0;
    *uncertain_count = 0;

    if (count_a == 0 || count_b == 0) {
        return PGACCEL_OK;
    }

    /* ----------------------------------------------------------
     * Layer 1: Bbox filter
     *
     * Extract flat bbox arrays from geometry descriptors and
     * call the bulk bbox intersection test.
     * ---------------------------------------------------------- */
    size_t total_pairs = count_a * count_b;

    std::vector<float> bboxes_a(count_a * 4);
    std::vector<float> bboxes_b(count_b * 4);

    for (size_t i = 0; i < count_a; ++i) {
        if (geoms_a[i].bbox != nullptr) {
            std::memcpy(&bboxes_a[i * 4], geoms_a[i].bbox, 4 * sizeof(float));
        } else {
            /* No bbox — use degenerate box that forces a miss. */
            bboxes_a[i * 4 + 0] = 1.0f;   /* xmin > xmax → always misses */
            bboxes_a[i * 4 + 1] = 1.0f;
            bboxes_a[i * 4 + 2] = -1.0f;
            bboxes_a[i * 4 + 3] = -1.0f;
        }
    }
    for (size_t j = 0; j < count_b; ++j) {
        if (geoms_b[j].bbox != nullptr) {
            std::memcpy(&bboxes_b[j * 4], geoms_b[j].bbox, 4 * sizeof(float));
        } else {
            bboxes_b[j * 4 + 0] = 1.0f;
            bboxes_b[j * 4 + 1] = 1.0f;
            bboxes_b[j * 4 + 2] = -1.0f;
            bboxes_b[j * 4 + 3] = -1.0f;
        }
    }

    std::vector<uint8_t> bbox_results(total_pairs);
    size_t bbox_hit_count = 0;

    pgaccel_status bbox_status = pgaccel_bbox_intersects_bulk_f32(
        bboxes_a.data(), count_a,
        bboxes_b.data(), count_b,
        bbox_results.data(),
        &bbox_hit_count);

    if (bbox_status != PGACCEL_OK) {
        return bbox_status;
    }

    /* ----------------------------------------------------------
     * Layer 2: Geometric predicate for bbox survivors
     *
     * Pairs that failed bbox are DEFINITE_FALSE (bbox is
     * conservative — never misses a true intersection).
     * ---------------------------------------------------------- */
    for (size_t i = 0; i < count_a; ++i) {
        for (size_t j = 0; j < count_b; ++j) {
            if (bbox_results[i * count_b + j] == 0) {
                /* Bbox miss → definite false. */
                definite_false_pairs[(*definite_false_count) * 2]     = static_cast<uint32_t>(i);
                definite_false_pairs[(*definite_false_count) * 2 + 1] = static_cast<uint32_t>(j);
                (*definite_false_count)++;
                continue;
            }

            /* Bbox hit → run geometric predicate. */
            int8_t result = evaluate_predicate(geoms_a[i], geoms_b[j]);

            switch (result) {
                case 1: /* DEFINITE_TRUE */
                    definite_true_pairs[(*definite_true_count) * 2]     = static_cast<uint32_t>(i);
                    definite_true_pairs[(*definite_true_count) * 2 + 1] = static_cast<uint32_t>(j);
                    (*definite_true_count)++;
                    break;
                case -1: /* DEFINITE_FALSE */
                    definite_false_pairs[(*definite_false_count) * 2]     = static_cast<uint32_t>(i);
                    definite_false_pairs[(*definite_false_count) * 2 + 1] = static_cast<uint32_t>(j);
                    (*definite_false_count)++;
                    break;
                default: /* UNCERTAIN (0) */
                    uncertain_pairs[(*uncertain_count) * 2]     = static_cast<uint32_t>(i);
                    uncertain_pairs[(*uncertain_count) * 2 + 1] = static_cast<uint32_t>(j);
                    (*uncertain_count)++;
                    break;
            }
        }
    }

    return PGACCEL_OK;
}

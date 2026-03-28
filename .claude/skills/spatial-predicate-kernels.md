---
name: Spatial Predicate Kernel Guide
description: How to port PostGIS spatial predicate fast-paths to GPU kernels with conservative UNCERTAIN fallback
---

# Spatial Predicate Kernel Development

## The Three-Layer Model

```
Layer 1 — GPU bbox filter         4 float comparisons/pair (kills 90-95%)
                ↓ survivors
Layer 2 — GPU geometric fast-path  10-20 lines arithmetic/pair
                │                  fp64 (CUDA/ROCm): resolves 99.9% → ~0.1% UNCERTAIN
                │                  fp32 (Metal):     resolves ~98%  → ~2% UNCERTAIN
                ↓ UNCERTAIN
Layer 3 — CPU recheck             existing PostGIS C function via fmgr_info (bit-identical)
```

**Platform impact:** On CUDA/ROCm with fp64, Layer 3 is nearly unused (~0.1% of rows).
On Metal with fp32, Layer 3 handles ~2% — still a massive win since 98% ran on GPU.

## Correctness Contract

```
For every (geom_a, geom_b) pair:
    gpu_result = gpu_fast_path(geom_a, geom_b)

    if gpu_result == DEFINITE_TRUE:
        return true    // We are 100% certain this is correct
    if gpu_result == DEFINITE_FALSE:
        return false   // We are 100% certain this is correct
    if gpu_result == UNCERTAIN:
        return cpu_postgis(geom_a, geom_b)  // Let PostGIS decide
```

**RULE: When in doubt, return UNCERTAIN.** A false DEFINITE is a correctness bug.
An unnecessary UNCERTAIN is just a performance miss.

## Result Codes
```cpp
enum PgAccelSpatialResult : int8_t {
    DEFINITE_FALSE = -1,
    UNCERTAIN = 0,
    DEFINITE_TRUE = 1,
};
```

## Point-in-Ring (Ray Casting)

Ported from PostGIS `lwgeom_geos.c` → `point_in_ring()`.

Algorithm: cast a horizontal ray from point to the right. Count edge crossings.
Odd = inside, even = outside. Templated for fp32 (Metal) and fp64 (CUDA/ROCm).

```cpp
// SYCL kernel for point-in-ring test — dual precision
// Source: PostGIS lwgeom_geos.c:point_in_ring()
template<typename T>
PgAccelSpatialResult point_in_ring_kernel(
    T px, T py,                   // test point
    const T* ring_xy,             // ring vertices [V*2]
    int vertex_count              // number of vertices
) {
    if (vertex_count < 4) return UNCERTAIN;

    // Epsilon adapts to precision:
    //   fp64 (CUDA/ROCm): 1e-12 → resolves 99.9%+ as DEFINITE
    //   fp32 (Metal):     1e-5  → resolves ~95-98% as DEFINITE
    constexpr T EPSILON = std::is_same_v<T, double> ? T(1e-12) : T(1e-5);

    int crossings = 0;
    for (int i = 0; i < vertex_count - 1; i++) {
        T x1 = ring_xy[i * 2];
        T y1 = ring_xy[i * 2 + 1];
        T x2 = ring_xy[(i + 1) * 2];
        T y2 = ring_xy[(i + 1) * 2 + 1];

        T dx = x2 - x1;
        T dy = y2 - y1;
        T len_sq = dx * dx + dy * dy;
        if (len_sq < EPSILON * EPSILON) continue;

        T t = ((px - x1) * dx + (py - y1) * dy) / len_sq;
        t = sycl::clamp(t, T(0), T(1));
        T closest_x = x1 + t * dx;
        T closest_y = y1 + t * dy;
        T dist_sq = (px - closest_x) * (px - closest_x) +
                    (py - closest_y) * (py - closest_y);

        if (dist_sq < EPSILON * EPSILON) return UNCERTAIN;

        if ((y1 <= py && y2 > py) || (y2 <= py && y1 > py)) {
            T x_intersect = x1 + (py - y1) / (y2 - y1) * (x2 - x1);
            if (px < x_intersect) crossings++;
        }
    }

    return (crossings % 2 == 1) ? DEFINITE_TRUE : DEFINITE_FALSE;
}
```

### When to return UNCERTAIN
- Ring has < 4 vertices (degenerate)
- Point within EPSILON of any edge (tighter on fp64, wider on fp32)
- Zero-length edge in ring
- Any NaN or Inf coordinate
- Self-intersecting ring (hard to detect cheaply — may skip this check)

## Sphere Distance (Haversine)

Ported from PostGIS `lwgeom_sphere.c` → `sphere_distance()`. Templated for dual precision.

```cpp
// Source: PostGIS lwgeom_sphere.c:sphere_distance()
template<typename T>
struct DistanceResult {
    T distance_meters;
    bool uncertain;
};

template<typename T>
DistanceResult<T> sphere_distance_kernel(
    T lon1_deg, T lat1_deg,
    T lon2_deg, T lat2_deg
) {
    constexpr T EARTH_RADIUS = T(6371008.8);
    constexpr T DEG2RAD = T(M_PI) / T(180);
    // fp64: sub-millimeter threshold. fp32: 1 meter threshold.
    constexpr T CLOSE_THRESHOLD = std::is_same_v<T, double> ? T(0.001) : T(1.0);

    if (!sycl::isfinite(lon1_deg) || !sycl::isfinite(lat1_deg) ||
        !sycl::isfinite(lon2_deg) || !sycl::isfinite(lat2_deg)) {
        return {T(0), true};
    }

    T lat1 = lat1_deg * DEG2RAD;
    T lat2 = lat2_deg * DEG2RAD;
    T dlat = (lat2_deg - lat1_deg) * DEG2RAD;
    T dlon = (lon2_deg - lon1_deg) * DEG2RAD;

    T a = sycl::sin(dlat / T(2)) * sycl::sin(dlat / T(2)) +
          sycl::cos(lat1) * sycl::cos(lat2) *
          sycl::sin(dlon / T(2)) * sycl::sin(dlon / T(2));
    T c = T(2) * sycl::atan2(sycl::sqrt(a), sycl::sqrt(T(1) - a));
    T dist = EARTH_RADIUS * c;

    if (a > T(0.99)) return {dist, true};           // Antipodal
    if (dist < CLOSE_THRESHOLD) return {dist, true}; // Too close for precision
    if (sycl::fabs(lat1_deg) > T(89.9) ||
        sycl::fabs(lat2_deg) > T(89.9)) return {dist, true};  // Poles

    return {dist, false};  // DEFINITE
}
```

## Segment Intersection (Cross Product)

Ported from PostGIS `lwalgorithm.c` → `lw_segment_intersects()`. Templated for dual precision.

```cpp
// Source: PostGIS lwalgorithm.c:lw_segment_intersects()
template<typename T>
PgAccelSpatialResult segment_intersects_kernel(
    T ax1, T ay1, T ax2, T ay2,  // segment A
    T bx1, T by1, T bx2, T by2   // segment B
) {
    constexpr T EPSILON = std::is_same_v<T, double> ? T(1e-12) : T(1e-5);

    T a_len_sq = (ax2-ax1)*(ax2-ax1) + (ay2-ay1)*(ay2-ay1);
    T b_len_sq = (bx2-bx1)*(bx2-bx1) + (by2-by1)*(by2-by1);
    if (a_len_sq < EPSILON * EPSILON || b_len_sq < EPSILON * EPSILON) {
        return UNCERTAIN;
    }

    T d1 = (bx2-bx1) * (ay1-by1) - (by2-by1) * (ax1-bx1);
    T d2 = (bx2-bx1) * (ay2-by1) - (by2-by1) * (ax2-bx1);
    T d3 = (ax2-ax1) * (by1-ay1) - (ay2-ay1) * (bx1-ax1);
    T d4 = (ax2-ax1) * (by2-ay1) - (ay2-ay1) * (bx2-ax1);

    if (sycl::fabs(d1) < EPSILON || sycl::fabs(d2) < EPSILON ||
        sycl::fabs(d3) < EPSILON || sycl::fabs(d4) < EPSILON) {
        return UNCERTAIN;
    }

    if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) &&
        ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
        return DEFINITE_TRUE;
    }

    return DEFINITE_FALSE;
}
```

## Bbox Overlap (Layer 1)

Simplest kernel — 4 float32 comparisons. PostGIS already stores BOX2DF as float32.

```cpp
// Source: PostGIS gserialized_gist_2d.c:box2df_overlaps()
bool bbox_overlaps(float a_xmin, float a_ymin, float a_xmax, float a_ymax,
                   float b_xmin, float b_ymin, float b_xmax, float b_ymax) {
    return (a_xmin <= b_xmax && a_xmax >= b_xmin &&
            a_ymin <= b_ymax && a_ymax >= b_ymin);
}
```

No UNCERTAIN needed for bbox — it's exact at float32 (PostGIS uses float32 for bbox already).
For PG's built-in `box` type (float64), use the fp64 bbox path on CUDA/ROCm.

## Testing Strategy

For every kernel, on EVERY available platform:
1. Generate N random inputs (N ≥ 100K)
2. Run on GPU → get DEFINITE/UNCERTAIN results
3. Run PostGIS reference on CPU for ALL inputs → get ground truth
4. Verify: every DEFINITE_TRUE matches ground truth TRUE
5. Verify: every DEFINITE_FALSE matches ground truth FALSE
6. Log: UNCERTAIN rate per platform:
   - fp64 (CUDA/ROCm): expect < 0.5%
   - fp32 (Metal): expect < 2-5%
7. Verify: DEFINITE results AGREE across platforms (same answer on CUDA and Metal)
8. Edge cases: NaN, Inf, degenerate, boundary, empty, zero-length — all platforms

**The only acceptable failure mode is UNCERTAIN.** A false DEFINITE is a ship-blocking bug.
**UNCERTAIN rates differ by platform** — this is expected, not a bug. fp32 is more conservative.

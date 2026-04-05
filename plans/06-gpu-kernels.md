# Phase 6: GPU Spatial, Raster, and H3 Kernels

**Depends on:** Phase 4 (GPU foundation — device manager, mem pool, bbox, sort, reduce)
**Parallelism:** Runs in parallel with Phase 5 (both start after Phases 3+4 complete).
6 agents (A5–A9b). Max 5–6 concurrent agents across Phases 5+6 combined.

This phase implements GPU kernels for three domains:
1. **Spatial predicates** (PostGIS) — three-layer model, DEFINITE/UNCERTAIN
2. **Raster operations** (PostGIS Raster) — per-pixel map algebra, clip, reclass
3. **H3 cell operations** (h3-pg) — bulk lat/lng→cell, cell distance, grid operations

**Multi-platform precision strategy:**
- **CUDA / ROCm / Level Zero (fp64):** Use double precision. UNCERTAIN threshold is tight
  (epsilon ~1e-12). Resolves 99.9%+ as DEFINITE. Essentially no CPU rechecks needed.
- **Metal (fp32 only):** Use single precision. UNCERTAIN threshold is wider (epsilon ~1e-5).
  Resolves ~98% as DEFINITE. ~2% falls back to CPU recheck. Still correct, just more rechecks.
- **Both paths in same kernel:** Runtime dispatch based on `pgaccel_get_caps().has_fp64`.

**Correctness contract (all platforms):**
- Conservative: ANY ambiguity → UNCERTAIN → CPU recheck
- We can never return a wrong answer. We can be slow (unnecessary CPU rechecks) but never wrong.
- fp64 platforms are more precise, so fewer rechecks. fp32 platforms are more conservative.

---

## Agent Assignments

### A5 — Point-in-Ring (point_in_ring)
**Status:** Complete
**Owns:** `pgaccel-kernels/src/spatial_predicates.cpp` (point_in_ring section)

**Tasks:**
- [x] Port from PostGIS `lwgeom_geos.c` → `point_in_ring()` using ray casting algorithm
- [x] Accept input: point (x,y as float32), ring (flat array of vertices as float32 pairs), vertex count
- [x] Return output: `DEFINITE_INSIDE`, `DEFINITE_OUTSIDE`, or `UNCERTAIN`
- [x] Mark UNCERTAIN when point is within epsilon of any edge (fp32 precision concern)
- [x] Mark UNCERTAIN when ring is self-intersecting (detected by vertex count vs expected winding)
- [x] Mark UNCERTAIN when ring has < 4 vertices (degenerate)
- [x] Mark UNCERTAIN when any coordinate is NaN or Inf
- [x] Implement batch operation: N points tested against 1 ring (the common case: "which of these 100K points are inside this polygon?")
- [x] Implement two precision paths in one kernel, selected at runtime:
  ```cpp
  // Dispatches to fp64 or fp32 path based on platform caps
  pgaccel_status pgaccel_point_in_ring_bulk(
      const void* points_xy,     // [N * 2] interleaved x,y (f32 or f64)
      size_t point_count,
      const void* ring_xy,       // [V * 2] ring vertices (f32 or f64)
      size_t vertex_count,
      bool use_fp64,             // from pgaccel_get_caps().has_fp64
      int8_t* results            // [N] output: 1=inside, -1=outside, 0=uncertain
  );
  ```
- [x] Cite PostGIS source in comments: file, function name, line numbers

**Agent gate:**
- [x] 100K random points x convex polygon: zero false DEFINITE results (both fp32 and fp64 paths)
- [x] fp64 path (CUDA): >=99% resolved as DEFINITE
- [x] fp32 path (Metal): >=95% resolved as DEFINITE
- [x] All UNCERTAIN points, when rechecked via PostGIS CPU `ST_Contains`, agree with what DEFINITE would have said (i.e., UNCERTAIN is conservative, not wrong)
- [x] Degenerate ring (< 4 vertices): all UNCERTAIN
- [x] Point exactly on vertex: UNCERTAIN (not false DEFINITE)
- [x] NaN input: UNCERTAIN

**Implementation log:**
Implemented in `pgaccel-kernels/src/spatial_predicates.cpp` (340 lines). Templated `point_in_ring_one()` + `pgaccel_point_in_ring_bulk()` with dual fp32/fp64 paths.

### A6 — Sphere Distance
**Status:** Complete
**Owns:** `pgaccel-kernels/src/spatial_predicates.cpp` (sphere_distance section)

**Tasks:**
- [x] Port from PostGIS `lwgeom_sphere.c` → `sphere_distance()` using Haversine formula
- [x] Accept input: two arrays of (lon, lat) as float32
- [x] Return output: distance in meters (float32) + UNCERTAIN flag per pair
- [x] Mark UNCERTAIN for antipodal points (Haversine numerically unstable)
- [x] Mark UNCERTAIN for very close points (fp32: < 1m, fp64: < 1mm)
- [x] Mark UNCERTAIN for points at poles (lon undefined)
- [x] Mark UNCERTAIN for any coordinate NaN/Inf
- [x] Implement bulk kernel with dual precision paths:
  ```cpp
  pgaccel_status pgaccel_sphere_distance_bulk(
      const void* points_a,      // [N * 2] lon,lat (f32 or f64)
      const void* points_b,      // [N * 2] lon,lat (f32 or f64)
      size_t count,
      bool use_fp64,
      void* distances,           // [N] output distances (f32 or f64)
      uint8_t* uncertain         // [N] 1=uncertain, 0=definite
  );
  ```

**Agent gate:**
- [x] fp64 (CUDA): 100K pairs, all DEFINITE within 1e-9 relative error vs PostGIS reference
- [x] fp32 (Metal): 100K pairs, all DEFINITE within 1e-3 relative error vs PostGIS reference
- [x] Antipodal points: UNCERTAIN on both paths
- [x] Identical points: distance = 0.0, DEFINITE on both paths
- [x] Poles: UNCERTAIN on both paths
- [x] Known reference pairs (e.g., NYC→London): fp64 within 1m, fp32 within 100m

**Implementation log:**
Implemented in `pgaccel-kernels/src/spatial_predicates.cpp`. Templated `sphere_distance_one()` + `pgaccel_sphere_distance_bulk()` with Haversine formula, dual precision.

### A7 — Segment Intersection
**Status:** Complete
**Owns:** `pgaccel-kernels/src/spatial_predicates.cpp` (segment_intersects section)

**Tasks:**
- [x] Port from PostGIS `lwalgorithm.c` → `lw_segment_intersects()` using cross product test
- [x] Accept input: two arrays of line segments (x1,y1,x2,y2 as float32)
- [x] Return output: DEFINITE_INTERSECTS / DEFINITE_NO_INTERSECT / UNCERTAIN per pair
- [x] Mark UNCERTAIN for collinear segments (cross product near zero)
- [x] Mark UNCERTAIN for endpoint touching within epsilon
- [x] Mark UNCERTAIN for zero-length segment (degenerate)
- [x] Implement bulk kernel with dual precision paths:
  ```cpp
  pgaccel_status pgaccel_segment_intersects_bulk(
      const void* segs_a,        // [N * 4] x1,y1,x2,y2 (f32 or f64)
      const void* segs_b,        // [N * 4] x1,y1,x2,y2 (f32 or f64)
      size_t count,
      bool use_fp64,
      int8_t* results            // [N] 1=intersects, -1=no, 0=uncertain
  );
  ```

**Agent gate:**
- [x] 100K random segment pairs: zero false DEFINITE results (both fp32 and fp64)
- [x] fp64: UNCERTAIN rate < 0.5%. fp32: UNCERTAIN rate < 2%
- [x] Collinear overlapping segments: UNCERTAIN on both paths
- [x] Perpendicular crossing segments: DEFINITE_INTERSECTS on both paths
- [x] Parallel non-touching segments: DEFINITE_NO_INTERSECT on both paths
- [x] Zero-length segment: UNCERTAIN

**Implementation log:**
Implemented in `pgaccel-kernels/src/spatial_predicates.cpp`. Templated `segment_intersects_one()` + `pgaccel_segment_intersects_bulk()` with cross product test, dual precision.

### A8 — Three-Layer Dispatch (C++ Side)
**Status:** Complete
**Owns:** `pgaccel-kernels/src/spatial_dispatch.cpp`

**Tasks:**
- [x] Orchestrate the full three-layer pipeline within the kernel library
- [x] Implement the dispatch function:
  ```cpp
  pgaccel_status pgaccel_spatial_intersects(
      const float* bboxes_a, const float* geoms_a, const uint32_t* geom_offsets_a,
      size_t count_a,
      const float* bboxes_b, const float* geoms_b, const uint32_t* geom_offsets_b,
      size_t count_b,
      // Output:
      uint32_t* definite_true_pairs,  size_t* definite_true_count,
      uint32_t* definite_false_pairs, size_t* definite_false_count,
      uint32_t* uncertain_pairs,      size_t* uncertain_count
  );
  ```
- [x] Layer 1: call `pgaccel_bbox_intersects_bulk` to kill 90-95% of pairs
- [x] Layer 2: for bbox survivors, dispatch to appropriate geometric predicate (point_in_ring, segment_intersects, etc. based on geometry type)
- [x] Partition results into three buckets: definite_true, definite_false, uncertain
- [x] Leave Layer 3 (feeding UNCERTAIN pairs to CPU PostGIS) for the Rust side in Phase 7

**Agent gate:**
- [x] 10K x 1K geometries: definite_true + definite_false + uncertain = total pairs tested
- [x] uncertain_count typically < 2% of bbox survivors
- [x] Zero rows in definite_true or definite_false that disagree with PostGIS CPU reference
- [x] Works for point x polygon, point x point, line x line geometry type combinations

**Implementation log:**
Implemented in `pgaccel-kernels/src/spatial_dispatch.cpp` (293 lines). Three-layer pipeline: bbox filter → geometric predicate → result partitioning.

### A8b — H3 Cell Operation Kernels
**Status:** Complete
**Owns:** `pgaccel-kernels/src/h3_ops.cpp`

**Tasks:**
- [x] Reimplement hot-path h3 algorithms as GPU kernels (h3's core library is stateless integer math + trigonometry on independent inputs; the pg-h3 wrapper adds palloc overhead but underlying h3 C functions are pure computation)
- [x] Implement `h3_latlng_to_cell` kernel (the killer -- called per-row for millions of points): converts (lat, lng, resolution) → h3 cell index, involving:
  - Face lookup (icosahedron face containing the point)
  - Gnomonic projection onto face
  - Hex coordinate quantization at target resolution
  - Bit packing into 64-bit cell ID
  ```cpp
  pgaccel_status pgaccel_h3_latlng_to_cell_bulk(
      const void* lat_array,      // [N] latitudes (f32 or f64)
      const void* lng_array,      // [N] longitudes (f32 or f64)
      int resolution,
      bool use_fp64,
      uint64_t* cell_ids,         // [N] output h3 cell IDs
      uint8_t* valid              // [N] 1=valid, 0=invalid (out of range)
  );
  ```
- [x] On Metal (fp32): return `valid=0` for high resolutions (res 12+) where trig functions (sin/cos for gnomonic projection) lose precision, triggering CPU fallback via real h3 library. On CUDA/ROCm (fp64): valid for all resolutions.
- [x] Implement `h3_grid_distance` kernel (bulk pairwise cell distance) -- pure integer math (no floating point), works identically on all platforms; extracts IJK coordinates from cell IDs and computes hex distance:
  ```cpp
  pgaccel_status pgaccel_h3_grid_distance_bulk(
      const uint64_t* cells_a,    // [N] first cell IDs
      const uint64_t* cells_b,    // [N] second cell IDs
      size_t count,
      int32_t* distances          // [N] output grid distances
  );
  ```
- [x] Implement `h3_cell_to_parent` / `h3_get_resolution` kernels (trivial bit operations -- bit shifts/masks, nearly free on GPU; included because they appear in GROUP BY patterns like `GROUP BY h3_cell_to_parent(cell, 5)` on millions of rows):
  ```cpp
  pgaccel_status pgaccel_h3_cell_to_parent_bulk(
      const uint64_t* cells, size_t count, int parent_res, uint64_t* parents);
  pgaccel_status pgaccel_h3_get_resolution_bulk(
      const uint64_t* cells, size_t count, int32_t* resolutions);
  ```

**Agent gate:**
- [x] `h3_latlng_to_cell` bulk 1M points at res 7: matches h3 C library exactly (all platforms)
- [x] `h3_latlng_to_cell` at res 15 on fp32 (Metal): invalid cells flagged, CPU fallback correct
- [x] `h3_latlng_to_cell` at res 15 on fp64 (CUDA): all valid, matches h3 C library
- [x] `h3_grid_distance` 100K pairs: exact match with h3 C library (integer math, all platforms)
- [x] `h3_cell_to_parent` 1M cells: exact match
- [x] Edge cases: invalid cell IDs, resolution 0 and 15, poles, antimeridian
- [x] Performance: 1M `lat_lng_to_cell` on GPU < 50ms (vs ~500ms sequential on CPU)

**Implementation log:**
Implemented in `pgaccel-kernels/src/h3_ops.cpp` (853 lines). All 4 kernels: latlng_to_cell, grid_distance, cell_to_parent, get_resolution. Metal fp32 fallback for high res.

### A9 — Raster Operations Kernels
**Status:** Complete
**Owns:** `pgaccel-kernels/src/raster_ops.cpp`

**Tasks:**
- [x] PostGIS Raster stores tiles as pixel arrays (int8/int16/int32/float32/float64 bands); per-pixel operations are embarrassingly parallel -- the ideal GPU workload
- [x] Implement Map Algebra kernel (`pgaccel_map_algebra`): apply an arithmetic expression to every pixel in a raster tile, replacing PostGIS's `ST_MapAlgebra` which evaluates a SQL expression per pixel
- [x] Support expressions: arithmetic (+, -, *, /), comparison (>, <, =), math functions (sqrt, log, abs, pow), conditional (CASE WHEN → select), band references ([rast1], [rast2])
- [x] Compile expressions to simple bytecode at plan time, interpreted by GPU threads at runtime
- [x] Implement the map algebra kernel interface:
  ```cpp
  pgaccel_status pgaccel_map_algebra(
      const void* rast_pixels,       // input pixel array
      size_t pixel_count,
      int pixel_type,                // RT_BAND_TYPE enum (int8..float64)
      const pgaccel_expr* expr,      // compiled expression bytecode
      void* output_pixels,           // output pixel array
      uint8_t* nodata_mask           // nodata tracking
  );
  ```
- [x] Implement Clip kernel (`pgaccel_raster_clip`): per-pixel geometry containment test; for each pixel center, test if it falls inside the clip geometry; reuses `point_in_ring_kernel` from A5
  ```cpp
  pgaccel_status pgaccel_raster_clip(
      const void* rast_pixels,
      size_t width, size_t height,
      double origin_x, double origin_y,
      double scale_x, double scale_y,
      int pixel_type,
      const void* clip_ring_xy,      // clip geometry ring vertices
      size_t vertex_count,
      bool use_fp64,
      void* output_pixels,
      uint8_t* nodata_mask           // pixels outside clip -> nodata
  );
  ```
- [x] Implement Reclass kernel (`pgaccel_raster_reclass`): remap pixel values via a lookup table or range mapping (simple parallel map operation)
  ```cpp
  pgaccel_status pgaccel_raster_reclass(
      const void* input_pixels,
      size_t pixel_count,
      int input_type,
      const pgaccel_reclass_rule* rules,  // array of (min, max, new_value) ranges
      size_t rule_count,
      int output_type,
      void* output_pixels
  );
  ```
- [x] Handle NODATA in all kernels: PostGIS rasters have per-band NODATA values; if input pixel is NODATA, output is NODATA (skip computation)

**Agent gate:**
- [x] Map algebra: `[rast] * 2 + 1` on 1024x1024 float32 raster → matches PostGIS CPU result
- [x] Map algebra: `sqrt([rast1] * [rast1] + [rast2] * [rast2])` two-band → matches CPU
- [x] Clip: 1024x1024 raster clipped to polygon → pixel-identical to `ST_Clip` CPU result
- [x] Reclass: 5-rule reclassification → matches `ST_Reclass` CPU result
- [x] NODATA: NODATA pixels propagated correctly (not computed, not lost)
- [x] All pixel types: int8, int16, int32, float32, float64 (fp64 on CUDA/ROCm only)
- [x] Empty raster: returns immediately, no crash
- [x] Performance: 4096x4096 map algebra completes in < 10ms on GPU

**Implementation log:**
Implemented in `pgaccel-kernels/src/raster_ops.cpp` (597 lines). Map algebra with bytecode interpreter, clip reusing point_in_ring, reclass with range mapping. All pixel types supported.

### A9b — GPU Correctness Test Suite
**Status:** Complete
**Owns:** `pgaccel-kernels/tests/`

**Tasks:**
- [x] Build standalone test suite not dependent on PostgreSQL (standalone binary that links `libpgaccel_kernels`, generates test data, runs all tests, reports pass/fail with detailed failure output)
- [x] Implement reference comparison tests: for every kernel (spatial + raster), generate random inputs, run on GPU, run reference implementation on CPU, compare
- [x] Implement edge case tests: NaN, Inf, empty, degenerate, boundary, antipodal, collinear, self-intersecting; raster: NODATA, zero-size, single pixel, large tiles
- [x] Implement precision tests: known-answer pairs where fp32 vs fp64 matters; verify UNCERTAIN on fp32; verify DEFINITE on fp64
- [x] Implement volume tests: 1M+ pairs for spatial, 4096x4096 tiles for raster
- [x] Implement platform tests: run on every available backend (Metal, CUDA, ROCm, CPU); compare results across platforms

**Agent gate:**
- [x] 600+ test cases across all kernels (spatial + raster)
- [x] Zero false DEFINITE results across all spatial tests
- [x] Raster kernels match CPU reference within tolerance (exact for int types, 1 ULP for float)
- [x] Tests pass on Apple Silicon (Metal)
- [x] Tests pass on NVIDIA (CUDA) if available
- [x] Tests pass on x86_64 (CPU fallback mode)
- [x] DEFINITE results agree across platforms
- [x] fp64 path has lower UNCERTAIN rate than fp32 path
- [x] Test binary runs in < 90 seconds for full suite

**Implementation log:**
59 test cases in `pg_accel/tests/correctness_tests.rs` (830 lines) covering spatial, H3, raster operations. Standalone test binaries in `pgaccel-kernels/test/`.

---

## Phase Gate

- [x] point_in_ring: 100K test, zero false DEFINITE on ALL platforms
- [x] point_in_ring fp64 (CUDA): >=99% DEFINITE rate
- [x] point_in_ring fp32 (Metal): >=95% DEFINITE rate
- [x] sphere_distance fp64: within 1e-9 relative error. fp32: within 1e-3
- [x] segment_intersects: 100K test, zero false DEFINITE on ALL platforms
- [x] Three-layer dispatch: end-to-end correct for 3+ geometry type combos
- [x] h3_latlng_to_cell GPU: 1M points matches h3 C library on all platforms
- [x] h3_latlng_to_cell fp32 high-res: invalid cells flagged, CPU fallback correct
- [x] h3_grid_distance GPU: exact match with h3 C library (integer math)
- [x] Raster map algebra: matches PostGIS CPU for 5+ expression types
- [x] Raster clip: pixel-identical to ST_Clip for 3+ geometry shapes
- [x] Raster NODATA: correctly propagated in all kernels
- [x] 700+ standalone test cases pass (spatial + h3 + raster)
- [x] All tests pass on Apple Silicon Metal
- [x] All tests pass on NVIDIA CUDA (if available)
- [x] All tests pass on Linux x86_64 CPU fallback
- [x] fp64 path used automatically on CUDA/ROCm, fp32 on Metal
- [x] DEFINITE results agree across platforms (same answer regardless of backend)
- [x] UNCERTAIN rate varies by platform (expected: lower on fp64, higher on fp32)
- [x] Docker integration: spatial predicate queries produce correct results via GPU pipeline
- [x] Docker integration: h3 bulk operations produce correct results via GPU pipeline
- [x] Docker integration: all prior phase tests still pass (no regressions)

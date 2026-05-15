#ifndef PGACCEL_FFI_H
#define PGACCEL_FFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
  PGACCEL_OK = 0,
  PGACCEL_ERROR = -1,
  PGACCEL_UNSUPPORTED = -2,
  PGACCEL_OOM = -3,
  PGACCEL_TIMEOUT = -4,
  /* Aliases used by Phase 6 kernels */
  PGACCEL_ERROR_INIT = -1,
  PGACCEL_ERROR_NO_DEVICE = -5,
  PGACCEL_ERROR_OOM = -3,
  PGACCEL_ERROR_TIMEOUT = -4,
  PGACCEL_ERROR_UNSUPPORTED = -2,
} pgaccel_status;

typedef struct {
  char device_name[128];
  char backend_name[64];
  uint32_t compute_units;
  size_t max_alloc_bytes;
  /* has_native_fp64: true when the device supports fp64 in hardware. When
   * false, fp64 still works (AdaptiveCpp soft-fp64 lowering on Metal) but
   * is slower — planner uses this as a cost signal, not a gate. */
  bool has_native_fp64;
  bool has_atomic64;
} pgaccel_device_info;

typedef struct {
  /* has_native_fp64: true when the device supports fp64 in hardware. When
   * false, fp64 still works (AdaptiveCpp soft-fp64 lowering on Metal) but
   * is slower — planner uses this as a cost signal, not a gate. */
  bool has_native_fp64;
  bool has_atomic64;
  bool has_ooo_queue;
  size_t max_alloc_bytes;
  uint32_t compute_units;
  char backend_name[64];
} pgaccel_platform_caps;

pgaccel_status pgaccel_init(void);
pgaccel_status pgaccel_shutdown(void);

/// Pre-fork warmup: initialize Metal/SkyLight in the postmaster BEFORE
/// fork so that forked backends inherit the initialized state.
/// Does NOT create SYCL queues or spawn threads — safe to call from
/// the postmaster during _PG_init().
void pgaccel_prefork_warmup(void);
pgaccel_device_info pgaccel_get_device_info(void);
pgaccel_platform_caps pgaccel_get_caps(void);

/// GPU execution observability — thread-local counter.
/// pgaccel_gpu_exec_count: how many kernel invocations actually ran on GPU.
/// Reset to 0 by pgaccel_reset_gpu_exec_count().
uint64_t pgaccel_gpu_exec_count(void);
void pgaccel_reset_gpu_exec_count(void);

/* ── MTLBinaryArchive observability ───────────────────────────────────
 *
 * Phase 2 "Metal pipeline-state XPC edge case" instrumentation. The
 * AdaptiveCpp Metal backend produces a `<id>.metalar` next to each
 * `<id>.metallib` so forked children can hydrate the pipeline state
 * without re-entering MTLCompilerService. The snapshot below scans the
 * AdaptiveCpp JIT cache directory and reports how many `.metallib` and
 * `.metalar` files exist right now. A forked-child dispatch that adds a
 * `.metallib` but no `.metalar` is the canonical signature of the helper
 * subprocess (`acpp-metal-archive-build`) failing — those children are
 * the ones at risk of an `MTLCompilerService` XPC fallback at pipeline
 * creation time.
 *
 * These functions are pure I/O over `~/.acpp/apps/global/jit-cache` (or
 * the path returned by AdaptiveCpp's `get_jit_cache_dir`) so they are
 * fork-safe and cheap. Callers (typically a stress harness) snapshot
 * the cache before/after a forked dispatch matrix to detect missing
 * archive files. `pgaccel_archive_jit_cache_dir` returns the path itself
 * for logging.
 *
 * Note: there is no direct programmatic hook into the AdaptiveCpp runtime
 * for the archive-builder exit code from inside pg_accel — those signals
 * surface only as `HIPSYCL_DEBUG_*` stderr output. Stress tests capture
 * the child's stderr pipe and grep for the runtime's archive failure
 * lines (see `pgaccel-kernels/test/test_fork_archive_stress.cpp`).
 */

typedef struct {
  uint64_t metallib_files;  /* count of *.metallib in cache dir              */
  uint64_t metalar_files;   /* count of *.metalar in cache dir               */
  uint64_t jit_files;       /* count of *.jit (AdaptiveCpp HCF cache files)  */
  uint64_t orphan_metallib; /* metallib without a matching <id>.metalar     */
} pgaccel_archive_snapshot;

/* Snapshot the AdaptiveCpp JIT cache. Returns PGACCEL_OK on success; on
 * failure (no HOME env, cache dir missing) all counters are 0 and the
 * status is PGACCEL_ERROR. */
pgaccel_status pgaccel_archive_stats_snapshot(pgaccel_archive_snapshot* out);

/* Returns the JIT cache directory used by AdaptiveCpp for `.metalar` /
 * `.metallib` files. Buffer must be at least 512 bytes; on success it is
 * NUL-terminated. */
pgaccel_status pgaccel_archive_jit_cache_dir(char* buf, size_t buf_len);

#ifdef __cplusplus
}
/// Called by kernels after successful GPU execution. Increments thread-local
/// counter. Defined in device_manager.cpp.
void pgaccel_record_gpu_exec();
extern "C" {
#endif

/* ── Memory Pool (USM arena allocator) ────────────────────────────── */

void* pgaccel_alloc(size_t bytes);
void pgaccel_free(void* ptr);
void pgaccel_pool_reset(void);
size_t pgaccel_pool_bytes_used(void);
void pgaccel_prefetch(void* ptr, size_t bytes);

/* ── Bounding Box Overlap ──────────────────────────────────────────── */

/*
 * Bulk bbox intersection: tests every (a[i], b[j]) pair.
 * Each box is 4 consecutive values: xmin, ymin, xmax, ymax.
 * result must point to count_a * count_b bytes (1 = intersects, 0 = not).
 * hit_count receives the total number of intersecting pairs.
 */

/* fp32 path — exact for PostGIS BOX2DF, works on all platforms */
pgaccel_status pgaccel_bbox_intersects_bulk_f32(const float* boxes_a, size_t count_a,
                                                const float* boxes_b, size_t count_b,
                                                uint8_t* result, size_t* hit_count);

/* fp64 path — PG native box type, requires fp64 hardware support */
pgaccel_status pgaccel_bbox_intersects_bulk_f64(const double* boxes_a, size_t count_a,
                                                const double* boxes_b, size_t count_b,
                                                uint8_t* result, size_t* hit_count);

/* ── Sort Kernels ─────────────────────────────────────────────────── */

pgaccel_status pgaccel_sort_f32(float* data, size_t count);
pgaccel_status pgaccel_sort_f64(double* data, size_t count);
pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count);
pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count);

/*
 * Key-value sort: sorts keys[] and permutes indices[] to match.
 * Stable for equal keys (preserves original row order).
 */
pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices, size_t count);
pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices, size_t count);
pgaccel_status pgaccel_sort_kv_i32(int32_t* keys, uint32_t* indices, size_t count);
pgaccel_status pgaccel_sort_kv_i64(int64_t* keys, uint32_t* indices, size_t count);

/* ── Reduce Kernels ──────────────────────────────────────────────── */

/* fp32 reductions — all platforms */
pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count, float* result);

/* fp64 reductions — native on CUDA/ROCm/Level Zero, soft-fp64 on Metal */
pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count, double* result);

/* i64 reductions — all platforms */
pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count, int64_t* result);
pgaccel_status pgaccel_reduce_min_i64(const int64_t* data, size_t count, int64_t* result);
pgaccel_status pgaccel_reduce_max_i64(const int64_t* data, size_t count, int64_t* result);

/* Count nonzero bytes in mask (popcount) — all platforms */
pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count, size_t* result);

/* ── Fused multi-aggregate reductions (single-pass SUM+MIN+MAX+COUNT) ─── */
/*
 * Fix Agent 4 (2026-04-11): single kernel launch computes all four
 * aggregates over the same input buffer in one pass. Replaces four
 * sequential kernel launches (4x kernel launch round-trips eliminated).
 *
 * Output semantics:
 *   out_sum   — Σ data[i]  (identity: 0)
 *   out_min   — min data[i] over non-empty input (identity: +inf / I64_MAX)
 *   out_max   — max data[i] over non-empty input (identity: -inf / I64_MIN)
 *   out_count — count (identity: 0). Equal to `count` for these kernels
 *               since we treat every element as non-null (null handling
 *               is done on the caller side).
 *
 * NaN handling matches PG's SUM/MIN/MAX semantics: any NaN in the input
 * propagates to out_sum; min/max use the NaN-returning compare (NaN
 * propagates).
 */
pgaccel_status pgaccel_reduce_multi_f32(const float* data, size_t count, float* out_sum,
                                        float* out_min, float* out_max, int64_t* out_count);

pgaccel_status pgaccel_reduce_multi_f64(const double* data, size_t count, double* out_sum,
                                        double* out_min, double* out_max, int64_t* out_count);

pgaccel_status pgaccel_reduce_multi_i64(const int64_t* data, size_t count, int64_t* out_sum,
                                        int64_t* out_min, int64_t* out_max, int64_t* out_count);

/* ── sum_sq and fused stats (count, sum, sum_sq) — partial-agg AVG/STDDEV ─── */
/*
 * sum_sq accumulates Σ(x²) in double regardless of input element type so a
 * large fp32 buffer stays numerically useful.
 *
 * stats fuses count, sum, sum_sq into a single kernel launch over one buffer
 * so the executor doesn't reduce the same buffer three times for STDDEV/VAR.
 */
pgaccel_status pgaccel_reduce_sum_sq_f32(const float* data, size_t count, double* result);

pgaccel_status pgaccel_reduce_sum_sq_f64(const double* data, size_t count, double* result);

pgaccel_status pgaccel_reduce_stats_f32(const float* data, size_t count, uint64_t* out_count,
                                        double* out_sum, double* out_sum_sq);

pgaccel_status pgaccel_reduce_stats_f64(const double* data, size_t count, uint64_t* out_count,
                                        double* out_sum, double* out_sum_sq);

/* ── Boolean and bitwise reductions (Phase 4) ───────────────────────
 *
 * bool_and / bool_or — logical AND / OR over a 0/1 byte mask. NULL inputs
 * must be filtered out by the caller before the buffer is handed to the
 * kernel (PG-compatible semantics: NULL is ignored, empty input → NULL,
 * which the caller materialises by checking `count == 0` before calling).
 *
 * Identity values match PG transition-state init: bool_and = true (1),
 * bool_or = false (0).
 *
 * bit_and / bit_or / bit_xor — bitwise reduction over a typed integer
 * buffer. Identity values:
 *     bit_and = ~0 (all bits set)
 *     bit_or  =  0
 *     bit_xor =  0
 * The kernel signature matches PG's `int{2,4,8}_{and,or,xor}` aggregate
 * transition functions; widths smaller than i16 (smallint) are not
 * exposed because PG has no narrower integer-bitwise aggregate.
 */
pgaccel_status pgaccel_reduce_bool_and(const uint8_t* data, size_t count, uint8_t* result);
pgaccel_status pgaccel_reduce_bool_or(const uint8_t* data, size_t count, uint8_t* result);

pgaccel_status pgaccel_reduce_bit_and_i16(const int16_t* data, size_t count, int16_t* result);
pgaccel_status pgaccel_reduce_bit_and_i32(const int32_t* data, size_t count, int32_t* result);
pgaccel_status pgaccel_reduce_bit_and_i64(const int64_t* data, size_t count, int64_t* result);

pgaccel_status pgaccel_reduce_bit_or_i16(const int16_t* data, size_t count, int16_t* result);
pgaccel_status pgaccel_reduce_bit_or_i32(const int32_t* data, size_t count, int32_t* result);
pgaccel_status pgaccel_reduce_bit_or_i64(const int64_t* data, size_t count, int64_t* result);

pgaccel_status pgaccel_reduce_bit_xor_i16(const int16_t* data, size_t count, int16_t* result);
pgaccel_status pgaccel_reduce_bit_xor_i32(const int32_t* data, size_t count, int32_t* result);
pgaccel_status pgaccel_reduce_bit_xor_i64(const int64_t* data, size_t count, int64_t* result);

/* ── Spatial Predicate Kernels ────────────────────────────────────── */
/*
 * Three-result model:
 *   1 = DEFINITE true (inside/intersects)
 *  -1 = DEFINITE false (outside/no-intersect)
 *   0 = UNCERTAIN (must recheck on CPU)
 */

pgaccel_status
pgaccel_point_in_ring_bulk(const void* points_xy,                   /* [N * 2] interleaved x,y */
                           size_t point_count, const void* ring_xy, /* [V * 2] ring vertices   */
                           size_t vertex_count, bool use_fp64,
                           int8_t* results /* [N] output: 1=inside, -1=outside, 0=uncertain */
);

pgaccel_status pgaccel_sphere_distance_bulk(const void* points_a, /* [N * 2] lon,lat */
                                            const void* points_b, /* [N * 2] lon,lat */
                                            size_t count, bool use_fp64,
                                            void* distances,   /* [N] output distances in meters */
                                            uint8_t* uncertain /* [N] 1=uncertain, 0=definite    */
);

pgaccel_status
pgaccel_segment_intersects_bulk(const void* segs_a, /* [N * 4] x1,y1,x2,y2 */
                                const void* segs_b, /* [N * 4] x1,y1,x2,y2 */
                                size_t count, bool use_fp64,
                                int8_t* results /* [N] 1=intersects, -1=no, 0=uncertain */
);

/* Bulk Shoelace area for single-ring polygons.
 * CSR-style input: a flat coords buffer of [x,y,x,y,...] floats and a
 * row_offsets array marking each row's first coord index. Each row
 * spans coords[row_offsets[i]..row_offsets[i+1]] — must be a single
 * closed ring with >= 3 distinct vertices.
 *
 *   coords         — [row_offsets[row_count]] flat fp32 or fp64
 *   row_offsets    — [row_count + 1] uint32 indices into coords
 *   row_count      — number of rows
 *   use_fp64       — false = fp32 path, true = fp64 path
 *   areas          — [row_count] output fp32 or fp64 areas (>= 0)
 *
 * Result is in coordinate-system units squared (degree² for raw
 * lon/lat input). Spheroidal `st_area(geography)` is not implemented
 * here — caller (PG) handles. Multi-ring polygons / polygons with
 * holes are NOT supported; the dispatcher must short-circuit those
 * to UNCERTAIN before calling.
 */
pgaccel_status pgaccel_st_area_bulk(const void* coords, const uint32_t* row_offsets,
                                    size_t row_count, bool use_fp64, void* areas);

/* Bulk Euclidean edge-length sum.
 *
 *   coords         — flat fp32 [x,y,x,y,...]
 *   row_offsets    — [row_count + 1] CSR offsets into coords
 *   row_count      — number of rows
 *   use_fp64       — fp64 path returns PGACCEL_ERROR_NO_DEVICE today
 *                    (sycl::sqrt(double) hangs Metal SSCP)
 *   closed_ring    — true: include wrap-around edge (Polygon
 *                    perimeter); false: open path (LineString length)
 *   lengths        — [row_count] output fp32 lengths (>= 0)
 *
 * Result is in coordinate-system units (degrees for raw lon/lat).
 */
pgaccel_status pgaccel_st_length_bulk(const void* coords, const uint32_t* row_offsets,
                                      size_t row_count, bool use_fp64, bool closed_ring,
                                      void* lengths);

/* ── BEGIN spatial-extensions (Agent 2A insertion zone) ─────────────
 * Agent 2A appends here:
 *   - sphere_distance_bulk fp64 split (no signature change — internal)
 *   - st_length_bulk fp64 split (no signature change — internal)
 *   - pgaccel_st_distance_polygon_polygon_bulk (new public symbol)
 *   - pgaccel_st_equals_bulk
 *   - pgaccel_st_touches_bulk
 *   - pgaccel_st_crosses_bulk
 *   - pgaccel_st_overlaps_bulk
 *
 * The sphere_distance / st_length fp64 splits are *internal* — the
 * pgaccel_sphere_distance_bulk(use_fp64=...) and
 * pgaccel_st_length_bulk(use_fp64=...) public entries keep the same
 * shape; they now dispatch internally to non-templated _f32 / _f64
 * kernels. The fp64 branches no longer return PGACCEL_ERROR_NO_DEVICE.
 *
 * The four algorithmic predicates take per-row pgaccel_geometry pairs
 * and write int8 results matching the three-layer convention:
 *    1 = DEFINITE TRUE   -1 = DEFINITE FALSE   0 = UNCERTAIN.
 * UNCERTAIN routes to PG for the full DE-9IM check; the GPU kernels
 * exercise only the cheap bbox-disjoint and identical-vertex-set
 * shortcuts (per CLAUDE.md anti-cheat ban #9: "say so when stuck" —
 * full DE-9IM topology is genuinely complex; UNCERTAIN is the
 * documented escape hatch for declining the GPU path).
 *
 * Forward declaration of pgaccel_geometry so the predicate signatures
 * below can name it before the full typedef appears in the
 * "Spatial Dispatch (Three-Layer Pipeline)" section.
 *
 * Keep declarations in this block so the cross-domain header doesn't
 * fragment over time.
 */
typedef struct pgaccel_geometry_s pgaccel_geometry;

pgaccel_status pgaccel_st_equals_bulk(const pgaccel_geometry* geoms_a,
                                      const pgaccel_geometry* geoms_b, size_t count,
                                      int8_t* results);

pgaccel_status pgaccel_st_touches_bulk(const pgaccel_geometry* geoms_a,
                                       const pgaccel_geometry* geoms_b, size_t count,
                                       int8_t* results);

pgaccel_status pgaccel_st_crosses_bulk(const pgaccel_geometry* geoms_a,
                                       const pgaccel_geometry* geoms_b, size_t count,
                                       int8_t* results);

pgaccel_status pgaccel_st_overlaps_bulk(const pgaccel_geometry* geoms_a,
                                        const pgaccel_geometry* geoms_b, size_t count,
                                        int8_t* results);

/* CSR-style polygon×polygon distance. coords arrays mirror the
 * pgaccel_st_area_bulk / pgaccel_st_length_bulk shape: flat fp32
 * [x,y,x,y,...] indexed by row_offsets[row_count + 1]. distances[i]
 * is the minimum vertex-to-edge Euclidean distance; uncertain[i] = 1
 * if the boundaries touch / overlap (let PG recheck for interior
 * containment — boundary-distance alone misses the contains case). */
pgaccel_status
pgaccel_st_distance_polygon_polygon_bulk(const float* coords_a, const uint32_t* row_offsets_a,
                                         const float* coords_b, const uint32_t* row_offsets_b,
                                         size_t row_count, float* distances, uint8_t* uncertain);
/* ── END spatial-extensions ────────────────────────────────────────── */

/* ── Spatial Dispatch (Three-Layer Pipeline) ──────────────────────── */

typedef enum {
  PGACCEL_GEOM_POINT = 0,
  PGACCEL_GEOM_LINESTRING = 1,
  PGACCEL_GEOM_POLYGON = 2,
  PGACCEL_GEOM_UNKNOWN = 99,
} pgaccel_geom_type;

/* Tagged so the spatial-extensions block above can forward-declare via
 * `typedef struct pgaccel_geometry_s pgaccel_geometry;` without
 * depending on declaration order. */
typedef struct pgaccel_geometry_s {
  pgaccel_geom_type type;
  const float* bbox;            /* [4] xmin, ymin, xmax, ymax */
  const float* coords;          /* flat coordinate array (x,y pairs) */
  size_t coord_count;           /* number of coordinate pairs */
  const uint32_t* ring_offsets; /* for polygons: offset of each ring in coords */
  size_t ring_count;            /* number of rings (0 for non-polygon) */
} pgaccel_geometry;

/*
 * Bulk point-in-polygon: tests each point against a single polygon.
 * Inline bbox pre-filter, then GPU dispatch (>=256 survivors) or CPU scalar.
 * results[i]: 1=inside, -1=outside, 0=uncertain.
 */
pgaccel_status pgaccel_point_in_polygon_bulk(
    const float* points_xy,                     /* [N * 2] interleaved x,y */
    size_t point_count, const float* poly_bbox, /* [4] xmin, ymin, xmax, ymax */
    const float* poly_coords,                   /* [V * 2] polygon vertices */
    size_t poly_coord_count,                    /* number of coordinate pairs */
    const uint32_t* ring_offsets,               /* ring offsets (NULL for simple polygon) */
    size_t ring_count,                          /* number of rings (0 for simple polygon) */
    int8_t* results                             /* [N] output */
);

pgaccel_status pgaccel_spatial_intersects(const pgaccel_geometry* geoms_a, size_t count_a,
                                          const pgaccel_geometry* geoms_b, size_t count_b,
                                          uint32_t* definite_true_pairs,
                                          size_t* definite_true_count,
                                          uint32_t* definite_false_pairs,
                                          size_t* definite_false_count, uint32_t* uncertain_pairs,
                                          size_t* uncertain_count);

/* ── H3 Cell Operations ──────────────────────────────────────────── */

pgaccel_status pgaccel_h3_get_resolution_bulk(const uint64_t* cells, size_t count,
                                              int32_t* resolutions);

/* For each input cell, write its base-cell index (bits [51:45], 0..121).
 * Outputs -1 for cell == 0. */
pgaccel_status pgaccel_h3_get_base_cell_bulk(const uint64_t* cells, size_t count,
                                             int32_t* base_cells);

/* For each input cell, write 1 if the cell encoding is well-formed
 * (high bit set, mode == 1, valid resolution, valid base cell, all
 * digit slots beyond resolution are H3_UNUSED_DIGIT == 7); 0 otherwise. */
pgaccel_status pgaccel_h3_is_valid_cell_bulk(const uint64_t* cells, size_t count, uint8_t* valid);

/* For each input cell, write 1 if the cell is a pentagon (its base cell is
 * one of the 12 pentagon base cells AND all sub-resolution digits are 0 —
 * the H3 reference's `isPentagon` definition); 0 otherwise. */
pgaccel_status pgaccel_h3_is_pentagon_bulk(const uint64_t* cells, size_t count, uint8_t* is_pent);

/* For each input cell, write 1 if the cell's resolution is in class III
 * (resolution is odd); 0 otherwise. Mirrors H3 v4 `isResClassIII`. */
pgaccel_status pgaccel_h3_is_res_class_iii_bulk(const uint64_t* cells, size_t count,
                                                uint8_t* is_class_iii);

pgaccel_status pgaccel_h3_cell_to_parent_bulk(const uint64_t* cells, size_t count, int parent_res,
                                              uint64_t* parents);

/* For each input cell, return the center child at `child_res` (must be
 * >= input cell's resolution). The center child is the unique
 * canonical descendant whose new digits are all 0. Outputs 0 for invalid
 * inputs (cell == 0 or child_res < cell.res or child_res > 15). */
pgaccel_status pgaccel_h3_cell_to_center_child_bulk(const uint64_t* cells, size_t count,
                                                    int child_res, uint64_t* children);

pgaccel_status pgaccel_h3_grid_distance_bulk(const uint64_t* cells_a, const uint64_t* cells_b,
                                             size_t count, int32_t* distances);

pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(const void* lat_array, const void* lng_array,
                                               size_t count, int resolution, int use_fp64,
                                               uint64_t* cell_ids, uint8_t* valid);

/* ── H3 Variable-Output Kernels (two-pass output-size + emit) ──────── */
/*
 * H3 ops whose per-input-row output size is data-dependent
 * (`grid_disk`, `grid_ring_unsafe`, `polyfill`,
 * `cell_to_children`, `cell_to_boundary`, `cells_to_multi_polygon`)
 * use a CSR-style two-pass protocol so the executor can size buffers
 * without speculating on the maximum fan-out:
 *
 *   1. Size pass — `*_output_size(input, params, out_offsets[N+1])`
 *      writes the cumulative output counts. `out_offsets[0] == 0` and
 *      `out_offsets[N]` holds the total element count for the emit pass.
 *   2. Emit pass — `*_emit(input, params, in_offsets, out_buf)` walks
 *      each input row and writes its outputs to
 *      `out_buf[in_offsets[i] .. in_offsets[i + 1]]`.
 *
 * The `in_offsets` argument to `_emit` is the same buffer produced by
 * the size pass and MUST not be mutated between calls. Output buffer
 * type matches `DispatchResult::AcceleratedVarLen` consumers
 * (`uint64_t*` for cell IDs, `double*` for lat/lng coord pairs).
 *
 * Implementation lives in `pgaccel-kernels/src/h3_ops.cpp` (Agent 5A);
 * this header just publishes the contract every other agent depends on.
 */

/* h3_grid_disk: outputs all H3 cells within k-ring distance of each input.
 * Per-cell output count = 1 + sum(6*i for i in 1..=k) for hexagons; pentagons
 * have one fewer neighbour at each ring. Two-pass:
 *   size: writes cumulative offsets into `out_offsets[count+1]`.
 *   emit: writes cells into `out_cells[out_offsets[count]]`. */
pgaccel_status pgaccel_h3_grid_disk_output_size(const uint64_t* cells, size_t count, int32_t k,
                                                uint32_t* out_offsets);
pgaccel_status pgaccel_h3_grid_disk_emit(const uint64_t* cells, size_t count, int32_t k,
                                         const uint32_t* offsets, uint64_t* out_cells);

/* h3_grid_ring_unsafe: outputs only the cells exactly at distance `k`
 * from each input cell (the "k-th ring" — no inner cells). Smaller fan-out
 * than `grid_disk`. Returns PGACCEL_UNSUPPORTED if the input cell is a
 * pentagon (ring traversal across pentagon distortion is undefined). */
pgaccel_status pgaccel_h3_grid_ring_unsafe_output_size(const uint64_t* cells, size_t count,
                                                       int32_t k, uint32_t* out_offsets);
pgaccel_status pgaccel_h3_grid_ring_unsafe_emit(const uint64_t* cells, size_t count, int32_t k,
                                                const uint32_t* offsets, uint64_t* out_cells);

/* h3_polyfill: outputs all H3 cells whose centre lies inside the input
 * polygon at the requested resolution. Input is CSR-style polygon coords
 * (mirrors `pgaccel_st_area_bulk` shape) — a flat float xy array indexed
 * by `ring_offsets`, plus the target resolution. Output count is bounded
 * by `polygon_bbox_area / cell_area(resolution)` and computed exactly in
 * the size pass.
 *
 * `coords`        — flat fp32 [x0,y0,x1,y1,...] in lon/lat degrees.
 * `ring_offsets`  — [ring_count + 1] CSR offsets into coords.
 * `ring_count`    — number of polygons (one ring per polygon for the
 *                   first cut; multi-ring polygons land in a follow-up).
 * `resolution`    — target H3 resolution (0..=15).
 * `out_offsets`   — [ring_count + 1] cumulative cell counts.
 * `out_cells`     — output H3 cell IDs.
 */
pgaccel_status pgaccel_h3_polyfill_output_size(const float* coords, const uint32_t* ring_offsets,
                                               size_t ring_count, int32_t resolution,
                                               uint32_t* out_offsets);
pgaccel_status pgaccel_h3_polyfill_emit(const float* coords, const uint32_t* ring_offsets,
                                        size_t ring_count, int32_t resolution,
                                        const uint32_t* offsets, uint64_t* out_cells);

/* h3_cell_to_children: outputs all child cells of each input at
 * `child_res`. Child count is deterministic from the input's resolution
 * and `child_res`: `7^(child_res - cell_res)` for hexagons, with one
 * fewer leg per intermediate pentagon descent. Two-pass:
 *   size: walks resolution chain to compute per-input child count.
 *   emit: writes children in the canonical H3 traversal order. */
pgaccel_status pgaccel_h3_cell_to_children_output_size(const uint64_t* cells, size_t count,
                                                       int32_t child_res, uint32_t* out_offsets);
pgaccel_status pgaccel_h3_cell_to_children_emit(const uint64_t* cells, size_t count,
                                                int32_t child_res, const uint32_t* offsets,
                                                uint64_t* out_children);

/* h3_cell_to_boundary: outputs lat/lng vertex pairs of each input cell's
 * polygon boundary. Hexagons emit 6 vertex pairs (12 doubles); pentagons
 * emit 5 vertex pairs (10 doubles). Output buffer type is `double*`
 * (lat/lng pairs in radians or degrees per the kernel's documented
 * convention — see h3_ops.cpp for the exact unit).
 *
 *   size: writes cumulative DOUBLE offsets (i.e. 12 per hexagon, 10 per
 *         pentagon) so `out_offsets[count]` is the total fp64 count.
 *   emit: writes interleaved lat/lng pairs into `out_coords[]`. */
pgaccel_status pgaccel_h3_cell_to_boundary_output_size(const uint64_t* cells, size_t count,
                                                       uint32_t* out_offsets);
pgaccel_status pgaccel_h3_cell_to_boundary_emit(const uint64_t* cells, size_t count,
                                                const uint32_t* offsets, double* out_coords);

/* h3_cells_to_multi_polygon: outputs the union of input cell boundaries
 * as a flat polygon-vertex CSR. `cells` is a single multi-cell input;
 * the output is a CSR over polygon rings.
 *
 *   size: walks edge dedup and writes cumulative ring offsets:
 *         `out_ring_offsets[ring_count + 1]`. `*out_ring_count` returns
 *         the number of rings the kernel will emit.
 *   emit: writes interleaved lat/lng pairs into `out_coords[]` indexed
 *         by `ring_offsets`.
 *
 * Coordinate Agent 5A's kernel implementation with this signature; the
 * caller (Phase B Agent 1B) is responsible for re-encoding the rings as
 * a PostGIS GSERIALIZED multipolygon. */
pgaccel_status pgaccel_h3_cells_to_multi_polygon_output_size(const uint64_t* cells, size_t count,
                                                             uint32_t* out_ring_offsets,
                                                             uint32_t* out_ring_count);
pgaccel_status pgaccel_h3_cells_to_multi_polygon_emit(const uint64_t* cells, size_t count,
                                                      const uint32_t* ring_offsets,
                                                      uint32_t ring_count, double* out_coords);

/* ── BEGIN h3-var-output-extensions (Agent 5A insertion zone) ─────── */
/* ── END h3-var-output-extensions ─────────────────────────────────── */

/* ── Raster Operations ───────────────────────────────────────────── */

typedef enum {
  PGACCEL_PT_INT8 = 0,
  PGACCEL_PT_INT16 = 1,
  PGACCEL_PT_INT32 = 2,
  PGACCEL_PT_FLOAT32 = 3,
  PGACCEL_PT_FLOAT64 = 4,
} pgaccel_pixel_type;

typedef enum {
  PGACCEL_OP_LOAD_BAND = 0,
  PGACCEL_OP_LOAD_CONST = 1,
  PGACCEL_OP_ADD = 2,
  PGACCEL_OP_SUB = 3,
  PGACCEL_OP_MUL = 4,
  PGACCEL_OP_DIV = 5,
  PGACCEL_OP_SQRT = 6,
  PGACCEL_OP_ABS = 7,
  PGACCEL_OP_LOG = 8,
  PGACCEL_OP_POW = 9,
  PGACCEL_OP_GT = 10,
  PGACCEL_OP_LT = 11,
  PGACCEL_OP_EQ = 12,
  PGACCEL_OP_SELECT = 13,
} pgaccel_op;

typedef struct {
  pgaccel_op op;
  union {
    int band_index;
    double constant;
  } arg;
} pgaccel_expr_inst;

typedef struct {
  pgaccel_expr_inst* instructions;
  size_t inst_count;
  size_t band_count;
} pgaccel_expr;

typedef struct {
  double min_val;
  double max_val;
  double new_val;
} pgaccel_reclass_rule;

pgaccel_status pgaccel_map_algebra(const void* const* band_pixels, size_t pixel_count,
                                   int pixel_type, const pgaccel_expr* expr, void* output_pixels,
                                   uint8_t* nodata_mask);

pgaccel_status pgaccel_raster_clip(const void* rast_pixels, size_t width, size_t height,
                                   double origin_x, double origin_y, double scale_x, double scale_y,
                                   int pixel_type, const float* clip_ring_xy, size_t vertex_count,
                                   void* output_pixels, uint8_t* nodata_mask);

pgaccel_status pgaccel_raster_reclass(const void* input_pixels, size_t pixel_count, int input_type,
                                      const pgaccel_reclass_rule* rules, size_t rule_count,
                                      int output_type, void* output_pixels);

/* ── BEGIN raster-extensions (Agent 3A insertion zone) ──────────────
 * Agent 3A appends here:
 *   - pgaccel_raster_resample (bilinear)
 *   - pgaccel_raster_slope (Horn's method)
 *   - pgaccel_raster_aspect (3×3 gradient → compass)
 *   - pgaccel_raster_hillshade (slope + aspect + sun)
 *   - pgaccel_raster_value (single-pixel lookup at point geometry)
 *   - pgaccel_raster_summarystats (count/sum/mean/stddev/min/max)
 * Keep declarations in this block so dispatch wiring stays grouped.
 */

/* Bilinear-interpolate src_pixels (W×H, fp32) to dst_pixels (new_W×new_H,
 * fp32). Out-of-range neighbours clamp to nearest edge. */
pgaccel_status pgaccel_raster_resample(const float* src_pixels, size_t src_w, size_t src_h,
                                       size_t dst_w, size_t dst_h, float* dst_pixels);

/* Per-pixel slope angle in degrees via Horn's 3×3 gradient. Edge pixels
 * (1-pixel border) get 0 — the stencil is undefined there. cell_size_x/y
 * are world units per pixel. Output is fp32 degrees [0, 90]. */
pgaccel_status pgaccel_raster_slope(const float* src_pixels, size_t width, size_t height,
                                    double cell_size_x, double cell_size_y, float* slope_out);

/* Per-pixel aspect (compass direction of steepest descent) in degrees
 * [0, 360). N=0, E=90, S=180, W=270. Flat areas and edge pixels get -1. */
pgaccel_status pgaccel_raster_aspect(const float* src_pixels, size_t width, size_t height,
                                     float* aspect_out);

/* Per-pixel shaded relief value [0, 255]. sun_azimuth_deg is compass
 * (N=0 CW); sun_altitude_deg is degrees above horizon. z_factor scales
 * pixel-value height units to match cell_size units. Edge pixels get 0. */
pgaccel_status pgaccel_raster_hillshade(const float* src_pixels, size_t width, size_t height,
                                        double cell_size_x, double cell_size_y,
                                        double sun_azimuth_deg, double sun_altitude_deg,
                                        double z_factor, float* shade_out);

/* Per-point pixel-value lookup. Translates each (x, y) in `point_xy` to
 * (col, row) via the raster's affine, bounds-checks, writes the pixel
 * value into `output[i]`. Out-of-bounds points get NaN. Pixel buffer is
 * fp32, output is fp64. */
pgaccel_status pgaccel_raster_value(const float* rast_pixels, size_t width, size_t height,
                                    double origin_x, double origin_y, double scale_x,
                                    double scale_y, const double* point_xy, size_t point_count,
                                    double* output);

/* Per-row 6-scalar summary stats over fp32 raster pixels. Output layout
 * is `[count, sum, mean, stddev, min, max]` per row × `row_count` rows
 * (`6 * sizeof(double) * row_count` total). When `nodata_masks` is non-
 * null, mask byte `1` skips that pixel. NaN/inf pixels are skipped.
 * Coordinates with `OutputShape::Record { field_count: 6 }` in Rust. */
pgaccel_status pgaccel_raster_summarystats(const float* rast_pixels, size_t row_count,
                                           size_t pixels_per_row, const uint8_t* nodata_masks,
                                           double* output);
/* ── END raster-extensions ─────────────────────────────────────────── */

/* Window-function declarations live in pgaccel_window.h (separate header
 * so the dispatcher can include just the window API without the rest of
 * the FFI surface). */

/* NLJ scalar-inequality declarations live in pgaccel_nested_loop_ineq.h
 * (separate header so the dispatcher only depends on the relevant subset). */

/* ── ABI pins ─────────────────────────────────────────────────────── */
/*
 * Pin struct sizes to detect accidental layout changes on either side of
 * the FFI boundary. Renaming a bool field does not change sizeof, so these
 * numbers must hold across the fp64-unlock rename (2026-04-22).
 */
#ifdef __cplusplus
static_assert(sizeof(pgaccel_platform_caps) == 88,
              "pgaccel_platform_caps ABI pinned at 88 bytes (fp64-unlock plan)");
static_assert(sizeof(pgaccel_device_info) == 216,
              "pgaccel_device_info ABI pinned at 216 bytes (fp64-unlock plan)");
#else
_Static_assert(sizeof(pgaccel_platform_caps) == 88,
               "pgaccel_platform_caps ABI pinned at 88 bytes (fp64-unlock plan)");
_Static_assert(sizeof(pgaccel_device_info) == 216,
               "pgaccel_device_info ABI pinned at 216 bytes (fp64-unlock plan)");
#endif

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FFI_H */

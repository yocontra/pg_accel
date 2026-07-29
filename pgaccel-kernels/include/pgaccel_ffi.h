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
  PGACCEL_INVALID_ARGUMENT = -6,
  PGACCEL_ERROR_OOM = -3,
  PGACCEL_ERROR_TIMEOUT = -4,
  PGACCEL_ERROR_UNSUPPORTED = -2,
} pgaccel_status;

/* Opaque hash-aggregate state returned by the H3 grouped-count entry points. */
struct pgaccel_agg_state;

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

/* fp64 path — PG native box type; raw IEEE-754 ordering on Metal */
pgaccel_status pgaccel_bbox_intersects_bulk_f64(const double* boxes_a, size_t count_a,
                                                const double* boxes_b, size_t count_b,
                                                uint8_t* result, size_t* hit_count);

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

/*
 * Masked fused multi-reduce for executor selection/qual masks.
 *
 * A row is consumed when:
 *   (selection == NULL || selection[i] != 0) &&
 *   (value_nulls == NULL || value_nulls[i] == 0)
 *
 * Empty effective input returns sum=0, min=0, max=0, count=0.
 */
pgaccel_status pgaccel_reduce_multi_masked_f32(const float* data, const uint8_t* value_nulls,
                                               const uint8_t* selection, size_t count,
                                               float* out_sum, float* out_min, float* out_max,
                                               int64_t* out_count);

pgaccel_status pgaccel_reduce_multi_masked_f64(const double* data, const uint8_t* value_nulls,
                                               const uint8_t* selection, size_t count,
                                               double* out_sum, double* out_min, double* out_max,
                                               int64_t* out_count);

pgaccel_status pgaccel_reduce_multi_masked_i64(const int64_t* data, const uint8_t* value_nulls,
                                               const uint8_t* selection, size_t count,
                                               int64_t* out_sum, int64_t* out_min, int64_t* out_max,
                                               int64_t* out_count);

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
 *   coords         — flat fp32 or fp64 [x,y,x,y,...]
 *   row_offsets    — [row_count + 1] CSR offsets into coords
 *   row_count      — number of rows
 *   use_fp64       — true selects the fp64 kernel (native where available,
 *                    soft-fp64 on Metal)
 *   closed_ring    — true: include wrap-around edge (Polygon
 *                    perimeter); false: open path (LineString length)
 *   lengths        — [row_count] output fp32 or fp64 lengths (>= 0)
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
 * Inline bbox pre-filter, then device-only dispatch for nonempty survivors.
 * results[i]: 1=inside, -1=outside, 0=uncertain/boundary.
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

/* Linear row-wise intersection classification. Pair i is
 * (geoms_a[i], geoms_b[i]); results[i] is 1=true, -1=false, 0=uncertain.
 * Supported predicates execute in one GPU kernel with a single packed device
 * allocation. Unsupported geometry combinations remain uncertain for exact
 * PostgreSQL/PostGIS recheck; no host predicate fallback is performed. */
pgaccel_status pgaccel_spatial_intersects_pairwise(const pgaccel_geometry* geoms_a,
                                                   const pgaccel_geometry* geoms_b, size_t count,
                                                   int8_t* results);

/* Resident fp64 spatial ABI. All lane and output pointers are current-context
 * DEVICE or SHARED_USM allocations. Each byte field is the actual readable or
 * writable allocation span from its paired pointer, not a size inferred from
 * logical counts. The descriptor itself remains host-owned for the duration of
 * the synchronous call. Geometry/ring offsets count coordinate pairs (not
 * scalar doubles or bytes), matching ResidentGeometryData.
 */
#define PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION 2u

typedef enum {
  PGACCEL_RESIDENT_GEOMETRY_POINT = 1,
  PGACCEL_RESIDENT_GEOMETRY_LINESTRING = 2,
  PGACCEL_RESIDENT_GEOMETRY_POLYGON = 3,
} pgaccel_resident_geometry_type;

typedef enum {
  PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID = 1u << 0,
} pgaccel_resident_geometry_flags;

typedef struct {
  uint32_t geom_type;
  int32_t srid;
  uint64_t first_ring;
  uint32_t ring_count;
  uint32_t flags;
} pgaccel_resident_geometry_row;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  const double* coordinates;                 /* [coordinate_pair_count * 2] */
  const double* bboxes;                      /* [row_count * 4] */
  const uint64_t* geometry_offsets;          /* [row_count + 1], coordinate-pair offsets */
  const uint64_t* ring_offsets;              /* [ring_count], coordinate-pair offsets */
  const pgaccel_resident_geometry_row* rows; /* [row_count] */
  const uint8_t* nulls;                      /* optional [row_count], canonical 0/1 */
  size_t coordinates_bytes;                  /* readable bytes from coordinates */
  size_t bboxes_bytes;                       /* readable bytes from bboxes */
  size_t geometry_offsets_bytes;             /* readable bytes from geometry_offsets */
  size_t ring_offsets_bytes;                 /* readable bytes from ring_offsets */
  size_t rows_bytes;                         /* readable bytes from rows */
  size_t nulls_bytes;                        /* readable bytes from nulls, zero when NULL */
  size_t row_count;
  size_t coordinate_pair_count;
  size_t ring_count;
} pgaccel_resident_geometry_view;

typedef struct {
  pgaccel_resident_geometry_view view;
  size_t first_row;
  size_t row_stride; /* 1 = aligned column, 0 = one-row constant */
} pgaccel_resident_geometry_operand;

typedef enum {
  PGACCEL_SPATIAL_PREDICATE_INTERSECTS = 0,
  PGACCEL_SPATIAL_PREDICATE_CONTAINS = 1,
  PGACCEL_SPATIAL_PREDICATE_WITHIN = 2,
  PGACCEL_SPATIAL_PREDICATE_DWITHIN = 3,
  PGACCEL_SPATIAL_PREDICATE_DISTANCE = 4,
} pgaccel_spatial_predicate;

typedef enum {
  PGACCEL_SPATIAL_DETAIL_NONE = 0,
  PGACCEL_SPATIAL_DETAIL_CONTRACT = 1,
  PGACCEL_SPATIAL_DETAIL_GEOMETRY = 2,
  PGACCEL_SPATIAL_DETAIL_SRID_MISMATCH = 3,
  PGACCEL_SPATIAL_DETAIL_BYTE_BUDGET = 4,
  PGACCEL_SPATIAL_DETAIL_TRISTATE = 5,
  PGACCEL_SPATIAL_DETAIL_RECHECK_INDEX = 6,
  PGACCEL_SPATIAL_DETAIL_RECHECK_PATCH = 7,
} pgaccel_spatial_detail;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  pgaccel_spatial_predicate predicate;
  uint32_t pad;
  double distance_threshold; /* finite and >= 0 only for DWithin */
  size_t count;
  size_t max_referenced_bytes; /* defensive per-call geometry work cap */
  pgaccel_resident_geometry_operand left;
  pgaccel_resident_geometry_operand right;
  int8_t* predicate_results;       /* device [output_capacity], boolean predicates */
  size_t predicate_results_bytes;  /* writable bytes from predicate_results */
  double* distances;               /* device [output_capacity], Distance only */
  size_t distances_bytes;          /* writable bytes from distances */
  uint8_t* distance_uncertain;     /* device [output_capacity], Distance only */
  size_t distance_uncertain_bytes; /* writable bytes from distance_uncertain */
  size_t output_capacity;
} pgaccel_spatial_resident_request;

/* Caller-owned device scratch shared by the resident evaluation and exact
 * recheck helpers. `control_bytes` and `failure_flags_bytes` are exact spans,
 * not capacities or upper bounds. Launch functions only copy control metadata
 * H2D and execute device work; the finish function performs the sole D2H
 * status read after resident input borrows have been released. */
#define PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION 1u
#define PGACCEL_SPATIAL_RECHECK_ABI_VERSION 1u
#define PGACCEL_SPATIAL_CONTROL_BYTES 384u
#define PGACCEL_SPATIAL_MAX_CHUNK_ROWS 65536u

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  uint8_t* control;
  size_t control_bytes;
  uint32_t* failure_flags;
  size_t failure_flags_bytes;
} pgaccel_spatial_workspace;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  const int8_t* tri_state; /* device [row_count], exactly {-1,0,+1} */
  size_t tri_state_bytes;
  int8_t* final_mask; /* device [row_count], SQL {-1,+1} */
  size_t final_mask_bytes;
  uint64_t* uncertain_indices; /* device [uncertain_capacity] */
  size_t uncertain_indices_bytes;
  uint64_t* uncertain_count; /* device scalar */
  size_t uncertain_count_bytes;
  size_t row_count;
  size_t uncertain_capacity;
} pgaccel_spatial_recheck_compact_request;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  const uint64_t* indices; /* device [patch_count], strictly increasing */
  size_t indices_bytes;
  const int8_t* results; /* device [patch_count], exactly {-1,+1} */
  size_t results_bytes;
  int8_t* final_mask; /* device [row_count] */
  size_t final_mask_bytes;
  size_t row_count;
  size_t patch_count;
} pgaccel_spatial_recheck_patch_request;

/* Evaluate one resident column/column or column/constant spatial operation.
 * Successful boolean rows write exactly {-1,0,+1}; zero is reserved for a
 * genuine algorithmic exact-recheck case. NULL/empty rows write -1 for filter
 * semantics and remain distinguishable through the input NULL sidecar.
 * Distance uses its dedicated fp64 output and uncertainty sidecar. Descriptor,
 * pointer, geometry-shape, SRID, byte-budget, device, allocation, and runtime
 * failures are hard non-OK statuses and never synthesize UNCERTAIN rows. */
/* Legacy synchronous entry point retained for link compatibility. Non-empty
 * requests return PGACCEL_UNSUPPORTED without dispatch or output writes. */
pgaccel_status pgaccel_spatial_eval_resident_ex(const pgaccel_spatial_resident_request* request,
                                                int32_t* detail);

/* Evaluation begins a launch chain and clears failure_flags. Compaction must
 * immediately follow evaluation on the same workspace: it preserves a sticky
 * evaluation failure and performs no writes when that failure is set. Call
 * workspace_finish after compaction and after releasing resident borrows.
 * Patching begins a separate chain after that finish and clears failure_flags;
 * call workspace_finish again after patching. Every non-empty launch is
 * limited to PGACCEL_SPATIAL_MAX_CHUNK_ROWS rows. */
pgaccel_status pgaccel_spatial_eval_resident_launch(const pgaccel_spatial_resident_request* request,
                                                    const pgaccel_spatial_workspace* workspace,
                                                    int32_t* detail);

pgaccel_status
pgaccel_spatial_recheck_compact_launch(const pgaccel_spatial_recheck_compact_request* request,
                                       const pgaccel_spatial_workspace* workspace, int32_t* detail);

pgaccel_status
pgaccel_spatial_recheck_patch_launch(const pgaccel_spatial_recheck_patch_request* request,
                                     const pgaccel_spatial_workspace* workspace, int32_t* detail);

pgaccel_status pgaccel_spatial_workspace_finish(const pgaccel_spatial_workspace* workspace,
                                                int32_t* detail);

/* Deprecated cross-product ABI retained for link compatibility. Non-empty
 * inputs return PGACCEL_UNSUPPORTED with zero counts and no GPU dispatch.
 * New callers must use pgaccel_spatial_intersects_pairwise. */
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

/* Detailed validation failures returned by cell_to_parent_resident_ex. These
 * refine PGACCEL_INVALID_ARGUMENT only; other status values leave detail as
 * NONE. CONTRACT identifies caller-owned pointer, shape, or null-sidecar
 * faults. INVALID_CELL and RES_MISMATCH identify H3 algorithm errors. */
typedef enum {
  PGACCEL_H3_PARENT_DETAIL_NONE = 0,
  PGACCEL_H3_PARENT_DETAIL_CONTRACT = 1,
  PGACCEL_H3_PARENT_DETAIL_INVALID_CELL = 2,
  PGACCEL_H3_PARENT_DETAIL_RES_MISMATCH = 3,
} pgaccel_h3_parent_detail;

/* Resident cell-to-parent transform. `cells`, optional canonical 0/1 `nulls`,
 * and caller-reserved `parents` are current-context DEVICE/SHARED_USM pointers.
 * Null rows write a canonical zero value and retain nullness in the unchanged
 * caller-owned sidecar. Invalid sidecars, cells, or ancestor resolutions fail
 * the whole call with PGACCEL_INVALID_ARGUMENT. No row buffers are allocated,
 * staged through host/shared memory, or copied back to the host. */
pgaccel_status pgaccel_h3_cell_to_parent_resident_ex(const uint64_t* cells, const uint8_t* nulls,
                                                     size_t count, int32_t parent_res,
                                                     uint64_t* parents, int32_t* detail);

/* Legacy ABI retained for existing callers. Equivalent to resident_ex with an
 * internal detail result. */
pgaccel_status pgaccel_h3_cell_to_parent_resident(const uint64_t* cells, const uint8_t* nulls,
                                                  size_t count, int32_t parent_res,
                                                  uint64_t* parents);

pgaccel_status pgaccel_h3_cell_to_parent_count_bulk(const uint64_t* cells, size_t count,
                                                    int parent_res,
                                                    struct pgaccel_agg_state** out_state);

/* For each input cell, return the center child at `child_res` (must be
 * >= input cell's resolution). The center child is the unique
 * canonical descendant whose new digits are all 0. Outputs 0 for invalid
 * inputs (cell == 0 or child_res < cell.res or child_res > 15). */
pgaccel_status pgaccel_h3_cell_to_center_child_bulk(const uint64_t* cells, size_t count,
                                                    int child_res, uint64_t* children);

pgaccel_status pgaccel_h3_grid_distance_bulk(const uint64_t* cells_a, const uint64_t* cells_b,
                                             size_t count, int32_t* distances);

/* `use_fp64` selects the caller-provided input type:
 *   false: lat_array/lng_array are float*
 *   true:  lat_array/lng_array are double*
 * High-resolution fp32 input may still be promoted internally for exact
 * computation, but the source buffers are always read according to this flag. */
pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(const void* lat_array, const void* lng_array,
                                               size_t count, int resolution, int use_fp64,
                                               uint64_t* cell_ids, uint8_t* valid);

/* Grouped lat/lng -> H3 COUNT(*) paths generate keys and exact boundary
 * fixups on GPU. The f64/exact arrays feed split fp64 projection +
 * integer-finalization kernels (native fp64 where available, soft-fp64 on
 * Metal); they are not a host fallback. */
pgaccel_status pgaccel_h3_lat_lng_count_bulk(const double* lat_array, const double* lng_array,
                                             size_t count, int resolution,
                                             struct pgaccel_agg_state** out_state);
pgaccel_status pgaccel_h3_lat_lng_count_bulk_f32_exact(const float* lat_f32_array,
                                                       const float* lng_f32_array,
                                                       const double* lat_exact_array,
                                                       const double* lng_exact_array, size_t count,
                                                       int resolution,
                                                       struct pgaccel_agg_state** out_state);
pgaccel_status pgaccel_h3_lat_lng_count_resident_bulk(
    const double* lat_exact_array, const double* lng_exact_array, const float* lat_f32_array,
    const float* lng_f32_array, size_t count, int resolution, struct pgaccel_agg_state** out_state);

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
 * A size or emit pass may return PGACCEL_ERROR_UNSUPPORTED without writing
 * output when exact device semantics are unavailable. Such operations must
 * remain outside production planner registration.
 */

/* h3_grid_disk ABI reservation. Nonempty work returns UNSUPPORTED until exact
 * cross-face device neighbor traversal is implemented. */
pgaccel_status pgaccel_h3_grid_disk_output_size(const uint64_t* cells, size_t count, int32_t k,
                                                uint32_t* out_offsets);
pgaccel_status pgaccel_h3_grid_disk_emit(const uint64_t* cells, size_t count, int32_t k,
                                         const uint32_t* offsets, uint64_t* out_cells);

/* h3_grid_ring_unsafe ABI reservation. Nonempty work returns UNSUPPORTED until
 * exact cross-face device neighbor traversal is implemented. */
pgaccel_status pgaccel_h3_grid_ring_unsafe_output_size(const uint64_t* cells, size_t count,
                                                       int32_t k, uint32_t* out_offsets);
pgaccel_status pgaccel_h3_grid_ring_unsafe_emit(const uint64_t* cells, size_t count, int32_t k,
                                                const uint32_t* offsets, uint64_t* out_cells);

/* h3_polyfill ABI reservation. Nonempty work returns UNSUPPORTED until exact
 * H3 containment and polygon topology are implemented on device. */
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

/* h3_cell_to_boundary ABI reservation. Nonempty work returns UNSUPPORTED until
 * exact icosahedral edge correction is implemented on device. */
pgaccel_status pgaccel_h3_cell_to_boundary_output_size(const uint64_t* cells, size_t count,
                                                       uint32_t* out_offsets);
pgaccel_status pgaccel_h3_cell_to_boundary_emit(const uint64_t* cells, size_t count,
                                                const uint32_t* offsets, double* out_coords);

/* h3_cells_to_multi_polygon ABI reservation. Nonempty work returns
 * UNSUPPORTED until exact device edge cancellation and ring linking land. */
pgaccel_status pgaccel_h3_cells_to_multi_polygon_output_size(const uint64_t* cells, size_t count,
                                                             uint32_t* out_ring_offsets,
                                                             uint32_t* out_ring_count);
pgaccel_status pgaccel_h3_cells_to_multi_polygon_emit(const uint64_t* cells, size_t count,
                                                      const uint32_t* ring_offsets,
                                                      uint32_t ring_count, double* out_coords);

/* ── BEGIN h3-var-output-extensions (Agent 5A insertion zone) ─────── */
/* ── END h3-var-output-extensions ─────────────────────────────────── */

/* ── Raster Operations ───────────────────────────────────────────── */

/* Exact resident PostGIS 3.6.4 ST_Reclass(raster,text,text) ABI.
 *
 * This is intentionally a Reclass-only surface. There is no resident summary-
 * statistics operation tag or entry point: PostGIS' sequential fp64/Welford
 * accumulation and host sqrt result are not proven bit-identical on every
 * supported device backend.
 *
 * Every non-empty pointer/span pair below names a current-context DEVICE or
 * SHARED_USM allocation. Span byte counts are exact, not lower bounds. Pixel
 * lanes use the literal PostGIS rt_pixtype tags and one native little-endian
 * element per pixel (1BB/2BUI/4BUI each occupy one byte in PostGIS WKB).
 * The descriptor itself remains host-owned for this synchronous call.
 */
#define PGACCEL_RESIDENT_RASTER_ABI_VERSION 1u
#define PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES 64u
#define PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH 65536u
#define PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS 4096u

typedef enum {
  PGACCEL_RESIDENT_RASTER_BOOL = 0,
  PGACCEL_RESIDENT_RASTER_UINT2 = 1,
  PGACCEL_RESIDENT_RASTER_UINT4 = 2,
  PGACCEL_RESIDENT_RASTER_INT8 = 3,
  PGACCEL_RESIDENT_RASTER_UINT8 = 4,
  PGACCEL_RESIDENT_RASTER_INT16 = 5,
  PGACCEL_RESIDENT_RASTER_UINT16 = 6,
  PGACCEL_RESIDENT_RASTER_INT32 = 7,
  PGACCEL_RESIDENT_RASTER_UINT32 = 8,
  PGACCEL_RESIDENT_RASTER_FLOAT32 = 10,
  PGACCEL_RESIDENT_RASTER_FLOAT64 = 11,
} pgaccel_resident_raster_pixel_type;

typedef enum {
  PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA = 1u << 0,
  PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA = 1u << 1,
} pgaccel_resident_raster_band_flags;

typedef struct {
  uint32_t width;
  uint32_t height;
  uint32_t first_band;
  uint32_t band_count;
  int32_t srid;
  uint32_t flags;
  double scale_x;
  double scale_y;
  double ip_x;
  double ip_y;
  double skew_x;
  double skew_y;
} pgaccel_resident_raster_row;

typedef struct {
  uint32_t pixel_type;
  uint32_t flags;
  double nodata;
} pgaccel_resident_raster_band;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  const uint8_t* pixels;
  size_t pixels_bytes;
  const uint64_t* band_offsets; /* [band_count + 1], byte offsets */
  size_t band_offsets_bytes;
  const pgaccel_resident_raster_row* rows; /* [row_count] */
  size_t rows_bytes;
  const pgaccel_resident_raster_band* bands; /* [band_count] */
  size_t bands_bytes;
  const uint8_t* nulls; /* optional [row_count], canonical 0/1 */
  size_t nulls_bytes;
  size_t row_count;
  size_t band_count;
} pgaccel_resident_raster_view;

typedef struct {
  int64_t source;
  int64_t destination;
} pgaccel_resident_raster_reclass_rule;

typedef enum {
  PGACCEL_RASTER_ROW_NULL = 0,
  PGACCEL_RASTER_ROW_PASSTHROUGH = 1,
  PGACCEL_RASTER_ROW_RECLASSIFIED = 2,
} pgaccel_resident_raster_row_action;

typedef enum {
  PGACCEL_RASTER_DETAIL_NONE = 0,
  PGACCEL_RASTER_DETAIL_CONTRACT = 1,
  PGACCEL_RASTER_DETAIL_VIEW = 2,
  PGACCEL_RASTER_DETAIL_RULES = 3,
  PGACCEL_RASTER_DETAIL_OFFSETS = 4,
  PGACCEL_RASTER_DETAIL_CAPACITY = 5,
  PGACCEL_RASTER_DETAIL_BYTE_BUDGET = 6,
  PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW = 7,
} pgaccel_resident_raster_detail;

typedef enum {
  PGACCEL_RASTER_VALIDATION_VIEW = 1u << 0,
  PGACCEL_RASTER_VALIDATION_RULES = 1u << 1,
  PGACCEL_RASTER_VALIDATION_OFFSETS = 1u << 2,
  PGACCEL_RASTER_VALIDATION_CAPACITY = 1u << 3,
  PGACCEL_RASTER_VALIDATION_BYTE_BUDGET = 1u << 4,
  PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW = 1u << 5,
} pgaccel_resident_raster_validation_failure;

/* Caller-owned device scratch. It is output-only and may be reused after the
 * synchronous call returns. Keeping this allocation outside the helper is
 * required by the resident-store borrow contract. */
typedef struct {
  uint32_t failures;
  uint32_t pad;
  uint64_t first_output_offset;
  uint64_t last_output_offset;
} pgaccel_resident_raster_validation_scratch;

typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  pgaccel_resident_raster_view input;
  size_t first_row;
  size_t count;
  uint32_t output_pixel_type; /* integer resident pixel tags 0..8 only */
  uint32_t pad;
  const pgaccel_resident_raster_reclass_rule* rules; /* [rule_count] */
  size_t rules_bytes;
  size_t rule_count;
  const uint64_t* output_offsets; /* [count + 1], global output byte offsets */
  size_t output_offsets_bytes;
  uint8_t* output_pixels;
  size_t output_pixels_bytes;
  uint8_t* row_actions; /* [count] */
  size_t row_actions_bytes;
  pgaccel_resident_raster_validation_scratch* validation_scratch; /* [1] */
  size_t validation_scratch_bytes;
  size_t max_total_pixels; /* exact selected pixels and defensive work cap */
  size_t max_chunk_pixels; /* maximum pixels in one device launch */
} pgaccel_raster_reclass_resident_request;

/* Exact output_offsets deltas are zero for NULL/passthrough rows and
 * width*height*output_pixel_width for reclassified rows. Offsets are caller-
 * owned and read-only. The function writes only output_pixels and row_actions;
 * Host descriptor/allocation failures are hard non-OK statuses. Device-view,
 * rule, offset, capacity, and budget failures are written to validation_scratch
 * and make every ordered output kernel a no-op. The caller reads/maps that
 * scratch only after releasing its resident-input borrow; no failed result may
 * be reconstructed or published. */
pgaccel_status
pgaccel_raster_reclass_resident_ex(const pgaccel_raster_reclass_resident_request* request,
                                   int32_t* detail);

/* ── ABI pins ─────────────────────────────────────────────────────── */
/*
 * Pin struct sizes to detect accidental layout changes on either side of
 * the FFI boundary. Renaming a bool field does not change sizeof, so these
 * numbers must hold across the fp64-unlock rename (2026-04-22).
 */
#ifdef __cplusplus
#define PGACCEL_ABI_ASSERT(condition, message) static_assert(condition, message)
#else
#define PGACCEL_ABI_ASSERT(condition, message) _Static_assert(condition, message)
#endif

#define PGACCEL_ABI_OFFSET(type, field, offset) \
  PGACCEL_ABI_ASSERT(offsetof(type, field) == offset, #type "." #field " ABI offset drifted")

PGACCEL_ABI_ASSERT(sizeof(pgaccel_platform_caps) == 88,
                   "pgaccel_platform_caps ABI pinned at 88 bytes (fp64-unlock plan)");
PGACCEL_ABI_ASSERT(sizeof(pgaccel_device_info) == 216,
                   "pgaccel_device_info ABI pinned at 216 bytes (fp64-unlock plan)");
PGACCEL_ABI_ASSERT(sizeof(pgaccel_geometry) == 48,
                   "pgaccel_geometry ABI pinned at 48 bytes (Rust mirror: gpu/types.rs)");
PGACCEL_ABI_OFFSET(pgaccel_geometry, bbox, 8);
PGACCEL_ABI_OFFSET(pgaccel_geometry, coords, 16);
PGACCEL_ABI_OFFSET(pgaccel_geometry, coord_count, 24);
PGACCEL_ABI_OFFSET(pgaccel_geometry, ring_offsets, 32);
PGACCEL_ABI_OFFSET(pgaccel_geometry, ring_count, 40);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_resident_raster_row) == 72,
                   "resident raster row ABI pinned at 72 bytes");
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, width, 0);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, height, 4);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, first_band, 8);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, band_count, 12);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, srid, 16);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, flags, 20);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, scale_x, 24);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, scale_y, 32);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, ip_x, 40);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, ip_y, 48);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, skew_x, 56);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_row, skew_y, 64);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_resident_raster_band) == 16,
                   "resident raster band ABI pinned at 16 bytes");
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_band, pixel_type, 0);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_band, flags, 4);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_band, nodata, 8);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_resident_raster_view) == 104,
                   "resident raster view ABI pinned at 104 bytes");
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, abi_version, 0);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, flags, 4);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, pixels, 8);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, pixels_bytes, 16);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, band_offsets, 24);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, band_offsets_bytes, 32);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, rows, 40);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, rows_bytes, 48);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, bands, 56);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, bands_bytes, 64);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, nulls, 72);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, nulls_bytes, 80);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, row_count, 88);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_view, band_count, 96);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_resident_raster_reclass_rule) == 16,
                   "resident raster rule ABI pinned at 16 bytes");
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_reclass_rule, source, 0);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_reclass_rule, destination, 8);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_resident_raster_validation_scratch) == 24,
                   "resident raster validation scratch ABI pinned at 24 bytes");
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_validation_scratch, failures, 0);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_validation_scratch, pad, 4);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_validation_scratch, first_output_offset, 8);
PGACCEL_ABI_OFFSET(pgaccel_resident_raster_validation_scratch, last_output_offset, 16);

PGACCEL_ABI_ASSERT(sizeof(pgaccel_raster_reclass_resident_request) == 240,
                   "resident raster request ABI pinned at 240 bytes");
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, abi_version, 0);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, flags, 4);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, input, 8);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, first_row, 112);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, count, 120);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, output_pixel_type, 128);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, pad, 132);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, rules, 136);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, rules_bytes, 144);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, rule_count, 152);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, output_offsets, 160);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, output_offsets_bytes, 168);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, output_pixels, 176);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, output_pixels_bytes, 184);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, row_actions, 192);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, row_actions_bytes, 200);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, validation_scratch, 208);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, validation_scratch_bytes, 216);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, max_total_pixels, 224);
PGACCEL_ABI_OFFSET(pgaccel_raster_reclass_resident_request, max_chunk_pixels, 232);

#undef PGACCEL_ABI_OFFSET
#undef PGACCEL_ABI_ASSERT

#ifdef __cplusplus
#define PGACCEL_RESIDENT_ABI_PIN(condition) static_assert(condition, #condition)
#else
#define PGACCEL_RESIDENT_ABI_PIN(condition) _Static_assert(condition, #condition)
#endif

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_resident_geometry_row) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_row, geom_type) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_row, srid) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_row, first_ring) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_row, ring_count) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_row, flags) == 20);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_resident_geometry_view) == 128);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, abi_version) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, flags) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, coordinates) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, bboxes) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, geometry_offsets) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, ring_offsets) == 32);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, rows) == 40);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, nulls) == 48);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, coordinates_bytes) == 56);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, bboxes_bytes) == 64);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, geometry_offsets_bytes) == 72);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, ring_offsets_bytes) == 80);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, rows_bytes) == 88);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, nulls_bytes) == 96);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, row_count) == 104);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, coordinate_pair_count) == 112);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_view, ring_count) == 120);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_resident_geometry_operand) == 144);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_operand, view) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_operand, first_row) == 128);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_resident_geometry_operand, row_stride) == 136);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_spatial_resident_request) == 384);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, abi_version) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, flags) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, predicate) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, pad) == 12);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, distance_threshold) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, count) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, max_referenced_bytes) == 32);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, left) == 40);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, right) == 184);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, predicate_results) == 328);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, predicate_results_bytes) ==
                         336);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, distances) == 344);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, distances_bytes) == 352);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, distance_uncertain) == 360);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, distance_uncertain_bytes) ==
                         368);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_resident_request, output_capacity) == 376);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_spatial_workspace) == 40);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, abi_version) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, flags) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, control) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, control_bytes) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, failure_flags) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_workspace, failure_flags_bytes) == 32);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_spatial_recheck_compact_request) == 88);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, abi_version) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, flags) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, tri_state) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, tri_state_bytes) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, final_mask) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, final_mask_bytes) == 32);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, uncertain_indices) ==
                         40);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request,
                                  uncertain_indices_bytes) == 48);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, uncertain_count) == 56);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, uncertain_count_bytes) ==
                         64);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, row_count) == 72);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_compact_request, uncertain_capacity) ==
                         80);

PGACCEL_RESIDENT_ABI_PIN(sizeof(pgaccel_spatial_recheck_patch_request) == 72);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, abi_version) == 0);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, flags) == 4);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, indices) == 8);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, indices_bytes) == 16);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, results) == 24);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, results_bytes) == 32);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, final_mask) == 40);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, final_mask_bytes) == 48);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, row_count) == 56);
PGACCEL_RESIDENT_ABI_PIN(offsetof(pgaccel_spatial_recheck_patch_request, patch_count) == 64);

#undef PGACCEL_RESIDENT_ABI_PIN

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FFI_H */

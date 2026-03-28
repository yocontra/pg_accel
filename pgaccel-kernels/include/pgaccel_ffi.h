#ifndef PGACCEL_FFI_H
#define PGACCEL_FFI_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

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
    bool has_fp64;
    bool has_atomic64;
    bool is_unified_memory;
} pgaccel_device_info;

typedef struct {
    bool has_fp64;
    bool has_atomic64;
    bool has_ooo_queue;
    bool is_unified_memory;
    size_t max_alloc_bytes;
    uint32_t compute_units;
    char backend_name[64];
} pgaccel_platform_caps;

pgaccel_status pgaccel_init(void);
pgaccel_status pgaccel_shutdown(void);
pgaccel_device_info pgaccel_get_device_info(void);
pgaccel_platform_caps pgaccel_get_caps(void);

/* ── Platform Capability Convenience Predicates ───────────────────── */

bool pgaccel_fp64_available(void);
bool pgaccel_unified_memory(void);
bool pgaccel_ooo_queue_available(void);


/* ── Memory Pool (USM arena allocator) ────────────────────────────── */

void*  pgaccel_alloc(size_t bytes);
void   pgaccel_free(void* ptr);
void   pgaccel_pool_reset(void);
size_t pgaccel_pool_bytes_used(void);
void   pgaccel_prefetch(void* ptr, size_t bytes);

/* ── Bounding Box Overlap ──────────────────────────────────────────── */

/*
 * Bulk bbox intersection: tests every (a[i], b[j]) pair.
 * Each box is 4 consecutive values: xmin, ymin, xmax, ymax.
 * result must point to count_a * count_b bytes (1 = intersects, 0 = not).
 * hit_count receives the total number of intersecting pairs.
 */

/* fp32 path — exact for PostGIS BOX2DF, works on all platforms */
pgaccel_status pgaccel_bbox_intersects_bulk_f32(
    const float* boxes_a,
    size_t count_a,
    const float* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
);

/* fp64 path — PG native box type, requires fp64 hardware support */
pgaccel_status pgaccel_bbox_intersects_bulk_f64(
    const double* boxes_a,
    size_t count_a,
    const double* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
);

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

/* ── Reduce Kernels ──────────────────────────────────────────────── */

/* fp32 reductions — all platforms */
pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count, float* result);
pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count, float* result);

/* fp64 reductions — CUDA/ROCm/Level Zero only, returns UNSUPPORTED on Metal */
pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count, double* result);
pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count, double* result);

/* i64 sum — all platforms */
pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count, int64_t* result);

/* Count nonzero bytes in mask (popcount) — all platforms */
pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count, size_t* result);

/* ── Spatial Predicate Kernels ────────────────────────────────────── */
/*
 * Three-result model:
 *   1 = DEFINITE true (inside/intersects)
 *  -1 = DEFINITE false (outside/no-intersect)
 *   0 = UNCERTAIN (must recheck on CPU)
 */

pgaccel_status pgaccel_point_in_ring_bulk(
    const void* points_xy,     /* [N * 2] interleaved x,y */
    size_t point_count,
    const void* ring_xy,       /* [V * 2] ring vertices   */
    size_t vertex_count,
    bool use_fp64,
    int8_t* results            /* [N] output: 1=inside, -1=outside, 0=uncertain */
);

pgaccel_status pgaccel_sphere_distance_bulk(
    const void* points_a,      /* [N * 2] lon,lat */
    const void* points_b,      /* [N * 2] lon,lat */
    size_t count,
    bool use_fp64,
    void* distances,           /* [N] output distances in meters */
    uint8_t* uncertain         /* [N] 1=uncertain, 0=definite    */
);

pgaccel_status pgaccel_segment_intersects_bulk(
    const void* segs_a,        /* [N * 4] x1,y1,x2,y2 */
    const void* segs_b,        /* [N * 4] x1,y1,x2,y2 */
    size_t count,
    bool use_fp64,
    int8_t* results            /* [N] 1=intersects, -1=no, 0=uncertain */
);

/* ── Spatial Dispatch (Three-Layer Pipeline) ──────────────────────── */

typedef enum {
    PGACCEL_GEOM_POINT = 0,
    PGACCEL_GEOM_LINESTRING = 1,
    PGACCEL_GEOM_POLYGON = 2,
    PGACCEL_GEOM_UNKNOWN = 99,
} pgaccel_geom_type;

typedef struct {
    pgaccel_geom_type type;
    const float* bbox;            /* [4] xmin, ymin, xmax, ymax */
    const float* coords;          /* flat coordinate array (x,y pairs) */
    size_t coord_count;           /* number of coordinate pairs */
    const uint32_t* ring_offsets; /* for polygons: offset of each ring in coords */
    size_t ring_count;            /* number of rings (0 for non-polygon) */
} pgaccel_geometry;

pgaccel_status pgaccel_spatial_intersects(
    const pgaccel_geometry* geoms_a,
    size_t count_a,
    const pgaccel_geometry* geoms_b,
    size_t count_b,
    uint32_t* definite_true_pairs,
    size_t* definite_true_count,
    uint32_t* definite_false_pairs,
    size_t* definite_false_count,
    uint32_t* uncertain_pairs,
    size_t* uncertain_count
);

/* ── H3 Cell Operations ──────────────────────────────────────────── */

pgaccel_status pgaccel_h3_get_resolution_bulk(
    const uint64_t* cells,
    size_t count,
    int32_t* resolutions);

pgaccel_status pgaccel_h3_cell_to_parent_bulk(
    const uint64_t* cells,
    size_t count,
    int parent_res,
    uint64_t* parents);

pgaccel_status pgaccel_h3_grid_distance_bulk(
    const uint64_t* cells_a,
    const uint64_t* cells_b,
    size_t count,
    int32_t* distances);

pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(
    const void* lat_array,
    const void* lng_array,
    size_t count,
    int resolution,
    int use_fp64,
    uint64_t* cell_ids,
    uint8_t* valid);

/* ── Raster Operations ───────────────────────────────────────────── */

typedef enum {
    PGACCEL_PT_INT8    = 0,
    PGACCEL_PT_INT16   = 1,
    PGACCEL_PT_INT32   = 2,
    PGACCEL_PT_FLOAT32 = 3,
    PGACCEL_PT_FLOAT64 = 4,
} pgaccel_pixel_type;

typedef enum {
    PGACCEL_OP_LOAD_BAND  = 0,
    PGACCEL_OP_LOAD_CONST = 1,
    PGACCEL_OP_ADD        = 2,
    PGACCEL_OP_SUB        = 3,
    PGACCEL_OP_MUL        = 4,
    PGACCEL_OP_DIV        = 5,
    PGACCEL_OP_SQRT       = 6,
    PGACCEL_OP_ABS        = 7,
    PGACCEL_OP_LOG        = 8,
    PGACCEL_OP_POW        = 9,
    PGACCEL_OP_GT         = 10,
    PGACCEL_OP_LT         = 11,
    PGACCEL_OP_EQ         = 12,
    PGACCEL_OP_SELECT     = 13,
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

pgaccel_status pgaccel_map_algebra(
    const void* const* band_pixels,
    size_t pixel_count,
    int pixel_type,
    const pgaccel_expr* expr,
    void* output_pixels,
    uint8_t* nodata_mask
);

pgaccel_status pgaccel_raster_clip(
    const void* rast_pixels,
    size_t width, size_t height,
    double origin_x, double origin_y,
    double scale_x, double scale_y,
    int pixel_type,
    const float* clip_ring_xy,
    size_t vertex_count,
    void* output_pixels,
    uint8_t* nodata_mask
);

pgaccel_status pgaccel_raster_reclass(
    const void* input_pixels,
    size_t pixel_count,
    int input_type,
    const pgaccel_reclass_rule* rules,
    size_t rule_count,
    int output_type,
    void* output_pixels
);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_FFI_H */

/*
 * pgaccel_expr.h — GPU expression evaluator types and API.
 *
 * Defines a stack-based bytecode VM for evaluating SQL WHERE clauses
 * and projections on the GPU. Extends the raster eval_expr pattern
 * with full SQL three-valued NULL logic, integer overflow detection,
 * and PG-compatible NaN semantics.
 *
 * Three-result model per row:
 *   PGACCEL_EXPR_TRUE      (+1) — predicate definitely true
 *   PGACCEL_EXPR_FALSE     (-1) — predicate definitely false
 *   PGACCEL_EXPR_UNCERTAIN ( 0) — must recheck on CPU
 */

#ifndef PGACCEL_EXPR_H
#define PGACCEL_EXPR_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Three-result constants ──────────────────────────────────────── */

#define PGACCEL_EXPR_TRUE 1
#define PGACCEL_EXPR_FALSE -1
#define PGACCEL_EXPR_UNCERTAIN 0

/* ── Value type tags ─────────────────────────────────────────────── */

typedef enum {
  PGACCEL_VAL_NULL = 0,
  PGACCEL_VAL_BOOL = 1,
  PGACCEL_VAL_INT32 = 2,
  PGACCEL_VAL_INT64 = 3,
  PGACCEL_VAL_FLOAT32 = 4,
  PGACCEL_VAL_FLOAT64 = 5,
  PGACCEL_VAL_DATE = 6,      /* int32: days since J2000 (PG internal) */
  PGACCEL_VAL_TIMESTAMP = 7, /* int64: microseconds since J2000 (PG internal) */
} pgaccel_val_tag;

/*
 * Tagged value — 16 bytes. Fits in a GPU register pair.
 * NULL is represented by tag == PGACCEL_VAL_NULL.
 */
typedef struct {
  pgaccel_val_tag tag;
  union {
    bool b;
    int32_t i32;
    int64_t i64;
    float f32;
    double f64;
  } data;
} pgaccel_val;

/* ── Expression opcodes ──────────────────────────────────────────── */

typedef enum {
  /* Stack manipulation */
  PGACCEL_EXPR_OP_LOAD_COL = 0,   /* Push column[arg.col_idx] */
  PGACCEL_EXPR_OP_LOAD_CONST = 1, /* Push constant from const pool */
  PGACCEL_EXPR_OP_LOAD_NULL = 2,  /* Push NULL */

  /* Arithmetic (pop 2, push 1) — integer ops detect overflow → UNCERTAIN */
  PGACCEL_EXPR_OP_ADD_I32 = 10,
  PGACCEL_EXPR_OP_ADD_I64 = 11,
  PGACCEL_EXPR_OP_ADD_F32 = 12,
  PGACCEL_EXPR_OP_ADD_F64 = 13,
  PGACCEL_EXPR_OP_SUB_I32 = 14,
  PGACCEL_EXPR_OP_SUB_I64 = 15,
  PGACCEL_EXPR_OP_SUB_F32 = 16,
  PGACCEL_EXPR_OP_SUB_F64 = 17,
  PGACCEL_EXPR_OP_MUL_I32 = 18,
  PGACCEL_EXPR_OP_MUL_I64 = 19,
  PGACCEL_EXPR_OP_MUL_F32 = 20,
  PGACCEL_EXPR_OP_MUL_F64 = 21,
  PGACCEL_EXPR_OP_DIV_I32 = 22, /* Division by zero → UNCERTAIN */
  PGACCEL_EXPR_OP_DIV_I64 = 23,
  PGACCEL_EXPR_OP_DIV_F32 = 24,
  PGACCEL_EXPR_OP_DIV_F64 = 25,
  PGACCEL_EXPR_OP_MOD_I32 = 26,
  PGACCEL_EXPR_OP_MOD_I64 = 27,
  PGACCEL_EXPR_OP_NEG_I32 = 28, /* Unary negate (pop 1, push 1) */
  PGACCEL_EXPR_OP_NEG_I64 = 29,
  PGACCEL_EXPR_OP_NEG_F32 = 30,
  PGACCEL_EXPR_OP_NEG_F64 = 31,

  /* Comparison (pop 2, push bool) — NaN handling: NaN = NaN → TRUE */
  PGACCEL_EXPR_OP_EQ = 40,
  PGACCEL_EXPR_OP_NE = 41,
  PGACCEL_EXPR_OP_LT = 42,
  PGACCEL_EXPR_OP_LE = 43,
  PGACCEL_EXPR_OP_GT = 44,
  PGACCEL_EXPR_OP_GE = 45,
  PGACCEL_EXPR_OP_ALWAYS_TRUE = 46, /* Predicate-only helper opcode */

  /* Boolean logic (SQL three-valued) */
  PGACCEL_EXPR_OP_AND = 50, /* NULL AND FALSE = FALSE */
  PGACCEL_EXPR_OP_OR = 51,  /* NULL OR TRUE = TRUE */
  PGACCEL_EXPR_OP_NOT = 52, /* NOT NULL = NULL */

  /* NULL tests (pop 1, push bool) */
  PGACCEL_EXPR_OP_IS_NULL = 60,
  PGACCEL_EXPR_OP_IS_NOT_NULL = 61,

  /* Type casts (pop 1, push 1) */
  PGACCEL_EXPR_OP_CAST_I32_I64 = 70,
  PGACCEL_EXPR_OP_CAST_I32_F64 = 71,
  PGACCEL_EXPR_OP_CAST_I64_F64 = 72,
  PGACCEL_EXPR_OP_CAST_F32_F64 = 73,
  PGACCEL_EXPR_OP_CAST_F64_F32 = 74,
  PGACCEL_EXPR_OP_CAST_BOOL_I32 = 75,

  /* Math functions (pop 1, push 1) */
  PGACCEL_EXPR_OP_ABS_I32 = 80,
  PGACCEL_EXPR_OP_ABS_I64 = 81,
  PGACCEL_EXPR_OP_ABS_F64 = 82,
  PGACCEL_EXPR_OP_SQRT_F64 = 83, /* sqrt(-x) → UNCERTAIN */
  PGACCEL_EXPR_OP_CEIL_F64 = 84,
  PGACCEL_EXPR_OP_FLOOR_F64 = 85,
  PGACCEL_EXPR_OP_ROUND_F64 = 86,

  /* Math functions (pop 2, push 1) */
  PGACCEL_EXPR_OP_POW_F64 = 90,

  /* CASE WHEN (encoded as conditional jumps) */
  PGACCEL_EXPR_OP_JUMP_IF_FALSE = 100, /* Pop bool; jump to arg if false/null */
  PGACCEL_EXPR_OP_JUMP = 101,          /* Unconditional jump to arg */

  /* Coalesce helper */
  PGACCEL_EXPR_OP_COALESCE = 110, /* Pop 2; push first non-null */

} pgaccel_expr_opcode;

/* ── Instruction ─────────────────────────────────────────────────── */

/*
 * 8 bytes per instruction. arg is overloaded:
 *   LOAD_COL:   col_idx (column index in batch)
 *   LOAD_CONST: const_idx (index into constant pool)
 *   JUMP*:      target instruction index
 *   Others:     unused (0)
 */
typedef struct {
  uint16_t opcode; /* pgaccel_expr_opcode */
  uint16_t _pad;
  uint32_t arg;
} pgaccel_expr_instruction;

/* ── Expression program ──────────────────────────────────────────── */

typedef struct {
  pgaccel_expr_instruction* instructions;
  size_t inst_count;
  pgaccel_val* const_pool; /* Constant values referenced by LOAD_CONST */
  size_t const_count;
  size_t max_stack; /* Maximum stack depth needed */
  size_t num_cols;  /* Number of input columns */
} pgaccel_expr_program;

/* ── Columnar batch ──────────────────────────────────────────────── */

/*
 * Columnar representation of a batch of rows. Each column is a
 * contiguous array of values + a null bitmap. Only columns referenced
 * by the expression program are populated.
 */
typedef struct {
  size_t num_rows;
  size_t num_cols;
  void** col_data;            /* [num_cols] pointers to column value arrays */
  uint8_t** col_nulls;        /* [num_cols] null bitmaps (1 = null, 0 = non-null);
                                 a null per-column pointer means all rows valid */
  pgaccel_val_tag* col_types; /* [num_cols] type tag per column */
} pgaccel_batch;

/*
 * Shared-USM column view for callers that already staged values into
 * device-accessible shared memory. nulls may be NULL, meaning all rows valid.
 */
typedef struct {
  const void* values;
  const uint8_t* nulls;
  pgaccel_val_tag type;
} pgaccel_expr_usm_col;

/* ── GPU-resident batch fabric (additive ABI) ───────────────────── */

#define PGACCEL_RESIDENT_BATCH_ABI_VERSION 1u

typedef enum {
  PGACCEL_MEM_SPACE_HOST = 0,
  PGACCEL_MEM_SPACE_SHARED_USM = 1,
  PGACCEL_MEM_SPACE_DEVICE = 2,
} pgaccel_mem_space;

/*
 * One typed resident column view. `values_space` / `nulls_space` describe
 * where the pointers live; kernels must not treat this as resident unless the
 * relevant space is SHARED_USM or DEVICE and the plan proof allows it.
 */
typedef struct {
  const void* values;
  const uint8_t* nulls;
  pgaccel_val_tag type;
  pgaccel_mem_space values_space;
  pgaccel_mem_space nulls_space;
  size_t element_size;
  uint32_t flags;
  uint32_t _pad;
} pgaccel_resident_column_view;

/*
 * Columnar batch view for end-to-end resident scan/expression/relational
 * pipelines. This intentionally does not replace pgaccel_batch; legacy callers
 * keep the old host-pointer ABI until migrated.
 */
typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  size_t num_rows;
  size_t num_cols;
  const pgaccel_resident_column_view* columns;
  const uint8_t* selection; /* optional, 1 = selected, 0 = filtered */
  pgaccel_mem_space selection_space;
  uint32_t _pad;
  size_t selected_rows;
} pgaccel_resident_batch;

/*
 * Device-resident variable-cardinality output. `offsets` is an exclusive
 * prefix sum with input_row_count + 1 entries and terminal value output_count.
 * Payload columns carry typed device/USM buffers for H3 cells, geometry
 * coordinates, raster pixels, join pairs, etc.
 */
typedef struct {
  uint32_t abi_version;
  uint32_t flags;
  size_t input_row_count;
  size_t output_count;
  size_t capacity;
  const uint64_t* offsets;
  const uint64_t* counts;
  const uint64_t* parent_row_ids;
  const pgaccel_resident_column_view* payload_cols;
  size_t payload_col_count;
  const uint8_t* null_mask;
  const uint8_t* unsupported_mask;
  const uint8_t* uncertain_mask;
  pgaccel_mem_space mask_space;
  uint32_t _pad;
} pgaccel_device_var_output;

/* ── Public API ──────────────────────────────────────────────────── */

/*
 * Evaluate a predicate expression on a columnar batch.
 *
 * For each row, evaluates the program and writes the three-result
 * outcome to results[]:
 *   +1 = TRUE  (row passes)
 *   -1 = FALSE (row filtered)
 *    0 = UNCERTAIN (CPU must recheck — overflow, domain error, etc.)
 *
 * Returns PGACCEL_OK on success. The results array is always fully
 * populated even on error (defaulting to UNCERTAIN).
 */
pgaccel_status pgaccel_expr_eval_predicate(const pgaccel_expr_program* program,
                                           const pgaccel_batch* batch,
                                           int8_t* results /* [num_rows] output */
);

/*
 * Evaluate a projection expression on a columnar batch.
 *
 * For each row, evaluates the program and writes the result value
 * to output[]. Rows where evaluation produces UNCERTAIN are flagged
 * in the uncertain mask.
 *
 * Returns PGACCEL_OK on success.
 */
pgaccel_status pgaccel_expr_eval_project(const pgaccel_expr_program* program,
                                         const pgaccel_batch* batch,
                                         pgaccel_val* output, /* [num_rows] output values */
                                         uint8_t* uncertain   /* [num_rows] 1=uncertain */
);

/*
 * Allocate/free SYCL shared-USM memory owned by the expression-template ABI.
 * Returned memory is host-writable and device-readable by pgaccel kernels.
 */
pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out);
void pgaccel_expr_shared_free(void* ptr);

/*
 * Allocate/free SYCL memory for resident cached columns and scratch.
 * `pgaccel_expr_device_alloc` returns device-owned memory for scratch/output.
 * The `_copy` variant copies an already-built host column into resident
 * GPU-readable memory once; on Apple/Metal it may use shared USM to avoid
 * unstable blit/copy-kernel paths in forked PostgreSQL backends.
 */
pgaccel_status pgaccel_expr_device_alloc(size_t bytes, void** out);
pgaccel_status pgaccel_expr_device_alloc_copy(const void* src, size_t bytes, void** out);
pgaccel_status pgaccel_expr_device_copy_from_host(void* dst, const void* src, size_t bytes);
pgaccel_status pgaccel_expr_device_copy_to_host(void* dst, const void* src, size_t bytes);
void pgaccel_expr_device_free(void* ptr);

/*
 * Fused template predicate + COUNT(*) helpers.
 *
 * These are for aggregate fusion: count only definite TRUE rows inside the
 * GPU kernel and return one scalar to the executor. FALSE and SQL NULL inputs
 * are excluded. Current template predicates do not produce UNCERTAIN, so
 * uncertain_count is set to 0 when provided.
 */
pgaccel_status pgaccel_expr_template_cmp_const_count(const pgaccel_batch* batch, uint32_t col_idx,
                                                     uint16_t cmp_opcode, double const_val,
                                                     size_t* true_count, size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_two_pred_and_count(const pgaccel_batch* batch,
                                                        uint32_t col1_idx, uint16_t cmp1_opcode,
                                                        double const1_val, uint32_t col2_idx,
                                                        uint16_t cmp2_opcode, double const2_val,
                                                        size_t* true_count,
                                                        size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_cmp_const_count_usm(pgaccel_expr_usm_col col, size_t row_count,
                                                         uint16_t cmp_opcode, double const_val,
                                                         size_t* true_count,
                                                         size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_cmp_const_mask_usm(pgaccel_expr_usm_col col, size_t row_count,
                                                        uint16_t cmp_opcode, double const_val,
                                                        uint8_t* selection, size_t* true_count,
                                                        size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_cmp_const_reduce_f32_usm(
    pgaccel_expr_usm_col pred_col, uint16_t cmp_opcode, double const_val,
    pgaccel_expr_usm_col value_col, size_t row_count, float* out_sum, float* out_min,
    float* out_max, int64_t* out_value_count, size_t* true_count, size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_two_pred_and_count_usm(pgaccel_expr_usm_col col1,
                                                            uint16_t cmp1_opcode, double const1_val,
                                                            pgaccel_expr_usm_col col2,
                                                            uint16_t cmp2_opcode, double const2_val,
                                                            size_t row_count, size_t* true_count,
                                                            size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_two_pred_and_mask_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, size_t row_count, uint8_t* selection,
    size_t* true_count, size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_two_pred_and_reduce_f32_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_value_count, size_t* true_count,
    size_t* uncertain_count);

/*
 * SSBM Q1.x resident filtered revenue aggregate.
 *
 * Consumes shared-USM/resident int32 lineorder columns and returns only the
 * final SUM(lo_extendedprice * lo_discount) plus selected-row count. NULL mask
 * pointers may be NULL, meaning the column is all-valid. If date_key_count is
 * zero, [orderdate_lo, orderdate_hi] is used as an inclusive range filter;
 * otherwise orderdate_keys points to a shared-USM/resident membership list.
 */
pgaccel_status pgaccel_expr_template_ssbm_q1_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t* out_sum, size_t* selected_count, size_t* uncertain_count);

size_t pgaccel_expr_template_ssbm_q1_scratch_items(size_t row_count);

pgaccel_status pgaccel_expr_template_ssbm_q1_revenue_i64_usm_scratch(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t* scratch_revenue_a, int64_t* scratch_count_a,
    int64_t* scratch_revenue_b, int64_t* scratch_count_b, size_t scratch_item_capacity,
    int64_t* out_sum, size_t* selected_count, size_t* uncertain_count);

/*
 * SSBM Q2.x resident grouped revenue aggregate.
 *
 * Consumes resident int32 lineorder columns and resident dimension lookup
 * buffers. `date_year_by_offset[lo_orderdate - date_key_min]` maps fact dates
 * to years. `part_brand_code_by_key[lo_partkey]` maps part keys to a stable
 * lexicographic brand code and `part_match_by_key` applies the Q2 part
 * predicate. `supplier_match_by_key` applies the Q2 supplier predicate.
 *
 * Output arrays are host-visible final materialization buffers with
 * `out_group_capacity >= year_count * brand_count`, laid out as
 * `(year - year_min) * brand_count + brand_code`. The kernel returns only
 * bounded grouped revenue/count arrays; PostgreSQL tuple materialization
 * happens above this ABI.
 */
pgaccel_status pgaccel_expr_template_ssbm_q2_grouped_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col partkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col revenue_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, size_t date_year_count,
    const int32_t* part_brand_code_by_key, const uint8_t* part_match_by_key, size_t part_key_count,
    const uint8_t* supplier_match_by_key, size_t supplier_key_count, int32_t year_min,
    int32_t year_count, int32_t brand_count, int64_t* out_revenue_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * SSBM Q3.x resident grouped revenue aggregate.
 *
 * This is the broader customer/supplier/date star-join shape used by Q3.1
 * through Q3.4. Fact columns stay resident, while date/customer/supplier
 * dimension predicates are precompiled into resident lookup maps:
 *
 *   date_match_by_offset[lo_orderdate - date_key_min]
 *   customer_match_by_key[lo_custkey]
 *   supplier_match_by_key[lo_suppkey]
 *
 * Customer and supplier group-code maps point at compact output labels
 * selected by the query variant: nation/nation for Q3.1 and city/city for
 * Q3.2-Q3.4. Output layout is
 * `((year - year_min) * customer_group_count + customer_code)
 *  * supplier_group_count + supplier_code`.
 */
pgaccel_status pgaccel_expr_template_ssbm_q3_grouped_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col custkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col revenue_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, const uint8_t* date_match_by_offset,
    size_t date_year_count, const int32_t* customer_group_code_by_key,
    const uint8_t* customer_match_by_key, size_t customer_key_count,
    const int32_t* supplier_group_code_by_key, const uint8_t* supplier_match_by_key,
    size_t supplier_key_count, int32_t year_min, int32_t year_count, int32_t customer_group_count,
    int32_t supplier_group_count, int64_t* out_revenue_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

/*
 * SSBM Q4.x resident grouped profit aggregate.
 *
 * Computes SUM(lo_revenue - lo_supplycost) for the full
 * lineorder/date/customer/supplier/part star join. The grouping layout is
 * `(year - year_min, geo_code, part_code)`, where `group_geo_source` selects
 * whether `geo_code` comes from customer (1) or supplier (2). Q4.1 uses one
 * synthetic part group; Q4.2 uses part category; Q4.3 uses part brand.
 */
pgaccel_status pgaccel_expr_template_ssbm_q4_grouped_profit_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col custkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col partkey_col,
    pgaccel_expr_usm_col revenue_col, pgaccel_expr_usm_col supplycost_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, const uint8_t* date_match_by_offset,
    size_t date_year_count, const int32_t* customer_group_code_by_key,
    const uint8_t* customer_match_by_key, size_t customer_key_count,
    const int32_t* supplier_group_code_by_key, const uint8_t* supplier_match_by_key,
    size_t supplier_key_count, const int32_t* part_group_code_by_key,
    const uint8_t* part_match_by_key, size_t part_key_count, int32_t group_geo_source,
    int32_t year_min, int32_t year_count, int32_t geo_group_count, int32_t part_group_count,
    uint32_t* scratch_profit_lo, uint32_t* scratch_profit_hi, uint32_t* scratch_count,
    size_t scratch_group_capacity, int64_t* out_profit_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

/*
 * Resident dense grouped aggregate for OLAP benchmark tables.
 *
 * Consumes resident int32 group keys and resident float8 values. Group keys
 * must map into `[group_min, group_min + group_count)`. The optional filter
 * column is a uint8/boolean selection mask where zero means reject and nonzero
 * means accept; NULL filter means all rows are selected.
 *
 * Scratch arrays are device-resident and reused by the caller. `scratch_sum`,
 * `scratch_min`, `scratch_max`, `scratch_count`, `scratch_group_start`, and
 * `scratch_group_cursor` must each have at least `group_count` elements.
 * `scratch_sorted_group` and `scratch_row_index` must each have at least
 * `row_count` elements. Final output arrays are host-visible materialization
 * buffers with `out_group_capacity >= group_count`.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    uint32_t* scratch_count, uint32_t* scratch_group_start, uint32_t* scratch_group_cursor,
    size_t scratch_group_capacity, int32_t* scratch_sorted_group, uint32_t* scratch_row_index,
    size_t scratch_row_capacity, double* out_sum_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * v3 extends the resident dense grouped aggregate with expression-defined
 * measures. `measure_op`: 0 = value_col, 1 = value_col * value_rhs_col,
 * 2 = value_col - value_rhs_col.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v3(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * v4 adds an aggregate lane mask so callers that only need SUM/COUNT do not
 * pay for MIN/MAX lanes. Mask bits: 1 = SUM, 2 = MIN, 4 = MAX, 8 = COUNT.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v4(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    pgaccel_expr_usm_col filter_col, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * v5 adds filter semantics. `filter_mode`: 0 = filter gates rows for every
 * aggregate lane, 1 = filter gates only measure lanes while COUNT(*) retains
 * the full grouped row domain. Mode 1 is for conditional aggregates such as
 * SUM(CASE WHEN predicate THEN measure ELSE 0 END), COUNT(*).
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v5(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, pgaccel_expr_usm_col filter_col, size_t row_count, int32_t group_min,
    int32_t group_count, double* scratch_sum, double* scratch_min, double* scratch_max,
    uint32_t* scratch_count, uint32_t* scratch_group_start, uint32_t* scratch_group_cursor,
    size_t scratch_group_capacity, int32_t* scratch_sorted_group, uint32_t* scratch_row_index,
    size_t scratch_row_capacity, double* out_sum_by_group, double* out_min_by_group,
    double* out_max_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count);

/*
 * v6 adds a compact measure-predicate descriptor. `measure_predicate_op`:
 * 0 = bool filter only; 1 = bool filter AND value_rhs BETWEEN arg1 AND arg2.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v6(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, double measure_predicate_arg1,
    double measure_predicate_arg2, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* out_sum_by_group,
    double* out_min_by_group, double* out_max_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

/*
 * v7 generalizes the v6 predicate descriptor to up to four inclusive
 * value_rhs intervals. This covers normalized ORs of comparison/range terms.
 * v7 remains RHS-sourced for compatibility.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v7(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_range_count,
    double measure_predicate_lo0, double measure_predicate_hi0, double measure_predicate_lo1,
    double measure_predicate_hi1, double measure_predicate_lo2, double measure_predicate_hi2,
    double measure_predicate_lo3, double measure_predicate_hi3, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * v8 adds `measure_predicate_source` immediately after
 * `measure_predicate_op`: 0 = value_col, 1 = value_rhs_col.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_source,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* out_sum_by_group,
    double* out_min_by_group, double* out_max_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

/*
 * v9 adds optional cache-owned row-block partial buffers after
 * `scratch_row_capacity`. If the partial pointers are null or capacity is too
 * small, the kernel falls back to internal per-dispatch allocation.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_source,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* scratch_partial_sum,
    double* scratch_partial_min, double* scratch_partial_max, uint32_t* scratch_partial_count,
    size_t scratch_partial_capacity, double* out_sum_by_group, double* out_min_by_group,
    double* out_max_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count);

/*
 * Resident scalar float8 reduction over one cached column. Mask bits:
 * 1 = SUM, 2 = MIN, 4 = MAX, 8 = COUNT, 16 = SUMSQ. NULL values are ignored.
 */
pgaccel_status pgaccel_expr_template_reduce_f64_usm(pgaccel_expr_usm_col value_col,
                                                    uint32_t aggregate_mask, size_t row_count,
                                                    double* out_sum, double* out_min,
                                                    double* out_max, double* out_sumsq,
                                                    uint64_t* out_count, size_t* selected_count,
                                                    size_t* uncertain_count);

/*
 * Resident dense grouped two-measure stats. The primary float8 value produces
 * SUM, SUMSQ, COUNT per group; the RHS float8 value produces SUM, COUNT per
 * group for AVG-style measures. NULLs are ignored independently per measure.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_stats_pair_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, double* scratch_sumsq, double* scratch_rhs_sum, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_len, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_sumsq_by_group, uint32_t* out_count_by_group,
    double* out_rhs_sum_by_group, uint32_t* out_rhs_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count);

/*
 * Narrow direct-column SUM/COUNT ABI for resident dense groups. This is kept
 * separate from v9 so Metal sees a small argument block for 129-256 group
 * one-scan reductions.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, uint32_t* scratch_count,
    double* scratch_partial_sum, uint32_t* scratch_partial_count, size_t scratch_partial_capacity,
    double* out_sum_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count);

/*
 * Narrow expression SUM/COUNT ABI for resident dense groups. `filter_mode`:
 * 0 = no filter, 1 = aggregate FILTER gates SUM and COUNT, 2 = CASE/measure
 * predicate gates SUM only while COUNT counts grouped rows.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col lhs_col, pgaccel_expr_usm_col rhs_col,
    pgaccel_expr_usm_col filter_col, int32_t filter_mode, size_t row_count, int32_t group_min,
    int32_t group_count, double* scratch_sum, uint32_t* scratch_count, double* scratch_partial_sum,
    uint32_t* scratch_partial_count, size_t scratch_partial_capacity, double* out_sum_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * Predicate-aware expression SUM/COUNT ABI for resident dense groups. It
 * consumes the same interval predicate descriptor as the generic v9 grouped
 * aggregate, but uses a one-scan wide layout for <=256 dense groups.
 */
pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_pred_sum_count_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, pgaccel_expr_usm_col filter_col, int32_t measure_op,
    int32_t filter_mode, int32_t measure_predicate_source, int32_t measure_predicate_op,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, uint32_t* scratch_count, double* scratch_partial_sum,
    uint32_t* scratch_partial_count, size_t scratch_partial_capacity, double* out_sum_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count);

/*
 * Generic one-dimension resident star projection.
 *
 * Projects fact rows into dense dimension group codes without materializing
 * joined tuples. Rows that fail the fact value predicate, have NULL keys or
 * values, miss the dimension map, or hit a filtered-out dimension write -1.
 * The caller can feed `out_group_codes` directly to the resident dense
 * grouped f64 aggregate; its group-range guard skips -1 rows.
 */
pgaccel_status pgaccel_expr_template_resident_star_dim_group_project_f64_usm(
    pgaccel_expr_usm_col fact_key_col, pgaccel_expr_usm_col value_col, size_t row_count,
    const uint8_t* dim_match_by_key, const int32_t* dim_group_code_by_key, size_t dim_key_count,
    uint16_t value_cmp_opcode, double value_const, int32_t* out_group_codes,
    size_t out_group_capacity);

pgaccel_status pgaccel_expr_template_resident_star_dim_group_compact_f64_usm(
    pgaccel_expr_usm_col fact_key_col, pgaccel_expr_usm_col value_col, size_t row_count,
    const uint8_t* dim_match_by_key, const int32_t* dim_group_code_by_key, size_t dim_key_count,
    uint16_t value_cmp_opcode, double value_const, int32_t* out_group_codes,
    double* out_values, size_t out_capacity, size_t* selected_count, size_t* uncertain_count);

/*
 * Fused one-dimension resident star join + grouped SUM/COUNT.
 *
 * This consumes resident fact key/value columns and resident dimension
 * match/group-code maps directly, applying the fact value predicate and
 * dimension membership before accumulating dense group sums/counts. It avoids
 * materializing compacted `(group_code, value)` rows when the aggregate shape
 * is the common SUM/COUNT lane.
 */
pgaccel_status pgaccel_expr_template_resident_star_dim_grouped_f64_sum_count_usm(
    pgaccel_expr_usm_col fact_key_col, pgaccel_expr_usm_col value_col, size_t row_count,
    const uint8_t* dim_match_by_key, const int32_t* dim_group_code_by_key, size_t dim_key_count,
    uint16_t value_cmp_opcode, double value_const, int32_t group_count, double* scratch_sum,
    uint32_t* scratch_count, double* scratch_partial_sum, uint32_t* scratch_partial_count,
    size_t scratch_partial_capacity, double* out_sum_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_EXPR_H */

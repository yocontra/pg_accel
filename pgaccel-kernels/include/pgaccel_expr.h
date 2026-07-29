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
 * Allocate/free SYCL shared-USM memory used by resident kernel tests and
 * callers that require host-writable, device-readable storage.
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

/* ── ABI pins ─────────────────────────────────────────────────────── */
/*
 * Two-sided layout pins for structs crossing the C/Rust FFI boundary.
 * Rust mirrors live in pg_accel/src/gpu/types.rs (PgaccelVal,
 * PgaccelExprInstruction, PgaccelBatch, PgaccelExprUsmCol, resident batch
 * fabric) with matching layout tests in pg_accel/src/gpu/bridge.rs and
 * types.rs. All pins assume LP64 (the only supported target family);
 * the pointer-size pin makes that explicit.
 */
#ifdef __cplusplus
#define PGACCEL_ABI_PIN(cond, msg) static_assert(cond, msg)
#else
#define PGACCEL_ABI_PIN(cond, msg) _Static_assert(cond, msg)
#endif

PGACCEL_ABI_PIN(sizeof(void*) == 8, "pgaccel FFI layout pins assume LP64 targets");

PGACCEL_ABI_PIN(sizeof(pgaccel_val) == 16, "pgaccel_val is 16 bytes (bridge.rs pin)");
PGACCEL_ABI_PIN(offsetof(pgaccel_val, data) == 8, "pgaccel_val.data at offset 8");

PGACCEL_ABI_PIN(sizeof(pgaccel_expr_instruction) == 8,
                "pgaccel_expr_instruction is 8 bytes (bridge.rs pin)");
PGACCEL_ABI_PIN(offsetof(pgaccel_expr_instruction, arg) == 4,
                "pgaccel_expr_instruction.arg at offset 4");

PGACCEL_ABI_PIN(sizeof(pgaccel_batch) == 40, "pgaccel_batch is 5 pointer-sized fields");
PGACCEL_ABI_PIN(offsetof(pgaccel_batch, num_cols) == 8, "pgaccel_batch.num_cols at offset 8");
PGACCEL_ABI_PIN(offsetof(pgaccel_batch, col_data) == 16, "pgaccel_batch.col_data at offset 16");
PGACCEL_ABI_PIN(offsetof(pgaccel_batch, col_nulls) == 24, "pgaccel_batch.col_nulls at offset 24");
PGACCEL_ABI_PIN(offsetof(pgaccel_batch, col_types) == 32, "pgaccel_batch.col_types at offset 32");

PGACCEL_ABI_PIN(sizeof(pgaccel_expr_usm_col) == 24,
                "pgaccel_expr_usm_col is 3 pointer-sized fields (bridge.rs pin)");

/* Resident batch fabric — Rust pins at pg_accel/src/gpu/types.rs
 * resident_batch_abi_layout_is_pinned(). */
PGACCEL_ABI_PIN(sizeof(pgaccel_resident_column_view) == 48,
                "pgaccel_resident_column_view ABI pinned at 48 bytes");
PGACCEL_ABI_PIN(sizeof(pgaccel_resident_batch) == 56,
                "pgaccel_resident_batch ABI pinned at 56 bytes");
PGACCEL_ABI_PIN(sizeof(pgaccel_device_var_output) == 104,
                "pgaccel_device_var_output ABI pinned at 104 bytes");

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_EXPR_H */

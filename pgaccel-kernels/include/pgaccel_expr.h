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
  uint8_t** col_nulls;        /* [num_cols] null bitmaps (1 = null, 0 = non-null) */
  pgaccel_val_tag* col_types; /* [num_cols] type tag per column */
} pgaccel_batch;

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

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_EXPR_H */

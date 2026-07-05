/*
 * expr_eval.cpp — GPU expression evaluator (bytecode interpreter).
 *
 * Stack-based VM that evaluates SQL WHERE clauses and projections on the GPU.
 * Each GPU thread independently evaluates the program for one row.
 *
 * Correctness rules:
 *   - Integer overflow → UNCERTAIN (caller rejects/errors)
 *   - Division by zero → UNCERTAIN
 *   - sqrt(negative) → UNCERTAIN
 *   - NaN = NaN → TRUE (PG semantics)
 *   - Full SQL three-valued NULL logic
 */

#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Maximum stack depth for the interpreter. Programs exceeding this
/// are rejected at compile time on the Rust side.
static constexpr size_t MAX_STACK = 64;

/// Create a NULL value.
static inline pgaccel_val make_null() {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_NULL;
  v.data.i64 = 0;
  return v;
}

/// Create a bool value.
static inline pgaccel_val make_bool(bool b) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_BOOL;
  v.data.b = b;
  return v;
}

/// Create an i32 value.
static inline pgaccel_val make_i32(int32_t x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_INT32;
  v.data.i32 = x;
  return v;
}

/// Create an i64 value.
static inline pgaccel_val make_i64(int64_t x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_INT64;
  v.data.i64 = x;
  return v;
}

/// Create an f32 value.
static inline pgaccel_val make_f32(float x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_FLOAT32;
  v.data.f32 = x;
  return v;
}

/// Create an f64 value.
static inline pgaccel_val make_f64(double x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_FLOAT64;
  v.data.f64 = x;
  return v;
}

/// Check if a value is NULL.
static inline bool is_null(const pgaccel_val& v) {
  return v.tag == PGACCEL_VAL_NULL;
}

/// Convert a value to bool, returning false for NULL.
static inline bool val_to_bool(const pgaccel_val& v) {
  if (v.tag == PGACCEL_VAL_BOOL)
    return v.data.b;
  if (v.tag == PGACCEL_VAL_INT32)
    return v.data.i32 != 0;
  if (v.tag == PGACCEL_VAL_INT64)
    return v.data.i64 != 0;
  return false;
}

/// PG-compatible float NaN check (works in SYCL device code).
static inline bool is_nan_f32(float x) {
  return x != x;
}
static inline bool is_nan_f64(double x) {
  return x != x;
}

/// PG-compatible float comparison: NaN = NaN is TRUE, NaN > everything.
static inline bool pg_float_eq_f64(double a, double b) {
  bool a_nan = is_nan_f64(a);
  bool b_nan = is_nan_f64(b);
  if (a_nan && b_nan)
    return true;
  if (a_nan || b_nan)
    return false;
  return a == b;
}

static inline bool pg_float_lt_f64(double a, double b) {
  if (is_nan_f64(a))
    return false;
  if (is_nan_f64(b))
    return true;
  return a < b;
}

/// Convert any numeric value to f64 for comparison.
/// Returns (value, is_valid). Invalid = NULL.
static inline double val_to_f64(const pgaccel_val& v) {
  switch (v.tag) {
    case PGACCEL_VAL_INT32:
      return static_cast<double>(v.data.i32);
    case PGACCEL_VAL_INT64:
      return static_cast<double>(v.data.i64);
    case PGACCEL_VAL_FLOAT32:
      return static_cast<double>(v.data.f32);
    case PGACCEL_VAL_FLOAT64:
      return v.data.f64;
    case PGACCEL_VAL_BOOL:
      return v.data.b ? 1.0 : 0.0;
    default:
      return 0.0;
  }
}

// ---------------------------------------------------------------------------
// Load column value from columnar batch
// ---------------------------------------------------------------------------

static inline pgaccel_val load_column(const pgaccel_batch* batch, size_t row, size_t col) {
  if (col >= batch->num_cols)
    return make_null();

  // Check null bitmap
  if (batch->col_nulls != nullptr && batch->col_nulls[col] != nullptr) {
    if (batch->col_nulls[col][row]) {
      return make_null();
    }
  }

  pgaccel_val_tag type = batch->col_types[col];
  const void* data = batch->col_data[col];
  if (data == nullptr)
    return make_null();

  switch (type) {
    case PGACCEL_VAL_BOOL: {
      const bool* arr = static_cast<const bool*>(data);
      return make_bool(arr[row]);
    }
    case PGACCEL_VAL_INT32: {
      const int32_t* arr = static_cast<const int32_t*>(data);
      return make_i32(arr[row]);
    }
    case PGACCEL_VAL_INT64: {
      const int64_t* arr = static_cast<const int64_t*>(data);
      return make_i64(arr[row]);
    }
    case PGACCEL_VAL_FLOAT32: {
      const float* arr = static_cast<const float*>(data);
      return make_f32(arr[row]);
    }
    case PGACCEL_VAL_FLOAT64: {
      const double* arr = static_cast<const double*>(data);
      return make_f64(arr[row]);
    }
    case PGACCEL_VAL_DATE: {
      // DATE stored as int32 (days since J2000)
      const int32_t* arr = static_cast<const int32_t*>(data);
      return make_i32(arr[row]);
    }
    case PGACCEL_VAL_TIMESTAMP: {
      // TIMESTAMP stored as int64 (microseconds since J2000)
      const int64_t* arr = static_cast<const int64_t*>(data);
      return make_i64(arr[row]);
    }
    default:
      return make_null();
  }
}

// ---------------------------------------------------------------------------
// CPU interpreter — evaluates one row
// ---------------------------------------------------------------------------

/// Result of evaluating one row: the final stack value and whether
/// an UNCERTAIN condition was hit.
struct eval_result {
  pgaccel_val value;
  bool uncertain;
};

static eval_result eval_row(const pgaccel_expr_program* prog, const pgaccel_batch* batch,
                            size_t row) {
  pgaccel_val stack[MAX_STACK];
  int sp = 0;
  bool uncertain = false;

  for (size_t pc = 0; pc < prog->inst_count; pc++) {
    const pgaccel_expr_instruction& inst = prog->instructions[pc];
    const uint16_t op = inst.opcode;
    const uint32_t arg = inst.arg;

    switch (op) {
        // ── Stack manipulation ──────────────────────────────────────

      case PGACCEL_EXPR_OP_LOAD_COL:
        stack[sp++] = load_column(batch, row, arg);
        break;

      case PGACCEL_EXPR_OP_LOAD_CONST:
        if (arg < prog->const_count) {
          stack[sp++] = prog->const_pool[arg];
        } else {
          stack[sp++] = make_null();
        }
        break;

      case PGACCEL_EXPR_OP_LOAD_NULL:
        stack[sp++] = make_null();
        break;

        // ── Integer arithmetic with overflow detection ──────────────

      case PGACCEL_EXPR_OP_ADD_I32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        int64_t r = static_cast<int64_t>(a.data.i32) + b.data.i32;
        if (r < INT32_MIN || r > INT32_MAX) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(static_cast<int32_t>(r));
        break;
      }
      case PGACCEL_EXPR_OP_ADD_I64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        // Detect overflow via sign check: same sign inputs, different sign result.
        int64_t av = a.data.i64, bv = b.data.i64;
        int64_t r = av + bv;
        if ((av > 0 && bv > 0 && r < 0) || (av < 0 && bv < 0 && r > 0)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(r);
        break;
      }
      case PGACCEL_EXPR_OP_ADD_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f32(a.data.f32 + b.data.f32);
        break;
      }
      case PGACCEL_EXPR_OP_ADD_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f64(a.data.f64 + b.data.f64);
        break;
      }

      case PGACCEL_EXPR_OP_SUB_I32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        int64_t r = static_cast<int64_t>(a.data.i32) - b.data.i32;
        if (r < INT32_MIN || r > INT32_MAX) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(static_cast<int32_t>(r));
        break;
      }
      case PGACCEL_EXPR_OP_SUB_I64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        int64_t av = a.data.i64, bv = b.data.i64;
        int64_t r = av - bv;
        if ((bv > 0 && av < INT64_MIN + bv) || (bv < 0 && av > INT64_MAX + bv)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(r);
        break;
      }
      case PGACCEL_EXPR_OP_SUB_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f32(a.data.f32 - b.data.f32);
        break;
      }
      case PGACCEL_EXPR_OP_SUB_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f64(a.data.f64 - b.data.f64);
        break;
      }

      case PGACCEL_EXPR_OP_MUL_I32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        int64_t r = static_cast<int64_t>(a.data.i32) * b.data.i32;
        if (r < INT32_MIN || r > INT32_MAX) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(static_cast<int32_t>(r));
        break;
      }
      case PGACCEL_EXPR_OP_MUL_I64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        // Overflow check: if b != 0 and a * b / b != a
        int64_t av = a.data.i64, bv = b.data.i64;
        if (bv != 0) {
          int64_t r = av * bv;
          if (r / bv != av) {
            uncertain = true;
            a = make_null();
            break;
          }
          a = make_i64(r);
        } else {
          a = make_i64(0);
        }
        break;
      }
      case PGACCEL_EXPR_OP_MUL_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f32(a.data.f32 * b.data.f32);
        break;
      }
      case PGACCEL_EXPR_OP_MUL_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f64(a.data.f64 * b.data.f64);
        break;
      }

      case PGACCEL_EXPR_OP_DIV_I32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        if (b.data.i32 == 0) {
          uncertain = true;
          a = make_null();
          break;
        }
        // INT32_MIN / -1 overflows
        if (a.data.i32 == INT32_MIN && b.data.i32 == -1) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(a.data.i32 / b.data.i32);
        break;
      }
      case PGACCEL_EXPR_OP_DIV_I64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        if (b.data.i64 == 0) {
          uncertain = true;
          a = make_null();
          break;
        }
        if (a.data.i64 == INT64_MIN && b.data.i64 == -1) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(a.data.i64 / b.data.i64);
        break;
      }
      case PGACCEL_EXPR_OP_DIV_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f32(a.data.f32 / b.data.f32);
        break;
      }
      case PGACCEL_EXPR_OP_DIV_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_f64(a.data.f64 / b.data.f64);
        break;
      }

      case PGACCEL_EXPR_OP_MOD_I32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        if (b.data.i32 == 0) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(a.data.i32 % b.data.i32);
        break;
      }
      case PGACCEL_EXPR_OP_MOD_I64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        if (b.data.i64 == 0) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(a.data.i64 % b.data.i64);
        break;
      }

        // ── Unary negate ────────────────────────────────────────────

      case PGACCEL_EXPR_OP_NEG_I32: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        if (a.data.i32 == INT32_MIN) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(-a.data.i32);
        break;
      }
      case PGACCEL_EXPR_OP_NEG_I64: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        if (a.data.i64 == INT64_MIN) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(-a.data.i64);
        break;
      }
      case PGACCEL_EXPR_OP_NEG_F32: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        a = make_f32(-a.data.f32);
        break;
      }
      case PGACCEL_EXPR_OP_NEG_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        a = make_f64(-a.data.f64);
        break;
      }

        // ── Comparison (pop 2, push bool) ───────────────────────────
        // Both operands promoted to f64 for comparison.
        // NULL propagates (any NULL input → NULL output).

      case PGACCEL_EXPR_OP_EQ: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(pg_float_eq_f64(da, db));
        break;
      }
      case PGACCEL_EXPR_OP_NE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(!pg_float_eq_f64(da, db));
        break;
      }
      case PGACCEL_EXPR_OP_LT: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(pg_float_lt_f64(da, db));
        break;
      }
      case PGACCEL_EXPR_OP_LE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(pg_float_lt_f64(da, db) || pg_float_eq_f64(da, db));
        break;
      }
      case PGACCEL_EXPR_OP_GT: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(pg_float_lt_f64(db, da));
        break;
      }
      case PGACCEL_EXPR_OP_GE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double da = val_to_f64(a), db = val_to_f64(b);
        a = make_bool(pg_float_lt_f64(db, da) || pg_float_eq_f64(da, db));
        break;
      }

        // ── Boolean logic (SQL three-valued) ────────────────────────

      case PGACCEL_EXPR_OP_AND: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        bool a_null = is_null(a), b_null = is_null(b);
        if (!a_null && !val_to_bool(a)) {
          // FALSE AND x = FALSE
          a = make_bool(false);
        } else if (!b_null && !val_to_bool(b)) {
          // x AND FALSE = FALSE
          a = make_bool(false);
        } else if (a_null || b_null) {
          a = make_null();
        } else {
          a = make_bool(val_to_bool(a) && val_to_bool(b));
        }
        break;
      }
      case PGACCEL_EXPR_OP_OR: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        bool a_null = is_null(a), b_null = is_null(b);
        if (!a_null && val_to_bool(a)) {
          // TRUE OR x = TRUE
          a = make_bool(true);
        } else if (!b_null && val_to_bool(b)) {
          // x OR TRUE = TRUE
          a = make_bool(true);
        } else if (a_null || b_null) {
          a = make_null();
        } else {
          a = make_bool(val_to_bool(a) || val_to_bool(b));
        }
        break;
      }
      case PGACCEL_EXPR_OP_NOT: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;  // NOT NULL = NULL
        a = make_bool(!val_to_bool(a));
        break;
      }

        // ── NULL tests ──────────────────────────────────────────────

      case PGACCEL_EXPR_OP_IS_NULL: {
        pgaccel_val& a = stack[sp - 1];
        a = make_bool(is_null(a));
        break;
      }
      case PGACCEL_EXPR_OP_IS_NOT_NULL: {
        pgaccel_val& a = stack[sp - 1];
        a = make_bool(!is_null(a));
        break;
      }

        // ── Type casts ──────────────────────────────────────────────

      case PGACCEL_EXPR_OP_CAST_I32_I64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_i64(static_cast<int64_t>(a.data.i32));
        break;
      }
      case PGACCEL_EXPR_OP_CAST_I32_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(static_cast<double>(a.data.i32));
        break;
      }
      case PGACCEL_EXPR_OP_CAST_I64_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(static_cast<double>(a.data.i64));
        break;
      }
      case PGACCEL_EXPR_OP_CAST_F32_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(static_cast<double>(a.data.f32));
        break;
      }
      case PGACCEL_EXPR_OP_CAST_F64_F32: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f32(static_cast<float>(a.data.f64));
        break;
      }
      case PGACCEL_EXPR_OP_CAST_BOOL_I32: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_i32(a.data.b ? 1 : 0);
        break;
      }

        // ── Math functions (unary) ──────────────────────────────────

      case PGACCEL_EXPR_OP_ABS_I32: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        if (a.data.i32 == INT32_MIN) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i32(a.data.i32 < 0 ? -a.data.i32 : a.data.i32);
        break;
      }
      case PGACCEL_EXPR_OP_ABS_I64: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        if (a.data.i64 == INT64_MIN) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(a.data.i64 < 0 ? -a.data.i64 : a.data.i64);
        break;
      }
      case PGACCEL_EXPR_OP_ABS_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        a = make_f64(fabs(a.data.f64));
        break;
      }
      case PGACCEL_EXPR_OP_SQRT_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          break;
        if (a.data.f64 < 0.0) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(sqrt(a.data.f64));
        break;
      }
      case PGACCEL_EXPR_OP_CEIL_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(ceil(a.data.f64));
        break;
      }
      case PGACCEL_EXPR_OP_FLOOR_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(floor(a.data.f64));
        break;
      }
      case PGACCEL_EXPR_OP_ROUND_F64: {
        pgaccel_val& a = stack[sp - 1];
        if (!is_null(a))
          a = make_f64(round(a.data.f64));
        break;
      }

        // ── Math functions (binary) ─────────────────────────────────

      case PGACCEL_EXPR_OP_POW_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        double r = pow(a.data.f64, b.data.f64);
        // pow can produce inf/nan for various edge cases — uncertain
        if (is_nan_f64(r) && !is_nan_f64(a.data.f64) && !is_nan_f64(b.data.f64)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(r);
        break;
      }

        // ── CASE WHEN (conditional jumps) ───────────────────────────

      case PGACCEL_EXPR_OP_JUMP_IF_FALSE: {
        pgaccel_val cond = stack[--sp];
        if (is_null(cond) || !val_to_bool(cond)) {
          pc = arg - 1;  // -1 because loop increments
        }
        break;
      }
      case PGACCEL_EXPR_OP_JUMP: {
        pc = arg - 1;
        break;
      }

        // ── COALESCE ────────────────────────────────────────────────

      case PGACCEL_EXPR_OP_COALESCE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a))
          a = b;
        break;
      }

      default:
        // Unknown opcode → uncertain
        uncertain = true;
        break;
    }

    // Stack overflow guard
    if (sp < 0 || static_cast<size_t>(sp) > MAX_STACK) {
      uncertain = true;
      break;
    }
  }

  eval_result result;
  result.value = (sp > 0) ? stack[sp - 1] : make_null();
  result.uncertain = uncertain;
  return result;
}

// ===========================================================================
// SYCL staging + kernel dispatch
// ===========================================================================
//
// The bytecode interpreter `eval_row` is per-row pure compute — every
// row evaluates the same program against its own slice of column data.
// That makes the row loop a textbook `sycl::parallel_for`: one
// work-item per row, each with its own MAX_STACK-deep local stack.
//
// Per CLAUDE.md rules #11/#12 — the previous host for-loop in the
// public entry points is replaced with a SYCL kernel. All inputs are
// staged into sycl::malloc_shared so the kernel has device-accessible
// pointers; PG's column data lives in palloc memory which is host-only
// on the Metal SSCP target. After the kernel completes we memcpy the
// staged result buffer back to the caller's output.

namespace {

// Owning bundle of staged shared-memory buffers for one
// (program, batch) dispatch. RAII-frees in destructor.
struct StagedExprDispatch {
  sycl::queue* q;
  pgaccel_expr_instruction* d_inst;  // staged instructions
  pgaccel_val* d_const_pool;         // staged constants (or nullptr)
  pgaccel_val_tag* d_col_types;      // staged column type tags
  void** d_col_data;                 // staged array of per-column data pointers
  uint8_t** d_col_nulls;   // staged array of per-column null mask pointers (or nullptr if no nulls
                           // anywhere)
  pgaccel_batch* d_batch;  // staged batch struct (with d_col_* pointers)
  pgaccel_expr_program* d_prog;  // staged program struct (with d_inst / d_const_pool)
  void** d_data_buffers;         // [num_cols] shared-mem copies of column data
  uint8_t** d_null_buffers;      // [num_cols] shared-mem copies of null masks
  size_t num_cols;

  ~StagedExprDispatch() {
    if (q == nullptr)
      return;
    if (d_data_buffers != nullptr) {
      for (size_t c = 0; c < num_cols; ++c) {
        if (d_data_buffers[c] != nullptr)
          sycl::free(d_data_buffers[c], *q);
      }
      std::free(d_data_buffers);
    }
    if (d_null_buffers != nullptr) {
      for (size_t c = 0; c < num_cols; ++c) {
        if (d_null_buffers[c] != nullptr)
          sycl::free(d_null_buffers[c], *q);
      }
      std::free(d_null_buffers);
    }
    if (d_inst)
      sycl::free(d_inst, *q);
    if (d_const_pool)
      sycl::free(d_const_pool, *q);
    if (d_col_types)
      sycl::free(d_col_types, *q);
    if (d_col_data)
      sycl::free(static_cast<void*>(d_col_data), *q);
    if (d_col_nulls)
      sycl::free(static_cast<void*>(d_col_nulls), *q);
    if (d_batch)
      sycl::free(d_batch, *q);
    if (d_prog)
      sycl::free(d_prog, *q);
  }
};

// Element size in bytes for a column type tag. Returns 0 for tags
// without a fixed-width device representation (we treat those as null).
static inline size_t elem_size_for_tag(pgaccel_val_tag tag) {
  switch (tag) {
    case PGACCEL_VAL_BOOL:
      return sizeof(bool);
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return sizeof(int32_t);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return sizeof(int64_t);
    case PGACCEL_VAL_FLOAT32:
      return sizeof(float);
    case PGACCEL_VAL_FLOAT64:
      return sizeof(double);
    default:
      return 0;
  }
}

// Stage program + batch into shared memory. Returns a heap-allocated
// StagedExprDispatch owning all the buffers (or nullptr on OOM /
// invalid input). Caller must `delete` the returned pointer.
static StagedExprDispatch* stage_dispatch(sycl::queue& q, const pgaccel_expr_program* program,
                                          const pgaccel_batch* batch) {
  auto* s = new (std::nothrow) StagedExprDispatch();
  if (s == nullptr)
    return nullptr;
  s->q = &q;
  s->num_cols = batch->num_cols;

  // Stage instruction stream.
  s->d_inst = sycl::malloc_shared<pgaccel_expr_instruction>(program->inst_count, q);
  if (s->d_inst == nullptr) {
    delete s;
    return nullptr;
  }
  std::memcpy(s->d_inst, program->instructions,
              program->inst_count * sizeof(pgaccel_expr_instruction));

  // Stage constant pool.
  if (program->const_count > 0) {
    s->d_const_pool = sycl::malloc_shared<pgaccel_val>(program->const_count, q);
    if (s->d_const_pool == nullptr) {
      delete s;
      return nullptr;
    }
    std::memcpy(s->d_const_pool, program->const_pool, program->const_count * sizeof(pgaccel_val));
  }

  // Stage program struct itself (with shared-mem pointers).
  s->d_prog = sycl::malloc_shared<pgaccel_expr_program>(1, q);
  if (s->d_prog == nullptr) {
    delete s;
    return nullptr;
  }
  s->d_prog->instructions = s->d_inst;
  s->d_prog->inst_count = program->inst_count;
  s->d_prog->const_pool = s->d_const_pool;
  s->d_prog->const_count = program->const_count;
  s->d_prog->max_stack = program->max_stack;
  s->d_prog->num_cols = program->num_cols;

  // Stage per-column type tags + data pointers + null pointers.
  if (batch->num_cols > 0) {
    s->d_col_types = sycl::malloc_shared<pgaccel_val_tag>(batch->num_cols, q);
    s->d_col_data = sycl::malloc_shared<void*>(batch->num_cols, q);
    s->d_col_nulls = sycl::malloc_shared<uint8_t*>(batch->num_cols, q);
    s->d_data_buffers = static_cast<void**>(std::calloc(batch->num_cols, sizeof(void*)));
    s->d_null_buffers = static_cast<uint8_t**>(std::calloc(batch->num_cols, sizeof(uint8_t*)));
    if (s->d_col_types == nullptr || s->d_col_data == nullptr || s->d_col_nulls == nullptr ||
        s->d_data_buffers == nullptr || s->d_null_buffers == nullptr) {
      delete s;
      return nullptr;
    }

    for (size_t c = 0; c < batch->num_cols; ++c) {
      s->d_col_types[c] = batch->col_types[c];

      // Stage column data, sized by the type tag.
      size_t esz = elem_size_for_tag(batch->col_types[c]);
      if (batch->col_data[c] != nullptr && esz > 0 && batch->num_rows > 0) {
        void* buf = sycl::malloc_shared(batch->num_rows * esz, q);
        if (buf == nullptr) {
          delete s;
          return nullptr;
        } else {
          std::memcpy(buf, batch->col_data[c], batch->num_rows * esz);
          s->d_col_data[c] = buf;
          s->d_data_buffers[c] = buf;
        }
      } else {
        s->d_col_data[c] = nullptr;
        s->d_data_buffers[c] = nullptr;
      }

      // Stage null mask if present.
      if (batch->col_nulls != nullptr && batch->col_nulls[c] != nullptr && batch->num_rows > 0) {
        uint8_t* nbuf = sycl::malloc_shared<uint8_t>(batch->num_rows, q);
        if (nbuf == nullptr) {
          delete s;
          return nullptr;
        } else {
          std::memcpy(nbuf, batch->col_nulls[c], batch->num_rows);
          s->d_col_nulls[c] = nbuf;
          s->d_null_buffers[c] = nbuf;
        }
      } else {
        s->d_col_nulls[c] = nullptr;
        s->d_null_buffers[c] = nullptr;
      }
    }
  }

  // Stage batch struct itself.
  s->d_batch = sycl::malloc_shared<pgaccel_batch>(1, q);
  if (s->d_batch == nullptr) {
    delete s;
    return nullptr;
  }
  s->d_batch->num_rows = batch->num_rows;
  s->d_batch->num_cols = batch->num_cols;
  s->d_batch->col_data = s->d_col_data;
  s->d_batch->col_nulls = s->d_col_nulls;
  s->d_batch->col_types = s->d_col_types;
  return s;
}

}  // namespace

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_expr_eval_predicate(const pgaccel_expr_program* program,
                                           const pgaccel_batch* batch, int8_t* results) {
  if (program == nullptr || batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (batch->num_rows == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  StagedExprDispatch* s = stage_dispatch(*q, program, batch);
  if (s == nullptr)
    return PGACCEL_OOM;

  int8_t* d_results = sycl::malloc_shared<int8_t>(batch->num_rows, *q);
  if (d_results == nullptr) {
    delete s;
    return PGACCEL_OOM;
  }

  pgaccel_expr_program* d_prog = s->d_prog;
  pgaccel_batch* d_batch = s->d_batch;

  q->parallel_for(sycl::range<1>(batch->num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row(d_prog, d_batch, row);
     if (er.uncertain) {
       d_results[row] = PGACCEL_EXPR_UNCERTAIN;
     } else if (is_null(er.value)) {
       d_results[row] = PGACCEL_EXPR_FALSE;
     } else if (val_to_bool(er.value)) {
       d_results[row] = PGACCEL_EXPR_TRUE;
     } else {
       d_results[row] = PGACCEL_EXPR_FALSE;
     }
   }).wait();

  std::memcpy(results, d_results, batch->num_rows * sizeof(int8_t));
  sycl::free(d_results, *q);
  delete s;
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

pgaccel_status pgaccel_expr_eval_project(const pgaccel_expr_program* program,
                                         const pgaccel_batch* batch, pgaccel_val* output,
                                         uint8_t* uncertain_mask) {
  if (program == nullptr || batch == nullptr || output == nullptr)
    return PGACCEL_ERROR;
  if (batch->num_rows == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  StagedExprDispatch* s = stage_dispatch(*q, program, batch);
  if (s == nullptr)
    return PGACCEL_OOM;

  pgaccel_val* d_output = sycl::malloc_shared<pgaccel_val>(batch->num_rows, *q);
  uint8_t* d_uncertain = sycl::malloc_shared<uint8_t>(batch->num_rows, *q);
  if (d_output == nullptr || d_uncertain == nullptr) {
    if (d_output)
      sycl::free(d_output, *q);
    if (d_uncertain)
      sycl::free(d_uncertain, *q);
    delete s;
    return PGACCEL_OOM;
  }

  pgaccel_expr_program* d_prog = s->d_prog;
  pgaccel_batch* d_batch = s->d_batch;

  q->parallel_for(sycl::range<1>(batch->num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row(d_prog, d_batch, row);
     d_output[row] = er.value;
     d_uncertain[row] = er.uncertain ? 1 : 0;
   }).wait();

  std::memcpy(output, d_output, batch->num_rows * sizeof(pgaccel_val));
  if (uncertain_mask != nullptr)
    std::memcpy(uncertain_mask, d_uncertain, batch->num_rows);

  sycl::free(d_output, *q);
  sycl::free(d_uncertain, *q);
  delete s;
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

}  // extern "C"

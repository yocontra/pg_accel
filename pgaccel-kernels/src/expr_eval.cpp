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
#include <memory>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

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

static inline pgaccel_val make_date(int32_t x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_DATE;
  v.data.i32 = x;
  return v;
}

static inline pgaccel_val make_timestamp(int64_t x) {
  pgaccel_val v;
  v.tag = PGACCEL_VAL_TIMESTAMP;
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

static inline bool is_finite_f32(float x) {
  const uint32_t bits = sycl::bit_cast<uint32_t>(x);
  return (bits & 0x7fffffffU) < 0x7f800000U;
}
static inline bool is_nan_f64(double x) {
  return x != x;
}

static inline bool is_inf_f64(double x) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(x);
  return (bits & 0x7fffffffffffffffULL) == 0x7ff0000000000000ULL;
}

static inline bool is_finite_f64(double x) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(x);
  return (bits & 0x7fffffffffffffffULL) < 0x7ff0000000000000ULL;
}

static inline bool is_negative_f64(double x) {
  return (sycl::bit_cast<uint64_t>(x) >> 63) != 0;
}

static inline bool is_odd_integer_f64(double x) {
  const uint64_t magnitude = sycl::bit_cast<uint64_t>(x) & 0x7fffffffffffffffULL;
  const uint64_t exponent_bits = magnitude >> 52;
  if (exponent_bits == 0 || exponent_bits == 0x7ff)
    return false;

  const int exponent = static_cast<int>(exponent_bits) - 1023;
  if (exponent < 0 || exponent > 52)
    return false;

  const uint64_t significand = (1ULL << 52) | (magnitude & 0x000fffffffffffffULL);
  const int fractional_bits = 52 - exponent;
  const uint64_t fractional_mask = (1ULL << fractional_bits) - 1;
  return (significand & fractional_mask) == 0 && ((significand >> fractional_bits) & 1ULL) != 0;
}

static inline bool generated_nonfinite_f32(float a, float b, float result) {
  return is_finite_f32(a) && is_finite_f32(b) && !is_finite_f32(result);
}

static inline bool generated_nonfinite_f64(double a, double b, double result) {
  return is_finite_f64(a) && is_finite_f64(b) && !is_finite_f64(result);
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

static inline bool is_numeric_integer(pgaccel_val_tag tag) {
  return tag == PGACCEL_VAL_INT32 || tag == PGACCEL_VAL_INT64;
}

static inline int64_t numeric_integer_value(const pgaccel_val& value) {
  return value.tag == PGACCEL_VAL_INT32 ? static_cast<int64_t>(value.data.i32) : value.data.i64;
}

static inline bool exact_integer_pair(const pgaccel_val& a, const pgaccel_val& b, int64_t& av,
                                      int64_t& bv) {
  if (is_numeric_integer(a.tag) && is_numeric_integer(b.tag)) {
    av = numeric_integer_value(a);
    bv = numeric_integer_value(b);
    return true;
  }
  if (a.tag == PGACCEL_VAL_DATE && b.tag == PGACCEL_VAL_DATE) {
    av = static_cast<int64_t>(a.data.i32);
    bv = static_cast<int64_t>(b.data.i32);
    return true;
  }
  if (a.tag == PGACCEL_VAL_TIMESTAMP && b.tag == PGACCEL_VAL_TIMESTAMP) {
    av = a.data.i64;
    bv = b.data.i64;
    return true;
  }
  return false;
}

/// Convert values without an exact integer comparison path to f64.
static inline double val_to_f64(const pgaccel_val& v) {
  switch (v.tag) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return static_cast<double>(v.data.i32);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
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

static inline bool pg_value_eq(const pgaccel_val& a, const pgaccel_val& b) {
  int64_t av;
  int64_t bv;
  if (exact_integer_pair(a, b, av, bv))
    return av == bv;
  return pg_float_eq_f64(val_to_f64(a), val_to_f64(b));
}

static inline bool pg_value_lt(const pgaccel_val& a, const pgaccel_val& b) {
  int64_t av;
  int64_t bv;
  if (exact_integer_pair(a, b, av, bv))
    return av < bv;
  return pg_float_lt_f64(val_to_f64(a), val_to_f64(b));
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
    case PGACCEL_VAL_BOOL:
      return make_bool(static_cast<const bool*>(data)[row]);
    case PGACCEL_VAL_INT32:
      return make_i32(static_cast<const int32_t*>(data)[row]);
    case PGACCEL_VAL_INT64:
      return make_i64(static_cast<const int64_t*>(data)[row]);
    case PGACCEL_VAL_FLOAT32:
      return make_f32(static_cast<const float*>(data)[row]);
    case PGACCEL_VAL_FLOAT64:
      return make_f64(static_cast<const double*>(data)[row]);
    case PGACCEL_VAL_DATE:
      return make_date(static_cast<const int32_t*>(data)[row]);
    case PGACCEL_VAL_TIMESTAMP:
      return make_timestamp(static_cast<const int64_t*>(data)[row]);
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

// Minimal production interpreter for the common "all required columns are
// present" predicate shape. Keeping this as a separate kernel capability tier
// avoids compiling the full expression VM when the bytecode can only load
// columns, test them for NULL, and combine the boolean results.
static eval_result eval_row_basic(const pgaccel_expr_program* prog, const pgaccel_batch* batch,
                                  size_t row) {
  pgaccel_val stack[MAX_STACK];
  int sp = 0;
  bool uncertain = false;

  for (size_t pc = 0; pc < prog->inst_count; ++pc) {
    const pgaccel_expr_instruction& inst = prog->instructions[pc];
    switch (inst.opcode) {
      case PGACCEL_EXPR_OP_LOAD_COL:
        if (static_cast<size_t>(sp) == MAX_STACK) {
          uncertain = true;
          break;
        }
        stack[sp++] = load_column(batch, row, inst.arg);
        break;

      case PGACCEL_EXPR_OP_IS_NOT_NULL:
        if (sp < 1) {
          uncertain = true;
          break;
        }
        stack[sp - 1] = make_bool(!is_null(stack[sp - 1]));
        break;

      case PGACCEL_EXPR_OP_AND: {
        if (sp < 2) {
          uncertain = true;
          break;
        }
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        const bool a_null = is_null(a);
        const bool b_null = is_null(b);
        if ((!a_null && !val_to_bool(a)) || (!b_null && !val_to_bool(b))) {
          a = make_bool(false);
        } else if (a_null || b_null) {
          a = make_null();
        } else {
          a = make_bool(val_to_bool(a) && val_to_bool(b));
        }
        break;
      }
    }

    if (uncertain || sp < 0 || static_cast<size_t>(sp) > MAX_STACK) {
      uncertain = true;
      break;
    }
  }

  eval_result result;
  result.value = (sp > 0) ? stack[sp - 1] : make_null();
  result.uncertain = uncertain;
  return result;
}

template <bool EnableExtendedMath>
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
        int64_t av = a.data.i64, bv = b.data.i64;
        if ((bv > 0 && av > INT64_MAX - bv) || (bv < 0 && av < INT64_MIN - bv)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(av + bv);
        break;
      }
      case PGACCEL_EXPR_OP_ADD_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const float r = a.data.f32 + b.data.f32;
        if (generated_nonfinite_f32(a.data.f32, b.data.f32, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f32(r);
        break;
      }
      case PGACCEL_EXPR_OP_ADD_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const double r = a.data.f64 + b.data.f64;
        if (generated_nonfinite_f64(a.data.f64, b.data.f64, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(r);
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
        if ((bv > 0 && av < INT64_MIN + bv) || (bv < 0 && av > INT64_MAX + bv)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(av - bv);
        break;
      }
      case PGACCEL_EXPR_OP_SUB_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const float r = a.data.f32 - b.data.f32;
        if (generated_nonfinite_f32(a.data.f32, b.data.f32, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f32(r);
        break;
      }
      case PGACCEL_EXPR_OP_SUB_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const double r = a.data.f64 - b.data.f64;
        if (generated_nonfinite_f64(a.data.f64, b.data.f64, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(r);
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
        int64_t av = a.data.i64, bv = b.data.i64;
        if (av == 0 || bv == 0) {
          a = make_i64(0);
          break;
        }
        const bool overflow =
            (av > 0 && ((bv > 0 && av > INT64_MAX / bv) || (bv < 0 && bv < INT64_MIN / av))) ||
            (av < 0 && ((bv > 0 && av < INT64_MIN / bv) || (bv < 0 && av < INT64_MAX / bv)));
        if (overflow) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_i64(av * bv);
        break;
      }
      case PGACCEL_EXPR_OP_MUL_F32: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const float r = a.data.f32 * b.data.f32;
        if (generated_nonfinite_f32(a.data.f32, b.data.f32, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f32(r);
        break;
      }
      case PGACCEL_EXPR_OP_MUL_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        const double r = a.data.f64 * b.data.f64;
        if (generated_nonfinite_f64(a.data.f64, b.data.f64, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(r);
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
        if (b.data.f32 == 0.0f) {
          uncertain = true;
          a = make_null();
          break;
        }
        const float r = a.data.f32 / b.data.f32;
        if (generated_nonfinite_f32(a.data.f32, b.data.f32, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f32(r);
        break;
      }
      case PGACCEL_EXPR_OP_DIV_F64: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        if (b.data.f64 == 0.0) {
          uncertain = true;
          a = make_null();
          break;
        }
        const double r = a.data.f64 / b.data.f64;
        if (generated_nonfinite_f64(a.data.f64, b.data.f64, r)) {
          uncertain = true;
          a = make_null();
          break;
        }
        a = make_f64(r);
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
        if (a.data.i32 == INT32_MIN && b.data.i32 == -1) {
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
        if (a.data.i64 == INT64_MIN && b.data.i64 == -1) {
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
        // Integer and temporal domains compare exactly; floats retain PG NaN semantics.
        // NULL propagates (any NULL input → NULL output).

      case PGACCEL_EXPR_OP_EQ: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(pg_value_eq(a, b));
        break;
      }
      case PGACCEL_EXPR_OP_NE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(!pg_value_eq(a, b));
        break;
      }
      case PGACCEL_EXPR_OP_LT: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(pg_value_lt(a, b));
        break;
      }
      case PGACCEL_EXPR_OP_LE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(pg_value_lt(a, b) || pg_value_eq(a, b));
        break;
      }
      case PGACCEL_EXPR_OP_GT: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(pg_value_lt(b, a));
        break;
      }
      case PGACCEL_EXPR_OP_GE: {
        pgaccel_val b = stack[--sp];
        pgaccel_val& a = stack[sp - 1];
        if (is_null(a) || is_null(b)) {
          a = make_null();
          break;
        }
        a = make_bool(pg_value_lt(b, a) || pg_value_eq(a, b));
        break;
      }

        // ── Boolean logic (SQL three-valued) ────────────────────────

      case PGACCEL_EXPR_OP_ALWAYS_TRUE:
        stack[sp++] = make_bool(true);
        break;

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
        // program_tier routes these opcodes only to the extended specialization.
      case PGACCEL_EXPR_OP_SQRT_F64: {
        if constexpr (EnableExtendedMath) {
          pgaccel_val& a = stack[sp - 1];
          if (is_null(a))
            break;
          if (a.data.f64 < 0.0) {
            uncertain = true;
            a = make_null();
            break;
          }
          a = make_f64(sqrt(a.data.f64));
        }
        break;
      }
      case PGACCEL_EXPR_OP_CEIL_F64: {
        if constexpr (EnableExtendedMath) {
          pgaccel_val& a = stack[sp - 1];
          if (!is_null(a))
            a = make_f64(ceil(a.data.f64));
        }
        break;
      }
      case PGACCEL_EXPR_OP_FLOOR_F64: {
        if constexpr (EnableExtendedMath) {
          pgaccel_val& a = stack[sp - 1];
          if (!is_null(a))
            a = make_f64(floor(a.data.f64));
        }
        break;
      }
      case PGACCEL_EXPR_OP_ROUND_F64: {
        if constexpr (EnableExtendedMath) {
          pgaccel_val& a = stack[sp - 1];
          if (!is_null(a)) {
            const double value = a.data.f64;
            const double rounded = value == 0.0  ? value
                                   : value > 0.0 ? floor(value + 0.5)
                                                 : ceil(value - 0.5);
            a = make_f64(rounded);
          }
        }
        break;
      }

        // ── Math functions (binary) ─────────────────────────────────

      case PGACCEL_EXPR_OP_POW_F64: {
        if constexpr (EnableExtendedMath) {
          pgaccel_val b = stack[--sp];
          pgaccel_val& a = stack[sp - 1];
          if (is_null(a) || is_null(b)) {
            a = make_null();
            break;
          }
          const double base = a.data.f64;
          const double exponent = b.data.f64;
          double r;
          if (is_inf_f64(base) && is_negative_f64(base) && !is_nan_f64(exponent)) {
            if (exponent == 0.0) {
              r = 1.0;
            } else if (exponent > 0.0) {
              r = is_odd_integer_f64(exponent) ? base : -base;
            } else {
              r = is_odd_integer_f64(exponent) ? -0.0 : 0.0;
            }
          } else {
            r = pow(base, exponent);
          }
          if (generated_nonfinite_f64(a.data.f64, b.data.f64, r)) {
            uncertain = true;
            a = make_null();
            break;
          }
          a = make_f64(r);
        }
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

static void free_usm_noexcept(void* pointer, sycl::queue& queue, const char* owner) noexcept {
  if (pointer == nullptr)
    return;
  try {
    sycl::free(pointer, queue);
  } catch (const std::exception& error) {
    std::fprintf(stderr, "pgaccel: %s: failed to free SYCL allocation: %s\n", owner, error.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: failed to free SYCL allocation: unknown exception\n", owner);
  }
}

// Owning bundle of staged shared-memory buffers for one
// (program, batch) dispatch. RAII-frees in destructor.
struct StagedExprDispatch {
  explicit StagedExprDispatch(sycl::queue& queue) : q(queue) {}

  sycl::queue& q;
  pgaccel_expr_instruction* d_inst{};  // staged instructions
  pgaccel_val* d_const_pool{};         // staged constants (or nullptr)
  pgaccel_val_tag* d_col_types{};      // staged column type tags
  void** d_col_data{};                 // staged array of per-column data pointers
  uint8_t** d_col_nulls{};  // staged array of per-column null mask pointers (or nullptr if no nulls
                            // anywhere)
  pgaccel_batch* d_batch{};        // staged batch struct (with d_col_* pointers)
  pgaccel_expr_program* d_prog{};  // staged program struct (with d_inst / d_const_pool)
  void** d_data_buffers{};         // [num_cols] shared-mem copies of column data
  uint8_t** d_null_buffers{};      // [num_cols] shared-mem copies of null masks
  size_t num_cols{};

  ~StagedExprDispatch() noexcept {
    if (d_data_buffers != nullptr) {
      for (size_t c = 0; c < num_cols; ++c)
        free_usm_noexcept(d_data_buffers[c], q, "StagedExprDispatch");
      std::free(d_data_buffers);
    }
    if (d_null_buffers != nullptr) {
      for (size_t c = 0; c < num_cols; ++c)
        free_usm_noexcept(d_null_buffers[c], q, "StagedExprDispatch");
      std::free(d_null_buffers);
    }
    free_usm_noexcept(d_inst, q, "StagedExprDispatch");
    free_usm_noexcept(d_const_pool, q, "StagedExprDispatch");
    free_usm_noexcept(d_col_types, q, "StagedExprDispatch");
    free_usm_noexcept(static_cast<void*>(d_col_data), q, "StagedExprDispatch");
    free_usm_noexcept(static_cast<void*>(d_col_nulls), q, "StagedExprDispatch");
    free_usm_noexcept(d_batch, q, "StagedExprDispatch");
    free_usm_noexcept(d_prog, q, "StagedExprDispatch");
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

// Stage program + batch into shared memory. Returns nullptr on OOM.
static std::unique_ptr<StagedExprDispatch>
stage_dispatch(sycl::queue& q, const pgaccel_expr_program* program, const pgaccel_batch* batch) {
  auto s = std::unique_ptr<StagedExprDispatch>(new (std::nothrow) StagedExprDispatch(q));
  if (s == nullptr)
    return nullptr;
  s->num_cols = batch->num_cols;

  // Stage instruction stream.
  s->d_inst = sycl::malloc_shared<pgaccel_expr_instruction>(program->inst_count, q);
  if (s->d_inst == nullptr)
    return nullptr;
  std::memcpy(s->d_inst, static_cast<const void*>(program->instructions),
              program->inst_count * sizeof(pgaccel_expr_instruction));

  // Stage constant pool.
  if (program->const_count > 0) {
    s->d_const_pool = sycl::malloc_shared<pgaccel_val>(program->const_count, q);
    if (s->d_const_pool == nullptr)
      return nullptr;
    std::memcpy(s->d_const_pool, static_cast<const void*>(program->const_pool),
                program->const_count * sizeof(pgaccel_val));
  }

  // Stage program struct itself (with shared-mem pointers).
  s->d_prog = sycl::malloc_shared<pgaccel_expr_program>(1, q);
  if (s->d_prog == nullptr)
    return nullptr;
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
        s->d_data_buffers == nullptr || s->d_null_buffers == nullptr)
      return nullptr;

    for (size_t c = 0; c < batch->num_cols; ++c) {
      const pgaccel_val_tag column_type = batch->col_types[c];
      s->d_col_types[c] = column_type;

      // Stage column data, sized by the type tag.
      size_t esz = elem_size_for_tag(column_type);
      if (batch->col_data[c] != nullptr && esz > 0 && batch->num_rows > 0) {
        void* buf = sycl::malloc_shared(batch->num_rows * esz, q);
        if (buf == nullptr)
          return nullptr;
        std::memcpy(buf, static_cast<const void*>(batch->col_data[c]), batch->num_rows * esz);
        s->d_col_data[c] = buf;
        s->d_data_buffers[c] = buf;
      } else {
        s->d_col_data[c] = nullptr;
        s->d_data_buffers[c] = nullptr;
      }

      // Stage null mask if present.
      if (batch->col_nulls != nullptr && batch->col_nulls[c] != nullptr && batch->num_rows > 0) {
        uint8_t* nbuf = sycl::malloc_shared<uint8_t>(batch->num_rows, q);
        if (nbuf == nullptr)
          return nullptr;
        std::memcpy(nbuf, static_cast<const void*>(batch->col_nulls[c]), batch->num_rows);
        s->d_col_nulls[c] = nbuf;
        s->d_null_buffers[c] = nbuf;
      } else {
        s->d_col_nulls[c] = nullptr;
        s->d_null_buffers[c] = nullptr;
      }
    }
  }

  // Stage batch struct itself.
  s->d_batch = sycl::malloc_shared<pgaccel_batch>(1, q);
  if (s->d_batch == nullptr)
    return nullptr;
  s->d_batch->num_rows = batch->num_rows;
  s->d_batch->num_cols = batch->num_cols;
  s->d_batch->col_data = s->d_col_data;
  s->d_batch->col_nulls = s->d_col_nulls;
  s->d_batch->col_types = s->d_col_types;
  return s;
}

// RAII guard for a USM allocation so every error/exception path in
// the extern "C" entry points below frees staged buffers exactly once.
struct UsmGuard {
  sycl::queue& q;
  void* p;
  ~UsmGuard() noexcept { free_usm_noexcept(p, q, "UsmGuard"); }
};

enum class ExprProgramTier {
  Basic,
  Common,
  Extended,
};

static ExprProgramTier program_tier(const pgaccel_expr_program* program) {
  bool basic = true;
  for (size_t pc = 0; pc < program->inst_count; ++pc) {
    switch (program->instructions[pc].opcode) {
      case PGACCEL_EXPR_OP_SQRT_F64:
      case PGACCEL_EXPR_OP_CEIL_F64:
      case PGACCEL_EXPR_OP_FLOOR_F64:
      case PGACCEL_EXPR_OP_ROUND_F64:
      case PGACCEL_EXPR_OP_POW_F64:
        return ExprProgramTier::Extended;
      case PGACCEL_EXPR_OP_LOAD_COL:
      case PGACCEL_EXPR_OP_IS_NOT_NULL:
      case PGACCEL_EXPR_OP_AND:
        break;
      default:
        basic = false;
        break;
    }
  }
  return basic ? ExprProgramTier::Basic : ExprProgramTier::Common;
}

struct StackEffect {
  uint8_t inputs;
  uint8_t outputs;
};

static StackEffect stack_effect(uint16_t opcode) {
  switch (opcode) {
    case PGACCEL_EXPR_OP_LOAD_COL:
    case PGACCEL_EXPR_OP_LOAD_CONST:
    case PGACCEL_EXPR_OP_LOAD_NULL:
    case PGACCEL_EXPR_OP_ALWAYS_TRUE:
      return {0, 1};

    case PGACCEL_EXPR_OP_NEG_I32:
    case PGACCEL_EXPR_OP_NEG_I64:
    case PGACCEL_EXPR_OP_NEG_F32:
    case PGACCEL_EXPR_OP_NEG_F64:
    case PGACCEL_EXPR_OP_NOT:
    case PGACCEL_EXPR_OP_IS_NULL:
    case PGACCEL_EXPR_OP_IS_NOT_NULL:
    case PGACCEL_EXPR_OP_CAST_I32_I64:
    case PGACCEL_EXPR_OP_CAST_I32_F64:
    case PGACCEL_EXPR_OP_CAST_I64_F64:
    case PGACCEL_EXPR_OP_CAST_F32_F64:
    case PGACCEL_EXPR_OP_CAST_F64_F32:
    case PGACCEL_EXPR_OP_CAST_BOOL_I32:
    case PGACCEL_EXPR_OP_ABS_I32:
    case PGACCEL_EXPR_OP_ABS_I64:
    case PGACCEL_EXPR_OP_ABS_F64:
    case PGACCEL_EXPR_OP_SQRT_F64:
    case PGACCEL_EXPR_OP_CEIL_F64:
    case PGACCEL_EXPR_OP_FLOOR_F64:
    case PGACCEL_EXPR_OP_ROUND_F64:
      return {1, 1};

    case PGACCEL_EXPR_OP_ADD_I32:
    case PGACCEL_EXPR_OP_ADD_I64:
    case PGACCEL_EXPR_OP_ADD_F32:
    case PGACCEL_EXPR_OP_ADD_F64:
    case PGACCEL_EXPR_OP_SUB_I32:
    case PGACCEL_EXPR_OP_SUB_I64:
    case PGACCEL_EXPR_OP_SUB_F32:
    case PGACCEL_EXPR_OP_SUB_F64:
    case PGACCEL_EXPR_OP_MUL_I32:
    case PGACCEL_EXPR_OP_MUL_I64:
    case PGACCEL_EXPR_OP_MUL_F32:
    case PGACCEL_EXPR_OP_MUL_F64:
    case PGACCEL_EXPR_OP_DIV_I32:
    case PGACCEL_EXPR_OP_DIV_I64:
    case PGACCEL_EXPR_OP_DIV_F32:
    case PGACCEL_EXPR_OP_DIV_F64:
    case PGACCEL_EXPR_OP_MOD_I32:
    case PGACCEL_EXPR_OP_MOD_I64:
    case PGACCEL_EXPR_OP_EQ:
    case PGACCEL_EXPR_OP_NE:
    case PGACCEL_EXPR_OP_LT:
    case PGACCEL_EXPR_OP_LE:
    case PGACCEL_EXPR_OP_GT:
    case PGACCEL_EXPR_OP_GE:
    case PGACCEL_EXPR_OP_AND:
    case PGACCEL_EXPR_OP_OR:
    case PGACCEL_EXPR_OP_POW_F64:
    case PGACCEL_EXPR_OP_COALESCE:
      return {2, 1};

    case PGACCEL_EXPR_OP_JUMP_IF_FALSE:
      return {1, 0};
    case PGACCEL_EXPR_OP_JUMP:
    default:
      return {0, 0};
  }
}

// Common and Extended programs can contain arbitrary VM instructions. Validate
// their forward-only control-flow graph before dispatch so device code never
// observes an invalid stack index or a non-terminating jump.
static pgaccel_status validate_device_program(const pgaccel_expr_program* program) noexcept {
  if (program->inst_count == SIZE_MAX)
    return PGACCEL_ERROR;
  try {
    std::vector<int16_t> depths(program->inst_count + 1, -1);
    depths[0] = 0;

    const auto propagate = [&depths](size_t pc, int16_t depth) {
      if (depths[pc] >= 0 && depths[pc] != depth)
        return false;
      depths[pc] = depth;
      return true;
    };

    for (size_t pc = 0; pc < program->inst_count; ++pc) {
      const pgaccel_expr_instruction& inst = program->instructions[pc];
      const bool conditional_jump = inst.opcode == PGACCEL_EXPR_OP_JUMP_IF_FALSE;
      const bool unconditional_jump = inst.opcode == PGACCEL_EXPR_OP_JUMP;
      if ((conditional_jump || unconditional_jump) &&
          (static_cast<size_t>(inst.arg) <= pc ||
           static_cast<size_t>(inst.arg) > program->inst_count))
        return PGACCEL_ERROR;

      if (depths[pc] < 0)
        continue;

      const StackEffect effect = stack_effect(inst.opcode);
      if (depths[pc] < static_cast<int16_t>(effect.inputs))
        return PGACCEL_ERROR;
      const int16_t next_depth =
          depths[pc] - static_cast<int16_t>(effect.inputs) + static_cast<int16_t>(effect.outputs);
      if (next_depth > static_cast<int16_t>(MAX_STACK))
        return PGACCEL_ERROR;

      if (unconditional_jump) {
        if (!propagate(inst.arg, next_depth))
          return PGACCEL_ERROR;
        continue;
      }
      if (conditional_jump && !propagate(inst.arg, next_depth))
        return PGACCEL_ERROR;
      if (!propagate(pc + 1, next_depth))
        return PGACCEL_ERROR;
    }
    return PGACCEL_OK;
  } catch (...) {
    return PGACCEL_OOM;
  }
}

static bool has_valid_host_layout(const pgaccel_expr_program* program, const pgaccel_batch* batch) {
  if (program->inst_count > 0 && program->instructions == nullptr)
    return false;
  if (program->const_count > 0 && program->const_pool == nullptr)
    return false;
  if (batch->num_cols > 0 && (batch->col_data == nullptr || batch->col_types == nullptr))
    return false;
  return true;
}

template <bool EnableExtendedMath>
static void submit_predicate_kernel(sycl::queue& q, size_t num_rows, pgaccel_expr_program* program,
                                    pgaccel_batch* batch, int8_t* results) {
  q.parallel_for(sycl::range<1>(num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row<EnableExtendedMath>(program, batch, row);
     if (er.uncertain) {
       results[row] = PGACCEL_EXPR_UNCERTAIN;
     } else if (is_null(er.value)) {
       results[row] = PGACCEL_EXPR_FALSE;
     } else if (val_to_bool(er.value)) {
       results[row] = PGACCEL_EXPR_TRUE;
     } else {
       results[row] = PGACCEL_EXPR_FALSE;
     }
   }).wait_and_throw();
}

static void submit_basic_predicate_kernel(sycl::queue& q, size_t num_rows,
                                          pgaccel_expr_program* program, pgaccel_batch* batch,
                                          int8_t* results) {
  q.parallel_for(sycl::range<1>(num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row_basic(program, batch, row);
     if (er.uncertain) {
       results[row] = PGACCEL_EXPR_UNCERTAIN;
     } else if (is_null(er.value)) {
       results[row] = PGACCEL_EXPR_FALSE;
     } else if (val_to_bool(er.value)) {
       results[row] = PGACCEL_EXPR_TRUE;
     } else {
       results[row] = PGACCEL_EXPR_FALSE;
     }
   }).wait_and_throw();
}

template <bool EnableExtendedMath>
static void submit_project_kernel(sycl::queue& q, size_t num_rows, pgaccel_expr_program* program,
                                  pgaccel_batch* batch, pgaccel_val* output,
                                  uint8_t* uncertain_mask) {
  q.parallel_for(sycl::range<1>(num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row<EnableExtendedMath>(program, batch, row);
     output[row] = er.value;
     uncertain_mask[row] = er.uncertain ? 1 : 0;
   }).wait_and_throw();
}

static void submit_basic_project_kernel(sycl::queue& q, size_t num_rows,
                                        pgaccel_expr_program* program, pgaccel_batch* batch,
                                        pgaccel_val* output, uint8_t* uncertain_mask) {
  q.parallel_for(sycl::range<1>(num_rows), [=](sycl::id<1> id) {
     const size_t row = id[0];
     eval_result er = eval_row_basic(program, batch, row);
     output[row] = er.value;
     uncertain_mask[row] = er.uncertain ? 1 : 0;
   }).wait_and_throw();
}

}  // namespace

// ===========================================================================
// Public C API
// ===========================================================================
//
// Both entry points cross the C ABI into Rust: an escaping C++ exception
// here is std::terminate → backend SIGABRT. Everything that can throw
// (USM allocation, kernel submission, wait_and_throw) runs inside the
// try/catch; staged buffers are RAII-owned so the error paths leak nothing.

extern "C" {

pgaccel_status pgaccel_expr_eval_predicate(const pgaccel_expr_program* program,
                                           const pgaccel_batch* batch, int8_t* results) {
  if (program == nullptr || batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (!has_valid_host_layout(program, batch))
    return PGACCEL_ERROR;
  if (batch->num_rows == 0)
    return PGACCEL_OK;

  const ExprProgramTier tier = program_tier(program);
  if (tier != ExprProgramTier::Basic) {
    const pgaccel_status validation_status = validate_device_program(program);
    if (validation_status != PGACCEL_OK)
      return validation_status;
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr) {
    static_assert(PGACCEL_EXPR_UNCERTAIN == 0);
    std::memset(static_cast<void*>(results), 0, batch->num_rows * sizeof(int8_t));
    return PGACCEL_ERROR_NO_DEVICE;
  }

  try {
    auto s = stage_dispatch(*q, program, batch);
    if (s == nullptr) {
      std::memset(static_cast<void*>(results), 0, batch->num_rows * sizeof(int8_t));
      return PGACCEL_OOM;
    }

    int8_t* d_results = sycl::malloc_device<int8_t>(batch->num_rows, *q);
    UsmGuard results_guard{*q, d_results};
    if (d_results == nullptr) {
      std::memset(static_cast<void*>(results), 0, batch->num_rows * sizeof(int8_t));
      return PGACCEL_OOM;
    }

    pgaccel_expr_program* d_prog = s->d_prog;
    pgaccel_batch* d_batch = s->d_batch;

    switch (tier) {
      case ExprProgramTier::Basic:
        submit_basic_predicate_kernel(*q, batch->num_rows, d_prog, d_batch, d_results);
        break;
      case ExprProgramTier::Common:
        submit_predicate_kernel<false>(*q, batch->num_rows, d_prog, d_batch, d_results);
        break;
      case ExprProgramTier::Extended:
        submit_predicate_kernel<true>(*q, batch->num_rows, d_prog, d_batch, d_results);
        break;
    }

    q->memcpy(results, d_results, batch->num_rows * sizeof(int8_t)).wait_and_throw();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::memset(static_cast<void*>(results), 0, batch->num_rows * sizeof(int8_t));
    return pgaccel_kernel_failure("pgaccel_expr_eval_predicate", &e);
  } catch (...) {
    std::memset(static_cast<void*>(results), 0, batch->num_rows * sizeof(int8_t));
    return pgaccel_kernel_failure("pgaccel_expr_eval_predicate", nullptr);
  }
}

pgaccel_status pgaccel_expr_eval_project(const pgaccel_expr_program* program,
                                         const pgaccel_batch* batch, pgaccel_val* output,
                                         uint8_t* uncertain_mask) {
  if (program == nullptr || batch == nullptr || output == nullptr)
    return PGACCEL_ERROR;
  if (!has_valid_host_layout(program, batch))
    return PGACCEL_ERROR;
  if (batch->num_rows == 0)
    return PGACCEL_OK;

  const ExprProgramTier tier = program_tier(program);
  if (tier != ExprProgramTier::Basic) {
    const pgaccel_status validation_status = validate_device_program(program);
    if (validation_status != PGACCEL_OK)
      return validation_status;
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    auto s = stage_dispatch(*q, program, batch);
    if (s == nullptr)
      return PGACCEL_OOM;

    pgaccel_val* d_output = sycl::malloc_device<pgaccel_val>(batch->num_rows, *q);
    uint8_t* d_uncertain = sycl::malloc_device<uint8_t>(batch->num_rows, *q);
    UsmGuard output_guard{*q, d_output};
    UsmGuard uncertain_guard{*q, d_uncertain};
    if (d_output == nullptr || d_uncertain == nullptr)
      return PGACCEL_OOM;

    pgaccel_expr_program* d_prog = s->d_prog;
    pgaccel_batch* d_batch = s->d_batch;

    switch (tier) {
      case ExprProgramTier::Basic:
        submit_basic_project_kernel(*q, batch->num_rows, d_prog, d_batch, d_output, d_uncertain);
        break;
      case ExprProgramTier::Common:
        submit_project_kernel<false>(*q, batch->num_rows, d_prog, d_batch, d_output, d_uncertain);
        break;
      case ExprProgramTier::Extended:
        submit_project_kernel<true>(*q, batch->num_rows, d_prog, d_batch, d_output, d_uncertain);
        break;
    }

    q->memcpy(output, d_output, batch->num_rows * sizeof(pgaccel_val)).wait_and_throw();
    if (uncertain_mask != nullptr)
      q->memcpy(uncertain_mask, d_uncertain, batch->num_rows).wait_and_throw();

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure("pgaccel_expr_eval_project", &e);
  } catch (...) {
    return pgaccel_kernel_failure("pgaccel_expr_eval_project", nullptr);
  }
}

}  // extern "C"

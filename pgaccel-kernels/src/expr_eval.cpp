/*
 * expr_eval.cpp — GPU expression evaluator (bytecode interpreter).
 *
 * Stack-based VM that evaluates SQL WHERE clauses and projections on the GPU.
 * Each GPU thread independently evaluates the program for one row.
 *
 * Correctness rules:
 *   - Integer overflow → UNCERTAIN (CPU recheck raises ERROR)
 *   - Division by zero → UNCERTAIN
 *   - sqrt(negative) → UNCERTAIN
 *   - NaN = NaN → TRUE (PG semantics)
 *   - Full SQL three-valued NULL logic
 */

#include "pgaccel_expr.h"
#include <cmath>
#include <cstdio>
#include <cstring>

#include <sycl/sycl.hpp>

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
    if (v.tag == PGACCEL_VAL_BOOL) return v.data.b;
    if (v.tag == PGACCEL_VAL_INT32) return v.data.i32 != 0;
    if (v.tag == PGACCEL_VAL_INT64) return v.data.i64 != 0;
    return false;
}

/// PG-compatible float NaN check (works in SYCL device code).
static inline bool is_nan_f32(float x) { return x != x; }
static inline bool is_nan_f64(double x) { return x != x; }

/// PG-compatible float comparison: NaN = NaN is TRUE, NaN > everything.
static inline bool pg_float_eq_f64(double a, double b) {
    bool a_nan = is_nan_f64(a);
    bool b_nan = is_nan_f64(b);
    if (a_nan && b_nan) return true;
    if (a_nan || b_nan) return false;
    return a == b;
}

static inline bool pg_float_lt_f64(double a, double b) {
    if (is_nan_f64(a)) return false;
    if (is_nan_f64(b)) return true;
    return a < b;
}

/// Convert any numeric value to f64 for comparison.
/// Returns (value, is_valid). Invalid = NULL.
static inline double val_to_f64(const pgaccel_val& v) {
    switch (v.tag) {
        case PGACCEL_VAL_INT32:   return static_cast<double>(v.data.i32);
        case PGACCEL_VAL_INT64:   return static_cast<double>(v.data.i64);
        case PGACCEL_VAL_FLOAT32: return static_cast<double>(v.data.f32);
        case PGACCEL_VAL_FLOAT64: return v.data.f64;
        case PGACCEL_VAL_BOOL:    return v.data.b ? 1.0 : 0.0;
        default:                  return 0.0;
    }
}

// ---------------------------------------------------------------------------
// Load column value from columnar batch
// ---------------------------------------------------------------------------

static inline pgaccel_val load_column(const pgaccel_batch* batch,
                                      size_t row, size_t col) {
    if (col >= batch->num_cols) return make_null();

    // Check null bitmap
    if (batch->col_nulls != nullptr && batch->col_nulls[col] != nullptr) {
        if (batch->col_nulls[col][row]) {
            return make_null();
        }
    }

    pgaccel_val_tag type = batch->col_types[col];
    const void* data = batch->col_data[col];
    if (data == nullptr) return make_null();

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

static eval_result eval_row(const pgaccel_expr_program* prog,
                            const pgaccel_batch* batch,
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
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            int64_t r = static_cast<int64_t>(a.data.i32) + b.data.i32;
            if (r < INT32_MIN || r > INT32_MAX) { uncertain = true; a = make_null(); break; }
            a = make_i32(static_cast<int32_t>(r));
            break;
        }
        case PGACCEL_EXPR_OP_ADD_I64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            // Detect overflow via sign check: same sign inputs, different sign result.
            int64_t av = a.data.i64, bv = b.data.i64;
            int64_t r = av + bv;
            if ((av > 0 && bv > 0 && r < 0) || (av < 0 && bv < 0 && r > 0)) {
                uncertain = true; a = make_null(); break;
            }
            a = make_i64(r);
            break;
        }
        case PGACCEL_EXPR_OP_ADD_F32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f32(a.data.f32 + b.data.f32);
            break;
        }
        case PGACCEL_EXPR_OP_ADD_F64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f64(a.data.f64 + b.data.f64);
            break;
        }

        case PGACCEL_EXPR_OP_SUB_I32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            int64_t r = static_cast<int64_t>(a.data.i32) - b.data.i32;
            if (r < INT32_MIN || r > INT32_MAX) { uncertain = true; a = make_null(); break; }
            a = make_i32(static_cast<int32_t>(r));
            break;
        }
        case PGACCEL_EXPR_OP_SUB_I64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            int64_t av = a.data.i64, bv = b.data.i64;
            int64_t r = av - bv;
            if ((bv > 0 && av < INT64_MIN + bv) || (bv < 0 && av > INT64_MAX + bv)) {
                uncertain = true; a = make_null(); break;
            }
            a = make_i64(r);
            break;
        }
        case PGACCEL_EXPR_OP_SUB_F32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f32(a.data.f32 - b.data.f32);
            break;
        }
        case PGACCEL_EXPR_OP_SUB_F64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f64(a.data.f64 - b.data.f64);
            break;
        }

        case PGACCEL_EXPR_OP_MUL_I32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            int64_t r = static_cast<int64_t>(a.data.i32) * b.data.i32;
            if (r < INT32_MIN || r > INT32_MAX) { uncertain = true; a = make_null(); break; }
            a = make_i32(static_cast<int32_t>(r));
            break;
        }
        case PGACCEL_EXPR_OP_MUL_I64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            // Overflow check: if b != 0 and a * b / b != a
            int64_t av = a.data.i64, bv = b.data.i64;
            if (bv != 0) {
                int64_t r = av * bv;
                if (r / bv != av) { uncertain = true; a = make_null(); break; }
                a = make_i64(r);
            } else {
                a = make_i64(0);
            }
            break;
        }
        case PGACCEL_EXPR_OP_MUL_F32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f32(a.data.f32 * b.data.f32);
            break;
        }
        case PGACCEL_EXPR_OP_MUL_F64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f64(a.data.f64 * b.data.f64);
            break;
        }

        case PGACCEL_EXPR_OP_DIV_I32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            if (b.data.i32 == 0) { uncertain = true; a = make_null(); break; }
            // INT32_MIN / -1 overflows
            if (a.data.i32 == INT32_MIN && b.data.i32 == -1) {
                uncertain = true; a = make_null(); break;
            }
            a = make_i32(a.data.i32 / b.data.i32);
            break;
        }
        case PGACCEL_EXPR_OP_DIV_I64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            if (b.data.i64 == 0) { uncertain = true; a = make_null(); break; }
            if (a.data.i64 == INT64_MIN && b.data.i64 == -1) {
                uncertain = true; a = make_null(); break;
            }
            a = make_i64(a.data.i64 / b.data.i64);
            break;
        }
        case PGACCEL_EXPR_OP_DIV_F32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f32(a.data.f32 / b.data.f32);
            break;
        }
        case PGACCEL_EXPR_OP_DIV_F64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            a = make_f64(a.data.f64 / b.data.f64);
            break;
        }

        case PGACCEL_EXPR_OP_MOD_I32: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            if (b.data.i32 == 0) { uncertain = true; a = make_null(); break; }
            a = make_i32(a.data.i32 % b.data.i32);
            break;
        }
        case PGACCEL_EXPR_OP_MOD_I64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            if (b.data.i64 == 0) { uncertain = true; a = make_null(); break; }
            a = make_i64(a.data.i64 % b.data.i64);
            break;
        }

        // ── Unary negate ────────────────────────────────────────────

        case PGACCEL_EXPR_OP_NEG_I32: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            if (a.data.i32 == INT32_MIN) { uncertain = true; a = make_null(); break; }
            a = make_i32(-a.data.i32);
            break;
        }
        case PGACCEL_EXPR_OP_NEG_I64: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            if (a.data.i64 == INT64_MIN) { uncertain = true; a = make_null(); break; }
            a = make_i64(-a.data.i64);
            break;
        }
        case PGACCEL_EXPR_OP_NEG_F32: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            a = make_f32(-a.data.f32);
            break;
        }
        case PGACCEL_EXPR_OP_NEG_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            a = make_f64(-a.data.f64);
            break;
        }

        // ── Comparison (pop 2, push bool) ───────────────────────────
        // Both operands promoted to f64 for comparison.
        // NULL propagates (any NULL input → NULL output).

        case PGACCEL_EXPR_OP_EQ: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double da = val_to_f64(a), db = val_to_f64(b);
            a = make_bool(pg_float_eq_f64(da, db));
            break;
        }
        case PGACCEL_EXPR_OP_NE: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double da = val_to_f64(a), db = val_to_f64(b);
            a = make_bool(!pg_float_eq_f64(da, db));
            break;
        }
        case PGACCEL_EXPR_OP_LT: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double da = val_to_f64(a), db = val_to_f64(b);
            a = make_bool(pg_float_lt_f64(da, db));
            break;
        }
        case PGACCEL_EXPR_OP_LE: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double da = val_to_f64(a), db = val_to_f64(b);
            a = make_bool(pg_float_lt_f64(da, db) || pg_float_eq_f64(da, db));
            break;
        }
        case PGACCEL_EXPR_OP_GT: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double da = val_to_f64(a), db = val_to_f64(b);
            a = make_bool(pg_float_lt_f64(db, da));
            break;
        }
        case PGACCEL_EXPR_OP_GE: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
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
            if (is_null(a)) break; // NOT NULL = NULL
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
            if (!is_null(a)) a = make_i64(static_cast<int64_t>(a.data.i32));
            break;
        }
        case PGACCEL_EXPR_OP_CAST_I32_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(static_cast<double>(a.data.i32));
            break;
        }
        case PGACCEL_EXPR_OP_CAST_I64_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(static_cast<double>(a.data.i64));
            break;
        }
        case PGACCEL_EXPR_OP_CAST_F32_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(static_cast<double>(a.data.f32));
            break;
        }
        case PGACCEL_EXPR_OP_CAST_F64_F32: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f32(static_cast<float>(a.data.f64));
            break;
        }
        case PGACCEL_EXPR_OP_CAST_BOOL_I32: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_i32(a.data.b ? 1 : 0);
            break;
        }

        // ── Math functions (unary) ──────────────────────────────────

        case PGACCEL_EXPR_OP_ABS_I32: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            if (a.data.i32 == INT32_MIN) { uncertain = true; a = make_null(); break; }
            a = make_i32(a.data.i32 < 0 ? -a.data.i32 : a.data.i32);
            break;
        }
        case PGACCEL_EXPR_OP_ABS_I64: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            if (a.data.i64 == INT64_MIN) { uncertain = true; a = make_null(); break; }
            a = make_i64(a.data.i64 < 0 ? -a.data.i64 : a.data.i64);
            break;
        }
        case PGACCEL_EXPR_OP_ABS_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            a = make_f64(fabs(a.data.f64));
            break;
        }
        case PGACCEL_EXPR_OP_SQRT_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a)) break;
            if (a.data.f64 < 0.0) { uncertain = true; a = make_null(); break; }
            a = make_f64(sqrt(a.data.f64));
            break;
        }
        case PGACCEL_EXPR_OP_CEIL_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(ceil(a.data.f64));
            break;
        }
        case PGACCEL_EXPR_OP_FLOOR_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(floor(a.data.f64));
            break;
        }
        case PGACCEL_EXPR_OP_ROUND_F64: {
            pgaccel_val& a = stack[sp - 1];
            if (!is_null(a)) a = make_f64(round(a.data.f64));
            break;
        }

        // ── Math functions (binary) ─────────────────────────────────

        case PGACCEL_EXPR_OP_POW_F64: {
            pgaccel_val b = stack[--sp];
            pgaccel_val& a = stack[sp - 1];
            if (is_null(a) || is_null(b)) { a = make_null(); break; }
            double r = pow(a.data.f64, b.data.f64);
            // pow can produce inf/nan for various edge cases — uncertain
            if (is_nan_f64(r) && !is_nan_f64(a.data.f64) && !is_nan_f64(b.data.f64)) {
                uncertain = true; a = make_null(); break;
            }
            a = make_f64(r);
            break;
        }

        // ── CASE WHEN (conditional jumps) ───────────────────────────

        case PGACCEL_EXPR_OP_JUMP_IF_FALSE: {
            pgaccel_val cond = stack[--sp];
            if (is_null(cond) || !val_to_bool(cond)) {
                pc = arg - 1; // -1 because loop increments
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
            if (is_null(a)) a = b;
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
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_expr_eval_predicate(
    const pgaccel_expr_program* program,
    const pgaccel_batch*        batch,
    int8_t*                     results)
{
    if (program == nullptr || batch == nullptr || results == nullptr) {
        return PGACCEL_ERROR;
    }

    for (size_t row = 0; row < batch->num_rows; row++) {
        eval_result er = eval_row(program, batch, row);

        if (er.uncertain) {
            results[row] = PGACCEL_EXPR_UNCERTAIN;
        } else if (is_null(er.value)) {
            // NULL in a WHERE context is false (row does not pass)
            results[row] = PGACCEL_EXPR_FALSE;
        } else if (val_to_bool(er.value)) {
            results[row] = PGACCEL_EXPR_TRUE;
        } else {
            results[row] = PGACCEL_EXPR_FALSE;
        }
    }

    return PGACCEL_OK;
}

pgaccel_status pgaccel_expr_eval_project(
    const pgaccel_expr_program* program,
    const pgaccel_batch*        batch,
    pgaccel_val*                output,
    uint8_t*                    uncertain_mask)
{
    if (program == nullptr || batch == nullptr || output == nullptr) {
        return PGACCEL_ERROR;
    }

    for (size_t row = 0; row < batch->num_rows; row++) {
        eval_result er = eval_row(program, batch, row);
        output[row] = er.value;
        if (uncertain_mask != nullptr) {
            uncertain_mask[row] = er.uncertain ? 1 : 0;
        }
    }

    return PGACCEL_OK;
}

} // extern "C"

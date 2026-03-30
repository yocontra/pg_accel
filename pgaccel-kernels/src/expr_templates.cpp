/*
 * expr_templates.cpp — Pre-compiled template kernels for common WHERE patterns.
 *
 * These cover ~80% of real WHERE clauses with zero interpretation overhead.
 * Each template is a direct C function that evaluates a specific expression
 * pattern on a columnar batch.
 *
 * Templates:
 *   1. col > const        (single comparison)
 *   2. col BETWEEN lo AND hi
 *   3. col IN (const, const, ...)  (up to 16 values)
 *   4. col IS NULL / IS NOT NULL
 *   5. col1 op const1 AND col2 op const2  (two-predicate conjunction)
 */

#include "pgaccel_expr.h"
#include <cstring>

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// PG-compatible NaN-aware comparison for f64.
static inline bool pg_lt_f64(double a, double b) {
    if (a != a) return false;  // NaN not less than anything
    if (b != b) return true;   // everything less than NaN
    return a < b;
}

static inline bool pg_le_f64(double a, double b) {
    if (a != a && b != b) return true; // NaN == NaN
    if (a != a) return false;
    if (b != b) return true;
    return a <= b;
}

static inline bool pg_gt_f64(double a, double b) {
    return pg_lt_f64(b, a);
}

static inline bool pg_ge_f64(double a, double b) {
    return pg_le_f64(b, a);
}

static inline bool pg_eq_f64(double a, double b) {
    if (a != a && b != b) return true;
    if (a != a || b != b) return false;
    return a == b;
}

static inline bool pg_ne_f64(double a, double b) {
    return !pg_eq_f64(a, b);
}

/// Read a f64 value from a typed column at given row.
/// Returns (value, is_null).
struct col_read_result {
    double value;
    bool is_null;
};

static inline col_read_result read_col_f64(const pgaccel_batch* batch,
                                            size_t col, size_t row) {
    col_read_result r = {0.0, true};

    if (col >= batch->num_cols || batch->col_data[col] == nullptr) return r;

    if (batch->col_nulls != nullptr && batch->col_nulls[col] != nullptr &&
        batch->col_nulls[col][row]) {
        return r;
    }

    r.is_null = false;
    switch (batch->col_types[col]) {
        case PGACCEL_VAL_INT32:
            r.value = static_cast<double>(
                static_cast<const int32_t*>(batch->col_data[col])[row]);
            break;
        case PGACCEL_VAL_INT64:
            r.value = static_cast<double>(
                static_cast<const int64_t*>(batch->col_data[col])[row]);
            break;
        case PGACCEL_VAL_FLOAT32:
            r.value = static_cast<double>(
                static_cast<const float*>(batch->col_data[col])[row]);
            break;
        case PGACCEL_VAL_FLOAT64:
            r.value = static_cast<const double*>(batch->col_data[col])[row];
            break;
        case PGACCEL_VAL_BOOL:
            r.value = static_cast<const bool*>(batch->col_data[col])[row] ? 1.0 : 0.0;
            break;
        case PGACCEL_VAL_DATE:
            r.value = static_cast<double>(
                static_cast<const int32_t*>(batch->col_data[col])[row]);
            break;
        case PGACCEL_VAL_TIMESTAMP:
            r.value = static_cast<double>(
                static_cast<const int64_t*>(batch->col_data[col])[row]);
            break;
        default:
            r.is_null = true;
            break;
    }
    return r;
}

/// Comparison function pointer type.
typedef bool (*cmp_fn)(double, double);

static cmp_fn get_cmp(uint16_t opcode) {
    switch (opcode) {
        case PGACCEL_EXPR_OP_LT: return pg_lt_f64;
        case PGACCEL_EXPR_OP_LE: return pg_le_f64;
        case PGACCEL_EXPR_OP_GT: return pg_gt_f64;
        case PGACCEL_EXPR_OP_GE: return pg_ge_f64;
        case PGACCEL_EXPR_OP_EQ: return pg_eq_f64;
        case PGACCEL_EXPR_OP_NE: return pg_ne_f64;
        default: return nullptr;
    }
}

// ===========================================================================
// Template 1: col <cmp> const
// ===========================================================================

extern "C"
pgaccel_status pgaccel_expr_template_cmp_const(
    const pgaccel_batch* batch,
    uint32_t             col_idx,
    uint16_t             cmp_opcode,
    double               const_val,
    int8_t*              results)
{
    if (batch == nullptr || results == nullptr) return PGACCEL_ERROR;

    cmp_fn cmp = get_cmp(cmp_opcode);
    if (cmp == nullptr) return PGACCEL_UNSUPPORTED;

    for (size_t row = 0; row < batch->num_rows; row++) {
        col_read_result cr = read_col_f64(batch, col_idx, row);
        if (cr.is_null) {
            results[row] = PGACCEL_EXPR_FALSE;
        } else {
            results[row] = cmp(cr.value, const_val)
                         ? PGACCEL_EXPR_TRUE
                         : PGACCEL_EXPR_FALSE;
        }
    }
    return PGACCEL_OK;
}

// ===========================================================================
// Template 2: col BETWEEN lo AND hi  (inclusive)
// ===========================================================================

extern "C"
pgaccel_status pgaccel_expr_template_between(
    const pgaccel_batch* batch,
    uint32_t             col_idx,
    double               lo,
    double               hi,
    int8_t*              results)
{
    if (batch == nullptr || results == nullptr) return PGACCEL_ERROR;

    for (size_t row = 0; row < batch->num_rows; row++) {
        col_read_result cr = read_col_f64(batch, col_idx, row);
        if (cr.is_null) {
            results[row] = PGACCEL_EXPR_FALSE;
        } else {
            results[row] = (pg_ge_f64(cr.value, lo) && pg_le_f64(cr.value, hi))
                         ? PGACCEL_EXPR_TRUE
                         : PGACCEL_EXPR_FALSE;
        }
    }
    return PGACCEL_OK;
}

// ===========================================================================
// Template 3: col IN (v0, v1, ..., vN)  — up to 16 values
// ===========================================================================

extern "C"
pgaccel_status pgaccel_expr_template_in_list(
    const pgaccel_batch* batch,
    uint32_t             col_idx,
    const double*        values,
    size_t               value_count,
    int8_t*              results)
{
    if (batch == nullptr || results == nullptr || values == nullptr) {
        return PGACCEL_ERROR;
    }
    if (value_count > 16) return PGACCEL_UNSUPPORTED;

    for (size_t row = 0; row < batch->num_rows; row++) {
        col_read_result cr = read_col_f64(batch, col_idx, row);
        if (cr.is_null) {
            results[row] = PGACCEL_EXPR_FALSE;
            continue;
        }
        bool found = false;
        for (size_t i = 0; i < value_count; i++) {
            if (pg_eq_f64(cr.value, values[i])) {
                found = true;
                break;
            }
        }
        results[row] = found ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
    }
    return PGACCEL_OK;
}

// ===========================================================================
// Template 4: col IS NULL / IS NOT NULL
// ===========================================================================

extern "C"
pgaccel_status pgaccel_expr_template_is_null(
    const pgaccel_batch* batch,
    uint32_t             col_idx,
    bool                 check_not_null,
    int8_t*              results)
{
    if (batch == nullptr || results == nullptr) return PGACCEL_ERROR;

    for (size_t row = 0; row < batch->num_rows; row++) {
        bool row_is_null = false;
        if (col_idx >= batch->num_cols || batch->col_data[col_idx] == nullptr) {
            row_is_null = true;
        } else if (batch->col_nulls != nullptr &&
                   batch->col_nulls[col_idx] != nullptr &&
                   batch->col_nulls[col_idx][row]) {
            row_is_null = true;
        }

        if (check_not_null) {
            results[row] = row_is_null ? PGACCEL_EXPR_FALSE : PGACCEL_EXPR_TRUE;
        } else {
            results[row] = row_is_null ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
        }
    }
    return PGACCEL_OK;
}

// ===========================================================================
// Template 5: col1 <cmp1> const1 AND col2 <cmp2> const2
// ===========================================================================

extern "C"
pgaccel_status pgaccel_expr_template_two_pred_and(
    const pgaccel_batch* batch,
    uint32_t             col1_idx,
    uint16_t             cmp1_opcode,
    double               const1_val,
    uint32_t             col2_idx,
    uint16_t             cmp2_opcode,
    double               const2_val,
    int8_t*              results)
{
    if (batch == nullptr || results == nullptr) return PGACCEL_ERROR;

    cmp_fn cmp1 = get_cmp(cmp1_opcode);
    cmp_fn cmp2 = get_cmp(cmp2_opcode);
    if (cmp1 == nullptr || cmp2 == nullptr) return PGACCEL_UNSUPPORTED;

    for (size_t row = 0; row < batch->num_rows; row++) {
        col_read_result cr1 = read_col_f64(batch, col1_idx, row);
        if (cr1.is_null || !cmp1(cr1.value, const1_val)) {
            results[row] = PGACCEL_EXPR_FALSE;
            continue;
        }
        col_read_result cr2 = read_col_f64(batch, col2_idx, row);
        if (cr2.is_null || !cmp2(cr2.value, const2_val)) {
            results[row] = PGACCEL_EXPR_FALSE;
        } else {
            results[row] = PGACCEL_EXPR_TRUE;
        }
    }
    return PGACCEL_OK;
}

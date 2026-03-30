/*
 * pgaccel_window.h — GPU window function types and API.
 *
 * Supports:
 *   - ROW_NUMBER, RANK, DENSE_RANK, NTILE (from sorted position)
 *   - SUM, COUNT, AVG OVER (segmented prefix scan)
 *   - LAG, LEAD (indexed lookups)
 *
 * Input: pre-sorted partition data with partition boundary markers.
 * Output: per-row window function results.
 */

#ifndef PGACCEL_WINDOW_H
#define PGACCEL_WINDOW_H

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Window function tags ───────────────────────────────────────── */

typedef enum {
    PGACCEL_WIN_ROW_NUMBER  = 0,
    PGACCEL_WIN_RANK        = 1,
    PGACCEL_WIN_DENSE_RANK  = 2,
    PGACCEL_WIN_NTILE       = 3,
    PGACCEL_WIN_SUM         = 10,
    PGACCEL_WIN_COUNT       = 11,
    PGACCEL_WIN_AVG         = 12,
    PGACCEL_WIN_MIN         = 13,
    PGACCEL_WIN_MAX         = 14,
    PGACCEL_WIN_LAG         = 20,
    PGACCEL_WIN_LEAD        = 21,
} pgaccel_window_func;

/* ── API ��───────────────────────────────────────────────────────── */

/// Compute ROW_NUMBER for each row within its partition.
///
/// `partition_starts[i] == 1` marks the first row of a new partition.
/// Output: `results[i]` = 1-based row number within partition.
pgaccel_status pgaccel_window_row_number(
    const uint8_t*  partition_starts,  /* [count] 1=new partition */
    size_t          count,
    int64_t*        results            /* [count] output */
);

/// Compute RANK for each row (requires sorted input).
///
/// `sort_keys` contains the ORDER BY values (f64).
/// Rows with equal sort keys get the same rank.
pgaccel_status pgaccel_window_rank(
    const uint8_t*  partition_starts,
    const double*   sort_keys,
    size_t          count,
    int64_t*        results
);

/// Compute DENSE_RANK for each row.
pgaccel_status pgaccel_window_dense_rank(
    const uint8_t*  partition_starts,
    const double*   sort_keys,
    size_t          count,
    int64_t*        results
);

/// Compute running SUM within each partition.
///
/// Uses Kahan compensated summation for accuracy.
/// NULL values (null_mask[i]==1) contribute 0 to the sum.
pgaccel_status pgaccel_window_sum(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,         /* [count] 1=null, or NULL for no nulls */
    size_t          count,
    double*         results
);

/// Compute running COUNT within each partition.
///
/// Counts non-NULL values.
pgaccel_status pgaccel_window_count(
    const uint8_t*  partition_starts,
    const uint8_t*  null_mask,
    size_t          count,
    int64_t*        results
);

/// Compute LAG(value, offset, default) within each partition.
///
/// `offset` is positive (look back). Returns `default_val` when
/// looking before partition start or at NULL.
pgaccel_status pgaccel_window_lag(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,
    size_t          count,
    int             offset,
    double          default_val,
    double*         results,
    uint8_t*        result_nulls       /* [count] 1=null output */
);

/// Compute LEAD(value, offset, default) within each partition.
pgaccel_status pgaccel_window_lead(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,
    size_t          count,
    int             offset,
    double          default_val,
    double*         results,
    uint8_t*        result_nulls
);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_WINDOW_H */

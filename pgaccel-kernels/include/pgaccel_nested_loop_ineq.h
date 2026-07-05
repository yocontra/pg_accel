#ifndef PGACCEL_NESTED_LOOP_INEQ_H
#define PGACCEL_NESTED_LOOP_INEQ_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "pgaccel_ffi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── NestedLoop scalar-inequality join kernel ────────────────────────────
 *
 * Targets the Phase 4 NLJ scalar-inequality recognizer in the planner
 * (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` —
 * `observe_nestloop_scalar_opportunity`). The kernel evaluates a single
 * scalar btree inequality (`<`, `<=`, `>=`, `>`) for every (outer_i,
 * inner_j) pair and emits matching `(outer_idx, inner_idx)` pairs to
 * `pairs_out` via an atomic counter.
 *
 * NULL handling: PG INNER-join semantics exclude any row where either
 * side is NULL. Callers MUST filter NULLs from both key arrays before
 * the buffers reach the kernel (the planner observability path counts
 * NULL-ratio in its rejection telemetry so the cost model can reject
 * pathologically-null inputs).
 *
 * Overflow handling: `pair_count_out` returns the TOTAL number of
 * matches the kernel observed (after atomic compaction). If
 * `*pair_count_out > max_pairs`, the kernel truncated the emit buffer
 * silently and the caller MUST treat the result as an overflow and
 * fall back to PG native NestedLoop. No partial results may be used.
 *
 * Currently exposed types: int64 and double. Other scalar types
 * (`int32`, `timestamp`, `date`) widen at the bridge layer.
 */

typedef enum {
  PGACCEL_NLJ_LT = 0, /* outer < inner */
  PGACCEL_NLJ_LE = 1, /* outer <= inner */
  PGACCEL_NLJ_GE = 2, /* outer >= inner */
  PGACCEL_NLJ_GT = 3, /* outer > inner */
} pgaccel_nlj_ineq_op;

/* Single-predicate inequality join.
 *
 *   outer_keys      — [n_outer] non-NULL outer-side keys
 *   inner_keys      — [n_inner] non-NULL inner-side keys
 *   op              — predicate to evaluate per pair
 *   pairs_out       — [max_pairs * 2] output buffer, interleaved
 *                     [outer_idx_0, inner_idx_0, outer_idx_1, ...]
 *   max_pairs       — output buffer capacity (in pair units)
 *   pair_count_out  — total matches observed (may exceed max_pairs)
 *
 * Returns PGACCEL_OK on success. Caller MUST check
 * `*pair_count_out <= max_pairs` before consuming `pairs_out`.
 */
pgaccel_status pgaccel_nlj_ineq_i64(const int64_t* outer_keys, size_t n_outer,
                                    const int64_t* inner_keys, size_t n_inner,
                                    pgaccel_nlj_ineq_op op, uint32_t* pairs_out, size_t max_pairs,
                                    size_t* pair_count_out);
pgaccel_status pgaccel_nlj_ineq_f64(const double* outer_keys, size_t n_outer,
                                    const double* inner_keys, size_t n_inner,
                                    pgaccel_nlj_ineq_op op, uint32_t* pairs_out, size_t max_pairs,
                                    size_t* pair_count_out);

/* BETWEEN-shape inequality join: predicate is
 *   inner_lo[j] <= outer[i] <= inner_hi[j]
 *
 * Matches the planner's expansion of `A.x BETWEEN B.lo AND B.hi` into two
 * conjoined btree quals, but evaluates both in a single kernel pass so the
 * driver doesn't materialise two intermediate match-pair buffers.
 */
pgaccel_status pgaccel_nlj_between_i64(const int64_t* outer_keys, size_t n_outer,
                                       const int64_t* inner_lo, const int64_t* inner_hi,
                                       size_t n_inner, uint32_t* pairs_out, size_t max_pairs,
                                       size_t* pair_count_out);
pgaccel_status pgaccel_nlj_between_f64(const double* outer_keys, size_t n_outer,
                                       const double* inner_lo, const double* inner_hi,
                                       size_t n_inner, uint32_t* pairs_out, size_t max_pairs,
                                       size_t* pair_count_out);

#ifdef __cplusplus
}
#endif

#endif /* PGACCEL_NESTED_LOOP_INEQ_H */

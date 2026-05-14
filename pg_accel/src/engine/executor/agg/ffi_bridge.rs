//! Map `AggOp` to the GPU FFI `PgaccelAggFunc` enum.

use crate::gpu::PgaccelAggFunc;

use super::ops::AggOp;

/// Map `AggOp` to the FFI `PgaccelAggFunc` enum.
///
/// AVG maps to the SUM lane only for non-grouped reduce, where the executor
/// keeps the input count and finalizes `sum / count` itself. Finalize-mode
/// grouped hash aggregation has no per-group count lane for AVG today, so the
/// planner must reject grouped AVG before this mapping can be used.
///
/// Stats/bitwise/bool aggregates do not have direct FFI counterparts in the
/// current `PgaccelAggFunc` enum; they route through the `reduce_stats_*`
/// kernels in `gpu/mod.rs`. Map them conservatively to `Sum` so that any code
/// path that still calls through `agg_op_to_ffi` treats them as reducible;
/// callers that need stats-specific dispatch branch on `AggOp` directly before
/// calling FFI.
pub const fn agg_op_to_ffi(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum
        | AggOp::Avg
        | AggOp::StddevSamp
        | AggOp::StddevPop
        | AggOp::VarSamp
        | AggOp::VarPop => PgaccelAggFunc::Sum,
        AggOp::Min | AggOp::BitAnd | AggOp::BoolAnd => PgaccelAggFunc::Min,
        AggOp::Max | AggOp::BitOr | AggOp::BoolOr => PgaccelAggFunc::Max,
        AggOp::Count | AggOp::Passthrough => PgaccelAggFunc::Count,
    }
}

/// Map `AggOp` to the FFI `PgaccelAggFunc` enum for **partial-mode**
/// dispatch (Phase 3B). Unlike `agg_op_to_ffi`, this preserves Avg /
/// Stddev / Var as their dedicated partial-mode variants so the GPU
/// kernel emits the correct per-group transition-state shape.
///
/// Bitwise / boolean reductions use the same single-f64 lane shape as
/// Min/Max in partial mode, so the planner can route them through the current
/// FFI enum without a separate kernel variant.
pub const fn agg_op_to_ffi_partial(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum => PgaccelAggFunc::Sum,
        AggOp::Avg => PgaccelAggFunc::Avg,
        AggOp::StddevSamp | AggOp::StddevPop => PgaccelAggFunc::Stddev,
        AggOp::VarSamp | AggOp::VarPop => PgaccelAggFunc::Var,
        AggOp::Min | AggOp::BitAnd | AggOp::BoolAnd => PgaccelAggFunc::Min,
        AggOp::Max | AggOp::BitOr | AggOp::BoolOr => PgaccelAggFunc::Max,
        AggOp::Count | AggOp::Passthrough => PgaccelAggFunc::Count,
    }
}

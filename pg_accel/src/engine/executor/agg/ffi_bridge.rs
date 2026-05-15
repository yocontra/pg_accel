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
/// current `PgaccelAggFunc` enum (the grouped GPU hash-agg kernel only models
/// SUM/MIN/MAX/COUNT/AVG/STDDEV/VAR lanes). Bitwise/boolean reductions are
/// dispatched through the typed `reduce_bool_*` / `reduce_bit_*` kernels at
/// the executor level (see `dispatch_gpu_reduce_*`); planner code that calls
/// `agg_op_to_ffi` must already have rejected grouped bool/bit because there
/// is no per-group GPU lane for them yet. Map to `Count` so an accidental
/// dispatch surfaces as a deterministic mismatch rather than silently
/// reducing through SUM.
pub const fn agg_op_to_ffi(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum
        | AggOp::Avg
        | AggOp::StddevSamp
        | AggOp::StddevPop
        | AggOp::VarSamp
        | AggOp::VarPop => PgaccelAggFunc::Sum,
        AggOp::Min => PgaccelAggFunc::Min,
        AggOp::Max => PgaccelAggFunc::Max,
        AggOp::Count
        | AggOp::Passthrough
        | AggOp::BitAnd
        | AggOp::BitOr
        | AggOp::BitXor
        | AggOp::BoolAnd
        | AggOp::BoolOr => PgaccelAggFunc::Count,
    }
}

/// Map `AggOp` to the FFI `PgaccelAggFunc` enum for **partial-mode**
/// dispatch (Phase 3B). Unlike `agg_op_to_ffi`, this preserves Avg /
/// Stddev / Var as their dedicated partial-mode variants so the GPU
/// kernel emits the correct per-group transition-state shape.
///
/// Bitwise / boolean reductions are emitted as scalar passthrough partial
/// columns (transtype matches the input column type for bit_*, BOOLOID for
/// bool_*). The GPU hash-agg kernel does not run them per-group today —
/// the planner gates that path off — so they map to `Count` here as a
/// safety net.
pub const fn agg_op_to_ffi_partial(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum => PgaccelAggFunc::Sum,
        AggOp::Avg => PgaccelAggFunc::Avg,
        AggOp::StddevSamp | AggOp::StddevPop => PgaccelAggFunc::Stddev,
        AggOp::VarSamp | AggOp::VarPop => PgaccelAggFunc::Var,
        AggOp::Min => PgaccelAggFunc::Min,
        AggOp::Max => PgaccelAggFunc::Max,
        AggOp::Count
        | AggOp::Passthrough
        | AggOp::BitAnd
        | AggOp::BitOr
        | AggOp::BitXor
        | AggOp::BoolAnd
        | AggOp::BoolOr => PgaccelAggFunc::Count,
    }
}

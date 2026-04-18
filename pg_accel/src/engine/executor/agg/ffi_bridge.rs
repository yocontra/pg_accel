//! Map `AggOp` to the GPU FFI `PgaccelAggFunc` enum.

use crate::gpu::PgaccelAggFunc;

use super::ops::AggOp;

/// Map `AggOp` to the FFI `PgaccelAggFunc` enum.
pub const fn agg_op_to_ffi(op: AggOp) -> PgaccelAggFunc {
    match op {
        AggOp::Sum | AggOp::Avg => PgaccelAggFunc::Sum,
        AggOp::Min => PgaccelAggFunc::Min,
        AggOp::Max => PgaccelAggFunc::Max,
        AggOp::Count | AggOp::Passthrough => PgaccelAggFunc::Count,
    }
}

//! Shared helpers used by both `agg.rs` (non-parallel) and `partial_agg.rs`
//! (parallel):
//! - Aggref classification (`Aggref → AggOp` + transtype / serialize-fn metadata)
//! - Target-list walking
//! - Cost estimation
//!
//! The body of the legacy `pgaccel_inject_gpu_agg` still lives in `mod.rs`;
//! these helpers are additive metadata used by `partial_agg::try_inject` to
//! build a `PartialAggSpec` without re-walking the target list twice.

use pgrx::pg_sys;

use crate::engine::executor::agg::AggOp;

/// Classification of an `Aggref` node into one of the partial-agg categories
/// recognised by pg_accel. Drives `PartialColumn` construction in
/// `partial_agg::try_inject`.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum AggClass {
    /// SUM/MIN/MAX where transtype == rtype (plain numeric scalars).
    ScalarPassthrough { transtype: pg_sys::Oid },
    /// `SUM(int4) → int8` (promotion, plain scalar transtype).
    IntegerSumPromotion,
    /// `COUNT(*)` / `COUNT(x)` (transtype = int8).
    Count,
    /// `SUM(int8)` / `SUM(numeric)` (NUMERIC transtype, plain scalar).
    NumericSum,
    /// AVG / STDDEV / VAR — INTERNAL transtype requires `serialize_fn`.
    Float8Stats { serialize_fn: pg_sys::Oid },
    /// BIT_AND / BIT_OR on integer types.
    BitReduction { transtype: pg_sys::Oid, op: BitOp },
    /// BOOL_AND / BOOL_OR.
    BoolReduction { op: BoolOp },
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum BitOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum BoolOp {
    And,
    Or,
}

/// Classify a PostgreSQL `Aggref` by its `aggfnoid`.
///
/// Returns `Some((AggOp, AggClass))` for aggregates pg_accel recognises,
/// `None` for everything else (the caller should bail and let PG handle it).
///
/// The returned `AggOp` is the public enum the executor consumes; `AggClass`
/// carries the extra metadata (transtype, serialize_fn) needed by the
/// partial-agg path to construct `PartialColumn`.
///
/// # Safety
///
/// `aggref` must be a valid non-null `Aggref` pointer on the main backend
/// thread. May call into syscache.
#[allow(dead_code)]
pub(super) unsafe fn classify_aggref(aggref: *const pg_sys::Aggref) -> Option<(AggOp, AggClass)> {
    if aggref.is_null() {
        return None;
    }
    // SAFETY: caller contract — aggref is a valid Aggref.
    let aggfnoid_raw = u32::from(unsafe { (*aggref).aggfnoid });

    match aggfnoid_raw {
        // --- SUM variants ------------------------------------------------
        pg_sys::F_SUM_INT4 | pg_sys::F_SUM_INT2 => {
            Some((AggOp::Sum, AggClass::IntegerSumPromotion))
        }
        pg_sys::F_SUM_INT8 | pg_sys::F_SUM_NUMERIC => Some((AggOp::Sum, AggClass::NumericSum)),
        pg_sys::F_SUM_FLOAT4 => Some((
            AggOp::Sum,
            AggClass::ScalarPassthrough {
                transtype: pg_sys::FLOAT4OID,
            },
        )),
        pg_sys::F_SUM_FLOAT8 => Some((
            AggOp::Sum,
            AggClass::ScalarPassthrough {
                transtype: pg_sys::FLOAT8OID,
            },
        )),

        // --- COUNT -------------------------------------------------------
        pg_sys::F_COUNT_ANY | pg_sys::F_COUNT_ => Some((AggOp::Count, AggClass::Count)),

        // --- AVG / STDDEV / VAR — INTERNAL transtype --------------------
        pg_sys::F_AVG_INT2
        | pg_sys::F_AVG_INT4
        | pg_sys::F_AVG_INT8
        | pg_sys::F_AVG_FLOAT4
        | pg_sys::F_AVG_FLOAT8
        | pg_sys::F_AVG_NUMERIC
        | pg_sys::F_AVG_INTERVAL => {
            // SAFETY: caller contract.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let serialize_fn = unsafe { super::super::syscache::agg_serialize_fn(aggfnoid) }
                .unwrap_or(pg_sys::InvalidOid);
            Some((AggOp::Avg, AggClass::Float8Stats { serialize_fn }))
        }
        pg_sys::F_STDDEV_SAMP_INT2
        | pg_sys::F_STDDEV_SAMP_INT4
        | pg_sys::F_STDDEV_SAMP_INT8
        | pg_sys::F_STDDEV_SAMP_FLOAT4
        | pg_sys::F_STDDEV_SAMP_FLOAT8
        | pg_sys::F_STDDEV_SAMP_NUMERIC
        | pg_sys::F_STDDEV_INT2
        | pg_sys::F_STDDEV_INT4
        | pg_sys::F_STDDEV_INT8
        | pg_sys::F_STDDEV_FLOAT4
        | pg_sys::F_STDDEV_FLOAT8
        | pg_sys::F_STDDEV_NUMERIC => {
            // SAFETY: caller contract.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let serialize_fn = unsafe { super::super::syscache::agg_serialize_fn(aggfnoid) }
                .unwrap_or(pg_sys::InvalidOid);
            Some((AggOp::StddevSamp, AggClass::Float8Stats { serialize_fn }))
        }
        pg_sys::F_STDDEV_POP_INT2
        | pg_sys::F_STDDEV_POP_INT4
        | pg_sys::F_STDDEV_POP_INT8
        | pg_sys::F_STDDEV_POP_FLOAT4
        | pg_sys::F_STDDEV_POP_FLOAT8
        | pg_sys::F_STDDEV_POP_NUMERIC => {
            // SAFETY: caller contract.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let serialize_fn = unsafe { super::super::syscache::agg_serialize_fn(aggfnoid) }
                .unwrap_or(pg_sys::InvalidOid);
            Some((AggOp::StddevPop, AggClass::Float8Stats { serialize_fn }))
        }
        pg_sys::F_VAR_SAMP_INT2
        | pg_sys::F_VAR_SAMP_INT4
        | pg_sys::F_VAR_SAMP_INT8
        | pg_sys::F_VAR_SAMP_FLOAT4
        | pg_sys::F_VAR_SAMP_FLOAT8
        | pg_sys::F_VAR_SAMP_NUMERIC => {
            // SAFETY: caller contract.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let serialize_fn = unsafe { super::super::syscache::agg_serialize_fn(aggfnoid) }
                .unwrap_or(pg_sys::InvalidOid);
            Some((AggOp::VarSamp, AggClass::Float8Stats { serialize_fn }))
        }
        pg_sys::F_VAR_POP_INT2
        | pg_sys::F_VAR_POP_INT4
        | pg_sys::F_VAR_POP_INT8
        | pg_sys::F_VAR_POP_FLOAT4
        | pg_sys::F_VAR_POP_FLOAT8
        | pg_sys::F_VAR_POP_NUMERIC => {
            // SAFETY: caller contract.
            let aggfnoid = unsafe { (*aggref).aggfnoid };
            let serialize_fn = unsafe { super::super::syscache::agg_serialize_fn(aggfnoid) }
                .unwrap_or(pg_sys::InvalidOid);
            Some((AggOp::VarPop, AggClass::Float8Stats { serialize_fn }))
        }

        // --- BIT_AND / BIT_OR -------------------------------------------
        pg_sys::F_BIT_AND_INT2 => Some((
            AggOp::Passthrough,
            AggClass::BitReduction {
                transtype: pg_sys::INT2OID,
                op: BitOp::And,
            },
        )),
        pg_sys::F_BIT_AND_INT4 => Some((
            AggOp::Passthrough,
            AggClass::BitReduction {
                transtype: pg_sys::INT4OID,
                op: BitOp::And,
            },
        )),
        pg_sys::F_BIT_AND_INT8 => Some((
            AggOp::Passthrough,
            AggClass::BitReduction {
                transtype: pg_sys::INT8OID,
                op: BitOp::And,
            },
        )),

        // --- BOOL_AND / BOOL_OR -----------------------------------------
        pg_sys::F_BOOL_AND => Some((
            AggOp::Passthrough,
            AggClass::BoolReduction { op: BoolOp::And },
        )),
        pg_sys::F_BOOL_OR => Some((
            AggOp::Passthrough,
            AggClass::BoolReduction { op: BoolOp::Or },
        )),

        _ => None,
    }
}

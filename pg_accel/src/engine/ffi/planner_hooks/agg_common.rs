//! Shared helpers used by both `agg.rs` (non-parallel) and `partial_agg.rs`
//! (parallel):
//! - Aggref classification (`Aggref → AggOp` + transtype / serialize-fn metadata)
//! - Target-list walking
//! - Cost estimation
//!
//! The body of `pgaccel_inject_gpu_agg` still lives in `mod.rs`;
//! these helpers are additive metadata used by `partial_agg::try_inject` to
//! build a `PartialAggSpec` without re-walking the target list twice.

use pgrx::pg_sys;

use crate::engine::executor::agg::AggOp;

/// Whether finalize-mode grouped AVG is safe to plan.
///
/// Keep this `false` until the finalize-mode grouped executor emits a true
/// average. Today that path maps `AggOp::Avg` through the SUM FFI lane and the
/// hash-agg result has no per-group count lane available to finalize `sum / N`.
#[must_use]
pub(super) const fn grouped_avg_finalize_supported() -> bool {
    false
}

/// True when an `Aggref` is one of PostgreSQL's built-in AVG aggregates.
///
/// # Safety
///
/// `aggref` must be a valid `Aggref` pointer on the main backend thread.
pub(super) unsafe fn aggref_is_avg(aggref: *const pg_sys::Aggref) -> bool {
    // SAFETY: caller contract.
    matches!(unsafe { classify_aggref(aggref) }, Some((AggOp::Avg, _)))
}

/// True when an aggregate is PostgreSQL's built-in `SUM(bigint)`.
///
/// The aggregate OID check is the authoritative guard. `arg_type_oid` is a
/// fallback for expression forms where the caller already resolved the input
/// type, but relying only on a plain `Var` argument misses cast-wrapped trees.
#[must_use]
pub(super) fn aggref_is_sum_int8(
    aggref: &pg_sys::Aggref,
    op: AggOp,
    arg_type_oid: pg_sys::Oid,
) -> bool {
    aggref.aggfnoid == pg_sys::Oid::from(pg_sys::F_SUM_INT8)
        || (matches!(op, AggOp::Sum) && arg_type_oid == pg_sys::INT8OID)
}

/// Classification of an `Aggref` node into one of the partial-agg categories
/// recognised by pg_accel. Drives `PartialColumn` construction in
/// `partial_agg::try_inject`.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: variant payloads (transtype, op) form the
// planner contract. partial_agg.rs reads
// AggClass::Float8Stats { serialize_fn } today; the
// ScalarPassthrough { transtype }, BitReduction
// { transtype, op }, and BoolReduction { op } payloads
// are constructed but not yet consumed — Phase 3a/b
// will read them when the executor grows group-keyed
// partial-state emit. Keep the payloads to avoid an
// ABI break when that work lands.
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
#[allow(dead_code)] // reason: variants are payload of AggClass::BitReduction;
// both are constructed by classify_aggref but the
// executor doesn't yet branch on which one — same Phase
// 3a/b unblock as AggClass.
pub(super) enum BitOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: variants are payload of AggClass::BoolReduction;
// see BitOp.
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
        // SUM(int8) promotes to NUMERIC result. Keep it classified so the
        // non-parallel direct typed i64 reduce can run; partial injectors
        // apply a separate guard because PG's parallel combine expects its
        // internal transition-state shape, not a scalar final sum.
        pg_sys::F_SUM_INT8 => Some((AggOp::Sum, AggClass::NumericSum)),
        // SUM(numeric) is rejected at classification: PG NUMERIC is arbitrary-precision,
        // but the partial-agg accumulator (`ColumnAccumulator.sum`) is f64 and silently
        // loses precision above 2^53. Returning None here forces the partial-agg planner
        // (partial_agg::try_inject) to bail, so PG handles SUM(numeric) natively.
        //
        // Option A (full fidelity multi-limb accumulator kernel) is tracked in TODO.md
        // "Post-1.0 (deferred)" under "NUMERIC multi-limb accumulator kernel".
        //
        // Defense in depth: partial path builders reject SUM(int8)/SUM(numeric)
        // before emitter construction until a real internal-state emitter lands.
        pg_sys::F_SUM_NUMERIC => None,
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

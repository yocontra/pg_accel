//! Typed SysCache readers for pg_accel.
//!
//! Wraps `SearchSysCache1(AGGFNOID, …)` / `GETSTRUCT` / `ReleaseSysCache`
//! in safe Rust returning `Option<Oid>`. Used by planner hooks to resolve
//! each Aggref's transition type and serialize function at plan time.
//!
//! Worker 5 fills in the bodies.

use pgrx::pg_sys;

/// Look up `pg_aggregate.aggserialfn` for an aggregate OID.
///
/// Returns `Some(fn_oid)` if the aggregate has a non-zero serialize fn,
/// `None` otherwise (including when the aggregate doesn't exist).
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_serialize_fn(_aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    todo!("Worker 5: agg_serialize_fn")
}

/// Look up `pg_aggregate.aggdeserialfn` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_deserialize_fn(_aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    todo!("Worker 5: agg_deserialize_fn")
}

/// Look up `pg_aggregate.aggtranstype` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_transtype(_aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    todo!("Worker 5: agg_transtype")
}

/// Look up `pg_aggregate.aggfinalfn` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_finalfn(_aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    todo!("Worker 5: agg_finalfn")
}

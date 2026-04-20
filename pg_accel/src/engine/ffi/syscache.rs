//! Typed SysCache readers for pg_accel.
//!
//! Wraps `SearchSysCache1(AGGFNOID, …)` / `SysCacheGetAttr` / `ReleaseSysCache`
//! in safe Rust returning `Option<Oid>`. Used by planner hooks to resolve
//! each Aggref's transition type and serialize function at plan time.

use pgrx::pg_sys;

// pg_aggregate column attnos (see src/include/catalog/pg_aggregate_d.h).
const ANUM_PG_AGGREGATE_AGGFINALFN: i16 = 5;
const ANUM_PG_AGGREGATE_AGGSERIALFN: i16 = 7;
const ANUM_PG_AGGREGATE_AGGDESERIALFN: i16 = 8;
const ANUM_PG_AGGREGATE_AGGTRANSTYPE: i16 = 17;

// pg_proc column attno for `proparallel` (see pg_proc_d.h:
// `Anum_pg_proc_proparallel 16`).
const ANUM_PG_PROC_PROPARALLEL: i16 = 16;

/// Look up a `regproc`/`Oid` column from the `AGGFNOID` syscache row
/// for `aggfnoid`. Returns `None` when the row is absent or the column
/// is NULL or `InvalidOid`.
///
/// # Safety
/// Must be called on the main backend thread. Acquires + releases a
/// syscache pin around the attr read.
#[allow(dead_code)]
unsafe fn agg_cache_oid_attr(aggfnoid: pg_sys::Oid, attnum: i16) -> Option<pg_sys::Oid> {
    // SAFETY: SearchSysCache1 with AGGFNOID expects a single Oid datum.
    // `aggfnoid.into()` converts into `Datum`.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::AGGFNOID as ::core::ffi::c_int,
            aggfnoid.into(),
        )
    };
    if tuple.is_null() {
        return None;
    }
    let mut isnull: bool = false;
    // SAFETY: tuple is a valid HeapTuple; isnull is a stack bool.
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::AGGFNOID as ::core::ffi::c_int,
            tuple,
            attnum,
            &raw mut isnull,
        )
    };
    // SAFETY: matches the SearchSysCache1 above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    if isnull {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let oid = pg_sys::Oid::from(datum.value() as u32);
    if oid == pg_sys::InvalidOid {
        None
    } else {
        Some(oid)
    }
}

/// Look up `pg_aggregate.aggserialfn` for an aggregate OID.
///
/// Returns `Some(fn_oid)` if the aggregate has a non-zero serialize fn,
/// `None` otherwise (including when the aggregate doesn't exist).
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_serialize_fn(aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: delegates to the shared syscache helper.
    unsafe { agg_cache_oid_attr(aggfnoid, ANUM_PG_AGGREGATE_AGGSERIALFN) }
}

/// Look up `pg_aggregate.aggdeserialfn` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_deserialize_fn(aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: delegates to the shared syscache helper.
    unsafe { agg_cache_oid_attr(aggfnoid, ANUM_PG_AGGREGATE_AGGDESERIALFN) }
}

/// Look up `pg_aggregate.aggtranstype` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_transtype(aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: delegates to the shared syscache helper.
    unsafe { agg_cache_oid_attr(aggfnoid, ANUM_PG_AGGREGATE_AGGTRANSTYPE) }
}

/// Look up `pg_aggregate.aggfinalfn` for an aggregate OID.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn agg_finalfn(aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: delegates to the shared syscache helper.
    unsafe { agg_cache_oid_attr(aggfnoid, ANUM_PG_AGGREGATE_AGGFINALFN) }
}

/// Returns `true` iff the function identified by `fn_oid` is marked
/// `PARALLEL SAFE` (`proparallel == 's'`).
///
/// Used to conservatively reject partial-agg injection when a pushed-down
/// expression or comparison operator is not parallel-safe.
///
/// # Safety
/// Must be called on the main backend thread.
#[allow(dead_code)]
pub unsafe fn op_is_parallel_safe(fn_oid: pg_sys::Oid) -> bool {
    if fn_oid == pg_sys::InvalidOid {
        return false;
    }
    // SAFETY: PROCOID syscache takes a single Oid datum.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::PROCOID as ::core::ffi::c_int,
            fn_oid.into(),
        )
    };
    if tuple.is_null() {
        return false;
    }
    let mut isnull: bool = false;
    // SAFETY: tuple is valid; isnull is a stack bool.
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::PROCOID as ::core::ffi::c_int,
            tuple,
            ANUM_PG_PROC_PROPARALLEL,
            &raw mut isnull,
        )
    };
    // SAFETY: matches SearchSysCache1 above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    if isnull {
        return false;
    }
    // proparallel is a single char ('s' = safe, 'r' = restricted, 'u' = unsafe).
    #[allow(clippy::cast_possible_truncation)]
    let c = datum.value() as u8;
    c == b's'
}

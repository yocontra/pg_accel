//! Typed SysCache readers for pg_accel.
//!
//! Wraps `SearchSysCache1(AGGFNOID, …)` / `SysCacheGetAttr` / `ReleaseSysCache`
//! in safe Rust returning `Option<Oid>`. Used by planner hooks to resolve
//! each Aggref's serialize function at plan time.

use pgrx::pg_sys;

// pg_aggregate column attnos (see src/include/catalog/pg_aggregate_d.h).
const ANUM_PG_AGGREGATE_AGGSERIALFN: i16 = 7;

/// Look up a `regproc`/`Oid` column from the `AGGFNOID` syscache row
/// for `aggfnoid`. Returns `None` when the row is absent or the column
/// is NULL or `InvalidOid`.
///
/// # Safety
/// Must be called on the main backend thread. Acquires + releases a
/// syscache pin around the attr read.
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
pub unsafe fn agg_serialize_fn(aggfnoid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: delegates to the shared syscache helper.
    unsafe { agg_cache_oid_attr(aggfnoid, ANUM_PG_AGGREGATE_AGGSERIALFN) }
}

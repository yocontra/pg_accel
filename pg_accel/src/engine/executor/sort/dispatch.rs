//! Sort dispatch helpers — comparator fallbacks invoked via FFI.

use pgrx::pg_sys;

/// Trivial comparator fallback that treats all values as equal.
/// Used only if `SortSupportData.comparator` is `None`, which should
/// never happen after `PrepareSortSupportFromOrderingOp`.
///
/// # Safety
///
/// Must be called by PostgreSQL's sort infrastructure with valid Datum
/// arguments and a valid `SortSupport` pointer. This function does not
/// dereference any of its arguments, so it is trivially safe.
pub(super) unsafe extern "C-unwind" fn trivial_cmp(
    _a: pg_sys::Datum,
    _b: pg_sys::Datum,
    _ssup: pg_sys::SortSupport,
) -> std::ffi::c_int {
    0
}

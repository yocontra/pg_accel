//! Small PostgreSQL ABI compatibility wrappers used by executor code.

use pgrx::pg_sys;

/// Return a tuple descriptor attribute by zero-based index.
///
/// PostgreSQL 18 compacted `TupleDescData`; using the exported
/// `TupleDescAttr` shim avoids direct layout access.
pub unsafe fn tuple_desc_attr(
    tupdesc: pg_sys::TupleDesc,
    idx: usize,
) -> *mut pg_sys::FormData_pg_attribute {
    debug_assert!(!tupdesc.is_null());
    unsafe { pg_sys::TupleDescAttr(tupdesc, idx as i32) }
}

/// Copy a minimal tuple without reserving extra bytes.
pub unsafe fn heap_copy_minimal_tuple(mtup: pg_sys::MinimalTuple) -> pg_sys::MinimalTuple {
    unsafe { pg_sys::heap_copy_minimal_tuple(mtup, 0) }
}

/// Convert a heap tuple to a minimal tuple without reserving extra bytes.
pub unsafe fn minimal_tuple_from_heap_tuple(htup: pg_sys::HeapTuple) -> pg_sys::MinimalTuple {
    unsafe { pg_sys::minimal_tuple_from_heap_tuple(htup, 0) }
}

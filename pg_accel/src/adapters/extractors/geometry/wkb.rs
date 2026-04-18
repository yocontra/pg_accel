//! `GSERIALIZED` datum detoast helper.

use pgrx::pg_sys::{self, Datum};

use super::header::MIN_HEADER_LEN;

/// Convert a `Datum` to owned bytes over the detoasted `GSERIALIZED` varlena.
///
/// Returns `None` if the datum is null (zero) or the varlena is too small.
pub(super) fn datum_to_gserialized_bytes(datum: Datum) -> Option<Vec<u8>> {
    if datum.value() == 0 {
        return None;
    }

    // SAFETY: Detoast the datum to get a flat, uncompressed varlena.
    // This handles TOAST pointers, compressed varlenas, and short headers.
    // Must run on the main backend thread.
    //
    // In `cfg(test)` builds (standalone `cargo test`, not `cargo pgrx test`),
    // libtest spawns a fresh thread per test and pgrx's guarded wrapper
    // panics with "postgres FFI may not be called from multiple threads".
    // The identity stub in `src/pg_stubs.rs` is what the linker resolves
    // `pg_detoast_datum` against on macOS, so calling the raw extern
    // directly gives us a safe passthrough without the thread guard.
    // Tests only use non-TOAST flat varlenas, so identity is correct.
    #[cfg(not(test))]
    let detoasted =
        unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>()) };
    #[cfg(test)]
    let detoasted = {
        unsafe extern "C" {
            fn pg_detoast_datum(datum: *mut pgrx::pg_sys::varlena) -> *mut pgrx::pg_sys::varlena;
        }
        unsafe { pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>()) }
    };
    if detoasted.is_null() {
        return None;
    }

    // SAFETY: detoasted is a valid flat varlena. VARSIZE returns total
    // size including the 4-byte header.
    let total_size = unsafe { pgrx::varsize(detoasted.cast()) };

    if total_size < MIN_HEADER_LEN {
        return None;
    }

    // SAFETY: `total_size` bytes starting at `detoasted` are the flat
    // varlena payload. Copy into owned Vec — PG memory may be freed
    // after tuple processing.
    let ptr = detoasted as *const u8;
    let bytes = unsafe { std::slice::from_raw_parts(ptr, total_size) };
    Some(bytes.to_vec())
}

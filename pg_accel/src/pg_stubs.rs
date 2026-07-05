//! PG symbol stubs for macOS test binaries.
//!
//! See `build.rs::pg_stub` for the rationale. This file is only compiled
//! under `cfg(all(test, target_os = "macos"))` and pulls in an
//! auto-generated source file from `OUT_DIR` containing one
//! `#[no_mangle] pub static NAME: u8 = 0;` per exported postgres symbol.
//!
//! The stubs satisfy dyld at load time on macOS Sequoia+. They are
//! never executed at runtime: pgrx tests run inside a real postgres
//! process where the genuine implementations are provided.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

include!(env!("PG_STUBS_GENERATED"));

// -- Manual passthrough stubs -------------------------------------------
//
// Some CPU-only unit tests exercise code paths that call PG FFI (e.g.
// `pg_detoast_datum`) even with a synthetic non-TOAST input. Provide
// identity implementations for those functions so the tests can run
// without a real postgres runtime. These names are excluded from the
// auto-generated stub list in `build.rs::pg_stub::is_manual_stub`.

/// Identity stub: `pg_detoast_datum` on a non-TOAST varlena returns
/// the input pointer unchanged inside real PG, so an identity function
/// is a correct stand-in for unit tests that pass flat buffers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pg_detoast_datum(datum: *mut u8) -> *mut u8 {
    datum
}

/// Same semantics as `pg_detoast_datum` — tests only use non-TOAST inputs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pg_detoast_datum_copy(datum: *mut u8) -> *mut u8 {
    datum
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pg_detoast_datum_packed(datum: *mut u8) -> *mut u8 {
    datum
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pg_detoast_datum_slice(
    datum: *mut u8,
    _first: i32,
    _count: i32,
) -> *mut u8 {
    datum
}

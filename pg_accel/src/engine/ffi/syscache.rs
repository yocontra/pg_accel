//! Typed SysCache readers for pg_accel.
//!
//! Wraps syscache/catalog helpers used by planner hooks.

use pgrx::{FromDatum, PgLogLevel, pg_sys, prelude::PgSqlErrorCode};

mod postgis;
mod raster;

pub use postgis::{PostgisCatalogIdentity, PostgisSpatialFunction, resolve_postgis_catalog};
pub use raster::{
    PostgisRasterCatalogIdentity, PostgisRasterFunction, postgis_raster_datum_from_wkb,
    postgis_raster_datum_to_wkb, resolve_postgis_raster_catalog, resolve_postgis_raster_function,
    validate_postgis_raster_type,
};

pub(crate) fn postgres_error_requires_rethrow(level: PgLogLevel, code: PgSqlErrorCode) -> bool {
    use PgSqlErrorCode::{
        ERRCODE_DATA_EXCEPTION, ERRCODE_DATATYPE_MISMATCH, ERRCODE_EXTERNAL_ROUTINE_EXCEPTION,
        ERRCODE_FEATURE_NOT_SUPPORTED, ERRCODE_INSUFFICIENT_PRIVILEGE,
        ERRCODE_INVALID_PARAMETER_VALUE, ERRCODE_INVALID_TEXT_REPRESENTATION,
        ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE, ERRCODE_UNDEFINED_COLUMN, ERRCODE_UNDEFINED_FUNCTION,
        ERRCODE_UNDEFINED_TABLE,
    };
    if matches!(level, PgLogLevel::FATAL | PgLogLevel::PANIC) {
        return true;
    }

    // Only expected catalog-race, authorization, type, and datum-conversion
    // failures may cross a PgTry boundary as an ordinary Rust error.
    !matches!(
        code,
        ERRCODE_DATA_EXCEPTION
            | ERRCODE_DATATYPE_MISMATCH
            | ERRCODE_EXTERNAL_ROUTINE_EXCEPTION
            | ERRCODE_FEATURE_NOT_SUPPORTED
            | ERRCODE_INSUFFICIENT_PRIVILEGE
            | ERRCODE_INVALID_PARAMETER_VALUE
            | ERRCODE_INVALID_TEXT_REPRESENTATION
            | ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE
            | ERRCODE_UNDEFINED_COLUMN
            | ERRCODE_UNDEFINED_FUNCTION
            | ERRCODE_UNDEFINED_TABLE
    )
}

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

/// Resolve a function OID to `(schema, function name)`.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL syscache helpers.
pub unsafe fn function_schema_and_name(fn_oid: pg_sys::Oid) -> Option<(String, String)> {
    if fn_oid == pg_sys::Oid::INVALID {
        return None;
    }

    // SAFETY: the caller guarantees backend-thread catalog access and fn_oid is
    // non-invalid; PostgreSQL returns null or a context-owned C string.
    let name_ptr = unsafe { pg_sys::get_func_name(fn_oid) };
    if name_ptr.is_null() {
        return None;
    }
    // SAFETY: a non-null get_func_name result is NUL-terminated and remains live
    // in the current PostgreSQL memory context while it is copied here.
    let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) }
        .to_str()
        .ok()?
        .to_ascii_lowercase();

    // SAFETY: fn_oid is the same live function catalog OID and the caller holds
    // the backend-thread catalog invariant.
    let namespace_oid = unsafe { pg_sys::get_func_namespace(fn_oid) };
    if namespace_oid == pg_sys::InvalidOid {
        return None;
    }
    // SAFETY: namespace_oid is non-invalid and catalog access remains on the
    // backend thread; PostgreSQL returns null or a context-owned C string.
    let namespace_ptr = unsafe { pg_sys::get_namespace_name(namespace_oid) };
    if namespace_ptr.is_null() {
        return None;
    }
    // SAFETY: a non-null namespace name is NUL-terminated and remains live in
    // the current PostgreSQL memory context while it is copied here.
    let schema = unsafe { std::ffi::CStr::from_ptr(namespace_ptr) }
        .to_str()
        .ok()?
        .to_ascii_lowercase();

    Some((schema, name))
}

unsafe fn object_is_extension_member(
    class_oid: pg_sys::Oid,
    object_oid: pg_sys::Oid,
    extname: &str,
) -> bool {
    if object_oid == pg_sys::Oid::INVALID {
        return false;
    }

    let extname_cstr = match std::ffi::CString::new(extname) {
        Ok(extname) => extname,
        Err(_) => return false,
    };
    // SAFETY: extname_cstr is NUL-terminated for the duration of this backend
    // catalog lookup; missing_ok=true permits an absent extension.
    let extension_oid = unsafe { pg_sys::get_extension_oid(extname_cstr.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return false;
    }

    // SAFETY: class_oid selects a PostgreSQL catalog and object_oid is non-invalid;
    // dependency catalog access occurs on the caller-guaranteed backend thread.
    (unsafe { pg_sys::getExtensionOfObject(class_oid, object_oid) }) == extension_oid
}

/// Return true when a function belongs to the named extension.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL extension/dependency helpers.
pub unsafe fn function_is_extension_member(fn_oid: pg_sys::Oid, extname: &str) -> bool {
    // SAFETY: this function forwards its backend-thread catalog requirement and
    // the ProcedureRelationId class for fn_oid.
    unsafe { object_is_extension_member(pg_sys::ProcedureRelationId, fn_oid, extname) }
}

/// Return true when a type belongs to the named extension.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL extension/dependency helpers.
pub unsafe fn type_is_extension_member(type_oid: pg_sys::Oid, extname: &str) -> bool {
    // SAFETY: this function forwards its backend-thread catalog requirement and
    // the TypeRelationId class for type_oid.
    unsafe { object_is_extension_member(pg_sys::TypeRelationId, type_oid, extname) }
}

/// Resolve a type OID to a lowercase SQL type name.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL syscache helpers.
pub unsafe fn type_name(type_oid: pg_sys::Oid) -> Option<String> {
    if type_oid == pg_sys::Oid::INVALID {
        return None;
    }

    // SAFETY: TYPEOID expects one OID Datum and the caller guarantees backend
    // syscache access; a non-null result remains pinned until ReleaseSysCache.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return None;
    }

    let mut isnull: bool = false;
    // SAFETY: tuple remains pinned and typname is an attribute of its TYPEOID
    // row; isnull points to writable stack storage for the call.
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            tuple,
            pg_sys::Anum_pg_type_typname as i16,
            &raw mut isnull,
        )
    };
    if isnull {
        // SAFETY: releases exactly the non-null TYPEOID pin acquired above.
        unsafe { pg_sys::ReleaseSysCache(tuple) };
        return None;
    }

    let name_ptr = datum.cast_mut_ptr::<pg_sys::NameData>();
    if name_ptr.is_null() {
        // SAFETY: releases exactly the non-null TYPEOID pin acquired above.
        unsafe { pg_sys::ReleaseSysCache(tuple) };
        return None;
    }
    // SAFETY: while tuple is pinned, its non-null typname Datum points to a
    // fixed-size, NUL-terminated NameData; the result is copied before release.
    let name = unsafe { std::ffi::CStr::from_ptr((*name_ptr).data.as_ptr()) }
        .to_str()
        .ok()
        .map(str::to_ascii_lowercase);
    // SAFETY: name no longer borrows the row, so the matching TYPEOID pin can be released.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    name
}

/// Resolve a type OID by exact schema and type name.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL syscache helpers.
#[allow(dead_code)]
pub unsafe fn type_oid_in_schema(schema: &str, type_name: &str) -> Option<pg_sys::Oid> {
    let schema_cstr = std::ffi::CString::new(schema).ok()?;
    let type_name_cstr = std::ffi::CString::new(type_name).ok()?;

    // SAFETY: schema_cstr is NUL-terminated for this backend catalog lookup;
    // missing_ok=true permits an absent namespace.
    let namespace_oid = unsafe { pg_sys::get_namespace_oid(schema_cstr.as_ptr(), true) };
    if namespace_oid == pg_sys::InvalidOid {
        return None;
    }

    // SAFETY: TYPENAMENSP expects the NUL-terminated type name and resolved
    // namespace OID; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::Datum::from(type_name_cstr.as_ptr()),
            pg_sys::ObjectIdGetDatum(namespace_oid),
        )
    };
    if tuple.is_null() {
        return None;
    }

    let mut isnull: bool = false;
    // SAFETY: tuple remains pinned and oid is an attribute of its TYPENAMENSP
    // row; isnull points to writable stack storage for the call.
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            tuple,
            pg_sys::Anum_pg_type_oid as i16,
            &raw mut isnull,
        )
    };
    // SAFETY: datum is by-value Oid data, so the matching syscache pin can be
    // released before decoding it.
    unsafe { pg_sys::ReleaseSysCache(tuple) };

    if isnull {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let oid = pg_sys::Oid::from(datum.value() as u32);
    (oid != pg_sys::InvalidOid).then_some(oid)
}

const H3_EXTENSION_NAME: &str = "h3";
const H3_TYPE_NAME: &str = "h3index";
const H3_PARENT_FUNCTION_NAME: &str = "h3_cell_to_parent";
const H3_EQUALITY_FUNCTION_NAME: &str = "h3index_eq";
const H3_EQUALITY_OPERATOR_NAME: &str = "=";
const H3_LANGUAGE_NAME: &str = "c";
// h3-pg declares `AS 'h3'`; PostgreSQL stores that C library token verbatim.
const H3_LIBRARY_NAME: &str = "h3";
const PG_OPERATOR_KIND_BINARY: u8 = b'b';
const H3_FINGERPRINT_VERSION: u32 = 3;

/// Exact catalog identity accepted by the H3 acceleration path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H3CatalogIdentity {
    pub extension_oid: pg_sys::Oid,
    pub schema_oid: pg_sys::Oid,
    pub type_oid: pg_sys::Oid,
    pub parent_fn_oid: pg_sys::Oid,
    pub equality_op_oid: pg_sys::Oid,
    pub equality_fn_oid: pg_sys::Oid,
    pub fingerprint_words: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct H3TypeShape {
    type_oid: pg_sys::Oid,
    name: String,
    schema_oid: pg_sys::Oid,
    typlen: i16,
    typbyval: bool,
    typtype: u8,
    typisdefined: bool,
    typrelid: pg_sys::Oid,
    typsubscript: pg_sys::Oid,
    typelem: pg_sys::Oid,
    typalign: u8,
    typstorage: u8,
    typbasetype: pg_sys::Oid,
    typndims: i32,
    typcollation: pg_sys::Oid,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct H3FunctionShape {
    fn_oid: pg_sys::Oid,
    name: String,
    schema_oid: pg_sys::Oid,
    language_oid: pg_sys::Oid,
    language_name: String,
    kind: u8,
    argument_defaults: i16,
    security_definer: bool,
    support_function: pg_sys::Oid,
    strict: bool,
    returns_set: bool,
    volatility: u8,
    parallel: u8,
    variadic_type: pg_sys::Oid,
    return_type: pg_sys::Oid,
    argument_types: Vec<pg_sys::Oid>,
    source: String,
    binary: Option<String>,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct H3OperatorShape {
    operator_oid: pg_sys::Oid,
    name: String,
    schema_oid: pg_sys::Oid,
    kind: u8,
    can_merge: bool,
    can_hash: bool,
    left_type: pg_sys::Oid,
    right_type: pg_sys::Oid,
    result_type: pg_sys::Oid,
    commutator_oid: pg_sys::Oid,
    function_oid: pg_sys::Oid,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct H3TypeProof {
    extension_oid: pg_sys::Oid,
    shape: H3TypeShape,
}

fn oid_word(oid: pg_sys::Oid) -> i32 {
    i32::from_ne_bytes(u32::from(oid).to_ne_bytes())
}

fn u32_word(value: u32) -> i32 {
    i32::from_ne_bytes(value.to_ne_bytes())
}

fn u64_words(value: u64) -> [i32; 2] {
    [u32_word(value as u32), u32_word((value >> 32) as u32)]
}

fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in *part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn validate_h3_type_shape(shape: &H3TypeShape, schema_oid: pg_sys::Oid) -> Result<(), String> {
    if shape.name != H3_TYPE_NAME {
        return Err(format!(
            "H3 type OID {} is named {:?}, expected {H3_TYPE_NAME:?}",
            u32::from(shape.type_oid),
            shape.name
        ));
    }
    if shape.schema_oid != schema_oid {
        return Err(format!(
            "H3 type OID {} is outside extension schema OID {}",
            u32::from(shape.type_oid),
            u32::from(schema_oid)
        ));
    }
    if shape.typtype != pg_sys::TYPTYPE_BASE {
        return Err("H3 h3index must be a base type".to_owned());
    }
    if !shape.typisdefined {
        return Err("H3 h3index is only a shell type".to_owned());
    }
    if shape.typlen != 8 || !shape.typbyval {
        return Err("H3 h3index must be an 8-byte pass-by-value type".to_owned());
    }
    if shape.typalign != pg_sys::TYPALIGN_DOUBLE {
        return Err("H3 h3index must use double alignment".to_owned());
    }
    if shape.typstorage != pg_sys::TYPSTORAGE_PLAIN {
        return Err("H3 h3index must use plain storage".to_owned());
    }
    if shape.typrelid != pg_sys::InvalidOid
        || shape.typsubscript != pg_sys::InvalidOid
        || shape.typelem != pg_sys::InvalidOid
        || shape.typbasetype != pg_sys::InvalidOid
        || shape.typndims != 0
    {
        return Err("H3 h3index must not be composite, domain, or array-like".to_owned());
    }
    if shape.typcollation != pg_sys::InvalidOid {
        return Err("H3 h3index must not be collatable".to_owned());
    }
    Ok(())
}

fn validate_h3_c_function_shape(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    expected_name: &str,
    expected_arguments: &[pg_sys::Oid],
    expected_return_type: pg_sys::Oid,
) -> Result<(), String> {
    if shape.name != expected_name || shape.schema_oid != schema_oid {
        return Err(format!(
            "function OID {} is not extension-schema {expected_name}",
            u32::from(shape.fn_oid),
        ));
    }
    if shape.kind != pg_sys::PROKIND_FUNCTION {
        return Err(format!("H3 {expected_name} must be an ordinary function"));
    }
    if shape.language_name != H3_LANGUAGE_NAME {
        return Err(format!("H3 {expected_name} must use the C language"));
    }
    if shape.source != expected_name || shape.binary.as_deref() != Some(H3_LIBRARY_NAME) {
        return Err(format!(
            "H3 {expected_name} must use canonical C symbol {expected_name} from library h3"
        ));
    }
    if shape.argument_types != expected_arguments || shape.return_type != expected_return_type {
        return Err(format!("H3 {expected_name} has a noncanonical signature"));
    }
    if shape.argument_defaults != 0 {
        return Err(format!(
            "H3 {expected_name} must not declare argument defaults"
        ));
    }
    if shape.security_definer {
        return Err(format!("H3 {expected_name} must not be security definer"));
    }
    if shape.support_function != pg_sys::InvalidOid {
        return Err(format!(
            "H3 {expected_name} must not declare a planner support function"
        ));
    }
    if shape.variadic_type != pg_sys::InvalidOid || shape.returns_set {
        return Err(format!("H3 {expected_name} must be scalar and nonvariadic"));
    }
    if !shape.strict {
        return Err(format!("H3 {expected_name} must be strict"));
    }
    if shape.volatility != pg_sys::PROVOLATILE_IMMUTABLE {
        return Err(format!("H3 {expected_name} must be immutable"));
    }
    if shape.parallel != pg_sys::PROPARALLEL_SAFE {
        return Err(format!("H3 {expected_name} must be parallel safe"));
    }
    Ok(())
}

fn validate_h3_function_shape(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<(), String> {
    validate_h3_c_function_shape(
        shape,
        schema_oid,
        H3_PARENT_FUNCTION_NAME,
        &[type_oid, pg_sys::INT4OID],
        type_oid,
    )
}

fn validate_h3_equality_function_shape(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<(), String> {
    validate_h3_c_function_shape(
        shape,
        schema_oid,
        H3_EQUALITY_FUNCTION_NAME,
        &[type_oid, type_oid],
        pg_sys::BOOLOID,
    )
}

fn validate_h3_equality_operator_shape(
    shape: &H3OperatorShape,
    schema_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<(), String> {
    if shape.name != H3_EQUALITY_OPERATOR_NAME || shape.schema_oid != schema_oid {
        return Err(format!(
            "operator OID {} is not the extension-schema H3 equality operator",
            u32::from(shape.operator_oid)
        ));
    }
    if shape.kind != PG_OPERATOR_KIND_BINARY
        || shape.left_type != type_oid
        || shape.right_type != type_oid
        || shape.result_type != pg_sys::BOOLOID
    {
        return Err(
            "H3 equality operator must have signature (h3index, h3index) -> bool".to_owned(),
        );
    }
    if !shape.can_hash || !shape.can_merge {
        return Err("H3 equality operator must be hashable and mergeable".to_owned());
    }
    if shape.commutator_oid != shape.operator_oid {
        return Err("H3 equality operator must be its own commutator".to_owned());
    }
    if shape.function_oid == pg_sys::InvalidOid {
        return Err("H3 equality operator has no implementation function".to_owned());
    }
    Ok(())
}

fn type_fingerprint(shape: &H3TypeShape) -> Vec<i32> {
    vec![
        oid_word(shape.type_oid),
        oid_word(shape.schema_oid),
        u32_word(shape.tuple_xmin),
        u32_word(shape.tuple_block),
        i32::from(shape.tuple_offset),
        i32::from(shape.typlen),
        i32::from(shape.typalign),
        i32::from(shape.typstorage),
    ]
}

fn function_fingerprint(shape: &H3FunctionShape) -> Vec<i32> {
    let implementation_hash = fnv1a64(&[
        shape.name.as_bytes(),
        shape.language_name.as_bytes(),
        shape.source.as_bytes(),
        shape.binary.as_deref().unwrap_or_default().as_bytes(),
    ]);
    let [hash_low, hash_high] = u64_words(implementation_hash);
    let mut words = vec![
        oid_word(shape.fn_oid),
        oid_word(shape.schema_oid),
        oid_word(shape.language_oid),
        u32_word(shape.tuple_xmin),
        u32_word(shape.tuple_block),
        i32::from(shape.tuple_offset),
        i32::from(shape.kind),
        i32::from(shape.argument_defaults),
        i32::from(shape.security_definer),
        oid_word(shape.support_function),
        i32::from(shape.strict),
        i32::from(shape.returns_set),
        i32::from(shape.volatility),
        i32::from(shape.parallel),
        oid_word(shape.variadic_type),
        oid_word(shape.return_type),
        i32::try_from(shape.argument_types.len()).unwrap_or(i32::MAX),
        hash_low,
        hash_high,
    ];
    words.extend(shape.argument_types.iter().copied().map(oid_word));
    words
}

fn operator_fingerprint(shape: &H3OperatorShape) -> Vec<i32> {
    let [name_hash_low, name_hash_high] = u64_words(fnv1a64(&[shape.name.as_bytes()]));
    vec![
        oid_word(shape.operator_oid),
        oid_word(shape.schema_oid),
        u32_word(shape.tuple_xmin),
        u32_word(shape.tuple_block),
        i32::from(shape.tuple_offset),
        i32::from(shape.kind),
        i32::from(shape.can_merge),
        i32::from(shape.can_hash),
        oid_word(shape.left_type),
        oid_word(shape.right_type),
        oid_word(shape.result_type),
        oid_word(shape.commutator_oid),
        oid_word(shape.function_oid),
        name_hash_low,
        name_hash_high,
    ]
}

unsafe fn named_extension_identity(
    expected_name: &str,
    required_relocatable: Option<bool>,
) -> Result<(pg_sys::Oid, pg_sys::Oid), String> {
    let extension_name = std::ffi::CString::new(expected_name)
        .map_err(|_| format!("invalid extension name {expected_name:?}"))?;
    // SAFETY: extension_name is NUL-terminated for this backend catalog lookup;
    // missing_ok=true permits the not-installed error path below.
    let extension_oid = unsafe { pg_sys::get_extension_oid(extension_name.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return Err(format!("extension {expected_name} is not installed"));
    }

    // SAFETY: EXTENSIONOID expects the resolved OID Datum; a non-null result is
    // pinned until the matching ReleaseSysCache below.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::EXTENSIONOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(extension_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "extension {expected_name} disappeared during catalog validation"
        ));
    }
    // SAFETY: tuple is a live pinned pg_extension row for extension_oid; all
    // FormData fields and NameData bytes are read before the pin is released.
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_extension>();
        let name = std::ffi::CStr::from_ptr((*form).extname.data.as_ptr())
            .to_str()
            .map_err(|_| format!("extension {expected_name} has an invalid catalog name"))?;
        if name != expected_name {
            Err("extension OID resolved to an unexpected catalog row".to_owned())
        } else if required_relocatable.is_some_and(|value| value != (*form).extrelocatable) {
            Err(format!(
                "extension {expected_name} has unexpected relocatability"
            ))
        } else if (*form).extnamespace == pg_sys::InvalidOid {
            Err(format!(
                "extension {expected_name} has no installation schema"
            ))
        } else {
            Ok((extension_oid, (*form).extnamespace))
        }
    })();
    // SAFETY: result owns every value copied from the row, so the matching
    // EXTENSIONOID pin can now be released exactly once.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn extension_identity() -> Result<(pg_sys::Oid, pg_sys::Oid), String> {
    // SAFETY: forwards this helper's backend-thread catalog invariant and fixes
    // the expected H3 extension identity and relocatability.
    unsafe { named_extension_identity(H3_EXTENSION_NAME, Some(true)) }
}

unsafe fn find_exact_function(
    schema_oid: pg_sys::Oid,
    function_name: &str,
    argument_types: &[pg_sys::Oid],
) -> Result<pg_sys::Oid, String> {
    let function_name_c = std::ffi::CString::new(function_name)
        .map_err(|_| format!("invalid function name {function_name:?}"))?;
    let argument_count = i32::try_from(argument_types.len())
        .map_err(|_| format!("function {function_name} has too many argument types"))?;
    // SAFETY: argument_types.as_ptr() is valid for argument_count initialized
    // OIDs during the call; buildoidvector copies them into palloc storage.
    let argument_vector =
        unsafe { pg_sys::buildoidvector(argument_types.as_ptr(), argument_count) };
    if argument_vector.is_null() {
        return Err(format!(
            "could not build function signature for {function_name}"
        ));
    }
    // SAFETY: PROCNAMEARGSNSP expects a NUL-terminated name, the live oidvector,
    // and schema OID; a non-null result remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::Datum::from(function_name_c.as_ptr()),
            pg_sys::Datum::from(argument_vector),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    // SAFETY: buildoidvector returned this palloc-owned allocation and the
    // syscache lookup no longer borrows it after returning.
    unsafe { pg_sys::pfree(argument_vector.cast()) };
    if tuple.is_null() {
        return Err(format!(
            "schema OID {} has no exact {function_name} function",
            u32::from(schema_oid)
        ));
    }
    // SAFETY: tuple is a live pinned pg_proc row; GETSTRUCT has pg_proc layout
    // and oid is copied before releasing the pin.
    let fn_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    // SAFETY: releases exactly the non-null PROCNAMEARGSNSP pin acquired above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(fn_oid)
}

unsafe fn find_h3_type_oid(schema_oid: pg_sys::Oid) -> Result<pg_sys::Oid, String> {
    let type_name =
        std::ffi::CString::new(H3_TYPE_NAME).map_err(|_| "invalid H3 type name".to_owned())?;
    // SAFETY: TYPENAMENSP expects the NUL-terminated H3 type name and schema OID;
    // a non-null result remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::Datum::from(type_name.as_ptr()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "extension h3 has no h3index type in schema OID {}",
            u32::from(schema_oid)
        ));
    }
    // SAFETY: tuple is a live pinned pg_type row; GETSTRUCT has pg_type layout
    // and oid is copied before releasing the pin.
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    // SAFETY: releases exactly the non-null TYPENAMENSP pin acquired above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_h3_type_shape(type_oid: pg_sys::Oid) -> Result<H3TypeShape, String> {
    // SAFETY: TYPEOID expects one OID Datum and the caller guarantees backend
    // syscache access; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("type OID {} does not exist", u32::from(type_oid)));
    }
    // SAFETY: tuple is a live pinned pg_type row for type_oid; GETSTRUCT,
    // NameData, tuple header, and item-pointer fields are copied before release.
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        let name = std::ffi::CStr::from_ptr((*form).typname.data.as_ptr())
            .to_str()
            .map_err(|_| format!("type OID {} has an invalid name", u32::from(type_oid)))?
            .to_owned();
        Ok(H3TypeShape {
            type_oid: (*form).oid,
            name,
            schema_oid: (*form).typnamespace,
            typlen: (*form).typlen,
            typbyval: (*form).typbyval,
            typtype: (*form).typtype as u8,
            typisdefined: (*form).typisdefined,
            typrelid: (*form).typrelid,
            typsubscript: (*form).typsubscript,
            typelem: (*form).typelem,
            typalign: (*form).typalign as u8,
            typstorage: (*form).typstorage as u8,
            typbasetype: (*form).typbasetype,
            typndims: (*form).typndims,
            typcollation: (*form).typcollation,
            tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
            tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
            tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
        })
    })();
    // SAFETY: result owns all copied row data, so the matching TYPEOID pin can
    // now be released exactly once.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn validate_h3_type(type_oid: pg_sys::Oid) -> Result<H3TypeProof, String> {
    // SAFETY: validation runs on the backend thread and retains only copied OIDs.
    let (extension_oid, schema_oid) = unsafe { extension_identity()? };
    // SAFETY: type_oid is inspected under a syscache pin held wholly inside the helper.
    let shape = unsafe { read_h3_type_shape(type_oid)? };
    validate_h3_type_shape(&shape, schema_oid)?;
    // SAFETY: type_oid is a catalog OID and dependency lookup occurs on the
    // caller-guaranteed backend thread.
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::TypeRelationId, type_oid) } != extension_oid {
        return Err(format!(
            "type OID {} is not owned by extension h3",
            u32::from(type_oid)
        ));
    }
    Ok(H3TypeProof {
        extension_oid,
        shape,
    })
}

unsafe fn find_h3_parent_function(
    schema_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<pg_sys::Oid, String> {
    let function_name = std::ffi::CString::new(H3_PARENT_FUNCTION_NAME)
        .map_err(|_| "invalid H3 parent function name".to_owned())?;
    let argument_types = [type_oid, pg_sys::INT4OID];
    // SAFETY: the fixed array contains exactly the initialized OIDs described by
    // its length; buildoidvector copies them into palloc-owned storage.
    let argument_vector =
        unsafe { pg_sys::buildoidvector(argument_types.as_ptr(), argument_types.len() as i32) };
    if argument_vector.is_null() {
        return Err("could not build H3 parent function signature".to_owned());
    }
    // SAFETY: PROCNAMEARGSNSP expects the NUL-terminated name, live oidvector,
    // and schema OID; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::Datum::from(function_name.as_ptr()),
            pg_sys::Datum::from(argument_vector),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    // SAFETY: buildoidvector returned this palloc-owned allocation and the
    // completed syscache lookup no longer borrows it.
    unsafe { pg_sys::pfree(argument_vector.cast()) };
    if tuple.is_null() {
        return Err(
            "extension h3 has no exact h3_cell_to_parent(h3index, int4) function".to_owned(),
        );
    }
    // SAFETY: tuple is a live pinned pg_proc row; its OID is copied before the
    // matching pin is released.
    let fn_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    // SAFETY: releases exactly the non-null PROCNAMEARGSNSP pin acquired above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(fn_oid)
}

unsafe fn default_h3_equality_operator(type_oid: pg_sys::Oid) -> Result<pg_sys::Oid, String> {
    // SAFETY: lookup_type_cache runs on the backend thread and returns a cache
    // entry whose lifetime is managed by PostgreSQL's current type cache.
    let entry = unsafe {
        pg_sys::lookup_type_cache(type_oid, pg_sys::TYPECACHE_EQ_OPR as ::core::ffi::c_int)
    };
    // SAFETY: short-circuiting proves entry is non-null before reading type_id.
    if entry.is_null() || unsafe { (*entry).type_id } != type_oid {
        return Err(format!(
            "type cache has no entry for H3 type OID {}",
            u32::from(type_oid)
        ));
    }
    // SAFETY: entry is non-null, belongs to type_oid, and remains live in the
    // backend type cache for this lookup.
    let equality_op_oid = unsafe { (*entry).eq_opr };
    if equality_op_oid == pg_sys::InvalidOid {
        return Err(format!(
            "H3 type OID {} has no default equality operator",
            u32::from(type_oid)
        ));
    }
    Ok(equality_op_oid)
}

unsafe fn read_h3_operator_shape(operator_oid: pg_sys::Oid) -> Result<H3OperatorShape, String> {
    // SAFETY: OPEROID expects one OID Datum and the caller guarantees backend
    // syscache access; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::OPEROID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(operator_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "operator OID {} does not exist",
            u32::from(operator_oid)
        ));
    }
    // SAFETY: tuple is a live pinned pg_operator row; GETSTRUCT, NameData,
    // tuple-header, and item-pointer fields are copied before release.
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_operator>();
        let name = std::ffi::CStr::from_ptr((*form).oprname.data.as_ptr())
            .to_str()
            .map_err(|_| {
                format!(
                    "operator OID {} has an invalid name",
                    u32::from(operator_oid)
                )
            })?
            .to_owned();
        Ok(H3OperatorShape {
            operator_oid: (*form).oid,
            name,
            schema_oid: (*form).oprnamespace,
            kind: (*form).oprkind as u8,
            can_merge: (*form).oprcanmerge,
            can_hash: (*form).oprcanhash,
            left_type: (*form).oprleft,
            right_type: (*form).oprright,
            result_type: (*form).oprresult,
            commutator_oid: (*form).oprcom,
            function_oid: (*form).oprcode,
            tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
            tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
            tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
        })
    })();
    // SAFETY: result owns all copied row data, so the matching OPEROID pin can be released.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn proc_text_attr(tuple: pg_sys::HeapTuple, attnum: i16) -> Result<Option<String>, String> {
    let mut isnull = false;
    // SAFETY: callers supply a live pinned PROCOID tuple; attnum selects a
    // pg_proc text attribute and isnull is writable stack storage.
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::PROCOID as ::core::ffi::c_int,
            tuple,
            attnum,
            &raw mut isnull,
        )
    };
    if isnull {
        return Ok(None);
    }
    // SAFETY: SysCacheGetAttr reported a non-null text Datum still protected by
    // the caller's tuple pin; FromDatum copies it into an owned String.
    unsafe { String::from_datum(datum, false) }
        .map(Some)
        .ok_or_else(|| "could not decode pg_proc implementation text".to_owned())
}

unsafe fn language_name(language_oid: pg_sys::Oid) -> Result<String, String> {
    // SAFETY: catalog lookup runs on the backend thread; missing_ok=true returns
    // null rather than raising for an absent language.
    let name_ptr = unsafe { pg_sys::get_language_name(language_oid, true) };
    if name_ptr.is_null() {
        return Err(format!(
            "function language OID {} does not exist",
            u32::from(language_oid)
        ));
    }
    // SAFETY: a non-null get_language_name result is a NUL-terminated,
    // palloc-owned string; the result is copied before pfree.
    let result = unsafe {
        std::ffi::CStr::from_ptr(name_ptr)
            .to_str()
            .map(str::to_owned)
            .map_err(|_| {
                format!(
                    "function language OID {} has an invalid name",
                    u32::from(language_oid)
                )
            })
    };
    // SAFETY: get_language_name returned this palloc-owned pointer and result no
    // longer borrows it after the copy above.
    unsafe { pg_sys::pfree(name_ptr.cast()) };
    result
}

unsafe fn read_h3_function_shape(fn_oid: pg_sys::Oid) -> Result<H3FunctionShape, String> {
    // SAFETY: PROCOID expects one OID Datum and the caller guarantees backend
    // syscache access; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::PROCOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(fn_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("function OID {} does not exist", u32::from(fn_oid)));
    }
    // SAFETY: tuple is a live pinned pg_proc row; GETSTRUCT, variable OID fields,
    // text attributes, tuple header, and item pointer are copied before release.
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        let name = std::ffi::CStr::from_ptr((*form).proname.data.as_ptr())
            .to_str()
            .map_err(|_| format!("function OID {} has an invalid name", u32::from(fn_oid)))?
            .to_owned();
        let argument_count = usize::try_from((*form).pronargs)
            .map_err(|_| format!("function OID {} has invalid pronargs", u32::from(fn_oid)))?;
        let argument_types = (*form).proargtypes.values.as_slice(argument_count).to_vec();
        let language_name = language_name((*form).prolang)?;
        let source = proc_text_attr(tuple, pg_sys::Anum_pg_proc_prosrc as i16)?
            .ok_or_else(|| format!("function OID {} has NULL prosrc", u32::from(fn_oid)))?;
        let binary = proc_text_attr(tuple, pg_sys::Anum_pg_proc_probin as i16)?;
        Ok(H3FunctionShape {
            fn_oid: (*form).oid,
            name,
            schema_oid: (*form).pronamespace,
            language_oid: (*form).prolang,
            language_name,
            kind: (*form).prokind as u8,
            argument_defaults: (*form).pronargdefaults,
            security_definer: (*form).prosecdef,
            support_function: (*form).prosupport,
            strict: (*form).proisstrict,
            returns_set: (*form).proretset,
            volatility: (*form).provolatile as u8,
            parallel: (*form).proparallel as u8,
            variadic_type: (*form).provariadic,
            return_type: (*form).prorettype,
            argument_types,
            source,
            binary,
            tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
            tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
            tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
        })
    })();
    // SAFETY: result owns every copied field, so the matching PROCOID pin can be released.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

/// Validate the exact extension-owned parent function and return its catalog
/// fingerprint fragment.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn validate_h3_parent_function(
    fn_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<Vec<i32>, String> {
    // SAFETY: catalog validation runs on the backend thread and each helper owns
    // the complete lifetime of its syscache pins.
    let type_proof = unsafe { validate_h3_type(type_oid)? };
    // SAFETY: fn_oid is inspected under a PROCOID pin held wholly by the helper.
    let shape = unsafe { read_h3_function_shape(fn_oid)? };
    validate_h3_function_shape(&shape, type_proof.shape.schema_oid, type_oid)?;
    // SAFETY: fn_oid is a catalog OID and dependency lookup occurs on the
    // caller-guaranteed backend thread.
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::ProcedureRelationId, fn_oid) }
        != type_proof.extension_oid
    {
        return Err(format!(
            "function OID {} is not owned by extension h3",
            u32::from(fn_oid)
        ));
    }
    Ok(function_fingerprint(&shape))
}

unsafe fn resolve_h3_equality_catalog(
    type_proof: &H3TypeProof,
) -> Result<(H3OperatorShape, H3FunctionShape), String> {
    let type_oid = type_proof.shape.type_oid;
    // SAFETY: type_oid was obtained from the already validated H3 type proof and
    // type-cache access occurs on the backend thread.
    let equality_op_oid = unsafe { default_h3_equality_operator(type_oid)? };
    // SAFETY: equality_op_oid is inspected under an OPEROID pin held wholly by the helper.
    let operator = unsafe { read_h3_operator_shape(equality_op_oid)? };
    validate_h3_equality_operator_shape(&operator, type_proof.shape.schema_oid, type_oid)?;
    // SAFETY: equality_op_oid is a catalog OID and dependency lookup occurs on
    // the backend thread.
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::OperatorRelationId, equality_op_oid) }
        != type_proof.extension_oid
    {
        return Err(format!(
            "equality operator OID {} is not owned by extension h3",
            u32::from(equality_op_oid)
        ));
    }

    // SAFETY: equality_op_oid was resolved from the type cache and validated as
    // a live operator catalog row before this backend lookup.
    let equality_fn_oid = unsafe { pg_sys::get_opcode(equality_op_oid) };
    if equality_fn_oid == pg_sys::InvalidOid || equality_fn_oid != operator.function_oid {
        return Err("H3 equality operator implementation changed during validation".to_owned());
    }
    // SAFETY: equality_fn_oid is inspected under a PROCOID pin held wholly by the helper.
    let function = unsafe { read_h3_function_shape(equality_fn_oid)? };
    validate_h3_equality_function_shape(&function, type_proof.shape.schema_oid, type_oid)?;
    // SAFETY: equality_fn_oid is a catalog OID and dependency lookup occurs on
    // the backend thread.
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::ProcedureRelationId, equality_fn_oid) }
        != type_proof.extension_oid
    {
        return Err(format!(
            "equality function OID {} is not owned by extension h3",
            u32::from(equality_fn_oid)
        ));
    }
    Ok((operator, function))
}

/// Resolve and prove the relocatable H3 extension catalog identity.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn resolve_h3_catalog() -> Result<H3CatalogIdentity, String> {
    // SAFETY: resolution runs on the backend thread and each helper owns its
    // complete syscache-pin lifetime.
    let (extension_oid, schema_oid) = unsafe { extension_identity()? };
    // SAFETY: schema_oid came from the live extension row and lookup is backend-local.
    let type_oid = unsafe { find_h3_type_oid(schema_oid)? };
    // SAFETY: type_oid is inspected and dependency-checked on the same backend thread.
    let type_proof = unsafe { validate_h3_type(type_oid)? };
    if type_proof.extension_oid != extension_oid || type_proof.shape.schema_oid != schema_oid {
        return Err("extension h3 catalog identity changed during validation".to_owned());
    }
    // SAFETY: schema_oid and type_oid are the mutually validated H3 catalog identity.
    let parent_fn_oid = unsafe { find_h3_parent_function(schema_oid, type_oid)? };
    let (equality_operator, equality_function) =
        // SAFETY: type_proof owns copied values from a validated H3 type row.
        unsafe { resolve_h3_equality_catalog(&type_proof)? };
    let mut fingerprint_words = vec![u32_word(H3_FINGERPRINT_VERSION), oid_word(extension_oid)];
    fingerprint_words.extend(type_fingerprint(&type_proof.shape));
    // SAFETY: both OIDs were resolved from the same validated H3 extension schema.
    fingerprint_words.extend(unsafe { validate_h3_parent_function(parent_fn_oid, type_oid)? });
    fingerprint_words.extend(operator_fingerprint(&equality_operator));
    fingerprint_words.extend(function_fingerprint(&equality_function));
    Ok(H3CatalogIdentity {
        extension_oid,
        schema_oid,
        type_oid,
        parent_fn_oid,
        equality_op_oid: equality_operator.operator_oid,
        equality_fn_oid: equality_function.fn_oid,
        fingerprint_words,
    })
}

#[cfg(test)]
mod h3_tests {
    use super::*;

    fn oid(value: u32) -> pg_sys::Oid {
        pg_sys::Oid::from(value)
    }

    fn valid_type() -> H3TypeShape {
        H3TypeShape {
            type_oid: oid(50_001),
            name: H3_TYPE_NAME.to_owned(),
            schema_oid: oid(50_000),
            typlen: 8,
            typbyval: true,
            typtype: pg_sys::TYPTYPE_BASE,
            typisdefined: true,
            typrelid: pg_sys::InvalidOid,
            typsubscript: pg_sys::InvalidOid,
            typelem: pg_sys::InvalidOid,
            typalign: pg_sys::TYPALIGN_DOUBLE,
            typstorage: pg_sys::TYPSTORAGE_PLAIN,
            typbasetype: pg_sys::InvalidOid,
            typndims: 0,
            typcollation: pg_sys::InvalidOid,
            tuple_xmin: 11,
            tuple_block: 7,
            tuple_offset: 2,
        }
    }

    fn valid_function() -> H3FunctionShape {
        H3FunctionShape {
            fn_oid: oid(50_002),
            name: H3_PARENT_FUNCTION_NAME.to_owned(),
            schema_oid: oid(50_000),
            language_oid: oid(13),
            language_name: H3_LANGUAGE_NAME.to_owned(),
            kind: pg_sys::PROKIND_FUNCTION,
            argument_defaults: 0,
            security_definer: false,
            support_function: pg_sys::InvalidOid,
            strict: true,
            returns_set: false,
            volatility: pg_sys::PROVOLATILE_IMMUTABLE,
            parallel: pg_sys::PROPARALLEL_SAFE,
            variadic_type: pg_sys::InvalidOid,
            return_type: oid(50_001),
            argument_types: vec![oid(50_001), pg_sys::INT4OID],
            source: H3_PARENT_FUNCTION_NAME.to_owned(),
            binary: Some(H3_LIBRARY_NAME.to_owned()),
            tuple_xmin: 12,
            tuple_block: 8,
            tuple_offset: 3,
        }
    }

    fn valid_equality_function() -> H3FunctionShape {
        let mut shape = valid_function();
        shape.fn_oid = oid(50_004);
        shape.name = H3_EQUALITY_FUNCTION_NAME.to_owned();
        shape.argument_types = vec![oid(50_001), oid(50_001)];
        shape.return_type = pg_sys::BOOLOID;
        shape.source = H3_EQUALITY_FUNCTION_NAME.to_owned();
        shape.tuple_xmin = 13;
        shape.tuple_offset = 4;
        shape
    }

    fn valid_equality_operator() -> H3OperatorShape {
        H3OperatorShape {
            operator_oid: oid(50_003),
            name: H3_EQUALITY_OPERATOR_NAME.to_owned(),
            schema_oid: oid(50_000),
            kind: PG_OPERATOR_KIND_BINARY,
            can_merge: true,
            can_hash: true,
            left_type: oid(50_001),
            right_type: oid(50_001),
            result_type: pg_sys::BOOLOID,
            commutator_oid: oid(50_003),
            function_oid: oid(50_004),
            tuple_xmin: 14,
            tuple_block: 9,
            tuple_offset: 5,
        }
    }

    #[test]
    fn exact_h3_type_shape_is_required() {
        let valid = valid_type();
        assert_eq!(validate_h3_type_shape(&valid, valid.schema_oid), Ok(()));

        let mut invalid = valid.clone();
        invalid.typbyval = false;
        assert!(validate_h3_type_shape(&invalid, invalid.schema_oid).is_err());
        let mut invalid = valid.clone();
        invalid.typelem = oid(23);
        assert!(validate_h3_type_shape(&invalid, invalid.schema_oid).is_err());
        let mut invalid = valid;
        invalid.typcollation = oid(100);
        assert!(validate_h3_type_shape(&invalid, invalid.schema_oid).is_err());
    }

    #[test]
    fn exact_h3_parent_shape_is_required() {
        let valid = valid_function();
        assert_eq!(
            validate_h3_function_shape(&valid, valid.schema_oid, oid(50_001)),
            Ok(())
        );

        let mut invalid = valid.clone();
        invalid.argument_types[1] = pg_sys::INT8OID;
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.strict = false;
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.parallel = pg_sys::PROPARALLEL_RESTRICTED;
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.language_name = "sql".to_owned();
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.source = "spoofed_parent".to_owned();
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.binary = Some("$libdir/h3".to_owned());
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.argument_defaults = 1;
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid.clone();
        invalid.security_definer = true;
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
        let mut invalid = valid;
        invalid.support_function = oid(50_003);
        assert!(validate_h3_function_shape(&invalid, invalid.schema_oid, oid(50_001)).is_err());
    }

    #[test]
    fn exact_h3_equality_shapes_are_required() {
        let function = valid_equality_function();
        assert_eq!(
            validate_h3_equality_function_shape(&function, function.schema_oid, oid(50_001)),
            Ok(())
        );
        let mut invalid_function = function;
        invalid_function.source = H3_PARENT_FUNCTION_NAME.to_owned();
        assert!(
            validate_h3_equality_function_shape(
                &invalid_function,
                invalid_function.schema_oid,
                oid(50_001)
            )
            .is_err()
        );

        let operator = valid_equality_operator();
        assert_eq!(
            validate_h3_equality_operator_shape(&operator, operator.schema_oid, oid(50_001)),
            Ok(())
        );
        let mut invalid_operator = operator.clone();
        invalid_operator.can_hash = false;
        assert!(
            validate_h3_equality_operator_shape(
                &invalid_operator,
                invalid_operator.schema_oid,
                oid(50_001)
            )
            .is_err()
        );
        let mut invalid_operator = operator.clone();
        invalid_operator.right_type = pg_sys::INT8OID;
        assert!(
            validate_h3_equality_operator_shape(
                &invalid_operator,
                invalid_operator.schema_oid,
                oid(50_001)
            )
            .is_err()
        );
        let mut invalid_operator = operator.clone();
        invalid_operator.commutator_oid = pg_sys::InvalidOid;
        assert!(
            validate_h3_equality_operator_shape(
                &invalid_operator,
                invalid_operator.schema_oid,
                oid(50_001)
            )
            .is_err()
        );

        let mut replacement = operator.clone();
        replacement.function_oid = oid(50_005);
        assert_ne!(
            operator_fingerprint(&operator),
            operator_fingerprint(&replacement)
        );
    }

    #[test]
    fn h3_function_fingerprint_changes_on_catalog_replacement() {
        let original = valid_function();
        let mut replacement = original.clone();
        replacement.tuple_xmin += 1;
        assert_ne!(
            function_fingerprint(&original),
            function_fingerprint(&replacement)
        );

        let mut same_transaction_replacement = original.clone();
        same_transaction_replacement.tuple_offset += 1;
        assert_ne!(
            function_fingerprint(&original),
            function_fingerprint(&same_transaction_replacement)
        );

        let mut implementation_change = original.clone();
        implementation_change.source.push_str("_v2");
        assert_ne!(
            function_fingerprint(&original),
            function_fingerprint(&implementation_change)
        );

        let mut flag_change = original.clone();
        flag_change.security_definer = true;
        assert_ne!(
            function_fingerprint(&original),
            function_fingerprint(&flag_change)
        );
    }

    #[test]
    fn catalog_free_invalid_inputs_fail_closed_before_syscache_access() {
        assert!(postgres_error_requires_rethrow(
            PgLogLevel::FATAL,
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE
        ));
        assert!(postgres_error_requires_rethrow(
            PgLogLevel::PANIC,
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE
        ));
        for code in [
            PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            PgSqlErrorCode::ERRCODE_EXTERNAL_ROUTINE_EXCEPTION,
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            PgSqlErrorCode::ERRCODE_UNDEFINED_FUNCTION,
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
        ] {
            assert!(!postgres_error_requires_rethrow(PgLogLevel::ERROR, code));
        }
        assert!(postgres_error_requires_rethrow(
            PgLogLevel::ERROR,
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR
        ));

        // SAFETY: every call exits on an invalid OID or embedded NUL before it
        // can enter a PostgreSQL catalog helper.
        unsafe {
            assert_eq!(function_schema_and_name(pg_sys::InvalidOid), None);
            assert_eq!(type_name(pg_sys::InvalidOid), None);
            assert!(!function_is_extension_member(pg_sys::InvalidOid, "h3"));
            assert!(!type_is_extension_member(pg_sys::InvalidOid, "h3"));
            assert!(!object_is_extension_member(
                pg_sys::TypeRelationId,
                oid(1),
                "not\0an-extension"
            ));
            assert_eq!(type_oid_in_schema("bad\0schema", "h3index"), None);
            assert_eq!(type_oid_in_schema("public", "bad\0type"), None);
        }
    }

    #[test]
    fn every_h3_type_invariant_has_a_specific_rejection() {
        let valid = valid_type();
        let schema_oid = valid.schema_oid;
        let cases: Vec<(H3TypeShape, &str)> = vec![
            (
                {
                    let mut value = valid.clone();
                    value.name = "h3index_spoof".to_owned();
                    value
                },
                "is named",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.schema_oid = oid(99);
                    value
                },
                "outside extension schema",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typtype = pg_sys::TYPTYPE_DOMAIN;
                    value
                },
                "must be a base type",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typisdefined = false;
                    value
                },
                "only a shell type",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typlen = 4;
                    value
                },
                "8-byte pass-by-value",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typbyval = false;
                    value
                },
                "8-byte pass-by-value",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typalign = pg_sys::TYPALIGN_INT;
                    value
                },
                "double alignment",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typstorage = pg_sys::TYPSTORAGE_EXTENDED;
                    value
                },
                "plain storage",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typrelid = oid(1);
                    value
                },
                "composite, domain, or array-like",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typsubscript = oid(1);
                    value
                },
                "composite, domain, or array-like",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typelem = oid(1);
                    value
                },
                "composite, domain, or array-like",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typbasetype = oid(1);
                    value
                },
                "composite, domain, or array-like",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.typndims = 1;
                    value
                },
                "composite, domain, or array-like",
            ),
            (
                {
                    let mut value = valid;
                    value.typcollation = oid(1);
                    value
                },
                "must not be collatable",
            ),
        ];
        for (shape, expected) in cases {
            let error = validate_h3_type_shape(&shape, schema_oid)
                .expect_err("each mutated type invariant must fail");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn every_h3_function_invariant_has_a_specific_rejection() {
        let valid = valid_function();
        let schema_oid = valid.schema_oid;
        let type_oid = oid(50_001);
        let cases: Vec<(H3FunctionShape, &str)> = vec![
            (
                {
                    let mut value = valid.clone();
                    value.name = "h3_cell_to_parent_spoof".to_owned();
                    value
                },
                "not extension-schema",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.schema_oid = oid(99);
                    value
                },
                "not extension-schema",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.kind = pg_sys::PROKIND_PROCEDURE;
                    value
                },
                "ordinary function",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.language_name = "sql".to_owned();
                    value
                },
                "C language",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.source.push_str("_spoof");
                    value
                },
                "canonical C symbol",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.binary = None;
                    value
                },
                "canonical C symbol",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.argument_types.pop();
                    value
                },
                "noncanonical signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.return_type = pg_sys::INT8OID;
                    value
                },
                "noncanonical signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.argument_defaults = 1;
                    value
                },
                "argument defaults",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.security_definer = true;
                    value
                },
                "security definer",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.support_function = oid(1);
                    value
                },
                "planner support function",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.variadic_type = type_oid;
                    value
                },
                "scalar and nonvariadic",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.returns_set = true;
                    value
                },
                "scalar and nonvariadic",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.strict = false;
                    value
                },
                "must be strict",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.volatility = pg_sys::PROVOLATILE_STABLE;
                    value
                },
                "must be immutable",
            ),
            (
                {
                    let mut value = valid;
                    value.parallel = pg_sys::PROPARALLEL_RESTRICTED;
                    value
                },
                "parallel safe",
            ),
        ];
        for (shape, expected) in cases {
            let error = validate_h3_function_shape(&shape, schema_oid, type_oid)
                .expect_err("each mutated function invariant must fail");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn every_h3_equality_operator_invariant_has_a_specific_rejection() {
        let valid = valid_equality_operator();
        let schema_oid = valid.schema_oid;
        let type_oid = oid(50_001);
        let cases: Vec<(H3OperatorShape, &str)> = vec![
            (
                {
                    let mut value = valid.clone();
                    value.name = "==".to_owned();
                    value
                },
                "not the extension-schema",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.schema_oid = oid(99);
                    value
                },
                "not the extension-schema",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.kind = b'l';
                    value
                },
                "must have signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.left_type = pg_sys::INT8OID;
                    value
                },
                "must have signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.right_type = pg_sys::INT8OID;
                    value
                },
                "must have signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.result_type = pg_sys::INT8OID;
                    value
                },
                "must have signature",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.can_hash = false;
                    value
                },
                "hashable and mergeable",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.can_merge = false;
                    value
                },
                "hashable and mergeable",
            ),
            (
                {
                    let mut value = valid.clone();
                    value.commutator_oid = oid(99);
                    value
                },
                "own commutator",
            ),
            (
                {
                    let mut value = valid;
                    value.function_oid = pg_sys::InvalidOid;
                    value
                },
                "no implementation function",
            ),
        ];
        for (shape, expected) in cases {
            let error = validate_h3_equality_operator_shape(&shape, schema_oid, type_oid)
                .expect_err("each mutated operator invariant must fail");
            assert!(error.contains(expected), "unexpected error: {error}");
        }
    }

    #[test]
    fn primitive_and_catalog_fingerprints_preserve_every_identity_word() {
        let high_bit_oid = oid(0xfeed_beef);
        assert_eq!(
            oid_word(high_bit_oid).to_ne_bytes(),
            0xfeed_beef_u32.to_ne_bytes()
        );
        assert_eq!(
            u32_word(0x8765_4321).to_ne_bytes(),
            0x8765_4321_u32.to_ne_bytes()
        );
        let words = u64_words(0x1122_3344_aabb_ccdd);
        assert_eq!(words[0].to_ne_bytes(), 0xaabb_ccdd_u32.to_ne_bytes());
        assert_eq!(words[1].to_ne_bytes(), 0x1122_3344_u32.to_ne_bytes());
        assert_ne!(fnv1a64(&[b"ab", b"c"]), fnv1a64(&[b"a", b"bc"]));
        assert_eq!(fnv1a64(&[]), 0xcbf2_9ce4_8422_2325);

        let type_shape = valid_type();
        let type_words = type_fingerprint(&type_shape);
        assert_eq!(type_words.len(), 8);
        assert_eq!(type_words[0], oid_word(type_shape.type_oid));
        assert_eq!(type_words[7], i32::from(type_shape.typstorage));

        let mut function = valid_function();
        function.binary = None;
        let function_words = function_fingerprint(&function);
        assert_eq!(function_words.len(), 19 + function.argument_types.len());
        assert_eq!(function_words[0], oid_word(function.fn_oid));
        let implementation_hash = fnv1a64(&[
            function.name.as_bytes(),
            function.language_name.as_bytes(),
            function.source.as_bytes(),
            b"",
        ]);
        let [hash_low, hash_high] = u64_words(implementation_hash);
        assert_eq!(&function_words[17..19], &[hash_low, hash_high]);

        let operator = valid_equality_operator();
        let operator_words = operator_fingerprint(&operator);
        assert_eq!(operator_words.len(), 15);
        assert_eq!(operator_words[0], oid_word(operator.operator_oid));
        assert_eq!(operator_words[12], oid_word(operator.function_oid));
    }
}

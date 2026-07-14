//! Typed SysCache readers for pg_accel.
//!
//! Wraps syscache/catalog helpers used by planner hooks.

use pgrx::{FromDatum, pg_sys};

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

    let name_ptr = unsafe { pg_sys::get_func_name(fn_oid) };
    if name_ptr.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) }
        .to_str()
        .ok()?
        .to_ascii_lowercase();

    let namespace_oid = unsafe { pg_sys::get_func_namespace(fn_oid) };
    if namespace_oid == pg_sys::InvalidOid {
        return None;
    }
    let namespace_ptr = unsafe { pg_sys::get_namespace_name(namespace_oid) };
    if namespace_ptr.is_null() {
        return None;
    }
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
    let extension_oid = unsafe { pg_sys::get_extension_oid(extname_cstr.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return false;
    }

    (unsafe { pg_sys::getExtensionOfObject(class_oid, object_oid) }) == extension_oid
}

/// Return true when a function belongs to the named extension.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL extension/dependency helpers.
pub unsafe fn function_is_extension_member(fn_oid: pg_sys::Oid, extname: &str) -> bool {
    unsafe { object_is_extension_member(pg_sys::ProcedureRelationId, fn_oid, extname) }
}

/// Return true when a type belongs to the named extension.
///
/// # Safety
/// Must be called on the main backend thread. Performs backend catalog
/// lookups via PostgreSQL extension/dependency helpers.
pub unsafe fn type_is_extension_member(type_oid: pg_sys::Oid, extname: &str) -> bool {
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
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            tuple,
            pg_sys::Anum_pg_type_typname as i16,
            &raw mut isnull,
        )
    };
    if isnull {
        unsafe { pg_sys::ReleaseSysCache(tuple) };
        return None;
    }

    let name_ptr = datum.cast_mut_ptr::<pg_sys::NameData>();
    if name_ptr.is_null() {
        unsafe { pg_sys::ReleaseSysCache(tuple) };
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*name_ptr).data.as_ptr()) }
        .to_str()
        .ok()
        .map(str::to_ascii_lowercase);
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

    let namespace_oid = unsafe { pg_sys::get_namespace_oid(schema_cstr.as_ptr(), true) };
    if namespace_oid == pg_sys::InvalidOid {
        return None;
    }

    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(type_name_cstr.as_ptr().cast()),
            pg_sys::ObjectIdGetDatum(namespace_oid),
        )
    };
    if tuple.is_null() {
        return None;
    }

    let mut isnull: bool = false;
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            tuple,
            pg_sys::Anum_pg_type_oid as i16,
            &raw mut isnull,
        )
    };
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

const POSTGIS_EXTENSION_NAME: &str = "postgis";
const POSTGIS_GEOMETRY_TYPE_NAME: &str = "geometry";
const POSTGIS_LANGUAGE_NAME: &str = "c";
const POSTGIS_LIBRARY_NAME: &str = "$libdir/postgis-3";
const POSTGIS_SUPPORT_FUNCTION_NAME: &str = "postgis_index_supportfn";
const POSTGIS_FINGERPRINT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostgisSpatialFunction {
    Intersects,
    Contains,
    Within,
    DWithin,
    Distance,
}

/// Exact extension-owned PostGIS catalog accepted by resident spatial paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgisCatalogIdentity {
    pub extension_oid: pg_sys::Oid,
    pub schema_oid: pg_sys::Oid,
    pub geometry_type_oid: pg_sys::Oid,
    pub intersects_fn_oid: pg_sys::Oid,
    pub contains_fn_oid: pg_sys::Oid,
    pub within_fn_oid: pg_sys::Oid,
    pub dwithin_fn_oid: pg_sys::Oid,
    pub distance_fn_oid: pg_sys::Oid,
    pub is_valid_fn_oid: pg_sys::Oid,
    pub fingerprint_words: Vec<i32>,
}

impl PostgisCatalogIdentity {
    #[must_use]
    pub fn classify_function(&self, oid: pg_sys::Oid) -> Option<PostgisSpatialFunction> {
        if oid == self.intersects_fn_oid {
            Some(PostgisSpatialFunction::Intersects)
        } else if oid == self.contains_fn_oid {
            Some(PostgisSpatialFunction::Contains)
        } else if oid == self.within_fn_oid {
            Some(PostgisSpatialFunction::Within)
        } else if oid == self.dwithin_fn_oid {
            Some(PostgisSpatialFunction::DWithin)
        } else if oid == self.distance_fn_oid {
            Some(PostgisSpatialFunction::Distance)
        } else {
            None
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostgisTypeShape {
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
    typinput: pg_sys::Oid,
    typoutput: pg_sys::Oid,
    typreceive: pg_sys::Oid,
    typsend: pg_sys::Oid,
    typmodin: pg_sys::Oid,
    typmodout: pg_sys::Oid,
    typanalyze: pg_sys::Oid,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
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
    let extension_oid = unsafe { pg_sys::get_extension_oid(extension_name.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return Err(format!("extension {expected_name} is not installed"));
    }

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
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn extension_identity() -> Result<(pg_sys::Oid, pg_sys::Oid), String> {
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
    let argument_vector =
        unsafe { pg_sys::buildoidvector(argument_types.as_ptr(), argument_count) };
    if argument_vector.is_null() {
        return Err(format!(
            "could not build function signature for {function_name}"
        ));
    }
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(function_name_c.as_ptr().cast()),
            pg_sys::PointerGetDatum(argument_vector.cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    unsafe { pg_sys::pfree(argument_vector.cast()) };
    if tuple.is_null() {
        return Err(format!(
            "schema OID {} has no exact {function_name} function",
            u32::from(schema_oid)
        ));
    }
    let fn_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(fn_oid)
}

unsafe fn find_h3_type_oid(schema_oid: pg_sys::Oid) -> Result<pg_sys::Oid, String> {
    let type_name =
        std::ffi::CString::new(H3_TYPE_NAME).map_err(|_| "invalid H3 type name".to_owned())?;
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(type_name.as_ptr().cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "extension h3 has no h3index type in schema OID {}",
            u32::from(schema_oid)
        ));
    }
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_h3_type_shape(type_oid: pg_sys::Oid) -> Result<H3TypeShape, String> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("type OID {} does not exist", u32::from(type_oid)));
    }
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
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn validate_h3_type(type_oid: pg_sys::Oid) -> Result<H3TypeProof, String> {
    let (extension_oid, schema_oid) = unsafe { extension_identity()? };
    let shape = unsafe { read_h3_type_shape(type_oid)? };
    validate_h3_type_shape(&shape, schema_oid)?;
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
    let argument_vector =
        unsafe { pg_sys::buildoidvector(argument_types.as_ptr(), argument_types.len() as i32) };
    if argument_vector.is_null() {
        return Err("could not build H3 parent function signature".to_owned());
    }
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(function_name.as_ptr().cast()),
            pg_sys::PointerGetDatum(argument_vector.cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    unsafe { pg_sys::pfree(argument_vector.cast()) };
    if tuple.is_null() {
        return Err(
            "extension h3 has no exact h3_cell_to_parent(h3index, int4) function".to_owned(),
        );
    }
    let fn_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(fn_oid)
}

unsafe fn default_h3_equality_operator(type_oid: pg_sys::Oid) -> Result<pg_sys::Oid, String> {
    let entry = unsafe {
        pg_sys::lookup_type_cache(type_oid, pg_sys::TYPECACHE_EQ_OPR as ::core::ffi::c_int)
    };
    if entry.is_null() || unsafe { (*entry).type_id } != type_oid {
        return Err(format!(
            "type cache has no entry for H3 type OID {}",
            u32::from(type_oid)
        ));
    }
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
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn proc_text_attr(tuple: pg_sys::HeapTuple, attnum: i16) -> Result<Option<String>, String> {
    let mut isnull = false;
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
    unsafe { String::from_datum(datum, false) }
        .map(Some)
        .ok_or_else(|| "could not decode pg_proc implementation text".to_owned())
}

unsafe fn language_name(language_oid: pg_sys::Oid) -> Result<String, String> {
    let name_ptr = unsafe { pg_sys::get_language_name(language_oid, true) };
    if name_ptr.is_null() {
        return Err(format!(
            "function language OID {} does not exist",
            u32::from(language_oid)
        ));
    }
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
    unsafe { pg_sys::pfree(name_ptr.cast()) };
    result
}

unsafe fn read_h3_function_shape(fn_oid: pg_sys::Oid) -> Result<H3FunctionShape, String> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::PROCOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(fn_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("function OID {} does not exist", u32::from(fn_oid)));
    }
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
    let type_proof = unsafe { validate_h3_type(type_oid)? };
    let shape = unsafe { read_h3_function_shape(fn_oid)? };
    validate_h3_function_shape(&shape, type_proof.shape.schema_oid, type_oid)?;
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
    let equality_op_oid = unsafe { default_h3_equality_operator(type_oid)? };
    let operator = unsafe { read_h3_operator_shape(equality_op_oid)? };
    validate_h3_equality_operator_shape(&operator, type_proof.shape.schema_oid, type_oid)?;
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::OperatorRelationId, equality_op_oid) }
        != type_proof.extension_oid
    {
        return Err(format!(
            "equality operator OID {} is not owned by extension h3",
            u32::from(equality_op_oid)
        ));
    }

    let equality_fn_oid = unsafe { pg_sys::get_opcode(equality_op_oid) };
    if equality_fn_oid == pg_sys::InvalidOid || equality_fn_oid != operator.function_oid {
        return Err("H3 equality operator implementation changed during validation".to_owned());
    }
    let function = unsafe { read_h3_function_shape(equality_fn_oid)? };
    validate_h3_equality_function_shape(&function, type_proof.shape.schema_oid, type_oid)?;
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
    let (extension_oid, schema_oid) = unsafe { extension_identity()? };
    let type_oid = unsafe { find_h3_type_oid(schema_oid)? };
    let type_proof = unsafe { validate_h3_type(type_oid)? };
    if type_proof.extension_oid != extension_oid || type_proof.shape.schema_oid != schema_oid {
        return Err("extension h3 catalog identity changed during validation".to_owned());
    }
    let parent_fn_oid = unsafe { find_h3_parent_function(schema_oid, type_oid)? };
    let (equality_operator, equality_function) =
        unsafe { resolve_h3_equality_catalog(&type_proof)? };
    let mut fingerprint_words = vec![u32_word(H3_FINGERPRINT_VERSION), oid_word(extension_oid)];
    fingerprint_words.extend(type_fingerprint(&type_proof.shape));
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

const POSTGIS_RASTER_EXTENSION_NAME: &str = "postgis_raster";
const POSTGIS_RASTER_TYPE_NAME: &str = "raster";
const POSTGIS_RASTER_SUMMARY_TYPE_NAME: &str = "summarystats";
const POSTGIS_RASTER_RECLASS_ARG_TYPE_NAME: &str = "reclassarg";
const POSTGIS_RASTER_LIBRARY: &str = "$libdir/postgis_raster-3";
const POSTGIS_RASTER_FINGERPRINT_VERSION: u32 = 1;

/// Exact public PostGIS Raster overload accepted by the resident planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostgisRasterFunction {
    Reclass,
    SummaryStats,
    SummaryStatsDefaultBand,
}

/// Replacement-sensitive PostGIS Raster catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgisRasterCatalogIdentity {
    pub extension_oid: pg_sys::Oid,
    pub schema_oid: pg_sys::Oid,
    pub raster_type_oid: pg_sys::Oid,
    pub summary_stats_type_oid: pg_sys::Oid,
    pub reclass_fn_oid: pg_sys::Oid,
    pub summary_stats_fn_oid: pg_sys::Oid,
    pub summary_stats_default_band_fn_oid: pg_sys::Oid,
    pub reclass_impl_fn_oid: pg_sys::Oid,
    pub summary_stats_impl_fn_oid: pg_sys::Oid,
    pub fingerprint_words: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostgisRasterExtensionShape {
    extension_oid: pg_sys::Oid,
    schema_oid: pg_sys::Oid,
    version: String,
    relocatable: bool,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostgisRasterTypeShape {
    base: H3TypeShape,
    input_fn: pg_sys::Oid,
    output_fn: pg_sys::Oid,
    receive_fn: pg_sys::Oid,
    send_fn: pg_sys::Oid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompositeFieldShape {
    name: String,
    type_oid: pg_sys::Oid,
    typmod: i32,
    collation_oid: pg_sys::Oid,
    not_null: bool,
    tuple_xmin: u32,
    tuple_block: u32,
    tuple_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompositeTypeShape {
    base: H3TypeShape,
    fields: Vec<CompositeFieldShape>,
}

unsafe fn extension_text_attr(tuple: pg_sys::HeapTuple, attnum: i16) -> Result<String, String> {
    let mut isnull = false;
    let datum = unsafe {
        pg_sys::SysCacheGetAttr(
            pg_sys::SysCacheIdentifier::EXTENSIONOID as ::core::ffi::c_int,
            tuple,
            attnum,
            &raw mut isnull,
        )
    };
    if isnull {
        return Err("PostGIS Raster extension version is NULL".to_owned());
    }
    unsafe { String::from_datum(datum, false) }
        .ok_or_else(|| "could not decode PostGIS Raster extension version".to_owned())
}

unsafe fn postgis_raster_extension_shape() -> Result<PostgisRasterExtensionShape, String> {
    let name = std::ffi::CString::new(POSTGIS_RASTER_EXTENSION_NAME)
        .map_err(|_| "invalid PostGIS Raster extension name".to_owned())?;
    let extension_oid = unsafe { pg_sys::get_extension_oid(name.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return Err("extension postgis_raster is not installed".to_owned());
    }
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::EXTENSIONOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(extension_oid),
        )
    };
    if tuple.is_null() {
        return Err("extension postgis_raster disappeared during validation".to_owned());
    }
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_extension>();
        let actual_name = std::ffi::CStr::from_ptr((*form).extname.data.as_ptr())
            .to_str()
            .map_err(|_| "extension postgis_raster has an invalid name".to_owned())?;
        if actual_name != POSTGIS_RASTER_EXTENSION_NAME {
            return Err("PostGIS Raster extension OID resolved to another row".to_owned());
        }
        if (*form).extrelocatable {
            return Err(
                "PostGIS Raster extension unexpectedly declares itself relocatable".to_owned(),
            );
        }
        if (*form).extnamespace == pg_sys::InvalidOid {
            return Err("PostGIS Raster extension has no installation schema".to_owned());
        }
        Ok(PostgisRasterExtensionShape {
            extension_oid,
            schema_oid: (*form).extnamespace,
            version: extension_text_attr(tuple, pg_sys::Anum_pg_extension_extversion as i16)?,
            relocatable: (*form).extrelocatable,
            tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
            tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
            tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
        })
    })();
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn find_type_oid(schema_oid: pg_sys::Oid, name: &str) -> Result<pg_sys::Oid, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "invalid catalog type name".to_owned())?;
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(name.as_ptr().cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "PostGIS Raster schema OID {} has no required type",
            u32::from(schema_oid)
        ));
    }
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_postgis_raster_type_shape(
    type_oid: pg_sys::Oid,
) -> Result<PostgisRasterTypeShape, String> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("type OID {} does not exist", u32::from(type_oid)));
    }
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        let name = std::ffi::CStr::from_ptr((*form).typname.data.as_ptr())
            .to_str()
            .map_err(|_| format!("type OID {} has an invalid name", u32::from(type_oid)))?
            .to_owned();
        Ok(PostgisRasterTypeShape {
            base: H3TypeShape {
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
            },
            input_fn: (*form).typinput,
            output_fn: (*form).typoutput,
            receive_fn: (*form).typreceive,
            send_fn: (*form).typsend,
        })
    })();
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

fn validate_postgis_raster_type_shape(
    shape: &PostgisRasterTypeShape,
    schema_oid: pg_sys::Oid,
) -> Result<(), String> {
    let base = &shape.base;
    if base.name != POSTGIS_RASTER_TYPE_NAME || base.schema_oid != schema_oid {
        return Err("type is not the PostGIS Raster schema raster type".to_owned());
    }
    if base.typtype != pg_sys::TYPTYPE_BASE
        || !base.typisdefined
        || base.typlen != -1
        || base.typbyval
        || base.typalign != pg_sys::TYPALIGN_DOUBLE
        || base.typstorage != pg_sys::TYPSTORAGE_EXTENDED
        || base.typrelid != pg_sys::InvalidOid
        || base.typsubscript != pg_sys::InvalidOid
        || base.typelem != pg_sys::InvalidOid
        || base.typbasetype != pg_sys::InvalidOid
        || base.typndims != 0
        || base.typcollation != pg_sys::InvalidOid
    {
        return Err("PostGIS raster type has a noncanonical varlena layout".to_owned());
    }
    if shape.input_fn == pg_sys::InvalidOid || shape.output_fn == pg_sys::InvalidOid {
        return Err("PostGIS raster type is missing input/output functions".to_owned());
    }
    if shape.receive_fn != pg_sys::InvalidOid || shape.send_fn != pg_sys::InvalidOid {
        return Err(
            "PostGIS raster type unexpectedly declares binary receive/send functions".to_owned(),
        );
    }
    Ok(())
}

unsafe fn read_composite_type_shape(type_oid: pg_sys::Oid) -> Result<CompositeTypeShape, String> {
    let base = unsafe { read_h3_type_shape(type_oid)? };
    if base.typtype != pg_sys::TYPTYPE_COMPOSITE || base.typrelid == pg_sys::InvalidOid {
        return Err(format!("type OID {} is not composite", u32::from(type_oid)));
    }
    let relation_tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::RELOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(base.typrelid),
        )
    };
    if relation_tuple.is_null() {
        return Err(format!(
            "composite type OID {} has no backing relation",
            u32::from(type_oid)
        ));
    }
    let field_count = unsafe {
        let form = pg_sys::GETSTRUCT(relation_tuple).cast::<pg_sys::FormData_pg_class>();
        usize::try_from((*form).relnatts).map_err(|_| {
            format!(
                "composite type OID {} has invalid relnatts",
                u32::from(type_oid)
            )
        })
    };
    unsafe { pg_sys::ReleaseSysCache(relation_tuple) };
    let field_count = field_count?;
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(field_count)
        .map_err(|_| "composite catalog field allocation failed".to_owned())?;
    for index in 0..field_count {
        let attno = i16::try_from(index + 1)
            .map_err(|_| "composite attribute number exceeds int16".to_owned())?;
        let tuple = unsafe {
            pg_sys::SearchSysCache2(
                pg_sys::SysCacheIdentifier::ATTNUM as ::core::ffi::c_int,
                base.typrelid.into(),
                pg_sys::Datum::from(attno),
            )
        };
        if tuple.is_null() {
            return Err(format!(
                "composite type OID {} is missing attribute {attno}",
                u32::from(type_oid)
            ));
        }
        let field = (|| unsafe {
            let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_attribute>();
            if (*form).attisdropped || (*form).attnum != attno {
                return Err(format!(
                    "composite type OID {} contains a dropped or reordered field",
                    u32::from(type_oid)
                ));
            }
            let name = std::ffi::CStr::from_ptr((*form).attname.data.as_ptr())
                .to_str()
                .map_err(|_| "composite field name is invalid".to_owned())?
                .to_owned();
            Ok(CompositeFieldShape {
                name,
                type_oid: (*form).atttypid,
                typmod: (*form).atttypmod,
                collation_oid: (*form).attcollation,
                not_null: (*form).attnotnull,
                tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
                tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
                tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
            })
        })();
        unsafe { pg_sys::ReleaseSysCache(tuple) };
        fields.push(field?);
    }
    Ok(CompositeTypeShape { base, fields })
}

fn validate_composite_type_shape(
    shape: &CompositeTypeShape,
    schema_oid: pg_sys::Oid,
    expected_name: &str,
    expected_fields: &[(&str, pg_sys::Oid)],
) -> Result<(), String> {
    let base = &shape.base;
    if base.name != expected_name
        || base.schema_oid != schema_oid
        || base.typtype != pg_sys::TYPTYPE_COMPOSITE
        || !base.typisdefined
        || base.typlen != -1
        || base.typbyval
        || base.typalign != pg_sys::TYPALIGN_DOUBLE
        || base.typstorage != pg_sys::TYPSTORAGE_EXTENDED
        || base.typrelid == pg_sys::InvalidOid
        || base.typsubscript != pg_sys::InvalidOid
        || base.typelem != pg_sys::InvalidOid
        || base.typbasetype != pg_sys::InvalidOid
        || base.typndims != 0
        || base.typcollation != pg_sys::InvalidOid
        || shape.fields.len() != expected_fields.len()
    {
        return Err(format!(
            "PostGIS Raster {expected_name} composite layout is noncanonical"
        ));
    }
    for (actual, (expected_field_name, expected_type_oid)) in
        shape.fields.iter().zip(expected_fields)
    {
        if actual.name != *expected_field_name
            || actual.type_oid != *expected_type_oid
            || actual.typmod != -1
            || actual.not_null
        {
            return Err(format!(
                "PostGIS Raster {expected_name}.{} has a noncanonical field layout",
                actual.name
            ));
        }
        let expected_collation = if *expected_type_oid == pg_sys::TEXTOID {
            pg_sys::DEFAULT_COLLATION_OID
        } else {
            pg_sys::InvalidOid
        };
        if actual.collation_oid != expected_collation {
            return Err(format!(
                "PostGIS Raster {expected_name}.{} has a noncanonical collation",
                actual.name
            ));
        }
    }
    Ok(())
}

fn composite_type_fingerprint(shape: &CompositeTypeShape) -> Vec<i32> {
    let mut words = type_fingerprint(&shape.base);
    words.push(i32::try_from(shape.fields.len()).unwrap_or(i32::MAX));
    for field in &shape.fields {
        let [name_low, name_high] = u64_words(fnv1a64(&[field.name.as_bytes()]));
        words.extend([
            oid_word(field.type_oid),
            field.typmod,
            oid_word(field.collation_oid),
            i32::from(field.not_null),
            u32_word(field.tuple_xmin),
            u32_word(field.tuple_block),
            i32::from(field.tuple_offset),
            name_low,
            name_high,
        ]);
    }
    words
}

fn normalized_catalog_source(source: &str) -> String {
    let mut without_line_comments = String::with_capacity(source.len());
    for line in source.lines() {
        without_line_comments.push_str(line.split_once("--").map_or(line, |(code, _)| code));
        without_line_comments.push('\n');
    }
    without_line_comments
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

unsafe fn namespace_name(schema_oid: pg_sys::Oid) -> Result<String, String> {
    let name = unsafe { pg_sys::get_namespace_name(schema_oid) };
    if name.is_null() {
        return Err(format!(
            "namespace OID {} does not exist",
            u32::from(schema_oid)
        ));
    }
    let result = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| {
            format!(
                "namespace OID {} has an invalid name",
                u32::from(schema_oid)
            )
        });
    unsafe { pg_sys::pfree(name.cast()) };
    result
}

unsafe fn find_function_oid(
    schema_oid: pg_sys::Oid,
    name: &str,
    argument_types: &[pg_sys::Oid],
) -> Result<pg_sys::Oid, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "invalid function name".to_owned())?;
    let vector = unsafe {
        pg_sys::buildoidvector(
            argument_types.as_ptr(),
            argument_types.len() as ::core::ffi::c_int,
        )
    };
    if vector.is_null() {
        return Err("could not build function signature".to_owned());
    }
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(name.as_ptr().cast()),
            pg_sys::PointerGetDatum(vector.cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    unsafe { pg_sys::pfree(vector.cast()) };
    if tuple.is_null() {
        return Err("PostGIS Raster exact function overload is missing".to_owned());
    }
    let oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(oid)
}

#[allow(clippy::too_many_arguments)]
fn validate_raster_function_common(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    name: &str,
    language: &str,
    arguments: &[pg_sys::Oid],
    return_type: pg_sys::Oid,
    strict: bool,
    argument_defaults: i16,
) -> Result<(), String> {
    if shape.name != name
        || shape.schema_oid != schema_oid
        || shape.language_name != language
        || shape.kind != pg_sys::PROKIND_FUNCTION
        || shape.argument_types != arguments
        || shape.return_type != return_type
        || shape.strict != strict
        || shape.argument_defaults != argument_defaults
        || shape.security_definer
        || shape.support_function != pg_sys::InvalidOid
        || shape.returns_set
        || shape.volatility != pg_sys::PROVOLATILE_IMMUTABLE
        || shape.parallel != pg_sys::PROPARALLEL_SAFE
    {
        return Err(format!(
            "PostGIS Raster {name} has a noncanonical catalog shape"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_raster_c_function(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    name: &str,
    arguments: &[pg_sys::Oid],
    return_type: pg_sys::Oid,
    strict: bool,
    argument_defaults: i16,
    source: &str,
) -> Result<(), String> {
    validate_raster_function_common(
        shape,
        schema_oid,
        name,
        "c",
        arguments,
        return_type,
        strict,
        argument_defaults,
    )?;
    if shape.source != source || shape.binary.as_deref() != Some(POSTGIS_RASTER_LIBRARY) {
        return Err(format!(
            "PostGIS Raster {name} has a noncanonical C implementation"
        ));
    }
    Ok(())
}

fn validate_raster_sql_function(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    name: &str,
    arguments: &[pg_sys::Oid],
    return_type: pg_sys::Oid,
    argument_defaults: i16,
    expected_source: &str,
) -> Result<(), String> {
    validate_raster_function_common(
        shape,
        schema_oid,
        name,
        "sql",
        arguments,
        return_type,
        true,
        argument_defaults,
    )?;
    if shape.binary.is_some()
        || normalized_catalog_source(&shape.source) != normalized_catalog_source(expected_source)
    {
        return Err(format!(
            "PostGIS Raster {name} SQL wrapper body is noncanonical"
        ));
    }
    Ok(())
}

fn validate_raster_plpgsql_function(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    name: &str,
    arguments: &[pg_sys::Oid],
    return_type: pg_sys::Oid,
    variadic_type: pg_sys::Oid,
    expected_source: &str,
) -> Result<(), String> {
    validate_raster_function_common(
        shape,
        schema_oid,
        name,
        "plpgsql",
        arguments,
        return_type,
        true,
        0,
    )?;
    if shape.variadic_type != variadic_type
        || shape.binary.is_some()
        || normalized_catalog_source(&shape.source) != normalized_catalog_source(expected_source)
    {
        return Err(format!(
            "PostGIS Raster {name} PL/pgSQL wrapper body is noncanonical"
        ));
    }
    Ok(())
}

fn require_extension_member(
    extension_oid: pg_sys::Oid,
    class_oid: pg_sys::Oid,
    object_oid: pg_sys::Oid,
    label: &str,
) -> Result<(), String> {
    if unsafe { pg_sys::getExtensionOfObject(class_oid, object_oid) } != extension_oid {
        return Err(format!(
            "{label} OID {} is not owned by extension postgis_raster",
            u32::from(object_oid)
        ));
    }
    Ok(())
}

fn raster_type_fingerprint(shape: &PostgisRasterTypeShape) -> Vec<i32> {
    let mut words = type_fingerprint(&shape.base);
    words.extend([
        oid_word(shape.input_fn),
        oid_word(shape.output_fn),
        oid_word(shape.receive_fn),
        oid_word(shape.send_fn),
    ]);
    words
}

/// Prove that `type_oid` is the exact extension-owned PostGIS raster base type.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn validate_postgis_raster_type(type_oid: pg_sys::Oid) -> Result<Vec<i32>, String> {
    let extension = unsafe { postgis_raster_extension_shape()? };
    let shape = unsafe { read_postgis_raster_type_shape(type_oid)? };
    validate_postgis_raster_type_shape(&shape, extension.schema_oid)?;
    require_extension_member(
        extension.extension_oid,
        pg_sys::TypeRelationId,
        type_oid,
        "raster type",
    )?;
    let input = unsafe { read_h3_function_shape(shape.input_fn)? };
    validate_raster_c_function(
        &input,
        extension.schema_oid,
        "raster_in",
        &[pg_sys::CSTRINGOID],
        type_oid,
        true,
        0,
        "RASTER_in",
    )?;
    require_extension_member(
        extension.extension_oid,
        pg_sys::ProcedureRelationId,
        shape.input_fn,
        "raster input function",
    )?;
    let output = unsafe { read_h3_function_shape(shape.output_fn)? };
    validate_raster_c_function(
        &output,
        extension.schema_oid,
        "raster_out",
        &[type_oid],
        pg_sys::CSTRINGOID,
        true,
        0,
        "RASTER_out",
    )?;
    require_extension_member(
        extension.extension_oid,
        pg_sys::ProcedureRelationId,
        shape.output_fn,
        "raster output function",
    )?;
    let mut words = raster_type_fingerprint(&shape);
    words.extend(function_fingerprint(&input));
    words.extend(function_fingerprint(&output));
    Ok(words)
}

/// Resolve and prove the exact public raster overloads selected by pg_accel.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn resolve_postgis_raster_catalog() -> Result<PostgisRasterCatalogIdentity, String> {
    let extension = unsafe { postgis_raster_extension_shape()? };
    let raster_type_oid = unsafe { find_type_oid(extension.schema_oid, POSTGIS_RASTER_TYPE_NAME)? };
    let summary_stats_type_oid =
        unsafe { find_type_oid(extension.schema_oid, POSTGIS_RASTER_SUMMARY_TYPE_NAME)? };
    let reclass_arg_type_oid =
        unsafe { find_type_oid(extension.schema_oid, POSTGIS_RASTER_RECLASS_ARG_TYPE_NAME)? };
    require_extension_member(
        extension.extension_oid,
        pg_sys::TypeRelationId,
        summary_stats_type_oid,
        "summarystats type",
    )?;
    require_extension_member(
        extension.extension_oid,
        pg_sys::TypeRelationId,
        reclass_arg_type_oid,
        "reclassarg type",
    )?;
    let summary_type = unsafe { read_composite_type_shape(summary_stats_type_oid)? };
    validate_composite_type_shape(
        &summary_type,
        extension.schema_oid,
        POSTGIS_RASTER_SUMMARY_TYPE_NAME,
        &[
            ("count", pg_sys::INT8OID),
            ("sum", pg_sys::FLOAT8OID),
            ("mean", pg_sys::FLOAT8OID),
            ("stddev", pg_sys::FLOAT8OID),
            ("min", pg_sys::FLOAT8OID),
            ("max", pg_sys::FLOAT8OID),
        ],
    )?;
    let reclass_arg_type = unsafe { read_composite_type_shape(reclass_arg_type_oid)? };
    validate_composite_type_shape(
        &reclass_arg_type,
        extension.schema_oid,
        POSTGIS_RASTER_RECLASS_ARG_TYPE_NAME,
        &[
            ("nband", pg_sys::INT4OID),
            ("reclassexpr", pg_sys::TEXTOID),
            ("pixeltype", pg_sys::TEXTOID),
            ("nodataval", pg_sys::FLOAT8OID),
        ],
    )?;
    let reclass_arg_array_oid = unsafe { pg_sys::get_array_type(reclass_arg_type_oid) };
    if reclass_arg_array_oid == pg_sys::InvalidOid {
        return Err("PostGIS Raster reclassarg has no array type".to_owned());
    }
    let schema_name = unsafe { namespace_name(extension.schema_oid)? };
    let reclass_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_reclass",
            &[raster_type_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
        )?
    };
    let summary_stats_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_summarystats",
            &[raster_type_oid, pg_sys::INT4OID, pg_sys::BOOLOID],
        )?
    };
    let summary_stats_default_band_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_summarystats",
            &[raster_type_oid, pg_sys::BOOLOID],
        )?
    };
    let reclass_variadic_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_reclass",
            &[raster_type_oid, reclass_arg_array_oid],
        )?
    };
    let reclass_impl_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "_st_reclass",
            &[raster_type_oid, reclass_arg_array_oid],
        )?
    };
    let summary_stats_impl_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "_st_summarystats",
            &[
                raster_type_oid,
                pg_sys::INT4OID,
                pg_sys::BOOLOID,
                pg_sys::FLOAT8OID,
            ],
        )?
    };
    let reclass = unsafe { read_h3_function_shape(reclass_fn_oid)? };
    validate_raster_sql_function(
        &reclass,
        extension.schema_oid,
        "st_reclass",
        &[raster_type_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
        raster_type_oid,
        0,
        &format!("SELECT {schema_name}.st_reclass($1, ROW(1, $2, $3, NULL))"),
    )?;
    if reclass.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster st_reclass(raster,text,text) became variadic".to_owned());
    }
    let reclass_variadic = unsafe { read_h3_function_shape(reclass_variadic_fn_oid)? };
    validate_raster_plpgsql_function(
        &reclass_variadic,
        extension.schema_oid,
        "st_reclass",
        &[raster_type_oid, reclass_arg_array_oid],
        raster_type_oid,
        reclass_arg_type_oid,
        &format!(
            "DECLARE
                i int;
                expr text;
             BEGIN
                FOR i IN SELECT * FROM generate_subscripts($2, 1) LOOP
                    IF $2[i].nband IS NULL OR $2[i].reclassexpr IS NULL OR $2[i].pixeltype IS NULL THEN
                        RAISE WARNING 'Values are required for the nband, reclassexpr and pixeltype attributes.';
                        RETURN rast;
                    END IF;
                END LOOP;
                RETURN {schema_name}._ST_reclass($1, VARIADIC $2);
             END;"
        ),
    )?;
    let reclass_impl = unsafe { read_h3_function_shape(reclass_impl_fn_oid)? };
    validate_raster_c_function(
        &reclass_impl,
        extension.schema_oid,
        "_st_reclass",
        &[raster_type_oid, reclass_arg_array_oid],
        raster_type_oid,
        true,
        0,
        "RASTER_reclass",
    )?;
    if reclass_impl.variadic_type != reclass_arg_type_oid {
        return Err("PostGIS Raster _st_reclass has a noncanonical variadic type".to_owned());
    }
    let summary = unsafe { read_h3_function_shape(summary_stats_fn_oid)? };
    validate_raster_sql_function(
        &summary,
        extension.schema_oid,
        "st_summarystats",
        &[raster_type_oid, pg_sys::INT4OID, pg_sys::BOOLOID],
        summary_stats_type_oid,
        2,
        &format!("SELECT {schema_name}._ST_summarystats($1, $2, $3, 1)"),
    )?;
    if summary.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster st_summarystats(raster,int4,bool) became variadic".to_owned());
    }
    let summary_default = unsafe { read_h3_function_shape(summary_stats_default_band_fn_oid)? };
    validate_raster_sql_function(
        &summary_default,
        extension.schema_oid,
        "st_summarystats",
        &[raster_type_oid, pg_sys::BOOLOID],
        summary_stats_type_oid,
        0,
        &format!("SELECT {schema_name}._ST_summarystats($1, 1, $2, 1)"),
    )?;
    if summary_default.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster st_summarystats(raster,bool) became variadic".to_owned());
    }
    let summary_impl = unsafe { read_h3_function_shape(summary_stats_impl_fn_oid)? };
    validate_raster_c_function(
        &summary_impl,
        extension.schema_oid,
        "_st_summarystats",
        &[
            raster_type_oid,
            pg_sys::INT4OID,
            pg_sys::BOOLOID,
            pg_sys::FLOAT8OID,
        ],
        summary_stats_type_oid,
        false,
        3,
        "RASTER_summaryStats",
    )?;
    if summary_impl.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster _st_summarystats became variadic".to_owned());
    }
    for (oid, label) in [
        (reclass_fn_oid, "st_reclass wrapper"),
        (summary_stats_fn_oid, "st_summarystats wrapper"),
        (
            summary_stats_default_band_fn_oid,
            "st_summarystats default-band wrapper",
        ),
        (reclass_variadic_fn_oid, "st_reclass variadic wrapper"),
        (reclass_impl_fn_oid, "_st_reclass implementation"),
        (summary_stats_impl_fn_oid, "_st_summarystats implementation"),
    ] {
        require_extension_member(
            extension.extension_oid,
            pg_sys::ProcedureRelationId,
            oid,
            label,
        )?;
    }

    let type_words = unsafe { validate_postgis_raster_type(raster_type_oid)? };
    let [extension_hash_low, extension_hash_high] =
        u64_words(fnv1a64(&[extension.version.as_bytes()]));
    let mut fingerprint_words = vec![
        u32_word(POSTGIS_RASTER_FINGERPRINT_VERSION),
        oid_word(extension.extension_oid),
        oid_word(extension.schema_oid),
        u32_word(extension.tuple_xmin),
        u32_word(extension.tuple_block),
        i32::from(extension.tuple_offset),
        i32::from(extension.relocatable),
        extension_hash_low,
        extension_hash_high,
        oid_word(summary_stats_type_oid),
        oid_word(reclass_arg_type_oid),
    ];
    fingerprint_words.extend(type_words);
    fingerprint_words.extend(composite_type_fingerprint(&summary_type));
    fingerprint_words.extend(composite_type_fingerprint(&reclass_arg_type));
    fingerprint_words.extend(function_fingerprint(&reclass));
    fingerprint_words.extend(function_fingerprint(&reclass_variadic));
    fingerprint_words.extend(function_fingerprint(&reclass_impl));
    fingerprint_words.extend(function_fingerprint(&summary));
    fingerprint_words.extend(function_fingerprint(&summary_default));
    fingerprint_words.extend(function_fingerprint(&summary_impl));

    Ok(PostgisRasterCatalogIdentity {
        extension_oid: extension.extension_oid,
        schema_oid: extension.schema_oid,
        raster_type_oid,
        summary_stats_type_oid,
        reclass_fn_oid,
        summary_stats_fn_oid,
        summary_stats_default_band_fn_oid,
        reclass_impl_fn_oid,
        summary_stats_impl_fn_oid,
        fingerprint_words,
    })
}

/// Resolve one function OID to the exact public PostGIS Raster overload.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn resolve_postgis_raster_function(
    fn_oid: pg_sys::Oid,
) -> Result<(PostgisRasterCatalogIdentity, PostgisRasterFunction), String> {
    let identity = unsafe { resolve_postgis_raster_catalog()? };
    let function = if fn_oid == identity.reclass_fn_oid {
        PostgisRasterFunction::Reclass
    } else if fn_oid == identity.summary_stats_fn_oid {
        PostgisRasterFunction::SummaryStats
    } else if fn_oid == identity.summary_stats_default_band_fn_oid {
        PostgisRasterFunction::SummaryStatsDefaultBand
    } else {
        return Err(format!(
            "function OID {} is not a supported PostGIS Raster overload",
            u32::from(fn_oid)
        ));
    };
    Ok((identity, function))
}

#[derive(Clone, Copy)]
struct PostgisFunctionContract<'a> {
    name: &'a str,
    source: &'a str,
    argument_types: &'a [pg_sys::Oid],
    return_type: pg_sys::Oid,
    support_function: pg_sys::Oid,
    strict: bool,
    volatility: u8,
    parallel: u8,
}

fn validate_postgis_type_shape(
    shape: &PostgisTypeShape,
    schema_oid: pg_sys::Oid,
) -> Result<(), String> {
    if shape.name != POSTGIS_GEOMETRY_TYPE_NAME || shape.schema_oid != schema_oid {
        return Err(format!(
            "type OID {} is not the PostGIS-schema geometry type",
            u32::from(shape.type_oid)
        ));
    }
    if shape.typtype != pg_sys::TYPTYPE_BASE || !shape.typisdefined {
        return Err("PostGIS geometry must be a defined base type".to_owned());
    }
    if shape.typlen != -1 || shape.typbyval {
        return Err("PostGIS geometry must be a variable-length pass-by-reference type".to_owned());
    }
    if shape.typalign != pg_sys::TYPALIGN_DOUBLE || shape.typstorage != pg_sys::TYPSTORAGE_MAIN {
        return Err("PostGIS geometry must use double alignment and main storage".to_owned());
    }
    if shape.typrelid != pg_sys::InvalidOid
        || shape.typsubscript != pg_sys::InvalidOid
        || shape.typelem != pg_sys::InvalidOid
        || shape.typbasetype != pg_sys::InvalidOid
        || shape.typndims != 0
        || shape.typcollation != pg_sys::InvalidOid
    {
        return Err(
            "PostGIS geometry must not be composite, domain, array-like, or collatable".to_owned(),
        );
    }
    if [
        shape.typinput,
        shape.typoutput,
        shape.typreceive,
        shape.typsend,
        shape.typmodin,
        shape.typmodout,
        shape.typanalyze,
    ]
    .contains(&pg_sys::InvalidOid)
    {
        return Err("PostGIS geometry has an incomplete type-function contract".to_owned());
    }
    Ok(())
}

fn validate_postgis_function_shape(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    contract: PostgisFunctionContract<'_>,
) -> Result<(), String> {
    if shape.name != contract.name || shape.schema_oid != schema_oid {
        return Err(format!(
            "function OID {} is not PostGIS-schema {}",
            u32::from(shape.fn_oid),
            contract.name
        ));
    }
    if shape.kind != pg_sys::PROKIND_FUNCTION
        || shape.language_name != POSTGIS_LANGUAGE_NAME
        || shape.source != contract.source
        || shape.binary.as_deref() != Some(POSTGIS_LIBRARY_NAME)
    {
        return Err(format!(
            "PostGIS {} does not have its canonical C implementation",
            contract.name
        ));
    }
    let has_canonical_signature = shape.argument_types == contract.argument_types
        && shape.return_type == contract.return_type;
    if !has_canonical_signature {
        return Err(format!(
            "PostGIS {} has a noncanonical signature",
            contract.name
        ));
    }
    if shape.argument_defaults != 0
        || shape.security_definer
        || shape.variadic_type != pg_sys::InvalidOid
        || shape.returns_set
    {
        return Err(format!(
            "PostGIS {} must be a scalar, nonvariadic invoker function without defaults",
            contract.name
        ));
    }
    if shape.support_function != contract.support_function
        || shape.strict != contract.strict
        || shape.volatility != contract.volatility
        || shape.parallel != contract.parallel
    {
        return Err(format!(
            "PostGIS {} planner/execution flags are noncanonical",
            contract.name
        ));
    }
    Ok(())
}

fn postgis_type_fingerprint(shape: &PostgisTypeShape) -> Vec<i32> {
    vec![
        oid_word(shape.type_oid),
        oid_word(shape.schema_oid),
        u32_word(shape.tuple_xmin),
        u32_word(shape.tuple_block),
        i32::from(shape.tuple_offset),
        i32::from(shape.typlen),
        i32::from(shape.typalign),
        i32::from(shape.typstorage),
        oid_word(shape.typinput),
        oid_word(shape.typoutput),
        oid_word(shape.typreceive),
        oid_word(shape.typsend),
        oid_word(shape.typmodin),
        oid_word(shape.typmodout),
        oid_word(shape.typanalyze),
    ]
}

unsafe fn find_postgis_geometry_type(schema_oid: pg_sys::Oid) -> Result<pg_sys::Oid, String> {
    let type_name = std::ffi::CString::new(POSTGIS_GEOMETRY_TYPE_NAME)
        .map_err(|_| "invalid PostGIS geometry type name".to_owned())?;
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::PointerGetDatum(type_name.as_ptr().cast()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "extension postgis has no geometry type in schema OID {}",
            u32::from(schema_oid)
        ));
    }
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_postgis_type_shape(type_oid: pg_sys::Oid) -> Result<PostgisTypeShape, String> {
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("type OID {} does not exist", u32::from(type_oid)));
    }
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        let name = std::ffi::CStr::from_ptr((*form).typname.data.as_ptr())
            .to_str()
            .map_err(|_| format!("type OID {} has an invalid name", u32::from(type_oid)))?
            .to_owned();
        Ok(PostgisTypeShape {
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
            typinput: (*form).typinput,
            typoutput: (*form).typoutput,
            typreceive: (*form).typreceive,
            typsend: (*form).typsend,
            typmodin: (*form).typmodin,
            typmodout: (*form).typmodout,
            typanalyze: (*form).typanalyze,
            tuple_xmin: pg_sys::htup::HeapTupleHeaderGetRawXmin((*tuple).t_data).into(),
            tuple_block: pg_sys::ItemPointerGetBlockNumber(&raw const (*tuple).t_self),
            tuple_offset: pg_sys::ItemPointerGetOffsetNumber(&raw const (*tuple).t_self),
        })
    })();
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

fn postgis_contract<'a>(
    name: &'a str,
    source: &'a str,
    arguments: &'a [pg_sys::Oid],
    return_type: pg_sys::Oid,
    support_function: pg_sys::Oid,
) -> PostgisFunctionContract<'a> {
    PostgisFunctionContract {
        name,
        source,
        argument_types: arguments,
        return_type,
        support_function,
        strict: true,
        volatility: pg_sys::PROVOLATILE_IMMUTABLE,
        parallel: pg_sys::PROPARALLEL_SAFE,
    }
}

unsafe fn validate_postgis_function(
    extension_oid: pg_sys::Oid,
    schema_oid: pg_sys::Oid,
    fn_oid: pg_sys::Oid,
    contract: PostgisFunctionContract<'_>,
) -> Result<H3FunctionShape, String> {
    let shape = unsafe { read_h3_function_shape(fn_oid)? };
    validate_postgis_function_shape(&shape, schema_oid, contract)?;
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::ProcedureRelationId, fn_oid) } != extension_oid
    {
        return Err(format!(
            "function OID {} is not owned by extension postgis",
            u32::from(fn_oid)
        ));
    }
    Ok(shape)
}

/// Resolve and prove the exact PostGIS geometry type and spatial functions.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn resolve_postgis_catalog() -> Result<PostgisCatalogIdentity, String> {
    let (extension_oid, schema_oid) =
        unsafe { named_extension_identity(POSTGIS_EXTENSION_NAME, Some(false))? };
    let geometry_type_oid = unsafe { find_postgis_geometry_type(schema_oid)? };
    let type_shape = unsafe { read_postgis_type_shape(geometry_type_oid)? };
    validate_postgis_type_shape(&type_shape, schema_oid)?;
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::TypeRelationId, geometry_type_oid) }
        != extension_oid
    {
        return Err(format!(
            "type OID {} is not owned by extension postgis",
            u32::from(geometry_type_oid)
        ));
    }

    let support_fn_oid = unsafe {
        find_exact_function(
            schema_oid,
            POSTGIS_SUPPORT_FUNCTION_NAME,
            &[pg_sys::INTERNALOID],
        )?
    };
    let support_contract = PostgisFunctionContract {
        name: POSTGIS_SUPPORT_FUNCTION_NAME,
        source: POSTGIS_SUPPORT_FUNCTION_NAME,
        argument_types: &[pg_sys::INTERNALOID],
        return_type: pg_sys::INTERNALOID,
        support_function: pg_sys::InvalidOid,
        strict: false,
        volatility: pg_sys::PROVOLATILE_VOLATILE,
        parallel: pg_sys::PROPARALLEL_UNSAFE,
    };
    let support_shape = unsafe {
        validate_postgis_function(extension_oid, schema_oid, support_fn_oid, support_contract)?
    };

    let cstring_args = [pg_sys::CSTRINGOID];
    let geometry_args = [geometry_type_oid];
    let internal_args = [pg_sys::INTERNALOID];
    let cstring_array_args = [pg_sys::CSTRINGARRAYOID];
    let int4_args = [pg_sys::INT4OID];
    let io_contracts = [
        (
            type_shape.typinput,
            postgis_contract(
                "geometry_in",
                "LWGEOM_in",
                &cstring_args,
                geometry_type_oid,
                pg_sys::InvalidOid,
            ),
        ),
        (
            type_shape.typoutput,
            postgis_contract(
                "geometry_out",
                "LWGEOM_out",
                &geometry_args,
                pg_sys::CSTRINGOID,
                pg_sys::InvalidOid,
            ),
        ),
        (
            type_shape.typreceive,
            postgis_contract(
                "geometry_recv",
                "LWGEOM_recv",
                &internal_args,
                geometry_type_oid,
                pg_sys::InvalidOid,
            ),
        ),
        (
            type_shape.typsend,
            postgis_contract(
                "geometry_send",
                "LWGEOM_send",
                &geometry_args,
                pg_sys::BYTEAOID,
                pg_sys::InvalidOid,
            ),
        ),
        (
            type_shape.typmodin,
            postgis_contract(
                "geometry_typmod_in",
                "geometry_typmod_in",
                &cstring_array_args,
                pg_sys::INT4OID,
                pg_sys::InvalidOid,
            ),
        ),
        (
            type_shape.typmodout,
            postgis_contract(
                "geometry_typmod_out",
                "postgis_typmod_out",
                &int4_args,
                pg_sys::CSTRINGOID,
                pg_sys::InvalidOid,
            ),
        ),
    ];
    let mut function_shapes = Vec::new();
    for (fn_oid, contract) in io_contracts {
        function_shapes.push(unsafe {
            validate_postgis_function(extension_oid, schema_oid, fn_oid, contract)?
        });
    }
    let analyze_contract = PostgisFunctionContract {
        name: "geometry_analyze",
        source: "gserialized_analyze_nd",
        argument_types: &[pg_sys::INTERNALOID],
        return_type: pg_sys::BOOLOID,
        support_function: pg_sys::InvalidOid,
        strict: true,
        volatility: pg_sys::PROVOLATILE_VOLATILE,
        parallel: pg_sys::PROPARALLEL_UNSAFE,
    };
    function_shapes.push(unsafe {
        validate_postgis_function(
            extension_oid,
            schema_oid,
            type_shape.typanalyze,
            analyze_contract,
        )?
    });

    let binary_args = [geometry_type_oid, geometry_type_oid];
    let intersects_fn_oid =
        unsafe { find_exact_function(schema_oid, "st_intersects", &binary_args)? };
    let contains_fn_oid = unsafe { find_exact_function(schema_oid, "st_contains", &binary_args)? };
    let within_fn_oid = unsafe { find_exact_function(schema_oid, "st_within", &binary_args)? };
    let dwithin_args = [geometry_type_oid, geometry_type_oid, pg_sys::FLOAT8OID];
    let dwithin_fn_oid = unsafe { find_exact_function(schema_oid, "st_dwithin", &dwithin_args)? };
    let distance_fn_oid = unsafe { find_exact_function(schema_oid, "st_distance", &binary_args)? };
    let is_valid_fn_oid = unsafe { find_exact_function(schema_oid, "st_isvalid", &geometry_args)? };
    for (fn_oid, contract) in [
        (
            intersects_fn_oid,
            postgis_contract(
                "st_intersects",
                "ST_Intersects",
                &binary_args,
                pg_sys::BOOLOID,
                support_fn_oid,
            ),
        ),
        (
            contains_fn_oid,
            postgis_contract(
                "st_contains",
                "contains",
                &binary_args,
                pg_sys::BOOLOID,
                support_fn_oid,
            ),
        ),
        (
            within_fn_oid,
            postgis_contract(
                "st_within",
                "within",
                &binary_args,
                pg_sys::BOOLOID,
                support_fn_oid,
            ),
        ),
        (
            dwithin_fn_oid,
            postgis_contract(
                "st_dwithin",
                "LWGEOM_dwithin",
                &dwithin_args,
                pg_sys::BOOLOID,
                support_fn_oid,
            ),
        ),
        (
            distance_fn_oid,
            postgis_contract(
                "st_distance",
                "ST_Distance",
                &binary_args,
                pg_sys::FLOAT8OID,
                pg_sys::InvalidOid,
            ),
        ),
        (
            is_valid_fn_oid,
            postgis_contract(
                "st_isvalid",
                "isvalid",
                &geometry_args,
                pg_sys::BOOLOID,
                pg_sys::InvalidOid,
            ),
        ),
    ] {
        function_shapes.push(unsafe {
            validate_postgis_function(extension_oid, schema_oid, fn_oid, contract)?
        });
    }

    let mut fingerprint_words = vec![
        u32_word(POSTGIS_FINGERPRINT_VERSION),
        oid_word(extension_oid),
        oid_word(schema_oid),
    ];
    fingerprint_words.extend(postgis_type_fingerprint(&type_shape));
    fingerprint_words.extend(function_fingerprint(&support_shape));
    for shape in &function_shapes {
        fingerprint_words.extend(function_fingerprint(shape));
    }
    Ok(PostgisCatalogIdentity {
        extension_oid,
        schema_oid,
        geometry_type_oid,
        intersects_fn_oid,
        contains_fn_oid,
        within_fn_oid,
        dwithin_fn_oid,
        distance_fn_oid,
        is_valid_fn_oid,
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
}

#[cfg(test)]
mod postgis_tests {
    use super::*;

    fn oid(value: u32) -> pg_sys::Oid {
        pg_sys::Oid::from(value)
    }

    fn valid_type() -> PostgisTypeShape {
        PostgisTypeShape {
            type_oid: oid(60_001),
            name: POSTGIS_GEOMETRY_TYPE_NAME.to_owned(),
            schema_oid: oid(60_000),
            typlen: -1,
            typbyval: false,
            typtype: pg_sys::TYPTYPE_BASE,
            typisdefined: true,
            typrelid: pg_sys::InvalidOid,
            typsubscript: pg_sys::InvalidOid,
            typelem: pg_sys::InvalidOid,
            typalign: pg_sys::TYPALIGN_DOUBLE,
            typstorage: pg_sys::TYPSTORAGE_MAIN,
            typbasetype: pg_sys::InvalidOid,
            typndims: 0,
            typcollation: pg_sys::InvalidOid,
            typinput: oid(60_010),
            typoutput: oid(60_011),
            typreceive: oid(60_012),
            typsend: oid(60_013),
            typmodin: oid(60_014),
            typmodout: oid(60_015),
            typanalyze: oid(60_016),
            tuple_xmin: 21,
            tuple_block: 10,
            tuple_offset: 2,
        }
    }

    fn valid_intersects() -> H3FunctionShape {
        H3FunctionShape {
            fn_oid: oid(60_020),
            name: "st_intersects".to_owned(),
            schema_oid: oid(60_000),
            language_oid: oid(13),
            language_name: POSTGIS_LANGUAGE_NAME.to_owned(),
            kind: pg_sys::PROKIND_FUNCTION,
            argument_defaults: 0,
            security_definer: false,
            support_function: oid(60_030),
            strict: true,
            returns_set: false,
            volatility: pg_sys::PROVOLATILE_IMMUTABLE,
            parallel: pg_sys::PROPARALLEL_SAFE,
            variadic_type: pg_sys::InvalidOid,
            return_type: pg_sys::BOOLOID,
            argument_types: vec![oid(60_001), oid(60_001)],
            source: "ST_Intersects".to_owned(),
            binary: Some(POSTGIS_LIBRARY_NAME.to_owned()),
            tuple_xmin: 22,
            tuple_block: 11,
            tuple_offset: 3,
        }
    }

    #[test]
    fn geometry_type_contract_is_exact_and_replacement_sensitive() {
        let valid = valid_type();
        assert_eq!(
            validate_postgis_type_shape(&valid, valid.schema_oid),
            Ok(())
        );

        let mut by_value = valid.clone();
        by_value.typbyval = true;
        assert!(validate_postgis_type_shape(&by_value, by_value.schema_oid).is_err());
        let mut storage = valid.clone();
        storage.typstorage = pg_sys::TYPSTORAGE_EXTENDED;
        assert!(validate_postgis_type_shape(&storage, storage.schema_oid).is_err());
        let mut missing_receive = valid.clone();
        missing_receive.typreceive = pg_sys::InvalidOid;
        assert!(validate_postgis_type_shape(&missing_receive, missing_receive.schema_oid).is_err());

        let mut replacement = valid.clone();
        replacement.tuple_offset += 1;
        assert_ne!(
            postgis_type_fingerprint(&valid),
            postgis_type_fingerprint(&replacement)
        );
    }

    #[test]
    fn spatial_function_contract_rejects_semantic_replacements() {
        let valid = valid_intersects();
        let arguments = [oid(60_001), oid(60_001)];
        let contract = postgis_contract(
            "st_intersects",
            "ST_Intersects",
            &arguments,
            pg_sys::BOOLOID,
            oid(60_030),
        );
        assert_eq!(
            validate_postgis_function_shape(&valid, valid.schema_oid, contract),
            Ok(())
        );

        let mut replacement = valid.clone();
        replacement.source = "replacement".to_owned();
        assert!(
            validate_postgis_function_shape(&replacement, replacement.schema_oid, contract)
                .is_err()
        );
        let mut wrong_support = valid.clone();
        wrong_support.support_function = pg_sys::InvalidOid;
        assert!(
            validate_postgis_function_shape(&wrong_support, wrong_support.schema_oid, contract)
                .is_err()
        );
        let mut security_definer = valid.clone();
        security_definer.security_definer = true;
        assert!(
            validate_postgis_function_shape(
                &security_definer,
                security_definer.schema_oid,
                contract
            )
            .is_err()
        );

        let mut same_transaction_replacement = valid.clone();
        same_transaction_replacement.tuple_offset += 1;
        assert_ne!(
            function_fingerprint(&valid),
            function_fingerprint(&same_transaction_replacement)
        );
    }

    #[test]
    fn geometry_validity_contract_is_exact_and_replacement_sensitive() {
        let geometry_oid = oid(60_001);
        let mut valid = valid_intersects();
        valid.name = "st_isvalid".to_owned();
        valid.source = "isvalid".to_owned();
        valid.argument_types = vec![geometry_oid];
        valid.support_function = pg_sys::InvalidOid;
        let arguments = [geometry_oid];
        let contract = postgis_contract(
            "st_isvalid",
            "isvalid",
            &arguments,
            pg_sys::BOOLOID,
            pg_sys::InvalidOid,
        );
        assert_eq!(
            validate_postgis_function_shape(&valid, valid.schema_oid, contract),
            Ok(())
        );

        let mut replacement = valid.clone();
        replacement.source = "replacement_isvalid".to_owned();
        assert!(
            validate_postgis_function_shape(&replacement, replacement.schema_oid, contract)
                .is_err()
        );
        assert_ne!(
            function_fingerprint(&valid),
            function_fingerprint(&replacement)
        );

        let mut same_signature_impostor = valid.clone();
        same_signature_impostor.fn_oid = oid(60_099);
        same_signature_impostor.tuple_offset += 1;
        assert_eq!(
            validate_postgis_function_shape(
                &same_signature_impostor,
                same_signature_impostor.schema_oid,
                contract,
            ),
            Ok(()),
            "shape validation alone cannot prove extension ownership",
        );
        assert_ne!(
            function_fingerprint(&valid),
            function_fingerprint(&same_signature_impostor),
            "the catalog identity must change for a same-signature replacement",
        );
    }

    #[test]
    fn catalog_classification_is_oid_exact() {
        let catalog = PostgisCatalogIdentity {
            extension_oid: oid(1),
            schema_oid: oid(2),
            geometry_type_oid: oid(3),
            intersects_fn_oid: oid(10),
            contains_fn_oid: oid(11),
            within_fn_oid: oid(12),
            dwithin_fn_oid: oid(13),
            distance_fn_oid: oid(14),
            is_valid_fn_oid: oid(15),
            fingerprint_words: Vec::new(),
        };
        assert_eq!(
            catalog.classify_function(oid(10)),
            Some(PostgisSpatialFunction::Intersects)
        );
        assert_eq!(
            catalog.classify_function(oid(13)),
            Some(PostgisSpatialFunction::DWithin)
        );
        assert_eq!(
            catalog.classify_function(oid(14)),
            Some(PostgisSpatialFunction::Distance)
        );
        assert_eq!(catalog.classify_function(oid(15)), None);
        assert_eq!(catalog.classify_function(oid(99)), None);
    }
}

#[cfg(test)]
mod postgis_raster_tests {
    use super::*;

    fn oid(value: u32) -> pg_sys::Oid {
        pg_sys::Oid::from(value)
    }

    fn valid_raster_type() -> PostgisRasterTypeShape {
        PostgisRasterTypeShape {
            base: H3TypeShape {
                type_oid: oid(60_001),
                name: POSTGIS_RASTER_TYPE_NAME.to_owned(),
                schema_oid: oid(60_000),
                typlen: -1,
                typbyval: false,
                typtype: pg_sys::TYPTYPE_BASE,
                typisdefined: true,
                typrelid: pg_sys::InvalidOid,
                typsubscript: pg_sys::InvalidOid,
                typelem: pg_sys::InvalidOid,
                typalign: pg_sys::TYPALIGN_DOUBLE,
                typstorage: pg_sys::TYPSTORAGE_EXTENDED,
                typbasetype: pg_sys::InvalidOid,
                typndims: 0,
                typcollation: pg_sys::InvalidOid,
                tuple_xmin: 10,
                tuple_block: 20,
                tuple_offset: 3,
            },
            input_fn: oid(60_002),
            output_fn: oid(60_003),
            receive_fn: pg_sys::InvalidOid,
            send_fn: pg_sys::InvalidOid,
        }
    }

    fn valid_function(
        name: &str,
        arguments: Vec<pg_sys::Oid>,
        return_type: pg_sys::Oid,
        source: &str,
    ) -> H3FunctionShape {
        H3FunctionShape {
            fn_oid: oid(60_010),
            name: name.to_owned(),
            schema_oid: oid(60_000),
            language_oid: oid(14),
            language_name: "sql".to_owned(),
            kind: pg_sys::PROKIND_FUNCTION,
            argument_defaults: 0,
            security_definer: false,
            support_function: pg_sys::InvalidOid,
            strict: true,
            returns_set: false,
            volatility: pg_sys::PROVOLATILE_IMMUTABLE,
            parallel: pg_sys::PROPARALLEL_SAFE,
            variadic_type: pg_sys::InvalidOid,
            return_type,
            argument_types: arguments,
            source: source.to_owned(),
            binary: None,
            tuple_xmin: 11,
            tuple_block: 21,
            tuple_offset: 4,
        }
    }

    fn valid_summary_composite() -> CompositeTypeShape {
        let mut base = valid_raster_type().base;
        base.type_oid = oid(60_020);
        base.name = POSTGIS_RASTER_SUMMARY_TYPE_NAME.to_owned();
        base.typtype = pg_sys::TYPTYPE_COMPOSITE;
        base.typrelid = oid(60_021);
        let fields = [
            ("count", pg_sys::INT8OID),
            ("sum", pg_sys::FLOAT8OID),
            ("mean", pg_sys::FLOAT8OID),
            ("stddev", pg_sys::FLOAT8OID),
            ("min", pg_sys::FLOAT8OID),
            ("max", pg_sys::FLOAT8OID),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, type_oid))| CompositeFieldShape {
            name: name.to_owned(),
            type_oid,
            typmod: -1,
            collation_oid: pg_sys::InvalidOid,
            not_null: false,
            tuple_xmin: 30 + index as u32,
            tuple_block: 40,
            tuple_offset: u16::try_from(index + 1).expect("small field index"),
        })
        .collect();
        CompositeTypeShape { base, fields }
    }

    #[test]
    fn raster_type_rejects_impostor_layouts() {
        let valid = valid_raster_type();
        assert_eq!(
            validate_postgis_raster_type_shape(&valid, valid.base.schema_oid),
            Ok(())
        );

        let mut impostor = valid.clone();
        impostor.base.typbyval = true;
        assert!(validate_postgis_raster_type_shape(&impostor, impostor.base.schema_oid).is_err());
        let mut impostor = valid.clone();
        impostor.base.typstorage = pg_sys::TYPSTORAGE_MAIN;
        assert!(validate_postgis_raster_type_shape(&impostor, impostor.base.schema_oid).is_err());
        let mut impostor = valid.clone();
        impostor.receive_fn = oid(60_004);
        assert!(validate_postgis_raster_type_shape(&impostor, impostor.base.schema_oid).is_err());
        let mut replacement = valid.clone();
        replacement.input_fn = oid(60_099);
        assert_ne!(
            raster_type_fingerprint(&valid),
            raster_type_fingerprint(&replacement)
        );
    }

    #[test]
    fn raster_wrapper_rejects_same_signature_replacement() {
        let raster_oid = oid(60_001);
        let source = "SELECT public.st_reclass($1, ROW(1, $2, $3, NULL))";
        let valid = valid_function(
            "st_reclass",
            vec![raster_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
            raster_oid,
            source,
        );
        assert_eq!(
            validate_raster_sql_function(
                &valid,
                valid.schema_oid,
                "st_reclass",
                &[raster_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
                raster_oid,
                0,
                source,
            ),
            Ok(())
        );

        let mut replacement = valid.clone();
        replacement.source = "SELECT $1".to_owned();
        assert!(
            validate_raster_sql_function(
                &replacement,
                replacement.schema_oid,
                "st_reclass",
                &[raster_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
                raster_oid,
                0,
                source,
            )
            .is_err()
        );
        assert_ne!(
            function_fingerprint(&valid),
            function_fingerprint(&replacement)
        );

        let mut replacement = valid;
        replacement.binary = Some(POSTGIS_RASTER_LIBRARY.to_owned());
        assert!(
            validate_raster_sql_function(
                &replacement,
                replacement.schema_oid,
                "st_reclass",
                &[raster_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
                raster_oid,
                0,
                source,
            )
            .is_err()
        );
    }

    #[test]
    fn raster_c_function_requires_exact_symbol_and_library() {
        let raster_oid = oid(60_001);
        let mut valid = valid_function(
            "_st_reclass",
            vec![raster_oid, oid(60_030)],
            raster_oid,
            "RASTER_reclass",
        );
        valid.language_name = "c".to_owned();
        valid.binary = Some(POSTGIS_RASTER_LIBRARY.to_owned());
        assert_eq!(
            validate_raster_c_function(
                &valid,
                valid.schema_oid,
                "_st_reclass",
                &valid.argument_types,
                raster_oid,
                true,
                0,
                "RASTER_reclass",
            ),
            Ok(())
        );
        let mut impostor = valid.clone();
        impostor.source = "RASTER_reclass_v2".to_owned();
        assert!(
            validate_raster_c_function(
                &impostor,
                impostor.schema_oid,
                "_st_reclass",
                &impostor.argument_types,
                raster_oid,
                true,
                0,
                "RASTER_reclass",
            )
            .is_err()
        );
        let mut impostor = valid;
        impostor.binary = Some("$libdir/impostor".to_owned());
        assert!(
            validate_raster_c_function(
                &impostor,
                impostor.schema_oid,
                "_st_reclass",
                &impostor.argument_types,
                raster_oid,
                true,
                0,
                "RASTER_reclass",
            )
            .is_err()
        );
    }

    #[test]
    fn raster_composite_layout_and_fingerprint_are_exact() {
        let valid = valid_summary_composite();
        let expected = [
            ("count", pg_sys::INT8OID),
            ("sum", pg_sys::FLOAT8OID),
            ("mean", pg_sys::FLOAT8OID),
            ("stddev", pg_sys::FLOAT8OID),
            ("min", pg_sys::FLOAT8OID),
            ("max", pg_sys::FLOAT8OID),
        ];
        assert_eq!(
            validate_composite_type_shape(
                &valid,
                valid.base.schema_oid,
                POSTGIS_RASTER_SUMMARY_TYPE_NAME,
                &expected,
            ),
            Ok(())
        );
        let mut replacement = valid.clone();
        replacement.fields.swap(1, 2);
        assert!(
            validate_composite_type_shape(
                &replacement,
                replacement.base.schema_oid,
                POSTGIS_RASTER_SUMMARY_TYPE_NAME,
                &expected,
            )
            .is_err()
        );
        assert_ne!(
            composite_type_fingerprint(&valid),
            composite_type_fingerprint(&replacement)
        );
    }

    #[test]
    fn source_normalization_ignores_formatting_not_semantics() {
        assert_eq!(
            normalized_catalog_source("SELECT -- stable comment\n public.f($1)"),
            normalized_catalog_source(" select PUBLIC.f ( $1 ) ")
        );
        assert_ne!(
            normalized_catalog_source("SELECT public.f($1)"),
            normalized_catalog_source("SELECT public.g($1)")
        );
    }
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    use super::*;

    fn ensure_postgis_raster() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis")
            .expect("PostGIS must be available for raster catalog tests");
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis_raster")
            .expect("PostGIS Raster must be available for catalog tests");
    }

    #[pg_test]
    fn exact_installed_postgis_raster_catalog_resolves() {
        ensure_postgis_raster();
        // SAFETY: pg_test runs on the PostgreSQL backend main thread.
        let identity = unsafe { resolve_postgis_raster_catalog() }
            .expect("installed PostGIS Raster catalog must match the exact proof");
        assert_ne!(identity.raster_type_oid, pg_sys::InvalidOid);
        assert_ne!(identity.reclass_fn_oid, pg_sys::InvalidOid);
        assert_ne!(identity.summary_stats_fn_oid, pg_sys::InvalidOid);
        assert!(!identity.fingerprint_words.is_empty());
    }

    #[pg_test]
    fn same_signature_replacement_is_rejected() {
        ensure_postgis_raster();
        Spi::run(
            "CREATE OR REPLACE FUNCTION public.st_reclass( \
             rast public.raster, reclassexpr text, pixeltype text) \
             RETURNS public.raster LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS \
             'SELECT $1'",
        )
        .expect("test replacement must be accepted by PostgreSQL");
        // SAFETY: pg_test runs on the PostgreSQL backend main thread.
        let error = unsafe { resolve_postgis_raster_catalog() }
            .expect_err("same-signature wrapper replacement must fail exact proof");
        assert!(
            error.contains("wrapper body"),
            "unexpected proof error: {error}"
        );
    }
}

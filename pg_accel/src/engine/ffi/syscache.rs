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
const H3_FINGERPRINT_VERSION: u32 = 1;

/// Exact catalog identity accepted by the H3 acceleration path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H3CatalogIdentity {
    pub extension_oid: pg_sys::Oid,
    pub schema_oid: pg_sys::Oid,
    pub type_oid: pg_sys::Oid,
    pub parent_fn_oid: pg_sys::Oid,
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
    kind: u8,
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

fn validate_h3_function_shape(
    shape: &H3FunctionShape,
    schema_oid: pg_sys::Oid,
    type_oid: pg_sys::Oid,
) -> Result<(), String> {
    if shape.name != H3_PARENT_FUNCTION_NAME || shape.schema_oid != schema_oid {
        return Err(format!(
            "function OID {} is not extension-schema {H3_PARENT_FUNCTION_NAME}",
            u32::from(shape.fn_oid)
        ));
    }
    if shape.kind != pg_sys::PROKIND_FUNCTION {
        return Err("H3 parent catalog object must be an ordinary function".to_owned());
    }
    if shape.argument_types != [type_oid, pg_sys::INT4OID] || shape.return_type != type_oid {
        return Err("H3 parent function must have signature (h3index, int4) -> h3index".to_owned());
    }
    if shape.variadic_type != pg_sys::InvalidOid || shape.returns_set {
        return Err("H3 parent function must be scalar and nonvariadic".to_owned());
    }
    if !shape.strict {
        return Err("H3 parent function must be strict".to_owned());
    }
    if shape.volatility != pg_sys::PROVOLATILE_IMMUTABLE {
        return Err("H3 parent function must be immutable".to_owned());
    }
    if shape.parallel != pg_sys::PROPARALLEL_SAFE {
        return Err("H3 parent function must be parallel safe".to_owned());
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
        shape.source.as_bytes(),
        shape.binary.as_deref().unwrap_or_default().as_bytes(),
    ]);
    let [hash_low, hash_high] = u64_words(implementation_hash);
    vec![
        oid_word(shape.fn_oid),
        oid_word(shape.schema_oid),
        oid_word(shape.language_oid),
        u32_word(shape.tuple_xmin),
        u32_word(shape.tuple_block),
        i32::from(shape.tuple_offset),
        hash_low,
        hash_high,
    ]
}

unsafe fn extension_identity() -> Result<(pg_sys::Oid, pg_sys::Oid), String> {
    let extension_name = std::ffi::CString::new(H3_EXTENSION_NAME)
        .map_err(|_| "invalid H3 extension name".to_owned())?;
    let extension_oid = unsafe { pg_sys::get_extension_oid(extension_name.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return Err("extension h3 is not installed".to_owned());
    }

    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::EXTENSIONOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(extension_oid),
        )
    };
    if tuple.is_null() {
        return Err("extension h3 disappeared during catalog validation".to_owned());
    }
    let result = (|| unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_extension>();
        let name = std::ffi::CStr::from_ptr((*form).extname.data.as_ptr())
            .to_str()
            .map_err(|_| "extension h3 has an invalid catalog name".to_owned())?;
        if name != H3_EXTENSION_NAME {
            Err("extension OID resolved to an unexpected catalog row".to_owned())
        } else if !(*form).extrelocatable {
            Err("extension h3 must be relocatable".to_owned())
        } else if (*form).extnamespace == pg_sys::InvalidOid {
            Err("extension h3 has no installation schema".to_owned())
        } else {
            Ok((extension_oid, (*form).extnamespace))
        }
    })();
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
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
        let source = proc_text_attr(tuple, pg_sys::Anum_pg_proc_prosrc as i16)?
            .ok_or_else(|| format!("function OID {} has NULL prosrc", u32::from(fn_oid)))?;
        let binary = proc_text_attr(tuple, pg_sys::Anum_pg_proc_probin as i16)?;
        Ok(H3FunctionShape {
            fn_oid: (*form).oid,
            name,
            schema_oid: (*form).pronamespace,
            language_oid: (*form).prolang,
            kind: (*form).prokind as u8,
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
    let mut fingerprint_words = vec![u32_word(H3_FINGERPRINT_VERSION), oid_word(extension_oid)];
    fingerprint_words.extend(type_fingerprint(&type_proof.shape));
    fingerprint_words.extend(unsafe { validate_h3_parent_function(parent_fn_oid, type_oid)? });
    Ok(H3CatalogIdentity {
        extension_oid,
        schema_oid,
        type_oid,
        parent_fn_oid,
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
            kind: pg_sys::PROKIND_FUNCTION,
            strict: true,
            returns_set: false,
            volatility: pg_sys::PROVOLATILE_IMMUTABLE,
            parallel: pg_sys::PROPARALLEL_SAFE,
            variadic_type: pg_sys::InvalidOid,
            return_type: oid(50_001),
            argument_types: vec![oid(50_001), pg_sys::INT4OID],
            source: "h3_cell_to_parent".to_owned(),
            binary: Some("$libdir/h3".to_owned()),
            tuple_xmin: 12,
            tuple_block: 8,
            tuple_offset: 3,
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
        let mut invalid = valid.clone();
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
    }
}

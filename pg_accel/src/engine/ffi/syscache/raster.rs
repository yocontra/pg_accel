//! Replacement-sensitive catalog proof for supported PostGIS Raster overloads.

use super::{
    FromDatum, H3FunctionShape, H3TypeShape, fnv1a64, function_fingerprint, oid_word, pg_sys,
    postgres_error_requires_rethrow, read_h3_function_shape, read_h3_type_shape, type_fingerprint,
    u32_word, u64_words,
};
use pgrx::IntoDatum;

const POSTGIS_RASTER_EXTENSION_NAME: &str = "postgis_raster";
const POSTGIS_RASTER_TYPE_NAME: &str = "raster";
const POSTGIS_RASTER_SUMMARY_TYPE_NAME: &str = "summarystats";
const POSTGIS_RASTER_RECLASS_ARG_TYPE_NAME: &str = "reclassarg";
const POSTGIS_RASTER_LIBRARY: &str = "$libdir/postgis_raster-3";
const POSTGIS_RASTER_FINGERPRINT_VERSION: u32 = 2;

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
    pub as_wkb_fn_oid: pg_sys::Oid,
    pub rast_from_wkb_fn_oid: pg_sys::Oid,
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
    // SAFETY: `tuple` is the live, pinned EXTENSIONOID syscache entry passed by our
    // caller and `isnull` is a valid out-pointer for the duration of the call.
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
    // SAFETY: `isnull` was checked false above, so `datum` is a valid text datum
    // read from the pinned extension tuple.
    unsafe { String::from_datum(datum, false) }
        .ok_or_else(|| "could not decode PostGIS Raster extension version".to_owned())
}

unsafe fn postgis_raster_extension_shape() -> Result<PostgisRasterExtensionShape, String> {
    let name = std::ffi::CString::new(POSTGIS_RASTER_EXTENSION_NAME)
        .map_err(|_| "invalid PostGIS Raster extension name".to_owned())?;
    // SAFETY: `name` is a valid NUL-terminated CString and this catalog lookup runs
    // on the backend main thread per this function's contract.
    let extension_oid = unsafe { pg_sys::get_extension_oid(name.as_ptr(), true) };
    if extension_oid == pg_sys::InvalidOid {
        return Err("extension postgis_raster is not installed".to_owned());
    }
    // SAFETY: main-thread syscache lookup per this function's contract; the result
    // is NULL-checked below and released via ReleaseSysCache on every path.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::EXTENSIONOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(extension_oid),
        )
    };
    if tuple.is_null() {
        return Err("extension postgis_raster disappeared during validation".to_owned());
    }
    // SAFETY: `tuple` was NULL-checked above and stays pinned until the
    // ReleaseSysCache below; GETSTRUCT of an EXTENSIONOID entry is a
    // FormData_pg_extension whose extname is NUL-terminated NameData.
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
    // SAFETY: releases the pin taken by SearchSysCache1 exactly once, after all
    // tuple reads above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    result
}

unsafe fn find_type_oid(schema_oid: pg_sys::Oid, name: &str) -> Result<pg_sys::Oid, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "invalid catalog type name".to_owned())?;
    // SAFETY: `name` is a valid NUL-terminated CString outliving the call; the
    // TYPENAMENSP lookup runs on the backend main thread per this function's
    // contract.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::Datum::from(name.as_ptr()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "PostGIS Raster schema OID {} has no required type",
            u32::from(schema_oid)
        ));
    }
    // SAFETY: `tuple` was NULL-checked above and remains pinned; GETSTRUCT of a
    // TYPENAMENSP entry is a FormData_pg_type.
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    // SAFETY: releases the SearchSysCache2 pin exactly once, after the oid was
    // copied out.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_postgis_raster_type_shape(
    type_oid: pg_sys::Oid,
) -> Result<PostgisRasterTypeShape, String> {
    // SAFETY: main-thread TYPEOID syscache lookup per this function's contract;
    // NULL-checked below and released on every path.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::TYPEOID as ::core::ffi::c_int,
            pg_sys::ObjectIdGetDatum(type_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!("type OID {} does not exist", u32::from(type_oid)));
    }
    // SAFETY: `tuple` was NULL-checked above and stays pinned until the
    // ReleaseSysCache below; GETSTRUCT of a TYPEOID entry is a FormData_pg_type
    // whose typname is NUL-terminated NameData.
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
    // SAFETY: releases the SearchSysCache1 pin exactly once, after the closure
    // finished all tuple reads.
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
    // SAFETY: delegated syscache read with the same main-backend-thread contract
    // as this function.
    let base = unsafe { read_h3_type_shape(type_oid)? };
    if base.typtype != pg_sys::TYPTYPE_COMPOSITE || base.typrelid == pg_sys::InvalidOid {
        return Err(format!("type OID {} is not composite", u32::from(type_oid)));
    }
    // SAFETY: main-thread RELOID syscache lookup per this function's contract;
    // NULL-checked below and released after the field count is read.
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
    // SAFETY: `relation_tuple` was NULL-checked above and remains pinned;
    // GETSTRUCT of a RELOID entry is a FormData_pg_class.
    let field_count = unsafe {
        let form = pg_sys::GETSTRUCT(relation_tuple).cast::<pg_sys::FormData_pg_class>();
        usize::try_from((*form).relnatts).map_err(|_| {
            format!(
                "composite type OID {} has invalid relnatts",
                u32::from(type_oid)
            )
        })
    };
    // SAFETY: releases the RELOID pin exactly once, after relnatts was copied
    // out.
    unsafe { pg_sys::ReleaseSysCache(relation_tuple) };
    let field_count = field_count?;
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(field_count)
        .map_err(|_| "composite catalog field allocation failed".to_owned())?;
    for index in 0..field_count {
        let attno = i16::try_from(index + 1)
            .map_err(|_| "composite attribute number exceeds int16".to_owned())?;
        // SAFETY: main-thread ATTNUM syscache lookup per this function's contract,
        // keyed by the composite's validated typrelid; NULL-checked below and released
        // every iteration.
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
        // SAFETY: `tuple` was NULL-checked above and stays pinned until the
        // ReleaseSysCache below; GETSTRUCT of an ATTNUM entry is a
        // FormData_pg_attribute whose attname is NUL-terminated NameData.
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
        // SAFETY: releases this iteration's ATTNUM pin exactly once, after the
        // closure finished all tuple reads.
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
    // SAFETY: main-thread catalog lookup per this function's contract; returns a
    // palloc'd C string or NULL, checked below.
    let name = unsafe { pg_sys::get_namespace_name(schema_oid) };
    if name.is_null() {
        return Err(format!(
            "namespace OID {} does not exist",
            u32::from(schema_oid)
        ));
    }
    // SAFETY: `name` was NULL-checked above and points at the NUL-terminated
    // palloc'd string returned by get_namespace_name.
    let result = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| {
            format!(
                "namespace OID {} has an invalid name",
                u32::from(schema_oid)
            )
        });
    // SAFETY: `name` was palloc'd by get_namespace_name and is freed exactly once,
    // after its contents were copied into an owned String.
    unsafe { pg_sys::pfree(name.cast()) };
    result
}

unsafe fn find_function_oid(
    schema_oid: pg_sys::Oid,
    name: &str,
    argument_types: &[pg_sys::Oid],
) -> Result<pg_sys::Oid, String> {
    let name = std::ffi::CString::new(name).map_err(|_| "invalid function name".to_owned())?;
    // SAFETY: `argument_types.as_ptr()` is valid for `argument_types.len()` OIDs;
    // buildoidvector copies them into a fresh palloc'd oidvector.
    let vector = unsafe {
        pg_sys::buildoidvector(
            argument_types.as_ptr(),
            argument_types.len() as ::core::ffi::c_int,
        )
    };
    if vector.is_null() {
        return Err("could not build function signature".to_owned());
    }
    // SAFETY: `name` and `vector` are valid pointers outliving the call; the
    // PROCNAMEARGSNSP lookup runs on the backend main thread per this function's
    // contract.
    let tuple = unsafe {
        pg_sys::SearchSysCache3(
            pg_sys::SysCacheIdentifier::PROCNAMEARGSNSP as ::core::ffi::c_int,
            pg_sys::Datum::from(name.as_ptr()),
            pg_sys::Datum::from(vector),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    // SAFETY: `vector` was palloc'd by buildoidvector (NULL-checked above) and is
    // freed exactly once, before any early return below.
    unsafe { pg_sys::pfree(vector.cast()) };
    if tuple.is_null() {
        return Err("PostGIS Raster exact function overload is missing".to_owned());
    }
    // SAFETY: `tuple` was NULL-checked above and remains pinned; GETSTRUCT of a
    // PROCNAMEARGSNSP entry is a FormData_pg_proc.
    let oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_proc>();
        (*form).oid
    };
    // SAFETY: releases the SearchSysCache3 pin exactly once, after the oid was
    // copied out.
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
    // SAFETY: getExtensionOfObject only reads the pg_depend catalog; every caller
    // is one of the unsafe raster catalog resolvers whose contract pins execution
    // to the backend main thread.
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
    // SAFETY: delegated syscache reads with the same main-backend-thread contract
    // as this function.
    let extension = unsafe { postgis_raster_extension_shape()? };
    // SAFETY: delegated syscache read of `type_oid` under the same
    // main-backend-thread contract as this function.
    let shape = unsafe { read_postgis_raster_type_shape(type_oid)? };
    validate_postgis_raster_type_shape(&shape, extension.schema_oid)?;
    require_extension_member(
        extension.extension_oid,
        pg_sys::TypeRelationId,
        type_oid,
        "raster type",
    )?;
    // SAFETY: main-thread syscache read of the raster type's input function, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the raster type's output function, per
    // this function's contract.
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
    // SAFETY: delegated syscache reads with the same main-backend-thread contract
    // as this function.
    let extension = unsafe { postgis_raster_extension_shape()? };
    // SAFETY: main-thread syscache lookup in the proven extension schema, per this
    // function's contract.
    let raster_type_oid = unsafe { find_type_oid(extension.schema_oid, POSTGIS_RASTER_TYPE_NAME)? };
    let summary_stats_type_oid =
        // SAFETY: main-thread syscache lookup in the proven extension schema, per this
        // function's contract.
        unsafe { find_type_oid(extension.schema_oid, POSTGIS_RASTER_SUMMARY_TYPE_NAME)? };
    let reclass_arg_type_oid =
        // SAFETY: main-thread syscache lookup in the proven extension schema, per this
        // function's contract.
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
    // SAFETY: main-thread composite-type syscache read of the OID resolved above,
    // per this function's contract.
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
    // SAFETY: main-thread composite-type syscache read of the OID resolved above,
    // per this function's contract.
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
    // SAFETY: get_array_type is a syscache lookup running on the backend main
    // thread per this function's contract.
    let reclass_arg_array_oid = unsafe { pg_sys::get_array_type(reclass_arg_type_oid) };
    if reclass_arg_array_oid == pg_sys::InvalidOid {
        return Err("PostGIS Raster reclassarg has no array type".to_owned());
    }
    // SAFETY: main-thread catalog read of the proven extension schema OID, per
    // this function's contract.
    let schema_name = unsafe { namespace_name(extension.schema_oid)? };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let reclass_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_reclass",
            &[raster_type_oid, pg_sys::TEXTOID, pg_sys::TEXTOID],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let summary_stats_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_summarystats",
            &[raster_type_oid, pg_sys::INT4OID, pg_sys::BOOLOID],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let summary_stats_default_band_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_summarystats",
            &[raster_type_oid, pg_sys::BOOLOID],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let as_wkb_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_aswkb",
            &[raster_type_oid, pg_sys::BOOLOID],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let rast_from_wkb_fn_oid =
        unsafe { find_function_oid(extension.schema_oid, "st_rastfromwkb", &[pg_sys::BYTEAOID])? };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let reclass_variadic_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "st_reclass",
            &[raster_type_oid, reclass_arg_array_oid],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
    let reclass_impl_fn_oid = unsafe {
        find_function_oid(
            extension.schema_oid,
            "_st_reclass",
            &[raster_type_oid, reclass_arg_array_oid],
        )?
    };
    // SAFETY: main-thread overload resolution in the proven extension schema, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the pg_proc OID resolved above, per
    // this function's contract.
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
    // SAFETY: main-thread syscache read of the exact st_aswkb OID resolved and
    // extension-checked above; the helper owns its tuple pin.
    let as_wkb = unsafe { read_h3_function_shape(as_wkb_fn_oid)? };
    validate_raster_c_function(
        &as_wkb,
        extension.schema_oid,
        "st_aswkb",
        &[raster_type_oid, pg_sys::BOOLOID],
        pg_sys::BYTEAOID,
        true,
        1,
        "RASTER_asWKB",
    )?;
    if as_wkb.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster st_aswkb became variadic".to_owned());
    }
    // SAFETY: main-thread syscache read of the exact st_rastfromwkb OID resolved
    // and extension-checked above; the helper owns its tuple pin.
    let rast_from_wkb = unsafe { read_h3_function_shape(rast_from_wkb_fn_oid)? };
    validate_raster_c_function(
        &rast_from_wkb,
        extension.schema_oid,
        "st_rastfromwkb",
        &[pg_sys::BYTEAOID],
        raster_type_oid,
        true,
        0,
        "RASTER_fromWKB",
    )?;
    if rast_from_wkb.variadic_type != pg_sys::InvalidOid {
        return Err("PostGIS Raster st_rastfromwkb became variadic".to_owned());
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
        (as_wkb_fn_oid, "st_aswkb WKB exporter"),
        (rast_from_wkb_fn_oid, "st_rastfromwkb WKB importer"),
    ] {
        require_extension_member(
            extension.extension_oid,
            pg_sys::ProcedureRelationId,
            oid,
            label,
        )?;
    }

    // SAFETY: same main-backend-thread contract as this function; the validation
    // only performs syscache reads.
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
    fingerprint_words.extend(function_fingerprint(&as_wkb));
    fingerprint_words.extend(function_fingerprint(&rast_from_wkb));

    Ok(PostgisRasterCatalogIdentity {
        extension_oid: extension.extension_oid,
        schema_oid: extension.schema_oid,
        raster_type_oid,
        summary_stats_type_oid,
        reclass_fn_oid,
        summary_stats_fn_oid,
        summary_stats_default_band_fn_oid,
        as_wkb_fn_oid,
        rast_from_wkb_fn_oid,
        reclass_impl_fn_oid,
        summary_stats_impl_fn_oid,
        fingerprint_words,
    })
}

fn caught_error_message(caught: &pgrx::pg_sys::panic::CaughtError) -> String {
    use pgrx::pg_sys::panic::CaughtError;
    match caught {
        CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
            error.message().to_owned()
        }
        CaughtError::RustPanic { ereport, .. } => ereport.message().to_owned(),
    }
}

/// Convert an exact PostGIS internal raster Datum to owned external WKB.
///
/// The caller must pass a freshly resolved catalog identity. PostgreSQL ERRORs
/// raised by the catalog-proved C conversion function are caught and returned
/// so executor callers can issue one contextual hard ERROR without falling
/// through to native execution.
///
/// # Safety
/// `raster` must be a live non-NULL Datum of `identity.raster_type_oid` on the
/// PostgreSQL backend main thread.
pub unsafe fn postgis_raster_datum_to_wkb(
    identity: &PostgisRasterCatalogIdentity,
    raster: pg_sys::Datum,
) -> Result<Vec<u8>, String> {
    if identity.as_wkb_fn_oid == pg_sys::InvalidOid || raster.value() == 0 {
        return Err("PostGIS Raster WKB export received an invalid identity or Datum".to_owned());
    }
    pgrx::pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the identity proves strict st_aswkb(raster, boolean) with a
        // bytea result, and the caller proves the raster argument type.
        let bytea = unsafe {
            pg_sys::OidFunctionCall2Coll(
                identity.as_wkb_fn_oid,
                pg_sys::InvalidOid,
                raster,
                pg_sys::BoolGetDatum(false),
            )
        };
        if bytea.value() == 0 {
            return Err("catalog-proved PostGIS st_aswkb returned NULL".to_owned());
        }
        let original = bytea.cast_mut_ptr::<pg_sys::varlena>();
        // SAFETY: the catalog proof fixes the result type as a freshly
        // allocated bytea. Detoast supplies one flat readable value.
        let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
        if detoasted.is_null() {
            // SAFETY: exact RASTER_asWKB returned this palloc-owned bytea.
            unsafe { pg_sys::pfree(original.cast()) };
            return Err("could not detoast catalog-proved PostGIS raster WKB".to_owned());
        }
        // SAFETY: detoasted is a flat bytea valid for its reported payload.
        let len = unsafe { pgrx::varsize_any_exhdr(detoasted) };
        // SAFETY: detoasted is non-null and flat; vardata_any points to its
        // payload for the len bytes reported immediately above.
        let data = unsafe { pgrx::vardata_any(detoasted).cast::<u8>() };
        // SAFETY: data addresses the complete readable payload of length len;
        // to_vec copies it before either palloc allocation is freed.
        let wkb = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        if detoasted != original {
            // SAFETY: a distinct detoast result is palloc-owned by this call.
            unsafe { pg_sys::pfree(detoasted.cast()) };
        }
        // SAFETY: exact RASTER_asWKB returns a fresh palloc-owned bytea and the
        // payload has already been copied into Rust-owned storage.
        unsafe { pg_sys::pfree(original.cast()) };
        Ok(wkb)
    }))
    .catch_others(|caught| {
        use pgrx::pg_sys::panic::CaughtError;
        let (level, code) = match &caught {
            CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
                (error.level(), error.sql_error_code())
            }
            CaughtError::RustPanic { .. } => caught.rethrow(),
        };
        if postgres_error_requires_rethrow(level, code) {
            caught.rethrow();
        }
        Err(format!(
            "PostGIS st_aswkb raised ERROR: {}",
            caught_error_message(&caught)
        ))
    })
    .execute()
}

/// Convert reconstructed external WKB to an exact PostGIS internal raster
/// Datum through the catalog-proved importer.
///
/// # Safety
/// The identity must be freshly revalidated and this must run on the
/// PostgreSQL backend main thread in the output tuple's live memory context.
pub unsafe fn postgis_raster_datum_from_wkb(
    identity: &PostgisRasterCatalogIdentity,
    wkb: &[u8],
) -> Result<pg_sys::Datum, String> {
    if identity.rast_from_wkb_fn_oid == pg_sys::InvalidOid {
        return Err("PostGIS Raster WKB import has no catalog-proved function".to_owned());
    }
    pgrx::pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
        // Keep the temporary bytea allocation inside the PostgreSQL ERROR
        // boundary. The caller's output context is reset when this returns an
        // error, covering allocation or importer failures alike.
        let bytea = wkb
            .into_datum()
            .ok_or_else(|| "could not allocate reconstructed raster WKB bytea".to_owned())?;
        // SAFETY: the identity proves strict st_rastfromwkb(bytea) with the
        // exact PostGIS raster result type.
        let raster = unsafe {
            pg_sys::OidFunctionCall1Coll(identity.rast_from_wkb_fn_oid, pg_sys::InvalidOid, bytea)
        };
        // SAFETY: IntoDatum allocated this temporary bytea in the current
        // memory context and the strict importer returned normally.
        unsafe { pg_sys::pfree(bytea.cast_mut_ptr::<std::ffi::c_void>()) };
        if raster.value() == 0 {
            Err("catalog-proved PostGIS st_rastfromwkb returned NULL".to_owned())
        } else {
            Ok(raster)
        }
    }))
    .catch_others(|caught| {
        Err(format!(
            "PostGIS st_rastfromwkb raised ERROR: {}",
            caught_error_message(&caught)
        ))
    })
    .execute()
}

/// Resolve one function OID to the exact public PostGIS Raster overload.
///
/// # Safety
/// Must be called on the PostgreSQL backend main thread.
pub unsafe fn resolve_postgis_raster_function(
    fn_oid: pg_sys::Oid,
) -> Result<(PostgisRasterCatalogIdentity, PostgisRasterFunction), String> {
    // SAFETY: the caller upholds this function's main-backend-thread contract,
    // which is what resolve_postgis_raster_catalog requires for its syscache
    // reads.
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
    fn raster_wkb_converters_require_exact_c_implementations() {
        let raster_oid = oid(60_001);
        let mut exporter = valid_function(
            "st_aswkb",
            vec![raster_oid, pg_sys::BOOLOID],
            pg_sys::BYTEAOID,
            "RASTER_asWKB",
        );
        exporter.language_name = "c".to_owned();
        exporter.binary = Some(POSTGIS_RASTER_LIBRARY.to_owned());
        exporter.argument_defaults = 1;
        assert_eq!(
            validate_raster_c_function(
                &exporter,
                exporter.schema_oid,
                "st_aswkb",
                &exporter.argument_types,
                pg_sys::BYTEAOID,
                true,
                1,
                "RASTER_asWKB",
            ),
            Ok(())
        );
        let mut replaced_exporter = exporter;
        replaced_exporter.source = "test_aswkb".to_owned();
        assert!(
            validate_raster_c_function(
                &replaced_exporter,
                replaced_exporter.schema_oid,
                "st_aswkb",
                &replaced_exporter.argument_types,
                pg_sys::BYTEAOID,
                true,
                1,
                "RASTER_asWKB",
            )
            .is_err()
        );

        let mut importer = valid_function(
            "st_rastfromwkb",
            vec![pg_sys::BYTEAOID],
            raster_oid,
            "RASTER_fromWKB",
        );
        importer.language_name = "c".to_owned();
        importer.binary = Some(POSTGIS_RASTER_LIBRARY.to_owned());
        assert_eq!(
            validate_raster_c_function(
                &importer,
                importer.schema_oid,
                "st_rastfromwkb",
                &importer.argument_types,
                raster_oid,
                true,
                0,
                "RASTER_fromWKB",
            ),
            Ok(())
        );
        let mut replaced_importer = importer;
        replaced_importer.binary = Some("$libdir/impostor".to_owned());
        assert!(
            validate_raster_c_function(
                &replaced_importer,
                replaced_importer.schema_oid,
                "st_rastfromwkb",
                &replaced_importer.argument_types,
                raster_oid,
                true,
                0,
                "RASTER_fromWKB",
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

    fn assert_common_rejected(shape: &H3FunctionShape, valid: &H3FunctionShape) {
        assert!(
            validate_raster_function_common(
                shape,
                valid.schema_oid,
                &valid.name,
                &valid.language_name,
                &valid.argument_types,
                valid.return_type,
                valid.strict,
                valid.argument_defaults,
            )
            .is_err()
        );
    }

    #[test]
    fn raster_type_rejects_each_outer_contract_family() {
        let valid = valid_raster_type();

        let mut changed = valid.clone();
        changed.base.name = "not_raster".to_owned();
        assert!(validate_postgis_raster_type_shape(&changed, valid.base.schema_oid).is_err());

        let mut changed = valid.clone();
        changed.input_fn = pg_sys::InvalidOid;
        assert_eq!(
            validate_postgis_raster_type_shape(&changed, valid.base.schema_oid),
            Err("PostGIS raster type is missing input/output functions".to_owned())
        );

        let mut changed = valid.clone();
        changed.send_fn = oid(60_004);
        assert!(validate_postgis_raster_type_shape(&changed, valid.base.schema_oid).is_err());
    }

    #[test]
    fn common_function_validator_rejects_every_catalog_invariant() {
        let arguments = vec![pg_sys::INT4OID, pg_sys::BOOLOID];
        let valid = valid_function("f", arguments, pg_sys::INT8OID, "SELECT 1");
        assert_eq!(
            validate_raster_function_common(
                &valid,
                valid.schema_oid,
                &valid.name,
                &valid.language_name,
                &valid.argument_types,
                valid.return_type,
                true,
                0,
            ),
            Ok(())
        );

        type ShapeMutation = Box<dyn Fn(&mut H3FunctionShape)>;
        let mutations: Vec<ShapeMutation> = vec![
            Box::new(|shape| shape.name = "g".to_owned()),
            Box::new(|shape| shape.schema_oid = oid(99)),
            Box::new(|shape| shape.language_name = "plpgsql".to_owned()),
            Box::new(|shape| shape.kind = pg_sys::PROKIND_AGGREGATE),
            Box::new(|shape| shape.argument_types = vec![pg_sys::INT8OID]),
            Box::new(|shape| shape.return_type = pg_sys::BOOLOID),
            Box::new(|shape| shape.strict = false),
            Box::new(|shape| shape.argument_defaults = 1),
            Box::new(|shape| shape.security_definer = true),
            Box::new(|shape| shape.support_function = oid(91)),
            Box::new(|shape| shape.returns_set = true),
            Box::new(|shape| shape.volatility = pg_sys::PROVOLATILE_STABLE),
            Box::new(|shape| shape.parallel = pg_sys::PROPARALLEL_RESTRICTED),
        ];
        for mutate in mutations {
            let mut changed = valid.clone();
            mutate(&mut changed);
            assert_common_rejected(&changed, &valid);
        }
    }

    #[test]
    fn composite_validator_checks_base_shape_and_text_collation() {
        let mut valid = valid_summary_composite();
        valid.base.name = "textrecord".to_owned();
        valid.fields = vec![CompositeFieldShape {
            name: "label".to_owned(),
            type_oid: pg_sys::TEXTOID,
            typmod: -1,
            collation_oid: pg_sys::DEFAULT_COLLATION_OID,
            not_null: false,
            tuple_xmin: 31,
            tuple_block: 40,
            tuple_offset: 1,
        }];
        let expected = [("label", pg_sys::TEXTOID)];
        assert_eq!(
            validate_composite_type_shape(&valid, valid.base.schema_oid, "textrecord", &expected,),
            Ok(())
        );

        let mut changed = valid.clone();
        changed.base.typisdefined = false;
        assert!(
            validate_composite_type_shape(
                &changed,
                valid.base.schema_oid,
                "textrecord",
                &expected,
            )
            .is_err()
        );

        let mut changed = valid;
        changed.fields[0].collation_oid = pg_sys::InvalidOid;
        assert_eq!(
            validate_composite_type_shape(
                &changed,
                changed.base.schema_oid,
                "textrecord",
                &expected,
            ),
            Err("PostGIS Raster textrecord.label has a noncanonical collation".to_owned())
        );
    }

    #[test]
    fn plpgsql_wrapper_requires_exact_variadic_shape_and_body() {
        let raster_oid = oid(60_001);
        let array_oid = oid(60_031);
        let variadic_oid = oid(60_030);
        let source = "BEGIN -- delegate\n RETURN public._st_reclass($1, VARIADIC $2); END";
        let mut valid = valid_function(
            "st_reclass",
            vec![raster_oid, array_oid],
            raster_oid,
            source,
        );
        valid.language_name = "plpgsql".to_owned();
        valid.variadic_type = variadic_oid;
        assert_eq!(
            validate_raster_plpgsql_function(
                &valid,
                valid.schema_oid,
                "st_reclass",
                &valid.argument_types,
                raster_oid,
                variadic_oid,
                " begin return PUBLIC._ST_RECLASS($1,variadic $2); end ",
            ),
            Ok(())
        );

        let mut changed = valid.clone();
        changed.variadic_type = oid(99);
        assert!(
            validate_raster_plpgsql_function(
                &changed,
                changed.schema_oid,
                "st_reclass",
                &changed.argument_types,
                raster_oid,
                variadic_oid,
                source,
            )
            .is_err()
        );

        let mut changed = valid.clone();
        changed.binary = Some(POSTGIS_RASTER_LIBRARY.to_owned());
        assert!(
            validate_raster_plpgsql_function(
                &changed,
                changed.schema_oid,
                "st_reclass",
                &changed.argument_types,
                raster_oid,
                variadic_oid,
                source,
            )
            .is_err()
        );

        let mut changed = valid;
        changed.source = "BEGIN RETURN NULL; END".to_owned();
        assert_eq!(
            validate_raster_plpgsql_function(
                &changed,
                changed.schema_oid,
                "st_reclass",
                &changed.argument_types,
                raster_oid,
                variadic_oid,
                source,
            ),
            Err("PostGIS Raster st_reclass PL/pgSQL wrapper body is noncanonical".to_owned())
        );
    }

    fn invalid_catalog_identity() -> PostgisRasterCatalogIdentity {
        PostgisRasterCatalogIdentity {
            extension_oid: pg_sys::InvalidOid,
            schema_oid: pg_sys::InvalidOid,
            raster_type_oid: pg_sys::InvalidOid,
            summary_stats_type_oid: pg_sys::InvalidOid,
            reclass_fn_oid: pg_sys::InvalidOid,
            summary_stats_fn_oid: pg_sys::InvalidOid,
            summary_stats_default_band_fn_oid: pg_sys::InvalidOid,
            as_wkb_fn_oid: pg_sys::InvalidOid,
            rast_from_wkb_fn_oid: pg_sys::InvalidOid,
            reclass_impl_fn_oid: pg_sys::InvalidOid,
            summary_stats_impl_fn_oid: pg_sys::InvalidOid,
            fingerprint_words: Vec::new(),
        }
    }

    #[test]
    fn wkb_conversion_rejects_invalid_identity_before_backend_calls() {
        let identity = invalid_catalog_identity();
        // SAFETY: each call fails its argument guard before any PostgreSQL API use.
        let export = unsafe { postgis_raster_datum_to_wkb(&identity, pg_sys::Datum::from(0usize)) };
        assert_eq!(
            export,
            Err("PostGIS Raster WKB export received an invalid identity or Datum".to_owned())
        );
        // SAFETY: the invalid function OID is rejected before bytea allocation.
        let import = unsafe { postgis_raster_datum_from_wkb(&identity, &[]) };
        assert!(matches!(
            import,
            Err(message) if message == "PostGIS Raster WKB import has no catalog-proved function"
        ));
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
        assert_ne!(identity.as_wkb_fn_oid, pg_sys::InvalidOid);
        assert_ne!(identity.rast_from_wkb_fn_oid, pg_sys::InvalidOid);
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

    #[pg_test]
    fn exporter_same_signature_replacement_is_rejected() {
        ensure_postgis_raster();
        Spi::run(
            "CREATE OR REPLACE FUNCTION public.st_aswkb( \
               public.raster, outasin boolean DEFAULT false) \
             RETURNS bytea LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS \
             $$ SELECT decode('', 'hex') $$",
        )
        .expect("replace exact WKB exporter signature");
        // SAFETY: pg_test runs on the PostgreSQL backend main thread.
        let error = unsafe { resolve_postgis_raster_catalog() }
            .expect_err("same-signature exporter replacement must fail exact proof");
        assert!(
            error.contains("st_aswkb"),
            "unexpected proof error: {error}"
        );
    }

    #[pg_test]
    fn importer_same_signature_replacement_is_rejected() {
        ensure_postgis_raster();
        Spi::run(
            "CREATE OR REPLACE FUNCTION public.st_rastfromwkb(bytea) \
             RETURNS public.raster LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS \
             $$ SELECT NULL::public.raster $$",
        )
        .expect("replace exact WKB importer signature");
        // SAFETY: pg_test runs on the PostgreSQL backend main thread.
        let error = unsafe { resolve_postgis_raster_catalog() }
            .expect_err("same-signature importer replacement must fail exact proof");
        assert!(
            error.contains("st_rastfromwkb"),
            "unexpected proof error: {error}"
        );
    }

    #[pg_test]
    fn malformed_wkb_error_is_caught_and_backend_remains_usable() {
        ensure_postgis_raster();
        Spi::run(
            "CREATE TEMP TABLE pgaccel_raster_import_error( \
               state text NOT NULL, message text NOT NULL) ON COMMIT DROP; \
             DO $pgaccel$ \
             BEGIN \
               BEGIN \
                 PERFORM ST_RastFromWKB(decode('00', 'hex')); \
               EXCEPTION WHEN OTHERS THEN \
                 INSERT INTO pgaccel_raster_import_error VALUES (SQLSTATE, SQLERRM); \
               END; \
             END \
             $pgaccel$",
        )
        .expect("capture exact malformed WKB SQL error");
        let (sqlstate, sqlerrm) = Spi::connect(|client| {
            let mut rows = client
                .select(
                    "SELECT state, message FROM pgaccel_raster_import_error",
                    None,
                    &[],
                )
                .expect("read exact malformed WKB SQL error");
            let row = rows.next().expect("malformed WKB must raise SQL ERROR");
            (
                row.get::<String>(1)
                    .expect("read malformed WKB SQLSTATE")
                    .expect("malformed WKB SQLSTATE is non-NULL"),
                row.get::<String>(2)
                    .expect("read malformed WKB SQLERRM")
                    .expect("malformed WKB SQLERRM is non-NULL"),
            )
        });
        assert_eq!(sqlstate, "XX000");
        assert_eq!(sqlerrm, "rt_raster_from_wkb: wkb size (1) < min size (61)");
        // SAFETY: pg_test runs on the PostgreSQL backend main thread.
        let identity = unsafe { resolve_postgis_raster_catalog() }
            .expect("resolve exact catalog for malformed WKB proof");
        // SAFETY: pg_test runs on the backend main thread and identity is the
        // freshly proved exact importer. Malformed external bytes are a valid
        // hostile input to its public bytea boundary.
        let import_error = unsafe { postgis_raster_datum_from_wkb(&identity, &[0]) }
            .expect_err("malformed WKB ERROR must become a Rust error");
        assert_eq!(
            import_error,
            "PostGIS st_rastfromwkb raised ERROR: \
             rt_raster_from_wkb: wkb size (1) < min size (61)"
        );
        assert_eq!(
            Spi::get_one::<i32>("SELECT 42").expect("backend remains usable"),
            Some(42)
        );
    }
}

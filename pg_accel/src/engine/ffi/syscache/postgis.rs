//! Exact extension-owned PostGIS catalog proof.

use pgrx::pg_sys;

use super::{
    H3FunctionShape, find_exact_function, function_fingerprint, named_extension_identity, oid_word,
    read_h3_function_shape, u32_word,
};

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
    // SAFETY: TYPENAMENSP expects the NUL-terminated geometry name and validated
    // extension schema OID; a non-null row remains pinned until release below.
    let tuple = unsafe {
        pg_sys::SearchSysCache2(
            pg_sys::SysCacheIdentifier::TYPENAMENSP as ::core::ffi::c_int,
            pg_sys::Datum::from(type_name.as_ptr()),
            pg_sys::ObjectIdGetDatum(schema_oid),
        )
    };
    if tuple.is_null() {
        return Err(format!(
            "extension postgis has no geometry type in schema OID {}",
            u32::from(schema_oid)
        ));
    }
    // SAFETY: tuple is a live pinned pg_type row; its OID is copied before the
    // matching syscache pin is released.
    let type_oid = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_type>();
        (*form).oid
    };
    // SAFETY: releases exactly the non-null TYPENAMENSP pin acquired above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Ok(type_oid)
}

unsafe fn read_postgis_type_shape(type_oid: pg_sys::Oid) -> Result<PostgisTypeShape, String> {
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
    // SAFETY: tuple is a live pinned pg_type row; GETSTRUCT, NameData, tuple
    // header, and item-pointer fields are copied before release.
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
    // SAFETY: result owns all copied row data, so the matching TYPEOID pin can be released.
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
    // SAFETY: fn_oid is inspected under a PROCOID pin held wholly by the helper.
    let shape = unsafe { read_h3_function_shape(fn_oid)? };
    validate_postgis_function_shape(&shape, schema_oid, contract)?;
    // SAFETY: fn_oid is a catalog OID and dependency lookup occurs on the
    // caller-guaranteed backend thread.
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
    // SAFETY: resolution runs on the backend thread and the helper owns the
    // complete lifetime of its extension syscache pin.
    let (extension_oid, schema_oid) =
        unsafe { named_extension_identity(POSTGIS_EXTENSION_NAME, Some(false))? };
    // SAFETY: schema_oid came from the validated PostGIS extension row.
    let geometry_type_oid = unsafe { find_postgis_geometry_type(schema_oid)? };
    // SAFETY: geometry_type_oid is inspected under a TYPEOID pin owned by the helper.
    let type_shape = unsafe { read_postgis_type_shape(geometry_type_oid)? };
    validate_postgis_type_shape(&type_shape, schema_oid)?;
    // SAFETY: geometry_type_oid is a catalog OID and dependency lookup occurs
    // on the backend thread.
    if unsafe { pg_sys::getExtensionOfObject(pg_sys::TypeRelationId, geometry_type_oid) }
        != extension_oid
    {
        return Err(format!(
            "type OID {} is not owned by extension postgis",
            u32::from(geometry_type_oid)
        ));
    }

    // SAFETY: schema_oid is the validated PostGIS schema and the helper owns its
    // signature oidvector and syscache pin lifetimes.
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
    // SAFETY: all OIDs belong to the same validated PostGIS extension identity;
    // validation owns each temporary syscache pin.
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
        // SAFETY: every fn_oid is copied from the validated geometry pg_type row
        // and each contract pins its exact expected signature.
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
    // SAFETY: typanalyze was copied from the validated geometry pg_type row and
    // the exact analysis contract is checked under a helper-owned pin.
    function_shapes.push(unsafe {
        validate_postgis_function(
            extension_oid,
            schema_oid,
            type_shape.typanalyze,
            analyze_contract,
        )?
    });

    let binary_args = [geometry_type_oid, geometry_type_oid];
    // SAFETY: schema_oid and both argument OIDs belong to the validated PostGIS
    // identity; the helper bounds and owns its temporary oidvector.
    let intersects_fn_oid =
        unsafe { find_exact_function(schema_oid, "st_intersects", &binary_args)? };
    // SAFETY: the exact binary geometry signature is backed by validated catalog OIDs.
    let contains_fn_oid = unsafe { find_exact_function(schema_oid, "st_contains", &binary_args)? };
    // SAFETY: the exact binary geometry signature is backed by validated catalog OIDs.
    let within_fn_oid = unsafe { find_exact_function(schema_oid, "st_within", &binary_args)? };
    let dwithin_args = [geometry_type_oid, geometry_type_oid, pg_sys::FLOAT8OID];
    // SAFETY: the exact geometry, geometry, float8 signature uses validated OIDs.
    let dwithin_fn_oid = unsafe { find_exact_function(schema_oid, "st_dwithin", &dwithin_args)? };
    // SAFETY: the exact binary geometry signature is backed by validated catalog OIDs.
    let distance_fn_oid = unsafe { find_exact_function(schema_oid, "st_distance", &binary_args)? };
    // SAFETY: the exact unary geometry signature is backed by the validated type OID.
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
        // SAFETY: each fn_oid was resolved by exact signature in the validated
        // PostGIS schema and contract validation owns its syscache-pin lifetime.
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

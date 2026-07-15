//! Execution-time revalidation of replacement-sensitive raster catalog proof.

use crate::engine::ffi::syscache::{PostgisRasterCatalogIdentity, resolve_postgis_raster_catalog};

use super::{RasterQuerySpec, RasterSpecError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterCatalogRevalidationError {
    InvalidSpec(RasterSpecError),
    Lookup(String),
    FingerprintChanged,
    RasterTypeChanged { expected: u32, current: u32 },
    FunctionChanged { expected: u32, current: u32 },
    AsWkbFunctionChanged { expected: u32, current: u32 },
    RastFromWkbFunctionChanged { expected: u32, current: u32 },
}

impl std::fmt::Display for RasterCatalogRevalidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RasterCatalogRevalidationError {}

fn validate_identity(
    spec: &RasterQuerySpec,
    current: &PostgisRasterCatalogIdentity,
) -> Result<(), RasterCatalogRevalidationError> {
    spec.validate()
        .map_err(RasterCatalogRevalidationError::InvalidSpec)?;
    if spec.catalog_fingerprint.as_ref() != current.fingerprint_words.as_slice() {
        return Err(RasterCatalogRevalidationError::FingerprintChanged);
    }
    let current_raster_type = u32::from(current.raster_type_oid);
    if spec.raster_type_oid != current_raster_type {
        return Err(RasterCatalogRevalidationError::RasterTypeChanged {
            expected: spec.raster_type_oid,
            current: current_raster_type,
        });
    }
    let current_function = u32::from(current.reclass_fn_oid);
    if spec.function_oid != current_function {
        return Err(RasterCatalogRevalidationError::FunctionChanged {
            expected: spec.function_oid,
            current: current_function,
        });
    }
    let current_as_wkb = u32::from(current.as_wkb_fn_oid);
    if spec.as_wkb_fn_oid != current_as_wkb {
        return Err(RasterCatalogRevalidationError::AsWkbFunctionChanged {
            expected: spec.as_wkb_fn_oid,
            current: current_as_wkb,
        });
    }
    let current_rast_from_wkb = u32::from(current.rast_from_wkb_fn_oid);
    if spec.rast_from_wkb_fn_oid != current_rast_from_wkb {
        return Err(RasterCatalogRevalidationError::RastFromWkbFunctionChanged {
            expected: spec.rast_from_wkb_fn_oid,
            current: current_rast_from_wkb,
        });
    }
    Ok(())
}

/// Resolve the entire current PostGIS Raster catalog identity and require an
/// exact match with the planned Reclass-only RQS2 fingerprint.
///
/// # Safety
/// Must run on the PostgreSQL backend main thread on the first tuple request
/// after BeginCustomScan and every ReScanCustomScan, before resident input is
/// borrowed.
pub unsafe fn revalidate_raster_catalog(
    spec: &RasterQuerySpec,
) -> Result<PostgisRasterCatalogIdentity, RasterCatalogRevalidationError> {
    // SAFETY: this function inherits the backend-thread contract and performs
    // the catalog resolution before any resident raster input is borrowed.
    let current = unsafe { resolve_postgis_raster_catalog() }
        .map_err(RasterCatalogRevalidationError::Lookup)?;
    validate_identity(spec, &current)?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgrx::pg_sys;

    use crate::engine::raster::parse_exact_reclass_spec;

    fn identity() -> PostgisRasterCatalogIdentity {
        PostgisRasterCatalogIdentity {
            extension_oid: pg_sys::Oid::from(1),
            schema_oid: pg_sys::Oid::from(2),
            raster_type_oid: pg_sys::Oid::from(3),
            summary_stats_type_oid: pg_sys::Oid::from(4),
            reclass_fn_oid: pg_sys::Oid::from(5),
            summary_stats_fn_oid: pg_sys::Oid::from(6),
            summary_stats_default_band_fn_oid: pg_sys::Oid::from(7),
            as_wkb_fn_oid: pg_sys::Oid::from(8),
            rast_from_wkb_fn_oid: pg_sys::Oid::from(9),
            reclass_impl_fn_oid: pg_sys::Oid::from(10),
            summary_stats_impl_fn_oid: pg_sys::Oid::from(11),
            fingerprint_words: vec![10, 11],
        }
    }

    fn spec() -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 20,
            raster_attno: 1,
            raster_type_oid: 3,
            function_oid: 5,
            as_wkb_fn_oid: 8,
            rast_from_wkb_fn_oid: 9,
            catalog_fingerprint: vec![10, 11].into_boxed_slice(),
            reclass: parse_exact_reclass_spec("0:1", "8BUI").expect("test mapping is canonical"),
        }
    }

    #[test]
    fn selected_reclass_revalidates_against_exact_identity() {
        assert_eq!(validate_identity(&spec(), &identity()), Ok(()));
    }

    #[test]
    fn fingerprint_type_and_function_replacements_fail_closed() {
        let identity = identity();
        let mut changed = identity.clone();
        changed.fingerprint_words[1] += 1;
        assert_eq!(
            validate_identity(&spec(), &changed),
            Err(RasterCatalogRevalidationError::FingerprintChanged)
        );

        let mut wrong_type = spec();
        wrong_type.raster_type_oid = 30;
        assert!(matches!(
            validate_identity(&wrong_type, &identity),
            Err(RasterCatalogRevalidationError::RasterTypeChanged { .. })
        ));

        let mut wrong_function = spec();
        wrong_function.function_oid = 60;
        assert!(matches!(
            validate_identity(&wrong_function, &identity),
            Err(RasterCatalogRevalidationError::FunctionChanged { .. })
        ));

        let mut wrong_export = spec();
        wrong_export.as_wkb_fn_oid = 80;
        assert!(matches!(
            validate_identity(&wrong_export, &identity),
            Err(RasterCatalogRevalidationError::AsWkbFunctionChanged { .. })
        ));

        let mut wrong_import = spec();
        wrong_import.rast_from_wkb_fn_oid = 90;
        assert!(matches!(
            validate_identity(&wrong_import, &identity),
            Err(RasterCatalogRevalidationError::RastFromWkbFunctionChanged { .. })
        ));
    }

    #[test]
    fn malformed_planned_spec_fails_before_catalog_comparison() {
        let mut malformed = spec();
        malformed.relation_oid = 0;
        assert!(matches!(
            validate_identity(&malformed, &identity()),
            Err(RasterCatalogRevalidationError::InvalidSpec(
                RasterSpecError::MissingRelationOid
            ))
        ));
    }
}

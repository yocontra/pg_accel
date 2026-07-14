//! Quarantined generic `PostGIS` raster adapter.
//!
//! Raster overloads cannot be admitted by schema and function name. The
//! dedicated raster planner proves exact extension ownership, argument and
//! result types, implementation symbols, and catalog fingerprints instead.

use crate::engine::registry::ExtensionAdapter;

/// Preserve the adapter identifier for diagnostics without exposing any
/// function to the generic name-based registry.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis_raster",
        functions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_raster_surface_is_empty() {
        let adapter = adapter();
        assert_eq!(adapter.name, "postgis_raster");
        assert!(adapter.functions.is_empty());
    }

    #[test]
    fn no_approved_or_unsupported_name_can_enter_the_generic_registry() {
        let registered: Vec<&str> = adapter()
            .functions
            .iter()
            .map(|function| function.name)
            .collect();
        for name in [
            "st_reclass",
            "st_summarystats",
            "st_mapalgebra",
            "st_clip",
            "st_resample",
            "st_slope",
            "st_aspect",
            "st_hillshade",
            "st_value",
        ] {
            assert!(!registered.contains(&name));
        }
    }
}

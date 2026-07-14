//! Phase 2 dispatch-correctness integration tests (Agent 2B).
//!
//! These `#[pg_test]`s exercise the Rust dispatch/extractor layer against
//! **real PostGIS** output rather than the in-crate test builders, so a wrong
//! decode table cannot pass by mirroring its own bug.
//!
//! The headline test builds actual PostGIS rasters via `ST_MakeEmptyRaster` +
//! `ST_AddBand` + `ST_SetValue`, fetches their canonical WKB via
//! `ST_AsBinary(raster)`, and confirms our parser
//! (`adapters::extractors::raster`) decodes the band pixel type and pixel
//! values for 8BUI, 32BF, and 16BSI. This is the regression pin for the P0
//! shifted-`rt_pixtype`-table bug (real 8BUI used to decode as UInt16, 32BF
//! was rejected outright).
//!
//! All tests skip cleanly (with a `NOTICE`) when `postgis_raster` is not
//! installable in the pgrx test DB — same idiom as the h3 dispatch tests.

// Wired into the build from `adapters/mod.rs` (a file this agent owns) via a
// `#[path]` module so `src/tests/mod.rs` — owned by another agent this phase —
// is not touched. pgrx `#[pg_test]` discovery is inventory-based, so the module
// location does not affect test collection.
#[cfg(any(test, feature = "pg_test"))]
#[allow(clippy::unwrap_used)]
// NOTE: module MUST be named `tests` — the pgrx test runner hardcodes SQL
// schema `tests` when invoking #[pg_test] wrappers; pgrx emits CREATE SCHEMA
// IF NOT EXISTS so this coexists with the other `mod tests` files.
#[pgrx::pg_schema]
mod tests {
    use crate::adapters::extractors::raster::{PixelType, parse_resident_raster};
    use pgrx::prelude::*;

    /// `CREATE EXTENSION IF NOT EXISTS <name> CASCADE`, returning whether the
    /// extension ended up installed. Mirrors `tests/mod.rs::ensure_extension`.
    fn ensure_extension(name: &str) -> bool {
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    /// Evaluate a SQL raster expression and return its canonical PostGIS WKB
    /// (`ST_AsBinary(raster)` → `bytea`).
    fn raster_wkb(raster_expr: &str) -> Vec<u8> {
        let sql = format!("SELECT ST_AsBinary({raster_expr})");
        Spi::get_one::<Vec<u8>>(&sql)
            .expect("ST_AsBinary query should succeed")
            .expect("ST_AsBinary should not be NULL")
    }

    /// Build a 2x2 single-band raster of `pixeltype`, initialised to
    /// `initial`, with pixel (col=1,row=1) set to `v00` and (col=2,row=2) set
    /// to `v11`. Returns the canonical WKB. Pixel (x=col, y=row) is 1-based in
    /// PostGIS; our parser yields row-major `[r0c0, r0c1, r1c0, r1c1]`.
    fn build_2x2(pixeltype: &str, initial: f64, v00: f64, v11: f64) -> Vec<u8> {
        let expr = format!(
            "ST_SetValue(\
               ST_SetValue(\
                 ST_AddBand(\
                   ST_MakeEmptyRaster(2, 2, 0, 0, 1, -1, 0, 0, 0), \
                   '{pixeltype}'::text, {initial}, NULL), \
                 1, 1, 1, {v00}), \
               1, 2, 2, {v11})"
        );
        raster_wkb(&expr)
    }

    /// 8BUI: real unsigned 8-bit band must decode as `UInt8` (NOT UInt16, the
    /// pre-fix shifted-table result) with exact pixel values.
    #[pg_test]
    fn raster_8bui_decodes_via_real_postgis() {
        if !ensure_extension("postgis_raster") {
            pgrx::notice!("postgis_raster not installable; skipping raster 8BUI decode test");
            return;
        }
        let wkb = build_2x2("8BUI", 0.0, 10.0, 250.0);
        let parsed = parse_resident_raster(&wkb).expect("resident raster should parse");
        let band = &parsed.bands[0];
        assert_eq!(
            band.pixel_type,
            PixelType::UInt8,
            "8BUI must decode as UInt8"
        );
        assert_eq!(band.pixels, [10, 0, 0, 250], "8BUI pixel bytes");
    }

    /// 32BF: real 32-bit float band must decode as `Float32` (the pre-fix
    /// table rejected code 9 entirely) and round-trip a fractional value.
    #[pg_test]
    fn raster_32bf_decodes_via_real_postgis() {
        if !ensure_extension("postgis_raster") {
            pgrx::notice!("postgis_raster not installable; skipping raster 32BF decode test");
            return;
        }
        let wkb = build_2x2("32BF", 0.0, 3.5, -1.25);
        let parsed = parse_resident_raster(&wkb).expect("resident raster should parse");
        let band = &parsed.bands[0];
        assert_eq!(
            band.pixel_type,
            PixelType::Float32,
            "32BF must decode as Float32"
        );
        let px: Vec<f32> = band
            .pixels
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte pixel")))
            .collect();
        assert_eq!(px.len(), 4, "2x2 raster has 4 pixels");
        assert!((px[0] - 3.5).abs() < 1e-6, "px[0] == 3.5, got {}", px[0]);
        assert!(
            (px[3] - (-1.25)).abs() < 1e-6,
            "px[3] == -1.25, got {}",
            px[3]
        );
    }

    /// 16BSI: real signed 16-bit band must decode as `Int16` and preserve a
    /// negative value (pre-fix, code 5 decoded as Int32).
    #[pg_test]
    fn raster_16bsi_decodes_via_real_postgis() {
        if !ensure_extension("postgis_raster") {
            pgrx::notice!("postgis_raster not installable; skipping raster 16BSI decode test");
            return;
        }
        let wkb = build_2x2("16BSI", 0.0, -1000.0, 32000.0);
        let parsed = parse_resident_raster(&wkb).expect("resident raster should parse");
        let band = &parsed.bands[0];
        assert_eq!(
            band.pixel_type,
            PixelType::Int16,
            "16BSI must decode as Int16"
        );
        let px: Vec<i16> = band
            .pixels
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes(bytes.try_into().expect("two-byte pixel")))
            .collect();
        assert_eq!(px, [-1000, 0, 0, 32000], "16BSI pixel values");
    }
}

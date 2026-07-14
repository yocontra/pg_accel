//! Strict semantic contract for resident PostGIS Raster execution.

mod codec;
mod spec;

pub use codec::{RASTER_QUERY_SPEC_MAX_WORDS, RASTER_QUERY_SPEC_WIRE_MAGIC, RasterSpecCodecError};
pub use spec::{
    MAX_RASTER_CATALOG_FINGERPRINT_WORDS, MAX_RASTER_RECLASS_RULES, RasterBorrowContract,
    RasterCardinalityContract, RasterExplainSpec, RasterOperation, RasterOrderContract,
    RasterOverload, RasterPixelType, RasterQuerySpec, RasterReclassParseError, RasterReclassRule,
    RasterReclassSemantics, RasterReclassSpec, RasterRelationContract, RasterRevalidationContract,
    RasterScanContract, RasterSpecError, parse_exact_reclass_spec,
};

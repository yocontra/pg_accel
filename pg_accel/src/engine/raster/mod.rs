//! Strict semantic contract for resident PostGIS Raster execution.

mod catalog;
mod codec;
mod cost;
mod execution;
mod planner;
mod spec;

pub use catalog::{RasterCatalogRevalidationError, revalidate_raster_catalog};
pub use codec::{RASTER_QUERY_SPEC_MAX_WORDS, RASTER_QUERY_SPEC_WIRE_MAGIC, RasterSpecCodecError};
pub use cost::{
    RasterCost, RasterCostGate, RasterCostInput, RasterResidentWork, RasterWorkEstimate,
    estimate_raster_cost,
};
pub use execution::{
    RasterExecutionAccounting, RasterExecutionError, RasterExecutionLayout,
    RasterExecutionPreflight, RasterExecutionSizing, RasterExecutionSnapshot,
    RasterReconstructedOutput, preflight_raster_execution, reconstruct_raster_output,
    size_empty_raster_execution, size_raster_execution,
};
pub use planner::{
    RasterCallShape, RasterCatalogContract, RasterCommandShape, RasterPlannerCandidate,
    RasterPlannerDecline, RasterQueryFeatures, RasterRelationShape, RasterShapeInput,
    build_raster_query_spec,
};
pub use spec::{
    MAX_RASTER_CATALOG_FINGERPRINT_WORDS, MAX_RASTER_RECLASS_RULES, RasterBorrowContract,
    RasterCardinalityContract, RasterExplainSpec, RasterOrderContract, RasterPixelType,
    RasterQuerySpec, RasterReclassParseError, RasterReclassRule, RasterReclassSemantics,
    RasterReclassSpec, RasterRelationContract, RasterRevalidationContract, RasterScanContract,
    RasterSpecError, parse_exact_reclass_spec,
};

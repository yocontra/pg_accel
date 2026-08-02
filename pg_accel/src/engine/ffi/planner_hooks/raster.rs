//! Exact RQS2 resident raster planner admission.
//!
//! This module recognizes replacement-sensitive PostGIS Raster calls and
//! admits only the catalog-proved three-argument reclass subset after exact
//! resident metadata and device work gates. The `pg_test` feature additionally
//! exposes a scoped forced path for fault and executor boundary proof.

use pgrx::FromDatum;
use pgrx::pg_sys::{self, Node, NodeTag};

use crate::engine::cost::{PgCost, device_limits};
use crate::engine::ffi::syscache::{PostgisRasterCatalogIdentity, resolve_postgis_raster_catalog};
use crate::engine::raster::{
    RasterCallShape, RasterCatalogContract, RasterCommandShape, RasterCostGate, RasterCostInput,
    RasterPlannerCandidate, RasterPlannerDecline, RasterQueryFeatures, RasterRelationShape,
    RasterResidentWork, RasterShapeInput, RasterWorkEstimate, build_raster_query_spec,
    estimate_raster_cost,
};
use crate::engine::residency::{ResidentColumnView, with_resident_column};
use crate::engine::stats;

use super::{RejectionReason, add_gpu_path_with_resident_proof, custom_scan, find_cheapest_path};
use crate::engine::executor::raster::RasterExecPlan;

const RECLASS_NAME: &str = "st_reclass";
const SUMMARY_STATS_NAME: &str = "st_summarystats";
const NORMAL_RASTER_MIN_SELECTED_PIXELS: u64 = 10_134_528;
const NORMAL_RASTER_MAX_SELECTED_PIXELS: u64 = 63_340_224;

fn list_len(list: *mut pg_sys::List) -> usize {
    if list.is_null() {
        0
    } else {
        // SAFETY: every caller passes a PostgreSQL-owned List from the live
        // planner tree; the null branch above excludes NIL/null storage.
        usize::try_from(unsafe { pg_sys::list_length(list) }).unwrap_or(usize::MAX)
    }
}

unsafe fn list_item<T>(list: *mut pg_sys::List, index: usize) -> Option<*mut T> {
    if index >= list_len(list) {
        return None;
    }
    let index = i32::try_from(index).ok()?;
    // SAFETY: list_len proved `index` is within this planner-owned List and
    // PostgreSQL retains its cells for the current planner context.
    let item = unsafe { pg_sys::list_nth(list, index) }.cast::<T>();
    (!item.is_null()).then_some(item)
}

unsafe fn function_name(function_oid: pg_sys::Oid) -> Option<String> {
    if function_oid == pg_sys::InvalidOid {
        return None;
    }
    // SAFETY: called on the backend thread with a valid pg_proc OID; PostgreSQL
    // returns a palloc'd function name or null when no catalog row exists.
    let name = unsafe { pg_sys::get_func_name(function_oid) };
    if name.is_null() {
        return None;
    }
    // SAFETY: the null check proves get_func_name returned a NUL-terminated
    // string valid in the current PostgreSQL memory context.
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_str()
        .ok()
        .map(str::to_ascii_lowercase)
}

#[derive(Debug, Clone, Copy)]
struct RasterNamePrefilterTarget {
    function: *mut pg_sys::FuncExpr,
    nonjunk_count: usize,
    wrapped: bool,
}

/// Cheap name lookup only decides whether the expensive catalog proof is
/// worth attempting. A name match never establishes semantics or admission.
unsafe fn raster_name_prefilter(node: *mut Node) -> Option<(*mut pg_sys::FuncExpr, bool)> {
    if node.is_null() {
        return None;
    }
    // SAFETY: callers pass a live planner expression node whose leading Node
    // header remains valid throughout this recursive prefilter.
    let (function, wrapped) = match unsafe { (*node).type_ } {
        NodeTag::T_FuncExpr => (node.cast::<pg_sys::FuncExpr>(), false),
        NodeTag::T_RelabelType => {
            // SAFETY: the tag proves RelabelType layout; its inner expression
            // is planner-owned and live for the recursive inspection.
            unsafe {
                let inner = (*node.cast::<pg_sys::RelabelType>()).arg.cast::<Node>();
                let (function, _) = raster_name_prefilter(inner)?;
                (function, true)
            }
        }
        NodeTag::T_CoerceViaIO => {
            // SAFETY: the tag proves CoerceViaIO layout; its inner expression
            // is planner-owned and live for the recursive inspection.
            unsafe {
                let inner = (*node.cast::<pg_sys::CoerceViaIO>()).arg.cast::<Node>();
                let (function, _) = raster_name_prefilter(inner)?;
                (function, true)
            }
        }
        NodeTag::T_CoerceToDomain => {
            // SAFETY: the tag proves CoerceToDomain layout; its inner
            // expression is planner-owned for this recursive inspection.
            unsafe {
                let inner = (*node.cast::<pg_sys::CoerceToDomain>()).arg.cast::<Node>();
                let (function, _) = raster_name_prefilter(inner)?;
                (function, true)
            }
        }
        NodeTag::T_CollateExpr => {
            // SAFETY: the tag proves CollateExpr layout; its inner expression
            // is planner-owned and live for the recursive inspection.
            unsafe {
                let inner = (*node.cast::<pg_sys::CollateExpr>()).arg.cast::<Node>();
                let (function, _) = raster_name_prefilter(inner)?;
                (function, true)
            }
        }
        _ => return None,
    };
    // SAFETY: each successful match returns a live planner-owned FuncExpr.
    let funcid = unsafe { (*function).funcid };
    // SAFETY: the extracted OID belongs to that FuncExpr and this lookup runs
    // on the owning backend thread.
    let name = unsafe { function_name(funcid) };
    name.as_deref()
        .is_some_and(|name| matches!(name, RECLASS_NAME | SUMMARY_STATS_NAME))
        .then_some((function, wrapped))
}

unsafe fn raster_target_function(query: &pg_sys::Query) -> Option<RasterNamePrefilterTarget> {
    let mut nonjunk = 0_usize;
    let mut candidate: Option<(*mut pg_sys::FuncExpr, bool)> = None;
    for index in 0..list_len(query.targetList) {
        // SAFETY: the loop bound came from this live Query target List.
        let target = unsafe { list_item::<pg_sys::TargetEntry>(query.targetList, index) }?;
        // SAFETY: a non-null target-list cell is a planner-owned TargetEntry.
        if unsafe { (*target).resjunk } {
            continue;
        }
        nonjunk = nonjunk.checked_add(1)?;
        // SAFETY: the verified TargetEntry owns its expression pointer for the
        // planner tree lifetime.
        let expression = unsafe { (*target).expr.cast::<Node>() };
        // SAFETY: `expression` is the live TargetEntry expression inspected by
        // the recursive wrapper-aware prefilter.
        if let Some(found) = unsafe { raster_name_prefilter(expression) } {
            if candidate.is_some() {
                candidate = Some(found);
                break;
            }
            candidate = Some(found);
        }
    }
    candidate.map(|(function, wrapped)| RasterNamePrefilterTarget {
        function,
        nonjunk_count: nonjunk,
        wrapped,
    })
}

unsafe fn direct_const(node: *mut Node) -> Option<pg_sys::Const> {
    if node.is_null() {
        return None;
    }
    // SAFETY: the non-null planner expression begins with a readable Node tag.
    if unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    // SAFETY: the tag proves Const layout; copy the by-value C struct while
    // its planner-owned source node remains live.
    Some(unsafe { *node.cast::<pg_sys::Const>() })
}

unsafe fn text_const(node: *mut Node) -> Option<String> {
    // SAFETY: the caller supplies a live planner expression; direct_const
    // validates the Node tag before copying its Const fields.
    let constant = unsafe { direct_const(node) }?;
    if constant.consttype != pg_sys::TEXTOID
        || constant.consttypmod != -1
        || constant.constlen != -1
        || constant.constbyval
        || constant.constisnull
    {
        return None;
    }
    // SAFETY: the strict type/length/byval/null checks establish a non-null
    // PostgreSQL text Datum; FromDatum copies it into an owned String.
    unsafe { String::from_datum(constant.constvalue, false) }
}

unsafe fn int4_const(node: *mut Node) -> Option<i32> {
    // SAFETY: the caller supplies a live planner expression; direct_const
    // validates the Node tag before copying its Const fields.
    let constant = unsafe { direct_const(node) }?;
    if constant.consttype != pg_sys::INT4OID
        || constant.consttypmod != -1
        || constant.constcollid != pg_sys::InvalidOid
        || constant.constlen != 4
        || !constant.constbyval
        || constant.constisnull
    {
        return None;
    }
    // SAFETY: the preceding metadata checks prove a non-null by-value int4
    // Datum, which DatumGetInt32 decodes without dereferencing external data.
    Some(unsafe { pg_sys::DatumGetInt32(constant.constvalue) })
}

unsafe fn bool_const(node: *mut Node) -> Option<bool> {
    // SAFETY: the caller supplies a live planner expression; direct_const
    // validates the Node tag before copying its Const fields.
    let constant = unsafe { direct_const(node) }?;
    if constant.consttype != pg_sys::BOOLOID
        || constant.consttypmod != -1
        || constant.constcollid != pg_sys::InvalidOid
        || constant.constlen != 1
        || !constant.constbyval
        || constant.constisnull
    {
        return None;
    }
    // SAFETY: the preceding metadata checks prove a non-null by-value bool
    // Datum, which DatumGetBool decodes without external pointer access.
    Some(unsafe { pg_sys::DatumGetBool(constant.constvalue) })
}

#[derive(Debug, Clone, Copy)]
struct RasterVar {
    relation_oid: u32,
    attno: i32,
    type_oid: u32,
}

unsafe fn direct_raster_var(
    node: *mut Node,
    rti: pg_sys::Index,
    rte: &pg_sys::RangeTblEntry,
    raster_type_oid: pg_sys::Oid,
) -> Option<RasterVar> {
    if node.is_null() {
        return None;
    }
    // SAFETY: the non-null planner expression begins with a readable Node tag.
    if unsafe { (*node).type_ } != NodeTag::T_Var {
        return None;
    }
    // SAFETY: the tag proves Var layout and the planner owns the expression
    // for this validation call.
    let var = unsafe { &*node.cast::<pg_sys::Var>() };
    let rti_i32 = i32::try_from(rti).ok()?;
    if var.varno != rti_i32
        || var.varlevelsup != 0
        || var.varattno <= 0
        || var.vartype != raster_type_oid
        || var.vartypmod != -1
        || var.varcollid != pg_sys::InvalidOid
        || rte.rtekind != pg_sys::RTEKind::RTE_RELATION
        || rte.relid == pg_sys::InvalidOid
    {
        return None;
    }
    // SAFETY: the checks above establish a live relation RTE with a valid OID
    // and a positive user attribute number before consulting pg_attribute.
    if unsafe { pg_sys::get_atttype(rte.relid, var.varattno) } != raster_type_oid {
        return None;
    }
    Some(RasterVar {
        relation_oid: u32::from(rte.relid),
        attno: i32::from(var.varattno),
        type_oid: u32::from(var.vartype),
    })
}

unsafe fn extract_call(
    function: &pg_sys::FuncExpr,
    identity: &PostgisRasterCatalogIdentity,
    rti: pg_sys::Index,
    rte: &pg_sys::RangeTblEntry,
) -> Result<RasterCallShape, ()> {
    if function.funcid == pg_sys::InvalidOid || function.funcretset || function.funcvariadic {
        return Err(());
    }
    if function.funcid == identity.reclass_fn_oid {
        if list_len(function.args) != 3 || function.funcresulttype != identity.raster_type_oid {
            return Err(());
        }
        // SAFETY: the exact three-element FuncExpr argument List is
        // planner-owned; indexes zero through two are therefore valid.
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        // SAFETY: the same verified List makes its expression argument valid.
        let expression_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        // SAFETY: the same verified List makes its pixel-type argument valid.
        let pixel_type_node = unsafe { list_item::<Node>(function.args, 2) }.ok_or(())?;
        // SAFETY: `raster_node` is a live planner expression; the helper proves
        // its Var/relation/catalog identity before returning a RasterVar.
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        return Ok(RasterCallShape::ReclassTextText {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
            // SAFETY: `expression_node` is the live second argument and the
            // helper requires an exact non-null text Const before decoding.
            expression: unsafe { text_const(expression_node) }.ok_or(())?,
            // SAFETY: `pixel_type_node` is the live third argument and the
            // helper requires an exact non-null text Const before decoding.
            pixel_type: unsafe { text_const(pixel_type_node) }.ok_or(())?,
        });
    }
    if function.funcid == identity.summary_stats_fn_oid {
        if list_len(function.args) != 3
            || function.funcresulttype != identity.summary_stats_type_oid
        {
            return Err(());
        }
        // SAFETY: the exact three-element FuncExpr argument List makes index
        // zero a live planner expression.
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        // SAFETY: the verified List makes index one a live band expression.
        let band_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        // SAFETY: the verified List makes index two a live exclude expression.
        let exclude_node = unsafe { list_item::<Node>(function.args, 2) }.ok_or(())?;
        // SAFETY: `raster_node` is validated against this relation and the
        // catalog-proven raster type by direct_raster_var.
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        // SAFETY: `band_node` is the live second argument; int4_const validates
        // an exact non-null by-value int4 Const before decoding.
        let band = u32::try_from(unsafe { int4_const(band_node) }.ok_or(())?).map_err(|_| ())?;
        return Ok(RasterCallShape::SummaryStatsBand {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
            band,
            // SAFETY: `exclude_node` is the live third argument; bool_const
            // validates an exact non-null by-value bool Const before decoding.
            exclude_nodata: unsafe { bool_const(exclude_node) }.ok_or(())?,
        });
    }
    if function.funcid == identity.summary_stats_default_band_fn_oid {
        if list_len(function.args) != 2
            || function.funcresulttype != identity.summary_stats_type_oid
        {
            return Err(());
        }
        // SAFETY: the exact two-element FuncExpr argument List makes index
        // zero a live planner expression.
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        // SAFETY: the verified List makes index one a live exclude expression.
        let exclude_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        // SAFETY: `raster_node` is validated against this relation and the
        // catalog-proven raster type by direct_raster_var.
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        return Ok(RasterCallShape::SummaryStatsDefaultBand {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
            // SAFETY: `exclude_node` is the live second argument; bool_const
            // validates an exact non-null by-value bool Const before decoding.
            exclude_nodata: unsafe { bool_const(exclude_node) }.ok_or(())?,
        });
    }
    Ok(RasterCallShape::UnsupportedFunction {
        function_oid: u32::from(function.funcid),
    })
}

fn add_feature(
    features: RasterQueryFeatures,
    condition: bool,
    feature: RasterQueryFeatures,
) -> RasterQueryFeatures {
    if condition {
        features.union(feature)
    } else {
        features
    }
}

unsafe fn query_features(
    query: &pg_sys::Query,
    rel: &pg_sys::RelOptInfo,
    rte: &pg_sys::RangeTblEntry,
) -> RasterQueryFeatures {
    // SAFETY: a non-null jointree is the live planner-owned FromExpr for this
    // Query, so its optional quals pointer may be inspected.
    let jointree_qual = !query.jointree.is_null() && unsafe { !(*query.jointree).quals.is_null() };
    let mut features = RasterQueryFeatures::NONE;
    features = add_feature(
        features,
        jointree_qual || !rel.baserestrictinfo.is_null(),
        RasterQueryFeatures::QUAL,
    );
    features = add_feature(
        features,
        !query.sortClause.is_null(),
        RasterQueryFeatures::SORT,
    );
    features = add_feature(
        features,
        query.hasDistinctOn || !query.distinctClause.is_null(),
        RasterQueryFeatures::DISTINCT,
    );
    features = add_feature(
        features,
        !query.groupClause.is_null(),
        RasterQueryFeatures::GROUP,
    );
    features = add_feature(
        features,
        !query.havingQual.is_null(),
        RasterQueryFeatures::HAVING,
    );
    features = add_feature(
        features,
        !query.limitOffset.is_null() || !query.limitCount.is_null(),
        RasterQueryFeatures::LIMIT_OR_OFFSET,
    );
    features = add_feature(
        features,
        query.hasWindowFuncs || !query.windowClause.is_null(),
        RasterQueryFeatures::WINDOW,
    );
    features = add_feature(
        features,
        query.hasTargetSRFs,
        RasterQueryFeatures::TARGET_SRF,
    );
    features = add_feature(features, query.hasSubLinks, RasterQueryFeatures::SUBLINK);
    features = add_feature(
        features,
        !query.setOperations.is_null(),
        RasterQueryFeatures::SET_OPERATION,
    );
    features = add_feature(features, !query.cteList.is_null(), RasterQueryFeatures::CTE);
    features = add_feature(
        features,
        !query.rowMarks.is_null(),
        RasterQueryFeatures::ROW_MARK,
    );
    add_feature(
        features,
        query.hasRowSecurity || !rte.securityQuals.is_null(),
        RasterQueryFeatures::ROW_SECURITY,
    )
}

unsafe fn relation_shape(
    query: &pg_sys::Query,
    rel: &pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: &pg_sys::RangeTblEntry,
) -> RasterRelationShape {
    let Ok(rti_i32) = i32::try_from(rti) else {
        return RasterRelationShape::Unsupported;
    };
    if list_len(query.rtable) != 1
        || rti != 1
        || rte.rtekind != pg_sys::RTEKind::RTE_RELATION
        || rte.relid == pg_sys::InvalidOid
        || !matches!(rel.reloptkind, pg_sys::RelOptKind::RELOPT_BASEREL)
        || !rte.tablesample.is_null()
        || query.jointree.is_null()
    {
        return RasterRelationShape::Unsupported;
    }
    // SAFETY: the null check above proves the Query owns a live FromExpr.
    let fromlist = unsafe { (*query.jointree).fromlist };
    if list_len(fromlist) != 1 {
        return RasterRelationShape::Unsupported;
    }
    // SAFETY: the verified singleton fromlist is planner-owned and index zero
    // remains live for this shape check.
    let Some(range_ref) = (unsafe { list_item::<Node>(fromlist, 0) }) else {
        return RasterRelationShape::Unsupported;
    };
    // SAFETY: `range_ref` is a non-null planner expression with a Node header.
    if unsafe { (*range_ref).type_ } != NodeTag::T_RangeTblRef {
        return RasterRelationShape::Unsupported;
    }
    // SAFETY: the tag check proves RangeTblRef layout; its rtindex is readable
    // while the planner jointree remains live.
    if unsafe { (*range_ref.cast::<pg_sys::RangeTblRef>()).rtindex } != rti_i32 {
        return RasterRelationShape::Unsupported;
    }
    RasterRelationShape::UnqualifiedSingleBase {
        relation_oid: u32::from(rte.relid),
    }
}

fn rows_estimate(rows: f64) -> u64 {
    if !rows.is_finite() || rows <= 0.0 {
        0
    } else if rows >= u64::MAX as f64 {
        u64::MAX
    } else {
        rows.ceil() as u64
    }
}

fn planner_decline_reason(decline: &RasterPlannerDecline) -> RejectionReason {
    match decline {
        RasterPlannerDecline::SummaryStatsBitExactUnavailable(_) => {
            RejectionReason::RasterSummaryStatsBitExactUnavailable
        }
        RasterPlannerDecline::UnsupportedFunctionOid(_)
        | RasterPlannerDecline::RasterTypeMismatch
        | RasterPlannerDecline::ResultTypeMismatch
        | RasterPlannerDecline::CatalogFingerprintUnavailable => {
            RejectionReason::RasterCatalogProofFailed
        }
        _ => RejectionReason::RasterUnsupportedShape,
    }
}

fn cost_decline_reason(gate: RasterCostGate) -> RejectionReason {
    match gate {
        RasterCostGate::SelectedBandMissing { .. } => RejectionReason::RasterSelectedBandMissing,
        RasterCostGate::NonRoundtrippableZeroGridBand { .. } => {
            RejectionReason::RasterZeroGridWkbNonRoundtrippable
        }
        RasterCostGate::ExactResidentMetadataUnavailable
        | RasterCostGate::ResidentMetadataOverflow
        | RasterCostGate::InvalidResidentMetadata
        | RasterCostGate::ReclassOutputBytesUnavailable => {
            RejectionReason::RasterResidentMetadataUnavailable
        }
        RasterCostGate::PixelsBelowDeviceMinimum { .. }
        | RasterCostGate::InvalidNativeCost
        | RasterCostGate::UncalibratedCoefficients => RejectionReason::RasterCostUncalibrated,
    }
}

fn exact_resident_work(spec: &crate::engine::raster::RasterQuerySpec) -> RasterWorkEstimate {
    let Ok(attno) = i16::try_from(spec.raster_attno) else {
        return RasterWorkEstimate::Overflow;
    };
    with_resident_column(
        pg_sys::Oid::from(spec.relation_oid),
        attno,
        |column| match column {
            ResidentColumnView::Raster {
                type_oid, stats, ..
            } if u32::from(type_oid) == spec.raster_type_oid => {
                let tag = spec.reclass.output_pixel_type.tag();
                let Some(selected_pixels) = stats.selected_band_pixels(1) else {
                    return RasterWorkEstimate::Overflow;
                };
                let Some(selected_band_rows) = stats.selected_band_rows(1) else {
                    return RasterWorkEstimate::Overflow;
                };
                let Some(output_pixel_bytes) = stats.reclass_output_pixel_bytes(tag) else {
                    return RasterWorkEstimate::Overflow;
                };
                let Some(output_wkb_bytes) = stats.reclass_output_wkb_bytes(tag) else {
                    return RasterWorkEstimate::Overflow;
                };
                RasterWorkEstimate::ResidentExact(RasterResidentWork {
                    row_count: stats.row_count,
                    non_null_rows: stats.non_null_rows,
                    zero_grid_present_band_rows: stats.zero_grid_present_band_rows,
                    selected_band_rows,
                    selected_pixels,
                    input_wkb_bytes: stats.input_wkb_bytes,
                    reclass_output_pixel_bytes: Some(output_pixel_bytes),
                    reclass_output_wkb_bytes: Some(output_wkb_bytes),
                })
            }
            _ => RasterWorkEstimate::Unavailable,
        },
    )
    .unwrap_or(RasterWorkEstimate::Unavailable)
}

fn normal_raster_promotion_envelope(cost: &crate::engine::raster::RasterCost) -> bool {
    cost.gate == RasterCostGate::UncalibratedCoefficients
        && cost.work.is_some_and(|work| {
            (NORMAL_RASTER_MIN_SELECTED_PIXELS..=NORMAL_RASTER_MAX_SELECTED_PIXELS)
                .contains(&work.selected_pixels)
        })
}

fn record(reason: RejectionReason, rows: u64) {
    stats::increment_planner_rejected(reason.stats_key(), rows);
    pgrx::debug1!(
        "pg_accel: exact raster observer declined candidate: {}",
        reason.stats_key()
    );
}

fn is_observer_owner(rti: pg_sys::Index, reloptkind: pg_sys::RelOptKind::Type) -> bool {
    rti == 1 && reloptkind == pg_sys::RelOptKind::RELOPT_BASEREL
}

unsafe fn validated_candidate(
    query: &pg_sys::Query,
    rel: &pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: &pg_sys::RangeTblEntry,
    prefilter: RasterNamePrefilterTarget,
) -> Result<RasterPlannerCandidate, RejectionReason> {
    // SAFETY: catalog resolution runs on the planner backend and balances all
    // syscache lookups before returning an owned identity snapshot.
    let identity = unsafe { resolve_postgis_raster_catalog() }
        .map_err(|_| RejectionReason::RasterCatalogProofFailed)?;
    // SAFETY: the prefilter pointer came from this Query's live target list and
    // remains planner-owned throughout candidate validation.
    let function = unsafe { &*prefilter.function };
    if prefilter.wrapped {
        let exact_oid = function.funcid == identity.reclass_fn_oid
            || function.funcid == identity.summary_stats_fn_oid
            || function.funcid == identity.summary_stats_default_band_fn_oid;
        return Err(if exact_oid {
            RejectionReason::RasterUnsupportedShape
        } else {
            RejectionReason::RasterCatalogProofFailed
        });
    }
    // SAFETY: `function` is the live target FuncExpr and `identity` contains
    // catalog-proven OIDs; extract_call bounds-checks every argument List read.
    let call = unsafe { extract_call(function, &identity, rti, rte) }
        .map_err(|()| RejectionReason::RasterUnsupportedShape)?;
    let catalog = RasterCatalogContract {
        raster_type_oid: u32::from(identity.raster_type_oid),
        summary_stats_type_oid: u32::from(identity.summary_stats_type_oid),
        reclass_fn_oid: u32::from(identity.reclass_fn_oid),
        summary_stats_fn_oid: u32::from(identity.summary_stats_fn_oid),
        summary_stats_default_band_fn_oid: u32::from(identity.summary_stats_default_band_fn_oid),
        as_wkb_fn_oid: u32::from(identity.as_wkb_fn_oid),
        rast_from_wkb_fn_oid: u32::from(identity.rast_from_wkb_fn_oid),
        fingerprint: identity.fingerprint_words.into_boxed_slice(),
    };
    let shape = RasterShapeInput {
        command: if query.commandType == pg_sys::CmdType::CMD_SELECT {
            RasterCommandShape::Select
        } else {
            RasterCommandShape::Unsupported
        },
        // SAFETY: query/rel/rte are live objects from the same planner hook;
        // relation_shape validates jointree and List structure before access.
        relation: unsafe { relation_shape(query, rel, rti, rte) },
        // SAFETY: the same live planner objects expose only pointer-presence
        // feature flags; query_features dereferences jointree after a null check.
        features: unsafe { query_features(query, rel, rte) },
        nonjunk_target_count: prefilter.nonjunk_count,
        call,
        estimated_rows: rows_estimate(rel.rows),
    };
    build_raster_query_spec(shape, &catalog).map_err(|decline| planner_decline_reason(&decline))
}

#[cfg(feature = "pg_test")]
thread_local! {
    static FORCED_RASTER_PATH_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static TAMPERED_RASTER_CATALOG_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Scope forced raster path construction to one pg_test planner invocation.
#[cfg(feature = "pg_test")]
pub fn with_forced_raster_path<R>(f: impl FnOnce() -> R) -> R {
    struct ForceGuard;

    impl Drop for ForceGuard {
        fn drop(&mut self) {
            FORCED_RASTER_PATH_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    FORCED_RASTER_PATH_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = ForceGuard;
    f()
}

#[cfg(feature = "pg_test")]
fn forced_raster_path_enabled() -> bool {
    FORCED_RASTER_PATH_DEPTH.with(|depth| depth.get() > 0)
}

/// Scope an invalid planned importer OID to one pg_test planner invocation.
#[cfg(feature = "pg_test")]
pub fn with_tampered_raster_catalog_oid<R>(f: impl FnOnce() -> R) -> R {
    struct TamperGuard;

    impl Drop for TamperGuard {
        fn drop(&mut self) {
            TAMPERED_RASTER_CATALOG_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    TAMPERED_RASTER_CATALOG_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = TamperGuard;
    f()
}

#[cfg(feature = "pg_test")]
fn tampered_raster_catalog_enabled() -> bool {
    TAMPERED_RASTER_CATALOG_DEPTH.with(|depth| depth.get() > 0)
}

/// Observe one base relation for an exact raster target and record why the
/// native plan remains selected. This function never constructs a CustomPath.
///
/// # Safety
/// All pointers must be the live objects supplied to `set_rel_pathlist_hook`.
pub(super) unsafe fn observe(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut pg_sys::RangeTblEntry,
) -> bool {
    if root.is_null() || rel.is_null() || rte.is_null() {
        return false;
    }
    // SAFETY: the null checks prove `root` is the live PlannerInfo supplied to
    // the hook; PostgreSQL retains its parsed Query for planning.
    let query_ptr = unsafe { (*root).parse };
    if query_ptr.is_null() {
        return false;
    }
    // SAFETY: all pointers were checked non-null and belong to this planner
    // invocation, so immutable references are valid for the observer call.
    let (query, rel_ref, rte_ref) = unsafe { (&*query_ptr, &*rel, &*rte) };
    // SAFETY: `query` is the live parsed Query; the helper bounds-checks its
    // target List and validates expression tags before dereferencing.
    let Some(prefilter) = (unsafe { raster_target_function(query) }) else {
        return false;
    };
    // set_rel_pathlist may run for every RTE and partition member. The first
    // baserel is the sole query-scoped observer owner, preventing duplicate
    // statistics while strict shape validation still sees the complete query.
    // Other invocations still return true so generic observers do not record
    // a second decline family for the same raster target.
    if !is_observer_owner(rti, rel_ref.reloptkind) {
        return true;
    }
    let rows = rows_estimate(rel_ref.rows);
    // SAFETY: all references and the prefiltered FuncExpr originate from this
    // same live planner tree; validation performs exact catalog/shape checks.
    let candidate = match unsafe { validated_candidate(query, rel_ref, rti, rte_ref, prefilter) } {
        Ok(candidate) => candidate,
        Err(reason) => {
            record(reason, rows);
            return true;
        }
    };
    #[cfg(feature = "pg_test")]
    if forced_raster_path_enabled() {
        return true;
    }
    // SAFETY: `pathlist` is the planner-owned List for this live RelOptInfo;
    // find_cheapest_path only traverses its Path entries.
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    let native_total_cost = if cheapest.is_null() {
        PgCost::new(0.0)
    } else {
        // SAFETY: a non-null result from find_cheapest_path is a live
        // planner-owned Path whose total_cost field is initialized.
        PgCost::new(unsafe { (*cheapest).total_cost })
    };
    let cost = estimate_raster_cost(
        RasterCostInput {
            work: exact_resident_work(&candidate.spec),
            native_total_cost,
        },
        device_limits(),
    );
    if normal_raster_promotion_envelope(&cost) {
        return true;
    }
    record(cost_decline_reason(cost.gate), rows);
    true
}

/// Inject the exact validated RQS2 candidate after all resident work gates.
/// `forced` exists only for pg_test fault injection; production reaches this
/// helper through [`try_inject`] and must clear the complete typed cost gate.
///
/// # Safety
/// Pointers must be the live objects supplied to `create_upper_paths_hook`.
unsafe fn try_inject_impl(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut pg_sys::RelOptInfo,
    forced: bool,
) -> bool {
    if root.is_null() || output_rel.is_null() {
        return false;
    }
    let root_ref = unsafe { &*root };
    if root_ref.parse.is_null()
        || root_ref.simple_rel_array.is_null()
        || root_ref.simple_rel_array_size <= 1
    {
        return false;
    }
    let query = unsafe { &*root_ref.parse };
    let Some(prefilter) = (unsafe { raster_target_function(query) }) else {
        return false;
    };
    let rel = unsafe { *root_ref.simple_rel_array.add(1) };
    let Some(rte) = (unsafe { list_item::<pg_sys::RangeTblEntry>(query.rtable, 0) }) else {
        return false;
    };
    if rel.is_null() {
        return false;
    }
    let candidate = match unsafe { validated_candidate(query, &*rel, 1, &*rte, prefilter) } {
        Ok(candidate) => candidate,
        Err(_) => return false,
    };
    #[cfg(feature = "pg_test")]
    let mut candidate = candidate;
    let base_path = unsafe { find_cheapest_path((*rel).pathlist) };
    if base_path.is_null() {
        return false;
    }
    if forced {
        if !matches!(
            exact_resident_work(&candidate.spec),
            RasterWorkEstimate::ResidentExact(work) if work.zero_grid_present_band_rows == 0
        ) {
            return false;
        }
    } else {
        let work = exact_resident_work(&candidate.spec);
        let cost = estimate_raster_cost(
            RasterCostInput {
                work,
                native_total_cost: PgCost::new(unsafe { (*base_path).total_cost }),
            },
            device_limits(),
        );
        // Qualified Apple Metal envelope: the deterministic exact-RQS2 lane
        // wins only inside the measured selected-pixel band.
        if !normal_raster_promotion_envelope(&cost) {
            return false;
        }
    }
    #[cfg(feature = "pg_test")]
    if forced && tampered_raster_catalog_enabled() {
        candidate.spec.rast_from_wkb_fn_oid =
            candidate.spec.rast_from_wkb_fn_oid.wrapping_add(1).max(1);
    }
    let final_target = unsafe { pg_sys::make_pathtarget_from_tlist(query.targetList) };
    if final_target.is_null() || unsafe { list_len((*final_target).exprs) } != 1 {
        return false;
    }
    // The measured pixel envelope is the hard admission boundary. Keep a
    // finite nonzero cost tied to PostgreSQL's cheapest base path so EXPLAIN
    // remains comparable and forced pg_test paths retain deterministic choice.
    let startup_cost = 0.0;
    let total_cost = if forced {
        0.0
    } else {
        (unsafe { (*base_path).total_cost } * 0.5).max(1.0)
    };
    let plan = RasterExecPlan::from_spec(candidate.spec)
        .unwrap_or_else(|error| pgrx::error!("pg_accel: invalid exact raster plan: {error}"));
    let cpath = unsafe {
        pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomPath>()).cast::<pg_sys::CustomPath>()
    };
    unsafe {
        (*cpath).path.type_ = NodeTag::T_CustomPath;
        (*cpath).path.pathtype = NodeTag::T_CustomScan;
        (*cpath).path.parent = output_rel;
        (*cpath).path.pathtarget = final_target;
        (*cpath).path.param_info = std::ptr::null_mut();
        (*cpath).path.parallel_aware = false;
        (*cpath).path.parallel_safe = false;
        (*cpath).path.parallel_workers = 0;
        (*cpath).path.rows = candidate.estimated_rows as f64;
        (*cpath).path.startup_cost = startup_cost;
        (*cpath).path.total_cost = total_cost;
        (*cpath).path.pathkeys = std::ptr::null_mut();
        (*cpath).flags = 0;
        (*cpath).custom_paths = std::ptr::null_mut();
        (*cpath).custom_restrictinfo = std::ptr::null_mut();
        (*cpath).methods = custom_scan::raster_path_methods();
        (*cpath).custom_private = custom_scan::append_raster_exec_plan(std::ptr::null_mut(), &plan);
    }
    unsafe {
        add_gpu_path_with_resident_proof(
            stats::PlannerHookStage::UpperFinal,
            if forced {
                "pg_test_forced_raster"
            } else {
                "raster_exact_reclass_candidate"
            },
            output_rel,
            cpath,
            custom_scan::raster_resident_proof(),
        )
    }
}

/// Try normal production admission for the exact resident RQS2 candidate.
///
/// # Safety
/// Pointers must be the live objects supplied to `create_upper_paths_hook`.
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) -> bool {
    unsafe { try_inject_impl(root, output_rel, false) }
}

/// Force the exact path for pg_test-only fault and boundary coverage.
///
/// # Safety
/// Pointers must be the live objects supplied to `create_upper_paths_hook`.
#[cfg(feature = "pg_test")]
pub(super) unsafe fn try_force_inject(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) -> bool {
    forced_raster_path_enabled() && unsafe { try_inject_impl(root, output_rel, true) }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn every_pure_raster_decline_maps_to_a_stable_family() {
        assert_eq!(
            planner_decline_reason(&RasterPlannerDecline::SummaryStatsBitExactUnavailable(1)),
            RejectionReason::RasterSummaryStatsBitExactUnavailable
        );
        assert_eq!(
            planner_decline_reason(&RasterPlannerDecline::UnsupportedFunctionOid(1)),
            RejectionReason::RasterCatalogProofFailed
        );
        assert_eq!(
            planner_decline_reason(&RasterPlannerDecline::UnsupportedRelation),
            RejectionReason::RasterUnsupportedShape
        );
    }

    #[test]
    fn cost_gate_mapping_is_stable_before_candidate_promotion() {
        assert_eq!(
            cost_decline_reason(RasterCostGate::ExactResidentMetadataUnavailable),
            RejectionReason::RasterResidentMetadataUnavailable
        );
        assert_eq!(
            cost_decline_reason(RasterCostGate::SelectedBandMissing {
                present_rows: 1,
                required_rows: 2,
            }),
            RejectionReason::RasterSelectedBandMissing
        );
        assert_eq!(
            cost_decline_reason(RasterCostGate::NonRoundtrippableZeroGridBand { rows: 1 }),
            RejectionReason::RasterZeroGridWkbNonRoundtrippable
        );
        assert_eq!(
            RejectionReason::RasterZeroGridWkbNonRoundtrippable.stats_key(),
            "raster_zero_grid_wkb_non_roundtrippable"
        );
        assert_eq!(
            cost_decline_reason(RasterCostGate::UncalibratedCoefficients),
            RejectionReason::RasterCostUncalibrated
        );
    }

    #[test]
    fn normal_raster_envelope_is_inclusive_and_rejects_nearby_unmeasured_work() {
        let limits = device_limits();
        let cost = |selected_pixels| {
            estimate_raster_cost(
                RasterCostInput {
                    work: RasterWorkEstimate::ResidentExact(RasterResidentWork {
                        row_count: 1,
                        non_null_rows: 1,
                        zero_grid_present_band_rows: 0,
                        selected_band_rows: 1,
                        selected_pixels,
                        input_wkb_bytes: 64,
                        reclass_output_pixel_bytes: Some(selected_pixels),
                        reclass_output_wkb_bytes: Some(selected_pixels.saturating_add(64)),
                    }),
                    native_total_cost: PgCost::new(1_000.0),
                },
                limits,
            )
        };

        assert!(normal_raster_promotion_envelope(&cost(
            NORMAL_RASTER_MIN_SELECTED_PIXELS
        )));
        assert!(normal_raster_promotion_envelope(&cost(
            NORMAL_RASTER_MAX_SELECTED_PIXELS
        )));
        assert!(!normal_raster_promotion_envelope(&cost(
            NORMAL_RASTER_MIN_SELECTED_PIXELS - 1
        )));
        assert!(!normal_raster_promotion_envelope(&cost(
            NORMAL_RASTER_MAX_SELECTED_PIXELS + 1
        )));
    }

    #[test]
    fn only_first_baserel_owns_query_scoped_observation() {
        assert!(is_observer_owner(1, pg_sys::RelOptKind::RELOPT_BASEREL));
        assert!(!is_observer_owner(2, pg_sys::RelOptKind::RELOPT_BASEREL));
        assert!(!is_observer_owner(
            1,
            pg_sys::RelOptKind::RELOPT_OTHER_MEMBER_REL
        ));
    }
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[derive(Debug, Clone, Copy)]
    struct TestRasterDatum(pg_sys::Datum);

    impl FromDatum for TestRasterDatum {
        unsafe fn from_polymorphic_datum(
            datum: pg_sys::Datum,
            is_null: bool,
            _type_oid: pg_sys::Oid,
        ) -> Option<Self> {
            (!is_null && datum.value() != 0).then_some(Self(datum))
        }
    }

    impl IntoDatum for TestRasterDatum {
        fn into_datum(self) -> Option<pg_sys::Datum> {
            Some(self.0)
        }

        fn type_oid() -> pg_sys::Oid {
            pg_sys::InvalidOid
        }

        fn is_compatible_with(_other: pg_sys::Oid) -> bool {
            true
        }
    }

    const RASTER_REASONS: &[&str] = &[
        "raster_unsupported_shape",
        "raster_catalog_proof_failed",
        "raster_summarystats_bit_exact_unavailable",
        "raster_resident_metadata_unavailable",
        "raster_zero_grid_wkb_non_roundtrippable",
        "raster_selected_band_missing",
        "raster_cost_uncalibrated",
        "raster_runtime_unavailable",
    ];

    fn setup_extension_and_fixture() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis")
            .expect("PostGIS must be available for raster planner tests");
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis_raster")
            .expect("PostGIS Raster must be available for planner tests");
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = off; \
             DROP TABLE IF EXISTS pgaccel_raster_observer; \
             CREATE TEMP TABLE pgaccel_raster_observer(id int4, rast raster); \
             INSERT INTO pgaccel_raster_observer VALUES ( \
               1, ST_AddBand( \
                    ST_MakeEmptyRaster(256, 256, 0, 0, 1, -1, 0, 0, 4326), \
                    '8BUI'::text, 0, 0)); \
             ANALYZE pgaccel_raster_observer",
        )
        .expect("create raster observer fixture");
    }

    fn setup_raster_matrix() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis")
            .expect("PostGIS must be available for raster executor tests");
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis_raster")
            .expect("PostGIS Raster must be available for raster executor tests");
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = off; \
             DROP TABLE IF EXISTS pgaccel_raster_matrix; \
             CREATE TEMP TABLE pgaccel_raster_matrix(id int4, rast raster); \
             INSERT INTO pgaccel_raster_matrix VALUES \
               (1, NULL), \
               (2, ST_MakeEmptyRaster(2, 2, 0, 0, 1, -1, 0, 0, 4326)), \
               (3, ST_SetValue(ST_SetValue(ST_AddBand( \
                     ST_MakeEmptyRaster(2, 2, 0, 0, 1, -1, 0, 0, 4326), \
                     '8BUI'::text, 0, 255), 1, 1, 1, 7), 1, 2, 1, 8)), \
               (4, ST_SetValue(ST_AddBand( \
                     ST_MakeEmptyRaster(2, 1, 0, 0, 1, -1, 0, 0, 4326), \
                     '8BUI'::text, 255, 255), 1, 1, 1, 7)), \
               (5, ST_AddBand(ST_AddBand( \
                     ST_MakeEmptyRaster(2, 1, 0, 0, 1, -1, 0, 0, 4326), \
                     '8BUI'::text, 7, 255), '16BSI'::text, 22, -999)); \
             ANALYZE pgaccel_raster_matrix; \
             SELECT pg_accel_pin( \
               'pgaccel_raster_matrix'::regclass, ARRAY['rast'])",
        )
        .expect("create and pin raster executor matrix");
    }

    fn setup_raster_memory_fixture() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis")
            .expect("PostGIS must be available for raster memory tests");
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis_raster")
            .expect("PostGIS Raster must be available for raster memory tests");
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = off; \
             DROP TABLE IF EXISTS pgaccel_raster_memory; \
             CREATE TEMP TABLE pgaccel_raster_memory(id int4, rast raster); \
             INSERT INTO pgaccel_raster_memory \
             SELECT g, ST_AddBand( \
               ST_MakeEmptyRaster(128, 128, 0, 0, 1, -1, 0, 0, 4326), \
               '8BUI'::text, (g % 8)::double precision, 255) \
             FROM generate_series(1, 1024) AS g; \
             ANALYZE pgaccel_raster_memory; \
             SELECT pg_accel_pin( \
               'pgaccel_raster_memory'::regclass, ARRAY['rast'])",
        )
        .expect("create and pin bounded raster output fixture");
    }

    fn setup_zero_grid_band_fixture() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis")
            .expect("PostGIS must be available for zero-grid raster tests");
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis_raster")
            .expect("PostGIS Raster must be available for zero-grid raster tests");
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = off; \
             DROP TABLE IF EXISTS pgaccel_raster_zero_grid; \
             CREATE TEMP TABLE pgaccel_raster_zero_grid(rast raster); \
             INSERT INTO pgaccel_raster_zero_grid VALUES ( \
               ST_AddBand( \
                 ST_MakeEmptyRaster(0, 0, 0, 0, 1, -1, 0, 0, 4326), \
                 '8BUI'::text, 0, 255)); \
             SELECT pg_accel_pin( \
               'pgaccel_raster_zero_grid'::regclass, ARRAY['rast'])",
        )
        .expect("create and pin zero-grid present-band fixture");
    }

    fn explain(sql: &str) -> String {
        Spi::connect(|client| {
            client
                .select(&format!("EXPLAIN (COSTS OFF) {sql}"), None, &[])
                .expect("EXPLAIN raster candidate")
                .filter_map(|row| row.get::<String>(1).expect("read EXPLAIN row"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn explain_analyze(sql: &str) -> String {
        Spi::connect(|client| {
            client
                .select(
                    &format!("EXPLAIN (ANALYZE, COSTS OFF, FORMAT TEXT) {sql}"),
                    None,
                    &[],
                )
                .expect("EXPLAIN ANALYZE raster candidate")
                .filter_map(|row| row.get::<String>(1).expect("read EXPLAIN ANALYZE row"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn raster_wkb_row(
        row: &pgrx::spi::SpiHeapTupleData<'_>,
        catalog: &crate::engine::ffi::syscache::PostgisRasterCatalogIdentity,
    ) -> Option<Vec<u8>> {
        let entry = row
            .get_datum_by_ordinal(1)
            .expect("read raster result entry");
        assert_eq!(
            entry.oid(),
            catalog.raster_type_oid,
            "forced result descriptor must retain the exact raster type"
        );
        entry
            .value::<TestRasterDatum>()
            .expect("read raw raster result Datum")
            .map(|datum| {
                // SAFETY: the SPI entry proves the exact raster type and
                // remains live for this synchronous call.
                unsafe {
                    crate::engine::ffi::syscache::postgis_raster_datum_to_wkb(catalog, datum.0)
                }
                .expect("export raster result through exact PostGIS st_aswkb")
            })
    }

    fn raster_wkb_rows(sql: &str) -> Vec<Option<Vec<u8>>> {
        // SAFETY: pg_tests run synchronously on the PostgreSQL backend main
        // thread and the exact catalog identity is held for this SPI scan.
        let catalog = unsafe { crate::engine::ffi::syscache::resolve_postgis_raster_catalog() }
            .expect("resolve exact PostGIS Raster catalog for result collection");
        Spi::connect(|client| {
            client
                .select(sql, None, &[])
                .expect("execute raster result query")
                .map(|row| raster_wkb_row(&row, &catalog))
                .collect()
        })
    }

    fn raster_wkb_cursor_rows(sql: &str) -> Vec<Option<Vec<u8>>> {
        // SAFETY: same catalog and backend-thread proof as raster_wkb_rows.
        let catalog = unsafe { crate::engine::ffi::syscache::resolve_postgis_raster_catalog() }
            .expect("resolve exact PostGIS Raster catalog for cursor collection");
        Spi::connect_mut(|client| {
            let mut cursor = client
                .try_open_cursor(sql, &[])
                .expect("open forced raster result cursor");
            let mut output = Vec::new();
            loop {
                let rows = cursor.fetch(2).expect("fetch raster result cursor batch");
                if rows.is_empty() {
                    let repeated_eof = cursor.fetch(2).expect("repeat raster cursor EOF fetch");
                    assert!(
                        repeated_eof.is_empty(),
                        "raster cursor must remain exhausted after EOF"
                    );
                    break;
                }
                output.extend(rows.map(|row| raster_wkb_row(&row, &catalog)));
            }
            output
        })
    }

    fn capture_sql_error(sql: &str) -> Option<(String, String)> {
        Spi::run(
            "CREATE TEMP TABLE IF NOT EXISTS pgaccel_raster_caught_error( \
               state text NOT NULL, message text NOT NULL) ON COMMIT DROP; \
             TRUNCATE pgaccel_raster_caught_error",
        )
        .expect("initialize SQL error capture");
        Spi::run(&format!(
            "DO $pgaccel$ \
             BEGIN \
               BEGIN \
                 EXECUTE $pgaccel_query${sql}$pgaccel_query$; \
               EXCEPTION WHEN OTHERS THEN \
                 INSERT INTO pgaccel_raster_caught_error VALUES (SQLSTATE, SQLERRM); \
               END; \
             END \
             $pgaccel$"
        ))
        .expect("execute SQL under an exception boundary");
        Spi::connect(|client| {
            let mut rows = client
                .select(
                    "SELECT state, message FROM pgaccel_raster_caught_error",
                    None,
                    &[],
                )
                .expect("read captured SQL error");
            let row = rows.next()?;
            Some((
                row.get::<String>(1)
                    .expect("read captured SQLSTATE")
                    .expect("captured SQLSTATE is non-NULL"),
                row.get::<String>(2)
                    .expect("read captured SQLERRM")
                    .expect("captured SQLERRM is non-NULL"),
            ))
        })
    }

    fn raster_derived_bytes() -> i64 {
        Spi::get_one::<i64>(
            "SELECT derived_bytes FROM pg_accel_resident_status() \
             WHERE relid = 'pgaccel_raster_observer'::regclass",
        )
        .expect("read resident raster artifact bytes")
        .expect("pinned raster relation must be resident")
    }

    fn raster_resident_status(table: &str) -> (i64, i64, i64) {
        Spi::connect(|client| {
            let mut rows = client
                .select(
                    &format!(
                        "SELECT raw_bytes, derived_bytes, generation \
                         FROM pg_accel_resident_status() \
                         WHERE relid = '{table}'::regclass"
                    ),
                    None,
                    &[],
                )
                .expect("read raster resident status");
            let row = rows.next().expect("pinned raster relation has status");
            (
                row.get::<i64>(1)
                    .expect("read resident raw bytes")
                    .expect("resident raw bytes are non-NULL"),
                row.get::<i64>(2)
                    .expect("read resident derived bytes")
                    .expect("resident derived bytes are non-NULL"),
                row.get::<i64>(3)
                    .expect("read resident generation")
                    .expect("resident generation is non-NULL"),
            )
        })
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct RasterOutputMemoryStats {
        contexts: i64,
        total_bytes: i64,
        used_bytes: i64,
    }

    fn raster_output_memory_stats() -> RasterOutputMemoryStats {
        let name = crate::engine::executor::raster::RASTER_OUTPUT_MEMORY_CONTEXT_NAME;
        Spi::connect(|client| {
            let mut rows = client
                .select(
                    &format!(
                        "SELECT count(*)::bigint, \
                                COALESCE(sum(total_bytes), 0)::bigint, \
                                COALESCE(sum(used_bytes), 0)::bigint \
                         FROM pg_backend_memory_contexts WHERE name = '{name}'"
                    ),
                    None,
                    &[],
                )
                .expect("read raster output memory context");
            let row = rows.next().expect("memory aggregate returns one row");
            RasterOutputMemoryStats {
                contexts: row
                    .get::<i64>(1)
                    .expect("read raster context count")
                    .expect("raster context count is non-NULL"),
                total_bytes: row
                    .get::<i64>(2)
                    .expect("read raster context total bytes")
                    .expect("raster context total bytes are non-NULL"),
                used_bytes: row
                    .get::<i64>(3)
                    .expect("read raster context used bytes")
                    .expect("raster context used bytes are non-NULL"),
            }
        })
    }

    unsafe fn rewind_named_raster_cursor(name: &std::ffi::CStr) {
        struct ActiveSnapshotGuard;

        impl Drop for ActiveSnapshotGuard {
            fn drop(&mut self) {
                // SAFETY: this guard is constructed only after one matching
                // PushActiveSnapshot call in the same backend callback.
                unsafe { pg_sys::PopActiveSnapshot() };
            }
        }

        let portal = unsafe { pg_sys::GetPortalByName(name.as_ptr()) };
        assert!(!portal.is_null(), "named raster cursor portal must exist");
        // SAFETY: GetPortalByName returned the live named portal.
        let query_desc = unsafe { (*portal).queryDesc };
        assert!(
            !query_desc.is_null(),
            "named raster cursor must retain its QueryDesc"
        );
        // SAFETY: query_desc is the live SELECT descriptor owned by the portal.
        let plan_state = unsafe { (*query_desc).planstate };
        assert!(!plan_state.is_null(), "raster cursor must have a PlanState");
        assert_eq!(
            unsafe { (*plan_state).type_ },
            pg_sys::NodeTag::T_CustomScanState,
            "the cursor top node must be the forced childless CustomScan"
        );
        let snapshot = unsafe { (*query_desc).snapshot };
        assert!(!snapshot.is_null(), "raster cursor must retain a snapshot");
        // SAFETY: the portal owns this registered snapshot through its QueryDesc.
        unsafe { pg_sys::PushActiveSnapshot(snapshot) };
        let snapshot_guard = ActiveSnapshotGuard;
        // SAFETY: the live SELECT QueryDesc has an initialized CustomScanState.
        unsafe { pg_sys::ExecutorRewind(query_desc) };
        drop(snapshot_guard);
        // SAFETY: ExecutorRewind reset the executor; these portal position
        // fields make the next forward FETCH consume that rewound state.
        unsafe {
            (*portal).atStart = true;
            (*portal).atEnd = false;
            (*portal).portalPos = 0;
        }
    }

    fn reason_count(reason: &str) -> i64 {
        Spi::get_one::<i64>(&format!(
            "SELECT pg_accel_planner_rejection_count('{reason}')"
        ))
        .expect("read raster rejection count")
        .expect("rejection count is non-NULL")
    }

    fn assert_raster_decline(sql: &str, expected: &str) -> String {
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset planner statistics");
        let plan = explain(sql);
        assert!(
            !plan.contains("GpuAccel") && !plan.contains("Custom Scan"),
            "raster candidate must remain native:\n{plan}"
        );
        for reason in RASTER_REASONS {
            assert_eq!(
                reason_count(reason),
                i64::from(*reason == expected),
                "raster observer must record exactly one reason family for {sql}; plan:\n{plan}"
            );
        }
        assert_eq!(
            Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                .expect("read last planner rejection")
                .as_deref(),
            Some(expected)
        );
        plan
    }

    #[pg_test]
    fn forced_exact_reclass_builds_only_the_test_raster_path() {
        setup_extension_and_fixture();
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer";
        let native_result = raster_wkb_rows(sql);
        Spi::run(
            "SELECT pg_accel_pin( \
               'pgaccel_raster_observer'::regclass, ARRAY['rast'])",
        )
        .expect("pin forced raster fixture");
        let native = explain(sql);
        assert!(
            !native.contains("GpuAccelRaster") && !native.contains("Custom Scan"),
            "ordinary raster planning must remain dark:\n{native}"
        );
        assert_eq!(raster_derived_bytes(), 0);
        crate::gpu::reset_gpu_exec_count();
        let forced = super::with_forced_raster_path(|| explain(sql));
        assert!(
            forced.contains("GpuAccelRaster"),
            "test force guard must select the exact raster path:\n{forced}"
        );
        assert!(
            forced.contains("GpuRaster"),
            "forced EXPLAIN must identify the raster strategy:\n{forced}"
        );
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            0,
            "plain EXPLAIN must not dispatch a raster kernel"
        );
        assert_eq!(
            raster_derived_bytes(),
            0,
            "plain EXPLAIN must not publish a raster artifact"
        );
        let analyzed = super::with_forced_raster_path(|| explain_analyze(sql));
        for field in [
            "Custom Scan (GpuAccelRaster)",
            "Strategy: GpuRaster",
            "GPU Resident Pipeline: true",
            "GPU Kernel Dispatched: true",
            "Rows Dispatched: 1",
            "Batches: 1",
        ] {
            assert!(
                analyzed.contains(field),
                "forced EXPLAIN ANALYZE must report {field:?}:\n{analyzed}"
            );
        }
        crate::gpu::assert_gpu_executed(1);
        assert!(
            raster_derived_bytes() > 0,
            "EXPLAIN ANALYZE must publish the reconstructed raster artifact"
        );
        let forced_result = super::with_forced_raster_path(|| raster_wkb_rows(sql));
        assert_eq!(
            forced_result, native_result,
            "forced raster Datum/WKB output must equal native ST_Reclass byte-for-byte"
        );
    }

    #[pg_test]
    fn forced_reclass_matrix_widths_and_cursor_eof() {
        use crate::adapters::extractors::raster::{PixelType, parse_resident_raster};

        setup_raster_matrix();
        let queries = [
            (
                "SELECT ST_Reclass(rast, '0:1,7:2,8:3,22:5,255:4', '8BUI') \
                 FROM pgaccel_raster_matrix",
                PixelType::UInt8,
            ),
            (
                "SELECT ST_Reclass(rast, '0:100,7:-200,8:300,22:500,255:400', \
                                   '16BSI') FROM pgaccel_raster_matrix",
                PixelType::Int16,
            ),
        ];
        let mut width_results = Vec::new();
        for (sql, output_type) in queries {
            let native = raster_wkb_rows(sql);
            let forced = super::with_forced_raster_path(|| raster_wkb_cursor_rows(sql));
            assert_eq!(forced.len(), 5, "forced cursor must return every input row");
            assert_eq!(
                forced, native,
                "forced raster rows must preserve byte/order parity"
            );
            assert!(forced[0].is_none(), "first insertion-ordered row is NULL");
            assert!(forced[1..].iter().all(Option::is_some));

            let zero_band = parse_resident_raster(forced[1].as_deref().expect("zero-band WKB"))
                .expect("zero-band output parses");
            assert_eq!(zero_band.header.num_bands, 0);
            let nodata = parse_resident_raster(forced[3].as_deref().expect("nodata WKB"))
                .expect("nodata output parses");
            assert!(
                !nodata.bands[0].has_nodata,
                "three-argument ST_Reclass maps source nodata as an ordinary pixel"
            );
            let multiband = parse_resident_raster(forced[4].as_deref().expect("multiband WKB"))
                .expect("multiband output parses");
            assert_eq!(multiband.header.num_bands, 2);
            assert_eq!(multiband.bands[0].pixel_type, output_type);
            assert_eq!(multiband.bands[1].pixel_type, PixelType::Int16);
            width_results.push(forced);
        }
        assert_eq!(
            width_results[0][1], width_results[1][1],
            "missing-band output must pass through unchanged for either width"
        );
        assert_ne!(
            width_results[0][2], width_results[1][2],
            "populated output must encode the selected pixel width"
        );
    }

    #[pg_test]
    fn forced_reclass_executor_rewind_reproves() {
        setup_raster_matrix();
        let sql = "SELECT ST_Reclass( \
                     rast, '0:1,7:2,8:3,22:5,255:4', '8BUI') \
                   FROM pgaccel_raster_matrix";
        let native = raster_wkb_rows(sql);
        crate::gpu::reset_gpu_exec_count();
        let cleanup_before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        super::with_forced_raster_path(|| {
            Spi::run(&format!(
                "DECLARE pgaccel_raster_rescan NO SCROLL CURSOR FOR {sql}"
            ))
            .expect("declare forced raster rescan cursor");
        });
        let first = raster_wkb_rows("FETCH FORWARD ALL FROM pgaccel_raster_rescan");
        assert_eq!(first, native, "first cursor pass must match native output");
        let cleanup_before_rewind = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(
            cleanup_before_rewind.installed,
            cleanup_before.installed + 1
        );

        // SAFETY: the named cursor is live in this pg_test transaction and the
        // helper validates its QueryDesc and top CustomScanState before rewind.
        unsafe { rewind_named_raster_cursor(c"pgaccel_raster_rescan") };
        let second = raster_wkb_rows("FETCH FORWARD ALL FROM pgaccel_raster_rescan");
        assert_eq!(second, first, "ExecReScan must rewind every raster row");
        assert!(
            raster_wkb_rows("FETCH FORWARD ALL FROM pgaccel_raster_rescan").is_empty(),
            "rewound cursor must reach stable EOF"
        );
        assert_eq!(
            crate::engine::ffi::custom_scan::test_executor_cleanup_counts(),
            cleanup_before_rewind,
            "ExecutorRewind must reset in place without dropping the resident executor"
        );
        crate::gpu::assert_gpu_executed(1);
        Spi::run("CLOSE pgaccel_raster_rescan").expect("close forced raster rescan cursor");
        let cleanup_after_close = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(
            cleanup_after_close.normal_end,
            cleanup_before_rewind.normal_end + 1,
            "portal close must release the executor through EndCustomScan"
        );
        assert_eq!(
            cleanup_after_close.query_reset, cleanup_before_rewind.query_reset,
            "normal portal close must not require abort cleanup"
        );
    }

    #[pg_test]
    fn forced_null_only_reclass_clears_the_virtual_slot_without_an_output_context() {
        setup_extension_and_fixture();
        Spi::run(
            "CREATE TEMP TABLE pgaccel_raster_null_only(id int4, rast raster); \
             INSERT INTO pgaccel_raster_null_only \
             SELECT g, NULL::raster FROM generate_series(1, 256) AS g; \
             ANALYZE pgaccel_raster_null_only; \
             SELECT pg_accel_pin( \
               'pgaccel_raster_null_only'::regclass, ARRAY['rast'])",
        )
        .expect("create and pin NULL-only raster fixture");
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') \
                   FROM pgaccel_raster_null_only";
        super::with_forced_raster_path(|| {
            Spi::run(&format!(
                "DECLARE pgaccel_raster_null_cursor NO SCROLL CURSOR FOR {sql}"
            ))
            .expect("declare forced NULL-only raster cursor");
        });
        for batch in 0..8 {
            let rows = raster_wkb_rows("FETCH FORWARD 32 FROM pgaccel_raster_null_cursor");
            assert_eq!(rows.len(), 32, "NULL-only cursor batch {batch}");
            assert!(rows.iter().all(Option::is_none));
            assert_eq!(
                raster_output_memory_stats(),
                RasterOutputMemoryStats {
                    contexts: 0,
                    total_bytes: 0,
                    used_bytes: 0,
                },
                "NULL rows must clear and reuse the slot without allocating an importer context"
            );
        }
        assert!(raster_wkb_rows("FETCH FORWARD 1 FROM pgaccel_raster_null_cursor").is_empty());
        assert!(
            raster_wkb_rows("FETCH FORWARD 1 FROM pgaccel_raster_null_cursor").is_empty(),
            "NULL-only cursor EOF must remain stable"
        );
        Spi::run("CLOSE pgaccel_raster_null_cursor").expect("close NULL-only raster cursor");
        assert_eq!(
            raster_output_memory_stats(),
            RasterOutputMemoryStats {
                contexts: 0,
                total_bytes: 0,
                used_bytes: 0,
            }
        );
    }

    #[pg_test]
    fn forced_reclass_output_memory_is_bounded_across_cursor_rescan_and_end() {
        setup_raster_memory_fixture();
        let sql = "SELECT ST_Reclass( \
                     rast, '0:8,1:9,2:10,3:11,4:12,5:13,6:14,7:15', '8BUI') \
                   FROM pgaccel_raster_memory";
        assert_eq!(
            raster_output_memory_stats(),
            RasterOutputMemoryStats {
                contexts: 0,
                total_bytes: 0,
                used_bytes: 0,
            },
            "no raster output context may predate executor tuple emission"
        );
        super::with_forced_raster_path(|| {
            Spi::run(&format!(
                "DECLARE pgaccel_raster_memory_cursor NO SCROLL CURSOR FOR {sql}"
            ))
            .expect("declare forced raster memory cursor");
        });

        let first = raster_wkb_rows("FETCH FORWARD 64 FROM pgaccel_raster_memory_cursor");
        assert_eq!(first.len(), 64);
        let row_bytes = i64::try_from(
            first
                .iter()
                .filter_map(Option::as_ref)
                .map(Vec::len)
                .max()
                .expect("memory fixture rows are non-NULL"),
        )
        .expect("one raster WKB length fits i64");
        let first_stats = raster_output_memory_stats();
        assert_eq!(first_stats.contexts, 1);
        assert!(first_stats.total_bytes > 0 && first_stats.used_bytes > 0);

        // AllocSet may retain one block sized for the largest single output,
        // but resetting before each row must prevent batch-count growth.
        let total_limit = first_stats
            .total_bytes
            .saturating_add(row_bytes.saturating_mul(4))
            .saturating_add(128 * 1024);
        let used_limit = first_stats
            .used_bytes
            .saturating_add(row_bytes.saturating_mul(2))
            .saturating_add(64 * 1024);
        let mut max_stats = first_stats;
        for batch in 1..12 {
            let rows = raster_wkb_rows("FETCH FORWARD 64 FROM pgaccel_raster_memory_cursor");
            assert_eq!(rows.len(), 64, "cursor batch {batch} must be complete");
            let stats = raster_output_memory_stats();
            assert_eq!(stats.contexts, 1);
            assert!(
                stats.total_bytes <= total_limit,
                "raster output total bytes grew with cursor batches: \
                 first={first_stats:?} current={stats:?} limit={total_limit}"
            );
            assert!(
                stats.used_bytes <= used_limit,
                "raster output used bytes grew with cursor batches: \
                 first={first_stats:?} current={stats:?} limit={used_limit}"
            );
            max_stats.total_bytes = max_stats.total_bytes.max(stats.total_bytes);
            max_stats.used_bytes = max_stats.used_bytes.max(stats.used_bytes);
        }
        assert!(
            max_stats.total_bytes <= total_limit && max_stats.used_bytes <= used_limit,
            "many emitted rows must retain only one-row-scale output memory"
        );

        let before_rewind = raster_output_memory_stats();
        // SAFETY: the named portal is live and the helper validates its
        // QueryDesc and top-level CustomScanState before ExecutorRewind.
        unsafe { rewind_named_raster_cursor(c"pgaccel_raster_memory_cursor") };
        let after_rewind = raster_output_memory_stats();
        assert_eq!(after_rewind.contexts, 1);
        assert!(
            after_rewind.used_bytes < before_rewind.used_bytes,
            "ReScan must clear the slot before resetting its output context: \
             before={before_rewind:?} after={after_rewind:?}"
        );
        assert!(
            after_rewind.used_bytes <= 8 * 1024,
            "rewound output context must be empty apart from allocator bookkeeping: \
             {after_rewind:?}"
        );
        let replay = raster_wkb_rows("FETCH FORWARD 64 FROM pgaccel_raster_memory_cursor");
        assert_eq!(replay, first, "rewound raster batch must remain bit-exact");
        let replay_stats = raster_output_memory_stats();
        assert!(replay_stats.total_bytes <= total_limit);
        assert!(replay_stats.used_bytes <= used_limit);

        Spi::run("CLOSE pgaccel_raster_memory_cursor").expect("close forced raster memory cursor");
        assert_eq!(
            raster_output_memory_stats(),
            RasterOutputMemoryStats {
                contexts: 0,
                total_bytes: 0,
                used_bytes: 0,
            },
            "EndCustomScan must delete the dedicated raster output context"
        );
    }

    #[pg_test]
    fn prepared_reclass_refreshes_after_dml() {
        setup_raster_matrix();
        let sql = "SELECT ST_Reclass( \
                     rast, '0:1,7:2,8:3,22:5,255:4', '8BUI') \
                   FROM pgaccel_raster_matrix";
        let native_before = raster_wkb_rows(sql);
        super::with_forced_raster_path(|| {
            Spi::run(&format!("PREPARE pgaccel_raster_prepared AS {sql}"))
                .expect("prepare exact raster query");
        });
        let first =
            super::with_forced_raster_path(|| raster_wkb_rows("EXECUTE pgaccel_raster_prepared"));
        assert_eq!(first, native_before);
        let cached_plan = explain("EXECUTE pgaccel_raster_prepared");
        assert!(
            cached_plan.contains("GpuAccelRaster"),
            "prepared statement must retain its forced raster plan:\n{cached_plan}"
        );
        let (raw_before, derived_before, generation_before) =
            raster_resident_status("pgaccel_raster_matrix");
        assert!(raw_before > 0 && derived_before > 0);

        Spi::run(
            "UPDATE pgaccel_raster_matrix \
             SET rast = ST_SetValue(rast, 1, 1, 1, 22) WHERE id = 3",
        )
        .expect("mutate prepared raster input");
        let (invalid_raw, invalid_derived, invalid_generation) =
            raster_resident_status("pgaccel_raster_matrix");
        assert_eq!((invalid_raw, invalid_derived), (0, 0));
        assert!(invalid_generation > generation_before);

        let native_after = raster_wkb_rows(sql);
        assert_ne!(
            native_after, native_before,
            "DML must change the exact output"
        );
        crate::gpu::reset_gpu_exec_count();
        let refreshed = raster_wkb_rows("EXECUTE pgaccel_raster_prepared");
        assert_eq!(
            refreshed, native_after,
            "prepared CustomScan must reprove and reload the new generation"
        );
        crate::gpu::assert_gpu_executed(1);
        let (raw_after, derived_after, generation_after) =
            raster_resident_status("pgaccel_raster_matrix");
        assert!(raw_after > 0 && derived_after > 0);
        assert!(generation_after >= invalid_generation);
        Spi::run("DEALLOCATE pgaccel_raster_prepared").expect("deallocate raster statement");
    }

    #[pg_test]
    fn planned_catalog_oid_mismatch_is_hard_error() {
        setup_extension_and_fixture();
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') \
                   FROM pgaccel_raster_observer";
        let native = raster_wkb_rows(sql);
        Spi::run(
            "SELECT pg_accel_pin( \
               'pgaccel_raster_observer'::regclass, ARRAY['rast'])",
        )
        .expect("pin catalog mismatch fixture");
        crate::gpu::reset_gpu_exec_count();
        let (sqlstate, message) = super::with_tampered_raster_catalog_oid(|| {
            super::with_forced_raster_path(|| capture_sql_error(sql))
        })
        .expect("tampered catalog OID must raise a hard execution error");
        assert_eq!(sqlstate, "XX000");
        assert!(
            message.contains("RastFromWkbFunctionChanged"),
            "unexpected catalog revalidation error: {message}"
        );
        assert_eq!(crate::gpu::gpu_exec_count(), 0);
        assert_eq!(raster_derived_bytes(), 0);
        assert_eq!(
            raster_wkb_rows(sql),
            native,
            "native execution remains valid"
        );
    }

    #[pg_test]
    fn injected_raster_failure_has_no_native_fallback() {
        use crate::gpu::{GpuFailureDomain, kernel_failure_count};

        setup_extension_and_fixture();
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') \
                   FROM pgaccel_raster_observer";
        let native = raster_wkb_rows(sql);
        Spi::run(
            "SELECT pg_accel_pin( \
               'pgaccel_raster_observer'::regclass, ARRAY['rast'])",
        )
        .expect("pin injected failure fixture");
        let failures_before = kernel_failure_count(GpuFailureDomain::Raster);
        crate::gpu::reset_gpu_exec_count();
        let (sqlstate, message) =
            crate::engine::executor::raster::with_test_raster_kernel_failure(|| {
                super::with_forced_raster_path(|| capture_sql_error(sql))
            })
            .expect("injected raster status must raise a hard execution error");
        assert_eq!(sqlstate, "XX000");
        assert!(
            message.contains(
                "GPU raster kernel(raster_reclass_resident) failed with execution_failed"
            ),
            "unexpected injected raster error: {message}"
        );
        crate::gpu::assert_gpu_executed(1);
        assert_eq!(
            kernel_failure_count(GpuFailureDomain::Raster),
            failures_before + 1,
            "the injected raw failure must be counted exactly once"
        );
        assert_eq!(
            raster_derived_bytes(),
            0,
            "a failed raster transform must not publish an artifact"
        );
        assert_eq!(
            raster_wkb_rows(sql),
            native,
            "native execution remains valid"
        );
    }

    #[pg_test]
    fn malformed_forced_import_restores_memory_context_without_native_fallback() {
        setup_extension_and_fixture();
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') \
                   FROM pgaccel_raster_observer";
        let native = raster_wkb_rows(sql);
        Spi::run(
            "SELECT pg_accel_pin( \
               'pgaccel_raster_observer'::regclass, ARRAY['rast'])",
        )
        .expect("pin malformed importer fixture");
        crate::gpu::reset_gpu_exec_count();
        // SAFETY: pg_test runs synchronously on the backend main thread.
        let context_before = unsafe { pg_sys::CurrentMemoryContext };
        assert!(!context_before.is_null());
        let cleanup_before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        let (sqlstate, message) =
            crate::engine::executor::raster::with_test_raster_import_wkb_corruption(|| {
                super::with_forced_raster_path(|| capture_sql_error(sql))
            })
            .expect("malformed forced import must raise a hard execution error");
        // SAFETY: the caught SQL error returned control to the same pg_test
        // backend callback, where CurrentMemoryContext must be restored.
        let context_after = unsafe { pg_sys::CurrentMemoryContext };
        assert_eq!(context_after, context_before);
        let cleanup_after = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(cleanup_after.installed, cleanup_before.installed + 1);
        assert_eq!(cleanup_after.normal_end, cleanup_before.normal_end);
        assert_eq!(
            cleanup_after.query_reset,
            cleanup_before.query_reset + 1,
            "raster importer ERROR must release the executor through query-context reset"
        );
        assert_eq!(sqlstate, "XX000");
        assert_eq!(
            message,
            "pg_accel: raster execution failed: PostGIS st_rastfromwkb raised ERROR: \
             rt_raster_from_wkb: wkb size (1) < min size (61)"
        );
        crate::gpu::assert_gpu_executed(1);
        assert!(
            raster_derived_bytes() > 0,
            "the real GPU artifact must precede the importer-only corruption"
        );
        assert_eq!(
            raster_output_memory_stats(),
            RasterOutputMemoryStats {
                contexts: 0,
                total_bytes: 0,
                used_bytes: 0,
            },
            "failed query recovery must release the named raster output context"
        );
        assert_eq!(
            Spi::get_one::<i32>("SELECT 42").expect("backend remains usable"),
            Some(42)
        );
        assert_eq!(
            raster_wkb_rows(sql),
            native,
            "malformed forced import must never fall through to native execution"
        );
    }

    #[pg_test]
    fn zero_grid_present_band_declines_before_dispatch() {
        use crate::adapters::extractors::raster::parse_resident_raster;

        setup_zero_grid_band_fixture();
        let sql = "SELECT ST_Reclass(rast, '0:1', '8BUI') \
                   FROM pgaccel_raster_zero_grid";
        let native = raster_wkb_rows(sql);
        assert_eq!(native.len(), 1);
        let parsed = parse_resident_raster(native[0].as_deref().expect("native zero-grid WKB"))
            .expect("native zero-grid output parses");
        assert_eq!((parsed.header.width, parsed.header.height), (0, 0));
        assert_eq!(parsed.header.num_bands, 1);

        assert_raster_decline(sql, "raster_zero_grid_wkb_non_roundtrippable");
        crate::gpu::reset_gpu_exec_count();
        let forced = super::with_forced_raster_path(|| explain(sql));
        assert!(
            !forced.contains("GpuAccel") && !forced.contains("Custom Scan"),
            "the pg_test force guard must not bypass the zero-grid gate:\n{forced}"
        );
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            0,
            "zero-grid present-band decline must happen before dispatch"
        );

        let (sqlstate, importer_error) = capture_sql_error(
            "SELECT ST_RastFromWKB(ST_AsWKB( \
               ST_Reclass(rast, '0:1', '8BUI'))) \
             FROM pgaccel_raster_zero_grid",
        )
        .expect("PostGIS must reject its own zero-grid present-band WKB round trip");
        assert_eq!(sqlstate, "XX000");
        assert!(
            importer_error.contains("Premature end of WKB on band novalue reading"),
            "unexpected PostGIS zero-grid importer error: {importer_error}"
        );
    }

    #[pg_test]
    fn exact_calls_without_residency_and_summary_stats_decline_precisely() {
        setup_extension_and_fixture();
        assert_raster_decline(
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer",
            "raster_resident_metadata_unavailable",
        );
        assert_raster_decline(
            "SELECT ST_SummaryStats(rast, 1, true) FROM pgaccel_raster_observer",
            "raster_summarystats_bit_exact_unavailable",
        );
        assert_raster_decline(
            "SELECT ST_SummaryStats(rast, true) FROM pgaccel_raster_observer",
            "raster_summarystats_bit_exact_unavailable",
        );
    }

    #[pg_test]
    fn target_wrappers_and_every_cardinality_modifier_decline_as_shape() {
        setup_extension_and_fixture();
        for sql in [
            "SELECT ST_Reclass(rast, '0:1', '8BUI'), id FROM pgaccel_raster_observer",
            "SELECT ST_Reclass(rast, '0:1', '8BUI')::text FROM pgaccel_raster_observer",
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer WHERE id = 1",
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer ORDER BY id",
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer LIMIT 1",
        ] {
            assert_raster_decline(sql, "raster_unsupported_shape");
        }
    }

    #[pg_test]
    fn replacement_schema_same_name_spoof_never_reaches_admission() {
        setup_extension_and_fixture();
        Spi::run(
            "DROP SCHEMA IF EXISTS pgaccel_raster_spoof CASCADE; \
             CREATE SCHEMA pgaccel_raster_spoof; \
             CREATE FUNCTION pgaccel_raster_spoof.st_reclass(raster, text, text) \
             RETURNS raster LANGUAGE sql IMMUTABLE STRICT AS 'SELECT $1'; \
             SET search_path = pgaccel_raster_spoof, public",
        )
        .expect("create same-name raster spoof");
        assert_raster_decline(
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_observer",
            "raster_catalog_proof_failed",
        );
        Spi::run("SET search_path = public").expect("restore search_path");
    }

    #[pg_test]
    fn row_security_is_a_structural_decline() {
        setup_extension_and_fixture();
        Spi::run(
            "DROP TABLE IF EXISTS public.pgaccel_raster_rls; \
             DROP ROLE IF EXISTS pgaccel_raster_rls_reader; \
             CREATE ROLE pgaccel_raster_rls_reader; \
             CREATE TABLE public.pgaccel_raster_rls AS \
               SELECT * FROM pgaccel_raster_observer; \
             ALTER TABLE public.pgaccel_raster_rls ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE public.pgaccel_raster_rls FORCE ROW LEVEL SECURITY; \
             CREATE POLICY pgaccel_raster_rls_policy ON public.pgaccel_raster_rls \
               FOR SELECT USING (true); \
             GRANT SELECT ON public.pgaccel_raster_rls TO pgaccel_raster_rls_reader; \
             SELECT pg_accel_reset_stats(); \
             SET ROLE pgaccel_raster_rls_reader",
        )
        .expect("create RLS raster fixture");
        let plan = explain("SELECT ST_Reclass(rast, '0:1', '8BUI') FROM public.pgaccel_raster_rls");
        Spi::run("RESET ROLE").expect("restore test role");
        assert!(
            !plan.contains("Custom Scan"),
            "RLS plan must be native:\n{plan}"
        );
        assert_eq!(reason_count("raster_unsupported_shape"), 1);
        for reason in RASTER_REASONS {
            if *reason != "raster_unsupported_shape" {
                assert_eq!(reason_count(reason), 0);
            }
        }
        Spi::run(
            "DROP TABLE public.pgaccel_raster_rls; \
             DROP ROLE pgaccel_raster_rls_reader",
        )
        .expect("drop RLS raster fixture");
    }

    #[pg_test]
    fn resident_missing_band_and_zero_pixel_present_band_are_distinct() {
        setup_extension_and_fixture();
        Spi::run(
            "CREATE TEMP TABLE pgaccel_raster_band_missing(rast raster); \
             CREATE TEMP TABLE pgaccel_raster_band_present(rast raster); \
             INSERT INTO pgaccel_raster_band_missing VALUES \
               (ST_MakeEmptyRaster(0, 0, 0, 0, 1, -1, 0, 0, 4326)); \
             INSERT INTO pgaccel_raster_band_present VALUES \
               (ST_AddBand( \
                  ST_MakeEmptyRaster(0, 0, 0, 0, 1, -1, 0, 0, 4326), \
                  '8BUI'::text, 0, 0)); \
             SELECT pg_accel_pin( \
               'pgaccel_raster_band_missing'::regclass, ARRAY['rast']); \
             SELECT pg_accel_pin( \
               'pgaccel_raster_band_present'::regclass, ARRAY['rast'])",
        )
        .expect("pin zero-pixel band-presence fixture");
        assert_raster_decline(
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_band_missing",
            "raster_selected_band_missing",
        );
        assert_raster_decline(
            "SELECT ST_Reclass(rast, '0:1', '8BUI') FROM pgaccel_raster_band_present",
            "raster_zero_grid_wkb_non_roundtrippable",
        );
    }

    #[pg_test]
    fn replaced_extension_wrapper_changes_catalog_proof_before_shape_or_cost() {
        setup_extension_and_fixture();
        Spi::run(
            "CREATE OR REPLACE FUNCTION public.st_reclass( \
               rast public.raster, reclassexpr text, pixeltype text) \
             RETURNS public.raster LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS \
             'SELECT $1'",
        )
        .expect("replace PostGIS wrapper inside pg_test transaction");
        assert_raster_decline(
            "SELECT public.ST_Reclass(rast, '0:1', '8BUI') \
             FROM pgaccel_raster_observer",
            "raster_catalog_proof_failed",
        );
    }
}

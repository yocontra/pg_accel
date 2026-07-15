//! Exact RQS2 raster planner observer.
//!
//! This module recognizes replacement-sensitive PostGIS Raster calls and
//! records deterministic declines. It deliberately has no path-construction
//! API: Phase 6 costing is uncalibrated and runtime selection remains dark.

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

use super::{RejectionReason, find_cheapest_path};

const RECLASS_NAME: &str = "st_reclass";
const SUMMARY_STATS_NAME: &str = "st_summarystats";

fn list_len(list: *mut pg_sys::List) -> usize {
    if list.is_null() {
        0
    } else {
        usize::try_from(unsafe { pg_sys::list_length(list) }).unwrap_or(usize::MAX)
    }
}

unsafe fn list_item<T>(list: *mut pg_sys::List, index: usize) -> Option<*mut T> {
    if index >= list_len(list) {
        return None;
    }
    let index = i32::try_from(index).ok()?;
    let item = unsafe { pg_sys::list_nth(list, index) }.cast::<T>();
    (!item.is_null()).then_some(item)
}

unsafe fn function_name(function_oid: pg_sys::Oid) -> Option<String> {
    if function_oid == pg_sys::InvalidOid {
        return None;
    }
    let name = unsafe { pg_sys::get_func_name(function_oid) };
    if name.is_null() {
        return None;
    }
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
    let (function, wrapped) = match unsafe { (*node).type_ } {
        NodeTag::T_FuncExpr => (node.cast::<pg_sys::FuncExpr>(), false),
        NodeTag::T_RelabelType => unsafe {
            let inner = (*node.cast::<pg_sys::RelabelType>()).arg.cast::<Node>();
            let (function, _) = raster_name_prefilter(inner)?;
            (function, true)
        },
        NodeTag::T_CoerceViaIO => unsafe {
            let inner = (*node.cast::<pg_sys::CoerceViaIO>()).arg.cast::<Node>();
            let (function, _) = raster_name_prefilter(inner)?;
            (function, true)
        },
        NodeTag::T_CoerceToDomain => unsafe {
            let inner = (*node.cast::<pg_sys::CoerceToDomain>()).arg.cast::<Node>();
            let (function, _) = raster_name_prefilter(inner)?;
            (function, true)
        },
        NodeTag::T_CollateExpr => unsafe {
            let inner = (*node.cast::<pg_sys::CollateExpr>()).arg.cast::<Node>();
            let (function, _) = raster_name_prefilter(inner)?;
            (function, true)
        },
        _ => return None,
    };
    let name = unsafe { function_name((*function).funcid) };
    name.as_deref()
        .is_some_and(|name| matches!(name, RECLASS_NAME | SUMMARY_STATS_NAME))
        .then_some((function, wrapped))
}

unsafe fn raster_target_function(query: &pg_sys::Query) -> Option<RasterNamePrefilterTarget> {
    let mut nonjunk = 0_usize;
    let mut candidate: Option<(*mut pg_sys::FuncExpr, bool)> = None;
    for index in 0..list_len(query.targetList) {
        let target = unsafe { list_item::<pg_sys::TargetEntry>(query.targetList, index) }?;
        if unsafe { (*target).resjunk } {
            continue;
        }
        nonjunk = nonjunk.checked_add(1)?;
        let expression = unsafe { (*target).expr.cast::<Node>() };
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
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    Some(unsafe { *node.cast::<pg_sys::Const>() })
}

unsafe fn text_const(node: *mut Node) -> Option<String> {
    let constant = unsafe { direct_const(node) }?;
    if constant.consttype != pg_sys::TEXTOID
        || constant.consttypmod != -1
        || constant.constlen != -1
        || constant.constbyval
        || constant.constisnull
    {
        return None;
    }
    unsafe { String::from_datum(constant.constvalue, false) }
}

unsafe fn int4_const(node: *mut Node) -> Option<i32> {
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
    Some(unsafe { pg_sys::DatumGetInt32(constant.constvalue) })
}

unsafe fn bool_const(node: *mut Node) -> Option<bool> {
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
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Var {
        return None;
    }
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
        || unsafe { pg_sys::get_atttype(rte.relid, var.varattno) } != raster_type_oid
    {
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
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        let expression_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        let pixel_type_node = unsafe { list_item::<Node>(function.args, 2) }.ok_or(())?;
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        return Ok(RasterCallShape::ReclassTextText {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
            expression: unsafe { text_const(expression_node) }.ok_or(())?,
            pixel_type: unsafe { text_const(pixel_type_node) }.ok_or(())?,
        });
    }
    if function.funcid == identity.summary_stats_fn_oid {
        if list_len(function.args) != 3
            || function.funcresulttype != identity.summary_stats_type_oid
        {
            return Err(());
        }
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        let band_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        let exclude_node = unsafe { list_item::<Node>(function.args, 2) }.ok_or(())?;
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        let band = u32::try_from(unsafe { int4_const(band_node) }.ok_or(())?).map_err(|_| ())?;
        return Ok(RasterCallShape::SummaryStatsBand {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
            band,
            exclude_nodata: unsafe { bool_const(exclude_node) }.ok_or(())?,
        });
    }
    if function.funcid == identity.summary_stats_default_band_fn_oid {
        if list_len(function.args) != 2
            || function.funcresulttype != identity.summary_stats_type_oid
        {
            return Err(());
        }
        let raster_node = unsafe { list_item::<Node>(function.args, 0) }.ok_or(())?;
        let exclude_node = unsafe { list_item::<Node>(function.args, 1) }.ok_or(())?;
        let raster = unsafe { direct_raster_var(raster_node, rti, rte, identity.raster_type_oid) }
            .ok_or(())?;
        return Ok(RasterCallShape::SummaryStatsDefaultBand {
            relation_oid: raster.relation_oid,
            raster_attno: raster.attno,
            raster_type_oid: raster.type_oid,
            function_oid: u32::from(function.funcid),
            result_type_oid: u32::from(function.funcresulttype),
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
        || list_len(unsafe { (*query.jointree).fromlist }) != 1
    {
        return RasterRelationShape::Unsupported;
    }
    let Some(range_ref) = (unsafe { list_item::<Node>((*query.jointree).fromlist, 0) }) else {
        return RasterRelationShape::Unsupported;
    };
    if unsafe { (*range_ref).type_ } != NodeTag::T_RangeTblRef
        || unsafe { (*range_ref.cast::<pg_sys::RangeTblRef>()).rtindex } != rti_i32
    {
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

fn record(reason: RejectionReason, rows: u64) {
    stats::increment_planner_rejected(reason.stats_key(), rows);
    stats::record_planner_fast_decline(reason.stats_key());
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
    let identity = unsafe { resolve_postgis_raster_catalog() }
        .map_err(|_| RejectionReason::RasterCatalogProofFailed)?;
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
        relation: unsafe { relation_shape(query, rel, rti, rte) },
        features: unsafe { query_features(query, rel, rte) },
        nonjunk_target_count: prefilter.nonjunk_count,
        call,
        estimated_rows: rows_estimate(rel.rows),
    };
    build_raster_query_spec(shape, &catalog).map_err(|decline| planner_decline_reason(&decline))
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
    let query_ptr = unsafe { (*root).parse };
    if query_ptr.is_null() {
        return false;
    }
    let query = unsafe { &*query_ptr };
    let rel_ref = unsafe { &*rel };
    let rte_ref = unsafe { &*rte };
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
    let candidate = match unsafe { validated_candidate(query, rel_ref, rti, rte_ref, prefilter) } {
        Ok(candidate) => candidate,
        Err(reason) => {
            record(reason, rows);
            return true;
        }
    };
    let cheapest = unsafe { find_cheapest_path(rel_ref.pathlist) };
    let native_total_cost = if cheapest.is_null() {
        PgCost::new(0.0)
    } else {
        PgCost::new(unsafe { (*cheapest).total_cost })
    };
    let cost = estimate_raster_cost(
        RasterCostInput {
            work: exact_resident_work(&candidate.spec),
            native_total_cost,
        },
        device_limits(),
    );
    record(cost_decline_reason(cost.gate), rows);
    true
}

#[cfg(test)]
mod tests {
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
    fn cost_gate_mapping_never_produces_an_acceptance() {
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
mod live_tests {
    use pgrx::prelude::*;

    const RASTER_REASONS: &[&str] = &[
        "raster_unsupported_shape",
        "raster_catalog_proof_failed",
        "raster_summarystats_bit_exact_unavailable",
        "raster_resident_metadata_unavailable",
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

    fn explain(sql: &str) -> String {
        Spi::connect(|client| {
            client
                .select(&format!("EXPLAIN (COSTS OFF) {sql}"), None, &[])
                .expect("EXPLAIN raster candidate")
                .into_iter()
                .filter_map(|row| row.get::<String>(1).expect("read EXPLAIN row"))
                .collect::<Vec<_>>()
                .join("\n")
        })
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
            "raster_cost_uncalibrated",
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

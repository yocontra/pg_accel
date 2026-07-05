//! Planner recognizer for resident dense grouped OLAP aggregates.

use std::ffi::CStr;

use pgrx::pg_sys::{self, List, NodeTag, PlannerInfo, RelOptInfo};

use super::rel_rows_estimate;
use super::resident_groupagg_path::{
    ResidentGroupAggPathShape, inject_childless_resident_groupagg_path,
};
use crate::adapters::extractors::array::{PgArray, parse_array};
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::{
    OlapAggSpec, RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES,
    ResidentDenseGroupedF64FilterMode, ResidentDenseGroupedF64Layout,
    ResidentDenseGroupedF64MeasurePredicate, ResidentDenseGroupedF64MeasurePredicateOp,
    ResidentDenseGroupedF64MeasurePredicateSource, ResidentDenseGroupedF64Source,
    ResidentDenseGroupedF64Spec, ResidentGroupAggLogicalSpec,
};
use crate::engine::olap_cache::{self, ResidentDenseGroupAggCacheShape, ResidentMeasureOp};
use crate::engine::residency::ResidentOperatorStage;
use crate::engine::stats;

const RESIDENT_DENSE_GROUPED_F64_SCALAR_ARRAY_MAX_VALUES: usize = 64;
const FILTERED_GROUPAGG_WHERE_MIN_INPUT_ROWS: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VarRef {
    varno: pg_sys::Index,
    attno: i32,
}

struct RecognizedDenseAgg {
    relid: pg_sys::Index,
    rel_oid: pg_sys::Oid,
    source: ResidentDenseGroupedF64Source,
    layout: ResidentDenseGroupedF64Layout,
    logical: ResidentGroupAggLogicalSpec,
    measure_op: ResidentMeasureOp,
    requires_rhs: bool,
    has_filter: bool,
    filter_mode: ResidentDenseGroupedF64FilterMode,
    measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
}

struct RecognizedTargetLayout {
    layout: ResidentDenseGroupedF64Layout,
    has_aggregate_filter: bool,
    has_measure_predicate: bool,
    measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
}

/// Try to inject a childless resident dense grouped f64 aggregate.
///
/// Returns true when this module either injects a cache-shape-driven resident
/// aggregate path or recognizes a matching SQL shape whose backend-local cache
/// disappeared before path injection. The latter keeps host-staged aggregate
/// paths from claiming resident-only SQL lanes.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let Some(spec) = (unsafe { recognize(root) }) else {
        return false;
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    if selective_where_filtered_groupagg_needs_larger_floor(&spec)
        && rows_est < FILTERED_GROUPAGG_WHERE_MIN_INPUT_ROWS
    {
        stats::increment_planner_rejected(
            "resident_groupagg_filtered_rows_below_selective_min",
            rows_est,
        );
        stats::record_planner_fast_decline(
            "upper_paths_resident_groupagg_filtered_rows_below_selective_min",
        );
        pgrx::debug1!(
            "pg_accel: upper_paths resident groupagg decline: relid={} rows_est={} below \
             selective WHERE-filter groupagg floor {}",
            spec.relid,
            rows_est,
            FILTERED_GROUPAGG_WHERE_MIN_INPUT_ROWS,
        );
        return true;
    }

    if !olap_cache::resident_dense_groupagg_cache_loaded_for_shape(
        spec.rel_oid,
        spec.measure_op,
        spec.requires_rhs,
        spec.has_filter,
    ) {
        stats::increment_planner_rejected("recognized_resident_groupagg_no_cache", rows_est);
        stats::record_planner_fast_decline("upper_paths_resident_groupagg_no_cache");
        pgrx::debug1!(
            "pg_accel: upper_paths resident groupagg decline: recognized relid={} but \
             backend-local resident cache with the required shape is not loaded",
            spec.relid,
        );
        return true;
    }

    if output_rel.is_null() {
        return true;
    }

    let olap_spec = OlapAggSpec::ResidentDenseGroupedF64(ResidentDenseGroupedF64Spec {
        rel_oid: spec.rel_oid,
        source: spec.source,
        layout: spec.layout,
        logical: spec.logical,
        measure_op: spec.measure_op,
        requires_rhs: spec.requires_rhs,
        filter_mode: spec.filter_mode,
        measure_predicate: spec.measure_predicate,
    });

    let mut stages = vec![ResidentOperatorStage::Scan];
    if spec.requires_rhs
        || matches!(
            spec.layout,
            ResidentDenseGroupedF64Layout::GroupSumAvgCount
                | ResidentDenseGroupedF64Layout::GroupMinMaxAvg
        )
    {
        stages.push(ResidentOperatorStage::Expression);
    }
    stages.push(ResidentOperatorStage::GroupedAggregate);
    stages.push(ResidentOperatorStage::FinalMaterialization);
    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_resident_dense_groupagg",
                olap_spec,
                agg_op: AggOp::Sum,
                result_oid: pg_sys::FLOAT8OID,
                output_rows: if spec.layout.is_single_group() {
                    1.0
                } else {
                    (*output_rel).rows.max(1.0)
                },
                cost_per_row: 0.00004,
                stages,
                device_columns: 2 + usize::from(spec.requires_rhs) + usize::from(spec.has_filter),
                has_filter: spec.has_filter,
            },
        );
    }
    true
}

fn selective_where_filtered_groupagg_needs_larger_floor(spec: &RecognizedDenseAgg) -> bool {
    spec.layout == ResidentDenseGroupedF64Layout::GroupSumAvgCount
        && spec.measure_op == ResidentMeasureOp::Column
        && !spec.requires_rhs
        && spec.filter_mode == ResidentDenseGroupedF64FilterMode::Where
        && spec.measure_predicate == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
}

unsafe fn recognize(root: *mut PlannerInfo) -> Option<RecognizedDenseAgg> {
    if root.is_null() || unsafe { (*root).parse }.is_null() {
        return None;
    }
    let query = unsafe { &*(*root).parse };
    if !query.hasAggs {
        return None;
    }
    if query.groupClause.is_null() || unsafe { pg_sys::list_length(query.groupClause) } == 0 {
        return unsafe { recognize_single_f64_reduce(query) };
    }
    if unsafe { pg_sys::list_length(query.groupClause) } != 1 {
        return None;
    }

    let group_var = (unsafe { single_group_var(query) })?;
    if group_var.attno <= 0 {
        return None;
    }
    let rel_oid = (unsafe { relation_oid_from_rtable(query.rtable, group_var.varno) })?;
    let cache_shape = olap_cache::resident_dense_groupagg_cache_shape(rel_oid)?;
    if group_var.attno != cache_shape.group_attno {
        return None;
    }
    let measure = DenseAggMeasure::from_cache_shape(cache_shape)?;
    let (filter_policy, target) =
        unsafe { recognize_target_for_cache(query, group_var, measure, cache_shape.filter_attno) }?;
    let logical = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
        target.layout,
        measure.op(),
        measure.requires_rhs(),
        filter_policy.mode(),
        target.measure_predicate,
    )?;

    Some(RecognizedDenseAgg {
        relid: group_var.varno,
        rel_oid,
        source: ResidentDenseGroupedF64Source::new(
            cache_shape.group_attno,
            cache_shape.value_attno,
            cache_shape.value_rhs_attno,
            cache_shape.filter_attno,
        ),
        layout: target.layout,
        logical,
        measure_op: measure.op(),
        requires_rhs: measure.requires_rhs(),
        has_filter: filter_policy.filter_attno().is_some(),
        filter_mode: filter_policy.mode(),
        measure_predicate: target.measure_predicate,
    })
}

unsafe fn recognize_single_f64_reduce(query: &pg_sys::Query) -> Option<RecognizedDenseAgg> {
    if !unsafe { query_has_no_quals(query) } {
        return None;
    }
    let (value_var, layout) = unsafe { single_f64_reduce_target(query.targetList) }?;
    if value_var.attno <= 0 {
        return None;
    }
    let rel_oid = unsafe { relation_oid_from_rtable(query.rtable, value_var.varno) }?;
    let cache_shape = olap_cache::resident_dense_groupagg_cache_shape(rel_oid)?;
    if cache_shape.group_attno != 0
        || cache_shape.value_attno != value_var.attno
        || cache_shape.value_rhs_attno.is_some()
        || cache_shape.filter_attno.is_some()
        || cache_shape.measure_op != ResidentMeasureOp::Column
    {
        return None;
    }
    let logical = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
        layout,
        ResidentMeasureOp::Column,
        false,
        ResidentDenseGroupedF64FilterMode::None,
        ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
    )?;

    Some(RecognizedDenseAgg {
        relid: value_var.varno,
        rel_oid,
        source: ResidentDenseGroupedF64Source::new(0, cache_shape.value_attno, None, None),
        layout,
        logical,
        measure_op: ResidentMeasureOp::Column,
        requires_rhs: false,
        has_filter: false,
        filter_mode: ResidentDenseGroupedF64FilterMode::None,
        measure_predicate: ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DenseAggFilterPolicy {
    None,
    Where { attno: i32 },
    Aggregate { attno: i32 },
    MeasurePredicate { attno: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenseAggMeasure {
    Column {
        attno: i32,
    },
    Binary {
        op: ResidentMeasureOp,
        left_attno: i32,
        right_attno: i32,
        commutative: bool,
    },
    StatsPair {
        sum_attno: i32,
        avg_attno: i32,
    },
}

impl DenseAggFilterPolicy {
    const fn mode(self) -> ResidentDenseGroupedF64FilterMode {
        match self {
            Self::None => ResidentDenseGroupedF64FilterMode::None,
            Self::Where { .. } => ResidentDenseGroupedF64FilterMode::Where,
            Self::Aggregate { .. } => ResidentDenseGroupedF64FilterMode::AggregateFilter,
            Self::MeasurePredicate { .. } => ResidentDenseGroupedF64FilterMode::MeasurePredicate,
        }
    }

    const fn filter_attno(self) -> Option<i32> {
        match self {
            Self::None => None,
            Self::Where { attno }
            | Self::Aggregate { attno }
            | Self::MeasurePredicate { attno, .. } => Some(attno),
        }
    }
}

impl DenseAggMeasure {
    fn from_cache_shape(shape: ResidentDenseGroupAggCacheShape) -> Option<Self> {
        match (shape.measure_op, shape.value_rhs_attno) {
            (ResidentMeasureOp::Column, None) => Some(Self::Column {
                attno: shape.value_attno,
            }),
            (ResidentMeasureOp::Mul | ResidentMeasureOp::Sub, Some(right_attno)) => {
                Some(Self::Binary {
                    op: shape.measure_op,
                    left_attno: shape.value_attno,
                    right_attno,
                    commutative: shape.measure_op == ResidentMeasureOp::Mul,
                })
            }
            (ResidentMeasureOp::StatsPair, Some(avg_attno)) => Some(Self::StatsPair {
                sum_attno: shape.value_attno,
                avg_attno,
            }),
            _ => None,
        }
    }

    const fn op(self) -> ResidentMeasureOp {
        match self {
            Self::Column { .. } => ResidentMeasureOp::Column,
            Self::Binary { op, .. } => op,
            Self::StatsPair { .. } => ResidentMeasureOp::StatsPair,
        }
    }

    const fn requires_rhs(self) -> bool {
        matches!(self, Self::Binary { .. } | Self::StatsPair { .. })
    }

    fn predicate_source_for_var(
        self,
        var: VarRef,
        varno: pg_sys::Index,
    ) -> Option<ResidentDenseGroupedF64MeasurePredicateSource> {
        match self {
            Self::Column { attno } => (var == VarRef { varno, attno })
                .then_some(ResidentDenseGroupedF64MeasurePredicateSource::Value),
            Self::Binary {
                left_attno,
                right_attno,
                ..
            } => {
                if var
                    == (VarRef {
                        varno,
                        attno: left_attno,
                    })
                {
                    Some(ResidentDenseGroupedF64MeasurePredicateSource::Value)
                } else if var
                    == (VarRef {
                        varno,
                        attno: right_attno,
                    })
                {
                    Some(ResidentDenseGroupedF64MeasurePredicateSource::Rhs)
                } else {
                    None
                }
            }
            Self::StatsPair {
                sum_attno,
                avg_attno,
            } => {
                if var
                    == (VarRef {
                        varno,
                        attno: sum_attno,
                    })
                {
                    Some(ResidentDenseGroupedF64MeasurePredicateSource::Value)
                } else if var
                    == (VarRef {
                        varno,
                        attno: avg_attno,
                    })
                {
                    Some(ResidentDenseGroupedF64MeasurePredicateSource::Rhs)
                } else {
                    None
                }
            }
        }
    }
}

unsafe fn recognize_target_for_cache(
    query: &pg_sys::Query,
    group_var: VarRef,
    measure: DenseAggMeasure,
    filter_attno: Option<i32>,
) -> Option<(DenseAggFilterPolicy, RecognizedTargetLayout)> {
    if unsafe { query_has_no_quals(query) }
        && let Some(target) =
            unsafe { target_layout(query.targetList, group_var, measure, None, None) }
        && !target.has_aggregate_filter
        && !target.has_measure_predicate
    {
        return Some((DenseAggFilterPolicy::None, target));
    }

    let filter_attno = filter_attno?;
    let filter_var = VarRef {
        varno: group_var.varno,
        attno: filter_attno,
    };

    if let Some(where_predicate) = unsafe { query_filter_predicate(query, filter_var, measure) }
        && let Some(target) =
            unsafe { target_layout(query.targetList, group_var, measure, None, None) }
        && !target.has_aggregate_filter
        && !target.has_measure_predicate
    {
        let mut target = target;
        target.measure_predicate = where_predicate;
        return Some((
            DenseAggFilterPolicy::Where {
                attno: filter_attno,
            },
            target,
        ));
    }

    if !unsafe { query_has_no_quals(query) } {
        return None;
    }

    if let Some(target) =
        unsafe { target_layout(query.targetList, group_var, measure, Some(filter_var), None) }
        && target.has_aggregate_filter
        && !target.has_measure_predicate
    {
        return Some((
            DenseAggFilterPolicy::Aggregate {
                attno: filter_attno,
            },
            target,
        ));
    }

    if let Some(target) =
        unsafe { target_layout(query.targetList, group_var, measure, None, Some(filter_var)) }
        && !target.has_aggregate_filter
        && target.has_measure_predicate
    {
        return Some((
            DenseAggFilterPolicy::MeasurePredicate {
                attno: filter_attno,
            },
            target,
        ));
    }

    None
}

unsafe fn single_group_var(query: &pg_sys::Query) -> Option<VarRef> {
    let sc = unsafe { pg_sys::list_nth(query.groupClause, 0).cast::<pg_sys::SortGroupClause>() };
    if sc.is_null() {
        return None;
    }
    unsafe { target_var_for_sortgroupref(query.targetList, (*sc).tleSortGroupRef) }
}

unsafe fn target_layout(
    tlist: *mut List,
    group_var: VarRef,
    measure: DenseAggMeasure,
    aggregate_filter: Option<VarRef>,
    measure_predicate: Option<VarRef>,
) -> Option<RecognizedTargetLayout> {
    if tlist.is_null() {
        return None;
    }
    let mut nonjunk = Vec::new();
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        nonjunk.push(unsafe { (*tle).expr.cast::<pg_sys::Node>() });
    }

    if let DenseAggMeasure::StatsPair {
        sum_attno,
        avg_attno,
    } = measure
    {
        if aggregate_filter.is_some() || measure_predicate.is_some() {
            return None;
        }
        if nonjunk.len() == 4 && unsafe { extract_var(nonjunk[0]) } == Some(group_var) {
            let sum_var = unsafe { aggref_direct_var(nonjunk[1], b"sum") };
            let avg_var = unsafe { aggref_direct_var(nonjunk[2], b"avg") };
            let stddev_var = unsafe { aggref_direct_var(nonjunk[3], b"stddev") };
            if sum_var
                == Some(VarRef {
                    varno: group_var.varno,
                    attno: sum_attno,
                })
                && avg_var
                    == Some(VarRef {
                        varno: group_var.varno,
                        attno: avg_attno,
                    })
                && stddev_var
                    == Some(VarRef {
                        varno: group_var.varno,
                        attno: sum_attno,
                    })
            {
                return Some(RecognizedTargetLayout {
                    layout: ResidentDenseGroupedF64Layout::GroupSumAvgStddev,
                    has_aggregate_filter: false,
                    has_measure_predicate: false,
                    measure_predicate: ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
                });
            }
        }
        return None;
    }

    if nonjunk.len() == 3 && unsafe { extract_var(nonjunk[0]) } == Some(group_var) {
        let sum_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[1],
                b"sum",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        if let Some(sum_predicate) = sum_predicate
            && unsafe { aggref_is_count_star(nonjunk[2], aggregate_filter, measure, sum_predicate) }
        {
            return Some(RecognizedTargetLayout {
                layout: ResidentDenseGroupedF64Layout::GroupSumCount,
                has_aggregate_filter: aggregate_filter.is_some(),
                has_measure_predicate: measure_predicate.is_some(),
                measure_predicate: sum_predicate,
            });
        }
    }
    if nonjunk.len() == 3 && unsafe { extract_var(nonjunk[0]) } == Some(group_var) {
        let sum_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[2],
                b"sum",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        if let Some(sum_predicate) = sum_predicate
            && unsafe { aggref_is_count_star(nonjunk[1], aggregate_filter, measure, sum_predicate) }
        {
            return Some(RecognizedTargetLayout {
                layout: ResidentDenseGroupedF64Layout::GroupCountSum,
                has_aggregate_filter: aggregate_filter.is_some(),
                has_measure_predicate: measure_predicate.is_some(),
                measure_predicate: sum_predicate,
            });
        }
    }
    if nonjunk.len() == 4 && unsafe { extract_var(nonjunk[0]) } == Some(group_var) {
        let sum_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[1],
                b"sum",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        let avg_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[2],
                b"avg",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        if let (Some(sum_predicate), Some(avg_predicate)) = (sum_predicate, avg_predicate)
            && sum_predicate == avg_predicate
            && unsafe { aggref_is_count_star(nonjunk[3], aggregate_filter, measure, sum_predicate) }
        {
            return Some(RecognizedTargetLayout {
                layout: ResidentDenseGroupedF64Layout::GroupSumAvgCount,
                has_aggregate_filter: aggregate_filter.is_some(),
                has_measure_predicate: measure_predicate.is_some(),
                measure_predicate: sum_predicate,
            });
        }
    }
    if nonjunk.len() == 4 && unsafe { extract_var(nonjunk[0]) } == Some(group_var) {
        let min_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[1],
                b"min",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        let max_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[2],
                b"max",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        let avg_predicate = unsafe {
            aggref_measure_predicate(
                nonjunk[3],
                b"avg",
                group_var.varno,
                measure,
                aggregate_filter,
                measure_predicate,
            )
        };
        if let (Some(min_predicate), Some(max_predicate), Some(avg_predicate)) =
            (min_predicate, max_predicate, avg_predicate)
            && min_predicate == max_predicate
            && min_predicate == avg_predicate
        {
            return Some(RecognizedTargetLayout {
                layout: ResidentDenseGroupedF64Layout::GroupMinMaxAvg,
                has_aggregate_filter: aggregate_filter.is_some(),
                has_measure_predicate: measure_predicate.is_some(),
                measure_predicate: min_predicate,
            });
        }
    }
    None
}

unsafe fn single_f64_reduce_target(
    tlist: *mut List,
) -> Option<(VarRef, ResidentDenseGroupedF64Layout)> {
    if tlist.is_null() {
        return None;
    }
    let mut nonjunk = Vec::new();
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        nonjunk.push(unsafe { (*tle).expr.cast::<pg_sys::Node>() });
    }

    if nonjunk.len() == 1 {
        let value_var = unsafe { aggref_direct_var(nonjunk[0], b"sum") }?;
        return Some((value_var, ResidentDenseGroupedF64Layout::SingleSum));
    }

    if nonjunk.len() == 2 {
        let min_var = unsafe { aggref_direct_var(nonjunk[0], b"min") }?;
        let max_var = unsafe { aggref_direct_var(nonjunk[1], b"max") }?;
        if min_var == max_var {
            return Some((min_var, ResidentDenseGroupedF64Layout::SingleMinMax));
        }
    }

    if nonjunk.len() == 3 {
        let avg_var = unsafe { aggref_direct_var(nonjunk[0], b"avg") }?;
        let stddev_var = unsafe { aggref_direct_var(nonjunk[1], b"stddev") }?;
        let var_pop_var = unsafe { aggref_direct_var(nonjunk[2], b"var_pop") }?;
        if avg_var == stddev_var && avg_var == var_pop_var {
            return Some((avg_var, ResidentDenseGroupedF64Layout::SingleStats));
        }
    }

    None
}

unsafe fn target_var_for_sortgroupref(tlist: *mut List, sgref: pg_sys::Index) -> Option<VarRef> {
    if tlist.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).ressortgroupref } != sgref {
            continue;
        }
        return unsafe { extract_var((*tle).expr.cast::<pg_sys::Node>()) };
    }
    None
}

unsafe fn extract_var(mut node: *mut pg_sys::Node) -> Option<VarRef> {
    while !node.is_null() {
        match unsafe { (*node).type_ } {
            NodeTag::T_Var => {
                let var = node.cast::<pg_sys::Var>();
                let varno = pg_sys::Index::try_from(unsafe { (*var).varno }).ok()?;
                return Some(VarRef {
                    varno,
                    attno: i32::from(unsafe { (*var).varattno }),
                });
            }
            NodeTag::T_RelabelType => {
                node = unsafe {
                    (*node.cast::<pg_sys::RelabelType>())
                        .arg
                        .cast::<pg_sys::Node>()
                };
            }
            _ => return None,
        }
    }
    None
}

unsafe fn aggref_direct_var(node: *mut pg_sys::Node, name: &[u8]) -> Option<VarRef> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return None;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    let agg_ref = unsafe { &*agg };
    if agg_ref.aggstar
        || !agg_ref.aggdistinct.is_null()
        || !agg_ref.aggorder.is_null()
        || !agg_ref.aggfilter.is_null()
        || !unsafe { agg_name_matches(agg_ref.aggfnoid, name) }
        || agg_ref.args.is_null()
        || unsafe { pg_sys::list_length(agg_ref.args) } != 1
    {
        return None;
    }
    let arg_tle = unsafe { pg_sys::list_nth(agg_ref.args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return None;
    }
    unsafe { extract_var((*arg_tle).expr.cast::<pg_sys::Node>()) }
}

unsafe fn aggref_measure_predicate(
    node: *mut pg_sys::Node,
    name: &[u8],
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
    aggregate_filter: Option<VarRef>,
    measure_predicate: Option<VarRef>,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return None;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    let agg_ref = unsafe { &*agg };
    if !agg_ref.aggdistinct.is_null() || !agg_ref.aggorder.is_null() {
        return None;
    }
    let filter_predicate = unsafe { aggref_filter_predicate(agg_ref, aggregate_filter, measure) }?;
    if !unsafe { agg_name_matches(agg_ref.aggfnoid, name) } {
        return None;
    }
    if agg_ref.args.is_null() || unsafe { pg_sys::list_length(agg_ref.args) } != 1 {
        return None;
    }
    let arg_tle = unsafe { pg_sys::list_nth(agg_ref.args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return None;
    }
    let arg_predicate = unsafe {
        measure_expr_predicate(
            (*arg_tle).expr.cast::<pg_sys::Node>(),
            varno,
            measure,
            measure_predicate,
        )
    }?;
    if aggregate_filter.is_some() {
        if arg_predicate != ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY {
            return None;
        }
        Some(filter_predicate)
    } else {
        Some(arg_predicate)
    }
}

unsafe fn measure_expr_predicate(
    mut node: *mut pg_sys::Node,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
    measure_predicate: Option<VarRef>,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }

    if let Some(predicate) = measure_predicate {
        return unsafe { case_when_measure_predicate(node, varno, measure, predicate) };
    }

    let matches = match measure {
        DenseAggMeasure::Column { attno } => {
            (unsafe { extract_var(node) }) == Some(VarRef { varno, attno })
        }
        DenseAggMeasure::Binary {
            op,
            left_attno,
            right_attno,
            commutative,
        } => unsafe {
            expr_matches_binary_measure(node, varno, op, left_attno, right_attno, commutative)
        },
        DenseAggMeasure::StatsPair { .. } => false,
    };
    matches.then_some(ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY)
}

unsafe fn expr_matches_measure(
    node: *mut pg_sys::Node,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> bool {
    unsafe { measure_expr_predicate(node, varno, measure, None).is_some() }
}

unsafe fn case_when_measure_predicate(
    node: *mut pg_sys::Node,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
    predicate: VarRef,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    let case_expr = unsafe { extract_searched_single_when_case(node)? };
    let when = unsafe { pg_sys::list_nth((*case_expr).args, 0).cast::<pg_sys::CaseWhen>() };
    if when.is_null() {
        return None;
    }
    if !unsafe { expr_matches_measure((*when).result.cast::<pg_sys::Node>(), varno, measure) }
        || !unsafe { const_numeric_zero((*case_expr).defresult.cast::<pg_sys::Node>()) }
    {
        return None;
    }
    unsafe { normalize_measure_predicate((*when).expr.cast::<pg_sys::Node>(), predicate, measure) }
}

unsafe fn extract_searched_single_when_case(
    node: *mut pg_sys::Node,
) -> Option<*mut pg_sys::CaseExpr> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_CaseExpr {
        return None;
    }
    let case_expr = node.cast::<pg_sys::CaseExpr>();
    if !unsafe { (*case_expr).arg }.is_null() || unsafe { (*case_expr).args }.is_null() {
        return None;
    }
    let args = unsafe { (*case_expr).args };
    if unsafe { pg_sys::list_length(args) } != 1 {
        return None;
    }
    Some(case_expr)
}

unsafe fn normalize_measure_predicate(
    node: *mut pg_sys::Node,
    active_var: VarRef,
    measure: DenseAggMeasure,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    if unsafe { expr_is_active_true(node, active_var) } {
        return Some(ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY);
    }
    let intervals = unsafe { active_guarded_measure_ranges(node, active_var, measure) }?;
    let Some(intervals) = intervals else {
        return Some(ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY);
    };
    let ranges: Vec<(f64, f64)> = intervals
        .ranges
        .into_iter()
        .map(|range| (range.lo, range.hi))
        .collect();
    ResidentDenseGroupedF64MeasurePredicate::bool_and_source_ranges(intervals.source, &ranges)
}

// Three distinct outcomes must be distinguished: `None` = predicate shape is not
// a guarded measure range (decline), `Some(None)` = recognized but produces no
// usable interval set, `Some(Some(..))` = the extracted interval set.
#[allow(clippy::option_option)]
unsafe fn active_guarded_measure_ranges(
    mut node: *mut pg_sys::Node,
    active_var: VarRef,
    measure: DenseAggMeasure,
) -> Option<Option<MeasureIntervalSet>> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() {
        return None;
    }
    if unsafe { expr_is_active_true(node, active_var) } {
        return Some(None);
    }
    if unsafe { (*node).type_ } != NodeTag::T_BoolExpr
        && unsafe { (*node).type_ } != NodeTag::T_List
    {
        return None;
    }
    let args = unsafe { implicit_and_args(node)? };
    if args.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(args) };
    if len <= 0 {
        return None;
    }
    let mut has_active = false;
    let mut intervals: Option<MeasureIntervalSet> = None;
    let mut has_measure_predicate = false;
    for i in 0..len {
        let arg = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        if unsafe { expr_is_active_true(arg, active_var) } {
            has_active = true;
        } else {
            let term_intervals =
                unsafe { normalize_measure_interval_predicate(arg, active_var.varno, measure)? };
            intervals = Some(match intervals {
                Some(current) => intersect_measure_interval_sets(&current, &term_intervals)?,
                None => term_intervals,
            });
            has_measure_predicate = true;
        }
    }
    if !has_active {
        return None;
    }
    if has_measure_predicate {
        Some(intervals)
    } else {
        Some(None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RhsInterval {
    lo: f64,
    hi: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct MeasureIntervalSet {
    source: ResidentDenseGroupedF64MeasurePredicateSource,
    ranges: Vec<RhsInterval>,
}

impl RhsInterval {
    fn new(lo: f64, hi: f64) -> Option<Self> {
        if lo.is_nan() || hi.is_nan() || lo > hi {
            return None;
        }
        Some(Self { lo, hi })
    }
}

unsafe fn normalize_measure_interval_predicate(
    mut node: *mut pg_sys::Node,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> Option<MeasureIntervalSet> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_OpExpr => unsafe {
            op_expr_measure_intervals(node.cast::<pg_sys::OpExpr>(), varno, measure)
        },
        NodeTag::T_ScalarArrayOpExpr => unsafe {
            scalar_array_measure_intervals(node.cast::<pg_sys::ScalarArrayOpExpr>(), varno, measure)
        },
        NodeTag::T_NullTest => unsafe {
            null_test_measure_intervals(node.cast::<pg_sys::NullTest>(), varno, measure)
        },
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            let args = unsafe { (*bool_expr).args };
            if args.is_null() {
                return None;
            }
            let len = unsafe { pg_sys::list_length(args) };
            if len <= 0 {
                return None;
            }
            match unsafe { (*bool_expr).boolop } {
                pg_sys::BoolExprType::AND_EXPR => {
                    let mut intervals: Option<MeasureIntervalSet> = None;
                    for i in 0..len {
                        let arg = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
                        let term_intervals =
                            unsafe { normalize_measure_interval_predicate(arg, varno, measure) }?;
                        intervals = Some(match intervals {
                            Some(current) => {
                                intersect_measure_interval_sets(&current, &term_intervals)?
                            }
                            None => term_intervals,
                        });
                    }
                    intervals
                }
                pg_sys::BoolExprType::OR_EXPR => {
                    let mut intervals: Option<MeasureIntervalSet> = None;
                    for i in 0..len {
                        let arg = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
                        let term_intervals =
                            unsafe { normalize_measure_interval_predicate(arg, varno, measure) }?;
                        intervals = Some(match intervals {
                            Some(current) => {
                                union_measure_interval_sets(&current, &term_intervals)?
                            }
                            None => term_intervals,
                        });
                    }
                    intervals
                }
                pg_sys::BoolExprType::NOT_EXPR => {
                    if len != 1 {
                        return None;
                    }
                    let arg = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
                    let intervals =
                        unsafe { normalize_measure_interval_predicate(arg, varno, measure) }?;
                    let ranges = complement_interval_set(&intervals.ranges)?;
                    Some(MeasureIntervalSet {
                        source: intervals.source,
                        ranges,
                    })
                }
                _ => None,
            }
        }
        NodeTag::T_List => unsafe {
            intersect_measure_interval_list(node.cast::<pg_sys::List>(), varno, measure)
        },
        _ => None,
    }
}

unsafe fn implicit_and_args(node: *mut pg_sys::Node) -> Option<*mut pg_sys::List> {
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            (unsafe { (*bool_expr).boolop } == pg_sys::BoolExprType::AND_EXPR)
                .then_some(unsafe { (*bool_expr).args })
        }
        NodeTag::T_List => Some(node.cast::<pg_sys::List>()),
        _ => None,
    }
}

unsafe fn intersect_measure_interval_list(
    args: *mut pg_sys::List,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> Option<MeasureIntervalSet> {
    if args.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(args) };
    if len <= 0 {
        return None;
    }
    let mut intervals: Option<MeasureIntervalSet> = None;
    for i in 0..len {
        let arg = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
        let term_intervals = unsafe { normalize_measure_interval_predicate(arg, varno, measure) }?;
        intervals = Some(match intervals {
            Some(current) => intersect_measure_interval_sets(&current, &term_intervals)?,
            None => term_intervals,
        });
    }
    intervals
}

fn intersect_measure_interval_sets(
    left: &MeasureIntervalSet,
    right: &MeasureIntervalSet,
) -> Option<MeasureIntervalSet> {
    if left.source != right.source {
        return None;
    }
    Some(MeasureIntervalSet {
        source: left.source,
        ranges: intersect_interval_sets(&left.ranges, &right.ranges)?,
    })
}

fn union_measure_interval_sets(
    left: &MeasureIntervalSet,
    right: &MeasureIntervalSet,
) -> Option<MeasureIntervalSet> {
    if left.source != right.source {
        return None;
    }
    let mut ranges = left.ranges.clone();
    ranges.extend_from_slice(&right.ranges);
    Some(MeasureIntervalSet {
        source: left.source,
        ranges: merge_intervals(ranges)?,
    })
}

fn intersect_interval_sets(
    left: &[RhsInterval],
    right: &[RhsInterval],
) -> Option<Vec<RhsInterval>> {
    let mut out = Vec::new();
    for lhs in left {
        for rhs in right {
            if let Some(interval) = RhsInterval::new(lhs.lo.max(rhs.lo), lhs.hi.min(rhs.hi)) {
                out.push(interval);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        merge_intervals(out)
    }
}

fn merge_intervals(mut ranges: Vec<RhsInterval>) -> Option<Vec<RhsInterval>> {
    if ranges.is_empty() {
        return None;
    }
    ranges.sort_by(|a, b| a.lo.total_cmp(&b.lo).then_with(|| a.hi.total_cmp(&b.hi)));
    let mut merged: Vec<RhsInterval> = Vec::new();
    for range in ranges {
        if range.lo.is_nan() || range.hi.is_nan() || range.lo > range.hi {
            return None;
        }
        if let Some(last) = merged.last_mut()
            && range.lo <= last.hi
        {
            last.hi = last.hi.max(range.hi);
            continue;
        }
        merged.push(range);
    }
    Some(merged)
}

fn complement_interval_set(ranges: &[RhsInterval]) -> Option<Vec<RhsInterval>> {
    let ranges = merge_intervals(ranges.to_vec())?;
    let mut out = Vec::new();
    let mut next_lo = f64::NEG_INFINITY;
    for range in ranges {
        if range.lo != f64::NEG_INFINITY {
            let before_hi = next_down(range.lo)?;
            push_interval_if_nonempty(&mut out, next_lo, before_hi)?;
        }
        if range.hi == f64::INFINITY {
            return (!out.is_empty()).then_some(out);
        }
        next_lo = next_up(range.hi)?;
    }
    push_interval_if_nonempty(&mut out, next_lo, f64::INFINITY)?;
    (!out.is_empty()).then_some(out)
}

fn push_interval_if_nonempty(out: &mut Vec<RhsInterval>, lo: f64, hi: f64) -> Option<()> {
    if lo <= hi {
        out.push(RhsInterval::new(lo, hi)?);
    }
    Some(())
}

unsafe fn null_test_measure_intervals(
    null_test: *mut pg_sys::NullTest,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> Option<MeasureIntervalSet> {
    if null_test.is_null()
        || unsafe { (*null_test).nulltesttype } != pg_sys::NullTestType::IS_NOT_NULL
    {
        return None;
    }
    let arg = unsafe { (*null_test).arg.cast::<pg_sys::Node>() };
    let source =
        unsafe { extract_var(arg) }.and_then(|var| measure.predicate_source_for_var(var, varno))?;
    Some(MeasureIntervalSet {
        source,
        ranges: vec![RhsInterval {
            lo: f64::NEG_INFINITY,
            hi: f64::INFINITY,
        }],
    })
}

unsafe fn op_expr_measure_intervals(
    op: *mut pg_sys::OpExpr,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> Option<MeasureIntervalSet> {
    if op.is_null() {
        return None;
    }
    let name = unsafe { op_name(op) }?;
    let args = unsafe { (*op).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    if let Some(source) =
        unsafe { extract_var(left) }.and_then(|var| measure.predicate_source_for_var(var, varno))
    {
        let value = unsafe { const_numeric_value(right) }?;
        return Some(MeasureIntervalSet {
            source,
            ranges: rhs_intervals_from_cmp(name.as_str(), value)?,
        });
    }
    if let Some(source) =
        unsafe { extract_var(right) }.and_then(|var| measure.predicate_source_for_var(var, varno))
    {
        let value = unsafe { const_numeric_value(left) }?;
        let flipped = flipped_cmp_op(name.as_str())?;
        return Some(MeasureIntervalSet {
            source,
            ranges: rhs_intervals_from_cmp(flipped, value)?,
        });
    }
    None
}

unsafe fn scalar_array_measure_intervals(
    scalar_array: *mut pg_sys::ScalarArrayOpExpr,
    varno: pg_sys::Index,
    measure: DenseAggMeasure,
) -> Option<MeasureIntervalSet> {
    if scalar_array.is_null() {
        return None;
    }
    let use_or = unsafe { (*scalar_array).useOr };
    let op_name = unsafe { op_name_from_oid((*scalar_array).opno) }?;
    let args = unsafe { (*scalar_array).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let lhs = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let rhs = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    let source =
        unsafe { extract_var(lhs) }.and_then(|var| measure.predicate_source_for_var(var, varno))?;
    let values = unsafe { scalar_array_numeric_values(rhs)? };
    if values.is_empty() {
        return None;
    }
    let mut ranges = Vec::with_capacity(values.len());
    for value in values {
        ranges.push(RhsInterval::new(value, value)?);
    }
    let merged = if use_or && op_name == "=" {
        merge_intervals(ranges)?
    } else if !use_or && is_not_equal_op(op_name.as_str()) {
        complement_interval_set(&ranges)?
    } else {
        return None;
    };
    if merged.len() > RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES {
        return None;
    }
    Some(MeasureIntervalSet {
        source,
        ranges: merged,
    })
}

unsafe fn scalar_array_numeric_values(mut node: *mut pg_sys::Node) -> Option<Vec<f64>> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_ArrayExpr => unsafe {
            array_expr_numeric_values(node.cast::<pg_sys::ArrayExpr>())
        },
        NodeTag::T_Const => unsafe { const_array_numeric_values(node.cast::<pg_sys::Const>()) },
        _ => None,
    }
}

unsafe fn array_expr_numeric_values(array: *mut pg_sys::ArrayExpr) -> Option<Vec<f64>> {
    if array.is_null() {
        return None;
    }
    if unsafe { (*array).multidims } {
        return None;
    }
    let elements = unsafe { (*array).elements };
    if elements.is_null() {
        return Some(Vec::new());
    }
    let len = unsafe { pg_sys::list_length(elements) };
    if len < 0 || usize::try_from(len).ok()? > RESIDENT_DENSE_GROUPED_F64_SCALAR_ARRAY_MAX_VALUES {
        return None;
    }
    let mut values = Vec::with_capacity(usize::try_from(len).ok()?);
    for idx in 0..len {
        let elem = unsafe { pg_sys::list_nth(elements, idx).cast::<pg_sys::Node>() };
        let value = unsafe { const_numeric_value(elem) }?;
        if value.is_nan() {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

unsafe fn const_array_numeric_values(array_const: *mut pg_sys::Const) -> Option<Vec<f64>> {
    if array_const.is_null()
        || unsafe { (*array_const).constisnull }
        || numeric_array_const_elem_type(unsafe { (*array_const).consttype }).is_none()
    {
        return None;
    }
    let array = unsafe { parse_array((*array_const).constvalue) }.ok()?;
    pg_array_numeric_values(&array)
}

fn pg_array_numeric_values(array: &PgArray<'_>) -> Option<Vec<f64>> {
    if array.nelems > RESIDENT_DENSE_GROUPED_F64_SCALAR_ARRAY_MAX_VALUES {
        return None;
    }
    let mut values = Vec::with_capacity(array.nelems);
    for element in array {
        let bytes = element?;
        let value = match array.elem_type {
            oid if oid == pg_sys::FLOAT8OID => read_array_element::<f64>(bytes)?,
            oid if oid == pg_sys::FLOAT4OID => f64::from(read_array_element::<f32>(bytes)?),
            oid if oid == pg_sys::INT2OID => f64::from(read_array_element::<i16>(bytes)?),
            oid if oid == pg_sys::INT4OID => f64::from(read_array_element::<i32>(bytes)?),
            oid if oid == pg_sys::INT8OID => read_array_element::<i64>(bytes)? as f64,
            _ => return None,
        };
        if value.is_nan() {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

fn read_array_element<T: Copy>(bytes: &[u8]) -> Option<T> {
    if bytes.len() != std::mem::size_of::<T>() {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
}

fn numeric_array_const_elem_type(array_oid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    match array_oid {
        oid if oid == pg_sys::FLOAT8ARRAYOID => Some(pg_sys::FLOAT8OID),
        oid if oid == pg_sys::FLOAT4ARRAYOID => Some(pg_sys::FLOAT4OID),
        oid if oid == pg_sys::INT2ARRAYOID => Some(pg_sys::INT2OID),
        oid if oid == pg_sys::INT4ARRAYOID => Some(pg_sys::INT4OID),
        oid if oid == pg_sys::INT8ARRAYOID => Some(pg_sys::INT8OID),
        _ => None,
    }
}

fn rhs_intervals_from_cmp(op: &str, value: f64) -> Option<Vec<RhsInterval>> {
    if value.is_nan() {
        return None;
    }
    let interval = match op {
        "=" => RhsInterval::new(value, value),
        ">=" => RhsInterval::new(value, f64::INFINITY),
        ">" => RhsInterval::new(next_up(value)?, f64::INFINITY),
        "<=" => RhsInterval::new(f64::NEG_INFINITY, value),
        "<" => RhsInterval::new(f64::NEG_INFINITY, next_down(value)?),
        op if is_not_equal_op(op) => {
            return complement_interval_set(&[RhsInterval::new(value, value)?]);
        }
        _ => None,
    }?;
    Some(vec![interval])
}

fn is_not_equal_op(op: &str) -> bool {
    matches!(op, "<>" | "!=")
}

fn next_up(value: f64) -> Option<f64> {
    if value.is_nan() || value == f64::INFINITY {
        return None;
    }
    if value == -0.0 {
        return Some(f64::MIN_POSITIVE);
    }
    let bits = value.to_bits();
    Some(if value >= 0.0 {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    })
}

fn next_down(value: f64) -> Option<f64> {
    if value.is_nan() || value == f64::NEG_INFINITY {
        return None;
    }
    if value == 0.0 {
        return Some(-f64::MIN_POSITIVE);
    }
    let bits = value.to_bits();
    Some(if value > 0.0 {
        f64::from_bits(bits - 1)
    } else {
        f64::from_bits(bits + 1)
    })
}

unsafe fn expr_matches_binary_measure(
    node: *mut pg_sys::Node,
    varno: pg_sys::Index,
    op: ResidentMeasureOp,
    left_attno: i32,
    right_attno: i32,
    commutative: bool,
) -> bool {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_OpExpr {
        return false;
    }
    let op_expr = node.cast::<pg_sys::OpExpr>();
    if unsafe { op_name(op_expr) }.as_deref() != Some(measure_op_name(op)) {
        return false;
    }
    let args = unsafe { (*op_expr).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return false;
    }
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    let wanted_left = VarRef {
        varno,
        attno: left_attno,
    };
    let wanted_right = VarRef {
        varno,
        attno: right_attno,
    };
    let actual_left = unsafe { extract_var(left) };
    let actual_right = unsafe { extract_var(right) };
    (actual_left == Some(wanted_left) && actual_right == Some(wanted_right))
        || (commutative && actual_left == Some(wanted_right) && actual_right == Some(wanted_left))
}

const fn measure_op_name(op: ResidentMeasureOp) -> &'static str {
    match op {
        ResidentMeasureOp::Column => "",
        ResidentMeasureOp::Mul => "*",
        ResidentMeasureOp::Sub => "-",
        ResidentMeasureOp::StatsPair => "",
    }
}

unsafe fn aggref_is_count_star(
    node: *mut pg_sys::Node,
    aggregate_filter: Option<VarRef>,
    measure: DenseAggMeasure,
    expected_filter_predicate: ResidentDenseGroupedF64MeasurePredicate,
) -> bool {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Aggref {
        return false;
    }
    let agg = node.cast::<pg_sys::Aggref>();
    let agg_ref = unsafe { &*agg };
    agg_ref.aggstar
        && unsafe { agg_name(agg_ref.aggfnoid) }.as_deref() == Some(b"count".as_slice())
        && agg_ref.aggdistinct.is_null()
        && agg_ref.aggorder.is_null()
        && unsafe {
            aggref_filter_predicate(agg_ref, aggregate_filter, measure)
                == Some(if aggregate_filter.is_some() {
                    expected_filter_predicate
                } else {
                    ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                })
        }
}

unsafe fn aggref_filter_predicate(
    agg_ref: &pg_sys::Aggref,
    aggregate_filter: Option<VarRef>,
    measure: DenseAggMeasure,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    match aggregate_filter {
        Some(active_var) => {
            if agg_ref.aggfilter.is_null() {
                return None;
            }
            unsafe {
                normalize_measure_predicate(
                    agg_ref.aggfilter.cast::<pg_sys::Node>(),
                    active_var,
                    measure,
                )
            }
        }
        None => agg_ref
            .aggfilter
            .is_null()
            .then_some(ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY),
    }
}

unsafe fn agg_name(oid: pg_sys::Oid) -> Option<Vec<u8>> {
    let ptr = unsafe { pg_sys::get_func_name(oid) };
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec())
}

unsafe fn agg_name_matches(oid: pg_sys::Oid, expected: &[u8]) -> bool {
    let Some(actual) = (unsafe { agg_name(oid) }) else {
        return false;
    };
    actual.as_slice() == expected
        || (expected == b"avg" && actual.as_slice() == b"float8_avg")
        || (expected == b"stddev"
            && (actual.as_slice() == b"stddev_samp" || actual.as_slice() == b"float8_stddev_samp"))
        || (expected == b"var_pop" && actual.as_slice() == b"float8_var_pop")
}

unsafe fn query_has_no_quals(query: &pg_sys::Query) -> bool {
    query.jointree.is_null() || unsafe { (*query.jointree).quals }.is_null()
}

unsafe fn query_filter_predicate(
    query: &pg_sys::Query,
    active_var: VarRef,
    measure: DenseAggMeasure,
) -> Option<ResidentDenseGroupedF64MeasurePredicate> {
    if query.jointree.is_null() {
        return None;
    }
    let quals = unsafe { (*query.jointree).quals };
    if quals.is_null() {
        return None;
    }
    unsafe { normalize_measure_predicate(quals.cast::<pg_sys::Node>(), active_var, measure) }
}

unsafe fn expr_is_active_true(mut node: *mut pg_sys::Node, active_var: VarRef) -> bool {
    while !node.is_null() {
        match unsafe { (*node).type_ } {
            NodeTag::T_Var => return unsafe { extract_var(node) } == Some(active_var),
            NodeTag::T_RelabelType => {
                node = unsafe {
                    (*node.cast::<pg_sys::RelabelType>())
                        .arg
                        .cast::<pg_sys::Node>()
                };
            }
            NodeTag::T_BooleanTest => {
                let bool_test = node.cast::<pg_sys::BooleanTest>();
                return unsafe { (*bool_test).booltesttype } == pg_sys::BoolTestType::IS_TRUE
                    && unsafe {
                        expr_is_active_true((*bool_test).arg.cast::<pg_sys::Node>(), active_var)
                    };
            }
            NodeTag::T_OpExpr => {
                return unsafe {
                    op_expr_is_active_eq_true(node.cast::<pg_sys::OpExpr>(), active_var)
                };
            }
            NodeTag::T_BoolExpr => {
                let bool_expr = node.cast::<pg_sys::BoolExpr>();
                if unsafe { (*bool_expr).boolop } != pg_sys::BoolExprType::AND_EXPR {
                    return false;
                }
                let args = unsafe { (*bool_expr).args };
                if args.is_null() || unsafe { pg_sys::list_length(args) } != 1 {
                    return false;
                }
                node = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
            }
            NodeTag::T_List => {
                let list = node.cast::<pg_sys::List>();
                if unsafe { pg_sys::list_length(list) } != 1 {
                    return false;
                }
                node = unsafe { pg_sys::list_nth(list, 0).cast::<pg_sys::Node>() };
            }
            _ => return false,
        }
    }
    false
}

unsafe fn op_expr_is_active_eq_true(op: *mut pg_sys::OpExpr, active_var: VarRef) -> bool {
    if op.is_null() || unsafe { op_name(op) }.as_deref() != Some("=") {
        return false;
    }
    let args = unsafe { (*op).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return false;
    }
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    (unsafe { extract_var(left) } == Some(active_var)
        && unsafe { const_bool_value(right) } == Some(true))
        || (unsafe { const_bool_value(left) } == Some(true)
            && unsafe { extract_var(right) } == Some(active_var))
}

fn flipped_cmp_op(op: &str) -> Option<&'static str> {
    Some(match op {
        ">=" => "<=",
        "<=" => ">=",
        ">" => "<",
        "<" => ">",
        "<>" | "!=" => "<>",
        _ => return None,
    })
}

unsafe fn const_bool_value(node: *mut pg_sys::Node) -> Option<bool> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } || unsafe { (*cst).consttype } != pg_sys::BOOLOID {
        return None;
    }
    Some(unsafe { (*cst).constvalue.value() != 0 })
}

unsafe fn const_numeric_value(mut node: *mut pg_sys::Node) -> Option<f64> {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return None;
    }
    let raw = unsafe { (*cst).constvalue.value() };
    match unsafe { (*cst).consttype } {
        oid if oid == pg_sys::FLOAT8OID => Some(f64::from_bits(raw as u64)),
        oid if oid == pg_sys::FLOAT4OID => Some(f64::from(f32::from_bits(raw as u32))),
        oid if oid == pg_sys::INT2OID => Some(f64::from(raw as i16)),
        oid if oid == pg_sys::INT4OID => Some(f64::from(raw as i32)),
        oid if oid == pg_sys::INT8OID => Some((raw as i64) as f64),
        _ => None,
    }
}

unsafe fn const_numeric_zero(mut node: *mut pg_sys::Node) -> bool {
    while !node.is_null() && unsafe { (*node).type_ } == NodeTag::T_RelabelType {
        node = unsafe {
            (*node.cast::<pg_sys::RelabelType>())
                .arg
                .cast::<pg_sys::Node>()
        };
    }
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return false;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return false;
    }
    let raw = unsafe { (*cst).constvalue.value() };
    match unsafe { (*cst).consttype } {
        oid if oid == pg_sys::FLOAT8OID => f64::from_bits(raw as u64) == 0.0,
        oid if oid == pg_sys::FLOAT4OID => f32::from_bits(raw as u32) == 0.0,
        oid if oid == pg_sys::INT2OID => raw as i16 == 0,
        oid if oid == pg_sys::INT4OID => raw as i32 == 0,
        oid if oid == pg_sys::INT8OID => raw as i64 == 0,
        _ => false,
    }
}

unsafe fn op_name(op: *mut pg_sys::OpExpr) -> Option<String> {
    if op.is_null() {
        return None;
    }
    unsafe { op_name_from_oid((*op).opno) }
}

unsafe fn op_name_from_oid(op_oid: pg_sys::Oid) -> Option<String> {
    let name_ptr = unsafe { pg_sys::get_opname(op_oid) };
    if name_ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(name_ptr) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

unsafe fn relation_oid_from_rtable(
    rtable: *mut pg_sys::List,
    varno: pg_sys::Index,
) -> Option<pg_sys::Oid> {
    if rtable.is_null() || varno == 0 {
        return None;
    }
    let index = i32::try_from(varno).ok()?.checked_sub(1)?;
    if index >= unsafe { pg_sys::list_length(rtable) } {
        return None;
    }
    let rte = unsafe { pg_sys::list_nth(rtable, index).cast::<pg_sys::RangeTblEntry>() };
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return None;
    }
    Some(unsafe { (*rte).relid })
}

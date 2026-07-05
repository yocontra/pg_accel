//! Planner recognizer for the first OLAP benchmark lane: SSBM Q1.x.
//!
//! This module recognizes canonical SSBM Q1.x filtered revenue queries and
//! injects a childless resident-GPU aggregate when the backend-local resident
//! cache is loaded for the referenced tables.

use std::collections::BTreeSet;
use std::ffi::CStr;

use pgrx::pg_sys::{self, List, NodeTag, PlannerInfo, RelOptInfo};

use super::rel_rows_estimate;
use super::resident_groupagg_path::{
    ResidentGroupAggPathShape, inject_childless_resident_groupagg_path,
};
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::olap::{
    OlapAggSpec, SsbmQ1RevenueSpec as ExecSsbmQ1RevenueSpec,
    SsbmQ2GroupedRevenueSpec as ExecSsbmQ2GroupedRevenueSpec,
    SsbmQ3GroupedRevenueSpec as ExecSsbmQ3GroupedRevenueSpec,
    SsbmQ4GroupedProfitSpec as ExecSsbmQ4GroupedProfitSpec,
};
use crate::engine::olap_cache;
use crate::engine::residency::ResidentOperatorStage;
use crate::engine::stats;

const LINEORDER_REL: &str = "ssbm_lineorder";
const DATE_REL: &str = "ssbm_date";
const PART_REL: &str = "ssbm_part";
const CUSTOMER_REL: &str = "ssbm_customer";
const SUPPLIER_REL: &str = "ssbm_supplier";

const LO_CUSTKEY_ATTNO: i32 = 3;
const LO_PARTKEY_ATTNO: i32 = 4;
const LO_SUPPKEY_ATTNO: i32 = 5;
const LO_ORDERDATE_ATTNO: i32 = 6;
const LO_QUANTITY_ATTNO: i32 = 9;
const LO_EXTENDEDPRICE_ATTNO: i32 = 10;
const LO_DISCOUNT_ATTNO: i32 = 12;
const LO_REVENUE_ATTNO: i32 = 13;
const LO_SUPPLYCOST_ATTNO: i32 = 14;

const D_DATEKEY_ATTNO: i32 = 1;
const D_YEAR_ATTNO: i32 = 5;
const D_YEARMONTHNUM_ATTNO: i32 = 6;
const D_YEARMONTH_ATTNO: i32 = 7;
const D_WEEKNUMINYEAR_ATTNO: i32 = 12;

const P_PARTKEY_ATTNO: i32 = 1;
const P_MFGR_ATTNO: i32 = 3;
const P_CATEGORY_ATTNO: i32 = 4;
const P_BRAND1_ATTNO: i32 = 5;

const S_SUPPKEY_ATTNO: i32 = 1;
const S_CITY_ATTNO: i32 = 4;
const S_NATION_ATTNO: i32 = 5;
const S_REGION_ATTNO: i32 = 6;

const C_CUSTKEY_ATTNO: i32 = 1;
const C_CITY_ATTNO: i32 = 4;
const C_NATION_ATTNO: i32 = 5;
const C_REGION_ATTNO: i32 = 6;

const TEXTOID_RAW: u32 = 25;
const BPCHAROID_RAW: u32 = 1042;
const VARCHAROID_RAW: u32 = 1043;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SsbmQ1RevenueSpec {
    pub fact_relid: pg_sys::Index,
    pub date_relid: pg_sys::Index,
    pub date_predicate: SsbmQ1DatePredicate,
    pub discount_lo: i32,
    pub discount_hi: i32,
    pub quantity_lo: i32,
    pub quantity_hi: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SsbmQ2GroupedRevenueSpec {
    pub fact_relid: pg_sys::Index,
    pub date_relid: pg_sys::Index,
    pub part_relid: pg_sys::Index,
    pub supplier_relid: pg_sys::Index,
    pub variant: olap_cache::SsbmQ2Variant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SsbmQ3GroupedRevenueSpec {
    pub fact_relid: pg_sys::Index,
    pub date_relid: pg_sys::Index,
    pub customer_relid: pg_sys::Index,
    pub supplier_relid: pg_sys::Index,
    pub variant: olap_cache::SsbmQ3Variant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SsbmQ4GroupedProfitSpec {
    pub fact_relid: pg_sys::Index,
    pub date_relid: pg_sys::Index,
    pub part_relid: pg_sys::Index,
    pub customer_relid: pg_sys::Index,
    pub supplier_relid: pg_sys::Index,
    pub variant: olap_cache::SsbmQ4Variant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SsbmQ1DatePredicate {
    Year(i32),
    YearMonthNum(i32),
    YearWeek { year: i32, week: i32 },
}

impl SsbmQ1DatePredicate {
    const fn to_cache_predicate(&self) -> olap_cache::SsbmQ1DatePredicate {
        match *self {
            Self::Year(year) => olap_cache::SsbmQ1DatePredicate::Year(year),
            Self::YearMonthNum(yearmonthnum) => {
                olap_cache::SsbmQ1DatePredicate::YearMonthNum(yearmonthnum)
            }
            Self::YearWeek { year, week } => {
                olap_cache::SsbmQ1DatePredicate::YearWeek { year, week }
            }
        }
    }
}

/// Recognize and inject the resident SSBM Q1 filtered-revenue path.
///
/// Returns `true` when the query was recognized, regardless of whether the
/// resident cache was loaded. That lets the caller avoid falling through to
/// generic host-staged aggregate planning for this OLAP lane.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject_revenue_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let spec = match unsafe { recognize_revenue_agg_result(root, input_rel) } {
        Ok(spec) => spec,
        Err(reason) => {
            if reason.should_record_for_ssbm_shape() {
                let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
                stats::increment_planner_rejected(reason.stats_key(), rows_est);
                stats::record_planner_fast_decline(reason.fast_decline_context());
                return true;
            }
            return false;
        }
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    let Some(fact_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.fact_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q1_invalid_fact_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q1_invalid_fact_rel");
        return true;
    };
    let Some(date_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.date_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q1_invalid_date_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q1_invalid_date_rel");
        return true;
    };

    if !olap_cache::ssbm_q1_cache_loaded_for(fact_rel_oid, date_rel_oid) {
        stats::increment_planner_rejected("recognized_ssbm_q1_no_resident_gpu_path", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q1_no_resident_gpu_path");
        pgrx::debug1!(
            "pg_accel: upper_paths SSBM Q1 decline: recognized filtered revenue aggregate for \
             fact relid={} date relid={} but backend-local resident cache is not loaded",
            spec.fact_relid,
            spec.date_relid,
        );
        return true;
    }

    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_ssbm_q1_resident_olap",
                olap_spec: OlapAggSpec::SsbmQ1Revenue(ExecSsbmQ1RevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    date_predicate: spec.date_predicate.to_cache_predicate(),
                    discount_lo: spec.discount_lo,
                    discount_hi: spec.discount_hi,
                    quantity_lo: spec.quantity_lo,
                    quantity_hi: spec.quantity_hi,
                }),
                agg_op: AggOp::Sum,
                result_oid: pg_sys::INT8OID,
                output_rows: 1.0,
                cost_per_row: 0.00005,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::Expression,
                    ResidentOperatorStage::Aggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns: 5,
                has_filter: true,
            },
        );
    }
    true
}

/// Recognize and inject the resident SSBM Q2 grouped revenue path.
///
/// Returns `true` when the query is recognized, even if the resident cache is
/// not loaded, so generic CPU-backed GPU aggregation does not claim this lane.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject_q2_grouped_revenue_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let spec = match unsafe { recognize_q2_grouped_revenue_result(root, input_rel) } {
        Ok(spec) => spec,
        Err(reason) => {
            if reason.should_record_for_ssbm_shape() {
                let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
                stats::increment_planner_rejected(reason.stats_key(), rows_est);
                stats::record_planner_fast_decline(reason.fast_decline_context());
                return true;
            }
            return false;
        }
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    let Some(fact_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.fact_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q2_invalid_fact_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q2_invalid_fact_rel");
        return true;
    };
    let Some(date_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.date_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q2_invalid_date_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q2_invalid_date_rel");
        return true;
    };
    let Some(part_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.part_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q2_invalid_part_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q2_invalid_part_rel");
        return true;
    };
    let Some(supplier_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.supplier_relid) })
    else {
        stats::increment_planner_rejected("recognized_ssbm_q2_invalid_supplier_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q2_invalid_supplier_rel");
        return true;
    };

    if !olap_cache::ssbm_q2_cache_loaded_for(
        fact_rel_oid,
        date_rel_oid,
        part_rel_oid,
        supplier_rel_oid,
    ) {
        stats::increment_planner_rejected("recognized_ssbm_q2_no_resident_gpu_path", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q2_no_resident_gpu_path");
        pgrx::debug1!(
            "pg_accel: upper_paths SSBM Q2 decline: recognized grouped revenue aggregate for \
             lineorder/date/part/supplier but backend-local resident cache is not loaded"
        );
        return true;
    }

    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_ssbm_q2_resident_olap",
                olap_spec: OlapAggSpec::SsbmQ2GroupedRevenue(ExecSsbmQ2GroupedRevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    part_rel_oid,
                    supplier_rel_oid,
                    variant: spec.variant,
                }),
                agg_op: AggOp::Sum,
                result_oid: pg_sys::INT8OID,
                output_rows: 512.0,
                cost_per_row: 0.00008,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::Join,
                    ResidentOperatorStage::Expression,
                    ResidentOperatorStage::GroupedAggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns: 7,
                has_filter: true,
            },
        );
    }
    true
}

/// Recognize and inject the resident SSBM Q3 grouped revenue path.
///
/// Returns `true` when the query is recognized, even if the resident cache is
/// not loaded, so generic CPU-backed GPU aggregation does not claim this lane.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject_q3_grouped_revenue_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let spec = match unsafe { recognize_q3_grouped_revenue_result(root, input_rel) } {
        Ok(spec) => spec,
        Err(reason) => {
            if reason.should_record_for_ssbm_shape() {
                let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
                stats::increment_planner_rejected(reason.stats_key(), rows_est);
                stats::record_planner_fast_decline(reason.fast_decline_context());
                return true;
            }
            return false;
        }
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    let Some(fact_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.fact_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q3_invalid_fact_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q3_invalid_fact_rel");
        return true;
    };
    let Some(date_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.date_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q3_invalid_date_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q3_invalid_date_rel");
        return true;
    };
    let Some(customer_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.customer_relid) })
    else {
        stats::increment_planner_rejected("recognized_ssbm_q3_invalid_customer_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q3_invalid_customer_rel");
        return true;
    };
    let Some(supplier_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.supplier_relid) })
    else {
        stats::increment_planner_rejected("recognized_ssbm_q3_invalid_supplier_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q3_invalid_supplier_rel");
        return true;
    };

    if !olap_cache::ssbm_q3_cache_loaded_for(
        fact_rel_oid,
        date_rel_oid,
        customer_rel_oid,
        supplier_rel_oid,
    ) {
        stats::increment_planner_rejected("recognized_ssbm_q3_no_resident_gpu_path", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q3_no_resident_gpu_path");
        pgrx::debug1!(
            "pg_accel: upper_paths SSBM Q3 decline: recognized grouped revenue aggregate for \
             lineorder/date/customer/supplier but backend-local resident cache is not loaded"
        );
        return true;
    }

    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_ssbm_q3_resident_olap",
                olap_spec: OlapAggSpec::SsbmQ3GroupedRevenue(ExecSsbmQ3GroupedRevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    customer_rel_oid,
                    supplier_rel_oid,
                    variant: spec.variant,
                }),
                agg_op: AggOp::Sum,
                result_oid: pg_sys::INT8OID,
                output_rows: 512.0,
                cost_per_row: 0.00008,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::Join,
                    ResidentOperatorStage::Expression,
                    ResidentOperatorStage::GroupedAggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns: 8,
                has_filter: true,
            },
        );
    }
    true
}

/// Recognize and inject the resident SSBM Q4 grouped profit path.
///
/// Returns `true` when the query is recognized, even if the resident cache is
/// not loaded, so generic CPU-backed GPU aggregation does not claim this lane.
///
/// # Safety
///
/// Planner pointers must be valid for the current `UPPERREL_GROUP_AGG` hook.
pub(super) unsafe fn try_inject_q4_grouped_profit_agg(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    output_rel: *mut RelOptInfo,
) -> bool {
    let spec = match unsafe { recognize_q4_grouped_profit_result(root, input_rel) } {
        Ok(spec) => spec,
        Err(reason) => {
            if reason.should_record_for_ssbm_shape() {
                let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);
                stats::increment_planner_rejected(reason.stats_key(), rows_est);
                stats::record_planner_fast_decline(reason.fast_decline_context());
                return true;
            }
            return false;
        }
    };
    let rows_est = rel_rows_estimate(input_rel).unwrap_or(0);

    let Some(fact_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.fact_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q4_invalid_fact_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_invalid_fact_rel");
        return true;
    };
    let Some(date_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.date_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q4_invalid_date_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_invalid_date_rel");
        return true;
    };
    let Some(part_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.part_relid) }) else {
        stats::increment_planner_rejected("recognized_ssbm_q4_invalid_part_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_invalid_part_rel");
        return true;
    };
    let Some(customer_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.customer_relid) })
    else {
        stats::increment_planner_rejected("recognized_ssbm_q4_invalid_customer_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_invalid_customer_rel");
        return true;
    };
    let Some(supplier_rel_oid) = (unsafe { relation_oid_for_varno(root, spec.supplier_relid) })
    else {
        stats::increment_planner_rejected("recognized_ssbm_q4_invalid_supplier_rel", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_invalid_supplier_rel");
        return true;
    };

    if !olap_cache::ssbm_q4_cache_loaded_for(
        fact_rel_oid,
        date_rel_oid,
        part_rel_oid,
        customer_rel_oid,
        supplier_rel_oid,
    ) {
        stats::increment_planner_rejected("recognized_ssbm_q4_no_resident_gpu_path", rows_est);
        stats::record_planner_fast_decline("upper_paths_ssbm_q4_no_resident_gpu_path");
        pgrx::debug1!(
            "pg_accel: upper_paths SSBM Q4 decline: recognized grouped profit aggregate for \
             lineorder/date/part/customer/supplier but backend-local resident cache is not loaded"
        );
        return true;
    }

    unsafe {
        inject_childless_resident_groupagg_path(
            output_rel,
            rows_est,
            ResidentGroupAggPathShape {
                context: "upper_paths_ssbm_q4_resident_olap",
                olap_spec: OlapAggSpec::SsbmQ4GroupedProfit(ExecSsbmQ4GroupedProfitSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    part_rel_oid,
                    customer_rel_oid,
                    supplier_rel_oid,
                    variant: spec.variant,
                }),
                agg_op: AggOp::Sum,
                result_oid: pg_sys::INT8OID,
                output_rows: 512.0,
                cost_per_row: 0.00009,
                stages: vec![
                    ResidentOperatorStage::Scan,
                    ResidentOperatorStage::Join,
                    ResidentOperatorStage::Expression,
                    ResidentOperatorStage::GroupedAggregate,
                    ResidentOperatorStage::FinalMaterialization,
                ],
                device_columns: 10,
                has_filter: true,
            },
        );
    }
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct I32Range {
    lo: Option<i32>,
    hi: Option<i32>,
}

impl I32Range {
    fn apply(&mut self, op: CmpOp, value: i32) {
        match op {
            CmpOp::Eq => {
                self.lo = Some(value);
                self.hi = Some(value);
            }
            CmpOp::Lt => self.tighten_hi(value.saturating_sub(1)),
            CmpOp::Le => self.tighten_hi(value),
            CmpOp::Gt => self.tighten_lo(value.saturating_add(1)),
            CmpOp::Ge => self.tighten_lo(value),
        }
    }

    fn tighten_lo(&mut self, value: i32) {
        self.lo = Some(self.lo.map_or(value, |lo| lo.max(value)));
    }

    fn tighten_hi(&mut self, value: i32) {
        self.hi = Some(self.hi.map_or(value, |hi| hi.min(value)));
    }

    fn complete(self) -> Option<(i32, i32)> {
        let lo = self.lo?;
        let hi = self.hi?;
        (lo <= hi).then_some((lo, hi))
    }

    fn bounded(self, default_lo: i32, default_hi: i32) -> Option<(i32, i32)> {
        let lo = self.lo.unwrap_or(default_lo);
        let hi = self.hi.unwrap_or(default_hi);
        (lo <= hi).then_some((lo, hi))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    const fn flip(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsbmQ1RecognitionDecline {
    NullPlannerInput,
    MissingQuery,
    NotSimpleUngroupedAggregate,
    MissingFactRelation,
    MissingDateRelation,
    MissingRevenueSum,
    MissingJointree,
    MissingDateJoin,
    MissingDatePredicate,
    MissingDiscountBounds,
    MissingQuantityBounds,
}

impl SsbmQ1RecognitionDecline {
    const fn stats_key(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "ssbm_q1_recognizer_null_planner_input",
            Self::MissingQuery => "ssbm_q1_recognizer_missing_query",
            Self::NotSimpleUngroupedAggregate => "ssbm_q1_recognizer_not_simple_ungrouped_agg",
            Self::MissingFactRelation => "ssbm_q1_recognizer_missing_fact_relation",
            Self::MissingDateRelation => "ssbm_q1_recognizer_missing_date_relation",
            Self::MissingRevenueSum => "ssbm_q1_recognizer_missing_revenue_sum",
            Self::MissingJointree => "ssbm_q1_recognizer_missing_jointree",
            Self::MissingDateJoin => "ssbm_q1_recognizer_missing_date_join",
            Self::MissingDatePredicate => "ssbm_q1_recognizer_missing_date_predicate",
            Self::MissingDiscountBounds => "ssbm_q1_recognizer_missing_discount_bounds",
            Self::MissingQuantityBounds => "ssbm_q1_recognizer_missing_quantity_bounds",
        }
    }

    const fn fast_decline_context(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "upper_paths_ssbm_q1_null_planner_input",
            Self::MissingQuery => "upper_paths_ssbm_q1_missing_query",
            Self::NotSimpleUngroupedAggregate => "upper_paths_ssbm_q1_not_simple_ungrouped_agg",
            Self::MissingFactRelation => "upper_paths_ssbm_q1_missing_fact_relation",
            Self::MissingDateRelation => "upper_paths_ssbm_q1_missing_date_relation",
            Self::MissingRevenueSum => "upper_paths_ssbm_q1_missing_revenue_sum",
            Self::MissingJointree => "upper_paths_ssbm_q1_missing_jointree",
            Self::MissingDateJoin => "upper_paths_ssbm_q1_missing_date_join",
            Self::MissingDatePredicate => "upper_paths_ssbm_q1_missing_date_predicate",
            Self::MissingDiscountBounds => "upper_paths_ssbm_q1_missing_discount_bounds",
            Self::MissingQuantityBounds => "upper_paths_ssbm_q1_missing_quantity_bounds",
        }
    }

    const fn should_record_for_ssbm_shape(self) -> bool {
        matches!(
            self,
            Self::MissingRevenueSum
                | Self::MissingJointree
                | Self::MissingDateJoin
                | Self::MissingDatePredicate
                | Self::MissingDiscountBounds
                | Self::MissingQuantityBounds
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsbmQ2RecognitionDecline {
    NullPlannerInput,
    MissingQuery,
    NotGroupedAggregate,
    MissingFactRelation,
    MissingDateRelation,
    MissingPartRelation,
    MissingSupplierRelation,
    MissingRevenueSum,
    MissingGroupOutputs,
    MissingJointree,
    MissingDateJoin,
    MissingPartJoin,
    MissingSupplierJoin,
    MissingVariantPredicate,
}

impl SsbmQ2RecognitionDecline {
    const fn stats_key(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "ssbm_q2_recognizer_null_planner_input",
            Self::MissingQuery => "ssbm_q2_recognizer_missing_query",
            Self::NotGroupedAggregate => "ssbm_q2_recognizer_not_grouped_agg",
            Self::MissingFactRelation => "ssbm_q2_recognizer_missing_fact_relation",
            Self::MissingDateRelation => "ssbm_q2_recognizer_missing_date_relation",
            Self::MissingPartRelation => "ssbm_q2_recognizer_missing_part_relation",
            Self::MissingSupplierRelation => "ssbm_q2_recognizer_missing_supplier_relation",
            Self::MissingRevenueSum => "ssbm_q2_recognizer_missing_revenue_sum",
            Self::MissingGroupOutputs => "ssbm_q2_recognizer_missing_group_outputs",
            Self::MissingJointree => "ssbm_q2_recognizer_missing_jointree",
            Self::MissingDateJoin => "ssbm_q2_recognizer_missing_date_join",
            Self::MissingPartJoin => "ssbm_q2_recognizer_missing_part_join",
            Self::MissingSupplierJoin => "ssbm_q2_recognizer_missing_supplier_join",
            Self::MissingVariantPredicate => "ssbm_q2_recognizer_missing_variant_predicate",
        }
    }

    const fn fast_decline_context(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "upper_paths_ssbm_q2_null_planner_input",
            Self::MissingQuery => "upper_paths_ssbm_q2_missing_query",
            Self::NotGroupedAggregate => "upper_paths_ssbm_q2_not_grouped_agg",
            Self::MissingFactRelation => "upper_paths_ssbm_q2_missing_fact_relation",
            Self::MissingDateRelation => "upper_paths_ssbm_q2_missing_date_relation",
            Self::MissingPartRelation => "upper_paths_ssbm_q2_missing_part_relation",
            Self::MissingSupplierRelation => "upper_paths_ssbm_q2_missing_supplier_relation",
            Self::MissingRevenueSum => "upper_paths_ssbm_q2_missing_revenue_sum",
            Self::MissingGroupOutputs => "upper_paths_ssbm_q2_missing_group_outputs",
            Self::MissingJointree => "upper_paths_ssbm_q2_missing_jointree",
            Self::MissingDateJoin => "upper_paths_ssbm_q2_missing_date_join",
            Self::MissingPartJoin => "upper_paths_ssbm_q2_missing_part_join",
            Self::MissingSupplierJoin => "upper_paths_ssbm_q2_missing_supplier_join",
            Self::MissingVariantPredicate => "upper_paths_ssbm_q2_missing_variant_predicate",
        }
    }

    const fn should_record_for_ssbm_shape(self) -> bool {
        matches!(
            self,
            Self::MissingRevenueSum
                | Self::MissingGroupOutputs
                | Self::MissingJointree
                | Self::MissingDateJoin
                | Self::MissingPartJoin
                | Self::MissingSupplierJoin
                | Self::MissingVariantPredicate
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsbmQ3RecognitionDecline {
    NullPlannerInput,
    MissingQuery,
    NotGroupedAggregate,
    MissingFactRelation,
    MissingDateRelation,
    MissingCustomerRelation,
    MissingSupplierRelation,
    MissingRevenueSum,
    MissingGroupOutputs,
    MissingJointree,
    MissingDateJoin,
    MissingCustomerJoin,
    MissingSupplierJoin,
    MissingVariantPredicate,
}

impl SsbmQ3RecognitionDecline {
    const fn stats_key(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "ssbm_q3_recognizer_null_planner_input",
            Self::MissingQuery => "ssbm_q3_recognizer_missing_query",
            Self::NotGroupedAggregate => "ssbm_q3_recognizer_not_grouped_agg",
            Self::MissingFactRelation => "ssbm_q3_recognizer_missing_fact_relation",
            Self::MissingDateRelation => "ssbm_q3_recognizer_missing_date_relation",
            Self::MissingCustomerRelation => "ssbm_q3_recognizer_missing_customer_relation",
            Self::MissingSupplierRelation => "ssbm_q3_recognizer_missing_supplier_relation",
            Self::MissingRevenueSum => "ssbm_q3_recognizer_missing_revenue_sum",
            Self::MissingGroupOutputs => "ssbm_q3_recognizer_missing_group_outputs",
            Self::MissingJointree => "ssbm_q3_recognizer_missing_jointree",
            Self::MissingDateJoin => "ssbm_q3_recognizer_missing_date_join",
            Self::MissingCustomerJoin => "ssbm_q3_recognizer_missing_customer_join",
            Self::MissingSupplierJoin => "ssbm_q3_recognizer_missing_supplier_join",
            Self::MissingVariantPredicate => "ssbm_q3_recognizer_missing_variant_predicate",
        }
    }

    const fn fast_decline_context(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "upper_paths_ssbm_q3_null_planner_input",
            Self::MissingQuery => "upper_paths_ssbm_q3_missing_query",
            Self::NotGroupedAggregate => "upper_paths_ssbm_q3_not_grouped_agg",
            Self::MissingFactRelation => "upper_paths_ssbm_q3_missing_fact_relation",
            Self::MissingDateRelation => "upper_paths_ssbm_q3_missing_date_relation",
            Self::MissingCustomerRelation => "upper_paths_ssbm_q3_missing_customer_relation",
            Self::MissingSupplierRelation => "upper_paths_ssbm_q3_missing_supplier_relation",
            Self::MissingRevenueSum => "upper_paths_ssbm_q3_missing_revenue_sum",
            Self::MissingGroupOutputs => "upper_paths_ssbm_q3_missing_group_outputs",
            Self::MissingJointree => "upper_paths_ssbm_q3_missing_jointree",
            Self::MissingDateJoin => "upper_paths_ssbm_q3_missing_date_join",
            Self::MissingCustomerJoin => "upper_paths_ssbm_q3_missing_customer_join",
            Self::MissingSupplierJoin => "upper_paths_ssbm_q3_missing_supplier_join",
            Self::MissingVariantPredicate => "upper_paths_ssbm_q3_missing_variant_predicate",
        }
    }

    const fn should_record_for_ssbm_shape(self) -> bool {
        matches!(
            self,
            Self::MissingRevenueSum
                | Self::MissingGroupOutputs
                | Self::MissingJointree
                | Self::MissingDateJoin
                | Self::MissingCustomerJoin
                | Self::MissingSupplierJoin
                | Self::MissingVariantPredicate
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SsbmQ4RecognitionDecline {
    NullPlannerInput,
    MissingQuery,
    NotGroupedAggregate,
    MissingFactRelation,
    MissingDateRelation,
    MissingPartRelation,
    MissingCustomerRelation,
    MissingSupplierRelation,
    MissingProfitSum,
    MissingGroupOutputs,
    MissingJointree,
    MissingDateJoin,
    MissingPartJoin,
    MissingCustomerJoin,
    MissingSupplierJoin,
    MissingVariantPredicate,
}

impl SsbmQ4RecognitionDecline {
    const fn stats_key(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "ssbm_q4_recognizer_null_planner_input",
            Self::MissingQuery => "ssbm_q4_recognizer_missing_query",
            Self::NotGroupedAggregate => "ssbm_q4_recognizer_not_grouped_agg",
            Self::MissingFactRelation => "ssbm_q4_recognizer_missing_fact_relation",
            Self::MissingDateRelation => "ssbm_q4_recognizer_missing_date_relation",
            Self::MissingPartRelation => "ssbm_q4_recognizer_missing_part_relation",
            Self::MissingCustomerRelation => "ssbm_q4_recognizer_missing_customer_relation",
            Self::MissingSupplierRelation => "ssbm_q4_recognizer_missing_supplier_relation",
            Self::MissingProfitSum => "ssbm_q4_recognizer_missing_profit_sum",
            Self::MissingGroupOutputs => "ssbm_q4_recognizer_missing_group_outputs",
            Self::MissingJointree => "ssbm_q4_recognizer_missing_jointree",
            Self::MissingDateJoin => "ssbm_q4_recognizer_missing_date_join",
            Self::MissingPartJoin => "ssbm_q4_recognizer_missing_part_join",
            Self::MissingCustomerJoin => "ssbm_q4_recognizer_missing_customer_join",
            Self::MissingSupplierJoin => "ssbm_q4_recognizer_missing_supplier_join",
            Self::MissingVariantPredicate => "ssbm_q4_recognizer_missing_variant_predicate",
        }
    }

    const fn fast_decline_context(self) -> &'static str {
        match self {
            Self::NullPlannerInput => "upper_paths_ssbm_q4_null_planner_input",
            Self::MissingQuery => "upper_paths_ssbm_q4_missing_query",
            Self::NotGroupedAggregate => "upper_paths_ssbm_q4_not_grouped_agg",
            Self::MissingFactRelation => "upper_paths_ssbm_q4_missing_fact_relation",
            Self::MissingDateRelation => "upper_paths_ssbm_q4_missing_date_relation",
            Self::MissingPartRelation => "upper_paths_ssbm_q4_missing_part_relation",
            Self::MissingCustomerRelation => "upper_paths_ssbm_q4_missing_customer_relation",
            Self::MissingSupplierRelation => "upper_paths_ssbm_q4_missing_supplier_relation",
            Self::MissingProfitSum => "upper_paths_ssbm_q4_missing_profit_sum",
            Self::MissingGroupOutputs => "upper_paths_ssbm_q4_missing_group_outputs",
            Self::MissingJointree => "upper_paths_ssbm_q4_missing_jointree",
            Self::MissingDateJoin => "upper_paths_ssbm_q4_missing_date_join",
            Self::MissingPartJoin => "upper_paths_ssbm_q4_missing_part_join",
            Self::MissingCustomerJoin => "upper_paths_ssbm_q4_missing_customer_join",
            Self::MissingSupplierJoin => "upper_paths_ssbm_q4_missing_supplier_join",
            Self::MissingVariantPredicate => "upper_paths_ssbm_q4_missing_variant_predicate",
        }
    }

    const fn should_record_for_ssbm_shape(self) -> bool {
        matches!(
            self,
            Self::MissingProfitSum
                | Self::MissingGroupOutputs
                | Self::MissingJointree
                | Self::MissingDateJoin
                | Self::MissingPartJoin
                | Self::MissingCustomerJoin
                | Self::MissingSupplierJoin
                | Self::MissingVariantPredicate
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct VarRef {
    varno: pg_sys::Index,
    attno: i32,
}

#[derive(Debug, Default)]
struct PredicateState {
    has_date_join: bool,
    discount: I32Range,
    quantity: I32Range,
    date_year: Option<i32>,
    date_yearmonthnum: Option<i32>,
    date_weeknuminyear: Option<i32>,
}

impl PredicateState {
    fn observe_op(
        &mut self,
        op: *mut pg_sys::OpExpr,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
    ) {
        let Some(cmp) = (unsafe { cmp_op_from_op_expr(op) }) else {
            return;
        };
        let Some((left, right)) = (unsafe { op_args(op) }) else {
            return;
        };

        if let Some((left_var, right_var)) = unsafe { extract_var_var(left, right) } {
            let fact_orderdate = VarRef {
                varno: fact_relid,
                attno: LO_ORDERDATE_ATTNO,
            };
            let date_datekey = VarRef {
                varno: date_relid,
                attno: D_DATEKEY_ATTNO,
            };
            if cmp == CmpOp::Eq
                && ((left_var == fact_orderdate && right_var == date_datekey)
                    || (right_var == fact_orderdate && left_var == date_datekey))
            {
                self.has_date_join = true;
            }
            return;
        }

        if let Some((var_ref, value)) = unsafe { extract_var_const(left, right) } {
            self.observe_var_const(var_ref, cmp, value, fact_relid, date_relid);
        } else if let Some((var_ref, value)) = unsafe { extract_var_const(right, left) } {
            self.observe_var_const(var_ref, cmp.flip(), value, fact_relid, date_relid);
        }
    }

    fn observe_var_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: i32,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
    ) {
        if var_ref.varno == fact_relid {
            match var_ref.attno {
                LO_DISCOUNT_ATTNO => self.discount.apply(cmp, value),
                LO_QUANTITY_ATTNO => self.quantity.apply(cmp, value),
                _ => {}
            }
        } else if var_ref.varno == date_relid && cmp == CmpOp::Eq {
            match var_ref.attno {
                D_YEAR_ATTNO => self.date_year = Some(value),
                D_YEARMONTHNUM_ATTNO => self.date_yearmonthnum = Some(value),
                D_WEEKNUMINYEAR_ATTNO => self.date_weeknuminyear = Some(value),
                _ => {}
            }
        }
    }

    fn date_predicate(&self) -> Option<SsbmQ1DatePredicate> {
        if let Some(yearmonthnum) = self.date_yearmonthnum {
            Some(SsbmQ1DatePredicate::YearMonthNum(yearmonthnum))
        } else if let (Some(year), Some(week)) = (self.date_year, self.date_weeknuminyear) {
            Some(SsbmQ1DatePredicate::YearWeek { year, week })
        } else {
            self.date_year.map(SsbmQ1DatePredicate::Year)
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TextRange {
    lo: Option<String>,
    hi: Option<String>,
}

impl TextRange {
    fn apply(&mut self, op: CmpOp, value: String) {
        match op {
            CmpOp::Eq => {
                self.lo = Some(value.clone());
                self.hi = Some(value);
            }
            CmpOp::Lt | CmpOp::Le => self.tighten_hi(value),
            CmpOp::Gt | CmpOp::Ge => self.tighten_lo(value),
        }
    }

    fn tighten_lo(&mut self, value: String) {
        if self.lo.as_ref().is_none_or(|lo| value > *lo) {
            self.lo = Some(value);
        }
    }

    fn tighten_hi(&mut self, value: String) {
        if self.hi.as_ref().is_none_or(|hi| value < *hi) {
            self.hi = Some(value);
        }
    }

    fn is_eq(&self, value: &str) -> bool {
        self.lo.as_deref() == Some(value) && self.hi.as_deref() == Some(value)
    }

    fn is_range(&self, lo: &str, hi: &str) -> bool {
        self.lo.as_deref() == Some(lo) && self.hi.as_deref() == Some(hi)
    }
}

#[derive(Debug, Default)]
struct Q2PredicateState {
    has_date_join: bool,
    has_part_join: bool,
    has_supplier_join: bool,
    part_category: TextRange,
    part_brand: TextRange,
    supplier_region: TextRange,
}

impl Q2PredicateState {
    fn observe_op(
        &mut self,
        op: *mut pg_sys::OpExpr,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        part_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        let Some(cmp) = (unsafe { cmp_op_from_op_expr(op) }) else {
            return;
        };
        let Some((left, right)) = (unsafe { op_args(op) }) else {
            return;
        };

        if let Some((left_var, right_var)) = unsafe { extract_var_var(left, right) } {
            self.observe_join(
                left_var,
                right_var,
                cmp,
                fact_relid,
                date_relid,
                part_relid,
                supplier_relid,
            );
            return;
        }

        if let Some((var_ref, value)) = unsafe { extract_var_text_const(left, right) } {
            self.observe_text_const(var_ref, cmp, value, part_relid, supplier_relid);
        } else if let Some((var_ref, value)) = unsafe { extract_var_text_const(right, left) } {
            self.observe_text_const(var_ref, cmp.flip(), value, part_relid, supplier_relid);
        }
    }

    // Each SSBM join observer needs every participating relid to disambiguate
    // which dimension a join clause touches; grouping them into a struct would
    // only move the same fields behind one parameter.
    #[allow(clippy::too_many_arguments)]
    fn observe_join(
        &mut self,
        left_var: VarRef,
        right_var: VarRef,
        cmp: CmpOp,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        part_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if cmp != CmpOp::Eq {
            return;
        }
        let fact_orderdate = VarRef {
            varno: fact_relid,
            attno: LO_ORDERDATE_ATTNO,
        };
        let date_datekey = VarRef {
            varno: date_relid,
            attno: D_DATEKEY_ATTNO,
        };
        if (left_var == fact_orderdate && right_var == date_datekey)
            || (right_var == fact_orderdate && left_var == date_datekey)
        {
            self.has_date_join = true;
            return;
        }

        let fact_partkey = VarRef {
            varno: fact_relid,
            attno: LO_PARTKEY_ATTNO,
        };
        let part_partkey = VarRef {
            varno: part_relid,
            attno: P_PARTKEY_ATTNO,
        };
        if (left_var == fact_partkey && right_var == part_partkey)
            || (right_var == fact_partkey && left_var == part_partkey)
        {
            self.has_part_join = true;
            return;
        }

        let fact_suppkey = VarRef {
            varno: fact_relid,
            attno: LO_SUPPKEY_ATTNO,
        };
        let supplier_suppkey = VarRef {
            varno: supplier_relid,
            attno: S_SUPPKEY_ATTNO,
        };
        if (left_var == fact_suppkey && right_var == supplier_suppkey)
            || (right_var == fact_suppkey && left_var == supplier_suppkey)
        {
            self.has_supplier_join = true;
        }
    }

    fn observe_text_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: String,
        part_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if var_ref.varno == part_relid {
            match var_ref.attno {
                P_CATEGORY_ATTNO => self.part_category.apply(cmp, value),
                P_BRAND1_ATTNO => self.part_brand.apply(cmp, value),
                _ => {}
            }
        } else if var_ref.varno == supplier_relid && var_ref.attno == S_REGION_ATTNO {
            self.supplier_region.apply(cmp, value);
        }
    }

    fn variant(&self) -> Option<olap_cache::SsbmQ2Variant> {
        if self.part_category.is_eq("MFGR#12") && self.supplier_region.is_eq("AMERICA") {
            return Some(olap_cache::SsbmQ2Variant::Q2_1);
        }
        if self.part_brand.is_range("MFGR#2221", "MFGR#2228") && self.supplier_region.is_eq("ASIA")
        {
            return Some(olap_cache::SsbmQ2Variant::Q2_2);
        }
        if self.part_brand.is_eq("MFGR#2239") && self.supplier_region.is_eq("EUROPE") {
            return Some(olap_cache::SsbmQ2Variant::Q2_3);
        }
        None
    }
}

#[derive(Debug, Default)]
struct Q3PredicateState {
    has_date_join: bool,
    has_customer_join: bool,
    has_supplier_join: bool,
    date_year: I32Range,
    date_yearmonth: TextRange,
    customer_region: TextRange,
    customer_nation: TextRange,
    customer_cities: BTreeSet<String>,
    supplier_region: TextRange,
    supplier_nation: TextRange,
    supplier_cities: BTreeSet<String>,
}

impl Q3PredicateState {
    fn observe_op(
        &mut self,
        op: *mut pg_sys::OpExpr,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        let Some(cmp) = (unsafe { cmp_op_from_op_expr(op) }) else {
            return;
        };
        let Some((left, right)) = (unsafe { op_args(op) }) else {
            return;
        };

        if let Some((left_var, right_var)) = unsafe { extract_var_var(left, right) } {
            self.observe_join(
                left_var,
                right_var,
                cmp,
                fact_relid,
                date_relid,
                customer_relid,
                supplier_relid,
            );
            return;
        }

        if let Some((var_ref, value)) = unsafe { extract_var_const(left, right) } {
            self.observe_i32_const(var_ref, cmp, value, date_relid);
        } else if let Some((var_ref, value)) = unsafe { extract_var_const(right, left) } {
            self.observe_i32_const(var_ref, cmp.flip(), value, date_relid);
        }

        if let Some((var_ref, value)) = unsafe { extract_var_text_const(left, right) } {
            self.observe_text_const(
                var_ref,
                cmp,
                value,
                date_relid,
                customer_relid,
                supplier_relid,
            );
        } else if let Some((var_ref, value)) = unsafe { extract_var_text_const(right, left) } {
            self.observe_text_const(
                var_ref,
                cmp.flip(),
                value,
                date_relid,
                customer_relid,
                supplier_relid,
            );
        }
    }

    // See the Q2 observer: every participating relid is required to classify a
    // join clause; a struct would not reduce the real parameter count.
    #[allow(clippy::too_many_arguments)]
    fn observe_join(
        &mut self,
        left_var: VarRef,
        right_var: VarRef,
        cmp: CmpOp,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if cmp != CmpOp::Eq {
            return;
        }
        let fact_orderdate = VarRef {
            varno: fact_relid,
            attno: LO_ORDERDATE_ATTNO,
        };
        let date_datekey = VarRef {
            varno: date_relid,
            attno: D_DATEKEY_ATTNO,
        };
        if (left_var == fact_orderdate && right_var == date_datekey)
            || (right_var == fact_orderdate && left_var == date_datekey)
        {
            self.has_date_join = true;
            return;
        }

        let fact_custkey = VarRef {
            varno: fact_relid,
            attno: LO_CUSTKEY_ATTNO,
        };
        let customer_custkey = VarRef {
            varno: customer_relid,
            attno: C_CUSTKEY_ATTNO,
        };
        if (left_var == fact_custkey && right_var == customer_custkey)
            || (right_var == fact_custkey && left_var == customer_custkey)
        {
            self.has_customer_join = true;
            return;
        }

        let fact_suppkey = VarRef {
            varno: fact_relid,
            attno: LO_SUPPKEY_ATTNO,
        };
        let supplier_suppkey = VarRef {
            varno: supplier_relid,
            attno: S_SUPPKEY_ATTNO,
        };
        if (left_var == fact_suppkey && right_var == supplier_suppkey)
            || (right_var == fact_suppkey && left_var == supplier_suppkey)
        {
            self.has_supplier_join = true;
        }
    }

    fn observe_i32_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: i32,
        date_relid: pg_sys::Index,
    ) {
        if var_ref.varno == date_relid && var_ref.attno == D_YEAR_ATTNO {
            self.date_year.apply(cmp, value);
        }
    }

    fn observe_text_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: String,
        date_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if var_ref.varno == date_relid && var_ref.attno == D_YEARMONTH_ATTNO {
            self.date_yearmonth.apply(cmp, value);
        } else if var_ref.varno == customer_relid {
            match var_ref.attno {
                C_REGION_ATTNO => self.customer_region.apply(cmp, value),
                C_NATION_ATTNO => self.customer_nation.apply(cmp, value),
                C_CITY_ATTNO if cmp == CmpOp::Eq => {
                    self.customer_cities.insert(value);
                }
                _ => {}
            }
        } else if var_ref.varno == supplier_relid {
            match var_ref.attno {
                S_REGION_ATTNO => self.supplier_region.apply(cmp, value),
                S_NATION_ATTNO => self.supplier_nation.apply(cmp, value),
                S_CITY_ATTNO if cmp == CmpOp::Eq => {
                    self.supplier_cities.insert(value);
                }
                _ => {}
            }
        }
    }

    fn variant(&self) -> Option<olap_cache::SsbmQ3Variant> {
        let date_range = self.date_year.complete() == Some((1992, 1997));
        let city_set = BTreeSet::from(["UNITED ST0".to_owned(), "UNITED ST1".to_owned()]);
        if date_range && self.customer_region.is_eq("ASIA") && self.supplier_region.is_eq("ASIA") {
            return Some(olap_cache::SsbmQ3Variant::Q3_1);
        }
        if date_range
            && self.customer_nation.is_eq("UNITED STATES")
            && self.supplier_nation.is_eq("UNITED STATES")
        {
            return Some(olap_cache::SsbmQ3Variant::Q3_2);
        }
        if date_range && self.customer_cities == city_set && self.supplier_cities == city_set {
            return Some(olap_cache::SsbmQ3Variant::Q3_3);
        }
        if self.date_yearmonth.is_eq("Dec1997")
            && self.customer_cities == city_set
            && self.supplier_cities == city_set
        {
            return Some(olap_cache::SsbmQ3Variant::Q3_4);
        }
        None
    }
}

// One independent presence flag per dimension join; these are not a state
// machine and modeling them as enums would obscure the predicate accumulation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
struct Q4PredicateState {
    has_date_join: bool,
    has_part_join: bool,
    has_customer_join: bool,
    has_supplier_join: bool,
    date_year_values: BTreeSet<i32>,
    customer_region: TextRange,
    supplier_region: TextRange,
    supplier_nation: TextRange,
    part_mfgr_values: BTreeSet<String>,
    part_category: TextRange,
}

impl Q4PredicateState {
    fn observe_op(
        &mut self,
        op: *mut pg_sys::OpExpr,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        part_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        let Some(cmp) = (unsafe { cmp_op_from_op_expr(op) }) else {
            return;
        };
        let Some((left, right)) = (unsafe { op_args(op) }) else {
            return;
        };

        if let Some((left_var, right_var)) = unsafe { extract_var_var(left, right) } {
            self.observe_join(
                left_var,
                right_var,
                cmp,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
            return;
        }

        if let Some((var_ref, value)) = unsafe { extract_var_const(left, right) } {
            self.observe_i32_const(var_ref, cmp, value, date_relid);
        } else if let Some((var_ref, value)) = unsafe { extract_var_const(right, left) } {
            self.observe_i32_const(var_ref, cmp.flip(), value, date_relid);
        }

        if let Some((var_ref, value)) = unsafe { extract_var_text_const(left, right) } {
            self.observe_text_const(
                var_ref,
                cmp,
                value,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        } else if let Some((var_ref, value)) = unsafe { extract_var_text_const(right, left) } {
            self.observe_text_const(
                var_ref,
                cmp.flip(),
                value,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        }
    }

    // Q4 joins span every dimension table, so the observer needs all relids to
    // classify each clause; collapsing them into a struct would not help.
    #[allow(clippy::too_many_arguments)]
    fn observe_join(
        &mut self,
        left_var: VarRef,
        right_var: VarRef,
        cmp: CmpOp,
        fact_relid: pg_sys::Index,
        date_relid: pg_sys::Index,
        part_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if cmp != CmpOp::Eq {
            return;
        }

        let fact_orderdate = VarRef {
            varno: fact_relid,
            attno: LO_ORDERDATE_ATTNO,
        };
        let date_datekey = VarRef {
            varno: date_relid,
            attno: D_DATEKEY_ATTNO,
        };
        if (left_var == fact_orderdate && right_var == date_datekey)
            || (right_var == fact_orderdate && left_var == date_datekey)
        {
            self.has_date_join = true;
            return;
        }

        let fact_partkey = VarRef {
            varno: fact_relid,
            attno: LO_PARTKEY_ATTNO,
        };
        let part_partkey = VarRef {
            varno: part_relid,
            attno: P_PARTKEY_ATTNO,
        };
        if (left_var == fact_partkey && right_var == part_partkey)
            || (right_var == fact_partkey && left_var == part_partkey)
        {
            self.has_part_join = true;
            return;
        }

        let fact_custkey = VarRef {
            varno: fact_relid,
            attno: LO_CUSTKEY_ATTNO,
        };
        let customer_custkey = VarRef {
            varno: customer_relid,
            attno: C_CUSTKEY_ATTNO,
        };
        if (left_var == fact_custkey && right_var == customer_custkey)
            || (right_var == fact_custkey && left_var == customer_custkey)
        {
            self.has_customer_join = true;
            return;
        }

        let fact_suppkey = VarRef {
            varno: fact_relid,
            attno: LO_SUPPKEY_ATTNO,
        };
        let supplier_suppkey = VarRef {
            varno: supplier_relid,
            attno: S_SUPPKEY_ATTNO,
        };
        if (left_var == fact_suppkey && right_var == supplier_suppkey)
            || (right_var == fact_suppkey && left_var == supplier_suppkey)
        {
            self.has_supplier_join = true;
        }
    }

    fn observe_i32_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: i32,
        date_relid: pg_sys::Index,
    ) {
        if var_ref.varno == date_relid && var_ref.attno == D_YEAR_ATTNO && cmp == CmpOp::Eq {
            self.date_year_values.insert(value);
        }
    }

    fn observe_text_const(
        &mut self,
        var_ref: VarRef,
        cmp: CmpOp,
        value: String,
        part_relid: pg_sys::Index,
        customer_relid: pg_sys::Index,
        supplier_relid: pg_sys::Index,
    ) {
        if var_ref.varno == part_relid {
            match var_ref.attno {
                P_MFGR_ATTNO if cmp == CmpOp::Eq => {
                    self.part_mfgr_values.insert(value);
                }
                P_CATEGORY_ATTNO => self.part_category.apply(cmp, value),
                _ => {}
            }
        } else if var_ref.varno == customer_relid && var_ref.attno == C_REGION_ATTNO {
            self.customer_region.apply(cmp, value);
        } else if var_ref.varno == supplier_relid {
            match var_ref.attno {
                S_REGION_ATTNO => self.supplier_region.apply(cmp, value),
                S_NATION_ATTNO => self.supplier_nation.apply(cmp, value),
                _ => {}
            }
        }
    }

    fn variant(&self) -> Option<olap_cache::SsbmQ4Variant> {
        let mfgr_set = BTreeSet::from(["MFGR#1".to_owned(), "MFGR#2".to_owned()]);
        let year_set = BTreeSet::from([1997, 1998]);
        if self.customer_region.is_eq("AMERICA")
            && self.supplier_region.is_eq("AMERICA")
            && self.part_mfgr_values == mfgr_set
            && self.date_year_values.is_empty()
        {
            return Some(olap_cache::SsbmQ4Variant::Q4_1);
        }
        if self.customer_region.is_eq("AMERICA")
            && self.supplier_region.is_eq("AMERICA")
            && self.part_mfgr_values == mfgr_set
            && self.date_year_values == year_set
        {
            return Some(olap_cache::SsbmQ4Variant::Q4_2);
        }
        if self.customer_region.is_eq("AMERICA")
            && self.supplier_nation.is_eq("UNITED STATES")
            && self.part_category.is_eq("MFGR#14")
            && self.date_year_values == year_set
        {
            return Some(olap_cache::SsbmQ4Variant::Q4_3);
        }
        None
    }
}

/// Recognize canonical SSBM Q1.x filtered revenue aggregate.
///
/// # Safety
///
/// `root` and `input_rel` must be PostgreSQL planner-owned pointers for the
/// current `UPPERREL_GROUP_AGG` hook invocation.
unsafe fn recognize_revenue_agg_result(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) -> Result<SsbmQ1RevenueSpec, SsbmQ1RecognitionDecline> {
    if root.is_null() || input_rel.is_null() {
        return Err(SsbmQ1RecognitionDecline::NullPlannerInput);
    }
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return Err(SsbmQ1RecognitionDecline::MissingQuery);
    }
    let query_ref = unsafe { &*query };
    if !query_ref.hasAggs || !query_ref.groupClause.is_null() {
        return Err(SsbmQ1RecognitionDecline::NotSimpleUngroupedAggregate);
    }

    let fact_relid = unsafe { find_relation_varno(root, LINEORDER_REL) }
        .ok_or(SsbmQ1RecognitionDecline::MissingFactRelation)?;
    let date_relid = unsafe { find_relation_varno(root, DATE_REL) }
        .ok_or(SsbmQ1RecognitionDecline::MissingDateRelation)?;

    if !unsafe { target_has_revenue_sum(query_ref.targetList, fact_relid) } {
        return Err(SsbmQ1RecognitionDecline::MissingRevenueSum);
    }

    let jointree = query_ref.jointree;
    if jointree.is_null() {
        return Err(SsbmQ1RecognitionDecline::MissingJointree);
    }
    let mut predicates = PredicateState::default();
    unsafe {
        collect_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid);
        });
        collect_planner_qual_ops(root, input_rel, fact_relid, date_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid);
        });
    }
    if !predicates.has_date_join {
        return Err(SsbmQ1RecognitionDecline::MissingDateJoin);
    }
    let date_predicate = predicates
        .date_predicate()
        .ok_or(SsbmQ1RecognitionDecline::MissingDatePredicate)?;
    let (discount_lo, discount_hi) = predicates
        .discount
        .complete()
        .ok_or(SsbmQ1RecognitionDecline::MissingDiscountBounds)?;
    let (quantity_lo, quantity_hi) = predicates
        .quantity
        .bounded(i32::MIN, i32::MAX)
        .ok_or(SsbmQ1RecognitionDecline::MissingQuantityBounds)?;

    Ok(SsbmQ1RevenueSpec {
        fact_relid,
        date_relid,
        date_predicate,
        discount_lo,
        discount_hi,
        quantity_lo,
        quantity_hi,
    })
}

/// Recognize canonical SSBM Q2.x grouped revenue aggregates.
///
/// # Safety
///
/// `root` and `input_rel` must be PostgreSQL planner-owned pointers for the
/// current `UPPERREL_GROUP_AGG` hook invocation.
unsafe fn recognize_q2_grouped_revenue_result(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) -> Result<SsbmQ2GroupedRevenueSpec, SsbmQ2RecognitionDecline> {
    if root.is_null() || input_rel.is_null() {
        return Err(SsbmQ2RecognitionDecline::NullPlannerInput);
    }
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return Err(SsbmQ2RecognitionDecline::MissingQuery);
    }
    let query_ref = unsafe { &*query };
    if !query_ref.hasAggs || query_ref.groupClause.is_null() {
        return Err(SsbmQ2RecognitionDecline::NotGroupedAggregate);
    }

    let fact_relid = unsafe { find_relation_varno(root, LINEORDER_REL) }
        .ok_or(SsbmQ2RecognitionDecline::MissingFactRelation)?;
    let date_relid = unsafe { find_relation_varno(root, DATE_REL) }
        .ok_or(SsbmQ2RecognitionDecline::MissingDateRelation)?;
    let part_relid = unsafe { find_relation_varno(root, PART_REL) }
        .ok_or(SsbmQ2RecognitionDecline::MissingPartRelation)?;
    let supplier_relid = unsafe { find_relation_varno(root, SUPPLIER_REL) }
        .ok_or(SsbmQ2RecognitionDecline::MissingSupplierRelation)?;

    if !unsafe { target_has_q2_revenue_sum(query_ref.targetList, fact_relid) } {
        return Err(SsbmQ2RecognitionDecline::MissingRevenueSum);
    }
    if !unsafe { target_has_var(query_ref.targetList, date_relid, D_YEAR_ATTNO) }
        || !unsafe { target_has_var(query_ref.targetList, part_relid, P_BRAND1_ATTNO) }
    {
        return Err(SsbmQ2RecognitionDecline::MissingGroupOutputs);
    }

    let jointree = query_ref.jointree;
    if jointree.is_null() {
        return Err(SsbmQ2RecognitionDecline::MissingJointree);
    }
    let mut predicates = Q2PredicateState::default();
    unsafe {
        collect_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, part_relid, supplier_relid);
        });
        collect_planner_qual_ops(root, input_rel, fact_relid, date_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, part_relid, supplier_relid);
        });
        collect_simple_rel_qual_ops(root, part_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, part_relid, supplier_relid);
        });
        collect_simple_rel_qual_ops(root, supplier_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, part_relid, supplier_relid);
        });
    }

    if !predicates.has_date_join {
        return Err(SsbmQ2RecognitionDecline::MissingDateJoin);
    }
    if !predicates.has_part_join {
        return Err(SsbmQ2RecognitionDecline::MissingPartJoin);
    }
    if !predicates.has_supplier_join {
        return Err(SsbmQ2RecognitionDecline::MissingSupplierJoin);
    }
    let variant = predicates
        .variant()
        .ok_or(SsbmQ2RecognitionDecline::MissingVariantPredicate)?;

    Ok(SsbmQ2GroupedRevenueSpec {
        fact_relid,
        date_relid,
        part_relid,
        supplier_relid,
        variant,
    })
}

/// Recognize canonical SSBM Q3.x grouped revenue aggregates.
///
/// # Safety
///
/// `root` and `input_rel` must be PostgreSQL planner-owned pointers for the
/// current `UPPERREL_GROUP_AGG` hook invocation.
unsafe fn recognize_q3_grouped_revenue_result(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) -> Result<SsbmQ3GroupedRevenueSpec, SsbmQ3RecognitionDecline> {
    if root.is_null() || input_rel.is_null() {
        return Err(SsbmQ3RecognitionDecline::NullPlannerInput);
    }
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return Err(SsbmQ3RecognitionDecline::MissingQuery);
    }
    let query_ref = unsafe { &*query };
    if !query_ref.hasAggs || query_ref.groupClause.is_null() {
        return Err(SsbmQ3RecognitionDecline::NotGroupedAggregate);
    }

    let fact_relid = unsafe { find_relation_varno(root, LINEORDER_REL) }
        .ok_or(SsbmQ3RecognitionDecline::MissingFactRelation)?;
    let date_relid = unsafe { find_relation_varno(root, DATE_REL) }
        .ok_or(SsbmQ3RecognitionDecline::MissingDateRelation)?;
    let customer_relid = unsafe { find_relation_varno(root, CUSTOMER_REL) }
        .ok_or(SsbmQ3RecognitionDecline::MissingCustomerRelation)?;
    let supplier_relid = unsafe { find_relation_varno(root, SUPPLIER_REL) }
        .ok_or(SsbmQ3RecognitionDecline::MissingSupplierRelation)?;

    if !unsafe { target_has_q2_revenue_sum(query_ref.targetList, fact_relid) } {
        return Err(SsbmQ3RecognitionDecline::MissingRevenueSum);
    }

    let jointree = query_ref.jointree;
    if jointree.is_null() {
        return Err(SsbmQ3RecognitionDecline::MissingJointree);
    }
    let mut predicates = Q3PredicateState::default();
    unsafe {
        collect_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_planner_qual_ops(root, input_rel, fact_relid, date_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_simple_rel_qual_ops(root, customer_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_simple_rel_qual_ops(root, supplier_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_q3_text_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_simple_rel_q3_text_qual_ops(root, date_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_simple_rel_q3_text_qual_ops(root, customer_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
        collect_simple_rel_q3_text_qual_ops(root, supplier_relid, &mut |op| {
            predicates.observe_op(op, fact_relid, date_relid, customer_relid, supplier_relid);
        });
    }

    if !predicates.has_date_join {
        return Err(SsbmQ3RecognitionDecline::MissingDateJoin);
    }
    if !predicates.has_customer_join {
        return Err(SsbmQ3RecognitionDecline::MissingCustomerJoin);
    }
    if !predicates.has_supplier_join {
        return Err(SsbmQ3RecognitionDecline::MissingSupplierJoin);
    }
    let variant = predicates
        .variant()
        .ok_or(SsbmQ3RecognitionDecline::MissingVariantPredicate)?;

    let (customer_attno, supplier_attno) = if variant.uses_nation_labels() {
        (C_NATION_ATTNO, S_NATION_ATTNO)
    } else {
        (C_CITY_ATTNO, S_CITY_ATTNO)
    };
    let group_vars = [
        VarRef {
            varno: customer_relid,
            attno: customer_attno,
        },
        VarRef {
            varno: supplier_relid,
            attno: supplier_attno,
        },
        VarRef {
            varno: date_relid,
            attno: D_YEAR_ATTNO,
        },
    ];
    if !unsafe {
        target_matches_q3_layout(
            query_ref.targetList,
            fact_relid,
            customer_relid,
            customer_attno,
            supplier_relid,
            supplier_attno,
            date_relid,
        )
    } || !unsafe { group_clause_vars_exact(query_ref, &group_vars) }
    {
        return Err(SsbmQ3RecognitionDecline::MissingGroupOutputs);
    }

    Ok(SsbmQ3GroupedRevenueSpec {
        fact_relid,
        date_relid,
        customer_relid,
        supplier_relid,
        variant,
    })
}

/// Recognize canonical SSBM Q4.x grouped profit aggregates.
///
/// # Safety
///
/// `root` and `input_rel` must be PostgreSQL planner-owned pointers for the
/// current `UPPERREL_GROUP_AGG` hook invocation.
unsafe fn recognize_q4_grouped_profit_result(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
) -> Result<SsbmQ4GroupedProfitSpec, SsbmQ4RecognitionDecline> {
    if root.is_null() || input_rel.is_null() {
        return Err(SsbmQ4RecognitionDecline::NullPlannerInput);
    }
    let query = unsafe { (*root).parse };
    if query.is_null() {
        return Err(SsbmQ4RecognitionDecline::MissingQuery);
    }
    let query_ref = unsafe { &*query };
    if !query_ref.hasAggs || query_ref.groupClause.is_null() {
        return Err(SsbmQ4RecognitionDecline::NotGroupedAggregate);
    }

    let fact_relid = unsafe { find_relation_varno(root, LINEORDER_REL) }
        .ok_or(SsbmQ4RecognitionDecline::MissingFactRelation)?;
    let date_relid = unsafe { find_relation_varno(root, DATE_REL) }
        .ok_or(SsbmQ4RecognitionDecline::MissingDateRelation)?;
    let part_relid = unsafe { find_relation_varno(root, PART_REL) }
        .ok_or(SsbmQ4RecognitionDecline::MissingPartRelation)?;
    let customer_relid = unsafe { find_relation_varno(root, CUSTOMER_REL) }
        .ok_or(SsbmQ4RecognitionDecline::MissingCustomerRelation)?;
    let supplier_relid = unsafe { find_relation_varno(root, SUPPLIER_REL) }
        .ok_or(SsbmQ4RecognitionDecline::MissingSupplierRelation)?;

    if !unsafe { target_has_q4_profit_sum(query_ref.targetList, fact_relid) } {
        return Err(SsbmQ4RecognitionDecline::MissingProfitSum);
    }

    let jointree = query_ref.jointree;
    if jointree.is_null() {
        return Err(SsbmQ4RecognitionDecline::MissingJointree);
    }
    let mut predicates = Q4PredicateState::default();
    unsafe {
        collect_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_planner_qual_ops(root, input_rel, fact_relid, date_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_qual_ops(root, part_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_qual_ops(root, customer_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_qual_ops(root, supplier_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_q3_text_jointree_qual_ops(jointree, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_q3_text_qual_ops(root, date_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_q3_text_qual_ops(root, part_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_q3_text_qual_ops(root, customer_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
        collect_simple_rel_q3_text_qual_ops(root, supplier_relid, &mut |op| {
            predicates.observe_op(
                op,
                fact_relid,
                date_relid,
                part_relid,
                customer_relid,
                supplier_relid,
            );
        });
    }

    if !predicates.has_date_join {
        return Err(SsbmQ4RecognitionDecline::MissingDateJoin);
    }
    if !predicates.has_part_join {
        return Err(SsbmQ4RecognitionDecline::MissingPartJoin);
    }
    if !predicates.has_customer_join {
        return Err(SsbmQ4RecognitionDecline::MissingCustomerJoin);
    }
    if !predicates.has_supplier_join {
        return Err(SsbmQ4RecognitionDecline::MissingSupplierJoin);
    }
    let variant = predicates
        .variant()
        .ok_or(SsbmQ4RecognitionDecline::MissingVariantPredicate)?;

    let group_vars: Vec<VarRef> = match variant {
        olap_cache::SsbmQ4Variant::Q4_1 => vec![
            VarRef {
                varno: date_relid,
                attno: D_YEAR_ATTNO,
            },
            VarRef {
                varno: customer_relid,
                attno: C_NATION_ATTNO,
            },
        ],
        olap_cache::SsbmQ4Variant::Q4_2 => vec![
            VarRef {
                varno: date_relid,
                attno: D_YEAR_ATTNO,
            },
            VarRef {
                varno: supplier_relid,
                attno: S_NATION_ATTNO,
            },
            VarRef {
                varno: part_relid,
                attno: P_CATEGORY_ATTNO,
            },
        ],
        olap_cache::SsbmQ4Variant::Q4_3 => vec![
            VarRef {
                varno: date_relid,
                attno: D_YEAR_ATTNO,
            },
            VarRef {
                varno: supplier_relid,
                attno: S_CITY_ATTNO,
            },
            VarRef {
                varno: part_relid,
                attno: P_BRAND1_ATTNO,
            },
        ],
    };
    if !unsafe { target_matches_q4_layout(query_ref.targetList, fact_relid, &group_vars) }
        || !unsafe { group_clause_vars_exact(query_ref, &group_vars) }
    {
        return Err(SsbmQ4RecognitionDecline::MissingGroupOutputs);
    }

    Ok(SsbmQ4GroupedProfitSpec {
        fact_relid,
        date_relid,
        part_relid,
        customer_relid,
        supplier_relid,
        variant,
    })
}

unsafe fn find_relation_varno(root: *mut PlannerInfo, relname: &str) -> Option<pg_sys::Index> {
    let root_ref = unsafe { &*root };
    if !root_ref.simple_rte_array.is_null() {
        for varno in 1..root_ref.simple_rel_array_size {
            let rte = unsafe { *root_ref.simple_rte_array.offset(varno as isize) };
            if unsafe { relation_rte_name_matches(rte, relname) } {
                return Some(varno as pg_sys::Index);
            }
        }
    }
    let query = root_ref.parse;
    if query.is_null() {
        return None;
    }
    unsafe { find_relation_varno_in_rtable((*query).rtable, relname) }
}

unsafe fn relation_oid_for_varno(
    root: *mut PlannerInfo,
    varno: pg_sys::Index,
) -> Option<pg_sys::Oid> {
    let root_ref = unsafe { &*root };
    if !root_ref.simple_rte_array.is_null() && (varno as i32) < root_ref.simple_rel_array_size {
        let rte = unsafe { *root_ref.simple_rte_array.offset(varno as isize) };
        if let Some(oid) = unsafe { relation_oid_from_rte(rte) } {
            return Some(oid);
        }
    }
    let query = root_ref.parse;
    if query.is_null() || varno == 0 {
        return None;
    }
    unsafe { relation_oid_from_rtable((*query).rtable, varno) }
}

unsafe fn find_relation_varno_in_rtable(
    rtable: *mut pg_sys::List,
    relname: &str,
) -> Option<pg_sys::Index> {
    if rtable.is_null() {
        return None;
    }
    let len = unsafe { pg_sys::list_length(rtable) };
    for i in 0..len {
        let rte = unsafe { pg_sys::list_nth(rtable, i).cast::<pg_sys::RangeTblEntry>() };
        if unsafe { relation_rte_name_matches(rte, relname) } {
            return pg_sys::Index::try_from(i + 1).ok();
        }
    }
    None
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
    unsafe { relation_oid_from_rte(rte) }
}

unsafe fn relation_rte_name_matches(rte: *mut pg_sys::RangeTblEntry, relname: &str) -> bool {
    unsafe { relation_oid_from_rte(rte) }
        .and_then(|oid| unsafe { relation_name(oid) })
        .as_deref()
        == Some(relname)
}

unsafe fn relation_oid_from_rte(rte: *mut pg_sys::RangeTblEntry) -> Option<pg_sys::Oid> {
    if rte.is_null()
        || unsafe { (*rte).rtekind } != pg_sys::RTEKind::RTE_RELATION
        || unsafe { (*rte).relid } == pg_sys::InvalidOid
    {
        return None;
    }
    Some(unsafe { (*rte).relid })
}

unsafe fn relation_name(relid: pg_sys::Oid) -> Option<String> {
    let ptr = unsafe { pg_sys::get_rel_name(relid) };
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

unsafe fn target_has_revenue_sum(tlist: *mut pg_sys::List, fact_relid: pg_sys::Index) -> bool {
    if tlist.is_null() {
        return false;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        let expr = unsafe { (*tle).expr.cast::<pg_sys::Node>() };
        if unsafe { (*expr).type_ } != NodeTag::T_Aggref {
            continue;
        }
        let agg = expr.cast::<pg_sys::Aggref>();
        if !unsafe { aggref_is_sum(agg) } {
            continue;
        }
        if unsafe { aggref_arg_is_fact_price_discount_product(agg, fact_relid) } {
            return true;
        }
    }
    false
}

unsafe fn target_has_q2_revenue_sum(tlist: *mut pg_sys::List, fact_relid: pg_sys::Index) -> bool {
    if tlist.is_null() {
        return false;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        let expr = unsafe { (*tle).expr.cast::<pg_sys::Node>() };
        if unsafe { (*expr).type_ } != NodeTag::T_Aggref {
            continue;
        }
        let agg = expr.cast::<pg_sys::Aggref>();
        if unsafe { aggref_is_sum(agg) }
            && unsafe { aggref_arg_is_var(agg, fact_relid, LO_REVENUE_ATTNO) }
        {
            return true;
        }
    }
    false
}

unsafe fn target_has_q4_profit_sum(tlist: *mut pg_sys::List, fact_relid: pg_sys::Index) -> bool {
    if tlist.is_null() {
        return false;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        let expr = unsafe { (*tle).expr.cast::<pg_sys::Node>() };
        if unsafe { (*expr).type_ } != NodeTag::T_Aggref {
            continue;
        }
        let agg = expr.cast::<pg_sys::Aggref>();
        if unsafe { aggref_is_sum(agg) }
            && unsafe { aggref_arg_is_fact_revenue_supplycost_diff(agg, fact_relid) }
        {
            return true;
        }
    }
    false
}

unsafe fn target_has_var(tlist: *mut pg_sys::List, varno: pg_sys::Index, attno: i32) -> bool {
    if tlist.is_null() {
        return false;
    }
    let target = VarRef { varno, attno };
    let len = unsafe { pg_sys::list_length(tlist) };
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).expr }.is_null() {
            continue;
        }
        if unsafe { extract_var((*tle).expr.cast::<pg_sys::Node>()) } == Some(target) {
            return true;
        }
    }
    false
}

unsafe fn target_matches_q3_layout(
    tlist: *mut pg_sys::List,
    fact_relid: pg_sys::Index,
    customer_relid: pg_sys::Index,
    customer_attno: i32,
    supplier_relid: pg_sys::Index,
    supplier_attno: i32,
    date_relid: pg_sys::Index,
) -> bool {
    if tlist.is_null() {
        return false;
    }
    let expected = [
        VarRef {
            varno: customer_relid,
            attno: customer_attno,
        },
        VarRef {
            varno: supplier_relid,
            attno: supplier_attno,
        },
        VarRef {
            varno: date_relid,
            attno: D_YEAR_ATTNO,
        },
    ];
    let len = unsafe { pg_sys::list_length(tlist) };
    let mut nonjunk_idx = 0usize;
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } {
            continue;
        }
        if unsafe { (*tle).expr }.is_null() {
            return false;
        }
        let expr = unsafe { (*tle).expr.cast::<pg_sys::Node>() };
        match nonjunk_idx {
            0..=2 => {
                if unsafe { extract_var(expr) } != Some(expected[nonjunk_idx]) {
                    return false;
                }
            }
            3 => {
                if unsafe { (*expr).type_ } != NodeTag::T_Aggref {
                    return false;
                }
                let agg = expr.cast::<pg_sys::Aggref>();
                if !unsafe { aggref_is_sum(agg) }
                    || !unsafe { aggref_arg_is_var(agg, fact_relid, LO_REVENUE_ATTNO) }
                {
                    return false;
                }
            }
            _ => return false,
        }
        nonjunk_idx += 1;
    }
    nonjunk_idx == 4
}

unsafe fn target_matches_q4_layout(
    tlist: *mut pg_sys::List,
    fact_relid: pg_sys::Index,
    expected_vars: &[VarRef],
) -> bool {
    if tlist.is_null() || expected_vars.is_empty() {
        return false;
    }
    let len = unsafe { pg_sys::list_length(tlist) };
    let mut nonjunk_idx = 0usize;
    for i in 0..len {
        let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
        if tle.is_null() || unsafe { (*tle).resjunk } {
            continue;
        }
        if unsafe { (*tle).expr }.is_null() {
            return false;
        }
        let expr = unsafe { (*tle).expr.cast::<pg_sys::Node>() };
        match nonjunk_idx.cmp(&expected_vars.len()) {
            std::cmp::Ordering::Less => {
                if unsafe { extract_var(expr) } != Some(expected_vars[nonjunk_idx]) {
                    return false;
                }
            }
            std::cmp::Ordering::Equal => {
                if unsafe { (*expr).type_ } != NodeTag::T_Aggref {
                    return false;
                }
                let agg = expr.cast::<pg_sys::Aggref>();
                if !unsafe { aggref_is_sum(agg) }
                    || !unsafe { aggref_arg_is_fact_revenue_supplycost_diff(agg, fact_relid) }
                {
                    return false;
                }
            }
            std::cmp::Ordering::Greater => {
                return false;
            }
        }
        nonjunk_idx += 1;
    }
    nonjunk_idx == expected_vars.len() + 1
}

unsafe fn group_clause_vars_exact(query: &pg_sys::Query, required: &[VarRef]) -> bool {
    if query.groupClause.is_null()
        || unsafe { pg_sys::list_length(query.groupClause) } != required.len() as i32
    {
        return false;
    }
    let required_set: BTreeSet<VarRef> = required.iter().copied().collect();
    let mut actual = BTreeSet::new();
    let len = unsafe { pg_sys::list_length(query.groupClause) };
    for i in 0..len {
        let sc =
            unsafe { pg_sys::list_nth(query.groupClause, i).cast::<pg_sys::SortGroupClause>() };
        if sc.is_null() {
            return false;
        }
        let sgref = unsafe { (*sc).tleSortGroupRef };
        let Some(var_ref) = (unsafe { target_var_for_sortgroupref(query.targetList, sgref) })
        else {
            return false;
        };
        actual.insert(var_ref);
    }
    actual == required_set
}

unsafe fn target_var_for_sortgroupref(
    tlist: *mut pg_sys::List,
    sgref: pg_sys::Index,
) -> Option<VarRef> {
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

unsafe fn collect_jointree_qual_ops<F>(jointree: *mut pg_sys::FromExpr, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if jointree.is_null() {
        return;
    }
    unsafe { collect_qual_ops((*jointree).quals, observe) };

    let fromlist = unsafe { (*jointree).fromlist };
    if fromlist.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(fromlist) };
    for i in 0..len {
        let item = unsafe { pg_sys::list_nth(fromlist, i).cast::<pg_sys::Node>() };
        unsafe { collect_from_item_qual_ops(item, observe) };
    }
}

unsafe fn collect_from_item_qual_ops<F>(node: *mut pg_sys::Node, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_JoinExpr => {
            let join = node.cast::<pg_sys::JoinExpr>();
            unsafe {
                collect_from_item_qual_ops((*join).larg.cast::<pg_sys::Node>(), observe);
                collect_from_item_qual_ops((*join).rarg.cast::<pg_sys::Node>(), observe);
                collect_qual_ops((*join).quals.cast::<pg_sys::Node>(), observe);
            }
        }
        NodeTag::T_FromExpr => unsafe {
            collect_jointree_qual_ops(node.cast::<pg_sys::FromExpr>(), observe);
        },
        _ => {}
    }
}

unsafe fn collect_planner_qual_ops<F>(
    root: *mut PlannerInfo,
    input_rel: *mut RelOptInfo,
    fact_relid: pg_sys::Index,
    date_relid: pg_sys::Index,
    observe: &mut F,
) where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if !input_rel.is_null() {
        unsafe {
            collect_rel_qual_ops(input_rel, observe);
            collect_pathlist_join_qual_ops((*input_rel).pathlist, observe);
            collect_path_join_qual_ops((*input_rel).cheapest_total_path, observe);
            collect_path_join_qual_ops((*input_rel).cheapest_startup_path, observe);
        }
    }
    unsafe {
        collect_simple_rel_qual_ops(root, fact_relid, observe);
        collect_simple_rel_qual_ops(root, date_relid, observe);
    }
}

unsafe fn collect_simple_rel_qual_ops<F>(
    root: *mut PlannerInfo,
    varno: pg_sys::Index,
    observe: &mut F,
) where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if root.is_null() || varno == 0 {
        return;
    }
    let root_ref = unsafe { &*root };
    if root_ref.simple_rel_array.is_null() || (varno as i32) >= root_ref.simple_rel_array_size {
        return;
    }
    let rel = unsafe { *root_ref.simple_rel_array.offset(varno as isize) };
    unsafe { collect_rel_qual_ops(rel, observe) };
}

unsafe fn collect_rel_qual_ops<F>(rel: *mut RelOptInfo, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if rel.is_null() {
        return;
    }
    unsafe {
        collect_restrictinfo_qual_ops((*rel).baserestrictinfo, observe);
        collect_restrictinfo_qual_ops((*rel).joininfo, observe);
    }
}

unsafe fn collect_pathlist_join_qual_ops<F>(pathlist: *mut List, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if pathlist.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(pathlist) };
    for i in 0..len {
        let path = unsafe { pg_sys::list_nth(pathlist, i).cast::<pg_sys::Path>() };
        unsafe { collect_path_join_qual_ops(path, observe) };
    }
}

unsafe fn collect_path_join_qual_ops<F>(path: *mut pg_sys::Path, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if path.is_null() {
        return;
    }
    let param_info = unsafe { (*path).param_info };
    if !param_info.is_null() {
        unsafe { collect_restrictinfo_qual_ops((*param_info).ppi_clauses, observe) };
    }
    match unsafe { (*path.cast::<pg_sys::Node>()).type_ } {
        NodeTag::T_IndexPath => {
            let index = path.cast::<pg_sys::IndexPath>();
            unsafe { collect_index_clause_qual_ops((*index).indexclauses, observe) };
        }
        NodeTag::T_HashPath => {
            let hash = path.cast::<pg_sys::HashPath>();
            let join = unsafe { std::ptr::addr_of_mut!((*hash).jpath) };
            unsafe {
                collect_restrictinfo_qual_ops((*hash).path_hashclauses, observe);
                collect_join_path_qual_ops(join, observe);
            }
        }
        NodeTag::T_MergePath => {
            let merge = path.cast::<pg_sys::MergePath>();
            let join = unsafe { std::ptr::addr_of_mut!((*merge).jpath) };
            unsafe {
                collect_restrictinfo_qual_ops((*merge).path_mergeclauses, observe);
                collect_join_path_qual_ops(join, observe);
            }
        }
        NodeTag::T_NestPath => {
            let join = path.cast::<pg_sys::JoinPath>();
            unsafe { collect_join_path_qual_ops(join, observe) };
        }
        NodeTag::T_GatherPath => {
            let gather = path.cast::<pg_sys::GatherPath>();
            unsafe { collect_path_join_qual_ops((*gather).subpath, observe) };
        }
        NodeTag::T_GatherMergePath => {
            let gather = path.cast::<pg_sys::GatherMergePath>();
            unsafe { collect_path_join_qual_ops((*gather).subpath, observe) };
        }
        NodeTag::T_AggPath => {
            let agg = path.cast::<pg_sys::AggPath>();
            unsafe { collect_path_join_qual_ops((*agg).subpath, observe) };
        }
        NodeTag::T_ProjectionPath => {
            let projection = path.cast::<pg_sys::ProjectionPath>();
            unsafe { collect_path_join_qual_ops((*projection).subpath, observe) };
        }
        NodeTag::T_SortPath => {
            let sort = path.cast::<pg_sys::SortPath>();
            unsafe { collect_path_join_qual_ops((*sort).subpath, observe) };
        }
        NodeTag::T_IncrementalSortPath => {
            let sort = path.cast::<pg_sys::IncrementalSortPath>();
            unsafe { collect_path_join_qual_ops((*sort).spath.subpath, observe) };
        }
        NodeTag::T_MaterialPath => {
            let material = path.cast::<pg_sys::MaterialPath>();
            unsafe { collect_path_join_qual_ops((*material).subpath, observe) };
        }
        NodeTag::T_LimitPath => {
            let limit = path.cast::<pg_sys::LimitPath>();
            unsafe { collect_path_join_qual_ops((*limit).subpath, observe) };
        }
        _ => {}
    }
}

unsafe fn collect_index_clause_qual_ops<F>(indexclauses: *mut List, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if indexclauses.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(indexclauses) };
    for i in 0..len {
        let clause = unsafe { pg_sys::list_nth(indexclauses, i).cast::<pg_sys::IndexClause>() };
        if clause.is_null() {
            continue;
        }
        unsafe {
            let rinfo = (*clause).rinfo;
            if !rinfo.is_null() && !(*rinfo).clause.is_null() {
                collect_qual_ops((*rinfo).clause.cast::<pg_sys::Node>(), observe);
            }
            collect_node_list_qual_ops((*clause).indexquals, observe);
        }
    }
}

unsafe fn collect_join_path_qual_ops<F>(join: *mut pg_sys::JoinPath, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if join.is_null() {
        return;
    }
    unsafe {
        collect_restrictinfo_qual_ops((*join).joinrestrictinfo, observe);
        collect_path_join_qual_ops((*join).outerjoinpath, observe);
        collect_path_join_qual_ops((*join).innerjoinpath, observe);
    }
}

unsafe fn collect_node_list_qual_ops<F>(nodes: *mut List, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if nodes.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(nodes) };
    for i in 0..len {
        let node = unsafe { pg_sys::list_nth(nodes, i).cast::<pg_sys::Node>() };
        unsafe { collect_qual_ops(node, observe) };
    }
}

unsafe fn collect_restrictinfo_qual_ops<F>(restrictinfo: *mut List, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if restrictinfo.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(restrictinfo) };
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        unsafe { collect_qual_ops(clause.cast::<pg_sys::Node>(), observe) };
    }
}

unsafe fn collect_q3_text_jointree_qual_ops<F>(jointree: *mut pg_sys::FromExpr, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if jointree.is_null() {
        return;
    }
    unsafe { collect_q3_text_qual_ops((*jointree).quals, observe) };

    let fromlist = unsafe { (*jointree).fromlist };
    if fromlist.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(fromlist) };
    for i in 0..len {
        let item = unsafe { pg_sys::list_nth(fromlist, i).cast::<pg_sys::Node>() };
        unsafe { collect_q3_text_from_item_qual_ops(item, observe) };
    }
}

unsafe fn collect_q3_text_from_item_qual_ops<F>(node: *mut pg_sys::Node, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_JoinExpr => {
            let join = node.cast::<pg_sys::JoinExpr>();
            unsafe {
                collect_q3_text_from_item_qual_ops((*join).larg.cast::<pg_sys::Node>(), observe);
                collect_q3_text_from_item_qual_ops((*join).rarg.cast::<pg_sys::Node>(), observe);
                collect_q3_text_qual_ops((*join).quals.cast::<pg_sys::Node>(), observe);
            }
        }
        NodeTag::T_FromExpr => unsafe {
            collect_q3_text_jointree_qual_ops(node.cast::<pg_sys::FromExpr>(), observe);
        },
        _ => {}
    }
}

unsafe fn collect_simple_rel_q3_text_qual_ops<F>(
    root: *mut PlannerInfo,
    varno: pg_sys::Index,
    observe: &mut F,
) where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if root.is_null() || varno == 0 {
        return;
    }
    let root_ref = unsafe { &*root };
    if root_ref.simple_rel_array.is_null() || (varno as i32) >= root_ref.simple_rel_array_size {
        return;
    }
    let rel = unsafe { *root_ref.simple_rel_array.offset(varno as isize) };
    if rel.is_null() {
        return;
    }
    unsafe {
        collect_restrictinfo_q3_text_qual_ops((*rel).baserestrictinfo, observe);
        collect_restrictinfo_q3_text_qual_ops((*rel).joininfo, observe);
    }
}

unsafe fn collect_restrictinfo_q3_text_qual_ops<F>(restrictinfo: *mut List, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if restrictinfo.is_null() {
        return;
    }
    let len = unsafe { pg_sys::list_length(restrictinfo) };
    for i in 0..len {
        let ri = unsafe { pg_sys::list_nth(restrictinfo, i).cast::<pg_sys::RestrictInfo>() };
        if ri.is_null() {
            continue;
        }
        let clause = unsafe { (*ri).clause };
        if clause.is_null() {
            continue;
        }
        unsafe { collect_q3_text_qual_ops(clause.cast::<pg_sys::Node>(), observe) };
    }
}

unsafe fn collect_q3_text_qual_ops<F>(node: *mut pg_sys::Node, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            match unsafe { (*bool_expr).boolop } {
                pg_sys::BoolExprType::AND_EXPR | pg_sys::BoolExprType::OR_EXPR => {}
                _ => return,
            }
            let args = unsafe { (*bool_expr).args };
            if args.is_null() {
                return;
            }
            let len = unsafe { pg_sys::list_length(args) };
            for i in 0..len {
                let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
                unsafe { collect_q3_text_qual_ops(child, observe) };
            }
        }
        NodeTag::T_OpExpr => observe(node.cast::<pg_sys::OpExpr>()),
        _ => {}
    }
}

unsafe fn aggref_is_sum(agg: *mut pg_sys::Aggref) -> bool {
    let name_ptr = unsafe { pg_sys::get_func_name((*agg).aggfnoid) };
    if name_ptr.is_null() {
        return false;
    }
    unsafe { CStr::from_ptr(name_ptr) }.to_bytes() == b"sum"
}

unsafe fn aggref_arg_is_fact_price_discount_product(
    agg: *mut pg_sys::Aggref,
    fact_relid: pg_sys::Index,
) -> bool {
    let args = unsafe { (*agg).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 1 {
        return false;
    }
    let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return false;
    }
    let arg = unsafe { (*arg_tle).expr.cast::<pg_sys::Node>() };
    if unsafe { (*arg).type_ } != NodeTag::T_OpExpr {
        return false;
    }
    let op = arg.cast::<pg_sys::OpExpr>();
    if unsafe { op_name(op) }.as_deref() != Some("*") {
        return false;
    }
    let Some((left, right)) = (unsafe { op_args(op) }) else {
        return false;
    };
    let Some((left_var, right_var)) = (unsafe { extract_var_var(left, right) }) else {
        return false;
    };
    let price = VarRef {
        varno: fact_relid,
        attno: LO_EXTENDEDPRICE_ATTNO,
    };
    let discount = VarRef {
        varno: fact_relid,
        attno: LO_DISCOUNT_ATTNO,
    };
    (left_var == price && right_var == discount) || (left_var == discount && right_var == price)
}

unsafe fn aggref_arg_is_var(agg: *mut pg_sys::Aggref, varno: pg_sys::Index, attno: i32) -> bool {
    let args = unsafe { (*agg).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 1 {
        return false;
    }
    let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return false;
    }
    (unsafe { extract_var((*arg_tle).expr.cast::<pg_sys::Node>()) })
        == Some(VarRef { varno, attno })
}

unsafe fn aggref_arg_is_fact_revenue_supplycost_diff(
    agg: *mut pg_sys::Aggref,
    fact_relid: pg_sys::Index,
) -> bool {
    let args = unsafe { (*agg).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 1 {
        return false;
    }
    let arg_tle = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::TargetEntry>() };
    if arg_tle.is_null() || unsafe { (*arg_tle).expr }.is_null() {
        return false;
    }
    let arg = unsafe { (*arg_tle).expr.cast::<pg_sys::Node>() };
    if unsafe { (*arg).type_ } != NodeTag::T_OpExpr {
        return false;
    }
    let op = arg.cast::<pg_sys::OpExpr>();
    if unsafe { op_name(op) }.as_deref() != Some("-") {
        return false;
    }
    let Some((left, right)) = (unsafe { op_args(op) }) else {
        return false;
    };
    let revenue = VarRef {
        varno: fact_relid,
        attno: LO_REVENUE_ATTNO,
    };
    let supplycost = VarRef {
        varno: fact_relid,
        attno: LO_SUPPLYCOST_ATTNO,
    };
    (unsafe { extract_var(left) } == Some(revenue))
        && (unsafe { extract_var(right) } == Some(supplycost))
}

unsafe fn collect_qual_ops<F>(node: *mut pg_sys::Node, observe: &mut F)
where
    F: FnMut(*mut pg_sys::OpExpr),
{
    if node.is_null() {
        return;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_BoolExpr => {
            let bool_expr = node.cast::<pg_sys::BoolExpr>();
            if unsafe { (*bool_expr).boolop } != pg_sys::BoolExprType::AND_EXPR {
                return;
            }
            let args = unsafe { (*bool_expr).args };
            if args.is_null() {
                return;
            }
            let len = unsafe { pg_sys::list_length(args) };
            for i in 0..len {
                let child = unsafe { pg_sys::list_nth(args, i).cast::<pg_sys::Node>() };
                unsafe { collect_qual_ops(child, observe) };
            }
        }
        NodeTag::T_OpExpr => observe(node.cast::<pg_sys::OpExpr>()),
        _ => {}
    }
}

unsafe fn cmp_op_from_op_expr(op: *mut pg_sys::OpExpr) -> Option<CmpOp> {
    match unsafe { op_name(op) }?.as_str() {
        "=" => Some(CmpOp::Eq),
        "<" => Some(CmpOp::Lt),
        "<=" => Some(CmpOp::Le),
        ">" => Some(CmpOp::Gt),
        ">=" => Some(CmpOp::Ge),
        _ => None,
    }
}

unsafe fn op_name(op: *mut pg_sys::OpExpr) -> Option<String> {
    if op.is_null() {
        return None;
    }
    let name_ptr = unsafe { pg_sys::get_opname((*op).opno) };
    if name_ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(name_ptr) }
        .to_str()
        .ok()
        .map(ToOwned::to_owned)
}

unsafe fn op_args(op: *mut pg_sys::OpExpr) -> Option<(*mut pg_sys::Node, *mut pg_sys::Node)> {
    if op.is_null() {
        return None;
    }
    let args = unsafe { (*op).args };
    if args.is_null() || unsafe { pg_sys::list_length(args) } != 2 {
        return None;
    }
    let left = unsafe { pg_sys::list_nth(args, 0).cast::<pg_sys::Node>() };
    let right = unsafe { pg_sys::list_nth(args, 1).cast::<pg_sys::Node>() };
    Some((left, right))
}

unsafe fn extract_var_var(
    left: *mut pg_sys::Node,
    right: *mut pg_sys::Node,
) -> Option<(VarRef, VarRef)> {
    Some((unsafe { extract_var(left) }?, unsafe {
        extract_var(right)
    }?))
}

unsafe fn extract_var_const(
    var_node: *mut pg_sys::Node,
    const_node: *mut pg_sys::Node,
) -> Option<(VarRef, i32)> {
    Some((unsafe { extract_var(var_node) }?, unsafe {
        extract_i32_const(const_node)
    }?))
}

unsafe fn extract_var_text_const(
    var_node: *mut pg_sys::Node,
    const_node: *mut pg_sys::Node,
) -> Option<(VarRef, String)> {
    Some((unsafe { extract_var(var_node) }?, unsafe {
        extract_text_const(const_node)
    }?))
}

unsafe fn extract_var(node: *mut pg_sys::Node) -> Option<VarRef> {
    if node.is_null() {
        return None;
    }
    match unsafe { (*node).type_ } {
        NodeTag::T_Var => {
            let var = node.cast::<pg_sys::Var>();
            let attno = i32::from(unsafe { (*var).varattno });
            let varno = pg_sys::Index::try_from(unsafe { (*var).varno }).ok()?;
            (attno > 0).then_some(VarRef { varno, attno })
        }
        NodeTag::T_RelabelType => {
            let relabel = node.cast::<pg_sys::RelabelType>();
            unsafe { extract_var((*relabel).arg.cast::<pg_sys::Node>()) }
        }
        _ => None,
    }
}

unsafe fn extract_text_const(node: *mut pg_sys::Node) -> Option<String> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return None;
    }
    match u32::from(unsafe { (*cst).consttype }) {
        TEXTOID_RAW | BPCHAROID_RAW | VARCHAROID_RAW => {}
        _ => return None,
    }

    let original = unsafe { (*cst).constvalue }.cast_mut_ptr::<pg_sys::varlena>();
    if original.is_null() {
        return None;
    }
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    let raw = detoasted.cast::<u8>();
    let (data_ptr, data_len) = unsafe { varlena_data_and_len(raw) };
    let bytes = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };
    let value = String::from_utf8(bytes.to_vec()).ok();
    if detoasted != original {
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
    value
}

unsafe fn varlena_data_and_len(ptr: *const u8) -> (*const u8, usize) {
    let first_byte = unsafe { *ptr };
    if first_byte & 0x01 != 0 {
        let total_size = (first_byte >> 1) as usize;
        return (unsafe { ptr.add(1) }, total_size.saturating_sub(1));
    }
    let mut len_bytes = [0u8; 4];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, len_bytes.as_mut_ptr(), 4);
    }
    let total_size = (u32::from_ne_bytes(len_bytes) >> 2) as usize;
    (unsafe { ptr.add(4) }, total_size.saturating_sub(4))
}

unsafe fn extract_i32_const(node: *mut pg_sys::Node) -> Option<i32> {
    if node.is_null() || unsafe { (*node).type_ } != NodeTag::T_Const {
        return None;
    }
    let cst = node.cast::<pg_sys::Const>();
    if unsafe { (*cst).constisnull } {
        return None;
    }
    let datum = unsafe { (*cst).constvalue };
    match u32::from(unsafe { (*cst).consttype }) {
        21 => Some(i32::from(datum.value() as i16)),
        23 | 1082 => Some(datum.value() as i32),
        20 => i32::try_from(datum.value() as i64).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CmpOp, I32Range};

    #[test]
    fn range_tightens_inclusive_and_exclusive_bounds() {
        let mut range = I32Range::default();
        range.apply(CmpOp::Ge, 10);
        range.apply(CmpOp::Lt, 20);
        range.apply(CmpOp::Gt, 11);
        range.apply(CmpOp::Le, 18);
        assert_eq!(range.complete(), Some((12, 18)));
    }

    #[test]
    fn range_bounded_allows_one_sided_quantity_filter() {
        let mut range = I32Range::default();
        range.apply(CmpOp::Lt, 25);
        assert_eq!(range.bounded(i32::MIN, i32::MAX), Some((i32::MIN, 24)));
    }
}

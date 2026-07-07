//! Resident OLAP aggregate executors.

use pgrx::pg_sys;

use crate::engine::expr_compiler::opcode;
use crate::engine::olap_cache::{
    self, ResidentGroupKeyOutput, ResidentH3GroupedCountKind, ResidentMeasureOp,
    ResidentStarDimGroupAggCacheShape, SsbmQ1DatePredicate, SsbmQ2Variant, SsbmQ3Variant,
    SsbmQ4Variant,
};
use crate::engine::stats;
use crate::gpu;

pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES: usize = 4;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MIN_ROWS: usize = 8_192;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MAX_ROWS: usize = 262_144;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_HIGH_GROUP_MAX_ROWS: usize = 65_536;
const RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_MIN_ROWS: usize = usize::MAX;
const RESIDENT_DENSE_GROUPED_F64_MUL_WIDE_MIN_ROWS: usize = 1_000_000;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MIN_GROUPS: i32 = 129;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_HIGH_GROUP_MIN_GROUPS: i32 = 2_049;
const RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MAX_GROUPS: i32 = 16_384;
const RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_MAX_GROUPS: i32 = 256;
const RESIDENT_DENSE_GROUPED_F64_MUL_WIDE_MAX_GROUPS: i32 = 256;
const RESIDENT_STAR_DIM_GROUPED_F64_FUSED_UNFILTERED_MIN_ROWS: usize = 8_192;
const RESIDENT_STAR_DIM_GROUPED_F64_FUSED_FILTERED_MIN_ROWS: usize = 1_000_000;
const RESIDENT_STAR_DIM_GROUPED_F64_FUSED_MAX_GROUPS: i32 = 256;

fn resident_dense_grouped_f64_simple_wide_allowed(row_count: usize, group_count: i32) -> bool {
    if row_count < RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MIN_ROWS
        || group_count < RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MIN_GROUPS
        || group_count > RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MAX_GROUPS
    {
        return false;
    }
    if group_count >= RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_HIGH_GROUP_MIN_GROUPS {
        return row_count < RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_HIGH_GROUP_MAX_ROWS;
    }
    row_count < RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MAX_ROWS
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OlapAggSpec {
    SsbmQ1Revenue(SsbmQ1RevenueSpec),
    SsbmQ2GroupedRevenue(SsbmQ2GroupedRevenueSpec),
    SsbmQ3GroupedRevenue(SsbmQ3GroupedRevenueSpec),
    SsbmQ4GroupedProfit(SsbmQ4GroupedProfitSpec),
    ResidentDenseGroupedF64(ResidentDenseGroupedF64Spec),
    ResidentStarDimGroupedF64(ResidentStarDimGroupedF64Spec),
    ResidentH3GroupedCount(ResidentH3GroupedCountSpec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SsbmQ1RevenueSpec {
    pub fact_rel_oid: pg_sys::Oid,
    pub date_rel_oid: pg_sys::Oid,
    pub date_predicate: SsbmQ1DatePredicate,
    pub discount_lo: i32,
    pub discount_hi: i32,
    pub quantity_lo: i32,
    pub quantity_hi: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SsbmQ2GroupedRevenueSpec {
    pub fact_rel_oid: pg_sys::Oid,
    pub date_rel_oid: pg_sys::Oid,
    pub part_rel_oid: pg_sys::Oid,
    pub supplier_rel_oid: pg_sys::Oid,
    pub variant: SsbmQ2Variant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SsbmQ3GroupedRevenueSpec {
    pub fact_rel_oid: pg_sys::Oid,
    pub date_rel_oid: pg_sys::Oid,
    pub customer_rel_oid: pg_sys::Oid,
    pub supplier_rel_oid: pg_sys::Oid,
    pub variant: SsbmQ3Variant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SsbmQ4GroupedProfitSpec {
    pub fact_rel_oid: pg_sys::Oid,
    pub date_rel_oid: pg_sys::Oid,
    pub part_rel_oid: pg_sys::Oid,
    pub customer_rel_oid: pg_sys::Oid,
    pub supplier_rel_oid: pg_sys::Oid,
    pub variant: SsbmQ4Variant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentDenseGroupedF64Layout {
    GroupSumCount,
    GroupCountSum,
    GroupSumAvgCount,
    GroupMinMaxAvg,
    SingleSum,
    SingleMinMax,
    SingleStats,
    GroupSumAvgStddev,
}

impl ResidentDenseGroupedF64Layout {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::GroupSumCount => 1,
            Self::GroupCountSum => 2,
            Self::GroupSumAvgCount => 3,
            Self::GroupMinMaxAvg => 4,
            Self::SingleSum => 5,
            Self::SingleMinMax => 6,
            Self::SingleStats => 7,
            Self::GroupSumAvgStddev => 8,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::GroupSumCount),
            2 => Some(Self::GroupCountSum),
            3 => Some(Self::GroupSumAvgCount),
            4 => Some(Self::GroupMinMaxAvg),
            5 => Some(Self::SingleSum),
            6 => Some(Self::SingleMinMax),
            7 => Some(Self::SingleStats),
            8 => Some(Self::GroupSumAvgStddev),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_single_group(self) -> bool {
        matches!(
            self,
            Self::SingleSum | Self::SingleMinMax | Self::SingleStats
        )
    }

    #[must_use]
    pub const fn aggregate_mask(self) -> u32 {
        match self {
            Self::GroupSumCount
            | Self::GroupCountSum
            | Self::GroupSumAvgCount
            | Self::SingleSum => {
                gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
            }
            Self::GroupMinMaxAvg => gpu::RESIDENT_DENSE_GROUPED_F64_AGG_ALL,
            Self::SingleMinMax => {
                gpu::RESIDENT_DENSE_GROUPED_F64_AGG_MIN
                    | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_MAX
                    | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
            }
            Self::SingleStats => {
                gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM
                    | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
                    | gpu::RESIDENT_F64_REDUCE_AGG_SUMSQ
            }
            Self::GroupSumAvgStddev => {
                gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM
                    | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
                    | gpu::RESIDENT_F64_REDUCE_AGG_SUMSQ
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidentDenseGroupedF64Spec {
    pub rel_oid: pg_sys::Oid,
    pub source: ResidentDenseGroupedF64Source,
    pub layout: ResidentDenseGroupedF64Layout,
    pub logical: ResidentGroupAggLogicalSpec,
    pub measure_op: ResidentMeasureOp,
    pub requires_rhs: bool,
    pub filter_mode: ResidentDenseGroupedF64FilterMode,
    pub measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentStarDimGroupedF64Spec {
    pub shape: ResidentStarDimGroupAggCacheShape,
    pub logical: ResidentGroupAggLogicalSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentDenseGroupedF64Source {
    pub group_attno: i32,
    pub value_attno: i32,
    pub value_rhs_attno: i32,
    pub filter_attno: i32,
}

impl ResidentDenseGroupedF64Source {
    pub const UNKNOWN: Self = Self {
        group_attno: 0,
        value_attno: 0,
        value_rhs_attno: 0,
        filter_attno: 0,
    };

    #[must_use]
    pub const fn new(
        group_attno: i32,
        value_attno: i32,
        value_rhs_attno: Option<i32>,
        filter_attno: Option<i32>,
    ) -> Self {
        Self {
            group_attno,
            value_attno,
            value_rhs_attno: match value_rhs_attno {
                Some(attno) => attno,
                None => 0,
            },
            filter_attno: match filter_attno {
                Some(attno) => attno,
                None => 0,
            },
        }
    }

    #[must_use]
    pub const fn rhs_attno(self) -> Option<i32> {
        if self.value_rhs_attno > 0 {
            Some(self.value_rhs_attno)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn filter_attno(self) -> Option<i32> {
        if self.filter_attno > 0 {
            Some(self.filter_attno)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentGroupAggLogicalSpec {
    pub group_key_expr: ResidentGroupAggGroupKeyExpr,
    pub measure_expr: ResidentGroupAggMeasureExpr,
    pub filter_expr: ResidentGroupAggFilterExpr,
    pub aggregate_lane_mask: u32,
}

impl ResidentGroupAggLogicalSpec {
    pub const H3_COUNT_AGGREGATE_MASK: u32 = crate::gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT;
    pub const SSBM_SUM_AGGREGATE_MASK: u32 = crate::gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM;

    #[must_use]
    pub fn for_dense_grouped_f64(
        layout: ResidentDenseGroupedF64Layout,
        measure_op: ResidentMeasureOp,
        requires_rhs: bool,
        filter_mode: ResidentDenseGroupedF64FilterMode,
        measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
    ) -> Option<Self> {
        if measure_predicate.source == ResidentDenseGroupedF64MeasurePredicateSource::Rhs
            && !requires_rhs
        {
            return None;
        }
        Some(Self {
            group_key_expr: if layout.is_single_group() {
                ResidentGroupAggGroupKeyExpr::SingleGroup
            } else {
                ResidentGroupAggGroupKeyExpr::ResidentI32
            },
            measure_expr: ResidentGroupAggMeasureExpr::from_dense_measure_abi(
                measure_op,
                requires_rhs,
            )?,
            filter_expr: ResidentGroupAggFilterExpr::from_dense_filter_abi(
                filter_mode,
                measure_predicate,
            )?,
            aggregate_lane_mask: layout.aggregate_mask(),
        })
    }

    #[must_use]
    pub const fn for_h3_grouped_count() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::H3Index,
            measure_expr: ResidentGroupAggMeasureExpr::CountStar,
            filter_expr: ResidentGroupAggFilterExpr::None,
            aggregate_lane_mask: Self::H3_COUNT_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub const fn for_ssbm_q1_revenue() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::SingleGroup,
            measure_expr: ResidentGroupAggMeasureExpr::SsbmDiscountedRevenue,
            filter_expr: ResidentGroupAggFilterExpr::SsbmDateFactPredicate,
            aggregate_lane_mask: Self::SSBM_SUM_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub const fn for_ssbm_q2_grouped_revenue() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::SsbmYearBrand,
            measure_expr: ResidentGroupAggMeasureExpr::SsbmRevenueColumn,
            filter_expr: ResidentGroupAggFilterExpr::SsbmStarJoinMembership,
            aggregate_lane_mask: Self::SSBM_SUM_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub const fn for_ssbm_q3_grouped_revenue() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::SsbmCustomerSupplierYear,
            measure_expr: ResidentGroupAggMeasureExpr::SsbmRevenueColumn,
            filter_expr: ResidentGroupAggFilterExpr::SsbmStarJoinMembership,
            aggregate_lane_mask: Self::SSBM_SUM_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub const fn for_ssbm_q4_grouped_profit() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::SsbmYearGeoPart,
            measure_expr: ResidentGroupAggMeasureExpr::SsbmProfitRevenueMinusSupplycost,
            filter_expr: ResidentGroupAggFilterExpr::SsbmStarJoinMembership,
            aggregate_lane_mask: Self::SSBM_SUM_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub const fn for_star_dim_grouped_f64() -> Self {
        Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::StarDimension,
            measure_expr: ResidentGroupAggMeasureExpr::DirectColumn,
            filter_expr: ResidentGroupAggFilterExpr::StarJoinMembership,
            aggregate_lane_mask: Self::SSBM_SUM_AGGREGATE_MASK,
        }
    }

    #[must_use]
    pub fn from_wire_values(
        group_key_expr: i32,
        measure_expr: i32,
        filter_expr: i32,
        aggregate_lane_mask: i32,
    ) -> Option<Self> {
        Some(Self {
            group_key_expr: ResidentGroupAggGroupKeyExpr::from_i32(group_key_expr)?,
            measure_expr: ResidentGroupAggMeasureExpr::from_i32(measure_expr)?,
            filter_expr: ResidentGroupAggFilterExpr::from_i32(filter_expr)?,
            aggregate_lane_mask: u32::try_from(aggregate_lane_mask).ok()?,
        })
    }

    #[must_use]
    pub const fn predicate_spec(self) -> ResidentGroupAggPredicateSpec {
        ResidentGroupAggPredicateSpec::from_filter_expr(self.filter_expr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentGroupAggGroupKeyExpr {
    ResidentI32,
    H3Index,
    SingleGroup,
    SsbmYearBrand,
    SsbmCustomerSupplierYear,
    SsbmYearGeoPart,
    StarDimension,
}

impl ResidentGroupAggGroupKeyExpr {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::ResidentI32 => 1,
            Self::H3Index => 2,
            Self::SingleGroup => 3,
            Self::SsbmYearBrand => 4,
            Self::SsbmCustomerSupplierYear => 5,
            Self::SsbmYearGeoPart => 6,
            Self::StarDimension => 7,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::ResidentI32),
            2 => Some(Self::H3Index),
            3 => Some(Self::SingleGroup),
            4 => Some(Self::SsbmYearBrand),
            5 => Some(Self::SsbmCustomerSupplierYear),
            6 => Some(Self::SsbmYearGeoPart),
            7 => Some(Self::StarDimension),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentGroupAggMeasureExpr {
    DirectColumn,
    BinaryMul,
    BinarySub,
    CountStar,
    SsbmDiscountedRevenue,
    SsbmRevenueColumn,
    SsbmProfitRevenueMinusSupplycost,
    TwoMeasureStats,
}

impl ResidentGroupAggMeasureExpr {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::DirectColumn => 1,
            Self::BinaryMul => 2,
            Self::BinarySub => 3,
            Self::CountStar => 4,
            Self::SsbmDiscountedRevenue => 5,
            Self::SsbmRevenueColumn => 6,
            Self::SsbmProfitRevenueMinusSupplycost => 7,
            Self::TwoMeasureStats => 8,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            1 => Some(Self::DirectColumn),
            2 => Some(Self::BinaryMul),
            3 => Some(Self::BinarySub),
            4 => Some(Self::CountStar),
            5 => Some(Self::SsbmDiscountedRevenue),
            6 => Some(Self::SsbmRevenueColumn),
            7 => Some(Self::SsbmProfitRevenueMinusSupplycost),
            8 => Some(Self::TwoMeasureStats),
            _ => None,
        }
    }

    #[must_use]
    pub const fn from_dense_measure_abi(
        measure_op: ResidentMeasureOp,
        requires_rhs: bool,
    ) -> Option<Self> {
        match (measure_op, requires_rhs) {
            (ResidentMeasureOp::Column, false) => Some(Self::DirectColumn),
            (ResidentMeasureOp::Mul, true) => Some(Self::BinaryMul),
            (ResidentMeasureOp::Sub, true) => Some(Self::BinarySub),
            (ResidentMeasureOp::StatsPair, true) => Some(Self::TwoMeasureStats),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentGroupAggFilterExpr {
    None,
    WhereBool,
    AggregateFilterBool,
    CaseBool,
    CaseBoolAndValueRanges,
    CaseBoolAndRhsRanges,
    SsbmDateFactPredicate,
    SsbmStarJoinMembership,
    WhereBoolAndValueRanges,
    WhereBoolAndRhsRanges,
    AggregateFilterBoolAndValueRanges,
    AggregateFilterBoolAndRhsRanges,
    StarJoinMembership,
}

impl ResidentGroupAggFilterExpr {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::None => 0,
            Self::WhereBool => 1,
            Self::AggregateFilterBool => 2,
            Self::CaseBool => 3,
            Self::CaseBoolAndValueRanges => 4,
            Self::CaseBoolAndRhsRanges => 5,
            Self::SsbmDateFactPredicate => 6,
            Self::SsbmStarJoinMembership => 7,
            Self::WhereBoolAndValueRanges => 8,
            Self::WhereBoolAndRhsRanges => 9,
            Self::AggregateFilterBoolAndValueRanges => 10,
            Self::AggregateFilterBoolAndRhsRanges => 11,
            Self::StarJoinMembership => 12,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::WhereBool),
            2 => Some(Self::AggregateFilterBool),
            3 => Some(Self::CaseBool),
            4 => Some(Self::CaseBoolAndValueRanges),
            5 => Some(Self::CaseBoolAndRhsRanges),
            6 => Some(Self::SsbmDateFactPredicate),
            7 => Some(Self::SsbmStarJoinMembership),
            8 => Some(Self::WhereBoolAndValueRanges),
            9 => Some(Self::WhereBoolAndRhsRanges),
            10 => Some(Self::AggregateFilterBoolAndValueRanges),
            11 => Some(Self::AggregateFilterBoolAndRhsRanges),
            12 => Some(Self::StarJoinMembership),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_dense_filter_abi(
        filter_mode: ResidentDenseGroupedF64FilterMode,
        measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
    ) -> Option<Self> {
        match filter_mode {
            ResidentDenseGroupedF64FilterMode::None => (measure_predicate
                == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY)
                .then_some(Self::None),
            ResidentDenseGroupedF64FilterMode::Where => Self::for_bool_guarded_dense_predicate(
                measure_predicate,
                Self::WhereBool,
                Self::WhereBoolAndValueRanges,
                Self::WhereBoolAndRhsRanges,
            ),
            ResidentDenseGroupedF64FilterMode::AggregateFilter => {
                Self::for_bool_guarded_dense_predicate(
                    measure_predicate,
                    Self::AggregateFilterBool,
                    Self::AggregateFilterBoolAndValueRanges,
                    Self::AggregateFilterBoolAndRhsRanges,
                )
            }
            ResidentDenseGroupedF64FilterMode::MeasurePredicate => match measure_predicate.op {
                ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly => Some(Self::CaseBool),
                ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsBetween
                | ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsRanges => {
                    match measure_predicate.source {
                        ResidentDenseGroupedF64MeasurePredicateSource::Value => {
                            Some(Self::CaseBoolAndValueRanges)
                        }
                        ResidentDenseGroupedF64MeasurePredicateSource::Rhs => {
                            Some(Self::CaseBoolAndRhsRanges)
                        }
                    }
                }
            },
        }
    }

    fn for_bool_guarded_dense_predicate(
        measure_predicate: ResidentDenseGroupedF64MeasurePredicate,
        bool_only: Self,
        value_ranges: Self,
        rhs_ranges: Self,
    ) -> Option<Self> {
        match measure_predicate.op {
            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly => Some(bool_only),
            ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsBetween
            | ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsRanges => {
                match measure_predicate.source {
                    ResidentDenseGroupedF64MeasurePredicateSource::Value => Some(value_ranges),
                    ResidentDenseGroupedF64MeasurePredicateSource::Rhs => Some(rhs_ranges),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentGroupAggPredicateSpec {
    pub guard: ResidentGroupAggPredicateGuard,
    pub value_predicate: ResidentGroupAggValuePredicate,
}

impl ResidentGroupAggPredicateSpec {
    #[must_use]
    pub const fn from_filter_expr(filter_expr: ResidentGroupAggFilterExpr) -> Self {
        match filter_expr {
            ResidentGroupAggFilterExpr::None => Self {
                guard: ResidentGroupAggPredicateGuard::None,
                value_predicate: ResidentGroupAggValuePredicate::None,
            },
            ResidentGroupAggFilterExpr::WhereBool
            | ResidentGroupAggFilterExpr::AggregateFilterBool
            | ResidentGroupAggFilterExpr::CaseBool => Self {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::None,
            },
            ResidentGroupAggFilterExpr::CaseBoolAndValueRanges
            | ResidentGroupAggFilterExpr::WhereBoolAndValueRanges
            | ResidentGroupAggFilterExpr::AggregateFilterBoolAndValueRanges => Self {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::ValueRanges,
            },
            ResidentGroupAggFilterExpr::CaseBoolAndRhsRanges
            | ResidentGroupAggFilterExpr::WhereBoolAndRhsRanges
            | ResidentGroupAggFilterExpr::AggregateFilterBoolAndRhsRanges => Self {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::RhsRanges,
            },
            ResidentGroupAggFilterExpr::SsbmDateFactPredicate => Self {
                guard: ResidentGroupAggPredicateGuard::SsbmDateFactPredicate,
                value_predicate: ResidentGroupAggValuePredicate::None,
            },
            ResidentGroupAggFilterExpr::SsbmStarJoinMembership => Self {
                guard: ResidentGroupAggPredicateGuard::SsbmStarJoinMembership,
                value_predicate: ResidentGroupAggValuePredicate::None,
            },
            ResidentGroupAggFilterExpr::StarJoinMembership => Self {
                guard: ResidentGroupAggPredicateGuard::StarJoinMembership,
                value_predicate: ResidentGroupAggValuePredicate::None,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentGroupAggPredicateGuard {
    None,
    ResidentBoolColumn,
    SsbmDateFactPredicate,
    SsbmStarJoinMembership,
    StarJoinMembership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentGroupAggValuePredicate {
    None,
    ValueRanges,
    RhsRanges,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidentDenseGroupedF64MeasurePredicate {
    pub op: ResidentDenseGroupedF64MeasurePredicateOp,
    pub source: ResidentDenseGroupedF64MeasurePredicateSource,
    pub range_count: u8,
    pub range_los: [f64; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
    pub range_his: [f64; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
}

impl ResidentDenseGroupedF64MeasurePredicate {
    pub const BOOL_ONLY: Self = Self {
        op: ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly,
        source: ResidentDenseGroupedF64MeasurePredicateSource::Value,
        range_count: 0,
        range_los: [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
        range_his: [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
    };

    #[must_use]
    pub const fn bool_and_rhs_between(lo: f64, hi: f64) -> Self {
        Self {
            op: ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsRanges,
            source: ResidentDenseGroupedF64MeasurePredicateSource::Rhs,
            range_count: 1,
            range_los: [lo, 0.0, 0.0, 0.0],
            range_his: [hi, 0.0, 0.0, 0.0],
        }
    }

    #[must_use]
    pub fn bool_and_rhs_ranges(ranges: &[(f64, f64)]) -> Option<Self> {
        Self::bool_and_source_ranges(ResidentDenseGroupedF64MeasurePredicateSource::Rhs, ranges)
    }

    #[must_use]
    pub fn bool_and_value_ranges(ranges: &[(f64, f64)]) -> Option<Self> {
        Self::bool_and_source_ranges(ResidentDenseGroupedF64MeasurePredicateSource::Value, ranges)
    }

    #[must_use]
    pub fn bool_and_source_ranges(
        source: ResidentDenseGroupedF64MeasurePredicateSource,
        ranges: &[(f64, f64)],
    ) -> Option<Self> {
        if ranges.is_empty()
            || ranges.len() > RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES
        {
            return None;
        }
        let mut range_los = [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES];
        let mut range_his = [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES];
        for (idx, (lo, hi)) in ranges.iter().copied().enumerate() {
            if lo > hi || lo.is_nan() || hi.is_nan() {
                return None;
            }
            range_los[idx] = lo;
            range_his[idx] = hi;
        }
        Some(Self {
            op: ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsRanges,
            source,
            range_count: u8::try_from(ranges.len()).ok()?,
            range_los,
            range_his,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentDenseGroupedF64MeasurePredicateSource {
    Value,
    Rhs,
}

impl ResidentDenseGroupedF64MeasurePredicateSource {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::Value => 0,
            Self::Rhs => 1,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Value),
            1 => Some(Self::Rhs),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentDenseGroupedF64MeasurePredicateOp {
    BoolOnly,
    BoolAndRhsBetween,
    BoolAndRhsRanges,
}

impl ResidentDenseGroupedF64MeasurePredicateOp {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::BoolOnly => 0,
            Self::BoolAndRhsBetween => 1,
            Self::BoolAndRhsRanges => 2,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::BoolOnly),
            1 => Some(Self::BoolAndRhsBetween),
            2 => Some(Self::BoolAndRhsRanges),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentDenseGroupedF64FilterMode {
    None,
    Where,
    AggregateFilter,
    MeasurePredicate,
}

impl ResidentDenseGroupedF64FilterMode {
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Where => 1,
            Self::AggregateFilter => 2,
            Self::MeasurePredicate => 3,
        }
    }

    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Where),
            2 => Some(Self::AggregateFilter),
            3 => Some(Self::MeasurePredicate),
            _ => None,
        }
    }

    #[must_use]
    pub const fn kernel_filter_mode(self) -> i32 {
        match self {
            Self::MeasurePredicate => gpu::RESIDENT_DENSE_GROUPED_F64_FILTER_MEASURE_ONLY,
            Self::None | Self::Where | Self::AggregateFilter => {
                gpu::RESIDENT_DENSE_GROUPED_F64_FILTER_ROWS
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentH3GroupedCountSpec {
    pub rel_oid: pg_sys::Oid,
    pub kind: ResidentH3GroupedCountKind,
    pub resolution: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmQ2OutputRow {
    revenue: i64,
    year: i32,
    brand: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmQ3OutputRow {
    customer_label: String,
    supplier_label: String,
    year: i32,
    revenue: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SsbmQ4OutputRow {
    year: i32,
    geo_label: String,
    part_label: String,
    profit: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct ResidentDenseGroupedF64Output {
    group_output: ResidentGroupKeyOutput,
    sum_by_group: Vec<f64>,
    min_by_group: Vec<f64>,
    max_by_group: Vec<f64>,
    sumsq_by_group: Vec<f64>,
    rhs_sum_by_group: Vec<f64>,
    count_by_group: Vec<u32>,
    rhs_count_by_group: Vec<u32>,
}

struct ResidentH3GroupedCountOutput {
    result: gpu::HashAggResult,
    group_count: usize,
}

pub struct OlapAggExecState {
    spec: OlapAggSpec,
    result_returned: bool,
    q2_rows: Option<Vec<SsbmQ2OutputRow>>,
    q2_emit_idx: usize,
    q3_rows: Option<Vec<SsbmQ3OutputRow>>,
    q3_emit_idx: usize,
    q4_rows: Option<Vec<SsbmQ4OutputRow>>,
    q4_emit_idx: usize,
    dense_f64_output: Option<ResidentDenseGroupedF64Output>,
    dense_f64_emit_idx: usize,
    h3_count_output: Option<ResidentH3GroupedCountOutput>,
    h3_count_emit_idx: usize,
    rows_dispatched: u64,
    batches_executed: u64,
    dispatch_time_us: u64,
    gpu_dispatched: bool,
    selected_rows: u64,
    uncertain_rows: u64,
}

impl OlapAggExecState {
    #[must_use]
    pub const fn new(spec: OlapAggSpec) -> Self {
        Self {
            spec,
            result_returned: false,
            q2_rows: None,
            q2_emit_idx: 0,
            q3_rows: None,
            q3_emit_idx: 0,
            q4_rows: None,
            q4_emit_idx: 0,
            dense_f64_output: None,
            dense_f64_emit_idx: 0,
            h3_count_output: None,
            h3_count_emit_idx: 0,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            gpu_dispatched: false,
            selected_rows: 0,
            uncertain_rows: 0,
        }
    }

    #[must_use]
    pub const fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }

    #[must_use]
    pub const fn batches_executed(&self) -> u64 {
        self.batches_executed
    }

    #[must_use]
    pub const fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }

    #[must_use]
    pub const fn gpu_dispatched(&self) -> bool {
        self.gpu_dispatched
    }

    #[must_use]
    pub const fn selected_rows(&self) -> u64 {
        self.selected_rows
    }

    #[must_use]
    pub const fn uncertain_rows(&self) -> u64 {
        self.uncertain_rows
    }

    #[must_use]
    pub const fn mode_label(&self) -> &'static str {
        match self.spec {
            OlapAggSpec::SsbmQ1Revenue(_) => "SSBM Q1 revenue",
            OlapAggSpec::SsbmQ2GroupedRevenue(_) => "SSBM Q2 grouped revenue",
            OlapAggSpec::SsbmQ3GroupedRevenue(_) => "SSBM Q3 grouped revenue",
            OlapAggSpec::SsbmQ4GroupedProfit(_) => "SSBM Q4 grouped profit",
            OlapAggSpec::ResidentDenseGroupedF64(_) => "resident dense grouped f64",
            OlapAggSpec::ResidentStarDimGroupedF64(_) => "resident star dim grouped f64",
            OlapAggSpec::ResidentH3GroupedCount(_) => "resident H3 grouped count",
        }
    }

    #[must_use]
    pub const fn spec(&self) -> OlapAggSpec {
        self.spec
    }

    /// Execute the resident OLAP aggregate and emit the next bounded output row.
    ///
    /// # Safety
    ///
    /// Must run on the main PostgreSQL backend thread. `result_slot` must be a
    /// valid slot for the CustomScan output tuple descriptor.
    pub unsafe fn next(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.result_returned {
            return std::ptr::null_mut();
        }

        match self.spec {
            OlapAggSpec::SsbmQ1Revenue(spec) => unsafe {
                self.next_ssbm_q1_revenue(result_slot, &spec)
            },
            OlapAggSpec::SsbmQ2GroupedRevenue(spec) => unsafe {
                self.next_ssbm_q2_grouped_revenue(result_slot, &spec)
            },
            OlapAggSpec::SsbmQ3GroupedRevenue(spec) => unsafe {
                self.next_ssbm_q3_grouped_revenue(result_slot, &spec)
            },
            OlapAggSpec::SsbmQ4GroupedProfit(spec) => unsafe {
                self.next_ssbm_q4_grouped_profit(result_slot, &spec)
            },
            OlapAggSpec::ResidentDenseGroupedF64(spec) => unsafe {
                self.next_resident_dense_grouped_f64(result_slot, &spec)
            },
            OlapAggSpec::ResidentStarDimGroupedF64(spec) => unsafe {
                self.next_resident_star_dim_grouped_f64(result_slot, &spec)
            },
            OlapAggSpec::ResidentH3GroupedCount(spec) => unsafe {
                self.next_resident_h3_grouped_count(result_slot, &spec)
            },
        }
    }

    unsafe fn next_ssbm_q1_revenue(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &SsbmQ1RevenueSpec,
    ) -> *mut pg_sys::TupleTableSlot {
        stats::record_query_accelerated();
        let start = std::time::Instant::now();

        let result = olap_cache::with_ssbm_q1_cache(
            spec.fact_rel_oid,
            spec.date_rel_oid,
            |cache| {
                cache
                    .with_date_filter(spec.date_predicate, |date_filter| {
                        gpu::expr_template_ssbm_q1_revenue_i64_usm_raw_keys_scratch(
                            cache.orderdate_col(),
                            cache.discount_col(),
                            cache.quantity_col(),
                            cache.extendedprice_col(),
                            cache.row_count(),
                            date_filter.orderdate_lo,
                            date_filter.orderdate_hi,
                            date_filter.orderdate_keys,
                            date_filter.orderdate_key_count,
                            spec.discount_lo,
                            spec.discount_hi,
                            spec.quantity_lo,
                            spec.quantity_hi,
                            cache.scratch(),
                        )
                        .map(|result| (cache.row_count(), result))
                    })
                    .unwrap_or_else(|err| {
                        pgrx::error!(
                            "pg_accel: SSBM Q1 date-filter device allocation failed ({err}); refusing CPU fallback"
                        )
                    })
            },
        )
        .unwrap_or_else(|| {
            pgrx::error!(
                "pg_accel: SSBM Q1 resident cache missing at execution; refusing CPU fallback"
            )
        })
        .unwrap_or_else(|| {
            pgrx::error!("pg_accel: SSBM Q1 GPU revenue kernel failed; refusing CPU fallback")
        });

        let (row_count, revenue) = result;
        if revenue.uncertain_count != 0 {
            pgrx::error!(
                "pg_accel: SSBM Q1 GPU revenue kernel returned {} uncertain rows; refusing CPU fallback",
                revenue.uncertain_count,
            );
        }

        self.rows_dispatched = row_count as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.gpu_dispatched = true;
        self.selected_rows = revenue.selected_count as u64;
        self.uncertain_rows = revenue.uncertain_count as u64;
        self.result_returned = true;

        stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
        stats::record_gpu_batch(self.rows_dispatched, 0);

        // SAFETY: result_slot is valid per caller contract.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(revenue.sum);
            *(*result_slot).tts_isnull.add(0) = revenue.selected_count == 0;
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_ssbm_q2_grouped_revenue(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &SsbmQ2GroupedRevenueSpec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.q2_rows.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized = olap_cache::with_ssbm_q2_cache(
                spec.fact_rel_oid,
                spec.date_rel_oid,
                spec.part_rel_oid,
                spec.supplier_rel_oid,
                |cache| {
                    cache
                        .with_q2_filter(spec.variant, |filter| {
                            let result = gpu::expr_template_ssbm_q2_grouped_revenue_i64_usm(
                                cache.orderdate_col(),
                                cache.partkey_col(),
                                cache.suppkey_col(),
                                cache.revenue_col(),
                                cache.row_count(),
                                cache.date_key_min(),
                                cache.date_year_by_offset_ptr(),
                                cache.date_year_count(),
                                cache.part_brand_code_by_key_ptr(),
                                filter.part_match_by_key,
                                filter.part_key_count,
                                filter.supplier_match_by_key,
                                filter.supplier_key_count,
                                cache.year_min(),
                                cache.year_count(),
                                cache.brand_count(),
                            )?;

                            let year_count = usize::try_from(cache.year_count()).ok()?;
                            let brand_count = usize::try_from(cache.brand_count()).ok()?;
                            let mut rows = Vec::new();
                            for year_idx in 0..year_count {
                                let year = cache.year_min()
                                    + i32::try_from(year_idx).unwrap_or(i32::MAX);
                                for brand_idx in 0..brand_count {
                                    let group = year_idx * brand_count + brand_idx;
                                    if result.count_by_group.get(group).copied().unwrap_or(0) == 0 {
                                        continue;
                                    }
                                    let brand = cache
                                        .brand_at(brand_idx)
                                        .unwrap_or("<missing-brand>")
                                        .to_owned();
                                    rows.push(SsbmQ2OutputRow {
                                        revenue: result.revenue_by_group[group],
                                        year,
                                        brand,
                                    });
                                }
                            }
                            Some((
                                cache.row_count(),
                                result.selected_count,
                                result.uncertain_count,
                                rows,
                            ))
                        })
                        .unwrap_or_else(|err| {
                            pgrx::error!(
                                "pg_accel: SSBM Q2 dimension-filter device allocation failed ({err}); refusing CPU fallback"
                            )
                        })
                },
            )
            .unwrap_or_else(|| {
                pgrx::error!(
                    "pg_accel: SSBM Q2 resident cache missing at execution; refusing CPU fallback"
                )
            })
            .unwrap_or_else(|| {
                pgrx::error!("pg_accel: SSBM Q2 GPU grouped revenue kernel failed; refusing CPU fallback")
            });

            let (row_count, selected_count, uncertain_count, rows) = materialized;
            if uncertain_count != 0 {
                pgrx::error!(
                    "pg_accel: SSBM Q2 GPU grouped revenue kernel returned {} uncertain rows; refusing CPU fallback",
                    uncertain_count,
                );
            }

            self.rows_dispatched = row_count as u64;
            self.batches_executed = 1;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = selected_count as u64;
            self.uncertain_rows = uncertain_count as u64;
            self.q2_rows = Some(rows);

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let rows = self
            .q2_rows
            .as_ref()
            .expect("SSBM Q2 rows materialized before emission");
        let Some(row) = rows.get(self.q2_emit_idx) else {
            self.result_returned = true;
            return std::ptr::null_mut();
        };
        self.q2_emit_idx += 1;

        // SAFETY: result_slot is valid per caller contract and has the query's
        // three projected output columns: sum, d_year, p_brand1.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(row.revenue);
            *(*result_slot).tts_isnull.add(0) = false;
            *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(row.year);
            *(*result_slot).tts_isnull.add(1) = false;
            *(*result_slot).tts_values.add(2) = text_datum_from_str(&row.brand);
            *(*result_slot).tts_isnull.add(2) = false;
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_ssbm_q3_grouped_revenue(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &SsbmQ3GroupedRevenueSpec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.q3_rows.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized = olap_cache::with_ssbm_q3_cache(
                spec.fact_rel_oid,
                spec.date_rel_oid,
                spec.customer_rel_oid,
                spec.supplier_rel_oid,
                |cache| {
                    cache
                        .with_q3_filter(spec.variant, |filter| {
                            let customer_group_count =
                                i32::try_from(filter.customer_labels.len()).ok()?;
                            let supplier_group_count =
                                i32::try_from(filter.supplier_labels.len()).ok()?;
                            let result = gpu::expr_template_ssbm_q3_grouped_revenue_i64_usm(
                                cache.orderdate_col(),
                                cache.custkey_col(),
                                cache.suppkey_col(),
                                cache.revenue_col(),
                                cache.row_count(),
                                cache.date_key_min(),
                                cache.date_year_by_offset_ptr(),
                                filter.date_match_by_offset,
                                filter.date_year_count,
                                filter.customer_group_code_by_key,
                                filter.customer_match_by_key,
                                filter.customer_key_count,
                                filter.supplier_group_code_by_key,
                                filter.supplier_match_by_key,
                                filter.supplier_key_count,
                                cache.year_min(),
                                cache.year_count(),
                                customer_group_count,
                                supplier_group_count,
                            )?;

                            let year_count = usize::try_from(cache.year_count()).ok()?;
                            let customer_group_count_usize =
                                usize::try_from(customer_group_count).ok()?;
                            let supplier_group_count_usize =
                                usize::try_from(supplier_group_count).ok()?;
                            let mut rows = Vec::new();
                            for year_idx in 0..year_count {
                                let year = cache.year_min()
                                    + i32::try_from(year_idx).unwrap_or(i32::MAX);
                                for customer_idx in 0..customer_group_count_usize {
                                    for supplier_idx in 0..supplier_group_count_usize {
                                        let group = (year_idx * customer_group_count_usize
                                            + customer_idx)
                                            * supplier_group_count_usize
                                            + supplier_idx;
                                        if result
                                            .count_by_group
                                            .get(group)
                                            .copied()
                                            .unwrap_or(0)
                                            == 0
                                        {
                                            continue;
                                        }
                                        rows.push(SsbmQ3OutputRow {
                                            customer_label: filter.customer_labels[customer_idx]
                                                .clone(),
                                            supplier_label: filter.supplier_labels[supplier_idx]
                                                .clone(),
                                            year,
                                            revenue: result.revenue_by_group[group],
                                        });
                                    }
                                }
                            }
                            rows.sort_by(|left, right| {
                                left.year
                                    .cmp(&right.year)
                                    .then_with(|| right.revenue.cmp(&left.revenue))
                                    .then_with(|| left.customer_label.cmp(&right.customer_label))
                                    .then_with(|| left.supplier_label.cmp(&right.supplier_label))
                            });
                            Some((
                                cache.row_count(),
                                result.selected_count,
                                result.uncertain_count,
                                rows,
                            ))
                        })
                        .unwrap_or_else(|err| {
                            pgrx::error!(
                                "pg_accel: SSBM Q3 dimension-filter device allocation failed ({err}); refusing CPU fallback"
                            )
                        })
                },
            )
            .unwrap_or_else(|| {
                pgrx::error!(
                    "pg_accel: SSBM Q3 resident cache missing at execution; refusing CPU fallback"
                )
            })
            .unwrap_or_else(|| {
                pgrx::error!("pg_accel: SSBM Q3 GPU grouped revenue kernel failed; refusing CPU fallback")
            });

            let (row_count, selected_count, uncertain_count, rows) = materialized;
            if uncertain_count != 0 {
                pgrx::error!(
                    "pg_accel: SSBM Q3 GPU grouped revenue kernel returned {} uncertain rows; refusing CPU fallback",
                    uncertain_count,
                );
            }

            self.rows_dispatched = row_count as u64;
            self.batches_executed = 1;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = selected_count as u64;
            self.uncertain_rows = uncertain_count as u64;
            self.q3_rows = Some(rows);

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let rows = self
            .q3_rows
            .as_ref()
            .expect("SSBM Q3 rows materialized before emission");
        let Some(row) = rows.get(self.q3_emit_idx) else {
            self.result_returned = true;
            return std::ptr::null_mut();
        };
        self.q3_emit_idx += 1;

        // SAFETY: result_slot is valid per caller contract and has the query's
        // four projected output columns: customer geo, supplier geo, year, sum.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            *(*result_slot).tts_values.add(0) = text_datum_from_str(&row.customer_label);
            *(*result_slot).tts_isnull.add(0) = false;
            *(*result_slot).tts_values.add(1) = text_datum_from_str(&row.supplier_label);
            *(*result_slot).tts_isnull.add(1) = false;
            *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(row.year);
            *(*result_slot).tts_isnull.add(2) = false;
            *(*result_slot).tts_values.add(3) = pg_sys::Datum::from(row.revenue);
            *(*result_slot).tts_isnull.add(3) = false;
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_ssbm_q4_grouped_profit(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &SsbmQ4GroupedProfitSpec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.q4_rows.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized = olap_cache::with_ssbm_q4_cache(
                spec.fact_rel_oid,
                spec.date_rel_oid,
                spec.part_rel_oid,
                spec.customer_rel_oid,
                spec.supplier_rel_oid,
                |cache| {
                    cache
                        .with_q4_filter(spec.variant, |filter| {
                            let geo_group_count = i32::try_from(filter.geo_labels.len()).ok()?;
                            let part_group_count = i32::try_from(filter.part_labels.len()).ok()?;
                            let result = gpu::expr_template_ssbm_q4_grouped_profit_i64_usm(
                                cache.orderdate_col(),
                                cache.custkey_col(),
                                cache.suppkey_col(),
                                cache.partkey_col(),
                                cache.revenue_col(),
                                cache.supplycost_col(),
                                cache.row_count(),
                                cache.date_key_min(),
                                cache.date_year_by_offset_ptr(),
                                filter.date_match_by_offset,
                                filter.date_year_count,
                                filter.customer_group_code_by_key,
                                filter.customer_match_by_key,
                                filter.customer_key_count,
                                filter.supplier_group_code_by_key,
                                filter.supplier_match_by_key,
                                filter.supplier_key_count,
                                filter.part_group_code_by_key,
                                filter.part_match_by_key,
                                filter.part_key_count,
                                filter.group_geo_source,
                                cache.year_min(),
                                cache.year_count(),
                                geo_group_count,
                                part_group_count,
                                cache.q4_scratch_profit_lo_ptr(),
                                cache.q4_scratch_profit_hi_ptr(),
                                cache.q4_scratch_count_ptr(),
                                cache.q4_scratch_group_capacity(),
                            )?;

                            let year_count = usize::try_from(cache.year_count()).ok()?;
                            let geo_group_count_usize =
                                usize::try_from(geo_group_count).ok()?;
                            let part_group_count_usize =
                                usize::try_from(part_group_count).ok()?;
                            let mut rows = Vec::new();
                            for year_idx in 0..year_count {
                                let year = cache.year_min()
                                    + i32::try_from(year_idx).unwrap_or(i32::MAX);
                                for geo_idx in 0..geo_group_count_usize {
                                    for part_idx in 0..part_group_count_usize {
                                        let group = (year_idx * geo_group_count_usize + geo_idx)
                                            * part_group_count_usize
                                            + part_idx;
                                        if result
                                            .count_by_group
                                            .get(group)
                                            .copied()
                                            .unwrap_or(0)
                                            == 0
                                        {
                                            continue;
                                        }
                                        rows.push(SsbmQ4OutputRow {
                                            year,
                                            geo_label: filter.geo_labels[geo_idx].clone(),
                                            part_label: filter.part_labels[part_idx].clone(),
                                            profit: result.profit_by_group[group],
                                        });
                                    }
                                }
                            }
                            rows.sort_by(|left, right| {
                                left.year
                                    .cmp(&right.year)
                                    .then_with(|| left.geo_label.cmp(&right.geo_label))
                                    .then_with(|| left.part_label.cmp(&right.part_label))
                            });
                            Some((
                                cache.row_count(),
                                result.selected_count,
                                result.uncertain_count,
                                rows,
                            ))
                        })
                        .unwrap_or_else(|err| {
                            pgrx::error!(
                                "pg_accel: SSBM Q4 dimension-filter device allocation failed ({err}); refusing CPU fallback"
                            )
                        })
                },
            )
            .unwrap_or_else(|| {
                pgrx::error!(
                    "pg_accel: SSBM Q4 resident cache missing at execution; refusing CPU fallback"
                )
            })
            .unwrap_or_else(|| {
                pgrx::error!(
                    "pg_accel: SSBM Q4 GPU grouped profit kernel failed; refusing CPU fallback"
                )
            });

            let (row_count, selected_count, uncertain_count, rows) = materialized;
            if uncertain_count != 0 {
                pgrx::error!(
                    "pg_accel: SSBM Q4 GPU grouped profit kernel returned {} uncertain rows; refusing CPU fallback",
                    uncertain_count,
                );
            }

            self.rows_dispatched = row_count as u64;
            self.batches_executed = 1;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = selected_count as u64;
            self.uncertain_rows = uncertain_count as u64;
            self.q4_rows = Some(rows);

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let rows = self
            .q4_rows
            .as_ref()
            .expect("SSBM Q4 rows materialized before emission");
        let Some(row) = rows.get(self.q4_emit_idx) else {
            self.result_returned = true;
            return std::ptr::null_mut();
        };
        self.q4_emit_idx += 1;

        // SAFETY: result_slot is valid per caller contract and has the query's
        // projected output columns. Q4.1 omits part_label; Q4.2/Q4.3 include it.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(row.year);
            *(*result_slot).tts_isnull.add(0) = false;
            *(*result_slot).tts_values.add(1) = text_datum_from_str(&row.geo_label);
            *(*result_slot).tts_isnull.add(1) = false;
            if spec.variant.emits_part_label() {
                *(*result_slot).tts_values.add(2) = text_datum_from_str(&row.part_label);
                *(*result_slot).tts_isnull.add(2) = false;
                *(*result_slot).tts_values.add(3) = pg_sys::Datum::from(row.profit);
                *(*result_slot).tts_isnull.add(3) = false;
            } else {
                *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(row.profit);
                *(*result_slot).tts_isnull.add(2) = false;
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_resident_dense_grouped_f64(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &ResidentDenseGroupedF64Spec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.dense_f64_output.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized = olap_cache::with_resident_dense_groupagg_cache(
                spec.rel_oid,
                spec.measure_op,
                spec.requires_rhs,
                spec.filter_mode != ResidentDenseGroupedF64FilterMode::None,
                |cache| {
                    if spec.source != ResidentDenseGroupedF64Source::UNKNOWN {
                        let cache_shape = cache.shape();
                        let cache_source = ResidentDenseGroupedF64Source::new(
                            cache_shape.group_attno,
                            cache_shape.value_attno,
                            cache_shape.value_rhs_attno,
                            cache_shape.filter_attno,
                        );
                        if cache_source != spec.source {
                            pgrx::warning!(
                                "pg_accel: resident dense grouped f64 cache source mismatch: \
                                 planned=({},{},{},{}) loaded=({},{},{},{}) rel_oid={}",
                                spec.source.group_attno,
                                spec.source.value_attno,
                                spec.source.value_rhs_attno,
                                spec.source.filter_attno,
                                cache_source.group_attno,
                                cache_source.value_attno,
                                cache_source.value_rhs_attno,
                                cache_source.filter_attno,
                                spec.rel_oid,
                            );
                            return None;
                        }
                    }
                    if spec.layout.is_single_group() {
                        if cache.measure_op() != ResidentMeasureOp::Column
                            || spec.requires_rhs
                            || spec.filter_mode != ResidentDenseGroupedF64FilterMode::None
                            || spec.measure_predicate
                                != ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                        {
                            pgrx::warning!(
                                "pg_accel: resident scalar f64 reduce received non-scalar cache shape: rel_oid={}",
                                spec.rel_oid,
                            );
                            return None;
                        }
                        let result = match gpu::try_expr_template_reduce_f64_usm(
                            cache.value_col(),
                            spec.layout.aggregate_mask(),
                            cache.row_count(),
                        ) {
                            Ok(result) => result,
                            Err(status) => {
                                pgrx::warning!(
                                    "pg_accel: resident scalar f64 reduce dispatch failed: status={:?} rel_oid={} rows={}",
                                    status,
                                    spec.rel_oid,
                                    cache.row_count(),
                                );
                                return None;
                            }
                        };
                        let count = if let Ok(count) = u32::try_from(result.count) {
                            count
                        } else {
                            pgrx::warning!(
                                "pg_accel: resident scalar f64 reduce count exceeds executor lane: rel_oid={} count={}",
                                spec.rel_oid,
                                result.count,
                            );
                            return None;
                        };
                        return Some((
                            cache.row_count(),
                            result.selected_count,
                            result.uncertain_count,
                            ResidentDenseGroupedF64Output {
                                group_output: cache.group_output().clone(),
                                sum_by_group: match spec.layout {
                                    ResidentDenseGroupedF64Layout::SingleSum
                                    | ResidentDenseGroupedF64Layout::SingleStats => {
                                        vec![result.sum]
                                    }
                                    _ => Vec::new(),
                                },
                                min_by_group: if spec.layout
                                    == ResidentDenseGroupedF64Layout::SingleMinMax
                                {
                                    vec![result.min]
                                } else {
                                    Vec::new()
                                },
                                max_by_group: if spec.layout
                                    == ResidentDenseGroupedF64Layout::SingleMinMax
                                {
                                    vec![result.max]
                                } else {
                                    Vec::new()
                                },
                                sumsq_by_group: if spec.layout
                                    == ResidentDenseGroupedF64Layout::SingleStats
                                {
                                    vec![result.sumsq]
                                } else {
                                    Vec::new()
                                },
                                rhs_sum_by_group: Vec::new(),
                                count_by_group: vec![count],
                                rhs_count_by_group: Vec::new(),
                            },
                        ));
                    }
                    let can_use_compact_filter = matches!(
                        spec.filter_mode,
                        ResidentDenseGroupedF64FilterMode::Where
                            | ResidentDenseGroupedF64FilterMode::AggregateFilter
                    ) && spec.measure_predicate == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY;
                    let compact_filter_cols = can_use_compact_filter
                        .then(|| {
                            Some((
                                cache.filtered_group_col()?,
                                cache.filtered_value_col()?,
                                if spec.requires_rhs {
                                    Some(cache.filtered_value_rhs_col()?)
                                } else {
                                    None
                                },
                                cache.filtered_row_count(),
                            ))
                        })
                        .flatten();
                    let rhs_col_for_stats_pair = cache.value_rhs_col();
                    let uses_stats_pair_kernel =
                        cache.measure_op() == ResidentMeasureOp::StatsPair
                            && spec.requires_rhs
                            && spec.filter_mode == ResidentDenseGroupedF64FilterMode::None
                            && spec.measure_predicate
                                == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                            && spec.layout == ResidentDenseGroupedF64Layout::GroupSumAvgStddev
                            && rhs_col_for_stats_pair.is_some();
                    let uses_simple_sum_count_kernel = cache.measure_op() == ResidentMeasureOp::Column
                        && !spec.requires_rhs
                        && spec.filter_mode == ResidentDenseGroupedF64FilterMode::None
                        && spec.measure_predicate == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                        && matches!(
                            spec.layout,
                            ResidentDenseGroupedF64Layout::GroupSumCount
                                | ResidentDenseGroupedF64Layout::GroupCountSum
                                | ResidentDenseGroupedF64Layout::GroupSumAvgCount
                        )
                        && resident_dense_grouped_f64_simple_wide_allowed(
                            cache.row_count(),
                            cache.group_count(),
                        );
                    let mul_filter_mode = match spec.filter_mode {
                        ResidentDenseGroupedF64FilterMode::None => {
                            Some(gpu::RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_NONE)
                        }
                        ResidentDenseGroupedF64FilterMode::AggregateFilter => {
                            Some(gpu::RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_AGGREGATE)
                        }
                        ResidentDenseGroupedF64FilterMode::MeasurePredicate => {
                            Some(gpu::RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_MEASURE_ONLY)
                        }
                        ResidentDenseGroupedF64FilterMode::Where => None,
                    };
                    let rhs_col_for_mul = cache.value_rhs_col();
                    let filter_col_for_mul = cache.filter_col();
                    let rhs_col_for_predicate = cache.value_rhs_col();
                    let filter_col_for_predicate = cache.filter_col();
                    let predicate_wide_rhs_required = cache.measure_op() != ResidentMeasureOp::Column
                        || spec.measure_predicate.source
                            == ResidentDenseGroupedF64MeasurePredicateSource::Rhs;
                    let uses_predicate_sum_count_kernel =
                        cache.measure_op() != ResidentMeasureOp::StatsPair
                            && spec.filter_mode == ResidentDenseGroupedF64FilterMode::MeasurePredicate
                            && spec.measure_predicate
                                != ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                            && matches!(
                                spec.layout,
                                ResidentDenseGroupedF64Layout::GroupSumCount
                                    | ResidentDenseGroupedF64Layout::GroupCountSum
                                    | ResidentDenseGroupedF64Layout::GroupSumAvgCount
                            )
                            && cache.row_count()
                                >= RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_MIN_ROWS
                            && cache.group_count()
                                <= RESIDENT_DENSE_GROUPED_F64_PREDICATE_WIDE_MAX_GROUPS
                            && (!predicate_wide_rhs_required || rhs_col_for_predicate.is_some());
                    let uses_mul_sum_count_kernel = cache.measure_op() == ResidentMeasureOp::Mul
                        && spec.requires_rhs
                        && spec.measure_predicate == ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY
                        && matches!(
                            spec.layout,
                            ResidentDenseGroupedF64Layout::GroupSumCount
                                | ResidentDenseGroupedF64Layout::GroupCountSum
                                | ResidentDenseGroupedF64Layout::GroupSumAvgCount
                        )
                        && cache.row_count() >= RESIDENT_DENSE_GROUPED_F64_MUL_WIDE_MIN_ROWS
                        && cache.group_count() >= RESIDENT_DENSE_GROUPED_F64_SIMPLE_WIDE_MIN_GROUPS
                        && cache.group_count() <= RESIDENT_DENSE_GROUPED_F64_MUL_WIDE_MAX_GROUPS
                        && rhs_col_for_mul.is_some()
                        && mul_filter_mode.is_some()
                        && (spec.filter_mode == ResidentDenseGroupedF64FilterMode::None
                            || filter_col_for_mul.is_some());
                    let (dispatched_rows, result) = if let Some((
                        compact_group_col,
                        compact_value_col,
                        compact_rhs_col,
                        compact_row_count,
                    )) = compact_filter_cols
                    {
                        (
                            compact_row_count,
                            gpu::try_expr_template_resident_dense_grouped_f64_usm_masked(
                                compact_group_col,
                                compact_value_col,
                                compact_rhs_col,
                                cache.measure_op().to_i32(),
                                spec.layout.aggregate_mask(),
                                ResidentDenseGroupedF64FilterMode::None.kernel_filter_mode(),
                                ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
                                ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
                                0,
                                [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
                                [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
                                None,
                                compact_row_count,
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_min_ptr(),
                                cache.scratch_max_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_group_start_ptr(),
                                cache.scratch_group_cursor_ptr(),
                                cache.scratch_group_capacity(),
                                cache.scratch_sorted_group_ptr(),
                                cache.scratch_row_index_ptr(),
                                cache.scratch_row_capacity(),
                                cache.scratch_partial_sum_ptr(),
                                cache.scratch_partial_min_ptr(),
                                cache.scratch_partial_max_ptr(),
                                cache.scratch_partial_count_ptr(),
                                cache.scratch_partial_capacity(),
                            ),
                        )
                    } else if uses_stats_pair_kernel {
                        (
                            cache.row_count(),
                            gpu::try_expr_template_resident_dense_grouped_f64_stats_pair_usm(
                                cache.group_col(),
                                cache.value_col(),
                                rhs_col_for_stats_pair.expect("checked above"),
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_min_ptr(),
                                cache.scratch_max_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_group_start_ptr(),
                                cache.scratch_group_cursor_ptr(),
                                cache.scratch_group_capacity(),
                                cache.scratch_sorted_group_ptr(),
                                cache.scratch_row_index_ptr(),
                                cache.scratch_row_capacity(),
                            ),
                        )
                    } else if uses_simple_sum_count_kernel {
                        (
                            cache.row_count(),
                            gpu::try_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
                                cache.group_col(),
                                cache.value_col(),
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_partial_sum_ptr(),
                                cache.scratch_partial_count_ptr(),
                                cache.scratch_partial_capacity(),
                            ),
                        )
                    } else if uses_predicate_sum_count_kernel {
                        (
                            cache.row_count(),
                            gpu::try_expr_template_resident_dense_grouped_f64_pred_sum_count_usm(
                                cache.group_col(),
                                cache.value_col(),
                                rhs_col_for_predicate,
                                filter_col_for_predicate,
                                cache.measure_op().to_i32(),
                                spec.filter_mode.kernel_filter_mode(),
                                spec.measure_predicate.op.to_i32(),
                                spec.measure_predicate.source.to_i32(),
                                i32::from(spec.measure_predicate.range_count),
                                spec.measure_predicate.range_los,
                                spec.measure_predicate.range_his,
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_partial_sum_ptr(),
                                cache.scratch_partial_count_ptr(),
                                cache.scratch_partial_capacity(),
                            ),
                        )
                    } else if uses_mul_sum_count_kernel {
                        (
                            cache.row_count(),
                            gpu::try_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
                                cache.group_col(),
                                cache.value_col(),
                                rhs_col_for_mul.expect("checked above"),
                                filter_col_for_mul,
                                mul_filter_mode.expect("checked above"),
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_partial_sum_ptr(),
                                cache.scratch_partial_count_ptr(),
                                cache.scratch_partial_capacity(),
                            ),
                        )
                    } else {
                        (
                            cache.row_count(),
                            gpu::try_expr_template_resident_dense_grouped_f64_usm_masked(
                                cache.group_col(),
                                cache.value_col(),
                                cache.value_rhs_col(),
                                cache.measure_op().to_i32(),
                                spec.layout.aggregate_mask(),
                                spec.filter_mode.kernel_filter_mode(),
                                spec.measure_predicate.op.to_i32(),
                                spec.measure_predicate.source.to_i32(),
                                i32::from(spec.measure_predicate.range_count),
                                spec.measure_predicate.range_los,
                                spec.measure_predicate.range_his,
                                cache.filter_col(),
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_sum_ptr(),
                                cache.scratch_min_ptr(),
                                cache.scratch_max_ptr(),
                                cache.scratch_count_ptr(),
                                cache.scratch_group_start_ptr(),
                                cache.scratch_group_cursor_ptr(),
                                cache.scratch_group_capacity(),
                                cache.scratch_sorted_group_ptr(),
                                cache.scratch_row_index_ptr(),
                                cache.scratch_row_capacity(),
                                cache.scratch_partial_sum_ptr(),
                                cache.scratch_partial_min_ptr(),
                                cache.scratch_partial_max_ptr(),
                                cache.scratch_partial_count_ptr(),
                                cache.scratch_partial_capacity(),
                            ),
                        )
                    };
                    let result = match result {
                        Ok(result) => result,
                        Err(status) => {
                            pgrx::warning!(
                                "pg_accel: resident dense grouped f64 dispatch failed: status={:?} rel_oid={} rows={} group_min={} group_count={} scratch_group_capacity={} scratch_row_capacity={}",
                                status,
                                spec.rel_oid,
                                cache.row_count(),
                                cache.group_min(),
                                cache.group_count(),
                                cache.scratch_group_capacity(),
                                cache.scratch_row_capacity(),
                            );
                            return None;
                        }
                    };

                    let group_count = usize::try_from(cache.group_count()).ok()?;
                    let needs_minmax = matches!(
                        spec.layout,
                        ResidentDenseGroupedF64Layout::GroupMinMaxAvg
                            | ResidentDenseGroupedF64Layout::SingleMinMax
                    );
                    let needs_stats_pair =
                        spec.layout == ResidentDenseGroupedF64Layout::GroupSumAvgStddev;
                    debug_assert_eq!(result.sum_by_group.len(), group_count);
                    if needs_minmax {
                        debug_assert_eq!(result.min_by_group.len(), group_count);
                        debug_assert_eq!(result.max_by_group.len(), group_count);
                    } else {
                        debug_assert!(result.min_by_group.is_empty());
                        debug_assert!(result.max_by_group.is_empty());
                    }
                    if needs_stats_pair {
                        debug_assert_eq!(result.sumsq_by_group.len(), group_count);
                        debug_assert_eq!(result.rhs_sum_by_group.len(), group_count);
                        debug_assert_eq!(result.rhs_count_by_group.len(), group_count);
                    } else {
                        debug_assert!(result.sumsq_by_group.is_empty());
                        debug_assert!(result.rhs_sum_by_group.is_empty());
                        debug_assert!(result.rhs_count_by_group.is_empty());
                    }
                    debug_assert_eq!(result.count_by_group.len(), group_count);
                    Some((
                        dispatched_rows,
                        result.selected_count,
                        result.uncertain_count,
                        ResidentDenseGroupedF64Output {
                            group_output: cache.group_output().clone(),
                            sum_by_group: result.sum_by_group,
                            min_by_group: result.min_by_group,
                            max_by_group: result.max_by_group,
                            sumsq_by_group: result.sumsq_by_group,
                            rhs_sum_by_group: result.rhs_sum_by_group,
                            count_by_group: result.count_by_group,
                            rhs_count_by_group: result.rhs_count_by_group,
                        },
                    ))
                },
            )
            .unwrap_or_else(|| {
                pgrx::warning!(
                    "pg_accel: resident dense groupagg cache missing at execution: rel_oid={}",
                    spec.rel_oid
                );
                pgrx::error!(
                    "pg_accel: resident dense groupagg cache missing at execution; refusing CPU fallback"
                )
            })
            .unwrap_or_else(|| {
                pgrx::error!(
                    "pg_accel: resident dense grouped f64 GPU kernel failed; refusing CPU fallback"
                )
            });

            let (row_count, selected_count, uncertain_count, output) = materialized;
            if uncertain_count != 0 {
                pgrx::warning!(
                    "pg_accel: resident dense grouped f64 kernel returned uncertain rows: rel_oid={} uncertain={}",
                    spec.rel_oid,
                    uncertain_count,
                );
                pgrx::error!(
                    "pg_accel: resident dense grouped f64 kernel returned {} uncertain rows; refusing CPU fallback",
                    uncertain_count,
                );
            }

            self.rows_dispatched = row_count as u64;
            self.batches_executed = 1;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = selected_count as u64;
            self.uncertain_rows = uncertain_count as u64;
            self.dense_f64_output = Some(output);

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let output = self
            .dense_f64_output
            .as_ref()
            .expect("resident dense grouped f64 output materialized before emission");
        if spec.layout.is_single_group() {
            if self.dense_f64_emit_idx > 0 {
                self.result_returned = true;
                return std::ptr::null_mut();
            }
            self.dense_f64_emit_idx = 1;
            let Some(count) = output.count_by_group.first().copied() else {
                pgrx::error!("pg_accel: resident single-group f64 output missing count lane");
            };

            unsafe {
                pg_sys::ExecClearTuple(result_slot);
                match spec.layout {
                    ResidentDenseGroupedF64Layout::SingleSum => {
                        if count == 0 {
                            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(0) = true;
                        } else {
                            let Some(sum) = output.sum_by_group.first().copied() else {
                                pgrx::error!(
                                    "pg_accel: resident single-group f64 output missing sum lane"
                                );
                            };
                            *(*result_slot).tts_values.add(0) = float8_datum(sum);
                            *(*result_slot).tts_isnull.add(0) = false;
                        }
                    }
                    ResidentDenseGroupedF64Layout::SingleMinMax => {
                        if count == 0 {
                            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(0) = true;
                            *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(1) = true;
                        } else {
                            let Some(min) = output.min_by_group.first().copied() else {
                                pgrx::error!(
                                    "pg_accel: resident single-group f64 output missing min lane"
                                );
                            };
                            let Some(max) = output.max_by_group.first().copied() else {
                                pgrx::error!(
                                    "pg_accel: resident single-group f64 output missing max lane"
                                );
                            };
                            *(*result_slot).tts_values.add(0) = float8_datum(min);
                            *(*result_slot).tts_isnull.add(0) = false;
                            *(*result_slot).tts_values.add(1) = float8_datum(max);
                            *(*result_slot).tts_isnull.add(1) = false;
                        }
                    }
                    ResidentDenseGroupedF64Layout::SingleStats => {
                        if count == 0 {
                            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(0) = true;
                            *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(1) = true;
                            *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(0);
                            *(*result_slot).tts_isnull.add(2) = true;
                        } else {
                            let Some(sum) = output.sum_by_group.first().copied() else {
                                pgrx::error!(
                                    "pg_accel: resident single-group f64 stats output missing sum lane"
                                );
                            };
                            let Some(sumsq) = output.sumsq_by_group.first().copied() else {
                                pgrx::error!(
                                    "pg_accel: resident single-group f64 stats output missing sumsq lane"
                                );
                            };
                            let n = f64::from(count);
                            let avg = sum / n;
                            let var_pop = avg.mul_add(-avg, sumsq / n).max(0.0);
                            *(*result_slot).tts_values.add(0) = float8_datum(avg);
                            *(*result_slot).tts_isnull.add(0) = false;
                            if count > 1 {
                                let var_samp = ((sumsq - (sum * sum) / n) / (n - 1.0)).max(0.0);
                                *(*result_slot).tts_values.add(1) = float8_datum(var_samp.sqrt());
                                *(*result_slot).tts_isnull.add(1) = false;
                            } else {
                                *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                                *(*result_slot).tts_isnull.add(1) = true;
                            }
                            *(*result_slot).tts_values.add(2) = float8_datum(var_pop);
                            *(*result_slot).tts_isnull.add(2) = false;
                        }
                    }
                    _ => unreachable!("checked scalar resident dense f64 layout above"),
                }
                pg_sys::ExecStoreVirtualTuple(result_slot);
            }

            return result_slot;
        }

        let mut group_idx = self.dense_f64_emit_idx;
        if spec.filter_mode != ResidentDenseGroupedF64FilterMode::AggregateFilter {
            while group_idx < output.count_by_group.len()
                && if spec.layout == ResidentDenseGroupedF64Layout::GroupSumAvgStddev {
                    output.count_by_group.get(group_idx).copied().unwrap_or(0) == 0
                        && output
                            .rhs_count_by_group
                            .get(group_idx)
                            .copied()
                            .unwrap_or(0)
                            == 0
                } else {
                    output.count_by_group.get(group_idx).copied().unwrap_or(0) == 0
                }
            {
                group_idx += 1;
            }
        }
        if group_idx >= output.count_by_group.len() {
            self.result_returned = true;
            return std::ptr::null_mut();
        }
        self.dense_f64_emit_idx = group_idx + 1;

        let sum = output.sum_by_group[group_idx];
        let count = output.count_by_group[group_idx];

        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            write_resident_group_key(result_slot, &output.group_output, group_idx);
            match spec.layout {
                ResidentDenseGroupedF64Layout::GroupSumCount => {
                    if spec.filter_mode == ResidentDenseGroupedF64FilterMode::AggregateFilter
                        && count == 0
                    {
                        *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(1) = true;
                    } else {
                        *(*result_slot).tts_values.add(1) = float8_datum(sum);
                        *(*result_slot).tts_isnull.add(1) = false;
                    }
                    *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(i64::from(count));
                    *(*result_slot).tts_isnull.add(2) = false;
                }
                ResidentDenseGroupedF64Layout::GroupCountSum => {
                    *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(i64::from(count));
                    *(*result_slot).tts_isnull.add(1) = false;
                    if spec.filter_mode == ResidentDenseGroupedF64FilterMode::AggregateFilter
                        && count == 0
                    {
                        *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(2) = true;
                    } else {
                        *(*result_slot).tts_values.add(2) = float8_datum(sum);
                        *(*result_slot).tts_isnull.add(2) = false;
                    }
                }
                ResidentDenseGroupedF64Layout::GroupSumAvgCount => {
                    if spec.filter_mode == ResidentDenseGroupedF64FilterMode::AggregateFilter
                        && count == 0
                    {
                        *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(1) = true;
                        *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(2) = true;
                    } else {
                        *(*result_slot).tts_values.add(1) = float8_datum(sum);
                        *(*result_slot).tts_isnull.add(1) = false;
                        let avg = sum / f64::from(count);
                        *(*result_slot).tts_values.add(2) = float8_datum(avg);
                        *(*result_slot).tts_isnull.add(2) = false;
                    }
                    *(*result_slot).tts_values.add(3) = pg_sys::Datum::from(i64::from(count));
                    *(*result_slot).tts_isnull.add(3) = false;
                }
                ResidentDenseGroupedF64Layout::GroupMinMaxAvg => {
                    let min = output.min_by_group[group_idx];
                    let max = output.max_by_group[group_idx];
                    if spec.filter_mode == ResidentDenseGroupedF64FilterMode::AggregateFilter
                        && count == 0
                    {
                        *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(1) = true;
                        *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(2) = true;
                        *(*result_slot).tts_values.add(3) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(3) = true;
                    } else {
                        *(*result_slot).tts_values.add(1) = float8_datum(min);
                        *(*result_slot).tts_isnull.add(1) = false;
                        *(*result_slot).tts_values.add(2) = float8_datum(max);
                        *(*result_slot).tts_isnull.add(2) = false;
                        let avg = sum / f64::from(count);
                        *(*result_slot).tts_values.add(3) = float8_datum(avg);
                        *(*result_slot).tts_isnull.add(3) = false;
                    }
                }
                ResidentDenseGroupedF64Layout::GroupSumAvgStddev => {
                    let sumsq = output.sumsq_by_group[group_idx];
                    let rhs_sum = output.rhs_sum_by_group[group_idx];
                    let rhs_count = output.rhs_count_by_group[group_idx];
                    if count == 0 {
                        *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(1) = true;
                    } else {
                        *(*result_slot).tts_values.add(1) = float8_datum(sum);
                        *(*result_slot).tts_isnull.add(1) = false;
                    }
                    if rhs_count == 0 {
                        *(*result_slot).tts_values.add(2) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(2) = true;
                    } else {
                        *(*result_slot).tts_values.add(2) =
                            float8_datum(rhs_sum / f64::from(rhs_count));
                        *(*result_slot).tts_isnull.add(2) = false;
                    }
                    if count > 1 {
                        let n = f64::from(count);
                        let var_samp = ((sumsq - (sum * sum) / n) / (n - 1.0)).max(0.0);
                        *(*result_slot).tts_values.add(3) = float8_datum(var_samp.sqrt());
                        *(*result_slot).tts_isnull.add(3) = false;
                    } else {
                        *(*result_slot).tts_values.add(3) = pg_sys::Datum::from(0);
                        *(*result_slot).tts_isnull.add(3) = true;
                    }
                }
                ResidentDenseGroupedF64Layout::SingleSum
                | ResidentDenseGroupedF64Layout::SingleMinMax
                | ResidentDenseGroupedF64Layout::SingleStats => {
                    unreachable!("scalar resident dense f64 layouts emit before grouped path")
                }
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_resident_star_dim_grouped_f64(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &ResidentStarDimGroupedF64Spec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.dense_f64_output.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized =
                olap_cache::with_resident_star_dim_groupagg_cache(spec.shape, |cache| {
                    let aggregate_mask = spec.logical.aggregate_lane_mask
                        | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT;
                    let sum_count_mask = gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM
                        | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT;
                    let unfiltered_star_join =
                        spec.shape.fact_value_cmp_opcode == opcode::ALWAYS_TRUE
                            && spec.shape.dim_filter_cmp_opcode == opcode::ALWAYS_TRUE;
                    let fused_min_rows = if unfiltered_star_join {
                        RESIDENT_STAR_DIM_GROUPED_F64_FUSED_UNFILTERED_MIN_ROWS
                    } else {
                        RESIDENT_STAR_DIM_GROUPED_F64_FUSED_FILTERED_MIN_ROWS
                    };
                    let use_fused_star_groupagg = aggregate_mask == sum_count_mask
                        && cache.row_count() >= fused_min_rows
                        && cache.group_count() > 0
                        && cache.group_count() <= RESIDENT_STAR_DIM_GROUPED_F64_FUSED_MAX_GROUPS;

                    let (result, batches_executed) = if use_fused_star_groupagg {
                        match gpu::try_expr_template_resident_star_dim_grouped_f64_sum_count_usm(
                            cache.fact_key_col(),
                            cache.value_col(),
                            cache.row_count(),
                            cache.dim_match_ptr(),
                            cache.dim_group_code_ptr(),
                            cache.dim_key_count(),
                            spec.shape.fact_value_cmp_opcode,
                            spec.shape.fact_value_cmp_const,
                            cache.group_count(),
                            cache.scratch_sum_ptr(),
                            cache.scratch_count_ptr(),
                            cache.scratch_partial_sum_ptr(),
                            cache.scratch_partial_count_ptr(),
                            cache.scratch_partial_capacity(),
                        ) {
                            Ok(result) => (result, 1),
                            Err(status) => {
                                pgrx::warning!(
                                    "pg_accel: resident fused star groupagg dispatch failed: status={:?} fact_rel_oid={} dim_rel_oid={} rows={} group_count={}",
                                    status,
                                    spec.shape.fact_rel_oid,
                                    spec.shape.dim_rel_oid,
                                    cache.row_count(),
                                    cache.group_count(),
                                );
                                return None;
                            }
                        }
                    } else {
                        let (group_col, value_col, aggregate_row_count) =
                            if unfiltered_star_join {
                                match gpu::try_expr_template_resident_star_dim_group_project_f64_usm(
                                    cache.fact_key_col(),
                                    cache.value_col(),
                                    cache.row_count(),
                                    cache.dim_match_ptr(),
                                    cache.dim_group_code_ptr(),
                                    cache.dim_key_count(),
                                    spec.shape.fact_value_cmp_opcode,
                                    spec.shape.fact_value_cmp_const,
                                    cache.projected_group_ptr(),
                                    cache.projected_group_capacity(),
                                ) {
                                    Ok(()) => (
                                        cache.projected_group_col(),
                                        cache.value_col(),
                                        cache.row_count(),
                                    ),
                                    Err(status) => {
                                        pgrx::warning!(
                                            "pg_accel: resident star group projection failed: status={:?} fact_rel_oid={} dim_rel_oid={} rows={}",
                                            status,
                                            spec.shape.fact_rel_oid,
                                            spec.shape.dim_rel_oid,
                                            cache.row_count(),
                                        );
                                        return None;
                                    }
                                }
                            } else {
                                let compact = match gpu::try_expr_template_resident_star_dim_group_compact_f64_usm(
                                cache.fact_key_col(),
                                cache.value_col(),
                                cache.row_count(),
                                cache.dim_match_ptr(),
                                cache.dim_group_code_ptr(),
                                cache.dim_key_count(),
                                spec.shape.fact_value_cmp_opcode,
                                spec.shape.fact_value_cmp_const,
                                cache.projected_group_ptr(),
                                cache.projected_value_ptr(),
                                cache.projected_group_capacity(),
                            ) {
                                    Ok(result) => result,
                                    Err(status) => {
                                        pgrx::warning!(
                                            "pg_accel: resident star group compaction failed: status={:?} fact_rel_oid={} dim_rel_oid={} rows={}",
                                            status,
                                            spec.shape.fact_rel_oid,
                                            spec.shape.dim_rel_oid,
                                            cache.row_count(),
                                        );
                                        return None;
                                    }
                                };
                                if compact.uncertain_count != 0 {
                                    pgrx::warning!(
                                        "pg_accel: resident star group compaction returned {} uncertain rows: fact_rel_oid={} dim_rel_oid={} rows={}",
                                        compact.uncertain_count,
                                        spec.shape.fact_rel_oid,
                                        spec.shape.dim_rel_oid,
                                        cache.row_count(),
                                    );
                                    return None;
                                }
                                (
                                    cache.projected_group_col(),
                                    cache.projected_value_col(),
                                    compact.selected_count,
                                )
                            };

                        match gpu::try_expr_template_resident_dense_grouped_f64_usm_masked(
                            group_col,
                            value_col,
                            None,
                            ResidentMeasureOp::Column.to_i32(),
                            aggregate_mask,
                            ResidentDenseGroupedF64FilterMode::None.kernel_filter_mode(),
                            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
                            ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
                            0,
                            [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
                            [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES],
                            None,
                            aggregate_row_count,
                            cache.group_min(),
                            cache.group_count(),
                            cache.scratch_sum_ptr(),
                            cache.scratch_min_ptr(),
                            cache.scratch_max_ptr(),
                            cache.scratch_count_ptr(),
                            cache.scratch_group_start_ptr(),
                            cache.scratch_group_cursor_ptr(),
                            cache.scratch_group_capacity(),
                            cache.scratch_sorted_group_ptr(),
                            cache.scratch_row_index_ptr(),
                            cache.scratch_row_capacity(),
                            cache.scratch_partial_sum_ptr(),
                            cache.scratch_partial_min_ptr(),
                            cache.scratch_partial_max_ptr(),
                            cache.scratch_partial_count_ptr(),
                            cache.scratch_partial_capacity(),
                        ) {
                            Ok(result) => (result, 2),
                            Err(status) => {
                                pgrx::warning!(
                                    "pg_accel: resident star groupagg dispatch failed: status={:?} fact_rel_oid={} dim_rel_oid={} rows={} group_count={}",
                                    status,
                                    spec.shape.fact_rel_oid,
                                    spec.shape.dim_rel_oid,
                                    aggregate_row_count,
                                    cache.group_count(),
                                );
                                return None;
                            }
                        }
                    };

                    let group_count = usize::try_from(cache.group_count()).ok()?;
                    debug_assert_eq!(result.sum_by_group.len(), group_count);
                    debug_assert_eq!(result.count_by_group.len(), group_count);
                    Some((
                        cache.row_count(),
                        result.selected_count,
                        result.uncertain_count,
                        batches_executed,
                        ResidentDenseGroupedF64Output {
                            group_output: cache.group_output().clone(),
                            sum_by_group: result.sum_by_group,
                            min_by_group: Vec::new(),
                            max_by_group: Vec::new(),
                            sumsq_by_group: Vec::new(),
                            rhs_sum_by_group: Vec::new(),
                            count_by_group: result.count_by_group,
                            rhs_count_by_group: Vec::new(),
                        },
                    ))
                })
                .unwrap_or_else(|| {
                    pgrx::warning!(
                        "pg_accel: resident star groupagg cache missing at execution: fact_rel_oid={} dim_rel_oid={}",
                        spec.shape.fact_rel_oid,
                        spec.shape.dim_rel_oid,
                    );
                    pgrx::error!(
                        "pg_accel: resident star groupagg cache missing at execution; refusing CPU fallback"
                    )
                })
                .unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: resident star grouped f64 GPU pipeline failed; refusing CPU fallback"
                    )
                });

            let (row_count, selected_count, uncertain_count, batches_executed, output) =
                materialized;
            if uncertain_count != 0 {
                pgrx::error!(
                    "pg_accel: resident star grouped f64 returned {} uncertain rows; refusing CPU fallback",
                    uncertain_count,
                );
            }

            self.rows_dispatched = row_count as u64;
            self.batches_executed = batches_executed;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = selected_count as u64;
            self.uncertain_rows = uncertain_count as u64;
            self.dense_f64_output = Some(output);

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let output = self
            .dense_f64_output
            .as_ref()
            .expect("resident star grouped f64 output materialized before emission");
        let mut group_idx = self.dense_f64_emit_idx;
        while group_idx < output.count_by_group.len()
            && output.count_by_group.get(group_idx).copied().unwrap_or(0) == 0
        {
            group_idx += 1;
        }
        if group_idx >= output.count_by_group.len() {
            self.result_returned = true;
            return std::ptr::null_mut();
        }
        self.dense_f64_emit_idx = group_idx + 1;

        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            write_resident_group_key(result_slot, &output.group_output, group_idx);
            *(*result_slot).tts_values.add(1) = float8_datum(output.sum_by_group[group_idx]);
            *(*result_slot).tts_isnull.add(1) = false;
            let tupdesc = (*result_slot).tts_tupleDescriptor;
            if !tupdesc.is_null() && (*tupdesc).natts >= 3 {
                *(*result_slot).tts_values.add(2) =
                    pg_sys::Datum::from(i64::from(output.count_by_group[group_idx]));
                *(*result_slot).tts_isnull.add(2) = false;
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    unsafe fn next_resident_h3_grouped_count(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        spec: &ResidentH3GroupedCountSpec,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.h3_count_output.is_none() {
            stats::record_query_accelerated();
            let start = std::time::Instant::now();

            let materialized =
                olap_cache::with_resident_h3_groupagg_cache(spec.rel_oid, spec.kind, |cache| {
                    let result = match spec.kind {
                        ResidentH3GroupedCountKind::LatLngToCell => {
                            let Some(cells) = cache.lat_lng_cell_buffer(spec.resolution) else {
                                pgrx::warning!(
                                    "pg_accel: resident H3 lat/lng cell cache has wrong input \
                                     kind or resolution"
                                );
                                return None;
                            };
                            gpu::hash_count_i64_sorted_device(cells)
                        }
                        ResidentH3GroupedCountKind::CellToParent => {
                            let Some(cells) = cache.cell_buffer() else {
                                pgrx::warning!(
                                    "pg_accel: resident H3 parent cache has wrong input kind"
                                );
                                return None;
                            };
                            gpu::h3_cell_to_parent_count_resident(cells, spec.resolution)
                        }
                    }?;
                    let group_count = result.group_count();
                    Some((cache.row_count(), group_count, result))
                })
                .unwrap_or_else(|| {
                    pgrx::warning!(
                        "pg_accel: resident H3 groupagg cache missing at execution: rel_oid={}",
                        spec.rel_oid
                    );
                    pgrx::error!(
                        "pg_accel: resident H3 groupagg cache missing at execution; refusing CPU fallback"
                    )
                })
                .unwrap_or_else(|| {
                    pgrx::error!(
                        "pg_accel: resident H3 grouped-count GPU kernel failed; refusing CPU fallback"
                    )
                });

            let (row_count, group_count, result) = materialized;
            self.rows_dispatched = row_count as u64;
            self.batches_executed = 1;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.gpu_dispatched = true;
            self.selected_rows = row_count as u64;
            self.uncertain_rows = 0;
            self.h3_count_output = Some(ResidentH3GroupedCountOutput {
                result,
                group_count,
            });

            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        let output = self
            .h3_count_output
            .as_ref()
            .expect("resident H3 grouped-count output materialized before emission");
        if self.h3_count_emit_idx >= output.group_count {
            self.result_returned = true;
            return std::ptr::null_mut();
        }
        let group_idx = self.h3_count_emit_idx;
        self.h3_count_emit_idx += 1;

        let keys_ptr = output.result.group_keys_ptr();
        let Some(counts) = output.result.counts() else {
            pgrx::error!("pg_accel: resident H3 grouped-count result missing count lane");
        };
        let Some(count) = counts.get(group_idx).copied() else {
            pgrx::error!(
                "pg_accel: resident H3 grouped-count result missing count for group {}",
                group_idx
            );
        };
        if keys_ptr.is_null() {
            pgrx::error!("pg_accel: resident H3 grouped-count result missing group keys");
        }

        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let key = *keys_ptr.cast::<i64>().add(group_idx);
            *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(key);
            *(*result_slot).tts_isnull.add(0) = false;
            *(*result_slot).tts_values.add(1) = pg_sys::Datum::from(count);
            *(*result_slot).tts_isnull.add(1) = false;
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }
}

fn float8_datum(value: f64) -> pg_sys::Datum {
    pg_sys::Datum::from(value.to_bits())
}

unsafe fn write_resident_group_key(
    result_slot: *mut pg_sys::TupleTableSlot,
    output: &ResidentGroupKeyOutput,
    group_idx: usize,
) {
    match output {
        ResidentGroupKeyOutput::Int4Dense { min } => {
            let key = *min + i32::try_from(group_idx).unwrap_or(i32::MAX);
            unsafe {
                *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(key);
                *(*result_slot).tts_isnull.add(0) = false;
            }
        }
        ResidentGroupKeyOutput::Int4Dictionary { keys } => {
            let Some(key) = keys.get(group_idx).copied() else {
                pgrx::error!(
                    "pg_accel: resident groupagg int4 dictionary missing key for group {}",
                    group_idx
                );
            };
            unsafe {
                *(*result_slot).tts_values.add(0) = pg_sys::Datum::from(key);
                *(*result_slot).tts_isnull.add(0) = false;
            }
        }
        ResidentGroupKeyOutput::TextDictionary { labels } => {
            let Some(label) = labels.get(group_idx) else {
                pgrx::error!(
                    "pg_accel: resident groupagg text dictionary missing label for group {}",
                    group_idx
                );
            };
            unsafe {
                *(*result_slot).tts_values.add(0) = text_datum_from_str(label);
                *(*result_slot).tts_isnull.add(0) = false;
            }
        }
    }
}

unsafe fn text_datum_from_str(value: &str) -> pg_sys::Datum {
    let total_size = pg_sys::VARHDRSZ + value.len();
    if total_size > i32::MAX as usize {
        pgrx::error!("pg_accel: SSBM text value is too large to materialize");
    }
    // SAFETY: palloc runs in the current PostgreSQL memory context.
    let ptr = unsafe { pg_sys::palloc(total_size).cast::<u8>() };
    // SAFETY: ptr is a fresh varlena allocation with total_size bytes.
    unsafe {
        pgrx::set_varsize_4b(ptr.cast(), total_size as i32);
        let data = pgrx::vardata_any(ptr.cast()).cast::<u8>();
        std::ptr::copy_nonoverlapping(value.as_ptr(), data.cast_mut(), value.len());
    }
    pg_sys::Datum::from(ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_groupagg_logical_spec_maps_dense_abi() {
        let spec = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumAvgCount,
            ResidentMeasureOp::Column,
            false,
            ResidentDenseGroupedF64FilterMode::None,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("direct column no-filter spec");
        assert_eq!(
            spec.group_key_expr,
            ResidentGroupAggGroupKeyExpr::ResidentI32
        );
        assert_eq!(spec.measure_expr, ResidentGroupAggMeasureExpr::DirectColumn);
        assert_eq!(spec.filter_expr, ResidentGroupAggFilterExpr::None);
        assert_eq!(
            spec.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::None,
                value_predicate: ResidentGroupAggValuePredicate::None,
            }
        );
        assert_eq!(
            spec.aggregate_lane_mask,
            ResidentDenseGroupedF64Layout::GroupSumAvgCount.aggregate_mask()
        );

        let where_spec = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumAvgCount,
            ResidentMeasureOp::Column,
            false,
            ResidentDenseGroupedF64FilterMode::Where,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("where-filtered spec");
        assert_eq!(
            where_spec.filter_expr,
            ResidentGroupAggFilterExpr::WhereBool
        );
        assert_eq!(
            where_spec.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::None,
            }
        );

        let aggregate_filter_spec = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Column,
            false,
            ResidentDenseGroupedF64FilterMode::AggregateFilter,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("aggregate FILTER spec");
        assert_eq!(
            aggregate_filter_spec.filter_expr,
            ResidentGroupAggFilterExpr::AggregateFilterBool
        );

        let value_ranges =
            ResidentDenseGroupedF64MeasurePredicate::bool_and_value_ranges(&[(10.0, 20.0)])
                .expect("value ranges");
        let value_where = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Mul,
            true,
            ResidentDenseGroupedF64FilterMode::Where,
            value_ranges,
        )
        .expect("WHERE value-range expression spec");
        assert_eq!(
            value_where.filter_expr,
            ResidentGroupAggFilterExpr::WhereBoolAndValueRanges
        );
        assert_eq!(
            value_where.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::ValueRanges,
            }
        );

        let rhs_ranges =
            ResidentDenseGroupedF64MeasurePredicate::bool_and_rhs_ranges(&[(0.1, 0.2)])
                .expect("rhs ranges");
        let rhs_aggregate_filter = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Mul,
            true,
            ResidentDenseGroupedF64FilterMode::AggregateFilter,
            rhs_ranges,
        )
        .expect("aggregate FILTER rhs-range expression spec");
        assert_eq!(
            rhs_aggregate_filter.filter_expr,
            ResidentGroupAggFilterExpr::AggregateFilterBoolAndRhsRanges
        );
        assert_eq!(
            rhs_aggregate_filter.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::RhsRanges,
            }
        );

        let case_spec = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Mul,
            true,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("CASE bool expression spec");
        assert_eq!(
            case_spec.measure_expr,
            ResidentGroupAggMeasureExpr::BinaryMul
        );
        assert_eq!(case_spec.filter_expr, ResidentGroupAggFilterExpr::CaseBool);
        assert_eq!(
            case_spec.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::None,
            }
        );

        let value_case = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Mul,
            true,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate,
            value_ranges,
        )
        .expect("CASE value-range expression spec");
        assert_eq!(
            value_case.filter_expr,
            ResidentGroupAggFilterExpr::CaseBoolAndValueRanges
        );
        assert_eq!(
            value_case.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::ValueRanges,
            }
        );

        let rhs_case = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Mul,
            true,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate,
            rhs_ranges,
        )
        .expect("CASE rhs-range expression spec");
        assert_eq!(
            rhs_case.filter_expr,
            ResidentGroupAggFilterExpr::CaseBoolAndRhsRanges
        );
        assert_eq!(
            rhs_case.predicate_spec(),
            ResidentGroupAggPredicateSpec {
                guard: ResidentGroupAggPredicateGuard::ResidentBoolColumn,
                value_predicate: ResidentGroupAggValuePredicate::RhsRanges,
            }
        );

        let sub_spec = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumCount,
            ResidentMeasureOp::Sub,
            true,
            ResidentDenseGroupedF64FilterMode::None,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("binary subtraction spec");
        assert_eq!(
            sub_spec.measure_expr,
            ResidentGroupAggMeasureExpr::BinarySub
        );

        let scalar_sum = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::SingleSum,
            ResidentMeasureOp::Column,
            false,
            ResidentDenseGroupedF64FilterMode::None,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("single-group direct column sum spec");
        assert_eq!(
            scalar_sum.group_key_expr,
            ResidentGroupAggGroupKeyExpr::SingleGroup
        );
        assert_eq!(
            scalar_sum.measure_expr,
            ResidentGroupAggMeasureExpr::DirectColumn
        );
        assert_eq!(scalar_sum.filter_expr, ResidentGroupAggFilterExpr::None);

        let scalar_stats = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::SingleStats,
            ResidentMeasureOp::Column,
            false,
            ResidentDenseGroupedF64FilterMode::None,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("single-group direct column stats spec");
        assert_eq!(
            scalar_stats.group_key_expr,
            ResidentGroupAggGroupKeyExpr::SingleGroup
        );
        assert_eq!(
            scalar_stats.aggregate_lane_mask,
            gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM
                | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
                | gpu::RESIDENT_F64_REDUCE_AGG_SUMSQ
        );

        let two_measure_stats = ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
            ResidentDenseGroupedF64Layout::GroupSumAvgStddev,
            ResidentMeasureOp::StatsPair,
            true,
            ResidentDenseGroupedF64FilterMode::None,
            ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
        )
        .expect("grouped two-measure stats spec");
        assert_eq!(
            two_measure_stats.group_key_expr,
            ResidentGroupAggGroupKeyExpr::ResidentI32
        );
        assert_eq!(
            two_measure_stats.measure_expr,
            ResidentGroupAggMeasureExpr::TwoMeasureStats
        );
        assert_eq!(
            two_measure_stats.aggregate_lane_mask,
            gpu::RESIDENT_DENSE_GROUPED_F64_AGG_SUM
                | gpu::RESIDENT_DENSE_GROUPED_F64_AGG_COUNT
                | gpu::RESIDENT_F64_REDUCE_AGG_SUMSQ
        );
    }

    #[test]
    fn resident_groupagg_logical_spec_maps_ssbm_star_schema_abi() {
        let q1 = ResidentGroupAggLogicalSpec::for_ssbm_q1_revenue();
        assert_eq!(q1.group_key_expr, ResidentGroupAggGroupKeyExpr::SingleGroup);
        assert_eq!(
            q1.measure_expr,
            ResidentGroupAggMeasureExpr::SsbmDiscountedRevenue
        );
        assert_eq!(
            q1.filter_expr,
            ResidentGroupAggFilterExpr::SsbmDateFactPredicate
        );
        assert_eq!(
            q1.predicate_spec().guard,
            ResidentGroupAggPredicateGuard::SsbmDateFactPredicate
        );
        assert_eq!(
            q1.aggregate_lane_mask,
            ResidentGroupAggLogicalSpec::SSBM_SUM_AGGREGATE_MASK
        );

        let q2 = ResidentGroupAggLogicalSpec::for_ssbm_q2_grouped_revenue();
        assert_eq!(
            q2.group_key_expr,
            ResidentGroupAggGroupKeyExpr::SsbmYearBrand
        );
        assert_eq!(
            q2.measure_expr,
            ResidentGroupAggMeasureExpr::SsbmRevenueColumn
        );
        assert_eq!(
            q2.filter_expr,
            ResidentGroupAggFilterExpr::SsbmStarJoinMembership
        );
        assert_eq!(
            q2.predicate_spec().guard,
            ResidentGroupAggPredicateGuard::SsbmStarJoinMembership
        );

        let q3 = ResidentGroupAggLogicalSpec::for_ssbm_q3_grouped_revenue();
        assert_eq!(
            q3.group_key_expr,
            ResidentGroupAggGroupKeyExpr::SsbmCustomerSupplierYear
        );
        assert_eq!(
            q3.measure_expr,
            ResidentGroupAggMeasureExpr::SsbmRevenueColumn
        );
        assert_eq!(
            q3.filter_expr,
            ResidentGroupAggFilterExpr::SsbmStarJoinMembership
        );

        let q4 = ResidentGroupAggLogicalSpec::for_ssbm_q4_grouped_profit();
        assert_eq!(
            q4.group_key_expr,
            ResidentGroupAggGroupKeyExpr::SsbmYearGeoPart
        );
        assert_eq!(
            q4.measure_expr,
            ResidentGroupAggMeasureExpr::SsbmProfitRevenueMinusSupplycost
        );
        assert_eq!(
            q4.filter_expr,
            ResidentGroupAggFilterExpr::SsbmStarJoinMembership
        );
    }

    #[test]
    fn resident_groupagg_logical_spec_rejects_invalid_dense_abi() {
        let range_predicate =
            ResidentDenseGroupedF64MeasurePredicate::bool_and_rhs_ranges(&[(0.1, 0.2)])
                .expect("rhs ranges");
        for (measure_op, requires_rhs) in [
            (ResidentMeasureOp::Column, true),
            (ResidentMeasureOp::Mul, false),
            (ResidentMeasureOp::StatsPair, false),
        ] {
            assert!(
                ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
                    ResidentDenseGroupedF64Layout::GroupSumCount,
                    measure_op,
                    requires_rhs,
                    ResidentDenseGroupedF64FilterMode::None,
                    ResidentDenseGroupedF64MeasurePredicate::BOOL_ONLY,
                )
                .is_none(),
                "{measure_op:?} requires_rhs={requires_rhs} should be rejected"
            );
        }

        assert!(
            ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
                ResidentDenseGroupedF64Layout::GroupSumCount,
                ResidentMeasureOp::Mul,
                true,
                ResidentDenseGroupedF64FilterMode::None,
                range_predicate,
            )
            .is_none(),
            "unfiltered groupagg should reject non-BOOL_ONLY predicates"
        );
        assert!(
            ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
                ResidentDenseGroupedF64Layout::GroupSumCount,
                ResidentMeasureOp::Column,
                false,
                ResidentDenseGroupedF64FilterMode::Where,
                range_predicate,
            )
            .is_none(),
            "rhs predicates require a resident rhs column"
        );
    }
}

//! Declarative metadata for every benchmark workload.
//!
//! Workload implementations own SQL and fixture construction. This registry
//! owns cross-cutting harness policy keyed by the exact workload name.

/// Stable workload categories used by CLI filters and report rollups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadCategory {
    GpuReduce,
    GpuHashAgg,
    GpuSort,
    GpuHashJoin,
    GpuJoin,
    GpuSpatial,
    GpuH3,
    GpuExpr,
    GpuWindow,
    /// Star-schema workloads generated from the SSBM fixture.
    StarSchemaSsbm,
    Mixed,
    GpuRaster,
    Regression,
    Fp64Matrix,
}

impl WorkloadCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GpuReduce => "gpu_reduce",
            Self::GpuHashAgg => "gpu_hashagg",
            Self::GpuSort => "gpu_sort",
            Self::GpuHashJoin => "gpu_hashjoin",
            Self::GpuJoin => "gpu_join",
            Self::GpuSpatial => "gpu_spatial",
            Self::GpuH3 => "gpu_h3",
            Self::GpuExpr => "gpu_expr",
            Self::GpuWindow => "gpu_window",
            Self::StarSchemaSsbm => "ssbm",
            Self::Mixed => "mixed",
            Self::GpuRaster => "gpu_raster",
            Self::Regression => "regression",
            Self::Fp64Matrix => "fp64_matrix",
        }
    }
}

/// Physical kernel family represented by a workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelClass {
    Reduce,
    HashAgg,
    Sort,
    HashJoin,
    MergeJoin,
    SetOp,
    RecursiveUnion,
    NestedLoopInequality,
    PointInRing,
    H3LatLng,
    H3CellToParent,
    Expr,
    Window,
    Raster,
    ResidentStarGroupAgg,
    ResidentF64Reduce,
    ResidentF64GroupedStats,
    Unclassified,
}

impl KernelClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reduce => "reduce",
            Self::HashAgg => "hash_agg",
            Self::Sort => "sort",
            Self::HashJoin => "hash_join",
            Self::MergeJoin => "merge_join",
            Self::SetOp => "set_op",
            Self::RecursiveUnion => "recursive_union",
            Self::NestedLoopInequality => "nested_loop_ineq",
            Self::PointInRing => "point_in_ring",
            Self::H3LatLng => "h3_latlng",
            Self::H3CellToParent => "h3_cell_to_parent",
            Self::Expr => "expr",
            Self::Window => "window",
            Self::Raster => "raster",
            Self::ResidentStarGroupAgg => "resident_star_groupagg",
            Self::ResidentF64Reduce => "resident_f64_reduce",
            Self::ResidentF64GroupedStats => "resident_f64_grouped_stats",
            Self::Unclassified => "unclassified",
        }
    }
}

/// One table/column set loaded into the resident cache before measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentPinSpec {
    pub table: &'static str,
    pub columns: &'static [&'static str],
}

/// PostgreSQL extensions a workload fixture or query depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredExtension {
    Postgis,
    H3,
    PostgisRaster,
}

impl RequiredExtension {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgis => "postgis",
            Self::H3 => "h3",
            Self::PostgisRaster => "postgis_raster",
        }
    }
}

/// Release policy for an H3 workload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum H3LaneClass {
    Winning { min_warm_speedup: f64 },
    Parity,
}

/// Whether threshold-matrix evidence is expected from a workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdEvidenceEligibility {
    NotGated,
    GpuWinner,
    NativeDeclineOnly,
}

/// Static eligibility for evidence paths that previously inferred policy from
/// workload-name prefixes or category strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceEligibility {
    pub threshold: ThresholdEvidenceEligibility,
    flags: u8,
}

impl EvidenceEligibility {
    const FUNCTION_KERNEL: u8 = 1 << 0;
    const CACHE_MODE_BOTH: u8 = 1 << 1;
    const FP64_CALIBRATION: u8 = 1 << 2;
    const H3_RESIDENT_ROLLUP: u8 = 1 << 3;

    const NONE: Self = Self {
        threshold: ThresholdEvidenceEligibility::NotGated,
        flags: 0,
    };

    #[must_use]
    pub const fn function_kernel(self) -> bool {
        self.flags & Self::FUNCTION_KERNEL != 0
    }

    #[must_use]
    pub const fn requires_cache_mode_both(self) -> bool {
        self.flags & Self::CACHE_MODE_BOTH != 0
    }

    #[must_use]
    pub const fn fp64_calibration(self) -> bool {
        self.flags & Self::FP64_CALIBRATION != 0
    }

    #[must_use]
    pub const fn h3_resident_rollup(self) -> bool {
        self.flags & Self::H3_RESIDENT_ROLLUP != 0
    }
}

/// Cross-cutting metadata for one exact workload identifier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkloadMetadata {
    pub name: &'static str,
    pub category: WorkloadCategory,
    pub kernel_class: KernelClass,
    pub resident_pins: &'static [ResidentPinSpec],
    pub required_extensions: &'static [RequiredExtension],
    pub h3_lane: Option<H3LaneClass>,
    pub evidence: EvidenceEligibility,
}

const fn workload(
    name: &'static str,
    category: WorkloadCategory,
    kernel_class: KernelClass,
) -> WorkloadMetadata {
    WorkloadMetadata {
        name,
        category,
        kernel_class,
        resident_pins: &[],
        required_extensions: &[],
        h3_lane: None,
        evidence: EvidenceEligibility::NONE,
    }
}

impl WorkloadMetadata {
    const fn pins(mut self, resident_pins: &'static [ResidentPinSpec]) -> Self {
        self.resident_pins = resident_pins;
        self
    }

    const fn extensions(mut self, required_extensions: &'static [RequiredExtension]) -> Self {
        self.required_extensions = required_extensions;
        self
    }

    const fn h3(mut self, h3_lane: H3LaneClass) -> Self {
        self.h3_lane = Some(h3_lane);
        self
    }

    const fn evidence(mut self, evidence: EvidenceEligibility) -> Self {
        self.evidence = evidence;
        self
    }
}

use KernelClass as K;
use WorkloadCategory as C;

const POSTGIS: &[RequiredExtension] = &[RequiredExtension::Postgis];
const H3: &[RequiredExtension] = &[RequiredExtension::H3];
const POSTGIS_RASTER: &[RequiredExtension] = &[RequiredExtension::PostgisRaster];

const WINNER: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::GpuWinner,
    ..EvidenceEligibility::NONE
};
const NATIVE_DECLINE: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::NativeDeclineOnly,
    ..EvidenceEligibility::NONE
};
const H3_WINNER: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::GpuWinner,
    flags: EvidenceEligibility::FUNCTION_KERNEL
        | EvidenceEligibility::CACHE_MODE_BOTH
        | EvidenceEligibility::H3_RESIDENT_ROLLUP,
};
const H3_NATIVE_DECLINE: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::NativeDeclineOnly,
    flags: EvidenceEligibility::FUNCTION_KERNEL | EvidenceEligibility::CACHE_MODE_BOTH,
};
const RASTER_WINNER: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::GpuWinner,
    flags: EvidenceEligibility::FUNCTION_KERNEL | EvidenceEligibility::CACHE_MODE_BOTH,
};
const FP64_CALIBRATION: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::NotGated,
    flags: EvidenceEligibility::FP64_CALIBRATION,
};
const FP64_WINNER: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::GpuWinner,
    flags: EvidenceEligibility::FP64_CALIBRATION,
};
const H3_FP64_NATIVE_DECLINE: EvidenceEligibility = EvidenceEligibility {
    threshold: ThresholdEvidenceEligibility::NativeDeclineOnly,
    flags: EvidenceEligibility::FUNCTION_KERNEL
        | EvidenceEligibility::CACHE_MODE_BOTH
        | EvidenceEligibility::FP64_CALIBRATION,
};

macro_rules! pin {
    ($table:literal, [$($column:literal),+ $(,)?]) => {
        ResidentPinSpec {
            table: $table,
            columns: &[$($column),+],
        }
    };
}

const PINS_HASHAGG_SWEEP: &[ResidentPinSpec] = &[pin!("bench_hagg_sweep", ["grp", "val"])];
const PINS_HASHAGG_MED: &[ResidentPinSpec] = &[pin!("bench_hashagg_med", ["user_id", "val"])];
const PINS_FILTERED_GROUPAGG: &[ResidentPinSpec] =
    &[pin!("bench_employees", ["dept", "salary", "active"])];
const PINS_GROUPED_AGG: &[ResidentPinSpec] = &[pin!("bench_employees_agg", ["dept", "salary"])];
const PINS_GROUPED_AGG_HIGH_CARD: &[ResidentPinSpec] =
    &[pin!("bench_events_agg", ["user_id", "val"])];
const PINS_TIMESERIES: &[ResidentPinSpec] = &[pin!("sensor_data", ["sensor_id", "value"])];
const PINS_DICTIONARY: &[ResidentPinSpec] = &[pin!("bench_dictionary_sales", ["region", "amount"])];
const PINS_EXPRESSION: &[ResidentPinSpec] = &[pin!(
    "bench_expression_sales",
    ["product_id", "price", "discount"]
)];
const PINS_PREDICATE_EXPRESSION: &[ResidentPinSpec] = &[pin!(
    "bench_predicate_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_EXPRESSION: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_RANGE: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_range_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_VALUE: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_value_predicate_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_NULL: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_null_predicate_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_OR: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_or_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_IN: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_in_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_CASE_NOT: &[ResidentPinSpec] = &[pin!(
    "bench_case_when_not_expression_sales",
    ["product_id", "price", "discount", "active"]
)];
const PINS_REDUCE_F64: &[ResidentPinSpec] = &[pin!("bench_fp64_num", ["v_f64"])];
const PINS_HASHAGG_F64: &[ResidentPinSpec] = &[pin!("bench_fp64_num", ["gk", "v_f64", "w_f64"])];
const PINS_H3_PARENT: &[ResidentPinSpec] = &[pin!("bench_h3_parent", ["cell"])];
const PINS_HASH_JOIN: &[ResidentPinSpec] = &[
    pin!("bench_orders", ["customer_id"]),
    pin!("bench_customers", ["customer_id"]),
];
const PINS_HASHJOIN_SWEEP: &[ResidentPinSpec] = &[
    pin!("bench_hj_outer", ["key"]),
    pin!("bench_hj_inner", ["key"]),
];
const PINS_HASHJOIN_LARGE_BUILD: &[ResidentPinSpec] = &[
    pin!("bench_hj_left", ["key"]),
    pin!("bench_hj_right", ["key"]),
];
const PINS_HASHJOIN_FILTER: &[ResidentPinSpec] = &[
    pin!("bench_hjf_fact", ["dim_id", "amount"]),
    pin!("bench_hjf_dim", ["id", "category", "name"]),
];
const PINS_MIXED_JOIN: &[ResidentPinSpec] = &[
    pin!("bench_mixed_facts", ["dim_id", "amount"]),
    pin!("bench_mixed_dims", ["id", "label"]),
];

const SSBM_Q1_FACT: &[&str] = &[
    "lo_orderdate",
    "lo_extendedprice",
    "lo_discount",
    "lo_quantity",
];
const SSBM_Q2_FACT: &[&str] = &["lo_orderdate", "lo_partkey", "lo_suppkey", "lo_revenue"];
const SSBM_Q3_FACT: &[&str] = &["lo_orderdate", "lo_custkey", "lo_suppkey", "lo_revenue"];
const SSBM_Q4_FACT: &[&str] = &[
    "lo_orderdate",
    "lo_custkey",
    "lo_suppkey",
    "lo_partkey",
    "lo_revenue",
    "lo_supplycost",
];
const PINS_SSBM_Q1_1: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q1_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
];
const PINS_SSBM_Q1_2: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q1_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_yearmonthnum"]),
];
const PINS_SSBM_Q1_3: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q1_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_weeknuminyear", "d_year"]),
];
const PINS_SSBM_Q2_1: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q2_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_part", ["p_partkey", "p_category", "p_brand1"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_region"]),
];
const PINS_SSBM_Q2_2_3: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q2_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_part", ["p_partkey", "p_brand1"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_region"]),
];
const PINS_SSBM_Q3_1: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q3_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_nation", "c_region"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_nation", "s_region"]),
];
const PINS_SSBM_Q3_2: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q3_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_city", "c_nation"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_city", "s_nation"]),
];
const PINS_SSBM_Q3_3: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q3_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_city"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_city"]),
];
const PINS_SSBM_Q3_4: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q3_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year", "d_yearmonth"]),
    pin!("ssbm_customer", ["c_custkey", "c_city"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_city"]),
];
const PINS_SSBM_Q4_1: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q4_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_nation", "c_region"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_region"]),
    pin!("ssbm_part", ["p_partkey", "p_mfgr"]),
];
const PINS_SSBM_Q4_2: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q4_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_region"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_nation", "s_region"]),
    pin!("ssbm_part", ["p_partkey", "p_category", "p_mfgr"]),
];
const PINS_SSBM_Q4_3: &[ResidentPinSpec] = &[
    ResidentPinSpec {
        table: "ssbm_lineorder",
        columns: SSBM_Q4_FACT,
    },
    pin!("ssbm_date", ["d_datekey", "d_year"]),
    pin!("ssbm_customer", ["c_custkey", "c_region"]),
    pin!("ssbm_supplier", ["s_suppkey", "s_city", "s_nation"]),
    pin!("ssbm_part", ["p_partkey", "p_brand1", "p_category"]),
];

/// Canonical registry. Exact workload names appear once and only once here.
pub const WORKLOAD_REGISTRY: &[WorkloadMetadata] = &[
    workload("gpu_reduce_sum", C::GpuReduce, K::Reduce),
    workload("gpu_reduce_scaling", C::GpuReduce, K::Reduce),
    workload("reduce_sum_f32", C::GpuReduce, K::Reduce),
    workload("reduce_sum_f64", C::GpuReduce, K::Reduce),
    workload("reduce_sum_i64", C::GpuReduce, K::Reduce),
    workload("reduce_min_f64", C::GpuReduce, K::Reduce),
    workload("reduce_max_f64", C::GpuReduce, K::Reduce),
    workload("reduce_multi", C::GpuReduce, K::Reduce),
    workload("grouped_agg", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_GROUPED_AGG)
        .evidence(WINNER),
    workload("grouped_agg_high_card", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_GROUPED_AGG_HIGH_CARD)
        .evidence(WINNER),
    workload("gpu_hashagg_med_card", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_MED)
        .evidence(WINNER),
    workload("timeseries_sensor_rollup", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_TIMESERIES)
        .evidence(WINNER),
    workload("dictionary_grouped_agg", C::GpuHashAgg, K::Unclassified)
        .pins(PINS_DICTIONARY)
        .evidence(WINNER),
    workload("expression_grouped_agg", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_EXPRESSION)
        .evidence(WINNER),
    workload(
        "predicate_filter_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_PREDICATE_EXPRESSION)
    .evidence(WINNER),
    workload(
        "case_when_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_EXPRESSION)
    .evidence(WINNER),
    workload(
        "case_when_range_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_RANGE)
    .evidence(WINNER),
    workload(
        "case_when_value_predicate_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_VALUE)
    .evidence(WINNER),
    workload(
        "case_when_null_predicate_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_NULL)
    .evidence(WINNER),
    workload(
        "case_when_or_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_OR)
    .evidence(WINNER),
    workload(
        "case_when_in_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_IN)
    .evidence(WINNER),
    workload(
        "case_when_not_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    )
    .pins(PINS_CASE_NOT)
    .evidence(WINNER),
    workload("hashagg_10g", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_SWEEP)
        .evidence(WINNER),
    workload("hashagg_100g", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_SWEEP)
        .evidence(WINNER),
    workload("hashagg_256g", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_SWEEP)
        .evidence(WINNER),
    workload("hashagg_1kg", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_SWEEP)
        .evidence(WINNER),
    workload("hashagg_10kg", C::GpuHashAgg, K::HashAgg)
        .pins(PINS_HASHAGG_SWEEP)
        .evidence(WINNER),
    workload("large_sort", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("gpu_sort_multikey", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("gpu_sort_topk_wide", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("sort_int4", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("sort_int8", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("sort_float4", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("sort_float8", C::GpuSort, K::Sort).evidence(NATIVE_DECLINE),
    workload("hash_join", C::GpuHashJoin, K::HashJoin)
        .pins(PINS_HASH_JOIN)
        .evidence(WINNER),
    workload("gpu_hashjoin_large_build", C::GpuHashJoin, K::HashJoin)
        .pins(PINS_HASHJOIN_LARGE_BUILD),
    workload(
        "gpu_hashjoin_filter",
        C::GpuHashJoin,
        K::ResidentStarGroupAgg,
    )
    .pins(PINS_HASHJOIN_FILTER),
    workload("gpu_nlj_between", C::GpuJoin, K::NestedLoopInequality).evidence(NATIVE_DECLINE),
    workload("hashjoin_100_1m", C::GpuHashJoin, K::HashJoin).pins(PINS_HASHJOIN_SWEEP),
    workload("hashjoin_1k_1m", C::GpuHashJoin, K::HashJoin).pins(PINS_HASHJOIN_SWEEP),
    workload("hashjoin_10k_1m", C::GpuHashJoin, K::HashJoin)
        .pins(PINS_HASHJOIN_SWEEP)
        .evidence(WINNER),
    workload("hashjoin_100k_1m", C::GpuHashJoin, K::HashJoin).pins(PINS_HASHJOIN_SWEEP),
    workload("spatial_filter", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_complex_poly", C::GpuSpatial, K::PointInRing).extensions(POSTGIS),
    workload("spatial_selectivity", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_mega_1kv", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("vsweep_low", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("vsweep_mid", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("vsweep_high", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("vsweep_pathological", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_concentric", C::GpuSpatial, K::PointInRing).extensions(POSTGIS),
    workload("spatial_star_1kv", C::GpuSpatial, K::PointInRing).extensions(POSTGIS),
    workload("spatial_multihole", C::GpuSpatial, K::PointInRing).extensions(POSTGIS),
    workload("spatial_zigzag", C::GpuSpatial, K::PointInRing).extensions(POSTGIS),
    workload("spatial_sel_1pct", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_sel_10pct", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_sel_50pct", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("spatial_sel_90pct", C::GpuSpatial, K::PointInRing)
        .extensions(POSTGIS)
        .evidence(NATIVE_DECLINE),
    workload("h3_bulk", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Winning {
            min_warm_speedup: 1.5,
        })
        .evidence(H3_WINNER),
    workload("h3_cell_to_parent", C::GpuH3, K::H3CellToParent)
        .pins(PINS_H3_PARENT)
        .extensions(H3)
        .h3(H3LaneClass::Winning {
            min_warm_speedup: 1.0,
        })
        .evidence(H3_WINNER),
    workload("h3_grid_distance", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_NATIVE_DECLINE),
    workload("h3_resolution_sweep", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Winning {
            min_warm_speedup: 1.5,
        })
        .evidence(H3_WINNER),
    workload("h3_srf_grid_disk", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_NATIVE_DECLINE),
    workload("h3_latlng_res15", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Winning {
            min_warm_speedup: 1.5,
        })
        .evidence(H3_WINNER),
    workload("h3_dist_near", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_NATIVE_DECLINE),
    workload("h3_dist_far", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_NATIVE_DECLINE),
    workload("h3_parent_deep", C::GpuH3, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_NATIVE_DECLINE),
    workload("gpu_expr_filter", C::GpuExpr, K::Expr),
    workload("gpu_expr_complex", C::GpuExpr, K::Expr),
    workload("gpu_expr_null_heavy", C::GpuExpr, K::Expr),
    workload("expr_2pred", C::GpuExpr, K::Expr),
    workload("expr_3pred", C::GpuExpr, K::Expr),
    workload("expr_4pred", C::GpuExpr, K::Expr),
    workload("expr_arith_chain", C::GpuExpr, K::Expr),
    workload("expr_deep_arith", C::GpuExpr, K::Expr),
    workload("expr_multi_or", C::GpuExpr, K::Expr),
    workload("expr_sqrt_heavy", C::GpuExpr, K::Expr),
    workload("expr_pow_chain", C::GpuExpr, K::Expr),
    workload("expr_math_mixed", C::GpuExpr, K::Expr),
    workload("window_analytics", C::GpuWindow, K::Window),
    workload("window_row_number", C::GpuWindow, K::Window),
    workload("window_rank", C::GpuWindow, K::Window),
    workload("window_dense_rank", C::GpuWindow, K::Window),
    workload("window_running_sum", C::GpuWindow, K::Window),
    workload("window_lag", C::GpuWindow, K::Window),
    workload("window_lead", C::GpuWindow, K::Window),
    workload("window_full_output_decline", C::GpuWindow, K::Window).evidence(NATIVE_DECLINE),
    workload("ssbm_q1_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q1_1)
        .evidence(WINNER),
    workload("ssbm_q1_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q1_2)
        .evidence(WINNER),
    workload("ssbm_q1_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q1_3)
        .evidence(WINNER),
    workload("ssbm_q2_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q2_1)
        .evidence(WINNER),
    workload("ssbm_q2_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q2_2_3)
        .evidence(WINNER),
    workload("ssbm_q2_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q2_2_3)
        .evidence(WINNER),
    workload("ssbm_q3_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q3_1)
        .evidence(WINNER),
    workload("ssbm_q3_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q3_2)
        .evidence(WINNER),
    workload("ssbm_q3_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q3_3)
        .evidence(WINNER),
    workload("ssbm_q3_4", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q3_4)
        .evidence(WINNER),
    workload("ssbm_q4_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q4_1)
        .evidence(WINNER),
    workload("ssbm_q4_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q4_2)
        .evidence(WINNER),
    workload("ssbm_q4_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg)
        .pins(PINS_SSBM_Q4_3)
        .evidence(WINNER),
    workload("parallel_stress", C::GpuReduce, K::Unclassified),
    workload("parallel_stress_grouped", C::GpuHashAgg, K::Unclassified),
    workload("parallel_stress_sort", C::GpuSort, K::Unclassified),
    workload("parallel_stress_window", C::GpuWindow, K::Unclassified),
    workload("spatial_agg", C::Mixed, K::HashAgg).extensions(POSTGIS),
    workload("spatial_sort", C::Mixed, K::Sort).extensions(POSTGIS),
    workload("filtered_grouped_agg", C::Mixed, K::HashAgg)
        .pins(PINS_FILTERED_GROUPAGG)
        .evidence(WINNER),
    workload("mixed_megapoly_agg", C::Mixed, K::PointInRing).extensions(POSTGIS),
    workload("mixed_expr_agg", C::Mixed, K::Expr),
    workload("mixed_join_agg", C::Mixed, K::ResidentStarGroupAgg).pins(PINS_MIXED_JOIN),
    workload("mixed_spatial_sort", C::Mixed, K::Sort).extensions(POSTGIS),
    workload("raster_ndvi", C::GpuRaster, K::Raster)
        .extensions(POSTGIS_RASTER)
        .evidence(RASTER_WINNER),
    workload("raster_slope", C::GpuRaster, K::Raster)
        .extensions(POSTGIS_RASTER)
        .evidence(RASTER_WINNER),
    workload("raster_reclass", C::GpuRaster, K::Raster)
        .extensions(POSTGIS_RASTER)
        .evidence(RASTER_WINNER),
    workload("raster_algebra_deep", C::GpuRaster, K::Raster)
        .extensions(POSTGIS_RASTER)
        .evidence(RASTER_WINNER),
    workload("proximity", C::Regression, K::PointInRing).extensions(POSTGIS),
    workload("index_recheck", C::Regression, K::PointInRing).extensions(POSTGIS),
    workload("spatial_join", C::Regression, K::PointInRing).extensions(POSTGIS),
    workload("spatial_contains", C::Regression, K::PointInRing).extensions(POSTGIS),
    workload("spatial_multi_pred", C::Regression, K::PointInRing).extensions(POSTGIS),
    workload("oltp_point_lookup", C::Regression, K::PointInRing),
    workload(
        "bitmap_heap_gpuexpr_decline",
        C::Regression,
        K::Unclassified,
    )
    .evidence(NATIVE_DECLINE),
    workload("mergejoin_decline", C::Regression, K::MergeJoin).evidence(NATIVE_DECLINE),
    workload("numeric_agg_decline", C::Regression, K::Unclassified).evidence(NATIVE_DECLINE),
    workload(
        "parallel_hashjoin_rebuild_decline",
        C::Regression,
        K::Unclassified,
    )
    .evidence(NATIVE_DECLINE),
    workload("recursive_union_decline", C::Regression, K::RecursiveUnion).evidence(NATIVE_DECLINE),
    workload("setop_intersect_decline", C::Regression, K::SetOp).evidence(NATIVE_DECLINE),
    workload("small_table_scan", C::Regression, K::Unclassified),
    workload("topk_wide", C::Regression, K::Sort).evidence(NATIVE_DECLINE),
    workload("reduce_f64_sum", C::Fp64Matrix, K::ResidentF64Reduce)
        .pins(PINS_REDUCE_F64)
        .evidence(FP64_WINNER),
    workload("reduce_f64_minmax", C::Fp64Matrix, K::ResidentF64Reduce)
        .pins(PINS_REDUCE_F64)
        .evidence(FP64_WINNER),
    workload("reduce_f64_stats", C::Fp64Matrix, K::ResidentF64Reduce)
        .pins(PINS_REDUCE_F64)
        .evidence(FP64_WINNER),
    workload("sort_f64_keys", C::Fp64Matrix, K::Unclassified).evidence(FP64_CALIBRATION),
    workload("hashagg_f64_keys", C::Fp64Matrix, K::HashAgg).evidence(FP64_CALIBRATION),
    workload(
        "hashagg_f64_aggs",
        C::Fp64Matrix,
        K::ResidentF64GroupedStats,
    )
    .pins(PINS_HASHAGG_F64)
    .evidence(FP64_WINNER),
    workload("spatial_fp64_recheck", C::Fp64Matrix, K::Unclassified)
        .extensions(POSTGIS)
        .evidence(FP64_CALIBRATION),
    workload("h3_fp64_ops", C::Fp64Matrix, K::H3LatLng)
        .extensions(H3)
        .h3(H3LaneClass::Parity)
        .evidence(H3_FP64_NATIVE_DECLINE),
];

/// Look up metadata by the exact, case-sensitive workload identifier.
#[must_use]
pub fn workload_metadata(name: &str) -> Option<&'static WorkloadMetadata> {
    WORKLOAD_REGISTRY.iter().find(|entry| entry.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn registry_names_are_unique() {
        let names = WORKLOAD_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), WORKLOAD_REGISTRY.len());
    }

    #[test]
    fn registry_covers_runtime_workloads_without_drift() {
        let workloads = super::super::all_workloads();
        let runtime_names = workloads
            .iter()
            .map(|workload| workload.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            runtime_names.len(),
            workloads.len(),
            "all_workloads() contains duplicate exact names"
        );
        let registry_names = WORKLOAD_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(runtime_names, registry_names);
        assert_eq!(workloads.len(), WORKLOAD_REGISTRY.len());
        for workload in workloads {
            let metadata = workload_metadata(workload.name()).unwrap_or_else(|| {
                panic!("registered workload `{}` lacks metadata", workload.name())
            });
            assert_eq!(metadata.category.as_str(), workload.category());
            assert_eq!(
                metadata.kernel_class.as_str(),
                crate::report::classify_kernel(workload.name())
            );
        }
    }

    #[test]
    fn only_ssbm_workloads_use_the_star_schema_category() {
        for entry in WORKLOAD_REGISTRY {
            assert_eq!(
                entry.category == WorkloadCategory::StarSchemaSsbm,
                entry.name.starts_with("ssbm_")
            );
        }
    }

    #[test]
    fn registry_policy_collections_are_nonempty_and_unique() {
        for entry in WORKLOAD_REGISTRY {
            let mut tables = BTreeSet::new();
            for pin in entry.resident_pins {
                assert!(
                    !pin.table.is_empty(),
                    "{} has an empty pin table",
                    entry.name
                );
                assert!(!pin.columns.is_empty(), "{} has an empty pin", entry.name);
                assert!(
                    tables.insert(pin.table),
                    "{} pins table `{}` more than once",
                    entry.name,
                    pin.table
                );
                let columns = pin.columns.iter().copied().collect::<BTreeSet<_>>();
                assert_eq!(
                    columns.len(),
                    pin.columns.len(),
                    "{} has duplicate columns for `{}`",
                    entry.name,
                    pin.table
                );
            }

            let extensions = entry
                .required_extensions
                .iter()
                .map(|extension| extension.as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(extensions.len(), entry.required_extensions.len());
        }
    }

    #[test]
    fn registry_typed_policy_is_complete_for_special_families() {
        for entry in WORKLOAD_REGISTRY {
            let is_h3 = matches!(entry.kernel_class, K::H3LatLng | K::H3CellToParent);
            assert_eq!(is_h3, entry.h3_lane.is_some(), "{} H3 policy", entry.name);
            if is_h3 {
                assert!(
                    entry.evidence.function_kernel(),
                    "{} function evidence",
                    entry.name
                );
                assert!(
                    entry.evidence.requires_cache_mode_both(),
                    "{} cache evidence",
                    entry.name
                );
                assert!(entry.required_extensions.contains(&RequiredExtension::H3));
            }

            assert_eq!(
                entry.category == C::Fp64Matrix,
                entry.evidence.fp64_calibration(),
                "{} fp64 calibration eligibility",
                entry.name
            );
            if entry.category == C::GpuRaster {
                assert!(entry.evidence.function_kernel());
                assert!(entry.evidence.requires_cache_mode_both());
                assert!(
                    entry
                        .required_extensions
                        .contains(&RequiredExtension::PostgisRaster)
                );
            }
        }
    }

    #[test]
    fn lookup_is_exact() {
        assert!(workload_metadata("h3_bulk").is_some());
        assert!(workload_metadata("H3_BULK").is_none());
    }
}

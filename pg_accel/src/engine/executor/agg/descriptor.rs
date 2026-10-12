//! Runtime capability checks and frozen-ABI binding for neutral aggregates.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::c_void;
use std::fmt;
use std::time::{Duration, Instant};

#[cfg(feature = "pg_test")]
use std::sync::atomic::{AtomicUsize, Ordering};

use pgrx::pg_sys;

use super::artifact::{
    ArtifactKeyInput, DescriptorAggArtifact, H3ParentArtifact, artifact_column_refs,
    descriptor_program_clone_bytes, estimate_descriptor_artifact_bytes_upper_bound,
    h3_count_filter, h3_parent_artifact_bytes, hash_where_filter, learned_hash_group_bound,
    prepare_agg_artifact,
};
use super::output::{DescriptorAggOutput, validate_compact_integer_key_buffers};
use super::spatial::{SpatialAggArtifact, SpatialTransformPlan, SpatialWorkspace};
use crate::engine::cost::{ExecutorLimits, GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS, Rows};
use crate::engine::executor::bounded::{
    BoundedDispatchError, InterruptibleAttempt, InterruptibleDispatchError,
    bounded_dispatch_call_count, capture_postgres_interrupt, cleanup_before_rethrow,
    dispatch_warning_threshold_exceeded, postgres_interrupt_pending, run_bounded_dispatch,
    run_interruptible_dispatch,
};
use crate::engine::ffi::syscache::{PostgisCatalogIdentity, resolve_postgis_catalog};
use crate::engine::gucs;
use crate::engine::residency::{
    ArtifactEnsureOutcome, ArtifactProbeOutcome, DerivedArtifact, DerivedArtifactIdentity,
    EphemeralDerivedArtifact, PreparedDerived, ResidentByteAccounting, ResidentColumnRef,
    ResidentColumnView, ResidentLoadError, ResidentRelationEvidence, ResolvedDerivedInputs,
    SelectedRelation, build_ephemeral_derived_artifact_with_report,
    build_ephemeral_host_device_derived_artifact_with_report,
    build_ephemeral_staged_device_transform_artifact_with_report,
    ensure_device_derived_artifact_with_report, ensure_host_device_derived_artifact_with_report,
    ensure_selected_relations, ensure_staged_device_transform_artifact_with_report,
    probe_derived_artifact, resident_live_bytes, with_derived_artifact_inputs,
    with_ephemeral_artifact_inputs,
};
use crate::engine::spec::abi;
use crate::engine::spec::scalar::{ScalarProgram, ScalarType, ValidatedScalarWire};
use crate::engine::spec::{
    AggOutputProjection, AggOutputSource, AggQuerySpec, AggregateKind, AggregateSource,
    BinaryMeasureOp, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity,
    MaskKind, MeasureExpr, ScalarRange, ScalarValue, SpatialOperand, SpatialPredicateKind,
    SpatialValueKind, SpatialValueMetadata,
};
use crate::engine::stats;
use crate::gpu::{
    GpuError, GpuErrorDomain, GpuOperation, GpuStatusDetail, GroupedAggKernelMode,
    GroupedAggOutcome, GroupedAggOutputStorage, GroupedAggSession, GroupedAggWorkspace,
    PgaccelExprUsmCol, PgaccelVal, PgaccelValTag, ResolvedGroupedAggPlan,
    execute_grouped_agg_one_shot_with_workspace_and_mode,
};

const BOOLOID: u32 = 16;
const INT2OID: u32 = 21;
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const TEXTOID: u32 = 25;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;
const BPCHAROID: u32 = 1042;
const VARCHAROID: u32 = 1043;
const DATEOID: u32 = 1082;
const TIMESTAMPOID: u32 = 1114;
const TIMESTAMPTZOID: u32 = 1184;
const NUMERICOID: u32 = 1700;
const DESCRIPTOR_SPECIALIZATION_CACHE_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescriptorSpecializationFamily {
    HashCount,
    HashInt4SumCount,
    HashInt4Extrema,
    DenseCount,
    DenseBoolCount,
    DenseInt2Count,
    DenseInt8Count,
    DenseDateCount,
    DenseTimestampCount,
    DenseFloat4Count,
    DenseFloat8Count,
    DenseInt2SumAvg,
    DenseInt4SumAvg,
    DenseMultiIntegerSumAvg,
    DenseFilteredIntegerSumAvg,
    DenseIntegerExpressions,
    DenseScalarPrograms,
    DenseNumericSumAvg,
    DenseIntegerWithCounts,
    DenseIntegerColumn,
    DenseIntegerColumnMeasureRange,
    DenseIntegerMultiply,
    DenseIntegerMultiplyRange,
    SerialGeneric,
}

/// One build-generated kernel class selected for an exact semantic descriptor.
///
/// The family fixes grouping and measure evaluation. The two booleans select
/// precompiled membership and SQL-mask variants, so released parallel kernels
/// do not rediscover those shape branches for every input row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DescriptorSpecialization {
    family: DescriptorSpecializationFamily,
    membership: bool,
    sql_mask: bool,
}

impl DescriptorSpecialization {
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match (self.family, self.membership, self.sql_mask) {
            (DescriptorSpecializationFamily::DenseIntegerWithCounts, false, false) => {
                "dense_integer_with_counts_plain"
            }
            (DescriptorSpecializationFamily::DenseIntegerWithCounts, _, _) => {
                "invalid_dense_integer_with_counts_specialization"
            }
            (DescriptorSpecializationFamily::HashCount, false, false) => "hash_count",
            (DescriptorSpecializationFamily::HashInt4SumCount, false, false) => {
                "hash_int4_sum_count"
            }
            (DescriptorSpecializationFamily::HashInt4Extrema, false, false) => "hash_int4_extrema",
            (DescriptorSpecializationFamily::DenseCount, false, false) => "dense_count_plain",
            (DescriptorSpecializationFamily::DenseCount, false, true) => "dense_count_sql_mask",
            (DescriptorSpecializationFamily::DenseCount, true, false) => "dense_count_membership",
            (DescriptorSpecializationFamily::DenseCount, true, true) => {
                "dense_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseBoolCount, false, false) => {
                "dense_bool_count_plain"
            }
            (DescriptorSpecializationFamily::DenseBoolCount, false, true) => {
                "dense_bool_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseBoolCount, true, false) => {
                "dense_bool_count_membership"
            }
            (DescriptorSpecializationFamily::DenseBoolCount, true, true) => {
                "dense_bool_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt2Count, false, false) => {
                "dense_int2_count_plain"
            }
            (DescriptorSpecializationFamily::DenseInt2Count, false, true) => {
                "dense_int2_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt2Count, true, false) => {
                "dense_int2_count_membership"
            }
            (DescriptorSpecializationFamily::DenseInt2Count, true, true) => {
                "dense_int2_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt8Count, false, false) => {
                "dense_int8_count_plain"
            }
            (DescriptorSpecializationFamily::DenseInt8Count, false, true) => {
                "dense_int8_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt8Count, true, false) => {
                "dense_int8_count_membership"
            }
            (DescriptorSpecializationFamily::DenseInt8Count, true, true) => {
                "dense_int8_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseDateCount, false, false) => {
                "dense_date_count_plain"
            }
            (DescriptorSpecializationFamily::DenseDateCount, false, true) => {
                "dense_date_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseDateCount, true, false) => {
                "dense_date_count_membership"
            }
            (DescriptorSpecializationFamily::DenseDateCount, true, true) => {
                "dense_date_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseTimestampCount, false, false) => {
                "dense_timestamp_count_plain"
            }
            (DescriptorSpecializationFamily::DenseTimestampCount, false, true) => {
                "dense_timestamp_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseTimestampCount, true, false) => {
                "dense_timestamp_count_membership"
            }
            (DescriptorSpecializationFamily::DenseTimestampCount, true, true) => {
                "dense_timestamp_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseFloat4Count, false, false) => {
                "dense_float4_count_plain"
            }
            (DescriptorSpecializationFamily::DenseFloat4Count, false, true) => {
                "dense_float4_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseFloat4Count, true, false) => {
                "dense_float4_count_membership"
            }
            (DescriptorSpecializationFamily::DenseFloat4Count, true, true) => {
                "dense_float4_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseFloat8Count, false, false) => {
                "dense_float8_count_plain"
            }
            (DescriptorSpecializationFamily::DenseFloat8Count, false, true) => {
                "dense_float8_count_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseFloat8Count, true, false) => {
                "dense_float8_count_membership"
            }
            (DescriptorSpecializationFamily::DenseFloat8Count, true, true) => {
                "dense_float8_count_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt2SumAvg, false, false) => {
                "dense_int2_sum_avg_plain"
            }
            (DescriptorSpecializationFamily::DenseInt2SumAvg, false, true) => {
                "dense_int2_sum_avg_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt2SumAvg, true, false) => {
                "dense_int2_sum_avg_membership"
            }
            (DescriptorSpecializationFamily::DenseInt2SumAvg, true, true) => {
                "dense_int2_sum_avg_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt4SumAvg, false, false) => {
                "dense_int4_sum_avg_plain"
            }
            (DescriptorSpecializationFamily::DenseInt4SumAvg, false, true) => {
                "dense_int4_sum_avg_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseInt4SumAvg, true, false) => {
                "dense_int4_sum_avg_membership"
            }
            (DescriptorSpecializationFamily::DenseInt4SumAvg, true, true) => {
                "dense_int4_sum_avg_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumn, false, false) => {
                "dense_integer_column_plain"
            }
            (DescriptorSpecializationFamily::DenseIntegerExpressions, false, false) => {
                "dense_integer_expressions_plain"
            }
            (DescriptorSpecializationFamily::DenseScalarPrograms, false, false) => {
                "dense_scalar_programs_plain"
            }
            (DescriptorSpecializationFamily::DenseScalarPrograms, false, true) => {
                "dense_scalar_programs_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseScalarPrograms, true, _) => {
                "invalid_dense_scalar_programs_specialization"
            }
            (DescriptorSpecializationFamily::DenseIntegerExpressions, false, true) => {
                "dense_integer_expressions_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerExpressions, true, _) => {
                "invalid_dense_integer_expressions_specialization"
            }
            (DescriptorSpecializationFamily::DenseMultiIntegerSumAvg, false, false) => {
                "dense_multi_integer_sum_avg_plain"
            }
            (DescriptorSpecializationFamily::DenseMultiIntegerSumAvg, _, _) => {
                "invalid_dense_multi_integer_specialization"
            }
            (DescriptorSpecializationFamily::DenseFilteredIntegerSumAvg, false, false) => {
                "dense_filtered_integer_sum_avg_plain"
            }
            (DescriptorSpecializationFamily::DenseFilteredIntegerSumAvg, _, _) => {
                "invalid_dense_filtered_integer_specialization"
            }
            (DescriptorSpecializationFamily::DenseNumericSumAvg, false, false) => {
                "dense_numeric_sum_avg_plain"
            }
            (DescriptorSpecializationFamily::DenseNumericSumAvg, false, true) => {
                "dense_numeric_sum_avg_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseNumericSumAvg, _, _) => {
                "invalid_dense_numeric_specialization"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumn, false, true) => {
                "dense_integer_column_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumn, true, false) => {
                "dense_integer_column_membership"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumn, true, true) => {
                "dense_integer_column_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumnMeasureRange, false, false) => {
                "dense_integer_column_measure_range"
            }
            (DescriptorSpecializationFamily::DenseIntegerMultiply, false, false) => {
                "dense_integer_multiply_plain"
            }
            (DescriptorSpecializationFamily::DenseIntegerMultiply, false, true) => {
                "dense_integer_multiply_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerMultiply, true, false) => {
                "dense_integer_multiply_membership"
            }
            (DescriptorSpecializationFamily::DenseIntegerMultiply, true, true) => {
                "dense_integer_multiply_membership_sql_mask"
            }
            (DescriptorSpecializationFamily::DenseIntegerMultiplyRange, false, false) => {
                "dense_integer_multiply_range"
            }
            (DescriptorSpecializationFamily::SerialGeneric, _, _) => "serial_generic",
            // Hash descriptors are admitted only without a separate dimension
            // or fact-mask stage. Keep impossible combinations fail-closed.
            (
                DescriptorSpecializationFamily::HashCount
                | DescriptorSpecializationFamily::HashInt4SumCount
                | DescriptorSpecializationFamily::HashInt4Extrema,
                _,
                _,
            ) => "invalid_hash_specialization",
            (DescriptorSpecializationFamily::DenseIntegerMultiplyRange, _, _) => {
                "invalid_dense_integer_range_specialization"
            }
            (DescriptorSpecializationFamily::DenseIntegerColumnMeasureRange, _, _) => {
                "invalid_dense_integer_measure_range_specialization"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DescriptorSpecializationCacheOutcome {
    Hit,
    Miss,
}

impl DescriptorSpecializationCacheOutcome {
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

#[derive(Debug, Clone)]
struct CachedDescriptorSpecialization {
    identity: DerivedArtifactIdentity,
    specialization: DescriptorSpecialization,
}

thread_local! {
    static DESCRIPTOR_SPECIALIZATION_CACHE: RefCell<VecDeque<CachedDescriptorSpecialization>> =
        RefCell::new(VecDeque::with_capacity(DESCRIPTOR_SPECIALIZATION_CACHE_CAPACITY));
}

// Atomic SUM one-shot execution only needs the row cap. Ordered integer
// statistics still use the bounded native partial layout mirrored here.
const DENSE_INTEGER_CHUNK_ROWS: usize = 1_024;
const DENSE_INTEGER_PARTIAL_BYTES: usize = 56;
const DENSE_INTEGER_MAX_PARTIAL_BYTES: usize = 32 * 1_024 * 1_024;

fn unique_dimension_inputs(spec: &AggQuerySpec) -> bool {
    spec.star_dims
        .iter()
        .all(|dimension| dimension.multiplicity == JoinMultiplicity::Unique)
}

pub(super) fn weighted_global_count_inputs(spec: &AggQuerySpec) -> bool {
    let [dimension] = spec.star_dims.as_slice() else {
        return false;
    };
    dimension.multiplicity == JoinMultiplicity::Counted
        && dimension.fact_key.type_oid == INT4OID
        && dimension.dim_key.type_oid == INT4OID
        && dimension.collation_oid == 0
        && dimension.filter == FilterSpec::None
        && spec.group_keys.is_empty()
        && spec.fact_filter == FilterSpec::None
        && spec.having.is_none()
}

fn parallel_dense_fact_filter(filter: &FilterSpec) -> bool {
    matches!(
        filter,
        FilterSpec::None
            | FilterSpec::Mask {
                kind: MaskKind::Sql,
                ..
            }
            | FilterSpec::Spatial { .. }
    )
}

fn canonical_count_measure(measure: &crate::engine::spec::MeasureSpec) -> bool {
    measure.expression == MeasureExpr::CountStar
        && measure.filter == FilterSpec::None
        && measure.outputs.as_slice()
            == [crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }]
}

fn canonical_dense_typed_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
    measure_type_oid: u32,
) -> bool {
    let [group_key] = spec.group_keys.as_slice() else {
        return false;
    };
    let GroupKeySource::FactColumn(group_column) = &group_key.source else {
        return false;
    };
    let MeasureExpr::Column(measure_column) = &measure.expression else {
        return false;
    };
    group_key.type_oid == BOOLOID
        && group_column.type_oid == BOOLOID
        && measure_column.type_oid == measure_type_oid
        && group_column.relation_oid == spec.fact_rel
        && measure_column.relation_oid == spec.fact_rel
        && (group_column.relation_oid, group_column.attno)
            != (measure_column.relation_oid, measure_column.attno)
        && spec.star_dims.is_empty()
        && spec.fact_filter == FilterSpec::None
        && spec.having.is_none()
        && measure.filter == FilterSpec::None
        && measure.outputs.as_slice()
            == [crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }]
}

fn canonical_dense_bool_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, BOOLOID)
}

fn canonical_dense_int2_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, INT2OID)
}

fn canonical_dense_int8_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, INT8OID)
}

fn canonical_dense_date_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, DATEOID)
}

fn canonical_dense_timestamp_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, TIMESTAMPOID)
        || canonical_dense_typed_count_measure(spec, measure, TIMESTAMPTZOID)
}

fn canonical_dense_float4_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, FLOAT4OID)
}

fn canonical_dense_float8_count_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    canonical_dense_typed_count_measure(spec, measure, FLOAT8OID)
}

fn canonical_dense_integer_sum_avg_measure(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
    measure_type_oid: u32,
) -> bool {
    measure.filter == FilterSpec::None
        && canonical_dense_integer_sum_avg_value(spec, measure, measure_type_oid)
}

fn canonical_dense_integer_sum_avg_value(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
    measure_type_oid: u32,
) -> bool {
    if !matches!(measure_type_oid, INT2OID | INT4OID) {
        return false;
    }
    let measure_column = match measure.expression {
        MeasureExpr::Column(column) => column,
        MeasureExpr::Binary { lhs, rhs, .. }
            if measure_type_oid == INT4OID
                && lhs.type_oid == INT4OID
                && rhs.type_oid == INT4OID
                && lhs.relation_oid == spec.fact_rel
                && rhs.relation_oid == spec.fact_rel
                && (measure.filter == FilterSpec::None
                    || measure.filter.range_parts().is_some_and(|(input, _, _)| {
                        filter_measure_binding(spec, input).is_ok()
                    })) =>
        {
            lhs
        }
        _ => match measure.filtered_count_input() {
            Some(column) => column,
            None => return false,
        },
    };
    let outputs = measure.outputs.as_slice();
    spec.group_keys.len() <= abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        && spec.group_keys.iter().all(|key| {
            matches!(&key.source, GroupKeySource::FactColumn(column)
                if (column.relation_oid, column.type_oid) == (spec.fact_rel, key.type_oid)
                    && is_supported_group_type(key.type_oid)
                    && is_text_family(key.type_oid) == (key.collation_oid != 0))
        })
        && measure_column.type_oid == measure_type_oid
        && measure_column.relation_oid == spec.fact_rel
        && spec.star_dims.is_empty()
        && canonical_dense_fact_filter(spec)
        && spec.having.is_none()
        && !outputs.is_empty()
        && outputs.iter().all(|output| {
            output.source == AggregateSource::Value
                && matches!(
                    output.kind,
                    AggregateKind::Sum
                        | AggregateKind::Avg
                        | AggregateKind::Count
                        | AggregateKind::Min
                        | AggregateKind::Max
                )
        })
}

fn canonical_dense_filtered_integer_sum_avg(spec: &AggQuerySpec) -> bool {
    if spec.measures.is_empty()
        || spec.measures.len() > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || spec
            .measures
            .iter()
            .all(|measure| measure.filter == FilterSpec::None)
    {
        return false;
    }
    let values = spec.measures.len()
        - usize::from(spec.measures.last().is_some_and(canonical_count_measure));
    values > 0
        && spec.measures[..values].iter().all(|measure| {
            (canonical_dense_integer_sum_avg_value(spec, measure, INT2OID)
                || canonical_dense_integer_sum_avg_value(spec, measure, INT4OID))
                && canonical_numeric_measure_filter(spec, &measure.filter)
        })
}

fn canonical_dense_multi_integer_sum_avg(spec: &AggQuerySpec) -> bool {
    if !(2..=abi::PGACCEL_GROUPED_AGG_MAX_MEASURES).contains(&spec.measures.len()) {
        return false;
    }
    let value_count = spec.measures.len()
        - usize::from(spec.measures.last().is_some_and(canonical_count_measure));
    value_count >= 2
        && spec.measures[..value_count].iter().all(|measure| {
            canonical_dense_integer_sum_avg_measure(spec, measure, INT2OID)
                || canonical_dense_integer_sum_avg_measure(spec, measure, INT4OID)
        })
}

fn canonical_dense_integer_rollup(spec: &AggQuerySpec) -> bool {
    let values = spec.measures.len()
        - usize::from(spec.measures.last().is_some_and(canonical_count_measure));
    values > 0
        && spec.measures.len() <= abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        && (canonical_dense_filtered_integer_sum_avg(spec)
            || spec.measures[..values].iter().all(|measure| {
                canonical_dense_integer_sum_avg_measure(spec, measure, INT2OID)
                    || canonical_dense_integer_sum_avg_measure(spec, measure, INT4OID)
            }))
}

fn has_scalar_measure(spec: &AggQuerySpec) -> bool {
    spec.measures
        .iter()
        .any(|measure| matches!(measure.expression, MeasureExpr::Bytecode { .. }))
}

/// Typed logical counterpart of the native fused scalar dense family. Spatial
/// predicates still require the separate geometry/preflight admission contract.
fn canonical_dense_scalar_programs(spec: &AggQuerySpec) -> bool {
    if !has_scalar_measure(spec)
        || spec.measures.len() > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || !spec.star_dims.is_empty()
        || spec.having.is_some()
        || !canonical_dense_fact_filter(spec)
        || spec.group_keys.len() > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        || !spec.group_keys.iter().all(|key| {
            matches!(key.source, GroupKeySource::FactColumn(column)
                if (column.relation_oid, column.type_oid) == (spec.fact_rel, key.type_oid)
                    && is_supported_group_type(key.type_oid)
                    && is_text_family(key.type_oid) == (key.collation_oid != 0))
        })
    {
        return false;
    }
    let values = spec.measures.len()
        - usize::from(spec.measures.last().is_some_and(canonical_count_measure));
    spec.measures[..values].iter().all(|measure| {
        if matches!(measure.expression, MeasureExpr::Bytecode { .. }) {
            scalar_measure_program(&measure.expression, spec.fact_rel).is_ok()
                && !measure.outputs.is_empty()
                && validate_measure_outputs(&measure.expression, &measure.outputs).is_ok()
                && canonical_numeric_measure_filter(spec, &measure.filter)
        } else {
            (canonical_dense_integer_sum_avg_value(spec, measure, INT2OID)
                || canonical_dense_integer_sum_avg_value(spec, measure, INT4OID))
                && canonical_numeric_measure_filter(spec, &measure.filter)
        }
    })
}

fn canonical_numeric_measure_filter(spec: &AggQuerySpec, filter: &FilterSpec) -> bool {
    match filter {
        FilterSpec::None => true,
        FilterSpec::Ranges { input, ranges } | FilterSpec::NullOrRanges { input, ranges } => {
            input.relation_oid == spec.fact_rel
                && input.type_oid == INT4OID
                && (matches!(filter, FilterSpec::NullOrRanges { .. }) || !ranges.is_empty())
                && ranges.len() <= abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES
                && ranges.iter().all(|range| {
                    matches!((range.lo, range.hi),
                    (ScalarValue::I32(lo), ScalarValue::I32(hi)) if lo <= hi)
                })
        }
        _ => false,
    }
}

/// Exact dense consumers share range or SQL-mask WHERE semantics. A range
/// input must already have a value binding (possibly supplied by the planner).
fn canonical_dense_fact_filter(spec: &AggQuerySpec) -> bool {
    match spec.fact_filter.scalar() {
        FilterSpec::None => true,
        FilterSpec::Bytecode { .. } => scalar_where_measure_binding(spec).is_some(),
        FilterSpec::Ranges { input, .. } | FilterSpec::NullOrRanges { input, .. } => {
            canonical_numeric_measure_filter(spec, spec.fact_filter.scalar())
                && spec
                    .measures
                    .iter()
                    .any(|measure| match measure.expression {
                        MeasureExpr::Column(column) => column == *input,
                        MeasureExpr::Binary { lhs, .. } => lhs == *input,
                        _ => false,
                    })
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } => input.relation_oid == spec.fact_rel && input.type_oid == BOOLOID,
        // The internal spatial executor supplies an exact, rechecked SQL
        // mask. Public admission still validates the complete geometry shape.
        FilterSpec::Spatial { .. } => true,
        _ => false,
    }
}

/// Decimal execution requires at least one decimal state; integer-only shapes
/// retain their own exact kernels.
fn canonical_dense_numeric_sum_avg(spec: &AggQuerySpec) -> bool {
    let value_count = spec.measures.len()
        - usize::from(spec.measures.last().is_some_and(canonical_count_measure));
    value_count > 0
        && has_numeric_measure(spec)
        && spec.measures.len() <= abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        && spec.group_keys.len() <= abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        && spec.group_keys.iter().all(|key| {
            matches!(&key.source, GroupKeySource::FactColumn(column)
                if (column.relation_oid, column.type_oid) == (spec.fact_rel, key.type_oid)
                    && is_supported_group_type(key.type_oid)
                    && is_text_family(key.type_oid) == (key.collation_oid != 0))
        })
        && spec.star_dims.is_empty()
        && canonical_dense_fact_filter(spec)
        && spec.having.is_none()
        && spec.measures[..value_count].iter().all(|measure| {
            (matches!(measure.expression, MeasureExpr::Column(column)
                if matches!(column.type_oid, NUMERICOID | INT2OID | INT4OID | INT8OID)
                    && column.relation_oid == spec.fact_rel)
                || measure.filtered_count_input().is_some())
                && canonical_numeric_measure_filter(spec, &measure.filter)
                && !measure.outputs.is_empty()
                && measure.outputs.iter().all(|output| {
                    output.source == AggregateSource::Value
                        && matches!(
                            output.kind,
                            AggregateKind::Sum | AggregateKind::Avg | AggregateKind::Count
                        )
                })
        })
}

fn has_numeric_measure(spec: &AggQuerySpec) -> bool {
    spec.measures
        .iter()
        .any(crate::engine::spec::MeasureSpec::requires_numeric_accumulator)
}

/// Allocation-only forecast for a supplied dense capacity. Rows/digits are
/// deliberately absent: NUMERIC workspace layout depends on digit extents,
/// measure masks and dense slots, not payload values or chunk row counts.
pub fn numeric_descriptor_memory_requirements(
    spec: &AggQuerySpec,
    columns: &[crate::engine::residency::ResidentNumericColumnStats],
    nullable_columns: &[i16],
    group_capacity: usize,
) -> Result<crate::gpu::GroupedAggMemoryRequirements, String> {
    if !canonical_dense_numeric_sum_avg(spec)
        || group_capacity == 0
        || (spec.group_keys.is_empty() && group_capacity != 1)
    {
        return Err(
            "NUMERIC memory forecast requires canonical dense SUM/AVG/COUNT and a valid capacity"
                .into(),
        );
    }
    // SAFETY: ABI records contain integers/raw pointers with valid zero bit
    // patterns. All active fields are initialized below; payloads stay empty.
    let mut bound = BoundDescriptor {
        desc: unsafe { std::mem::zeroed() },
        _numeric_views: std::array::from_fn(|_| None),
        _scalar_views: std::array::from_fn(|_| None),
    };
    let desc = &mut bound.desc;
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.group_capacity = group_capacity;
    desc.key_count = spec.group_keys.len() as u32;
    for index in 0..spec.group_keys.len() {
        desc.keys[index].values.tag = PgaccelValTag::Int32;
        desc.keys[index].cardinality = if index == 0 {
            u32::try_from(group_capacity)
                .map_err(|_| "NUMERIC forecast group capacity exceeds u32")?
        } else {
            1
        };
        let GroupKeySource::FactColumn(column) = spec.group_keys[index].source else {
            return Err("NUMERIC forecast key is not a fact column".into());
        };
        desc.keys[index].null_code = if nullable_columns
            .iter()
            .any(|attno| i32::from(*attno) == column.attno)
        {
            0
        } else {
            abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE
        };
    }
    desc.where_filter = disabled_filter();
    desc.measure_filters.fill(disabled_filter());
    desc.measure_count = spec.measures.len() as u32;
    for (index, measure) in spec.measures.iter().enumerate() {
        let target = &mut desc.measures[index];
        target.agg_mask = lane_mask(&measure.outputs);
        if let Some(column) = match measure.expression {
            MeasureExpr::Column(column) => Some(column),
            _ => measure.filtered_count_input(),
        } {
            if column.type_oid == NUMERICOID {
                let matching = columns
                    .iter()
                    .filter(|stats| i32::from(stats.attno) == column.attno)
                    .collect::<Vec<_>>();
                let [stats] = matching.as_slice() else {
                    return Err(
                        "NUMERIC forecast requires exactly one resident digit extent per column"
                            .into(),
                    );
                };
                target.state_bytes = if target.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT {
                    8
                } else {
                    stats
                        .bounds
                        .sum_state_bytes()
                        .ok_or("invalid NUMERIC digit bounds")?
                };
                target.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
                let metadata = Box::new(abi::PgaccelNumericView {
                    rows: std::ptr::null(),
                    digits: std::ptr::null(),
                    row_count: 0,
                    digit_count: 0,
                    min_weight: stats.bounds.min_weight,
                    max_weight: stats.bounds.max_weight,
                    max_ndigits: stats.bounds.max_ndigits,
                    reserved: 0,
                });
                target.value.values = std::ptr::from_ref(metadata.as_ref()).cast();
                target.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
                target.value.element_bytes = size_of::<abi::PgaccelNumericView>() as u32;
                target.value.flags = abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW;
                bound._numeric_views[index] = Some(metadata);
            } else if column.type_oid == INT8OID {
                target.state_bytes = if target.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT {
                    8
                } else {
                    abi::PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES
                };
                target.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
                target.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
                target.value.element_bytes = 8;
            } else {
                target.state_bytes = 8;
                target.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
                target.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
                target.value.element_bytes = 4;
            }
            if measure.filter != FilterSpec::None {
                target.flags = abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
                target.rhs.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
                target.rhs.element_bytes = 4;
                desc.measure_filters[index] = build_numeric_measure_filter(spec, index)?;
            }
        } else {
            target.op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
            target.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
            target.state_bytes = 8;
        }
    }
    bound.desc.where_filter = build_filter(spec, 0, None)?;
    // SAFETY: the owned metadata above is live for the synchronous snapshot;
    // zero rows mean there are no resident device payloads to borrow.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }
        .map_err(|error| error.to_string())?;
    GroupedAggSession::memory_requirements(&plan, 1).map_err(|error| error.to_string())
}

fn canonical_dense_integer_value_measure(measure: &crate::engine::spec::MeasureSpec) -> bool {
    if measure.filter != FilterSpec::None {
        return false;
    }
    let outputs = measure.outputs.as_slice();
    match &measure.expression {
        MeasureExpr::Column(column) if column.type_oid == INT4OID => {
            outputs
                == [crate::engine::spec::AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                }]
                || outputs
                    == [
                        crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Sum,
                        },
                        crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Min,
                        },
                        crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Max,
                        },
                    ]
        }
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        } if lhs.type_oid == INT4OID && rhs.type_oid == INT4OID => {
            outputs
                == [crate::engine::spec::AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                }]
        }
        _ => false,
    }
}

fn canonical_dense_integer_measure_range(
    spec: &AggQuerySpec,
    measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    let MeasureExpr::Column(value) = &measure.expression else {
        return false;
    };
    let FilterSpec::Ranges { input, ranges } = &measure.filter else {
        return false;
    };
    let [range] = ranges.as_slice() else {
        return false;
    };
    let (ScalarValue::I32(lo), ScalarValue::I32(hi)) = (range.lo, range.hi) else {
        return false;
    };
    let [group_key] = spec.group_keys.as_slice() else {
        return false;
    };
    let GroupKeySource::FactColumn(group_column) = &group_key.source else {
        return false;
    };
    value.type_oid == INT4OID
        && input == value
        && lo != i32::MIN
        && hi != i32::MAX
        && lo < hi
        && value.relation_oid == spec.fact_rel
        && group_key.type_oid == INT4OID
        && group_column.type_oid == INT4OID
        && group_column.relation_oid == spec.fact_rel
        && group_column != value
        && spec.star_dims.is_empty()
        && spec.fact_filter == FilterSpec::None
        && spec.having.is_none()
        && measure.outputs.as_slice()
            == [crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Sum,
            }]
}

fn canonical_dense_integer_range_intersection(
    spec: &AggQuerySpec,
    value_measure: &crate::engine::spec::MeasureSpec,
) -> bool {
    let FilterSpec::Ranges { input, ranges } = &spec.fact_filter else {
        return false;
    };
    let [range] = ranges.as_slice() else {
        return false;
    };
    let (ScalarValue::I32(lo), ScalarValue::I32(hi)) = (range.lo, range.hi) else {
        return false;
    };
    let [group_key] = spec.group_keys.as_slice() else {
        return false;
    };
    let GroupKeySource::FactColumn(group_column) = &group_key.source else {
        return false;
    };
    let MeasureExpr::Binary {
        op: BinaryMeasureOp::Mul,
        lhs,
        rhs,
    } = &value_measure.expression
    else {
        return false;
    };
    lo != i32::MIN
        && hi != i32::MAX
        && lo < hi
        && input.type_oid == INT4OID
        && input == lhs
        && lhs.type_oid == INT4OID
        && rhs.type_oid == INT4OID
        && lhs.relation_oid == spec.fact_rel
        && rhs.relation_oid == spec.fact_rel
        && group_key.type_oid == INT4OID
        && group_column.type_oid == INT4OID
        && group_column.relation_oid == spec.fact_rel
        && group_column != lhs
        && group_column != rhs
        && spec.star_dims.is_empty()
        && spec.having.is_none()
}

/// Classify the only physical branch that normal planning may advertise for
/// this logical descriptor shape.
#[must_use]
pub fn planned_descriptor_kernel_mode(
    spec: &AggQuerySpec,
    hash_grouping: bool,
) -> GroupedAggKernelMode {
    if hash_grouping {
        return if matches!(spec.measures.as_slice(), [count] if canonical_count_measure(count)) {
            GroupedAggKernelMode::ParallelHash
        } else {
            GroupedAggKernelMode::SerialGeneric
        };
    }
    if canonical_dense_numeric_sum_avg(spec) {
        return GroupedAggKernelMode::ParallelDenseNumeric;
    }
    if canonical_dense_scalar_programs(spec) {
        return GroupedAggKernelMode::ParallelDenseInteger;
    }
    if matches!(spec.measures.as_slice(), [count] if canonical_count_measure(count))
        && ((unique_dimension_inputs(spec) && parallel_dense_fact_filter(&spec.fact_filter))
            || weighted_global_count_inputs(spec))
    {
        return GroupedAggKernelMode::ParallelDenseCount;
    }
    if !unique_dimension_inputs(spec) {
        return GroupedAggKernelMode::SerialGeneric;
    }
    if matches!(spec.measures.as_slice(), [measure]
        if matches!(measure.expression, MeasureExpr::Column(_))
            && measure.outputs.iter().all(|output| output.kind == AggregateKind::Count)
            && parallel_dense_fact_filter(&spec.fact_filter)
            && (canonical_dense_integer_sum_avg_measure(spec, measure, INT2OID)
                || canonical_dense_integer_sum_avg_measure(spec, measure, INT4OID)))
    {
        return GroupedAggKernelMode::ParallelDenseCount;
    }
    if canonical_dense_integer_rollup(spec) {
        return GroupedAggKernelMode::ParallelDenseInteger;
    }
    match spec.measures.as_slice() {
        [count]
            if parallel_dense_fact_filter(&spec.fact_filter)
                && (canonical_dense_bool_count_measure(spec, count)
                    || canonical_dense_int2_count_measure(spec, count)
                    || canonical_dense_int8_count_measure(spec, count)
                    || canonical_dense_date_count_measure(spec, count)
                    || canonical_dense_timestamp_count_measure(spec, count)
                    || canonical_dense_float4_count_measure(spec, count)
                    || canonical_dense_float8_count_measure(spec, count)) =>
        {
            GroupedAggKernelMode::ParallelDenseCount
        }
        [value]
            if canonical_dense_integer_sum_avg_measure(spec, value, INT2OID)
                || canonical_dense_integer_sum_avg_measure(spec, value, INT4OID) =>
        {
            GroupedAggKernelMode::ParallelDenseInteger
        }
        [value, count]
            if (canonical_dense_integer_value_measure(value)
                || canonical_dense_integer_measure_range(spec, value)
                || canonical_dense_integer_sum_avg_measure(spec, value, INT2OID)
                || canonical_dense_integer_sum_avg_measure(spec, value, INT4OID))
                && canonical_count_measure(count)
                && (parallel_dense_fact_filter(&spec.fact_filter)
                    || canonical_dense_integer_range_intersection(spec, value)) =>
        {
            GroupedAggKernelMode::ParallelDenseInteger
        }
        _ => GroupedAggKernelMode::SerialGeneric,
    }
}

fn classify_descriptor_specialization(
    spec: &AggQuerySpec,
    mode: GroupedAggKernelMode,
) -> DescriptorSpecialization {
    let membership = !spec.star_dims.is_empty();
    let sql_mask = matches!(
        spec.fact_filter,
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        } | FilterSpec::Spatial { .. }
            | FilterSpec::SpatialAnd { .. }
    );
    let family = match mode {
        GroupedAggKernelMode::ParallelHash
            if matches!(
                HashMeasureInput::from_spec(spec),
                Ok(HashMeasureInput::IntegerStates(_))
            ) =>
        {
            if spec.measures[0]
                .outputs
                .iter()
                .any(|output| matches!(output.kind, AggregateKind::Min | AggregateKind::Max))
            {
                DescriptorSpecializationFamily::HashInt4Extrema
            } else {
                DescriptorSpecializationFamily::HashInt4SumCount
            }
        }
        GroupedAggKernelMode::ParallelHash => DescriptorSpecializationFamily::HashCount,
        GroupedAggKernelMode::ParallelDenseCount => match spec.measures.first() {
            Some(measure) if canonical_dense_bool_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseBoolCount
            }
            Some(measure) if canonical_dense_int2_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseInt2Count
            }
            Some(measure) if canonical_dense_int8_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseInt8Count
            }
            Some(measure) if canonical_dense_date_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseDateCount
            }
            Some(measure) if canonical_dense_timestamp_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseTimestampCount
            }
            Some(measure) if canonical_dense_float4_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseFloat4Count
            }
            Some(measure) if canonical_dense_float8_count_measure(spec, measure) => {
                DescriptorSpecializationFamily::DenseFloat8Count
            }
            _ => DescriptorSpecializationFamily::DenseCount,
        },
        GroupedAggKernelMode::ParallelDenseInteger => match spec.measures.first() {
            _ if canonical_dense_scalar_programs(spec) => {
                DescriptorSpecializationFamily::DenseScalarPrograms
            }
            _ if spec.measures.iter().any(|measure| {
                matches!(measure.expression, MeasureExpr::Column(_))
                    && measure
                        .outputs
                        .iter()
                        .any(|output| output.kind == AggregateKind::Count)
            }) =>
            {
                DescriptorSpecializationFamily::DenseIntegerWithCounts
            }
            Some(measure)
                if canonical_dense_integer_measure_range(spec, measure)
                    && spec.measures.len() == 2
                    && canonical_count_measure(&spec.measures[1]) =>
            {
                DescriptorSpecializationFamily::DenseIntegerColumnMeasureRange
            }
            _ if canonical_dense_integer_rollup(spec)
                && spec
                    .measures
                    .iter()
                    .any(|measure| matches!(measure.expression, MeasureExpr::Binary { .. })) =>
            {
                DescriptorSpecializationFamily::DenseIntegerExpressions
            }
            _ if canonical_dense_filtered_integer_sum_avg(spec) => {
                DescriptorSpecializationFamily::DenseFilteredIntegerSumAvg
            }
            _ if canonical_dense_multi_integer_sum_avg(spec) => {
                DescriptorSpecializationFamily::DenseMultiIntegerSumAvg
            }
            Some(measure) if canonical_dense_integer_sum_avg_measure(spec, measure, INT2OID) => {
                DescriptorSpecializationFamily::DenseInt2SumAvg
            }
            Some(measure) if canonical_dense_integer_sum_avg_measure(spec, measure, INT4OID) => {
                DescriptorSpecializationFamily::DenseInt4SumAvg
            }
            Some(measure) if canonical_dense_integer_range_intersection(spec, measure) => {
                DescriptorSpecializationFamily::DenseIntegerMultiplyRange
            }
            Some(measure) if matches!(&measure.expression, MeasureExpr::Column(_)) => {
                DescriptorSpecializationFamily::DenseIntegerColumn
            }
            Some(measure)
                if matches!(
                    &measure.expression,
                    MeasureExpr::Binary {
                        op: BinaryMeasureOp::Mul,
                        ..
                    }
                ) =>
            {
                DescriptorSpecializationFamily::DenseIntegerMultiply
            }
            _ => DescriptorSpecializationFamily::SerialGeneric,
        },
        GroupedAggKernelMode::SerialGeneric => DescriptorSpecializationFamily::SerialGeneric,
        GroupedAggKernelMode::ParallelDenseNumeric => {
            DescriptorSpecializationFamily::DenseNumericSumAvg
        }
    };
    if matches!(
        family,
        DescriptorSpecializationFamily::HashCount
            | DescriptorSpecializationFamily::HashInt4SumCount
            | DescriptorSpecializationFamily::HashInt4Extrema
    ) {
        DescriptorSpecialization {
            family,
            membership: false,
            sql_mask: false,
        }
    } else {
        DescriptorSpecialization {
            family,
            membership,
            sql_mask,
        }
    }
}

fn cached_descriptor_specialization(
    identity: &DerivedArtifactIdentity,
    spec: &AggQuerySpec,
    mode: GroupedAggKernelMode,
) -> (
    DescriptorSpecialization,
    DescriptorSpecializationCacheOutcome,
) {
    DESCRIPTOR_SPECIALIZATION_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|entry| {
            entry.identity.digest() == identity.digest() && entry.identity == *identity
        }) {
            let entry = cache
                .remove(index)
                .expect("specialization cache index came from the same deque");
            let specialization = entry.specialization;
            cache.push_front(entry);
            return (specialization, DescriptorSpecializationCacheOutcome::Hit);
        }

        let specialization = classify_descriptor_specialization(spec, mode);
        cache.push_front(CachedDescriptorSpecialization {
            identity: identity.clone(),
            specialization,
        });
        cache.truncate(DESCRIPTOR_SPECIALIZATION_CACHE_CAPACITY);
        (specialization, DescriptorSpecializationCacheOutcome::Miss)
    })
}

fn is_text_family(type_oid: u32) -> bool {
    matches!(type_oid, TEXTOID | VARCHAROID | BPCHAROID)
}

fn is_supported_group_type(type_oid: u32) -> bool {
    matches!(
        type_oid,
        BOOLOID
            | INT2OID
            | INT4OID
            | INT8OID
            | FLOAT4OID
            | FLOAT8OID
            | DATEOID
            | TIMESTAMPOID
            | TIMESTAMPTZOID
            | TEXTOID
            | VARCHAROID
            | BPCHAROID
    )
}

fn validate_filter(
    filter: &FilterSpec,
    relation_oid: u32,
    dimension: bool,
    allow_internal_spatial: bool,
) -> Result<(), String> {
    match filter {
        FilterSpec::None => Ok(()),
        FilterSpec::NullOrRanges { input, .. }
            if !dimension && input.relation_oid == relation_oid && input.type_oid == INT4OID =>
        {
            Ok(())
        }
        FilterSpec::Ranges { input, .. }
            if input.relation_oid == relation_oid
                && if dimension {
                    matches!(
                        input.type_oid,
                        BOOLOID
                            | INT4OID
                            | INT8OID
                            | FLOAT4OID
                            | FLOAT8OID
                            | DATEOID
                            | TIMESTAMPOID
                            | TIMESTAMPTZOID
                    )
                } else {
                    matches!(
                        input.type_oid,
                        INT4OID
                            | INT8OID
                            | FLOAT4OID
                            | FLOAT8OID
                            | DATEOID
                            | TIMESTAMPOID
                            | TIMESTAMPTZOID
                    )
                } =>
        {
            Ok(())
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.relation_oid == relation_oid && input.type_oid == BOOLOID => Ok(()),
        FilterSpec::Spatial { .. } if !dimension && allow_internal_spatial => Ok(()),
        FilterSpec::Bytecode { inputs, program } if !dimension => {
            if inputs
                .iter()
                .any(|input| input.relation_oid != relation_oid)
                || !crate::engine::spec::scalar::ScalarProgram::validate_wire(program).is_ok_and(
                    |proof| proof.result_type() == crate::engine::spec::scalar::ScalarType::Bool,
                )
            {
                return Err("invalid typed scalar WHERE".into());
            }
            Ok(())
        }
        FilterSpec::SpatialAnd { scalar, .. } if !dimension && allow_internal_spatial => {
            validate_filter(scalar, relation_oid, false, false)
        }
        _ if dimension => Err("dimension filter is outside the Phase 5D descriptor subset".into()),
        _ => Err("fact filter is outside the Phase 5D descriptor subset".into()),
    }
}

fn validate_measure_outputs(
    expression: &MeasureExpr,
    outputs: &[crate::engine::spec::AggregateOutput],
) -> Result<(), String> {
    if outputs
        .iter()
        .any(|output| output.source != AggregateSource::Value)
    {
        return Err("Phase 5D does not expose RHS aggregate lanes".into());
    }
    let count_min_max = || {
        outputs.iter().all(|output| {
            matches!(
                output.kind,
                AggregateKind::Count | AggregateKind::Min | AggregateKind::Max
            )
        })
    };
    let basic = |kind: AggregateKind| {
        matches!(
            kind,
            AggregateKind::Sum | AggregateKind::Count | AggregateKind::Min | AggregateKind::Max
        )
    };
    let widened_integer = |kind: AggregateKind| basic(kind) || kind == AggregateKind::Avg;
    match expression {
        MeasureExpr::Bytecode {
            program,
            result_type_oid,
            ..
        } if ScalarProgram::validate_wire(program).is_ok_and(|proof| {
            (*result_type_oid == INT4OID
                && proof.result_type() == ScalarType::Int4
                && outputs.iter().all(|output| widened_integer(output.kind)))
                || (*result_type_oid == BOOLOID
                    && proof.result_type() == ScalarType::Bool
                    && outputs
                        .iter()
                        .all(|output| output.kind == AggregateKind::Count))
                || (*result_type_oid == DATEOID
                    && proof.result_type() == ScalarType::Date
                    && count_min_max())
        }) =>
        {
            Ok(())
        }
        MeasureExpr::Column(column)
            if matches!(column.type_oid, NUMERICOID | INT8OID)
                && !outputs.is_empty()
                && outputs.iter().all(|output| {
                    matches!(
                        output.kind,
                        AggregateKind::Sum | AggregateKind::Avg | AggregateKind::Count
                    )
                }) =>
        {
            Ok(())
        }
        MeasureExpr::CountStar if outputs.len() == 1 && outputs[0].kind == AggregateKind::Count => {
            Ok(())
        }
        MeasureExpr::Column(column) if column.type_oid == INT4OID => {
            if outputs.iter().all(|output| widened_integer(output.kind)) {
                Ok(())
            } else {
                Err("INT4 descriptor measure requests an unsupported aggregate lane".into())
            }
        }
        MeasureExpr::Column(column)
            if matches!(
                column.type_oid,
                INT8OID | FLOAT4OID | FLOAT8OID | DATEOID | TIMESTAMPOID | TIMESTAMPTZOID
            ) && count_min_max() =>
        {
            Ok(())
        }
        MeasureExpr::Column(column)
            if column.type_oid == INT2OID
                && outputs.iter().all(|output| widened_integer(output.kind)) =>
        {
            Ok(())
        }
        MeasureExpr::Column(column)
            if column.type_oid == BOOLOID
                && outputs
                    .iter()
                    .all(|output| output.kind == AggregateKind::Count) =>
        {
            Ok(())
        }
        MeasureExpr::Binary { op, lhs, rhs }
            if lhs.type_oid == INT4OID
                && rhs.type_oid == INT4OID
                && matches!(
                    op,
                    BinaryMeasureOp::Mul | BinaryMeasureOp::Sub | BinaryMeasureOp::Add
                )
                && outputs.iter().all(|output| widened_integer(output.kind)) =>
        {
            Ok(())
        }
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Sub,
            lhs,
            rhs,
        } if lhs.type_oid == INT8OID && rhs.type_oid == INT8OID && count_min_max() => Ok(()),
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs,
            rhs,
        } if lhs.type_oid == INT8OID && rhs.type_oid == INT8OID => {
            Err("INT8 multiplication is not implemented by the generic kernel".into())
        }
        _ => Err("measure is outside the Phase 5D descriptor subset".into()),
    }
}

fn h3_parent_group(spec: &AggQuerySpec) -> Option<(ColumnRef, i32, u32)> {
    let [key] = spec.group_keys.as_slice() else {
        return None;
    };
    let GroupKeySource::H3CellToParent { cell, resolution } = key.source else {
        return None;
    };
    Some((cell, resolution, key.type_oid))
}

fn raw_hash_group(spec: &AggQuerySpec) -> Option<ColumnRef> {
    let [key] = spec.group_keys.as_slice() else {
        return None;
    };
    let GroupKeySource::FactColumn(column) = key.source else {
        return None;
    };
    (column.relation_oid == spec.fact_rel
        && column.attno > 0
        && matches!(column.type_oid, INT2OID | INT4OID | INT8OID)
        && key.type_oid == column.type_oid
        && key.collation_oid == 0
        && key.encoding == GroupKeyEncoding::Hash)
        .then_some(column)
}

fn compact_hash_column_refs(spec: &AggQuerySpec) -> Result<Vec<ResidentColumnRef>, String> {
    let mut requests = dispatch_column_refs(spec)?;
    if let Some(column) = hash_where_filter(spec)? {
        let predicate = ResidentColumnRef {
            relid: column.relation_oid.into(),
            attno: i16::try_from(column.attno).map_err(|_| "hash WHERE attribute exceeds int16")?,
        };
        if !requests.contains(&predicate) {
            requests.push(predicate);
            requests.sort_unstable_by_key(|request| (u32::from(request.relid), request.attno));
        }
    }
    if let Some(column) = raw_hash_group(spec) {
        let key = ResidentColumnRef {
            relid: column.relation_oid.into(),
            attno: i16::try_from(column.attno).map_err(|_| "raw hash attribute exceeds int16")?,
        };
        if !requests.contains(&key) {
            requests.push(key);
            requests.sort_unstable_by_key(|request| (u32::from(request.relid), request.attno));
        }
    }
    Ok(requests)
}

fn validate_h3_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    let Some((cell, resolution, result_type_oid)) = h3_parent_group(spec) else {
        return Err("only one H3 cell-to-parent group key is supported".to_owned());
    };
    if cell.relation_oid != spec.fact_rel
        || cell.type_oid != result_type_oid
        || !(0..=15).contains(&resolution)
        || spec.group_keys[0].encoding != GroupKeyEncoding::Hash
        || spec.group_keys[0].collation_oid != 0
    {
        return Err("H3 parent group key metadata is not canonical".to_owned());
    }
    if !spec.star_dims.is_empty()
        || !matches!(spec.fact_filter, FilterSpec::None)
        || spec.having.is_some()
    {
        return Err("H3 parent grouping does not support joins, filters, or HAVING".to_owned());
    }
    let [measure] = spec.measures.as_slice() else {
        return Err("H3 parent grouping requires exactly one COUNT(*) measure".to_owned());
    };
    if !matches!(measure.expression, MeasureExpr::CountStar)
        || !matches!(measure.filter, FilterSpec::None)
        || measure.outputs.len() != 1
        || measure.outputs[0].source != AggregateSource::Value
        || measure.outputs[0].kind != AggregateKind::Count
    {
        return Err("H3 parent grouping requires exactly one unfiltered COUNT(*)".to_owned());
    }
    if projection.slots.len() != 2
        || projection
            .slots
            .iter()
            .filter(|slot| {
                matches!(
                    slot.source,
                    crate::engine::spec::AggOutputSource::GroupKey { key_index: 0 }
                )
            })
            .count()
            != 1
        || projection
            .slots
            .iter()
            .filter(|slot| {
                matches!(
                    slot.source,
                    crate::engine::spec::AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }
                )
            })
            .count()
            != 1
    {
        return Err(
            "H3 parent grouping projection must contain its key and COUNT(*) once".to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_capability_with_spatial(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
    allow_internal_spatial: bool,
) -> Result<(), String> {
    spec.validate()
        .map_err(|error| format!("invalid aggregate query spec: {error}"))?;
    for (index, measure) in spec.measures.iter().enumerate() {
        if matches!(measure.expression, MeasureExpr::Bytecode { .. })
            && scalar_measure_program(&measure.expression, spec.fact_rel)
                .is_ok_and(|proof| proof.uses_predicate_mask())
            && (spec.fact_filter.spatial().is_none()
                || scalar_where_measure_binding(spec) != Some(index))
        {
            return Err("predicate-mask program requires its spatial WHERE carrier".into());
        }
    }
    projection
        .validate(spec)
        .map_err(|error| format!("invalid aggregate output projection: {error}"))?;
    if spec.group_keys.iter().any(|key| {
        matches!(
            key.source,
            GroupKeySource::H3CellToParent { .. } | GroupKeySource::H3LatLngToCell { .. }
        )
    }) {
        return validate_h3_runtime_capability(spec, projection);
    }
    if spec.having.is_some() {
        return Err("HAVING is not implemented by the Phase 5D descriptor executor".into());
    }
    if has_numeric_measure(spec) && !canonical_dense_numeric_sum_avg(spec) {
        return Err("NUMERIC requires fact-column NUMERIC/INT2/INT4/INT8 SUM/AVG/COUNT states, optional exact INT4 aggregate filters and final unfiltered COUNT(*)".into());
    }
    if has_scalar_measure(spec) && !canonical_dense_scalar_programs(spec) {
        return Err("scalar programs require typed fact-only dense integer/Boolean states and exact INT4 filters".into());
    }
    for key in &spec.group_keys {
        if !is_supported_group_type(key.type_oid) {
            return Err(format!("group type OID {} is unsupported", key.type_oid));
        }
        if key.encoding != GroupKeyEncoding::Hash {
            return Err("generic plan group keys must use unresolved HASH encoding".into());
        }
        if is_text_family(key.type_oid) != (key.collation_oid != 0) {
            return Err("group-key type/collation metadata is not canonical".into());
        }
        if !matches!(
            key.source,
            GroupKeySource::FactColumn(_) | GroupKeySource::StarDimension { .. }
        ) {
            return Err("Phase 5D supports only column group keys".into());
        }
    }
    let counted_dimension = spec
        .star_dims
        .iter()
        .any(|dimension| dimension.multiplicity == JoinMultiplicity::Counted);
    for dimension in &spec.star_dims {
        if dimension.fact_key.type_oid != dimension.dim_key.type_oid {
            return Err("join inputs must have exactly the same type OID".into());
        }
        match dimension.fact_key.type_oid {
            INT4OID | INT8OID if dimension.collation_oid == 0 => {}
            type_oid if is_text_family(type_oid) && dimension.collation_oid != 0 => {}
            type_oid => return Err(format!("join type OID {type_oid} is unsupported")),
        }
        validate_filter(
            &dimension.filter,
            dimension.relation_oid,
            true,
            allow_internal_spatial,
        )?;
    }
    validate_filter(
        &spec.fact_filter,
        spec.fact_rel,
        false,
        allow_internal_spatial,
    )?;
    for measure in &spec.measures {
        if !matches!(measure.filter, FilterSpec::None)
            && !canonical_dense_integer_measure_range(spec, measure)
            && !canonical_dense_filtered_integer_sum_avg(spec)
            && !canonical_dense_numeric_sum_avg(spec)
            && !canonical_dense_scalar_programs(spec)
        {
            return Err("aggregate FILTER is not implemented by Phase 5D".into());
        }
        validate_measure_outputs(&measure.expression, &measure.outputs)?;
        if counted_dimension
            && measure
                .outputs
                .iter()
                .any(|output| output.kind == AggregateKind::Sum)
            && matches!(
                measure.expression,
                MeasureExpr::Column(ColumnRef {
                    type_oid: INT4OID,
                    ..
                }) | MeasureExpr::Binary {
                    lhs: ColumnRef {
                        type_oid: INT4OID,
                        ..
                    },
                    ..
                }
            )
        {
            return Err("integer SUM with a counted dimension is conservatively declined".into());
        }
    }
    Ok(())
}

pub(super) fn validate_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    validate_runtime_capability_with_spatial(spec, projection, false)
}

/// Production admission is deliberately narrower than executor capability.
/// INT8 membership is released only for the independently measured two-unique-
/// dimension SSBM shape; keeping this check here prevents later executor work
/// from silently widening planner admission.
pub(super) fn validate_normal_descriptor_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    validate_runtime_capability(spec, projection)?;
    let has_int8_membership = spec
        .star_dims
        .iter()
        .any(|dimension| dimension.fact_key.type_oid == INT8OID);
    if !has_int8_membership {
        return Ok(());
    }

    let [first_dim, second_dim] = spec.star_dims.as_slice() else {
        return Err("normal INT8 star admission requires exactly two unique dimensions".to_owned());
    };
    for dimension in [first_dim, second_dim] {
        if dimension.relation_oid == spec.fact_rel
            || dimension.fact_key.relation_oid != spec.fact_rel
            || dimension.dim_key.relation_oid != dimension.relation_oid
            || dimension.fact_key.type_oid != INT8OID
            || dimension.dim_key.type_oid != INT8OID
            || dimension.collation_oid != 0
            || dimension.multiplicity != JoinMultiplicity::Unique
            || dimension.filter != FilterSpec::None
        {
            return Err(
                "normal INT8 star admission requires exact unfiltered unique INT8 membership"
                    .to_owned(),
            );
        }
    }
    if first_dim.relation_oid == second_dim.relation_oid
        || !matches!(spec.fact_filter, FilterSpec::None)
        || spec.having.is_some()
    {
        return Err("normal INT8 star admission requires the unfiltered SSBM star".to_owned());
    }

    let [first_key, second_key] = spec.group_keys.as_slice() else {
        return Err(
            "normal INT8 star admission requires exactly two INT4 dimension keys".to_owned(),
        );
    };
    for (index, (key, dimension)) in [(first_key, first_dim), (second_key, second_dim)]
        .into_iter()
        .enumerate()
    {
        let GroupKeySource::StarDimension {
            dim_index,
            group_column,
        } = key.source
        else {
            return Err("normal INT8 star admission groups only by dimension columns".to_owned());
        };
        if usize::try_from(dim_index) != Ok(index)
            || group_column.relation_oid != dimension.relation_oid
            || group_column.type_oid != INT4OID
            || key.type_oid != INT4OID
            || key.collation_oid != 0
            || key.encoding != GroupKeyEncoding::Hash
        {
            return Err(
                "normal INT8 star admission requires canonical INT4 dimension grouping".to_owned(),
            );
        }
    }

    let [sum, count] = spec.measures.as_slice() else {
        return Err("normal INT8 star admission requires SUM(int4) and COUNT(*)".to_owned());
    };
    let MeasureExpr::Column(sum_column) = sum.expression else {
        return Err("normal INT8 star admission requires SUM over one fact INT4 column".to_owned());
    };
    if sum_column.relation_oid != spec.fact_rel
        || sum_column.type_oid != INT4OID
        || sum.filter != FilterSpec::None
        || sum.outputs.as_slice()
            != [crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Sum,
            }]
        || count.expression != MeasureExpr::CountStar
        || count.filter != FilterSpec::None
        || count.outputs.as_slice()
            != [crate::engine::spec::AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }]
    {
        return Err("normal INT8 star admission requires canonical SUM(int4), COUNT(*)".to_owned());
    }

    let [group_zero, group_one, sum_slot, count_slot] = projection.slots.as_slice() else {
        return Err("normal INT8 star admission requires four canonical outputs".to_owned());
    };
    let canonical_group = |slot: &crate::engine::spec::AggOutputSlot, key_index| {
        slot.source == AggOutputSource::GroupKey { key_index }
            && slot.source_type_oid == INT4OID
            && slot.result_type_oid == INT4OID
            && slot.result_typmod == -1
            && slot.result_collation_oid == 0
            && !slot.nullable
    };
    if !canonical_group(group_zero, 0)
        || !canonical_group(group_one, 1)
        || sum_slot.source
            != (AggOutputSource::Aggregate {
                measure_index: 0,
                source: AggregateSource::Value,
                kind: AggregateKind::Sum,
            })
        || sum_slot.source_type_oid != INT4OID
        || sum_slot.result_type_oid != INT8OID
        || sum_slot.result_typmod != -1
        || sum_slot.result_collation_oid != 0
        || !sum_slot.nullable
        || count_slot.source
            != (AggOutputSource::Aggregate {
                measure_index: 1,
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            })
        || count_slot.source_type_oid != 0
        || count_slot.result_type_oid != INT8OID
        || count_slot.result_typmod != -1
        || count_slot.result_collation_oid != 0
        || count_slot.nullable
    {
        return Err("normal INT8 star admission output projection is not canonical".to_owned());
    }
    Ok(())
}

fn validate_internal_runtime_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    validate_runtime_capability_with_spatial(spec, projection, true)
}

fn geometry_typmod_shape(metadata: SpatialValueMetadata) -> Option<(u32, i32)> {
    if metadata.kind != SpatialValueKind::Geometry
        || metadata.typmod < 0
        || metadata.typmod & 3 != 0
    {
        return None;
    }
    let geometry_type = u32::try_from((metadata.typmod & 0xfc) >> 2).ok()?;
    let srid = ((metadata.typmod & 0x0fff_ff00) - (metadata.typmod & 0x1000_0000)) >> 8;
    Some((geometry_type, srid))
}

/// Production admission for resident point/polygon topological aggregates.
/// Geometry validation, exact boundary rechecks, bounded coordinate work and
/// costing remain required; distance and row-producing joins stay separate.
pub(super) fn validate_normal_spatial_candidate_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    if !canonical_dense_numeric_sum_avg(spec)
        && !canonical_dense_integer_rollup(spec)
        && !canonical_dense_scalar_programs(spec)
    {
        let [measure] = spec.measures.as_slice() else {
            return Err(
                "normal spatial admission requires exactly one COUNT(*) measure".to_owned(),
            );
        };
        if !spec.group_keys.is_empty()
            || !spec.star_dims.is_empty()
            || spec.having.is_some()
            || !matches!(measure.expression, MeasureExpr::CountStar)
            || !matches!(measure.filter, FilterSpec::None)
            || measure.outputs.as_slice()
                != [crate::engine::spec::AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }]
        {
            return Err(
                "normal spatial admission covers only ungrouped, unjoined COUNT(*)".to_owned(),
            );
        }
        let [slot] = projection.slots.as_slice() else {
            return Err("normal spatial admission requires exactly one COUNT(*) output".to_owned());
        };
        if slot.source
            != (AggOutputSource::Aggregate {
                measure_index: 0,
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            })
            || slot.source_type_oid != 0
            || slot.result_type_oid != INT8OID
            || slot.result_typmod != -1
            || slot.result_collation_oid != 0
            || slot.nullable
        {
            return Err("normal spatial admission COUNT(*) projection is not canonical".to_owned());
        }
    }
    let FilterSpec::Spatial {
        predicate,
        left,
        right,
        distance: None,
    } = spec
        .fact_filter
        .spatial()
        .ok_or("spatial predicate missing")?
    else {
        return Err("normal spatial admission requires a topological spatial predicate".to_owned());
    };
    let (column, column_metadata, constant_metadata, bytes) = match (predicate, left, right) {
        (SpatialPredicateKind::Intersects | SpatialPredicateKind::Within | SpatialPredicateKind::CoveredBy,
         SpatialOperand::Column { column, metadata },
         SpatialOperand::Constant { metadata: constant_metadata, bytes }) =>
            (column, metadata, constant_metadata, bytes),
        (SpatialPredicateKind::Intersects | SpatialPredicateKind::Contains | SpatialPredicateKind::Covers,
         SpatialOperand::Constant { metadata: constant_metadata, bytes },
         SpatialOperand::Column { column, metadata }) =>
            (column, metadata, constant_metadata, bytes),
        _ => return Err("normal spatial admission requires point/polygon Intersects, Contains, Within, Covers or CoveredBy with the correct orientation".to_owned()),
    };
    let limits = crate::engine::cost::device_limits();
    let parsed = crate::engine::residency::validate_resident_geometry_value(
        bytes,
        limits.resident_domain_max_exact_value_bytes,
    )?;
    let point_type = crate::engine::residency::RESIDENT_GEOMETRY_POINT;
    let polygon_type = crate::engine::residency::RESIDENT_GEOMETRY_POLYGON;
    let multi_type = crate::engine::residency::RESIDENT_GEOMETRY_MULTIPOLYGON;
    if column.relation_oid != spec.fact_rel
        || geometry_typmod_shape(*column_metadata) != Some((point_type, parsed.srid))
        || column_metadata.srid != Some(parsed.srid)
        || parsed.srid == 0
        || constant_metadata.kind != SpatialValueKind::Geometry
        || constant_metadata.srid != Some(parsed.srid)
        || (constant_metadata.typmod != -1
            && geometry_typmod_shape(*constant_metadata) != Some((parsed.geom_type, parsed.srid)))
        || (parsed.geom_type != polygon_type && parsed.geom_type != multi_type)
        || parsed.ring_count == 0
        || parsed.coordinate_pairs < 4
        || parsed.coordinate_pairs > limits.gpu_spatial_max_vertices_per_row
    {
        return Err(
            "normal spatial admission requires a bounded same-SRID POINT/polygonal contract"
                .to_owned(),
        );
    }
    validate_internal_runtime_capability(spec, projection)
}

#[cfg(any(test, feature = "pg_test"))]
pub(super) fn validate_test_forced_spatial_capability(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    if spec.fact_filter.spatial().is_none() {
        return Err("test-forced spatial capability requires a spatial fact filter".to_owned());
    }
    validate_internal_runtime_capability(spec, projection)
}

fn validate_catalog_contract(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> Result<(), String> {
    let collations = spec
        .group_keys
        .iter()
        .map(|key| key.collation_oid)
        .chain(
            spec.star_dims
                .iter()
                .map(|dimension| dimension.collation_oid),
        )
        .filter(|oid| *oid != 0)
        .collect::<BTreeSet<_>>();
    for oid in collations {
        // SAFETY: executor Begin runs on the PostgreSQL main backend thread and
        // the strict wire has already proved a nonzero collation OID.
        if !unsafe { pg_sys::get_collation_isdeterministic(pg_sys::Oid::from(oid)) } {
            return Err(format!(
                "collation OID {oid} is missing or nondeterministic at execution time"
            ));
        }
    }
    let validate_column = |column: ColumnRef, expected_collation: u32| -> Result<i32, String> {
        let attnum = i16::try_from(column.attno)
            .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
        let mut type_oid = pg_sys::InvalidOid;
        let mut typmod = -1;
        let mut collation_oid = pg_sys::InvalidOid;
        // SAFETY: executor Begin runs on the main backend thread. The strict
        // spec proved positive relation/attribute identities before lookup.
        unsafe {
            pg_sys::get_atttypetypmodcoll(
                pg_sys::Oid::from(column.relation_oid),
                attnum,
                std::ptr::from_mut(&mut type_oid),
                std::ptr::from_mut(&mut typmod),
                std::ptr::from_mut(&mut collation_oid),
            );
        }
        if u32::from(type_oid) != column.type_oid {
            return Err(format!(
                "column ({}, {}) catalog type OID {} does not match planned OID {}",
                column.relation_oid,
                column.attno,
                u32::from(type_oid),
                column.type_oid
            ));
        }
        if u32::from(collation_oid) != expected_collation {
            return Err(format!(
                "column ({}, {}) catalog collation OID {} does not match planned OID {}",
                column.relation_oid,
                column.attno,
                u32::from(collation_oid),
                expected_collation
            ));
        }
        Ok(typmod)
    };
    let mut key_typmods = Vec::with_capacity(spec.group_keys.len());
    for key in &spec.group_keys {
        let typmod = match key.source {
            GroupKeySource::FactColumn(column)
            | GroupKeySource::StarDimension {
                group_column: column,
                ..
            } => validate_column(column, key.collation_oid)?,
            GroupKeySource::H3CellToParent { cell, .. } => {
                let typmod = validate_column(cell, 0)?;
                if typmod != -1 || cell.type_oid != key.type_oid || key.collation_oid != 0 {
                    return Err("H3 parent column/result catalog metadata is not canonical".into());
                }
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }?;
                if u32::from(catalog.type_oid) != key.type_oid {
                    return Err(format!(
                        "planned H3 type OID {} is not the canonical h3index OID {}",
                        key.type_oid,
                        u32::from(catalog.type_oid)
                    ));
                }
                // SAFETY: both OIDs were resolved from the canonical catalog proof.
                unsafe {
                    crate::engine::ffi::syscache::validate_h3_parent_function(
                        catalog.parent_fn_oid,
                        catalog.type_oid,
                    )
                }?;
                typmod
            }
            GroupKeySource::Expression { .. } | GroupKeySource::H3LatLngToCell { .. } => -1,
        };
        key_typmods.push(typmod);
    }
    for dimension in &spec.star_dims {
        validate_column(dimension.fact_key, dimension.collation_oid)?;
        validate_column(dimension.dim_key, dimension.collation_oid)?;
    }
    if let Some(FilterSpec::Spatial { left, right, .. }) = spec.fact_filter.spatial() {
        for operand in [left, right] {
            let SpatialOperand::Column { column, metadata } = operand else {
                continue;
            };
            let actual_typmod = validate_column(*column, 0)?;
            if actual_typmod != metadata.typmod {
                return Err(format!(
                    "spatial column ({}, {}) catalog typmod {actual_typmod} does not match planned typmod {}",
                    column.relation_oid, column.attno, metadata.typmod
                ));
            }
        }
    }
    for slot in &projection.slots {
        if let crate::engine::spec::AggOutputSource::GroupKey { key_index } = slot.source {
            let key_index = usize::try_from(key_index)
                .map_err(|_| "group projection key index exceeds usize".to_owned())?;
            let actual_typmod = key_typmods
                .get(key_index)
                .copied()
                .ok_or_else(|| "group projection references a missing key typmod".to_owned())?;
            if slot.result_typmod != actual_typmod {
                return Err(format!(
                    "group projection key {key_index} typmod {} does not match catalog typmod {actual_typmod}",
                    slot.result_typmod
                ));
            }
        }
    }
    Ok(())
}

fn insert_column(
    relations: &mut BTreeMap<u32, BTreeSet<i16>>,
    column: ColumnRef,
) -> Result<(), String> {
    let attno = i16::try_from(column.attno)
        .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
    relations
        .entry(column.relation_oid)
        .or_default()
        .insert(attno);
    Ok(())
}

fn insert_filter_columns(
    relations: &mut BTreeMap<u32, BTreeSet<i16>>,
    filter: &FilterSpec,
) -> Result<(), String> {
    match filter {
        FilterSpec::SpatialAnd { spatial, scalar } => {
            insert_filter_columns(relations, spatial)?;
            insert_filter_columns(relations, scalar)
        }
        FilterSpec::None => Ok(()),
        FilterSpec::Ranges { input, .. }
        | FilterSpec::NullOrRanges { input, .. }
        | FilterSpec::Mask { input, .. } => insert_column(relations, *input),
        FilterSpec::Bytecode { inputs, .. } => {
            for input in inputs {
                insert_column(relations, *input)?;
            }
            Ok(())
        }
        FilterSpec::Spatial { left, right, .. } => {
            if let Some(column) = left.column() {
                insert_column(relations, column)?;
            }
            if let Some(column) = right.column() {
                insert_column(relations, column)?;
            }
            Ok(())
        }
    }
}

fn selected_relations(spec: &AggQuerySpec) -> Result<Vec<SelectedRelation>, String> {
    let mut relations = BTreeMap::<u32, BTreeSet<i16>>::new();
    relations.entry(spec.fact_rel).or_default();
    for key in &spec.group_keys {
        match &key.source {
            GroupKeySource::FactColumn(column) => insert_column(&mut relations, *column)?,
            GroupKeySource::StarDimension { group_column, .. } => {
                insert_column(&mut relations, *group_column)?;
            }
            GroupKeySource::Expression { inputs, .. } => {
                for input in inputs {
                    insert_column(&mut relations, *input)?;
                }
            }
            GroupKeySource::H3CellToParent { cell, .. } => {
                insert_column(&mut relations, *cell)?;
            }
            GroupKeySource::H3LatLngToCell {
                latitude,
                longitude,
                ..
            } => {
                insert_column(&mut relations, *latitude)?;
                insert_column(&mut relations, *longitude)?;
            }
        }
    }
    for measure in &spec.measures {
        match &measure.expression {
            MeasureExpr::CountStar => {}
            MeasureExpr::Column(column) => insert_column(&mut relations, *column)?,
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                insert_column(&mut relations, *lhs)?;
                insert_column(&mut relations, *rhs)?;
            }
            MeasureExpr::Bytecode { inputs, .. } => {
                for input in inputs {
                    insert_column(&mut relations, *input)?;
                }
            }
        }
        insert_filter_columns(&mut relations, &measure.filter)?;
    }
    insert_filter_columns(&mut relations, &spec.fact_filter)?;
    for dimension in &spec.star_dims {
        relations.entry(dimension.relation_oid).or_default();
        insert_column(&mut relations, dimension.fact_key)?;
        insert_column(&mut relations, dimension.dim_key)?;
        insert_filter_columns(&mut relations, &dimension.filter)?;
    }
    Ok(relations
        .into_iter()
        .map(|(relid, columns)| SelectedRelation {
            relid: pg_sys::Oid::from(relid),
            columns: columns.into_iter().collect(),
        })
        .collect())
}

fn dispatch_column_refs(spec: &AggQuerySpec) -> Result<Vec<ResidentColumnRef>, String> {
    let mut columns = BTreeSet::new();
    let mut insert = |column: ColumnRef| -> Result<(), String> {
        columns.insert((
            column.relation_oid,
            i16::try_from(column.attno)
                .map_err(|_| format!("attribute {} exceeds int16", column.attno))?,
        ));
        Ok(())
    };
    for measure in &spec.measures {
        match &measure.expression {
            MeasureExpr::CountStar => {}
            MeasureExpr::Column(column) => insert(*column)?,
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                insert(*lhs)?;
                insert(*rhs)?;
            }
            MeasureExpr::Bytecode { inputs, .. } => {
                for input in inputs {
                    insert(*input)?;
                }
            }
        }
        if let Some((input, _, _)) = measure.filter.range_parts() {
            insert(input)?;
        }
        if let FilterSpec::Mask { input, .. } = measure.filter {
            insert(input)?;
        }
    }
    if let Some((cell, ..)) = h3_parent_group(spec) {
        insert(cell)?;
    }
    if let Some((input, _, _)) = spec.fact_filter.scalar().range_parts() {
        insert(input)?;
    }
    Ok(columns
        .into_iter()
        .map(|(relid, attno)| ResidentColumnRef {
            relid: pg_sys::Oid::from(relid),
            attno,
        })
        .collect())
}

fn dependency_relids(spec: &AggQuerySpec) -> Vec<pg_sys::Oid> {
    std::iter::once(spec.fact_rel)
        .chain(
            spec.star_dims
                .iter()
                .map(|dimension| dimension.relation_oid),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(pg_sys::Oid::from)
        .collect()
}

/// Begin-time plan for a generic childless aggregate.
#[derive(Debug, Clone)]
enum DescriptorArtifactKind {
    Dense,
    RawHash {
        column: ColumnRef,
    },
    H3Parent {
        cell: ColumnRef,
        resolution: i32,
        max_chunk_rows: usize,
    },
    Spatial(SpatialTransformPlan),
}

/// Physical ownership policy for descriptor-derived state. `EphemeralFused`
/// is query-owned and never enters the backend LRU; H3 additionally performs
/// its exact parent transform inside the consuming hash kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DescriptorArtifactPolicy {
    CachedReusable,
    EphemeralFused,
}

impl DescriptorArtifactPolicy {
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::CachedReusable => "cached_reusable",
            Self::EphemeralFused => "ephemeral_fused",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactPolicyReason {
    CacheHit,
    EmptyArtifact,
    MemoryBudget,
    LowerExpectedCost,
    PredicateOrder,
    #[cfg(feature = "pg_test")]
    QualificationOverride,
}

impl ArtifactPolicyReason {
    const fn label(self) -> &'static str {
        match self {
            Self::CacheHit => "exact_cache_hit",
            Self::EmptyArtifact => "zero_retained_bytes",
            Self::MemoryBudget => "retained_memory_budget",
            Self::LowerExpectedCost => "lower_expected_total_cost",
            Self::PredicateOrder => "where_before_h3_transform",
            #[cfg(feature = "pg_test")]
            Self::QualificationOverride => "test_only_qualification_override",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactPolicyInput {
    construction_bytes: u64,
    ephemeral_bytes: u64,
    construction_time_us: u64,
    expected_reuses: u32,
    cache_hit: bool,
    invalidation_risk_ppm: u32,
    cached_build_launches: u64,
    ephemeral_build_launches: u64,
    consumer_launches: u64,
    budget_headroom_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ArtifactPolicyEvidence {
    policy: DescriptorArtifactPolicy,
    reason: ArtifactPolicyReason,
    input: ArtifactPolicyInput,
}

impl ArtifactPolicyEvidence {
    pub(super) const fn policy_label(self) -> &'static str {
        self.policy.label()
    }

    pub(super) fn summary(self) -> String {
        format!(
            "{} reason={} construction_bytes={} ephemeral_bytes={} construction_time_us={} expected_reuses={} cache_hit={} invalidation_risk_ppm={} cached_build_launches={} ephemeral_build_launches={} consumer_launches={} budget_headroom_bytes={}",
            self.policy.label(),
            self.reason.label(),
            self.input.construction_bytes,
            self.input.ephemeral_bytes,
            self.input.construction_time_us,
            self.input.expected_reuses,
            self.input.cache_hit,
            self.input.invalidation_risk_ppm,
            self.input.cached_build_launches,
            self.input.ephemeral_build_launches,
            self.input.consumer_launches,
            self.input.budget_headroom_bytes,
        )
    }
}

fn choose_artifact_policy(input: ArtifactPolicyInput) -> ArtifactPolicyEvidence {
    if input.cache_hit {
        return ArtifactPolicyEvidence {
            policy: DescriptorArtifactPolicy::CachedReusable,
            reason: ArtifactPolicyReason::CacheHit,
            input,
        };
    }
    if input.construction_bytes == 0 {
        return ArtifactPolicyEvidence {
            policy: DescriptorArtifactPolicy::EphemeralFused,
            reason: ArtifactPolicyReason::EmptyArtifact,
            input,
        };
    }
    if input.construction_bytes > input.budget_headroom_bytes
        && input.ephemeral_bytes < input.construction_bytes
        && input.ephemeral_bytes <= input.budget_headroom_bytes
    {
        return ArtifactPolicyEvidence {
            policy: DescriptorArtifactPolicy::EphemeralFused,
            reason: ArtifactPolicyReason::MemoryBudget,
            input,
        };
    }

    const LAUNCH_US: u128 = 50;
    let reuses = u128::from(input.expected_reuses.max(1));
    let risk = u128::from(input.invalidation_risk_ppm.min(1_000_000));
    let rebuilds_ppm = 1_000_000_u128.saturating_add(reuses.saturating_sub(1).saturating_mul(risk));
    let construction = u128::from(input.construction_time_us)
        .saturating_add(u128::from(input.cached_build_launches).saturating_mul(LAUNCH_US));
    let cached_cost = construction
        .saturating_mul(rebuilds_ppm)
        .saturating_div(1_000_000)
        .saturating_add(
            reuses
                .saturating_mul(u128::from(input.consumer_launches))
                .saturating_mul(LAUNCH_US),
        )
        // Retained bytes carry a bounded pressure penalty as headroom shrinks.
        .saturating_add(
            u128::from(input.construction_bytes)
                .saturating_mul(50)
                .saturating_div(u128::from(input.budget_headroom_bytes.max(1))),
        );
    let fused_compute = u128::from(input.construction_time_us);
    let ephemeral_cost = reuses.saturating_mul(
        fused_compute
            .saturating_add(u128::from(input.ephemeral_build_launches).saturating_mul(LAUNCH_US))
            .saturating_add(u128::from(input.consumer_launches).saturating_mul(LAUNCH_US)),
    );
    ArtifactPolicyEvidence {
        policy: if ephemeral_cost <= cached_cost {
            DescriptorArtifactPolicy::EphemeralFused
        } else {
            DescriptorArtifactPolicy::CachedReusable
        },
        reason: ArtifactPolicyReason::LowerExpectedCost,
        input,
    }
}

#[cfg(test)]
pub(super) fn test_artifact_policy_evidence() -> ArtifactPolicyEvidence {
    choose_artifact_policy(ArtifactPolicyInput {
        construction_bytes: 1,
        ephemeral_bytes: 1,
        construction_time_us: 1,
        expected_reuses: 2,
        cache_hit: true,
        invalidation_risk_ppm: 0,
        cached_build_launches: 0,
        ephemeral_build_launches: 0,
        consumer_launches: 1,
        budget_headroom_bytes: 1_024,
    })
}

enum EphemeralDescriptorArtifact {
    Dense(EphemeralDerivedArtifact<DescriptorAggArtifact>),
    Hash(EphemeralDerivedArtifact<FusedHashArtifact>),
    Spatial(EphemeralDerivedArtifact<SpatialAggArtifact>),
}

struct FusedHashArtifact {
    resolved_spec: AggQuerySpec,
    fact_rows: usize,
    group_capacity: Cell<usize>,
    // H3 parent resolution; canonical zero for an untransformed BIGINT key.
    resolution: i32,
}

impl FusedHashArtifact {
    /// Call only after full compact validation/materialization setup succeeds,
    /// while the dependency-stamped residency borrow still pins every input.
    fn record_validated_raw_groups(&self, emitted: usize) -> Result<(), String> {
        if raw_hash_group(&self.resolved_spec).is_some() {
            // Compact validation has already proved that zero groups imply
            // zero selected rows. WHERE can reject a nonempty resident input;
            // its dependency stamp also invalidates this learned empty bound.
            let rows = if emitted == 0 && hash_where_filter(&self.resolved_spec)?.is_some() {
                0
            } else {
                self.fact_rows
            };
            let capacity = learned_hash_group_bound(rows, self.group_capacity.get(), emitted)?;
            self.group_capacity.set(capacity);
        }
        Ok(())
    }
}

impl DerivedArtifact for FusedHashArtifact {
    fn device_bytes(&self) -> u64 {
        0
    }

    fn retained_host_exact_bytes(&self) -> u64 {
        0
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn descriptor_artifact_identity(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
    catalog_fingerprint: &[i32],
) -> Result<DerivedArtifactIdentity, String> {
    let mut identity_words = spec
        .encode_i32()
        .map_err(|error| format!("could not encode aggregate query spec: {error}"))?;
    identity_words.extend(
        projection
            .encode_i32(spec)
            .map_err(|error| format!("could not encode aggregate projection: {error}"))?,
    );
    identity_words.extend_from_slice(catalog_fingerprint);
    Ok(DerivedArtifactIdentity::from_canonical_words(
        identity_words,
    ))
}

fn spatial_artifact_kind(
    spec: &AggQuerySpec,
    catalog: &PostgisCatalogIdentity,
    max_groups: usize,
) -> Result<DescriptorArtifactKind, String> {
    Ok(DescriptorArtifactKind::Spatial(SpatialTransformPlan::new(
        spec, catalog, max_groups,
    )?))
}

fn verify_catalog_before<T>(
    verify_catalog: impl FnOnce() -> Result<(), ResidentLoadError>,
    operation: impl FnOnce() -> Result<T, ResidentLoadError>,
) -> Result<T, ResidentLoadError> {
    verify_catalog()?;
    operation()
}

fn artifact_failure_allows_single_rebuild(error: &ResidentLoadError) -> bool {
    matches!(
        error,
        ResidentLoadError::MissingRelation(_)
            | ResidentLoadError::MissingColumn { .. }
            | ResidentLoadError::ArtifactDependencyChanged { .. }
            | ResidentLoadError::ArtifactNotFound { .. }
    )
}

fn execute_with_single_artifact_rebuild<T, R, E>(
    mut execute_once: impl FnMut() -> Result<Result<T, E>, ResidentLoadError>,
    rebuild: impl FnOnce() -> Result<R, E>,
) -> Result<(T, Option<R>), E>
where
    E: From<ResidentLoadError>,
{
    match execute_once() {
        Ok(result) => result.map(|result| (result, None)),
        Err(error) if artifact_failure_allows_single_rebuild(&error) => {
            let residency = rebuild()?;
            let result = execute_once().map_err(E::from)??;
            Ok((result, Some(residency)))
        }
        Err(error) => Err(E::from(error)),
    }
}

pub(super) struct DescriptorAggPlan {
    spec: AggQuerySpec,
    projection: AggOutputProjection,
    planned_kernel_mode: GroupedAggKernelMode,
    specialization: DescriptorSpecialization,
    specialization_cache_outcome: DescriptorSpecializationCacheOutcome,
    identity: DerivedArtifactIdentity,
    selected: Vec<SelectedRelation>,
    dependencies: Vec<pg_sys::Oid>,
    artifact_columns: Vec<ResidentColumnRef>,
    dispatch_columns: Vec<ResidentColumnRef>,
    max_groups: usize,
    dispatch_chunk_rows: usize,
    grouped_agg_one_shot_max_rows: Rows,
    artifact_kind: DescriptorArtifactKind,
    artifact_policy: Cell<DescriptorArtifactPolicy>,
    artifact_policy_evidence: Cell<Option<ArtifactPolicyEvidence>>,
    ephemeral_artifact: RefCell<Option<EphemeralDescriptorArtifact>>,
    #[cfg(feature = "pg_test")]
    qualification_policy: Option<DescriptorArtifactPolicy>,
}

pub(super) struct DescriptorResidencyReport {
    pub(super) artifact_outcome: ArtifactEnsureOutcome,
    pub(super) artifact_policy: ArtifactPolicyEvidence,
    pub(super) relations: Vec<ResidentRelationEvidence>,
    pub(super) loaded_relations: Vec<pg_sys::Oid>,
    pub(super) artifact_bytes: u64,
    pub(super) raw_load_ms: f64,
    pub(super) preparation_time_us: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DescriptorAggExecutionError {
    NumericOverflow,
    DivisionByZero,
    DateOutOfRange,
    ExternalRoutineException(String),
    Failure(String),
}

impl fmt::Display for DescriptorAggExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NumericOverflow => formatter.write_str("numeric value out of range"),
            Self::DivisionByZero => formatter.write_str("division by zero"),
            Self::DateOutOfRange => formatter.write_str("date out of range"),
            Self::ExternalRoutineException(message) | Self::Failure(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl From<String> for DescriptorAggExecutionError {
    fn from(message: String) -> Self {
        Self::Failure(message)
    }
}

impl From<ResidentLoadError> for DescriptorAggExecutionError {
    fn from(error: ResidentLoadError) -> Self {
        match error {
            ResidentLoadError::Gpu(error)
                if error.domain == GpuErrorDomain::H3
                    && error.operation == GpuOperation::Kernel("h3_cell_to_parent_resident")
                    && matches!(
                        error.status,
                        GpuStatusDetail::InvalidArgument | GpuStatusDetail::ShapeMismatch
                    ) =>
            {
                Self::ExternalRoutineException(error.to_string())
            }
            error => Self::Failure(error.to_string()),
        }
    }
}

fn gpu_execution_error(context: &'static str, error: GpuError) -> DescriptorAggExecutionError {
    if error.status == GpuStatusDetail::DateOutOfRange {
        DescriptorAggExecutionError::DateOutOfRange
    } else if error.status == GpuStatusDetail::DivisionByZero {
        DescriptorAggExecutionError::DivisionByZero
    } else if error.status == GpuStatusDetail::NumericOverflow {
        DescriptorAggExecutionError::NumericOverflow
    } else {
        DescriptorAggExecutionError::Failure(format!("{context}: {error}"))
    }
}

impl DescriptorResidencyReport {
    pub(super) fn merge(&mut self, latest: Self) {
        if latest.artifact_outcome != ArtifactEnsureOutcome::Hit {
            self.artifact_outcome = latest.artifact_outcome;
        }
        self.relations = latest.relations;
        self.artifact_policy = latest.artifact_policy;
        for relid in latest.loaded_relations {
            if !self.loaded_relations.contains(&relid) {
                self.loaded_relations.push(relid);
            }
        }
        self.artifact_bytes = latest.artifact_bytes;
        self.raw_load_ms += latest.raw_load_ms;
        self.preparation_time_us = self
            .preparation_time_us
            .saturating_add(latest.preparation_time_us);
    }
}

fn h3_group_capacity(
    fact_rows: usize,
    resolution: i32,
    max_groups: usize,
) -> Result<usize, String> {
    let resolution = u32::try_from(resolution)
        .ok()
        .filter(|resolution| *resolution <= 15)
        .ok_or_else(|| "H3 parent resolution is outside 0..=15".to_owned())?;
    let possible_nonnull = 7_usize
        .checked_pow(resolution)
        .and_then(|cells| cells.checked_mul(120))
        .and_then(|cells| cells.checked_add(2))
        .ok_or_else(|| "H3 group bound overflows usize".to_owned())?;
    let possible_with_null = possible_nonnull
        .checked_add(1)
        .ok_or_else(|| "H3 NULL group bound overflows usize".to_owned())?;
    let kernel_max_groups = 1_usize << 30;
    let capacity = fact_rows.max(1).min(possible_with_null);
    if capacity > max_groups.min(kernel_max_groups) {
        return Err("H3 group upper bound exceeds the bounded workspace capacity".to_owned());
    }
    Ok(capacity)
}

fn raw_hash_group_capacity(rows: usize, max_groups: usize) -> Result<usize, String> {
    if rows > u32::MAX as usize || max_groups == 0 {
        return Err("raw hash rows/capacity exceed the native domain".into());
    }
    // Capacity is a bounded allocation budget, not a distinct-count estimate.
    // The native insertion/finalization gates reject overflow before publishing
    // output. Thus many rows with few groups need not allocate one slot per row.
    Ok(rows.max(1).min(max_groups).min(1 << 30))
}

fn selected_fact_rows(
    selected: &[ResidentRelationEvidence],
    fact_rel: u32,
) -> Result<usize, String> {
    let rows = selected
        .iter()
        .find(|evidence| u32::from(evidence.relid) == fact_rel)
        .ok_or_else(|| format!("resident evidence is missing fact relation OID {fact_rel}"))?
        .row_count;
    usize::try_from(rows).map_err(|_| format!("fact row count {rows} exceeds usize"))
}

fn h3_mask_work(spec: &AggQuerySpec, rows: usize) -> Result<(u64, u64), String> {
    let masks =
        u64::from(h3_count_filter(spec)?.is_some()) + u64::from(hash_where_filter(spec)?.is_some());
    Ok((
        u64::try_from(rows)
            .ok()
            .and_then(|rows| rows.checked_mul(masks))
            .ok_or_else(|| "hash mask bytes overflow".to_owned())?,
        u64::try_from(rows.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS))
            .ok()
            .and_then(|launches| launches.checked_mul(masks))
            .ok_or_else(|| "hash mask launch count overflow".to_owned())?,
    ))
}

fn descriptor_artifact_policy_input(
    plan: &DescriptorAggPlan,
    selected: &[ResidentRelationEvidence],
    cache_hit: bool,
) -> Result<ArtifactPolicyInput, String> {
    let fact_rows = selected_fact_rows(selected, plan.spec.fact_rel)?;
    let (mask_bytes, mask_launches) = if matches!(
        &plan.artifact_kind,
        DescriptorArtifactKind::H3Parent { .. } | DescriptorArtifactKind::RawHash { .. }
    ) {
        h3_mask_work(&plan.spec, fact_rows)?
    } else {
        (0, 0)
    };
    let relation_rows = selected
        .iter()
        .map(|relation| (u32::from(relation.relid), relation.row_count))
        .collect::<BTreeMap<_, _>>();
    let construction_bytes = match &plan.artifact_kind {
        DescriptorArtifactKind::RawHash { .. } => 0,
        DescriptorArtifactKind::H3Parent { .. } => h3_parent_artifact_bytes(&plan.spec, fact_rows)?,
        DescriptorArtifactKind::Dense | DescriptorArtifactKind::Spatial(_) => {
            estimate_descriptor_artifact_bytes_upper_bound(&plan.spec, &relation_rows)
                .ok_or_else(|| "descriptor construction-byte estimate is unavailable".to_owned())?
                .checked_add(descriptor_program_clone_bytes(&plan.spec)?)
                .ok_or_else(|| "descriptor construction-byte estimate overflow".to_owned())?
        }
    };
    let cached_build_launches = match &plan.artifact_kind {
        DescriptorArtifactKind::H3Parent { max_chunk_rows, .. } => {
            if *max_chunk_rows == 0 {
                return Err("H3 construction chunk limit is zero".to_owned());
            }
            u64::try_from(fact_rows.div_ceil(*max_chunk_rows))
                .unwrap_or(u64::MAX)
                .saturating_add(mask_launches)
        }
        DescriptorArtifactKind::Spatial(spatial) => {
            let chunks = fact_rows.div_ceil(spatial.chunk_rows());
            // Candidate evaluation, uncertainty compaction, and the possible
            // exact-result patch are distinct observable submissions.
            u64::try_from(chunks.saturating_mul(3)).unwrap_or(u64::MAX)
        }
        DescriptorArtifactKind::Dense | DescriptorArtifactKind::RawHash { .. } => 0,
    };
    let ephemeral_build_launches = match &plan.artifact_kind {
        DescriptorArtifactKind::H3Parent { .. } | DescriptorArtifactKind::RawHash { .. } => {
            mask_launches
        }
        DescriptorArtifactKind::Dense | DescriptorArtifactKind::Spatial(_) => cached_build_launches,
    };
    let ephemeral_bytes = match &plan.artifact_kind {
        DescriptorArtifactKind::H3Parent { .. } | DescriptorArtifactKind::RawHash { .. } => {
            mask_bytes
        }
        DescriptorArtifactKind::Dense | DescriptorArtifactKind::Spatial(_) => construction_bytes,
    };
    let consumer_launches = match &plan.artifact_kind {
        DescriptorArtifactKind::Spatial(_)
            if plan.planned_kernel_mode == GroupedAggKernelMode::ParallelDenseNumeric
                || canonical_dense_integer_rollup(&plan.spec)
                || canonical_dense_scalar_programs(&plan.spec) =>
        {
            bounded_dispatch_call_count(fact_rows, plan.dispatch_chunk_rows)
                .and_then(|calls| u64::try_from(calls).ok())
                .ok_or_else(|| "spatial rollup lifecycle-call estimate overflow".to_owned())?
        }
        DescriptorArtifactKind::H3Parent { .. }
        | DescriptorArtifactKind::RawHash { .. }
        | DescriptorArtifactKind::Spatial(_) => 1,
        DescriptorArtifactKind::Dense if fact_rows <= plan.grouped_agg_one_shot_max_rows.get() => 1,
        DescriptorArtifactKind::Dense => {
            bounded_dispatch_call_count(fact_rows, plan.dispatch_chunk_rows)
                .and_then(|calls| u64::try_from(calls).ok())
                .ok_or_else(|| "dense lifecycle-call estimate overflow".to_owned())?
        }
    };
    // Twelve GB/s is deliberately conservative for a transform that reads and
    // writes fixed-width resident lanes. Spatial exact recheck also receives a
    // bounded host-call allowance so its construction-time input is never
    // represented as a byte-copy-only estimate.
    let mut construction_time_us = construction_bytes.div_ceil(12_000).max(1);
    if matches!(&plan.artifact_kind, DescriptorArtifactKind::Spatial(_)) {
        construction_time_us = construction_time_us
            .saturating_add(u64::try_from(fact_rows.div_ceil(1_000)).unwrap_or(u64::MAX));
    }
    let invalidation_risk_ppm = u32::try_from(plan.dependencies.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(50_000)
        .min(750_000);
    let budget_headroom_bytes =
        gucs::resident_memory_budget_bytes().saturating_sub(resident_live_bytes());
    Ok(ArtifactPolicyInput {
        construction_bytes,
        ephemeral_bytes,
        construction_time_us,
        expected_reuses: crate::engine::cost::device_limits()
            .auto_load_amortization_queries
            .max(1),
        cache_hit,
        invalidation_risk_ppm,
        cached_build_launches,
        ephemeral_build_launches,
        consumer_launches,
        budget_headroom_bytes,
    })
}

impl DescriptorAggPlan {
    pub(super) fn new(spec: AggQuerySpec, projection: AggOutputProjection) -> Result<Self, String> {
        validate_internal_runtime_capability(&spec, &projection)?;
        let planned_kernel_mode =
            planned_descriptor_kernel_mode(&spec, h3_parent_group(&spec).is_some());
        Self::from_validated_spec(spec, projection, planned_kernel_mode)
    }

    // Capability validation stays at the entry point. The only other caller is
    // the pg_test-only compact-hash qualification module below.
    fn from_validated_spec(
        spec: AggQuerySpec,
        projection: AggOutputProjection,
        planned_kernel_mode: GroupedAggKernelMode,
    ) -> Result<Self, String> {
        validate_catalog_contract(&spec, &projection)?;
        let limits = crate::engine::cost::device_limits();
        let executor_limits = ExecutorLimits::from(crate::engine::cost::execution_device_limits());
        let (artifact_kind, catalog_fingerprint) =
            if let Some((cell, resolution, result_type_oid)) = h3_parent_group(&spec) {
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }?;
                if u32::from(catalog.type_oid) != cell.type_oid || cell.type_oid != result_type_oid
                {
                    return Err("H3 catalog identity changed after validation".to_owned());
                }
                (
                    DescriptorArtifactKind::H3Parent {
                        cell,
                        resolution,
                        max_chunk_rows: limits.gpu_h3_max_chunk_rows,
                    },
                    catalog.fingerprint_words,
                )
            } else if planned_kernel_mode == GroupedAggKernelMode::ParallelHash {
                let column = raw_hash_group(&spec)
                    .ok_or("raw hash requires one canonical integer fact key")?;
                HashMeasureInput::from_spec(&spec)?;
                (DescriptorArtifactKind::RawHash { column }, Vec::new())
            } else if spec.fact_filter.spatial().is_some() {
                // SAFETY: executor Begin runs on PostgreSQL's main backend thread.
                let catalog = unsafe { resolve_postgis_catalog() }?;
                let artifact_kind =
                    spatial_artifact_kind(&spec, &catalog, limits.gpu_hash_agg_max_groups)?;
                (artifact_kind, catalog.fingerprint_words)
            } else {
                (DescriptorArtifactKind::Dense, Vec::new())
            };
        let identity = descriptor_artifact_identity(&spec, &projection, &catalog_fingerprint)?;
        let (specialization, specialization_cache_outcome) =
            cached_descriptor_specialization(&identity, &spec, planned_kernel_mode);
        let selected = selected_relations(&spec)?;
        let dependencies = dependency_relids(&spec);
        let artifact_columns = artifact_column_refs(&spec)?;
        if let DescriptorArtifactKind::Spatial(plan) = &artifact_kind
            && !artifact_columns.contains(&plan.column_request())
        {
            return Err("spatial source column is missing from artifact requests".to_owned());
        }
        let dispatch_columns = if planned_kernel_mode == GroupedAggKernelMode::ParallelHash {
            compact_hash_column_refs(&spec)?
        } else {
            dispatch_column_refs(&spec)?
        };
        let dispatch_chunk_rows = if h3_parent_group(&spec).is_some() {
            executor_limits.gpu_h3_max_chunk_rows.get()
        } else {
            dense_session_chunk_rows(
                executor_limits.gpu_reduce_max_chunk.get(),
                executor_limits.gpu_grouped_agg_one_shot_max_rows.get(),
            )
        };
        let max_groups = if h3_parent_group(&spec).is_some() {
            limits.gpu_h3_max_groups
        } else {
            limits.gpu_hash_agg_max_groups
        };
        Ok(Self {
            spec,
            projection,
            planned_kernel_mode,
            specialization,
            specialization_cache_outcome,
            identity,
            selected,
            dependencies,
            artifact_columns,
            dispatch_columns,
            max_groups,
            dispatch_chunk_rows,
            grouped_agg_one_shot_max_rows: executor_limits.gpu_grouped_agg_one_shot_max_rows,
            artifact_kind,
            artifact_policy: Cell::new(DescriptorArtifactPolicy::CachedReusable),
            artifact_policy_evidence: Cell::new(None),
            ephemeral_artifact: RefCell::new(None),
            #[cfg(feature = "pg_test")]
            qualification_policy: None,
        })
    }

    pub(super) fn spec(&self) -> &AggQuerySpec {
        &self.spec
    }

    pub(super) fn projection(&self) -> &AggOutputProjection {
        &self.projection
    }

    pub(super) const fn planned_kernel_mode(&self) -> GroupedAggKernelMode {
        self.planned_kernel_mode
    }

    pub(super) const fn specialization(&self) -> DescriptorSpecialization {
        self.specialization
    }

    pub(super) const fn specialization_cache_outcome(
        &self,
    ) -> DescriptorSpecializationCacheOutcome {
        self.specialization_cache_outcome
    }

    pub(super) fn artifact_policy_evidence(&self) -> Option<ArtifactPolicyEvidence> {
        self.artifact_policy_evidence.get()
    }

    pub(super) fn ensure_artifact(
        &self,
    ) -> Result<DescriptorResidencyReport, DescriptorAggExecutionError> {
        self.ensure_artifact_with_poll(postgres_interrupt_pending)
    }

    // The injected callback only polls signals; PostgreSQL delivery remains
    // outside every residency/native frame in run_interruptible_dispatch.
    fn ensure_artifact_with_poll(
        &self,
        pending: impl FnMut() -> bool,
    ) -> Result<DescriptorResidencyReport, DescriptorAggExecutionError> {
        let preparation_started = Instant::now();
        let selected = match &self.artifact_kind {
            DescriptorArtifactKind::Spatial(plan) => verify_catalog_before(
                || plan.verify_catalog(),
                || ensure_selected_relations(&self.selected),
            )?,
            DescriptorArtifactKind::Dense
            | DescriptorArtifactKind::H3Parent { .. }
            | DescriptorArtifactKind::RawHash { .. } => ensure_selected_relations(&self.selected)?,
        };
        let owner_relid = pg_sys::Oid::from(self.spec.fact_rel);
        let fact_rows = selected_fact_rows(&selected.evidence, self.spec.fact_rel)?;
        if matches!(
            &self.artifact_kind,
            DescriptorArtifactKind::H3Parent { .. } | DescriptorArtifactKind::RawHash { .. }
        ) && fact_rows > u32::MAX as usize
        {
            return Err(format!(
                "H3 grouped aggregation row count {fact_rows} exceeds the kernel u32 domain"
            )
            .into());
        }
        let h3_accounting = ResidentByteAccounting {
            device_bytes: if matches!(&self.artifact_kind, DescriptorArtifactKind::H3Parent { .. })
            {
                h3_parent_artifact_bytes(&self.spec, fact_rows)?
            } else {
                0
            },
            retained_host_exact_bytes: 0,
        };
        let probe = match &self.artifact_kind {
            DescriptorArtifactKind::RawHash { .. } => ArtifactProbeOutcome::Miss,
            DescriptorArtifactKind::Dense => probe_derived_artifact::<DescriptorAggArtifact>(
                owner_relid,
                &self.identity,
                &self.dependencies,
                &self.artifact_columns,
                None,
            )?,
            DescriptorArtifactKind::H3Parent { .. } => probe_derived_artifact::<H3ParentArtifact>(
                owner_relid,
                &self.identity,
                &self.dependencies,
                &self.artifact_columns,
                Some(h3_accounting),
            )?,
            DescriptorArtifactKind::Spatial(_) => probe_derived_artifact::<SpatialAggArtifact>(
                owner_relid,
                &self.identity,
                &self.dependencies,
                &self.artifact_columns,
                None,
            )?,
        };
        let stale = matches!(probe, ArtifactProbeOutcome::Stale);
        let hit_report = match probe {
            ArtifactProbeOutcome::Hit(report) => Some(report),
            ArtifactProbeOutcome::Miss | ArtifactProbeOutcome::Stale => None,
        };
        let policy = choose_artifact_policy(descriptor_artifact_policy_input(
            self,
            &selected.evidence,
            hit_report.is_some(),
        )?);
        #[cfg(feature = "pg_test")]
        let policy = if let Some(forced) = self.qualification_policy {
            ArtifactPolicyEvidence {
                policy: forced,
                reason: ArtifactPolicyReason::QualificationOverride,
                ..policy
            }
        } else {
            policy
        };
        // Raw keys already belong to residency: no derived dictionary/parent
        // lane exists to cache. FILTER masks remain charged query-local state.
        let policy = if matches!(&self.artifact_kind, DescriptorArtifactKind::RawHash { .. }) {
            ArtifactPolicyEvidence {
                policy: DescriptorArtifactPolicy::EphemeralFused,
                reason: ArtifactPolicyReason::EmptyArtifact,
                ..policy
            }
        } else if matches!(&self.artifact_kind, DescriptorArtifactKind::H3Parent { .. })
            && hash_where_filter(&self.spec)?.is_some()
        {
            ArtifactPolicyEvidence {
                policy: DescriptorArtifactPolicy::EphemeralFused,
                reason: ArtifactPolicyReason::PredicateOrder,
                ..policy
            }
        } else {
            policy
        };
        self.artifact_policy.set(policy.policy);
        self.artifact_policy_evidence.set(Some(policy));
        self.ephemeral_artifact.borrow_mut().take();

        let hit_report =
            hit_report.filter(|_| policy.policy == DescriptorArtifactPolicy::CachedReusable);
        let (artifact_outcome, relations, artifact_bytes) = if let Some(report) = hit_report {
            (
                report.outcome,
                report.evidence,
                report.accounting.device_bytes,
            )
        } else if policy.policy == DescriptorArtifactPolicy::EphemeralFused {
            match &self.artifact_kind {
                DescriptorArtifactKind::Dense => {
                    let report = build_ephemeral_host_device_derived_artifact_with_report(
                        owner_relid,
                        &self.dependencies,
                        &self.artifact_columns,
                        descriptor_program_clone_bytes(&self.spec)
                            .map_err(ResidentLoadError::Loader)?,
                        |bundle| {
                            prepare_agg_artifact(
                                &self.spec,
                                &self.artifact_columns,
                                bundle,
                                self.max_groups,
                            )
                        },
                        DescriptorAggArtifact::build,
                    )?;
                    let outcome = if stale {
                        ArtifactEnsureOutcome::Rebuilt
                    } else {
                        ArtifactEnsureOutcome::Built
                    };
                    let bytes = report.accounting.device_bytes;
                    let evidence = report.evidence;
                    self.ephemeral_artifact
                        .borrow_mut()
                        .replace(EphemeralDescriptorArtifact::Dense(report.artifact));
                    (outcome, evidence, bytes)
                }
                DescriptorArtifactKind::H3Parent { .. }
                | DescriptorArtifactKind::RawHash { .. } => {
                    let (group_capacity, resolution) = match &self.artifact_kind {
                        DescriptorArtifactKind::H3Parent { resolution, .. } => (
                            h3_group_capacity(fact_rows, *resolution, self.max_groups)?,
                            *resolution,
                        ),
                        DescriptorArtifactKind::RawHash { .. } => {
                            (raw_hash_group_capacity(fact_rows, self.max_groups)?, 0)
                        }
                        _ => unreachable!("matched compact hash artifact"),
                    };
                    let report = build_ephemeral_derived_artifact_with_report(
                        owner_relid,
                        &self.dependencies,
                        &self.artifact_columns,
                        |_bundle| {
                            Ok(PreparedDerived {
                                prepared: FusedHashArtifact {
                                    resolved_spec: self.spec.clone(),
                                    fact_rows,
                                    group_capacity: Cell::new(group_capacity),
                                    resolution,
                                },
                                device_bytes: 0,
                            })
                        },
                        Ok,
                    )?;
                    self.ephemeral_artifact
                        .borrow_mut()
                        .replace(EphemeralDescriptorArtifact::Hash(report.artifact));
                    (
                        if stale {
                            ArtifactEnsureOutcome::Rebuilt
                        } else {
                            ArtifactEnsureOutcome::Built
                        },
                        report.evidence,
                        report.accounting.device_bytes,
                    )
                }
                DescriptorArtifactKind::Spatial(plan) => {
                    let report = verify_catalog_before(
                        || plan.verify_catalog(),
                        || {
                            build_ephemeral_staged_device_transform_artifact_with_report(
                                owner_relid,
                                &self.dependencies,
                                &self.artifact_columns,
                                |bundle| plan.preflight(&self.spec, &self.artifact_columns, bundle),
                                |preflight, bundle| {
                                    plan.snapshot_prepare(
                                        &self.spec,
                                        &self.artifact_columns,
                                        preflight,
                                        bundle,
                                    )
                                },
                                |snapshot| SpatialWorkspace::build(plan, &self.spec, snapshot),
                                |workspace, bundle| {
                                    workspace.launch(plan, bundle);
                                    Ok(())
                                },
                                |workspace| workspace.finalize(plan, &self.spec),
                            )
                        },
                    )?;
                    let outcome = if stale {
                        ArtifactEnsureOutcome::Rebuilt
                    } else {
                        ArtifactEnsureOutcome::Built
                    };
                    let bytes = report.accounting.device_bytes;
                    let evidence = report.evidence;
                    self.ephemeral_artifact
                        .borrow_mut()
                        .replace(EphemeralDescriptorArtifact::Spatial(report.artifact));
                    (outcome, evidence, bytes)
                }
            }
        } else {
            let report = match &self.artifact_kind {
                DescriptorArtifactKind::RawHash { .. } => {
                    return Err("raw hash artifacts must remain query-owned"
                        .to_owned()
                        .into());
                }
                DescriptorArtifactKind::Dense => ensure_host_device_derived_artifact_with_report(
                    owner_relid,
                    &self.identity,
                    &self.dependencies,
                    &self.artifact_columns,
                    descriptor_program_clone_bytes(&self.spec)
                        .map_err(ResidentLoadError::Loader)?,
                    |bundle| {
                        prepare_agg_artifact(
                            &self.spec,
                            &self.artifact_columns,
                            bundle,
                            self.max_groups,
                        )
                    },
                    DescriptorAggArtifact::build,
                )?,
                DescriptorArtifactKind::H3Parent {
                    cell,
                    resolution,
                    max_chunk_rows,
                } => {
                    let group_capacity =
                        h3_group_capacity(fact_rows, *resolution, self.max_groups)?;
                    let result = run_interruptible_dispatch(
                        |pending| match ensure_device_derived_artifact_with_report(
                            owner_relid,
                            &self.identity,
                            &self.dependencies,
                            &self.artifact_columns,
                            h3_accounting,
                            |bundle| {
                                let view =
                                    find_view(&self.artifact_columns, &bundle.columns, *cell)?;
                                let (cells, nulls) = match view {
                                    ResidentColumnView::Empty { type_oid }
                                        if fact_rows == 0
                                            && u32::from(*type_oid) == cell.type_oid =>
                                    {
                                        (std::ptr::null(), std::ptr::null())
                                    }
                                    ResidentColumnView::H3 {
                                        type_oid,
                                        values,
                                        nulls,
                                    } if u32::from(*type_oid) == cell.type_oid
                                        && values.len() == fact_rows =>
                                    {
                                        validate_h3_input_lengths(
                                            values.len(),
                                            nulls.map(crate::gpu::ExprDeviceBuffer::len),
                                            fact_rows,
                                        )?;
                                        (values.as_ptr(), null_ptr(*nulls))
                                    }
                                    _ => {
                                        return Err(ResidentLoadError::Loader(
                                        "resident H3 source does not match the planned raw lane"
                                            .to_owned(),
                                    ));
                                    }
                                };
                                let parents = if fact_rows == 0 {
                                    None
                                } else {
                                    Some(
                                        crate::gpu::ExprDeviceBuffer::<u64>::new(fact_rows)
                                            .ok_or_else(|| {
                                                "could not allocate derived H3 parent device lane"
                                                    .to_owned()
                                            })?,
                                    )
                                };
                                if fact_rows != 0 {
                                    if *max_chunk_rows == 0 {
                                        return Err(ResidentLoadError::Loader(
                                            "H3 device chunk limit is zero".to_owned(),
                                        ));
                                    }
                                    let output = parents.as_ref().ok_or_else(|| {
                                        "nonempty H3 parent artifact has no output".to_owned()
                                    })?;
                                    for offset in (0..fact_rows).step_by(*max_chunk_rows) {
                                        if pending() {
                                            return Err(ResidentLoadError::PreparationInterrupted);
                                        }
                                        let count = (fact_rows - offset).min(*max_chunk_rows);
                                        // SAFETY: matched buffers span fact_rows and this chunk.
                                        let chunk_cells = unsafe { cells.add(offset) };
                                        let chunk_nulls = if nulls.is_null() {
                                            std::ptr::null()
                                        } else {
                                            // SAFETY: one NULL byte exists for every source row.
                                            unsafe { nulls.add(offset) }
                                        };
                                        // SAFETY: output has fact_rows elements.
                                        let chunk_parents =
                                            unsafe { output.as_mut_ptr().add(offset) };
                                        // SAFETY: pointers remain pinned for this synchronous call.
                                        unsafe {
                                            crate::gpu::h3_cell_to_parent_resident(
                                                chunk_cells,
                                                chunk_nulls,
                                                count,
                                                *resolution,
                                                chunk_parents,
                                            )
                                        }
                                        .map_err(ResidentLoadError::Gpu)?;
                                    }
                                }
                                if pending() {
                                    return Err(ResidentLoadError::PreparationInterrupted);
                                }
                                Ok(H3ParentArtifact::new(
                                    self.spec.clone(),
                                    fact_rows,
                                    group_capacity,
                                    parents,
                                    build_cached_h3_filter(
                                        &self.spec,
                                        &self.artifact_columns,
                                        &bundle.columns,
                                        fact_rows,
                                        pending,
                                    )?,
                                )?)
                            },
                        ) {
                            Ok(report) => Ok(InterruptibleAttempt::Complete(report)),
                            Err(ResidentLoadError::PreparationInterrupted) => {
                                Ok(InterruptibleAttempt::Interrupted)
                            }
                            Err(error) => Err(error),
                        },
                        pending,
                        || {
                            capture_postgres_interrupt()
                                .map_or(Ok(()), |error| Err(Box::new(error)))
                        },
                    );
                    match result {
                        Ok(report) => report,
                        Err(InterruptibleDispatchError::Dispatch(error)) => {
                            return Err(error.into());
                        }
                        Err(InterruptibleDispatchError::Interrupt(error)) => (*error).rethrow(),
                    }
                }
                DescriptorArtifactKind::Spatial(plan) => verify_catalog_before(
                    || plan.verify_catalog(),
                    || {
                        ensure_staged_device_transform_artifact_with_report(
                            owner_relid,
                            &self.identity,
                            &self.dependencies,
                            &self.artifact_columns,
                            |bundle| plan.preflight(&self.spec, &self.artifact_columns, bundle),
                            |preflight, bundle| {
                                plan.snapshot_prepare(
                                    &self.spec,
                                    &self.artifact_columns,
                                    preflight,
                                    bundle,
                                )
                            },
                            |snapshot| SpatialWorkspace::build(plan, &self.spec, snapshot),
                            |workspace, bundle| {
                                workspace.launch(plan, bundle);
                                Ok(())
                            },
                            |workspace| workspace.finalize(plan, &self.spec),
                        )
                    },
                )?,
            };
            (
                if stale {
                    ArtifactEnsureOutcome::Rebuilt
                } else {
                    report.outcome
                },
                report.evidence,
                report.accounting.device_bytes,
            )
        };
        Ok(DescriptorResidencyReport {
            artifact_outcome,
            artifact_policy: policy,
            relations,
            loaded_relations: selected.loaded_relations,
            artifact_bytes,
            raw_load_ms: selected.raw_load_ms,
            preparation_time_us: u64::try_from(preparation_started.elapsed().as_micros())
                .unwrap_or(u64::MAX),
        })
    }

    fn execute_once(
        &self,
        pending: &mut dyn FnMut() -> bool,
    ) -> Result<Result<DescriptorAggDispatch, DescriptorAggExecutionError>, ResidentLoadError> {
        let owner_relid = pg_sys::Oid::from(self.spec.fact_rel);
        let result: Result<DescriptorAggDispatch, DescriptorDispatchFailure> =
            if self.artifact_policy.get() == DescriptorArtifactPolicy::CachedReusable {
                match &self.artifact_kind {
                    DescriptorArtifactKind::RawHash { .. } => Err(
                        DescriptorDispatchFailure::Execution(DescriptorAggExecutionError::Failure(
                            "raw hash artifacts cannot use the derived LRU".into(),
                        )),
                    ),
                    DescriptorArtifactKind::Dense => execute_bounded_dense_artifact(
                        owner_relid,
                        &self.identity,
                        &self.dispatch_columns,
                        &self.projection,
                        self.dispatch_chunk_rows,
                        self.grouped_agg_one_shot_max_rows.get(),
                        self.planned_kernel_mode,
                    ),
                    DescriptorArtifactKind::H3Parent { cell, .. } => {
                        match with_derived_artifact_inputs::<H3ParentArtifact, _>(
                            owner_relid,
                            &self.identity,
                            &self.dispatch_columns,
                            |inputs| {
                                build_and_execute_h3_one_shot(
                                    inputs,
                                    &self.dispatch_columns,
                                    *cell,
                                    &self.projection,
                                    self.planned_kernel_mode,
                                )
                            },
                        ) {
                            Ok(dispatch) => dispatch.map_err(DescriptorDispatchFailure::Execution),
                            Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                        }
                    }
                    DescriptorArtifactKind::Spatial(_)
                        if self.planned_kernel_mode
                            == GroupedAggKernelMode::ParallelDenseNumeric
                            || canonical_dense_integer_rollup(&self.spec)
                            || canonical_dense_scalar_programs(&self.spec) =>
                    {
                        execute_bounded_dense_with_resolver(
                            &CachedSpatialArtifactResolver {
                                owner_relid,
                                identity: &self.identity,
                                requests: &self.dispatch_columns,
                            },
                            &self.dispatch_columns,
                            &self.projection,
                            self.dispatch_chunk_rows,
                            self.grouped_agg_one_shot_max_rows.get(),
                            self.planned_kernel_mode,
                        )
                    }
                    DescriptorArtifactKind::Spatial(_) => {
                        match with_derived_artifact_inputs::<SpatialAggArtifact, _>(
                            owner_relid,
                            &self.identity,
                            &self.dispatch_columns,
                            |inputs| {
                                build_and_execute_spatial(
                                    inputs,
                                    &self.dispatch_columns,
                                    &self.projection,
                                    self.planned_kernel_mode,
                                )
                            },
                        ) {
                            Ok(dispatch) => dispatch.map_err(DescriptorDispatchFailure::Execution),
                            Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                        }
                    }
                }
            } else {
                let ephemeral = self.ephemeral_artifact.borrow();
                match (&self.artifact_kind, ephemeral.as_ref()) {
                    (
                        DescriptorArtifactKind::Dense,
                        Some(EphemeralDescriptorArtifact::Dense(artifact)),
                    ) => execute_bounded_dense_ephemeral(
                        owner_relid,
                        artifact,
                        &self.dispatch_columns,
                        &self.projection,
                        self.dispatch_chunk_rows,
                        self.grouped_agg_one_shot_max_rows.get(),
                        self.planned_kernel_mode,
                    ),
                    (
                        DescriptorArtifactKind::H3Parent { cell, .. }
                        | DescriptorArtifactKind::RawHash { column: cell },
                        Some(EphemeralDescriptorArtifact::Hash(artifact)),
                    ) => match with_ephemeral_artifact_inputs(
                        owner_relid,
                        artifact,
                        &self.dispatch_columns,
                        |inputs| {
                            build_and_execute_fused_hash_one_shot(
                                inputs,
                                &self.dispatch_columns,
                                *cell,
                                &self.projection,
                                self.planned_kernel_mode,
                                pending,
                            )
                        },
                    ) {
                        Ok(dispatch) => dispatch,
                        Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                    },
                    (
                        DescriptorArtifactKind::Spatial(_),
                        Some(EphemeralDescriptorArtifact::Spatial(artifact)),
                    ) if self.planned_kernel_mode == GroupedAggKernelMode::ParallelDenseNumeric
                        || canonical_dense_integer_rollup(&self.spec)
                        || canonical_dense_scalar_programs(&self.spec) =>
                    {
                        execute_bounded_dense_with_resolver(
                            &EphemeralSpatialArtifactResolver {
                                owner_relid,
                                artifact,
                                requests: &self.dispatch_columns,
                            },
                            &self.dispatch_columns,
                            &self.projection,
                            self.dispatch_chunk_rows,
                            self.grouped_agg_one_shot_max_rows.get(),
                            self.planned_kernel_mode,
                        )
                    }
                    (
                        DescriptorArtifactKind::Spatial(_),
                        Some(EphemeralDescriptorArtifact::Spatial(artifact)),
                    ) => match with_ephemeral_artifact_inputs(
                        owner_relid,
                        artifact,
                        &self.dispatch_columns,
                        |inputs| {
                            build_and_execute_spatial(
                                inputs,
                                &self.dispatch_columns,
                                &self.projection,
                                self.planned_kernel_mode,
                            )
                        },
                    ) {
                        Ok(dispatch) => dispatch.map_err(DescriptorDispatchFailure::Execution),
                        Err(error) => Err(DescriptorDispatchFailure::Residency(error)),
                    },
                    _ => Err(DescriptorDispatchFailure::Execution(
                        DescriptorAggExecutionError::Failure(
                            "ephemeral descriptor artifact is missing or has the wrong kind"
                                .to_owned(),
                        ),
                    )),
                }
            };
        match result {
            Ok(dispatch) => Ok(Ok(dispatch)),
            Err(DescriptorDispatchFailure::Residency(error)) => Err(error),
            Err(DescriptorDispatchFailure::Execution(error)) => Ok(Err(error)),
        }
    }

    /// Execute after Begin or rescan established a current artifact.
    ///
    /// A generation change between preparation and first execution is still
    /// handled fail-closed: the pinned resolver evicts the stale artifact,
    /// this method rebuilds it, and exactly one retry receives fresh pointers.
    pub(super) fn execute_prepared(
        &self,
    ) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
        self.execute_prepared_with_poll(postgres_interrupt_pending)
    }

    fn execute_hash_once_interruptible(
        &self,
        pending: &mut dyn FnMut() -> bool,
    ) -> Result<Result<DescriptorAggDispatch, DescriptorAggExecutionError>, ResidentLoadError> {
        self.execute_hash_once_with_service(pending, || {
            capture_postgres_interrupt().map_or(Ok(()), |error| Err(Box::new(error)))
        })
    }

    fn execute_hash_once_with_service(
        &self,
        pending: &mut dyn FnMut() -> bool,
        service: impl FnMut() -> Result<(), Box<pg_sys::panic::CaughtError>>,
    ) -> Result<Result<DescriptorAggDispatch, DescriptorAggExecutionError>, ResidentLoadError> {
        if !matches!(
            self.artifact_kind,
            DescriptorArtifactKind::H3Parent { .. } | DescriptorArtifactKind::RawHash { .. }
        ) {
            return self.execute_once(pending);
        }
        // Retain only a scalar, never an owner/store borrow across PG service.
        // A fatal signal can arrive after the final poll and completed output
        // validation; that abandoned attempt must not publish learned capacity.
        let previous_raw_capacity =
            if matches!(self.artifact_kind, DescriptorArtifactKind::RawHash { .. }) {
                match self.ephemeral_artifact.borrow().as_ref() {
                    Some(EphemeralDescriptorArtifact::Hash(owner)) => {
                        Some(owner.artifact().group_capacity.get())
                    }
                    _ => None,
                }
            } else {
                None
            };
        let result = run_interruptible_dispatch(
            |pending| match self.execute_once(pending) {
                Ok(dispatch) => Ok(InterruptibleAttempt::Complete(dispatch)),
                Err(ResidentLoadError::PreparationInterrupted) => {
                    Ok(InterruptibleAttempt::Interrupted)
                }
                Err(error) => Err(error),
            },
            pending,
            service,
        );
        if !matches!(&result, Ok(Ok(_)))
            && let Some(previous) = previous_raw_capacity
            && let Some(EphemeralDescriptorArtifact::Hash(owner)) =
                self.ephemeral_artifact.borrow().as_ref()
        {
            owner.artifact().group_capacity.set(previous);
        }
        // execute_once returned: both the ephemeral owner RefCell guard and
        // resident input borrow are gone before PostgreSQL runs. On a fatal
        // interrupt the driver drops any completed output before rethrow too.
        match result {
            Ok(dispatch) => Ok(dispatch),
            Err(InterruptibleDispatchError::Dispatch(error)) => Err(error),
            Err(InterruptibleDispatchError::Interrupt(error)) => (*error).rethrow(),
        }
    }

    fn execute_prepared_with_poll(
        &self,
        mut pending: impl FnMut() -> bool,
    ) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
        // Spatial dispatch depends on resolved PostGIS function and type OIDs in
        // addition to resident relation generations. Revalidate that catalog
        // identity at the execution boundary without repeating artifact ensure.
        if let DescriptorArtifactKind::Spatial(plan) = &self.artifact_kind {
            plan.verify_catalog()?;
        }
        let (mut result, residency) = execute_with_single_artifact_rebuild(
            || self.execute_hash_once_interruptible(&mut pending),
            || self.ensure_artifact(),
        )?;
        if result.kernel_mode != self.planned_kernel_mode {
            return Err(DescriptorAggExecutionError::Failure(format!(
                "planned grouped physical mode {} does not match native dispatched mode {}",
                self.planned_kernel_mode.label(),
                result.kernel_mode.label()
            )));
        }
        result.residency = residency;
        Ok(result)
    }
}

pub(super) struct DescriptorAggDispatch {
    pub output: DescriptorAggOutput,
    pub fact_rows: usize,
    /// Lifecycle calls predicted from the exact execution branch before its
    /// first submission. Successful execution must complete this many calls.
    pub planned_batches: u64,
    pub batches_executed: u64,
    /// Data-bearing calls that used `kernel_mode`; lifecycle-only finalize or
    /// reset calls are deliberately excluded.
    pub physical_mode_calls: u64,
    pub dispatch_time_us: u64,
    pub kernel_mode: GroupedAggKernelMode,
    pub residency: Option<DescriptorResidencyReport>,
}

fn find_view<'a>(
    requests: &[ResidentColumnRef],
    views: &'a [ResidentColumnView<'a>],
    column: ColumnRef,
) -> Result<&'a ResidentColumnView<'a>, String> {
    let attno = i16::try_from(column.attno)
        .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
    let index = requests
        .iter()
        .position(|request| {
            u32::from(request.relid) == column.relation_oid && request.attno == attno
        })
        .ok_or_else(|| {
            format!(
                "descriptor did not request raw column ({}, {})",
                column.relation_oid, column.attno
            )
        })?;
    let view = views
        .get(index)
        .ok_or_else(|| "resident raw-column bundle changed shape".to_owned())?;
    if u32::from(view.type_oid()) != column.type_oid {
        return Err(format!(
            "resident raw column ({}, {}) type OID {} does not match planned OID {}",
            column.relation_oid,
            column.attno,
            u32::from(view.type_oid()),
            column.type_oid
        ));
    }
    Ok(view)
}

fn null_ptr(nulls: Option<&crate::gpu::ExprDeviceBuffer<u8>>) -> *const u8 {
    nulls.map_or(std::ptr::null(), |buffer| buffer.as_ptr())
}

fn measure_column(
    view: &ResidentColumnView<'_>,
    expected_rows: usize,
) -> Result<abi::PgaccelGroupedAggMeasureCol, String> {
    if view.len() != expected_rows {
        return Err(format!(
            "resident measure has {} rows, fact artifact has {expected_rows}",
            view.len()
        ));
    }
    let (values, nulls, physical_type, element_bytes) = match view {
        ResidentColumnView::Empty { type_oid } => match u32::from(*type_oid) {
            BOOLOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                1,
            ),
            INT2OID | INT4OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                4,
            ),
            INT8OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64,
                8,
            ),
            FLOAT8OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64,
                8,
            ),
            FLOAT4OID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
                4,
            ),
            DATEOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                4,
            ),
            TIMESTAMPOID | TIMESTAMPTZOID => (
                std::ptr::null(),
                std::ptr::null(),
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
                8,
            ),
            type_oid => return Err(format!("type OID {type_oid} cannot be a measure")),
        },
        ResidentColumnView::Bool {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == BOOLOID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
            1,
        ),
        ResidentColumnView::I32 {
            type_oid,
            values,
            nulls,
        } if matches!(u32::from(*type_oid), INT2OID | INT4OID) => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
            4,
        ),
        ResidentColumnView::I32 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == DATEOID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
            4,
        ),
        ResidentColumnView::I64 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == INT8OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64,
            8,
        ),
        ResidentColumnView::I64 {
            type_oid,
            values,
            nulls,
        } if matches!(u32::from(*type_oid), TIMESTAMPOID | TIMESTAMPTZOID) => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
            8,
        ),
        ResidentColumnView::F32 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == FLOAT4OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32,
            4,
        ),
        ResidentColumnView::F64 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == FLOAT8OID => (
            values.as_ptr().cast::<c_void>(),
            null_ptr(*nulls),
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64,
            8,
        ),
        _ => {
            return Err(format!(
                "resident type OID {} has no Phase 5D measure representation",
                u32::from(view.type_oid())
            ));
        }
    };
    Ok(abi::PgaccelGroupedAggMeasureCol {
        values,
        nulls,
        physical_type,
        element_bytes,
        scale: 0,
        flags: 0,
    })
}

fn lane_mask(outputs: &[crate::engine::spec::AggregateOutput]) -> u32 {
    outputs.iter().fold(0, |mask, output| {
        mask | match output.kind {
            AggregateKind::Sum | AggregateKind::Avg => abi::PGACCEL_GROUPED_AGG_LANE_SUM,
            AggregateKind::Count => abi::PGACCEL_GROUPED_AGG_LANE_COUNT,
            AggregateKind::Min => abi::PGACCEL_GROUPED_AGG_LANE_MIN,
            AggregateKind::Max => abi::PGACCEL_GROUPED_AGG_LANE_MAX,
            AggregateKind::StddevSamp => 0,
        }
    })
}

fn disabled_filter() -> abi::PgaccelGroupedAggFilter {
    // SAFETY: all-zero is valid for every field, including PgaccelValTag::Null.
    let mut filter: abi::PgaccelGroupedAggFilter = unsafe { std::mem::zeroed() };
    filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
    filter
}

fn scalar_value(value: ScalarValue) -> PgaccelVal {
    match value {
        ScalarValue::Bool(value) => PgaccelVal::from_bool(value),
        ScalarValue::I32(value) => PgaccelVal::from_i32(value),
        ScalarValue::I64(value) => PgaccelVal::from_i64(value),
        ScalarValue::F32(value) => PgaccelVal::from_f32(value),
        ScalarValue::F64(value) => PgaccelVal::from_f64(value),
        ScalarValue::Date(value) => PgaccelVal {
            tag: PgaccelValTag::Date,
            data: value as u64,
        },
        ScalarValue::Timestamp(value) | ScalarValue::TimestampTz(value) => PgaccelVal {
            tag: PgaccelValTag::Timestamp,
            data: value as u64,
        },
    }
}

fn filter_measure_binding(spec: &AggQuerySpec, input: ColumnRef) -> Result<usize, String> {
    spec.measures
        .iter()
        .position(|measure| match measure.expression {
            MeasureExpr::Column(column) => column == input,
            MeasureExpr::Binary { lhs, .. } | MeasureExpr::StatsPair { value: lhs, .. } => {
                lhs == input
            }
            MeasureExpr::CountStar | MeasureExpr::Bytecode { .. } => false,
        })
        .ok_or_else(|| "fact range input has no descriptor measure binding".to_owned())
}

fn scalar_where_measure_binding(spec: &AggQuerySpec) -> Option<usize> {
    let FilterSpec::Bytecode { inputs, program } = spec.fact_filter.scalar() else {
        return None;
    };
    spec.measures.iter().position(|measure| {
        measure.filter == FilterSpec::None
            && matches!(&measure.expression, MeasureExpr::Bytecode {
                inputs: bound_inputs, program: bound_program, result_type_oid: BOOLOID,
            } if inputs == bound_inputs && program == bound_program)
            && scalar_measure_program(&measure.expression, spec.fact_rel).is_ok()
    })
}

fn build_sql_mask_filter(
    fact_rows: usize,
    mask: Option<*const i8>,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    if fact_rows == 0 {
        return Ok(disabled_filter());
    }
    let mask = mask
        .filter(|mask| !mask.is_null())
        .ok_or_else(|| "nonempty SQL fact mask has no derived device buffer".to_owned())?;
    let mut filter = disabled_filter();
    filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
    filter.mask = mask;
    Ok(filter)
}

fn build_filter(
    spec: &AggQuerySpec,
    fact_rows: usize,
    fact_mask: Option<*const i8>,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    match spec.fact_filter.scalar() {
        FilterSpec::None => Ok(disabled_filter()),
        FilterSpec::Bytecode { .. } => {
            let mut filter = if spec.fact_filter.spatial().is_some() {
                build_sql_mask_filter(fact_rows, fact_mask)?
            } else {
                disabled_filter()
            };
            filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
            filter.predicate_measure_slot = scalar_where_measure_binding(spec)
                .ok_or_else(|| "scalar WHERE has no unfiltered Boolean carrier".to_owned())?
                as i32;
            Ok(filter)
        }
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        }
        | FilterSpec::Spatial { .. } => build_sql_mask_filter(fact_rows, fact_mask),
        FilterSpec::Ranges { input, ranges } | FilterSpec::NullOrRanges { input, ranges } => {
            if ranges.len() > abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES {
                return Err("fact filter range count exceeds descriptor capacity".to_owned());
            }
            let mut filter = if spec.fact_filter.spatial().is_some() {
                build_sql_mask_filter(fact_rows, fact_mask)?
            } else {
                disabled_filter()
            };
            filter.predicate_measure_slot = i32::try_from(filter_measure_binding(spec, *input)?)
                .map_err(|_| "filter measure index exceeds i32".to_owned())?;
            filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
            if matches!(spec.fact_filter.scalar(), FilterSpec::NullOrRanges { .. }) {
                filter.flags = abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
            }
            filter.predicate_range_count = i32::try_from(ranges.len())
                .map_err(|_| "fact filter range count exceeds i32".to_owned())?;
            for (index, range) in ranges.iter().enumerate() {
                filter.predicate_lo[index] = scalar_value(range.lo);
                filter.predicate_hi[index] = scalar_value(range.hi);
            }
            Ok(filter)
        }
        _ => Err("fact filter escaped the Phase 5D capability backstop".into()),
    }
}

fn build_numeric_measure_filter(
    spec: &AggQuerySpec,
    index: usize,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    let measure = spec
        .measures
        .get(index)
        .ok_or("NUMERIC filter slot missing")?;
    if matches!(measure.expression, MeasureExpr::Binary { .. }) {
        return build_measure_filter(spec, &measure.filter);
    }
    if !canonical_numeric_measure_filter(spec, &measure.filter) {
        return Err("NUMERIC predicate must be an exact fact INT4 range filter".into());
    }
    let mut result = disabled_filter();
    if let Some((_, ranges, accept_null)) = measure.filter.range_parts() {
        if accept_null {
            result.flags = abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
        }
        result.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
        result.predicate_measure_slot =
            i32::try_from(index).map_err(|_| "NUMERIC slot exceeds i32")?;
        result.predicate_range_count = ranges.len() as i32;
        for (index, range) in ranges.iter().enumerate() {
            result.predicate_lo[index] = scalar_value(range.lo);
            result.predicate_hi[index] = scalar_value(range.hi);
        }
    }
    Ok(result)
}

fn uses_raw_rhs_measure_filter(spec: &AggQuerySpec) -> bool {
    has_numeric_measure(spec)
        || canonical_dense_filtered_integer_sum_avg(spec)
        || has_scalar_measure(spec)
}

fn build_measure_filter(
    spec: &AggQuerySpec,
    filter: &FilterSpec,
) -> Result<abi::PgaccelGroupedAggFilter, String> {
    match filter {
        FilterSpec::None => Ok(disabled_filter()),
        FilterSpec::Ranges { input, ranges } | FilterSpec::NullOrRanges { input, ranges } => {
            if ranges.len() > abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES {
                return Err("measure filter range count exceeds descriptor capacity".to_owned());
            }
            let accept_null = matches!(filter, FilterSpec::NullOrRanges { .. });
            let mut filter = disabled_filter();
            if accept_null {
                filter.flags = abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
            }
            filter.predicate_measure_slot = i32::try_from(filter_measure_binding(spec, *input)?)
                .map_err(|_| "measure filter input index exceeds i32".to_owned())?;
            filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
            filter.predicate_range_count = i32::try_from(ranges.len())
                .map_err(|_| "measure filter range count exceeds i32".to_owned())?;
            for (index, range) in ranges.iter().enumerate() {
                filter.predicate_lo[index] = scalar_value(range.lo);
                filter.predicate_hi[index] = scalar_value(range.hi);
            }
            Ok(filter)
        }
        _ => Err("measure filter escaped the Phase 5D capability backstop".into()),
    }
}

/// Pins host-readable metadata until the resolved GPU plan snapshots it.
/// Resident payload ownership remains with the surrounding resolver.
struct BoundDescriptor {
    desc: abi::PgaccelGroupedAggDesc,
    _numeric_views: [Option<Box<abi::PgaccelNumericView>>; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
    _scalar_views: [Option<BoundScalarView>; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
}

pub struct BoundScalarView {
    _view: Box<abi::PgaccelScalarView>,
    _words: Box<[i32]>,
    _inputs: Box<[abi::PgaccelScalarInput]>,
    _charge: crate::engine::residency::ExecutionMemoryCharge,
}

/// Conservative peak for runtime scalar snapshot payloads only. Bounded
/// dispatch holds session + bound descriptor + resolved plan + row chunk at
/// once. One-shot uses fewer snapshots; admission deliberately budgets both
/// modes without relying on a row-count estimate to predict the runtime mode.
/// This excludes workspace/output and the logical spec/artifact allocations.
pub fn scalar_metadata_peak_bytes(spec: &AggQuerySpec) -> Result<u64, String> {
    let mut snapshot = 0_u64;
    for measure in &spec.measures {
        let MeasureExpr::Bytecode {
            inputs, program, ..
        } = &measure.expression
        else {
            continue;
        };
        if spec.measures.len() > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES {
            return Err("scalar metadata forecast exceeds descriptor measure capacity".into());
        }
        scalar_measure_program(&measure.expression, spec.fact_rel)?;
        let bytes = crate::gpu::scalar_metadata_bytes(program.len(), inputs.len())
            .map_err(|error| error.to_string())?;
        snapshot = snapshot
            .checked_add(bytes as u64)
            .ok_or("scalar snapshot byte forecast overflow")?;
    }
    snapshot
        .checked_mul(4)
        .ok_or_else(|| "scalar metadata peak byte forecast overflow".into())
}

#[allow(clippy::redundant_pub_crate)] // re-exported from the private descriptor module
pub(crate) fn scalar_measure_program(
    expression: &MeasureExpr,
    fact_rel: u32,
) -> Result<ValidatedScalarWire, String> {
    let MeasureExpr::Bytecode {
        inputs,
        program,
        result_type_oid,
    } = expression
    else {
        return Err("scalar binding requires a versioned measure program".into());
    };
    // Unlike the logical envelope, runtime never accepts opaque legacy words.
    let decoded = ScalarProgram::validate_wire(program)
        .map_err(|error| format!("scalar measure program failed validation: {error}"))?;
    let oid = |kind| match kind {
        ScalarType::Bool => BOOLOID,
        ScalarType::Int4 => INT4OID,
        ScalarType::Date => DATEOID,
    };
    if *result_type_oid != oid(decoded.result_type())
        || inputs.len() != decoded.inputs().len()
        || inputs.iter().zip(decoded.inputs()).any(|(column, kind)| {
            column.relation_oid != fact_rel
                || column.attno <= 0
                || i16::try_from(column.attno).is_err()
                || column.type_oid != oid(*kind)
        })
    {
        return Err("scalar measure columns/result differ from the typed fact program".into());
    }
    Ok(decoded)
}

/// Native workspace/output forecast for the fused dense scalar family.
/// Metadata lives on the stack and borrows program words; no payloads, owners,
/// ledger reservations or allocation-pool entries are created by this query.
pub fn scalar_descriptor_memory_requirements(
    spec: &AggQuerySpec,
    nullable_columns: &[i16],
    group_capacity: usize,
    rows: usize,
    max_chunk_rows: usize,
    one_shot_max_rows: usize,
) -> Result<crate::gpu::GroupedAggMemoryRequirements, String> {
    if spec.measures.is_empty()
        || spec.measures.len() > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || spec.group_keys.len() > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        || !spec.star_dims.is_empty()
        || spec.having.is_some()
        || group_capacity == 0
        || (spec.group_keys.is_empty() && group_capacity != 1)
        || !spec
            .measures
            .iter()
            .any(|measure| matches!(measure.expression, MeasureExpr::Bytecode { .. }))
    {
        return Err("scalar layout forecast requires a bounded dense scalar shape".into());
    }
    let marker = 0_i32;
    let null_marker = 0_u8;
    let nullable = |column: ColumnRef| {
        nullable_columns
            .iter()
            .any(|attno| i32::from(*attno) == column.attno)
    };
    let raw_column = |column: ColumnRef| -> Result<abi::PgaccelGroupedAggMeasureCol, String> {
        if column.relation_oid != spec.fact_rel
            || column.attno <= 0
            || i16::try_from(column.attno).is_err()
            || !matches!(column.type_oid, INT2OID | INT4OID)
        {
            return Err("scalar layout forecast raw inputs must be fact INT2/INT4 columns".into());
        }
        Ok(abi::PgaccelGroupedAggMeasureCol {
            values: std::ptr::from_ref(&marker).cast(),
            nulls: if nullable(column) {
                &raw const null_marker
            } else {
                std::ptr::null()
            },
            physical_type: abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
            element_bytes: 4,
            scale: 0,
            flags: 0,
        })
    };
    // SAFETY: ABI records have valid zero bit patterns. All active fields are
    // initialized below, and stack metadata is never passed to execution.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    // SAFETY: zero is a valid initial value for each scalar view/input field.
    let mut views: [abi::PgaccelScalarView; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES] =
        unsafe { std::mem::zeroed() };
    // SAFETY: same zero-initialized plain ABI records; unused entries stay inert.
    let mut bindings: [[abi::PgaccelScalarInput; abi::PGACCEL_SCALAR_MAX_INPUTS];
        abi::PGACCEL_GROUPED_AGG_MAX_MEASURES] = unsafe { std::mem::zeroed() };
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = rows;
    desc.group_capacity = group_capacity;
    desc.key_count = spec.group_keys.len() as u32;
    desc.measure_count = spec.measures.len() as u32;
    desc.measure_filters.fill(disabled_filter());
    for (index, key) in spec.group_keys.iter().enumerate() {
        let GroupKeySource::FactColumn(column) = key.source else {
            return Err("scalar layout forecast requires fact dictionary keys".into());
        };
        if column.relation_oid != spec.fact_rel
            || column.attno <= 0
            || i16::try_from(column.attno).is_err()
        {
            return Err("scalar layout forecast key identity is invalid".into());
        }
        desc.keys[index].values.values = std::ptr::from_ref(&marker).cast();
        desc.keys[index].values.tag = PgaccelValTag::Int32;
        desc.keys[index].cardinality = if index == 0 {
            u32::try_from(group_capacity).map_err(|_| "scalar dense capacity exceeds u32")?
        } else {
            1
        };
        desc.keys[index].null_code = if nullable(column) {
            0
        } else {
            abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE
        };
    }
    for (index, measure) in spec.measures.iter().enumerate() {
        let target = &mut desc.measures[index];
        target.agg_mask = lane_mask(&measure.outputs);
        target.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        target.state_bytes = 8;
        match &measure.expression {
            MeasureExpr::Bytecode {
                inputs, program, ..
            } => {
                let proof = scalar_measure_program(&measure.expression, spec.fact_rel)?;
                for ((binding, column), kind) in
                    bindings[index].iter_mut().zip(inputs).zip(proof.inputs())
                {
                    binding.values = std::ptr::from_ref(&marker).cast();
                    binding.nulls = if nullable(*column) {
                        &raw const null_marker
                    } else {
                        std::ptr::null()
                    };
                    binding.physical_type = match kind {
                        ScalarType::Bool => abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        ScalarType::Int4 => abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                        ScalarType::Date => abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                    };
                }
                views[index] = abi::PgaccelScalarView {
                    version: abi::PGACCEL_SCALAR_VIEW_VERSION,
                    reserved: 0,
                    row_count: rows as u64,
                    word_count: program.len() as u32,
                    input_count: inputs.len() as u32,
                    words: program.as_ptr(),
                    inputs: if inputs.is_empty() {
                        std::ptr::null()
                    } else {
                        bindings[index].as_ptr()
                    },
                };
                target.op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
                target.value = abi::PgaccelGroupedAggMeasureCol {
                    values: std::ptr::from_ref(&views[index]).cast(),
                    nulls: std::ptr::null(),
                    physical_type: match proof.result_type() {
                        ScalarType::Bool => abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                        ScalarType::Int4 => abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                        ScalarType::Date => abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                    },
                    element_bytes: size_of::<abi::PgaccelScalarView>() as u32,
                    scale: 0,
                    flags: abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1,
                };
            }
            MeasureExpr::Column(column) => {
                target.value = raw_column(*column)?;
            }
            MeasureExpr::Binary { op, lhs, rhs } => {
                target.op = match op {
                    BinaryMeasureOp::Add => abi::PGACCEL_GROUPED_AGG_MEASURE_ADD,
                    BinaryMeasureOp::Sub => abi::PGACCEL_GROUPED_AGG_MEASURE_SUB,
                    BinaryMeasureOp::Mul => abi::PGACCEL_GROUPED_AGG_MEASURE_MUL,
                };
                target.value = raw_column(*lhs)?;
                target.rhs = raw_column(*rhs)?;
            }
            MeasureExpr::CountStar if measure.filtered_count_input().is_some() => {
                target.value = raw_column(
                    measure
                        .filtered_count_input()
                        .ok_or("filtered count input missing")?,
                )?;
                target.value.nulls = std::ptr::null();
            }
            MeasureExpr::CountStar if measure.filter == FilterSpec::None => {
                target.op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
            }
            _ => return Err("scalar layout forecast measure is unsupported".into()),
        }
        if uses_raw_rhs_measure_filter(spec)
            && !matches!(measure.expression, MeasureExpr::Binary { .. })
            && let Some((column, _, _)) = measure.filter.range_parts()
        {
            target.flags = abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
            target.rhs = raw_column(column)?;
        }
        desc.measure_filters[index] = if uses_raw_rhs_measure_filter(spec) {
            build_numeric_measure_filter(spec, index)?
        } else {
            build_measure_filter(spec, &measure.filter)?
        };
    }
    desc.where_filter = build_filter(spec, rows, Some(std::ptr::from_ref(&null_marker).cast()))?;
    if !parallel_dense_integer_shape(&desc) {
        return Err("scalar layout forecast is outside the parallel dense integer family".into());
    }
    if !dense_one_shot_eligible(&desc, dense_one_shot_row_cap(one_shot_max_rows)) {
        let chunk = dense_partial_chunk_rows(
            &desc,
            dense_session_chunk_rows(max_chunk_rows, one_shot_max_rows),
        );
        if chunk == 0 {
            return Err("scalar layout forecast has no bounded chunk capacity".into());
        }
        desc.row_count = rows.min(chunk);
        desc.execution_flags =
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    }
    // SAFETY: every host view and nested slice above is live and validated.
    // Native layout queries inspect pointer presence but never resident rows;
    // markers and this private descriptor are never exposed for execution.
    unsafe { crate::gpu::query_dense_grouped_memory_layout(&desc) }
        .map_err(|error| error.to_string())
}

pub fn scalar_measure_column(
    expression: &MeasureExpr,
    fact_rel: u32,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    expected_rows: usize,
    agg_mask: u32,
) -> Result<(abi::PgaccelGroupedAggMeasureCol, BoundScalarView), String> {
    let decoded = scalar_measure_program(expression, fact_rel)?;
    let MeasureExpr::Bytecode {
        inputs, program, ..
    } = expression
    else {
        unreachable!("validated scalar expression");
    };
    if requests.len() != views.len()
        || agg_mask == 0
        || agg_mask
            & !(abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX)
            != 0
        || (decoded.result_type() == ScalarType::Bool
            && agg_mask != abi::PGACCEL_GROUPED_AGG_LANE_COUNT)
        || (decoded.result_type() == ScalarType::Date
            && agg_mask & abi::PGACCEL_GROUPED_AGG_LANE_SUM != 0)
    {
        return Err("scalar measure binding bundle or output lanes are invalid".into());
    }
    if requests.iter().enumerate().any(|(index, request)| {
        requests[..index]
            .iter()
            .any(|previous| previous.relid == request.relid && previous.attno == request.attno)
    }) {
        return Err("scalar resident bundle repeats a column identity".into());
    }
    let mut bindings = [abi::PgaccelScalarInput {
        values: std::ptr::null(),
        nulls: std::ptr::null(),
        physical_type: 0,
        reserved: 0,
    }; abi::PGACCEL_SCALAR_MAX_INPUTS];
    for (binding, column) in bindings.iter_mut().zip(inputs) {
        let view = find_view(requests, views, *column)?;
        if matches!(view, ResidentColumnView::Bool { nulls: Some(nulls), .. }
                | ResidentColumnView::I32 { nulls: Some(nulls), .. } if nulls.len() != expected_rows)
        {
            return Err("scalar input NULL sidecar length differs from fact rows".into());
        }
        let native = measure_column(view, expected_rows)?;
        // Logical OIDs above prohibit INT2 widening and all reinterpretation.
        let expected = if column.type_oid == BOOLOID {
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
        } else if column.type_oid == DATEOID {
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE
        } else {
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        };
        if native.physical_type != expected || native.flags != 0 {
            return Err("scalar input lacks its exact resident physical representation".into());
        }
        *binding = abi::PgaccelScalarInput {
            values: native.values,
            nulls: native.nulls,
            physical_type: native.physical_type,
            reserved: 0,
        };
    }
    let charge = crate::gpu::reserve_scalar_metadata(program.len(), inputs.len())
        .map_err(|error| format!("scalar binding memory reservation failed: {error}"))?;
    let bindings = bindings[..inputs.len()].to_vec().into_boxed_slice();
    let words = program.clone().into_boxed_slice();
    let pinned = Box::new(abi::PgaccelScalarView {
        version: abi::PGACCEL_SCALAR_VIEW_VERSION,
        word_count: words.len() as u32,
        input_count: bindings.len() as u32,
        reserved: 0,
        row_count: expected_rows as u64,
        words: words.as_ptr(),
        inputs: if bindings.is_empty() {
            std::ptr::null()
        } else {
            bindings.as_ptr()
        },
    });
    let column = abi::PgaccelGroupedAggMeasureCol {
        values: std::ptr::from_ref(pinned.as_ref()).cast(),
        nulls: std::ptr::null(),
        physical_type: match decoded.result_type() {
            ScalarType::Bool => abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
            ScalarType::Int4 => abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
            ScalarType::Date => abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
        },
        element_bytes: size_of::<abi::PgaccelScalarView>() as u32,
        scale: 0,
        flags: abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1,
    };
    Ok((
        column,
        BoundScalarView {
            _view: pinned,
            _words: words,
            _inputs: bindings,
            _charge: charge,
        },
    ))
}

fn numeric_measure_column(
    view: &ResidentColumnView<'_>,
    expected_rows: usize,
) -> Result<
    (
        abi::PgaccelGroupedAggMeasureCol,
        Box<abi::PgaccelNumericView>,
        u32,
    ),
    String,
> {
    if u32::from(view.type_oid()) != NUMERICOID || view.len() != expected_rows {
        return Err("NUMERIC measure type/length differs from the resolved artifact".to_owned());
    }
    let (metadata, nulls) = match view {
        ResidentColumnView::Numeric { data, .. } => (data.native_view(), null_ptr(data.nulls)),
        ResidentColumnView::Empty { .. } if expected_rows == 0 => (
            abi::PgaccelNumericView {
                rows: std::ptr::null(),
                digits: std::ptr::null(),
                digit_count: 0,
                row_count: 0,
                min_weight: 0,
                max_weight: 0,
                max_ndigits: 0,
                reserved: 0,
            },
            std::ptr::null(),
        ),
        _ => return Err("NUMERIC measure lacks exact resident row/digit buffers".to_owned()),
    };
    let state_bytes = crate::engine::residency::NumericDigitBounds {
        min_weight: metadata.min_weight,
        max_weight: metadata.max_weight,
        max_ndigits: metadata.max_ndigits,
    }
    .sum_state_bytes()
    .ok_or("NUMERIC digit extents exceed PostgreSQL's stored range")?;
    let pinned = Box::new(metadata);
    let column = abi::PgaccelGroupedAggMeasureCol {
        values: std::ptr::from_ref(pinned.as_ref()).cast(),
        nulls,
        physical_type: abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC,
        element_bytes: size_of::<abi::PgaccelNumericView>() as u32,
        scale: 0,
        flags: abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW,
    };
    Ok((column, pinned, state_bytes))
}

fn build_descriptor_with_fact_mask(
    artifact: &DescriptorAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    fact_mask: Option<*const i8>,
) -> Result<BoundDescriptor, String> {
    let spec = &artifact.resolved_spec;
    if artifact.keys.len() != spec.group_keys.len()
        || artifact.domains.len() != spec.group_keys.len()
        || artifact.dimensions.len() != spec.star_dims.len()
    {
        return Err("derived aggregate artifact changed structural shape".into());
    }
    // SAFETY: every enum-bearing field has zero as a valid discriminant and
    // all inactive ABI slots are required to be bytewise zero.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    let mut numeric_views = std::array::from_fn(|_| None);
    let mut scalar_views = std::array::from_fn(|_| None);
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = artifact.fact_rows;
    desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
    desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
    desc.key_count = u32::try_from(spec.group_keys.len())
        .map_err(|_| "group-key count exceeds u32".to_owned())?;
    desc.group_capacity = artifact.group_capacity;
    for (index, ((key, input), domain)) in spec
        .group_keys
        .iter()
        .zip(&artifact.keys)
        .zip(artifact.domains.iter())
        .enumerate()
    {
        let (source, values, lookup_by_key) = match (&key.source, input) {
            (GroupKeySource::FactColumn(_), ArtifactKeyInput::Fact(values)) => (
                abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT,
                PgaccelExprUsmCol {
                    values: values
                        .as_ref()
                        .map_or(std::ptr::null(), |values| values.as_ptr().cast()),
                    nulls: std::ptr::null(),
                    tag: PgaccelValTag::Int32,
                },
                std::ptr::null(),
            ),
            (
                GroupKeySource::StarDimension { dim_index, .. },
                ArtifactKeyInput::Dimension {
                    dim_index: artifact_dim,
                    lookup,
                },
            ) if usize::try_from(*dim_index).ok() == Some(*artifact_dim) => (
                abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0
                    + i32::try_from(*artifact_dim)
                        .map_err(|_| "dimension index exceeds i32".to_owned())?,
                PgaccelExprUsmCol {
                    values: std::ptr::null(),
                    nulls: std::ptr::null(),
                    tag: PgaccelValTag::Null,
                },
                lookup
                    .as_ref()
                    .map_or(std::ptr::null(), |lookup| lookup.as_ptr()),
            ),
            _ => return Err("artifact group-key source does not match resolved spec".into()),
        };
        desc.keys[index] = abi::PgaccelGroupedAggKey {
            values,
            lookup_by_key,
            source,
            code_min: 0,
            cardinality: domain.cardinality()?,
            null_code: domain
                .null_code
                .unwrap_or(abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE),
            flags: 0,
            pad0: 0,
        };
    }
    desc.measure_count =
        u32::try_from(spec.measures.len()).map_err(|_| "measure count exceeds u32".to_owned())?;
    for (index, measure) in spec.measures.iter().enumerate() {
        let agg_mask = lane_mask(&measure.outputs);
        let mut state_bytes = 8;
        let (op, value, rhs, accumulator_kind) = match &measure.expression {
            MeasureExpr::CountStar if measure.filtered_count_input().is_some() => {
                let input = measure
                    .filtered_count_input()
                    .ok_or("filtered COUNT input missing")?;
                let mut value =
                    measure_column(find_view(requests, views, input)?, artifact.fact_rows)?;
                // Count every accepted row, including NULL predicate inputs.
                // The independent RHS binding retains the real NULL sidecar.
                value.nulls = std::ptr::null();
                (
                    abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                    value,
                    // SAFETY: unary counter has no expression RHS; its typed
                    // predicate binding is installed below before dispatch.
                    unsafe { std::mem::zeroed() },
                    abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
                )
            }
            MeasureExpr::CountStar => (
                abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
                // SAFETY: inactive measure columns must be bytewise zero.
                unsafe { std::mem::zeroed() },
                // SAFETY: COUNT(*) has no right-hand input column, and the ABI
                // requires its inactive column descriptor to be all zeroes.
                unsafe { std::mem::zeroed() },
                abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
            ),
            MeasureExpr::Column(column) => {
                let view = find_view(requests, views, *column)?;
                let input = if column.type_oid == NUMERICOID {
                    let (input, pinned, width) = numeric_measure_column(view, artifact.fact_rows)?;
                    numeric_views[index] = Some(pinned);
                    state_bytes = if agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT {
                        8
                    } else {
                        width
                    };
                    input
                } else {
                    if column.type_oid == INT8OID
                        && has_numeric_measure(spec)
                        && agg_mask != abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                    {
                        state_bytes = abi::PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES;
                    }
                    measure_column(view, artifact.fact_rows)?
                };
                (
                    abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN,
                    input,
                    // SAFETY: a unary column measure has no right-hand input;
                    // the ABI requires that inactive descriptor to be zeroed.
                    unsafe { std::mem::zeroed() },
                    if column.type_oid == NUMERICOID
                        || (column.type_oid == INT8OID && has_numeric_measure(spec))
                    {
                        abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC
                    } else if matches!(column.type_oid, FLOAT4OID | FLOAT8OID)
                        && agg_mask != abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                    {
                        abi::PGACCEL_GROUPED_AGG_ACCUM_F64
                    } else {
                        abi::PGACCEL_GROUPED_AGG_ACCUM_I64
                    },
                )
            }
            MeasureExpr::Binary { op, lhs, rhs } => {
                let lhs_view = find_view(requests, views, *lhs)?;
                let rhs_view = find_view(requests, views, *rhs)?;
                (
                    match op {
                        BinaryMeasureOp::Mul => abi::PGACCEL_GROUPED_AGG_MEASURE_MUL,
                        BinaryMeasureOp::Sub => abi::PGACCEL_GROUPED_AGG_MEASURE_SUB,
                        BinaryMeasureOp::Add => abi::PGACCEL_GROUPED_AGG_MEASURE_ADD,
                    },
                    measure_column(lhs_view, artifact.fact_rows)?,
                    measure_column(rhs_view, artifact.fact_rows)?,
                    abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
                )
            }
            MeasureExpr::Bytecode { .. } => {
                let (column, owner) = scalar_measure_column(
                    &measure.expression,
                    spec.fact_rel,
                    requests,
                    views,
                    artifact.fact_rows,
                    agg_mask,
                )?;
                scalar_views[index] = Some(owner);
                (
                    abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR,
                    column,
                    // SAFETY: the inactive RHS must be canonical zero; an
                    // independent predicate may be bound below.
                    unsafe { std::mem::zeroed() },
                    abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
                )
            }
            MeasureExpr::StatsPair { .. } => {
                return Err("unsupported measure escaped the Phase 5D capability backstop".into());
            }
        };
        desc.measures[index] = abi::PgaccelGroupedAggMeasure {
            value,
            rhs,
            op,
            agg_mask,
            accumulator_kind,
            state_bytes,
            flags: 0,
            pad0: 0,
        };
        if uses_raw_rhs_measure_filter(spec)
            && !matches!(measure.expression, MeasureExpr::Binary { .. })
            && let Some((input, _, _)) = measure.filter.range_parts()
        {
            let view = find_view(requests, views, input)?;
            desc.measures[index].rhs = measure_column(view, artifact.fact_rows)?;
            desc.measures[index].flags = abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
        }
    }
    desc.where_filter = build_filter(spec, artifact.fact_rows, fact_mask)?;
    for (index, filter) in desc.measure_filters.iter_mut().enumerate() {
        *filter = if let Some(measure) = spec.measures.get(index) {
            if uses_raw_rhs_measure_filter(spec) {
                build_numeric_measure_filter(spec, index)?
            } else {
                build_measure_filter(spec, &measure.filter)?
            }
        } else {
            disabled_filter()
        };
    }
    desc.dim_count = u32::try_from(artifact.dimensions.len())
        .map_err(|_| "dimension count exceeds u32".to_owned())?;
    for (index, dimension) in artifact.dimensions.iter().enumerate() {
        desc.dims[index] = abi::PgaccelGroupedAggDim {
            fact_key: PgaccelExprUsmCol {
                values: dimension
                    .fact_codes
                    .as_ref()
                    .map_or(std::ptr::null(), |codes| codes.as_ptr().cast()),
                nulls: std::ptr::null(),
                tag: PgaccelValTag::Int32,
            },
            match_by_key: dimension
                .match_by_key
                .as_ref()
                .map_or(std::ptr::null(), |mask| mask.as_ptr()),
            multiplicity_by_key: dimension
                .multiplicity_by_key
                .as_ref()
                .map_or(std::ptr::null(), |multiplicity| multiplicity.as_ptr()),
            key_min: 0,
            key_count: u32::try_from(dimension.key_count)
                .map_err(|_| "dimension dictionary exceeds u32".to_owned())?,
            flags: 0,
            pad0: 0,
        };
    }
    Ok(BoundDescriptor {
        desc,
        _numeric_views: numeric_views,
        _scalar_views: scalar_views,
    })
}

fn build_descriptor(
    artifact: &DescriptorAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
) -> Result<BoundDescriptor, String> {
    build_descriptor_with_fact_mask(
        artifact,
        requests,
        views,
        artifact.fact_mask.as_ref().map(|mask| mask.as_ptr()),
    )
}

fn build_spatial_descriptor(
    artifact: &SpatialAggArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
) -> Result<BoundDescriptor, String> {
    let fact_mask = spatial_final_mask_binding(
        artifact.base.fact_rows,
        artifact
            .final_mask
            .as_ref()
            .map(|mask| (mask.len(), mask.as_ptr())),
    )?;
    build_descriptor_with_fact_mask(&artifact.base, requests, views, fact_mask)
}

fn spatial_final_mask_binding(
    fact_rows: usize,
    mask: Option<(usize, *const i8)>,
) -> Result<Option<*const i8>, String> {
    match (fact_rows, mask) {
        (0, None) => Ok(None),
        (0, Some(_)) => Err("empty spatial artifact unexpectedly retained a final mask".to_owned()),
        (_, None) => Err("nonempty spatial artifact has no final mask".to_owned()),
        (rows, Some((len, _))) if len != rows => Err(format!(
            "spatial final mask length {len} does not match fact rows {rows}"
        )),
        (_, Some((_, mask))) if mask.is_null() => {
            Err("nonempty spatial final mask has a null device pointer".to_owned())
        }
        (_, Some((_, mask))) => Ok(Some(mask)),
    }
}

fn hash_count_descriptor(
    fact_rows: usize,
    group_capacity: usize,
    values: *const c_void,
    nulls: *const u8,
    flags: u32,
    transform_arg: u32,
) -> abi::PgaccelGroupedAggDesc {
    // SAFETY: all-zero is canonical for every inactive ABI slot.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = fact_rows;
    desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
    desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT;
    desc.key_count = 1;
    desc.group_capacity = group_capacity;
    desc.keys[0] = abi::PgaccelGroupedAggKey {
        values: PgaccelExprUsmCol {
            values,
            nulls,
            tag: PgaccelValTag::Int64,
        },
        lookup_by_key: std::ptr::null(),
        source: abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT,
        code_min: 0,
        cardinality: 0,
        null_code: abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE,
        flags,
        pad0: transform_arg,
    };
    desc.measure_count = 1;
    desc.measures[0] = abi::PgaccelGroupedAggMeasure {
        // SAFETY: COUNT(*) has no input columns.
        value: unsafe { std::mem::zeroed() },
        // SAFETY: COUNT(*) has no right-hand input column.
        rhs: unsafe { std::mem::zeroed() },
        op: abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR,
        agg_mask: abi::PGACCEL_GROUPED_AGG_LANE_COUNT,
        accumulator_kind: abi::PGACCEL_GROUPED_AGG_ACCUM_I64,
        state_bytes: 8,
        flags: 0,
        pad0: 0,
    };
    desc.where_filter = disabled_filter();
    for filter in &mut desc.measure_filters {
        *filter = disabled_filter();
    }
    desc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HashMeasureInput {
    Star,
    FilteredStar,
    Column(ColumnRef),
    /// INT2 residency is losslessly widened to the same INT32 physical lane.
    IntegerStates(ColumnRef),
}

impl HashMeasureInput {
    const fn label(self) -> &'static str {
        match self {
            Self::Star => "COUNT(*)",
            Self::FilteredStar => "COUNT(*) FILTER",
            Self::Column(_) => "COUNT(column)",
            Self::IntegerStates(column) if column.type_oid == INT2OID => "integer states(int2)",
            Self::IntegerStates(_) => "integer states(int4)",
        }
    }

    fn from_spec(spec: &AggQuerySpec) -> Result<Self, String> {
        let [measure] = spec.measures.as_slice() else {
            return Err("hash binding requires one COUNT or INT2/INT4 measure".into());
        };
        if !spec.star_dims.is_empty() || spec.having.is_some() {
            return Err("H3 hash binding does not support joins/HAVING".into());
        }
        hash_where_filter(spec)?;
        let filtered = h3_count_filter(spec)?.is_some();
        let count = crate::engine::spec::AggregateOutput {
            source: AggregateSource::Value,
            kind: AggregateKind::Count,
        };
        // Value queries need not expose COUNT. Physical state still uses it
        // for the exact AVG divisor and SQL NULL validity. Keep logical
        // projection distinct from the required native lanes.
        let integer_states = (1..=5).contains(&measure.outputs.len())
            && measure.outputs.iter().enumerate().all(|(index, output)| {
                output.source == AggregateSource::Value
                    && matches!(
                        output.kind,
                        AggregateKind::Sum
                            | AggregateKind::Avg
                            | AggregateKind::Count
                            | AggregateKind::Min
                            | AggregateKind::Max
                    )
                    && !measure.outputs[..index].contains(output)
            })
            && measure
                .outputs
                .iter()
                .any(|output| output.kind != AggregateKind::Count);
        if integer_states {
            return match measure.expression {
                MeasureExpr::Column(column)
                    if column.relation_oid == spec.fact_rel
                        && column.attno > 0
                        && matches!(column.type_oid, INT2OID | INT4OID) =>
                {
                    Ok(Self::IntegerStates(column))
                }
                _ => Err("hash value states require an exact INT2/INT4 fact column".into()),
            };
        }
        if measure.outputs.as_slice() != [count] {
            return Err("hash binding requires COUNT or exact INT2/INT4 states".into());
        }
        match measure.expression {
            MeasureExpr::CountStar if filtered => Ok(Self::FilteredStar),
            MeasureExpr::CountStar => Ok(Self::Star),
            MeasureExpr::Column(column)
                if column.relation_oid == spec.fact_rel
                    && column.attno > 0
                    && matches!(
                        column.type_oid,
                        BOOLOID
                            | INT2OID
                            | INT4OID
                            | INT8OID
                            | FLOAT4OID
                            | FLOAT8OID
                            | DATEOID
                            | TIMESTAMPOID
                            | TIMESTAMPTZOID
                    ) =>
            {
                Ok(Self::Column(column))
            }
            _ => Err("H3 hash COUNT input is not a supported fact column".into()),
        }
    }
}

struct BoundHashDescriptor {
    desc: abi::PgaccelGroupedAggDesc,
    // Must outlive the synchronous plan/dispatch consuming desc's raw pointer.
    _filter: Option<crate::gpu::SqlBoolMask>,
    _where: Option<crate::gpu::SqlBoolMask>,
}

struct H3FilterInput<'a> {
    values: &'a crate::gpu::ExprDeviceBuffer<u8>,
    nulls: Option<&'a crate::gpu::ExprDeviceBuffer<u8>>,
}

fn resolve_h3_filter<'a>(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    views: &'a [ResidentColumnView<'a>],
    rows: usize,
) -> Result<Option<H3FilterInput<'a>>, String> {
    resolve_hash_boolean_input(h3_count_filter(spec)?, requests, views, rows)
}

fn resolve_hash_boolean_input<'a>(
    column: Option<ColumnRef>,
    requests: &[ResidentColumnRef],
    views: &'a [ResidentColumnView<'a>],
    rows: usize,
) -> Result<Option<H3FilterInput<'a>>, String> {
    let Some(column) = column else {
        return Ok(None);
    };
    let view = find_view(requests, views, column)?;
    match view {
        ResidentColumnView::Empty { .. } if rows == 0 => Ok(None),
        ResidentColumnView::Bool { values, nulls, .. } => {
            validate_h3_input_lengths(
                values.len(),
                nulls.map(crate::gpu::ExprDeviceBuffer::len),
                rows,
            )?;
            Ok(Some(H3FilterInput {
                values,
                nulls: *nulls,
            }))
        }
        _ => Err("hash predicate has no exact resident Boolean lane".into()),
    }
}

/// Called only inside ensure_device_derived_artifact_with_report after its
/// whole-artifact reservation. Do not acquire a second query-local charge.
fn build_cached_h3_filter(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    rows: usize,
    pending: &mut dyn FnMut() -> bool,
) -> Result<Option<crate::gpu::ExprDeviceBuffer<i8>>, ResidentLoadError> {
    let Some(input) = resolve_h3_filter(spec, requests, views, rows)? else {
        return Ok(None);
    };
    let mask = crate::gpu::ExprDeviceBuffer::<i8>::new(rows)
        .ok_or_else(|| "could not allocate reserved cached H3 FILTER mask".to_owned())?;
    if !crate::gpu::write_sql_bool_mask_with_poll(input.values, input.nulls, &mask, pending)
        .map_err(ResidentLoadError::Gpu)?
    {
        return Err(ResidentLoadError::PreparationInterrupted);
    }
    Ok(Some(mask))
}

fn bind_h3_filter(
    mut desc: abi::PgaccelGroupedAggDesc,
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    pending: &mut dyn FnMut() -> bool,
) -> Result<BoundHashDescriptor, ResidentLoadError> {
    let where_mask = if let Some(input) =
        resolve_hash_boolean_input(hash_where_filter(spec)?, requests, views, desc.row_count)?
    {
        Some(
            crate::gpu::SqlBoolMask::from_resident_with_poll(
                input.values,
                input.nulls,
                &mut *pending,
            )
            .map_err(ResidentLoadError::Gpu)?
            .ok_or(ResidentLoadError::PreparationInterrupted)?,
        )
    } else {
        None
    };
    let filter = if let Some(input) = resolve_h3_filter(spec, requests, views, desc.row_count)? {
        Some(
            crate::gpu::SqlBoolMask::from_resident_with_poll(input.values, input.nulls, pending)
                .map_err(ResidentLoadError::Gpu)?
                .ok_or(ResidentLoadError::PreparationInterrupted)?,
        )
    } else {
        if where_mask.is_none() && pending() {
            return Err(ResidentLoadError::PreparationInterrupted);
        }
        None
    };
    if let Some(filter) = &filter {
        desc.measure_filters[0] = disabled_filter();
        desc.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.measure_filters[0].mask = filter.as_ptr();
    }
    if let Some(mask) = &where_mask {
        desc.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.where_filter.mask = mask.as_ptr();
    }
    Ok(BoundHashDescriptor {
        desc,
        _filter: filter,
        _where: where_mask,
    })
}

fn validate_h3_input_lengths(
    values: usize,
    nulls: Option<usize>,
    rows: usize,
) -> Result<(), String> {
    if values != rows || nulls.is_some_and(|len| len != rows) {
        return Err("H3 hash input value/NULL lengths differ from fact rows".into());
    }
    Ok(())
}

/// Bind only while the caller's residency borrow pins both the grouping and
/// measure columns. This staged binding does not authorize SQL admission.
fn bind_hash_measure(
    desc: &mut abi::PgaccelGroupedAggDesc,
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
) -> Result<(), String> {
    let input = HashMeasureInput::from_spec(spec)?;
    if requests.len() != views.len()
        || requests
            .iter()
            .enumerate()
            .any(|(index, request)| requests[..index].iter().any(|previous| previous == request))
    {
        return Err("H3 hash resident binding bundle is ambiguous".into());
    }
    let column = match input {
        HashMeasureInput::Column(column) | HashMeasureInput::IntegerStates(column) => column,
        HashMeasureInput::Star | HashMeasureInput::FilteredStar => return Ok(()),
    };
    let view = find_view(requests, views, column)?;
    let nulls = match view {
        ResidentColumnView::Empty { .. } => None,
        ResidentColumnView::Bool { nulls, .. }
        | ResidentColumnView::I32 { nulls, .. }
        | ResidentColumnView::I64 { nulls, .. }
        | ResidentColumnView::F32 { nulls, .. }
        | ResidentColumnView::F64 { nulls, .. } => nulls.map(crate::gpu::ExprDeviceBuffer::len),
        _ => return Err("H3 hash COUNT input is not a fixed-width resident lane".into()),
    };
    validate_h3_input_lengths(view.len(), nulls, desc.row_count)?;
    desc.measures[0].value = measure_column(view, desc.row_count)?;
    desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
    if matches!(input, HashMeasureInput::IntegerStates(_)) {
        if desc.measures[0].value.physical_type != abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32 {
            return Err("hash integer states have no exact resident INT32 lane".into());
        }
        desc.measures[0].agg_mask = hash_integer_lanes(&spec.measures[0]);
    }
    Ok(())
}

fn build_h3_descriptor(
    artifact: &H3ParentArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    cell: ColumnRef,
) -> Result<BoundHashDescriptor, String> {
    if hash_where_filter(&artifact.resolved_spec)?.is_some() {
        return Err("cached H3 parents cannot precede WHERE evaluation".into());
    }
    let source = find_view(requests, views, cell)?;
    let source_nulls = match source {
        ResidentColumnView::Empty { type_oid }
            if artifact.fact_rows == 0 && u32::from(*type_oid) == cell.type_oid =>
        {
            std::ptr::null()
        }
        ResidentColumnView::H3 {
            type_oid,
            values,
            nulls,
        } if u32::from(*type_oid) == cell.type_oid && values.len() == artifact.fact_rows => {
            validate_h3_input_lengths(
                values.len(),
                nulls.map(crate::gpu::ExprDeviceBuffer::len),
                artifact.fact_rows,
            )?;
            null_ptr(*nulls)
        }
        _ => {
            return Err("H3 descriptor source is not the planned resident h3index lane".to_owned());
        }
    };
    if artifact
        .parents
        .as_ref()
        .map_or(0, crate::gpu::ExprDeviceBuffer::len)
        != artifact.fact_rows
    {
        return Err("H3 parent artifact length changed before dispatch".to_owned());
    }

    let mut desc = hash_count_descriptor(
        artifact.fact_rows,
        artifact.group_capacity(),
        artifact
            .parents
            .as_ref()
            .map_or(std::ptr::null(), |parents| {
                parents.as_ptr().cast::<c_void>()
            }),
        source_nulls,
        0,
        0,
    );
    bind_hash_measure(&mut desc, &artifact.resolved_spec, requests, views)?;
    let input = resolve_h3_filter(&artifact.resolved_spec, requests, views, desc.row_count)?;
    let expected = if input.is_some() { desc.row_count } else { 0 };
    if artifact
        .filter_mask
        .as_ref()
        .map_or(0, crate::gpu::ExprDeviceBuffer::len)
        != expected
    {
        return Err("cached H3 FILTER mask changed length before dispatch".into());
    }
    if let Some(mask) = &artifact.filter_mask {
        desc.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        desc.measure_filters[0].mask = mask.as_ptr();
    }
    Ok(BoundHashDescriptor {
        desc,
        _filter: None,
        _where: None,
    })
}

fn build_fused_hash_descriptor(
    artifact: &FusedHashArtifact,
    requests: &[ResidentColumnRef],
    views: &[ResidentColumnView<'_>],
    cell: ColumnRef,
    pending: &mut dyn FnMut() -> bool,
) -> Result<BoundHashDescriptor, ResidentLoadError> {
    let raw = raw_hash_group(&artifact.resolved_spec);
    let (flags, transform_arg) = if let Some(column) = raw {
        if column != cell || artifact.resolution != 0 {
            return Err("raw hash key identity/transform metadata is not canonical"
                .to_owned()
                .into());
        }
        (0, 0)
    } else {
        let (planned_cell, resolution, _) =
            h3_parent_group(&artifact.resolved_spec).ok_or_else(|| {
                "compact hash requires an integer fact key or H3 parent key".to_owned()
            })?;
        if planned_cell != cell || resolution != artifact.resolution {
            return Err(
                "fused H3 key identity/resolution differs from its specification"
                    .to_owned()
                    .into(),
            );
        }
        let resolution = u32::try_from(resolution)
            .ok()
            .filter(|value| *value <= 15)
            .ok_or_else(|| "fused H3 parent resolution is outside 0..=15".to_owned())?;
        (abi::PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT, resolution)
    };
    let source = find_view(requests, views, cell)?;
    let (values, nulls) = match source {
        ResidentColumnView::Empty { type_oid }
            if artifact.fact_rows == 0 && u32::from(*type_oid) == cell.type_oid =>
        {
            (std::ptr::null(), std::ptr::null())
        }
        ResidentColumnView::H3 {
            type_oid,
            values,
            nulls,
        } if raw.is_none()
            && u32::from(*type_oid) == cell.type_oid
            && values.len() == artifact.fact_rows =>
        {
            validate_h3_input_lengths(
                values.len(),
                nulls.map(crate::gpu::ExprDeviceBuffer::len),
                artifact.fact_rows,
            )?;
            (values.as_ptr().cast::<c_void>(), null_ptr(*nulls))
        }
        ResidentColumnView::I64 { values, nulls, .. }
            if raw.is_some() && cell.type_oid == INT8OID =>
        {
            validate_h3_input_lengths(
                values.len(),
                nulls.map(crate::gpu::ExprDeviceBuffer::len),
                artifact.fact_rows,
            )?;
            (values.as_ptr().cast::<c_void>(), null_ptr(*nulls))
        }
        ResidentColumnView::I32 { values, nulls, .. }
            if raw.is_some() && matches!(cell.type_oid, INT2OID | INT4OID) =>
        {
            validate_h3_input_lengths(
                values.len(),
                nulls.map(crate::gpu::ExprDeviceBuffer::len),
                artifact.fact_rows,
            )?;
            (values.as_ptr().cast::<c_void>(), null_ptr(*nulls))
        }
        _ => {
            return Err("fused H3 source is not the planned resident h3index lane"
                .to_owned()
                .into());
        }
    };
    let mut desc = hash_count_descriptor(
        artifact.fact_rows,
        artifact.group_capacity.get(),
        values,
        nulls,
        flags,
        transform_arg,
    );
    if raw.is_some() && matches!(cell.type_oid, INT2OID | INT4OID) {
        desc.keys[0].values.tag = PgaccelValTag::Int32;
    }
    bind_hash_measure(&mut desc, &artifact.resolved_spec, requests, views)?;
    bind_h3_filter(desc, &artifact.resolved_spec, requests, views, pending)
}

fn verify_native_one_shot_mode(
    plan: &ResolvedGroupedAggPlan<'_>,
    expected: GroupedAggKernelMode,
) -> Result<(), DescriptorAggExecutionError> {
    let native = plan
        .one_shot_kernel_mode()
        .map_err(|error| gpu_execution_error("grouped physical-mode validation failed", error))?;
    if native != expected {
        return Err(DescriptorAggExecutionError::Failure(format!(
            "planned grouped physical mode {} does not match native one-shot mode {}",
            expected.label(),
            native.label(),
        )));
    }
    Ok(())
}

fn build_and_execute_spatial(
    inputs: ResolvedDerivedInputs<'_, SpatialAggArtifact>,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
    expected_kernel_mode: GroupedAggKernelMode,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::DerivedInputBinding, || {
            build_spatial_descriptor(inputs.artifact, requests, &inputs.columns)
        })
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: the composite artifact owns the base descriptor lanes and final
    // SQL mask for the duration of this synchronous residency callback.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }
        .map_err(|error| gpu_execution_error("spatial aggregate descriptor rejected", error))?;
    verify_native_one_shot_mode(&plan, expected_kernel_mode)?;
    plan.preflight_one_shot_memory()
        .map_err(|error| gpu_execution_error("spatial aggregate memory preflight failed", error))?;
    let mut storage =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::OutputAllocation, || {
            GroupedAggOutputStorage::new(&plan)
        })
        .map_err(|error| {
            gpu_execution_error("spatial aggregate output allocation failed", error)
        })?;
    let mut workspace =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::WorkspaceAllocation, || {
            GroupedAggWorkspace::allocate(&plan)
        })
        .map_err(|error| {
            gpu_execution_error("spatial aggregate workspace allocation failed", error)
        })?;
    let dispatch_started = Instant::now();
    let (outcome, kernel_mode) =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::KernelExecution, || {
            execute_grouped_agg_one_shot_with_workspace_and_mode(
                &plan,
                &mut storage,
                &mut workspace,
            )
        })
        .map_err(|error| gpu_execution_error("spatial aggregate kernel failed", error))?;
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        inputs.artifact.base.domains.clone(),
        &inputs.artifact.base.resolved_spec,
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.base.fact_rows,
        planned_batches: 1,
        batches_executed: 1,
        physical_mode_calls: 1,
        dispatch_time_us,
        kernel_mode,
        residency: None,
    })
}

fn build_and_execute_h3_one_shot(
    inputs: ResolvedDerivedInputs<'_, H3ParentArtifact>,
    requests: &[ResidentColumnRef],
    cell: ColumnRef,
    projection: &AggOutputProjection,
    expected_kernel_mode: GroupedAggKernelMode,
) -> Result<DescriptorAggDispatch, DescriptorAggExecutionError> {
    let desc =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::DerivedInputBinding, || {
            build_h3_descriptor(inputs.artifact, requests, &inputs.columns, cell)
        })
        .map_err(DescriptorAggExecutionError::Failure)?;
    // SAFETY: hash/H3 is deliberately one-shot because its native owner
    // indexes are relative to this exact row pointer range. Every pointer is
    // pinned by the active residency callback through the synchronous call.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }
        .map_err(|error| gpu_execution_error("H3 grouped descriptor rejected", error))?;
    verify_native_one_shot_mode(&plan, expected_kernel_mode)?;
    plan.preflight_hash_memory()
        .map_err(|error| gpu_execution_error("H3 grouped memory preflight failed", error))?;
    let mut storage =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::OutputAllocation, || {
            GroupedAggOutputStorage::new(&plan)
        })
        .map_err(|error| gpu_execution_error("H3 grouped output allocation failed", error))?;
    let mut workspace =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::WorkspaceAllocation, || {
            GroupedAggWorkspace::allocate(&plan)
        })
        .map_err(|error| gpu_execution_error("H3 grouped workspace allocation failed", error))?;
    let dispatch_started = Instant::now();
    let (outcome, kernel_mode) =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::KernelExecution, || {
            execute_grouped_agg_one_shot_with_workspace_and_mode(
                &plan,
                &mut storage,
                &mut workspace,
            )
        })
        .map_err(|error| gpu_execution_error("H3 grouped kernel failed", error))?;
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    validate_hash_compact_outcome(
        outcome,
        &storage,
        inputs.artifact.fact_rows,
        &inputs.artifact.resolved_spec,
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    if let GroupedAggOutcome::Complete(result) = outcome {
        inputs
            .artifact
            .record_validated_groups(result.emitted_group_count)
            .map_err(DescriptorAggExecutionError::Failure)?;
    }
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        std::rc::Rc::from(Vec::<super::artifact::GroupDomain>::new()),
        &inputs.artifact.resolved_spec,
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.fact_rows,
        planned_batches: 1,
        batches_executed: 1,
        physical_mode_calls: 1,
        dispatch_time_us,
        kernel_mode,
        residency: None,
    })
}

fn build_and_execute_fused_hash_one_shot(
    inputs: ResolvedDerivedInputs<'_, FusedHashArtifact>,
    requests: &[ResidentColumnRef],
    cell: ColumnRef,
    projection: &AggOutputProjection,
    expected_kernel_mode: GroupedAggKernelMode,
    pending: &mut dyn FnMut() -> bool,
) -> Result<DescriptorAggDispatch, DescriptorDispatchFailure> {
    let desc =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::DerivedInputBinding, || {
            build_fused_hash_descriptor(inputs.artifact, requests, &inputs.columns, cell, pending)
        })
        .map_err(DescriptorDispatchFailure::Residency)?;
    if pending() {
        return Err(DescriptorDispatchFailure::Residency(
            ResidentLoadError::PreparationInterrupted,
        ));
    }
    // SAFETY: raw integer/H3 inputs and query-owned metadata are pinned through
    // this synchronous callback. The native row loader preserves BIGINT bits
    // or validates/transforms H3 cells, according to the exact descriptor flag.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }
        .map_err(|error| gpu_execution_error("compact hash grouped descriptor rejected", error))?;
    verify_native_one_shot_mode(&plan, expected_kernel_mode)?;
    plan.preflight_hash_memory()
        .map_err(|error| gpu_execution_error("compact hash memory preflight failed", error))?;
    let mut storage =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::OutputAllocation, || {
            GroupedAggOutputStorage::new(&plan)
        })
        .map_err(|error| gpu_execution_error("compact hash output allocation failed", error))?;
    let mut workspace =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::WorkspaceAllocation, || {
            GroupedAggWorkspace::allocate(&plan)
        })
        .map_err(|error| gpu_execution_error("compact hash workspace allocation failed", error))?;
    let dispatch_started = Instant::now();
    let (outcome, kernel_mode) =
        stats::profile_descriptor_stage(stats::DescriptorExecStage::KernelExecution, || {
            execute_grouped_agg_one_shot_with_workspace_and_mode(
                &plan,
                &mut storage,
                &mut workspace,
            )
        })
        .map_err(|error| {
            if error.status == GpuStatusDetail::NumericOverflow {
                DescriptorAggExecutionError::NumericOverflow
            } else {
                DescriptorAggExecutionError::ExternalRoutineException(format!(
                    "compact hash aggregate rejected an input: {error}"
                ))
            }
        })?;
    if pending() {
        return Err(DescriptorDispatchFailure::Residency(
            ResidentLoadError::PreparationInterrupted,
        ));
    }
    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    validate_hash_compact_outcome(
        outcome,
        &storage,
        inputs.artifact.fact_rows,
        &inputs.artifact.resolved_spec,
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    let output = DescriptorAggOutput::new(
        storage,
        outcome,
        std::rc::Rc::from(Vec::<super::artifact::GroupDomain>::new()),
        &inputs.artifact.resolved_spec,
        projection.clone(),
    )
    .map_err(DescriptorAggExecutionError::Failure)?;
    if let GroupedAggOutcome::Complete(result) = outcome {
        inputs
            .artifact
            .record_validated_raw_groups(result.emitted_group_count)
            .map_err(DescriptorAggExecutionError::Failure)?;
    }
    Ok(DescriptorAggDispatch {
        output,
        fact_rows: inputs.artifact.fact_rows,
        planned_batches: 1,
        batches_executed: 1,
        physical_mode_calls: 1,
        dispatch_time_us,
        kernel_mode,
        residency: None,
    })
}

enum DescriptorDispatchFailure {
    Residency(ResidentLoadError),
    Execution(DescriptorAggExecutionError),
}

impl From<DescriptorAggExecutionError> for DescriptorDispatchFailure {
    fn from(error: DescriptorAggExecutionError) -> Self {
        Self::Execution(error)
    }
}

enum DenseBoundedFailure {
    Dispatch(DescriptorDispatchFailure),
    Interrupt(Box<pg_sys::panic::CaughtError>),
}

enum DenseExecutionSetup {
    OneShot {
        dispatch: Box<DescriptorAggDispatch>,
        elapsed: Duration,
    },
    Bounded {
        session: Box<GroupedAggSession>,
        fact_rows: usize,
        chunk_rows: usize,
    },
}

#[cfg(feature = "pg_test")]
static TEST_DENSE_CHUNK_ROWS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_ONE_SHOT_MAX_ROWS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_TIMEOUT_AFTER_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "pg_test")]
static TEST_DENSE_COMPLETED_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Configure the live PostgreSQL cancellation fixture. Zero chunk and
/// one-shot overrides preserve their independent production device limits; a
/// zero timeout count disables the timeout arm. This code is absent from
/// release builds.
#[cfg(feature = "pg_test")]
pub fn configure_dense_dispatch_test(
    chunk_rows: usize,
    one_shot_max_rows: usize,
    timeout_after_calls: usize,
) -> (usize, usize, usize) {
    let previous_chunk_rows = TEST_DENSE_CHUNK_ROWS.swap(chunk_rows, Ordering::SeqCst);
    let previous_one_shot_max_rows =
        TEST_DENSE_ONE_SHOT_MAX_ROWS.swap(one_shot_max_rows, Ordering::SeqCst);
    let previous_timeout_after =
        TEST_DENSE_TIMEOUT_AFTER_CALLS.swap(timeout_after_calls, Ordering::SeqCst);
    TEST_DENSE_COMPLETED_CALLS.store(0, Ordering::SeqCst);
    (
        previous_chunk_rows,
        previous_one_shot_max_rows,
        previous_timeout_after,
    )
}

#[cfg(feature = "pg_test")]
#[must_use]
pub fn dense_dispatch_test_completed_calls() -> usize {
    TEST_DENSE_COMPLETED_CALLS.load(Ordering::SeqCst)
}

fn effective_dense_chunk_rows(device_limit: usize) -> usize {
    #[cfg(feature = "pg_test")]
    {
        let test_limit = TEST_DENSE_CHUNK_ROWS.load(Ordering::SeqCst);
        if test_limit != 0 {
            return test_limit.min(device_limit);
        }
    }
    device_limit
}

/// Bound every synchronous dense aggregate transition by both the allocation-
/// derived reduce limit and the compute-derived grouped-aggregate limit. The
/// latter is especially important for a bounded lifecycle: taking the
/// multi-call path must not let one ACCUMULATE command buffer exceed the same
/// physical-device work envelope that made the one-shot path ineligible.
fn dense_session_chunk_rows(max_chunk_rows: usize, synchronous_row_cap: usize) -> usize {
    max_chunk_rows.min(synchronous_row_cap)
}

fn clamp_dense_one_shot_row_cap(device_limit: usize, test_override: usize) -> usize {
    let production_limit = device_limit.min(GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS);
    if test_override == 0 {
        production_limit
    } else {
        test_override.min(production_limit)
    }
}

fn dense_one_shot_row_cap(device_limit: usize) -> usize {
    #[cfg(feature = "pg_test")]
    let test_override = TEST_DENSE_ONE_SHOT_MAX_ROWS.load(Ordering::SeqCst);
    #[cfg(not(feature = "pg_test"))]
    let test_override = 0;
    clamp_dense_one_shot_row_cap(device_limit, test_override)
}

fn canonical_null_value(value: &PgaccelVal) -> bool {
    value.tag == PgaccelValTag::Null && value.data == 0
}

fn canonical_parallel_filter_fields(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    filter.predicate_source == 0
        && filter.predicate_measure_slot == 0
        && filter.predicate_range_count == 0
        && filter.value_cmp_opcode == crate::engine::expr_compiler::opcode::ALWAYS_TRUE
        && filter.pad0 == 0
        && filter.flags == 0
        && canonical_null_value(&filter.value_cmp_const)
        && filter.predicate_lo.iter().all(canonical_null_value)
        && filter.predicate_hi.iter().all(canonical_null_value)
}

fn canonical_disabled_filter(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_NONE
        && filter.mask.is_null()
        && canonical_parallel_filter_fields(filter)
}

fn parallel_dense_filter_kind(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    canonical_disabled_filter(filter)
        || (filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL
            && !filter.mask.is_null()
            && canonical_parallel_filter_fields(filter))
}

fn canonical_dense_integer_range_filter(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    let lo = filter.predicate_lo[0].data as i32;
    let hi = filter.predicate_hi[0].data as i32;
    filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_NONE
        && filter.mask.is_null()
        && filter.predicate_source == abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE
        && filter.predicate_measure_slot == 0
        && filter.predicate_range_count == 1
        && filter.value_cmp_opcode == crate::engine::expr_compiler::opcode::ALWAYS_TRUE
        && filter.pad0 == 0
        && filter.flags == 0
        && canonical_null_value(&filter.value_cmp_const)
        && filter.predicate_lo[0].tag == PgaccelValTag::Int32
        && filter.predicate_hi[0].tag == PgaccelValTag::Int32
        && lo != i32::MIN
        && hi != i32::MAX
        && lo < hi
        && filter.predicate_lo[1..].iter().all(canonical_null_value)
        && filter.predicate_hi[1..].iter().all(canonical_null_value)
}

fn parallel_dense_integer_filter_kind(filter: &abi::PgaccelGroupedAggFilter) -> bool {
    parallel_dense_filter_kind(filter) || canonical_dense_integer_range_filter(filter)
}

fn parallel_dense_unique_dimensions(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    let Ok(dim_count) = usize::try_from(desc.dim_count) else {
        return false;
    };
    dim_count <= abi::PGACCEL_GROUPED_AGG_MAX_DIMS
        && desc.dims[..dim_count]
            .iter()
            .all(|dim| dim.multiplicity_by_key.is_null())
}

fn parallel_dense_count_shape(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.measure_count != 1
        || !parallel_dense_filter_kind(&desc.where_filter)
        || !canonical_disabled_filter(&desc.measure_filters[0])
    {
        return false;
    }
    let count = &desc.measures[0];
    let count_star = count.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
    let count_typed_column = count.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        && ((count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
            && count.value.element_bytes == 1)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                && count.value.element_bytes == 4)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64
                && count.value.element_bytes == 8)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE
                && count.value.element_bytes == 4)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP
                && count.value.element_bytes == 8)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32
                && count.value.element_bytes == 4)
            || (count.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64
                && count.value.element_bytes == 8));
    let canonical_count = (count_star || count_typed_column)
        && count.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        && count.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
        && count.state_bytes == 8;
    if !canonical_count {
        return false;
    }
    if parallel_dense_unique_dimensions(desc) {
        return true;
    }
    count_star
        && desc.key_count == 0
        && desc.group_capacity == 1
        && desc.dim_count != 0
        && canonical_disabled_filter(&desc.where_filter)
}

fn exact_dense_integer_measure_filter(
    desc: &abi::PgaccelGroupedAggDesc,
    filter: &abi::PgaccelGroupedAggFilter,
    rhs_input: bool,
) -> bool {
    if canonical_disabled_filter(filter) {
        return true;
    }
    let Ok(slot) = usize::try_from(filter.predicate_measure_slot) else {
        return false;
    };
    let Ok(ranges) = usize::try_from(filter.predicate_range_count) else {
        return false;
    };
    let accept_null = filter.flags == abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
    if slot >= desc.measure_count as usize
        || slot >= abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || (ranges == 0 && !accept_null)
        || ranges > abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES
        || filter.kind != abi::PGACCEL_GROUPED_AGG_FILTER_NONE
        || !filter.mask.is_null()
        || filter.predicate_source
            != if rhs_input {
                abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
            } else {
                abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE
            }
        || filter.value_cmp_opcode != crate::engine::expr_compiler::opcode::ALWAYS_TRUE
        || filter.pad0 != 0
        || (filter.flags != 0 && !accept_null)
        || !canonical_null_value(&filter.value_cmp_const)
    {
        return false;
    }
    let source = &desc.measures[slot];
    let column = if rhs_input {
        &source.rhs
    } else {
        &source.value
    };
    (source.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        || (rhs_input && source.op == abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR)
        || (!rhs_input && integer_binary_op(source.op)))
        && column.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        && column.element_bytes == 4
        && (0..ranges).all(|range| {
            let lo = filter.predicate_lo[range];
            let hi = filter.predicate_hi[range];
            lo.tag == PgaccelValTag::Int32
                && hi.tag == PgaccelValTag::Int32
                && (lo.data as i32) <= (hi.data as i32)
        })
        && filter.predicate_lo[ranges..]
            .iter()
            .all(canonical_null_value)
        && filter.predicate_hi[ranges..]
            .iter()
            .all(canonical_null_value)
}

fn exact_dense_where_ranges(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    let mut range = desc.where_filter;
    if range.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL && !range.mask.is_null() {
        range.kind = abi::PGACCEL_GROUPED_AGG_FILTER_NONE;
        range.mask = std::ptr::null();
    }
    exact_dense_integer_measure_filter(desc, &range, false)
}

fn exact_dense_scalar_where(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    let mut filter = desc.where_filter;
    if filter.predicate_source != abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR
        || filter.predicate_measure_slot < 0
        || filter.predicate_measure_slot as u32 >= desc.measure_count
        || filter.predicate_measure_slot as usize >= abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
    {
        return false;
    }
    let slot = filter.predicate_measure_slot as usize;
    let measure = &desc.measures[slot];
    if measure.op != abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR
        || measure.value.physical_type != abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
        || !canonical_disabled_filter(&desc.measure_filters[slot])
    {
        return false;
    }
    filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
    filter.predicate_measure_slot = 0;
    parallel_dense_filter_kind(&filter)
}

fn integer_binary_op(op: i32) -> bool {
    matches!(
        op,
        abi::PGACCEL_GROUPED_AGG_MEASURE_MUL
            | abi::PGACCEL_GROUPED_AGG_MEASURE_SUB
            | abi::PGACCEL_GROUPED_AGG_MEASURE_ADD
    )
}

fn parallel_dense_multi_integer_shape(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    if parallel_dense_count_shape(desc) {
        return false;
    }
    let count = desc.measure_count as usize;
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.group_capacity == 0
        || !(1..=abi::PGACCEL_GROUPED_AGG_MAX_MEASURES).contains(&count)
        || desc.dim_count != 0
        || !(parallel_dense_filter_kind(&desc.where_filter)
            || exact_dense_where_ranges(desc)
            || exact_dense_scalar_where(desc))
    {
        return false;
    }
    let values = count
        - usize::from(desc.measures[count - 1].op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR);
    values > 0
        && (0..count).all(|slot| {
            let measure = &desc.measures[slot];
            let predicate_input =
                measure.flags == abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT;
            if predicate_input {
                if slot >= values
                    || desc.measure_filters[slot].predicate_measure_slot != slot as i32
                    || (desc.measure_filters[slot].predicate_range_count == 0
                        && desc.measure_filters[slot].flags
                            != abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL)
                {
                    return false;
                }
            } else if measure.flags != 0 {
                return false;
            }
            measure.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
                && measure.state_bytes == 8
                && if slot < values {
                    exact_dense_integer_measure_filter(
                        desc,
                        &desc.measure_filters[slot],
                        predicate_input,
                    ) && (measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
                        || measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR
                        || integer_binary_op(measure.op))
                        && measure.agg_mask != 0
                        && measure.agg_mask
                            & !(abi::PGACCEL_GROUPED_AGG_LANE_SUM
                                | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                                | abi::PGACCEL_GROUPED_AGG_LANE_MAX)
                            == 0
                        && if measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR {
                            measure.value.flags == abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1
                                && measure.value.element_bytes as usize
                                    == size_of::<abi::PgaccelScalarView>()
                                && measure.value.scale == 0
                                && measure.value.nulls.is_null()
                                && !measure.value.values.is_null()
                                && (measure.value.physical_type
                                    == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                                    || (measure.value.physical_type
                                        == abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE
                                        && measure.agg_mask & abi::PGACCEL_GROUPED_AGG_LANE_SUM
                                            == 0)
                                    || (measure.value.physical_type
                                        == abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
                                        && measure.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT))
                        } else {
                            measure.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                                && measure.value.element_bytes == 4
                        }
                        && (!integer_binary_op(measure.op)
                            || (measure.rhs.physical_type
                                == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                                && measure.rhs.element_bytes == 4))
                } else {
                    canonical_disabled_filter(&desc.measure_filters[slot])
                        && measure.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                }
        })
}

fn parallel_dense_integer_shape(desc: &abi::PgaccelGroupedAggDesc) -> bool {
    if parallel_dense_multi_integer_shape(desc) {
        return true;
    }
    let measure_range = canonical_dense_integer_range_filter(&desc.measure_filters[0]);
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.group_capacity == 0
        || !matches!(desc.measure_count, 1 | 2)
        || !parallel_dense_integer_filter_kind(&desc.where_filter)
        || !parallel_dense_unique_dimensions(desc)
        || (!canonical_disabled_filter(&desc.measure_filters[0]) && !measure_range)
        || (desc.measure_count == 2 && !canonical_disabled_filter(&desc.measure_filters[1]))
    {
        return false;
    }
    let value = &desc.measures[0];
    let count = &desc.measures[1];
    let direct_sum = value.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        && (value.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM
            || value.agg_mask
                == (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                    | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                    | abi::PGACCEL_GROUPED_AGG_LANE_MAX));
    let product_sum = value.op == abi::PGACCEL_GROUPED_AGG_MEASURE_MUL
        && value.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM
        && value.rhs.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        && value.rhs.element_bytes == 4;
    (direct_sum || product_sum)
        && (!canonical_dense_integer_range_filter(&desc.where_filter)
            || (product_sum && desc.key_count == 1 && desc.dim_count == 0))
        && (!measure_range
            || (direct_sum
                && value.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM
                && canonical_disabled_filter(&desc.where_filter)
                && desc.key_count == 1
                && desc.dim_count == 0))
        && value.value.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        && value.value.element_bytes == 4
        && value.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
        && value.state_bytes == 8
        && (desc.measure_count == 1
            || (count.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
                && count.agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                && count.accumulator_kind == abi::PGACCEL_GROUPED_AGG_ACCUM_I64
                && count.state_bytes == 8))
}

fn dense_partial_chunk_rows(desc: &abi::PgaccelGroupedAggDesc, requested: usize) -> usize {
    if !parallel_dense_integer_shape(desc) {
        return requested;
    }
    let count = desc.measure_count as usize;
    let values = count
        - usize::from(desc.measures[count - 1].op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR);
    desc.group_capacity
        .checked_mul(values)
        .and_then(|groups| {
            (DENSE_INTEGER_MAX_PARTIAL_BYTES / DENSE_INTEGER_PARTIAL_BYTES).checked_div(groups)
        })
        .and_then(|chunks| chunks.checked_mul(DENSE_INTEGER_CHUNK_ROWS))
        .map_or(0, |rows| requested.min(rows))
}

fn dense_one_shot_eligible(desc: &abi::PgaccelGroupedAggDesc, row_cap: usize) -> bool {
    if row_cap == 0 || desc.row_count > row_cap {
        return false;
    }
    if parallel_dense_count_shape(desc) {
        return true;
    }
    if !parallel_dense_integer_shape(desc) {
        return false;
    }
    if desc.measures[0].agg_mask == abi::PGACCEL_GROUPED_AGG_LANE_SUM {
        return true;
    }
    desc.group_capacity
        .checked_mul(desc.row_count.div_ceil(DENSE_INTEGER_CHUNK_ROWS))
        .and_then(|partials| partials.checked_mul(DENSE_INTEGER_PARTIAL_BYTES))
        .is_some_and(|bytes| bytes <= DENSE_INTEGER_MAX_PARTIAL_BYTES)
}

#[cfg(feature = "pg_test")]
unsafe extern "C" {
    fn enable_timeout_after(id: std::ffi::c_int, delay_ms: std::ffi::c_int);
    fn get_timeout_indicator(id: std::ffi::c_int, reset_indicator: bool) -> bool;
}

#[cfg(feature = "pg_test")]
fn note_dense_test_call_and_maybe_arm_timeout() {
    const STATEMENT_TIMEOUT_ID: std::ffi::c_int = 3;

    let completed = TEST_DENSE_COMPLETED_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    if completed != TEST_DENSE_TIMEOUT_AFTER_CALLS.load(Ordering::SeqCst) {
        return;
    }

    // SAFETY: this pg_test-only hook runs on the initialized PostgreSQL main
    // backend thread. TimeoutId::STATEMENT_TIMEOUT has stable discriminant 3.
    unsafe { enable_timeout_after(STATEMENT_TIMEOUT_ID, 1) };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        // SAFETY: same backend-thread contract as enable_timeout_after. The
        // indicator remains set so the normal interrupt boundary consumes it.
        if unsafe { get_timeout_indicator(STATEMENT_TIMEOUT_ID, false) } {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "PostgreSQL statement timeout did not become pending"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(not(feature = "pg_test"))]
fn note_dense_test_call_and_maybe_arm_timeout() {}

fn warn_if_grouped_dispatch_slow(phase: &str, elapsed: Duration) {
    let threshold_ms = gucs::kernel_timeout_ms();
    if !dispatch_warning_threshold_exceeded(elapsed, threshold_ms) {
        return;
    }
    pgrx::warning!(
        "pg_accel: grouped aggregate {phase} dispatch returned after {}us (warning threshold {threshold_ms}ms); cancellation and statement_timeout are observed only between synchronous calls",
        elapsed.as_micros()
    );
}

trait DenseExecutionArtifact {
    fn base(&self) -> &DescriptorAggArtifact;
    fn bind(
        &self,
        requests: &[ResidentColumnRef],
        views: &[ResidentColumnView<'_>],
    ) -> Result<BoundDescriptor, String>;
}

impl DenseExecutionArtifact for DescriptorAggArtifact {
    fn base(&self) -> &DescriptorAggArtifact {
        self
    }
    fn bind(
        &self,
        requests: &[ResidentColumnRef],
        views: &[ResidentColumnView<'_>],
    ) -> Result<BoundDescriptor, String> {
        build_descriptor(self, requests, views)
    }
}

impl DenseExecutionArtifact for SpatialAggArtifact {
    fn base(&self) -> &DescriptorAggArtifact {
        &self.base
    }
    fn bind(
        &self,
        requests: &[ResidentColumnRef],
        views: &[ResidentColumnView<'_>],
    ) -> Result<BoundDescriptor, String> {
        build_spatial_descriptor(self, requests, views)
    }
}

trait DenseArtifactInputResolver {
    type Artifact: DenseExecutionArtifact;
    fn resolve<R>(
        &self,
        callback: impl FnOnce(ResolvedDerivedInputs<'_, Self::Artifact>) -> R,
    ) -> Result<R, ResidentLoadError>;
}

struct CachedDenseArtifactResolver<'a> {
    owner_relid: pg_sys::Oid,
    identity: &'a DerivedArtifactIdentity,
    requests: &'a [ResidentColumnRef],
}

impl DenseArtifactInputResolver for CachedDenseArtifactResolver<'_> {
    type Artifact = DescriptorAggArtifact;
    fn resolve<R>(
        &self,
        callback: impl FnOnce(ResolvedDerivedInputs<'_, DescriptorAggArtifact>) -> R,
    ) -> Result<R, ResidentLoadError> {
        with_derived_artifact_inputs(self.owner_relid, self.identity, self.requests, callback)
    }
}

/// Live integration probe for scalar dependency invalidation. Uses the real
/// artifact preparation, cached resolver and binder, bypassing admission only.
#[cfg(feature = "pg_test")]
pub fn test_scalar_resident_aggregate(
    spec: &AggQuerySpec,
    refresh: bool,
) -> Result<(Option<i64>, u64), String> {
    struct SnapshotGuard;
    impl Drop for SnapshotGuard {
        fn drop(&mut self) {
            // SAFETY: this helper pushes exactly one private snapshot below.
            unsafe { pg_sys::PopActiveSnapshot() };
        }
    }
    // SAFETY: the live test runs under an active outer SQL snapshot, whose
    // command ID predates its nested SPI writes. Match a fresh SQL statement
    // using a private current-command copy without mutating the outer snapshot.
    unsafe {
        pg_sys::PushCopiedSnapshot(pg_sys::GetActiveSnapshot());
        pg_sys::UpdateActiveSnapshotCommandId();
    }
    let _snapshot = SnapshotGuard;
    if !spec.group_keys.is_empty() || !spec.star_dims.is_empty() || spec.measures.len() != 1 {
        return Err("scalar currentness fixture requires one global measure".into());
    }
    scalar_measure_program(&spec.measures[0].expression, spec.fact_rel)?;
    let selected = selected_relations(spec)?;
    let dependencies: Vec<_> = selected.iter().map(|relation| relation.relid).collect();
    let requests = dispatch_column_refs(spec)?;
    let artifact_columns = artifact_column_refs(spec)?;
    let identity = DerivedArtifactIdentity::from_canonical_words(
        spec.encode_i32().map_err(|error| error.to_string())?,
    );
    let owner = pg_sys::Oid::from(spec.fact_rel);
    if refresh {
        ensure_selected_relations(&selected).map_err(|error| error.to_string())?;
        let host_bytes = descriptor_program_clone_bytes(spec)?;
        let before = resident_live_bytes();
        let report = ensure_host_device_derived_artifact_with_report(
            owner,
            &identity,
            &dependencies,
            &artifact_columns,
            host_bytes,
            |bundle| prepare_agg_artifact(spec, &artifact_columns, bundle, 1),
            DescriptorAggArtifact::build,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(report.accounting.retained_host_exact_bytes, host_bytes);
        assert_eq!(
            report.accounting.device_bytes, 0,
            "global unfiltered artifact has no derived device lanes"
        );
        assert_eq!(
            resident_live_bytes(),
            before
                + if report.outcome == ArtifactEnsureOutcome::Hit {
                    0
                } else {
                    host_bytes
                }
        );
        let before_local = resident_live_bytes();
        let local = build_ephemeral_host_device_derived_artifact_with_report(
            owner,
            &dependencies,
            &artifact_columns,
            host_bytes,
            |bundle| prepare_agg_artifact(spec, &artifact_columns, bundle, 1),
            DescriptorAggArtifact::build,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(local.accounting.retained_host_exact_bytes, host_bytes);
        assert_eq!(resident_live_bytes(), before_local + host_bytes);
        with_ephemeral_artifact_inputs(owner, &local.artifact, &requests, |inputs| {
            assert_eq!(inputs.artifact.retained_host_exact_bytes(), host_bytes);
            inputs.artifact.bind(&requests, &inputs.columns)
        })
        .map_err(|error| error.to_string())??;
        drop(local);
        assert_eq!(resident_live_bytes(), before_local);
    }
    let resolver = CachedDenseArtifactResolver {
        owner_relid: owner,
        identity: &identity,
        requests: &requests,
    };
    let projection = AggOutputProjection {
        slots: [AggregateKind::Sum, AggregateKind::Count]
            .into_iter()
            .map(|kind| crate::engine::spec::AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind,
                },
                source_type_oid: INT4OID,
                result_type_oid: INT8OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: kind != AggregateKind::Count,
            })
            .collect(),
    };
    let mut results = Vec::new();
    for one_shot_rows in [usize::MAX, 1024] {
        let dispatch = execute_bounded_dense_with_resolver(
            &resolver,
            &requests,
            &projection,
            1024,
            one_shot_rows,
            GroupedAggKernelMode::ParallelDenseInteger,
        )
        .map_err(|error| match error {
            DescriptorDispatchFailure::Residency(error) => error.to_string(),
            DescriptorDispatchFailure::Execution(error) => error.to_string(),
        })?;
        if dispatch.fact_rows > one_shot_rows {
            assert!(
                dispatch.batches_executed > 1,
                "fixture must exercise bounded execution"
            );
        } else {
            assert_eq!(dispatch.batches_executed, 1);
        }
        results.push(dispatch.output.test_global_int8_values()?);
    }
    assert_eq!(
        results[0], results[1],
        "one-shot and bounded SQL result conversion"
    );
    let count = results[0][1].ok_or("COUNT returned NULL")?;
    Ok((
        results[0][0],
        u64::try_from(count).map_err(|_| "negative COUNT")?,
    ))
}

struct EphemeralDenseArtifactResolver<'a> {
    owner_relid: pg_sys::Oid,
    artifact: &'a EphemeralDerivedArtifact<DescriptorAggArtifact>,
    requests: &'a [ResidentColumnRef],
}

impl DenseArtifactInputResolver for EphemeralDenseArtifactResolver<'_> {
    type Artifact = DescriptorAggArtifact;
    fn resolve<R>(
        &self,
        callback: impl FnOnce(ResolvedDerivedInputs<'_, DescriptorAggArtifact>) -> R,
    ) -> Result<R, ResidentLoadError> {
        with_ephemeral_artifact_inputs(self.owner_relid, self.artifact, self.requests, callback)
    }
}

struct CachedSpatialArtifactResolver<'a> {
    owner_relid: pg_sys::Oid,
    identity: &'a DerivedArtifactIdentity,
    requests: &'a [ResidentColumnRef],
}

impl DenseArtifactInputResolver for CachedSpatialArtifactResolver<'_> {
    type Artifact = SpatialAggArtifact;
    fn resolve<R>(
        &self,
        callback: impl FnOnce(ResolvedDerivedInputs<'_, SpatialAggArtifact>) -> R,
    ) -> Result<R, ResidentLoadError> {
        with_derived_artifact_inputs(self.owner_relid, self.identity, self.requests, callback)
    }
}

struct EphemeralSpatialArtifactResolver<'a> {
    owner_relid: pg_sys::Oid,
    artifact: &'a EphemeralDerivedArtifact<SpatialAggArtifact>,
    requests: &'a [ResidentColumnRef],
}

impl DenseArtifactInputResolver for EphemeralSpatialArtifactResolver<'_> {
    type Artifact = SpatialAggArtifact;
    fn resolve<R>(
        &self,
        callback: impl FnOnce(ResolvedDerivedInputs<'_, SpatialAggArtifact>) -> R,
    ) -> Result<R, ResidentLoadError> {
        with_ephemeral_artifact_inputs(self.owner_relid, self.artifact, self.requests, callback)
    }
}

fn execute_bounded_dense_artifact(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
    max_chunk_rows: usize,
    one_shot_max_rows: usize,
    expected_kernel_mode: GroupedAggKernelMode,
) -> Result<DescriptorAggDispatch, DescriptorDispatchFailure> {
    let resolver = CachedDenseArtifactResolver {
        owner_relid,
        identity,
        requests,
    };
    execute_bounded_dense_with_resolver(
        &resolver,
        requests,
        projection,
        max_chunk_rows,
        one_shot_max_rows,
        expected_kernel_mode,
    )
}

fn execute_bounded_dense_ephemeral(
    owner_relid: pg_sys::Oid,
    artifact: &EphemeralDerivedArtifact<DescriptorAggArtifact>,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
    max_chunk_rows: usize,
    one_shot_max_rows: usize,
    expected_kernel_mode: GroupedAggKernelMode,
) -> Result<DescriptorAggDispatch, DescriptorDispatchFailure> {
    let resolver = EphemeralDenseArtifactResolver {
        owner_relid,
        artifact,
        requests,
    };
    execute_bounded_dense_with_resolver(
        &resolver,
        requests,
        projection,
        max_chunk_rows,
        one_shot_max_rows,
        expected_kernel_mode,
    )
}

fn execute_bounded_dense_with_resolver(
    resolver: &impl DenseArtifactInputResolver,
    requests: &[ResidentColumnRef],
    projection: &AggOutputProjection,
    max_chunk_rows: usize,
    one_shot_max_rows: usize,
    expected_kernel_mode: GroupedAggKernelMode,
) -> Result<DescriptorAggDispatch, DescriptorDispatchFailure> {
    let bounded_chunk_rows =
        effective_dense_chunk_rows(dense_session_chunk_rows(max_chunk_rows, one_shot_max_rows));
    if bounded_chunk_rows == 0 {
        return Err(DescriptorDispatchFailure::Execution(
            DescriptorAggExecutionError::Failure(
                "grouped aggregate chunk limit is zero".to_owned(),
            ),
        ));
    }
    if let Some(caught) = capture_postgres_interrupt() {
        caught.rethrow();
    }

    let initialized = resolver
        .resolve(|inputs| {
            let desc = stats::profile_descriptor_stage(
                stats::DescriptorExecStage::DerivedInputBinding,
                || inputs.artifact.bind(requests, &inputs.columns),
            )
            .map_err(DescriptorAggExecutionError::Failure)?;
            // SAFETY: every active pointer is pinned by this exact residency
            // callback. The session copies only shape identity; it never
            // submits or dereferences those pointer values after return.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }
                .map_err(|error| gpu_execution_error("grouped descriptor rejected", error))?;
            if dense_one_shot_eligible(plan.descriptor(), dense_one_shot_row_cap(one_shot_max_rows))
            {
                verify_native_one_shot_mode(&plan, expected_kernel_mode)?;
                plan.preflight_one_shot_memory().map_err(|error| {
                    gpu_execution_error("grouped one-shot memory preflight failed", error)
                })?;
                let mut storage = stats::profile_descriptor_stage(
                    stats::DescriptorExecStage::OutputAllocation,
                    || GroupedAggOutputStorage::new(&plan),
                )
                .map_err(|error| {
                    gpu_execution_error("grouped one-shot output allocation failed", error)
                })?;
                let mut workspace = stats::profile_descriptor_stage(
                    stats::DescriptorExecStage::WorkspaceAllocation,
                    || GroupedAggWorkspace::allocate(&plan),
                )
                .map_err(|error| {
                    gpu_execution_error("grouped one-shot workspace allocation failed", error)
                })?;
                let dispatch_started = Instant::now();
                let (outcome, kernel_mode) = stats::profile_descriptor_stage(
                    stats::DescriptorExecStage::KernelExecution,
                    || {
                        execute_grouped_agg_one_shot_with_workspace_and_mode(
                            &plan,
                            &mut storage,
                            &mut workspace,
                        )
                    },
                )
                .map_err(|error| gpu_execution_error("grouped one-shot kernel failed", error))?;
                let elapsed = dispatch_started.elapsed();
                let output = DescriptorAggOutput::new(
                    storage,
                    outcome,
                    inputs.artifact.base().domains.clone(),
                    &inputs.artifact.base().resolved_spec,
                    projection.clone(),
                )
                .map_err(DescriptorAggExecutionError::Failure)?;
                return Ok(DenseExecutionSetup::OneShot {
                    dispatch: Box::new(DescriptorAggDispatch {
                        output,
                        fact_rows: inputs.artifact.base().fact_rows,
                        planned_batches: 1,
                        batches_executed: 1,
                        physical_mode_calls: 1,
                        dispatch_time_us: u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
                        kernel_mode,
                        residency: None,
                    }),
                    elapsed,
                });
            }
            let bounded_chunk_rows =
                dense_partial_chunk_rows(plan.descriptor(), bounded_chunk_rows);
            let session = stats::profile_descriptor_stage(
                stats::DescriptorExecStage::WorkspaceAllocation,
                || GroupedAggSession::start(&plan, bounded_chunk_rows),
            )
            .map_err(|error| gpu_execution_error("grouped session setup failed", error))?;
            if session.kernel_mode() != expected_kernel_mode {
                return Err(DescriptorAggExecutionError::Failure(format!(
                    "planned grouped physical mode {} does not match native bounded data mode {}",
                    expected_kernel_mode.label(),
                    session.kernel_mode().label(),
                )));
            }
            Ok::<_, DescriptorAggExecutionError>(DenseExecutionSetup::Bounded {
                session: Box::new(session),
                fact_rows: inputs.artifact.base().fact_rows,
                chunk_rows: bounded_chunk_rows,
            })
        })
        .map_err(DescriptorDispatchFailure::Residency)?
        .map_err(DescriptorDispatchFailure::Execution)?;
    let (mut session, fact_rows, bounded_chunk_rows) = match initialized {
        DenseExecutionSetup::OneShot { dispatch, elapsed } => {
            note_dense_test_call_and_maybe_arm_timeout();
            warn_if_grouped_dispatch_slow("one-shot", elapsed);
            return match capture_postgres_interrupt() {
                Some(caught) => cleanup_before_rethrow(dispatch, caught, |caught| caught.rethrow()),
                None => Ok(*dispatch),
            };
        }
        DenseExecutionSetup::Bounded {
            session,
            fact_rows,
            chunk_rows,
        } => (session, fact_rows, chunk_rows),
    };
    let kernel_mode = session.kernel_mode();
    let planned_calls =
        bounded_dispatch_call_count(fact_rows, bounded_chunk_rows).ok_or_else(|| {
            DescriptorDispatchFailure::Execution(DescriptorAggExecutionError::Failure(
                "grouped aggregate bounded dispatch call count overflowed".to_owned(),
            ))
        })?;

    let dispatch_started = Instant::now();
    let dispatch = run_bounded_dispatch(
        fact_rows,
        bounded_chunk_rows,
        |range| {
            let elapsed = resolver
                .resolve(|inputs| {
                    let desc = stats::profile_descriptor_stage(
                        stats::DescriptorExecStage::DerivedInputBinding,
                        || inputs.artifact.bind(requests, &inputs.columns),
                    )
                    .map_err(DescriptorAggExecutionError::Failure)?;
                    // SAFETY: the descriptor is used only during this synchronous
                    // callback while all row pointers remain pinned.
                    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }.map_err(
                        |error| {
                            gpu_execution_error("grouped descriptor changed between chunks", error)
                        },
                    )?;
                    let chunk =
                        plan.row_chunk(range.first_row, range.row_count)
                            .map_err(|error| {
                                gpu_execution_error("grouped chunk descriptor rejected", error)
                            })?;
                    let chunk_started = Instant::now();
                    stats::profile_descriptor_stage(
                        stats::DescriptorExecStage::KernelExecution,
                        || session.accumulate(&chunk),
                    )
                    .map_err(|error| gpu_execution_error("grouped chunk kernel failed", error))?;
                    Ok::<_, DescriptorAggExecutionError>(chunk_started.elapsed())
                })
                .map_err(DescriptorDispatchFailure::Residency)
                .and_then(|result| result.map_err(DescriptorDispatchFailure::Execution))
                .map_err(DenseBoundedFailure::Dispatch)?;
            warn_if_grouped_dispatch_slow("accumulate chunk", elapsed);
            note_dense_test_call_and_maybe_arm_timeout();
            Ok(())
        },
        || {
            // This closure runs only after the preceding residency callback
            // returned and its RefCell guard was dropped.
            capture_postgres_interrupt().map_or(Ok(()), |caught| {
                Err(DenseBoundedFailure::Interrupt(Box::new(caught)))
            })
        },
    );
    let launches = match dispatch {
        Ok(launches) => launches,
        Err(BoundedDispatchError::ZeroChunkLimit) => unreachable!("checked before setup"),
        Err(
            BoundedDispatchError::Dispatch(error) | BoundedDispatchError::InterruptBoundary(error),
        ) => match error {
            DenseBoundedFailure::Interrupt(caught) => {
                return cleanup_before_rethrow(session, caught, |caught| (*caught).rethrow());
            }
            DenseBoundedFailure::Dispatch(error) => return Err(error),
        },
    };

    let (output, finalize_elapsed) = resolver
        .resolve(|inputs| {
            let desc = stats::profile_descriptor_stage(
                stats::DescriptorExecStage::DerivedInputBinding,
                || inputs.artifact.bind(requests, &inputs.columns),
            )
            .map_err(DescriptorAggExecutionError::Failure)?;
            // SAFETY: finalization is synchronous and this fresh plan remains
            // pinned until the callback returns.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc.desc) }.map_err(|error| {
                gpu_execution_error("grouped descriptor changed before finalize", error)
            })?;
            // Output buffers carry an exact resolved-plan identity. Allocate
            // them from this final pinned plan, not the temporary setup plan
            // that was dropped before the bounded residency borrows began.
            let mut storage = stats::profile_descriptor_stage(
                stats::DescriptorExecStage::OutputAllocation,
                || GroupedAggOutputStorage::new(&plan),
            )
            .map_err(|error| {
                gpu_execution_error("grouped final output allocation failed", error)
            })?;
            let finalize_started = Instant::now();
            let outcome = stats::profile_descriptor_stage(
                stats::DescriptorExecStage::KernelExecution,
                || session.finalize(&plan, &mut storage),
            )
            .map_err(|error| gpu_execution_error("grouped finalization failed", error))?;
            let finalize_elapsed = finalize_started.elapsed();
            // Validate output against the current, pinned artifact. The output
            // retains its dictionary owners, not a duplicate logical spec or
            // scalar program, after this residency callback ends.
            let output = DescriptorAggOutput::new(
                storage,
                outcome,
                inputs.artifact.base().domains.clone(),
                &inputs.artifact.base().resolved_spec,
                projection.clone(),
            )
            .map_err(DescriptorAggExecutionError::Failure)?;
            Ok::<_, DescriptorAggExecutionError>((output, finalize_elapsed))
        })
        .map_err(DescriptorDispatchFailure::Residency)?
        .map_err(DescriptorDispatchFailure::Execution)?;
    note_dense_test_call_and_maybe_arm_timeout();
    warn_if_grouped_dispatch_slow("finalize", finalize_elapsed);
    // Finalize is another synchronous native call. Capture only after its
    // store guard has been released, then release detached resources before
    // preserving the original PostgreSQL error and SQLSTATE.
    if let Some(caught) = capture_postgres_interrupt() {
        return cleanup_before_rethrow((session, output), caught, |caught| caught.rethrow());
    }

    let dispatch_time_us =
        u64::try_from(dispatch_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let completed_calls = launches.saturating_add(1);
    debug_assert_eq!(completed_calls, planned_calls);
    Ok(DescriptorAggDispatch {
        output,
        fact_rows,
        planned_batches: u64::try_from(planned_calls).unwrap_or(u64::MAX),
        batches_executed: u64::try_from(completed_calls).unwrap_or(u64::MAX),
        physical_mode_calls: u64::try_from(launches).unwrap_or(u64::MAX),
        dispatch_time_us,
        kernel_mode,
        residency: None,
    })
}

fn validate_hash_selected_rows(
    spec: &AggQuerySpec,
    fact_rows: usize,
    selected: u64,
) -> Result<(), String> {
    let rows = u64::try_from(fact_rows)
        .map_err(|_| "hash fact rows exceed the result domain".to_owned())?;
    if selected > rows || (hash_where_filter(spec)?.is_none() && selected != rows) {
        return Err(format!(
            "hash selected-row count {selected} violates input bound {rows}"
        ));
    }
    Ok(())
}

fn validate_hash_compact_outcome(
    outcome: GroupedAggOutcome,
    storage: &GroupedAggOutputStorage,
    fact_rows: usize,
    spec: &AggQuerySpec,
) -> Result<(), String> {
    let count_input = HashMeasureInput::from_spec(spec)?;
    let result = match outcome {
        GroupedAggOutcome::Complete(result) => result,
        GroupedAggOutcome::NeedsRecheck(result) => {
            return Err(format!(
                "H3 grouped aggregate returned {} uncertain rows",
                result.uncertain_count
            ));
        }
    };
    if result.uncertain_count != 0 {
        return Err(format!(
            "complete H3 grouped aggregate contains {} uncertain rows",
            result.uncertain_count
        ));
    }
    validate_hash_selected_rows(spec, fact_rows, result.selected_count)?;
    if result.emitted_group_count > result.group_capacity {
        return Err("H3 emitted group count exceeds group capacity".to_owned());
    }
    let counts = storage
        .measure_count(0)
        .ok_or_else(|| "H3 compact output is missing its COUNT lane".to_owned())?;
    let key_values = storage
        .key_values(0)
        .ok_or_else(|| "H3 compact output is missing its key lane".to_owned())?;
    validate_compact_integer_key_buffers(
        key_values,
        storage.key_nulls(0),
        result.group_capacity,
        result.emitted_group_count,
        spec.group_keys[0].type_oid,
    )?;
    validate_h3_count_partition(
        counts,
        result.group_capacity,
        result.emitted_group_count,
        result.selected_count,
        count_input,
    )?;
    if let HashMeasureInput::IntegerStates(column) = count_input {
        use crate::gpu::GroupedAggStateLane;
        let mask = hash_integer_lanes(&spec.measures[0]);
        let mut lanes = [None; 3];
        for (index, (flag, lane)) in [
            (abi::PGACCEL_GROUPED_AGG_LANE_SUM, GroupedAggStateLane::Sum),
            (abi::PGACCEL_GROUPED_AGG_LANE_MIN, GroupedAggStateLane::Min),
            (abi::PGACCEL_GROUPED_AGG_LANE_MAX, GroupedAggStateLane::Max),
        ]
        .into_iter()
        .enumerate()
        {
            if mask & flag != 0 {
                lanes[index] = Some(storage.measure_state(0, lane).ok_or_else(|| {
                    "compact hash output is missing a requested integer lane".to_owned()
                })?);
            }
        }
        validate_hash_integer_states(
            lanes,
            storage
                .measure_nonnull_count(0)
                .ok_or_else(|| "compact hash output is missing integer validity".to_owned())?,
            counts,
            column.type_oid,
        )?;
    }
    Ok(())
}

/// Validate finalized aggregate state, never scan or recompute resident inputs.
fn validate_hash_integer_states(
    lanes: [Option<&[u8]>; 3],
    nonnull: &[u64],
    counts: &[u64],
    type_oid: u32,
) -> Result<(), String> {
    let (minimum, maximum) = match type_oid {
        INT2OID => (i64::from(i16::MIN), i64::from(i16::MAX)),
        INT4OID => (i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => return Err("hash integer states have an unsupported logical input type".into()),
    };
    if lanes.iter().all(Option::is_none)
        || lanes
            .iter()
            .flatten()
            .any(|lane| counts.len().checked_mul(size_of::<i64>()) != Some(lane.len()))
        || nonnull != counts
    {
        return Err("hash integer output lengths or non-NULL counts disagree".into());
    }
    for (index, count) in counts.iter().enumerate() {
        if *count > u64::from(u32::MAX) {
            return Err("H3 SUM/COUNT exceeds its UINT32_MAX row domain".into());
        }
        let [sum, low, high] = lanes.map(|lane| {
            lane.map(|bytes| {
                i64::from_ne_bytes(
                    bytes[index * 8..index * 8 + 8]
                        .try_into()
                        .expect("exact state width"),
                )
            })
        });
        // Both products fit signed 64-bit under the checked native row cap.
        let count = i64::try_from(*count).expect("bounded count");
        if sum.is_some_and(|sum| sum < minimum * count || sum > maximum * count) {
            return Err("hash integer SUM is outside its logical type/count bounds".into());
        }
        for value in [low, high].into_iter().flatten() {
            if (count == 0 && value != 0) || (count != 0 && !(minimum..=maximum).contains(&value)) {
                return Err("hash extremum is outside its logical type/NULL bounds".into());
            }
        }
        if low.zip(high).is_some_and(|(low, high)| low > high)
            || sum.zip(low).is_some_and(|(sum, low)| sum < low * count)
            || sum.zip(high).is_some_and(|(sum, high)| sum > high * count)
            || (count == 1
                && (low.zip(high).is_some_and(|(low, high)| low != high)
                    || sum.zip(low).is_some_and(|(sum, low)| sum != low)
                    || sum.zip(high).is_some_and(|(sum, high)| sum != high)))
        {
            return Err("hash integer states are mutually inconsistent".into());
        }
    }
    Ok(())
}

/// The caller has validated a single integer measure through HashMeasureInput.
fn hash_integer_lanes(measure: &crate::engine::spec::MeasureSpec) -> u32 {
    measure
        .outputs
        .iter()
        .fold(abi::PGACCEL_GROUPED_AGG_LANE_COUNT, |mask, output| {
            mask | match output.kind {
                AggregateKind::Sum | AggregateKind::Avg => abi::PGACCEL_GROUPED_AGG_LANE_SUM,
                AggregateKind::Min => abi::PGACCEL_GROUPED_AGG_LANE_MIN,
                AggregateKind::Max => abi::PGACCEL_GROUPED_AGG_LANE_MAX,
                _ => 0,
            }
        })
}

#[cfg(test)]
fn validate_h3_sum_states(
    sums: &[u8],
    nonnull: &[u64],
    counts: &[u64],
    type_oid: u32,
) -> Result<(), String> {
    validate_hash_integer_states([Some(sums), None, None], nonnull, counts, type_oid)
}

fn validate_h3_count_partition(
    counts: &[u64],
    group_capacity: usize,
    emitted_group_count: usize,
    selected_count: u64,
    count_input: HashMeasureInput,
) -> Result<(), String> {
    if counts.len() != group_capacity {
        return Err(format!(
            "H3 {} lane has length {}, expected {}",
            count_input.label(),
            counts.len(),
            group_capacity
        ));
    }
    if emitted_group_count > group_capacity {
        return Err("H3 emitted group count exceeds group capacity".to_owned());
    }
    let emitted_rows = u64::try_from(emitted_group_count)
        .map_err(|_| "H3 group count exceeds the u64 row domain".to_owned())?;
    if emitted_rows > selected_count || (emitted_rows == 0 && selected_count != 0) {
        return Err("H3 emitted groups violate input-row bounds".into());
    }
    let (emitted, unused) = counts.split_at(emitted_group_count);
    if count_input == HashMeasureInput::Star && emitted.contains(&0) {
        return Err("H3 compact output contains an emitted zero-count group".to_owned());
    }
    let counted_rows = emitted.iter().try_fold(0_u64, |total, count| {
        total.checked_add(*count).ok_or_else(|| {
            format!(
                "H3 compact output {} sum overflows u64",
                count_input.label()
            )
        })
    })?;
    if counted_rows > selected_count
        || (count_input == HashMeasureInput::Star && counted_rows != selected_count)
    {
        let bound = if count_input == HashMeasureInput::Star {
            "exactly"
        } else {
            "at most"
        };
        return Err(format!(
            "H3 compact output counts sum to {counted_rows}, expected {bound} {selected_count} selected rows"
        ));
    }
    if unused.iter().any(|count| *count != 0) {
        return Err(format!(
            "H3 compact output contains nonzero unused {} state",
            count_input.label()
        ));
    }
    Ok(())
}

#[cfg(feature = "pg_test")]
#[path = "h3_count_test.rs"]
mod h3_count_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::residency::ResidentInputBundle;
    use crate::engine::spec::{
        AggOutputSlot, AggOutputSource, AggregateOutput, DimSpec, GroupKeyRef, MeasureSpec,
        ScalarRange, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
    };

    const TEST_GEOMETRY_OID: u32 = 60_001;
    const NUMERICOID: u32 = 1700;
    const TEST_SRID: i32 = 4_326;

    fn policy_input() -> ArtifactPolicyInput {
        ArtifactPolicyInput {
            construction_bytes: 1_000_000,
            ephemeral_bytes: 1_000_000,
            construction_time_us: 100,
            expected_reuses: 8,
            cache_hit: false,
            invalidation_risk_ppm: 0,
            cached_build_launches: 2,
            ephemeral_build_launches: 2,
            consumer_launches: 1,
            budget_headroom_bytes: 100_000_000,
        }
    }

    #[test]
    fn artifact_policy_accounts_for_every_admission_factor() {
        let baseline = policy_input();
        assert_eq!(
            choose_artifact_policy(baseline).policy,
            DescriptorArtifactPolicy::CachedReusable
        );

        let hit = choose_artifact_policy(ArtifactPolicyInput {
            cache_hit: true,
            expected_reuses: 1,
            ..baseline
        });
        assert_eq!(hit.policy, DescriptorArtifactPolicy::CachedReusable);
        assert_eq!(hit.reason, ArtifactPolicyReason::CacheHit);

        let one_use = choose_artifact_policy(ArtifactPolicyInput {
            expected_reuses: 1,
            ..baseline
        });
        assert_eq!(one_use.policy, DescriptorArtifactPolicy::EphemeralFused);

        let invalidation = choose_artifact_policy(ArtifactPolicyInput {
            invalidation_risk_ppm: 1_000_000,
            ..baseline
        });
        assert_eq!(
            invalidation.policy,
            DescriptorArtifactPolicy::EphemeralFused
        );

        let memory = choose_artifact_policy(ArtifactPolicyInput {
            ephemeral_bytes: 0,
            budget_headroom_bytes: 999_999,
            ..baseline
        });
        assert_eq!(memory.policy, DescriptorArtifactPolicy::EphemeralFused);
        assert_eq!(memory.reason, ArtifactPolicyReason::MemoryBudget);

        let launch_heavy_cache = choose_artifact_policy(ArtifactPolicyInput {
            expected_reuses: 2,
            cached_build_launches: 10,
            ephemeral_build_launches: 0,
            ..baseline
        });
        assert_eq!(
            launch_heavy_cache.policy,
            DescriptorArtifactPolicy::EphemeralFused
        );

        let empty = choose_artifact_policy(ArtifactPolicyInput {
            construction_bytes: 0,
            ephemeral_bytes: 0,
            ..baseline
        });
        assert_eq!(empty.reason, ArtifactPolicyReason::EmptyArtifact);
        let summary = baseline;
        let summary = choose_artifact_policy(summary).summary();
        for field in [
            "construction_bytes=",
            "construction_time_us=",
            "expected_reuses=",
            "cache_hit=",
            "invalidation_risk_ppm=",
            "cached_build_launches=",
            "consumer_launches=",
            "budget_headroom_bytes=",
        ] {
            assert!(summary.contains(field), "missing policy field {field}");
        }
    }

    fn parallel_dense_integer_desc(
        row_count: usize,
        group_capacity: usize,
    ) -> abi::PgaccelGroupedAggDesc {
        // SAFETY: zero is the canonical inactive representation for every ABI
        // field; the active shape fields are populated below.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.row_count = row_count;
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
        desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
        desc.group_capacity = group_capacity;
        desc.measure_count = 2;
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM;
        desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[0].state_bytes = 8;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        desc.measures[1].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
        desc.measures[1].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.measures[1].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[1].state_bytes = 8;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        desc
    }

    fn column(type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid,
        }
    }

    #[test]
    fn dense_one_shot_branch_obeys_row_and_ordered_partial_budgets() {
        let release_shape = parallel_dense_integer_desc(1_000_000, 350);
        assert!(dense_one_shot_eligible(
            &release_shape,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let chunks = release_shape.row_count.div_ceil(DENSE_INTEGER_CHUNK_ROWS);
        let max_groups = DENSE_INTEGER_MAX_PARTIAL_BYTES / (chunks * DENSE_INTEGER_PARTIAL_BYTES);
        let over_budget = parallel_dense_integer_desc(1_000_000, max_groups + 1);
        assert!(dense_one_shot_eligible(
            &over_budget,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
        let mut ordered_over_budget = over_budget;
        ordered_over_budget.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM
            | abi::PGACCEL_GROUPED_AGG_LANE_MIN
            | abi::PGACCEL_GROUPED_AGG_LANE_MAX;
        assert!(!dense_one_shot_eligible(
            &ordered_over_budget,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        assert!(!dense_one_shot_eligible(&release_shape, 999_999));
        let oversized = parallel_dense_integer_desc(1_000_001, 350);
        assert!(!dense_one_shot_eligible(
            &oversized,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let mut unsupported = release_shape;
        unsupported.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_SUB;
        assert!(!dense_one_shot_eligible(
            &unsupported,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let mask = [1_i8];
        let mut noncanonical = release_shape;
        noncanonical.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        noncanonical.where_filter.mask = mask.as_ptr();
        assert!(dense_one_shot_eligible(
            &noncanonical,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
        noncanonical.where_filter.predicate_range_count = 1;
        assert!(!dense_one_shot_eligible(
            &noncanonical,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        let mut range = release_shape;
        range.key_count = 1;
        range.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_MUL;
        range.measures[0].rhs.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        range.measures[0].rhs.element_bytes = 4;
        range.where_filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
        range.where_filter.predicate_measure_slot = 0;
        range.where_filter.predicate_range_count = 1;
        range.where_filter.predicate_lo[0] = scalar_value(ScalarValue::I32(200));
        range.where_filter.predicate_hi[0] = scalar_value(ScalarValue::I32(800));
        assert!(parallel_dense_integer_shape(&range));
        assert!(dense_one_shot_eligible(
            &range,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));

        range.key_count = 0;
        assert!(parallel_dense_integer_shape(&range));
        range.key_count = 1;

        range.where_filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS;
        assert!(!parallel_dense_integer_shape(&range));
        range.where_filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
        range.where_filter.predicate_range_count = 2;
        range.where_filter.predicate_lo[1] = scalar_value(ScalarValue::I32(300));
        range.where_filter.predicate_hi[1] = scalar_value(ScalarValue::I32(700));
        assert!(parallel_dense_integer_shape(&range));

        let mut measure_range = release_shape;
        measure_range.key_count = 1;
        measure_range.measure_filters[0].predicate_source =
            abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE;
        measure_range.measure_filters[0].predicate_measure_slot = 0;
        measure_range.measure_filters[0].predicate_range_count = 1;
        measure_range.measure_filters[0].predicate_lo[0] = scalar_value(ScalarValue::I32(200));
        measure_range.measure_filters[0].predicate_hi[0] = scalar_value(ScalarValue::I32(800));
        assert!(parallel_dense_integer_shape(&measure_range));
        assert!(dense_one_shot_eligible(
            &measure_range,
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        ));
        measure_range.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        measure_range.where_filter.mask = mask.as_ptr();
        assert!(parallel_dense_integer_shape(&measure_range));
        measure_range.where_filter.mask = std::ptr::null();
        assert!(!parallel_dense_integer_shape(&measure_range));
    }

    #[test]
    fn dense_count_one_shot_honors_explicit_limit() {
        let mut desc = parallel_dense_integer_desc(10, 1);
        desc.measure_count = 1;
        desc.measures[0] = desc.measures[1];
        assert!(dense_one_shot_eligible(&desc, 10));
        assert!(!dense_one_shot_eligible(&desc, 4));

        let multiplicity = [2_u64];
        let mut weighted = desc;
        weighted.dim_count = 1;
        weighted.dims[0].multiplicity_by_key = multiplicity.as_ptr();
        assert!(parallel_dense_count_shape(&weighted));
        assert!(dense_one_shot_eligible(&weighted, 10));

        weighted.key_count = 1;
        assert!(!parallel_dense_count_shape(&weighted));
        weighted.key_count = 0;
        let mask = [1_i8];
        weighted.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        weighted.where_filter.mask = mask.as_ptr();
        assert!(!parallel_dense_count_shape(&weighted));

        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL;
        desc.measures[0].value.element_bytes = 1;
        assert!(parallel_dense_count_shape(&desc));
        assert!(dense_one_shot_eligible(&desc, 10));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        desc.measures[0].value.element_bytes = 8;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE;
        desc.measures[0].value.element_bytes = 4;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP;
        desc.measures[0].value.element_bytes = 8;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32;
        desc.measures[0].value.element_bytes = 4;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64;
        desc.measures[0].value.element_bytes = 8;
        assert!(parallel_dense_count_shape(&desc));

        desc.measures[0].value.element_bytes = 4;
        assert!(!parallel_dense_count_shape(&desc));
    }

    #[test]
    fn one_shot_limit_boundary_preserves_bounded_cancellation_contract() {
        const DEVICE_ONE_SHOT_LIMIT: usize = 250_000;
        const SESSION_CHUNK_ROWS: usize = 65_536;

        assert_eq!(
            clamp_dense_one_shot_row_cap(DEVICE_ONE_SHOT_LIMIT, 0),
            DEVICE_ONE_SHOT_LIMIT
        );
        assert_eq!(
            clamp_dense_one_shot_row_cap(DEVICE_ONE_SHOT_LIMIT, 1),
            1,
            "the pg_test one-shot override is independent of session chunking"
        );
        assert_eq!(
            clamp_dense_one_shot_row_cap(GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS + 1, 0),
            GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS
        );

        let at_limit = parallel_dense_integer_desc(DEVICE_ONE_SHOT_LIMIT, 350);
        assert!(dense_one_shot_eligible(&at_limit, DEVICE_ONE_SHOT_LIMIT));
        let above_limit = parallel_dense_integer_desc(DEVICE_ONE_SHOT_LIMIT + 1, 350);
        assert!(!dense_one_shot_eligible(
            &above_limit,
            DEVICE_ONE_SHOT_LIMIT
        ));
        assert_eq!(
            bounded_dispatch_call_count(above_limit.row_count, SESSION_CHUNK_ROWS),
            Some(5),
            "limit+1 must use four bounded calls plus finalize, leaving interrupt boundaries"
        );
    }

    #[test]
    fn bounded_dense_chunks_cannot_bypass_the_physical_synchronous_row_cap() {
        const HOSTED_FIXTURE_ROWS: usize = 312_500;
        const MEMORY_DERIVED_CHUNK_ROWS: usize = 1_000_000;
        const ONE_CU_SYNCHRONOUS_CAP: usize = 64_000;

        let chunk_rows =
            dense_session_chunk_rows(MEMORY_DERIVED_CHUNK_ROWS, ONE_CU_SYNCHRONOUS_CAP);
        assert_eq!(chunk_rows, ONE_CU_SYNCHRONOUS_CAP);
        assert_eq!(
            bounded_dispatch_call_count(HOSTED_FIXTURE_ROWS, chunk_rows),
            Some(6),
            "the hosted resident fixture needs five watchdog-safe accumulates plus finalize"
        );
        assert_eq!(
            dense_session_chunk_rows(65_536, 250_000),
            65_536,
            "the allocation-derived bound remains authoritative when it is smaller"
        );
    }

    #[test]
    fn release_dense_session_uses_at_most_eleven_synchronous_calls_at_ten_million_rows() {
        let chunk_rows = crate::engine::cost::DeviceLimits::cpu_only().gpu_reduce_max_chunk;
        assert_eq!(chunk_rows, 1_000_000);
        assert_eq!(
            bounded_dispatch_call_count(10_000_000, chunk_rows),
            Some(11)
        );
    }

    fn projection(
        kind: AggregateKind,
        source_type_oid: u32,
        result_type_oid: u32,
    ) -> AggOutputProjection {
        AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind,
                },
                source_type_oid,
                result_type_oid,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: kind != AggregateKind::Count,
            }],
        }
    }

    fn spec(expression: MeasureExpr, kind: AggregateKind) -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 42,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn dense_bool_count_spec() -> AggQuerySpec {
        let mut count = spec(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: BOOLOID,
            }),
            AggregateKind::Count,
        );
        count.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(ColumnRef {
                relation_oid: 42,
                attno: 1,
                type_oid: BOOLOID,
            }),
            type_oid: BOOLOID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::DictionaryI32 {
                cardinality: 3,
                null_code: Some(2),
            },
        });
        count
    }

    fn dense_int2_count_spec() -> AggQuerySpec {
        let mut count = dense_bool_count_spec();
        count.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: INT2OID,
        });
        count
    }

    fn dense_int8_count_spec() -> AggQuerySpec {
        let mut count = dense_bool_count_spec();
        count.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: INT8OID,
        });
        count
    }

    fn dense_date_count_spec() -> AggQuerySpec {
        let mut count = dense_bool_count_spec();
        count.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: DATEOID,
        });
        count
    }

    fn dense_timestamp_count_spec(type_oid: u32) -> AggQuerySpec {
        assert!(matches!(type_oid, TIMESTAMPOID | TIMESTAMPTZOID));
        let mut count = dense_bool_count_spec();
        count.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid,
        });
        count
    }

    fn dense_float_count_spec(type_oid: u32) -> AggQuerySpec {
        assert!(matches!(type_oid, FLOAT4OID | FLOAT8OID));
        let mut count = dense_bool_count_spec();
        count.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid,
        });
        count
    }

    fn dense_integer_sum_avg_spec(type_oid: u32) -> AggQuerySpec {
        assert!(matches!(type_oid, INT2OID | INT4OID));
        let mut spec = dense_bool_count_spec();
        spec.measures = vec![
            MeasureSpec {
                expression: MeasureExpr::Column(ColumnRef {
                    relation_oid: 42,
                    attno: 2,
                    type_oid,
                }),
                outputs: vec![
                    AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    },
                    AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Avg,
                    },
                ],
                filter: FilterSpec::None,
            },
            MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            },
        ];
        spec
    }

    fn dense_integer_range_spec() -> AggQuerySpec {
        let group = ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid: INT4OID,
        };
        let lhs = ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: INT4OID,
        };
        let rhs = ColumnRef {
            relation_oid: 42,
            attno: 3,
            type_oid: INT4OID,
        };
        AggQuerySpec {
            fact_rel: 42,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::FactColumn(group),
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::DictionaryI32 {
                    cardinality: 256,
                    null_code: None,
                },
            }],
            measures: vec![
                MeasureSpec {
                    expression: MeasureExpr::Binary {
                        op: BinaryMeasureOp::Mul,
                        lhs,
                        rhs,
                    },
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    }],
                    filter: FilterSpec::None,
                },
                MeasureSpec {
                    expression: MeasureExpr::CountStar,
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }],
                    filter: FilterSpec::None,
                },
            ],
            fact_filter: FilterSpec::Ranges {
                input: lhs,
                ranges: vec![ScalarRange {
                    lo: ScalarValue::I32(200),
                    hi: ScalarValue::I32(800),
                }],
            },
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn dense_integer_measure_range_spec() -> AggQuerySpec {
        let group = ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid: INT4OID,
        };
        let value = ColumnRef {
            relation_oid: 42,
            attno: 2,
            type_oid: INT4OID,
        };
        AggQuerySpec {
            fact_rel: 42,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::FactColumn(group),
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::DictionaryI32 {
                    cardinality: 256,
                    null_code: None,
                },
            }],
            measures: vec![
                MeasureSpec {
                    expression: MeasureExpr::Column(value),
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    }],
                    filter: FilterSpec::Ranges {
                        input: value,
                        ranges: vec![ScalarRange {
                            lo: ScalarValue::I32(200),
                            hi: ScalarValue::I32(800),
                        }],
                    },
                },
                MeasureSpec {
                    expression: MeasureExpr::CountStar,
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }],
                    filter: FilterSpec::None,
                },
            ],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        }
    }

    #[test]
    fn measure_filter_builder_rejects_ranges_beyond_the_frozen_abi_capacity() {
        let spec = dense_integer_measure_range_spec();
        let value = match spec.measures[0].expression {
            MeasureExpr::Column(value) => value,
            ref expression => panic!("expected direct column measure, got {expression:?}"),
        };
        let filter = FilterSpec::Ranges {
            input: value,
            ranges: vec![
                ScalarRange {
                    lo: ScalarValue::I32(1),
                    hi: ScalarValue::I32(2),
                };
                abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES + 1
            ],
        };
        assert!(matches!(
            build_measure_filter(&spec, &filter),
            Err(error) if error == "measure filter range count exceeds descriptor capacity"
        ));
    }

    #[test]
    fn planned_kernel_mode_is_stable_and_rejects_serial_shapes() {
        let count = spec(MeasureExpr::CountStar, AggregateKind::Count);
        assert_eq!(
            planned_descriptor_kernel_mode(&count, false),
            GroupedAggKernelMode::ParallelDenseCount
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&count, true),
            GroupedAggKernelMode::ParallelHash
        );

        let counted_dimension = DimSpec {
            relation_oid: 99,
            fact_key: ColumnRef {
                relation_oid: 42,
                attno: 1,
                type_oid: INT4OID,
            },
            dim_key: ColumnRef {
                relation_oid: 99,
                attno: 1,
                type_oid: INT4OID,
            },
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Counted,
            filter: FilterSpec::None,
        };
        let mut weighted_count = count;
        weighted_count.star_dims.push(counted_dimension.clone());
        assert_eq!(
            planned_descriptor_kernel_mode(&weighted_count, false),
            GroupedAggKernelMode::ParallelDenseCount,
            "an exact unfiltered global COUNT(*) uses the weighted parallel lane"
        );

        let mut grouped_weighted_count = weighted_count.clone();
        grouped_weighted_count.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: INT4OID,
            }),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::DictionaryI32 {
                cardinality: 2,
                null_code: None,
            },
        });
        assert_eq!(
            planned_descriptor_kernel_mode(&grouped_weighted_count, false),
            GroupedAggKernelMode::SerialGeneric,
            "grouped fanout has no released weighted kernel"
        );

        let mut filtered_weighted_count = weighted_count.clone();
        filtered_weighted_count.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 3,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        assert_eq!(
            planned_descriptor_kernel_mode(&filtered_weighted_count, false),
            GroupedAggKernelMode::SerialGeneric,
            "filtered fanout has no released weighted kernel"
        );

        let mut multiple_weighted_count = weighted_count.clone();
        let mut second_dimension = counted_dimension.clone();
        second_dimension.relation_oid = 100;
        second_dimension.dim_key.relation_oid = 100;
        multiple_weighted_count.star_dims.push(second_dimension);
        assert_eq!(
            planned_descriptor_kernel_mode(&multiple_weighted_count, false),
            GroupedAggKernelMode::SerialGeneric,
            "multi-dimension fanout has no independently qualified planner lane"
        );

        let mut int8_weighted_count = weighted_count.clone();
        int8_weighted_count.star_dims[0].fact_key.type_oid = INT8OID;
        int8_weighted_count.star_dims[0].dim_key.type_oid = INT8OID;
        assert_eq!(
            planned_descriptor_kernel_mode(&int8_weighted_count, false),
            GroupedAggKernelMode::SerialGeneric,
            "counted INT8 membership has no independently qualified planner lane"
        );

        let mut typed_weighted_count = spec(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: INT4OID,
            }),
            AggregateKind::Count,
        );
        typed_weighted_count.star_dims.push(counted_dimension);
        assert_eq!(
            planned_descriptor_kernel_mode(&typed_weighted_count, false),
            GroupedAggKernelMode::SerialGeneric,
            "COUNT(column) fanout has no released weighted kernel"
        );

        let bool_count = dense_bool_count_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&bool_count, false),
            GroupedAggKernelMode::ParallelDenseCount
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&bool_count, true),
            GroupedAggKernelMode::SerialGeneric,
            "a bool column count is not silently routed through COUNT(*) hash semantics"
        );
        let int2_count = dense_int2_count_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&int2_count, false),
            GroupedAggKernelMode::ParallelDenseCount
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&int2_count, true),
            GroupedAggKernelMode::SerialGeneric,
            "an INT2 column count is not silently routed through COUNT(*) hash semantics"
        );
        let int8_count = dense_int8_count_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&int8_count, false),
            GroupedAggKernelMode::ParallelDenseCount
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&int8_count, true),
            GroupedAggKernelMode::SerialGeneric,
            "an INT8 column count is not silently routed through COUNT(*) hash semantics"
        );
        let date_count = dense_date_count_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&date_count, false),
            GroupedAggKernelMode::ParallelDenseCount
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&date_count, true),
            GroupedAggKernelMode::SerialGeneric,
            "a DATE column count is not silently routed through COUNT(*) hash semantics"
        );
        for (type_oid, type_name) in [(TIMESTAMPOID, "TIMESTAMP"), (TIMESTAMPTZOID, "TIMESTAMPTZ")]
        {
            let timestamp_count = dense_timestamp_count_spec(type_oid);
            assert_eq!(
                planned_descriptor_kernel_mode(&timestamp_count, false),
                GroupedAggKernelMode::ParallelDenseCount
            );
            assert_eq!(
                planned_descriptor_kernel_mode(&timestamp_count, true),
                GroupedAggKernelMode::SerialGeneric,
                "a {type_name} column count is not silently routed through COUNT(*) hash semantics"
            );
        }
        for (type_oid, type_name) in [(FLOAT4OID, "FLOAT4"), (FLOAT8OID, "FLOAT8")] {
            let float_count = dense_float_count_spec(type_oid);
            assert_eq!(
                planned_descriptor_kernel_mode(&float_count, false),
                GroupedAggKernelMode::ParallelDenseCount
            );
            assert_eq!(
                planned_descriptor_kernel_mode(&float_count, true),
                GroupedAggKernelMode::SerialGeneric,
                "a {type_name} column count is not silently routed through COUNT(*) hash semantics"
            );
        }
        for type_oid in [INT2OID, INT4OID] {
            let integer_sum_avg = dense_integer_sum_avg_spec(type_oid);
            assert_eq!(
                planned_descriptor_kernel_mode(&integer_sum_avg, false),
                GroupedAggKernelMode::ParallelDenseInteger
            );
            let mut avg_only = integer_sum_avg;
            avg_only.measures[0]
                .outputs
                .retain(|output| output.kind == AggregateKind::Avg);
            assert_eq!(
                planned_descriptor_kernel_mode(&avg_only, false),
                GroupedAggKernelMode::ParallelDenseInteger,
                "AVG consumes the exact SUM/non-NULL state without requiring SUM output"
            );
            avg_only.measures.pop();
            assert_eq!(
                planned_descriptor_kernel_mode(&avg_only, false),
                GroupedAggKernelMode::ParallelDenseInteger,
                "selected-row tracking is internal even without COUNT(*) output"
            );
        }
        let mut same_column = bool_count.clone();
        same_column.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 42,
            attno: 1,
            type_oid: BOOLOID,
        });
        assert_eq!(
            planned_descriptor_kernel_mode(&same_column, false),
            GroupedAggKernelMode::SerialGeneric
        );
        let mut filtered_bool = bool_count;
        filtered_bool.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        assert_eq!(
            planned_descriptor_kernel_mode(&filtered_bool, false),
            GroupedAggKernelMode::SerialGeneric
        );

        let range_integer = dense_integer_range_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&range_integer, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );
        let measure_range = dense_integer_measure_range_spec();
        assert_eq!(
            planned_descriptor_kernel_mode(&measure_range, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );
        let mut one_sided_measure_range = measure_range;
        let FilterSpec::Ranges { ranges, .. } = &mut one_sided_measure_range.measures[0].filter
        else {
            unreachable!("measure range fixture")
        };
        ranges[0].hi = ScalarValue::I32(i32::MAX);
        assert_eq!(
            planned_descriptor_kernel_mode(&one_sided_measure_range, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );
        let mut one_sided_range = range_integer.clone();
        let FilterSpec::Ranges { ranges, .. } = &mut one_sided_range.fact_filter else {
            unreachable!("range fixture")
        };
        ranges[0].lo = ScalarValue::I32(i32::MIN);
        assert_eq!(
            planned_descriptor_kernel_mode(&one_sided_range, false),
            GroupedAggKernelMode::ParallelDenseInteger,
            "computed integer measures share the qualified scalar filter lane"
        );
        let mut range_on_rhs = range_integer;
        let MeasureExpr::Binary { rhs, .. } = &range_on_rhs.measures[0].expression else {
            unreachable!("product fixture")
        };
        let rhs = *rhs;
        let FilterSpec::Ranges { input, .. } = &mut range_on_rhs.fact_filter else {
            unreachable!("range fixture")
        };
        *input = rhs;
        assert_eq!(
            planned_descriptor_kernel_mode(&range_on_rhs, false),
            GroupedAggKernelMode::SerialGeneric,
            "the fused implementation reuses only the qualified product lhs"
        );

        let mut integer = spec(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: INT4OID,
            }),
            AggregateKind::Sum,
        );
        integer.measures.push(MeasureSpec {
            expression: MeasureExpr::CountStar,
            outputs: vec![AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }],
            filter: FilterSpec::None,
        });
        assert_eq!(
            planned_descriptor_kernel_mode(&integer, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );

        integer.fact_filter = FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: INT4OID,
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(i32::MIN),
                hi: ScalarValue::I32(800),
            }],
        };
        assert_eq!(
            planned_descriptor_kernel_mode(&integer, false),
            GroupedAggKernelMode::ParallelDenseInteger,
            "exact integer WHERE ranges use the bounded dense kernel"
        );
        integer.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 3,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        assert_eq!(
            planned_descriptor_kernel_mode(&integer, false),
            GroupedAggKernelMode::ParallelDenseInteger,
            "a derived SQL mask is supported by the parallel dense branch"
        );

        let float_min = spec(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 42,
                attno: 2,
                type_oid: FLOAT8OID,
            }),
            AggregateKind::Min,
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&float_min, false),
            GroupedAggKernelMode::SerialGeneric
        );

        integer.fact_filter = FilterSpec::None;
        integer.star_dims.push(DimSpec {
            relation_oid: 99,
            fact_key: column(INT4OID),
            dim_key: ColumnRef {
                relation_oid: 99,
                attno: 1,
                type_oid: INT4OID,
            },
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Counted,
            filter: FilterSpec::None,
        });
        assert_eq!(
            planned_descriptor_kernel_mode(&integer, false),
            GroupedAggKernelMode::SerialGeneric
        );
    }

    #[test]
    fn specialization_cache_uses_complete_identity_and_stable_variants() {
        DESCRIPTOR_SPECIALIZATION_CACHE.with(|cache| cache.borrow_mut().clear());
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![1, 2, 3, 4]);
        let changed = DerivedArtifactIdentity::from_canonical_words(vec![1, 2, 3, 5]);
        let count = spec(MeasureExpr::CountStar, AggregateKind::Count);

        let (plain, first) = cached_descriptor_specialization(
            &identity,
            &count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(plain.label(), "dense_count_plain");
        assert_eq!(first, DescriptorSpecializationCacheOutcome::Miss);

        let bool_count = dense_bool_count_spec();
        let bool_identity = DerivedArtifactIdentity::from_canonical_words(vec![5, 6, 7, 8]);
        let (bool_specialization, bool_outcome) = cached_descriptor_specialization(
            &bool_identity,
            &bool_count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(bool_specialization.label(), "dense_bool_count_plain");
        assert_eq!(bool_outcome, DescriptorSpecializationCacheOutcome::Miss);

        let int2_count = dense_int2_count_spec();
        let int2_identity = DerivedArtifactIdentity::from_canonical_words(vec![17, 18, 19, 20]);
        let (int2_specialization, int2_outcome) = cached_descriptor_specialization(
            &int2_identity,
            &int2_count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(int2_specialization.label(), "dense_int2_count_plain");
        assert_eq!(int2_outcome, DescriptorSpecializationCacheOutcome::Miss);

        let int8_count = dense_int8_count_spec();
        let int8_identity = DerivedArtifactIdentity::from_canonical_words(vec![21, 22, 23, 24]);
        let (int8_specialization, int8_outcome) = cached_descriptor_specialization(
            &int8_identity,
            &int8_count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(int8_specialization.label(), "dense_int8_count_plain");
        assert_eq!(int8_outcome, DescriptorSpecializationCacheOutcome::Miss);

        let date_count = dense_date_count_spec();
        let date_identity = DerivedArtifactIdentity::from_canonical_words(vec![25, 26, 27, 28]);
        let (date_specialization, date_outcome) = cached_descriptor_specialization(
            &date_identity,
            &date_count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(date_specialization.label(), "dense_date_count_plain");
        assert_eq!(date_outcome, DescriptorSpecializationCacheOutcome::Miss);

        for (offset, type_oid) in [TIMESTAMPOID, TIMESTAMPTZOID].into_iter().enumerate() {
            let timestamp_count = dense_timestamp_count_spec(type_oid);
            let first_word = 29 + i32::try_from(offset).expect("bounded timestamp index") * 4;
            let timestamp_identity = DerivedArtifactIdentity::from_canonical_words(vec![
                first_word,
                first_word + 1,
                first_word + 2,
                first_word + 3,
            ]);
            let (timestamp_specialization, timestamp_outcome) = cached_descriptor_specialization(
                &timestamp_identity,
                &timestamp_count,
                GroupedAggKernelMode::ParallelDenseCount,
            );
            assert_eq!(
                timestamp_specialization.label(),
                "dense_timestamp_count_plain"
            );
            assert_eq!(
                timestamp_outcome,
                DescriptorSpecializationCacheOutcome::Miss
            );
        }

        for (offset, (type_oid, expected_label)) in [
            (FLOAT4OID, "dense_float4_count_plain"),
            (FLOAT8OID, "dense_float8_count_plain"),
        ]
        .into_iter()
        .enumerate()
        {
            let float_count = dense_float_count_spec(type_oid);
            let first_word = 37 + i32::try_from(offset).expect("bounded float index") * 4;
            let float_identity = DerivedArtifactIdentity::from_canonical_words(vec![
                first_word,
                first_word + 1,
                first_word + 2,
                first_word + 3,
            ]);
            let (float_specialization, float_outcome) = cached_descriptor_specialization(
                &float_identity,
                &float_count,
                GroupedAggKernelMode::ParallelDenseCount,
            );
            assert_eq!(float_specialization.label(), expected_label);
            assert_eq!(float_outcome, DescriptorSpecializationCacheOutcome::Miss);
        }

        for (offset, (type_oid, expected_label)) in [
            (INT2OID, "dense_int2_sum_avg_plain"),
            (INT4OID, "dense_int4_sum_avg_plain"),
        ]
        .into_iter()
        .enumerate()
        {
            let integer_sum_avg = dense_integer_sum_avg_spec(type_oid);
            let first_word = 45 + i32::try_from(offset).expect("bounded integer index") * 4;
            let sum_avg_identity = DerivedArtifactIdentity::from_canonical_words(vec![
                first_word,
                first_word + 1,
                first_word + 2,
                first_word + 3,
            ]);
            let (specialization, outcome) = cached_descriptor_specialization(
                &sum_avg_identity,
                &integer_sum_avg,
                GroupedAggKernelMode::ParallelDenseInteger,
            );
            assert_eq!(specialization.label(), expected_label);
            assert_eq!(outcome, DescriptorSpecializationCacheOutcome::Miss);
        }

        let (cached, second) = cached_descriptor_specialization(
            &identity,
            &count,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(cached, plain);
        assert_eq!(second, DescriptorSpecializationCacheOutcome::Hit);

        let mut masked = count;
        masked.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 3,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        let (masked_variant, changed_outcome) = cached_descriptor_specialization(
            &changed,
            &masked,
            GroupedAggKernelMode::ParallelDenseCount,
        );
        assert_eq!(masked_variant.label(), "dense_count_sql_mask");
        assert_eq!(changed_outcome, DescriptorSpecializationCacheOutcome::Miss);

        let range = dense_integer_range_spec();
        let range_identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 10, 11, 12]);
        let (range_variant, range_outcome) = cached_descriptor_specialization(
            &range_identity,
            &range,
            GroupedAggKernelMode::ParallelDenseInteger,
        );
        assert_eq!(range_variant.label(), "dense_integer_expressions_plain");
        assert_eq!(range_outcome, DescriptorSpecializationCacheOutcome::Miss);
        let measure_range = dense_integer_measure_range_spec();
        let measure_range_identity =
            DerivedArtifactIdentity::from_canonical_words(vec![13, 14, 15, 16]);
        let (measure_range_variant, measure_range_outcome) = cached_descriptor_specialization(
            &measure_range_identity,
            &measure_range,
            GroupedAggKernelMode::ParallelDenseInteger,
        );
        assert_eq!(
            measure_range_variant.label(),
            "dense_integer_column_measure_range"
        );
        assert_eq!(
            measure_range_outcome,
            DescriptorSpecializationCacheOutcome::Miss
        );
        assert_eq!(
            DESCRIPTOR_SPECIALIZATION_CACHE.with(|cache| cache.borrow().len()),
            14,
            "a digest lookup never aliases a different complete canonical identity"
        );
    }

    fn h3_count_shape() -> (AggQuerySpec, AggOutputProjection) {
        const H3OID: u32 = 90_001;
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::H3CellToParent {
                cell: column(H3OID),
                resolution: 7,
            },
            type_oid: H3OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let projection = AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: H3OID,
                    result_type_oid: H3OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                    source_type_oid: 0,
                    result_type_oid: INT8OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            ],
        };
        (spec, projection)
    }

    fn geometry_typmod(geometry_type: u32, srid: i32) -> i32 {
        (srid << 8) | i32::try_from(geometry_type).expect("geometry tag fits i32") << 2
    }

    fn polygon_bytes(srid: i32, coordinate_pairs: u32) -> Box<[u8]> {
        assert!(coordinate_pairs >= 4);
        let srid = u32::try_from(srid).expect("test SRID is nonnegative");
        let mut bytes = vec![
            0,
            0,
            0,
            0,
            ((srid >> 16) & 0xff) as u8,
            ((srid >> 8) & 0xff) as u8,
            (srid & 0xff) as u8,
            0,
        ];
        bytes.extend_from_slice(&crate::engine::residency::RESIDENT_GEOMETRY_POLYGON.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&coordinate_pairs.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        let unique = coordinate_pairs - 1;
        for index in 0..unique {
            let angle = std::f64::consts::TAU * f64::from(index) / f64::from(unique);
            let (y, x) = angle.sin_cos();
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        bytes.extend_from_slice(&1.0_f64.to_le_bytes());
        bytes.extend_from_slice(&0.0_f64.to_le_bytes());
        bytes.into_boxed_slice()
    }

    fn spatial_count_shape() -> (AggQuerySpec, AggOutputProjection) {
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.fact_filter = FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Intersects,
            left: SpatialOperand::Column {
                column: column(TEST_GEOMETRY_OID),
                metadata: SpatialValueMetadata {
                    kind: SpatialValueKind::Geometry,
                    typmod: geometry_typmod(
                        crate::engine::residency::RESIDENT_GEOMETRY_POINT,
                        TEST_SRID,
                    ),
                    srid: Some(TEST_SRID),
                },
            },
            right: SpatialOperand::Constant {
                metadata: SpatialValueMetadata {
                    kind: SpatialValueKind::Geometry,
                    typmod: -1,
                    srid: Some(TEST_SRID),
                },
                bytes: polygon_bytes(TEST_SRID, 1_025),
            },
            distance: None,
        };
        (
            spec,
            projection(AggregateKind::Count, 0, u32::from(pg_sys::INT8OID)),
        )
    }

    fn postgis_catalog(fingerprint_words: Vec<i32>) -> PostgisCatalogIdentity {
        PostgisCatalogIdentity {
            extension_oid: pg_sys::Oid::from(1_u32),
            schema_oid: pg_sys::Oid::from(2_u32),
            geometry_type_oid: pg_sys::Oid::from(TEST_GEOMETRY_OID),
            intersects_fn_oid: pg_sys::Oid::from(10_u32),
            contains_fn_oid: pg_sys::Oid::from(11_u32),
            within_fn_oid: pg_sys::Oid::from(12_u32),
            covers_fn_oid: pg_sys::Oid::from(16_u32),
            coveredby_fn_oid: pg_sys::Oid::from(17_u32),
            dwithin_fn_oid: pg_sys::Oid::from(13_u32),
            distance_fn_oid: pg_sys::Oid::from(14_u32),
            is_valid_fn_oid: pg_sys::Oid::from(15_u32),
            fingerprint_words,
        }
    }

    #[test]
    fn spatial_capability_is_internal_while_planner_validation_stays_dark() {
        let (spec, projection) = spatial_count_shape();
        assert!(validate_runtime_capability(&spec, &projection).is_err());
        validate_internal_runtime_capability(&spec, &projection)
            .expect("executor-internal spatial capability should bind");
    }

    #[test]
    fn normal_spatial_candidate_checks_topology_orientation_and_geometry_not_fixture_size() {
        let (spec, projection) = spatial_count_shape();
        validate_normal_spatial_candidate_capability(&spec, &projection)
            .expect("the evidenced normal spatial candidate should bind");

        for predicate in [
            SpatialPredicateKind::Contains,
            SpatialPredicateKind::Covers,
            SpatialPredicateKind::DWithin,
        ] {
            let mut unsupported = spec.clone();
            let FilterSpec::Spatial {
                predicate: actual, ..
            } = &mut unsupported.fact_filter
            else {
                unreachable!("spatial test shape changed")
            };
            *actual = predicate;
            assert!(
                validate_normal_spatial_candidate_capability(&unsupported, &projection).is_err(),
                "{predicate:?} must remain outside normal admission"
            );
        }

        let mut reversed = spec.clone();
        let FilterSpec::Spatial { left, right, .. } = &mut reversed.fact_filter else {
            unreachable!("spatial test shape changed")
        };
        std::mem::swap(left, right);
        assert!(validate_normal_spatial_candidate_capability(&reversed, &projection).is_ok());
        let FilterSpec::Spatial { predicate, .. } = &mut reversed.fact_filter else {
            unreachable!()
        };
        *predicate = SpatialPredicateKind::Contains;
        assert!(validate_normal_spatial_candidate_capability(&reversed, &projection).is_ok());
        let mut within = spec.clone();
        let FilterSpec::Spatial { predicate, .. } = &mut within.fact_filter else {
            unreachable!()
        };
        *predicate = SpatialPredicateKind::Within;
        assert!(validate_normal_spatial_candidate_capability(&within, &projection).is_ok());

        for (mut candidate, kind) in [
            (reversed.clone(), SpatialPredicateKind::Covers),
            (within.clone(), SpatialPredicateKind::CoveredBy),
        ] {
            let FilterSpec::Spatial { predicate, .. } = &mut candidate.fact_filter else {
                unreachable!()
            };
            *predicate = kind;
            assert!(validate_normal_spatial_candidate_capability(&candidate, &projection).is_ok());
            let FilterSpec::Spatial { left, right, .. } = &mut candidate.fact_filter else {
                unreachable!()
            };
            std::mem::swap(left, right);
            assert!(validate_normal_spatial_candidate_capability(&candidate, &projection).is_err());
        }

        let mut grouped = spec.clone();
        grouped.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        assert!(validate_normal_spatial_candidate_capability(&grouped, &projection).is_err());

        let mut multi = spec.clone();
        let FilterSpec::Spatial {
            right: SpatialOperand::Constant { bytes, metadata },
            ..
        } = &mut multi.fact_filter
        else {
            unreachable!()
        };
        let polygon = bytes.to_vec();
        let mut encoded = polygon[..8].to_vec();
        encoded.extend_from_slice(
            &crate::engine::residency::RESIDENT_GEOMETRY_MULTIPOLYGON.to_le_bytes(),
        );
        encoded.extend_from_slice(&2_u32.to_le_bytes());
        encoded.extend_from_slice(&polygon[8..]);
        encoded.extend_from_slice(&polygon[8..]);
        *bytes = encoded.into_boxed_slice();
        metadata.typmod = (TEST_SRID << 8) | (6 << 2);
        assert!(validate_normal_spatial_candidate_capability(&multi, &projection).is_ok());
        assert!(
            spatial_artifact_kind(&multi, &postgis_catalog(vec![101, 202, 303]), 1_024).is_ok()
        );

        let mut unmeasured_polygon = spec;
        let FilterSpec::Spatial {
            right: SpatialOperand::Constant { bytes, .. },
            ..
        } = &mut unmeasured_polygon.fact_filter
        else {
            unreachable!("spatial test shape changed")
        };
        *bytes = polygon_bytes(TEST_SRID, 1_024);
        assert!(
            validate_normal_spatial_candidate_capability(&unmeasured_polygon, &projection).is_ok(),
            "nearby valid polygon sizes use work limits, not fixture matching"
        );
    }

    #[test]
    fn spatial_kind_requests_source_and_fingerprint_changes_identity() {
        let (spec, projection) = spatial_count_shape();
        let catalog = postgis_catalog(vec![101, 202, 303]);
        let kind =
            spatial_artifact_kind(&spec, &catalog, 1_024).expect("valid spatial kind should bind");
        let DescriptorArtifactKind::Spatial(plan) = kind else {
            panic!("spatial spec selected a nonspatial artifact kind");
        };
        assert_eq!(
            plan.column_request(),
            ResidentColumnRef {
                relid: pg_sys::Oid::from(42_u32),
                attno: 1,
            }
        );
        let requests = artifact_column_refs(&spec).expect("spatial artifact requests bind");
        assert!(requests.contains(&plan.column_request()));

        let first = descriptor_artifact_identity(&spec, &projection, &catalog.fingerprint_words)
            .expect("first identity encodes");
        let second = descriptor_artifact_identity(&spec, &projection, &[101, 202, 304])
            .expect("second identity encodes");
        assert_ne!(first, second);
        assert!(
            first
                .canonical_words()
                .ends_with(&catalog.fingerprint_words)
        );
    }

    #[test]
    fn catalog_verification_precedes_guarded_operation() {
        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = verify_catalog_before(
            || {
                calls.borrow_mut().push("verify");
                Ok(())
            },
            || {
                calls.borrow_mut().push("ensure");
                Ok(ArtifactEnsureOutcome::Hit)
            },
        )
        .expect("verified cache hit should pass");
        assert_eq!(outcome, ArtifactEnsureOutcome::Hit);
        assert_eq!(*calls.borrow(), ["verify", "ensure"]);

        let ensure_called = std::cell::Cell::new(false);
        let error = verify_catalog_before(
            || Err(ResidentLoadError::Loader("catalog changed".to_owned())),
            || {
                ensure_called.set(true);
                Ok(ArtifactEnsureOutcome::Hit)
            },
        )
        .expect_err("catalog mismatch must reject before cache lookup");
        assert!(!ensure_called.get());
        assert!(error.to_string().contains("catalog changed"));
    }

    #[test]
    fn missing_residency_rebuilds_once_and_never_loops() {
        for missing_column in [false, true] {
            let execute_calls = std::cell::Cell::new(0_u32);
            let rebuild_calls = std::cell::Cell::new(0_u32);
            let outcome = execute_with_single_artifact_rebuild(
                || -> Result<Result<u32, DescriptorAggExecutionError>, ResidentLoadError> {
                    execute_calls.set(execute_calls.get() + 1);
                    if execute_calls.get() == 1 {
                        if missing_column {
                            Err(ResidentLoadError::MissingColumn {
                                relid: pg_sys::Oid::from(42_u32),
                                attno: 3,
                            })
                        } else {
                            Err(ResidentLoadError::MissingRelation(pg_sys::Oid::from(
                                42_u32,
                            )))
                        }
                    } else {
                        Ok(Ok(7))
                    }
                },
                || {
                    rebuild_calls.set(rebuild_calls.get() + 1);
                    Ok(9)
                },
            )
            .expect("one rebuild should repair missing resident input");
            assert_eq!(outcome, (7, Some(9)));
            assert_eq!(execute_calls.get(), 2);
            assert_eq!(rebuild_calls.get(), 1);
        }

        let execute_calls = std::cell::Cell::new(0_u32);
        let rebuild_calls = std::cell::Cell::new(0_u32);
        let second_failure = execute_with_single_artifact_rebuild(
            || -> Result<Result<(), DescriptorAggExecutionError>, ResidentLoadError> {
                execute_calls.set(execute_calls.get() + 1);
                Err(ResidentLoadError::MissingRelation(pg_sys::Oid::from(
                    42_u32,
                )))
            },
            || {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Ok(())
            },
        );
        assert!(matches!(
            second_failure,
            Err(DescriptorAggExecutionError::Failure(message))
                if message.contains("is not resident")
        ));
        assert_eq!(
            execute_calls.get(),
            2,
            "a second miss must not execute again"
        );
        assert_eq!(
            rebuild_calls.get(),
            1,
            "a second miss must not rebuild again"
        );
    }

    #[test]
    fn auto_load_disabled_rebuild_failure_stops_before_retry() {
        let relid = pg_sys::Oid::from(42_u32);
        let execute_calls = std::cell::Cell::new(0_u32);
        let rebuild_calls = std::cell::Cell::new(0_u32);
        let outcome = execute_with_single_artifact_rebuild(
            || -> Result<Result<(), DescriptorAggExecutionError>, ResidentLoadError> {
                execute_calls.set(execute_calls.get() + 1);
                Err(ResidentLoadError::MissingRelation(relid))
            },
            || -> Result<(), DescriptorAggExecutionError> {
                rebuild_calls.set(rebuild_calls.get() + 1);
                Err(DescriptorAggExecutionError::from(
                    ResidentLoadError::AutoLoadDisabled { relid },
                ))
            },
        );
        assert!(matches!(
            outcome,
            Err(DescriptorAggExecutionError::Failure(message))
                if message.contains("auto_load is off")
        ));
        assert_eq!(execute_calls.get(), 1);
        assert_eq!(rebuild_calls.get(), 1);
    }

    #[test]
    fn spatial_mask_binding_rechecks_empty_and_nonempty_shape() {
        assert_eq!(spatial_final_mask_binding(0, None), Ok(None));

        let mask = [1_i8, -1_i8];
        let mask_ptr = mask.as_ptr();
        assert_eq!(
            spatial_final_mask_binding(mask.len(), Some((mask.len(), mask_ptr))),
            Ok(Some(mask_ptr))
        );
        assert!(spatial_final_mask_binding(2, Some((1, mask_ptr))).is_err());
        assert!(spatial_final_mask_binding(2, Some((2, std::ptr::null()))).is_err());
        assert!(spatial_final_mask_binding(2, None).is_err());
        assert!(spatial_final_mask_binding(0, Some((0, mask_ptr))).is_err());

        let empty = build_sql_mask_filter(0, None).expect("zero rows disable the filter");
        assert_eq!(empty.kind, abi::PGACCEL_GROUPED_AGG_FILTER_NONE);
        assert!(empty.mask.is_null());
        let bound = build_sql_mask_filter(mask.len(), Some(mask_ptr))
            .expect("nonempty mask should bind as SQL filter");
        assert_eq!(bound.kind, abi::PGACCEL_GROUPED_AGG_FILTER_SQL);
        assert_eq!(bound.mask, mask_ptr);
    }

    #[test]
    fn runtime_backstop_accepts_only_exact_h3_parent_count_shape() {
        let (spec, projection) = h3_count_shape();
        validate_runtime_capability(&spec, &projection).expect("exact H3 COUNT(*) shape");

        let mut wrong = spec.clone();
        wrong.measures[0].outputs[0].kind = AggregateKind::Sum;
        assert!(validate_runtime_capability(&wrong, &projection).is_err());

        let mut wrong = spec;
        let GroupKeySource::H3CellToParent { resolution, .. } = &mut wrong.group_keys[0].source
        else {
            unreachable!()
        };
        *resolution = 16;
        assert!(validate_runtime_capability(&wrong, &projection).is_err());
    }

    #[test]
    fn native_hash_column_count_does_not_admit_unqualified_sql() {
        for type_oid in [
            BOOLOID,
            INT2OID,
            INT4OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
        ] {
            let (mut spec, mut projection) = h3_count_shape();
            let mut input = column(type_oid);
            input.attno = 2;
            spec.measures[0].expression = MeasureExpr::Column(input);
            projection.slots[1].source_type_oid = type_oid;
            assert_eq!(
                planned_descriptor_kernel_mode(&spec, true),
                GroupedAggKernelMode::SerialGeneric
            );
            assert!(validate_runtime_capability(&spec, &projection).is_err());
            assert!(validate_normal_descriptor_capability(&spec, &projection).is_err());
        }
    }

    #[test]
    fn h3_group_capacity_obeys_universe_and_runtime_bounds() {
        assert_eq!(h3_group_capacity(0, 0, usize::MAX), Ok(1));
        assert_eq!(h3_group_capacity(1_000, 0, usize::MAX), Ok(123));
        assert_eq!(h3_group_capacity(7, 15, usize::MAX), Ok(7));
        assert_eq!(h3_group_capacity(12_748_986, 9, 16_776_704), Ok(12_748_986));
        assert!(h3_group_capacity(16_776_705, 9, 16_776_704).is_err());
        assert!(h3_group_capacity(1_000, 15, 19).is_err());
        assert!(h3_group_capacity((1_usize << 30) + 1, 15, usize::MAX).is_err());
        assert!(h3_group_capacity(1, 0, 0).is_err());
        assert!(h3_group_capacity(1, -1, usize::MAX).is_err());
        assert!(h3_group_capacity(1, 16, usize::MAX).is_err());
    }

    #[test]
    fn h3_count_partition_rejects_invalid_compact_state() {
        validate_h3_count_partition(&[2, 3, 0], 3, 2, 5, HashMeasureInput::Star)
            .expect("valid emitted counts with a zero tail");
        assert!(validate_h3_count_partition(&[2, 0, 0], 3, 2, 2, HashMeasureInput::Star).is_err());
        assert!(
            validate_h3_count_partition(&[u64::MAX, 1], 2, 2, u64::MAX, HashMeasureInput::Star)
                .is_err()
        );
        assert!(validate_h3_count_partition(&[2, 3, 0], 3, 2, 6, HashMeasureInput::Star).is_err());
        assert!(validate_h3_count_partition(&[2, 3, 1], 3, 2, 5, HashMeasureInput::Star).is_err());
    }

    #[test]
    fn runtime_backstop_accepts_int4_binary_sum() {
        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        let spec = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Mul,
                lhs: column(INT4OID),
                rhs,
            },
            AggregateKind::Sum,
        );
        validate_runtime_capability(&spec, &projection(AggregateKind::Sum, INT4OID, INT8OID))
            .expect("INT4 multiplication has checked PostgreSQL overflow semantics");
    }

    #[test]
    fn runtime_backstop_declines_int8_multiplication() {
        let mut rhs = column(INT8OID);
        rhs.attno = 2;
        let spec = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Mul,
                lhs: column(INT8OID),
                rhs,
            },
            AggregateKind::Count,
        );
        assert!(
            validate_runtime_capability(&spec, &projection(AggregateKind::Count, INT8OID, INT8OID))
                .is_err()
        );
    }

    #[test]
    fn runtime_backstop_accepts_single_int8_star_membership_key() {
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.star_dims.push(DimSpec {
            relation_oid: 43,
            fact_key: column(INT8OID),
            dim_key: ColumnRef {
                relation_oid: 43,
                attno: 1,
                type_oid: INT8OID,
            },
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Counted,
            filter: FilterSpec::None,
        });

        validate_runtime_capability(&spec, &projection(AggregateKind::Count, 0, INT8OID))
            .expect("single exact INT8 star membership is supported");
        assert!(
            validate_normal_descriptor_capability(
                &spec,
                &projection(AggregateKind::Count, 0, INT8OID)
            )
            .is_err(),
            "executor support must not widen normal admission beyond the measured INT8 star"
        );
    }

    fn normal_int8_star_shape() -> (AggQuerySpec, AggOutputProjection) {
        let dimensions = [43_u32, 44_u32]
            .into_iter()
            .map(|relation_oid| DimSpec {
                relation_oid,
                fact_key: ColumnRef {
                    relation_oid: 42,
                    attno: i32::try_from(relation_oid - 41).expect("test attno"),
                    type_oid: INT8OID,
                },
                dim_key: ColumnRef {
                    relation_oid,
                    attno: 1,
                    type_oid: INT8OID,
                },
                collation_oid: 0,
                multiplicity: JoinMultiplicity::Unique,
                filter: FilterSpec::None,
            })
            .collect::<Vec<_>>();
        let group_keys = dimensions
            .iter()
            .enumerate()
            .map(|(index, dimension)| GroupKeyRef {
                source: GroupKeySource::StarDimension {
                    dim_index: u32::try_from(index).expect("test dimension index"),
                    group_column: ColumnRef {
                        relation_oid: dimension.relation_oid,
                        attno: 2,
                        type_oid: INT4OID,
                    },
                },
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            })
            .collect();
        let spec = AggQuerySpec {
            fact_rel: 42,
            group_keys,
            measures: vec![
                MeasureSpec {
                    expression: MeasureExpr::Column(ColumnRef {
                        relation_oid: 42,
                        attno: 4,
                        type_oid: INT4OID,
                    }),
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    }],
                    filter: FilterSpec::None,
                },
                MeasureSpec {
                    expression: MeasureExpr::CountStar,
                    outputs: vec![AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }],
                    filter: FilterSpec::None,
                },
            ],
            fact_filter: FilterSpec::None,
            star_dims: dimensions,
            having: None,
        };
        let projection = AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: INT4OID,
                    result_type_oid: INT4OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 1 },
                    source_type_oid: INT4OID,
                    result_type_oid: INT4OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    },
                    source_type_oid: INT4OID,
                    result_type_oid: INT8OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 1,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                    source_type_oid: 0,
                    result_type_oid: INT8OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            ],
        };
        (spec, projection)
    }

    #[test]
    fn normal_int8_star_admission_is_exactly_the_measured_ssbm_contract() {
        let (spec, projection) = normal_int8_star_shape();
        validate_normal_descriptor_capability(&spec, &projection)
            .expect("the measured two-dimension INT8 star should bind");

        let mut counted = spec.clone();
        counted.star_dims[0].multiplicity = JoinMultiplicity::Counted;
        assert!(validate_normal_descriptor_capability(&counted, &projection).is_err());

        let mut filtered = spec.clone();
        filtered.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 42,
                attno: 5,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        assert!(validate_normal_descriptor_capability(&filtered, &projection).is_err());

        let mut noncanonical_projection = projection;
        noncanonical_projection.slots[0].nullable = true;
        assert!(validate_normal_descriptor_capability(&spec, &noncanonical_projection).is_err());
    }

    #[test]
    fn runtime_backstop_requires_unresolved_dictionary_keys() {
        let mut spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::DenseI32 {
                code_min: 0,
                cardinality: 1,
                null_code: None,
            },
        });
        let projection = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::GroupKey { key_index: 0 },
                source_type_oid: INT4OID,
                result_type_oid: INT4OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            }],
        };
        assert!(validate_runtime_capability(&spec, &projection).is_err());
    }

    #[test]
    fn logical_temporal_scalars_map_to_distinct_abi_tags() {
        let date = scalar_value(ScalarValue::Date(-7));
        let timestamp = scalar_value(ScalarValue::Timestamp(-11));
        let timestamptz = scalar_value(ScalarValue::TimestampTz(13));
        assert_eq!(date.tag, PgaccelValTag::Date);
        assert_eq!(date.data, (-7_i32) as u64);
        assert_eq!(timestamp.tag, PgaccelValTag::Timestamp);
        assert_eq!(timestamp.data, (-11_i64) as u64);
        assert_eq!(timestamptz.tag, PgaccelValTag::Timestamp);
        assert_eq!(timestamptz.data, 13);
    }

    #[test]
    fn predicate_only_columns_use_exact_physical_types() {
        for (type_oid, expected_physical, expected_bytes) in [
            (BOOLOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1),
            (INT2OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4),
            (FLOAT4OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4),
            (DATEOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 4),
            (TIMESTAMPOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 8),
            (
                TIMESTAMPTZOID,
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
                8,
            ),
        ] {
            let column = measure_column(
                &ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(type_oid),
                },
                0,
            )
            .expect("predicate-only type binds");
            assert_eq!(column.physical_type, expected_physical);
            assert_eq!(column.element_bytes, expected_bytes);
        }
    }

    #[test]
    fn runtime_backstop_matches_straightforward_aggregate_matrix() {
        for (type_oid, kinds) in [
            (BOOLOID, &[AggregateKind::Count][..]),
            (
                INT2OID,
                &[AggregateKind::Count, AggregateKind::Min, AggregateKind::Max][..],
            ),
            (
                FLOAT4OID,
                &[AggregateKind::Count, AggregateKind::Min, AggregateKind::Max][..],
            ),
            (
                DATEOID,
                &[AggregateKind::Count, AggregateKind::Min, AggregateKind::Max][..],
            ),
            (
                TIMESTAMPOID,
                &[AggregateKind::Count, AggregateKind::Min, AggregateKind::Max][..],
            ),
            (
                TIMESTAMPTZOID,
                &[AggregateKind::Count, AggregateKind::Min, AggregateKind::Max][..],
            ),
        ] {
            for &kind in kinds {
                let spec = spec(MeasureExpr::Column(column(type_oid)), kind);
                let result_type_oid = if kind == AggregateKind::Count {
                    INT8OID
                } else {
                    type_oid
                };
                validate_runtime_capability(&spec, &projection(kind, type_oid, result_type_oid))
                    .unwrap_or_else(|error| {
                        panic!("type OID {type_oid} {kind:?} should bind: {error}")
                    });
            }
        }
        for type_oid in [INT2OID, INT4OID] {
            for (kind, result_type_oid) in [
                (AggregateKind::Sum, INT8OID),
                (AggregateKind::Avg, NUMERICOID),
            ] {
                let spec = spec(MeasureExpr::Column(column(type_oid)), kind);
                validate_runtime_capability(&spec, &projection(kind, type_oid, result_type_oid))
                    .unwrap_or_else(|error| {
                        panic!("widened integer type OID {type_oid} {kind:?} should bind: {error}")
                    });
            }
        }
    }

    #[test]
    fn typed_column_count_runtime_contract_matches_the_physical_abi() {
        for (type_oid, physical_type, element_bytes) in [
            (BOOLOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1),
            (INT2OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 4),
            (INT8OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64, 8),
            (FLOAT4OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4),
            (FLOAT8OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8),
            (DATEOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 4),
            (TIMESTAMPOID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 8),
            (
                TIMESTAMPTZOID,
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP,
                8,
            ),
        ] {
            let spec = spec(MeasureExpr::Column(column(type_oid)), AggregateKind::Count);
            validate_runtime_capability(
                &spec,
                &projection(AggregateKind::Count, type_oid, INT8OID),
            )
            .unwrap_or_else(|error| {
                panic!("nullable typed column COUNT OID {type_oid} should bind: {error}")
            });

            let binding = measure_column(
                &ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(type_oid),
                },
                0,
            )
            .unwrap_or_else(|error| {
                panic!("empty resident column OID {type_oid} should preserve its ABI: {error}")
            });
            assert_eq!(binding.physical_type, physical_type);
            assert_eq!(binding.element_bytes, element_bytes);
            assert!(binding.values.is_null());
            assert!(binding.nulls.is_null());
        }
    }

    #[test]
    fn float_count_descriptors_use_integer_count_state_without_reading_payloads() {
        for (type_oid, physical_type, element_bytes) in [
            (FLOAT4OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4),
            (FLOAT8OID, abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 8),
        ] {
            let spec = dense_float_count_spec(type_oid);
            let desc = zero_descriptor(&spec, &[BOOLOID], &[type_oid]);
            assert_eq!(
                desc.measures[0].agg_mask,
                abi::PGACCEL_GROUPED_AGG_LANE_COUNT
            );
            assert_eq!(
                desc.measures[0].accumulator_kind,
                abi::PGACCEL_GROUPED_AGG_ACCUM_I64
            );
            assert_eq!(desc.measures[0].value.physical_type, physical_type);
            assert_eq!(desc.measures[0].value.element_bytes, element_bytes);
            assert!(parallel_dense_count_shape(&desc));
        }
    }

    #[test]
    fn integer_sum_avg_admits_bounded_multi_key_dictionaries() {
        for type_oid in [INT2OID, INT4OID] {
            for key_type in [
                BOOLOID,
                INT2OID,
                INT4OID,
                INT8OID,
                FLOAT4OID,
                FLOAT8OID,
                DATEOID,
                TIMESTAMPOID,
                TIMESTAMPTZOID,
                TEXTOID,
                VARCHAROID,
                BPCHAROID,
            ] {
                let mut spec = dense_integer_sum_avg_spec(type_oid);
                spec.measures.pop();
                let mut key = spec.group_keys[0].clone();
                key.source = GroupKeySource::FactColumn(ColumnRef {
                    relation_oid: spec.fact_rel,
                    attno: 3,
                    type_oid: key_type,
                });
                key.type_oid = key_type;
                key.collation_oid = if is_text_family(key_type) { 950 } else { 0 };
                spec.group_keys = vec![key.clone()];
                for count in 1..=abi::PGACCEL_GROUPED_AGG_MAX_KEYS {
                    spec.group_keys.resize(count, key.clone());
                    assert_eq!(
                        planned_descriptor_kernel_mode(&spec, false),
                        GroupedAggKernelMode::ParallelDenseInteger
                    );
                }
                spec.group_keys.push(key);
                assert_eq!(
                    planned_descriptor_kernel_mode(&spec, false),
                    GroupedAggKernelMode::SerialGeneric
                );
                spec.group_keys.clear();
                assert_eq!(
                    planned_descriptor_kernel_mode(&spec, false),
                    GroupedAggKernelMode::ParallelDenseInteger
                );
            }
        }
    }

    #[test]
    fn independent_integer_filters_bind_raw_inputs_and_keep_exact_states() {
        let mut spec = dense_integer_sum_avg_spec(INT4OID);
        let count = spec.measures.pop().expect("count fixture");
        let mut second = spec.measures[0].clone();
        let MeasureExpr::Column(input) = spec.measures[0].expression else {
            unreachable!()
        };
        second.expression = MeasureExpr::Column(ColumnRef {
            attno: 3,
            type_oid: INT2OID,
            ..input
        });
        spec.measures[0].filter = FilterSpec::Ranges {
            input,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(0),
                hi: ScalarValue::I32(i32::MAX),
            }],
        };
        second.filter = FilterSpec::Ranges {
            input,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(i32::MIN),
                hi: ScalarValue::I32(-1),
            }],
        };
        spec.measures.push(second);
        spec.measures.push(count);
        assert!(canonical_dense_filtered_integer_sum_avg(&spec));
        let mode = planned_descriptor_kernel_mode(&spec, false);
        assert_eq!(mode, GroupedAggKernelMode::ParallelDenseInteger);
        assert_eq!(
            classify_descriptor_specialization(&spec, mode).label(),
            "dense_filtered_integer_sum_avg_plain"
        );
        let desc = zero_descriptor(&spec, &[BOOLOID], &[INT4OID, INT2OID]);
        assert!(parallel_dense_integer_shape(&desc));
        assert_eq!(desc.measure_filters[1].predicate_measure_slot, 1);
        let mut malformed = desc;
        malformed.measure_filters[0].predicate_measure_slot = 2; // COUNT has no value.
        assert!(!parallel_dense_integer_shape(&malformed));
        malformed = desc;
        malformed.measure_filters[0].predicate_range_count = -1;
        assert!(!parallel_dense_integer_shape(&malformed));
        malformed = desc;
        malformed.measure_filters[0].predicate_hi[0] = scalar_value(ScalarValue::I32(-1));
        assert!(!parallel_dense_integer_shape(&malformed));
        malformed = desc;
        malformed.measure_filters[0].predicate_hi[0] = scalar_value(ScalarValue::I32(0));
        assert!(parallel_dense_integer_shape(&malformed)); // Equality is exact.
        let mut unsupported = spec.clone();
        unsupported.measures[2].filter = spec.measures[0].filter.clone();
        assert!(canonical_dense_filtered_integer_sum_avg(&unsupported));
        let FilterSpec::Ranges { input, .. } = &mut spec.measures[0].filter else {
            unreachable!()
        };
        input.attno = 9;
        assert!(canonical_dense_filtered_integer_sum_avg(&spec));
        assert_eq!(
            planned_descriptor_kernel_mode(&spec, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );
        let FilterSpec::Ranges { input, .. } = &mut spec.measures[0].filter else {
            unreachable!()
        };
        input.relation_oid = 99;
        assert!(!canonical_dense_filtered_integer_sum_avg(&spec));
        assert_eq!(
            planned_descriptor_kernel_mode(&spec, false),
            GroupedAggKernelMode::SerialGeneric
        );
    }

    #[test]
    fn integer_count_outputs_share_nullable_states_and_match_physical_modes() {
        for filtered in [false, true] {
            for combined in [false, true] {
                for lanes in 1..=4 {
                    let mut spec = dense_integer_sum_avg_spec(INT4OID);
                    spec.measures.truncate(1);
                    if !combined {
                        spec.measures[0].outputs.clear();
                    }
                    spec.measures[0]
                        .outputs
                        .push(crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Count,
                        });
                    let value = spec.measures[0].clone();
                    spec.measures = (0..lanes)
                        .map(|slot| {
                            let mut measure = value.clone();
                            let MeasureExpr::Column(ref mut column) = measure.expression else {
                                unreachable!()
                            };
                            column.attno += slot as i32;
                            if filtered {
                                measure.filter = FilterSpec::Ranges {
                                    input: *column,
                                    ranges: vec![ScalarRange {
                                        lo: ScalarValue::I32(0),
                                        hi: ScalarValue::I32(20),
                                    }],
                                };
                            }
                            measure
                        })
                        .collect();
                    let mode = planned_descriptor_kernel_mode(&spec, false);
                    let desc = zero_descriptor(&spec, &[BOOLOID], &vec![INT4OID; lanes]);
                    if lanes == 1 && !filtered && !combined {
                        assert_eq!(mode, GroupedAggKernelMode::ParallelDenseCount);
                        assert!(parallel_dense_count_shape(&desc));
                        assert!(!parallel_dense_integer_shape(&desc));
                    } else {
                        assert_eq!(mode, GroupedAggKernelMode::ParallelDenseInteger);
                        assert!(parallel_dense_integer_shape(&desc));
                        assert_eq!(
                            classify_descriptor_specialization(&spec, mode).label(),
                            "dense_integer_with_counts_plain"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn multiple_integer_sum_avg_measures_bind_independent_states() {
        for lanes in 2..=abi::PGACCEL_GROUPED_AGG_MAX_MEASURES {
            for with_count in [false, true] {
                if lanes == abi::PGACCEL_GROUPED_AGG_MAX_MEASURES && with_count {
                    continue;
                }
                let mut spec = dense_integer_sum_avg_spec(INT4OID);
                let count = spec.measures.pop().expect("fixture count measure");
                let template = spec.measures[0].clone();
                spec.measures.clear();
                let mut types = Vec::new();
                for slot in 0..lanes {
                    let mut measure = template.clone();
                    let MeasureExpr::Column(column) = &mut measure.expression else {
                        unreachable!()
                    };
                    column.attno = i32::try_from(slot + 2).expect("bounded measure slot");
                    column.type_oid = if slot % 2 == 0 { INT2OID } else { INT4OID };
                    types.push(column.type_oid);
                    spec.measures.push(measure);
                }
                if with_count {
                    spec.measures.push(count);
                }
                assert!(canonical_dense_multi_integer_sum_avg(&spec));
                let mode = planned_descriptor_kernel_mode(&spec, false);
                assert_eq!(mode, GroupedAggKernelMode::ParallelDenseInteger);
                assert_eq!(
                    classify_descriptor_specialization(&spec, mode).label(),
                    "dense_multi_integer_sum_avg_plain"
                );
                let desc = zero_descriptor(&spec, &[BOOLOID], &types);
                assert!(parallel_dense_multi_integer_shape(&desc));
                assert!(dense_one_shot_eligible(&desc, 1_000_000));
                let mut bounded = desc;
                bounded.group_capacity = 100_000;
                let chunk_rows = dense_partial_chunk_rows(&bounded, 1_000_000);
                assert!(chunk_rows >= DENSE_INTEGER_CHUNK_ROWS);
                let bytes = bounded.group_capacity
                    * lanes
                    * chunk_rows.div_ceil(DENSE_INTEGER_CHUNK_ROWS)
                    * DENSE_INTEGER_PARTIAL_BYTES;
                assert!(bytes <= DENSE_INTEGER_MAX_PARTIAL_BYTES);
                assert!(
                    bytes + bounded.group_capacity * lanes * DENSE_INTEGER_PARTIAL_BYTES
                        > DENSE_INTEGER_MAX_PARTIAL_BYTES
                );
                let mut unsupported = desc;
                unsupported.measures[1].agg_mask |= abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ;
                assert!(!parallel_dense_integer_shape(&unsupported));
                let mut unsupported = spec.clone();
                unsupported.measures[1].outputs[0].kind = AggregateKind::StddevSamp;
                assert_eq!(
                    planned_descriptor_kernel_mode(&unsupported, false),
                    GroupedAggKernelMode::SerialGeneric
                );
                let mut unsupported = spec;
                let MeasureExpr::Column(column) = &mut unsupported.measures[1].expression else {
                    unreachable!()
                };
                column.type_oid = INT8OID;
                assert_eq!(
                    planned_descriptor_kernel_mode(&unsupported, false),
                    GroupedAggKernelMode::ParallelDenseNumeric
                );
            }
        }
    }

    #[test]
    fn integer_extrema_combine_with_all_exact_outputs_and_independent_filters() {
        let kinds = [
            AggregateKind::Sum,
            AggregateKind::Avg,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ];
        for type_oid in [INT2OID, INT4OID] {
            for bits in 1..32 {
                for grouped in [false, true] {
                    for filtered in [false, true] {
                        let mut spec = dense_integer_sum_avg_spec(type_oid);
                        spec.measures.truncate(1);
                        if !grouped {
                            spec.group_keys.clear();
                        }
                        spec.measures[0].outputs = kinds
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| bits & (1 << index) != 0)
                            .map(|(_, kind)| output(AggregateSource::Value, *kind))
                            .collect();
                        if filtered {
                            spec.measures[0].filter = FilterSpec::Ranges {
                                input: ColumnRef {
                                    relation_oid: spec.fact_rel,
                                    attno: 9,
                                    type_oid: INT4OID,
                                },
                                ranges: vec![ScalarRange {
                                    lo: ScalarValue::I32(-7),
                                    hi: ScalarValue::I32(9),
                                }],
                            };
                        }
                        assert!(canonical_dense_integer_rollup(&spec));
                        let expected = if bits == 4 && !filtered {
                            GroupedAggKernelMode::ParallelDenseCount
                        } else {
                            GroupedAggKernelMode::ParallelDenseInteger
                        };
                        assert_eq!(planned_descriptor_kernel_mode(&spec, false), expected);
                        let desc = zero_descriptor(
                            &spec,
                            if grouped { &[BOOLOID] } else { &[] },
                            &if filtered {
                                vec![type_oid, INT4OID]
                            } else {
                                vec![type_oid]
                            },
                        );
                        assert!(
                            parallel_dense_integer_shape(&desc)
                                || parallel_dense_count_shape(&desc)
                        );
                        assert_eq!(
                            desc.measures[0].agg_mask & abi::PGACCEL_GROUPED_AGG_LANE_MIN != 0,
                            bits & 8 != 0
                        );
                        assert_eq!(
                            desc.measures[0].agg_mask & abi::PGACCEL_GROUPED_AGG_LANE_MAX != 0,
                            bits & 16 != 0
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn widened_integer_sum_avg_descriptors_share_exact_sum_and_nonnull_state() {
        for type_oid in [INT2OID, INT4OID] {
            for kinds in [
                vec![AggregateKind::Sum],
                vec![AggregateKind::Avg],
                vec![AggregateKind::Sum, AggregateKind::Avg],
            ] {
                let mut optional = dense_integer_sum_avg_spec(type_oid);
                optional.measures[0]
                    .outputs
                    .retain(|output| kinds.contains(&output.kind));
                optional.measures.pop();
                let desc = zero_descriptor(&optional, &[BOOLOID], &[type_oid]);
                assert_eq!(desc.measure_count, 1);
                assert_eq!(desc.measures[0].agg_mask, abi::PGACCEL_GROUPED_AGG_LANE_SUM);
                assert_eq!(
                    desc.measures[0].accumulator_kind,
                    abi::PGACCEL_GROUPED_AGG_ACCUM_I64
                );
                assert!(parallel_dense_integer_shape(&desc));
                assert!(dense_one_shot_eligible(&desc, 1_000_000));
            }
            let spec = dense_integer_sum_avg_spec(type_oid);
            let desc = zero_descriptor(&spec, &[BOOLOID], &[type_oid]);
            assert_eq!(desc.measure_count, 2);
            assert_eq!(desc.measures[0].agg_mask, abi::PGACCEL_GROUPED_AGG_LANE_SUM);
            assert_eq!(
                desc.measures[0].accumulator_kind,
                abi::PGACCEL_GROUPED_AGG_ACCUM_I64
            );
            assert_eq!(
                desc.measures[0].value.physical_type,
                abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
            );
            assert_eq!(desc.measures[0].value.element_bytes, 4);
            assert_eq!(
                desc.measures[1].agg_mask,
                abi::PGACCEL_GROUPED_AGG_LANE_COUNT
            );
            assert!(parallel_dense_integer_shape(&desc));
        }
    }

    #[test]
    fn only_typed_gpu_numeric_overflow_reaches_sql_overflow_path() {
        let date = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::DateOutOfRange,
            ),
        );
        assert_eq!(date, DescriptorAggExecutionError::DateOutOfRange);
        assert_eq!(date.to_string(), "date out of range");
        let numeric = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::NumericOverflow,
            ),
        );
        assert_eq!(numeric, DescriptorAggExecutionError::NumericOverflow);

        let division = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::DivisionByZero,
            ),
        );
        assert_eq!(division, DescriptorAggExecutionError::DivisionByZero);
        assert_eq!(division.to_string(), "division by zero");

        let invalid = gpu_execution_error(
            "kernel failed",
            GpuError::new(
                crate::gpu::GpuErrorDomain::GroupedAgg,
                crate::gpu::GpuOperation::Kernel("grouped_agg_execute"),
                GpuStatusDetail::ExecutionFailed,
            ),
        );
        let DescriptorAggExecutionError::Failure(message) = invalid else {
            panic!("invalid runtime metadata must stay a generic execution failure")
        };
        assert!(message.contains("kernel failed"));
        assert!(message.contains("execution_failed"));
    }

    #[test]
    fn only_algorithmic_h3_parent_errors_are_external_routine_exceptions() {
        for status in [
            GpuStatusDetail::InvalidArgument,
            GpuStatusDetail::ShapeMismatch,
        ] {
            let error = DescriptorAggExecutionError::from(ResidentLoadError::Gpu(GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                status,
            )));
            assert!(matches!(
                error,
                DescriptorAggExecutionError::ExternalRoutineException(_)
            ));
        }

        for error in [
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::NoDevice,
            ),
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("another_h3_kernel"),
                GpuStatusDetail::InvalidArgument,
            ),
            GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::InvalidDescriptor,
            ),
            GpuError::new(
                GpuErrorDomain::GroupedAgg,
                GpuOperation::Kernel("h3_cell_to_parent_resident"),
                GpuStatusDetail::InvalidArgument,
            ),
        ] {
            assert!(matches!(
                DescriptorAggExecutionError::from(ResidentLoadError::Gpu(error)),
                DescriptorAggExecutionError::Failure(_)
            ));
        }
    }

    #[test]
    fn execution_errors_and_residency_reports_preserve_semantics() {
        assert_eq!(
            DescriptorAggExecutionError::NumericOverflow.to_string(),
            "numeric value out of range"
        );
        assert_eq!(
            DescriptorAggExecutionError::ExternalRoutineException("external".to_owned())
                .to_string(),
            "external"
        );
        assert_eq!(
            DescriptorAggExecutionError::from("failure".to_owned()),
            DescriptorAggExecutionError::Failure("failure".to_owned())
        );

        let mut report = DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Hit,
            artifact_policy: test_artifact_policy_evidence(),
            relations: Vec::new(),
            loaded_relations: vec![pg_sys::Oid::from(10_u32)],
            artifact_bytes: 10,
            raw_load_ms: 1.5,
            preparation_time_us: 7,
        };
        report.merge(DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Built,
            artifact_policy: test_artifact_policy_evidence(),
            relations: Vec::new(),
            loaded_relations: vec![pg_sys::Oid::from(10_u32), pg_sys::Oid::from(20_u32)],
            artifact_bytes: 20,
            raw_load_ms: 2.5,
            preparation_time_us: 11,
        });
        assert_eq!(report.artifact_outcome, ArtifactEnsureOutcome::Built);
        assert_eq!(
            report.loaded_relations,
            vec![pg_sys::Oid::from(10_u32), pg_sys::Oid::from(20_u32)]
        );
        assert_eq!(report.artifact_bytes, 20);
        assert_eq!(report.raw_load_ms, 4.0);
        assert_eq!(report.preparation_time_us, 18);

        report.merge(DescriptorResidencyReport {
            artifact_outcome: ArtifactEnsureOutcome::Hit,
            artifact_policy: test_artifact_policy_evidence(),
            relations: Vec::new(),
            loaded_relations: Vec::new(),
            artifact_bytes: 21,
            raw_load_ms: 0.0,
            preparation_time_us: u64::MAX,
        });
        assert_eq!(report.artifact_outcome, ArtifactEnsureOutcome::Built);
        assert_eq!(report.artifact_bytes, 21);
        assert_eq!(report.preparation_time_us, u64::MAX);
    }

    #[test]
    fn h3_count_partition_reports_shape_and_capacity_errors() {
        assert_eq!(
            validate_h3_count_partition(&[1], 2, 1, 1, HashMeasureInput::Star),
            Err("H3 COUNT(*) lane has length 1, expected 2".to_owned())
        );
        assert_eq!(
            validate_h3_count_partition(&[1], 1, 2, 1, HashMeasureInput::Star),
            Err("H3 emitted group count exceeds group capacity".to_owned())
        );
    }

    fn relation_evidence(relid: u32) -> ResidentRelationEvidence {
        ResidentRelationEvidence {
            relid: pg_sys::Oid::from(relid),
            generation: 1,
            global_generation: 1,
            relfilenode: pg_sys::Oid::from(relid + 10_000),
            row_count: 0,
            raw_bytes: 0,
            raw_accounting: ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: 0,
            },
            derived_bytes: 0,
            loaded_at_us: 0,
            last_used_us: 0,
            load_ms: 0.0,
        }
    }

    fn zero_artifact(spec: &AggQuerySpec, artifact_types: &[u32]) -> DescriptorAggArtifact {
        let requests = artifact_column_refs(spec).expect("artifact requests resolve");
        assert_eq!(requests.len(), artifact_types.len());
        let columns = artifact_types
            .iter()
            .map(|type_oid| ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(*type_oid),
            })
            .collect();
        let relids = dependency_relids(spec);
        let prepared = prepare_agg_artifact(
            spec,
            &requests,
            ResidentInputBundle {
                columns,
                evidence: relids
                    .into_iter()
                    .map(|relid| relation_evidence(u32::from(relid)))
                    .collect(),
            },
            1_024,
        )
        .expect("zero-row artifact prepares");
        assert_eq!(prepared.device_bytes, 0);
        DescriptorAggArtifact::build(prepared.prepared).expect("empty lanes require no device")
    }

    fn zero_descriptor(
        spec: &AggQuerySpec,
        artifact_types: &[u32],
        dispatch_types: &[u32],
    ) -> abi::PgaccelGroupedAggDesc {
        assert!(
            !has_numeric_measure(spec)
                && !spec
                    .measures
                    .iter()
                    .any(|measure| matches!(measure.expression, MeasureExpr::Bytecode { .. })),
            "use an owned bound descriptor for NUMERIC/scalar views"
        );
        let artifact = zero_artifact(spec, artifact_types);
        let requests = dispatch_column_refs(&artifact.resolved_spec).expect("dispatch requests");
        assert_eq!(requests.len(), dispatch_types.len());
        let views = dispatch_types
            .iter()
            .map(|type_oid| ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(*type_oid),
            })
            .collect::<Vec<_>>();
        build_descriptor(&artifact, &requests, &views)
            .expect("zero descriptor binds")
            .desc
    }

    fn output(source: AggregateSource, kind: AggregateKind) -> AggregateOutput {
        AggregateOutput { source, kind }
    }

    #[test]
    fn integer_where_requires_bound_exact_predicate_inputs() {
        let mut integer = dense_integer_sum_avg_spec(INT4OID);
        let MeasureExpr::Column(value) = integer.measures[0].expression else {
            unreachable!()
        };
        for ranges in 1..=abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES {
            integer.fact_filter = FilterSpec::Ranges {
                input: value,
                ranges: (0..ranges)
                    .map(|n| ScalarRange {
                        lo: ScalarValue::I32(n as i32 * 10),
                        hi: ScalarValue::I32(n as i32 * 10 + 5),
                    })
                    .collect(),
            };
            assert!(canonical_dense_integer_rollup(&integer));
            assert_eq!(
                planned_descriptor_kernel_mode(&integer, false),
                GroupedAggKernelMode::ParallelDenseInteger
            );
        }
        for invalid in [
            ColumnRef { attno: 99, ..value },
            ColumnRef {
                relation_oid: 99,
                ..value
            },
            ColumnRef {
                type_oid: INT8OID,
                ..value
            },
        ] {
            let mut bad = integer.clone();
            if let FilterSpec::Ranges { input, .. } = &mut bad.fact_filter {
                *input = invalid;
            }
            assert!(!canonical_dense_integer_rollup(&bad));
        }
        integer.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 99,
                type_oid: BOOLOID,
                ..value
            },
            kind: MaskKind::Sql,
        };
        assert!(canonical_dense_integer_rollup(&integer));
        integer.group_keys.clear();
        assert!(canonical_dense_integer_rollup(&integer));
    }

    #[test]
    fn spatial_integer_rollups_preserve_projection_and_geometry_guards() {
        for kind in [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count] {
            for type_oid in [INT2OID, INT4OID] {
                let mut integer = spec(MeasureExpr::Column(column(type_oid)), kind);
                integer.fact_filter = spatial_count_shape().0.fact_filter;
                if let MeasureExpr::Column(ref mut value) = integer.measures[0].expression {
                    value.attno = 2;
                }
                let projection = projection(
                    kind,
                    type_oid,
                    if kind == AggregateKind::Avg {
                        NUMERICOID
                    } else {
                        INT8OID
                    },
                );
                assert!(canonical_dense_integer_rollup(&integer));
                assert!(
                    validate_normal_spatial_candidate_capability(&integer, &projection).is_ok()
                );
                assert!(validate_normal_descriptor_capability(&integer, &projection).is_err());
                assert_eq!(
                    planned_descriptor_kernel_mode(&integer, false),
                    if kind == AggregateKind::Count {
                        GroupedAggKernelMode::ParallelDenseCount
                    } else {
                        GroupedAggKernelMode::ParallelDenseInteger
                    }
                );
                let mut bad_output = projection.clone();
                bad_output.slots[0].result_type_oid = FLOAT8OID;
                assert!(
                    validate_normal_spatial_candidate_capability(&integer, &bad_output).is_err()
                );
                let FilterSpec::Spatial { predicate, .. } = &mut integer.fact_filter else {
                    unreachable!()
                };
                *predicate = SpatialPredicateKind::Covers;
                assert!(
                    validate_normal_spatial_candidate_capability(&integer, &projection).is_err()
                );
            }
        }
    }

    #[test]
    fn spatial_scalar_rollups_require_the_geometry_and_typed_program_contracts() {
        for kind in [
            AggregateKind::Sum,
            AggregateKind::Avg,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ] {
            let mut query = spec(scalar_expression_fixture(), kind);
            if let MeasureExpr::Bytecode { inputs, .. } = &mut query.measures[0].expression {
                for input in inputs {
                    input.attno += 2;
                }
            }
            query.fact_filter = spatial_count_shape().0.fact_filter;
            let result = match kind {
                AggregateKind::Avg => NUMERICOID,
                AggregateKind::Min | AggregateKind::Max => INT4OID,
                _ => INT8OID,
            };
            let projection = projection(kind, INT4OID, result);
            assert!(canonical_dense_scalar_programs(&query));
            assert!(validate_normal_spatial_candidate_capability(&query, &projection).is_ok());
            assert!(validate_normal_descriptor_capability(&query, &projection).is_err());
            assert_eq!(
                planned_descriptor_kernel_mode(&query, false),
                GroupedAggKernelMode::ParallelDenseInteger
            );
            let mut malformed = query.clone();
            if let MeasureExpr::Bytecode { program, .. } = &mut malformed.measures[0].expression {
                program[0] = 0;
            }
            assert!(validate_normal_spatial_candidate_capability(&malformed, &projection).is_err());
            let FilterSpec::Spatial {
                predicate,
                left,
                right,
                ..
            } = &mut query.fact_filter
            else {
                unreachable!()
            };
            *predicate = SpatialPredicateKind::Covers;
            std::mem::swap(left, right);
            assert!(validate_normal_spatial_candidate_capability(&query, &projection).is_ok());
            let FilterSpec::Spatial { predicate, .. } = &mut query.fact_filter else {
                unreachable!()
            };
            *predicate = SpatialPredicateKind::Within;
            assert!(validate_normal_spatial_candidate_capability(&query, &projection).is_err());
        }
    }

    #[test]
    fn spatial_where_mask_program_requires_the_exact_carrier_context() {
        use crate::engine::spec::scalar::{ScalarInstruction as I, ScalarProgram, ScalarType};
        let mut input = column(INT4OID);
        input.attno = 3;
        let inputs = vec![input];
        let program = ScalarProgram::new(
            vec![ScalarType::Int4],
            ScalarType::Bool,
            vec![I::PredicateMask],
        )
        .expect("mask producer")
        .encode();
        let mut query = spec(
            MeasureExpr::Bytecode {
                inputs: inputs.clone(),
                program: program.clone(),
                result_type_oid: BOOLOID,
            },
            AggregateKind::Count,
        );
        query.fact_filter = FilterSpec::SpatialAnd {
            spatial: Box::new(spatial_count_shape().0.fact_filter),
            scalar: Box::new(FilterSpec::Bytecode { inputs, program }),
        };
        let projection = projection(AggregateKind::Count, BOOLOID, INT8OID);
        assert!(query.validate().is_ok());
        assert!(validate_normal_spatial_candidate_capability(&query, &projection).is_ok());
        assert!(validate_normal_descriptor_capability(&query, &projection).is_err());
        let mut invalid = query.clone();
        invalid.fact_filter = *match invalid.fact_filter {
            FilterSpec::SpatialAnd { scalar, .. } => scalar,
            _ => unreachable!(),
        };
        assert!(validate_normal_descriptor_capability(&invalid, &projection).is_err());
        invalid = query.clone();
        invalid.measures.push(invalid.measures[0].clone());
        assert!(validate_normal_spatial_candidate_capability(&invalid, &projection).is_err());
        let FilterSpec::SpatialAnd { scalar, .. } = &mut query.fact_filter else {
            unreachable!()
        };
        let FilterSpec::Bytecode { program, .. } = scalar.as_mut() else {
            unreachable!()
        };
        *program = ScalarProgram::new(
            vec![ScalarType::Int4],
            ScalarType::Bool,
            vec![I::Bool(true)],
        )
        .expect("no mask")
        .encode();
        assert!(query.validate().is_err());
    }

    #[test]
    fn spatial_numeric_admission_keeps_geometry_and_projection_guards() {
        let mut numeric = spec(MeasureExpr::Column(column(NUMERICOID)), AggregateKind::Sum);
        numeric.fact_filter = spatial_count_shape().0.fact_filter;
        // The geometry and value are different physical attributes.
        if let MeasureExpr::Column(ref mut value) = numeric.measures[0].expression {
            value.attno = 2;
        }
        let projection = projection(AggregateKind::Sum, NUMERICOID, NUMERICOID);
        assert!(canonical_dense_numeric_sum_avg(&numeric));
        assert!(validate_normal_spatial_candidate_capability(&numeric, &projection).is_ok());
        assert!(validate_normal_descriptor_capability(&numeric, &projection).is_err());
        assert_eq!(
            planned_descriptor_kernel_mode(&numeric, false),
            GroupedAggKernelMode::ParallelDenseNumeric
        );
        let mut bad_output = projection.clone();
        bad_output.slots[0].result_type_oid = INT8OID;
        assert!(validate_normal_spatial_candidate_capability(&numeric, &bad_output).is_err());
        let FilterSpec::Spatial { predicate, .. } = &mut numeric.fact_filter else {
            unreachable!()
        };
        *predicate = SpatialPredicateKind::Covers;
        assert!(validate_normal_spatial_candidate_capability(&numeric, &projection).is_err());
        let FilterSpec::Spatial { left, right, .. } = &mut numeric.fact_filter else {
            unreachable!()
        };
        std::mem::swap(left, right);
        assert!(validate_normal_spatial_candidate_capability(&numeric, &projection).is_ok());
    }

    #[test]
    fn numeric_descriptor_capability_supports_independent_count_states() {
        let base = spec(MeasureExpr::Column(column(NUMERICOID)), AggregateKind::Sum);
        for kind in [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count] {
            let mut numeric = base.clone();
            numeric.measures[0].outputs = vec![output(AggregateSource::Value, kind)];
            let projection = projection(
                kind,
                NUMERICOID,
                if kind == AggregateKind::Count {
                    INT8OID
                } else {
                    NUMERICOID
                },
            );
            assert!(validate_runtime_capability(&numeric, &projection).is_ok());
            assert!(validate_normal_descriptor_capability(&numeric, &projection).is_ok());
            for count in 1..=abi::PGACCEL_GROUPED_AGG_MAX_MEASURES {
                numeric.measures = vec![base.measures[0].clone(); count];
                for grouped in [false, true] {
                    numeric.group_keys = if grouped {
                        dense_bool_count_spec().group_keys
                    } else {
                        vec![]
                    };
                    assert!(canonical_dense_numeric_sum_avg(&numeric));
                    assert_eq!(
                        planned_descriptor_kernel_mode(&numeric, false),
                        GroupedAggKernelMode::ParallelDenseNumeric
                    );
                    assert_eq!(
                        planned_descriptor_kernel_mode(&numeric, true),
                        GroupedAggKernelMode::SerialGeneric
                    );
                }
            }
        }
        for kind in [
            AggregateKind::Min,
            AggregateKind::Max,
            AggregateKind::StddevSamp,
        ] {
            let mut invalid = base.clone();
            invalid.measures[0].outputs = vec![output(AggregateSource::Value, kind)];
            assert!(!canonical_dense_numeric_sum_avg(&invalid));
            assert!(
                validate_measure_outputs(
                    &invalid.measures[0].expression,
                    &invalid.measures[0].outputs
                )
                .is_err()
            );
        }
        let mut mixed = base.clone();
        mixed.measures.push(
            spec(MeasureExpr::CountStar, AggregateKind::Count)
                .measures
                .remove(0),
        );
        assert!(canonical_dense_numeric_sum_avg(&mixed));
        mixed.measures.swap(0, 1);
        assert!(!canonical_dense_numeric_sum_avg(&mixed));
        mixed.measures[0] = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Sum)
            .measures
            .remove(0);
        assert!(canonical_dense_numeric_sum_avg(&mixed));
        for type_oid in [INT2OID, INT4OID, INT8OID] {
            mixed.measures[0].expression = MeasureExpr::Column(column(type_oid));
            for kind in [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count] {
                mixed.measures[0].outputs = vec![output(AggregateSource::Value, kind)];
                assert!(canonical_dense_numeric_sum_avg(&mixed));
            }
        }
        mixed.measures[0].expression = MeasureExpr::Column(column(FLOAT8OID));
        assert!(!canonical_dense_numeric_sum_avg(&mixed));
        let mut filtered = base.clone();
        filtered.fact_filter = FilterSpec::Mask {
            input: column(BOOLOID),
            kind: MaskKind::Sql,
        };
        assert!(canonical_dense_numeric_sum_avg(&filtered));
        filtered.measures[0].filter = filtered.fact_filter.clone();
        filtered.fact_filter = FilterSpec::None;
        assert!(!canonical_dense_numeric_sum_avg(&filtered));
        let mut foreign = base;
        foreign.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 43,
            ..column(NUMERICOID)
        });
        assert!(!canonical_dense_numeric_sum_avg(&foreign));
    }

    #[test]
    fn numeric_filter_binds_an_independent_typed_raw_input_without_a_value_state() {
        let mut numeric = spec(MeasureExpr::Column(column(NUMERICOID)), AggregateKind::Sum);
        let predicate = ColumnRef {
            attno: 2,
            ..column(INT4OID)
        };
        numeric.measures[0].filter = FilterSpec::Ranges {
            input: predicate,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(-1),
                hi: ScalarValue::I32(2),
            }],
        };
        assert!(canonical_dense_numeric_sum_avg(&numeric));
        let artifact = zero_artifact(&numeric, &[]);
        let requests = dispatch_column_refs(&numeric).expect("decimal and predicate requests");
        let views = [
            ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(NUMERICOID),
            },
            ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(INT4OID),
            },
        ];
        let bound = build_descriptor(&artifact, &requests, &views).expect("empty filtered binding");
        assert_eq!(bound.desc.measure_count, 1);
        assert_eq!(
            bound.desc.measures[0].flags,
            abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT
        );
        assert_eq!(
            bound.desc.measures[0].rhs.physical_type,
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        );
        assert_eq!(bound.desc.measures[0].rhs.element_bytes, 4);
        assert_eq!(
            bound.desc.measure_filters[0].predicate_source,
            abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
        );
        assert_eq!(bound.desc.measure_filters[0].predicate_measure_slot, 0);
        assert!(build_descriptor(&artifact, &requests[..1], &views[..1]).is_err());
        for invalid in [
            ColumnRef {
                relation_oid: 99,
                ..predicate
            },
            ColumnRef {
                type_oid: NUMERICOID,
                ..predicate
            },
            ColumnRef {
                type_oid: INT8OID,
                ..predicate
            },
        ] {
            let mut changed = numeric.clone();
            if let FilterSpec::Ranges { input, .. } = &mut changed.measures[0].filter {
                *input = invalid;
            }
            assert!(!canonical_dense_numeric_sum_avg(&changed));
        }
    }

    #[test]
    fn integer_filter_binds_predicate_only_columns_without_extra_states() {
        for type_oid in [INT2OID, INT4OID] {
            let mut query = spec(MeasureExpr::Column(column(type_oid)), AggregateKind::Sum);
            query.group_keys.clear();
            let predicate = ColumnRef {
                attno: 9,
                ..column(INT4OID)
            };
            query.measures[0].filter = FilterSpec::Ranges {
                input: predicate,
                ranges: vec![ScalarRange {
                    lo: ScalarValue::I32(1),
                    hi: ScalarValue::I32(1),
                }],
            };
            for count in 1..=4 {
                query.measures.resize(count, query.measures[0].clone());
                let artifact = zero_artifact(&query, &[]);
                let requests = dispatch_column_refs(&query).expect("value and predicate");
                assert_eq!(requests.len(), 2);
                let views = [
                    ResidentColumnView::Empty {
                        type_oid: pg_sys::Oid::from(type_oid),
                    },
                    ResidentColumnView::Empty {
                        type_oid: pg_sys::Oid::from(INT4OID),
                    },
                ];
                let bound =
                    build_descriptor(&artifact, &requests, &views).expect("predicate binding");
                assert_eq!(bound.desc.measure_count as usize, count);
                assert!(parallel_dense_integer_shape(&bound.desc));
                for slot in 0..count {
                    assert_eq!(
                        bound.desc.measure_filters[slot].predicate_measure_slot,
                        slot as i32
                    );
                    assert_eq!(
                        bound.desc.measure_filters[slot].predicate_source,
                        abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
                    );
                    assert_eq!(
                        bound.desc.measures[slot].flags,
                        abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT
                    );
                }
                assert!(build_descriptor(&artifact, &requests[..1], &views[..1]).is_err());
                let mut malformed = bound.desc;
                malformed.measures[0].flags = 0;
                assert!(!parallel_dense_integer_shape(&malformed));
                malformed = bound.desc;
                malformed.measures[0].rhs.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
                assert!(!parallel_dense_integer_shape(&malformed));
            }
        }
    }

    fn scalar_expression_fixture() -> MeasureExpr {
        MeasureExpr::Bytecode {
            inputs: vec![
                column(INT4OID),
                ColumnRef {
                    attno: 2,
                    ..column(BOOLOID)
                },
            ],
            program: vec![
                crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
                2,
                2,
                2,
                1,
                0,
                0,
            ],
            result_type_oid: INT4OID,
        }
    }

    #[test]
    fn scalar_metadata_forecast_covers_simultaneous_owners_and_rejects_opaque_wire() {
        let mut query = spec(scalar_expression_fixture(), AggregateKind::Sum);
        let baseline = resident_live_bytes();
        for slots in 1..=4 {
            query.measures.resize(slots, query.measures[0].clone());
            let artifact = zero_artifact(&query, &[]);
            let requests = dispatch_column_refs(&query).expect("requests");
            let views = [
                ResidentColumnView::Empty {
                    type_oid: pg_sys::INT4OID,
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::BOOLOID,
                },
            ];
            let bound = build_descriptor(&artifact, &requests, &views).expect("bound metadata");
            // SAFETY: zero-row descriptors have no resident payload. These
            // independent snapshots model bound + plan + session + chunk.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }.expect("plan");
            // SAFETY: same live zero-row metadata as above.
            let session_snapshot = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }
                .expect("independent session-sized snapshot");
            let chunk = plan.row_chunk(0, 0).expect("owned chunk");
            let peak = scalar_metadata_peak_bytes(&query).expect("peak");
            assert_eq!(peak, resident_live_bytes() - baseline);
            assert_eq!(peak, slots as u64 * 4 * (40 + 7 * 4 + 2 * 24));
            drop(chunk);
            drop(session_snapshot);
        }
        assert_eq!(resident_live_bytes(), baseline);
        query.measures.resize(5, query.measures[0].clone());
        assert!(scalar_metadata_peak_bytes(&query).is_err());
        query.measures.truncate(1);
        let MeasureExpr::Bytecode { program, .. } = &mut query.measures[0].expression else {
            unreachable!()
        };
        program[0] = 0;
        assert!(scalar_metadata_peak_bytes(&query).is_err());
        assert_eq!(
            scalar_metadata_peak_bytes(&spec(MeasureExpr::CountStar, AggregateKind::Count)),
            Ok(0)
        );
    }

    #[test]
    fn scalar_executor_binds_owned_metadata_and_independent_predicates() {
        let baseline = resident_live_bytes();
        let mut query = spec(scalar_expression_fixture(), AggregateKind::Sum);
        query.measures[0].filter = FilterSpec::NullOrRanges {
            input: ColumnRef {
                attno: 9,
                ..column(INT4OID)
            },
            ranges: vec![],
        };
        for slots in 1..=4 {
            query.measures.resize(slots, query.measures[0].clone());
            let artifact = zero_artifact(&query, &[]);
            let requests = dispatch_column_refs(&query).expect("scalar and predicate requests");
            assert_eq!(
                requests.iter().map(|r| r.attno).collect::<Vec<_>>(),
                vec![1, 2, 9]
            );
            let views = [
                ResidentColumnView::Empty {
                    type_oid: pg_sys::INT4OID,
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::BOOLOID,
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::INT4OID,
                },
            ];
            let bound = build_descriptor(&artifact, &requests, &views).expect("scalar descriptor");
            let snapshot_bytes =
                (slots * crate::gpu::scalar_metadata_bytes(7, 2).expect("metadata size")) as u64;
            assert_eq!(resident_live_bytes(), baseline + snapshot_bytes);
            assert!(parallel_dense_integer_shape(&bound.desc));
            for slot in 0..slots {
                let owner = bound._scalar_views[slot]
                    .as_ref()
                    .expect("owned scalar metadata");
                assert_eq!(owner._view.row_count, 0);
                assert_eq!(owner._inputs.len(), 2);
                assert_eq!(
                    owner._inputs[0].physical_type,
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                );
                assert_eq!(
                    owner._inputs[1].physical_type,
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
                );
                assert_eq!(owner._view.words, owner._words.as_ptr());
                assert_eq!(owner._view.inputs, owner._inputs.as_ptr());
                assert_eq!(
                    bound.desc.measures[slot].value.values,
                    std::ptr::from_ref(owner._view.as_ref()).cast()
                );
                assert_eq!(
                    bound.desc.measures[slot].op,
                    abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR
                );
                assert_eq!(
                    bound.desc.measure_filters[slot].predicate_source,
                    abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
                );
                assert_eq!(
                    bound.desc.measure_filters[slot].predicate_measure_slot,
                    slot as i32
                );
                assert_eq!(
                    bound.desc.measure_filters[slot].flags,
                    abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL
                );
            }
            assert!(build_descriptor(&artifact, &requests[..2], &views[..2]).is_err());
            // The bound owner can die once the facade has copied host metadata.
            // SAFETY: zero-row resident views contain no active payload pointers.
            let plan =
                unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }.expect("resolved scalar");
            assert_eq!(resident_live_bytes(), baseline + 2 * snapshot_bytes);
            drop(bound);
            assert_eq!(resident_live_bytes(), baseline + snapshot_bytes);
            assert!(plan.row_chunk(0, 0).is_ok());
        }
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn scalar_binding_rejects_opaque_programs_bad_oids_and_incomplete_bundles() {
        let expression = scalar_expression_fixture();
        let query = spec(expression.clone(), AggregateKind::Sum);
        let requests = dispatch_column_refs(&query).expect("requests");
        let views = [
            ResidentColumnView::Empty {
                type_oid: pg_sys::INT4OID,
            },
            ResidentColumnView::Empty {
                type_oid: pg_sys::BOOLOID,
            },
        ];
        let bind = |expr: &MeasureExpr, refs: &[ResidentColumnRef], rows| {
            scalar_measure_column(
                expr,
                42,
                refs,
                &views,
                rows,
                abi::PGACCEL_GROUPED_AGG_LANE_SUM,
            )
        };
        assert!(bind(&expression, &requests, 0).is_ok());
        assert!(bind(&expression, &requests, 1).is_err());
        assert!(bind(&expression, &requests[..1], 0).is_err());
        let duplicate = [requests[0], requests[0]];
        assert!(bind(&expression, &duplicate, 0).is_err());
        for kind in 0..7 {
            let mut changed = expression.clone();
            let MeasureExpr::Bytecode {
                inputs,
                program,
                result_type_oid,
            } = &mut changed
            else {
                unreachable!()
            };
            match kind {
                0 => *program = vec![1, 2, 3], // accepted opaque envelope is not executable SPG1
                1 => program[5] = 99,
                2 => inputs[0].type_oid = INT2OID,
                3 => inputs[0].relation_oid = 43,
                4 => inputs[0].attno = 0,
                5 => inputs[0].attno = i32::MAX,
                _ => *result_type_oid = BOOLOID,
            }
            assert!(bind(&changed, &requests, 0).is_err(), "mutation {kind}");
        }
        let wrong_views = [
            ResidentColumnView::Empty {
                type_oid: pg_sys::INT2OID,
            },
            ResidentColumnView::Empty {
                type_oid: pg_sys::BOOLOID,
            },
        ];
        assert!(
            scalar_measure_column(
                &expression,
                42,
                &requests,
                &wrong_views,
                0,
                abi::PGACCEL_GROUPED_AGG_LANE_SUM
            )
            .is_err()
        );
    }

    #[test]
    fn scalar_literal_boolean_and_date_count_bind_without_resident_inputs() {
        for ty in [1, 2, 6] {
            let query = spec(
                MeasureExpr::Bytecode {
                    inputs: vec![],
                    program: vec![
                        crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
                        ty,
                        0,
                        3,
                        ty,
                    ],
                    result_type_oid: match ty {
                        1 => BOOLOID,
                        6 => DATEOID,
                        _ => INT4OID,
                    },
                },
                AggregateKind::Count,
            );
            let artifact = zero_artifact(&query, &[]);
            let bound = build_descriptor(&artifact, &[], &[]).expect("typed NULL literal");
            assert!(parallel_dense_integer_shape(&bound.desc));
            if ty == 6 {
                for lanes in [
                    abi::PGACCEL_GROUPED_AGG_LANE_MIN,
                    abi::PGACCEL_GROUPED_AGG_LANE_MAX,
                    abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                        | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                        | abi::PGACCEL_GROUPED_AGG_LANE_MAX,
                ] {
                    let mut date = bound.desc;
                    date.measures[0].agg_mask = lanes;
                    assert!(parallel_dense_integer_shape(&date), "DATE extrema mirror");
                    // SAFETY: bound owns the complete scalar metadata; the
                    // zero-row literal has no resident payloads to dereference.
                    unsafe { ResolvedGroupedAggPlan::from_abi(date) }.expect("DATE extrema facade");
                    date.measures[0].agg_mask |= abi::PGACCEL_GROUPED_AGG_LANE_SUM;
                    assert!(
                        !parallel_dense_integer_shape(&date),
                        "DATE SUM is not an integer SUM"
                    );
                }
            }
            assert!(
                bound._scalar_views[0]
                    .as_ref()
                    .expect("view")
                    ._view
                    .inputs
                    .is_null()
            );
            let mut malformed = bound.desc;
            malformed.measures[0].value.element_bytes = 4;
            assert!(!parallel_dense_integer_shape(&malformed));
            if ty == 1 || ty == 6 {
                assert!(
                    scalar_measure_column(
                        &query.measures[0].expression,
                        42,
                        &[],
                        &[],
                        0,
                        abi::PGACCEL_GROUPED_AGG_LANE_SUM
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn scalar_where_requires_an_identical_unfiltered_boolean_carrier() {
        use crate::engine::spec::scalar::{ScalarInstruction as I, ScalarProgram, ScalarType};
        let inputs = vec![column(INT4OID)];
        let program = ScalarProgram::new(
            vec![ScalarType::Int4],
            ScalarType::Bool,
            vec![I::Input(0), I::Int4(0), I::Gt],
        )
        .expect("predicate")
        .encode();
        let expression = MeasureExpr::Bytecode {
            inputs: inputs.clone(),
            program: program.clone(),
            result_type_oid: BOOLOID,
        };
        let mut query = spec(expression, AggregateKind::Count);
        query.fact_filter = FilterSpec::Bytecode { inputs, program };
        assert!(query.validate().is_ok());
        assert!(canonical_dense_scalar_programs(&query));
        assert_eq!(scalar_where_measure_binding(&query), Some(0));
        let filter = build_filter(&query, 10, None).expect("scalar WHERE");
        assert_eq!(
            filter.predicate_source,
            abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR
        );
        assert_eq!(filter.predicate_range_count, 0);
        assert!(filter.mask.is_null());
        let raw = spec(MeasureExpr::Column(column(BOOLOID)), AggregateKind::Count);
        let mut desc = zero_descriptor(&raw, &[], &[BOOLOID]);
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL;
        desc.where_filter = filter;
        assert!(exact_dense_scalar_where(&desc));
        desc.where_filter.flags = abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL;
        assert!(!exact_dense_scalar_where(&desc));
        query.measures[0].filter = FilterSpec::Ranges {
            input: column(INT4OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(2),
            }],
        };
        assert!(!canonical_dense_scalar_programs(&query));
        assert!(build_filter(&query, 10, None).is_err());
        query.measures[0].filter = FilterSpec::None;
        let FilterSpec::Bytecode { program, .. } = &mut query.fact_filter else {
            unreachable!()
        };
        *program = ScalarProgram::new(vec![ScalarType::Int4], ScalarType::Int4, vec![I::Input(0)])
            .expect("non-Boolean")
            .encode();
        assert!(query.validate().is_err());
        assert!(build_filter(&query, 10, None).is_err());
    }

    #[test]
    fn scalar_runtime_family_admits_only_the_typed_dense_contract() {
        let query = spec(scalar_expression_fixture(), AggregateKind::Sum);
        let projection = projection(AggregateKind::Sum, INT4OID, INT8OID);
        assert!(query.validate().is_ok());
        assert!(validate_runtime_capability(&query, &projection).is_ok());
        assert_eq!(
            planned_descriptor_kernel_mode(&query, false),
            GroupedAggKernelMode::ParallelDenseInteger
        );
        assert_eq!(
            planned_descriptor_kernel_mode(&query, true),
            GroupedAggKernelMode::SerialGeneric
        );
        assert_eq!(
            classify_descriptor_specialization(&query, GroupedAggKernelMode::ParallelDenseInteger)
                .label(),
            "dense_scalar_programs_plain"
        );
        assert!(validate_normal_descriptor_capability(&query, &projection).is_ok());
        let mut malformed = query.clone();
        malformed.measures[0].expression = MeasureExpr::Bytecode {
            inputs: vec![],
            program: vec![1],
            result_type_oid: INT4OID,
        };
        assert!(validate_runtime_capability(&malformed, &projection).is_err());
        assert_eq!(
            planned_descriptor_kernel_mode(&malformed, false),
            GroupedAggKernelMode::SerialGeneric
        );
        let mut oversized = query.clone();
        oversized.measures.resize(5, oversized.measures[0].clone());
        assert!(validate_runtime_capability(&oversized, &projection).is_err());
        let mut wrong_relation = query;
        wrong_relation.fact_rel = 99;
        assert!(validate_runtime_capability(&wrong_relation, &projection).is_err());
    }

    #[test]
    fn scalar_mixed_states_preserve_independent_raw_filters_and_specialization() {
        for type_oid in [INT2OID, INT4OID] {
            let mut query = spec(scalar_expression_fixture(), AggregateKind::Avg);
            let mut raw = spec(
                MeasureExpr::Column(ColumnRef {
                    attno: 3,
                    ..column(type_oid)
                }),
                AggregateKind::Count,
            )
            .measures
            .remove(0);
            raw.filter = FilterSpec::NullOrRanges {
                input: ColumnRef {
                    attno: 9,
                    ..column(INT4OID)
                },
                ranges: vec![],
            };
            query.measures.push(raw);
            query.measures.push(
                spec(MeasureExpr::CountStar, AggregateKind::Count)
                    .measures
                    .remove(0),
            );
            let projection = projection(AggregateKind::Avg, INT4OID, NUMERICOID);
            assert!(validate_runtime_capability(&query, &projection).is_ok());
            assert_eq!(
                planned_descriptor_kernel_mode(&query, false),
                GroupedAggKernelMode::ParallelDenseInteger
            );
            assert_eq!(
                classify_descriptor_specialization(
                    &query,
                    GroupedAggKernelMode::ParallelDenseInteger
                )
                .label(),
                "dense_scalar_programs_plain"
            );
            let artifact = zero_artifact(&query, &[]);
            let requests = dispatch_column_refs(&query).expect("mixed inputs");
            let views =
                [INT4OID, BOOLOID, type_oid, INT4OID].map(|oid| ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(oid),
                });
            let bound = build_descriptor(&artifact, &requests, &views).expect("mixed binding");
            assert!(parallel_dense_integer_shape(&bound.desc));
            assert_eq!(
                bound.desc.measures[1].flags,
                abi::PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT
            );
            assert_eq!(
                bound.desc.measure_filters[1].predicate_source,
                abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
            );
            query.fact_filter = FilterSpec::Mask {
                input: ColumnRef {
                    attno: 2,
                    ..column(BOOLOID)
                },
                kind: MaskKind::Sql,
            };
            assert_eq!(
                classify_descriptor_specialization(
                    &query,
                    GroupedAggKernelMode::ParallelDenseInteger
                )
                .label(),
                "dense_scalar_programs_sql_mask"
            );
        }
    }

    #[test]
    fn null_accepting_filters_bind_integer_decimal_and_filtered_star_counts() {
        for type_oid in [INT2OID, INT4OID, INT8OID, NUMERICOID] {
            for null_only in [false, true] {
                let mut query = spec(MeasureExpr::Column(column(type_oid)), AggregateKind::Sum);
                let predicate = ColumnRef {
                    attno: 9,
                    ..column(INT4OID)
                };
                let filter = FilterSpec::NullOrRanges {
                    input: predicate,
                    ranges: if null_only {
                        vec![]
                    } else {
                        vec![ScalarRange {
                            lo: ScalarValue::I32(-1),
                            hi: ScalarValue::I32(1),
                        }]
                    },
                };
                query.measures[0].filter = filter.clone();
                query.measures.push(crate::engine::spec::MeasureSpec {
                    expression: MeasureExpr::CountStar,
                    outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                    filter,
                });
                query.group_keys.clear();
                assert!(
                    canonical_dense_integer_rollup(&query)
                        || canonical_dense_numeric_sum_avg(&query)
                );
                let artifact = zero_artifact(&query, &[]);
                let requests = dispatch_column_refs(&query).expect("value and nullable predicate");
                let views = [
                    ResidentColumnView::Empty {
                        type_oid: pg_sys::Oid::from(type_oid),
                    },
                    ResidentColumnView::Empty {
                        type_oid: pg_sys::Oid::from(INT4OID),
                    },
                ];
                let bound =
                    build_descriptor(&artifact, &requests, &views).expect("nullable binding");
                assert_eq!(
                    bound.desc.measures[1].op,
                    abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
                );
                assert_eq!(
                    bound.desc.measures[1].agg_mask,
                    abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                );
                assert!(bound.desc.measures[1].value.nulls.is_null());
                for slot in 0..2 {
                    assert_eq!(
                        bound.desc.measure_filters[slot].flags,
                        abi::PGACCEL_GROUPED_AGG_FILTER_ACCEPT_NULL
                    );
                    assert_eq!(
                        bound.desc.measure_filters[slot].predicate_range_count,
                        i32::from(!null_only)
                    );
                    assert_eq!(
                        bound.desc.measure_filters[slot].predicate_source,
                        abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS
                    );
                }
                if matches!(type_oid, INT2OID | INT4OID) {
                    assert!(parallel_dense_integer_shape(&bound.desc));
                    let mut bad = bound.desc;
                    bad.measure_filters[0].flags = 2;
                    assert!(!parallel_dense_integer_shape(&bad));
                    if null_only {
                        bad = bound.desc;
                        bad.measure_filters[0].flags = 0;
                        assert!(!parallel_dense_integer_shape(&bad));
                    }
                }
            }
        }
    }

    #[test]
    fn bigint_numeric_binding_keeps_raw_buffers_and_independent_filters() {
        let mut query = spec(MeasureExpr::Column(column(INT8OID)), AggregateKind::Sum);
        query.group_keys.clear();
        query.measures[0].outputs = [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count]
            .into_iter()
            .map(|kind| output(AggregateSource::Value, kind))
            .collect();
        let predicate = ColumnRef {
            attno: 9,
            ..column(INT4OID)
        };
        query.measures[0].filter = FilterSpec::Ranges {
            input: predicate,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(1),
            }],
        };
        for count in 1..=4 {
            query.measures.resize(count, query.measures[0].clone());
            assert!(canonical_dense_numeric_sum_avg(&query));
            let artifact = zero_artifact(&query, &[]);
            let requests = dispatch_column_refs(&query).expect("raw bigint and predicate");
            let views = [
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(INT8OID),
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(INT4OID),
                },
            ];
            let bound = build_descriptor(&artifact, &requests, &views).expect("bigint binding");
            assert_eq!(bound.desc.measure_count as usize, count);
            for slot in 0..count {
                let measure = &bound.desc.measures[slot];
                assert_eq!(
                    measure.state_bytes,
                    abi::PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES
                );
                assert_eq!(
                    measure.accumulator_kind,
                    abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC
                );
                assert_eq!(
                    measure.value.physical_type,
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64
                );
                assert_eq!(measure.value.element_bytes, 8);
                assert_eq!(measure.value.flags, 0);
                assert!(bound._numeric_views[slot].is_none());
                assert_eq!(
                    bound.desc.measure_filters[slot].predicate_measure_slot,
                    slot as i32
                );
            }
            assert!(build_descriptor(&artifact, &requests[..1], &views[..1]).is_err());
        }
        query.measures.truncate(1);
        query.measures[0].filter = FilterSpec::None;
        query.measures[0].outputs = vec![output(AggregateSource::Value, AggregateKind::Count)];
        assert!(!query.measures[0].requires_numeric_accumulator());
        assert!(!canonical_dense_numeric_sum_avg(&query));
        for kind in [
            AggregateKind::Min,
            AggregateKind::Max,
            AggregateKind::StddevSamp,
        ] {
            query.measures[0].outputs = vec![
                output(AggregateSource::Value, AggregateKind::Sum),
                output(AggregateSource::Value, kind),
            ];
            assert!(!canonical_dense_numeric_sum_avg(&query));
        }
    }

    #[test]
    fn empty_numeric_binding_owns_metadata_and_shares_sum_avg_count() {
        let mut numeric = spec(MeasureExpr::Column(column(NUMERICOID)), AggregateKind::Sum);
        numeric.measures[0].outputs =
            [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count]
                .into_iter()
                .map(|kind| output(AggregateSource::Value, kind))
                .collect();
        let artifact = zero_artifact(&numeric, &[]);
        let requests = dispatch_column_refs(&artifact.resolved_spec).expect("dispatch columns");
        let views = [ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(NUMERICOID),
        }];
        let bound = build_descriptor(&artifact, &requests, &views).expect("numeric binding");
        let measure = &bound.desc.measures[0];
        assert_eq!(
            measure.agg_mask,
            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        );
        assert_eq!(
            measure.accumulator_kind,
            abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC
        );
        assert_eq!(measure.state_bytes, 32);
        assert_eq!(
            measure.value.values,
            std::ptr::from_ref(bound._numeric_views[0].as_deref().expect("pinned metadata")).cast()
        );
        // SAFETY: the bound descriptor pins the readable metadata, and an empty
        // relation has no device payloads to borrow. Construction snapshots it.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }.expect("numeric plan");
        drop(bound);
        let chunk = plan
            .row_chunk(0, 0)
            .expect("owned empty metadata survives binding");
        drop(chunk);
        assert_eq!(plan.row_count(), 0);
        numeric.measures[0].outputs = vec![output(AggregateSource::Value, AggregateKind::Count)];
        let artifact = zero_artifact(&numeric, &[]);
        let bound =
            build_descriptor(&artifact, &requests, &views).expect("empty numeric count binding");
        assert_eq!(
            bound.desc.measures[0].agg_mask,
            abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        );
        assert_eq!(bound.desc.measures[0].state_bytes, 8);
    }

    #[test]
    fn numeric_binding_rejects_type_and_row_count_mismatches() {
        let empty = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(NUMERICOID),
        };
        assert!(numeric_measure_column(&empty, 1).is_err());
        let wrong_type = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(INT4OID),
        };
        assert!(numeric_measure_column(&wrong_type, 0).is_err());
    }

    #[test]
    fn primitive_capability_checks_cover_supported_and_rejected_domains() {
        for oid in [TEXTOID, VARCHAROID, BPCHAROID] {
            assert!(is_text_family(oid));
            assert!(is_supported_group_type(oid));
        }
        for oid in [
            BOOLOID,
            INT2OID,
            INT4OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
        ] {
            assert!(!is_text_family(oid));
            assert!(is_supported_group_type(oid));
        }
        assert!(!is_supported_group_type(60_001));

        let range = |input| FilterSpec::Ranges {
            input,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(2),
            }],
        };
        assert!(validate_filter(&FilterSpec::None, 42, false, false).is_ok());
        assert!(validate_filter(&range(column(INT4OID)), 42, false, false).is_ok());
        assert!(validate_filter(&range(column(BOOLOID)), 42, true, false).is_ok());
        assert!(validate_filter(&range(column(INT2OID)), 42, true, false).is_err());
        assert!(validate_filter(&range(column(INT4OID)), 99, false, false).is_err());
        assert!(
            validate_filter(
                &FilterSpec::Mask {
                    input: column(BOOLOID),
                    kind: MaskKind::Sql
                },
                42,
                false,
                false,
            )
            .is_ok()
        );
        assert!(
            validate_filter(
                &FilterSpec::Mask {
                    input: column(INT4OID),
                    kind: MaskKind::Sql
                },
                42,
                false,
                false,
            )
            .is_err()
        );
        let spatial = spatial_count_shape().0.fact_filter;
        assert!(validate_filter(&spatial, 42, false, true).is_ok());
        assert!(validate_filter(&spatial, 42, false, false).is_err());

        let all_basic = [
            output(AggregateSource::Value, AggregateKind::Sum),
            output(AggregateSource::Value, AggregateKind::Count),
            output(AggregateSource::Value, AggregateKind::Min),
            output(AggregateSource::Value, AggregateKind::Max),
        ];
        assert!(validate_measure_outputs(&MeasureExpr::CountStar, &all_basic[1..2]).is_ok());
        assert!(validate_measure_outputs(&MeasureExpr::CountStar, &all_basic[..1]).is_err());
        assert!(
            validate_measure_outputs(&MeasureExpr::Column(column(INT4OID)), &all_basic).is_ok()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Column(column(INT4OID)),
                &[output(AggregateSource::Value, AggregateKind::Avg)],
            )
            .is_ok()
        );
        for oid in [
            INT2OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
        ] {
            assert!(
                validate_measure_outputs(
                    &MeasureExpr::Column(column(oid)),
                    &[all_basic[1], all_basic[2], all_basic[3]],
                )
                .is_ok()
            );
            if matches!(oid, INT2OID | INT8OID) {
                assert!(
                    validate_measure_outputs(
                        &MeasureExpr::Column(column(oid)),
                        &[
                            output(AggregateSource::Value, AggregateKind::Sum),
                            output(AggregateSource::Value, AggregateKind::Avg),
                        ],
                    )
                    .is_ok()
                );
            } else {
                assert!(
                    validate_measure_outputs(&MeasureExpr::Column(column(oid)), &all_basic[..1],)
                        .is_err()
                );
            }
        }
        assert!(
            validate_measure_outputs(&MeasureExpr::Column(column(BOOLOID)), &all_basic[1..2],)
                .is_ok()
        );
        assert!(
            validate_measure_outputs(&MeasureExpr::Column(column(BOOLOID)), &all_basic[2..4],)
                .is_err()
        );
        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        for op in [BinaryMeasureOp::Mul, BinaryMeasureOp::Sub] {
            assert!(
                validate_measure_outputs(
                    &MeasureExpr::Binary {
                        op,
                        lhs: column(INT4OID),
                        rhs
                    },
                    &all_basic,
                )
                .is_ok()
            );
        }
        let mut rhs64 = column(INT8OID);
        rhs64.attno = 2;
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: column(INT8OID),
                    rhs: rhs64
                },
                &[all_basic[1], all_basic[2], all_basic[3]],
            )
            .is_ok()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Binary {
                    op: BinaryMeasureOp::Mul,
                    lhs: column(INT8OID),
                    rhs: rhs64
                },
                &all_basic[1..2],
            )
            .is_err()
        );
        assert!(
            validate_measure_outputs(
                &MeasureExpr::Column(column(INT4OID)),
                &[output(AggregateSource::Rhs, AggregateKind::Count)],
            )
            .is_err()
        );
    }

    #[test]
    fn relation_and_dispatch_collection_cover_every_pure_source_shape() {
        let mut collection_spec = spec(MeasureExpr::CountStar, AggregateKind::Count);
        let mut fact_second = column(INT4OID);
        fact_second.attno = 2;
        let dim_key = ColumnRef {
            relation_oid: 43,
            attno: 1,
            type_oid: INT4OID,
        };
        collection_spec.group_keys = vec![
            GroupKeyRef {
                source: GroupKeySource::FactColumn(column(INT4OID)),
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::StarDimension {
                    dim_index: 0,
                    group_column: dim_key,
                },
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::Expression {
                    inputs: vec![fact_second],
                    program: vec![1],
                },
                type_oid: INT4OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
            GroupKeyRef {
                source: GroupKeySource::H3LatLngToCell {
                    latitude: ColumnRef {
                        relation_oid: 42,
                        attno: 3,
                        type_oid: FLOAT8OID,
                    },
                    longitude: ColumnRef {
                        relation_oid: 42,
                        attno: 4,
                        type_oid: FLOAT8OID,
                    },
                    resolution: 7,
                },
                type_oid: 90_001,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            },
        ];
        collection_spec.measures = vec![
            MeasureSpec {
                expression: MeasureExpr::StatsPair {
                    value: fact_second,
                    rhs: column(INT4OID),
                },
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::Mask {
                    input: column(BOOLOID),
                    kind: MaskKind::Sql,
                },
            },
            MeasureSpec {
                expression: MeasureExpr::Bytecode {
                    inputs: vec![ColumnRef {
                        relation_oid: 42,
                        attno: 5,
                        type_oid: INT4OID,
                    }],
                    program: vec![1],
                    result_type_oid: INT4OID,
                },
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::None,
            },
        ];
        collection_spec.fact_filter = FilterSpec::Bytecode {
            inputs: vec![ColumnRef {
                relation_oid: 42,
                attno: 6,
                type_oid: BOOLOID,
            }],
            program: vec![1],
        };
        collection_spec.star_dims.push(DimSpec {
            relation_oid: 43,
            fact_key: fact_second,
            dim_key,
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::Mask {
                input: ColumnRef {
                    relation_oid: 43,
                    attno: 2,
                    type_oid: BOOLOID,
                },
                kind: MaskKind::Sql,
            },
        });
        let selected = selected_relations(&collection_spec).expect("all column sources collect");
        assert_eq!(
            selected
                .iter()
                .map(|relation| u32::from(relation.relid))
                .collect::<Vec<_>>(),
            vec![42, 43]
        );
        assert!(selected[0].columns.len() >= 6);
        assert_eq!(
            dependency_relids(&collection_spec),
            vec![pg_sys::Oid::from(42_u32), pg_sys::Oid::from(43_u32)]
        );

        let mut dispatch_spec = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Count);
        dispatch_spec.fact_filter = FilterSpec::Ranges {
            input: fact_second,
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(0),
                hi: ScalarValue::I32(1),
            }],
        };
        let refs = dispatch_column_refs(&dispatch_spec).expect("measure and range inputs collect");
        assert_eq!(refs.len(), 2);
        dispatch_spec.measures[0].expression = MeasureExpr::Column(column(NUMERICOID));
        dispatch_spec.measures[0].filter = dispatch_spec.fact_filter.clone();
        dispatch_spec.fact_filter = FilterSpec::None;
        let refs =
            dispatch_column_refs(&dispatch_spec).expect("independent decimal predicate input");
        assert_eq!(
            refs.len(),
            2,
            "predicate-only raw inputs must be borrowed for dispatch"
        );
        assert!(
            refs.iter()
                .any(|request| i32::from(request.attno) == fact_second.attno)
        );
        let mut invalid = column(INT4OID);
        invalid.attno = i32::MAX;
        dispatch_spec.measures[0].expression = MeasureExpr::Column(invalid);
        assert!(dispatch_column_refs(&dispatch_spec).is_err());
    }

    #[test]
    fn zero_row_descriptor_builder_covers_all_host_only_shapes() {
        let global = spec(MeasureExpr::CountStar, AggregateKind::Count);
        let desc = zero_descriptor(&global, &[], &[]);
        assert_eq!(desc.row_count, 0);
        assert_eq!(desc.measure_count, 1);
        assert_eq!(
            desc.measures[0].op,
            abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
        );

        let mut scalar = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Sum);
        scalar.measures[0].outputs = vec![
            output(AggregateSource::Value, AggregateKind::Sum),
            output(AggregateSource::Value, AggregateKind::Count),
            output(AggregateSource::Value, AggregateKind::Min),
            output(AggregateSource::Value, AggregateKind::Max),
        ];
        let desc = zero_descriptor(&scalar, &[], &[INT4OID]);
        assert_eq!(desc.measures[0].op, abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN);
        assert_eq!(
            desc.measures[0].agg_mask,
            abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
        );

        let mut rhs = column(INT4OID);
        rhs.attno = 2;
        let binary = spec(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Sub,
                lhs: column(INT4OID),
                rhs,
            },
            AggregateKind::Count,
        );
        let desc = zero_descriptor(&binary, &[], &[INT4OID, INT4OID]);
        assert_eq!(desc.measures[0].op, abi::PGACCEL_GROUPED_AGG_MEASURE_SUB);

        let mut ranged = spec(MeasureExpr::Column(column(INT4OID)), AggregateKind::Count);
        ranged.fact_filter = FilterSpec::Ranges {
            input: column(INT4OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(-2),
                hi: ScalarValue::I32(9),
            }],
        };
        let desc = zero_descriptor(&ranged, &[], &[INT4OID]);
        assert_eq!(desc.where_filter.predicate_range_count, 1);
        assert_eq!(desc.where_filter.predicate_measure_slot, 0);

        let measure_filtered = dense_integer_measure_range_spec();
        let desc = zero_descriptor(&measure_filtered, &[INT4OID], &[INT4OID]);
        assert_eq!(desc.measure_filters[0].predicate_range_count, 1);
        assert_eq!(desc.measure_filters[0].predicate_measure_slot, 0);
        assert!(canonical_disabled_filter(&desc.measure_filters[1]));

        let mut masked = global.clone();
        masked.fact_filter = FilterSpec::Mask {
            input: column(BOOLOID),
            kind: MaskKind::Sql,
        };
        let desc = zero_descriptor(&masked, &[BOOLOID], &[]);
        assert_eq!(desc.where_filter.kind, abi::PGACCEL_GROUPED_AGG_FILTER_NONE);

        let mut grouped = global.clone();
        grouped.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let desc = zero_descriptor(&grouped, &[INT4OID], &[]);
        assert_eq!(desc.key_count, 1);
        assert_eq!(
            desc.keys[0].source,
            abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
        );
        assert_eq!(desc.keys[0].cardinality, 1);

        let fact_key = column(INT4OID);
        let dim_key = ColumnRef {
            relation_oid: 43,
            attno: 1,
            type_oid: INT4OID,
        };
        let mut dimension = global;
        dimension.star_dims.push(DimSpec {
            relation_oid: 43,
            fact_key,
            dim_key,
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::None,
        });
        dimension.group_keys.push(GroupKeyRef {
            source: GroupKeySource::StarDimension {
                dim_index: 0,
                group_column: dim_key,
            },
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let desc = zero_descriptor(&dimension, &[INT4OID, INT4OID], &[]);
        assert_eq!(desc.dim_count, 1);
        assert_eq!(
            desc.keys[0].source,
            abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0
        );
        assert_eq!(desc.dims[0].key_count, 0);
    }

    #[test]
    fn staged_h3_integer_sum_count_binds_cached_and_fused_without_sql_admission() {
        for value_type in [INT2OID, INT4OID] {
            for filtered in [false, true] {
                for reverse in [false, true] {
                    let (mut spec, mut projection) = h3_count_shape();
                    let GroupKeySource::H3CellToParent { cell, resolution } =
                        spec.group_keys[0].source
                    else {
                        unreachable!()
                    };
                    let value = ColumnRef {
                        attno: 2,
                        type_oid: value_type,
                        ..cell
                    };
                    spec.measures[0].expression = MeasureExpr::Column(value);
                    spec.measures[0]
                        .outputs
                        .push(crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Sum,
                        });
                    if reverse {
                        spec.measures[0].outputs.reverse();
                    }
                    if filtered {
                        spec.measures[0].filter = FilterSpec::Mask {
                            input: ColumnRef {
                                attno: 3,
                                type_oid: BOOLOID,
                                ..cell
                            },
                            kind: MaskKind::Sql,
                        };
                    }
                    projection.slots[1].source_type_oid = value_type;
                    let mut sum_slot = projection.slots[1];
                    sum_slot.source = AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    };
                    sum_slot.nullable = true;
                    projection.slots.push(sum_slot);
                    spec.validate().expect("logical SUM/COUNT");
                    projection
                        .validate(&spec)
                        .expect("canonical SUM/COUNT projection");
                    assert!(validate_internal_runtime_capability(&spec, &projection).is_err());
                    assert!(validate_normal_descriptor_capability(&spec, &projection).is_err());
                    assert_eq!(
                        HashMeasureInput::from_spec(&spec),
                        Ok(HashMeasureInput::IntegerStates(value))
                    );
                    assert_eq!(
                        classify_descriptor_specialization(
                            &spec,
                            GroupedAggKernelMode::ParallelHash
                        )
                        .family,
                        DescriptorSpecializationFamily::HashInt4SumCount
                    );
                    let requests = dispatch_column_refs(&spec).expect("complete dependencies");
                    assert_eq!(
                        requests
                            .iter()
                            .map(|request| request.attno)
                            .collect::<Vec<_>>(),
                        if filtered { vec![1, 2, 3] } else { vec![1, 2] }
                    );
                    let mut views = vec![
                        ResidentColumnView::Empty {
                            type_oid: cell.type_oid.into(),
                        },
                        ResidentColumnView::Empty {
                            type_oid: value_type.into(),
                        },
                    ];
                    if filtered {
                        views.push(ResidentColumnView::Empty {
                            type_oid: BOOLOID.into(),
                        });
                    }
                    let cached = H3ParentArtifact::new(spec.clone(), 0, 1, None, None)
                        .expect("empty artifact");
                    let fused = FusedHashArtifact {
                        resolved_spec: spec.clone(),
                        fact_rows: 0,
                        group_capacity: Cell::new(1),
                        resolution,
                    };
                    for bound in [
                        build_h3_descriptor(&cached, &requests, &views, cell)
                            .expect("cached SUM binding"),
                        build_fused_hash_descriptor(&fused, &requests, &views, cell, &mut || false)
                            .expect("fused SUM binding"),
                    ] {
                        let measure = bound.desc.measures[0];
                        assert_eq!(measure.op, abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN);
                        assert_eq!(
                            measure.agg_mask,
                            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                        );
                        assert_eq!(
                            measure.value.physical_type,
                            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                        );
                        assert_eq!(measure.value.element_bytes, 4);
                        assert_eq!(measure.accumulator_kind, abi::PGACCEL_GROUPED_AGG_ACCUM_I64);
                        assert_eq!(measure.state_bytes, 8);
                        // SAFETY: zero rows, canonical inactive slots and no device inputs.
                        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }
                            .expect("valid wire descriptor");
                        assert_eq!(
                            plan.one_shot_kernel_mode().expect("native hash shape"),
                            GroupedAggKernelMode::ParallelHash
                        );
                    }
                    views[1] = ResidentColumnView::Empty {
                        type_oid: INT8OID.into(),
                    };
                    assert!(build_h3_descriptor(&cached, &requests, &views, cell).is_err());
                    assert!(
                        build_fused_hash_descriptor(&fused, &requests, &views, cell, &mut || false)
                            .is_err()
                    );
                    for type_oid in [BOOLOID, INT8OID, NUMERICOID, FLOAT4OID, DATEOID] {
                        let mut wrong = spec.clone();
                        wrong.measures[0].expression =
                            MeasureExpr::Column(ColumnRef { type_oid, ..value });
                        assert!(HashMeasureInput::from_spec(&wrong).is_err());
                    }
                    let mut wrong = spec.clone();
                    wrong.measures[0].outputs[0] = wrong.measures[0].outputs[1];
                    assert!(HashMeasureInput::from_spec(&wrong).is_err());
                    let mut wrong = spec;
                    wrong.measures[0]
                        .outputs
                        .push(crate::engine::spec::AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::StddevSamp,
                        });
                    assert!(HashMeasureInput::from_spec(&wrong).is_err());
                }
            }
        }
    }

    #[test]
    fn h3_integer_aggregate_subsets_keep_internal_count_out_of_projection() {
        use crate::engine::spec::AggregateOutput;
        let kinds = [
            AggregateKind::Sum,
            AggregateKind::Avg,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ];
        for value_type in [INT2OID, INT4OID] {
            for mask in 1..32 {
                let outputs = kinds
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, kind)| AggregateOutput {
                        source: AggregateSource::Value,
                        kind: *kind,
                    })
                    .collect::<Vec<_>>();
                for rotation in 0..outputs.len() {
                    for reverse in [false, true] {
                        for filtered in [false, true] {
                            let (mut spec, mut projection) = h3_count_shape();
                            let GroupKeySource::H3CellToParent { cell, resolution } =
                                spec.group_keys[0].source
                            else {
                                unreachable!()
                            };
                            let column = ColumnRef {
                                attno: 2,
                                type_oid: value_type,
                                ..cell
                            };
                            spec.measures[0].expression = MeasureExpr::Column(column);
                            spec.measures[0].outputs = outputs.clone();
                            spec.measures[0].outputs.rotate_left(rotation);
                            if reverse {
                                spec.measures[0].outputs.reverse();
                            }
                            if filtered {
                                spec.measures[0].filter = FilterSpec::Mask {
                                    input: ColumnRef {
                                        attno: 3,
                                        type_oid: BOOLOID,
                                        ..cell
                                    },
                                    kind: MaskKind::Sql,
                                };
                            }
                            let template = projection.slots[1];
                            projection.slots.truncate(1);
                            for output in &spec.measures[0].outputs {
                                projection.slots.push(crate::engine::spec::AggOutputSlot {
                                    source: AggOutputSource::Aggregate {
                                        measure_index: 0,
                                        source: output.source,
                                        kind: output.kind,
                                    },
                                    source_type_oid: value_type,
                                    result_type_oid: if output.kind == AggregateKind::Avg {
                                        NUMERICOID
                                    } else if matches!(
                                        output.kind,
                                        AggregateKind::Min | AggregateKind::Max
                                    ) {
                                        value_type
                                    } else {
                                        INT8OID
                                    },
                                    nullable: output.kind != AggregateKind::Count,
                                    ..template
                                });
                            }
                            spec.validate().expect("logical aggregate subset");
                            projection
                                .validate(&spec)
                                .expect("no extra COUNT projection needed");
                            assert_eq!(projection.slots.len(), 1 + outputs.len());
                            assert!(
                                validate_internal_runtime_capability(&spec, &projection).is_err()
                            );
                            assert!(
                                validate_normal_descriptor_capability(&spec, &projection).is_err()
                            );
                            assert_eq!(
                                planned_descriptor_kernel_mode(&spec, true),
                                GroupedAggKernelMode::SerialGeneric
                            );
                            let requests =
                                dispatch_column_refs(&spec).expect("all resident dependencies");
                            let mut views = vec![
                                ResidentColumnView::Empty {
                                    type_oid: cell.type_oid.into(),
                                },
                                ResidentColumnView::Empty {
                                    type_oid: value_type.into(),
                                },
                            ];
                            if filtered {
                                views.push(ResidentColumnView::Empty {
                                    type_oid: BOOLOID.into(),
                                });
                            }
                            let cached = H3ParentArtifact::new(spec.clone(), 0, 1, None, None)
                                .expect("empty artifact");
                            let fused = FusedHashArtifact {
                                resolved_spec: spec.clone(),
                                fact_rows: 0,
                                group_capacity: Cell::new(1),
                                resolution,
                            };
                            for bound in [
                                build_h3_descriptor(&cached, &requests, &views, cell)
                                    .expect("cached subset"),
                                build_fused_hash_descriptor(
                                    &fused,
                                    &requests,
                                    &views,
                                    cell,
                                    &mut || false,
                                )
                                .expect("fused subset"),
                            ] {
                                let expected = abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                                    | if mask & 3 != 0 {
                                        abi::PGACCEL_GROUPED_AGG_LANE_SUM
                                    } else {
                                        0
                                    }
                                    | if mask & 8 != 0 {
                                        abi::PGACCEL_GROUPED_AGG_LANE_MIN
                                    } else {
                                        0
                                    }
                                    | if mask & 16 != 0 {
                                        abi::PGACCEL_GROUPED_AGG_LANE_MAX
                                    } else {
                                        0
                                    };
                                assert_eq!(bound.desc.measures[0].agg_mask, expected);
                                // SAFETY: canonical zero-row descriptor, no readable input payloads.
                                let plan = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }
                                    .expect("physical aggregate states");
                                assert_eq!(
                                    plan.one_shot_kernel_mode().expect("native mode"),
                                    GroupedAggKernelMode::ParallelHash
                                );
                            }
                            let mut duplicate = spec.clone();
                            let first = duplicate.measures[0].outputs[0];
                            duplicate.measures[0].outputs.push(first);
                            assert!(HashMeasureInput::from_spec(&duplicate).is_err());
                            let mut wrong = spec.clone();
                            wrong.measures[0].outputs[0].kind = AggregateKind::StddevSamp;
                            assert!(HashMeasureInput::from_spec(&wrong).is_err());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hash_sum_output_validity_and_extreme_bounds() {
        let bytes = |sums: &[i64]| {
            sums.iter()
                .flat_map(|sum| sum.to_ne_bytes())
                .collect::<Vec<_>>()
        };
        for (type_oid, minimum, maximum) in [
            (INT2OID, i64::from(i16::MIN), i64::from(i16::MAX)),
            (INT4OID, i64::from(i32::MIN), i64::from(i32::MAX)),
        ] {
            for count in [0_u64, 1, 2, 1_000_017, u64::from(u32::MAX)] {
                for sum in [minimum * count as i64, 0, maximum * count as i64] {
                    validate_h3_sum_states(&bytes(&[sum, 0]), &[count, 0], &[count, 0], type_oid)
                        .expect("valid exact range");
                }
                for sum in [minimum * count as i64 - 1, maximum * count as i64 + 1] {
                    assert!(
                        validate_h3_sum_states(&bytes(&[sum]), &[count], &[count], type_oid)
                            .is_err()
                    );
                }
            }
            validate_h3_sum_states(&[], &[], &[], type_oid).expect("empty lanes");
            assert!(validate_h3_sum_states(&[0; 7], &[0], &[0], type_oid).is_err());
            assert!(validate_h3_sum_states(&[0; 16], &[0], &[0], type_oid).is_err());
            assert!(validate_h3_sum_states(&[0; 8], &[], &[0], type_oid).is_err());
            assert!(validate_h3_sum_states(&[0; 8], &[1], &[0], type_oid).is_err());
            assert!(validate_h3_sum_states(&[0; 8], &[u64::MAX], &[u64::MAX], type_oid).is_err());
            assert!(
                validate_h3_sum_states(
                    &[0; 8],
                    &[u64::from(u32::MAX) + 1],
                    &[u64::from(u32::MAX) + 1],
                    type_oid
                )
                .is_err()
            );
            assert!(validate_h3_sum_states(&bytes(&[1]), &[0], &[0], type_oid).is_err());
        }
        assert!(validate_h3_sum_states(&[], &[], &[], INT8OID).is_err());
        assert!(validate_h3_sum_states(&bytes(&[32768]), &[1], &[1], INT2OID).is_err());
        validate_h3_sum_states(&bytes(&[32768]), &[1], &[1], INT4OID)
            .expect("valid INT4, invalid INT2");
    }

    #[test]
    fn hash_extrema_validate_optional_lanes_nulls_and_joint_states() {
        for (type_oid, low, high) in [
            (INT2OID, i64::from(i16::MIN), i64::from(i16::MAX)),
            (INT4OID, i64::from(i32::MIN), i64::from(i32::MAX)),
        ] {
            for count in [0_u64, 1, 2, 1_000_017, u64::from(u32::MAX)] {
                let values = match count {
                    0 => [0, 0, 0],
                    1 => [low, low, low],
                    _ => [low * (count as i64 - 1) + high, low, high],
                };
                let bytes = values.map(i64::to_ne_bytes);
                for mask in 1..8 {
                    let lanes = std::array::from_fn(|index| {
                        (mask & (1 << index) != 0).then_some(bytes[index].as_slice())
                    });
                    validate_hash_integer_states(lanes, &[count], &[count], type_oid)
                        .expect("exact optional states");
                }
            }
            let check = |sum: Option<i64>, low: Option<i64>, high: Option<i64>, count: u64| {
                let values = [sum, low, high].map(|value| value.map(i64::to_ne_bytes));
                let lanes = values
                    .each_ref()
                    .map(|value| value.as_ref().map(<[u8; 8]>::as_slice));
                validate_hash_integer_states(lanes, &[count], &[count], type_oid)
            };
            assert!(check(None, Some(low - 1), None, 2).is_err());
            assert!(check(None, None, Some(high + 1), 2).is_err());
            assert!(check(None, Some(1), None, 0).is_err());
            assert!(check(None, None, Some(-1), 0).is_err());
            assert!(check(None, Some(7), Some(6), 2).is_err());
            assert!(check(Some(3), Some(2), None, 2).is_err());
            assert!(check(Some(5), None, Some(2), 2).is_err());
            assert!(check(None, Some(2), Some(3), 1).is_err());
            assert!(check(Some(3), Some(2), None, 1).is_err());
            assert!(check(Some(2), None, Some(3), 1).is_err());
            assert!(check(None, Some(0), None, u64::from(u32::MAX) + 1).is_err());
        }
        assert!(validate_hash_integer_states([None; 3], &[], &[], INT4OID).is_err());
        assert!(
            validate_hash_integer_states([None, Some(&[0; 7]), None], &[0], &[0], INT4OID).is_err()
        );
        assert!(
            validate_hash_integer_states([None, None, Some(&[0; 16])], &[0], &[0], INT4OID)
                .is_err()
        );
        assert!(
            validate_hash_integer_states([None, Some(&[0; 8]), None], &[1], &[0], INT4OID).is_err()
        );
        assert!(validate_hash_integer_states([None, Some(&[]), None], &[], &[], INT8OID).is_err());
    }

    #[test]
    fn hash_integer_full_projection_permutations_preserve_minimal_lanes() {
        use crate::engine::spec::AggregateOutput;
        let kinds = [
            AggregateKind::Sum,
            AggregateKind::Avg,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ];
        let mut permutations = 0;
        for a in 0..5 {
            for b in 0..5 {
                for c in 0..5 {
                    for d in 0..5 {
                        for e in 0..5 {
                            let indices = [a, b, c, d, e];
                            if indices
                                .iter()
                                .enumerate()
                                .any(|(index, value)| indices[..index].contains(value))
                            {
                                continue;
                            }
                            let (mut spec, _) = h3_count_shape();
                            let value = ColumnRef {
                                relation_oid: spec.fact_rel,
                                attno: 2,
                                type_oid: INT2OID,
                            };
                            spec.measures[0].expression = MeasureExpr::Column(value);
                            spec.measures[0].outputs = indices
                                .map(|index| AggregateOutput {
                                    source: AggregateSource::Value,
                                    kind: kinds[index],
                                })
                                .to_vec();
                            assert_eq!(
                                HashMeasureInput::from_spec(&spec),
                                Ok(HashMeasureInput::IntegerStates(value))
                            );
                            assert_eq!(
                                hash_integer_lanes(&spec.measures[0]),
                                abi::PGACCEL_GROUPED_AGG_LANE_SUM
                                    | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                                    | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                                    | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                            );
                            let specialization = classify_descriptor_specialization(
                                &spec,
                                GroupedAggKernelMode::ParallelHash,
                            );
                            assert_eq!(
                                specialization.family,
                                DescriptorSpecializationFamily::HashInt4Extrema
                            );
                            assert_eq!(specialization.label(), "hash_int4_extrema");
                            permutations += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(permutations, 120);
    }

    #[test]
    fn hash_column_counts_preserve_zero_groups_without_weakening_star_checks() {
        let input = HashMeasureInput::Column(column(INT4OID));
        for rows in 0_usize..=8 {
            for null_mask in 0..(1_u32 << rows) {
                let mut counts = [0_u64; 3];
                for row in 0..rows {
                    if null_mask & (1 << row) == 0 {
                        counts[row % 3] += 1;
                    }
                }
                let groups = rows.min(3);
                validate_h3_count_partition(&counts, 3, groups, rows as u64, input)
                    .expect("nullable counts retain every input group");
                assert_eq!(
                    validate_h3_count_partition(
                        &counts,
                        3,
                        groups,
                        rows as u64,
                        HashMeasureInput::Star
                    )
                    .is_ok(),
                    null_mask == 0,
                    "COUNT(*) may not borrow nullable-count validation"
                );
            }
        }
        for (counts, emitted, selected) in [
            ([0, 0, 0], 0, 1),               // missing all input groups
            ([0, 0, 0], 2, 1),               // more groups than rows
            ([0, 0, 0], 1, 0),               // ghost group on empty input
            ([2, 2, 0], 2, 3),               // counts exceed rows
            ([0, 0, 1], 2, 3),               // nonzero unused tail
            ([u64::MAX, 1, 0], 2, u64::MAX), // addition overflow
        ] {
            assert!(validate_h3_count_partition(&counts, 3, emitted, selected, input).is_err());
        }
    }

    #[test]
    fn h3_input_spans_require_exact_value_and_null_domains() {
        for rows in [0, 1, 8192, 1_000_000] {
            assert!(validate_h3_input_lengths(rows, None, rows).is_ok());
            assert!(validate_h3_input_lengths(rows, Some(rows), rows).is_ok());
            assert!(validate_h3_input_lengths(rows + 1, None, rows).is_err());
            assert!(validate_h3_input_lengths(rows, Some(rows + 1), rows).is_err());
            if rows != 0 {
                assert!(validate_h3_input_lengths(rows - 1, None, rows).is_err());
                assert!(validate_h3_input_lengths(rows, Some(rows - 1), rows).is_err());
            }
        }
    }

    #[test]
    fn staged_h3_column_count_binds_all_fixed_width_types_and_dependencies() {
        for type_oid in [
            BOOLOID,
            INT2OID,
            INT4OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
        ] {
            let (mut spec, _) = h3_count_shape();
            let GroupKeySource::H3CellToParent { cell, resolution } = spec.group_keys[0].source
            else {
                unreachable!()
            };
            let input = ColumnRef {
                attno: 2,
                type_oid,
                ..cell
            };
            spec.measures[0].expression = MeasureExpr::Column(input);
            assert_eq!(
                HashMeasureInput::from_spec(&spec),
                Ok(HashMeasureInput::Column(input))
            );
            let requests = [
                ResidentColumnRef {
                    relid: pg_sys::Oid::from(cell.relation_oid),
                    attno: 1,
                },
                ResidentColumnRef {
                    relid: pg_sys::Oid::from(input.relation_oid),
                    attno: 2,
                },
            ];
            assert_eq!(
                dispatch_column_refs(&spec).expect("dispatch dependencies"),
                requests
            );
            assert_eq!(
                selected_relations(&spec).expect("resident inputs")[0].columns,
                [1, 2]
            );
            let views = [
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(cell.type_oid),
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(type_oid),
                },
            ];
            let cached =
                H3ParentArtifact::new(spec.clone(), 0, 1, None, None).expect("empty artifact");
            let fused = FusedHashArtifact {
                resolved_spec: spec,
                fact_rows: 0,
                group_capacity: Cell::new(1),
                resolution,
            };
            for desc in [
                build_h3_descriptor(&cached, &requests, &views, cell).expect("cached binding"),
                build_fused_hash_descriptor(&fused, &requests, &views, cell, &mut || false)
                    .expect("fused binding"),
            ] {
                let desc = &desc.desc;
                assert_eq!(desc.measures[0].op, abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN);
                assert_eq!(
                    desc.measures[0].agg_mask,
                    abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                );
                let expected = measure_column(&views[1], 0).expect("physical mapping");
                assert_eq!(desc.measures[0].value.physical_type, expected.physical_type);
                assert_eq!(desc.measures[0].value.element_bytes, expected.element_bytes);
                assert!(desc.measures[0].value.values.is_null());
                assert!(desc.measures[0].value.nulls.is_null());
            }
            assert!(build_h3_descriptor(&cached, &requests[..1], &views[..1], cell).is_err());
            assert!(
                build_fused_hash_descriptor(&fused, &requests, &views[..1], cell, &mut || false)
                    .is_err()
            );
            let duplicated = [requests[0], requests[0]];
            assert!(build_h3_descriptor(&cached, &duplicated, &views, cell).is_err());
            let wrong_views = [
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(cell.type_oid),
                },
                ResidentColumnView::Empty {
                    type_oid: pg_sys::Oid::from(TEXTOID),
                },
            ];
            assert!(
                build_fused_hash_descriptor(&fused, &requests, &wrong_views, cell, &mut || false)
                    .is_err()
            );
        }
    }

    #[test]
    fn staged_hash_count_binding_rejects_other_measure_semantics() {
        let (original, _) = h3_count_shape();
        assert_eq!(
            HashMeasureInput::from_spec(&original),
            Ok(HashMeasureInput::Star)
        );
        for type_oid in [TEXTOID, NUMERICOID, 0] {
            let mut wrong = original.clone();
            wrong.measures[0].expression = MeasureExpr::Column(ColumnRef {
                attno: 2,
                ..column(type_oid)
            });
            assert!(HashMeasureInput::from_spec(&wrong).is_err());
        }
        for input in [
            ColumnRef {
                relation_oid: original.fact_rel + 1,
                ..column(INT4OID)
            },
            ColumnRef {
                attno: 0,
                ..column(INT4OID)
            },
        ] {
            let mut wrong = original.clone();
            wrong.measures[0].expression = MeasureExpr::Column(input);
            assert!(HashMeasureInput::from_spec(&wrong).is_err());
        }
        let mut wrong = original.clone();
        wrong.measures[0].outputs[0].kind = AggregateKind::Sum;
        assert!(HashMeasureInput::from_spec(&wrong).is_err());
        let mut wrong = original.clone();
        wrong.measures.push(wrong.measures[0].clone());
        assert!(HashMeasureInput::from_spec(&wrong).is_err());
        let mut wrong = original;
        wrong.measures[0].filter = FilterSpec::Mask {
            input: column(BOOLOID),
            kind: MaskKind::Recheck,
        };
        assert!(HashMeasureInput::from_spec(&wrong).is_err());
        wrong.fact_filter = wrong.measures[0].filter.clone();
        wrong.measures[0].filter = FilterSpec::None;
        assert!(HashMeasureInput::from_spec(&wrong).is_err());
    }

    #[test]
    fn filtered_h3_counts_keep_star_partition_checks_separate() {
        for counts in [[0, 0, 0], [0, 2, 0], [1, 1, 0]] {
            validate_h3_count_partition(&counts, 3, 2, 5, HashMeasureInput::FilteredStar)
                .expect("FILTER permits zero groups and rejected rows");
            assert!(validate_h3_count_partition(&counts, 3, 2, 5, HashMeasureInput::Star).is_err());
        }
        for (counts, emitted, rows) in [
            ([1, 2, 0], 2, 2),
            ([0, 0, 1], 2, 5),
            ([u64::MAX, 1, 0], 2, u64::MAX),
            ([0, 0, 0], 0, 5),
            ([0, 0, 0], 2, 1),
        ] {
            assert!(
                validate_h3_count_partition(
                    &counts,
                    3,
                    emitted,
                    rows,
                    HashMeasureInput::FilteredStar
                )
                .is_err()
            );
        }
        validate_h3_count_partition(&[0], 1, 0, 0, HashMeasureInput::FilteredStar)
            .expect("empty FILTER");
    }

    #[test]
    fn h3_mask_policy_accounts_for_bounded_conversion_work() {
        let (mut spec, _) = h3_count_shape();
        for rows in [0, 1, 65535, 65536, 65537, 1_000_017, 12_748_986] {
            assert_eq!(h3_mask_work(&spec, rows), Ok((0, 0)));
        }
        spec.measures[0].filter = FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: spec.fact_rel,
                attno: 3,
                type_oid: BOOLOID,
            },
            kind: MaskKind::Sql,
        };
        for (rows, launches) in [
            (0, 0),
            (1, 1),
            (65535, 1),
            (65536, 1),
            (65537, 2),
            (1_000_017, 16),
            (12_748_986, 195),
        ] {
            assert_eq!(h3_mask_work(&spec, rows), Ok((rows as u64, launches)));
        }
        spec.measures[0].filter = FilterSpec::Mask {
            input: column(BOOLOID),
            kind: MaskKind::Recheck,
        };
        assert!(h3_mask_work(&spec, 1).is_err());
    }

    #[test]
    fn filtered_h3_binding_keeps_predicate_dependencies_and_empty_ownership() {
        let (mut spec, projection) = h3_count_shape();
        let GroupKeySource::H3CellToParent { cell, resolution } = spec.group_keys[0].source else {
            unreachable!()
        };
        let predicate = ColumnRef {
            attno: 3,
            type_oid: BOOLOID,
            ..cell
        };
        spec.measures[0].filter = FilterSpec::Mask {
            input: predicate,
            kind: MaskKind::Sql,
        };
        assert_eq!(
            HashMeasureInput::from_spec(&spec),
            Ok(HashMeasureInput::FilteredStar)
        );
        assert!(validate_normal_descriptor_capability(&spec, &projection).is_err());
        assert!(validate_internal_runtime_capability(&spec, &projection).is_err());
        let requests = dispatch_column_refs(&spec).expect("predicate request");
        assert_eq!(requests.iter().map(|r| r.attno).collect::<Vec<_>>(), [1, 3]);
        let selected = selected_relations(&spec).expect("resident predicate dependency");
        assert_eq!(selected[0].columns, [1, 3]);
        let views = [
            ResidentColumnView::Empty {
                type_oid: cell.type_oid.into(),
            },
            ResidentColumnView::Empty {
                type_oid: BOOLOID.into(),
            },
        ];
        let cached = H3ParentArtifact::new(spec.clone(), 0, 1, None, None).expect("empty artifact");
        let fused = FusedHashArtifact {
            resolved_spec: spec.clone(),
            fact_rows: 0,
            group_capacity: Cell::new(1),
            resolution,
        };
        for bound in [
            build_h3_descriptor(&cached, &requests, &views, cell).expect("cached empty filter"),
            build_fused_hash_descriptor(&fused, &requests, &views, cell, &mut || false)
                .expect("fused empty filter"),
        ] {
            assert!(bound._filter.is_none());
            assert!(canonical_disabled_filter(&bound.desc.measure_filters[0]));
        }
        assert!(build_h3_descriptor(&cached, &requests[..1], &views[..1], cell).is_err());
        let wrong_views = [
            ResidentColumnView::Empty {
                type_oid: cell.type_oid.into(),
            },
            ResidentColumnView::Empty {
                type_oid: INT4OID.into(),
            },
        ];
        assert!(
            build_fused_hash_descriptor(&fused, &requests, &wrong_views, cell, &mut || false)
                .is_err()
        );
        for input in [
            ColumnRef {
                relation_oid: cell.relation_oid + 1,
                ..predicate
            },
            ColumnRef {
                attno: 0,
                ..predicate
            },
            ColumnRef {
                type_oid: INT4OID,
                ..predicate
            },
        ] {
            spec.measures[0].filter = FilterSpec::Mask {
                input,
                kind: MaskKind::Sql,
            };
            assert!(HashMeasureInput::from_spec(&spec).is_err());
        }
    }

    #[test]
    fn raw_integer_hash_binding_preserves_inputs_and_native_shape() {
        for key_type in [INT2OID, INT4OID, INT8OID] {
            for kind in [
                AggregateKind::Count,
                AggregateKind::Sum,
                AggregateKind::Avg,
                AggregateKind::Min,
                AggregateKind::Max,
            ] {
                for filtered in [false, true] {
                    let (mut spec, _) = h3_count_shape();
                    let key = column(key_type);
                    spec.group_keys[0].source = GroupKeySource::FactColumn(key);
                    spec.group_keys[0].type_oid = key_type;
                    spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
                        attno: 2,
                        ..column(INT4OID)
                    });
                    spec.measures[0].outputs[0].kind = kind;
                    if filtered {
                        spec.measures[0].filter = FilterSpec::Mask {
                            input: ColumnRef {
                                attno: 3,
                                ..column(BOOLOID)
                            },
                            kind: MaskKind::Sql,
                        };
                    }
                    assert_eq!(raw_hash_group(&spec), Some(key));
                    assert_ne!(
                        planned_descriptor_kernel_mode(&spec, false),
                        GroupedAggKernelMode::ParallelHash
                    );
                    let requests =
                        compact_hash_column_refs(&spec).expect("raw key and measure dependencies");
                    let mut views = vec![
                        ResidentColumnView::Empty {
                            type_oid: key_type.into(),
                        },
                        ResidentColumnView::Empty {
                            type_oid: INT4OID.into(),
                        },
                    ];
                    if filtered {
                        views.push(ResidentColumnView::Empty {
                            type_oid: BOOLOID.into(),
                        });
                    }
                    let mut artifact = FusedHashArtifact {
                        resolved_spec: spec,
                        fact_rows: 0,
                        group_capacity: Cell::new(1),
                        resolution: 0,
                    };
                    let bound =
                        build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || {
                            false
                        })
                        .expect("raw integer binding");
                    assert_eq!(bound.desc.keys[0].flags, 0);
                    assert_eq!(bound.desc.keys[0].pad0, 0);
                    assert_eq!(
                        bound.desc.keys[0].values.tag,
                        if key_type == INT8OID {
                            PgaccelValTag::Int64
                        } else {
                            PgaccelValTag::Int32
                        }
                    );
                    assert_eq!(
                        bound.desc.measures[0].agg_mask,
                        abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                            | match kind {
                                AggregateKind::Count => 0,
                                AggregateKind::Sum | AggregateKind::Avg =>
                                    abi::PGACCEL_GROUPED_AGG_LANE_SUM,
                                AggregateKind::Min => abi::PGACCEL_GROUPED_AGG_LANE_MIN,
                                AggregateKind::Max => abi::PGACCEL_GROUPED_AGG_LANE_MAX,
                                AggregateKind::StddevSamp => unreachable!(),
                            }
                    );
                    // SAFETY: canonical zero-row descriptor, no readable input payloads.
                    let native = unsafe { ResolvedGroupedAggPlan::from_abi(bound.desc) }
                        .expect("raw hash ABI");
                    assert_eq!(
                        native.one_shot_kernel_mode().expect("native shape"),
                        GroupedAggKernelMode::ParallelHash
                    );
                    assert!(matches!(
                        build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || {
                            true
                        }),
                        Err(ResidentLoadError::PreparationInterrupted)
                    ));
                    artifact.resolution = 1;
                    assert!(
                        build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || {
                            false
                        })
                        .is_err()
                    );
                    artifact.resolution = 0;
                    views[0] = ResidentColumnView::Empty {
                        type_oid: BOOLOID.into(),
                    };
                    assert!(
                        build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || {
                            false
                        })
                        .is_err()
                    );
                    for type_oid in [BOOLOID, TIMESTAMPOID, NUMERICOID] {
                        let mut wrong = artifact.resolved_spec.clone();
                        wrong.group_keys[0].type_oid = type_oid;
                        assert!(raw_hash_group(&wrong).is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn hash_where_binding_tracks_predicates_and_keeps_sql_admission_closed() {
        for key_type in [INT2OID, INT4OID, INT8OID] {
            for filtered in [false, true] {
                let (mut spec, projection) = h3_count_shape();
                let key = column(key_type);
                spec.group_keys[0].source = GroupKeySource::FactColumn(key);
                spec.group_keys[0].type_oid = key_type;
                let predicate = ColumnRef {
                    attno: 4,
                    ..column(BOOLOID)
                };
                spec.fact_filter = FilterSpec::Mask {
                    input: predicate,
                    kind: MaskKind::Sql,
                };
                if filtered {
                    spec.measures[0].filter = FilterSpec::Mask {
                        input: ColumnRef {
                            attno: 3,
                            ..predicate
                        },
                        kind: MaskKind::Sql,
                    };
                }
                assert!(HashMeasureInput::from_spec(&spec).is_ok());
                assert_ne!(
                    planned_descriptor_kernel_mode(&spec, false),
                    GroupedAggKernelMode::ParallelHash
                );
                assert!(validate_normal_descriptor_capability(&spec, &projection).is_err());
                let requests = compact_hash_column_refs(&spec).expect("key and both predicates");
                let attributes: Vec<_> = requests.iter().map(|request| request.attno).collect();
                assert_eq!(
                    attributes,
                    if filtered { vec![1, 3, 4] } else { vec![1, 4] }
                );
                assert_eq!(
                    selected_relations(&spec).expect("predicate dependency")[0].columns,
                    attributes
                );
                assert!(
                    artifact_column_refs(&spec)
                        .expect("WHERE snapshot identity")
                        .iter()
                        .any(|request| request.attno == 4)
                );
                let mut views = vec![ResidentColumnView::Empty {
                    type_oid: key_type.into(),
                }];
                views.extend((1..requests.len()).map(|_| ResidentColumnView::Empty {
                    type_oid: BOOLOID.into(),
                }));
                let artifact = FusedHashArtifact {
                    resolved_spec: spec.clone(),
                    fact_rows: 0,
                    group_capacity: Cell::new(1),
                    resolution: 0,
                };
                let bound =
                    build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || false)
                        .expect("empty WHERE binding requires no device allocation");
                assert!(bound._where.is_none() && bound._filter.is_none());
                assert!(canonical_disabled_filter(&bound.desc.where_filter));
                assert!(
                    build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || true)
                        .is_err()
                );
                let last = views.len() - 1;
                views[last] = ResidentColumnView::Empty {
                    type_oid: INT4OID.into(),
                };
                assert!(
                    build_fused_hash_descriptor(&artifact, &requests, &views, key, &mut || false)
                        .is_err()
                );
                for bad in [
                    FilterSpec::Mask {
                        input: predicate,
                        kind: MaskKind::Recheck,
                    },
                    FilterSpec::Mask {
                        input: ColumnRef {
                            type_oid: INT4OID,
                            ..predicate
                        },
                        kind: MaskKind::Sql,
                    },
                    FilterSpec::Mask {
                        input: ColumnRef {
                            relation_oid: spec.fact_rel + 1,
                            ..predicate
                        },
                        kind: MaskKind::Sql,
                    },
                    FilterSpec::Mask {
                        input: ColumnRef {
                            attno: 0,
                            ..predicate
                        },
                        kind: MaskKind::Sql,
                    },
                ] {
                    spec.fact_filter = bad;
                    assert!(HashMeasureInput::from_spec(&spec).is_err());
                }
            }
        }
        let (mut h3, _) = h3_count_shape();
        h3.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 4,
                ..column(BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        let GroupKeySource::H3CellToParent { cell, resolution } = h3.group_keys[0].source else {
            unreachable!()
        };
        let requests = compact_hash_column_refs(&h3).expect("H3 WHERE dependencies");
        let views = [
            ResidentColumnView::Empty {
                type_oid: cell.type_oid.into(),
            },
            ResidentColumnView::Empty {
                type_oid: BOOLOID.into(),
            },
        ];
        let fused = FusedHashArtifact {
            resolved_spec: h3.clone(),
            fact_rows: 0,
            group_capacity: Cell::new(1),
            resolution,
        };
        let bound = build_fused_hash_descriptor(&fused, &requests, &views, cell, &mut || false)
            .expect("empty H3 WHERE still uses fused key flags");
        assert_eq!(
            bound.desc.keys[0].flags,
            abi::PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT
        );
        assert!(bound._where.is_none());
        assert!(
            H3ParentArtifact::new(h3, 0, 1, None, None).is_err(),
            "cached prevalidation must not move H3 errors before WHERE"
        );
    }

    #[test]
    fn hash_where_selection_and_empty_capacity_learning_are_exact() {
        let (mut spec, _) = h3_count_shape();
        spec.group_keys[0].source = GroupKeySource::FactColumn(column(INT8OID));
        spec.group_keys[0].type_oid = INT8OID;
        for rows in 0..8 {
            for selected in 0..10 {
                assert_eq!(
                    validate_hash_selected_rows(&spec, rows, selected).is_ok(),
                    selected == rows as u64
                );
            }
        }
        spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 4,
                ..column(BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        for rows in 0..8 {
            for selected in 0..10 {
                assert_eq!(
                    validate_hash_selected_rows(&spec, rows, selected).is_ok(),
                    selected <= rows as u64
                );
            }
        }
        assert!(validate_hash_selected_rows(&spec, 1, u64::MAX).is_err());
        let make = || FusedHashArtifact {
            resolved_spec: spec.clone(),
            fact_rows: 65_537,
            group_capacity: Cell::new(100),
            resolution: 0,
        };
        let artifact = make();
        artifact
            .record_validated_raw_groups(0)
            .expect("WHERE rejected every row");
        assert_eq!(artifact.group_capacity.get(), 1);
        artifact
            .record_validated_raw_groups(0)
            .expect("warm empty result");
        assert!(artifact.record_validated_raw_groups(2).is_err());
        assert_eq!(artifact.group_capacity.get(), 1);
        assert_eq!(
            make().group_capacity.get(),
            100,
            "new generation forgets the empty bound"
        );
        validate_h3_count_partition(&[0], 1, 0, 0, HashMeasureInput::Star)
            .expect("empty WHERE count");
        assert!(validate_h3_count_partition(&[0], 1, 1, 0, HashMeasureInput::Star).is_err());
        assert_eq!(h3_mask_work(&spec, 65_537), Ok((65_537, 2)));
        spec.measures[0].filter = spec.fact_filter.clone();
        assert_eq!(h3_mask_work(&spec, 65_537), Ok((65_537, 2)));
        spec.measures[0].filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 3,
                ..column(BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        assert_eq!(h3_mask_work(&spec, 65_537), Ok((131_074, 4)));
        assert!(h3_mask_work(&spec, usize::MAX).is_err());
    }

    #[test]
    fn hash_where_implies_only_identical_validated_boolean_filters() {
        let (mut spec, projection) = h3_count_shape();
        spec.fact_filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 3,
                ..column(BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        spec.measures[0].filter = spec.fact_filter.clone();
        assert_eq!(h3_count_filter(&spec), Ok(None));
        assert_eq!(
            HashMeasureInput::from_spec(&spec),
            Ok(HashMeasureInput::Star)
        );
        // Every admitted row contributes to COUNT(*): folding strengthens,
        // rather than relaxes, the compact output partition proof.
        let kind = HashMeasureInput::from_spec(&spec).expect("validated implied FILTER");
        validate_h3_count_partition(&[2, 3, 0], 3, 2, 5, kind)
            .expect("exact selected-row partition");
        assert!(validate_h3_count_partition(&[2, 2, 0], 3, 2, 5, kind).is_err());
        assert!(validate_h3_count_partition(&[0, 5, 0], 3, 2, 5, kind).is_err());
        for rows in [0, 1, 65_535, 65_536, 65_537, 1_000_017] {
            assert_eq!(
                h3_mask_work(&spec, rows),
                Ok((
                    rows as u64,
                    rows.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS) as u64
                ))
            );
        }
        for raw in [false, true] {
            let mut shape = spec.clone();
            if raw {
                shape.group_keys[0].source = GroupKeySource::FactColumn(column(INT8OID));
                shape.group_keys[0].type_oid = INT8OID;
            }
            let requests = compact_hash_column_refs(&shape).expect("shared predicate requests");
            assert_eq!(requests.iter().map(|r| r.attno).collect::<Vec<_>>(), [1, 3]);
            assert_eq!(
                selected_relations(&shape).expect("shared dependencies")[0].columns,
                [1, 3]
            );
            assert!(
                artifact_column_refs(&shape)
                    .expect("shared artifact dependency")
                    .iter()
                    .any(|r| r.attno == 3)
            );
            assert!(validate_normal_descriptor_capability(&shape, &projection).is_err());
        }
        // COUNT(column) remains nullable even though its FILTER is implied.
        let value = ColumnRef {
            attno: 2,
            ..column(INT4OID)
        };
        spec.measures[0].expression = MeasureExpr::Column(value);
        assert_eq!(
            HashMeasureInput::from_spec(&spec),
            Ok(HashMeasureInput::Column(value))
        );
        validate_h3_count_partition(&[0, 2, 0], 3, 2, 5, HashMeasureInput::Column(value))
            .expect("nullable measure may contribute fewer selected rows");
        spec.measures[0].expression = MeasureExpr::CountStar;

        // Neither another attribute nor another Boolean predicate kind proves
        // implication. Identical invalid predicates must still fail closed.
        spec.measures[0].filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 4,
                ..column(BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        assert!(
            h3_count_filter(&spec)
                .expect("distinct valid FILTER")
                .is_some()
        );
        assert_eq!(
            HashMeasureInput::from_spec(&spec),
            Ok(HashMeasureInput::FilteredStar)
        );
        for bad in [
            FilterSpec::Mask {
                input: column(BOOLOID),
                kind: MaskKind::Recheck,
            },
            FilterSpec::Mask {
                input: column(INT4OID),
                kind: MaskKind::Sql,
            },
            FilterSpec::Mask {
                input: ColumnRef {
                    attno: 0,
                    ..column(BOOLOID)
                },
                kind: MaskKind::Sql,
            },
            FilterSpec::Mask {
                input: ColumnRef {
                    relation_oid: spec.fact_rel + 1,
                    ..column(BOOLOID)
                },
                kind: MaskKind::Sql,
            },
        ] {
            spec.fact_filter = bad.clone();
            spec.measures[0].filter = bad;
            assert!(h3_count_filter(&spec).is_err());
            assert!(HashMeasureInput::from_spec(&spec).is_err());
        }
    }

    #[test]
    fn raw_hash_capacity_bounds_workspace_not_input_cardinality() {
        assert_eq!(raw_hash_group_capacity(0, 1), Ok(1));
        assert_eq!(raw_hash_group_capacity(16385, 16385), Ok(16385));
        assert!(raw_hash_group_capacity(0, 0).is_err());
        assert_eq!(raw_hash_group_capacity(16385, 16384), Ok(16384));
        assert_eq!(raw_hash_group_capacity(12_748_986, 100_000), Ok(100_000));
        assert_eq!(raw_hash_group_capacity(u32::MAX as usize, 1), Ok(1));
        assert_eq!(raw_hash_group_capacity(1 << 30, usize::MAX), Ok(1 << 30));
        assert_eq!(
            raw_hash_group_capacity((1 << 30) + 1, usize::MAX),
            Ok(1 << 30)
        );
        assert!(raw_hash_group_capacity(u32::MAX as usize + 1, usize::MAX).is_err());
    }

    #[test]
    fn raw_hash_learns_only_validated_capacity_for_its_current_generation() {
        let (mut raw, _) = h3_count_shape();
        raw.group_keys[0].source = GroupKeySource::FactColumn(column(INT8OID));
        raw.group_keys[0].type_oid = INT8OID;
        let make = || FusedHashArtifact {
            resolved_spec: raw.clone(),
            fact_rows: 12_748_986,
            group_capacity: Cell::new(100_000),
            resolution: 0,
        };
        let artifact = make();
        artifact
            .record_validated_raw_groups(4467)
            .expect("successful compact result");
        assert_eq!(artifact.group_capacity.get(), 4467);
        artifact
            .record_validated_raw_groups(4467)
            .expect("same immutable keys");
        for invalid in [0, 4468, usize::MAX] {
            assert!(artifact.record_validated_raw_groups(invalid).is_err());
            assert_eq!(
                artifact.group_capacity.get(),
                4467,
                "failure cannot publish a new bound"
            );
        }
        let rebuilt = make();
        assert_eq!(
            rebuilt.group_capacity.get(),
            100_000,
            "new generation discards learned state"
        );
        let (h3, _) = h3_count_shape();
        let fused_h3 = FusedHashArtifact {
            resolved_spec: h3,
            ..make()
        };
        fused_h3
            .record_validated_raw_groups(4467)
            .expect("H3 behavior unchanged");
        assert_eq!(fused_h3.group_capacity.get(), 100_000);
    }

    #[test]
    fn empty_h3_descriptor_and_view_lookup_fail_closed_without_device_work() {
        let (spec, _) = h3_count_shape();
        let GroupKeySource::H3CellToParent { cell, .. } = spec.group_keys[0].source else {
            unreachable!()
        };
        let artifact = H3ParentArtifact::new(spec, 0, 1, None, None).expect("empty H3 artifact");
        let request = ResidentColumnRef {
            relid: pg_sys::Oid::from(cell.relation_oid),
            attno: 1,
        };
        let view = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(cell.type_oid),
        };
        let desc = build_h3_descriptor(&artifact, &[request], std::slice::from_ref(&view), cell)
            .expect("empty H3 descriptor binds")
            .desc;
        assert_eq!(desc.grouping_mode, abi::PGACCEL_GROUPED_AGG_GROUPING_HASH);
        assert_eq!(desc.output_mode, abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        assert_eq!(desc.group_capacity, 1);
        assert!(desc.keys[0].values.values.is_null());

        let fused = FusedHashArtifact {
            resolved_spec: artifact.resolved_spec,
            fact_rows: 0,
            group_capacity: Cell::new(1),
            resolution: 7,
        };
        let fused_desc = build_fused_hash_descriptor(
            &fused,
            &[request],
            std::slice::from_ref(&view),
            cell,
            &mut || false,
        )
        .expect("empty fused H3 descriptor binds")
        .desc;
        assert_eq!(
            fused_desc.keys[0].flags,
            abi::PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT
        );
        assert_eq!(fused_desc.keys[0].pad0, 7);
        assert!(fused_desc.keys[0].values.values.is_null());
        assert_eq!(fused_desc.group_capacity, desc.group_capacity);
        assert!(
            matches!(
                build_fused_hash_descriptor(
                    &fused,
                    &[request],
                    std::slice::from_ref(&view),
                    cell,
                    &mut || true
                ),
                Err(ResidentLoadError::PreparationInterrupted)
            ),
            "empty unfiltered binding still observes pending signals without FFI"
        );

        let invalid_fused = FusedHashArtifact {
            resolution: 16,
            ..fused
        };
        assert!(
            build_fused_hash_descriptor(
                &invalid_fused,
                &[request],
                std::slice::from_ref(&view),
                cell,
                &mut || false,
            )
            .is_err()
        );

        assert!(find_view(&[], &[], cell).is_err());
        assert!(find_view(&[request], &[], cell).is_err());
        let wrong = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(INT4OID),
        };
        assert!(find_view(&[request], &[wrong], cell).is_err());
        let mut invalid = cell;
        invalid.attno = i32::MAX;
        let view = ResidentColumnView::Empty {
            type_oid: pg_sys::Oid::from(cell.type_oid),
        };
        assert!(find_view(&[request], &[view], invalid).is_err());
    }
}

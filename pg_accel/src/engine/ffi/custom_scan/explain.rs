//! EXPLAIN output for Custom Scan nodes.
//!
//! Reports strategy + batch config always; execution counters only under
//! EXPLAIN ANALYZE.

use std::ffi::{CStr, CString, c_int};

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy, dsm};
use crate::engine::executor::agg::AggExecState;
use crate::engine::residency::{
    ArtifactEnsureOutcome, ResidentProofSnapshot, ResidentRelationEvidence,
};
use crate::engine::spec::{
    AggOutputProjection, AggOutputSource, AggQuerySpec, AggregateKind, AggregateSource,
    BinaryMeasureOp, FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity, MaskKind,
    MeasureExpr, SpatialOperand, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
};

use super::private_data::{RESIDENT_PROOF_VERSION, resident_proof_default_for_strategy};

/// `ExplainCustomScan`: emit EXPLAIN output.
///
/// Always shows Strategy, Batch Size, Expected Threads. When `EXPLAIN ANALYZE`,
/// also shows Rows Dispatched, Batches, and Dispatch Time.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
#[pgrx::pg_guard]
pub(super) unsafe extern "C-unwind" fn explain_custom_scan(
    node: *mut pg_sys::CustomScanState,
    _ancestors: *mut pg_sys::List,
    es: *mut pg_sys::ExplainState,
) {
    let _span = tracing::debug_span!("ffi.explain_custom_scan").entered();
    let state = node.cast::<GpuAccelScanState>();

    // SAFETY: state is our extended struct, es is a valid ExplainState.
    unsafe {
        let strategy = GpuStrategy::decode((*state).accel.strategy);

        pg_sys::ExplainPropertyText(c"Strategy".as_ptr(), strategy.label().as_ptr(), es);
        pg_sys::ExplainPropertyBool(c"Plan Selected".as_ptr(), true, es);
        pg_sys::ExplainPropertyInteger(
            c"Batch Size".as_ptr(),
            std::ptr::null(),
            i64::from((*state).accel.batch_size),
            es,
        );
        pg_sys::ExplainPropertyInteger(
            c"Expected Threads".as_ptr(),
            std::ptr::null(),
            i64::from((*state).accel.expected_threads),
            es,
        );
        let resident_proof = (*state).accel.resident_proof;
        pg_sys::ExplainPropertyBool(
            c"GPU Resident Pipeline".as_ptr(),
            resident_proof.gpu_resident_pipeline(),
            es,
        );
        pg_sys::ExplainPropertyInteger(
            c"GPU Resident Proof Version".as_ptr(),
            std::ptr::null(),
            i64::from(RESIDENT_PROOF_VERSION),
            es,
        );
        explain_resident_operator_class(resident_proof, es);
        if strategy == GpuStrategy::Agg && !(*state).accel.executor.is_null() {
            let agg_state = &*(*state).accel.executor.cast::<AggExecState>();
            explain_descriptor_agg(agg_state, es);
        }
        pg_sys::ExplainPropertyInteger(
            c"GPU Resident Stage Mask".as_ptr(),
            std::ptr::null(),
            i64::from(resident_proof.stage_mask),
            es,
        );
        pg_sys::ExplainPropertyInteger(
            c"GPU Resident Device Columns".as_ptr(),
            std::ptr::null(),
            i64::from(resident_proof.device_columns),
            es,
        );
        explain_resident_boundary(strategy, resident_proof, es);

        // Execution stats only with EXPLAIN ANALYZE.
        if (*es).analyze {
            pg_sys::ExplainPropertyBool(
                c"GPU Kernel Dispatched".as_ptr(),
                gpu_kernel_dispatched_for_explain(strategy, state),
                es,
            );
            pg_sys::ExplainPropertyInteger(
                c"Rows Returned To CPU".as_ptr(),
                std::ptr::null(),
                rows_returned_to_cpu(node),
                es,
            );
            pg_sys::ExplainPropertyInteger(
                c"Rows Dispatched".as_ptr(),
                std::ptr::null(),
                (*state).accel.rows_dispatched as i64,
                es,
            );
            pg_sys::ExplainPropertyInteger(
                c"Batches".as_ptr(),
                std::ptr::null(),
                (*state).accel.batches_executed as i64,
                es,
            );
            pg_sys::ExplainPropertyFloat(
                c"Rows Per Batch".as_ptr(),
                std::ptr::null(),
                rows_per_batch_for_explain(
                    (*state).accel.rows_dispatched,
                    (*state).accel.batches_executed,
                ),
                3,
                es,
            );

            #[allow(clippy::cast_precision_loss)]
            let time_ms = (*state).accel.dispatch_time_us as f64 / 1000.0;
            pg_sys::ExplainPropertyFloat(c"Dispatch Time".as_ptr(), c"ms".as_ptr(), time_ms, 3, es);
            pg_sys::ExplainPropertyFloat(
                c"Avg Dispatch Time Per Batch".as_ptr(),
                c"ms".as_ptr(),
                avg_dispatch_time_per_batch_ms_for_explain(
                    (*state).accel.dispatch_time_us,
                    (*state).accel.batches_executed,
                ),
                3,
                es,
            );

            let parallel_agg_counters = if strategy == GpuStrategy::Agg {
                dsm::parallel_agg_counter_snapshot(state)
            } else {
                None
            };
            pg_sys::ExplainPropertyText(
                c"Counter Scope".as_ptr(),
                if parallel_agg_counters.is_some() {
                    c"Local Backend; Parallel Totals Below".as_ptr()
                } else {
                    c"Local Backend".as_ptr()
                },
                es,
            );
            if let Some(counters) = parallel_agg_counters
                && counters.participants > 0
            {
                pg_sys::ExplainPropertyInteger(
                    c"Parallel Participants Reported".as_ptr(),
                    std::ptr::null(),
                    i64::from(counters.participants),
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Parallel Active Participants".as_ptr(),
                    std::ptr::null(),
                    i64::from(counters.active_participants),
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Parallel Rows Dispatched Total".as_ptr(),
                    std::ptr::null(),
                    counters.rows_dispatched as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Parallel Batches Total".as_ptr(),
                    std::ptr::null(),
                    counters.batches_executed as i64,
                    es,
                );
                #[allow(clippy::cast_precision_loss)]
                let parallel_time_ms = counters.dispatch_time_us as f64 / 1000.0;
                pg_sys::ExplainPropertyFloat(
                    c"Parallel Dispatch Time Sum".as_ptr(),
                    c"ms".as_ptr(),
                    parallel_time_ms,
                    3,
                    es,
                );
                pg_sys::ExplainPropertyFloat(
                    c"Parallel Avg Dispatch Time Per Batch".as_ptr(),
                    c"ms".as_ptr(),
                    avg_dispatch_time_per_batch_ms_for_explain(
                        counters.dispatch_time_us,
                        counters.batches_executed,
                    ),
                    3,
                    es,
                );
            }

            // For Agg strategy, report whether GPU reduce was used and
            // whether this is a partial (worker-side) aggregate path.
            if strategy == GpuStrategy::Agg && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as AggExecState.
                let agg_state = &*(*state).accel.executor.cast::<AggExecState>();
                pg_sys::ExplainPropertyBool(
                    c"GPU Dispatched".as_ptr(),
                    agg_state.gpu_dispatched,
                    es,
                );
                if agg_state.descriptor_contract().is_some() {
                    pg_sys::ExplainPropertyFloat(
                        c"Raw Relation Load Time".as_ptr(),
                        c"ms".as_ptr(),
                        agg_state.descriptor_raw_load_ms().unwrap_or(0.0),
                        3,
                        es,
                    );
                    #[allow(clippy::cast_precision_loss)]
                    let preparation_ms =
                        agg_state.descriptor_preparation_time_us().unwrap_or(0) as f64 / 1000.0;
                    pg_sys::ExplainPropertyFloat(
                        c"Total Residency Preparation Time".as_ptr(),
                        c"ms".as_ptr(),
                        preparation_ms,
                        3,
                        es,
                    );
                }
            }
        }
    }
}

/// Determine expected thread count (GPU-only: always 1, no CPU worker pool).
pub(super) fn resolve_thread_count() -> c_int {
    1
}

unsafe fn gpu_kernel_dispatched_for_explain(
    strategy: GpuStrategy,
    state: *const GpuAccelScanState,
) -> bool {
    // SAFETY: the EXPLAIN callback passes this node's live extended
    // CustomScanState, which remains allocated for the callback duration.
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        return false;
    }

    match strategy {
        GpuStrategy::Agg | GpuStrategy::Raster => {
            // SAFETY: resident executors store counters in this live state.
            unsafe { (*state).accel.batches_executed > 0 }
        }
        GpuStrategy::Scan
        | GpuStrategy::Join
        | GpuStrategy::Sort
        | GpuStrategy::Window
        | GpuStrategy::PreAgg
        | GpuStrategy::FunctionScan
        | GpuStrategy::SrfTargetList => false,
    }
}

unsafe fn rows_returned_to_cpu(node: *mut pg_sys::CustomScanState) -> i64 {
    // SAFETY: `node` is the live CustomScanState passed to EXPLAIN; PostgreSQL
    // owns its PlanState instrumentation pointer for the query lifetime.
    let instrument = unsafe { (*node).ss.ps.instrument };
    if instrument.is_null() {
        return 0;
    }
    // SAFETY: the null check proves PostgreSQL supplied an Instrumentation
    // object that remains live while EXPLAIN reads its tuple counter.
    let tuple_count = unsafe { (*instrument).tuplecount };
    if tuple_count.is_finite() && tuple_count > 0.0 {
        tuple_count.round() as i64
    } else {
        0
    }
}

fn rows_per_batch_for_explain(rows_dispatched: u64, batches_executed: u64) -> f64 {
    if batches_executed == 0 {
        0.0
    } else {
        rows_dispatched as f64 / batches_executed as f64
    }
}

fn avg_dispatch_time_per_batch_ms_for_explain(dispatch_time_us: u64, batches_executed: u64) -> f64 {
    if batches_executed == 0 {
        0.0
    } else {
        (dispatch_time_us as f64 / 1000.0) / batches_executed as f64
    }
}

fn gpu_resident_boundary_reason(strategy: GpuStrategy) -> &'static CStr {
    match strategy {
        GpuStrategy::Scan => c"GpuScan retired; no executable method table is registered",
        GpuStrategy::Join => c"GpuJoin retired; resident joins execute inside GpuAgg descriptors",
        GpuStrategy::Agg => c"GpuAgg requires a sealed resident proof and childless descriptor",
        GpuStrategy::Sort => c"GpuSort strategy retired; no plan can carry it",
        GpuStrategy::Window => c"GpuWindow retired; no executable method table is registered",
        GpuStrategy::PreAgg => c"GpuPreAgg strategy retired; no plan can carry it",
        GpuStrategy::FunctionScan => c"GpuFunctionScan retired; no executable method table is registered",
        GpuStrategy::SrfTargetList => c"GpuAccelSrfTargetList retired; no executable method table is registered",
        GpuStrategy::Raster => c"GpuRaster reads a generation-stamped resident raster column, retains only reconstructed output WKB, and materializes PostgreSQL raster values at final output",
    }
}

unsafe fn explain_resident_boundary(
    strategy: GpuStrategy,
    proof: ResidentProofSnapshot,
    es: *mut pg_sys::ExplainState,
) {
    if !proof.gpu_resident_pipeline() && proof == resident_proof_default_for_strategy(strategy) {
        // SAFETY: PostgreSQL supplied a live ExplainState; both property name
        // and strategy boundary label are static NUL-terminated strings.
        unsafe {
            pg_sys::ExplainPropertyText(
                c"GPU Resident Boundary".as_ptr(),
                gpu_resident_boundary_reason(strategy).as_ptr(),
                es,
            );
        }
        return;
    }

    let boundary = if let Ok(boundary) = CString::new(proof.boundary_label()) {
        boundary
    } else {
        // SAFETY: PostgreSQL supplied a live ExplainState and both fallback
        // strings have static storage for the synchronous property call.
        unsafe {
            pg_sys::ExplainPropertyText(
                c"GPU Resident Boundary".as_ptr(),
                c"invalid_boundary_label".as_ptr(),
                es,
            );
        }
        return;
    };
    // SAFETY: `es` is live for this callback and `boundary` remains allocated
    // until ExplainPropertyText has copied/consumed the C string.
    unsafe {
        pg_sys::ExplainPropertyText(c"GPU Resident Boundary".as_ptr(), boundary.as_ptr(), es);
    }
}

unsafe fn explain_resident_operator_class(
    proof: ResidentProofSnapshot,
    es: *mut pg_sys::ExplainState,
) {
    let label = if let Ok(label) = CString::new(proof.operator_class_label()) {
        label
    } else {
        // SAFETY: PostgreSQL supplied a live ExplainState and both fallback
        // strings have static storage for the synchronous property call.
        unsafe {
            pg_sys::ExplainPropertyText(
                c"GPU Resident Operator Class".as_ptr(),
                c"invalid_operator_class".as_ptr(),
                es,
            );
        }
        return;
    };
    // SAFETY: `es` is live for this callback and `label` remains allocated
    // through the synchronous ExplainPropertyText call.
    unsafe {
        pg_sys::ExplainPropertyText(c"GPU Resident Operator Class".as_ptr(), label.as_ptr(), es);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DescriptorExplainSummary {
    strategy: &'static str,
    group_keys: String,
    aggregates: String,
    filter: String,
    star_dimensions: String,
    output: String,
}

#[derive(Debug, PartialEq, Eq)]
struct DescriptorResidencySummary {
    state: String,
    artifact: String,
    generations: String,
    bytes: String,
}

fn descriptor_explain_summary(
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
) -> DescriptorExplainSummary {
    let strategy = if spec.group_keys.is_empty() {
        "descriptor_ungrouped_aggregate"
    } else {
        "descriptor_grouped_aggregate"
    };
    let group_keys = format_or_none(spec.group_keys.iter().enumerate().map(|(index, key)| {
        format!(
            "k{index}:{} type={} collation={} encoding={}",
            group_key_source_summary(&key.source),
            key.type_oid,
            key.collation_oid,
            group_key_encoding_summary(key.encoding)
        )
    }));
    let aggregates = format_or_none(spec.measures.iter().enumerate().map(|(index, measure)| {
        let outputs = format_or_none(measure.outputs.iter().map(|output| {
            format!(
                "{}.{}",
                aggregate_source_label(output.source),
                aggregate_kind_label(output.kind)
            )
        }));
        format!(
            "m{index}:{} -> {outputs}",
            measure_expr_summary(&measure.expression)
        )
    }));
    let measure_filters = format_or_none(
        spec.measures
            .iter()
            .enumerate()
            .map(|(index, measure)| format!("m{index}={}", filter_summary(&measure.filter))),
    );
    let dimension_filters = format_or_none(
        spec.star_dims
            .iter()
            .enumerate()
            .map(|(index, dimension)| format!("d{index}={}", filter_summary(&dimension.filter))),
    );
    let filter = format!(
        "fact={}; measures=[{measure_filters}]; dimensions=[{dimension_filters}]",
        filter_summary(&spec.fact_filter)
    );
    let star_dimensions =
        format_or_none(spec.star_dims.iter().enumerate().map(|(index, dimension)| {
            format!(
                "d{index}:rel={} join={}<->{} collation={} multiplicity={} filter={}",
                dimension.relation_oid,
                column_summary(dimension.fact_key),
                column_summary(dimension.dim_key),
                dimension.collation_oid,
                join_multiplicity_label(dimension.multiplicity),
                filter_summary(&dimension.filter)
            )
        }));
    let output = format_or_none(projection.slots.iter().enumerate().map(|(index, slot)| {
        let source = match slot.source {
            AggOutputSource::GroupKey { key_index } => format!("group[k{key_index}]"),
            AggOutputSource::Aggregate {
                measure_index,
                source,
                kind,
            } => format!(
                "aggregate[m{measure_index}].{}.{}",
                aggregate_source_label(source),
                aggregate_kind_label(kind)
            ),
        };
        format!(
            "slot{index}:{source} source_type={} result_type={} typmod={} collation={} nullable={}",
            slot.source_type_oid,
            slot.result_type_oid,
            slot.result_typmod,
            slot.result_collation_oid,
            slot.nullable
        )
    }));
    DescriptorExplainSummary {
        strategy,
        group_keys,
        aggregates,
        filter,
        star_dimensions,
        output,
    }
}

fn descriptor_residency_summary(
    evidence: Option<&[ResidentRelationEvidence]>,
    loaded_relations: Option<&[pg_sys::Oid]>,
    artifact_outcome: Option<ArtifactEnsureOutcome>,
    artifact_bytes: Option<u64>,
) -> DescriptorResidencySummary {
    let Some(evidence) = evidence else {
        return DescriptorResidencySummary {
            state: "not initialized (EXPLAIN ONLY)".to_owned(),
            artifact: "not initialized".to_owned(),
            generations: "not inspected".to_owned(),
            bytes: "not inspected".to_owned(),
        };
    };
    let raw_bytes = evidence.iter().fold(0_u64, |total, relation| {
        total.saturating_add(relation.raw_bytes)
    });
    let derived_bytes = evidence.iter().fold(0_u64, |total, relation| {
        total.saturating_add(relation.derived_bytes)
    });
    let total_bytes = raw_bytes.saturating_add(derived_bytes);
    let generations = format_or_none(evidence.iter().map(|relation| {
        format!(
            "rel={} generation={} global={} relfilenode={}",
            u32::from(relation.relid),
            relation.generation,
            relation.global_generation,
            u32::from(relation.relfilenode)
        )
    }));
    let loaded_relations = loaded_relations.unwrap_or_default();
    let loaded = format_or_none(
        loaded_relations
            .iter()
            .map(|relid| u32::from(*relid).to_string()),
    );
    DescriptorResidencySummary {
        state: format!(
            "resident ({} relations; loaded/reloaded={loaded})",
            evidence.len()
        ),
        artifact: artifact_outcome.map_or_else(
            || "unknown".to_owned(),
            |outcome| artifact_ensure_outcome_label(outcome).to_owned(),
        ),
        generations,
        bytes: format!(
            "raw={raw_bytes} derived={derived_bytes} artifact={} total={total_bytes}",
            artifact_bytes.unwrap_or(0)
        ),
    }
}

fn format_or_none(values: impl Iterator<Item = String>) -> String {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join("; ")
    }
}

fn column_summary(column: crate::engine::spec::ColumnRef) -> String {
    format!(
        "{}.{}:type={}",
        column.relation_oid, column.attno, column.type_oid
    )
}

fn group_key_source_summary(source: &GroupKeySource) -> String {
    match source {
        GroupKeySource::FactColumn(column) => format!("fact({})", column_summary(*column)),
        GroupKeySource::StarDimension {
            dim_index,
            group_column,
        } => format!("dimension[{dim_index}]({})", column_summary(*group_column)),
        GroupKeySource::Expression { inputs, program } => format!(
            "expression(inputs={}, words={})",
            inputs.len(),
            program.len()
        ),
        GroupKeySource::H3CellToParent { cell, resolution } => format!(
            "h3_cell_to_parent({}, resolution={resolution})",
            column_summary(*cell)
        ),
        GroupKeySource::H3LatLngToCell {
            latitude,
            longitude,
            resolution,
        } => format!(
            "h3_latlng_to_cell(latitude={}, longitude={}, resolution={resolution})",
            column_summary(*latitude),
            column_summary(*longitude)
        ),
    }
}

fn group_key_encoding_summary(encoding: GroupKeyEncoding) -> String {
    match encoding {
        GroupKeyEncoding::DenseI32 {
            code_min,
            cardinality,
            null_code,
        } => format!(
            "dense_i32(min={code_min}, cardinality={cardinality}, null={})",
            optional_i32_label(null_code)
        ),
        GroupKeyEncoding::DictionaryI32 {
            cardinality,
            null_code,
        } => format!(
            "dictionary_i32(cardinality={cardinality}, null={})",
            optional_i32_label(null_code)
        ),
        GroupKeyEncoding::Hash => "hash".to_owned(),
    }
}

fn optional_i32_label(value: Option<i32>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn measure_expr_summary(expression: &MeasureExpr) -> String {
    match expression {
        MeasureExpr::CountStar => "count_star".to_owned(),
        MeasureExpr::Column(column) => format!("column({})", column_summary(*column)),
        MeasureExpr::Binary { op, lhs, rhs } => format!(
            "{}({}, {})",
            binary_measure_op_label(*op),
            column_summary(*lhs),
            column_summary(*rhs)
        ),
        MeasureExpr::StatsPair { value, rhs } => format!(
            "stats_pair({}, {})",
            column_summary(*value),
            column_summary(*rhs)
        ),
        MeasureExpr::Bytecode {
            inputs,
            program,
            result_type_oid,
        } => format!(
            "bytecode(inputs={}, words={}, result_type={result_type_oid})",
            inputs.len(),
            program.len()
        ),
    }
}

const fn binary_measure_op_label(op: BinaryMeasureOp) -> &'static str {
    match op {
        BinaryMeasureOp::Mul => "mul",
        BinaryMeasureOp::Sub => "sub",
    }
}

const fn aggregate_source_label(source: AggregateSource) -> &'static str {
    match source {
        AggregateSource::Value => "value",
        AggregateSource::Rhs => "rhs",
    }
}

const fn aggregate_kind_label(kind: AggregateKind) -> &'static str {
    match kind {
        AggregateKind::Sum => "sum",
        AggregateKind::Count => "count",
        AggregateKind::Min => "min",
        AggregateKind::Max => "max",
        AggregateKind::Avg => "avg",
        AggregateKind::StddevSamp => "stddev_samp",
    }
}

fn filter_summary(filter: &FilterSpec) -> String {
    match filter {
        FilterSpec::None => "none".to_owned(),
        FilterSpec::Ranges { input, ranges } => format!(
            "ranges(input={}, count={})",
            column_summary(*input),
            ranges.len()
        ),
        FilterSpec::Mask { input, kind } => format!(
            "{}_mask(input={})",
            mask_kind_label(*kind),
            column_summary(*input)
        ),
        FilterSpec::Bytecode { inputs, program } => {
            format!("bytecode(inputs={}, words={})", inputs.len(), program.len())
        }
        FilterSpec::Spatial {
            predicate,
            left,
            right,
            distance,
        } => format!(
            "spatial(predicate={}, left={}, right={}, distance={})",
            spatial_predicate_label(*predicate),
            spatial_operand_summary(left),
            spatial_operand_summary(right),
            distance.is_some(),
        ),
    }
}

fn spatial_operand_summary(operand: &SpatialOperand) -> String {
    match operand {
        SpatialOperand::Column { column, metadata } => format!(
            "column({}, {})",
            column_summary(*column),
            spatial_metadata_summary(*metadata)
        ),
        SpatialOperand::Constant { metadata, bytes } => format!(
            "constant({}, bytes={})",
            spatial_metadata_summary(*metadata),
            bytes.len()
        ),
    }
}

fn spatial_metadata_summary(metadata: SpatialValueMetadata) -> String {
    format!(
        "kind={}, typmod={}, srid={}",
        match metadata.kind {
            SpatialValueKind::Geometry => "geometry",
            SpatialValueKind::Geography => "geography",
        },
        metadata.typmod,
        metadata
            .srid
            .map_or_else(|| "dynamic".to_owned(), |srid| srid.to_string())
    )
}

const fn spatial_predicate_label(predicate: SpatialPredicateKind) -> &'static str {
    match predicate {
        SpatialPredicateKind::Intersects => "intersects",
        SpatialPredicateKind::Contains => "contains",
        SpatialPredicateKind::Within => "within",
        SpatialPredicateKind::DWithin => "dwithin",
        SpatialPredicateKind::Disjoint => "disjoint",
        SpatialPredicateKind::Equals => "equals",
        SpatialPredicateKind::Touches => "touches",
        SpatialPredicateKind::Crosses => "crosses",
        SpatialPredicateKind::Overlaps => "overlaps",
    }
}

const fn mask_kind_label(kind: MaskKind) -> &'static str {
    match kind {
        MaskKind::Sql => "sql",
        MaskKind::Recheck => "recheck",
    }
}

const fn join_multiplicity_label(multiplicity: JoinMultiplicity) -> &'static str {
    match multiplicity {
        JoinMultiplicity::Unique => "unique",
        JoinMultiplicity::Counted => "counted",
    }
}

const fn artifact_ensure_outcome_label(outcome: ArtifactEnsureOutcome) -> &'static str {
    match outcome {
        ArtifactEnsureOutcome::Hit => "hit",
        ArtifactEnsureOutcome::Built => "built",
        ArtifactEnsureOutcome::Rebuilt => "rebuilt",
    }
}

unsafe fn explain_descriptor_agg(agg_state: &AggExecState, es: *mut pg_sys::ExplainState) {
    let Some((spec, projection)) = agg_state.descriptor_contract() else {
        return;
    };
    let logical = descriptor_explain_summary(spec, projection);
    let residency = descriptor_residency_summary(
        agg_state.descriptor_residency_evidence(),
        agg_state.descriptor_loaded_relations(),
        agg_state.descriptor_artifact_outcome(),
        agg_state.descriptor_artifact_bytes(),
    );
    for (name, value) in [
        (c"GPU Descriptor Strategy", logical.strategy.to_owned()),
        (c"GPU Descriptor Group Keys", logical.group_keys),
        (c"GPU Descriptor Aggregates", logical.aggregates),
        (c"GPU Descriptor Filter", logical.filter),
        (c"GPU Descriptor Star Dimensions", logical.star_dimensions),
        (c"GPU Descriptor Output", logical.output),
        (c"GPU Descriptor Residency State", residency.state),
        (c"GPU Descriptor Artifact", residency.artifact),
        (c"GPU Descriptor Generations", residency.generations),
        (c"GPU Descriptor Bytes", residency.bytes),
    ] {
        let value = CString::new(value).expect("descriptor EXPLAIN values never contain NUL");
        // SAFETY: name and value are live C strings; `es` is PostgreSQL-owned.
        unsafe { pg_sys::ExplainPropertyText(name.as_ptr(), value.as_ptr(), es) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::registry::AccelStrategy;
    use crate::engine::spec::{
        AggOutputSlot, AggregateOutput, ColumnRef, DimSpec, GroupKeyRef, MeasureSpec, ScalarRange,
        ScalarValue,
    };

    fn descriptor_contract() -> (AggQuerySpec, AggOutputProjection) {
        let fact_column = |attno, type_oid| ColumnRef {
            relation_oid: 10,
            attno,
            type_oid,
        };
        let dim_column = |attno, type_oid| ColumnRef {
            relation_oid: 20,
            attno,
            type_oid,
        };
        (
            AggQuerySpec {
                fact_rel: 10,
                group_keys: vec![GroupKeyRef {
                    source: GroupKeySource::StarDimension {
                        dim_index: 0,
                        group_column: dim_column(3, 25),
                    },
                    type_oid: 25,
                    collation_oid: 100,
                    encoding: GroupKeyEncoding::Hash,
                }],
                measures: vec![MeasureSpec {
                    expression: MeasureExpr::Column(fact_column(4, 23)),
                    outputs: vec![
                        AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Sum,
                        },
                        AggregateOutput {
                            source: AggregateSource::Value,
                            kind: AggregateKind::Count,
                        },
                    ],
                    filter: FilterSpec::None,
                }],
                fact_filter: FilterSpec::Ranges {
                    input: fact_column(5, 23),
                    ranges: vec![ScalarRange {
                        lo: ScalarValue::I32(1),
                        hi: ScalarValue::I32(9),
                    }],
                },
                star_dims: vec![DimSpec {
                    relation_oid: 20,
                    fact_key: fact_column(2, 23),
                    dim_key: dim_column(1, 23),
                    collation_oid: 0,
                    multiplicity: JoinMultiplicity::Counted,
                    filter: FilterSpec::Mask {
                        input: dim_column(4, 16),
                        kind: MaskKind::Sql,
                    },
                }],
                having: None,
            },
            AggOutputProjection {
                slots: vec![
                    AggOutputSlot {
                        source: AggOutputSource::GroupKey { key_index: 0 },
                        source_type_oid: 25,
                        result_type_oid: 25,
                        result_typmod: -1,
                        result_collation_oid: 100,
                        nullable: true,
                    },
                    AggOutputSlot {
                        source: AggOutputSource::Aggregate {
                            measure_index: 0,
                            source: AggregateSource::Value,
                            kind: AggregateKind::Sum,
                        },
                        source_type_oid: 23,
                        result_type_oid: 20,
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
                        source_type_oid: 23,
                        result_type_oid: 20,
                        result_typmod: -1,
                        result_collation_oid: 0,
                        nullable: false,
                    },
                ],
            },
        )
    }

    #[test]
    fn descriptor_logical_and_output_summaries_are_stable() {
        let (spec, projection) = descriptor_contract();
        let summary = descriptor_explain_summary(&spec, &projection);
        assert_eq!(summary.strategy, "descriptor_grouped_aggregate");
        assert_eq!(
            summary.group_keys,
            "k0:dimension[0](20.3:type=25) type=25 collation=100 encoding=hash"
        );
        assert_eq!(
            summary.aggregates,
            "m0:column(10.4:type=23) -> value.sum; value.count"
        );
        assert_eq!(
            summary.filter,
            "fact=ranges(input=10.5:type=23, count=1); measures=[m0=none]; dimensions=[d0=sql_mask(input=20.4:type=16)]"
        );
        assert_eq!(
            summary.star_dimensions,
            "d0:rel=20 join=10.2:type=23<->20.1:type=23 collation=0 multiplicity=counted filter=sql_mask(input=20.4:type=16)"
        );
        assert_eq!(
            summary.output,
            "slot0:group[k0] source_type=25 result_type=25 typmod=-1 collation=100 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false"
        );
    }

    #[test]
    fn h3_hash_count_star_summary_is_explicit() {
        const H3INDEXOID: u32 = 90_001;
        let cell = ColumnRef {
            relation_oid: 10,
            attno: 1,
            type_oid: H3INDEXOID,
        };
        let spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::H3CellToParent {
                    cell,
                    resolution: 7,
                },
                type_oid: H3INDEXOID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let projection = AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: H3INDEXOID,
                    result_type_oid: H3INDEXOID,
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
                    result_type_oid: u32::from(pg_sys::INT8OID),
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            ],
        };

        let summary = descriptor_explain_summary(&spec, &projection);
        assert_eq!(summary.strategy, "descriptor_grouped_aggregate");
        assert_eq!(
            summary.group_keys,
            "k0:h3_cell_to_parent(10.1:type=90001, resolution=7) type=90001 collation=0 encoding=hash"
        );
        assert_eq!(summary.aggregates, "m0:count_star -> value.count");
        assert_eq!(
            summary.filter,
            "fact=none; measures=[m0=none]; dimensions=[none]"
        );
        assert_eq!(summary.star_dimensions, "none");
        assert_eq!(
            summary.output,
            "slot0:group[k0] source_type=90001 result_type=90001 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false"
        );
    }

    #[test]
    fn descriptor_ungrouped_strategy_and_empty_shape_labels_are_stable() {
        let (mut spec, mut projection) = descriptor_contract();
        spec.group_keys.clear();
        spec.star_dims.clear();
        spec.fact_filter = FilterSpec::None;
        projection.slots.remove(0);
        let summary = descriptor_explain_summary(&spec, &projection);
        assert_eq!(summary.strategy, "descriptor_ungrouped_aggregate");
        assert_eq!(summary.group_keys, "none");
        assert_eq!(summary.star_dimensions, "none");
        assert_eq!(
            summary.filter,
            "fact=none; measures=[m0=none]; dimensions=[none]"
        );
    }

    #[test]
    fn descriptor_property_helpers_cover_every_logical_variant() {
        let first = ColumnRef {
            relation_oid: 10,
            attno: 1,
            type_oid: 23,
        };
        let second = ColumnRef {
            relation_oid: 10,
            attno: 2,
            type_oid: 701,
        };
        assert_eq!(format_or_none(std::iter::empty()), "none");
        assert_eq!(
            format_or_none(["a".to_owned(), "b".to_owned()].into_iter()),
            "a; b"
        );
        assert_eq!(
            group_key_source_summary(&GroupKeySource::FactColumn(first)),
            "fact(10.1:type=23)"
        );
        assert_eq!(
            group_key_source_summary(&GroupKeySource::Expression {
                inputs: vec![first, second],
                program: vec![1, 2, 3],
            }),
            "expression(inputs=2, words=3)"
        );
        assert!(
            group_key_source_summary(&GroupKeySource::H3LatLngToCell {
                latitude: first,
                longitude: second,
                resolution: 9,
            })
            .contains("resolution=9")
        );

        assert_eq!(
            group_key_encoding_summary(GroupKeyEncoding::DenseI32 {
                code_min: -2,
                cardinality: 7,
                null_code: None,
            }),
            "dense_i32(min=-2, cardinality=7, null=none)"
        );
        assert_eq!(
            group_key_encoding_summary(GroupKeyEncoding::DictionaryI32 {
                cardinality: 8,
                null_code: Some(7),
            }),
            "dictionary_i32(cardinality=8, null=7)"
        );

        for (expression, expected) in [
            (
                MeasureExpr::Binary {
                    op: BinaryMeasureOp::Mul,
                    lhs: first,
                    rhs: second,
                },
                "mul(10.1:type=23, 10.2:type=701)",
            ),
            (
                MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: first,
                    rhs: second,
                },
                "sub(10.1:type=23, 10.2:type=701)",
            ),
            (
                MeasureExpr::StatsPair {
                    value: first,
                    rhs: second,
                },
                "stats_pair(10.1:type=23, 10.2:type=701)",
            ),
            (
                MeasureExpr::Bytecode {
                    inputs: vec![first],
                    program: vec![4, 5],
                    result_type_oid: 20,
                },
                "bytecode(inputs=1, words=2, result_type=20)",
            ),
        ] {
            assert_eq!(measure_expr_summary(&expression), expected);
        }

        assert_eq!(aggregate_source_label(AggregateSource::Value), "value");
        assert_eq!(aggregate_source_label(AggregateSource::Rhs), "rhs");
        for (kind, label) in [
            (AggregateKind::Sum, "sum"),
            (AggregateKind::Count, "count"),
            (AggregateKind::Min, "min"),
            (AggregateKind::Max, "max"),
            (AggregateKind::Avg, "avg"),
            (AggregateKind::StddevSamp, "stddev_samp"),
        ] {
            assert_eq!(aggregate_kind_label(kind), label);
        }

        assert_eq!(
            filter_summary(&FilterSpec::Bytecode {
                inputs: vec![first, second],
                program: vec![1],
            }),
            "bytecode(inputs=2, words=1)"
        );
        assert_eq!(
            filter_summary(&FilterSpec::Mask {
                input: first,
                kind: MaskKind::Recheck,
            }),
            "recheck_mask(input=10.1:type=23)"
        );
        let geometry = SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: -1,
            srid: None,
        };
        let geography = SpatialValueMetadata {
            kind: SpatialValueKind::Geography,
            typmod: 7,
            srid: Some(4_326),
        };
        assert_eq!(
            spatial_metadata_summary(geometry),
            "kind=geometry, typmod=-1, srid=dynamic"
        );
        assert_eq!(
            spatial_metadata_summary(geography),
            "kind=geography, typmod=7, srid=4326"
        );
        assert!(
            spatial_operand_summary(&SpatialOperand::Column {
                column: first,
                metadata: geometry,
            })
            .starts_with("column(")
        );
        assert!(
            spatial_operand_summary(&SpatialOperand::Constant {
                metadata: geography,
                bytes: vec![1_u8, 2].into_boxed_slice(),
            })
            .contains("bytes=2")
        );

        for (predicate, label) in [
            (SpatialPredicateKind::Intersects, "intersects"),
            (SpatialPredicateKind::Contains, "contains"),
            (SpatialPredicateKind::Within, "within"),
            (SpatialPredicateKind::DWithin, "dwithin"),
            (SpatialPredicateKind::Disjoint, "disjoint"),
            (SpatialPredicateKind::Equals, "equals"),
            (SpatialPredicateKind::Touches, "touches"),
            (SpatialPredicateKind::Crosses, "crosses"),
            (SpatialPredicateKind::Overlaps, "overlaps"),
        ] {
            assert_eq!(spatial_predicate_label(predicate), label);
        }
        assert_eq!(mask_kind_label(MaskKind::Sql), "sql");
        assert_eq!(mask_kind_label(MaskKind::Recheck), "recheck");
        assert_eq!(join_multiplicity_label(JoinMultiplicity::Unique), "unique");
        assert_eq!(
            join_multiplicity_label(JoinMultiplicity::Counted),
            "counted"
        );
        assert_eq!(
            artifact_ensure_outcome_label(ArtifactEnsureOutcome::Hit),
            "hit"
        );
        assert_eq!(
            artifact_ensure_outcome_label(ArtifactEnsureOutcome::Built),
            "built"
        );
        assert_eq!(
            artifact_ensure_outcome_label(ArtifactEnsureOutcome::Rebuilt),
            "rebuilt"
        );
    }

    #[test]
    fn descriptor_residency_summary_reports_exact_generations_and_charges() {
        let evidence = [
            ResidentRelationEvidence {
                relid: pg_sys::Oid::from(10u32),
                generation: 7,
                global_generation: 2,
                relfilenode: pg_sys::Oid::from(110u32),
                row_count: 100,
                raw_bytes: 100,
                raw_accounting: crate::engine::residency::ResidentByteAccounting {
                    device_bytes: 100,
                    retained_host_exact_bytes: 0,
                },
                derived_bytes: 30,
                loaded_at_us: 1,
                last_used_us: 2,
                load_ms: 1.25,
            },
            ResidentRelationEvidence {
                relid: pg_sys::Oid::from(20u32),
                generation: 9,
                global_generation: 2,
                relfilenode: pg_sys::Oid::from(220u32),
                row_count: 10,
                raw_bytes: 50,
                raw_accounting: crate::engine::residency::ResidentByteAccounting {
                    device_bytes: 50,
                    retained_host_exact_bytes: 0,
                },
                derived_bytes: 0,
                loaded_at_us: 1,
                last_used_us: 2,
                load_ms: 0.75,
            },
        ];
        let loaded = [pg_sys::Oid::from(10u32)];
        let summary = descriptor_residency_summary(
            Some(&evidence),
            Some(&loaded),
            Some(ArtifactEnsureOutcome::Built),
            Some(25),
        );
        assert_eq!(summary.state, "resident (2 relations; loaded/reloaded=10)");
        assert_eq!(summary.artifact, "built");
        assert_eq!(
            summary.generations,
            "rel=10 generation=7 global=2 relfilenode=110; rel=20 generation=9 global=2 relfilenode=220"
        );
        assert_eq!(summary.bytes, "raw=150 derived=30 artifact=25 total=180");
    }

    #[test]
    fn descriptor_explain_only_residency_is_explicitly_uninitialized() {
        let summary = descriptor_residency_summary(None, None, None, None);
        assert_eq!(summary.state, "not initialized (EXPLAIN ONLY)");
        assert_eq!(summary.artifact, "not initialized");
        assert_eq!(summary.generations, "not inspected");
        assert_eq!(summary.bytes, "not inspected");
    }

    #[test]
    fn every_gpu_strategy_has_explicit_compatibility_boundary() {
        for strategy in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Window,
            GpuStrategy::FunctionScan,
            GpuStrategy::SrfTargetList,
        ] {
            let reason = gpu_resident_boundary_reason(strategy)
                .to_str()
                .expect("reason is utf8");
            assert!(
                !reason.is_empty(),
                "{strategy:?} must have a boundary reason"
            );
        }
    }

    #[test]
    fn explain_dispatch_flag_uses_strategy_specific_state() {
        let mut state = GpuAccelScanState {
            // SAFETY: CustomScanState is a PostgreSQL C aggregate containing
            // pointer/scalar fields; zero is a valid inert test fixture state.
            css: unsafe { std::mem::zeroed() },
            accel: super::super::GpuAccelState {
                strategy: GpuStrategy::Agg as i32,
                exec_method: super::super::PlanExecMethod::Agg as i32,
                batch_size: 1024,
                expected_threads: 1,
                rows_dispatched: 0,
                batches_executed: 0,
                dispatch_time_us: 0,
                parallel_worker_number: -1,
                dsm_flags: 0,
                dsm_state: std::ptr::null_mut(),
                dsm_counters_recorded: false,
                parallel_agg_participants: 0,
                parallel_agg_active_participants: 0,
                parallel_agg_rows_dispatched: 0,
                parallel_agg_batches_executed: 0,
                parallel_agg_dispatch_time_us: 0,
                resident_proof: ResidentProofSnapshot::not_proven(),
                executor: std::ptr::dangling_mut::<u8>().cast(),
                executor_drop: None,
                executor_prepare_reset: None,
            },
            executor_cleanup: pg_sys::MemoryContextCallback::default(),
        };

        // SAFETY: `state` is a live local GpuAccelScanState for the duration
        // of the helper call; its dangling executor is never dereferenced here.
        assert!(!unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Agg, &raw const state) });
        state.accel.batches_executed = 1;
        // SAFETY: the same live local state remains valid and the Agg branch
        // reads only its inline batch counter.
        assert!(unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Agg, &raw const state) });
    }

    #[test]
    fn explain_batch_amortization_helpers_handle_zero_and_nonzero_batches() {
        assert_eq!(rows_per_batch_for_explain(0, 0), 0.0);
        assert_eq!(avg_dispatch_time_per_batch_ms_for_explain(0, 0), 0.0);
        assert_eq!(rows_per_batch_for_explain(1_000, 4), 250.0);
        assert_eq!(avg_dispatch_time_per_batch_ms_for_explain(2_000, 4), 0.5);
    }
}

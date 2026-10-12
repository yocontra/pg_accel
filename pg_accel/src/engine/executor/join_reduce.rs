//! Resident join-column consumer. Row expressions and aggregation stay on the
//! device; only four bounded SUM/COUNT state pairs may cross back to the host.

use pgrx::pg_sys;

use super::bounded::{BoundedDispatchError, capture_postgres_interrupt, run_bounded_dispatch};
use crate::engine::spec::{
    abi,
    join::{MAX_JOIN_COLUMNS, OccurrenceColumn},
    join_reduce::{JoinReductionSpec, MAX_JOIN_REDUCTIONS, MAX_REDUCTION_PROGRAM_WORDS},
};
use crate::gpu::join_project::{
    ProjectedJoinColumn, ResidentJoinProjection, ResidentProjectedColumn,
};
use crate::gpu::{
    ExprDeviceBuffer, GpuError, GpuErrorDomain, GpuOperation, GpuStatusDetail,
    GroupedAggKernelMode, GroupedAggOutcome, GroupedAggOutputStorage, GroupedAggSession,
    GroupedAggStateLane, ResolvedGroupedAggPlan,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JoinReductionState {
    pub sum: Option<i64>,
    pub nonnull_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinReductionOutput {
    pub states: [JoinReductionState; MAX_JOIN_REDUCTIONS],
    pub measure_count: usize,
    pub joined_rows: usize,
    /// Rows for which post-join WHERE is SQL TRUE (before aggregate FILTER).
    pub selected_rows: u64,
    pub projected_columns: usize,
    /// Exact value and NULL-sidecar allocation bytes, excluding join pairs.
    pub projected_bytes: usize,
    pub join_kernel_launches: u64,
    /// Bounded accumulate calls plus finalization; zero for cardinality-only.
    pub aggregate_calls: usize,
}

#[derive(Debug)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged reduction outcome, pending selected SQL consumer.
pub enum JoinReducedOutcome {
    Complete(JoinReductionOutput),
    NeedsCapacity {
        required_rows: u64,
        kernel_launches: u64,
    },
}

pub(super) enum ReductionError {
    Device(GpuError),
    Interrupt(Box<pg_sys::panic::CaughtError>),
}

impl From<GpuError> for ReductionError {
    fn from(error: GpuError) -> Self {
        Self::Device(error)
    }
}

/// Bounded physical gather selection for one consumer, without rewriting its
/// logical slots or cloning scalar programs. Equality includes occurrence ID:
/// two aliases of one physical table can name different joined rows.
pub(super) struct ReductionProjection {
    source_slots: [usize; MAX_JOIN_COLUMNS],
    count: usize,
    consumer_slots: [Option<usize>; MAX_JOIN_COLUMNS],
}

impl ReductionProjection {
    pub(super) fn new(
        schema: &[OccurrenceColumn],
        spec: &JoinReductionSpec,
    ) -> Result<Self, GpuError> {
        spec.validate_columns(schema)
            .map_err(|_| contract_error("invalid reduction projection schema"))?;
        let mut result = Self {
            source_slots: [0; MAX_JOIN_COLUMNS],
            count: 0,
            consumer_slots: [None; MAX_JOIN_COLUMNS],
        };
        for (inputs, _, _) in spec.programs() {
            for &slot in inputs {
                let slot = slot as usize;
                if result.consumer_slots[slot].is_some() {
                    continue;
                }
                let projected = result
                    .source_slots()
                    .iter()
                    .position(|&source| schema[source] == schema[slot]);
                let projected = projected.unwrap_or_else(|| {
                    let index = result.count;
                    result.source_slots[index] = slot;
                    result.count += 1;
                    index
                });
                result.consumer_slots[slot] = Some(projected);
            }
        }
        Ok(result)
    }

    pub(super) fn source_slots(&self) -> &[usize] {
        &self.source_slots[..self.count]
    }

    fn projected_slot(&self, original: usize) -> Result<usize, GpuError> {
        self.consumer_slots
            .get(original)
            .copied()
            .flatten()
            .ok_or_else(|| contract_error("unbound reduction projection slot"))
    }
}

pub(super) fn contract_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::HashJoin,
        GpuOperation::ValidateDeviceInput,
        GpuStatusDetail::ShapeMismatch,
        detail,
    )
}

/// Final scalar publication of a device-produced join cardinality. No input
/// rows, pairs or expression values are read or aggregated on the host.
pub(super) fn cardinality_output(
    spec: &JoinReductionSpec,
    rows: u64,
    kernel_launches: u64,
) -> Result<JoinReductionOutput, GpuError> {
    if !spec.is_cardinality_only() {
        return Err(contract_error(
            "join cardinality cannot replace this reduction",
        ));
    }
    let sum = i64::try_from(rows).map_err(|_| {
        GpuError::with_detail(
            GpuErrorDomain::HashJoin,
            GpuOperation::ValidateDeviceInput,
            GpuStatusDetail::NumericOverflow,
            "join cardinality exceeds PostgreSQL bigint aggregate state",
        )
    })?;
    let joined_rows = usize::try_from(rows)
        .map_err(|_| contract_error("join cardinality exceeds host row metadata"))?;
    let mut states = [JoinReductionState::default(); MAX_JOIN_REDUCTIONS];
    states[..spec.measures.len()].fill(JoinReductionState {
        sum: (rows != 0).then_some(sum),
        nonnull_count: rows,
    });
    Ok(JoinReductionOutput {
        states,
        measure_count: spec.measures.len(),
        joined_rows,
        selected_rows: rows,
        projected_columns: 0,
        projected_bytes: 0,
        join_kernel_launches: kernel_launches,
        aggregate_calls: 0,
    })
}

pub(super) fn validate_chunk_rows(rows: usize) -> Result<(), GpuError> {
    if rows == 0 || rows > crate::engine::cost::GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS {
        return Err(contract_error(
            "join reduction chunk exceeds dense integer proof bound",
        ));
    }
    Ok(())
}

fn final_state(sum: i64, count: u64, selected: u64) -> Result<JoinReductionState, GpuError> {
    if count > selected {
        return Err(contract_error("join reduction count exceeds row domain"));
    }
    Ok(JoinReductionState {
        sum: (count != 0).then_some(sum),
        nonnull_count: count,
    })
}

fn interrupt_boundary() -> Result<(), ReductionError> {
    capture_postgres_interrupt().map_or(Ok(()), |error| {
        Err(ReductionError::Interrupt(Box::new(error)))
    })
}

fn input<T>(
    column: &ResidentProjectedColumn<T>,
    rows: usize,
    physical_type: i32,
) -> Result<abi::PgaccelScalarInput, GpuError> {
    if column.values().map_or(0, ExprDeviceBuffer::len) != rows
        || column.nulls().map_or(0, ExprDeviceBuffer::len) != rows
    {
        return Err(contract_error(
            "join reduction lane has a different row domain",
        ));
    }
    Ok(abi::PgaccelScalarInput {
        values: column
            .values()
            .map_or(std::ptr::null(), |values| values.as_ptr().cast()),
        nulls: column
            .nulls()
            .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
        physical_type,
        reserved: 0,
    })
}

/// Only the owning JQS1 binder calls this after validating spec against its
/// exact producer schema. No arbitrary caller can attach another schema to a
/// projection. Source columns stay borrowed until all native/session owners drop.
pub(super) fn reduce(
    projection: &ResidentJoinProjection,
    schema: &[OccurrenceColumn],
    spec: &JoinReductionSpec,
    selection: &ReductionProjection,
    chunk_rows: usize,
    join_kernel_launches: u64,
) -> Result<JoinReductionOutput, ReductionError> {
    validate_chunk_rows(chunk_rows)?;
    spec.validate_columns(schema)
        .map_err(|_| contract_error("invalid join reduction schema or program"))?;
    interrupt_boundary()?;
    if projection.column_count() != selection.source_slots().len()
        || spec.measures.len() > MAX_JOIN_REDUCTIONS
    {
        return Err(contract_error("join reduction producer schema mismatch").into());
    }
    let zero_input = abi::PgaccelScalarInput {
        values: std::ptr::null(),
        nulls: std::ptr::null(),
        physical_type: 0,
        reserved: 0,
    };
    let mut words = [[0_i32; MAX_REDUCTION_PROGRAM_WORDS]; MAX_JOIN_REDUCTIONS];
    let mut inputs =
        [[zero_input; crate::engine::spec::scalar::MAX_SCALAR_INPUTS]; MAX_JOIN_REDUCTIONS];
    let mut views = [abi::PgaccelScalarView {
        version: abi::PGACCEL_SCALAR_VIEW_VERSION,
        word_count: 0,
        input_count: 0,
        reserved: 0,
        row_count: projection.len() as u64,
        words: std::ptr::null(),
        inputs: std::ptr::null(),
    }; MAX_JOIN_REDUCTIONS];
    // SAFETY: this C POD descriptor has only scalar/pointer fields for which
    // zero is valid. Active fields are initialized below; inactive slots stay canonical.
    let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
    desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
    desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
    desc.row_count = projection.len();
    desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
    desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
    desc.group_capacity = 1;
    desc.measure_count = (spec.measures.len() + usize::from(spec.row_filter.is_some())) as u32;
    desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
    for filter in &mut desc.measure_filters {
        filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
    }
    for (index, (bindings, program, result_type)) in spec.programs().enumerate() {
        let word_count = program.encoded_word_len();
        program
            .encode_into(&mut words[index][..word_count])
            .map_err(|_| contract_error("invalid join reduction scalar serialization"))?;
        for (binding, &slot) in bindings.iter().enumerate() {
            let slot = slot as usize;
            let column = projection
                .column(selection.projected_slot(slot)?)
                .ok_or_else(|| contract_error("missing projected reduction input"))?;
            let logical = schema
                .get(slot)
                .ok_or_else(|| contract_error("missing reduction schema slot"))?;
            inputs[index][binding] = match (logical.type_oid, column) {
                (23, ProjectedJoinColumn::Int32(column)) => input(
                    column,
                    projection.len(),
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                )?,
                (1082, ProjectedJoinColumn::Int32(column)) => input(
                    column,
                    projection.len(),
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
                )?,
                (16, ProjectedJoinColumn::Byte(column)) => input(
                    column,
                    projection.len(),
                    abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
                )?,
                _ => return Err(contract_error("join reduction SQL/physical types differ").into()),
            };
        }
        views[index].word_count = word_count as u32;
        views[index].input_count = bindings.len() as u32;
        views[index].words = words[index].as_ptr();
        views[index].inputs = if bindings.is_empty() {
            std::ptr::null()
        } else {
            inputs[index].as_ptr()
        };
        let lane = &mut desc.measures[index];
        lane.op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
        let is_filter = result_type == crate::engine::spec::scalar::ScalarType::Bool;
        lane.agg_mask = if is_filter {
            abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        } else {
            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        };
        lane.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        lane.state_bytes = 8;
        lane.value.values = std::ptr::from_ref(&views[index]).cast();
        lane.value.element_bytes = size_of::<abi::PgaccelScalarView>() as u32;
        lane.value.physical_type = if is_filter {
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
        } else {
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
        };
        lane.value.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1;
    }
    if spec.row_filter.is_some() {
        desc.where_filter.predicate_source = abi::PGACCEL_GROUPED_AGG_PRED_SOURCE_SCALAR;
        desc.where_filter.predicate_measure_slot = spec.measures.len() as i32;
    }
    // SAFETY: the exact producer owns every initialized device lane for this
    // call; SQL types, slots and row lengths have been checked. The fixed host
    // metadata arrays stay live while from_abi takes independently charged
    // snapshots. Neither the plan nor its session can escape this source borrow.
    let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }?;
    let mut session = GroupedAggSession::start(&plan, chunk_rows)?;
    if !projection.is_empty() && session.kernel_mode() != GroupedAggKernelMode::ParallelDenseInteger
    {
        return Err(contract_error(
            "join reduction did not select the parallel dense integer kernel",
        )
        .into());
    }
    let mut output = GroupedAggOutputStorage::new(&plan)?;
    let calls = run_bounded_dispatch(
        projection.len(),
        chunk_rows,
        |chunk| {
            let chunk = plan.row_chunk(chunk.first_row, chunk.row_count)?;
            session.accumulate(&chunk).map_err(ReductionError::Device)
        },
        interrupt_boundary,
    )
    .map_err(|error| match error {
        BoundedDispatchError::Dispatch(error) | BoundedDispatchError::InterruptBoundary(error) => {
            error
        }
        BoundedDispatchError::ZeroChunkLimit => {
            ReductionError::Device(contract_error("zero reduction chunk limit"))
        }
    })?;
    let final_result = session.finalize(&plan, &mut output);
    interrupt_boundary()?;
    let GroupedAggOutcome::Complete(result) = final_result? else {
        return Err(contract_error("join reduction unexpectedly requires recheck").into());
    };
    if result.group_capacity != 1
        || result.emitted_group_count > 1
        || result.uncertain_count != 0
        || result.selected_count > projection.len() as u64
        || (spec.row_filter.is_none() && result.selected_count != projection.len() as u64)
    {
        return Err(contract_error("invalid join reduction result cardinality").into());
    }
    let mut states = [JoinReductionState::default(); MAX_JOIN_REDUCTIONS];
    for (index, state) in states[..spec.measures.len()].iter_mut().enumerate() {
        let count = output
            .measure_nonnull_count(index)
            .and_then(|counts| counts.first())
            .copied()
            .ok_or_else(|| contract_error("missing join reduction count state"))?;
        let sum = output.measure_i64_at(index, GroupedAggStateLane::Sum, 0)?;
        *state = final_state(sum, count, result.selected_count)?;
    }
    Ok(JoinReductionOutput {
        states,
        measure_count: spec.measures.len(),
        joined_rows: projection.len(),
        selected_rows: result.selected_count,
        projected_columns: projection.column_count(),
        projected_bytes: projection.allocated_bytes(),
        join_kernel_launches,
        aggregate_calls: calls
            .checked_add(1)
            .ok_or_else(|| contract_error("reduction call count overflow"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::spec::join_reduce::{JoinReduction, JoinRowFilter};
    use crate::engine::spec::scalar::{ScalarInstruction as I, ScalarProgram, ScalarType};

    #[test]
    fn device_cardinality_publication_preserves_empty_sum_and_wide_counts() {
        let spec = JoinReductionSpec {
            measures: vec![
                JoinReduction {
                    inputs: vec![],
                    program: ScalarProgram::new(vec![], ScalarType::Int4, vec![I::Int4(1)])
                        .expect("one"),
                };
                2
            ],
            row_filter: None,
        };
        for rows in [0, 1, u64::from(u32::MAX) + 1, i64::MAX as u64] {
            let result = cardinality_output(&spec, rows, 29);
            if usize::try_from(rows).is_err() {
                assert!(result.is_err());
                continue;
            }
            let result = result.expect("representable PostgreSQL count");
            assert_eq!(
                result.states[0],
                JoinReductionState {
                    sum: (rows != 0).then_some(rows as i64),
                    nonnull_count: rows
                }
            );
            assert_eq!(result.states[1], result.states[0]);
            assert_eq!(result.states[2..], [JoinReductionState::default(); 2]);
            assert_eq!(result.measure_count, 2);
            assert_eq!(result.joined_rows as u64, rows);
            assert_eq!(result.selected_rows, rows);
            assert_eq!(result.join_kernel_launches, 29);
            assert_eq!(result.aggregate_calls, 0);
            assert_eq!(result.projected_columns, 0);
            assert_eq!(result.projected_bytes, 0);
        }
        for rows in [i64::MAX as u64 + 1, u64::MAX] {
            assert_eq!(
                cardinality_output(&spec, rows, 0)
                    .expect_err("bigint overflow")
                    .status,
                GpuStatusDetail::NumericOverflow
            );
        }
        let mut invalid = spec;
        invalid.measures.clear();
        assert!(cardinality_output(&invalid, 17, 0).is_err());
    }

    #[test]
    fn reduction_gathers_only_used_lanes_deduplicating_columns_but_not_aliases() {
        use ScalarType::{Bool, Date, Int4};
        let column = |occurrence, attno, type_oid| OccurrenceColumn {
            occurrence,
            attno,
            type_oid,
        };
        let schema = [
            column(1, 1, 23),
            column(2, 1, 23),
            column(1, 2, 16),
            column(1, 1, 23),
            column(2, 3, 1082),
            column(1, 3, 1082),
        ];
        let spec = JoinReductionSpec {
            measures: vec![JoinReduction {
                inputs: vec![3, 0, 1, 3],
                program: ScalarProgram::new(
                    vec![Int4; 4],
                    Int4,
                    vec![
                        I::Input(0),
                        I::Input(1),
                        I::Add,
                        I::Input(2),
                        I::Add,
                        I::Input(3),
                        I::Add,
                    ],
                )
                .expect("sum aliases"),
            }],
            row_filter: Some(JoinRowFilter {
                inputs: vec![2, 4],
                program: ScalarProgram::new(
                    vec![Bool, Date],
                    Bool,
                    vec![I::Input(0), I::Input(1), I::Date(0), I::Gt, I::And],
                )
                .expect("WHERE"),
            }),
        };
        let selected = ReductionProjection::new(&schema, &spec).expect("selection");
        assert_eq!(selected.source_slots(), &[3, 1, 2, 4]);
        for (original, projected) in [(0, 0), (1, 1), (2, 2), (3, 0), (4, 3)] {
            assert_eq!(
                selected.projected_slot(original).expect("used lane"),
                projected
            );
        }
        assert!(selected.projected_slot(5).is_err());
        assert!(selected.projected_slot(usize::MAX).is_err());
        let mut wrong_type = spec;
        wrong_type.row_filter.as_mut().expect("filter").inputs[0] = 0;
        assert!(ReductionProjection::new(&schema, &wrong_type).is_err());
    }

    #[test]
    fn constant_reduction_has_no_gather_and_maximum_bindings_remain_bounded() {
        use ScalarType::{Bool, Int4};
        let schema = std::array::from_fn::<_, MAX_JOIN_COLUMNS, _>(|index| OccurrenceColumn {
            occurrence: 1,
            attno: index as i32 + 1,
            type_oid: 23,
        });
        let mut spec = JoinReductionSpec {
            measures: vec![JoinReduction {
                inputs: vec![],
                program: ScalarProgram::new(vec![], Int4, vec![I::Int4(1)]).expect("constant"),
            }],
            row_filter: Some(JoinRowFilter {
                inputs: vec![],
                program: ScalarProgram::new(vec![], Bool, vec![I::Bool(true)])
                    .expect("constant WHERE"),
            }),
        };
        assert!(
            ReductionProjection::new(&schema, &spec)
                .expect("constants")
                .source_slots()
                .is_empty()
        );
        let slots = (0..crate::engine::spec::scalar::MAX_SCALAR_INPUTS)
            .map(|index| (index % MAX_JOIN_COLUMNS) as u32)
            .collect::<Vec<_>>();
        spec.measures[0] = JoinReduction {
            inputs: slots,
            // Declared but unused SPG1 inputs still have exact bound slots.
            program: ScalarProgram::new(
                vec![Int4; crate::engine::spec::scalar::MAX_SCALAR_INPUTS],
                Int4,
                vec![I::Input(0)],
            )
            .expect("max inputs"),
        };
        let selected = ReductionProjection::new(&schema, &spec).expect("all columns");
        assert_eq!(
            selected.source_slots(),
            (0..MAX_JOIN_COLUMNS).collect::<Vec<_>>()
        );
        spec.measures[0].inputs[0] = MAX_JOIN_COLUMNS as u32;
        assert!(ReductionProjection::new(&schema, &spec).is_err());
    }

    #[test]
    fn join_reduction_respects_existing_dense_integer_proof_bound() {
        let max = crate::engine::cost::GPU_GROUPED_AGG_ONE_SHOT_ABSOLUTE_MAX_ROWS;
        for rows in [1, 2, max] {
            assert!(validate_chunk_rows(rows).is_ok());
        }
        for rows in [0, max + 1, usize::MAX] {
            assert!(validate_chunk_rows(rows).is_err());
        }
    }

    #[test]
    fn final_sum_preserves_null_instead_of_fabricating_zero() {
        assert_eq!(
            final_state(0, 0, 0).expect("empty"),
            JoinReductionState {
                sum: None,
                nonnull_count: 0
            }
        );
        assert_eq!(
            final_state(0, 0, 7).expect("all NULL"),
            JoinReductionState {
                sum: None,
                nonnull_count: 0
            }
        );
        assert_eq!(
            final_state(0, 2, 7).expect("non-NULL zero"),
            JoinReductionState {
                sum: Some(0),
                nonnull_count: 2
            }
        );
        assert_eq!(
            final_state(i64::MIN, 7, 7).expect("signed sum").sum,
            Some(i64::MIN)
        );
        assert!(final_state(9, 8, 7).is_err());
    }
}

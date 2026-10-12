//! Occurrence-aware raw residency binding for the staged JQS1 join producer.
//! No PostgreSQL path/executor registration yet. In particular, callers must
//! prove SQL permissions/shape and bound synchronous work before dispatch.

use pgrx::pg_sys;

use super::bounded::{
    InterruptibleAttempt, InterruptibleDispatchError, capture_postgres_interrupt,
    postgres_interrupt_pending, run_interruptible_dispatch,
};

use crate::engine::residency::{
    ResidentColumnRef, ResidentColumnView, ResidentDependencyStamp, ResidentDispatchBundle,
    ResidentLoadError, SelectedRelation, ensure_selected_relations, with_resident_dispatch_inputs,
};
use crate::engine::spec::join::{
    JoinKind, JoinQuerySpec, MAX_JOIN_COLUMNS, MAX_JOIN_KEYS, OccurrenceColumn,
};
use crate::gpu::join_project::{
    ControlledProjectionOutcome, JoinProjectionColumn, JoinProjectionInput, JoinSide,
    ResidentJoinProjection,
};
use crate::gpu::join_rows::{
    ControlledJoinCountOutcome, ControlledJoinOutcome, JoinDispatchLimits, ResidentJoin,
    ResidentJoinKey, ResidentJoinKind, ResidentJoinOutcome,
};
use crate::gpu::{GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail};

fn invalid(detail: impl Into<String>) -> ResidentLoadError {
    ResidentLoadError::InvalidArtifactDependencies(detail.into())
}

#[derive(Debug)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged executor error; no planner selection yet.
pub enum JoinExecutionError {
    Residency(ResidentLoadError),
    Device(GpuError),
}

impl std::fmt::Display for JoinExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Residency(error) => write!(formatter, "join residency: {error}"),
            Self::Device(error) => write!(formatter, "join dispatch: {error}"),
        }
    }
}

impl std::error::Error for JoinExecutionError {}

/// Query-local logical metadata and generation evidence, never device pointers.
/// A stale instance fails closed; explicit preparation of a new instance is
/// required after a generation change, eviction or relation replacement.
/// A same-generation refresh is safe: buffers are resolved anew on every borrow.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged pipeline binder, not a selected executor.
pub struct ResidentJoinInputs {
    spec: JoinQuerySpec,
    columns: Vec<ResidentColumnRef>,
    dependencies: Vec<ResidentDependencyStamp>,
    _backend_local: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Live-tested, pending SQL planner/lifecycle wiring.
impl ResidentJoinInputs {
    /// Compose the occurrence-aware producer with a versioned global scalar
    /// aggregate consumer. Joined rows remain resident through reduction;
    /// only bounded final states are materialized. No SQL planner admission.
    /// Capacity bounds allocated pairs, not cardinality-only aggregate input.
    pub fn execute_reduced(
        &self,
        spec: &crate::engine::spec::join_reduce::JoinReductionSpec,
        capacity: usize,
        join_limits: JoinDispatchLimits,
        reduction_chunk_rows: usize,
    ) -> Result<super::join_reduce::JoinReducedOutcome, JoinExecutionError> {
        spec.validate(&self.spec)
            .map_err(|error| JoinExecutionError::Residency(invalid(error.to_string())))?;
        super::join_reduce::validate_chunk_rows(reduction_chunk_rows)
            .map_err(JoinExecutionError::Device)?;
        if spec.is_cardinality_only() {
            let (rows, kernel_launches) = self.execute_cardinality(join_limits)?;
            return super::join_reduce::cardinality_output(spec, rows, kernel_launches)
                .map(super::join_reduce::JoinReducedOutcome::Complete)
                .map_err(JoinExecutionError::Device);
        }
        let selection = super::join_reduce::ReductionProjection::new(&self.spec.columns, spec)
            .map_err(JoinExecutionError::Device)?;
        let (columns, kernel_launches) = match self.execute_with_interrupt_poll(
            capacity,
            join_limits,
            Some(selection.source_slots()),
            postgres_interrupt_pending,
        )? {
            JoinStageOutcome::NeedsCapacity {
                required_rows,
                kernel_launches,
            } => {
                return Ok(super::join_reduce::JoinReducedOutcome::NeedsCapacity {
                    required_rows,
                    kernel_launches,
                });
            }
            JoinStageOutcome::Complete {
                columns,
                kernel_launches,
            } => (columns, kernel_launches),
        };
        let reduced = super::join_reduce::reduce(
            &columns,
            &self.spec.columns,
            spec,
            &selection,
            reduction_chunk_rows,
            kernel_launches,
        );
        // Captured PG errors must not rethrow across the owner of joined rows.
        // The consumer has already dropped plan/session/scratch before return.
        drop(columns);
        match reduced {
            Ok(output) => Ok(super::join_reduce::JoinReducedOutcome::Complete(output)),
            Err(super::join_reduce::ReductionError::Device(error)) => {
                // A failed allocation/dispatch may have raced with query
                // cancellation. Preserve PostgreSQL's error after every
                // consumer owner and the joined batch have been released.
                if let Some(interrupt) = capture_postgres_interrupt() {
                    interrupt.rethrow();
                }
                Err(JoinExecutionError::Device(error))
            }
            Err(super::join_reduce::ReductionError::Interrupt(error)) => (*error).rethrow(),
        }
    }

    fn execute_cardinality(
        &self,
        limits: JoinDispatchLimits,
    ) -> Result<(u64, u64), JoinExecutionError> {
        let mut previous_launches = 0;
        let result = run_interruptible_dispatch(
            |pending| {
                let outcome = self
                    .with_bound(|bound| bound.join()?.count_controlled(limits, pending))
                    .map_err(JoinExecutionError::Residency)?
                    .map_err(JoinExecutionError::Device)?;
                match outcome {
                    ControlledJoinCountOutcome::Interrupted {
                        kernel_launches, ..
                    } => {
                        previous_launches = progress_sum(previous_launches, kernel_launches)
                            .map_err(JoinExecutionError::Device)?;
                        Ok(InterruptibleAttempt::Interrupted)
                    }
                    ControlledJoinCountOutcome::Complete {
                        rows,
                        kernel_launches,
                    } => {
                        let launches = progress_sum(previous_launches, kernel_launches)
                            .map_err(JoinExecutionError::Device)?;
                        Ok(InterruptibleAttempt::Complete((rows, launches)))
                    }
                }
            },
            postgres_interrupt_pending,
            || capture_postgres_interrupt().map_or(Ok(()), |error| Err(Box::new(error))),
        );
        match result {
            Ok(output) => Ok(output),
            Err(InterruptibleDispatchError::Dispatch(error)) => Err(error),
            Err(InterruptibleDispatchError::Interrupt(error)) => (*error).rethrow(),
        }
    }

    /// PostgreSQL-aware bounded entry point. SQL permissions, shape and limit
    /// admission are still the caller's responsibility. Service interrupts only
    /// outside native frames and store borrows; preserve PostgreSQL SQLSTATE and
    /// error text. A benign interrupt restarts the GPU attempt after rebinding,
    /// never falls back to CPU joins or consumes an incomplete device output.
    pub fn execute_interruptible(
        &self,
        capacity: usize,
        limits: JoinDispatchLimits,
    ) -> Result<JoinStageOutcome, JoinExecutionError> {
        self.execute_with_interrupt_poll(capacity, limits, None, postgres_interrupt_pending)
    }

    #[cfg(feature = "pg_test")]
    pub(crate) fn execute_interruptible_test_hook(
        &self,
        capacity: usize,
        limits: JoinDispatchLimits,
        mut on_poll: impl FnMut(),
    ) -> Result<JoinStageOutcome, JoinExecutionError> {
        // Tests may set signal flags here, but must obey the native callback's
        // no-PostgreSQL/no-longjmp contract. Production uses no test hook.
        self.execute_with_interrupt_poll(capacity, limits, None, || {
            on_poll();
            postgres_interrupt_pending()
        })
    }

    fn execute_with_interrupt_poll(
        &self,
        capacity: usize,
        limits: JoinDispatchLimits,
        projection_slots: Option<&[usize]>,
        pending: impl FnMut() -> bool,
    ) -> Result<JoinStageOutcome, JoinExecutionError> {
        let mut previous_launches = 0;
        let result = run_interruptible_dispatch(
            |pending| {
                let outcome = self
                    .with_bound(|mut bound| {
                        if let Some(slots) = projection_slots {
                            bound.select_projection(slots)?;
                        }
                        bound.execute_controlled(capacity, limits, pending)
                    })
                    .map_err(JoinExecutionError::Residency)?
                    .map_err(JoinExecutionError::Device)?;
                match outcome {
                    ControlledJoinStageOutcome::Interrupted {
                        kernel_launches, ..
                    } => {
                        previous_launches = progress_sum(previous_launches, kernel_launches)
                            .map_err(JoinExecutionError::Device)?;
                        Ok(InterruptibleAttempt::Interrupted)
                    }
                    ControlledJoinStageOutcome::Finished { mut outcome, .. } => {
                        let (JoinStageOutcome::Complete {
                            kernel_launches, ..
                        }
                        | JoinStageOutcome::NeedsCapacity {
                            kernel_launches, ..
                        }) = &mut outcome;
                        *kernel_launches = progress_sum(previous_launches, *kernel_launches)
                            .map_err(JoinExecutionError::Device)?;
                        Ok(InterruptibleAttempt::Complete(outcome))
                    }
                }
            },
            pending,
            || capture_postgres_interrupt().map_or(Ok(()), |error| Err(Box::new(error))),
        );
        match result {
            Ok(output) => Ok(output),
            Err(InterruptibleDispatchError::Dispatch(error)) => Err(error),
            Err(InterruptibleDispatchError::Interrupt(error)) => (*error).rethrow(),
        }
    }

    pub fn prepare(spec: JoinQuerySpec) -> Result<Self, ResidentLoadError> {
        let physical = spec
            .physical_columns()
            .map_err(|error| invalid(error.to_string()))?;
        let mut requests: Vec<SelectedRelation> = Vec::new();
        let mut columns = Vec::with_capacity(physical.len());
        for column in physical {
            let relid = pg_sys::Oid::from(column.relation_oid);
            let attno =
                i16::try_from(column.attno).map_err(|_| invalid("join attribute overflow"))?;
            if requests.last().is_none_or(|request| request.relid != relid) {
                requests.push(SelectedRelation {
                    relid,
                    columns: Vec::new(),
                });
            }
            if let Some(request) = requests.last_mut() {
                request.columns.push(attno);
            }
            columns.push(ResidentColumnRef { relid, attno });
        }
        let ensured = ensure_selected_relations(&requests)?;
        let dependencies = ensured
            .evidence
            .iter()
            .map(|evidence| ResidentDependencyStamp {
                relid: evidence.relid,
                generation: evidence.generation,
                global_generation: evidence.global_generation,
                relfilenode: evidence.relfilenode,
            })
            .collect();
        let result = Self {
            spec,
            columns,
            dependencies,
            _backend_local: std::marker::PhantomData,
        };
        result.with_bound(|_| ())?;
        Ok(result)
    }

    /// Revalidate and bind under a single immutable store borrow. The HRTB
    /// callback cannot return a borrowed source owner. Allocation of bounded
    /// metadata happens before dispatch; source rows are never read on CPU.
    pub fn with_bound<R>(
        &self,
        callback: impl FnOnce(BoundJoinInputs<'_>) -> R,
    ) -> Result<R, ResidentLoadError> {
        with_resident_dispatch_inputs(&self.columns, &self.dependencies, |inputs| {
            Ok(callback(bind(&self.spec, &inputs)?))
        })?
    }
}

/// Stack-only typed borrows, independently oriented even for the same table.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Does not escape the residency callback.
pub struct BoundJoinInputs<'a> {
    kind: ResidentJoinKind,
    outer: [ResidentJoinKey<'a>; MAX_JOIN_KEYS],
    inner: [ResidentJoinKey<'a>; MAX_JOIN_KEYS],
    key_count: usize,
    projection: [JoinProjectionInput<'a>; MAX_JOIN_COLUMNS],
    column_count: usize,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Native composition caller; production admission remains closed.
impl BoundJoinInputs<'_> {
    fn select_projection(&mut self, slots: &[usize]) -> GpuResult<()> {
        if slots.len() > MAX_JOIN_COLUMNS || slots.iter().any(|&slot| slot >= self.column_count) {
            return Err(super::join_reduce::contract_error(
                "invalid physical gather selection",
            ));
        }
        let original = self.projection;
        for (index, &slot) in slots.iter().enumerate() {
            self.projection[index] = original[slot];
        }
        self.column_count = slots.len();
        Ok(())
    }

    fn join(&self) -> GpuResult<ResidentJoin<'_>> {
        ResidentJoin::new(
            self.kind,
            &self.outer[..self.key_count],
            &self.inner[..self.key_count],
        )
    }

    /// Execute this exact producer and its own projection together. An output
    /// from a different join cannot accidentally be gathered against our inputs.
    /// Capacity limits allocation/output, not probe work: the caller must bound
    /// synchronous work independently before entering this method.
    pub fn execute(&self, capacity: usize) -> GpuResult<JoinStageOutcome> {
        match self.join()?.execute(capacity)? {
            ResidentJoinOutcome::Complete(output) => {
                let columns = output.project(&self.projection[..self.column_count])?;
                let kernel_launches = output.kernel_launches() + columns.kernel_launches();
                Ok(JoinStageOutcome::Complete {
                    columns,
                    kernel_launches,
                })
            }
            ResidentJoinOutcome::NeedsCapacity {
                required_rows,
                kernel_launches,
            } => Ok(JoinStageOutcome::NeedsCapacity {
                required_rows,
                kernel_launches,
            }),
        }
    }

    /// Execute this producer and gather with bounded launches in both stages.
    /// The callback must only inspect host cancellation state: no PostgreSQL
    /// APIs, longjmp or resident-store mutation under this immutable borrow.
    /// Callers deliver PostgreSQL interrupts after with_bound has returned.
    pub fn execute_controlled(
        &self,
        capacity: usize,
        limits: JoinDispatchLimits,
        check: &mut dyn FnMut() -> bool,
    ) -> GpuResult<ControlledJoinStageOutcome> {
        let output = match self.join()?.execute_controlled(capacity, limits, check)? {
            ControlledJoinOutcome::Interrupted {
                kernel_launches,
                cancel_checks,
            } => {
                return Ok(ControlledJoinStageOutcome::Interrupted {
                    kernel_launches,
                    cancel_checks,
                });
            }
            ControlledJoinOutcome::Finished(ResidentJoinOutcome::NeedsCapacity {
                required_rows,
                kernel_launches,
            }) => {
                return Ok(ControlledJoinStageOutcome::Finished {
                    outcome: JoinStageOutcome::NeedsCapacity {
                        required_rows,
                        kernel_launches,
                    },
                    cancel_checks: progress_sum(kernel_launches, 1)?,
                });
            }
            ControlledJoinOutcome::Finished(ResidentJoinOutcome::Complete(output)) => output,
        };
        let producer_launches = output.kernel_launches();
        let projected = if self.column_count == 0 {
            // Constant programs need the verified joined row domain, not a
            // dummy column. Preserve the stage-entry cancellation boundary.
            output.project_row_domain_controlled(check)
        } else {
            output.project_controlled(&self.projection[..self.column_count], limits, check)?
        };
        match projected {
            ControlledProjectionOutcome::Complete(columns) => {
                let kernel_launches = progress_sum(producer_launches, columns.kernel_launches())?;
                Ok(ControlledJoinStageOutcome::Finished {
                    outcome: JoinStageOutcome::Complete {
                        columns,
                        kernel_launches,
                    },
                    // Each native stage also polls once before its first launch.
                    cancel_checks: progress_sum(kernel_launches, 2)?,
                })
            }
            ControlledProjectionOutcome::Interrupted {
                kernel_launches,
                cancel_checks,
            } => Ok(ControlledJoinStageOutcome::Interrupted {
                kernel_launches: progress_sum(producer_launches, kernel_launches)?,
                cancel_checks: progress_sum(progress_sum(producer_launches, 1)?, cancel_checks)?,
            }),
        }
    }
}

fn progress_sum(left: u64, right: u64) -> GpuResult<u64> {
    left.checked_add(right).ok_or_else(|| {
        GpuError::with_detail(
            GpuErrorDomain::HashJoin,
            GpuOperation::ValidateDeviceInput,
            GpuStatusDetail::CapacityOverflow,
            "join-stage progress count overflow",
        )
    })
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged bounded pipeline, pending normal planner/lifecycle wiring.
#[allow(clippy::large_enum_variant)] // Fixed-capacity inline consumer metadata; no heap allocation under the borrow.
pub enum ControlledJoinStageOutcome {
    Finished {
        outcome: JoinStageOutcome,
        cancel_checks: u64,
    },
    Interrupted {
        kernel_launches: u64,
        cancel_checks: u64,
    },
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Resident consumer owner, pending selected pipeline.
#[allow(clippy::large_enum_variant)] // Fixed-size inline metadata avoids a heap allocation under the resident borrow.
pub enum JoinStageOutcome {
    Complete {
        columns: ResidentJoinProjection,
        kernel_launches: u64,
    },
    NeedsCapacity {
        required_rows: u64,
        kernel_launches: u64,
    },
}

fn view<'a>(
    spec: &JoinQuerySpec,
    inputs: &'a ResidentDispatchBundle<'_>,
    column: OccurrenceColumn,
) -> Result<ResidentColumnView<'a>, ResidentLoadError> {
    let relation = spec
        .relation(column.occurrence)
        .map_err(|error| invalid(error.to_string()))?;
    let relid = pg_sys::Oid::from(relation.relation_oid);
    let attno = i16::try_from(column.attno).map_err(|_| invalid("join attribute overflow"))?;
    let view = inputs.find_column(ResidentColumnRef { relid, attno })?;
    if u32::from(view.type_oid()) != column.type_oid
        || view.len() as u64 != inputs.evidence_for(relid)?.row_count
    {
        return Err(invalid("join resident column type or row domain changed"));
    }
    Ok(view)
}

fn project_column(
    view: ResidentColumnView<'_>,
) -> Result<JoinProjectionColumn<'_>, ResidentLoadError> {
    let nulls = match &view {
        ResidentColumnView::I32 { nulls, .. }
        | ResidentColumnView::I64 { nulls, .. }
        | ResidentColumnView::Bool { nulls, .. } => *nulls,
        _ => None,
    };
    if nulls.is_some_and(|nulls| nulls.len() != view.len()) {
        return Err(invalid(
            "join resident NULL sidecar has the wrong row domain",
        ));
    }
    match view {
        ResidentColumnView::I32 {
            type_oid,
            values,
            nulls,
        } if matches!(u32::from(type_oid), 23 | 1082) => Ok(JoinProjectionColumn::Int32 {
            values: Some(values),
            nulls,
        }),
        ResidentColumnView::I64 {
            type_oid,
            values,
            nulls,
        } if u32::from(type_oid) == 20 => Ok(JoinProjectionColumn::Int64 {
            values: Some(values),
            nulls,
        }),
        ResidentColumnView::Bool {
            type_oid,
            values,
            nulls,
        } if u32::from(type_oid) == 16 => Ok(JoinProjectionColumn::Byte {
            values: Some(values),
            nulls,
        }),
        ResidentColumnView::Empty { type_oid } => match u32::from(type_oid) {
            23 | 1082 => Ok(JoinProjectionColumn::Int32 {
                values: None,
                nulls: None,
            }),
            20 => Ok(JoinProjectionColumn::Int64 {
                values: None,
                nulls: None,
            }),
            16 => Ok(JoinProjectionColumn::Byte {
                values: None,
                nulls: None,
            }),
            _ => Err(invalid("unsupported empty join column type")),
        },
        _ => Err(invalid("unsupported resident join storage")),
    }
}

fn key(view: ResidentColumnView<'_>) -> Result<ResidentJoinKey<'_>, ResidentLoadError> {
    match project_column(view)? {
        JoinProjectionColumn::Int32 { values, nulls } => {
            Ok(ResidentJoinKey::Int32 { values, nulls })
        }
        JoinProjectionColumn::Int64 { values, nulls } => {
            Ok(ResidentJoinKey::Int64 { values, nulls })
        }
        JoinProjectionColumn::Byte { .. } => Err(invalid("noninteger resident join key")),
    }
}

fn bind<'a>(
    spec: &JoinQuerySpec,
    inputs: &'a ResidentDispatchBundle<'_>,
) -> Result<BoundJoinInputs<'a>, ResidentLoadError> {
    let empty = ResidentJoinKey::Int32 {
        values: None,
        nulls: None,
    };
    let mut bound = BoundJoinInputs {
        kind: match spec.kind {
            JoinKind::Inner => ResidentJoinKind::Inner,
            JoinKind::Left => ResidentJoinKind::Left,
            JoinKind::Semi => ResidentJoinKind::Semi,
            JoinKind::Anti => ResidentJoinKind::Anti,
        },
        outer: [empty; MAX_JOIN_KEYS],
        inner: [empty; MAX_JOIN_KEYS],
        key_count: spec.keys.len(),
        projection: [JoinProjectionInput {
            side: JoinSide::Outer,
            column: JoinProjectionColumn::Byte {
                values: None,
                nulls: None,
            },
        }; MAX_JOIN_COLUMNS],
        column_count: spec.columns.len(),
    };
    for (index, pair) in spec.keys.iter().enumerate() {
        bound.outer[index] = key(view(spec, inputs, pair.outer)?)?;
        bound.inner[index] = key(view(spec, inputs, pair.inner)?)?;
    }
    for (index, column) in spec.columns.iter().enumerate() {
        bound.projection[index] = JoinProjectionInput {
            side: if column.occurrence == spec.outer.id {
                JoinSide::Outer
            } else {
                JoinSide::Inner
            },
            column: project_column(view(spec, inputs, *column)?)?,
        };
    }
    // Includes exact NULL-sidecar lengths and native row-domain limits. It
    // reads only buffer metadata and does not launch or allocate on the GPU.
    bound.join().map_err(ResidentLoadError::Gpu)?;
    Ok(bound)
}

#[cfg(test)]
mod tests {
    #[test]
    fn physical_projection_selection_uses_original_slots_and_fails_before_mutation() {
        let empty = ResidentJoinKey::Int32 {
            values: None,
            nulls: None,
        };
        let mut bound = BoundJoinInputs {
            kind: ResidentJoinKind::Inner,
            outer: [empty; MAX_JOIN_KEYS],
            inner: [empty; MAX_JOIN_KEYS],
            key_count: 1,
            projection: [JoinProjectionInput {
                side: JoinSide::Outer,
                column: JoinProjectionColumn::Byte {
                    values: None,
                    nulls: None,
                },
            }; MAX_JOIN_COLUMNS],
            column_count: 3,
        };
        bound.projection[1].column = JoinProjectionColumn::Int32 {
            values: None,
            nulls: None,
        };
        bound.projection[2].column = JoinProjectionColumn::Int64 {
            values: None,
            nulls: None,
        };
        bound.projection[2].side = JoinSide::Inner;
        for slots in [vec![2, 3], vec![usize::MAX], vec![0; MAX_JOIN_COLUMNS + 1]] {
            assert!(bound.select_projection(&slots).is_err());
            assert_eq!(bound.column_count, 3);
            assert!(matches!(
                bound.projection[0].column,
                JoinProjectionColumn::Byte { .. }
            ));
        }
        bound
            .select_projection(&[2, 0])
            .expect("reorder without overwriting later source");
        assert_eq!(bound.column_count, 2);
        assert!(matches!(
            bound.projection[0].column,
            JoinProjectionColumn::Int64 { .. }
        ));
        assert!(matches!(bound.projection[0].side, JoinSide::Inner));
        assert!(matches!(
            bound.projection[1].column,
            JoinProjectionColumn::Byte { .. }
        ));
        assert!(matches!(bound.projection[1].side, JoinSide::Outer));
        bound
            .select_projection(&[])
            .expect("constant-only consumer");
        assert_eq!(bound.column_count, 0);
    }

    #[test]
    fn bounded_stage_progress_counts_both_entry_polls_without_overflow() {
        assert_eq!(super::progress_sum(0, 2).expect("empty stages"), 2);
        assert_eq!(super::progress_sum(5, 7).expect("stage progress"), 12);
        assert!(super::progress_sum(u64::MAX, 1).is_err());
    }
    use super::*;
    use crate::engine::spec::join::RelationOccurrence;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(ResidentJoinInputs: Send, Sync);
    assert_not_impl_any!(BoundJoinInputs<'static>: Send, Sync);

    #[test]
    fn invalid_join_spec_never_reaches_resident_loading() {
        let spec = JoinQuerySpec {
            outer: RelationOccurrence {
                id: 1,
                relation_oid: 1,
            },
            inner: RelationOccurrence {
                id: 1,
                relation_oid: 1,
            },
            kind: JoinKind::Inner,
            keys: Vec::new(),
            columns: Vec::new(),
        };
        assert!(matches!(
            ResidentJoinInputs::prepare(spec),
            Err(ResidentLoadError::InvalidArtifactDependencies(_))
        ));
    }

    #[test]
    fn typed_empty_projection_preserves_width_and_rejects_unsupported_types() {
        for (oid, width) in [(16, 1), (20, 8), (23, 4), (1082, 4)] {
            let column = project_column(ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(oid as u32),
            })
            .expect("typed empty");
            let actual = match column {
                JoinProjectionColumn::Int32 { values, nulls } => {
                    assert!(values.is_none() && nulls.is_none());
                    4
                }
                JoinProjectionColumn::Int64 { values, nulls } => {
                    assert!(values.is_none() && nulls.is_none());
                    8
                }
                JoinProjectionColumn::Byte { values, nulls } => {
                    assert!(values.is_none() && nulls.is_none());
                    1
                }
            };
            assert_eq!(actual, width);
        }
        assert!(
            project_column(ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(25_u32)
            })
            .is_err()
        );
        assert!(
            key(ResidentColumnView::Empty {
                type_oid: pg_sys::Oid::from(16_u32)
            })
            .is_err()
        );
    }
}

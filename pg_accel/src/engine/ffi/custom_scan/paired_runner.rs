use super::{
    AggExecState, GpuAccelScanState, abandon_aggregate_transport, close_aggregate_transport,
    install_boxed_executor, pg_sys, register_executor_cleanup, release_executor,
    with_installed_executor_error_boundary,
};

#[cfg(feature = "pg_test")]
pub(super) struct PairedOwnedOutput {
    pub(super) rows: Vec<(Option<i64>, i64)>,
    pub(super) key_type: u32,
    pub(super) mode: Option<crate::gpu::GroupedAggKernelMode>,
    pub(super) calls: u64,
    pub(super) scheduled: u64,
    pub(super) dispatch_us: u64,
    pub(super) artifact_summary: String,
    pub(super) artifact_outcome: Option<crate::engine::residency::ArtifactEnsureOutcome>,
    pub(super) artifact_bytes: Option<u64>,
    pub(super) residency: Vec<crate::engine::residency::ResidentRelationEvidence>,
    pub(super) loaded_relations: Vec<pg_sys::Oid>,
    pub(super) preparation_ns: u128,
    pub(super) consumption_ns: u128,
    pub(super) descriptor_executor_ns: u128,
    pub(super) diagnostic_capture_ns: u128,
    pub(super) completed: bool,
    pub(super) identity: crate::engine::residency::DerivedArtifactIdentity,
}

#[cfg(feature = "pg_test")]
pub(super) unsafe fn paired_run_installed(
    spec: crate::engine::spec::AggQuerySpec,
    projection: crate::engine::spec::AggOutputProjection,
    selection: crate::engine::executor::agg::AggExecutionSelection,
) -> PairedOwnedOutput {
    // SAFETY: the normal diagnostic path never injects failures.
    unsafe { paired_run_with_fault(spec, projection, selection, None) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PairedFault {
    Prepare,
    AfterPrepare,
    AfterDispatch,
    Close,
}

unsafe fn failing_normal_close(executor: *mut std::ffi::c_void) {
    // SAFETY: the real typed close must run while ownership remains installed.
    unsafe { close_aggregate_transport(executor) };
    pgrx::error!("paired injected normal-close failure");
}

pub(super) unsafe fn paired_run_with_fault(
    spec: crate::engine::spec::AggQuerySpec,
    projection: crate::engine::spec::AggOutputProjection,
    selection: crate::engine::executor::agg::AggExecutionSelection,
    fault: Option<PairedFault>,
) -> PairedOwnedOutput {
    let descriptor_started = std::time::Instant::now();
    // These two diagnosed fixtures project one signed integer key and COUNT.
    assert_eq!(projection.slots.len(), 2);
    let key_type = projection.slots[0].result_type_oid;
    assert!(matches!(key_type, 20 | 21 | 23));
    assert_eq!(projection.slots[1].result_type_oid, 20);
    // SAFETY: main backend only. PG owns this EState and its query context.
    let estate = unsafe { pg_sys::CreateExecutorState() };
    // SAFETY: CreateExecutorState returned its initialized query context.
    let context = unsafe { (*estate).es_query_cxt };
    // SAFETY: same zero-initialized extended state allocation as CustomScan;
    // callback storage belongs to the query context, never a stack local.
    let state = unsafe {
        pg_sys::MemoryContextAllocZero(context, std::mem::size_of::<GpuAccelScanState>())
            .cast::<GpuAccelScanState>()
    };
    // SAFETY: callback lifetime matches allocated state/context.
    unsafe { register_executor_cleanup(state, context) };
    // All PG ERROR paths below use the real transport-abort/owner-drop boundary.
    // The outer diagnostic SQL subtransaction MUST retain and rethrow failures.
    let mut output = unsafe {
        with_installed_executor_error_boundary(state, || {
            // SAFETY: switch only within the boundary restoring entry globals on error.
            let previous = pg_sys::MemoryContextSwitchTo(context);
            let descriptor = pg_sys::CreateTemplateTupleDesc(2);
            for (index, field) in projection.slots.iter().enumerate() {
                pg_sys::TupleDescInitEntry(
                    descriptor,
                    (index + 1) as i16,
                    c"value".as_ptr(),
                    field.result_type_oid.into(),
                    -1,
                    0,
                );
            }
            let slot =
                pg_sys::MakeSingleTupleTableSlot(descriptor, &raw const pg_sys::TTSOpsVirtual);
            let exec = AggExecState::new_selected_descriptor_unprepared(
                spec, projection, false, selection,
            )
            .unwrap_or_else(|error| pgrx::error!("paired descriptor construction failed: {error}"));
            (*state).accel.executor_abort = Some(abandon_aggregate_transport);
            (*state).accel.executor_close = Some(if fault == Some(PairedFault::Close) {
                failing_normal_close
            } else {
                close_aggregate_transport
            });
            install_boxed_executor(
                &mut (*state).accel.executor,
                &mut (*state).accel.executor_drop,
                &mut (*state).accel.executor_prepare_reset,
                exec,
                Some(abandon_aggregate_transport),
            );
            let exec = &mut *(*state).accel.executor.cast::<AggExecState>();
            exec.set_query_context(context);
            if fault == Some(PairedFault::Prepare) {
                pgrx::error!("paired injected installed preparation failure");
            }
            let started = std::time::Instant::now();
            exec.prepare_descriptor();
            if fault == Some(PairedFault::AfterPrepare) {
                pgrx::error!("paired injected prepared-owner failure");
            }
            let prepared = std::time::Instant::now();
            let mut rows = Vec::new();
            while !exec.next(slot).is_null() {
                if fault == Some(PairedFault::AfterDispatch) {
                    pgrx::error!("paired injected post-dispatch failure");
                }
                // Agg next supplies the complete virtual slot. Copy typed scalars
                // now; no Datum or slot pointer escapes the consumption interval.
                let key = if *(*slot).tts_isnull {
                    None
                } else {
                    let datum = *(*slot).tts_values;
                    Some(match key_type {
                        21 => i64::from(
                            <i16 as pgrx::FromDatum>::from_datum(datum, false).expect("INT2"),
                        ),
                        23 => i64::from(
                            <i32 as pgrx::FromDatum>::from_datum(datum, false).expect("INT4"),
                        ),
                        20 => <i64 as pgrx::FromDatum>::from_datum(datum, false).expect("INT8"),
                        _ => unreachable!("validated key type"),
                    })
                };
                assert!(!*(*slot).tts_isnull.add(1), "COUNT must be non-NULL");
                let count = <i64 as pgrx::FromDatum>::from_datum(*(*slot).tts_values.add(1), false)
                    .expect("COUNT INT8");
                if rows.len() == super::paired_sink::ROW_LIMIT {
                    super::paired_sink::retain_row_bound(&rows, &vec![(key, count)]);
                    pgrx::error!("paired typed collector exceeded fixed row bound");
                }
                rows.push((key, count));
            }
            let consumed = std::time::Instant::now();
            let mode = exec.descriptor_dispatched_kernel_mode();
            let calls = exec.batches_executed;
            let scheduled = exec.descriptor_planned_batches();
            let dispatch_us = exec.dispatch_time_us;
            let artifact_summary = exec.descriptor_artifact_policy_summary();
            let (completed, identity) = exec.paired_completion_identity();
            let artifact_outcome = exec.descriptor_artifact_outcome();
            let artifact_bytes = exec.descriptor_artifact_bytes();
            let residency = exec
                .descriptor_residency_evidence()
                .unwrap_or_default()
                .to_vec();
            let loaded_relations = exec
                .descriptor_loaded_relations()
                .unwrap_or_default()
                .to_vec();
            let captured = std::time::Instant::now();
            // Fallible normal close happens while ownership and the error boundary
            // remain installed. No stack-owned raw executor bypass is permitted.
            release_executor(&mut (*state).accel, false);
            pg_sys::ExecDropSingleTupleTableSlot(slot);
            pg_sys::FreeTupleDesc(descriptor);
            pg_sys::MemoryContextSwitchTo(previous);
            PairedOwnedOutput {
                rows,
                key_type,
                mode,
                calls,
                scheduled,
                dispatch_us,
                artifact_summary,
                artifact_outcome,
                artifact_bytes,
                residency,
                loaded_relations,
                completed,
                identity,
                preparation_ns: prepared.duration_since(started).as_nanos(),
                consumption_ns: consumed.duration_since(prepared).as_nanos(),
                descriptor_executor_ns: 0,
                diagnostic_capture_ns: captured.duration_since(consumed).as_nanos(),
            }
        })
    };
    // SAFETY: normal close removed the executor. Delete the context outside
    // the installed-state catch boundary, which must never see freed state.
    unsafe { pg_sys::FreeExecutorState(estate) };
    output.descriptor_executor_ns = descriptor_started.elapsed().as_nanos();
    output
}

//! Resident descriptor aggregate executor for pg_accel Custom Scan nodes.
//!
//! [`AggExecState`] adapts the strict neutral AQS6/AOP2 contract to the Custom
//! Scan FFI layer.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — validates the plan descriptor and allocates
//!    `AggExecState`.
//! 2. **`exec_custom_scan`** (repeated) — dispatches the resident aggregate
//!    descriptor and emits result tuples.
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

mod artifact;
mod descriptor;
mod execute;
mod output;
mod spatial;

#[cfg(feature = "pg_test")]
pub(crate) use descriptor::scalar_measure_column;
#[cfg(feature = "pg_test")]
pub(crate) use descriptor::test_scalar_resident_aggregate;

#[cfg(any(test, feature = "pg_test"))]
pub(crate) use output::materialize_numeric_sum;

/// Live-test entry into the production descriptor builder, residency resolver,
/// bounded executor and tuple materializer. It bypasses planner admission only.
#[cfg(feature = "pg_test")]
pub(crate) fn test_execute_numeric_descriptor(
    spec: crate::engine::spec::AggQuerySpec,
    projection: crate::engine::spec::AggOutputProjection,
) -> Vec<Vec<Option<Vec<u8>>>> {
    test_execute_descriptor(
        spec,
        projection,
        crate::gpu::GroupedAggKernelMode::ParallelDenseNumeric,
    )
}

/// Full scalar descriptor lifecycle under a fresh statement-visible snapshot.
/// This test bridge bypasses admission only; it does not inject a SQL path.
#[cfg(feature = "pg_test")]
pub(crate) fn test_execute_scalar_descriptor(
    spec: crate::engine::spec::AggQuerySpec,
    projection: crate::engine::spec::AggOutputProjection,
) -> Vec<Vec<Option<Vec<u8>>>> {
    struct SnapshotGuard;
    impl Drop for SnapshotGuard {
        fn drop(&mut self) {
            // SAFETY: exactly one private copied snapshot is pushed below.
            unsafe { pg_sys::PopActiveSnapshot() };
        }
    }
    // SAFETY: live backend test with an active outer SQL snapshot. Its command
    // ID predates nested SPI fixture writes; do not mutate the outer snapshot.
    unsafe {
        pg_sys::PushCopiedSnapshot(pg_sys::GetActiveSnapshot());
        pg_sys::UpdateActiveSnapshotCommandId();
    }
    let _snapshot = SnapshotGuard;
    test_execute_descriptor(
        spec,
        projection,
        crate::gpu::GroupedAggKernelMode::ParallelDenseInteger,
    )
}

#[cfg(feature = "pg_test")]
fn test_execute_descriptor(
    spec: crate::engine::spec::AggQuerySpec,
    projection: crate::engine::spec::AggOutputProjection,
    expected_mode: crate::gpu::GroupedAggKernelMode,
) -> Vec<Vec<Option<Vec<u8>>>> {
    let plan = descriptor::DescriptorAggPlan::new(spec, projection.clone())
        .expect("validated descriptor plan");
    plan.ensure_artifact()
        .expect("NUMERIC descriptor preparation");
    let mut dispatched = plan
        .execute_prepared()
        .expect("NUMERIC descriptor execution");
    assert_eq!(dispatched.kernel_mode, expected_mode);
    assert_eq!(dispatched.planned_batches, dispatched.batches_executed);
    // SAFETY: running on the PostgreSQL backend thread; each attribute is
    // initialized with a built-in result type from the validated projection.
    let (tuple_desc, slot) = unsafe {
        let tuple_desc = pg_sys::CreateTemplateTupleDesc(projection.slots.len() as i32);
        for (index, field) in projection.slots.iter().enumerate() {
            pg_sys::TupleDescInitEntry(
                tuple_desc,
                (index + 1) as i16,
                c"value".as_ptr(),
                pg_sys::Oid::from(field.result_type_oid),
                -1,
                0,
            );
        }
        let slot = pg_sys::MakeSingleTupleTableSlot(tuple_desc, &raw const pg_sys::TTSOpsVirtual);
        (tuple_desc, slot)
    };
    let mut rows = Vec::new();
    // SAFETY: slot has exactly the descriptor's arity and valid storage, and
    // remains live until the complete synchronous materialization finishes.
    while !unsafe { dispatched.output.next(slot) }.is_null() {
        let row = projection
            .slots
            .iter()
            .enumerate()
            .map(|(index, field)| {
                // SAFETY: next() filled every value/null flag at this checked index.
                let (datum, is_null) = unsafe {
                    (
                        *(*slot).tts_values.add(index),
                        *(*slot).tts_isnull.add(index),
                    )
                };
                if is_null {
                    return None;
                }
                let send = match field.result_type_oid {
                    23 => pg_sys::int4send,
                    20 => pg_sys::int8send,
                    1700 => pg_sys::numeric_send,
                    _ => panic!("unsupported NUMERIC fixture output type"),
                };
                // SAFETY: the validated result type selects its exact PostgreSQL
                // binary sender; the slot's Datum stays live during this call.
                unsafe { pgrx::direct_function_call::<Vec<u8>>(send, &[Some(datum)]) }
            })
            .collect();
        rows.push(row);
    }
    // SAFETY: slot was allocated above and is not retained. The template
    // descriptor is unrefcounted (-1), so dropping the slot does not free it.
    unsafe {
        pg_sys::ExecDropSingleTupleTableSlot(slot);
        pg_sys::FreeTupleDesc(tuple_desc);
    }
    rows.sort();
    rows
}

pub(crate) use artifact::descriptor_program_clone_bytes;
pub(crate) use artifact::estimate_descriptor_artifact_bytes_upper_bound;
pub(crate) use descriptor::dense_descriptor_memory_requirements;
// Keep the existing pg_test scalar-layout probes on the shared native quote.
#[cfg(feature = "pg_test")]
pub(crate) use descriptor::dense_descriptor_memory_requirements as scalar_descriptor_memory_requirements;
pub(crate) use descriptor::numeric_descriptor_memory_requirements;
pub(crate) use descriptor::planned_descriptor_kernel_mode;
pub(crate) use descriptor::scalar_measure_program;
pub(crate) use descriptor::scalar_metadata_peak_bytes;
pub use execute::AggExecState;

pub(crate) fn is_weighted_global_count_descriptor(
    spec: &crate::engine::spec::AggQuerySpec,
) -> bool {
    descriptor::weighted_global_count_inputs(spec)
}

#[cfg(feature = "pg_test")]
pub(crate) use descriptor::{configure_dense_dispatch_test, dense_dispatch_test_completed_calls};

pub(crate) fn validate_normal_descriptor_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_normal_descriptor_capability(spec, projection)
}

pub(crate) fn validate_normal_spatial_candidate_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_normal_spatial_candidate_capability(spec, projection)
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn validate_test_forced_spatial_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_test_forced_spatial_capability(spec, projection)
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn validate_test_forced_numeric_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_runtime_capability(spec, projection)
}

use pgrx::pg_sys;

impl crate::engine::executor::state::ExecutorState for AggExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: trait contract — main backend thread, `css` is a valid
        // CustomScanState whose scan slot was built by ExecInitCustomScan.
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        // SAFETY: main backend thread; scan_slot is a valid TupleTableSlot.
        let result = unsafe { self.next(scan_slot) };
        if result.is_null() {
            // SAFETY: scan_slot is a valid TupleTableSlot on the main thread.
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        result
    }
    fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }
    fn batches_executed(&self) -> u64 {
        self.batches_executed
    }
    fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }
}

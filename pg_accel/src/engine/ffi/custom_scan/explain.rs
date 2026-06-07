//! EXPLAIN output for Custom Scan nodes.
//!
//! Reports strategy + batch config always; execution counters only under
//! EXPLAIN ANALYZE.

use std::ffi::{CStr, c_int};

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy, function_scan, srf_target_list};
use crate::engine::executor::agg::AggExecState;
use crate::engine::executor::join::JoinExecState;
use crate::engine::executor::preagg::PreAggExecState;
use crate::engine::executor::scan::ScanExecState;
use crate::engine::executor::sort::SortExecState;
use crate::engine::registry::AccelStrategy;

/// `ExplainCustomScan`: emit EXPLAIN output.
///
/// Always shows Strategy, Batch Size, Expected Threads. When `EXPLAIN ANALYZE`,
/// also shows Rows Dispatched, Batches, and Dispatch Time.
///
/// # Safety
///
/// Called by the executor on the main backend thread.
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
        if strategy == GpuStrategy::Scan && !(*state).accel.executor.is_null() {
            let scan_state = &*(*state).accel.executor.cast::<ScanExecState>();
            pg_sys::ExplainPropertyText(
                c"Accel Strategy".as_ptr(),
                accel_strategy_label(scan_state.strategy()).as_ptr(),
                es,
            );
        }
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
        pg_sys::ExplainPropertyBool(c"GPU Resident Pipeline".as_ptr(), false, es);
        pg_sys::ExplainPropertyText(
            c"GPU Resident Boundary".as_ptr(),
            gpu_resident_boundary_reason(strategy).as_ptr(),
            es,
        );

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

            if strategy == GpuStrategy::Scan {
                pg_sys::ExplainPropertyInteger(
                    c"Parallel Worker".as_ptr(),
                    std::ptr::null(),
                    i64::from((*state).accel.parallel_worker_number),
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
                if agg_state.partial_emitters.is_some() {
                    pg_sys::ExplainPropertyBool(c"Partial".as_ptr(), true, es);
                }
            }

            if strategy == GpuStrategy::Join && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as JoinExecState.
                let join_state = &*(*state).accel.executor.cast::<JoinExecState>();
                pg_sys::ExplainPropertyBool(
                    c"Hash Join Count Only".as_ptr(),
                    join_state.hash_join_count_only(),
                    es,
                );
                if join_state.strategy() == crate::engine::registry::AccelStrategy::GpuHashJoin {
                    let telemetry = join_state.hash_join_telemetry();
                    pg_sys::ExplainPropertyInteger(
                        c"Hash Join Build Count".as_ptr(),
                        std::ptr::null(),
                        telemetry.build_count() as i64,
                        es,
                    );
                    pg_sys::ExplainPropertyInteger(
                        c"Hash Join Redundant Builds".as_ptr(),
                        std::ptr::null(),
                        telemetry.redundant_inner_builds() as i64,
                        es,
                    );
                    pg_sys::ExplainPropertyInteger(
                        c"Hash Join Build Rows".as_ptr(),
                        std::ptr::null(),
                        telemetry.build_rows() as i64,
                        es,
                    );
                    pg_sys::ExplainPropertyInteger(
                        c"Hash Join Build Non-Null Rows".as_ptr(),
                        std::ptr::null(),
                        telemetry.build_non_null_rows() as i64,
                        es,
                    );
                    pg_sys::ExplainPropertyInteger(
                        c"Hash Join Probe Batches".as_ptr(),
                        std::ptr::null(),
                        telemetry.probe_batches() as i64,
                        es,
                    );
                    pg_sys::ExplainPropertyBool(
                        c"GPU Hash Table Reused Across Probe Batches".as_ptr(),
                        join_state.hash_join_reuses_build_across_probe_batches(),
                        es,
                    );
                    pg_sys::ExplainPropertyBool(
                        c"Shared GPU Inner Reuse".as_ptr(),
                        join_state.hash_join_shared_inner_reuse(),
                        es,
                    );
                }
            }

            // For Sort strategy, distinguish rows consumed from rows actually
            // submitted to the bounded top-k GPU kernel.
            if strategy == GpuStrategy::Sort && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as SortExecState.
                let sort_state = &*(*state).accel.executor.cast::<SortExecState>();
                pg_sys::ExplainPropertyBool(
                    c"GPU Dispatched".as_ptr(),
                    sort_state.gpu_dispatched,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"GPU Rows Dispatched".as_ptr(),
                    std::ptr::null(),
                    sort_state.gpu_rows_dispatched as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Top-K Limit".as_ptr(),
                    std::ptr::null(),
                    sort_state.limit().unwrap_or(0) as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Input Rows Materialized".as_ptr(),
                    std::ptr::null(),
                    sort_state.input_rows_materialized() as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Output Tuples Retained".as_ptr(),
                    std::ptr::null(),
                    sort_state.retained_output_tuples() as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Rows Pruned After Top-K".as_ptr(),
                    std::ptr::null(),
                    sort_state.rows_pruned_after_topk() as i64,
                    es,
                );
                pg_sys::ExplainPropertyBool(
                    c"Full Input Materialized".as_ptr(),
                    sort_state.full_input_materialized(),
                    es,
                );
            }

            // For PreAgg strategy, report fused pipeline metrics.
            if strategy == GpuStrategy::PreAgg && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as PreAggExecState.
                let preagg_state = &*(*state).accel.executor.cast::<PreAggExecState>();
                pg_sys::ExplainPropertyInteger(
                    c"Depths".as_ptr(),
                    std::ptr::null(),
                    preagg_state.depths.len() as i64,
                    es,
                );
                pg_sys::ExplainPropertyInteger(
                    c"Fact Rows Scanned".as_ptr(),
                    std::ptr::null(),
                    preagg_state.rows_dispatched as i64,
                    es,
                );
                pg_sys::ExplainPropertyBool(
                    c"Has Scan Expr".as_ptr(),
                    preagg_state.scan_expr.is_some(),
                    es,
                );
            }
        }
    }
}

/// Determine expected thread count (GPU-only: always 1, no CPU worker pool).
pub(super) fn resolve_thread_count() -> c_int {
    1
}

const fn accel_strategy_label(strategy: AccelStrategy) -> &'static CStr {
    match strategy {
        AccelStrategy::GpuSpatial => c"GpuSpatial",
        AccelStrategy::GpuRaster => c"GpuRaster",
        AccelStrategy::GpuH3 => c"GpuH3",
        AccelStrategy::GpuSort => c"GpuSort",
        AccelStrategy::GpuReduce => c"GpuReduce",
        AccelStrategy::GpuExpr => c"GpuExpr",
        AccelStrategy::GpuHashJoin => c"GpuHashJoin",
        AccelStrategy::GpuWindow => c"GpuWindow",
        AccelStrategy::GpuNestedLoopIneq => c"GpuNestedLoopIneq",
    }
}

unsafe fn gpu_kernel_dispatched_for_explain(
    strategy: GpuStrategy,
    state: *const GpuAccelScanState,
) -> bool {
    let executor = unsafe { (*state).accel.executor };
    if executor.is_null() {
        return false;
    }

    match strategy {
        GpuStrategy::Agg => unsafe { (*executor.cast::<AggExecState>()).gpu_dispatched },
        GpuStrategy::Sort => unsafe { (*executor.cast::<SortExecState>()).gpu_dispatched },
        GpuStrategy::FunctionScan => unsafe { function_scan::dispatched_ok(executor) },
        GpuStrategy::SrfTargetList => unsafe { srf_target_list::batches_executed(executor) > 0 },
        GpuStrategy::Scan | GpuStrategy::Join | GpuStrategy::Window | GpuStrategy::PreAgg => unsafe {
            (*state).accel.batches_executed > 0
        },
    }
}

unsafe fn rows_returned_to_cpu(node: *mut pg_sys::CustomScanState) -> i64 {
    let instrument = unsafe { (*node).ss.ps.instrument };
    if instrument.is_null() {
        return 0;
    }
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
        GpuStrategy::Scan => c"GpuScan consumes heap or child tuples on CPU via heap_getnext/ExecProcNode/MinimalTuple staging and emits PostgreSQL slots",
        GpuStrategy::Join => c"GpuJoin collects child rows through ExecProcNode into host MinimalTuple/key buffers and reconstructs joined PostgreSQL slots",
        GpuStrategy::Agg => c"GpuAgg drains child tuples and stages host value/key/null Vec buffers before GPU reduce or grouped aggregation",
        GpuStrategy::Sort => c"GpuSort materializes input tuples on CPU, sends key vectors only, reorders host MinimalTuples, and emits PostgreSQL slots",
        GpuStrategy::Window => c"GpuWindow buffers input MinimalTuples, extracts host columns, stores host result vectors, and emits PostgreSQL slots",
        GpuStrategy::PreAgg => c"GpuPreAgg materializes dimensions in host HashMap state and scans/probes fact rows through ExecProcNode/materialized slots",
        GpuStrategy::FunctionScan => c"GpuFunctionScan dispatches constant arguments once, buffers host Datums, and drains output through PostgreSQL slots",
        GpuStrategy::SrfTargetList => c"GpuAccelSrfTargetList drives ProjectSet input through ExecProcNode, buffers per-row SRF output, and emits expanded PostgreSQL tuples",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::executor::agg::AggOp;
    use crate::engine::registry::AccelStrategy;

    #[test]
    fn every_gpu_strategy_has_non_resident_boundary_reason() {
        for strategy in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
            GpuStrategy::PreAgg,
            GpuStrategy::FunctionScan,
            GpuStrategy::SrfTargetList,
        ] {
            let reason = gpu_resident_boundary_reason(strategy)
                .to_str()
                .expect("reason is utf8");
            assert!(
                reason.contains("CPU")
                    || reason.contains("ExecProcNode")
                    || reason.contains("host")
                    || reason.contains("PostgreSQL slots"),
                "{strategy:?} reason must name the CPU/PostgreSQL boundary: {reason}"
            );
        }
    }

    #[test]
    fn accel_strategy_labels_are_explain_stable() {
        assert_eq!(accel_strategy_label(AccelStrategy::GpuExpr), c"GpuExpr");
        assert_eq!(
            accel_strategy_label(AccelStrategy::GpuSpatial),
            c"GpuSpatial"
        );
    }

    #[test]
    fn explain_dispatch_flag_uses_strategy_specific_state() {
        let mut agg = AggExecState::new(AccelStrategy::GpuReduce, 1024, &[(AggOp::Count, 0)]);
        agg.gpu_dispatched = true;
        let state = GpuAccelScanState {
            css: unsafe { std::mem::zeroed() },
            accel: super::super::GpuAccelState {
                strategy: GpuStrategy::Agg as i32,
                batch_size: 1024,
                expected_threads: 1,
                rows_dispatched: 0,
                batches_executed: 0,
                dispatch_time_us: 0,
                parallel_worker_number: -1,
                dsm_flags: 0,
                executor: (&mut agg as *mut AggExecState).cast(),
            },
        };

        assert!(unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Agg, &raw const state) });
        agg.gpu_dispatched = false;
        assert!(!unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Agg, &raw const state) });
    }

    #[test]
    fn explain_dispatch_flag_uses_batch_counters_for_scan_like_strategies() {
        let mut state = GpuAccelScanState {
            css: unsafe { std::mem::zeroed() },
            accel: super::super::GpuAccelState {
                strategy: GpuStrategy::Scan as i32,
                batch_size: 1024,
                expected_threads: 1,
                rows_dispatched: 0,
                batches_executed: 0,
                dispatch_time_us: 0,
                parallel_worker_number: -1,
                dsm_flags: 0,
                executor: std::ptr::dangling_mut::<u8>().cast(),
            },
        };

        assert!(!unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Scan, &raw const state) });
        state.accel.batches_executed = 1;
        assert!(unsafe { gpu_kernel_dispatched_for_explain(GpuStrategy::Scan, &raw const state) });
    }

    #[test]
    fn explain_batch_amortization_helpers_handle_zero_and_nonzero_batches() {
        assert_eq!(rows_per_batch_for_explain(0, 0), 0.0);
        assert_eq!(avg_dispatch_time_per_batch_ms_for_explain(0, 0), 0.0);
        assert_eq!(rows_per_batch_for_explain(1_000, 4), 250.0);
        assert_eq!(avg_dispatch_time_per_batch_ms_for_explain(2_000, 4), 0.5);
    }
}

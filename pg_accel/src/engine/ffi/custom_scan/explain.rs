//! EXPLAIN output for Custom Scan nodes.
//!
//! Reports strategy + batch config always; execution counters only under
//! EXPLAIN ANALYZE.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy};
use crate::engine::executor::agg::AggExecState;
use crate::engine::executor::preagg::PreAggExecState;

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
        let strategy = GpuStrategy::from_i32((*state).accel.strategy);

        pg_sys::ExplainPropertyText(c"Strategy".as_ptr(), strategy.label().as_ptr(), es);
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

        // Execution stats only with EXPLAIN ANALYZE.
        if (*es).analyze {
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

            #[allow(clippy::cast_precision_loss)]
            let time_ms = (*state).accel.dispatch_time_us as f64 / 1000.0;
            pg_sys::ExplainPropertyFloat(c"Dispatch Time".as_ptr(), c"ms".as_ptr(), time_ms, 3, es);

            // For Agg strategy, report whether GPU reduce was used.
            if strategy == GpuStrategy::Agg && !(*state).accel.executor.is_null() {
                // SAFETY: executor was Box::into_raw'd as AggExecState.
                let agg_state = &*(*state).accel.executor.cast::<AggExecState>();
                pg_sys::ExplainPropertyBool(
                    c"GPU Dispatched".as_ptr(),
                    agg_state.gpu_dispatched,
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

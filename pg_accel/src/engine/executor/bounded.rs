//! Portable bounded-dispatch orchestration for synchronous device calls.

use std::time::Duration;

/// One exact, nonempty input range presented to a device launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DispatchChunk {
    pub first_row: usize,
    pub row_count: usize,
}

/// Failure phase retained by the chunk driver without rewriting the caller's
/// domain error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BoundedDispatchError<E> {
    ZeroChunkLimit,
    Dispatch(E),
    InterruptBoundary(E),
}

/// Run synchronous device calls over bounded, contiguous input ranges.
///
/// `interrupt_boundary` runs once before the first launch and once after every
/// completed launch. Consequently there is exactly one check between adjacent
/// chunks. A device call already in progress remains synchronous and cannot be
/// cancelled by this helper.
pub(super) fn run_bounded_dispatch<E>(
    row_count: usize,
    chunk_limit: usize,
    mut dispatch: impl FnMut(DispatchChunk) -> Result<(), E>,
    mut interrupt_boundary: impl FnMut() -> Result<(), E>,
) -> Result<usize, BoundedDispatchError<E>> {
    if chunk_limit == 0 {
        return Err(BoundedDispatchError::ZeroChunkLimit);
    }

    interrupt_boundary().map_err(BoundedDispatchError::InterruptBoundary)?;
    let mut first_row = 0;
    let mut launches = 0;
    while first_row < row_count {
        let current_rows = (row_count - first_row).min(chunk_limit);
        dispatch(DispatchChunk {
            first_row,
            row_count: current_rows,
        })
        .map_err(BoundedDispatchError::Dispatch)?;
        launches += 1;
        first_row += current_rows;
        interrupt_boundary().map_err(BoundedDispatchError::InterruptBoundary)?;
    }
    Ok(launches)
}

/// Release detached device/session resources before propagating a captured
/// PostgreSQL error. `rethrow` is injectable so drop ordering is testable
/// without manufacturing a backend error in a Rust unit test.
pub(super) fn cleanup_before_rethrow<C, E, R>(
    cleanup: C,
    error: E,
    rethrow: impl FnOnce(E) -> R,
) -> R {
    drop(cleanup);
    rethrow(error)
}

/// Exact number of completed synchronous calls for a successful bounded
/// dense aggregate: one call per nonempty input chunk and one finalization
/// call. Empty input still submits one combined RESET|FINALIZE call.
#[must_use]
pub(super) fn bounded_dispatch_call_count(row_count: usize, chunk_limit: usize) -> Option<usize> {
    if chunk_limit == 0 {
        return None;
    }
    row_count.div_ceil(chunk_limit).checked_add(1)
}

/// Whether a completed synchronous dispatch call crossed its warning
/// threshold. This never claims to cancel an in-flight call.
#[must_use]
pub(super) fn dispatch_warning_threshold_exceeded(elapsed: Duration, threshold_ms: i32) -> bool {
    let Ok(threshold_ms) = u64::try_from(threshold_ms) else {
        return false;
    };
    threshold_ms != 0 && elapsed > Duration::from_millis(threshold_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn chunks_are_contiguous_bounded_and_cover_the_input() {
        let mut chunks = Vec::new();
        let mut boundaries = 0;
        let launches = run_bounded_dispatch::<&'static str>(
            10,
            4,
            |chunk| {
                chunks.push(chunk);
                Ok(())
            },
            || {
                boundaries += 1;
                Ok(())
            },
        )
        .expect("bounded dispatch succeeds");

        assert_eq!(launches, 3);
        assert_eq!(boundaries, 4);
        assert_eq!(
            chunks,
            [
                DispatchChunk {
                    first_row: 0,
                    row_count: 4,
                },
                DispatchChunk {
                    first_row: 4,
                    row_count: 4,
                },
                DispatchChunk {
                    first_row: 8,
                    row_count: 2,
                },
            ]
        );
    }

    #[test]
    fn zero_limit_fails_before_callbacks_run() {
        let mut dispatches = 0;
        let mut boundaries = 0;
        let result = run_bounded_dispatch::<&'static str>(
            10,
            0,
            |_| {
                dispatches += 1;
                Ok(())
            },
            || {
                boundaries += 1;
                Ok(())
            },
        );

        assert_eq!(result, Err(BoundedDispatchError::ZeroChunkLimit));
        assert_eq!(dispatches, 0);
        assert_eq!(boundaries, 0);
    }

    #[test]
    fn empty_input_checks_interrupts_without_launching() {
        let mut dispatches = 0;
        let mut boundaries = 0;
        let launches = run_bounded_dispatch::<&'static str>(
            0,
            8,
            |_| {
                dispatches += 1;
                Ok(())
            },
            || {
                boundaries += 1;
                Ok(())
            },
        )
        .expect("empty input is valid");

        assert_eq!(launches, 0);
        assert_eq!(dispatches, 0);
        assert_eq!(boundaries, 1);
    }

    #[test]
    fn dispatch_error_is_not_rewritten_and_stops_before_next_boundary() {
        let mut chunks = Vec::new();
        let mut boundaries = 0;
        let result = run_bounded_dispatch(
            9,
            3,
            |chunk| {
                chunks.push(chunk);
                if chunk.first_row == 3 {
                    Err("native status")
                } else {
                    Ok(())
                }
            },
            || {
                boundaries += 1;
                Ok(())
            },
        );

        assert_eq!(result, Err(BoundedDispatchError::Dispatch("native status")));
        assert_eq!(chunks.len(), 2);
        assert_eq!(boundaries, 2);
    }

    #[test]
    fn interrupt_boundary_error_is_distinct_and_prevents_later_launches() {
        let mut dispatches = 0;
        let mut boundaries = 0;
        let result = run_bounded_dispatch(
            9,
            3,
            |_| {
                dispatches += 1;
                Ok(())
            },
            || {
                boundaries += 1;
                if boundaries == 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            },
        );

        assert_eq!(
            result,
            Err(BoundedDispatchError::InterruptBoundary("cancelled"))
        );
        assert_eq!(dispatches, 1);
        assert_eq!(boundaries, 2);
    }

    #[test]
    fn detached_resources_drop_before_rethrow_boundary() {
        struct DropProbe<'a>(&'a Cell<usize>);

        impl Drop for DropProbe<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let dropped = Cell::new(0);
        let result = cleanup_before_rethrow(
            (DropProbe(&dropped), DropProbe(&dropped)),
            "cancelled",
            |error| {
                assert_eq!(dropped.get(), 2);
                error
            },
        );

        assert_eq!(result, "cancelled");
        assert_eq!(dropped.get(), 2);
    }

    #[test]
    fn dispatch_warning_threshold_is_post_call_and_strictly_exceeded() {
        assert!(!dispatch_warning_threshold_exceeded(
            Duration::from_millis(101),
            0
        ));
        assert!(!dispatch_warning_threshold_exceeded(
            Duration::from_millis(101),
            -1
        ));
        assert!(!dispatch_warning_threshold_exceeded(
            Duration::from_millis(100),
            100
        ));
        assert!(dispatch_warning_threshold_exceeded(
            Duration::from_micros(100_001),
            100
        ));
    }

    #[test]
    fn successful_call_count_includes_finalize_and_empty_reset_finalize() {
        assert_eq!(bounded_dispatch_call_count(0, 64), Some(1));
        assert_eq!(bounded_dispatch_call_count(1, 64), Some(2));
        assert_eq!(bounded_dispatch_call_count(64, 64), Some(2));
        assert_eq!(bounded_dispatch_call_count(65, 64), Some(3));
        assert_eq!(bounded_dispatch_call_count(10, 0), None);
        assert_eq!(bounded_dispatch_call_count(usize::MAX, 1), None);
    }
}

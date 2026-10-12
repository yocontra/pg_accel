//! Portable bounded-dispatch orchestration for synchronous device calls.

use std::time::Duration;

/// A device attempt either owns a complete result or has discarded all of its
/// partial output at an interrupt boundary. Interrupted attempts are restartable
/// only after the outer caller has serviced PostgreSQL outside native/store
/// borrows and revalidated every source dependency.
pub(super) enum InterruptibleAttempt<T> {
    Complete(T),
    Interrupted,
}

#[derive(Debug)]
pub(super) enum InterruptibleDispatchError<E, I> {
    Dispatch(E),
    Interrupt(I),
}

/// Separate read-only signal polling from interrupt delivery. `dispatch` must
/// release native frames and all residency borrows before returning. `service`
/// must capture PostgreSQL ERROR rather than longjmp through Rust owners.
/// An interrupt error takes precedence over a simultaneous dispatch error;
/// both unsuccessful and successful device owners drop before it is returned.
pub(super) fn run_interruptible_dispatch<T, E, I>(
    mut dispatch: impl FnMut(&mut dyn FnMut() -> bool) -> Result<InterruptibleAttempt<T>, E>,
    mut pending: impl FnMut() -> bool,
    mut service: impl FnMut() -> Result<(), I>,
) -> Result<T, InterruptibleDispatchError<E, I>> {
    service().map_err(InterruptibleDispatchError::Interrupt)?;
    loop {
        let attempt = dispatch(&mut pending);
        if let Err(interrupt) = service() {
            drop(attempt);
            return Err(InterruptibleDispatchError::Interrupt(interrupt));
        }
        match attempt.map_err(InterruptibleDispatchError::Dispatch)? {
            InterruptibleAttempt::Complete(output) => return Ok(output),
            InterruptibleAttempt::Interrupted => {
                // ProcessInterrupts may only service a barrier, memory-context
                // log request, etc. A native cancellation is not itself a SQL
                // error. Retry with fresh binding after that benign interrupt.
            }
        }
    }
}

/// PostgreSQL's signal flags are read-only here. Respect both global interrupt
/// holdoff and critical sections. Query-cancel holdoff must not create a retry
/// loop around ProcessInterrupts, but does not defer a pending backend death.
/// This mirrors the guards in PG18/19 miscadmin.h and ProcessInterrupts().
fn can_service_pending_interrupt(
    pending: bool,
    process_death: bool,
    interrupt_holdoff: u32,
    cancel_holdoff: u32,
    critical_section: u32,
) -> bool {
    pending
        && interrupt_holdoff == 0
        && critical_section == 0
        && (cancel_holdoff == 0 || process_death)
}

/// Backend-main-thread signal poll, safe to use from a synchronous native host
/// callback. Never invokes PostgreSQL, allocates, raises an error or clears a
/// flag. A signal arriving after these reads is seen at the next boundary.
pub(super) fn postgres_interrupt_pending() -> bool {
    // SAFETY: PostgreSQL exports these signal-handler-owned volatile globals.
    // This function runs only on its backend main thread. Volatile reads match
    // the C header's contract and do not retain references to mutable globals.
    unsafe {
        can_service_pending_interrupt(
            std::ptr::read_volatile(&raw const pgrx::pg_sys::InterruptPending) != 0,
            std::ptr::read_volatile(&raw const pgrx::pg_sys::ProcDiePending) != 0,
            std::ptr::read_volatile(&raw const pgrx::pg_sys::InterruptHoldoffCount),
            std::ptr::read_volatile(&raw const pgrx::pg_sys::QueryCancelHoldoffCount),
            std::ptr::read_volatile(&raw const pgrx::pg_sys::CritSectionCount),
        )
    }
}

/// Invoke only after native execution and every residency borrow have ended.
/// Retain the original PostgreSQL error for cleanup-before-rethrow handling.
pub(super) fn capture_postgres_interrupt() -> Option<pgrx::pg_sys::panic::CaughtError> {
    pgrx::pg_sys::PgTryBuilder::new(|| {
        pgrx::check_for_interrupts!();
        None
    })
    .catch_others(Some)
    .execute()
}

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
    fn native_signal_poll_respects_postgres_holdoffs_and_process_death_priority() {
        for pending in [false, true] {
            for death in [false, true] {
                for holdoff in [0, 1, u32::MAX] {
                    for cancel_holdoff in [0, 1, u32::MAX] {
                        for critical in [0, 1, u32::MAX] {
                            let expected = pending
                                && holdoff == 0
                                && critical == 0
                                && (cancel_holdoff == 0 || death);
                            assert_eq!(
                                can_service_pending_interrupt(
                                    pending,
                                    death,
                                    holdoff,
                                    cancel_holdoff,
                                    critical
                                ),
                                expected
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn interrupt_before_first_attempt_never_binds_or_dispatches() {
        let mut calls = 0;
        let result = run_interruptible_dispatch::<(), (), _>(
            |_| {
                calls += 1;
                Ok(InterruptibleAttempt::Complete(()))
            },
            || panic!("no native polling before interrupt delivery"),
            || Err("57014: statement timeout"),
        );
        assert_eq!(calls, 0);
        assert!(matches!(
            result,
            Err(InterruptibleDispatchError::Interrupt(
                "57014: statement timeout"
            ))
        ));
    }

    #[test]
    fn interrupt_service_runs_after_borrow_and_native_frames_release() {
        let store = std::cell::RefCell::new(());
        let native_live = Cell::new(false);
        let services = Cell::new(0);
        let result = run_interruptible_dispatch::<_, (), ()>(
            |pending| {
                let _borrow = store.borrow();
                native_live.set(true);
                assert!(!pending());
                native_live.set(false);
                Ok(InterruptibleAttempt::Complete(47))
            },
            || {
                assert!(native_live.get());
                false
            },
            || {
                assert!(!native_live.get());
                assert!(store.try_borrow_mut().is_ok());
                services.set(services.get() + 1);
                Ok(())
            },
        )
        .expect("complete outside interrupt frames");
        assert_eq!(result, 47);
        assert_eq!(services.get(), 2);
    }

    struct InterruptTestOwner<'a>(&'a Cell<usize>);
    impl Drop for InterruptTestOwner<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    #[test]
    fn captured_interrupt_drops_success_or_failure_owners_before_propagation() {
        for dispatch_failed in [false, true] {
            let owners = Cell::new(0);
            let calls = Cell::new(0);
            let result = run_interruptible_dispatch(
                |_| {
                    owners.set(owners.get() + 1);
                    let owner = InterruptTestOwner(&owners);
                    if dispatch_failed {
                        Err(owner)
                    } else {
                        Ok(InterruptibleAttempt::Complete(owner))
                    }
                },
                || false,
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 1 {
                        Ok(())
                    } else {
                        Err("original PG error")
                    }
                },
            );
            assert!(matches!(
                result,
                Err(InterruptibleDispatchError::Interrupt("original PG error"))
            ));
            assert_eq!(
                owners.get(),
                0,
                "release before caller rethrows PostgreSQL error"
            );
            assert_eq!(calls.get(), 2);
        }
    }

    #[test]
    fn benign_interrupt_discards_partial_attempt_then_rebinds_and_retries() {
        let attempts = Cell::new(0);
        let services = Cell::new(0);
        let flag = Cell::new(false);
        let owners = Cell::new(0);
        let output = run_interruptible_dispatch::<_, (), ()>(
            |pending| {
                attempts.set(attempts.get() + 1);
                owners.set(owners.get() + 1);
                let owner = InterruptTestOwner(&owners);
                if attempts.get() == 1 {
                    flag.set(true);
                    assert!(pending());
                    drop(owner);
                    Ok(InterruptibleAttempt::Interrupted)
                } else {
                    assert!(!pending());
                    Ok(InterruptibleAttempt::Complete(owner))
                }
            },
            || flag.get(),
            || {
                services.set(services.get() + 1);
                flag.set(false);
                Ok(())
            },
        )
        .expect("benign interrupt is not a SQL error");
        assert_eq!(attempts.get(), 2);
        assert_eq!(services.get(), 3);
        assert_eq!(owners.get(), 1);
        drop(output);
        assert_eq!(owners.get(), 0);
    }

    #[test]
    fn retry_binding_failure_stops_without_reusing_cancelled_output() {
        let mut attempts = 0;
        let result = run_interruptible_dispatch::<(), _, ()>(
            |_| {
                attempts += 1;
                if attempts == 1 {
                    Ok(InterruptibleAttempt::Interrupted)
                } else {
                    Err("resident generation changed")
                }
            },
            || true,
            || Ok(()),
        );
        assert_eq!(attempts, 2);
        assert!(matches!(
            result,
            Err(InterruptibleDispatchError::Dispatch(
                "resident generation changed"
            ))
        ));
    }

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

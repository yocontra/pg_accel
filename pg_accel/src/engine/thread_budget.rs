//! Cluster-wide thread budget managed via PostgreSQL shared memory.
//!
//! A single [`PgLwLock`]-protected [`ThreadBudgetData`] struct lives in shared
//! memory. Each backend that needs worker threads calls [`request_threads`],
//! which atomically checks the budget (governed by `pg_accel.max_workers_total`)
//! and grants up to the requested count. [`release_threads`] returns threads to
//! the pool. [`request_threads`] lazily registers [`cleanup_backend`] via
//! `before_shmem_exit` to reclaim any threads leaked by a crashing backend.

use crate::engine::gucs;
use pgrx::lwlock::PgLwLock;
use pgrx::pg_shmem_init;
#[cfg(not(test))]
use pgrx::prelude::*;
use pgrx::shmem::PGRXSharedMemory;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent backends we track individually.
///
/// The shared-memory struct is a fixed-size array (`pg_shmem_init!` allocates
/// `sizeof(ThreadBudgetData)` once, and `PGRXSharedMemory` requires a
/// `'static`, `Copy`, fixed-layout type — a runtime-sized `Vec` in shared
/// memory is not supported by that trait), so this cannot be derived from
/// `MaxBackends` at segment-registration time without a much larger rework of
/// how the segment is requested.
///
/// It is therefore sized generously (1024 slots × 8 bytes ≈ 8 KiB of shared
/// memory) so it comfortably covers a default cluster's `MaxBackends`
/// (`max_connections` + autovacuum workers + background workers + WAL
/// senders, typically a few hundred). When `MaxBackends` still exceeds this
/// ceiling, [`warn_if_backends_exceed_capacity`] emits a loud one-time
/// WARNING and backends past the ceiling fall through to the degraded
/// global-only accounting path in [`record_allocation`] (the cluster-wide
/// total stays correct; only per-backend reclaim-on-crash is lost for those
/// overflow backends).
const MAX_BACKENDS: usize = 1024;

// ---------------------------------------------------------------------------
// Shared-memory data
// ---------------------------------------------------------------------------

/// Per-backend allocation record stored in shared memory.
#[derive(Debug, Clone, Copy, Default)]
struct BackendSlot {
    /// `pg_sys::MyProcPid` of the owning backend, or 0 if unused.
    pid: i32,
    /// Number of threads currently allocated to this backend.
    allocated: i32,
}

/// Shared-memory state for the cluster-wide thread budget.
///
/// Protected by an [`PgLwLock`]; all access must hold the lock.
#[derive(Debug)]
pub struct ThreadBudgetData {
    /// Sum of all per-backend allocations.
    total_allocated: i32,
    /// Fixed-size array of per-backend slots.
    backends: [BackendSlot; MAX_BACKENDS],
}

impl Default for ThreadBudgetData {
    fn default() -> Self {
        Self {
            total_allocated: 0,
            backends: [BackendSlot::default(); MAX_BACKENDS],
        }
    }
}

// SAFETY: `ThreadBudgetData` contains only `Copy` primitives (i32) and fixed-size
// arrays thereof. It has no heap allocations, pointers, or non-Send types.
// All access is serialised by the enclosing `PgLwLock`.
unsafe impl PGRXSharedMemory for ThreadBudgetData {}

/// Global LwLock-protected thread budget in shared memory.
///
/// # Safety
///
/// `PgLwLock::new` is `const unsafe` because the caller must ensure the lock is
/// initialised via `pg_shmem_init!` before any access.  We guarantee this by
/// calling `init_shmem()` from `_PG_init`.
pub static BUDGET: PgLwLock<ThreadBudgetData> =
    // SAFETY: Initialised in `init_shmem()` which is called from `_PG_init`.
    unsafe { PgLwLock::new(c"pg_accel_thread_budget") };

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register the shared-memory segment. **Must be called from `_PG_init`.**
///
/// Gated behind `#[cfg(not(test))]` because the test binary links as a
/// standalone executable where PG server symbols (`shmem_request_hook`, etc.)
/// are unavailable. The actual extension `.so` loaded by PG is built as
/// `cdylib` without the `test` cfg and includes this code.
#[cfg(not(test))]
#[cfg(not(feature = "pg_test"))]
#[allow(unexpected_cfgs)]
pub fn init_shmem() {
    pg_shmem_init!(BUDGET);
}

/// No-op stub used by the test binary so that `_PG_init` compiles without
/// referencing PG server-internal shared-memory symbols.
#[cfg(feature = "pg_test")]
pub fn init_shmem() {}

/// Request `n` worker threads from the cluster-wide budget.
///
/// Returns the number of threads actually granted (0..=n). When the GUC
/// `pg_accel.max_workers_total` is 0 (unlimited) the full request is always
/// granted. When the budget is exhausted the caller should fall back to
/// sequential execution.
///
/// Non-positive requests are treated as no-ops and return 0.
pub fn request_threads(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }

    crate::ensure_backend_exit_callback();
    warn_if_backends_exceed_capacity();

    let max = gucs::max_workers_total();

    // Unlimited mode: grant the full request, while still tracking the backend
    // allocation so cleanup_backend can release it during backend exit.
    if max == 0 {
        let mut exclusive = BUDGET.exclusive();
        record_allocation(&mut exclusive, n);
        return n;
    }

    let mut exclusive = BUDGET.exclusive();
    let mut granted = grant_from_budget(n, max, exclusive.total_allocated);

    // If budget is exhausted, try to reclaim slots from dead backends.
    if granted == 0 {
        reclaim_dead_backends(&mut exclusive);
        granted = grant_from_budget(n, max, exclusive.total_allocated);
    }

    if granted == 0 {
        return 0;
    }

    record_allocation(&mut exclusive, granted);
    granted
}

/// Release `n` worker threads back to the cluster-wide budget.
///
/// If `n` exceeds the backend's current allocation the allocation is clamped
/// to zero (never goes negative).
pub fn release_threads(n: i32) {
    if n <= 0 {
        return;
    }

    let mut exclusive = BUDGET.exclusive();
    let pid = current_pid();

    for i in 0..MAX_BACKENDS {
        if exclusive.backends[i].pid == pid {
            let actual = n.min(exclusive.backends[i].allocated);
            // Saturating for defence in depth: `actual` is clamped to the
            // slot's allocation so neither value can legitimately go negative,
            // but never let a corrupted invariant wrap past zero.
            exclusive.backends[i].allocated =
                exclusive.backends[i].allocated.saturating_sub(actual);
            exclusive.total_allocated = exclusive.total_allocated.saturating_sub(actual);

            if exclusive.backends[i].allocated == 0 {
                exclusive.backends[i].pid = 0; // free the slot
            }
            return;
        }
    }
    // Backend not found — nothing to release.
}

/// Release **all** threads owned by the current backend.
///
/// Intended to be called from a `before_shmem_exit` callback so that a
/// crashing backend does not leak budget.
pub fn cleanup_backend() {
    // This runs from a `before_shmem_exit` callback at backend termination.
    // If shared memory was never wired up (e.g. extension loaded without
    // `shared_preload_libraries`, or during early startup failure) the
    // `PgLwLock::exclusive()` call can fail. We cannot propagate errors from a
    // shmem_exit callback, so we contain the failure here — leaking a slot is
    // strictly better than aborting the backend exit path.
    //
    // Honesty caveat: `catch_unwind` only catches a *Rust* panic. If the lock
    // acquisition raises a PostgreSQL ERROR that longjmps at the C level
    // (rather than being converted to a Rust panic by a `#[pg_guard]` frame),
    // that longjmp bypasses `catch_unwind` entirely and PG's own shmem-exit
    // machinery handles it. So this guard hardens the common Rust-panic case
    // (poisoned lock, uninitialised shmem) but is not a universal shield.
    let result = std::panic::catch_unwind(|| {
        let mut exclusive = BUDGET.exclusive();
        let pid = current_pid();

        for i in 0..MAX_BACKENDS {
            if exclusive.backends[i].pid == pid {
                let freed = exclusive.backends[i].allocated;
                // Saturating: total should always be >= this slot's share, but
                // never let a broken invariant drive the shared total negative.
                exclusive.total_allocated = exclusive.total_allocated.saturating_sub(freed);
                exclusive.backends[i].allocated = 0;
                exclusive.backends[i].pid = 0;
                return;
            }
        }
    });

    if let Err(panic) = result {
        // Do NOT swallow silently: a panic here means we may have leaked this
        // backend's thread budget. Log it (stderr is the only safe channel in
        // a shmem-exit callback — the tracing subscriber and PG's ereport
        // machinery may already be torn down at this point in exit) so the
        // leak is at least visible in the postmaster log.
        let detail = panic_message(&*panic);
        eprintln!(
            "pg_accel: thread-budget cleanup_backend panicked ({detail}); \
             this backend's thread-budget slot may be leaked until a later \
             reclaim_dead_backends sweep frees it"
        );
    }
}

/// Best-effort extraction of a human-readable message from a caught panic
/// payload, for logging only.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.as_str()
    } else {
        "non-string panic payload"
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Return the current backend's PID.
#[inline]
fn current_pid() -> i32 {
    #[cfg(not(test))]
    {
        // SAFETY: `MyProcPid` is a global i32 set by PostgreSQL at backend startup.
        // Reading it is safe from the main backend thread (which is the only place
        // we ever call this module).
        unsafe { pgrx::pg_sys::MyProcPid }
    }
    #[cfg(test)]
    {
        std::process::id() as i32
    }
}

/// Scan for backend slots whose owning process has exited (e.g. after
/// `kill -9`) and reclaim their thread budget.
///
/// Uses `kill(pid, 0)` to probe whether each tracked PID is still alive.
/// Dead slots (ESRCH) have their allocation freed. This makes the budget
/// self-healing after abnormal backend termination.
///
/// Caller must hold the exclusive lock.
fn reclaim_dead_backends(data: &mut ThreadBudgetData) {
    let my_pid = current_pid();

    for slot in &mut data.backends {
        if slot.pid == 0 || slot.pid == my_pid {
            continue;
        }

        // Check if the backend process is still alive using PG's own
        // process table. This avoids TOCTOU races from PID recycling
        // that affect the kill(pid, 0) approach.
        // SAFETY: `BackendPidGetProc` is a PG server function that looks up
        // a PGPROC entry by PID. We hold the exclusive LwLock so the slot's
        // PID is stable. Returns NULL if no backend with that PID exists.
        #[cfg(not(test))]
        let alive = unsafe { !pg_sys::BackendPidGetProc(slot.pid).is_null() };
        // SAFETY: `kill(pid, 0)` with signal 0 is a POSIX-defined existence
        // check that does not deliver a signal. The PID is a valid i32 read
        // from a slot we hold under exclusive lock.
        #[cfg(test)]
        let alive = unsafe { libc::kill(slot.pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);

        if !alive {
            #[cfg(not(test))]
            pgrx::warning!(
                "pg_accel: reclaiming {} threads from dead backend PID {}",
                slot.allocated,
                slot.pid,
            );
            data.total_allocated = data.total_allocated.saturating_sub(slot.allocated);
            slot.allocated = 0;
            slot.pid = 0;
        }
    }
}

/// Record an allocation of `granted` threads for the current backend.
///
/// Caller must hold the exclusive lock.
fn record_allocation(data: &mut ThreadBudgetData, granted: i32) {
    if granted <= 0 {
        return;
    }
    let pid = current_pid();

    // Try to find an existing slot for this backend.
    //
    // All arithmetic is saturating: a plain `+=` on i32 can wrap to a
    // *negative* allocation under pathological accumulation (e.g. unlimited
    // mode, where every grant is recorded and a long-lived backend keeps
    // requesting without releasing). A negative total would then make
    // `grant_from_budget`'s `max.saturating_sub(total)` hand out more than
    // the cap. Saturating at i32::MAX is a correct, monotone ceiling; if we
    // ever hit it something upstream is leaking, so surface it loudly rather
    // than silently wrapping.
    for slot in &mut data.backends {
        if slot.pid == pid {
            slot.allocated = saturating_add_checked(slot.allocated, granted, "backend slot");
            data.total_allocated =
                saturating_add_checked(data.total_allocated, granted, "total_allocated");
            return;
        }
    }

    // Find a free slot.
    for slot in &mut data.backends {
        if slot.pid == 0 {
            slot.pid = pid;
            slot.allocated = granted;
            data.total_allocated =
                saturating_add_checked(data.total_allocated, granted, "total_allocated");
            return;
        }
    }

    // No free slots — still update the global total so the budget stays
    // correct, but per-backend tracking is degraded (cleanup_backend won't
    // reclaim these). Rare: only when more than MAX_BACKENDS backends hold a
    // pg_accel allocation simultaneously.
    data.total_allocated = saturating_add_checked(data.total_allocated, granted, "total_allocated");
    warn_msg(&format!(
        "pg_accel: no free backend slot for PID {pid} (>{MAX_BACKENDS} tracked), \
         thread tracking degraded — cluster total stays correct but this \
         backend's threads will not be reclaimed on crash"
    ));
}

/// Add two non-negative thread counts, saturating at `i32::MAX` instead of
/// wrapping to a negative value. Logs loudly on saturation because reaching
/// `i32::MAX` concurrent thread requests is an impossible-in-practice state
/// that indicates an upstream leak.
#[inline]
fn saturating_add_checked(current: i32, delta: i32, what: &str) -> i32 {
    current.checked_add(delta).unwrap_or_else(|| {
        warn_msg(&format!(
            "pg_accel: thread budget {what} would overflow i32 \
             ({current} + {delta}); saturating at i32::MAX — this indicates \
             a thread-accounting leak upstream"
        ));
        i32::MAX
    })
}

/// Emit a warning through the right channel for the current build.
///
/// On a real backend this is `pgrx::warning!` (ereport WARNING). Under the
/// standalone `#[cfg(test)]` binary PG server symbols are unavailable, so we
/// fall back to stderr.
#[inline]
fn warn_msg(msg: &str) {
    #[cfg(not(test))]
    pgrx::warning!("{msg}");
    #[cfg(test)]
    eprintln!("{msg}");
}

/// Emit a loud one-time WARNING if PostgreSQL's `MaxBackends` exceeds the
/// fixed number of per-backend slots we can track individually.
///
/// `MaxBackends` is not known at `_PG_init` time (it is computed later during
/// shared-memory sizing), so this cannot run at segment registration; instead
/// it runs at most once, lazily, on the first thread request in a backend.
/// When it fires, backends beyond slot `MAX_BACKENDS` still get a correct
/// cluster-wide budget but lose per-backend crash reclaim — see the
/// [`MAX_BACKENDS`] doc comment.
#[cfg(not(test))]
fn warn_if_backends_exceed_capacity() {
    use std::sync::Once;
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        // SAFETY: `MaxBackends` is a global i32 set once by PostgreSQL during
        // shared-memory initialisation, before any backend serves queries.
        // We only read it, from the main backend thread.
        let max_backends = unsafe { pgrx::pg_sys::MaxBackends };
        if max_backends > 0 && (max_backends as i64) > MAX_BACKENDS as i64 {
            pgrx::warning!(
                "pg_accel: MaxBackends ({max_backends}) exceeds the {MAX_BACKENDS} \
                 per-backend thread-budget slots; backends past the ceiling keep a \
                 correct cluster-wide budget but will not have their threads reclaimed \
                 on crash. Increase MAX_BACKENDS in thread_budget.rs if this cluster \
                 routinely runs that many pg_accel backends."
            );
        }
    });
}

/// Test-build stub: `MaxBackends` and `pgrx::warning!` are unavailable in the
/// standalone unit-test binary.
#[cfg(test)]
fn warn_if_backends_exceed_capacity() {}

/// Compute how many requested threads can be granted under a cluster-wide cap.
///
/// `max == 0` is unlimited. Callers are still responsible for recording the
/// returned allocation in shared memory when the grant is nonzero.
#[inline]
fn grant_from_budget(requested: i32, max: i32, total_allocated: i32) -> i32 {
    if requested <= 0 {
        return 0;
    }

    if max == 0 {
        return requested;
    }

    let remaining = max.saturating_sub(total_allocated);
    requested.min(remaining)
}

// ---------------------------------------------------------------------------
// Pure-logic unit tests (no PG required)
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used, dead_code)]
mod tests {
    use super::*;

    /// Helper: build a default `ThreadBudgetData` for testing.
    fn empty_budget() -> ThreadBudgetData {
        ThreadBudgetData::default()
    }

    #[test]
    fn default_budget_is_zero() {
        let b = empty_budget();
        assert_eq!(b.total_allocated, 0);
        for slot in &b.backends {
            assert_eq!(slot.pid, 0);
            assert_eq!(slot.allocated, 0);
        }
    }

    #[test]
    fn record_allocation_new_backend() {
        let mut b = empty_budget();
        // Simulate a specific PID by manually writing the slot.
        let pid = 42;
        b.backends[0].pid = pid;
        b.backends[0].allocated = 0;

        // Add allocation to existing slot.
        b.backends[0].allocated += 4;
        b.total_allocated += 4;

        assert_eq!(b.total_allocated, 4);
        assert_eq!(b.backends[0].allocated, 4);
        assert_eq!(b.backends[0].pid, pid);
    }

    #[test]
    fn budget_remaining_calculation() {
        let max_total = 16;
        let mut b = empty_budget();
        b.total_allocated = 10;

        let remaining = max_total - b.total_allocated;
        assert_eq!(remaining, 6);

        let requested = 8;
        let granted = grant_from_budget(requested, max_total, b.total_allocated);
        assert_eq!(granted, 6);
    }

    #[test]
    fn budget_exhausted_returns_zero() {
        let max_total: i32 = 8;
        let mut b = empty_budget();
        b.total_allocated = 8;

        let granted = grant_from_budget(1, max_total, b.total_allocated);
        assert_eq!(granted, 0);
    }

    #[test]
    fn release_clamps_to_zero() {
        let mut slot = BackendSlot {
            pid: 99,
            allocated: 3,
        };

        // Release more than allocated.
        let release = 10;
        let actual = release.min(slot.allocated);
        slot.allocated -= actual;
        assert_eq!(slot.allocated, 0);
        assert_eq!(actual, 3);
    }

    #[test]
    fn cleanup_frees_slot() {
        let mut b = empty_budget();
        b.backends[5].pid = 123;
        b.backends[5].allocated = 7;
        b.total_allocated = 7;

        // Simulate cleanup for PID 123.
        let pid = 123;
        for slot in &mut b.backends {
            if slot.pid == pid {
                b.total_allocated -= slot.allocated;
                slot.allocated = 0;
                slot.pid = 0;
                break;
            }
        }

        assert_eq!(b.total_allocated, 0);
        assert_eq!(b.backends[5].pid, 0);
        assert_eq!(b.backends[5].allocated, 0);
    }

    #[test]
    fn multiple_backends_tracked() {
        let mut b = empty_budget();
        b.backends[0] = BackendSlot {
            pid: 10,
            allocated: 4,
        };
        b.backends[1] = BackendSlot {
            pid: 20,
            allocated: 6,
        };
        b.total_allocated = 10;

        // Verify independent tracking.
        assert_eq!(b.backends[0].allocated, 4);
        assert_eq!(b.backends[1].allocated, 6);
        assert_eq!(b.total_allocated, 10);

        // Release from backend 10.
        let release = 2;
        b.backends[0].allocated -= release;
        b.total_allocated -= release;

        assert_eq!(b.backends[0].allocated, 2);
        assert_eq!(b.total_allocated, 8);
    }

    #[test]
    fn unlimited_mode_grants_full_request() {
        // When max == 0, the full request should be granted.
        let max: i32 = 0;
        let requested = 64;
        let granted = grant_from_budget(requested, max, i32::MAX);
        assert_eq!(granted, 64);
    }

    #[test]
    fn backend_slot_default() {
        let s = BackendSlot::default();
        assert_eq!(s.pid, 0);
        assert_eq!(s.allocated, 0);
    }

    #[test]
    fn reclaim_dead_backends_frees_dead_pids() {
        let mut b = empty_budget();
        // Use a PID that definitely does not exist (max i32).
        let dead_pid: i32 = i32::MAX;
        b.backends[0] = BackendSlot {
            pid: dead_pid,
            allocated: 4,
        };
        b.total_allocated = 4;

        reclaim_dead_backends(&mut b);

        // Dead PID should have been reclaimed.
        assert_eq!(b.backends[0].pid, 0);
        assert_eq!(b.backends[0].allocated, 0);
        assert_eq!(b.total_allocated, 0);
    }

    #[test]
    fn reclaim_dead_backends_skips_own_pid() {
        let mut b = empty_budget();
        // Use our own PID — should NOT be reclaimed.
        let my_pid = std::process::id() as i32;
        b.backends[0] = BackendSlot {
            pid: my_pid,
            allocated: 2,
        };
        b.total_allocated = 2;

        reclaim_dead_backends(&mut b);

        // Our own PID slot should be untouched.
        assert_eq!(b.backends[0].pid, my_pid);
        assert_eq!(b.backends[0].allocated, 2);
        assert_eq!(b.total_allocated, 2);
    }

    #[test]
    fn reclaim_dead_backends_skips_empty_slots() {
        let mut b = empty_budget();
        // All slots empty — should not panic or change anything.
        reclaim_dead_backends(&mut b);
        assert_eq!(b.total_allocated, 0);
    }

    // -- request 0 threads ----------------------------------------------------

    #[test]
    fn request_zero_returns_zero() {
        assert_eq!(grant_from_budget(0, 8, 0), 0);
    }

    #[test]
    fn request_negative_returns_zero() {
        assert_eq!(grant_from_budget(-5, 8, 0), 0);
    }

    // -- request more than available ------------------------------------------

    #[test]
    fn request_exceeding_budget_gets_partial() {
        let max_total: i32 = 8;
        let mut b = empty_budget();
        b.total_allocated = 5;

        let requested = 10;
        let granted = grant_from_budget(requested, max_total, b.total_allocated);
        assert_eq!(granted, 3);

        // After granting, total should be at max.
        b.total_allocated += granted;
        assert_eq!(b.total_allocated, max_total);
    }

    #[test]
    fn request_when_fully_exhausted() {
        let max_total: i32 = 4;
        let mut b = empty_budget();
        b.total_allocated = 4;

        // Any request should get 0.
        let granted = grant_from_budget(5, max_total, b.total_allocated);
        assert_eq!(granted, 0);
    }

    // -- release tracking -----------------------------------------------------

    #[test]
    fn release_partial_keeps_remainder() {
        let mut slot = BackendSlot {
            pid: 50,
            allocated: 10,
        };
        let mut total = 10_i32;

        // Release 4 of 10.
        let release = 4;
        let actual = release.min(slot.allocated);
        slot.allocated -= actual;
        total -= actual;

        assert_eq!(slot.allocated, 6);
        assert_eq!(total, 6);
        // Slot still active.
        assert_eq!(slot.pid, 50);
    }

    #[test]
    fn release_exact_allocation_frees_slot() {
        let mut slot = BackendSlot {
            pid: 77,
            allocated: 5,
        };
        let mut total = 5_i32;

        let release = 5;
        let actual = release.min(slot.allocated);
        slot.allocated -= actual;
        total -= actual;

        assert_eq!(slot.allocated, 0);
        assert_eq!(total, 0);
        // In real code, slot.pid would be set to 0 when allocated == 0.
        if slot.allocated == 0 {
            slot.pid = 0;
        }
        assert_eq!(slot.pid, 0);
    }

    #[test]
    fn release_more_than_allocated_clamps() {
        let mut slot = BackendSlot {
            pid: 88,
            allocated: 2,
        };
        let mut total = 2_i32;

        let release = 100;
        let actual = release.min(slot.allocated);
        slot.allocated -= actual;
        total -= actual;

        assert_eq!(slot.allocated, 0);
        assert_eq!(actual, 2);
        assert_eq!(total, 0);
    }

    // -- saturating math (overflow safety) -----------------------------------

    #[test]
    fn saturating_add_checked_normal_case() {
        assert_eq!(saturating_add_checked(10, 5, "test"), 15);
        assert_eq!(saturating_add_checked(0, 0, "test"), 0);
    }

    #[test]
    fn saturating_add_checked_saturates_instead_of_wrapping() {
        // A plain i32 `+=` here would wrap to a negative value; the checked
        // helper must pin at i32::MAX instead so the budget never goes
        // negative (which would make grant_from_budget over-allocate).
        assert_eq!(saturating_add_checked(i32::MAX, 1, "test"), i32::MAX);
        assert_eq!(saturating_add_checked(i32::MAX - 3, 10, "test"), i32::MAX);
    }

    #[test]
    fn record_allocation_never_goes_negative_on_overflow() {
        // Seed a slot near the ceiling, then request more. Without saturating
        // math total_allocated would wrap negative.
        let mut b = empty_budget();
        let pid = current_pid();
        b.backends[0] = BackendSlot {
            pid,
            allocated: i32::MAX - 2,
        };
        b.total_allocated = i32::MAX - 2;

        record_allocation(&mut b, 100);

        assert!(
            b.total_allocated >= 0,
            "total_allocated wrapped negative: {}",
            b.total_allocated
        );
        assert_eq!(b.total_allocated, i32::MAX);
        assert_eq!(b.backends[0].allocated, i32::MAX);
    }

    #[test]
    fn record_allocation_ignores_non_positive() {
        let mut b = empty_budget();
        record_allocation(&mut b, 0);
        record_allocation(&mut b, -5);
        assert_eq!(b.total_allocated, 0);
    }
}

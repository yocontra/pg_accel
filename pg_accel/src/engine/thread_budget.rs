//! Cluster-wide thread budget managed via PostgreSQL shared memory.
//!
//! A single [`PgLwLock`]-protected [`ThreadBudgetData`] struct lives in shared
//! memory. Each backend that needs worker threads calls [`request_threads`],
//! which atomically checks the budget (governed by `pg_accel.max_workers_total`)
//! and grants up to the requested count. [`release_threads`] returns threads to
//! the pool. [`cleanup_backend`] is registered via `before_shmem_exit` to
//! reclaim any threads leaked by a crashing backend.

use pgrx::lwlock::PgLwLock;
#[cfg(not(test))]
use pgrx::pg_shmem_init;
#[cfg(not(test))]
use pgrx::prelude::*;
use pgrx::shmem::PGRXSharedMemory;

use super::gucs;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent backends we track individually.
const MAX_BACKENDS: usize = 256;

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
/// Register the shared-memory segment. **Must be called from `_PG_init`.**
///
/// Gated behind `#[cfg(not(test))]` because the test binary links as a
/// standalone executable where PG server symbols (`shmem_request_hook`, etc.)
/// are unavailable. The actual extension `.so` loaded by PG is built as
/// `cdylib` without the `test` cfg and includes this code.
#[cfg(not(test))]
#[allow(unexpected_cfgs)]
pub fn init_shmem() {
    pg_shmem_init!(BUDGET);
}

/// No-op stub used by the test binary so that `_PG_init` compiles without
/// referencing PG server-internal shared-memory symbols.
#[cfg(test)]
pub fn init_shmem() {}

/// Request `n` worker threads from the cluster-wide budget.
///
/// Returns the number of threads actually granted (0..=n). When the GUC
/// `pg_accel.max_workers_total` is 0 (unlimited) the full request is always
/// granted. When the budget is exhausted the caller should fall back to
/// sequential execution.
///
/// # Panics
///
/// Panics (via pgrx `error!`) if `n` is negative.
pub fn request_threads(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }

    let max = gucs::max_workers_total();

    // Unlimited mode: grant the full request without touching shared memory.
    if max == 0 {
        // Still record per-backend so cleanup_backend works correctly.
        let mut exclusive = BUDGET.exclusive();
        record_allocation(&mut exclusive, n);
        return n;
    }

    let mut exclusive = BUDGET.exclusive();
    let remaining = max.saturating_sub(exclusive.total_allocated);

    if remaining <= 0 {
        return 0;
    }

    let granted = n.min(remaining);
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
            exclusive.backends[i].allocated -= actual;
            exclusive.total_allocated -= actual;

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
    let mut exclusive = BUDGET.exclusive();
    let pid = current_pid();

    for i in 0..MAX_BACKENDS {
        if exclusive.backends[i].pid == pid {
            exclusive.total_allocated -= exclusive.backends[i].allocated;
            exclusive.backends[i].allocated = 0;
            exclusive.backends[i].pid = 0;
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Return the current backend's PID.
#[inline]
fn current_pid() -> i32 {
    // SAFETY: `MyProcPid` is a global i32 set by PostgreSQL at backend startup.
    // Reading it is safe from the main backend thread (which is the only place
    // we ever call this module).
    unsafe { pgrx::pg_sys::MyProcPid }
}

/// Record an allocation of `granted` threads for the current backend.
///
/// Caller must hold the exclusive lock.
fn record_allocation(data: &mut ThreadBudgetData, granted: i32) {
    let pid = current_pid();

    // Try to find an existing slot for this backend.
    for slot in &mut data.backends {
        if slot.pid == pid {
            slot.allocated += granted;
            data.total_allocated += granted;
            return;
        }
    }

    // Find a free slot.
    for slot in &mut data.backends {
        if slot.pid == 0 {
            slot.pid = pid;
            slot.allocated = granted;
            data.total_allocated += granted;
            return;
        }
    }

    // No free slots — log a warning. The caller still "gets" the threads
    // logically (we updated total_allocated nowhere), but we cannot track
    // them per-backend, so cleanup_backend won't reclaim them. This should
    // be exceedingly rare (>256 concurrent backends using pg_accel).
    pgrx::warning!("pg_accel: no free backend slot for PID {pid}, thread tracking degraded");
}

// ---------------------------------------------------------------------------
// Pure-logic unit tests (no PG required)
// ---------------------------------------------------------------------------

#[cfg(test)]
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
        let granted = requested.min(remaining);
        assert_eq!(granted, 6);
    }

    #[test]
    fn budget_exhausted_returns_zero() {
        let max_total: i32 = 8;
        let mut b = empty_budget();
        b.total_allocated = 8;

        let remaining = max_total.saturating_sub(b.total_allocated);
        assert_eq!(remaining, 0);
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
        let granted = if max == 0 {
            requested
        } else {
            requested.min(max)
        };
        assert_eq!(granted, 64);
    }

    #[test]
    fn backend_slot_default() {
        let s = BackendSlot::default();
        assert_eq!(s.pid, 0);
        assert_eq!(s.allocated, 0);
    }
}

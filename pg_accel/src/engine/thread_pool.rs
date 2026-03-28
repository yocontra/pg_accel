//! Per-backend rayon thread pool with signal safety.
//!
//! Rayon worker threads must **never** call PostgreSQL C functions — they exist
//! solely for GPU kernel orchestration, sort-key extraction, and top-k merge.
//! POSIX signals (SIGINT, SIGTERM, SIGUSR1, SIGUSR2) are masked in every
//! worker so that only the main backend thread handles them.

use std::cell::RefCell;
use std::thread;

/// Resolve the effective worker count from the GUC value.
///
/// * `guc == 0` → auto-detect: `available_parallelism() - 1`, minimum 1.
/// * `guc > 0`  → use that value directly.
fn effective_worker_count(guc: i32) -> usize {
    if guc > 0 {
        return guc as usize;
    }
    thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1)
}

/// Block delivery of SIGINT, SIGTERM, SIGUSR1 and SIGUSR2 in the calling
/// thread so that PostgreSQL signal handlers only fire on the main backend
/// thread.
fn mask_signals() {
    // SAFETY: We construct a valid sigset_t via libc helpers and pass it to
    // pthread_sigmask.  This is async-signal-safe and affects only the
    // calling thread.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGINT);
        libc::sigaddset(&raw mut set, libc::SIGTERM);
        libc::sigaddset(&raw mut set, libc::SIGUSR1);
        libc::sigaddset(&raw mut set, libc::SIGUSR2);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
    }
}

thread_local! {
    /// Per-backend pool stored in a `RefCell` so we can replace it on shutdown.
    static POOL: RefCell<Option<rayon::ThreadPool>> = const { RefCell::new(None) };
}

/// Build a new [`rayon::ThreadPool`] with signal-masked workers.
fn build_pool() -> Option<rayon::ThreadPool> {
    let n = effective_worker_count(super::gucs::workers());
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .start_handler(|_index| {
            mask_signals();
        })
        .thread_name(|i| format!("pgaccel-w{i}"))
        .build()
        .ok()
}

/// Run `f` on the per-backend rayon pool, lazily creating it on first call.
///
/// Returns `None` if the pool could not be created.
///
/// # Important
///
/// `f` must **not** invoke any PostgreSQL C functions — rayon workers are not
/// on the main backend thread.
pub fn with_pool<T>(f: impl FnOnce(&rayon::ThreadPool) -> T) -> Option<T> {
    POOL.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = build_pool();
        }
        borrow.as_ref().map(f)
    })
}

/// Shut down and drop the per-backend rayon pool.
///
/// Intended to be called from `before_shmem_exit` so that worker threads are
/// joined before the backend process exits.
pub fn shutdown_pool() {
    POOL.with(|cell| {
        let _ = cell.borrow_mut().take();
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_thread_count_is_at_least_one() {
        let count = effective_worker_count(0);
        assert!(count >= 1, "auto count must be >= 1, got {count}");
    }

    #[test]
    fn explicit_thread_count_honoured() {
        assert_eq!(effective_worker_count(4), 4);
        assert_eq!(effective_worker_count(1), 1);
    }

    #[test]
    fn shutdown_pool_is_idempotent() {
        // Calling shutdown when no pool exists must not panic.
        shutdown_pool();
        shutdown_pool();
    }

    #[test]
    fn pool_creates_with_correct_thread_count() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .start_handler(|_| mask_signals())
            .thread_name(|i| format!("pgaccel-test-w{i}"))
            .build();
        assert!(pool.is_ok(), "pool creation should succeed");
        let pool = pool.expect("tested above");
        assert_eq!(pool.current_num_threads(), 2);
    }
}

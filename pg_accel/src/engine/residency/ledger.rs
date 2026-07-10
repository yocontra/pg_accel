//! Cluster-wide resident-byte accounting and relation generations.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

use pgrx::pg_sys;

const MAX_BACKENDS: usize = 1024;
const MAX_GENERATIONS: usize = 4096;

#[derive(Debug, Clone, Copy, Default)]
struct BackendBytes {
    pid: i32,
    bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct RelationGeneration {
    database_oid: u32,
    relation_oid: u32,
    generation: u64,
}

#[derive(Debug)]
pub(super) struct ResidencyLedger {
    total_bytes: u64,
    global_generation: u64,
    backends: [BackendBytes; MAX_BACKENDS],
    generations: [RelationGeneration; MAX_GENERATIONS],
}

impl Default for ResidencyLedger {
    // The shared-memory value must remain one fixed-layout allocation; a
    // heap-backed generation table cannot live inside PostgreSQL shmem.
    #[allow(clippy::large_stack_arrays)]
    fn default() -> Self {
        Self {
            total_bytes: 0,
            global_generation: 1,
            backends: [BackendBytes::default(); MAX_BACKENDS],
            generations: [RelationGeneration::default(); MAX_GENERATIONS],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GenerationStamp {
    pub global: u64,
    pub relation: u64,
}

#[cfg(all(not(test), not(feature = "pg_test")))]
mod shared {
    use pgrx::lwlock::PgLwLock;
    use pgrx::pg_shmem_init;
    use pgrx::pg_sys;
    use pgrx::prelude::*;
    use pgrx::shmem::PGRXSharedMemory;

    use super::ResidencyLedger;

    // SAFETY: the ledger contains only fixed-size arrays of integer fields.
    // Every access is serialized by the enclosing PostgreSQL LWLock.
    unsafe impl PGRXSharedMemory for ResidencyLedger {}

    pub(super) static LEDGER: PgLwLock<ResidencyLedger> =
        // SAFETY: `_PG_init` calls `init` before any backend can access it.
        unsafe { PgLwLock::new(c"pg_accel_residency_ledger") };

    pub(super) fn init() {
        pg_shmem_init!(LEDGER);
    }

    pub(super) fn with_mut<R>(f: impl FnOnce(&mut ResidencyLedger) -> R) -> R {
        let mut ledger = LEDGER.exclusive();
        f(&mut ledger)
    }

    pub(super) fn with_ref<R>(f: impl FnOnce(&ResidencyLedger) -> R) -> R {
        let ledger = LEDGER.share();
        f(&ledger)
    }
}

#[cfg(any(test, feature = "pg_test"))]
mod shared {
    use std::sync::{LazyLock, Mutex};

    use super::ResidencyLedger;

    static LEDGER: LazyLock<Mutex<ResidencyLedger>> =
        LazyLock::new(|| Mutex::new(ResidencyLedger::default()));

    pub(super) fn init() {}

    pub(super) fn with_mut<R>(f: impl FnOnce(&mut ResidencyLedger) -> R) -> R {
        let mut ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut ledger)
    }

    pub(super) fn with_ref<R>(f: impl FnOnce(&ResidencyLedger) -> R) -> R {
        let ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&ledger)
    }
}

thread_local! {
    static PENDING_COMMIT_BUMPS: RefCell<BTreeSet<u32>> = const { RefCell::new(BTreeSet::new()) };
    static XACT_CALLBACKS_ARMED: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn init_shmem() {
    shared::init();
}

fn current_pid() -> i32 {
    #[cfg(any(test, feature = "pg_test"))]
    {
        1
    }
    #[cfg(all(not(test), not(feature = "pg_test")))]
    {
        // SAFETY: PostgreSQL initializes MyProcPid before loading extensions.
        unsafe { pg_sys::MyProcPid }
    }
}

fn current_database_oid() -> u32 {
    #[cfg(any(test, feature = "pg_test"))]
    {
        1
    }
    #[cfg(all(not(test), not(feature = "pg_test")))]
    {
        // SAFETY: PostgreSQL initializes MyDatabaseId before extension code is called.
        u32::from(unsafe { pg_sys::MyDatabaseId })
    }
}

fn backend_slot(ledger: &ResidencyLedger, pid: i32) -> Option<usize> {
    ledger
        .backends
        .iter()
        .position(|slot| slot.pid == pid)
        .or_else(|| ledger.backends.iter().position(|slot| slot.pid == 0))
}

#[cfg(any(test, not(feature = "pg_test")))]
fn reclaim_dead_backends_with(
    ledger: &mut ResidencyLedger,
    mut is_alive: impl FnMut(i32) -> bool,
) -> u64 {
    let my_pid = current_pid();
    let mut reclaimed = 0_u64;
    for slot in &mut ledger.backends {
        if slot.pid == 0 || slot.pid == my_pid || is_alive(slot.pid) {
            continue;
        }
        reclaimed = reclaimed
            .checked_add(slot.bytes)
            .expect("resident ledger reclaimed-byte total overflow");
        *slot = BackendBytes::default();
    }
    ledger.total_bytes = ledger
        .total_bytes
        .checked_sub(reclaimed)
        .expect("resident ledger total smaller than reclaimed backend bytes");
    reclaimed
}

// The production cfg mutates `ledger`; the unit-test cfg intentionally leaves
// the synthetic backend alone and exercises reclamation through the injected
// predicate helper.
#[allow(clippy::needless_pass_by_ref_mut)]
fn reclaim_dead_backends(ledger: &mut ResidencyLedger) {
    #[cfg(all(not(test), not(feature = "pg_test")))]
    {
        reclaim_dead_backends_with(ledger, |pid| {
            // SAFETY: BackendPidGetProc performs a process-array lookup and
            // returns NULL when no live PostgreSQL backend owns this PID.
            unsafe { !pg_sys::BackendPidGetProc(pid).is_null() }
        });
    }
    #[cfg(any(test, feature = "pg_test"))]
    {
        // The test ledger uses one synthetic backend. Unit tests exercise the
        // reclamation algorithm directly with an injected liveness predicate.
        let _ = ledger;
    }
}

/// RAII ownership of bytes reserved in the cluster-wide ledger.
#[derive(Debug)]
pub(super) struct LedgerCharge {
    bytes: u64,
}

impl LedgerCharge {
    pub(super) fn reserve(bytes: u64, budget: u64) -> Result<Self, u64> {
        if bytes == 0 {
            return Ok(Self { bytes: 0 });
        }
        let pid = current_pid();
        shared::with_mut(|ledger| {
            reclaim_dead_backends(ledger);
            let Some(next_total) = ledger.total_bytes.checked_add(bytes) else {
                return Err(ledger.total_bytes);
            };
            if next_total > budget {
                return Err(ledger.total_bytes);
            }
            let Some(index) = backend_slot(ledger, pid) else {
                return Err(ledger.total_bytes);
            };
            ledger.backends[index].pid = pid;
            ledger.backends[index].bytes = ledger.backends[index]
                .bytes
                .checked_add(bytes)
                .expect("resident backend byte total overflow after cluster-total check");
            ledger.total_bytes = next_total;
            Ok(Self { bytes })
        })
    }

    #[must_use]
    pub(super) const fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for LedgerCharge {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        let pid = current_pid();
        shared::with_mut(|ledger| {
            if let Some(index) = ledger.backends.iter().position(|slot| slot.pid == pid) {
                let released = self.bytes.min(ledger.backends[index].bytes);
                ledger.backends[index].bytes -= released;
                ledger.total_bytes = ledger
                    .total_bytes
                    .checked_sub(released)
                    .expect("resident backend charge exceeds cluster ledger during drop");
                if ledger.backends[index].bytes == 0 {
                    ledger.backends[index].pid = 0;
                }
            }
        });
    }
}

#[must_use]
pub(super) fn total_bytes() -> u64 {
    shared::with_ref(|ledger| ledger.total_bytes)
}

#[must_use]
pub(super) fn generation_stamp(relid: pg_sys::Oid) -> GenerationStamp {
    let database_oid = current_database_oid();
    let relation_oid = u32::from(relid);
    shared::with_ref(|ledger| {
        let relation = ledger
            .generations
            .iter()
            .find(|entry| entry.database_oid == database_oid && entry.relation_oid == relation_oid)
            .map_or(0, |entry| entry.generation);
        GenerationStamp {
            global: ledger.global_generation,
            relation,
        }
    })
}

fn bump_generation(relid: pg_sys::Oid) {
    let database_oid = current_database_oid();
    let relation_oid = u32::from(relid);
    shared::with_mut(|ledger| {
        if let Some(entry) = ledger
            .generations
            .iter_mut()
            .find(|entry| entry.database_oid == database_oid && entry.relation_oid == relation_oid)
        {
            entry.generation = entry.generation.wrapping_add(1).max(1);
            return;
        }
        if let Some(entry) = ledger
            .generations
            .iter_mut()
            .find(|entry| entry.relation_oid == 0)
        {
            *entry = RelationGeneration {
                database_oid,
                relation_oid,
                generation: 1,
            };
            return;
        }

        // Generation-table exhaustion must invalidate too much, never too
        // little. Advancing the global epoch makes every existing stamp stale.
        ledger.global_generation = ledger.global_generation.wrapping_add(1).max(1);
        ledger.generations.fill(RelationGeneration::default());
        ledger.generations[0] = RelationGeneration {
            database_oid,
            relation_oid,
            generation: 1,
        };
    });
}

/// Record a statement-level change immediately and once more after commit.
///
/// The immediate nontransactional bump makes the writing backend discard its
/// own old snapshot. The post-commit bump closes the concurrent-loader race:
/// a backend that reloaded the pre-commit snapshot after the immediate bump
/// is invalidated again once the changed rows become visible. Abort also
/// advances because this backend may have refreshed uncommitted rows after
/// the first bump. Extra bumps are harmless fail-closed invalidations.
pub(super) fn note_relation_change(relid: pg_sys::Oid) {
    bump_generation(relid);
    PENDING_COMMIT_BUMPS.with(|pending| {
        pending.borrow_mut().insert(u32::from(relid));
    });
    arm_xact_callbacks();
}

fn arm_xact_callbacks() {
    if XACT_CALLBACKS_ARMED.with(Cell::get) {
        return;
    }
    XACT_CALLBACKS_ARMED.with(|armed| armed.set(true));

    pgrx::register_xact_callback(pgrx::PgXactCallbackEvent::Commit, || {
        close_generation_window();
    });
    pgrx::register_xact_callback(pgrx::PgXactCallbackEvent::Abort, || {
        // A cache may have been reloaded with this transaction's uncommitted
        // rows after the immediate trigger bump. Abort must advance once more
        // so that rolled-back device data cannot retain the current stamp.
        close_generation_window();
    });
    pgrx::register_xact_callback(pgrx::PgXactCallbackEvent::PrePrepare, || {
        pgrx::error!(
            "pg_accel cannot PREPARE TRANSACTION after modifying a resident relation; \
             commit or roll back normally so the post-visibility generation bump can run"
        );
    });
    pgrx::register_subxact_callback(
        pgrx::PgSubXactCallbackEvent::AbortSub,
        |_my_subid, _parent_subid| {
            // A refresh inside the aborted subtransaction can contain rows
            // that disappear at ROLLBACK TO SAVEPOINT. Bump every relation
            // already pending in the outer transaction. Keeping the set
            // intact preserves the final commit/abort visibility bump.
            bump_pending_relations();
        },
    );
}

fn bump_pending_relations() {
    PENDING_COMMIT_BUMPS.with(|pending| {
        for raw in pending.borrow().iter().copied() {
            bump_generation(pg_sys::Oid::from(raw));
        }
    });
}

fn close_generation_window() {
    let relids = PENDING_COMMIT_BUMPS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    XACT_CALLBACKS_ARMED.with(|armed| armed.set(false));
    for raw in relids {
        bump_generation(pg_sys::Oid::from(raw));
    }
}

pub(super) fn cleanup_backend() {
    let pid = current_pid();
    shared::with_mut(|ledger| {
        if let Some(index) = ledger.backends.iter().position(|slot| slot.pid == pid) {
            ledger.total_bytes = ledger
                .total_bytes
                .checked_sub(ledger.backends[index].bytes)
                .expect("resident backend slot exceeds cluster ledger during cleanup");
            ledger.backends[index] = BackendBytes::default();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LEDGER_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LEDGER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn charge_tracks_exact_bytes_and_releases_on_drop() {
        let _guard = test_guard();
        cleanup_backend();
        assert_eq!(total_bytes(), 0);
        let charge = LedgerCharge::reserve(257, 1024).expect("reserve");
        assert_eq!(charge.bytes(), 257);
        assert_eq!(total_bytes(), 257);
        drop(charge);
        assert_eq!(total_bytes(), 0);
    }

    #[test]
    fn charge_rejects_cluster_budget_overflow() {
        let _guard = test_guard();
        cleanup_backend();
        let _first = LedgerCharge::reserve(800, 1024).expect("first reserve");
        assert_eq!(
            LedgerCharge::reserve(225, 1024).expect_err("reservation must exceed budget"),
            800
        );
    }

    #[test]
    fn dead_backend_slots_are_reclaimed_exactly() {
        let mut ledger = ResidencyLedger::default();
        ledger.backends[0] = BackendBytes {
            pid: 77,
            bytes: 123,
        };
        ledger.backends[1] = BackendBytes {
            pid: 88,
            bytes: 456,
        };
        ledger.total_bytes = 579;
        let reclaimed = reclaim_dead_backends_with(&mut ledger, |pid| pid == 88);
        assert_eq!(reclaimed, 123);
        assert_eq!(ledger.total_bytes, 456);
        assert_eq!(ledger.backends[0].pid, 0);
        assert_eq!(ledger.backends[1].bytes, 456);
    }

    #[test]
    fn relation_generation_is_database_scoped_and_monotonic() {
        let _guard = test_guard();
        let oid = pg_sys::Oid::from(4242_u32);
        let before = generation_stamp(oid);
        bump_generation(oid);
        let after = generation_stamp(oid);
        assert_eq!(after.global, before.global);
        assert!(after.relation > before.relation);
    }

    #[test]
    fn transaction_close_bumps_pending_relation_again() {
        let _guard = test_guard();
        let oid = pg_sys::Oid::from(5252_u32);
        bump_generation(oid);
        let after_statement = generation_stamp(oid);
        PENDING_COMMIT_BUMPS.with(|pending| {
            pending.borrow_mut().insert(u32::from(oid));
        });
        XACT_CALLBACKS_ARMED.with(|armed| armed.set(true));
        close_generation_window();
        let after_close = generation_stamp(oid);
        assert!(after_close.relation > after_statement.relation);
        assert!(!XACT_CALLBACKS_ARMED.with(Cell::get));
    }

    #[test]
    fn subtransaction_abort_bumps_without_closing_generation_window() {
        let _guard = test_guard();
        let oid = pg_sys::Oid::from(6262_u32);
        PENDING_COMMIT_BUMPS.with(|pending| {
            pending.borrow_mut().insert(u32::from(oid));
        });
        let before_abort = generation_stamp(oid);
        bump_pending_relations();
        let after_abort = generation_stamp(oid);
        assert!(after_abort.relation > before_abort.relation);
        assert!(PENDING_COMMIT_BUMPS.with(|pending| pending.borrow().contains(&u32::from(oid))));

        close_generation_window();
    }
}

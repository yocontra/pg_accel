//! Query-owned transport for final GPU aggregate tuples only.
//!
//! Drop NEVER calls PostgreSQL: tuplestore_end may flush and ERROR. Normal
//! close is explicit while the installed executor owns this record; caught
//! ERROR/reset detaches handles and leaves VFD/context cleanup to PostgreSQL.

use pgrx::pg_sys;

#[cfg(feature = "pg_test")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TestSpoolFailure {
    Begin,
    Put,
    Read,
    Close,
}

#[cfg(feature = "pg_test")]
thread_local! {
    static TEST_SPOOL_FAILURE: std::cell::Cell<Option<TestSpoolFailure>> = const { std::cell::Cell::new(None) };
}

#[cfg(feature = "pg_test")]
pub(super) fn set_test_spool_failure(point: Option<TestSpoolFailure>) -> Option<TestSpoolFailure> {
    TEST_SPOOL_FAILURE.with(|failure| failure.replace(point))
}

#[cfg(feature = "pg_test")]
fn test_spool_failure(point: TestSpoolFailure) {
    if TEST_SPOOL_FAILURE.with(|failure| {
        if failure.get() == Some(point) {
            failure.set(None);
            true
        } else {
            false
        }
    }) {
        pgrx::error!("pg_accel: injected final spool failure {point:?}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Empty,
    Building,
    Ready,
    Exhausted,
    Closing,
    Closed,
    Abandoned,
}

pub(super) struct FinalTupleSpool {
    context: pg_sys::MemoryContext,
    store: *mut pg_sys::Tuplestorestate,
    read_slot: *mut pg_sys::TupleTableSlot,
    phase: Phase,
    spilled: bool,
}

impl Default for FinalTupleSpool {
    fn default() -> Self {
        Self {
            context: std::ptr::null_mut(),
            store: std::ptr::null_mut(),
            read_slot: std::ptr::null_mut(),
            phase: Phase::Empty,
            spilled: false,
        }
    }
}

impl FinalTupleSpool {
    pub(super) fn spilled(&self) -> bool {
        self.spilled
    }

    /// Caller must install this owner before entry and catch PG errors outside
    /// all STORE/native leases, restoring entry memory context/resource owner.
    pub(super) unsafe fn begin(
        &mut self,
        parent: pg_sys::MemoryContext,
        output: *mut pg_sys::TupleTableSlot,
    ) {
        if self.phase != Phase::Empty || parent.is_null() || output.is_null() {
            pgrx::error!("pg_accel: invalid final spool creation state");
        }
        self.phase = Phase::Building;
        // SAFETY: parent is the live EState query context. Assign ownership
        // before another fallible call; PG owns this child on ERROR.
        self.context = unsafe {
            pg_sys::AllocSetContextCreateInternal(
                parent,
                c"pg_accel_hash_final_tuples".as_ptr(),
                pg_sys::ALLOCSET_DEFAULT_MINSIZE as pg_sys::Size,
                pg_sys::ALLOCSET_DEFAULT_INITSIZE as pg_sys::Size,
                pg_sys::ALLOCSET_DEFAULT_MAXSIZE as pg_sys::Size,
            )
        };
        // SAFETY: child is live; outer callback restores context/owner on ERROR.
        let old = unsafe { pg_sys::MemoryContextSwitchTo(self.context) };
        #[cfg(feature = "pg_test")]
        test_spool_failure(TestSpoolFailure::Begin);
        // SAFETY: non-interXact temporary files belong to the ambient query
        // resource owner, and memory belongs to this dedicated query child.
        self.store = unsafe { pg_sys::tuplestore_begin_heap(false, false, pg_sys::work_mem) };
        // SAFETY: the supplied executor slot owns a live tuple descriptor.
        // Tuplestore requires MinimalTuple operations, not CustomScan's Virtual
        // result slot. Install this child-owned handle before any later PG call.
        self.read_slot = unsafe {
            pg_sys::MakeSingleTupleTableSlot(
                (*output).tts_tupleDescriptor,
                &raw const pg_sys::TTSOpsMinimalTuple,
            )
        };
        // SAFETY: old context remains live throughout this operation.
        unsafe { pg_sys::MemoryContextSwitchTo(old) };
    }

    /// Copy a completed final tuple; never aggregate or retain source pointers.
    pub(super) unsafe fn put(&mut self, slot: *mut pg_sys::TupleTableSlot) {
        if self.phase != Phase::Building || self.store.is_null() {
            pgrx::error!("pg_accel: final spool write outside build phase");
        }
        // SAFETY: installed owner and its child context remain live; slot
        // contains one completed final tuple, with no STORE/native borrow.
        unsafe {
            let old = pg_sys::MemoryContextSwitchTo(self.context);
            #[cfg(feature = "pg_test")]
            test_spool_failure(TestSpoolFailure::Put);
            pg_sys::tuplestore_puttupleslot(self.store, slot);
            self.spilled |= !pg_sys::tuplestore_in_memory(self.store);
            pg_sys::MemoryContextSwitchTo(old);
        }
    }

    pub(super) unsafe fn finish(&mut self) {
        if self.phase != Phase::Building || self.store.is_null() {
            pgrx::error!("pg_accel: invalid final spool completion");
        }
        // SAFETY: all leaves succeeded and store is live. Rescan rewinds the
        // read pointer; the first read may still perform fallible file I/O.
        unsafe { pg_sys::tuplestore_rescan(self.store) };
        self.phase = Phase::Ready;
    }

    pub(super) unsafe fn next(&mut self, slot: *mut pg_sys::TupleTableSlot) -> bool {
        if matches!(self.phase, Phase::Exhausted | Phase::Closed) {
            return false;
        }
        if self.phase != Phase::Ready || self.store.is_null() {
            pgrx::error!("pg_accel: incomplete final spool cannot publish tuples");
        }
        // SAFETY: copy=true gives the slot independent tuple ownership; later
        // spool/context deletion cannot leave a slot pointing into that child.
        #[cfg(feature = "pg_test")]
        test_spool_failure(TestSpoolFailure::Read);
        let found =
            unsafe { pg_sys::tuplestore_gettupleslot(self.store, true, true, self.read_slot) };
        if found {
            // SAFETY: ExecCopySlot invokes the destination's slot operations
            // and materializes an independent copy before clearing the source.
            unsafe {
                pg_sys::ExecCopySlot(slot, self.read_slot);
                pg_sys::ExecClearTuple(self.read_slot);
            }
        }
        if !found {
            self.phase = Phase::Exhausted;
        }
        found
    }

    /// Fallible normal cleanup, called while executor remains installed.
    pub(super) unsafe fn close(&mut self) {
        if matches!(self.phase, Phase::Empty | Phase::Closed | Phase::Abandoned) {
            return;
        }
        if self.phase == Phase::Closing {
            pgrx::error!("pg_accel: final spool close cannot be retried");
        }
        self.phase = Phase::Closing;
        let read_slot = std::mem::replace(&mut self.read_slot, std::ptr::null_mut());
        // SAFETY: the slot is live only on normal cleanup; detach first in case
        // PostgreSQL raises while releasing its tuple descriptor reference.
        if !read_slot.is_null() {
            unsafe { pg_sys::ExecDropSingleTupleTableSlot(read_slot) };
        }
        let store = std::mem::replace(&mut self.store, std::ptr::null_mut());
        #[cfg(feature = "pg_test")]
        test_spool_failure(TestSpoolFailure::Close);
        // SAFETY: only normal cleanup reaches a live store. Taking the pointer
        // before this potentially flushing call prevents a second end on ERROR.
        if !store.is_null() {
            unsafe { pg_sys::tuplestore_end(store) };
        }
        let context = std::mem::replace(&mut self.context, std::ptr::null_mut());
        // SAFETY: tuplestore end succeeded and no slot references child memory.
        if !context.is_null() {
            unsafe { pg_sys::MemoryContextDelete(context) };
        }
        self.phase = Phase::Closed;
    }

    /// ERROR/reset backstop: no dereference, flush, PG API or second error.
    pub(super) fn abandon(&mut self) {
        self.context = std::ptr::null_mut();
        self.store = std::ptr::null_mut();
        self.read_slot = std::ptr::null_mut();
        self.phase = Phase::Abandoned;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abort_detaches_handles_without_dereferencing_or_flushing() {
        for phase in [
            Phase::Building,
            Phase::Ready,
            Phase::Exhausted,
            Phase::Closing,
        ] {
            let mut spool = FinalTupleSpool {
                phase,
                context: std::ptr::dangling_mut(),
                store: std::ptr::dangling_mut(),
                read_slot: std::ptr::dangling_mut(),
                spilled: false,
            };
            spool.abandon();
            assert_eq!(spool.phase, Phase::Abandoned);
            assert!(spool.context.is_null() && spool.store.is_null());
            spool.abandon();
        }
    }
}

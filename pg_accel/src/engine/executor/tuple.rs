//! MinimalTuple ownership helpers for executor buffers.
//!
//! PostgreSQL plan nodes commonly reuse their output slot. Executors that need
//! to buffer rows must copy the slot into a `MinimalTuple` and later free that
//! palloc'd copy. `OwnedMinimalTuple` provides a small RAII wrapper for buffered
//! tuple ownership.

use std::marker::PhantomData;
use std::rc::Rc;

use pgrx::pg_sys;

/// Owned palloc'd `MinimalTuple` copy.
///
/// The wrapper is intentionally not `Clone`, `Copy`, `Send`, or `Sync`.
/// PostgreSQL tuple memory belongs to one backend, and the tuple must have a
/// single freeing owner unless ownership is explicitly transferred with
/// [`OwnedMinimalTuple::into_raw`].
#[derive(Debug)]
pub struct OwnedMinimalTuple {
    tuple: pg_sys::MinimalTuple,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl OwnedMinimalTuple {
    /// Construct an empty owner.
    ///
    /// This is mainly useful as a sentinel during state construction and tests.
    #[must_use]
    pub const fn null() -> Self {
        Self {
            tuple: std::ptr::null_mut(),
            not_send_or_sync: PhantomData,
        }
    }

    /// Take ownership of a raw `MinimalTuple`.
    ///
    /// # Safety
    ///
    /// `tuple` must either be null or point to a `MinimalTuple` allocated with
    /// PostgreSQL allocation routines, such as `ExecCopySlotMinimalTuple`, and
    /// it must not be freed elsewhere after ownership is transferred here. This
    /// value must be dropped only while PostgreSQL `pfree` is valid for the
    /// current backend.
    #[must_use]
    pub unsafe fn from_raw(tuple: pg_sys::MinimalTuple) -> Self {
        Self {
            tuple,
            not_send_or_sync: PhantomData,
        }
    }

    /// Copy the current contents of `slot` into an owned `MinimalTuple`.
    ///
    /// # Safety
    ///
    /// Must run on the main PostgreSQL backend thread. `slot` must be a valid
    /// slot containing a tuple that `ExecCopySlotMinimalTuple` can copy.
    #[must_use]
    pub unsafe fn copy_from_slot(slot: *mut pg_sys::TupleTableSlot) -> Self {
        let tuple = unsafe { pg_sys::ExecCopySlotMinimalTuple(slot) };
        unsafe { Self::from_raw(tuple) }
    }

    /// Borrow the raw tuple pointer.
    #[must_use]
    pub const fn as_ptr(&self) -> pg_sys::MinimalTuple {
        self.tuple
    }

    /// Whether this owner currently holds a null tuple pointer.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.tuple.is_null()
    }

    /// Release ownership and return the raw tuple pointer.
    ///
    /// The caller becomes responsible for freeing the returned tuple or
    /// transferring it to another owner.
    #[must_use]
    pub fn into_raw(mut self) -> pg_sys::MinimalTuple {
        let tuple = self.tuple;
        self.tuple = std::ptr::null_mut();
        tuple
    }

    /// Remove and return the raw tuple pointer, leaving this owner empty.
    ///
    /// The caller becomes responsible for freeing the returned tuple or
    /// transferring it to another owner.
    #[must_use]
    pub fn take(&mut self) -> pg_sys::MinimalTuple {
        let tuple = self.tuple;
        self.tuple = std::ptr::null_mut();
        tuple
    }
}

impl Default for OwnedMinimalTuple {
    fn default() -> Self {
        Self::null()
    }
}

impl Drop for OwnedMinimalTuple {
    fn drop(&mut self) {
        if !self.tuple.is_null() {
            unsafe {
                pg_sys::pfree(self.tuple.cast());
            }
            self.tuple = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_owner_is_empty() {
        let owner = OwnedMinimalTuple::null();

        assert!(owner.is_null());
        assert!(owner.as_ptr().is_null());
    }

    #[test]
    fn default_owner_is_empty() {
        let owner = OwnedMinimalTuple::default();

        assert!(owner.is_null());
    }

    #[test]
    fn into_raw_releases_null_without_drop_work() {
        let owner = OwnedMinimalTuple::null();
        let raw = owner.into_raw();

        assert!(raw.is_null());
    }

    #[test]
    fn take_releases_and_leaves_owner_empty() {
        let mut owner = OwnedMinimalTuple::null();
        let raw = owner.take();

        assert!(raw.is_null());
        assert!(owner.is_null());
    }
}

use super::{ExprDeviceBuffer, PgaccelAggState, bridge};

// ---------------------------------------------------------------------------
// Hash aggregation wrappers
//
// Only the resident grouped-count lanes survive the Phase 3 demolition: the
// host-staged grouped aggregate executors that drove `hash_agg_execute` /
// `hash_agg_execute_partial` were deleted with their planner injectors, and
// those wrappers (plus their `results`/`partial_results` accessors and
// `pgaccel_hash_agg_execute*` extern declarations) went with them. The C
// kernels remain (Phase 4/5 scope).
// ---------------------------------------------------------------------------

/// Opaque wrapper around GPU hash aggregation state.
///
/// Owns the underlying C++ allocation and frees it on drop.
pub struct HashAggResult {
    state: *mut PgaccelAggState,
}

impl Drop for HashAggResult {
    fn drop(&mut self) {
        if !self.state.is_null() {
            // SAFETY: state was returned by a pgaccel hash-count kernel and is non-null.
            unsafe { bridge::pgaccel_agg_free(self.state) };
        }
    }
}

impl HashAggResult {
    /// Take ownership of a raw aggregation state returned by libpgaccel.
    ///
    /// # Safety
    ///
    /// `state` must either be null or point to a live `PgaccelAggState`
    /// allocation whose ownership is transferred to the returned wrapper.
    pub(crate) unsafe fn from_raw(state: *mut PgaccelAggState) -> Option<Self> {
        (!state.is_null()).then_some(Self { state })
    }

    /// Number of distinct groups.
    #[must_use]
    pub fn group_count(&self) -> usize {
        // SAFETY: state is a valid PgaccelAggState pointer (or null, handled by C).
        unsafe { bridge::pgaccel_agg_group_count(self.state) }
    }

    /// Raw pointer to the contiguous group key buffer.
    ///
    /// The buffer contains `group_count` keys packed according to `key_type`.
    #[must_use]
    pub fn group_keys_ptr(&self) -> *const std::ffi::c_void {
        // SAFETY: state is a valid PgaccelAggState pointer.
        unsafe { bridge::pgaccel_agg_get_group_keys(self.state) }
    }

    /// Get per-group row counts as a slice.
    ///
    /// Returns `None` if the pointer is null.
    #[must_use]
    pub fn counts(&self) -> Option<&[i64]> {
        let count = self.group_count();
        if count == 0 {
            return Some(&[]);
        }
        let ptr = unsafe {
            // SAFETY: state is valid.
            bridge::pgaccel_agg_get_counts(self.state)
        };

        if ptr.is_null() {
            return None;
        }
        // SAFETY: ptr points to count i64 values owned by state (valid until drop).
        Some(unsafe { std::slice::from_raw_parts(ptr, count) })
    }
}

/// Execute grouped COUNT(*) over an existing resident int64 key column.
///
/// The input buffer must be device-accessible and remains owned by the caller.
/// The C++ side copies compacted group keys/counts into the returned aggregate
/// state, so the result is independent of the input buffer lifetime.
#[allow(dead_code)] // reason: resident OLAP lanes may still need bounded hash-count when keys are not pre-sorted.
pub fn hash_count_i64_device_bounded(
    keys: &ExprDeviceBuffer<i64>,
    max_distinct_hint: usize,
) -> Option<HashAggResult> {
    if keys.len() == 0 {
        return None;
    }
    let state = unsafe {
        bridge::pgaccel_hash_count_i64_device_hash_execute_bounded(
            keys.as_mut_ptr(),
            keys.len(),
            max_distinct_hint,
        )
    };
    // SAFETY: state is either null or an owned pgaccel_agg_state allocation.
    unsafe { HashAggResult::from_raw(state) }
}

/// Execute grouped COUNT(*) over an already-sorted resident int64 key column.
pub fn hash_count_i64_sorted_device(keys: &ExprDeviceBuffer<i64>) -> Option<HashAggResult> {
    if keys.len() == 0 {
        return None;
    }
    let state = unsafe {
        bridge::pgaccel_hash_count_i64_sorted_device_execute(keys.as_mut_ptr(), keys.len())
    };
    // SAFETY: state is either null or an owned pgaccel_agg_state allocation.
    unsafe { HashAggResult::from_raw(state) }
}

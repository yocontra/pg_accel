use super::{ExprDeviceBuffer, PgaccelAggCol, PgaccelAggState, bridge};

// ---------------------------------------------------------------------------
// Hash aggregation wrappers
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
            // SAFETY: state was returned by pgaccel_hash_agg_execute and is non-null.
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

    /// Get aggregate results for one aggregate column as a slice.
    ///
    /// Returns `None` if the pointer is null.
    #[must_use]
    pub fn results(&self, agg_idx: usize) -> Option<&[f64]> {
        let count = self.group_count();
        if count == 0 {
            return Some(&[]);
        }
        let ptr = unsafe {
            // SAFETY: state is valid; agg_idx bounds are checked by C side.
            bridge::pgaccel_agg_get_results(self.state, agg_idx)
        };

        if ptr.is_null() {
            return None;
        }
        // SAFETY: ptr points to count f64 values owned by state (valid until drop).
        Some(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get per-group row counts as a slice.
    ///
    /// Returns `None` if the pointer is null.
    #[must_use]
    #[allow(dead_code)] // reason: per-group COUNT(*) accessor; executor reads results() today, counts() is for future GROUP BY+COUNT shapes
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

/// Execute GPU hash aggregation on columnar data.
///
/// # Arguments
///
/// * `group_keys` — raw pointer to contiguous group key values
/// * `group_null_mask` — null mask for group keys (1 = null)
/// * `row_count` — number of rows
/// * `key_type` — key type tag (0=i32, 1=i64, 2=f64)
/// * `value_cols` — per-aggregate column data pointers
/// * `value_nulls` — per-aggregate null masks
/// * `value_types` — per-aggregate value type tags
/// * `agg_cols` — aggregate column descriptors
///
/// Returns `None` if the GPU is unavailable or the call fails.
#[allow(clippy::too_many_arguments)]
pub fn hash_agg_execute(
    group_keys: *const std::ffi::c_void,
    group_null_mask: *const u8,
    row_count: usize,
    key_type: i32,
    value_cols: &[*const std::ffi::c_void],
    value_nulls: &[*const u8],
    value_types: &[i32],
    agg_cols: &[PgaccelAggCol],
) -> Option<HashAggResult> {
    let _span = tracing::info_span!(
        "gpu.hash_agg",
        n_rows = row_count,
        n_aggs = agg_cols.len(),
        key_type,
    )
    .entered();
    if row_count == 0 || agg_cols.is_empty() {
        return None;
    }

    let state = unsafe {
        // SAFETY: All pointers are caller-provided and valid for row_count elements.
        // value_cols, value_nulls, value_types have num_aggs elements each.
        bridge::pgaccel_hash_agg_execute(
            group_keys,
            group_null_mask,
            row_count,
            key_type,
            value_cols.as_ptr(),
            value_nulls.as_ptr(),
            value_types.as_ptr(),
            agg_cols.as_ptr(),
            agg_cols.len(),
        )
    };

    if state.is_null() {
        return None;
    }
    Some(HashAggResult { state })
}

/// Execute fail-closed grouped COUNT(*) over int64 keys.
///
/// This does not fall back to the legacy host hash-table group-assignment
/// path.
#[allow(dead_code)] // reason: generic count-only FFI wrapper; H3 currently uses fused lat/lng+count
pub fn hash_count_i64_execute(keys: &[i64], nulls: Option<&[u8]>) -> Option<HashAggResult> {
    if keys.is_empty() {
        return None;
    }
    let null_ptr = nulls.map_or(std::ptr::null(), |n| n.as_ptr());
    let state =
        unsafe { bridge::pgaccel_hash_count_i64_execute(keys.as_ptr(), null_ptr, keys.len()) };
    // SAFETY: state is either null or an owned pgaccel_agg_state allocation.
    unsafe { HashAggResult::from_raw(state) }
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

impl HashAggResult {
    /// Get **partial-mode** aggregate results for one aggregate column.
    ///
    /// Returns a slice of length `group_count * partial_width(func)`,
    /// laid out group-major: `[g0_lane0, g0_lane1, ..., g1_lane0, ...]`.
    ///
    /// For finalize-mode states (produced by `hash_agg_execute`), this
    /// is identical to [`results`](Self::results) — width is 1, so the
    /// slice has length `group_count`.
    ///
    /// Use [`partial_width`](Self::partial_width) to interpret the layout.
    #[must_use]
    pub fn partial_results(&self, agg_idx: usize) -> Option<&[f64]> {
        let count = self.group_count();
        if count == 0 {
            return Some(&[]);
        }
        // SAFETY: state is valid; agg_idx bounds checked by C side.
        let width = unsafe { bridge::pgaccel_agg_get_partial_width(self.state, agg_idx) };
        if width == 0 {
            return None;
        }
        // SAFETY: state is valid; agg_idx bounds checked by C side.
        let ptr = unsafe { bridge::pgaccel_agg_get_partial_results(self.state, agg_idx) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: ptr references count*width f64s owned by state (valid until drop).
        Some(unsafe { std::slice::from_raw_parts(ptr, count * width) })
    }

    /// Get the partial-mode lane width for one aggregate column.
    ///
    /// 1 for SUM/MIN/MAX/COUNT, 2 for AVG, 3 for STDDEV/VAR. Returns 0 when
    /// the state has no partial buffer for `agg_idx`.
    #[must_use]
    pub fn partial_width(&self, agg_idx: usize) -> usize {
        // SAFETY: state is valid; agg_idx bounds checked by C side.
        unsafe { bridge::pgaccel_agg_get_partial_width(self.state, agg_idx) }
    }
}

/// Execute GPU hash aggregation in **partial** mode (Phase 3B).
///
/// Same input shape as [`hash_agg_execute`], but per-group output carries
/// PG combine-function transition states for AVG / STDDEV / VAR — one
/// `[N, sum]` (AVG) or `[N, sum, sum_sq]` (STDDEV/VAR) tuple per group.
/// SUM / MIN / MAX / COUNT keep their finalize-mode width-1 shape.
///
/// Use [`HashAggResult::partial_results`] (returns slice of width × n_groups
/// f64s) to read each agg's per-group state; the planner / executor
/// converts these to the matching float8[] / bytea Datums PG's combine
/// functions accept.
#[allow(clippy::too_many_arguments)]
pub fn hash_agg_execute_partial(
    group_keys: *const std::ffi::c_void,
    group_null_mask: *const u8,
    row_count: usize,
    key_type: i32,
    value_cols: &[*const std::ffi::c_void],
    value_nulls: &[*const u8],
    value_types: &[i32],
    agg_cols: &[PgaccelAggCol],
) -> Option<HashAggResult> {
    let _span = tracing::info_span!(
        "gpu.hash_agg_partial",
        n_rows = row_count,
        n_aggs = agg_cols.len(),
        key_type,
    )
    .entered();
    if row_count == 0 || agg_cols.is_empty() {
        return None;
    }

    // SAFETY: All pointers are caller-provided and valid for row_count
    // elements. value_cols, value_nulls, value_types have num_aggs
    // elements each.
    let state = unsafe {
        bridge::pgaccel_hash_agg_execute_partial(
            group_keys,
            group_null_mask,
            row_count,
            key_type,
            value_cols.as_ptr(),
            value_nulls.as_ptr(),
            value_types.as_ptr(),
            agg_cols.as_ptr(),
            agg_cols.len(),
        )
    };

    if state.is_null() {
        return None;
    }
    Some(HashAggResult { state })
}

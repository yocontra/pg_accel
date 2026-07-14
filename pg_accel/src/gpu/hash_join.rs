use super::{PgaccelHashTable, PgaccelKeyType, bridge};

// ---------------------------------------------------------------------------
// Hash join wrappers
// ---------------------------------------------------------------------------

/// RAII wrapper around a GPU-side hash table.
pub struct GpuHashTable {
    ht: *mut PgaccelHashTable,
}

impl Drop for GpuHashTable {
    fn drop(&mut self) {
        if !self.ht.is_null() {
            // SAFETY: ht was returned by pgaccel_hash_join_build and is non-null.
            unsafe { bridge::pgaccel_hash_join_free(self.ht) };
        }
    }
}

impl GpuHashTable {
    /// Build a hash table from inner relation keys.
    ///
    /// * `keys` — pointer to typed key array (int32/int64/float64)
    /// * `null_mask` — per-row null mask (1 = null, excluded from table)
    /// * `indices` — original row indices for mapping probe results back
    /// * `key_type` — the key type tag
    ///
    /// Returns `None` if the GPU is unavailable or the build fails.
    pub fn build(
        keys: *const std::ffi::c_void,
        null_mask: &[u8],
        indices: &[u32],
        key_type: PgaccelKeyType,
    ) -> Option<Self> {
        let count = null_mask.len().min(indices.len());
        if count == 0 {
            return None;
        }

        crate::ensure_backend_exit_callback();
        let ht = unsafe {
            // SAFETY: keys points to `count` elements of the specified type.
            // null_mask and indices are valid slices with at least `count` elements.
            bridge::pgaccel_hash_join_build(
                keys,
                null_mask.as_ptr(),
                indices.as_ptr(),
                count,
                key_type,
            )
        };

        if ht.is_null() {
            None
        } else {
            crate::note_backend_gpu_owner_acquired();
            Some(Self { ht })
        }
    }

    /// Probe the hash table with outer relation keys.
    ///
    /// Returns matched `(outer_idx, inner_idx)` pairs, or `None` on failure.
    pub fn probe(
        &self,
        outer_keys: *const std::ffi::c_void,
        outer_null_mask: &[u8],
        max_matches: usize,
    ) -> Option<Vec<(u32, u32)>> {
        if self.ht.is_null() || outer_null_mask.is_empty() {
            return None;
        }
        let outer_count = outer_null_mask.len();
        let buf_len = max_matches.checked_mul(2)?;
        let mut match_pairs = vec![0u32; buf_len];
        let mut match_count: usize = 0;

        let status = unsafe {
            // SAFETY: ht is non-null (checked above). outer_keys points to
            // outer_count elements. match_pairs has capacity for max_matches*2.
            bridge::pgaccel_hash_join_probe(
                self.ht,
                outer_keys,
                outer_null_mask.as_ptr(),
                outer_count,
                match_pairs.as_mut_ptr(),
                max_matches,
                std::ptr::addr_of_mut!(match_count),
            )
        };

        if !status.is_ok() {
            return None;
        }

        let pair_u32s = probe_pair_slice_len(match_count, max_matches, match_pairs.len())?;
        let pairs = match_pairs[..pair_u32s]
            .chunks_exact(2)
            .map(|c| (c[0], c[1]))
            .collect();
        Some(pairs)
    }

    /// Count matches for an outer key batch without materializing row pairs.
    pub fn count_matches(
        &self,
        outer_keys: *const std::ffi::c_void,
        outer_null_mask: &[u8],
    ) -> Option<usize> {
        if self.ht.is_null() || outer_null_mask.is_empty() {
            return Some(0);
        }
        let outer_count = outer_null_mask.len();
        let mut match_count: usize = 0;

        let status = unsafe {
            // SAFETY: ht is non-null (checked above). outer_keys points to
            // outer_count elements matching the table key type.
            bridge::pgaccel_hash_join_count(
                self.ht,
                outer_keys,
                outer_null_mask.as_ptr(),
                outer_count,
                std::ptr::addr_of_mut!(match_count),
            )
        };

        status.is_ok().then_some(match_count)
    }
}

#[must_use]
fn probe_pair_slice_len(
    match_count: usize,
    max_matches: usize,
    match_buffer_u32s: usize,
) -> Option<usize> {
    if match_count > max_matches {
        return None;
    }
    let pair_u32s = match_count.checked_mul(2)?;
    (pair_u32s <= match_buffer_u32s).then_some(pair_u32s)
}

#[cfg(test)]
mod tests {
    use super::probe_pair_slice_len;

    #[test]
    fn probe_pair_slice_len_rejects_over_reported_match_count() {
        assert_eq!(probe_pair_slice_len(4, 4, 8), Some(8));
        assert_eq!(probe_pair_slice_len(5, 4, 8), None);
    }

    #[test]
    fn probe_pair_slice_len_rejects_overflow_and_short_buffers() {
        assert_eq!(
            probe_pair_slice_len((usize::MAX / 2) + 1, usize::MAX, usize::MAX),
            None
        );
        assert_eq!(probe_pair_slice_len(4, 4, 7), None);
    }
}

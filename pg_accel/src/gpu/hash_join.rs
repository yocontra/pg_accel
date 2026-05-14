use super::{PgaccelHashTable, PgaccelKeyType, bridge};

// ---------------------------------------------------------------------------
// Hash join wrappers
// ---------------------------------------------------------------------------

/// RAII wrapper around a GPU-side hash table.
pub struct GpuHashTable {
    ht: *mut PgaccelHashTable,
}

// SAFETY: The hash table handle is only accessed from the main backend thread
// via the safe wrapper methods. No concurrent access occurs.
unsafe impl Send for GpuHashTable {}

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
        let buf_len = max_matches * 2;
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

        let pairs = match_pairs[..match_count * 2]
            .chunks_exact(2)
            .map(|c| (c[0], c[1]))
            .collect();
        Some(pairs)
    }
}

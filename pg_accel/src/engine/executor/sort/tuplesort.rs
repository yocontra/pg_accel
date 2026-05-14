//! GPU sort kernel helpers — chunked bitonic/radix sort for scalar keys.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::engine::cost;
use crate::gpu;

/// PG type OIDs for GPU-sortable numeric types.
pub(super) const INT4OID: u32 = 23;
pub(super) const INT8OID: u32 = 20;
pub(super) const FLOAT4OID: u32 = 700;
pub(super) const FLOAT8OID: u32 = 701;

// ---------------------------------------------------------------------------
// Chunked GPU sort — handles arbitrary row counts
// ---------------------------------------------------------------------------

pub(super) fn f32_to_sort_key(f: f32) -> u32 {
    let bits = f.to_bits();
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000
    }
}

/// Convert f64 to u64 preserving total order (for k-way merge comparison).
pub(super) fn f64_to_sort_key(f: f64) -> u64 {
    let bits = f.to_bits();
    if bits & 0x8000_0000_0000_0000 != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000_0000_0000
    }
}

/// GPU-sort f32 keys in chunks, returning the merged global permutation.
///
/// Handles arbitrary input sizes by splitting into chunks of
/// `gpu_sort_max_elements`, GPU-sorting each independently, and
/// performing a k-way merge of the sorted chunks.
pub(super) fn gpu_sort_chunked_f32(keys: &[f32]) -> Option<Vec<u32>> {
    let chunk_size = cost::device_limits().gpu_sort_max_elements;
    let n = keys.len();

    if n <= chunk_size {
        let mut k = keys.to_vec();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        gpu::sort_kv_f32(&mut k, &mut idx)?;
        return Some(idx);
    }

    // Multi-chunk: sort each chunk independently, then k-way merge.
    let num_chunks = n.div_ceil(chunk_size);
    let mut chunk_keys: Vec<Vec<f32>> = Vec::with_capacity(num_chunks);
    let mut chunk_globals: Vec<Vec<u32>> = Vec::with_capacity(num_chunks);

    for start in (0..n).step_by(chunk_size) {
        let end = (start + chunk_size).min(n);
        let mut ck = keys[start..end].to_vec();
        let mut ci: Vec<u32> = (0..ck.len() as u32).collect();
        gpu::sort_kv_f32(&mut ck, &mut ci)?;
        let gi: Vec<u32> = ci.iter().map(|&i| start as u32 + i).collect();
        chunk_keys.push(ck);
        chunk_globals.push(gi);
        pgrx::check_for_interrupts!();
    }

    // K-way merge via min-heap on sort-order-preserving u32 keys.
    let mut positions = vec![0usize; num_chunks];
    let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::with_capacity(num_chunks);
    for (ci, ck) in chunk_keys.iter().enumerate() {
        if !ck.is_empty() {
            heap.push(Reverse((f32_to_sort_key(ck[0]), ci)));
        }
    }

    let mut merged = Vec::with_capacity(n);
    while let Some(Reverse((_, ci))) = heap.pop() {
        let pos = positions[ci];
        merged.push(chunk_globals[ci][pos]);
        positions[ci] = pos + 1;
        if positions[ci] < chunk_keys[ci].len() {
            heap.push(Reverse((
                f32_to_sort_key(chunk_keys[ci][positions[ci]]),
                ci,
            )));
        }
    }

    Some(merged)
}

/// GPU-sort f64 keys in chunks, returning the merged global permutation.
///
/// Same algorithm as [`gpu_sort_chunked_f32`] but for f64 keys.
pub(super) fn gpu_sort_chunked_f64(keys: &[f64]) -> Option<Vec<u32>> {
    let chunk_size = cost::device_limits().gpu_sort_max_elements;
    let n = keys.len();

    if n <= chunk_size {
        let mut k = keys.to_vec();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        gpu::sort_kv_f64(&mut k, &mut idx)?;
        return Some(idx);
    }

    let num_chunks = n.div_ceil(chunk_size);
    let mut chunk_keys: Vec<Vec<f64>> = Vec::with_capacity(num_chunks);
    let mut chunk_globals: Vec<Vec<u32>> = Vec::with_capacity(num_chunks);

    for start in (0..n).step_by(chunk_size) {
        let end = (start + chunk_size).min(n);
        let mut ck = keys[start..end].to_vec();
        let mut ci: Vec<u32> = (0..ck.len() as u32).collect();
        gpu::sort_kv_f64(&mut ck, &mut ci)?;
        let gi: Vec<u32> = ci.iter().map(|&i| start as u32 + i).collect();
        chunk_keys.push(ck);
        chunk_globals.push(gi);
        pgrx::check_for_interrupts!();
    }

    let mut positions = vec![0usize; num_chunks];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::with_capacity(num_chunks);
    for (ci, ck) in chunk_keys.iter().enumerate() {
        if !ck.is_empty() {
            heap.push(Reverse((f64_to_sort_key(ck[0]), ci)));
        }
    }

    let mut merged = Vec::with_capacity(n);
    while let Some(Reverse((_, ci))) = heap.pop() {
        let pos = positions[ci];
        merged.push(chunk_globals[ci][pos]);
        positions[ci] = pos + 1;
        if positions[ci] < chunk_keys[ci].len() {
            heap.push(Reverse((
                f64_to_sort_key(chunk_keys[ci][positions[ci]]),
                ci,
            )));
        }
    }

    Some(merged)
}

/// GPU-sort i32 keys in chunks, returning the merged global permutation.
pub(super) fn gpu_sort_chunked_i32(keys: &[i32]) -> Option<Vec<u32>> {
    let chunk_size = cost::device_limits().gpu_sort_max_elements;
    let n = keys.len();

    if n <= chunk_size {
        let mut k = keys.to_vec();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        gpu::sort_kv_i32(&mut k, &mut idx)?;
        return Some(idx);
    }

    let num_chunks = n.div_ceil(chunk_size);
    let mut chunk_keys: Vec<Vec<i32>> = Vec::with_capacity(num_chunks);
    let mut chunk_globals: Vec<Vec<u32>> = Vec::with_capacity(num_chunks);

    for start in (0..n).step_by(chunk_size) {
        let end = (start + chunk_size).min(n);
        let mut ck = keys[start..end].to_vec();
        let mut ci: Vec<u32> = (0..ck.len() as u32).collect();
        gpu::sort_kv_i32(&mut ck, &mut ci)?;
        let gi: Vec<u32> = ci.iter().map(|&i| start as u32 + i).collect();
        chunk_keys.push(ck);
        chunk_globals.push(gi);
        pgrx::check_for_interrupts!();
    }

    let mut positions = vec![0usize; num_chunks];
    let mut heap: BinaryHeap<Reverse<(i32, usize)>> = BinaryHeap::with_capacity(num_chunks);
    for (ci, ck) in chunk_keys.iter().enumerate() {
        if !ck.is_empty() {
            heap.push(Reverse((ck[0], ci)));
        }
    }

    let mut merged = Vec::with_capacity(n);
    while let Some(Reverse((_, ci))) = heap.pop() {
        let pos = positions[ci];
        merged.push(chunk_globals[ci][pos]);
        positions[ci] = pos + 1;
        if positions[ci] < chunk_keys[ci].len() {
            heap.push(Reverse((chunk_keys[ci][positions[ci]], ci)));
        }
    }

    Some(merged)
}

/// GPU-sort i64 keys in chunks, returning the merged global permutation.
pub(super) fn gpu_sort_chunked_i64(keys: &[i64]) -> Option<Vec<u32>> {
    let chunk_size = cost::device_limits().gpu_sort_max_elements;
    let n = keys.len();

    if n <= chunk_size {
        let mut k = keys.to_vec();
        let mut idx: Vec<u32> = (0..n as u32).collect();
        gpu::sort_kv_i64(&mut k, &mut idx)?;
        return Some(idx);
    }

    let num_chunks = n.div_ceil(chunk_size);
    let mut chunk_keys: Vec<Vec<i64>> = Vec::with_capacity(num_chunks);
    let mut chunk_globals: Vec<Vec<u32>> = Vec::with_capacity(num_chunks);

    for start in (0..n).step_by(chunk_size) {
        let end = (start + chunk_size).min(n);
        let mut ck = keys[start..end].to_vec();
        let mut ci: Vec<u32> = (0..ck.len() as u32).collect();
        gpu::sort_kv_i64(&mut ck, &mut ci)?;
        let gi: Vec<u32> = ci.iter().map(|&i| start as u32 + i).collect();
        chunk_keys.push(ck);
        chunk_globals.push(gi);
        pgrx::check_for_interrupts!();
    }

    let mut positions = vec![0usize; num_chunks];
    let mut heap: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::with_capacity(num_chunks);
    for (ci, ck) in chunk_keys.iter().enumerate() {
        if !ck.is_empty() {
            heap.push(Reverse((ck[0], ci)));
        }
    }

    let mut merged = Vec::with_capacity(n);
    while let Some(Reverse((_, ci))) = heap.pop() {
        let pos = positions[ci];
        merged.push(chunk_globals[ci][pos]);
        positions[ci] = pos + 1;
        if positions[ci] < chunk_keys[ci].len() {
            heap.push(Reverse((chunk_keys[ci][positions[ci]], ci)));
        }
    }

    Some(merged)
}

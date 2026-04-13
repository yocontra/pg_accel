// sort.metal — GPU sort kernels (bitonic + radix) for uint32/uint64 keys.
//
// All types are converted to sortable uint on the CPU side before dispatch:
//   f32 → u32 (via float-to-sortable encoding), i32 → u32 (flip sign bit),
//   i64 → u64 (flip sign bit). This keeps the GPU kernels simple and uniform.
//
// Two algorithms:
//   1. Bitonic sort — O(n·log²n), one dispatch per network step, all steps
//      batched into a single command buffer with memory barriers.
//   2. LSD Radix sort — O(n·w), 4 passes for u32, 8 for u64. Each pass:
//      histogram kernel → CPU prefix scan → scatter kernel.

#include <metal_stdlib>
using namespace metal;

constant uint WG_SIZE = 256;
constant uint RADIX_BINS = 256;

// ── Bitonic sort step (key-value, uint32 keys) ────────────────────
// One step of the bitonic network. Stable: equal keys break ties by index.

kernel void bitonic_step_kv_u32(
    device uint* keys [[buffer(0)]],
    device uint* indices [[buffer(1)]],
    constant uint& k_param [[buffer(2)]],
    constant uint& j_param [[buffer(3)]],
    constant uint& padded_count [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    uint i = gid;
    uint partner = i ^ j_param;
    if (partner <= i || partner >= padded_count) return;

    bool ascending = ((i & k_param) == 0);
    uint ki = keys[i];
    uint kp = keys[partner];
    uint ii = indices[i];
    uint ip = indices[partner];

    bool should_swap;
    if (ki == kp) {
        should_swap = ascending ? (ii > ip) : (ii < ip);
    } else {
        should_swap = ascending ? (kp < ki) : (ki < kp);
    }

    if (should_swap) {
        keys[i] = kp;    keys[partner] = ki;
        indices[i] = ip;  indices[partner] = ii;
    }
}

// ── Bitonic sort step (key-value, uint64 keys) ────────────────────

kernel void bitonic_step_kv_u64(
    device ulong* keys [[buffer(0)]],
    device uint* indices [[buffer(1)]],
    constant uint& k_param [[buffer(2)]],
    constant uint& j_param [[buffer(3)]],
    constant uint& padded_count [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    uint i = gid;
    uint partner = i ^ j_param;
    if (partner <= i || partner >= padded_count) return;

    bool ascending = ((i & k_param) == 0);
    ulong ki = keys[i];
    ulong kp = keys[partner];
    uint ii = indices[i];
    uint ip = indices[partner];

    bool should_swap;
    if (ki == kp) {
        should_swap = ascending ? (ii > ip) : (ii < ip);
    } else {
        should_swap = ascending ? (kp < ki) : (ki < kp);
    }

    if (should_swap) {
        keys[i] = kp;    keys[partner] = ki;
        indices[i] = ip;  indices[partner] = ii;
    }
}

// ── Radix histogram (uint32 keys) ─────────────────────────────────
// Each threadgroup processes one tile of WG_SIZE elements. Builds a
// 256-bin histogram in threadgroup memory via local atomics, then
// writes it to the global per-group histogram buffer.

kernel void radix_histogram_u32(
    device const uint* keys [[buffer(0)]],
    device uint* group_hist [[buffer(1)]],
    constant uint& shift [[buffer(2)]],
    constant uint& padded_count [[buffer(3)]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup atomic_uint local_hist[RADIX_BINS];

    // Zero local histogram (WG_SIZE == RADIX_BINS == 256)
    atomic_store_explicit(&local_hist[lid], 0, memory_order_relaxed);
    threadgroup_barrier(mem_flags::mem_threadgroup);

    uint idx = group_id * WG_SIZE + lid;
    if (idx < padded_count) {
        uint digit = (keys[idx] >> shift) & 0xFFu;
        atomic_fetch_add_explicit(&local_hist[digit], 1u, memory_order_relaxed);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // Write to global histogram
    group_hist[group_id * RADIX_BINS + lid] =
        atomic_load_explicit(&local_hist[lid], memory_order_relaxed);
}

// ── Radix histogram (uint64 keys) ─────────────────────────────────

kernel void radix_histogram_u64(
    device const ulong* keys [[buffer(0)]],
    device uint* group_hist [[buffer(1)]],
    constant uint& shift [[buffer(2)]],
    constant uint& padded_count [[buffer(3)]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup atomic_uint local_hist[RADIX_BINS];

    atomic_store_explicit(&local_hist[lid], 0, memory_order_relaxed);
    threadgroup_barrier(mem_flags::mem_threadgroup);

    uint idx = group_id * WG_SIZE + lid;
    if (idx < padded_count) {
        uint digit = uint((keys[idx] >> ulong(shift)) & 0xFFUL);
        atomic_fetch_add_explicit(&local_hist[digit], 1u, memory_order_relaxed);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    group_hist[group_id * RADIX_BINS + lid] =
        atomic_load_explicit(&local_hist[lid], memory_order_relaxed);
}

// ── Radix scatter (uint32 keys + uint32 indices) ──────────────────
// Stable scatter: each threadgroup reloads its tile, computes per-element
// rank within the tile's contribution to each bin (by counting predecessors
// with the same digit), then writes to the destination at
// group_base[group][digit] + rank.

kernel void radix_scatter_kv_u32(
    device const uint* src_keys [[buffer(0)]],
    device const uint* src_idx [[buffer(1)]],
    device uint* dst_keys [[buffer(2)]],
    device uint* dst_idx [[buffer(3)]],
    device const uint* group_base [[buffer(4)]],
    constant uint& shift [[buffer(5)]],
    constant uint& padded_count [[buffer(6)]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup uint local_digits[WG_SIZE];

    uint tile_start = group_id * WG_SIZE;
    uint my_key = 0;
    uint my_idx = 0;
    uint my_digit = 0xFFFFFFFFu;  // sentinel for out-of-range

    if (tile_start + lid < padded_count) {
        my_key = src_keys[tile_start + lid];
        my_idx = src_idx[tile_start + lid];
        my_digit = (my_key >> shift) & 0xFFu;
    }
    local_digits[lid] = my_digit;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    if (my_digit != 0xFFFFFFFFu) {
        // Count predecessors with same digit for stable rank
        uint rank = 0;
        for (uint i = 0; i < lid; ++i) {
            if (local_digits[i] == my_digit) rank++;
        }
        uint base = group_base[group_id * RADIX_BINS + my_digit];
        dst_keys[base + rank] = my_key;
        dst_idx[base + rank] = my_idx;
    }
}

// ── Radix scatter (uint64 keys + uint32 indices) ──────────────────

kernel void radix_scatter_kv_u64(
    device const ulong* src_keys [[buffer(0)]],
    device const uint* src_idx [[buffer(1)]],
    device ulong* dst_keys [[buffer(2)]],
    device uint* dst_idx [[buffer(3)]],
    device const uint* group_base [[buffer(4)]],
    constant uint& shift [[buffer(5)]],
    constant uint& padded_count [[buffer(6)]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup uint local_digits[WG_SIZE];

    uint tile_start = group_id * WG_SIZE;
    ulong my_key = 0;
    uint my_idx = 0;
    uint my_digit = 0xFFFFFFFFu;

    if (tile_start + lid < padded_count) {
        my_key = src_keys[tile_start + lid];
        my_idx = src_idx[tile_start + lid];
        my_digit = uint((my_key >> ulong(shift)) & 0xFFUL);
    }
    local_digits[lid] = my_digit;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    if (my_digit != 0xFFFFFFFFu) {
        uint rank = 0;
        for (uint i = 0; i < lid; ++i) {
            if (local_digits[i] == my_digit) rank++;
        }
        uint base = group_base[group_id * RADIX_BINS + my_digit];
        dst_keys[base + rank] = my_key;
        dst_idx[base + rank] = my_idx;
    }
}

// reduce.metal — GPU reduction kernels (sum, min, max, count, multi-agg)
//
// Tree reduction using threadgroup shared memory. Each threadgroup reduces
// WG_SIZE elements to a single partial result. Host sums the partials.
//
// All kernels use float (fp32). fp64 is emulated as 2x float where needed
// (handled on the C++ side by casting). i64 uses int/long atomics.

#include <metal_stdlib>
using namespace metal;

constant uint WG_SIZE = 256;

// ── reduce_sum_f32 ──────────────────────────────────────────────────

kernel void reduce_sum_f32(
    device const float* input [[buffer(0)]],
    device float* partials [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup float shared[WG_SIZE];
    shared[lid] = (gid < count) ? input[gid] : 0.0f;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) shared[lid] += shared[lid + s];
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) partials[group_id] = shared[0];
}

// ── reduce_min_f32 ──────────────────────────────────────────────────

kernel void reduce_min_f32(
    device const float* input [[buffer(0)]],
    device float* partials [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup float shared[WG_SIZE];
    shared[lid] = (gid < count) ? input[gid] : HUGE_VALF;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) shared[lid] = min(shared[lid], shared[lid + s]);
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) partials[group_id] = shared[0];
}

// ── reduce_max_f32 ──────────────────────────────────────────────────

kernel void reduce_max_f32(
    device const float* input [[buffer(0)]],
    device float* partials [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup float shared[WG_SIZE];
    shared[lid] = (gid < count) ? input[gid] : -HUGE_VALF;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) shared[lid] = max(shared[lid], shared[lid + s]);
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) partials[group_id] = shared[0];
}

// ── reduce_sum_i64 ──────────────────────────────────────────────────
// Metal uses `long` for 64-bit integers.

kernel void reduce_sum_i64(
    device const long* input [[buffer(0)]],
    device long* partials [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup long shared[WG_SIZE];
    shared[lid] = (gid < count) ? input[gid] : 0L;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) shared[lid] += shared[lid + s];
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) partials[group_id] = shared[0];
}

// ── reduce_count (popcount of mask bytes) ───────────────────────────

kernel void reduce_count(
    device const uchar* mask [[buffer(0)]],
    device uint* partials [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup uint shared[WG_SIZE];
    shared[lid] = (gid < count && mask[gid] != 0) ? 1u : 0u;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) shared[lid] += shared[lid + s];
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) partials[group_id] = shared[0];
}

// ── reduce_multi_f32 (fused SUM+MIN+MAX+COUNT in one pass) ─────────

kernel void reduce_multi_f32(
    device const float* input [[buffer(0)]],
    device float* partial_sum [[buffer(1)]],
    device float* partial_min [[buffer(2)]],
    device float* partial_max [[buffer(3)]],
    constant uint& count [[buffer(4)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup float s_sum[WG_SIZE];
    threadgroup float s_min[WG_SIZE];
    threadgroup float s_max[WG_SIZE];

    float val = (gid < count) ? input[gid] : 0.0f;
    s_sum[lid] = val;
    s_min[lid] = (gid < count) ? val : HUGE_VALF;
    s_max[lid] = (gid < count) ? val : -HUGE_VALF;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) {
            s_sum[lid] += s_sum[lid + s];
            s_min[lid] = min(s_min[lid], s_min[lid + s]);
            s_max[lid] = max(s_max[lid], s_max[lid + s]);
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) {
        partial_sum[group_id] = s_sum[0];
        partial_min[group_id] = s_min[0];
        partial_max[group_id] = s_max[0];
    }
}

// ── reduce_multi_i64 (fused SUM+MIN+MAX+COUNT for int64) ───────────

kernel void reduce_multi_i64(
    device const long* input [[buffer(0)]],
    device long* partial_sum [[buffer(1)]],
    device long* partial_min [[buffer(2)]],
    device long* partial_max [[buffer(3)]],
    constant uint& count [[buffer(4)]],
    uint gid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_id [[threadgroup_position_in_grid]])
{
    threadgroup long s_sum[WG_SIZE];
    threadgroup long s_min[WG_SIZE];
    threadgroup long s_max[WG_SIZE];

    long val = (gid < count) ? input[gid] : 0L;
    s_sum[lid] = val;
    // LONG_MAX/LONG_MIN for identity
    s_min[lid] = (gid < count) ? val : 0x7FFFFFFFFFFFFFFFL;
    s_max[lid] = (gid < count) ? val : (-0x7FFFFFFFFFFFFFFFL - 1L);
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = WG_SIZE / 2; s > 0; s >>= 1) {
        if (lid < s) {
            s_sum[lid] += s_sum[lid + s];
            s_min[lid] = min(s_min[lid], s_min[lid + s]);
            s_max[lid] = max(s_max[lid], s_max[lid + s]);
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) {
        partial_sum[group_id] = s_sum[0];
        partial_min[group_id] = s_min[0];
        partial_max[group_id] = s_max[0];
    }
}

// window.metal — GPU window function kernels (ROW_NUMBER, LAG, LEAD).
//
// All kernels are embarrassingly parallel (one thread per row).
// Partition boundaries are pre-computed on the CPU and uploaded as
// per-row index arrays (part_start or part_end).
//
// LAG/LEAD values are doubles from PG but treated as raw 8-byte (ulong)
// blobs — the GPU just copies them, no fp64 arithmetic needed.

#include <metal_stdlib>
using namespace metal;

// ── window_row_number ─────────────────────────────────────────────
// results[i] = i - part_start[i] + 1  (1-based row number in partition)

kernel void window_row_number(
    device const uint* part_start [[buffer(0)]],
    device long* results [[buffer(1)]],
    constant uint& count [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= count) return;
    results[gid] = long(gid) - long(part_start[gid]) + 1L;
}

// ── window_lag ────────────────────────────────────────────────────
// target = i - offset; if target < part_start[i] → write default

struct LagLeadParams {
    uint offset;
    uint count;
    uint has_nulls;
    uint has_result_nulls;
    ulong default_val_bits;  // double reinterpreted as ulong
};

kernel void window_lag(
    device const uint* part_start [[buffer(0)]],
    device const ulong* values [[buffer(1)]],
    device const uchar* null_mask [[buffer(2)]],
    device ulong* results [[buffer(3)]],
    device uchar* result_nulls [[buffer(4)]],
    constant LagLeadParams& params [[buffer(5)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    uint target = (gid >= params.offset) ? (gid - params.offset) : 0xFFFFFFFFu;

    if (target == 0xFFFFFFFFu || target < part_start[gid]) {
        // Before partition start — use default
        results[gid] = params.default_val_bits;
        if (params.has_result_nulls) result_nulls[gid] = 0;
    } else if (params.has_nulls && null_mask[target]) {
        // Source is NULL
        results[gid] = params.default_val_bits;
        if (params.has_result_nulls) result_nulls[gid] = 1;
    } else {
        results[gid] = values[target];
        if (params.has_result_nulls) result_nulls[gid] = 0;
    }
}

// ── window_lead ───────────────────────────────────────────────────
// target = i + offset; if target > part_end[i] → write default

kernel void window_lead(
    device const uint* part_end [[buffer(0)]],
    device const ulong* values [[buffer(1)]],
    device const uchar* null_mask [[buffer(2)]],
    device ulong* results [[buffer(3)]],
    device uchar* result_nulls [[buffer(4)]],
    constant LagLeadParams& params [[buffer(5)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    uint target = gid + params.offset;

    if (target > part_end[gid]) {
        // Past partition end — use default
        results[gid] = params.default_val_bits;
        if (params.has_result_nulls) result_nulls[gid] = 0;
    } else if (params.has_nulls && null_mask[target]) {
        // Source is NULL
        results[gid] = params.default_val_bits;
        if (params.has_result_nulls) result_nulls[gid] = 1;
    } else {
        results[gid] = values[target];
        if (params.has_result_nulls) result_nulls[gid] = 0;
    }
}

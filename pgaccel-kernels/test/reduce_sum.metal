#include <metal_stdlib>
using namespace metal;

kernel void reduce_sum_f32(
    device const float* input [[buffer(0)]],
    device atomic_float* output [[buffer(1)]],
    device const uint& count [[buffer(2)]],
    uint tid [[thread_position_in_grid]],
    uint lid [[thread_position_in_threadgroup]],
    uint group_size [[threads_per_threadgroup]])
{
    threadgroup float shared[1024];
    shared[lid] = (tid < count) ? input[tid] : 0.0f;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    for (uint s = group_size / 2; s > 0; s >>= 1) {
        if (lid < s) {
            shared[lid] += shared[lid + s];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) {
        atomic_fetch_add_explicit(output, shared[0], memory_order_relaxed);
    }
}

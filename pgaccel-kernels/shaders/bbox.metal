// bbox.metal — Bulk bounding box intersection test for Metal GPU backend.
//
// Each thread evaluates one (i, j) pair from the cross-product of boxes_a × boxes_b.
// Boxes are stored as flat float4: [xmin, ymin, xmax, ymax].

#include <metal_stdlib>
using namespace metal;

struct BBoxParams {
    uint count_a;
    uint count_b;
};

kernel void bbox_intersects_f32(
    device const float* boxes_a  [[buffer(0)]],
    device const float* boxes_b  [[buffer(1)]],
    device uchar*       result   [[buffer(2)]],
    device atomic_uint* hits     [[buffer(3)]],
    constant BBoxParams& params  [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    uint total = params.count_a * params.count_b;
    if (gid >= total) return;

    uint i = gid / params.count_b;
    uint j = gid % params.count_b;

    float a_xmin = boxes_a[i * 4 + 0];
    float a_ymin = boxes_a[i * 4 + 1];
    float a_xmax = boxes_a[i * 4 + 2];
    float a_ymax = boxes_a[i * 4 + 3];

    float b_xmin = boxes_b[j * 4 + 0];
    float b_ymin = boxes_b[j * 4 + 1];
    float b_xmax = boxes_b[j * 4 + 2];
    float b_ymax = boxes_b[j * 4 + 3];

    bool intersects = !(a_xmax < b_xmin || a_xmin > b_xmax ||
                        a_ymax < b_ymin || a_ymin > b_ymax);

    result[gid] = intersects ? 1 : 0;

    if (intersects) {
        atomic_fetch_add_explicit(hits, 1u, memory_order_relaxed);
    }
}

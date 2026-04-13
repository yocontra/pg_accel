// fused.metal — Fused filter+reduce GPU kernels for Metal backend.
//
// Combines predicate evaluation and reduction into a single kernel launch.
// One memory pass instead of three (filter, compact, reduce).
//
// Kernels:
//   fused_filter_reduce_f32  — filter + SUM/MIN/MAX/COUNT on one column
//   fused_filter_count_f32   — filter + COUNT(*) (no value column)

#include <metal_stdlib>
using namespace metal;

// Comparison operators (must match pgaccel_cmp_op enum)
// 0=EQ, 1=NE, 2=LT, 3=LE, 4=GT, 5=GE

// Aggregation operations (must match pgaccel_fused_agg_op enum)
// 0=SUM, 1=MIN, 2=MAX, 3=COUNT

struct FusedReduceParams {
    uint  count;
    uint  cmp_op;
    float filter_val;
    uint  agg_op;
};

static inline bool eval_cmp(float val, uint op, float ref) {
    switch (op) {
        case 0: return val == ref;  // EQ
        case 1: return val != ref;  // NE
        case 2: return val < ref;   // LT
        case 3: return val <= ref;  // LE
        case 4: return val > ref;   // GT
        case 5: return val >= ref;  // GE
        default: return false;
    }
}

kernel void fused_filter_reduce_f32(
    device const float*  filter_col   [[buffer(0)]],
    device const float*  agg_col      [[buffer(1)]],
    device atomic_uint*  result_bits  [[buffer(2)]],  // float as uint bits (atomic)
    device atomic_uint*  match_count  [[buffer(3)]],
    constant FusedReduceParams& params [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    if (!eval_cmp(filter_col[gid], params.cmp_op, params.filter_val)) return;

    // Increment match count
    atomic_fetch_add_explicit(match_count, 1u, memory_order_relaxed);

    float val = agg_col[gid];

    switch (params.agg_op) {
        case 0: { // SUM — atomic float add
            // Metal supports atomic float add on Apple Silicon (A14+/M1+)
            // Use reinterpret as uint for atomic CAS loop
            uint expected = atomic_load_explicit(result_bits, memory_order_relaxed);
            while (true) {
                float current = as_type<float>(expected);
                float desired = current + val;
                uint desired_bits = as_type<uint>(desired);
                if (atomic_compare_exchange_weak_explicit(
                        result_bits, &expected, desired_bits,
                        memory_order_relaxed, memory_order_relaxed)) {
                    break;
                }
            }
            break;
        }
        case 1: { // MIN — atomic CAS loop
            uint expected = atomic_load_explicit(result_bits, memory_order_relaxed);
            while (true) {
                float current = as_type<float>(expected);
                if (val >= current) break; // not smaller
                uint desired_bits = as_type<uint>(val);
                if (atomic_compare_exchange_weak_explicit(
                        result_bits, &expected, desired_bits,
                        memory_order_relaxed, memory_order_relaxed)) {
                    break;
                }
            }
            break;
        }
        case 2: { // MAX — atomic CAS loop
            uint expected = atomic_load_explicit(result_bits, memory_order_relaxed);
            while (true) {
                float current = as_type<float>(expected);
                if (val <= current) break; // not larger
                uint desired_bits = as_type<uint>(val);
                if (atomic_compare_exchange_weak_explicit(
                        result_bits, &expected, desired_bits,
                        memory_order_relaxed, memory_order_relaxed)) {
                    break;
                }
            }
            break;
        }
        case 3: { // COUNT — just use match_count, result not needed
            break;
        }
    }
}

struct FusedCountParams {
    uint  count;
    uint  cmp_op;
    float filter_val;
};

kernel void fused_filter_count_f32(
    device const float*  filter_col   [[buffer(0)]],
    device atomic_uint*  match_count  [[buffer(1)]],
    constant FusedCountParams& params [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    if (eval_cmp(filter_col[gid], params.cmp_op, params.filter_val)) {
        atomic_fetch_add_explicit(match_count, 1u, memory_order_relaxed);
    }
}

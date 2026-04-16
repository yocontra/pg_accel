---
name: AdaptiveCpp Multi-Platform GPU
description: AdaptiveCpp/SYCL is the sole GPU backend for pg_accel. One source tree compiles to CUDA, ROCm, Level Zero, Metal, and CPU. This skill documents the five targets, their capability matrix, Metal-specific constraints, and the runtime dispatch pattern.
---

# AdaptiveCpp Multi-Platform GPU Guide for pg_accel

pg_accel builds **every** GPU kernel with AdaptiveCpp/SYCL. There is no native
Metal backend, no CUDA-specific hand-rolled path, and no backend switch. One `.cpp`
kernel compiles to all five targets; AdaptiveCpp's SSCP runtime picks the right
backend per device at load time. Capability differences are handled at runtime via
`sycl::device::has(...)` probes, not compile-time branches.

Metal is **one of five supported SYCL targets**, not an alternative to SYCL. It has
more constraints than the other four (no fp64, no atomic64, in-order queues only),
but the source code path is the same — only runtime kernel selection differs.

## Platform Capability Matrix

| Capability | CUDA (NVIDIA) | ROCm (AMD) | Level Zero (Intel) | Metal (Apple) | CPU |
|------------|--------------|------------|-------------------|--------------|-----|
| **Status** | Stable (v25.10) | Stable (v25.10) | Stable (v25.10) | develop branch | Stable |
| **FP64** | native | native | varies by GPU | NO | native |
| **Atomic64** | yes | yes | yes | NO | yes |
| **OOQ** | yes | yes | yes | NO (deadlocks) | N/A |
| **USM shared** | yes | yes | yes | yes (zero-copy) | N/A |
| **USM device** | yes | yes | yes | yes | N/A |
| **USM ptr indirection** | yes | yes | yes | NO | N/A |
| **Parallel sort lib** | thrust | rocThrust | oneDPL | NONE | std::sort |
| **Memory model** | discrete (PCIe) | discrete (PCIe) | discrete/integrated | unified (zero-copy) | host |
| **-ffast-math** | yes | yes | yes | BROKEN (minnum/maxnum) | yes |
| **Subgroup reductions** | yes | yes | yes | yes (PR #1996) | N/A |
| **Math functions** | yes | yes | yes | yes (PR #1997) | yes |

## Building AdaptiveCpp

### macOS (Metal + CPU)
```bash
git clone https://github.com/AdaptiveCpp/AdaptiveCpp.git
cd AdaptiveCpp && git checkout develop   # Metal requires develop branch
mkdir build && cd build
cmake .. -DCMAKE_INSTALL_PREFIX=/usr/local \
         -DWITH_METAL_BACKEND=ON \
         -DWITH_CPU_BACKEND=ON
make -j$(sysctl -n hw.ncpu) && sudo make install
```

### Linux (CUDA + ROCm + CPU)
```bash
git clone https://github.com/AdaptiveCpp/AdaptiveCpp.git
cd AdaptiveCpp && git checkout v25.10.0   # Use stable release on Linux
mkdir build && cd build
cmake .. -DCMAKE_INSTALL_PREFIX=/usr/local \
         -DWITH_CUDA_BACKEND=ON \
         -DWITH_ROCM_BACKEND=ON \
         -DWITH_CPU_BACKEND=ON \
         -DCUDA_TOOLKIT_ROOT_DIR=/usr/local/cuda
make -j$(nproc) && sudo make install
```

### Linux (Level Zero / Intel + CPU)
```bash
cmake .. -DCMAKE_INSTALL_PREFIX=/usr/local \
         -DWITH_LEVEL_ZERO_BACKEND=ON \
         -DWITH_CPU_BACKEND=ON
```

## Runtime Platform Detection

All platform differences are handled at RUNTIME, not compile time.
Query capabilities once at init, branch in kernel dispatch code.

```cpp
#include <sycl/sycl.hpp>

struct pgaccel_platform_caps {
    bool has_fp64;
    bool has_atomic64;
    bool has_ooo_queue;
    bool is_unified_memory;
    size_t max_alloc_bytes;
    uint32_t compute_units;
    char backend_name[64];   // "cuda", "hip", "level_zero", "metal", "cpu"
};

pgaccel_platform_caps detect_caps(sycl::device& dev) {
    pgaccel_platform_caps caps{};
    caps.has_fp64 = dev.has(sycl::aspect::fp64);
    caps.has_atomic64 = dev.has(sycl::aspect::atomic64);
    caps.is_unified_memory = dev.has(sycl::aspect::usm_shared_allocations)
                             && (dev.get_info<sycl::info::device::host_unified_memory>());
    caps.compute_units = dev.get_info<sycl::info::device::max_compute_units>();
    caps.max_alloc_bytes = dev.get_info<sycl::info::device::max_mem_alloc_size>();
    // backend_name from platform info
    return caps;
}
```

## Queue Creation (Platform-Adaptive)

```cpp
sycl::queue create_queue(sycl::device& dev, const pgaccel_platform_caps& caps) {
    if (caps.has_ooo_queue) {
        // CUDA, ROCm, Level Zero: out-of-order for async overlap
        return sycl::queue{dev};
    } else {
        // Metal: MUST use in-order (out-of-order deadlocks)
        return sycl::queue{dev, sycl::property::queue::in_order{}};
    }
}
```

## Memory Allocation (Platform-Adaptive)

```cpp
void* pgaccel_alloc(size_t bytes, sycl::queue& q, const pgaccel_platform_caps& caps) {
    if (caps.is_unified_memory) {
        // Apple Silicon: zero-copy shared memory
        // GPU break-even is much lower (~1K elements) because no transfer cost
        return sycl::malloc_shared(bytes, q);
    } else {
        // Discrete GPU: device memory + explicit prefetch
        // GPU break-even is higher (~10K-100K) due to PCIe transfer
        return sycl::malloc_device(bytes, q);
    }
}

void pgaccel_prefetch(void* ptr, size_t bytes, sycl::queue& q,
                      const pgaccel_platform_caps& caps) {
    if (!caps.is_unified_memory) {
        q.prefetch(ptr, bytes);  // hint to move data to device
    }
    // No-op on unified memory — data is already accessible
}
```

## Kernel Patterns: Dual-Precision Spatial Predicates

Write kernels that work at both fp32 and fp64, dispatched at runtime:

```cpp
// Template for precision-adaptive kernels
template<typename T>
int8_t point_in_ring_impl(T px, T py, const T* ring_xy, int vertex_count) {
    // Epsilon adapts to precision
    constexpr T EPSILON = std::is_same_v<T, double> ? T(1e-12) : T(1e-5);

    if (vertex_count < 4) return 0;  // UNCERTAIN

    int crossings = 0;
    for (int i = 0; i < vertex_count - 1; i++) {
        T x1 = ring_xy[i * 2], y1 = ring_xy[i * 2 + 1];
        T x2 = ring_xy[(i+1) * 2], y2 = ring_xy[(i+1) * 2 + 1];

        // Distance-to-edge check — tighter on fp64, wider on fp32
        T dx = x2 - x1, dy = y2 - y1;
        T len_sq = dx*dx + dy*dy;
        if (len_sq < EPSILON * EPSILON) continue;
        T t = sycl::clamp(((px-x1)*dx + (py-y1)*dy) / len_sq, T(0), T(1));
        T dist_sq = (px - (x1 + t*dx)) * (px - (x1 + t*dx))
                  + (py - (y1 + t*dy)) * (py - (y1 + t*dy));
        if (dist_sq < EPSILON * EPSILON) return 0;  // UNCERTAIN

        // Ray casting
        if ((y1 <= py && y2 > py) || (y2 <= py && y1 > py)) {
            T x_int = x1 + (py - y1) / (y2 - y1) * (x2 - x1);
            if (px < x_int) crossings++;
        }
    }
    return (crossings % 2 == 1) ? 1 : -1;  // DEFINITE_TRUE / DEFINITE_FALSE
}

// Dispatch at runtime based on caps
pgaccel_status pgaccel_point_in_ring_bulk(
    const void* points, size_t count,
    const void* ring, size_t vcount,
    bool use_fp64, int8_t* results)
{
    if (use_fp64) {
        // CUDA/ROCm/Level Zero path — tight epsilon, ~99.9% DEFINITE
        auto pts = static_cast<const double*>(points);
        auto rng = static_cast<const double*>(ring);
        q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
            results[i] = point_in_ring_impl<double>(
                pts[i*2], pts[i*2+1], rng, vcount);
        }).wait();
    } else {
        // Metal path — wider epsilon, ~95-98% DEFINITE, more CPU rechecks
        auto pts = static_cast<const float*>(points);
        auto rng = static_cast<const float*>(ring);
        q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
            results[i] = point_in_ring_impl<float>(
                pts[i*2], pts[i*2+1], rng, vcount);
        }).wait();
    }
    return PGACCEL_OK;
}
```

## Sort: Platform-Adaptive Strategy

```cpp
pgaccel_status pgaccel_sort_f64(double* data, size_t count) {
    if (!caps.has_fp64) return PGACCEL_UNSUPPORTED;  // Caller uses rayon CPU sort

#if __has_include(<oneapi/dpl/algorithm>)
    // Intel oneDPL available — use it
    oneapi::dpl::sort(oneapi::dpl::execution::make_device_policy(q), data, data + count);
#else
    // Bitonic sort fallback (works everywhere)
    bitonic_sort_gpu(q, data, count);
#endif
    return PGACCEL_OK;
}
```

## Metal-Specific Constraints (Summary)

These apply ONLY on Metal, not on CUDA/ROCm/Level Zero:
1. **fp32 only** — no double precision anywhere in Metal kernel code
2. **In-order queues only** — out-of-order deadlocks
3. **No atomic64** — use 32-bit atomics or reduce differently
4. **No USM pointer indirection** — flat arrays only
5. **No -ffast-math** — broken llvm.minnum/maxnum intrinsics
6. **No parallel sort library** — must use bitonic sort
7. **Must build from develop branch** — no stable release includes Metal yet

On CUDA/ROCm/Level Zero, NONE of these constraints apply.

## PostGIS Source References for Spatial Kernels

| Kernel | PostGIS Source File | Function | fp64 DEFINITE rate | fp32 DEFINITE rate |
|--------|-------------------|----------|-------------------|-------------------|
| point_in_ring | `lwgeom_geos.c` | `point_in_ring()` | ~99.9% | ~95-98% |
| sphere_distance | `lwgeom_sphere.c` | `sphere_distance()` | ~99.9% | ~98% |
| segment_intersects | `lwalgorithm.c` | `lw_segment_intersects()` | ~99.5% | ~98% |
| bbox_overlap | `gserialized_gist_2d.c` | `box2df_overlaps()` | 100% | 100% (already f32) |

PostGIS stores BOX2DF as float32 — no precision concern for bbox on any platform.

## Upstream PRs We Plan to Submit (Metal-specific)

1. **Bitonic sort kernel** — P0, no parallel sort exists on Metal
2. **llvm.minnum/maxnum fix** — P1, blocks -ffast-math on Metal MSL emitter
3. **atomic64 support** — P1, Apple Silicon M1+ hardware supports it
4. **Expanded Metal test suite** — P2, based on issues we discover
5. **Out-of-order queue deadlock fix** — P2, improves async dispatch on Metal

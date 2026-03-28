# Phase 4: GPU Foundation

**Depends on:** Phase 0 (kernel lib scaffold)
**Parallelism:** 5 agents (A5–A9). Runs concurrently with Phase 3 (Planner FFI).

Build the AdaptiveCpp GPU kernel library. After this phase, we have working
GPU primitives (bbox, sort, reduce, memory pool) tested standalone on multiple
platforms, ready to wire into the Rust extension in Phase 7.

**Multi-platform strategy:** All kernels written in standard SYCL, compiled by
AdaptiveCpp to all backends. Platform differences handled at runtime via
capability queries — not compile-time ifdefs.

| Capability | CUDA / ROCm / Level Zero | Metal | CPU Fallback |
|------------|--------------------------|-------|--------------|
| Precision | fp64 (native) | fp32 + wider UNCERTAIN | fp64 (native) |
| Queue type | out-of-order | in-order only | N/A |
| Memory | device USM + prefetch | shared USM (zero-copy) | host memory |
| Sort | oneDPL/thrust if avail, else bitonic | bitonic only | std::sort |
| Atomics | 64-bit | 32-bit only | 64-bit native |

---

## Agent Assignments

### A5 — Device Manager + Platform Capabilities
**Status:** Not Started
**Owns:** `pgaccel-kernels/src/device_manager.cpp`, `pgaccel-kernels/src/platform_caps.cpp`

**Tasks:**
- [ ] Initialize SYCL runtime, select best available device, and query platform capabilities at runtime
- [ ] Implement device selection priority: discrete GPU (CUDA > ROCm > Level Zero) > integrated GPU (Metal > Intel) > CPU fallback
- [ ] Create queue adapted to platform:
  - CUDA/ROCm/Level Zero: out-of-order queue (better async overlap)
  - Metal: in-order queue (out-of-order deadlocks on Metal backend)
- [ ] Define and populate platform capabilities struct at init:
  ```cpp
  struct pgaccel_platform_caps {
      bool has_fp64;           // true on CUDA/ROCm/Level Zero, false on Metal
      bool has_atomic64;       // true on CUDA/ROCm/Level Zero, false on Metal
      bool has_ooo_queue;      // true on CUDA/ROCm/Level Zero, false on Metal
      bool is_unified_memory;  // true on Apple Silicon, false on discrete GPU
      size_t max_alloc_bytes;  // device-specific
      uint32_t compute_units;  // SMs / CUs / EUs / GPU cores
      char backend_name[64];   // "cuda", "hip", "level_zero", "metal", "cpu"
  };
  ```
- [ ] Implement public API:
  ```cpp
  pgaccel_status pgaccel_init();
  pgaccel_status pgaccel_shutdown();
  pgaccel_device_info pgaccel_get_device_info();
  pgaccel_platform_caps pgaccel_get_caps();    // runtime capability query
  pgaccel_queue* pgaccel_get_queue();          // platform-appropriate queue
  ```
- [ ] Ensure `pgaccel_shutdown()` frees all resources
- [ ] Write standalone test binary that exercises all API functions on all available platforms

**Agent gate:**
- [ ] `pgaccel_init()` succeeds on Metal Mac, returns Apple GPU device
- [ ] `pgaccel_init()` succeeds on NVIDIA Linux, returns CUDA device
- [ ] `pgaccel_get_caps()` correctly reports fp64=false on Metal, fp64=true on CUDA
- [ ] `pgaccel_get_caps()` correctly reports unified_memory=true on Apple Silicon
- [ ] `pgaccel_init()` on machine with no GPU returns CPU device gracefully
- [ ] `pgaccel_shutdown()` frees all resources
- [ ] Standalone test binary works on all available platforms

**Implementation log:**
_(no deviations)_

### A6 — USM Memory Pool (Platform-Adaptive)
**Status:** Not Started
**Owns:** `pgaccel-kernels/src/mem_pool.cpp`

**Tasks:**
- [ ] Implement pool allocator using SYCL USM that adapts allocation strategy based on platform caps:
  - Unified memory (Apple Silicon): `sycl::malloc_shared` (zero-copy, no transfer)
  - Discrete GPU (NVIDIA/AMD/Intel): `sycl::malloc_device` + explicit prefetch
  - CPU fallback: standard `malloc`
- [ ] Implement public API:
  ```cpp
  void* pgaccel_alloc(size_t bytes);     // from pool, strategy based on caps
  void  pgaccel_free(void* ptr);         // return to pool
  void  pgaccel_pool_reset();            // free all (between queries)
  size_t pgaccel_pool_bytes_used();      // monitoring
  void  pgaccel_prefetch(void* ptr, size_t bytes);  // no-op on unified, prefetch on discrete
  ```
- [ ] Implement arena allocator with configurable block sizes (256KB default)
- [ ] Implement bump-allocation within blocks for fast sequential allocs during batch processing
- [ ] Implement `pool_reset()` to free all blocks at once (between queries -- no individual free needed during a batch)
- [ ] Route over-size allocations direct to USM (bypass arena)
- [ ] Return nullptr for zero-sized allocations (not error)
- [ ] Enforce flat data constraint (Metal): no nested pointer indirection in USM allocations -- all kernel data must be flat arrays of scalars or flat structs (applied to all platforms for portability, since discrete GPUs also prefer flat layouts)

**Agent gate:**
- [ ] Alloc 1000 x 4KB buffers, free all, realloc: no leak (bytes_used returns to baseline)
- [ ] Alloc 0 bytes: returns nullptr, no crash
- [ ] Alloc > slab size: succeeds via direct allocation
- [ ] pool_reset() frees everything
- [ ] Works on Metal (malloc_shared), CUDA (malloc_device), and CPU fallback
- [ ] prefetch() is no-op on unified memory, issues prefetch on discrete

**Implementation log:**
_(no deviations)_

### A7 — Bbox Overlap Kernel
**Status:** Not Started
**Owns:** `pgaccel-kernels/src/bbox_ops.cpp`

**Tasks:**
- [ ] Implement bulk bounding box intersection test using fp32 (PostGIS BOX2DF is float32, so Layer 1 is always fp32 regardless of platform -- this is exact)
- [ ] Implement fp64 bbox path for non-PostGIS use cases (e.g., PG's built-in `box` type which stores as float64) on platforms with fp64
- [ ] Implement public API:
  ```cpp
  // fp32 path (PostGIS BOX2DF -- all platforms)
  pgaccel_status pgaccel_bbox_intersects_bulk_f32(
      const float* boxes_a, size_t count_a,
      const float* boxes_b, size_t count_b,
      uint8_t* result, size_t* hit_count
  );

  // fp64 path (PG native box type -- CUDA/ROCm/Level Zero only)
  pgaccel_status pgaccel_bbox_intersects_bulk_f64(
      const double* boxes_a, size_t count_a,
      const double* boxes_b, size_t count_b,
      uint8_t* result, size_t* hit_count
  );
  ```
- [ ] Implement kernel: 4 float comparisons per pair (this is Layer 1 of the three-layer model, kills 90-95% of pairs before geometric predicates)
- [ ] Implement tiling for N x M > threshold to fit computation in GPU memory

**Agent gate:**
- [ ] 1K x 1K random bboxes: result matches CPU brute-force (zero false negatives, zero false positives)
- [ ] 10K x 10K: completes without OOM on 8GB unified memory
- [ ] Empty input (N=0 or M=0): returns immediately, hit_count = 0
- [ ] All-intersecting input: hit_count = N*M
- [ ] fp64 path (on CUDA): matches CPU reference exactly
- [ ] fp32 path (on Metal): matches CPU reference exactly (bbox is already float32)

**Implementation log:**
_(no deviations)_

### A8 — Sort Kernel (Platform-Adaptive)
**Status:** Not Started
**Owns:** `pgaccel-kernels/src/sort.cpp`

**Tasks:**
- [ ] Implement platform-adaptive parallel sort with backend-specific dispatch:
  - CUDA: use thrust::sort if available (optimal for NVIDIA), bitonic fallback
  - ROCm: use rocThrust if available, bitonic fallback
  - Level Zero: use oneDPL sort if available, bitonic fallback
  - Metal: bitonic sort (no parallel sort library exists yet -- upstream PR candidate)
  - CPU fallback: std::sort with execution policy par_unseq
- [ ] Implement bitonic sort as universal fallback (fixed communication pattern, no divergent branches, O(N log^2 N) comparisons but massively parallel)
- [ ] Implement support for fp32, fp64 (where available), int32, int64 keys:
  ```cpp
  pgaccel_status pgaccel_sort_f32(float* data, size_t count);
  pgaccel_status pgaccel_sort_f64(double* data, size_t count);   // no-op/error on Metal
  pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count);
  pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count);
  pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices, size_t count);
  pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices, size_t count);
  ```
- [ ] Implement CPU fallback for count < 4096 (GPU overhead not worth it)
- [ ] Return `PGACCEL_UNSUPPORTED` for fp64 variants on platforms without fp64 (Rust side uses rayon CPU sort instead)

**Agent gate:**
- [ ] Sort 1M random float32: matches CPU reference sort (all platforms)
- [ ] Sort 1M random float64: matches CPU reference sort (CUDA/ROCm)
- [ ] Sort 1M random float64 on Metal: returns UNSUPPORTED, caller uses CPU
- [ ] Key-value sort: indices correctly permuted to match sorted key order
- [ ] Sort already-sorted data: still correct (not O(N^2))
- [ ] Sort 100 elements: falls back to CPU, correct
- [ ] Stable for equal keys within key-value sort

**Implementation log:**
_(no deviations)_

### A9 — Reduce Kernel
**Status:** Not Started
**Owns:** `pgaccel-kernels/src/reduce.cpp`

**Tasks:**
- [ ] Implement GPU reduction using SYCL reduction primitives (PR #1996 confirmed working on Metal)
- [ ] Implement public API:
  ```cpp
  // fp32 (all platforms)
  pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count, float* result);
  pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count, float* result);
  pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count, float* result);
  // fp64 (CUDA/ROCm/Level Zero -- returns UNSUPPORTED on Metal)
  pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count, double* result);
  pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count, double* result);
  pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count, double* result);
  // integer (all platforms for i32, CUDA/ROCm for i64)
  pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count, int64_t* result);
  pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count, size_t* result);
  ```
- [ ] Use Kahan summation for float32 SUM to reduce accumulation error (result should match CPU `std::accumulate` within 1 ULP for sorted input)

**Agent gate:**
- [ ] Sum 10M float32: matches CPU accumulate within 1e-4 relative error
- [ ] Min/Max 1M: exact match with CPU
- [ ] Sum of all-zeros: exactly 0.0
- [ ] Sum of single element: exact
- [ ] Count of bitmap: exact match with popcount

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] pgaccel_init() works on Metal Mac (shows Apple GPU, caps.has_fp64=false)
- [ ] pgaccel_init() works on NVIDIA Linux (shows CUDA device, caps.has_fp64=true)
- [ ] pgaccel_init() works on Linux without GPU (CPU fallback)
- [ ] Platform caps correctly detected on each platform
- [ ] Memory pool: adapts strategy per platform (shared on unified, device on discrete)
- [ ] Memory pool: alloc/free/reset cycle with zero leaks
- [ ] Bbox kernel: 1K x 1K correctness on all available platforms
- [ ] Sort kernel: 1M float32 matches CPU reference on all platforms
- [ ] Sort kernel: fp64 sort works on CUDA, returns UNSUPPORTED on Metal
- [ ] Reduce kernel: 10M sum within tolerance on all platforms
- [ ] Reduce kernel: fp64 reduce works on CUDA, returns UNSUPPORTED on Metal
- [ ] All standalone tests pass on Apple Silicon (Metal)
- [ ] All standalone tests pass on Linux x86_64 (CUDA or CPU fallback)
- [ ] Queue type matches platform (out-of-order on CUDA, in-order on Metal)
- [ ] Docker integration: pg_accel_device_info() returns correct GPU/CPU info in container
- [ ] Docker integration: all prior phase tests still pass (no regressions)

---
name: AdaptiveCpp (SYCL compiler + runtime)
description: AdaptiveCpp/SYCL is the sole GPU backend for pg_accel. Single-source code compiles to CUDA, ROCm, Level Zero, Metal, and CPU via the generic SSCP JIT compiler. This skill is a reference for compilation flows, environment variables, preprocessor macros, SYCL extensions, scoped parallelism, Metal constraints, performance tuning, and debugging the SSCP JIT — distilled from the AdaptiveCpp develop-branch docs.
---

# AdaptiveCpp Reference for pg_accel

pg_accel builds **every** GPU kernel with AdaptiveCpp. One `.cpp` source compiles
to all five targets; the SSCP (single-source, single compiler pass) runtime picks
the right backend per device at load time. Capability differences are handled at
runtime via `sycl::device::has(...)` probes and `__acpp_if_target_*` branches, not
compile-time backend switches.

Metal is **one of five supported SYCL targets**, not an alternative to SYCL. It
has more constraints than CUDA/ROCm/L0 (soft fp64 at 1/32× perf, atomic64 on
Apple8+ only, in-order queue default) but source code is identical — only
runtime kernel selection differs.

## Compilation Flows

AdaptiveCpp supports three classes of compilation flow. **pg_accel uses `generic`
exclusively** — do not switch. Other flows exist only for CUDA/HIP source-level
interop (which pg_accel does not use).

| Flow | Target | Notes |
|---|---|---|
| `generic` | CUDA, ROCm, L0, Metal, CPU, OpenCL+SPIR-V | **Default.** Stage 1 parses once → LLVM IR; Stage 2 JITs at runtime to PTX/amdgcn/SPIR-V/MSL. Fastest compile, fastest binaries, most portable. |
| `cuda.{integrated,explicit}-multipass` | NVIDIA GPUs | clang CUDA toolchain + plugin. Enables CUDA source extensions (`<<<>>>`, `__device__` builtins, CUB/rocPRIM). Not used by pg_accel. |
| `hip.{integrated,explicit}-multipass` | AMD GPUs | clang HIP toolchain + plugin. Same story for HIP. |
| `cuda-nvcxx` | NVIDIA GPUs | Library-only, uses nvc++. Day-1 hardware support at cost of flexibility. |
| `omp.{library-only,accelerated}` | Any CPU | OpenMP; `.accelerated` uses clang plugin for deep loop fission on `nd_range`. |

### Generic SSCP (the only flow we use)

1. **Stage 1 (compile time)**: During regular C++ host compilation, AdaptiveCpp
   extracts LLVM IR for kernels with backend-independent builtin calls
   (`__acpp_sscp_*`) and embeds the IR as a string in the host module.
2. **Stage 2 (runtime)**: `llvm-to-backend` infrastructure lowers the IR to the
   selected device's format — PTX / amdgcn / SPIR-V / MSL. Each target has a
   manual tool (`llvm-to-ptx-tool`, `llvm-to-spirv-tool`, …) for debugging.

SSCP means kernels get **runtime specialization**: when the target device is
actually known, the JIT can hard-wire the device capability, invariant kernel
args (at `ACPP_ADAPTIVITY_LEVEL >= 2`), etc. This is why `generic` typically
beats multipass on perf despite its "generic" name.

### `--acpp-targets` syntax

```
"flow1:target1,target2,...;flow2:...;..."
```

- `omp.*` and `generic` take no targets.
- `cuda.*` targets are `sm_XY` (e.g., `sm_70` Volta, `sm_80` Ampere, `sm_90` Hopper).
- `hip.*` targets are `gfxXYZ` (e.g., `gfx900` Vega 10, `gfx906` MI50, `gfx908` MI100, `gfx90a` MI250).

Also settable via `ACPP_TARGETS` env var or `-DACPP_TARGETS=` CMake variable.

## Platform Capability Matrix

| Capability | CUDA (NVIDIA) | ROCm (AMD) | Level Zero (Intel) | Metal (Apple) | CPU |
|---|---|---|---|---|---|
| **Status** | Stable (v25.10) | Stable (v25.10) | Stable (v25.10) | Stable (fork-safe-metal) | Stable |
| **FP64** | native | native | varies by GPU | soft (external `acpp_metal_fp64` dep, gated by `ACPP_METAL_EXTERNAL_FP64=ON`; ~1/32× perf) | native |
| **Atomic64** | yes | yes | yes | Apple8+ (load/store/exchange/add/sub/min/max; no cmpxchg/and/or/xor) | yes |
| **OOQ queue** | yes | yes | yes | yes (cross-queue sync via MTLSharedEvent) | N/A |
| **USM shared** | yes | yes | yes | yes (zero-copy) | yes |
| **USM device** | yes | yes | yes | yes | yes |
| **USM ptr indirection** | yes | yes | yes | NO (permanent MSL constraint, flatten instead) | yes |
| **Parallel sort lib** | thrust | rocThrust | oneDPL | bitonic (`acpp::sort_into`) | std::sort |
| **Memory model** | discrete PCIe | discrete PCIe | discrete/integrated | unified (zero-copy) | host |
| **sycl::stream / printf** | yes | yes | yes | NO | yes |
| **PCUDA dialect** | yes | yes | yes | yes (experimental, PR #1983 merged) | yes |

### Metal-only constraints

1. **fp64 via soft-double at 1/32–1/64× fp32 perf.** Bodies live in the external `acpp_metal_fp64` CMake package (separate repo). AdaptiveCpp links them when configured with `-DACPP_METAL_EXTERNAL_FP64=ON`; the hook is a `find_package(acpp_metal_fp64 CONFIG REQUIRED)` in `src/libkernel/sscp/metal/CMakeLists.txt`. ABI contract (symbol list, target name, defines) in `src/libkernel/sscp/metal/float64/README.md`. With the option OFF, the trap-stub bodies make any fp64 use on Metal crash — pg_accel's `caps.has_fp64` gate keeps those paths off until the dep + option ship together.
2. **Out-of-order queues work** via `MTLSharedEvent` cross-queue synchronization (AdaptiveCpp's `multi_queue_executor` handles this transparently). In-order is still the default for lowest submission latency.
3. **Atomic64 on Apple8+** (M2 and later). Op coverage: load/store/exchange/add/sub/min/max. NOT supported: cmpxchg/and/or/xor on i64 (hardware limit, not emulatable). Pre-Apple8 GPUs (e.g. M1) fall back to u32 atomics via pg_accel's caps-aware dispatch.
4. **No USM pointer indirection** — pass pointers as top-level kernel args. **Permanent MSL constraint** (Metal 4 does NOT enable this). Worked example: `pgaccel-kernels/src/expr_eval.cpp`.
5. **Parallel sort** via `acpp::sort_into` (bitonic, in-tree at `include/hipSYCL/algorithms/sort/`). Power-of-2 sizes recommended; caller pads non-power-of-2 with sentinels. Not stable — use pg_accel's native Metal sort in `src/engine/executor/sort.rs` for stability-critical sorts.
6. No `sycl::stream` / printf (no MSL `printf`).
7. Build from the `fork-safe-metal` branch of AdaptiveCpp at `/Users/contra/Projects/AdaptiveCpp` — atomic64, soft-fp64 aspect probes, bitonic sort, and llvm.minnum/maxnum NaN-preserving lowering live there.

## Environment Variables (runtime)

### Core

| Var | What it does |
|---|---|
| `ACPP_DEBUG_LEVEL` | 0 none, 1 error, 2 warning, 3 info, 4 verbose. Default warning. |
| `ACPP_VISIBILITY_MASK` | Activate subset of backends, e.g. `omp;cuda` or `omp;ocl:0,4`. `omp` is always on. Also supports name-based OCL matching: `omp;ocl:Intel.0`. |
| `ACPP_RT_SCHEDULER` | `direct` (low-latency) or `unbound` (default, multi-device). Must be `unbound` for `ACPP_EXT_MULTI_DEVICE_QUEUE`. |
| `ACPP_DEFAULT_SELECTOR_BEHAVIOR` | `strict` (spec-conformant, default), `multigpu`, `system`. |
| `ACPP_PERSISTENT_RUNTIME` | `1` keeps runtime alive when no SYCL objects are in use — useful for multi-phase apps. |

### JIT cache / performance

| Var | What it does |
|---|---|
| `ACPP_APPDB_DIR` | Override JIT-cache location (default `$HOME/.acpp` on Linux/macOS, `%LOCALAPPDATA%\acpp` on Windows). |
| `ACPP_ADAPTIVITY_LEVEL` | 0 disable adaptivity, 1 default (convergence at 2nd run), 2 aggressive — detects invariant kernel args and hardwires them as constants. |
| `ACPP_ALLOCATION_TRACKING` | `1` allows runtime to track allocations for extra JIT optimizations (non-aliasing detection). **Enabled by default as of AdaptiveCpp 25.10.** Set to `0` to reduce kernel launch latency if you don't need the JIT wins. |
| `ACPP_JITOPT_HOST_VECTOR_MATH_LIBRARY` | `none`/`libmvec`/`svml`/`sleef`/`armpl` — override vector math library at JIT time. |
| `ACPP_JITOPT_IADS_RELATIVE_THRESHOLD` | Default 0.8. Fraction of invocations sharing an arg value before that arg gets hardwired. |
| `ACPP_JITOPT_IADS_RELATIVE_THRESHOLD_MIN_DATA` | Default 1024. Min kernel invocations before threshold is consulted. |
| `ACPP_RT_NO_JIT_CACHE_POPULATION` | `1` prevents storing JIT binaries in the on-disk cache (e.g. MPI: only one rank populates). |
| `ACPP_RT_DAG_REQ_OPTIMIZATION_DEPTH` | Max depth for DAG requirement optimization. |
| `ACPP_RT_MQE_LANE_STATISTICS_MAX_SIZE` | Max submissions tracked by `multi_queue_executor`. |
| `ACPP_RT_MQE_LANE_STATISTICS_DECAY_TIME_SEC` | Forget old-submission stats after this many seconds. |
| `ACPP_RT_GC_TRIGGER_BATCH_SIZE` | Nodes in flight before a GC job is spawned. |
| `ACPP_RT_OCL_NO_SHARED_CONTEXT` | `1` disables shared OpenCL context across devices (some impls don't support it). |
| `ACPP_RT_OCL_SHOW_ALL_DEVICES` | `1` exposes OpenCL devices even if not SYCL-compatible. |
| `ACPP_HCF_DUMP_DIRECTORY` | Dump embedded HCF (heterogeneous container format) data files here. |

### Stdpar (C++ parallel STL offloading — not currently used by pg_accel)

| Var | What it does |
|---|---|
| `ACPP_STDPAR_MEM_POOL_SIZE` | USM mem-pool size in GB. `0` disables. Default = 40% of device global memory. |
| `ACPP_STDPAR_PREFETCH_MODE` | `always`, `never`, `after-sync`, `first`, `auto`. |
| `ACPP_STDPAR_HOST_SAMPLING` | `1` runs stdpar calls on host and measures — for offload-viability heuristic. |
| `ACPP_STDPAR_OFFLOAD_SAMPLING` | `1` runs via offload + measures. |
| `ACPP_STDPAR_DATASET_NAME` | Identifier used in stdpar profile filename. |
| `ACPP_STDPAR_OHC_MIN_OPS` | Min dispatches before offloading decision is reconsidered. |
| `ACPP_STDPAR_OHC_MIN_TIME` | Min seconds before offloading decision is reconsidered. |

### IR dumps (debugging SSCP JIT — very useful for kernel bugs)

All only affect `--acpp-targets=generic`. Value `1` writes to `<source>.ll`; any
other value is treated as a file path. Within one run, dumps are appended.

| Stage | When |
|---|---|
| `ACPP_S2_DUMP_IR_INPUT` | Raw input generic IR |
| `ACPP_S2_DUMP_IR_INITIAL_OUTLINING` | After kernel outlining |
| `ACPP_S2_DUMP_IR_SPECIALIZATION` | After specializations applied |
| `ACPP_S2_DUMP_IR_REFLECTION` | After JIT-time reflection queries |
| `ACPP_S2_DUMP_IR_JIT_OPTIMIZATIONS` | After JIT-info optimizations |
| `ACPP_S2_DUMP_IR_BACKEND_FLAVORING` | After backend flavoring (target-specific) |
| `ACPP_S2_DUMP_IR_BUILTIN_REFLECTION` | After second reflection pass |
| `ACPP_S2_DUMP_IR_FULL_OPTIMIZATIONS` | After full LLVM opt pipeline |
| `ACPP_S2_DUMP_IR_FINAL` | Final IR before backend lowering (PTX/amdgcn/SPIR-V/MSL) |
| `ACPP_S2_DUMP_IR_ALL` | All stages |
| `ACPP_S2_DUMP_IR_FILTER` | Kernel-identifier filter (mangled name) |
| `ACPP_SSCP_FAILED_IR_DUMP_DIRECTORY` | Where to dump IR when SSCP JIT fails |

Each dump block is delimited by `;---------------- Begin AdaptiveCpp IR dump --------------`.

### Config files (runtime only)

Runtime env-vars (not JIT dumps) can also be set in a config file **in the same
directory as the program**. Env vars take precedence over config files.

- `acpp-config.cfg` is loaded first.
- `acpp-config-<app-name>.cfg` is loaded second and overrides.

Format is `<VAR>=<value>`, one per line.

## Preprocessor Macros

### Backend branching (use these for kernel-level specialization)

```cpp
__acpp_if_target_host(...)      // host backend only
__acpp_if_target_device(...)    // any device backend
__acpp_if_target_cuda(...)      // CUDA only
__acpp_if_target_hip(...)       // HIP only
__acpp_if_target_hiplike(...)   // CUDA or HIP
// No __acpp_if_target_metal or __acpp_if_target_sscp macro — SSCP uses runtime branches instead.
```

The SSCP equivalent is `if (__acpp_sscp_is_device) { ... } else { ... }` —
constant-folded by the JIT, zero runtime cost.

### Compilation-pass introspection

| Macro | Meaning |
|---|---|
| `__ACPP__`, `__ADAPTIVECPP__` | Defined if compiling with AdaptiveCpp. |
| `__ACPP_CLANG__` | Defined when clang plugin is active. |
| `SYCL_DEVICE_ONLY` | Device pass, but only if not unified host+device pass. **Not reliable on `cuda-nvcxx`.** |
| `ACPP_LIBKERNEL_IS_DEVICE_PASS` | Current pass targets any device. |
| `ACPP_LIBKERNEL_IS_DEVICE_PASS_{HOST,CUDA,HIP,SSCP}` | Per-backend pass flag. |
| `ACPP_LIBKERNEL_IS_EXCLUSIVE_PASS(CUDA\|HIP\|HOST)` | Pass targets only this backend. |
| `ACPP_LIBKERNEL_IS_UNIFIED_HOST_DEVICE_PASS` | Single pass for host+device (nvc++). |
| `ACPP_LIBKERNEL_COMPILER_SUPPORTS_{CUDA,HIP,HOST,SSCP}` | Compiler capability. |
| `__ACPP_ENABLE_{CUDA,HIP,OMPHOST,LLVM_SSCP}_TARGET__` | Target enabled in this build. |
| `ACPP_EXT_<NAME>` | Feature-test macro for a specific extension. |

### Portability helpers

```cpp
ACPP_UNIVERSAL_TARGET   // expands to __host__ __device__ — function available everywhere
ACPP_KERNEL_TARGET      // expands to __host__ __device__ — function available in kernels
```

## AdaptiveCpp SYCL Extensions

Extensions are feature-test-gated via `ACPP_EXT_<NAME>`. **Bolded entries are
the ones most relevant to pg_accel kernel work.**

| Extension | Purpose |
|---|---|
| **`ACPP_EXT_RESTRICT_PTR`** | Wrapper hinting a kernel pointer arg does not alias other arg pointers. Generic-only. Reduces register pressure and enables vectorization. |
| **`ACPP_EXT_SCOPED_PARALLELISM_V2`** | Recommended performance-portable kernel model (see below). Always enabled. |
| **`ACPP_EXT_ENQUEUE_CUSTOM_OPERATION`** | `handler::AdaptiveCpp_enqueue_custom_operation(lambda)` — submit an async native-backend op (CUDA/HIP stream op, Metal command, etc.) without bouncing back to the host. Always enabled. |
| **`ACPP_EXT_BUFFER_USM_INTEROP`** | Query or construct a `sycl::buffer` on top of USM pointers (`buffer::get_pointer`, `buffer::for_each_allocation`, `own_allocation`, `disown_allocation`). |
| **`ACPP_EXT_COARSE_GRAINED_EVENTS`** | Hint that events returned from `submit()` can sync with more ops than strictly needed. Lowers kernel launch latency when you rarely consult the event. |
| **`ACPP_EXT_JIT_COMPILE_IF`** | Compile-time branch on JIT-only-known target properties. Guard with `__acpp_if_target_sscp()` for portability. |
| **`ACPP_EXT_DYNAMIC_FUNCTIONS`** | Runtime-selected function definitions inside kernels. Hardwired by JIT — no runtime overhead once compiled. |
| **`ACPP_EXT_SPECIALIZED`** | `sycl::specialized<T>` — hint to JIT that a kernel arg should be hardwired as a constant specialization. Alternative to SYCL 2020 specialization constants. |
| **`ACPP_EXT_ACCESSOR_VARIANTS`** + `_DEDUCTION` | Compact accessor flavors (`ranged`, `unranged`, `placeholder`, `raw`) encoded in accessor type. Elides unused info — wins in register-pressure-bound kernels. |
| `ACPP_EXT_EXPLICIT_BUFFER_POLICIES` | Explicit view/non-view buffer semantics; non-blocking destructors. |
| `ACPP_EXT_MULTI_DEVICE_QUEUE` | `queue` that auto-distributes across multiple devices (`system_selector_v`, `multi_gpu_selector_v`). Experimental / primitive scheduler. |
| `ACPP_EXT_UPDATE_DEVICE` | `handler::update()` for device accessors — preallocate or separate data transfer from kernel exec. |
| `ACPP_EXT_QUEUE_WAIT_LIST` | `queue::get_wait_list()` — async barrier-like semantics via `handler::depends_on()`. |
| `ACPP_EXT_QUEUE_PRIORITY` + `_PRIORITY_RANGE` | Queue-priority property and range query. |
| `ACPP_EXT_CG_PROPERTY_PREFER_GROUP_SIZE` | Command-group property: preferred work-group size. |
| `ACPP_EXT_CG_PROPERTY_RETARGET` | Command-group property: retarget to a different device. |
| `ACPP_EXT_CG_PROPERTY_PREFER_EXECUTION_LANE` | Command-group property: preferred execution lane. |
| `ACPP_EXT_BUFFER_PAGE_SIZE` | Buffer page-size property. |
| `ACPP_EXT_PREFETCH_HOST` | `handler::prefetch_host()` for shared USM → host prefetch. |
| `ACPP_EXT_SYNCHRONOUS_MEM_ADVISE` | Free-function sync `sycl::mem_advise()` — cheaper than async form. |
| `ACPP_EXT_FP_ATOMICS` | Atomic ops on FP types. Must be explicitly `#define`d; portability-breaking. |
| `ACPP_EXT_AUTO_PLACEHOLDER_REQUIRE` | Auto `require()` placeholder accessors. |
| `ACPP_EXT_CUSTOM_PFWI_SYNCHRONIZATION` | Control sync at end of `parallel_for_work_item`. |
| `ACPP_EXT_TARGET_NUMA_NODE_PROPERTY` | OpenMP-only: pin USM allocation to NUMA nodes. |

### Example: enqueue custom operation

```cpp
q.submit([&](sycl::handler &cgh) {
    auto acc = some_buff.get_access<sycl::access::mode::read>(cgh);
    cgh.AdaptiveCpp_enqueue_custom_operation([=](sycl::interop_handle &h) {
      void *native = h.get_native_mem<sycl::backend::hip>(acc);
      hipStream_t s = h.get_native_queue<sycl::backend::hip>();
      hipMemcpyAsync(dst, native, nbytes, hipMemcpyDeviceToHost, s);
    });
});
```

Rules: don't touch host data inside the lambda (deps may not be complete at
submission); only submit async ops to the provided backend queue. Use a host
task instead when you need host work or synchronous backend ops.

## Scoped Parallelism (performance-portable kernel model)

Scoped parallelism is AdaptiveCpp's preferred way to write performance-portable
kernels across CPU and GPU. Recommended over `nd_range parallel_for` on CPU.

```cpp
sycl::range<1> num_work_groups = ...;
sycl::range<1> logical_group_size = ...;

q.parallel(num_work_groups, logical_group_size, [=](auto group){
    // Runs per physical work item. Vars here are in private mem.
    sycl::distribute_items(group, [&](sycl::s_item<1> logical_idx){
        // Runs once per logical item in the logical iteration space.
    });
});
```

### Key primitives

- `distribute_items(group, λ)` — distribute a group's logical iteration space over physical work items.
- `distribute_groups(group, λ)` — subdivide group for tiling; nesting allowed.
- `single_item(group, λ)` — run once per group.
- `*_and_wait` — variants that synchronize afterwards.
- `s_item<N>` — logical item index (vs. `sycl::item` for physical).
- `s_private_memory<T>` — per-logical-item private memory.

### Rules

1. Group arg passed to distribute/single/group-algorithms must be the **smallest available subunit** at that nesting point.
2. Cannot call `distribute_items`, `distribute_groups`, `single_item`, or group algorithms from **inside** `distribute_items` — those are collective on physical items. Also can't declare `s_private_memory` there.
3. All physical items in the group must reach the collective call site.

## Performance: the rules

- **Use `generic` compilation flow.** Other flows exist only for CUDA/HIP source interop.
- **Use USM, never `sycl::buffer`.** USM has lower launch overhead; buffers add register pressure and runtime dependency tracking and can silently sync in destructors. Prefer `sycl::malloc_device` for control, `sycl::malloc_shared` for productivity.
- **In-order queues bypass scheduling layers** → lower submission latency (`sycl::property::queue::in_order{}`). Also mandatory on Metal.
- **Run the app multiple times at `ACPP_ADAPTIVITY_LEVEL=1` (default) or `=2`.** The SSCP JIT warms up its cache and specializes invariant args. The `[AdaptiveCpp Warning] kernel_cache: ... JIT-compiled` message means convergence isn't reached yet.
- **AdaptiveCpp does not imply `-O3`.** Unlike nvcc/hipcc/icpx, you must pass `-O3` yourself. Also no `-ffast-math` by default.
- **AdaptiveCpp rounds `sqrt` correctly by default** (like hipcc). DPC++/icpx uses approximate builtins even with `-fno-fast-math`. For fair comparisons use `-fsycl-fp32-prec-sqrt` on DPC++.
- **`-ffp-contract=fast` is default on clang-based flows** (generic/cuda/hip/omp). Non-clang flows (cuda-nvcxx, omp.library-only) do not set it.
- **Clear the JIT cache after upgrading AdaptiveCpp, drivers, or the stack**: `rm -rf ~/.acpp/apps/*`. User-code changes do not require this.
- **Try `ACPP_ALLOCATION_TRACKING=1`** — can speed kernels via non-aliasing detection, at the cost of slightly higher submission latency.
- **For latency-bound workloads**: in-order queues + USM + `ACPP_EXT_COARSE_GRAINED_EVENTS` + lower adaptivity level + `ACPP_ALLOCATION_TRACKING=0`.
- **CPU / OpenMP backend**: `OMP_PROC_BIND=true` and ensure `libomp` matches the one AdaptiveCpp was built against. For multi-socket NUMA, run one process per socket and MPI between them — `queue::memcpy` is not NUMA-aware on OpenMP.

## Building AdaptiveCpp

### macOS (Metal + CPU) — pg_accel

pg_accel does NOT use upstream AdaptiveCpp. It requires the `fork-safe-metal`
branch (kernel-dispatch fork-safety patches for PG's forked backends) installed
to `~/local`. The `just setup-gpu-acpp` recipe wraps the build:

```bash
# One-time: clone the fork-safe-metal checkout next to pg_accel.
git clone -b fork-safe-metal <fork-url> ~/Projects/AdaptiveCpp

# Then from the pg_accel repo:
just setup-gpu          # installs llvm@20, metal-cpp headers, and builds acpp
# (delegates to setup-gpu-deps, setup-gpu-metal-headers, setup-gpu-acpp)
```

The recipe installs to `$HOME/local` (binary archive helper ends up at
`~/local/bin/acpp-metal-archive-build`). Override the source path with
`ACPP_SRC=...` if your checkout lives elsewhere. Verify with `acpp-info`.

See the "MTLBinaryArchive cache" and "Crash diagnosis workflow" sections of
the top-level `CLAUDE.md` for the fork-safety contract this branch provides.

### Linux (CUDA + ROCm + CPU)
```bash
cd AdaptiveCpp && git checkout v25.10.0
cmake .. -DWITH_CUDA_BACKEND=ON -DWITH_ROCM_BACKEND=ON -DWITH_CPU_BACKEND=ON \
         -DCUDA_TOOLKIT_ROOT_DIR=/usr/local/cuda
```

### Linux (Intel Level Zero + CPU)
```bash
cmake .. -DWITH_LEVEL_ZERO_BACKEND=ON -DWITH_CPU_BACKEND=ON
```

## Runtime Detection & Queue Creation

```cpp
#include <sycl/sycl.hpp>

struct pgaccel_caps {
    bool has_fp64;
    bool has_atomic64;
    bool has_ooo_queue;       // false on Metal
    bool is_unified_memory;   // true on Apple Silicon / iGPU
    size_t max_alloc_bytes;
    uint32_t compute_units;
};

pgaccel_caps detect(sycl::device& d) {
    pgaccel_caps c{};
    c.has_fp64 = d.has(sycl::aspect::fp64);
    c.has_atomic64 = d.has(sycl::aspect::atomic64);
    c.is_unified_memory = d.has(sycl::aspect::usm_shared_allocations)
                       && d.get_info<sycl::info::device::host_unified_memory>();
    c.compute_units = d.get_info<sycl::info::device::max_compute_units>();
    c.max_alloc_bytes = d.get_info<sycl::info::device::max_mem_alloc_size>();
    // Metal doesn't advertise OOQ support cleanly — gate by platform name in practice.
    return c;
}

sycl::queue make_queue(sycl::device& d, const pgaccel_caps& c) {
    if (c.has_ooo_queue) return sycl::queue{d};
    return sycl::queue{d, sycl::property::queue::in_order{}};
}

void* alloc(size_t bytes, sycl::queue& q, const pgaccel_caps& c) {
    return c.is_unified_memory
        ? sycl::malloc_shared(bytes, q)   // Apple Silicon: zero-copy
        : sycl::malloc_device(bytes, q);  // Discrete: explicit prefetch needed
}
```

## Buffer-USM Interop (when you need both)

Any `sycl::buffer` allocation is internally a USM pointer. Extract with:

```cpp
buffer.get_pointer(dev);                 // USM ptr for a given device or nullptr
buffer.has_allocation(dev);
buffer.get_allocation(dev);              // buffer_allocation::descriptor<T>
buffer.for_each_allocation(λ);           // iterate all allocations
buffer.own_allocation(dev);              // free at buffer destruction
buffer.disown_allocation(dev);           // don't free
```

Buffers can also be constructed on top of an existing USM pointer.

## Debugging the SSCP JIT — workflow

When a kernel misbehaves on a specific backend:

1. **Confirm JIT convergence.** The `kernel_cache: ... JIT-compiled` warning means the adaptivity engine hasn't converged; re-run once or twice before debugging correctness.
2. **Dump final IR**: `ACPP_S2_DUMP_IR_FINAL=1 ACPP_S2_DUMP_IR_FILTER=<mangled_name> ./app`. Inspect for missing fp64, unexpected calls to unsupported intrinsics (e.g. llvm.minnum on Metal), or pointer-indirection patterns that Metal can't handle.
3. **Dump earlier stages** to locate where transformation broke things:
   - `INPUT` — unoptimized input
   - `BACKEND_FLAVORING` — after target-specific flavoring (first point where backend assumptions are applied)
   - `FULL_OPTIMIZATIONS` — after full LLVM opt (last point before lowering)
4. **On SSCP JIT failure**: set `ACPP_SSCP_FAILED_IR_DUMP_DIRECTORY=/tmp/fail-ir` to capture the IR that failed lowering — look at it directly, then retry manually with the relevant `llvm-to-<backend>-tool`.
5. **Increase runtime verbosity**: `ACPP_DEBUG_LEVEL=4`.
6. **For fork-related Metal crashes**, see the "MTLBinaryArchive cache" section in the project's top-level CLAUDE.md — archive-builder failures manifest as `MTLCompilerService error 3`.

## Metal-specific: current status (fork-safe-metal branch)

- **PCUDA dialect** — supported on Metal as of PR #1983 (experimental); pg_accel still uses SYCL only.
- **fp64 via soft-double** — MSL emitter lowers `double` to `struct acpp_f64 { uint lo; uint hi; }` with per-op dispatch to `__acpp_sscp_soft_f64_*` symbols. Math bodies come from the external `acpp_metal_fp64` CMake package (separate repo); AdaptiveCpp's `src/libkernel/sscp/metal/CMakeLists.txt` consumes it via `find_package(acpp_metal_fp64)` when built with `-DACPP_METAL_EXTERNAL_FP64=ON`. Symbol / target / define contract: `src/libkernel/sscp/metal/float64/README.md`. With the option OFF (default), bodies trap (`__builtin_trap()`), and Metal's `sycl::aspect::fp64` probe returns false — pg_accel's fp64-gated paths stay disabled. Once the external dep + option ship together, `caps.has_fp64` flips true automatically.
- **Atomic64 — currently disabled on Metal.** Apple8+ hardware (M2 and later) supports 64-bit device atomics per MSL 2.4+, but AdaptiveCpp's SSCP Metal emitter still emits program-scope simdgroup builtins (`__simd_size [[threads_per_simdgroup]]`) that only parse under `xcrun metal`'s permissive default dialect — a dialect that rejects `atomic_fetch_add_explicit(device atomic<ulong>*, ulong, …)` via `_valid_fetch_add_type` SFINAE. `metal_hardware_manager` advertises `atomic64 = false` on all Metal devices until the emitter produces MSL 2.4+-compliant source. pg_accel's `bbox_ops.cpp` uses the u32 atomics fallback; pre-Apple8 GPUs (M1) are unchanged.
- **llvm.minnum/maxnum** — IEEE 754-2008 NaN + signed-zero semantics are preserved in two layers: (1) the Metal SSCP libkernel body for `__acpp_sscp_fmin_f32` / `__acpp_sscp_fmax_f32` in `src/libkernel/sscp/metal/math.cpp` implements the NaN-propagating + `(−0, +0)` signed-zero-preserving sequence directly (always-inlined before MetalEmitter runs); (2) `Emitter.cpp` emits a defensive NaN-only fallback for any call that survives inlining. `-ffast-math` is therefore safe and **enabled globally** in `pgaccel-kernels/CMakeLists.txt`, with per-file `-fno-fast-math` opt-outs for the three kernels that depend on NaN/Inf propagation:
  - `raster_ops.cpp` — `isnan` / `isinf` drive NODATA propagation for div-by-zero, sqrt(-x), log(0/-x).
  - `spatial_predicates.cpp` — `isfinite` gates NaN/Inf coordinates to UNCERTAIN.
  - `sort.cpp` — `pad_value<float>()` returns `+infinity` as the bitonic-sort sentinel; the no-infinities assumption otherwise lets pad elements leak into the output with `index = UINT32_MAX`.
- **OOQ queues** — cross-queue sync via `MTLSharedEvent` is already implemented correctly in `src/runtime/metal/metal_queue.cpp:submit_queue_wait_for` and `multi_queue_executor` handles the scheduling. No deadlock in current tree; `tests/metal_ooq.cpp` passes.
- **Bitonic sort** — `acpp::sort_into` in `include/hipSYCL/algorithms/sort/sort_into.hpp` is a thin facade over the pre-existing `hipsycl::algorithms::sorting::bitonic_sort`. Keys-only + key-value overloads. Not stable — pg_accel's native Metal sort in `sort.rs` remains the primary path.
- **No `sycl::stream` / printf** — no MSL `printf`, no host callback path. Permanent constraint.
- **No full USM pointer semantics** — pass flat buffers explicitly. Passing
  `struct Entity { double* data; }` and dereferencing `entity.data` inside the
  kernel will crash. **Permanent MSL constraint** — Metal 4 does NOT enable this. `MTL4ArgumentTable` (macOS 15+, WWDC 2025) is a binding-layout redesign, not an MMU. Use pg_accel's flat-array flattening pattern — worked example in `pgaccel-kernels/src/expr_eval.cpp`.
- **SYCL event perf is suboptimal** — known issue, separate from the above.

## PostGIS source refs for spatial kernels

| Kernel | Source | fp64 DEFINITE | fp32 DEFINITE |
|---|---|---|---|
| point_in_ring | `lwgeom_geos.c` `point_in_ring()` | ~99.9% | ~95-98% |
| sphere_distance | `lwgeom_sphere.c` `sphere_distance()` | ~99.9% | ~98% |
| segment_intersects | `lwalgorithm.c` `lw_segment_intersects()` | ~99.5% | ~98% |
| bbox_overlap | `gserialized_gist_2d.c` `box2df_overlaps()` | 100% | 100% (BOX2DF is f32) |

## Useful CLI

- `acpp-info` — list AdaptiveCpp-visible devices per backend.
- `acpp --acpp-dryrun ...` — print the compiler invocation without running it.
- `acpp --acpp-save-temps ...` — keep intermediates.
- `acpp --acpp-version` — print version/config.
- `llvm-to-ptx-tool`, `llvm-to-spirv-tool`, `llvm-to-amdgpu-tool`, `llvm-to-metal-tool` — manual stage-2 lowering for debugging.

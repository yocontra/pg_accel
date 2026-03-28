# pg_accel — Engineering Plan Overview

## Mission

Make PostgreSQL measurably faster for everyone — Mac users get Metal GPU acceleration,
NVIDIA users get CUDA, AMD gets ROCm, Intel gets Level Zero, and CPU-only still delivers
significant speedup on any multi-core machine. One extension, every platform.

Batched executor nodes via Custom Scan Provider (late materialization, predicate reordering),
GPU-accelerated spatial predicates via AdaptiveCpp (SYCL) targeting all major GPU backends,
and GPU-accelerated raster operations (per-pixel map algebra, clip, reclass).
The GPU layer is platform-adaptive: fp64 on CUDA/ROCm, fp32+UNCERTAIN on Metal,
out-of-order queues where supported, in-order where not.

---

## Key Research Findings (March 2026)

### pgrx 0.17.0
- Supports PG 14–18
- **No safe Custom Scan Provider API** — all planner hooks and CustomPath/CustomScan
  work requires `unsafe` FFI through `pg_sys` auto-generated bindings
- `PgHooks` trait removed in v0.16.0 — hooks are raw `unsafe extern "C"` function pointers
- Shared memory (`pg_shmem_init!`) supported
- rayon threads MUST NOT call any PG internal function
- PARALLEL SAFE ≠ thread-safe: PG's parallel safety is process-level (forked workers),
  not thread-level. ALL PG C functions are called on the main backend thread only.
- rayon threads are used ONLY for GPU kernel orchestration, parallel sort-key
  extraction, and top-k merge — never for calling PG C function pointers.
- Two acceleration strategies:
  - **BatchedEval**: all PG/extension C functions called on main thread, batched Custom
    Scan node provides late materialization + predicate reordering (no rayon for functions)
  - **GpuSpatial**: GPU kernel (layers 1+2) + CPU recheck on main thread (layer 3)

### AdaptiveCpp GPU Backends

AdaptiveCpp provides a single SYCL codebase targeting multiple GPU platforms.
Our kernels are written once in SYCL and compile to all backends.

| Backend | Status | FP64 | Atomics64 | OOQ | USM Model | Maturity |
|---------|--------|------|-----------|-----|-----------|----------|
| **CUDA** (NVIDIA) | Stable (v25.10+) | native | yes | yes | device + managed | Production |
| **ROCm** (AMD) | Stable (v25.10+) | native | yes | yes | device + managed | Production |
| **Level Zero** (Intel) | Stable (v25.10+) | varies | yes | yes | device + shared | Production |
| **Metal** (Apple) | Experimental (develop) | NO (fp32 only) | NO (planned) | NO (deadlocks) | shared (zero-copy) | Early |
| **OpenCL** | Stable | varies | varies | yes | varies | Production |
| **CPU** | Stable | native | yes | N/A | N/A | Fallback |

**Metal-specific notes** (develop branch, merged 2026-02-02, PR #1961):
- Custom kernels, reductions, math functions, in-order queues all work
- No FP64, no atomic64, no out-of-order queues, no USM pointer indirection
- Must build from `develop` branch (no release includes Metal yet)
- Active development by @resetius — 11 PRs merged in 8 weeks

**Platform-adaptive kernel strategy:**
- Spatial predicates: fp64 on CUDA/ROCm/Level Zero (exact), fp32+UNCERTAIN on Metal
- Queues: out-of-order where supported (CUDA/ROCm), in-order on Metal
- Memory: device USM + prefetch on discrete GPU, shared USM (zero-copy) on Apple Silicon
- Sort: use platform parallel sort where available, bitonic sort fallback for Metal

### Upstream PRs We Need to Send to AdaptiveCpp
| PR | Why | Priority |
|----|-----|----------|
| Bitonic/merge sort kernel for Metal | No parallel sort exists on Metal; needed for GpuAccelSort | P0 |
| atomic64 support for Metal | Needed for 64-bit reduction counters on Apple Silicon | P1 |
| llvm.minnum/maxnum intrinsic expansion | Blocks -ffast-math on Metal MSL emitter | P1 |
| Out-of-order queue deadlock fix (Metal) | Improves async dispatch; workaround exists (in-order) | P2 |
| USM pointer indirection (Metal) | Nice-to-have for complex struct passing | P2 |

### Installation Path
- Homebrew custom tap (like TimescaleDB): `brew tap pg-accel/tap && brew install pg_accel`
- Requires `shared_preload_libraries` (planner hooks need startup registration)
- Unlike PostGIS which is `CREATE EXTENSION` only

---

## Phase Dependency Graph

```
Phase 0: Bootstrap & Infrastructure ────────────────┐
    │ (repo scaffold, Docker test harness, CI/CD,    │
    │  linting, Justfile, release infra)             │
    │                                                │
Phase 2: Core Engine                                 │
    │ (dispatch, batch accumulator, thread budget)   │
    │                                                │
    ├──────────────────────┐                         │
    │                      │                         │
Phase 3: Planner FFI    Phase 4: GPU Foundation ─────┘
    │ (Custom Scan        │ (AdaptiveCpp, device mgr,
    │  hooks, unsafe)     │  mem pool, basic kernels)
    │                     │
Phase 5: Executor Nodes  Phase 6: GPU Kernels
    │ (Scan, Join,        │ (spatial predicates,
    │  Agg, Sort)         │  sort, reduce, h3, raster)
    │                     │
    └──────────┬──────────┘
               │
Phase 7: Integration
    │ (adapters, three-layer wiring, end-to-end)
    │
Phase 8: Correctness Gauntlet
    │ (fuzz, stress, edge cases, concurrency)
    │
Phase 9: Hardening + Upstream PRs
    │ (production safety, AdaptiveCpp PRs)
    │
Phase 10: Benchmarks
    │ (framework, workloads, three-way comparison)
    │
Phase 11: Packaging + Docs
    │ (Homebrew tap, production Docker, README)
    │
Phase 12: Launch
```

**Execution order:**
Max 5–6 concurrent agents per phase.
AdaptiveCpp is installed (Metal backend ready on Apple Silicon).
Order: 3 → 4 → 5+6 → 7 → 8 → 9 → 10 → 11.

Phase 3 uses spike-first strategy: single agent builds minimal no-op Custom
Scan end-to-end, then remaining agents fan out in parallel once spike works.
Phases 3→4 run sequentially. Phases 5+6 run in parallel (5 depends on 3, 6 on 4).

**Testing at every phase:** Phase 0 establishes a Docker-based integration test
harness (PG 17 + PostGIS + h3-pg + pg_accel, real data fixtures). Each subsequent
phase adds test queries to `docker/tests/`. Every phase gate requires ALL Docker
integration tests to pass (ON == OFF). Tests are cumulative — Phase 7's gate runs
all tests from Phases 0–7.

**Agent coordination with Docker:** One PG container runs on port 5488 with
10 per-agent databases (`pgaccel_a0`–`pgaccel_a9`), each cloned from a shared
template via `CREATE DATABASE ... TEMPLATE`. Agents test against their own
database — full isolation, no cross-talk. A single watcher (`just dev-watch`)
monitors `src/` for changes, rebuilds the `.so`, and restarts PG inside the
container. Coordination uses a POSIX flock reader-writer lock: agents acquire
shared locks to run tests (concurrent), watcher acquires exclusive lock to
reload (waits for tests to finish, blocks new tests until done). No agent
rebuilds Docker or restarts PG directly.

---

## Architecture

```
pg_accel.so (Rust, pgrx 0.17, PG 15–18)
├── engine/                     (renamed from core/ — edition 2024 std::core shadowing)
│   ├── ffi/
│   │   ├── custom_scan.rs      unsafe Custom Scan Provider bindings
│   │   ├── planner_hooks.rs    set_rel_pathlist_hook, set_join_pathlist_hook
│   │   └── pg_compat.rs        PG 15/16/17/18 version shims
│   ├── type_extractor.rs       per PG type: datum → flat repr     [DONE]
│   ├── function_matcher.rs     runtime pg_proc discovery          [DONE]
│   ├── dispatch.rs             strategy dispatch (BatchedEval done, GPU stub)
│   ├── dispatch_fallback.rs    fallback decision logic            [DONE]
│   ├── batch.rs                batch accumulator                  [DONE]
│   ├── cost.rs                 platform-aware cost model          [DONE]
│   ├── thread_budget.rs        shared memory LWLock thread counter [DONE]
│   ├── thread_pool.rs          per-backend rayon pool             [DONE]
│   ├── gucs.rs                 all GUCs                           [DONE]
│   ├── stats.rs                per-backend perf counters          [DONE]
│   ├── device_info.rs          pg_accel_device_info() SQL fn      [DONE]
│   ├── registry.rs             OID->strategy HashMap              [PARTIAL]
│   ├── executor/
│   │   ├── scan.rs             GpuAccelScan — batched scan + vectorized WHERE
│   │   ├── join.rs             GpuAccelJoin — batched probe + residual
│   │   ├── agg.rs              GpuAccelAgg — combined filter+aggregate
│   │   └── sort.rs             GpuAccelSort — parallel sort + top-k
│   └── planner.rs              path injection logic (calls into ffi/)
│
├── adapters/
│   ├── mod.rs                  adapter registry + loader
│   ├── postgis.rs              PostGIS functions (~20 entries)
│   ├── h3.rs                   h3-pg functions (~8 entries)
│   └── pg_builtins.rs          stock PG functions
│
├── gpu/
│   ├── bridge.rs               Rust ↔ C++ FFI for kernel library
│   └── fallback.rs             CPU-only paths when GPU unavailable
│
└── bench/                      standalone benchmark CLI

libpgaccel_kernels.so (C++/SYCL, AdaptiveCpp, feature "gpu")
├── device_manager.cpp          init, device selection, queue management
│                               (out-of-order on CUDA/ROCm, in-order on Metal)
├── platform_caps.cpp           runtime capability query (fp64, atomics, queue type)
├── mem_pool.cpp                USM pool — adapts per platform:
│                               shared (zero-copy) on unified memory (Apple Silicon)
│                               device + prefetch on discrete GPU (NVIDIA/AMD/Intel)
├── sort.cpp                    parallel sort — platform dispatch:
│                               oneDPL/thrust where available, bitonic fallback
├── reduce.cpp                  SUM/MIN/MAX/AVG via SYCL reduction primitives
├── bbox_ops.cpp                bulk bbox overlap (4 float comparisons/pair)
│                               fp64 on CUDA/ROCm, fp32 on Metal
├── spatial_predicates.cpp
│     point_in_ring()           fp64 path (CUDA/ROCm) + fp32 path (Metal)
│     sphere_distance()         fp64 path + fp32 path
│     segment_intersects()      fp64 path + fp32 path
│     UNCERTAIN threshold adapts per platform:
│       fp64: tight epsilon (exact for 99.9%+ of rows)
│       fp32: wider epsilon (exact for ~98%, more CPU rechecks)
├── h3_ops.cpp
│     h3_lat_lng_to_cell()      coord→cell, trig+bit ops (fp64 CUDA, fp32+fallback Metal)
│     h3_grid_distance()        pairwise cell distance (pure integer math, all platforms)
│     h3_cell_to_parent()       bit shift (nearly free on GPU)
│     h3_get_resolution()       bit mask (nearly free on GPU)
└── raster_ops.cpp
      map_algebra_kernel()      per-pixel expression evaluation (f32/f64)
      raster_clip_kernel()      pixel-level geometry clip
      raster_reclass_kernel()   pixel value reclassification
```

---

## Agent Model

10 agents (A0–A9). Not permanently specialized — spun up fresh per phase with
relevant context. Within a phase, each agent owns specific files/modules to avoid
merge conflicts. Agent assignments are designed so no two agents edit the same file.

When an agent's subtask naturally continues into the next phase (e.g., the agent
that built planner hooks in Phase 3 continues to wire them in Phase 5), reuse that
agent to preserve context.

---

## Success Gate Protocol

Every phase has a **phase gate** — a set of binary pass/fail criteria that ALL must
pass before the next phase begins. Every agent task within a phase has an **agent
gate** — the specific deliverable that agent must produce.

Gate format:
```
[PASS/FAIL] <description> — <verification command or test>
```

No phase advances until all agent gates AND the phase gate pass.

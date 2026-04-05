# pg_accel

PostgreSQL extension: GPU-accelerated spatial predicates, h3 cell ops, raster operations,
and batched executor nodes via Custom Scan Provider. Rust (pgrx 0.17) + C++/SYCL (AdaptiveCpp).

## Build & Test Commands

```bash
just fmt              # cargo fmt
just lint             # cargo clippy -- -D warnings
just check            # cargo check --all-features
just deny             # cargo deny check (licenses + advisories)
just test             # cargo pgrx test pg17
just bench            # run benchmark suite against local pgrx PG

just ci               # Full local CI: lint + test
just package          # cargo pgrx package (installable .so)
just gpu-build        # cmake build for GPU kernels (requires AdaptiveCpp)
just gpu-test         # Run standalone GPU kernel tests
```

## Architecture (4 layers)

1. **Adapters** (`src/adapters/`) — register extension functions + strategy classification
2. **Dispatch** (`src/core/dispatch.rs`) — batch accumulator, strategy routing
3. **Executor Nodes** (`src/core/executor/`) — Custom Scan: scan, join, agg, sort
4. **GPU Kernels** (`pgaccel-kernels/src/`) — C++/SYCL spatial, h3, raster kernels

## Benchmark Rules

11. **NEVER compare against PG single-threaded.** All benchmarks compare pg_accel vs PG with parallel workers enabled (`max_parallel_workers_per_gather = DEFAULT`). Comparing against `max_parallel_workers_per_gather = 0` is deceptive — 100% of production PG uses parallel query. There is no `BenchMode::PgSingle`, no `single_ms` field, no "vs Single" metric. Any code introducing single-threaded comparisons must be rejected.

## Critical Safety Rules

1. **ALL PG C functions on main backend thread ONLY.** Never call PG functions from worker threads.
2. **GPU-only**: pg_accel only injects Custom Scan paths when GPU hardware is available. No GPU = no-op.
3. **GPU strategies only**: GpuSpatial, GpuH3, GpuSort, GpuReduce, GpuHashAgg, GpuHashJoin, GpuWindow, GpuExpr, GpuRaster.
4. **Custom Scan has THREE vtables**: CustomPathMethods, CustomScanMethods, CustomExecMethods. Never confuse them.
5. **Every `unsafe` block needs `// SAFETY:` comment.** No exceptions.
6. **No `unwrap()` outside tests.** Use `unwrap_or`, `?`, or explicit error handling.
7. **CHECK_FOR_INTERRUPTS() between batches** on main thread.
8. **Thread budget via shared memory LWLock.** Always release in before_shmem_exit.
9. **PARALLEL SAFE != thread-safe.** PG parallel = forked processes, not threads.
10. **No hardcoded GPU thresholds.** All dispatch limits (min rows, max chunk sizes, batch bounds) go in `DeviceLimits` (`src/engine/cost.rs`) and are derived from hardware profile. Never use magic constants in executor/planner code.

## Linting (enforced by CI, don't configure manually)

- `cargo fmt` — rustfmt with `.rustfmt.toml` (style_edition 2024, max_width 100)
- `cargo clippy` — `deny(clippy::all)`, `warn(clippy::pedantic, clippy::nursery)`, `deny(unwrap_used)`
- `cargo deny` — license allowlist + vulnerability check via `deny.toml`
- Conventional commits required: `feat(scope):`, `fix(scope):`, `perf(scope):`

## Skill Router

| Skill File | Use When |
|---|---|
| `pgrx-extension-dev` | PG extension structure, _PG_init, pg_module_magic, hooks, FFI |
| `custom-scan-ffi` | Custom Scan vtables, CustomPath/CustomScan/CustomExecMethods |
| `thread-safety-model` | Thread budget, signal masking, what can/can't run on threads |
| `adapter-development` | Writing function adapters, strategy classification, type extractors |
| `spatial-predicate-kernels` | GPU spatial kernels: point_in_ring, sphere_distance, segment_intersects |
| `geometry-deserialization` | GSERIALIZED format, bbox/point/vertex extraction from PostGIS |
| `adaptivecpp-metal` | AdaptiveCpp SYCL, Metal backend, fp32 constraints, platform caps |
| `cost-model` | Decision chain, GPU break-even, late materialization, platform profiles |
| `benchmark-methodology` | Benchmark harness, workload definitions, statistical methodology |

## Agent Coordination

- **10 agents per phase.** Each owns specific files — no two agents edit the same file.
- **Plans live in `plans/`.** Each agent updates their checklist status and implementation log.
- **Phase gates are binary.** ALL items must pass before next phase starts.

## Commit Convention

```
feat(gpu): add point_in_ring kernel with dual fp32/fp64 paths
fix(adapter): handle NULL geometry in PostGIS extractor
perf(executor): skip geometry deser for rows filtered by cheap predicate
test(correctness): add 150 degenerate geometry test cases
```

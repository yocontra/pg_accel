# pg_accel

PostgreSQL extension: GPU-accelerated spatial predicates, h3 cell ops, raster operations,
and batched executor nodes via Custom Scan Provider. Rust (pgrx 0.17) + C++/SYCL via AdaptiveCpp
(one source → CUDA / ROCm / Level Zero / Metal / CPU).

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
just gpu-build        # cmake build for GPU kernels (AdaptiveCpp/SYCL)
just gpu-test         # Run standalone GPU kernel tests
```

## Architecture (4 layers)

1. **Adapters** (`src/adapters/`) — register extension functions + strategy classification
2. **Dispatch** (`src/engine/dispatch.rs`) — batch accumulator, strategy routing
3. **Executor Nodes** (`src/engine/executor/`) — Custom Scan: scan, join, agg, sort
4. **GPU Kernels** (`pgaccel-kernels/src/`) — SYCL spatial, h3, raster kernels via AdaptiveCpp

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
11. **NEVER add CPU fallbacks.** GPU execution is the entire purpose of this library. If a GPU kernel crashes or fails, the fix is to make the GPU path work — not to add a CPU fallback that bypasses it. CPU fallbacks are test cheats that hide real bugs. Any code introducing CPU fallback paths for operations that should run on GPU must be rejected.
12. **AdaptiveCpp/SYCL ONLY.** All GPU code MUST use AdaptiveCpp/SYCL 100%. NO raw Metal (no `#import <Metal/Metal.h>`, no `.metal` shaders, no `.metallib` files period, no `MTLDevice`/`MTLCommandQueue`, no `.mm` Objective-C++ GPU files, no binary archives, no metallib compilation/loading). NO raw CoreML. NO CPU fallbacks. One source, one backend abstraction — AdaptiveCpp dispatches to CUDA/ROCm/L0/Metal/CPU transparently. Any code introducing raw Metal, `.metallib` artifacts, raw CoreML, or CPU fallback paths must be rejected.

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
| `adaptivecpp-metal` | AdaptiveCpp backends (CUDA/ROCm/L0/Metal/CPU), capability detection, kernel constraints |
| `cost-model` | Decision chain, GPU break-even, late materialization, platform profiles |
| `benchmark-methodology` | Benchmark harness, workload definitions, statistical methodology |

## Diagnostics & Tracing

### Architecture

pg_accel uses the `tracing` crate with a triple-output subscriber initialized lazily in each backend:

1. **OTel JSONL file** — `$PGDATA/pg_accel_otel.jsonl` (OTLP JSON format, consumed by `otel-tui`)
2. **tracing JSONL file** — `$PGDATA/pg_accel_traces.jsonl` (tracing-subscriber JSON, for Claude `Read` tool)
3. **PG stderr** — compact human-readable format for `pg_log` / terminal

The subscriber is configured by the `pg_accel.log_level` GUC (default: `notice`). Set to `debug` for full span output. The filter is read once when the backend's first query triggers tracing init.

**Source:** `src/engine/otel.rs`

### Viewing traces

```bash
# Live OTel span viewer TUI (install: brew install ymtdzzz/tap/otel-tui)
just otel-tui

# Tail the tracing-subscriber JSONL (for manual inspection)
just traces

# Last N entries
just traces-last 20
```

Claude agents: use the `Read` tool on `~/.pgrx/data-17/pg_accel_traces.jsonl` to inspect spans.

### Key span names

| Span | Location | What it tells you |
|------|----------|-------------------|
| `exec.window_compute` | `executor/window.rs` | Window consume + compute phase (n_specs) |
| `exec.window_emit` | `executor/window.rs` | Per-row tuple emission (pos) |
| `gpu.window.*` | `executor/window.rs` | Per-spec GPU dispatch (func, n) |
| `gpu.window_row_number` etc. | `gpu/mod.rs` | Individual GPU kernel call (n) |
| `gpu.reduce_*` | `gpu/mod.rs` | GPU reduce kernels (n) |
| `gpu.sort_*` | `gpu/mod.rs` | GPU sort kernels (n, key_type) |
| `exec.agg_*` | `executor/agg.rs` | Agg executor next/fused/grouped |
| `exec.sort_next` | `executor/sort.rs` | Sort executor emission |
| `planner.*` | `ffi/planner_hooks.rs` | Planner hook decisions |

### Crash diagnosis workflow

1. **Check PG logs:** `tail -50 ~/.pgrx/data-17/pg.log` — look for `signal 6` (SIGABRT), `signal 11` (SIGSEGV)
2. **Check macOS crash reports:** `ls -lt ~/Library/Logs/DiagnosticReports/postgres-*.ips | head -5` — parse with `grep "symbol"` for stack frames
3. **Check trace file:** `cat ~/.pgrx/data-17/pg_accel_traces.jsonl` — last completed span shows where execution reached before crash
4. **Check stats:** `SELECT * FROM pg_accel_stats();` — counters for hook calls, skips, GPU failures
5. **Common crash patterns:**
   - `apply_tlist_labeling` assert → target list mismatch in `PlanCustomPath` callback
   - `ExceptionalCondition` in planner → Custom Scan path metadata issue
   - AdaptiveCpp SSCP JIT failure → check `ACPP_TARGETS` and that the `develop` branch is installed to `~/local`
   - `crashed on child side of fork` → AdaptiveCpp SSCP caches compiled kernels per-backend, avoiding fork+exec recompilation

### GUCs

| GUC | Default | Purpose |
|-----|---------|---------|
| `pg_accel.enabled` | `true` | Master switch |
| `pg_accel.gpu_enabled` | `true` | GPU dispatch switch |
| `pg_accel.log_level` | `notice` | Tracing filter (debug/info/notice/warning/error) |
| `pg_accel.min_batch_size` | `65536` | Min rows for GPU dispatch |
| `pg_accel.kernel_timeout_ms` | `5000` | GPU kernel timeout |
| `pg_accel.cost_multiplier` | `1.0` | Cost estimate multiplier (>1 = more conservative) |

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

# pg_accel

PostgreSQL extension: GPU-accelerated spatial predicates, h3 cell ops, raster operations,
and batched executor nodes via Custom Scan Provider. Rust (pgrx 0.17) + C++/SYCL via AdaptiveCpp
(one source → CUDA / ROCm / Level Zero / Metal).

## Build & Test Commands

```bash
just fmt              # cargo fmt
just lint             # cargo clippy -- -D warnings
just check            # cargo check --all-features
just deny             # cargo deny check (licenses + advisories)
just test             # cargo pgrx test matrix (PG17/18, PG19 when pgrx supports it)
just coverage         # cargo-llvm-cov gate; writes LCOV/JSON/summary artifacts and fails below 90% lines
just bench            # run benchmark suite against local pgrx PG

just ci               # Full local CI: lint + test
just package          # cargo pgrx package (installable .so)
just gpu-build        # cmake build for GPU kernels (AdaptiveCpp/SYCL)
just gpu-test         # Run standalone GPU kernel tests (warm cache, quiet console)
just gpu-test-cold-all # Cold-cache GPU run for JIT/archive/fork-safety
just metal-stress     # M-series Metal stress gate with benchmark/cancellation artifacts
just cuda-stress      # NVIDIA CUDA stress gate with benchmark/cancellation artifacts
just release-verify   # Release verification matrix with provenance/audit/benchmark/stress artifacts
just release-checklist-audit # Fails while release checklist evidence is placeholder/unchecked
make clear-jit        # Clear AdaptiveCpp Metal SSCP JIT cache (~/.acpp/apps/global/jit-cache)
cargo run -p pg_accel_bench -- fp64-calibrate --connection "host=localhost port=28817 dbname=postgres" # soft-fp64 multiplier sweep; omit --max-size for release evidence
```

Treat noisy output as a bug. If a command emits repeated warning spam, fix the
recipe or wrapper so the console stays actionable while preserving a raw log.
Only filter understood patterns, keep full logs under `.pgaccel/logs`, and make
failure/error/result lines visible without scrolling. Do not normalize "noisy
but harmless" runs; fix noise papercuts as they appear.

**Always use `make clear-jit` to clear the JIT cache.** Never use `rm -rf
~/.acpp/apps/global/jit-cache/*` and do NOT invoke `just clear-jit` directly
either — `make clear-jit` is the single canonical entrypoint. The `make`
recipe is auto-allowed by the harness; the bare `rm` prompts on every
invocation. Use `make clear-jit` to force a cold-cache run before any test
that needs to verify fork-safety, archive-builder behaviour, or kernel
re-compilation after a code change.

## Architecture (4 layers)

1. **Adapters** (`src/adapters/`) — register extension functions + strategy classification
2. **Dispatch** (`src/engine/dispatch.rs`) — batch accumulator, strategy routing
3. **Executor Nodes** (`src/engine/executor/`) — Custom Scan: scan, join, agg, sort, window, preagg, sort_scan, vectorized_scan
4. **GPU Kernels** (`pgaccel-kernels/src/`) — SYCL spatial, h3, raster kernels via AdaptiveCpp

## Benchmark Rules

11. **NEVER compare against PG single-threaded.** All benchmarks compare pg_accel vs PG with parallel workers enabled (`max_parallel_workers_per_gather = DEFAULT`). Comparing against `max_parallel_workers_per_gather = 0` is deceptive — 100% of production PG uses parallel query. There is no `BenchMode::PgSingle`, no `single_ms` field, no "vs Single" metric. Any code introducing single-threaded comparisons must be rejected.

## Critical Safety Rules

1. **ALL PG C functions on main backend thread ONLY.** Never call PG functions from worker threads.
2. **GPU-only (runtime behavior)**: at runtime, if the device registry reports no capable GPU, the planner hooks skip Custom Scan injection and the query runs via PG's native plan untouched. This is a runtime no-op on unsupported hardware — NOT a compile-time CPU fallback. The GPU bridge is unconditionally compiled (see rule #12); there is no build configuration that swaps in a CPU implementation of a GPU kernel.
3. **GPU strategies only**: GpuSpatial, GpuH3, GpuSort, GpuReduce, GpuHashAgg, GpuHashJoin, GpuWindow, GpuExpr, GpuRaster.
4. **Custom Scan has THREE vtables**: CustomPathMethods, CustomScanMethods, CustomExecMethods. Never confuse them.
5. **Every `unsafe` block needs `// SAFETY:` comment.** No exceptions.
6. **No `unwrap()` outside tests.** Use `unwrap_or`, `?`, or explicit error handling.
7. **CHECK_FOR_INTERRUPTS() between batches** on main thread.
8. **Thread budget via shared memory LWLock.** Always release in before_shmem_exit.
9. **PARALLEL SAFE != thread-safe.** PG parallel = forked processes, not threads.
10. **No hardcoded GPU thresholds.** All dispatch limits (min rows, max chunk sizes, batch bounds) go in `DeviceLimits` (`src/engine/cost.rs`) and are derived from hardware profile. Never use magic constants in executor/planner code.
11. **NEVER add CPU fallbacks.** GPU execution is the entire purpose of this library. If a GPU kernel crashes or fails, the fix is to make the GPU path work — not to add a CPU fallback that bypasses it. CPU fallbacks are test cheats that hide real bugs. See rule #12 — this is enforced at compile time, not code review.
12. **SYCL-only, enforced at compile time.** The machinery that used to make CPU fallbacks reviewable-but-forbidden has been deleted. Adding any of the following MUST FAIL `cargo check -p pg_accel` or `cmake --build pgaccel-kernels/build` — it is no longer a code-review question:
    - The `PGACCEL_HAS_SYCL` preprocessor gate is gone. Kernel `.cpp` files unconditionally require SYCL; there is no `#if PGACCEL_HAS_SYCL` / `#else` branch to slip a CPU path into.
    - The `gpu` Cargo feature is gone. `cargo check -p pg_accel` (and `--no-default-features`) unconditionally compile the GPU bridge. There is no `#[cfg(feature = "gpu")]` / `#[cfg(not(feature = "gpu"))]` gate.
    - `pg_accel/src/gpu/stubs.rs` is gone. There is no `mod stubs` to host a CPU implementation of a GPU function.
    - `pgaccel_cpu_fallback_count` / `pgaccel_reset_cpu_fallback_count` / `pgaccel_warn_cpu_fallback` FFI symbols and their Rust wrappers are gone. There is no counter to increment from a fallback branch.
    - Policy still forbids raw Metal (no `#import <Metal/Metal.h>`, no `.metal` shaders, no `.metallib` files period, no `MTLDevice`/`MTLCommandQueue`, no `.mm` Objective-C++ GPU files, no binary archives, no metallib compilation/loading) and raw CoreML. AdaptiveCpp dispatches to CUDA / ROCm / Level Zero / Metal transparently from one source.
    - If an agent reaches for a CPU fallback, the correct response is to fix the GPU path (kernel, bridge, dispatch). The build is the enforcement mechanism.

## Anti-Cheat Rails

This is a hard problem space and agents cheat when stuck. Deterministic hooks in
`.claude/hooks/` block the worst patterns at Edit/Write/Bash time (exit 2 = blocked,
no negotiation). Full rule list, bypass mechanism, and reviewer-enforced rules in
`.claude/rules/anti-cheat.md` — read it before marking work done, editing code in
`src/` or `pgaccel-kernels/src/`, or citing benchmark numbers.

TL;DR: no fake success, no weakening tests, no hiding regressions, no silent error
swallowing on GPU paths, no fabricated evidence, no guessed APIs, no stubs as done,
no bypassing the build, say "I'm stuck" when stuck, cite `file:line` for code claims.

## Cross-Verification Protocol

After non-trivial changes (cited benchmarks, crash/correctness fixes, new or
rewritten GPU kernels, planner strategy changes, diffs spanning >1 of {kernel,
bridge, executor, planner}), spawn 2–3 fresh verifier agents in parallel with
disjoint briefs and block on their reports before reporting done. Full protocol,
verifier roles (A: re-run, B: audit diff, C: trace check), and prompt requirements
in `.claude/rules/cross-verification.md`.

`FAIL` from a verifier is ground truth. Fix and re-verify, or escalate honestly.

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
| `adaptivecpp-metal` | AdaptiveCpp backends (CUDA/ROCm/L0/Metal), capability detection, kernel constraints |
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

Claude agents: use `just traces` / `just traces-last` or inspect
`~/.pgrx/data-19/pg_accel_traces.jsonl` for the repo-default PG target.

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

1. **Check PG logs:** `tail -50 ~/.pgrx/data-19/pg.log` — look for `signal 6` (SIGABRT), `signal 11` (SIGSEGV)
2. **Check macOS crash reports:** `ls -lt ~/Library/Logs/DiagnosticReports/postgres-*.ips | head -5` — parse with `grep "symbol"` for stack frames
3. **Check trace file:** `cat ~/.pgrx/data-19/pg_accel_traces.jsonl` — last completed span shows where execution reached before crash
4. **Check stats:** `SELECT * FROM pg_accel_stats();` — counters for hook calls, skips, GPU failures
5. **Common crash patterns:**
   - `apply_tlist_labeling` assert → target list mismatch in `PlanCustomPath` callback
   - `ExceptionalCondition` in planner → Custom Scan path metadata issue
   - AdaptiveCpp SSCP JIT failure → check `ACPP_TARGETS` and that the `fork-safe-metal` branch is installed to `~/local` (run `just setup-gpu-acpp`)
   - `Unable to reach MTLCompilerService ... error 3 - No such process` in a forked backend → the `.metalar` archive path didn't run. See "MTLBinaryArchive cache" below.

### MTLBinaryArchive cache (fork-safety on Apple Silicon)

Metal kernel dispatch in forked PG backends goes through a two-stage cache at
`~/.acpp/apps/global/jit-cache/`:

| File | Produced by | Loaded via | Why it exists |
|------|-------------|------------|---------------|
| `<id>.metallib` | `xcrun metal` + `xcrun metallib` subprocess | `device->newLibrary(url)` | Library load must not require `MTLCompilerService` — file load is pure I/O |
| `<id>.metalar` | `acpp-metal-archive-build` subprocess | `device->newBinaryArchive(url)` | Pre-compiled AGX pipeline states; passed with `MTLPipelineOptionFailOnBinaryArchiveMiss` so pipeline creation has zero XPC dependency |

**Diagnostic flow when a forked backend crashes at kernel dispatch:**

1. `ls ~/.acpp/apps/global/jit-cache/*.metalar` — if empty after a child dispatch,
   the archive builder silently failed. Check that `~/local/bin/acpp-metal-archive-build`
   exists and is on the runtime's derived lookup path (dladdr of libacpp-rt.dylib
   → `../bin/` or `../../bin/`).
2. Run the helper manually: `~/local/bin/acpp-metal-archive-build <metallib> /tmp/x.metalar <kernel_name>` —
   exit codes: `2`=bad args, `3`=no device, `4`=newLibrary failed, `5`=newBinaryArchive failed,
   `6`=addComputePipelineFunctions failed, `7`=no kernels added, `8`=serializeToURL failed.
3. Override with `ACPP_METAL_ARCHIVE_BUILD=/abs/path/to/helper` if the dladdr
   lookup guesses wrong (e.g. non-standard install layout).
4. `test_fork` asserts a `.metalar` exists after a successful forked dispatch;
   if that assertion starts failing, the archive path broke before the crash did.

### GUCs

| GUC | Default | Purpose |
|-----|---------|---------|
| `pg_accel.enabled` | `true` | Master switch |
| `pg_accel.gpu_enabled` | `true` | GPU dispatch switch |
| `pg_accel.log_level` | `notice` | Tracing filter (debug/info/notice/warning/error) |
| `pg_accel.min_batch_size` | `65536` | Min rows for GPU dispatch |
| `pg_accel.kernel_timeout_ms` | `5000` | GPU kernel timeout |
| `pg_accel.cost_multiplier` | `1.0` | Cost estimate multiplier (>1 = more conservative) |

## Effective Device Limits (hardware-derived vs fallback)

Dispatch thresholds (`min_rows`, `max_chunk`, cost ratios, break-even points)
are **not** module-level constants. They live in the `DeviceLimits` struct at
`pg_accel/src/engine/cost/device_limits.rs` and are produced by one of two
paths, decided once per backend at `device_limits()` init
(`pg_accel/src/engine/cost/device_limits.rs:505-522`):

| Source tag | Produced by | Used when |
|------------|-------------|-----------|
| `hardware_derived` | `DeviceLimits::from_profile(&PlatformProfile)` at `pg_accel/src/engine/cost/device_limits.rs:186` | GPU detected (`profile.has_gpu == true`) |
| `fallback_cpu_only` | `DeviceLimits::cpu_only()` at `pg_accel/src/engine/cost/device_limits.rs:404` | No GPU detected, or `#[cfg(test)]` builds |

**The constants in `cpu_only()` (e.g. `gpu_reduce_min_rows: 25_000` at
`pg_accel/src/engine/cost/device_limits.rs:412`, `gpu_sort_max_elements:
2_000_000` at `:416`, `gpu_join_max_output_rows: 100_000` at `:417`) are NOT
the runtime defaults on a GPU-equipped machine.** When a GPU is present,
every threshold is recomputed from the detected profile. Benchmarks that
quote these values without calling `pg_accel_device_limits()` are reporting
the no-GPU fallback, not what the current session is actually using.

Benchmark threshold-matrix rows must carry dispatch/output evidence,
correctness-diff evidence, and cache-mode evidence. H3 and raster expected
winner rows require `--cache-mode both` release artifacts so warm speedup and
bounded cold-start cost are checked together. H3 and raster rows use
operation-specific lanes (lat/lng-to-cell, SRF expansion, map algebra, slope,
reclass, deep algebra) rather than generic extension-function dispatch claims;
small H3 grouped rows below the grouped-aggregate admission floor must remain
native-decline cells with captured benchmark-threshold decline reason evidence.
Every native-decline threshold-matrix row must carry an exact visible decline
reason in the captured plan snippet and no pg_accel plan label.

### How `from_profile()` derives each limit

The scale factor is `cu_scale` (in
`pg_accel/src/engine/cost/device_limits.rs`). `cu_scale(base)` computes
`base × BASELINE_CUS (32) / compute_units`. Higher CU count lowers the
threshold.

Representative formulas (cite `pg_accel/src/engine/cost/device_limits.rs:<line>`):

- `gpu_min_rows = cu_scale(10_000).clamp(1_000, 100_000)` — `:199`
- `gpu_sort_min_rows = cu_scale(100_000).clamp(10_000, 1_000_000)` — `:200`
- `gpu_window_min_rows = cu_scale(100_000).clamp(50_000, 500_000)` — `:206`
- `gpu_reduce_min_rows = cu_scale(25_000).clamp(5_000, 200_000)` — `:210`
- `gpu_hash_agg_min_rows = cu_scale(250_000).clamp(50_000, 2_000_000)` — `:211`
- `gpu_hash_agg_max_groups = (mem / 256 / 64).clamp(1_000, 100_000)` — `:216-220`
- `gpu_reduce_max_chunk`: unified → `100_000_000`; discrete → `(mem / 32 / 8).clamp(64_000, 256_000)` — `:228-234`
- `gpu_sort_max_elements = (mem / 32 / 12).clamp(64_000, 4_000_000)` — `:241-245`
- `gpu_join_max_output_rows = (100_000 × cus / 32).clamp(50_000, 500_000)` — `:252-256`
- `gpu_spatial_min_vertices = cu_scale(100).clamp(32, 1_000)` — `:291`
- `gpu_spatial_max_output_fraction = 0.80` — high-output heap-backed spatial
  scans stay PostgreSQL-native until spatial predicates can feed a
  GPU-resident aggregate/filter pipeline.
- `gpu_expr_min_rows = cu_scale(250_000).clamp(50_000, 2_000_000)` — `:295`
- `gpu_hash_join_build_max_rows = (mem / 64 / 64).clamp(10_000, 1_000_000)` — `:298-302`
- `reduce_f32_break_even_rows = cu_scale(25_000).clamp(4_000, 250_000)` — `:364`
- `reduce_f64_break_even_rows = cu_scale(50_000).clamp(8_000, 500_000)` — `:365`
- `reduce_i64_break_even_rows = cu_scale(75_000).clamp(10_000, 750_000)` — `:366`
- `sort_break_even_rows_int = cu_scale(100_000).clamp(20_000, 1_000_000)` — `:374`
- `sort_break_even_rows_float = cu_scale(80_000).clamp(16_000, 800_000)` — `:375`
- `window_min_partition_rows = cu_scale(10_000).clamp(2_000, 100_000)` — `:383`
- `hashjoin_min_build_rows = cu_scale(5_000).clamp(1_000, 50_000)` — `:387`
- `soft_fp64_cost_multiplier = crate::soft_fp64_cost_multiplier()` — `:396` (unused when `has_native_fp64`)

#### Worked example — Apple M2 Max (32 CUs, unified memory, 64 GiB max alloc)

With `cus = 32` (== `BASELINE_CUS`) and `unified = true`, `cu_scale(base) =
(base × 32 / 32) / 2 = base / 2`. The effective values on an M2 Max become:

| Field | `from_profile` value |
|-------|----------------------|
| `gpu_reduce_min_rows` | `25_000 / 2 = 12_500`, clamped to `[5_000, 200_000]` → **12_500** |
| `gpu_hash_agg_min_rows` | `250_000 / 2 = 125_000`, clamped → **125_000** |
| `gpu_sort_min_rows` | `100_000 / 2 = 50_000`, clamped → **50_000** |
| `gpu_expr_min_rows` | `250_000 / 2 = 125_000`, clamped → **125_000** |
| `reduce_f32_break_even_rows` | `25_000 / 2 = 12_500`, clamped to `[4_000, 250_000]` → **12_500** |

All of these are below the `cpu_only()` fallback values (25_000 / 250_000 /
etc.), which is the point: the fallback is conservative so a machine without
a detected GPU never dispatches borderline workloads.

### Dumping the effective limits

On any PG session, as SQL:

```sql
-- Full dump: one row per DeviceLimits field.
SELECT name, value, source FROM pg_accel_device_limits() ORDER BY name;

-- Check the source tag for the current session.
SELECT DISTINCT source FROM pg_accel_device_limits();
-- → 'hardware_derived' or 'fallback_cpu_only'

-- Look up a specific threshold.
SELECT value FROM pg_accel_device_limits()
WHERE name = 'gpu_reduce_min_rows';
```

`pg_accel_device_limits()` is defined at
`pg_accel/src/engine/stats.rs:349` and reads the cached `DeviceLimits`
via `engine::cost::device_limits()`; the `source` column is populated from
`engine::cost::device_limits_source()`
(`pg_accel/src/engine/cost/device_limits.rs:531`). When citing a
threshold in a benchmark report, paste the row from this SRF rather than the
fallback constant — otherwise you are citing a value that was never active
on the test machine.

## Agent Coordination

- **Agents are partitioned by file ownership.** Each worker owns a disjoint file set — no two agents edit the same file in a single phase.
- **Phase gates are binary.** ALL items must pass before the next phase starts.

## Commit Convention

```
feat(gpu): add point_in_ring kernel with dual fp32/fp64 paths
fix(adapter): handle NULL geometry in PostGIS extractor
perf(executor): skip geometry deser for rows filtered by cheap predicate
test(correctness): add 150 degenerate geometry test cases
```

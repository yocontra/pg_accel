# pg_accel Developer Guide

`pg_accel` is a PostgreSQL extension written in Rust/pgrx with an
AdaptiveCpp/SYCL kernel library. The current production planner selects only
covered resident reducing/grouped aggregate shapes as
`Custom Scan (GpuAccelAgg)`. Kernel, bridge, executor, adapter, and benchmark
code for other families is not evidence of planner selection.

The validated GPU development target is Metal on Apple Silicon. CUDA/NVIDIA validation is owner-deferred
in `TODO.md`; do not present it as supported or
complete without owner-supplied evidence.

## Build and test

```bash
just fmt                 # format Rust
just fmt-check           # verify formatting
just lint                # clippy for the active supported PostgreSQL major
just check               # type-check the active major
just check-matrix        # type-check every configured supported major
just deny                # license/advisory policy
just audit               # cargo-audit using deny.toml's ignored set
just doc-parity          # documentation citation/GUC/capability parity
just pg-version-audit    # PostgreSQL-version plumbing audit
just coverage            # Rust, C++/SYCL, and SQL coverage; each layer gates at 90%
just test                # pgrx test matrix
just gpu-build           # build the AdaptiveCpp/SYCL kernel library
just gpu-test            # run the standalone GPU kernel suite
just metal-stress        # run the Apple Silicon Metal stress gate
just package             # build an installable pgrx package
just release-verify      # run the release verification matrix
just release-checklist-audit # fail while release evidence is incomplete
just ci                  # pre-commit gates plus test matrix
```

Use `make clear-jit` as the canonical Metal JIT-cache clearing entry point.
Do not delete the cache with an ad hoc recursive command.

## Architecture

1. **Planner hooks**: `pg_accel/src/engine/ffi/planner_hooks/` observes paths;
   only `generic_groupagg` injects a normal production candidate.
2. **Shape model**: `planner_hooks/shape/` extracts a reducing query into a
   `ShapePlan` or returns a stable decline code.
3. **Neutral contract**: `pg_accel/src/engine/spec/` owns AQS3 logical semantics
   and the separate AOP2 output projection. It contains no device pointers.
4. **Residency**: `pg_accel/src/engine/residency/` owns backend-local resident
   columns/artifacts, generations, pins, and the cluster byte ledger.
5. **Custom Scan executor**: `pg_accel/src/engine/ffi/custom_scan/` validates the
   plan wire and delegates selected aggregates to
   `pg_accel/src/engine/executor/agg/`.
6. **GPU bridge/kernels**: `pg_accel/src/gpu/` and `pgaccel-kernels/` bind the
   frozen C ABI and run synchronous device calls.

Read [ARCHITECTURE.md](ARCHITECTURE.md) before changing a cross-layer contract.

## Production planner rules

- Normal path injection occurs only from `UPPERREL_GROUP_AGG` through
  `generic_groupagg::try_inject` at
  `pg_accel/src/engine/ffi/planner_hooks/mod.rs:204-208`.
- Base scans, filters, sorts, and function scans remain native at
  `pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:483-502`.
- Row-returning joins remain native at
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:123-131`. A star join
  consumed inside one aggregate descriptor is a different, childless shape.
- Windows and target-list SRFs remain native at
  `pg_accel/src/engine/ffi/planner_hooks/mod.rs:209-234`.
- Production raster planning is observation-only. Forced raster and spatial
  descriptor admissions are test-only.
- Adapter registration discovers an OID and operation contract. It does not add
  a PostgreSQL path.
- A new selected path requires complete shape extraction, runtime descriptor
  capability, resident proof, exact budget/currentness checks, device
  availability, cost admission, EXPLAIN evidence, and native-equivalence tests.
- Never use a test GUC to claim normal planner support.

## Safety rules

1. PostgreSQL C APIs and palloc-owned values stay on the backend main thread.
2. Device pointers live only in execution-time ABI bindings; never place them in
   AQS3, AOP2, `custom_private`, shared memory, or another backend.
3. Every `unsafe` block requires a specific `// SAFETY:` justification.
4. Do not use `unwrap()` outside tests. Propagate or map errors deliberately.
5. Check PostgreSQL interrupts between bounded synchronous calls. A running
   device call is not asynchronously cancellable.
6. A selected Custom Scan must fail on a GPU/contract error; it must not switch
   to a hidden PostgreSQL child executor.
7. `PARALLEL SAFE` means forked PostgreSQL processes, not Rust thread safety.
8. Shared-ledger reservations and backend-owned device resources must be
   released through the registered backend-exit lifecycle.
9. Pins resist local LRU eviction but never bypass the resident memory budget.
10. Device thresholds and chunk caps come from `DeviceLimits`; do not tune
    production selection with benchmark-only constants.
11. Rust/C ABI changes require matching layout, version, validation, and tests
    on both sides.
12. A planner decline is a valid outcome. Record a stable reason and leave the
    PostgreSQL path unchanged.

## Configuration

This is the complete released GUC inventory. The table must remain in semantic
parity with [README.md](README.md#configuration) and the registration source.
Test-only `pg_accel.test_*` settings are deliberately absent.

| Parameter | Type | Default | Context | Range | Effect |
|---|---|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | user | - | Planning master switch. While off, new pg_accel paths are not added; an already-planned Custom Scan fails closed if it reaches execution while off. |
| `pg_accel.min_batch_size` | int | `65536` | user | `1..16777216` | Fill target only for legacy row-fed Custom Scans. Resident descriptor calls use device-limit chunk caps and independent admission costs. |
| `pg_accel.gpu_enabled` | bool | `on` | user | - | Planning GPU-path switch. It adds no new GPU path while off and does not rewrite an already-planned path. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | user | `100..60000` | Millisecond warning threshold measured after a synchronous dispatch returns. Bounded dense aggregation checks interrupts between calls; it does not asynchronously cancel an in-flight call. |
| `pg_accel.max_workers_total` | int | `0` | superuser | `0..4096` | Cluster host-thread ledger cap; `0` is unlimited. Current executors request no host threads, and PostgreSQL parallel worker processes are not counted. |
| `pg_accel.resident_memory_budget_mb` | int | `-1` | superuser | `-1..1048576` | Cluster-wide MiB cap for all charged residency bytes. `-1` derives the cap from `DeviceLimits`; pins never bypass it. |
| `pg_accel.auto_load` | bool | `on` | user | - | Authorizes a selected resident plan to load missing columns synchronously. Explicit pin and reload of an existing pin remain authorized while off. |
| `pg_accel.cost_multiplier` | float | `1.0` | user | `0.1..10.0` | Cost multiplier only for resident generic aggregate candidates; values above one are more conservative. |
| `pg_accel.log_level` | enum | `notice` | user | `debug,info,notice,warning,error` | Initial per-backend trace filter, sampled when the first Custom Scan executes. Later changes do not rebuild the subscriber; `notice` and `warning` both map to WARN. |
| `pg_accel.assert_dispatch` | bool | `off` | user | - | Reserved no-op compatibility setting; it changes neither planning nor execution. |
| `pg_accel.parallel_fused_count` | bool | `off` | superuser | - | Reserved no-op roadmap setting; the parallel fused-count shape remains native. |
| `pg_accel.otel_log_max_mb` | int | `256` | user | `1..65536` | Per-file trace cap in MiB, sampled at trace initialization. A valid `PG_ACCEL_TRACE_FILE_MAX_BYTES` environment value takes precedence. |
| `pg_accel.otel_log_max_rotations` | int | `4` | user | `0..32` | Retained rotated trace files, sampled at trace initialization; `0` discards rotated copies. |
| `pg_accel.fp64_enabled` | bool | `on` | user | - | Deprecated no-op compatibility flag; it does not disable fp64 planning or execution. |
| `pg_accel.soft_fp64_cost_multiplier` | float | `32.0` | user | `1.0..64.0` | Extra planner cost for fp64 work when the device lacks native fp64. |

The main registrations are at `pg_accel/src/engine/gucs.rs:105-245`; the two
local fp64 registrations are at `pg_accel/src/lib.rs:260-283`.

## Device limits

Dispatch thresholds, maximum chunks, cost coefficients, and the derived
resident budget live in `DeviceLimits`, not the GUC table. They are selected
once per backend from the detected hardware profile or from the no-device
fallback. Inspect the values actually active in a session:

```sql
SELECT name, value, source
FROM pg_accel_device_limits()
ORDER BY name;
```

Do not quote a constant from `DeviceLimits::cpu_only()` as a GPU runtime value.
Benchmark artifacts must capture the SQL function output from the same backend
that planned the measured query.

## Residency operations

```sql
SELECT pg_accel_pin('table_name', ARRAY['needed_column']);
SELECT * FROM pg_accel_resident_status();
SELECT pg_accel_resident_live_bytes();
SELECT pg_accel_refresh('table_name');
SELECT pg_accel_unpin('table_name');
SELECT pg_accel_evict('table_name');
```

`pg_accel.auto_load=off` still permits an explicit pin and reload of an
existing pin. It prevents an ordinary selected plan from synchronously loading
missing unpinned data. The authorization check is at
`pg_accel/src/engine/residency/store.rs:1827-1865`.

## EXPLAIN and diagnostics

For a selected plan, require exact plan and execution evidence:

```text
Custom Scan (GpuAccelAgg)
  Plan Selected: true
  GPU Resident Pipeline: true
  GPU Resident Operator Class: resident_groupagg
  GPU Kernel Dispatched: true
```

The last property requires `EXPLAIN ANALYZE`. Also capture a before/after delta
from `pg_accel_kernel_executions()` when proving dispatch.

Useful backend-local diagnostics:

```sql
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_stats();
SELECT * FROM pg_accel_gpu_failures();
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_planner_rejection_count('no_gpu_resident_pipeline');
SELECT pg_accel_planner_overhead_us();
SELECT pg_accel_planner_fast_decline_count();
```

Tracing initializes lazily on the backend's first executing Custom Scan. It
writes bounded JSONL artifacts under `$PGDATA`; `pg_accel.log_level` and both
rotation GUCs are sampled at that initialization point.

## Benchmark evidence

- Baseline and accelerated arms use the same loaded extension and connection;
  the baseline sets `pg_accel.enabled=off` before planning.
- A workload definition is not a support claim. Native-decline workloads must
  contain no pg_accel node and must capture a stable rejection reason.
- A selected row needs exact plan label, `Plan Selected`, resident proof,
  `GPU Kernel Dispatched`, a positive kernel counter delta, consumed output,
  and a correctness oracle/diff.
- Keep warm and cold samples separate. Include setup, residency load/refresh,
  and derived-artifact costs explicitly rather than folding them into an
  unexplained speedup.
- Do not publish results without commit, PostgreSQL build, device info, effective
  GUCs, effective device limits, command line, seed, iteration/warmup counts,
  raw samples, and plan evidence.
- Do not introduce a PostgreSQL single-thread comparison lane. The repository
  benchmark contract compares against its configured PostgreSQL baseline.

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Review discipline

For changes spanning planner, plan wire, residency, executor, or kernel ABI,
review each boundary independently and run focused tests before the broad
matrix. A passing kernel test does not prove planner admission; a selected plan
does not prove dispatch; dispatch does not prove result correctness.

Never fabricate evidence, weaken a gate, hide a native decline, or describe a
stub/dormant path as complete. When citing source, use an exact repository-root
`path:line` or `path:start-end` reference and run `just doc-parity`.

## Commit convention

Use a scoped conventional subject, for example:

```text
docs(architecture): describe resident aggregate admission
fix(planner): reject stale residency proof
test(residency): cover generation invalidation
```

---
name: pg_accel Thread Safety Model
description: Single-threaded main-backend execution model, SYCL-side device parallelism, shared-memory thread budget skeleton, fork safety on Apple Silicon, and PG process-model constraints
---

# pg_accel Thread Safety Model

pg_accel has **no CPU worker threads**. All Rust code runs on the main
PostgreSQL backend thread. Parallelism comes from:

1. **SYCL / AdaptiveCpp device-side work** (CUDA / ROCm / Level Zero / Metal)
   dispatched synchronously from the main thread via the C++ bridge.
2. **PG's own parallel workers** (forked processes, not threads) when pg_accel
   does not replace the plan.

There is no rayon, no `std::thread::spawn`, no crossbeam, no tokio. Verify
with `rg '\brayon\b|thread::spawn|crossbeam|tokio' pg_accel/src` — no matches.

## Core rules

### R1: Everything Rust runs on the main backend thread
All `dispatch::*`, all executor nodes (`src/engine/executor/*`), all GPU bridge
calls (`src/gpu/bridge.rs`), the planner hooks
(`src/engine/ffi/planner_hooks/`) and Custom Scan callbacks
(`src/engine/ffi/custom_scan/`) execute on the backend that was handed the
query. `dispatch` is marked `unsafe` with the contract that it must be called
on the main backend thread (`src/engine/dispatch/mod.rs:68-73`).

### R2: SYCL work blocks the main thread
GPU kernels are enqueued + awaited synchronously; the main thread waits on the
SYCL queue. There is no background GPU polling thread. This means PG FFI
(`palloc`, `FunctionCallInvoke`, `ereport`, `CurrentMemoryContext`, SPI,
syscache, elog) is always safe to call before/after a GPU dispatch — there is
no thread boundary to respect.

### R3: CHECK_FOR_INTERRUPTS between batches
Cancellation (`pg_cancel_backend`, statement timeout) is only processed when
the backend calls `CHECK_FOR_INTERRUPTS()`. Executors do this between batches:

- `src/engine/executor/scan/exec.rs:68`
- `src/engine/executor/agg/execute.rs:915` (every 8192 rows)
- `src/engine/executor/vectorized_scan.rs:138` (every 8192 rows)
- `src/engine/executor/join/mod.rs:272` (between outer tuples)
- `src/engine/executor/preagg/mod.rs:477`

The stride between checks lives in `DeviceLimits`
(`src/engine/cost/device_limits.rs:76`), not as a magic constant.

`pg_accel.kernel_timeout_ms` is a warning threshold recorded after a synchronous
GPU runtime call returns. It does not cancel an in-flight kernel, so statement
timeout and user cancel can be observed only after the current dispatch wait
finishes and the backend reaches the next interrupt check.

### R4: PARALLEL SAFE labels describe forked processes, not threads
When the planner marks a Custom Scan parallel-safe, PG may run it inside its
own parallel workers — each worker is a **forked process** with its own
address space. Nothing is shared across workers except shared memory. Do not
treat the parallel marker as permission to spawn threads.

### R5: Fork safety (Apple Silicon / Metal)
SYCL queues, Metal devices, and AdaptiveCpp SSCP state cannot survive
`fork(2)`. `_PG_init` runs in the postmaster before fork; heavy GPU init is
deferred to each backend's first query (see `src/lib.rs:107-113`,
`src/gpu/mod.rs:68-74`). The only thing done pre-fork is `prefork_warmup`
(`pgaccel_prefork_warmup` FFI, `src/gpu/bridge.rs:37-39`) which touches
SkyLight / IOKit so `MTLCreateSystemDefaultDevice` in the forked child does
not crash. `prefork_warmup` explicitly **does not spawn threads**.

The `.metallib` + `.metalar` cache in `~/.acpp/apps/global/jit-cache/` is how
kernel dispatch works in a forked backend without `MTLCompilerService`; see
top-level CLAUDE.md "MTLBinaryArchive cache" for diagnostics.

## Shared-memory thread budget (skeleton)

`src/engine/thread_budget.rs` defines a `PgLwLock<ThreadBudgetData>` that
tracks per-backend thread allocations in shared memory (256 backend slots).
The API:

- `request_threads(n) -> i32` — `src/engine/thread_budget.rs:105`
- `release_threads(n)` — `src/engine/thread_budget.rs:142`
- `cleanup_backend()` — `src/engine/thread_budget.rs:169`, wired to
  `before_shmem_exit` via `pgaccel_shmem_exit` in `src/lib.rs:131-138`
- `init_shmem()` — called from `_PG_init` (`src/lib.rs:92`)
- `reclaim_dead_backends()` — probes tracked PIDs with
  `BackendPidGetProc` under the exclusive lock
  (`src/engine/thread_budget.rs:219`) and frees slots for gone backends.

**Current status:** the budget is a GUC-backed skeleton. `request_threads`
reads `pg_accel.max_workers_total`; `0` means unlimited, and positive values
cap recorded per-backend allocations. Nothing in the current executor calls
`request_threads`/`release_threads` today, so this does not imply a CPU worker
pool exists. The LwLock, shmem init, and `before_shmem_exit` wiring are kept so
future host-thread budgeting remains cheap. Do not delete them.

## Signal masking

Not applicable. There are no worker threads to mask signals on.
`pthread_sigmask` is never called in Rust code (`rg pthread_sigmask
pg_accel/src` → no matches). Signals are handled by PG on the backend
process as normal.

## Interaction with PG parallel query

- If pg_accel injects a Custom Scan, PG's Gather / parallel workers are
  typically replaced — GPU provides the parallelism.
- If pg_accel runs underneath a Gather (partial aggregate path,
  `src/engine/ffi/planner_hooks/partial_agg.rs`), each forked worker has
  its own backend, its own SYCL queue (created lazily after fork), and
  its own `BackendSlot`. No coordination between workers at the pg_accel
  layer — PG handles the partial→final aggregation.

## Cleanup checklist

- Normal query end → Custom Scan `End` node path runs.
- Error / statement timeout / cancel → PG runs `End` paths via
  `AbortTransaction`.
- Backend exit (normal or `pg_terminate_backend`) →
  `pgaccel_shmem_exit` fires, calling `thread_budget::cleanup_backend`
  and `otel::flush_tracing` (`src/lib.rs:133-138`).
- `kill -9` → other backends' next `request_threads` detects the dead
  PID via `BackendPidGetProc` and reclaims the slot
  (`thread_budget.rs:234`).

## What must still be done on the main thread

Even though there are no worker threads, executors still obey the
"PG-C-functions-on-main-backend-only" invariant because code paths that
look like they could be parallelized (predicate eval, tuple build,
`heap_getnext`, `ExecEvalExpr`, `FunctionCallInvoke`) are all in PG C
and all called from the executor node's `Next` callback on the main
backend. Do not introduce a thread pool to "help" any of these — the
project has repeatedly chosen SYCL-side parallelism over CPU threading
(see `CLAUDE.md` rules #1, #7, #8, #9).

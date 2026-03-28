---
name: pg_accel Thread Safety Model
description: Rules for rayon thread pool interaction with PostgreSQL's process model, signal handling, and thread budget
---

# pg_accel Thread Safety Model

## Core Rules

### Rule 0: NO PG C function calls from rayon threads
ALL PG/extension C functions are called on the main backend thread only.
Even trivially cheap functions like `int4abs` aren't worth threading — rayon
dispatch overhead per item exceeds a single arithmetic instruction. And every
non-trivial PG function calls `palloc` (unsafe from threads).

**Two dispatch strategies:**
1. **BatchedEval** — all C functions on main thread, Custom Scan node batching
2. **GpuSpatial** — GPU kernel (layers 1+2) + CPU recheck on main thread (layer 3)

rayon is used ONLY for:
- GPU kernel orchestration (launching + collecting GPU work)
- Parallel sort-key extraction (extracting Rust values from Datums for GPU sort)
- Top-k merge across partitions

### Rule 1: rayon threads MUST NOT call PG functions
rayon worker threads can ONLY:
- Read from memory set up by the main thread
- Write results to pre-allocated buffers
- Do pure Rust computation (sort merging, data extraction, GPU dispatch)

rayon threads MUST NOT:
- Call ANY PG C function (not even "simple" ones like `int4abs`)
- Call `palloc`, `pfree`, or any PG memory allocator
- Access PG catalog (SPI, syscache)
- Call `elog`, `ereport`
- Touch PG's memory contexts
- Call `CHECK_FOR_INTERRUPTS()`
- Access `CurrentMemoryContext` or any thread-local PG state

### Rule 2: Signal masking
PG uses signals for cancellation and communication. rayon threads MUST mask:
- `SIGINT` (cancel)
- `SIGTERM` (terminate)
- `SIGUSR1` (latch)
- `SIGUSR2`

```rust
rayon::ThreadPoolBuilder::new()
    .start_handler(|_| {
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGUSR1);
            libc::sigaddset(&mut set, libc::SIGUSR2);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
    })
    .build()
```

### Rule 3: CHECK_FOR_INTERRUPTS between batches
The main thread (PG backend) must call `CHECK_FOR_INTERRUPTS()` between every batch.
This is how PG processes cancel requests. If we don't call this, `pg_cancel_backend()`
will appear to hang.

```rust
for batch in batches {
    // Evaluate predicates on main thread (BatchedEval)
    let results = evaluate_batch(batch, fmgr_info);

    // CRITICAL: check for cancel between batches
    unsafe { pg_sys::CHECK_FOR_INTERRUPTS() };

    emit_results(results);
}
```

### Rule 4: Datum stays on main thread
`pg_sys::Datum` contains raw pointers that may reference PG backend-local memory.
Since all function dispatch is on the main thread (BatchedEval), Datums never
cross thread boundaries for function calls.

For GPU sort/reduce, extract to Rust values first:
```rust
// Extract sort keys on main thread
let keys: Vec<f64> = datums.iter().map(|d| extract_f64(*d)).collect();
// GPU sort operates on extracted Rust values, not Datums
let sorted_indices = gpu_sort(&keys);
```

### Rule 5: All function dispatch is BatchedEval
Every PG/extension C function call goes through `FunctionCallInvoke` on the main
thread. The Custom Scan node provides speedup via late materialization and predicate
reordering — not via parallelizing function calls. GPU kernels provide parallelism
for spatial predicates only.

## Thread Budget System

### Architecture
```
Shared Memory (via LWLock):
┌──────────────────────────────────┐
│ total_active_threads: i32        │
│ max_total: i32 (from GUC)       │
│ per_backend_slots: [(pid, n)]    │
└──────────────────────────────────┘

Per-Backend:
┌──────────────────────────────────┐
│ rayon::ThreadPool (lazily init)  │
│ my_thread_count: usize           │
│ my_slot_index: usize             │
└──────────────────────────────────┘
```

### Lifecycle
```
Query starts:
  1. request_threads(wanted) → granted (may be < wanted)
  2. rayon pool available for GPU orchestration + sort-key extraction
  3. Execute batches (C function calls on main thread, GPU work via rayon)

Query ends (normal):
  4. release_threads(granted) → counter decremented

Backend exits (normal):
  5. before_shmem_exit callback → release_threads(my_count)

Backend crashes (kill -9):
  6. Other backends detect stale PID on next request_threads()
     → reclaim dead backend's budget
```

### Budget Exhaustion
When `request_threads()` returns 0:
- GpuSpatial downgrades to BatchedEval (CPU recheck for all rows)
- GPU sort/reduce falls back to main-thread sequential
- Query still returns correct results
- No error, no retry

### Interaction with PG Parallel

When GpuAccelScan REPLACES a Gather + parallel workers plan:
- PG's workers DON'T spawn (our Custom Scan replaces the parallel plan)
- GPU provides parallelism instead of PG's forked workers

When pg_accel accelerates WITHIN a PG parallel plan:
- Both our rayon pool AND PG's workers may be active
- Budget auto-calc subtracts max_parallel_workers to avoid oversubscription

### Auto Thread Calculation
```
available_cores = cpu_cores - max_parallel_workers
expected_active_backends = max(max_connections / 4, 1)
per_backend = clamp(available_cores / expected_active_backends, 1, min(cpu_cores, 16))
```

## Error Handling

Errors in rayon threads cannot use PG's ereport. Strategy:

```rust
// In rayon thread: capture error as Result
let results: Vec<Result<f64, PgAccelError>> = batch
    .par_iter()
    .map(|v| -> Result<f64, PgAccelError> {
        // ... computation that might fail
        Ok(result)
    })
    .collect();

// On main thread: check for errors, then use PG's error reporting
for result in &results {
    if let Err(e) = result {
        // Now safe to call ereport on main thread
        ereport!(ERROR, PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                 "pg_accel: batch dispatch error: {}", e);
    }
}
```

## Cleanup Checklist

Every code path that acquires thread budget MUST have a corresponding release:
- [ ] Normal query completion → EndCustomScan releases budget
- [ ] LIMIT early termination → EndCustomScan releases budget
- [ ] Error/exception → EndCustomScan releases budget (PG calls End on error)
- [ ] Statement timeout → EndCustomScan releases budget
- [ ] pg_cancel_backend → EndCustomScan releases budget
- [ ] pg_terminate_backend → before_shmem_exit callback releases budget
- [ ] kill -9 → other backends detect stale PID and reclaim

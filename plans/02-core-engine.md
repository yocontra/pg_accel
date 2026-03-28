# Phase 2: Core Engine

**Depends on:** Phase 0
**Parallelism:** All 10 agents, distinct modules

This phase builds the batch dispatch engine — the heart of pg_accel's CPU acceleration.
After this phase, we can call any PARALLEL SAFE PG function in batch-parallel via rayon.

---

## Agent Assignments

### A0 — Rayon Thread Pool + Signal Safety
**Status:** Not Started
**Owns:** `pg_accel/src/core/thread_pool.rs`

**Tasks:**
- [ ] Create a per-backend rayon `ThreadPool` lazily on first GPU-accelerated query
- [ ] Read thread count from `pg_accel.workers` GUC (auto-calculated or explicit)
- [ ] Use rayon threads for GPU kernel orchestration, parallel sort-key extraction, and top-k merge — NOT for calling PG C functions
- [ ] If no GPU is available and no sort/reduce workload needs it, the pool may never be created for a given backend
- [ ] Mask signals in rayon worker threads (SIGINT, SIGTERM, SIGUSR1, SIGUSR2) so only the main PG backend thread handles signals:
  ```rust
  ThreadPoolBuilder::new()
      .num_threads(count)
      .start_handler(|_| {
          // Mask SIGINT, SIGTERM, SIGUSR1, SIGUSR2
          // Only the main PG backend thread handles signals
          unsafe { pthread_sigmask(SIG_BLOCK, &signal_set, null_mut()) }
      })
      .build()
  ```
- [ ] Ensure pool is per-backend (not global)
- [ ] Destroy pool on backend exit via `before_shmem_exit` hook

**Agent gate:**
- [ ] Thread pool creates with correct thread count matching GUC
- [ ] Worker threads have signals masked (verify via `/proc/self/status` or equivalent)
- [ ] Pool cleanup runs on backend exit (no leaked threads after `pg_terminate_backend`)
- [ ] Main thread can still receive SIGINT between batches

**Implementation log:**
_(no deviations)_

### A1 — Thread Budget (Shared Memory)
**Status:** Not Started
**Owns:** `pg_accel/src/core/thread_budget.rs`

**Tasks:**
- [ ] Implement shared memory counter protected by LWLock
- [ ] Implement `request_threads(n) -> usize` — returns number actually granted (may be < n)
- [ ] Implement `release_threads(n)` — decrements counter
- [ ] If `max_workers_total = 0`: always grant full request (no limit)
- [ ] If budget exhausted: return 0, caller falls back to sequential
- [ ] Register via `pg_shmem_init!` in `_PG_init` (requires `shared_preload_libraries`)
- [ ] Implement cleanup via `before_shmem_exit` callback that always releases this backend's threads, even on crash/kill
- [ ] Track per-backend allocation for recovery

**Agent gate:**
- [ ] `#[pg_test]`: request(4) -> 4 granted, release(4) -> counter back to 0
- [ ] With `max_workers_total = 8`: two backends requesting 6 each -> first gets 6, second gets 2
- [ ] After `pg_terminate_backend()` on one: counter decremented correctly
- [ ] `max_workers_total = 0`: always grants full request

**Implementation log:**
_(no deviations)_

### A2 — Batch Accumulator
**Status:** Not Started
**Owns:** `pg_accel/src/core/batch.rs`

**Tasks:**
- [ ] Define `BatchAccumulator` struct:
  ```rust
  pub struct BatchAccumulator {
      buffer: Vec<(pg_sys::Datum, bool)>,  // (datum, is_null)
      batch_size: usize,
      flushed: usize,
  }
  ```
- [ ] Implement `push(datum, is_null)` — adds to buffer
- [ ] Implement `should_flush()` — true when buffer.len() >= batch_size or scan is ending
- [ ] Implement `flush() -> &[(Datum, bool)]` — returns batch, resets buffer
- [ ] Implement `finish()` — flushes remaining rows (partial last batch)
- [ ] Handle LIMIT: external caller can stop pushing, flush partial
- [ ] NULL passthrough: NULLs stored in buffer but skipped during dispatch (STRICT functions)

**Agent gate:**
- [ ] Accumulate 5000 rows with batch_size=2000 -> 3 flushes (2000+2000+1000)
- [ ] Accumulate 100 rows with batch_size=2000 -> 1 flush of 100 on finish()
- [ ] NULL rows tracked but not dispatched for STRICT functions
- [ ] Zero-copy: datums stored by value, not cloned

**Implementation log:**
_(no deviations)_

### A3 — Batch Dispatch Core
**Status:** Not Started
**Owns:** `pg_accel/src/core/dispatch.rs`

**Tasks:**
- [ ] Implement **Strategy 1: BatchedEval** (all PG/extension C functions) via `dispatch_batched_eval(batch, fmgr_info) -> Vec<(Datum, bool)>`
- [ ] Call function via normal `FunctionCallInvoke` on main thread, one at a time
- [ ] Call `CHECK_FOR_INTERRUPTS()` every N calls (e.g., every 1000)
- [ ] NULL passthrough: STRICT functions with NULL args -> NULL without calling
- [ ] ALL PG C functions run on the main backend thread — no rayon for function calls
- [ ] Implement late materialization: only deserialize expensive columns for rows that pass cheap predicates (e.g., skip geometry deser for 95% of rows filtered by int)
- [ ] Implement predicate reordering: evaluate cheapest predicate first, most selective first
- [ ] Implement column-at-a-time deserialization: batch deser of one column, then filter, then deser next column (cache-friendly, avoids touching cold columns)
- [ ] Implement **Strategy 2: GpuSpatial** (GPU + CPU recheck) — handled by `gpu/three_layer.rs` with GPU kernels for layers 1+2 and CPU recheck on main thread for UNCERTAIN results
- [ ] rayon IS used for: GPU kernel orchestration (launching + collecting GPU work), parallel sort-key extraction (extracting Rust values from Datums for GPU sort), and top-k merge across partitions (merging partial results)
- [ ] Do NOT use rayon for C function calls — even trivially cheap functions like `int4abs` aren't worth threading (rayon dispatch overhead per item exceeds single arithmetic instruction cost), and every non-trivial PG function calls `palloc` (touches `CurrentMemoryContext`), making it unsafe from threads

**Agent gate:**
- [ ] 10K calls to `abs(int4)` via BatchedEval = identical results to PG sequential
- [ ] 10K calls to `lower(text)` via BatchedEval = identical results to PG sequential
- [ ] NULL passthrough: `abs(NULL)` -> NULL without calling function
- [ ] `CHECK_FOR_INTERRUPTS()` called periodically during batch
- [ ] Late materialization: wide table with selective int filter + expensive geometry predicate -> fewer geometry deserializations than vanilla PG (verify via counter)

**Implementation log:**
_(no deviations)_

### A4 — Dispatch Fallback Logic
**Status:** Not Started
**Owns:** `pg_accel/src/core/dispatch_fallback.rs`

**Tasks:**
- [ ] Implement vanilla PG fallback when any one condition triggers:
  1. `pg_accel.enabled = off`
  2. `should_batch(rows, cost)` returns false
  3. Function OID not in accelerated registry
  4. Estimated rows below `min_batch_size`
- [ ] Implement GpuSpatial -> BatchedEval downgrade when any one condition triggers:
  5. `pg_accel.gpu_enabled = off` -> use BatchedEval (CPU recheck for all rows)
  6. GPU unavailable at runtime -> use BatchedEval
  7. Thread budget exhausted for GPU orchestration -> use BatchedEval
- [ ] Log reason for each fallback/downgrade at DEBUG level
- [ ] Vanilla fallback calls the original PG function one-at-a-time — zero overhead, bit-identical to vanilla PG

**Agent gate:**
- [ ] `SET pg_accel.enabled = off;` -> query returns correct result via vanilla path
- [ ] Function not in registry -> vanilla path, no error
- [ ] Thread budget = 0 -> sequential, correct result, logged
- [ ] Fallback reason visible in `SET pg_accel.log_level = debug;` output

**Implementation log:**
_(no deviations)_

### A5 — _PG_init Pipeline
**Status:** Not Started
**Owns:** `pg_accel/src/lib.rs` (_PG_init implementation)

**Tasks:**
- [ ] Wire everything from Phase 1 + 2 together in `_PG_init`:
  1. Register GUCs
  2. Initialize shared memory (thread budget counter) via `pg_shmem_init!`
  3. Register `before_shmem_exit` cleanup callback
  4. Detect platform (cpu cores, GPU availability)
  5. Probe installed extensions (`pg_extension` catalog)
  6. Initialize adapters for detected extensions
  7. Discover functions via `pg_proc` matching
  8. Build `OID -> FunctionAccelEntry` HashMap
  9. Build `OID -> TypeExtractor` HashMap
  10. Log summary: "pg_accel: detected [PostGIS 3.x, h3 4.x], registered N functions (M parallel-safe), thread budget: K/backend (auto), GPU: available/unavailable"

**Agent gate:**
- [ ] Log output shows all detected extensions + function counts
- [ ] Thread budget reported accurately
- [ ] GPU status correct (Metal on Mac, CUDA on NVIDIA Linux, unavailable without gpu feature)
- [ ] Init completes in < 100ms

**Implementation log:**
_(no deviations)_

### A6 — Stats Infrastructure
**Status:** Not Started
**Owns:** `pg_accel/src/core/stats.rs`

**Tasks:**
- [ ] Implement per-backend counters (not shared memory — per-process):
  - `queries_accelerated: u64`
  - `rows_dispatched: u64`
  - `batches_executed: u64`
  - `total_dispatch_us: u64` (microseconds)
  - `fallback_count: u64`
  - `gpu_rows_processed: u64`
  - `gpu_uncertain_count: u64` (rows that fell back to CPU recheck)
  - `thread_budget_exhausted_count: u64`
- [ ] Implement SQL function `pg_accel_stats()` -> returns all counters as a row
- [ ] Implement SQL function `pg_accel_reset_stats()` -> zeros all counters

**Agent gate:**
- [ ] After 100 accelerated queries: `pg_accel_stats()` shows non-zero values
- [ ] After `pg_accel_reset_stats()`: all zeros
- [ ] Counters increment correctly (dispatch_us > 0, rows match expected)

**Implementation log:**
_(no deviations)_

### A7 — Device Info SQL Function
**Status:** Not Started
**Owns:** `pg_accel/src/core/device_info.rs`

**Tasks:**
- [ ] Implement `pg_accel_device_info()` returning:
  - `cpu_cores: int`
  - `rayon_threads: int` (per this backend, auto-calculated or explicit)
  - `gpu_available: bool`
  - `gpu_device_name: text` (or "N/A")
  - `memory_model: text` ("unified" / "discrete" / "cpu_only")
  - `extensions_detected: text[]`
  - `functions_registered: int`
  - `parallel_safe_count: int`
  - `pg_version: int`
  - `pg_accel_version: text`

**Agent gate:**
- [ ] `SELECT * FROM pg_accel_device_info();` returns populated row
- [ ] On Metal Mac: gpu_available = true, gpu_device_name = "Apple M3 Max", memory_model = "unified"
- [ ] On NVIDIA Linux: gpu_available = true, gpu_device_name = "NVIDIA RTX 4070", memory_model = "discrete"
- [ ] On Linux no GPU: gpu_available = false, memory_model = "cpu_only"
- [ ] functions_registered matches actual discovered count

**Implementation log:**
_(no deviations)_

### A8 — Integration Test Harness
**Status:** Not Started
**Owns:** `pg_accel/tests/integration.rs`

**Tasks:**
- [ ] Build framework for end-to-end tests that:
  1. Create test table with known data
  2. Run query with `pg_accel.enabled = on`
  3. Run same query with `pg_accel.enabled = off`
  4. Assert results are bit-identical
  5. Assert `pg_accel_stats()` shows expected dispatch counts
- [ ] Implement helper macro `assert_accel_matches!(query, setup_sql)` — runs both ways, compares
- [ ] Implement helper macro `assert_accel_faster!(query, setup_sql, min_speedup)` — also checks timing

**Agent gate:**
- [ ] Framework compiles and runs
- [ ] Trivial test: `SELECT abs(x) FROM generate_series(1,1000) x` -> ON == OFF
- [ ] Stats show 1000 rows dispatched after test

**Implementation log:**
_(no deviations)_

### A9 — Benchmark Framework Scaffold
**Status:** Not Started
**Owns:** `pg_accel_bench/` crate

**Tasks:**
- [ ] Implement CLI with subcommands: `setup`, `run`, `report`
- [ ] `setup --rows N` — creates test tables with seeded RNG data
- [ ] `run --workload <name> --iterations N` — runs query, collects `EXPLAIN ANALYZE` timing
- [ ] `report --format markdown|json` — outputs results
- [ ] Define Workload trait:
  ```rust
  pub trait Workload {
      fn name(&self) -> &str;
      fn setup_sql(&self, rows: usize, seed: u64) -> Vec<String>;
      fn query_sql(&self) -> String;
  }
  ```
- [ ] Stub 3 workloads: `simple_agg`, `spatial_join`, `large_sort`

**Agent gate:**
- [ ] `cargo build -p pg_accel_bench` succeeds
- [ ] `pg_accel_bench setup --rows 1000` creates tables
- [ ] `pg_accel_bench run --workload simple_agg --iterations 3` produces timing output

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] dispatch_batch_parallel produces identical results to sequential PG for all 8 types
- [ ] Thread pool creates, masks signals, cleans up on exit
- [ ] Thread budget correctly tracks across 4 concurrent backends
- [ ] Fallback logic triggers correctly for all 5 conditions
- [ ] _PG_init pipeline completes and logs summary
- [ ] pg_accel_stats() and pg_accel_device_info() return correct data
- [ ] pg_accel_bench framework operational with 3 stub workloads
- [ ] cargo pgrx test pg17 — all tests pass including new integration tests
- [ ] Docker integration: dispatch batch queries (ON == OFF) on real PG with real data
- [ ] Docker integration: all Phase 0 tests still pass (no regressions)

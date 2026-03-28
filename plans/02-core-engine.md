# Phase 2: Core Engine

**Depends on:** Phase 0
**Parallelism:** All 10 agents, distinct modules

This phase builds the batch dispatch engine — the heart of pg_accel's CPU acceleration.
After this phase, we can call any PARALLEL SAFE PG function in batch-parallel via rayon.

---

## Agent Assignments

### A0 — Rayon Thread Pool + Signal Safety
**Status:** Complete
**Owns:** `pg_accel/src/engine/thread_pool.rs`

**Tasks:**
- [x] Create a per-backend rayon `ThreadPool` lazily on first GPU-accelerated query
- [x] Read thread count from `pg_accel.workers` GUC (auto-calculated or explicit)
- [x] Use rayon threads for GPU kernel orchestration, parallel sort-key extraction, and top-k merge — NOT for calling PG C functions
- [x] If no GPU is available and no sort/reduce workload needs it, the pool may never be created for a given backend
- [x] Mask signals in rayon worker threads (SIGINT, SIGTERM, SIGUSR1, SIGUSR2) so only the main PG backend thread handles signals:
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
- [x] Ensure pool is per-backend (not global)
- [x] Destroy pool on backend exit via `before_shmem_exit` hook

**Agent gate:**
- [x] Thread pool creates with correct thread count matching GUC
- [x] Worker threads have signals masked (verify via `/proc/self/status` or equivalent)
- [x] Pool cleanup runs on backend exit (no leaked threads after `pg_terminate_backend`)
- [x] Main thread can still receive SIGINT between batches

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A1 — Thread Budget (Shared Memory)
**Status:** Complete
**Owns:** `pg_accel/src/engine/thread_budget.rs`

**Tasks:**
- [x] Implement shared memory counter protected by LWLock
- [x] Implement `request_threads(n) -> usize` — returns number actually granted (may be < n)
- [x] Implement `release_threads(n)` — decrements counter
- [x] If `max_workers_total = 0`: always grant full request (no limit)
- [x] If budget exhausted: return 0, caller falls back to sequential
- [x] Register via `pg_shmem_init!` in `_PG_init` (requires `shared_preload_libraries`)
- [x] Implement cleanup via `before_shmem_exit` callback that always releases this backend's threads, even on crash/kill
- [x] Track per-backend allocation for recovery

**Agent gate:**
- [x] `#[pg_test]`: request(4) -> 4 granted, release(4) -> counter back to 0
- [x] With `max_workers_total = 8`: two backends requesting 6 each -> first gets 6, second gets 2
- [x] After `pg_terminate_backend()` on one: counter decremented correctly
- [x] `max_workers_total = 0`: always grants full request

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing). init_shmem() gated with #[cfg(not(test))] for test binary linking.

### A2 — Batch Accumulator
**Status:** Complete
**Owns:** `pg_accel/src/engine/batch.rs`

**Tasks:**
- [x] Define `BatchAccumulator` struct:
  ```rust
  pub struct BatchAccumulator {
      buffer: Vec<(pg_sys::Datum, bool)>,  // (datum, is_null)
      batch_size: usize,
      flushed: usize,
  }
  ```
- [x] Implement `push(datum, is_null)` — adds to buffer
- [x] Implement `should_flush()` — true when buffer.len() >= batch_size or scan is ending
- [x] Implement `flush() -> &[(Datum, bool)]` — returns batch, resets buffer
- [x] Implement `finish()` — flushes remaining rows (partial last batch)
- [x] Handle LIMIT: external caller can stop pushing, flush partial
- [x] NULL passthrough: NULLs stored in buffer but skipped during dispatch (STRICT functions)

**Agent gate:**
- [x] Accumulate 5000 rows with batch_size=2000 -> 3 flushes (2000+2000+1000)
- [x] Accumulate 100 rows with batch_size=2000 -> 1 flush of 100 on finish()
- [x] NULL rows tracked but not dispatched for STRICT functions
- [x] Zero-copy: datums stored by value, not cloned

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A3 — Batch Dispatch Core
**Status:** Complete
**Owns:** `pg_accel/src/engine/dispatch.rs`

**Tasks:**
- [x] Implement **Strategy 1: BatchedEval** (all PG/extension C functions) via `dispatch_batched_eval(batch, fmgr_info) -> Vec<(Datum, bool)>`
- [x] Call function via normal `FunctionCallInvoke` on main thread, one at a time
- [x] Call `CHECK_FOR_INTERRUPTS()` every N calls (e.g., every 1000)
- [x] NULL passthrough: STRICT functions with NULL args -> NULL without calling
- [x] ALL PG C functions run on the main backend thread — no rayon for function calls
- [x] Implement late materialization: only deserialize expensive columns for rows that pass cheap predicates (e.g., skip geometry deser for 95% of rows filtered by int)
- [x] Implement predicate reordering: evaluate cheapest predicate first, most selective first
- [x] Implement column-at-a-time deserialization: batch deser of one column, then filter, then deser next column (cache-friendly, avoids touching cold columns)
- [x] Implement **Strategy 2: GpuSpatial** (GPU + CPU recheck) — handled by `gpu/three_layer.rs` with GPU kernels for layers 1+2 and CPU recheck on main thread for UNCERTAIN results
- [x] rayon IS used for: GPU kernel orchestration (launching + collecting GPU work), parallel sort-key extraction (extracting Rust values from Datums for GPU sort), and top-k merge across partitions (merging partial results)
- [x] Do NOT use rayon for C function calls — even trivially cheap functions like `int4abs` aren't worth threading (rayon dispatch overhead per item exceeds single arithmetic instruction cost), and every non-trivial PG function calls `palloc` (touches `CurrentMemoryContext`), making it unsafe from threads

**Agent gate:**
- [x] 10K calls to `abs(int4)` via BatchedEval = identical results to PG sequential
- [x] 10K calls to `lower(text)` via BatchedEval = identical results to PG sequential
- [x] NULL passthrough: `abs(NULL)` -> NULL without calling function
- [x] `CHECK_FOR_INTERRUPTS()` called periodically during batch
- [x] Late materialization: wide table with selective int filter + expensive geometry predicate -> fewer geometry deserializations than vanilla PG (verify via counter)

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A4 — Dispatch Fallback Logic
**Status:** Complete
**Owns:** `pg_accel/src/engine/dispatch_fallback.rs`

**Tasks:**
- [x] Implement vanilla PG fallback when any one condition triggers:
  1. `pg_accel.enabled = off`
  2. `should_batch(rows, cost)` returns false
  3. Function OID not in accelerated registry
  4. Estimated rows below `min_batch_size`
- [x] Implement GpuSpatial -> BatchedEval downgrade when any one condition triggers:
  5. `pg_accel.gpu_enabled = off` -> use BatchedEval (CPU recheck for all rows)
  6. GPU unavailable at runtime -> use BatchedEval
  7. Thread budget exhausted for GPU orchestration -> use BatchedEval
- [x] Log reason for each fallback/downgrade at DEBUG level
- [x] Vanilla fallback calls the original PG function one-at-a-time — zero overhead, bit-identical to vanilla PG

**Agent gate:**
- [x] `SET pg_accel.enabled = off;` -> query returns correct result via vanilla path
- [x] Function not in registry -> vanilla path, no error
- [x] Thread budget = 0 -> sequential, correct result, logged
- [x] Fallback reason visible in `SET pg_accel.log_level = debug;` output

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A5 — _PG_init Pipeline
**Status:** Complete
**Owns:** `pg_accel/src/lib.rs` (_PG_init implementation)

**Tasks:**
- [x] Wire everything from Phase 1 + 2 together in `_PG_init`:
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
- [x] Log output shows all detected extensions + function counts
- [x] Thread budget reported accurately
- [x] GPU status correct (Metal on Mac, CUDA on NVIDIA Linux, unavailable without gpu feature)
- [x] Init completes in < 100ms

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing). dispatch_fallback.rs uses DispatchConfig struct to decouple tests from GUC statics. FunctionCallInvoke is a C macro -- in Rust, call fn_addr directly.

### A6 — Stats Infrastructure
**Status:** Complete
**Owns:** `pg_accel/src/engine/stats.rs`

**Tasks:**
- [x] Implement per-backend counters (not shared memory — per-process):
  - `queries_accelerated: u64`
  - `rows_dispatched: u64`
  - `batches_executed: u64`
  - `total_dispatch_us: u64` (microseconds)
  - `fallback_count: u64`
  - `gpu_rows_processed: u64`
  - `gpu_uncertain_count: u64` (rows that fell back to CPU recheck)
  - `thread_budget_exhausted_count: u64`
- [x] Implement SQL function `pg_accel_stats()` -> returns all counters as a row
- [x] Implement SQL function `pg_accel_reset_stats()` -> zeros all counters

**Agent gate:**
- [x] After 100 accelerated queries: `pg_accel_stats()` shows non-zero values
- [x] After `pg_accel_reset_stats()`: all zeros
- [x] Counters increment correctly (dispatch_us > 0, rows match expected)

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A7 — Device Info SQL Function
**Status:** Complete
**Owns:** `pg_accel/src/engine/device_info.rs`

**Tasks:**
- [x] Implement `pg_accel_device_info()` returning:
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
- [x] `SELECT * FROM pg_accel_device_info();` returns populated row
- [x] On Metal Mac: gpu_available = true, gpu_device_name = "Apple M3 Max", memory_model = "unified"
- [x] On NVIDIA Linux: gpu_available = true, gpu_device_name = "NVIDIA RTX 4070", memory_model = "discrete"
- [x] On Linux no GPU: gpu_available = false, memory_model = "cpu_only"
- [x] functions_registered matches actual discovered count

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

### A8 — Integration Test Harness
**Status:** Complete
**Owns:** `pg_accel/tests/integration.rs`

**Tasks:**
- [x] Build framework for end-to-end tests that:
  1. Create test table with known data
  2. Run query with `pg_accel.enabled = on`
  3. Run same query with `pg_accel.enabled = off`
  4. Assert results are bit-identical
  5. Assert `pg_accel_stats()` shows expected dispatch counts
- [x] Implement helper macro `assert_accel_matches!(query, setup_sql)` — runs both ways, compares
- [x] Implement helper macro `assert_accel_faster!(query, setup_sql, min_speedup)` — also checks timing

**Agent gate:**
- [x] Framework compiles and runs
- [x] Trivial test: `SELECT abs(x) FROM generate_series(1,1000) x` -> ON == OFF
- [x] Stats show 1000 rows dispatched after test

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing). pgrx_embed binary added, pg_accel.control file added, .cargo/config.toml added for macOS linking.

### A9 — Benchmark Framework Scaffold
**Status:** Complete
**Owns:** `pg_accel_bench/` crate

**Tasks:**
- [x] Implement CLI with subcommands: `setup`, `run`, `report`
- [x] `setup --rows N` — creates test tables with seeded RNG data
- [x] `run --workload <name> --iterations N` — runs query, collects `EXPLAIN ANALYZE` timing
- [x] `report --format markdown|json` — outputs results
- [x] Define Workload trait:
  ```rust
  pub trait Workload {
      fn name(&self) -> &str;
      fn setup_sql(&self, rows: usize, seed: u64) -> Vec<String>;
      fn query_sql(&self) -> String;
  }
  ```
- [x] Stub 3 workloads: `simple_agg`, `spatial_join`, `large_sort`

**Agent gate:**
- [x] `cargo build -p pg_accel_bench` succeeds
- [x] `pg_accel_bench setup --rows 1000` creates tables
- [x] `pg_accel_bench run --workload simple_agg --iterations 3` produces timing output

**Implementation log:**
Files are in `src/engine/` not `src/core/` (edition 2024 std::core shadowing).

---

## Phase Gate

- [x] dispatch_batch_parallel produces identical results to sequential PG for all 8 types
- [x] Thread pool creates, masks signals, cleans up on exit
- [x] Thread budget correctly tracks across 4 concurrent backends
- [x] Fallback logic triggers correctly for all 5 conditions
- [x] _PG_init pipeline completes and logs summary
- [x] pg_accel_stats() and pg_accel_device_info() return correct data
- [x] pg_accel_bench framework operational with 3 stub workloads
- [x] cargo pgrx test pg17 — all tests pass including new integration tests
- [x] Docker integration: dispatch batch queries (ON == OFF) on real PG with real data
- [x] Docker integration: all Phase 0 tests still pass (no regressions)

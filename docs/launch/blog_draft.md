# pg_accel: Batch-Parallel Query Execution for PostgreSQL

PostgreSQL processes rows one at a time. Every `ExecCustomScan` call returns a single tuple. Every predicate evaluation -- no matter how expensive -- happens once per row, in sequence. For simple OLTP this is fine. For spatial joins across millions of geometries, it is the bottleneck.

pg_accel is a PostgreSQL extension that changes this. It intercepts queries using supported functions (PostGIS spatial predicates, H3 cell operations, raster algebra, built-in aggregates), accumulates rows into batches of 256--8192, and evaluates them in a tight loop or a single GPU kernel launch. When done, it drains results back one at a time to satisfy the executor protocol. PostgreSQL never knows the difference.

The extension is written in Rust (pgrx 0.17) with C++/SYCL GPU kernels compiled via AdaptiveCpp. It ships as a standard `.so` loaded via `shared_preload_libraries`.

## How It Works

Three things happen at query time:

- **Planner hook intercepts supported functions.** pg_accel installs a `set_rel_pathlist_hook` that walks the qual list, looks up function OIDs against an adapter registry, and -- if the cost model predicts a net speedup -- calls `add_path()` with a Custom Scan node. The standard executor path remains available. If our path is not cheapest, PostgreSQL ignores it.

- **Batch executor bridges row-at-a-time and batch-parallel.** The `ExecCustomScan` callback pulls tuples from the child node into a batch buffer. Once the buffer is full (or the child is exhausted), the batch is dispatched through the strategy router. Results are buffered and drained one per call. `CHECK_FOR_INTERRUPTS()` runs between batches so cancellation works normally.

- **GPU pipeline with correctness guarantees.** For spatial predicates, batches flow through a three-layer pipeline. Rows that survive the cheap filter hit the GPU. Rows the GPU cannot resolve with confidence fall back to PostGIS on the CPU. No silent precision errors.

## The Three-Layer Spatial Model

This is the part that took the most design work. GPU spatial math typically runs in fp32 for throughput, but fp32 produces wrong answers near geometric boundaries. The usual solutions are "just use fp64" (halving throughput on consumer GPUs) or "accept the errors" (unacceptable for a database).

pg_accel uses a three-result model instead. GPU kernels return `+1` (true), `-1` (false), or `0` (uncertain):

**Layer 1: Bounding box filter.** AABB overlap test, estimated around 1 ns per geometry pair. On typical spatial data we expect this rejects 60--80% of pairs (not yet measured). Integer/float comparisons only -- no geometry parsing needed.

**Layer 2: GPU geometric predicate.** Point-in-ring, segment intersection, sphere distance. Runs in fp32 on the GPU at an estimated 50 ns per pair. Target is resolving about 90% of the remaining pairs -- those that are clearly inside or clearly outside.

**Layer 3: CPU recheck.** An estimated 1--5% of pairs where fp32 produced ambiguous results go back to the original PostGIS function running in fp64 on the CPU. Full correctness, no compromise.

The key insight: you do not need GPU precision for every row. You need GPU speed for the easy rows and CPU precision for the hard ones. The three-result model lets you have both.

## PostgreSQL Parallel Workers vs. In-Process Threads

PostgreSQL already has parallel query, so why not just use that? The answer matters and deserves an honest comparison.

PG parallel workers are forked processes. They communicate via shared memory and DSM segments. Each worker has its own memory context, its own snapshot, its own connection to shared buffers. The coordination overhead is meaningful -- fork cost, tuple serialization through shared memory queues, gather node merging.

pg_accel uses rayon threads within a single backend process. The threads share the process address space, so passing a batch of geometry data to a GPU kernel is a pointer handoff, not a copy through shared memory. Thread creation is near-zero (rayon uses a work-stealing pool).

The tradeoff: rayon threads cannot call any PostgreSQL C function. PG's backend code is not thread-safe. All `pg_sys::*` calls, SPI, palloc, elog -- everything must happen on the main backend thread. pg_accel maintains this discipline by design: rayon threads only do GPU kernel orchestration, sort-key extraction, and top-k merge. All PG interaction stays on the main thread.

A cluster-wide thread budget (stored in shared memory, protected by an LWLock) prevents oversubscription when many backends run pg_accel concurrently. If the budget is exhausted, the backend falls back to sequential execution gracefully.

## Where It Helps

- **Spatial joins with expensive predicates.** `ST_Contains`, `ST_Intersects`, `ST_DWithin` on large geometry tables. The bbox filter alone eliminates most of the work; the GPU handles the rest.
- **Complex predicate chains.** Predicates are sorted by selectivity/cost ratio. A cheap predicate that eliminates 80% of rows runs before expensive geometry deserialization. Late materialization means rejected rows never touch the GPU.
- **Sort and aggregate on large result sets (planned).** GPU radix sort and parallel reduction for `ORDER BY`, `SUM`, `AVG`, `MIN`, `MAX`, `COUNT` on numeric columns are on the roadmap but not yet shipped.
- **H3 hexagonal operations.** Cell-to-parent, grid distance, lat/lng-to-cell in batches.

## Where It Does Not Help

- **Simple OLTP.** Point lookups, index scans returning a few rows, short transactions. The cost model will not inject a Custom Scan node for these -- the minimum batch size threshold (default 256 rows) prevents it.
- **Small tables.** If the table fits in a few pages, the per-batch overhead exceeds the per-row savings. PostgreSQL's standard executor is already fast for small data.
- **Already-indexed spatial lookups.** If a GiST index on geometry reduces your candidate set to a handful of rows, there is nothing left to batch. pg_accel targets the cases where the index still leaves thousands of candidates for predicate evaluation.

## Multi-Platform GPU Support

pg_accel compiles GPU kernels via AdaptiveCpp (formerly hipSYCL), which targets multiple backends from a single SYCL source:

- **Apple Metal** -- first-class support, tested on M1/M2/M3
- **CUDA** -- NVIDIA GPUs
- **ROCm** -- AMD GPUs
- **Level Zero** -- Intel GPUs
- **CPU-only mode** -- no GPU required; batched evaluation with rayon still provides speedup from amortized per-row overhead

The same kernel source compiles for all targets. Platform-specific tuning (workgroup sizes, memory access patterns) is handled through platform profiles in the cost model.

## Current Status

v0.1.0 is an infrastructure release. The planner hook, batch executor, Custom Scan vtables, thread budget, cost model, adapter registry, and GPU kernel FFI are all implemented and tested. The three-layer spatial pipeline is wired end-to-end. GPU kernels for spatial predicates, H3 operations, sort, and reduce compile and pass standalone tests.

What remains is integration testing under real PostGIS workloads and performance tuning of batch sizes and cost model parameters against production-scale data. We have not published benchmark numbers because we have not run benchmarks we trust yet. When we do, they will be reproducible and methodology will be documented.

## Try It

```bash
# Requires Rust nightly + pgrx 0.17
cargo install cargo-pgrx
cargo pgrx init
cargo pgrx install
```

The source is at [github.com/yocontra/pg_accel](https://github.com/yocontra/pg_accel). PostgreSQL License (same as PostgreSQL itself).

If you work with PostGIS, H3, or raster data at scale, try it. If you maintain a PostgreSQL extension with expensive per-row functions, consider writing an adapter -- it is a pure data declaration, about 20 lines of Rust.

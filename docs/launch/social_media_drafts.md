# Social Media Post Drafts

> **NOTE:** These are drafts for human review. A human must review and actually post.

---

## r/PostgreSQL

**Title:** pg_accel: Batch-Parallel Query Execution for PostgreSQL via Custom Scan Provider

PostgreSQL evaluates row-returning functions one tuple at a time. For spatial joins across millions of geometries, this per-row executor overhead dominates. pg_accel is an extension that intercepts queries using supported functions (PostGIS spatial predicates, H3 cell ops, raster algebra) and evaluates them in batches via Custom Scan Provider nodes.

Key design points:

- Uses `set_rel_pathlist_hook` to inject Custom Scan paths. The standard optimizer still picks cheapest -- if batching isn't worth it, PG ignores our path.
- Rayon threads handle GPU dispatch only. All PG C functions stay on the main backend thread. Shared-memory LWLock prevents thread oversubscription.
- Three-result GPU model for spatial predicates: true/false/uncertain. Uncertain rows get rechecked on CPU via original PostGIS function. No silent precision errors.
- Works without a GPU -- CPU-only batched evaluation via rayon still amortizes per-row overhead.

Written in Rust (pgrx 0.17) + C++/SYCL (AdaptiveCpp). Supports Metal, CUDA, ROCm, Level Zero. PostgreSQL License.

GitHub: https://github.com/yocontra/pg_accel

---

## r/rust

**Title:** pg_accel: Using pgrx + rayon + SYCL FFI to batch-parallelize PostgreSQL query execution

Built a PostgreSQL extension in Rust using pgrx 0.17 that replaces row-at-a-time executor nodes with batch-parallel Custom Scan nodes. Thought the Rust community might find the architecture interesting:

- **pgrx for PG FFI:** Custom Scan Provider requires implementing three separate vtables (CustomPathMethods, CustomScanMethods, CustomExecMethods). pgrx handles the PG boilerplate but the vtable wiring is manual unsafe FFI with SAFETY comments on every block.
- **rayon for thread parallelism inside a PG backend:** PG's backend code is not thread-safe, so rayon threads are restricted by convention to GPU kernel orchestration, sort-key extraction, and top-k merge. All `pg_sys::*` calls stay on the main thread. A shared-memory LWLock manages a cluster-wide thread budget.
- **C++ SYCL kernels via narrow C FFI:** GPU spatial kernels are C++/SYCL (AdaptiveCpp), compiled to Metal/CUDA/ROCm/Level Zero. The Rust-C++ boundary is a single `pgaccel_ffi.h` header. Kernel code has zero PG dependencies and tests as a standalone library.
- **No `unwrap()` outside tests.** clippy::deny(unwrap_used) enforced in CI.

The adapter pattern is interesting from a Rust design perspective -- adding support for a new SQL function is a pure data declaration (~20 lines), no GPU code needed for the BatchedEval strategy.

GitHub: https://github.com/yocontra/pg_accel | PostgreSQL License

---

## r/gis

**Title:** pg_accel: GPU-accelerated PostGIS spatial predicates without leaving PostgreSQL

If you run PostGIS spatial joins on large datasets, you know the pain: ST_Contains and ST_Intersects are evaluated one row at a time, and the executor overhead adds up fast.

pg_accel is a PostgreSQL extension that batches spatial predicate evaluation and optionally dispatches to GPU kernels. It uses a three-layer pipeline:

1. **Bounding box filter** -- estimated to reject 60-80% of geometry pairs with cheap AABB tests
2. **GPU spatial kernel** -- point-in-ring, segment intersection, sphere distance in fp32 (estimated ~50ns/pair)
3. **CPU recheck** -- the estimated 1-5% of pairs where fp32 is ambiguous go back to PostGIS in fp64

No changes to your queries. No changes to PostGIS. It installs as a standard extension and the query planner picks it up automatically when the cost model says it helps.

Works on Apple Silicon (Metal), NVIDIA (CUDA), AMD (ROCm), Intel (Level Zero), and CPU-only (no GPU required). Also accelerates H3 cell operations.

GitHub: https://github.com/yocontra/pg_accel | PostgreSQL License

---

## Twitter/X

pg_accel: batch-parallel query execution for PostgreSQL.

Intercepts PostGIS spatial predicates via Custom Scan Provider, evaluates in batches on GPU (Metal/CUDA/ROCm) or CPU. Three-result model ensures correctness -- uncertain fp32 results recheck on CPU in fp64.

Rust + C++/SYCL. PostgreSQL License.

https://github.com/yocontra/pg_accel

---

## LinkedIn

**Announcing pg_accel: Batch-Parallel Query Execution for PostgreSQL**

PostgreSQL's executor processes one tuple at a time. For workloads with expensive per-row functions -- spatial predicates, H3 cell operations, raster algebra -- this sequential evaluation is the bottleneck.

pg_accel is a PostgreSQL extension that uses the Custom Scan Provider interface to inject batch-parallel executor nodes into query plans. Supported function calls are accumulated into batches of 256-8192 rows and evaluated in a tight loop or dispatched to GPU kernels.

Technical highlights:

- Planner hook with cost-model gating -- the optimizer still picks the cheapest path
- Three-result GPU spatial model (true/false/uncertain) with CPU recheck for precision
- Rayon-based threading with careful separation of PG thread-safety constraints
- Multi-platform GPU support via AdaptiveCpp/SYCL: Metal, CUDA, ROCm, Level Zero
- CPU-only mode when no GPU is available

Built with Rust (pgrx 0.17) and C++/SYCL. PostgreSQL License.

https://github.com/yocontra/pg_accel

# HN Submission Draft

## Title Options

1. **pg_accel: Batch-Parallel Query Execution for PostgreSQL** (factual, no hype)
2. **pg_accel: GPU-Accelerated Spatial Predicates for PostgreSQL via Custom Scan Provider** (technical audience)
3. **Show HN: pg_accel -- batching PostgreSQL's row-at-a-time executor for GPU dispatch** (Show HN format)

URL: https://github.com/yocontra/pg_accel

---

## First Comment (post within 60 seconds of submission)

Author here. pg_accel is a PostgreSQL extension that intercepts queries using supported functions (PostGIS spatial predicates, H3 cell operations, raster algebra) and evaluates them in batches rather than one row at a time. When a GPU is available, batches go to SYCL kernels via AdaptiveCpp. When not, rayon handles CPU-side batched evaluation.

Architecture in brief:

- **Custom Scan Provider.** Uses `set_rel_pathlist_hook` to inject alternative paths into the planner. The optimizer picks our path only when the cost model says it is cheaper. Standard executor is always the fallback.
- **Three-result GPU model.** GPU spatial kernels return true/false/uncertain. Rows marked uncertain (fp32 edge cases near geometric boundaries) get rechecked on the CPU via the original PostGIS function. No silent precision errors.
- **Thread safety by convention and code review.** Rayon threads never call PG C functions. All PG interaction stays on the main backend thread. A shared-memory LWLock prevents thread oversubscription across backends.

Comparison with PG-Strom: different approach and scope. PG-Strom is a more mature project with broader scope -- it uses the Custom Scan Provider interface with deep CUDA integration for general-purpose analytics acceleration (joins, aggregates, projections, and more). pg_accel is narrower, focused specifically on spatial predicates with a cross-platform GPU backend (Metal, CUDA, ROCm, Level Zero via AdaptiveCpp). The three-result model is designed around the specific challenge of fp32 precision in geometric operations. Both are great projects tackling different problems.

Honest limitations for v0.1.0:

- Infrastructure is complete and tested, but end-to-end integration testing with real PostGIS workloads is still in progress
- No published benchmarks yet -- we want reproducible numbers before claiming speedups
- GPU kernel coverage is spatial predicates, H3, sort, reduce; not general SQL
- Requires `shared_preload_libraries` (cannot be loaded per-session)
- AdaptiveCpp build toolchain is nontrivial to set up for GPU targets

Written in Rust (pgrx 0.17) + C++/SYCL. PostgreSQL License.

---

## Prepared Answers for Likely Questions

### Q1: "How does this compare to PG-Strom?"

PG-Strom and pg_accel solve different problems with different architectures. PG-Strom is a more mature project that also uses the Custom Scan Provider interface, with deep CUDA integration for general-purpose analytics acceleration -- joins, aggregates, projections, and much more. It's broader in scope and has been around longer.

pg_accel is narrower. The focus is specifically on spatial predicates and geometric operations where the three-result model (true/false/uncertain) matters for correctness. pg_accel also targets multiple GPU backends (Metal, CUDA, ROCm, Level Zero) via AdaptiveCpp, whereas PG-Strom is CUDA-focused.

If you need general analytics acceleration on NVIDIA hardware, PG-Strom is a great choice. If you want spatial predicate acceleration across GPU vendors with precision guarantees, that's the niche pg_accel is exploring.

### Q2: "Why not just use PostGIS parallel query support?"

PostGIS marks many functions as PARALLEL SAFE, so PostgreSQL's parallel workers can evaluate spatial predicates in forked processes. This works and helps. The limitation is per-worker overhead: each forked process has its own memory context, communicates through shared memory queues, and the gather node must merge results.

pg_accel's approach is complementary. Within a single backend process, it batches rows and dispatches to GPU kernels via pointer handoff (no serialization). The bbox pre-filter eliminates 60-80% of geometry pairs before any expensive evaluation happens. These two approaches can coexist -- pg_accel can run inside a parallel worker process.

### Q3: "What are the actual speedup numbers?"

We do not have published benchmarks yet. This is deliberate. The infrastructure is complete and the GPU pipeline is wired end-to-end, but we have not yet run the kind of controlled, reproducible benchmarks we would be comfortable publishing. Claiming "10x faster" without methodology is worse than claiming nothing.

The benchmark harness exists and workload definitions are written. Once integration testing with real PostGIS data is complete, we will publish numbers with full methodology, hardware specs, dataset descriptions, and reproduction scripts.

### Q4: "Does this require a GPU? What about cloud VMs without GPUs?"

No GPU required. When no GPU is detected (or `pg_accel.gpu_enabled = off`), the extension falls back to CPU-side batched evaluation using rayon. You still get the benefit of amortized per-row overhead and the bbox pre-filter. The cost model adjusts estimates for CPU-only operation.

This means pg_accel is useful on any machine where spatial queries are a bottleneck, not just GPU-equipped servers.

### Q5: "Why Rust + C++/SYCL instead of pure Rust or pure C?"

The PG extension glue -- FFI, memory management, error handling, signal safety -- benefits enormously from Rust via pgrx. Unsafe blocks are annotated and contained. No unwrap() outside tests. The codebase is structured so that PG functions are not called from rayon threads.

GPU kernels are C++ because SYCL compilers (AdaptiveCpp) require C++ source. There is no production-ready Rust SYCL implementation. The boundary between Rust and C++ is a narrow C FFI defined in a single header file (`pgaccel_ffi.h`). The kernel code is pure computation with no PG dependencies, so it compiles and tests as a standalone C++ library.

### Q6: "How do I add support for my own extension's functions?"

Write an adapter. It is a pure data declaration -- no GPU code needed for `BatchedEval` strategy:

```rust
ExtensionAdapter {
    name: "my_extension",
    version_query: "SELECT my_extension_version()",
    functions: vec![
        FunctionAccelEntry {
            schema: "public",
            name: "my_expensive_function",
            strategy: AccelStrategy::BatchedEval,
        },
    ],
}
```

This tells pg_accel to batch-evaluate `my_expensive_function` calls instead of running them row-by-row. The adapter system resolves function names to OIDs at load time via `pg_proc` lookup. GPU strategies require corresponding kernel implementations, but `BatchedEval` works for any scalar function on the main thread.

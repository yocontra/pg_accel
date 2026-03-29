# Conference Abstracts

## FOSS4G: "GPU-Accelerated PostGIS Without Leaving PostgreSQL"

PostGIS spatial predicates like ST_Contains and ST_Intersects are evaluated one row at a time inside PostgreSQL's executor. For large spatial joins, this per-row overhead dominates query time. pg_accel is a PostgreSQL extension that intercepts these queries via the Custom Scan Provider interface and evaluates spatial predicates in batches, optionally offloading to GPU kernels compiled through AdaptiveCpp/SYCL.

The core design challenge is correctness. GPU spatial math runs in fp32 for throughput, but fp32 produces wrong answers near geometric boundaries. pg_accel solves this with a three-result model: GPU kernels return true, false, or uncertain. The estimated 1-5% of geometry pairs that land in the uncertain category are rechecked on the CPU using the original PostGIS function in fp64. A bounding-box pre-filter is estimated to eliminate 60-80% of pairs before they reach the GPU at all.

The extension installs as a standard shared library, requires no changes to PostGIS, and falls back to CPU-only batched evaluation when no GPU is present. This talk covers the three-layer spatial pipeline, the adapter pattern for function interception, and the thread-safety constraints imposed by PostgreSQL's architecture. Written in Rust (pgrx) and C++/SYCL. PostgreSQL License.

---

## PGConf: "Batch-Parallel Query Execution via Custom Scan Provider"

PostgreSQL's executor returns one tuple at a time. This is elegant and composable, but it means expensive per-row functions -- spatial predicates, H3 cell operations, raster algebra -- pay full executor overhead on every row. pg_accel is an extension that uses the Custom Scan Provider API to inject batch-parallel executor nodes into query plans.

A planner hook walks qual lists, identifies supported functions via an adapter registry, and adds alternative Custom Scan paths when a cost model predicts net speedup. The standard executor path is always available; the optimizer chooses based on cost. At execution time, a batch executor accumulates tuples from child nodes, dispatches them through a strategy router (CPU batched evaluation or GPU kernel), and drains results back one at a time to satisfy the executor protocol.

Thread safety is maintained by convention and code review: rayon worker threads handle only GPU orchestration and never call PG C functions. A cluster-wide thread budget in shared memory, protected by an LWLock, prevents oversubscription across concurrent backends. The extension supports PostgreSQL 15-18 via pgrx 0.17, with GPU kernels targeting Metal, CUDA, ROCm, and Level Zero through AdaptiveCpp/SYCL. This talk covers the Custom Scan vtable implementation, the cost model, and lessons learned about threading inside a PostgreSQL backend. PostgreSQL License.

---

## IWOCL/SYCLcon: "Spatial Predicate Acceleration on Apple Metal via AdaptiveCpp"

Spatial database predicates -- point-in-polygon, segment intersection, great-circle distance -- are embarrassingly parallel but traditionally run sequentially inside database executors. pg_accel is a PostgreSQL extension that batches spatial predicate evaluation and dispatches to GPU kernels written in SYCL, compiled via AdaptiveCpp to target Apple Metal, CUDA, ROCm, and Level Zero from a single source.

The primary challenge is precision. Spatial predicates demand exact answers, but consumer GPUs (especially Apple Silicon) perform best in fp32. pg_accel uses a three-result kernel model: each geometry pair receives a verdict of true, false, or uncertain. Uncertain results -- those within an epsilon of geometric boundaries where fp32 rounding could flip the answer -- are rechecked on the CPU in fp64. On typical spatial datasets, an estimated 95-99% of pairs resolve definitively on the GPU (not yet measured on production data).

This talk focuses on the Metal backend experience: AdaptiveCpp compilation targeting Metal via SPIR-V, fp32 precision boundaries for spatial primitives (point-in-ring winding number, segment-segment intersection, Haversine distance), workgroup size tuning on Apple Silicon unified memory, and the C FFI boundary between the Rust database extension and C++/SYCL kernels. We discuss where Metal's unified memory architecture provides advantages over discrete GPU memory models and where it introduces constraints.

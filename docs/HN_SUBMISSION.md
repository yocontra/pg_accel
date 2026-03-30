# Hacker News Submission Draft

## Title

pg_accel — GPU-accelerated spatial queries for PostgreSQL via Custom Scan Provider

(63 chars — under HN's 80 char limit)

## URL

https://github.com/yocontra/pg_accel

## Alternate titles (pick based on what resonates)

- Show HN: pg_accel — batched GPU execution for PostGIS and H3 in PostgreSQL
- Show HN: pg_accel — PostgreSQL extension that offloads spatial predicates to Metal/CUDA
- pg_accel: three-result GPU model for spatial queries (true/false/uncertain + CPU recheck)

## Top-level comment (post immediately after submission)

pg_accel is a PostgreSQL extension I've been building that accelerates spatial
predicates (ST_Contains, ST_DWithin, etc.), H3 cell operations, and aggregates
by batching rows and dispatching them to the GPU.

The core idea: PostgreSQL evaluates predicates one row at a time. For expensive
functions like ST_Contains over a million geometries, the per-row overhead
dominates. pg_accel accumulates rows into batches (256–8192) and evaluates them
in a single GPU kernel launch via AdaptiveCpp/SYCL (Metal on macOS, CUDA,
ROCm).

The part I'm most interested in feedback on is the **three-result GPU model**.
GPU kernels return True, False, or Uncertain for each row. The Uncertain rows
(typically <1%) get rechecked on the CPU with the original PostGIS function at
full fp64 precision. This gives GPU throughput without sacrificing correctness
at geometric boundary conditions.

It installs as a standard Custom Scan Provider — same interface PG-Strom uses.
No fork, no custom planner. Queries that don't benefit go through the stock
executor untouched. The cost model gates on row count and per-row cost, so
OLTP workloads aren't affected.

Tech stack: Rust (pgrx 0.17) for the PostgreSQL integration, C++/SYCL
(AdaptiveCpp) for GPU kernels, targeting PG 15–18.

<!-- TODO: Add benchmark numbers when Phase 10 completes -->

Happy to answer questions about the Custom Scan interface, the three-result
model, or the AdaptiveCpp/Metal experience.

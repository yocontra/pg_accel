# Social Media Post Templates

## Twitter/X

### Launch announcement

pg_accel: GPU-accelerated spatial queries for PostgreSQL.

Batches ST_Contains, ST_DWithin, H3 ops into GPU kernel launches via Metal/CUDA. Three-result model (true/false/uncertain) rechecks edge cases on CPU — GPU speed, PostGIS correctness.

Custom Scan Provider, no fork. Rust + SYCL.

github.com/yocontra/pg_accel

### Technical hook

PostgreSQL evaluates ST_Contains one row at a time. pg_accel batches 8192 rows and dispatches them in a single GPU kernel launch.

The trick: GPU returns true/false/uncertain. The <1% uncertain rows get rechecked on CPU at fp64. You get GPU throughput without fp32 precision bugs.

github.com/yocontra/pg_accel

### Performance hook (update with real numbers)

<!-- TODO: Update with real benchmark numbers from Phase 10 -->

ST_Contains over 1M geometries:
- Stock PostgreSQL: TODO ms
- pg_accel (CPU batching): TODO ms
- pg_accel (Metal GPU): TODO ms

No application changes. Just `CREATE EXTENSION pg_accel;`

github.com/yocontra/pg_accel

---

## LinkedIn

### Launch post

**Releasing pg_accel — GPU-accelerated query processing for PostgreSQL**

I've been working on a PostgreSQL extension that intercepts spatial predicates (PostGIS), H3 hexagonal indexing, and aggregates, then re-executes them in batches on the GPU.

The approach: instead of evaluating ST_Contains row by row, pg_accel accumulates tuples into batches and dispatches them as a single GPU kernel. It uses a three-result model — True, False, or Uncertain — where uncertain rows (typically <1%) are rechecked on the CPU with the original PostGIS function. GPU throughput, CPU correctness.

Key design choices:
- Custom Scan Provider interface (same as PG-Strom) — no fork needed
- Cross-platform GPU via AdaptiveCpp/SYCL (Metal, CUDA, ROCm)
- Cost model gates prevent acceleration on small queries
- Cluster-wide thread budget via shared memory LWLock

Built with Rust (pgrx 0.17) and C++/SYCL. Supports PostgreSQL 15–18.

github.com/yocontra/pg_accel

---

## Mastodon / Bluesky

pg_accel: a PostgreSQL extension that batches spatial predicates and runs them on the GPU.

ST_Contains over 1M rows? Instead of row-by-row, batch 8192 and launch one Metal/CUDA kernel. Uncertain results (<1%) get CPU recheck at fp64.

Custom Scan Provider — no fork, works with standard PostgreSQL. Rust + SYCL.

github.com/yocontra/pg_accel

---

## Reddit (r/PostgreSQL, r/gis, r/rust)

### Title
pg_accel: GPU-accelerated spatial queries for PostgreSQL via Custom Scan Provider (Rust + SYCL)

### Body
I've been building a PostgreSQL extension that accelerates PostGIS spatial predicates, H3 operations, and aggregates by batching rows and dispatching them to the GPU.

**How it works:**
- Hooks into the planner via Custom Scan Provider (same interface as PG-Strom)
- Accumulates rows into batches (256–8192 rows)
- Dispatches to GPU kernels via AdaptiveCpp/SYCL (Metal on macOS, CUDA, ROCm)
- Three-result model: True / False / Uncertain — uncertain rows rechecked on CPU

**What's accelerated:**
- PostGIS: ST_Contains, ST_Intersects, ST_DWithin, ST_Distance
- h3-pg: h3_cell_to_parent, h3_grid_disk, h3_cell_to_boundary
- Builtins: aggregates (SUM, AVG, MIN, MAX), sort

**Key design decisions:**
- No fork — standard extension, enable/disable per session
- Cost model prevents acceleration on small queries (OLTP unaffected)
- Cluster-wide thread budget in shared memory
- Works without GPU too (CPU batching via rayon)

Tech stack: Rust (pgrx 0.17) for PG integration, C++/SYCL for kernels.

<!-- TODO: Add benchmark numbers -->

Feedback welcome, especially on the three-result GPU model and the Custom Scan integration.

github.com/yocontra/pg_accel

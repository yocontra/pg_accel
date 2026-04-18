# pg_accel Ideas

Compute-side improvements, roughly ordered by expected impact. pg_accel
doesn't touch storage.

## High Impact

### Arena allocator for tuple materialization

Currently every `ExecCopySlotMinimalTuple` call does a separate `palloc`.
For 10M rows that's 10M allocations + frees. Replace with a single large
arena (Vec<u8> or mmap region) where tuples are packed contiguously. Copy
from PG's palloc'd result into the arena, pfree immediately. Benefits:
eliminates palloc overhead (~50-100ns/call), improves cache locality during
reorder phase, reduces fragmentation. Estimated saving: 500ms-1s on 10M
rows.

### Late materialization for sort

Instead of copying full tuples during consumption, extract only
(sort_key, ctid) pairs (8 bytes/row vs 120 bytes/row). GPU sort the
key-ctid pairs, then fetch full tuples by ctid in sorted order. All heap
pages are hot in buffer cache from the initial scan so fetches should be
fast. Eliminates the dominant materialization phase for 10M wide rows.
Risk: random heap access after sort may thrash cache for very large tables
that don't fit in shared_buffers.

### Parallel-safe scan-level CustomScan

The single biggest perf ceiling at 10M+ rows is single-thread
`heap_getnext` in the consumption loop (measured: 178ms of 189ms at 10M
reduce). PG parallel scans with 8 workers in ~87ms. Inject a CustomPath
at the base rel via `set_rel_pathlist_hook` with `parallel_safe=true` and
`scanrelid=rti`, emitting raw columns. PG's Partial Agg + Gather + Finalize
Agg run unchanged on top. Each worker drives its own GPU-accelerated scan
on its slice. Lower complexity than the agg-level parallel_safe path
because the tlist has no Aggrefs (avoids `fix_scan_expr` T_Aggref crash).

### Fix & re-enable bytecode expression evaluator

Already compiled and wired — disabled because the interpreter produces
incorrect results on some inputs (see `engine/executor/scan.rs` comments).
Fixing it unlocks GPU WHERE evaluation for arbitrary expressions beyond
the five template-matched shapes. Currently complex predicates defer to PG.

### Multi-key sort via composite radix keys

Current GPU sort is single-key. For `ORDER BY a, b, c`, encode multiple
keys into a single sortable composite value (radix key), or do cascading
stable sorts (last key first). Unlocks GPU sort for many more real-world
queries. `gpu_sort_multikey` bench exists — check its current state
before building.

## Medium Impact

### Predicate pushdown into sort consumption

When the query has both WHERE and ORDER BY, PG filters rows via
SeqScan + Filter before pg_accel sorts. Push the predicate into our
consumption loop to skip materializing rows that don't pass. Saves
tuple-copy overhead for filtered-out rows.

### Parallel tuple consumption with io_uring / readahead

The SeqScan child feeds tuples one at a time. On Linux, hint the OS to
prefetch upcoming heap pages while we're processing current tuples. Won't
help on macOS (no io_uring) but could reduce I/O stalls on Linux
deployments with cold caches.

### GPU-accelerated JOIN residuals

For spatial joins (t1 JOIN t2 ON ST_Contains(t1.geom, t2.geom)), the join
itself is PG's (nested loop or hash join), but the residual predicate is
evaluated row-at-a-time. Batch the residual geometry pairs and evaluate on
GPU. Already scaffolded in `join.rs`, needs wiring.

### Adaptive batch sizing

Current batch_size is fixed at 1000. For GPU kernels, larger batches
amortize launch overhead better. Dynamically size batches based on:
available GPU memory, tuple width, and observed throughput. Start small,
grow if GPU is keeping up.

### Top-K GPU partial sort

For `ORDER BY ... LIMIT k` where k is moderate (not tiny enough for PG's
heapsort but not full table), implement a GPU partial sort: selection to
find the k-th element in O(n), partition, then sort only the top-k
partition. Avoids sorting the entire dataset.

### EXPLAIN ANALYZE instrumentation

Add more detail to Custom Scan EXPLAIN output: GPU kernel time vs CPU
overhead, memory allocated, keys extracted, cache hit rate. Currently
shows total dispatch time only. Fine-grained timing helps users
understand whether GPU is actually helping.

## Lower Impact / Exploratory

### Compressed sort keys for strings

For string columns, compute a fixed-width prefix key (e.g., first 8 bytes)
that resolves most comparisons without touching the full string. GPU sorts
prefix keys; ties resolved by CPU using full string comparison. Extends
GPU sort to text/varchar columns.

### GPU-accelerated index builds

CREATE INDEX on large tables is CPU-bound (sort + B-tree construction).
Offload the sort phase to GPU. Needs an AM build hook.

### Memory-mapped tuple arena

Use `mmap(MAP_ANONYMOUS)` for the arena instead of Vec<u8>. Lets the OS
manage paging for very large sorts exceeding physical RAM. Tuples not
actively being reordered can be paged out transparently.

### Streaming external GPU sort

For datasets exceeding GPU memory, sort chunks that fit, write sorted runs
to an arena, then merge. Like PG's external merge sort but with GPU-sorted
runs. Matters mostly for >100M rows where we currently defer to PG parallel
sort anyway.

### Vectorized aggregate accumulation (CPU path)

For CPU paths that still exist at scale boundaries, use SIMD (NEON / AVX)
to accumulate SUM/MIN/MAX over f32/f64 arrays. Rust auto-vectorization
sometimes catches this; explicit SIMD would guarantee it. Small win (~2x)
since aggregates are already memory-bound.

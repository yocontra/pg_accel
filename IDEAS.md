# pg_accel Ideas

Potential improvements, roughly ordered by expected impact. Everything here is
compute-side — pg_accel doesn't touch storage.

## High Impact

### Arena allocator for tuple materialization

Currently every `ExecCopySlotMinimalTuple` call does a separate `palloc`. For
10M rows that's 10M allocations + frees. Replace with a single large arena
(Vec<u8> or mmap region) where tuples are packed contiguously. Copy from PG's
palloc'd result into the arena, pfree immediately. Benefits: eliminates palloc
overhead (~50-100ns/call), improves cache locality during reorder phase, reduces
memory fragmentation. Estimated saving: 500ms-1s on 10M rows.

### Late materialization for sort

Instead of copying full tuples during consumption, extract only (sort_key, ctid)
pairs (8 bytes/row vs 120 bytes/row). GPU sort the key-ctid pairs. Then fetch
full tuples by ctid in sorted order. All heap pages are hot in buffer cache from
the initial scan so fetches should be fast. Eliminates the dominant 2.6s
materialization phase for 10M wide rows. Risk: random heap access pattern after
sort may thrash cache for very large tables that don't fit in shared_buffers.

### GPU-accelerated WHERE clause evaluation

PG evaluates WHERE predicates row-at-a-time via ExecQual. For simple numeric
predicates (col > 5, col BETWEEN x AND y, col IS NOT NULL), extract the column
into a contiguous array, evaluate the predicate on GPU in bulk, produce a
bitmask, then skip non-matching tuples during emit. This is essentially what
PG-Strom does for scan-level acceleration. Pairs well with GPU sort — filter
first, then sort only qualifying rows.

### GPU hash aggregation

Current aggregate path does serial accumulation + GPU reduce for the final
reduction. For GROUP BY queries, build a hash table on GPU: extract group keys
+ aggregate columns into arrays, GPU-parallel hash + accumulate. Return
(group_key, aggregate_result) pairs. Would handle the common
`SELECT col, count(*), sum(val) FROM t GROUP BY col` pattern that PG currently
does with HashAgg.

### Multi-key sort

Current GPU sort only handles single-key ORDER BY. For multi-key sort
(ORDER BY a, b, c): use lexicographic comparison by encoding multiple keys into
a single sortable value (e.g., composite radix key), or do cascading sorts
(stable sort by last key first, then by previous keys). Would unlock GPU sort
for many more real-world queries.

### Radix sort kernel for large datasets

Bitonic sort is O(n log^2 n). Radix sort is O(n * k) where k is key width in
bits. For datasets >10M rows on discrete GPUs with high memory bandwidth, radix
sort would be significantly faster. Keep bitonic for unified memory / smaller
datasets where its simplicity and low memory overhead wins.

## Medium Impact

### Predicate pushdown into sort consumption

When the query has both WHERE and ORDER BY, currently PG filters rows via
SeqScan + Filter, then pg_accel sorts the results. If we push the predicate
into our consumption loop, we can skip materializing rows that don't pass the
filter. Saves tuple copy overhead for filtered-out rows.

### Parallel tuple consumption with io_uring / readahead

The SeqScan child feeds tuples one at a time. On Linux, we could hint the OS to
prefetch upcoming heap pages while we're processing current tuples. Won't help
on macOS (no io_uring) but could reduce I/O stalls on Linux deployments with
cold caches.

### GPU-accelerated JOIN residuals

For spatial joins (t1 JOIN t2 ON ST_Contains(t1.geom, t2.geom)), the join
itself is done by PG (nested loop or hash join), but the residual predicate
(ST_Contains) is evaluated row-at-a-time. Batch the residual geometry pairs and
evaluate on GPU. Already scaffolded in join.rs, needs wiring.

### Adaptive batch sizing

Current batch_size is fixed at 1000. For GPU kernels, larger batches amortize
launch overhead better. Dynamically size batches based on: available GPU memory,
tuple width, and observed throughput. Start small, grow if GPU is keeping up.

### Top-K GPU sort (partial sort)

For ORDER BY ... LIMIT k where k is moderate (not tiny enough for PG's heapsort
but not full table), implement a GPU partial sort: use a GPU selection algorithm
to find the k-th element in O(n), partition, then sort only the top-k partition.
Avoids sorting the entire dataset.

### EXPLAIN ANALYZE instrumentation

Add more detail to Custom Scan EXPLAIN output: GPU kernel time vs CPU overhead,
memory allocated, keys extracted, cache hit rate. Currently only shows total
dispatch time. Fine-grained timing would help users understand where time goes
and whether GPU sort is actually helping.

## Lower Impact / Exploratory

### Expression JIT on GPU

For complex WHERE clauses or computed columns (e.g., `sqrt(a*a + b*b) < 10`),
compile the expression into a GPU kernel at plan time. Avoids per-row PG
expression evaluation overhead. Very complex to implement — essentially a
mini-compiler from PG Expr trees to SYCL.

### Compressed sort keys

For string columns, compute a fixed-width prefix key (e.g., first 8 bytes) that
allows most comparisons to resolve without touching the full string. GPU sorts
the prefix keys; ties are resolved by CPU using full string comparison. Would
extend GPU sort to text/varchar columns.

### GPU-accelerated index builds

CREATE INDEX on large tables is CPU-bound (sort + B-tree construction). Could
offload the sort phase to GPU, similar to table sort. Would need to hook into
the index AM build interface.

### Memory-mapped tuple arena

Instead of Vec<u8> for the arena, use mmap with MAP_ANONYMOUS. Lets the OS
manage paging for very large sorts that exceed physical RAM. Tuples that aren't
actively being reordered can be paged out transparently.

### Streaming sort (external GPU merge)

For datasets that exceed GPU memory, implement a streaming approach: sort chunks
that fit in GPU memory, write sorted runs to an arena, then merge. Like PG's
external merge sort but with GPU-sorted runs. Probably only matters for datasets
>100M rows where we currently defer to PG's parallel sort.

### CUDA-specific optimizations

AdaptiveCpp/SYCL targets portability. For NVIDIA GPUs specifically, could write
CUDA kernels using CUB/Thrust for sort (device-wide radix sort) and reduction.
CUB's radix sort is highly optimized and would outperform our bitonic sort on
large datasets. Ship as an optional feature alongside the SYCL path.

### Vectorized aggregate accumulation

For CPU fallback aggregates, use SIMD (NEON on ARM, AVX on x86) to accumulate
SUM/MIN/MAX over f32/f64 arrays. Rust's auto-vectorization sometimes catches
this but explicit SIMD would guarantee it. Small win (~2x for aggregates that
are already memory-bound).

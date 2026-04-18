# pg_accel Implementation Notes

Why things are the way they are.

## Custom Scan at scan level, not upper paths

GPU sort injection was originally in `create_upper_paths_hook`
(UPPERREL_ORDERED). This crashed with a PG assert in `apply_tlist_labeling`
because `custom_scan_tlist` length didn't match the child plan's targetlist.
Upper-path Custom Scans need a carefully constructed tlist that mirrors the
child — fragile and version-dependent.

Fix: inject at `set_rel_pathlist_hook` with `pathkeys = root->sort_pathkeys`.
PG sees the scan output as pre-sorted and never adds a Sort node. Avoids all
upper-path tlist complications. The sort happens inside our Custom Scan's
ExecCustomScan, not as a separate upper path.

## Static linking for the kernel library

The GPU kernel library was originally a shared library. This caused a runtime
crash ("failed to initiate panic, error 5") in release builds because:
- `cargo pgrx install` copies the `.so` to PG's `$libdir` but doesn't copy
  dependent shared libraries
- The rpath pointed to cargo's build directory, which differs between debug
  and release profiles
- `otool -L` on the installed `.so` showed no dependency on the kernel lib

Fix: build as a static library (`add_library(pgaccel_kernels STATIC ...)`),
link with `cargo::rustc-link-lib=static=pgaccel_kernels`. Also need to link
`c++` (macOS) / `stdc++` (Linux) and the OpenMP runtime explicitly since they
were transitive deps of the shared lib. A separate shared library target
exists for the standalone kernel test executables.

## Inline sort key extraction

The original design had two passes:
1. Consume all tuples (ExecProcNode + ExecCopySlotMinimalTuple per row)
2. Extract sort keys from materialized tuples (ExecForceStoreMinimalTuple +
   slot_getattr per row)

Pass 2 was as expensive as pass 1 — it doubled the PG executor overhead. Fix:
extract the sort key from the live child slot (already populated by
ExecProcNode) BEFORE calling ExecCopySlotMinimalTuple. The slot is valid, the
datum is there, one `slot_getattr` call per row during consumption. The
`try_gpu_sort` two-pass method still exists as fallback for non-inline cases
(multi-key sort, non-GpuSort strategy).

## Width gate at 40 bytes

GPU sort was 11% slower than PG native on narrow rows (single float4 column).
Root cause: PG's external merge sort spills only ~58 MB for 5M narrow rows.
On SSD this fits in page cache — merge passes are fast. Our materialization
overhead (~150ns/row for ExecCopySlotMinimalTuple) isn't amortized by I/O
savings when the I/O is cheap.

The planner's cost model couldn't distinguish this because PG's `cost_sort`
overestimates disk I/O cost for narrow rows on SSD (it assumes spinning disk
throughput). Our GPU path looked cheaper in cost units but was slower in wall
time.

Fix: gate on output row width from the planner (`(*(*rel).reltarget).width`).
Below 40 bytes, skip GPU sort. Threshold determined empirically — below it PG
wins, above it GPU wins.

## Top-K deferral

GPU sort was 2x slower than PG for `ORDER BY ... LIMIT 100` on 5M rows. PG
uses top-N heapsort: maintains a 100-element heap while scanning, O(n log k)
with 48kB memory. Our path sorted all 5M rows then truncated.

Fix: check `root->limit_tuples` in the planner. If `limit < rows / 4`, skip
GPU sort injection. PG's heapsort is always better for small LIMIT relative
to table size.

## Parallel worker deferral at 20M rows

PG's Gather Merge with parallel workers was faster than GPU sort for 50M rows
(9.5s parallel vs 18.3s GPU). PG forks worker processes, each sorts a
partition, leader merges sorted streams. This scales with CPU cores. Our GPU
sort runs single-backend because PG extension code can't safely coordinate
across forked processes.

Fix: skip injection when `rel->consider_parallel && rows > 20_000_000 &&
max_parallel_workers_per_gather > 0`. The 20M threshold is where PG typically
chooses parallel sort.

## Aggregate cost model tuning

Early GPU aggregate was 2x slower than PG native for `SELECT avg/min/max
FROM wide_5m`. PG's native Agg accumulates inline during scan — no
materialization. Our path materializes every tuple, extracts values to
Vec<f64>, then calls GPU reduce. The materialization overhead swamps any GPU
reduction benefit for simple full-table aggregates.

Initial per-row cost was set too optimistic (0.001). Real cost measured at
~0.14µs/row including materialization. Cost model now charges a realistic
per-row plus materialization overhead so PG's native Agg wins for simple
aggregates. GPU reduce only helps when data is already buffered (after a GPU
spatial filter) or when the reduction is complex enough to amortize.

## Borrow checker and sort key cloning

`self.sort_keys[0]` creates an immutable borrow on `self`, but
`self.apply_gpu_sort_result(...)` needs `&mut self`. Rust rejects this.

Fix: `let key = self.sort_keys[0].clone()` before calling apply. SortKeyDesc
is small (attno, sort_op, collation, nulls_first) so cloning is cheap.

## AggColumn API: 2-arg vs 3-arg new()

Production code in custom_scan passed `(AggOp, attno)` pairs but
`AggColumn::new` expected `(AggOp, attno, result_type_oid)` triples. The
result type OID isn't always known at construction time (it comes from
`Aggref.aggtype`, only available during plan deserialization).

Fix: 2-arg `new(op, attno)` delegates to `with_result_type(op, attno,
InvalidOid)`. The result type is set later during plan deserialization.

## Dual cmake library targets

The kernel library needs to be static for embedding in Rust, but the
standalone test executables need a shared library because they can't
statically link the OpenMP runtime on macOS (duplicate symbol errors).

Fix: build both `pgaccel_kernels` (STATIC) and `pgaccel_kernels_shared`
(SHARED) from the same sources. Tests link against shared, Rust links
against static.

## NaN semantics in GPU sort

PostgreSQL sorts NaN as the LARGEST float value (greater than +infinity).
The C standard doesn't define NaN comparison; GPU hardware varies.

Fix: custom comparison `pg_float_less()` in sort.cpp explicitly handles NaN:
```cpp
if (std::isnan(a)) return false;  // NaN is never less than anything
if (std::isnan(b)) return true;   // anything is less than NaN
return a < b;
```

The key-value sort uses index as tiebreaker for equal keys to maintain
stability.

## DESC sort via reversal

GPU sort always produces ascending order with NaN-last semantics. For DESC
queries we detect the sort operator OID (float4gt=623, float8gt=674,
int4gt=521) and reverse the GPU output. Cheaper than a separate descending
kernel — one `rev()` iterator over the index array.

## Batch size 1000 with interrupt checks

The consumption loop processes tuples in batches of 1000 with
`check_for_interrupts!()` between batches. This ensures PG can respond to
`pg_cancel_backend()` and statement_timeout during long sorts. The batch
size is small enough for responsive cancellation but large enough that the
interrupt check overhead (one atomic load per batch) is negligible.

## Three-result model for spatial predicates

GPU fp32 can misclassify points near polygon edges. Instead of:
- Always using fp64 (not available on Metal, slow on consumer GPUs)
- Accepting wrong answers
- Adding epsilon bands (geometry-dependent, error-prone)

Kernels return three results: `definite_true`, `definite_false`, `uncertain`.
The bbox pre-filter (layer 1) and point-in-ring test (layer 2) run on GPU.
Uncertain pairs get a precise recheck (layer 3) using PG's exact-precision
spatial functions. This recheck is a **correctness path for fp32 edge-case
classification only** — it is NOT a CPU fallback for GPU unavailability
or GPU failure. Layers 1 and 2 always run on GPU; layer 3 resolves the
small set of geometrically-ambiguous rows that fp32 cannot classify
confidently. Typically <5% of rows are uncertain, so GPU throughput is
preserved while correctness is guaranteed. Implemented in
`src/gpu/three_layer.rs`.

## Bytecode expression evaluator — currently disabled

`expr_compiler.rs` converts PG expression trees to GPU bytecode; the
interpreter lives in `pgaccel-kernels/src/expr_eval.cpp`. The path is wired
through `GpuExpr` strategy but **disabled** in `custom_scan/mod.rs` and
`executor/scan.rs` — the interpreter produces incorrect results for some
inputs. Compilation still runs (for stats/logging), execution defers to
PG's scalar qual. Re-enabling requires fixing the interpreter's correctness
bugs; template-match path (`col op const`, `BETWEEN`, `IN`, `IS NULL`,
two-predicate AND) is unaffected.

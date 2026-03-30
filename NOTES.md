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
`c++` (macOS) / `stdc++` (Linux), `omp` (OpenMP runtime), and `acpp-rt`
(AdaptiveCpp runtime) explicitly since they were transitive deps of the shared
lib. A separate shared library target exists for the standalone test executables.

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

GPU sort was 11% SLOWER than PG native on narrow rows (single float4 column).
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
wins, above it GPU wins. Simple and correct.

## Top-K deferral

GPU sort was 2x slower than PG for `ORDER BY ... LIMIT 100` on 5M rows. PG
uses top-N heapsort: maintains a 100-element heap while scanning, O(n log k)
with 48kB memory. Our path sorted all 5M rows then truncated.

Fix: check `root->limit_tuples` in the planner. If `limit < rows / 4`, skip
GPU sort injection. PG's heapsort is always better for small LIMIT relative to
table size.

## Parallel worker deferral at 20M rows

PG's Gather Merge with parallel workers was faster than GPU sort for 50M rows
(9.5s parallel vs 18.3s GPU). PG forks worker processes, each sorts a
partition, leader merges sorted streams. This scales with CPU cores. Our GPU
sort runs single-backend because PG extension code can't safely coordinate
across forked processes.

Fix: skip injection when `rel->consider_parallel && rows > 20_000_000 &&
max_parallel_workers_per_gather > 0`. The 20M threshold is where PG typically
chooses parallel sort.

## Aggregate cost model at 0.05/row

GPU aggregate was 2x SLOWER than PG native for `SELECT avg/min/max FROM
wide_5m`. PG's native Agg accumulates inline during scan — no materialization.
Our path materializes every tuple, extracts values to Vec<f64>, then calls GPU
reduce. The materialization overhead swamps any GPU reduction benefit for simple
full-table aggregates.

The original `GPU_REDUCE_PER_ROW_COST` was 0.001 — wildly optimistic. PG's Agg
costs ~0.01/row. With materialization our real cost is ~0.14µs/row (measured).

Fix: set `GPU_REDUCE_PER_ROW_COST = 0.03` and add 0.02 materialization overhead
in the planner cost estimate. This makes PG's native Agg win for simple
aggregates. GPU reduce only helps when data is already buffered (e.g., after a
GPU spatial filter) or when the reduction is complex enough to amortize the
materialization cost.

## Borrow checker and sort key cloning

`self.sort_keys[0]` created an immutable borrow on `self`, but
`self.apply_gpu_sort_result(...)` needs `&mut self`. Rust rejects this.

Fix: `let key = self.sort_keys[0].clone()` before calling apply. SortKeyDesc is
small (attno, sort_op, collation, nulls_first) so cloning is cheap. The
alternative — splitting the struct into separate fields — would make the code
harder to follow for minimal performance benefit.

## AggColumn API: 2-arg vs 3-arg new()

Production code in custom_scan.rs passed `(AggOp, attno)` pairs but
`AggColumn::new` expected `(AggOp, attno, result_type_oid)` triples. The result
type OID isn't always known at construction time (it comes from Aggref.aggtype
which is only available during plan deserialization).

Fix: 2-arg `new(op, attno)` that delegates to `with_result_type(op, attno,
InvalidOid)`. The result type is set later during plan deserialization.

## Dual cmake library targets

The kernel library needs to be static for embedding in Rust, but the standalone
test executables (`test_device`, `test_bbox`, etc.) need a shared library
because they can't statically link the OpenMP runtime on macOS (duplicate symbol
errors).

Fix: build both `pgaccel_kernels` (STATIC) and `pgaccel_kernels_shared`
(SHARED) from the same `KERNEL_SOURCES`. Tests link against shared, Rust links
against static.

## NaN semantics in GPU sort

PostgreSQL sorts NaN as the LARGEST float value (greater than +infinity). The C
standard doesn't define NaN comparison behavior, and GPU hardware varies.

Fix: custom comparison function `pg_float_less()` in sort.cpp that explicitly
handles NaN:
```cpp
if (std::isnan(a)) return false;  // NaN is never less than anything
if (std::isnan(b)) return true;   // anything is less than NaN
return a < b;
```

Used in both the CPU fallback (std::stable_sort comparator) and the SYCL
bitonic sort kernel. The key-value sort also uses index as tiebreaker for equal
keys to maintain stability.

## DESC sort via reversal

GPU sort always produces ascending order with NaN-last semantics. For DESC
queries, we detect the sort operator OID (float4gt=623, float8gt=674,
int4gt=521) and reverse the GPU output. This is cheaper than implementing a
separate descending sort kernel — one `rev()` iterator over the index array.

## Batch size 1000 with interrupt checks

The consumption loop processes tuples in batches of 1000 with
`check_for_interrupts!()` between batches. This ensures PG can respond to
`pg_cancel_backend()` and statement_timeout during long sorts. The batch size
is small enough for responsive cancellation but large enough that the interrupt
check overhead (one atomic load per batch) is negligible.

## Three-result model for spatial predicates

GPU fp32 can misclassify points near polygon edges. Instead of:
- Always using fp64 (not available on Metal, slow on consumer GPUs)
- Accepting wrong answers
- Adding epsilon bands (geometry-dependent, error-prone)

Kernels return three results: `definite_true`, `definite_false`, `uncertain`.
The bbox pre-filter (layer 1) and point-in-ring test (layer 2) run on GPU.
Uncertain pairs get CPU recheck (layer 3) with PG's exact-precision functions.
Typically <5% of rows are uncertain, so GPU throughput is preserved while
correctness is guaranteed.

## AdaptiveCpp target set to `omp` only

The cmake configure step passes `-DACPP_TARGETS=omp` explicitly. Without this,
AdaptiveCpp's default target concatenation produces `ompmetal` as a single
string instead of two targets, which fails compilation. The Metal backend is
selected at runtime by AdaptiveCpp's device selection — setting the compile
target to `omp` just means the ahead-of-time compilation targets OpenMP, and
Metal JIT happens at runtime if a Metal device is found.

## General-Purpose Analytics Expansion

pg_accel is expanding from spatial-only to full general-purpose analytics,
matching and exceeding PG-Strom. The expansion follows an 11-phase plan with
one hard constraint: **zero overhead** — no query may ever be slower with
pg_accel loaded.

### Phase 0: Bug Fixes + Zero-Overhead Planner (complete)

- Fixed MinimalTuple memory leak in scan.rs
- Fixed COUNT/AVG NULL handling in agg.rs
- Fixed datum-as-f64 reinterpretation for integer columns
- Fixed reused slot pointers in join.rs
- Fixed GPU sort NaN handling (C++ and Rust sides)
- Fixed rescan stale pointers in join.rs
- Planner hook fast-reject: empty registry exits in <50ns
- Conservative cost model: GPU needs 30% cheaper estimate to be chosen
- GPU dispatch minimum raised to 10,000 rows

### Phase 1: GPU Expression Evaluator (complete)

The foundation for everything else. A hybrid approach:

**Stack-based bytecode interpreter** (`expr_eval.cpp`): Each GPU thread
independently evaluates a program for one row. Full SQL three-valued NULL
logic, integer overflow detection (→ UNCERTAIN), division by zero (→ UNCERTAIN),
PG-compatible NaN semantics (NaN = NaN is TRUE).

**Pre-compiled template kernels** (`expr_templates.cpp`): Five templates
cover ~80% of real WHERE clauses with zero interpretation overhead:
1. `col > const` (any comparison)
2. `col BETWEEN lo AND hi`
3. `col IN (v0, ..., v15)`
4. `col IS NULL / IS NOT NULL`
5. `col1 cmp1 const1 AND col2 cmp2 const2`

**Rust expression compiler** (`expr_compiler.rs`): Converts PG expression
trees to GPU bytecode or template matches. Three-tier: Template → Bytecode →
CpuFallback. Called in `BeginCustomScan`, not during planning.

**Columnar batch builder** (`columnar.rs`): Transposes row-oriented
`MinimalTuple` buffers into columnar `PgaccelBatch` format. Only referenced
columns are transposed.

### Phase 2: Generalized GpuScan (complete)

GpuExpr strategy variant added to AccelStrategy enum, wired through dispatch
routing and scan executor. Falls back to scalar qual until full expression
compilation is wired into BeginCustomScan.

### Phase 3: GPU Hash Join (complete — kernel + executor wired)

Open-addressing hash table with linear probing and 0.5 load factor.
- `hash_join.cpp`: Build (inner keys → hash table) + Probe (outer keys → match pairs)
- NULL keys excluded from build side, NULL outer keys skip probe (SQL semantics)
- PG-compatible NaN handling (NaN = NaN is TRUE, canonical NaN bit pattern)
- Supports int32, int64, float64 key types
- Executor wiring: equi-join detection in planner, hash table build/probe in join.rs
- CPU implementation with SYCL GPU path planned

### Phase 4: GPU Grouped Aggregation (complete — kernel + executor wired)

Hash-based grouped aggregation with per-group accumulators.
- `hash_agg.cpp`: SUM, MIN, MAX, COUNT with f64 accumulators (overflow-safe)
- NULL group keys go to a single NULL group (PG semantics)
- NULL values skipped for SUM/MIN/MAX/COUNT(col) but not COUNT(*)
- Supports multiple aggregates per query
- Executor wiring: GROUP BY detection in planner, FFI bridge, grouped mode in agg.rs
- API: build → get_group_count → get_results → free

### Phase 5: Extended Data Types (complete)

Added DATE (int32 days since J2000) and TIMESTAMP/TIMESTAMPTZ (int64
microseconds since J2000) to the expression evaluator and columnar
batch builder. Both map to existing integer storage with new type tags
for proper OID mapping. `numeric` type is explicitly CpuFallback (arbitrary
precision cannot be safely represented on GPU).

### Phase 6: GPU Window Functions (complete — kernel + executor wired)

`window.cpp` implements:
- ROW_NUMBER, RANK, DENSE_RANK — computed from sorted position
- Running SUM with Kahan compensated summation
- Running COUNT (non-NULL)
- LAG/LEAD with offset and default values, partition-aware

All operate on pre-sorted, pre-partitioned data with `partition_starts`
boundary markers. PG-compatible NaN equality for rank comparisons.
Executor wiring: UPPERREL_WINDOW hook in planner, WindowExecState in
executor/window.rs, FFI bridge for all 7 kernel functions.

### Future phases

| Phase | Status |
|---|---|
| 7: Fused Operators | Planned — requires runtime integration |
| 8: Arrow Columnar Storage | Planned — FDW integration |
| 9: GPU Direct Storage | Planned — platform-specific I/O bypass |
| 10: Self-tuning Cost Model | Planned — runtime feedback loop |
| 11: Extension Acceleration | Planned — pgvector, pg_trgm, etc. |

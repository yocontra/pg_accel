# TODO

Open work only. When an item is finished, remove it from this file; use
`git log`, `CHANGELOG.md`, and release notes for audit history.

pg_accel is a PostgreSQL 18 GPU accelerator extension, with PG19 source-smoke
preview pending a real pgrx `pg19` feature. Selected pg_accel plans must
dispatch real GPU work through AdaptiveCpp kernels and must never represent
CPU-backed execution as a pg_accel plan. If a query shape cannot be accelerated
on GPU, the planner should decline it and let PostgreSQL plan it natively.

Current integration pins:

- PostgreSQL support: PG18 is the supported pgrx extension target. PG19
  source smoke testing uses `19beta1`, but PG19 extension builds stay pending
  until pgrx exposes a real `pg19` feature. Older majors are not supported.
- PG version gate: keep `scripts/pg_version_audit.sh` passing, run PG18 pgrx
  extension tests as the default support gate, and run PG19 source smoke until
  pgrx exposes `pg19`; when pgrx adds `pg19`, add the real Cargo feature and
  CI extension build before claiming PG19 extension support.
- AdaptiveCpp: `yocontra/AdaptiveCpp`, branch `fork-safe-metal`, minimum
  SHA `456ae6910720810f5fe59f160e6707d46bb8e5f0`.
  As of 2026-07-04 this fork is merged with upstream `develop` through
  `9a912721` and retains pg_accel's Metal fork-safety, soft-fp64 lowering,
  generated-MSL polish, and `DEFAULT_TARGETS` JSON escaping fix on top.
  Upstream `feature/metal-interop` was inspected but not merged because it is
  not a clean superset of current `develop` and would drop the latest CLSPV
  chained-GEP fix.
- soft-fp64: `yocontra/soft-fp`, tag `v1.3.0`, consumed by AdaptiveCpp via
  `ACPP_SOFT_FP64_SRC_DIR`.
- macOS local PG bench note: resident Metal JIT paths require the Metal
  Toolchain component. If `xcrun metal` is missing, run
  `xcodebuild -downloadComponent MetalToolchain` before treating local PG bench
  failures as pg_accel regressions.

## Owner-Deferred CUDA Work

The project owner has deferred the following CUDA-device work until an NVIDIA
CUDA host is available. These items do not block the current Metal and no-GPU
Linux rebuild, but must be completed before making CUDA support or performance
claims:

- Provision the CUDA test host and validate the pinned AdaptiveCpp CUDA backend,
  cross-backend ABI compatibility, and Metal/CUDA result parity.
- Run `just cuda-stress`, the generic benchmark subset, cold/warm evidence runs,
  crash-band probes, and the full CUDA correctness matrix.
- Debug and tune every selected CUDA benchmark cell; retain only evidenced GPU
  wins and record honest planner declines for cells that cannot beat PostgreSQL.
- Run the CUDA fp64 calibration sweep and set any CUDA-specific cost parameters
  only from correctness-clean, crash-free evidence.
- Install and benchmark PG-Strom on the same CUDA host for like-for-like
  comparison artifacts.
- Add CUDA smoke, stress, packaging, and release-checklist evidence to CI before
  advertising CUDA as release-validated.

## Critical Path Now

Work is organized by common OLAP query families. A lane is not complete when a
helper kernel exists; it is complete only when the canonical SQL benchmark
query runs through a selected pg_accel GPU-resident plan and beats the forced
PostgreSQL parallel baseline with correctness and resident-proof artifacts.

Default planner policy (2026-06-14): pg_accel must never expose a selected
CustomScan path unless the path can honestly report `GPU Resident Pipeline:
true`. Planner hooks decline host-staged CustomScan candidates with
`no_gpu_resident_pipeline`. There is no user-facing opt-out; legacy
host-staged proof belongs in lower-level kernel/executor tests, not selected
SQL plan admission.

Generic GPU pattern policy (2026-06-28): pg_accel should ship broad,
composable GPU operator classes first, with focused kernels only when they are
measured specializations of those classes. A benchmark win is not release-ready
if it creates another isolated table/query path that cannot be reused by nearby
SQL shapes.

GPU timeout and cancellation semantics (updated 2026-07-14):
`pg_accel.kernel_timeout_ms` remains a warning threshold measured after a
synchronous GPU dispatch returns. Dense resident grouped aggregation now uses
bounded calls with PostgreSQL interrupt checks before the first call and after
each call, so cancel and `statement_timeout` are observed at those boundaries.
One-shot dispatch paths still cannot cancel an in-flight call. Before claiming
hard query-timeout semantics for every path, investigate a portable model across
AdaptiveCpp backends. This is a blocker for hard GPU-kernel cancellation claims,
not for a release that documents the GUC as a post-dispatch warning threshold
and proves bounded-batch interrupt behavior with stress artifacts.

Release-safe near-term behavior:

1. Keep GPU work chunked into bounded batches/tiles.
2. Call PostgreSQL interrupt checks between dispatches.
3. Document that PostgreSQL `statement_timeout` and user cancel may only be
   observed after the current GPU runtime wait returns.
4. Add stress tests for long-running kernels, user cancel, backend exit, and
   repeated timeout warnings.
5. Do not change the GUC contract from warning threshold to hard timeout until
   Metal, CUDA, ROCm, and Level Zero behavior is proven with crash-free
   artifacts.

Generic-consolidation gates:

1. Every new GPU lane must name its reusable operator class before admission:
   resident source/cache, expression/predicate lowering, group aggregation,
   join/PreAgg, sort/top-k/window, variable-output function, or final bounded
   materialization.
2. Focused kernels are allowed only as specializations under a generic spec:
   the planner/executor must route through shared descriptors, scratch/state,
   proof vocabulary, benchmark reporting, and correctness machinery.
3. Any path that duplicates a resident cache, expression parser, aggregate
   state layout, output emitter, planner proof field, or benchmark classifier
   must either merge into the shared layer or carry an explicit removal plan.
4. A generic operator class is not done until at least two nearby canonical SQL
   families reuse it, or the TODO records the next family that will reuse it and
   why reuse is blocked today.
5. Benchmark reports should group evidence by operator class as well as
   workload name so reviewers can see broad DB applicability, not just isolated
   workload wins.

Done criteria for every OLAP lane:

1. Canonical SQL: the normal PostgreSQL query text is benchmarked unchanged.
   Benchmark-only helper functions may exist for kernel bring-up, but they do
   not satisfy the lane.
2. GPU-resident execution: scans, dimension predicate folding, expressions,
   joins/grouping/reductions, and intermediate buffers stay in device/shared
   resident memory. PostgreSQL only sees the final bounded result.
3. Planner proof: EXPLAIN shows `GPU Resident Pipeline: true`, nonzero resident
   stage/device-column evidence, the final materialization boundary, and no
   hidden CPU executor boundary.
4. Benchmark proof: correctness diff passes, pg_accel kernel counters increase,
   stock fallback counters stay zero, the report threshold matrix marks the
   lane as `gpu_winner`, and median speedup is greater than 1.0x vs forced PG
   parallel at the lane's canonical row scales.
   A lane with any known entry in a current benchmark report's `crashes` array
   is not complete. Correctness-diff failures are crash-class blockers, even
   when the PostgreSQL server stays alive; stale TODO claims must be reopened
   until a fresh artifact proves `crashes: []`.
5. Maintainability proof: the implementation is a reusable OLAP operator
   class, not a one-off table hack; adding the next query in the family should
   reuse the same resident batch/cache, expression, join, and aggregate pieces.
6. Consolidation proof: if the lane introduced a focused fast path, there is
   either a follow-up generic extraction item above any new focused benchmark
   work, or the focused path is already expressed as a specialization of a
   shared GPU operator spec.

High-priority FP64 / soft-fp64 closure (2026-07-04):

1. Treat Metal fp64 as available through the pinned `yocontra/soft-fp`
   library, not as disabled hardware. `has_native_fp64` is a planner cost
   signal only; any planner, executor, kernel, benchmark, or doc path that
   declines solely because Metal lacks native fp64 must be fixed or reopened
   here.
2. Close the remaining fp32-only spatial dispatch paths. The C++ kernels now
   include non-templated fp64 `sphere_distance` and `st_length` entry points,
   but Rust geometry dispatch still stores many PostGIS coordinates as `f32`
   and passes `use_fp64=false` for point-in-ring, DWithin, area, and
   length-style paths. Build a typed geometry extraction/dispatch contract
   that preserves native PostGIS f64 coordinates for fp64 SQL semantics.
   Existing f32 shortcuts may remain only as measured approximate
   specializations with explicit uncertainty gates.
3. Finish the immutable `fp64_matrix` as actual GPU wins, not native declines:
   current resident wins cover scalar reduce SUM/MIN/MAX/stats and grouped
   `hashagg_f64_aggs`; remaining high-priority rows are `sort_f64_keys`,
   `hashagg_f64_keys`, `spatial_fp64_recheck`, and `h3_fp64_ops`, plus any
   row scale where the selected GPU path still loses PG parallel.
4. Calibrate `pg_accel.soft_fp64_cost_multiplier` only after the full fp64
   matrix dispatches. Calibration cannot hide missing GPU paths; a multiplier
   candidate with planner declines, crashes, correctness gaps, or sub-parity
   cells is disqualified.
5. Remove release-facing fp64 disable behavior. Compatibility-only no-op GUCs
   may remain while old configs exist, but planner/executor admission must not
   treat `fp64_enabled=false` as a reason to skip implemented fp64 GPU work:
   pg_accel's supported behavior is to run fp64 on GPU via native fp64 or Metal
   soft-fp64, and to decline only unsupported SQL shapes or cost-losing plans.

Progress (2026-07-04 point-distance fp64 dispatch): `ST_Distance(Point,
Point)` dispatch now extracts f64 PostGIS point coordinates and calls
`pgaccel_sphere_distance_bulk(use_fp64=true)`, so the runtime exercises the
native/soft-fp64 sphere-distance kernel instead of forcing the fp32 branch.
`pgaccel_h3_lat_lng_to_cell_bulk` also now honors `use_fp64` instead of
silently forcing the fp32 path, and it reads `float*`/`double*` inputs according
to that flag before any high-resolution promotion. H3 scalar callers that
request fp64 now reach the f64/soft-fp64 kernel. The broader typed geometry
contract remains open for polygon, line, DWithin, area, and point-in-ring
paths.

Progress (2026-07-04 H3 soft-fp64 GPU residency): exact H3 lat/lng-to-cell is
now split into GPU projection and integer-finalization kernels for scalar
lat/lng conversion, direct grouped count, f32/exact grouped count, and the
resident grouped-count entry point. The old host exact-fixup loops and the
high-resolution host `cells -> keys -> pgaccel_hash_count_i64_execute` branch
were removed from `h3_ops.cpp`; the only remaining lat/lng-to-cell calls in
that file are the fast fp32 candidate kernels. Targeted Metal probes passed for
scalar fp32/fp64 res5/res12 and grouped count `bulk`/`f32_exact` res5/res12.
The 2026-07-04 post-toolchain run of `test_h3` completed with `856 passed, 0
failed`; cold JIT still emits small unused builtin-string warnings, but the
previous soft-fp64 crash/timeout concern is closed for this harness. `README.md`
no longer presents `pg_accel.fp64_enabled` as a user-facing fp64 disable
feature; the legacy GUC is retained only as a deprecated no-op compatibility
flag.

Progress (2026-07-04 fp64 admission cleanup): float8 sort and float64 hash-join
planner admission no longer consult `pg_accel.fp64_enabled`; implemented fp64
operators are admitted or rejected by support/cost only. The legacy GUC is now
registered as a deprecated no-op compatibility flag so old configs still load
without creating a release-facing fp64 disable path.

Progress (2026-07-04 AdaptiveCpp/soft-fp64 Metal closure): diagnosed and fixed
the fp64 failures in the compiler/toolchain instead of adding planner bypasses.
Root causes fixed in AdaptiveCpp's Metal path:
SLEEF helper address-space specialization for outlined soft-fp64 helpers,
unreachable soft-fp64 function/global pruning after the post-link preservation
pass, demand-driven retention of emitter-implicit soft-fp64 primitive helpers,
source-level propagation of LLVM `noinline` into generated MSL to stop Apple's
pipeline compiler from flattening the `atan2` call graph, and null/undef pointer
PHI handling so generic `null` does not conflict with device/thread pointers.
Evidence: cold fp64 probe matrix passed for add, mul, sqrt, sin, cos, asin,
atan2, and haversine with zero mismatches; cold `test_spatial` passed
`162/162`; `test_h3` passed `856/856`.

Remaining H3/FP64 release work: make the SQL `h3_fp64_ops` and
`spatial_fp64_recheck` benchmark rows dispatch GPU plans with kernel-counter
evidence instead of planner declines, add positive Rust SQL tests for admitted
soft-fp64 H3/spatial plans, and finish the AdaptiveCpp/soft-fp64 generated-MSL
warning sweep. The crash class is closed in the local kernel harnesses above;
remaining warning/JIT churn is polish and benchmark-methodology noise unless a
new crash or timeout appears.

Current OLAP benchmark ladder:

1. SSBM Q1.x / filtered fact-table revenue aggregate. Canonical targets:
   `ssbm_q1_1`, `ssbm_q1_2`, and `ssbm_q1_3`.
   - Query class: star-schema date filter folded to fact-table `lo_orderdate`
     filters, `lo_discount` and `lo_quantity` predicates, and
     `SUM(lo_extendedprice * lo_discount)`.
   - Implementation target: resident SSBM lineorder column cache or resident
     scan producer for `lo_orderdate`, `lo_discount`, `lo_quantity`, and
     `lo_extendedprice`; GPU date-filter application; integer revenue
     expression; scalar aggregate finalization.
   - Done: all three Q1 queries select a resident GPU plan and beat PG
     parallel in the benchmark report.
2. Low-cardinality filtered GROUP BY. Canonical targets: `grouped_agg`,
   `grouped_agg_high_card`, `hashagg_*`, `filtered_grouped_agg`, and the
   reusable grouped aggregate pieces proven by SSBM Q2.x.
   - Query class: fact filters plus low/medium-cardinality grouping and one or
     more SUM/COUNT aggregates.
   - Implementation target: device hash/group aggregate over resident batches,
     reusable aggregate state layout, bounded result materialization, and
     planner admission for the generic benchmark shapes.
3. Star joins and PreAgg. Canonical targets: SSBM Q2.x, Q3.x, and Q4.x.
   - Query class: retained dimension filters, fact joins, grouped revenue or
     profit aggregates, and ORDER BY over small grouped output.
   - Implementation target: generalize the Q2 retained dimension/build-side
     buffers and GPU membership filters into multi-dimension Q3/Q4 joins,
     PreAgg consumers, profit expressions, and grouped output sort when needed.
4. Top-K and window analytics. Canonical targets: `topk_wide`,
   `gpu_sort_topk_wide`, `window_analytics`, and time-series rollups.
   - Query class: ORDER BY/LIMIT, rank/row_number, partitioned windows, and
     time-bucket aggregates.
   - Implementation target: internal GPU top-k, segmented sort/window
     primitives, and downstream aggregate/window consumers before any full
     row-output path is admitted.
5. Geo/H3/raster OLAP. Canonical targets: H3 grouped count/SRF aggregate,
   PostGIS spatial selectivity, raster map algebra, and mixed geo rollups.
   - Query class: variable-cardinality GPU functions and geospatial predicates
     consumed by aggregates or joins.
   - Implementation target: device CSR outputs, prepared geometry/raster
     buffers, GPU predicate masks, grouped consumers, and bounded result
     materialization.

Use up to six parallel agents against these broad generic operator tracks:

- Agent A: resident source/cache and columnar batch contracts shared by OLAP,
  time-series, H3, PostGIS, and raster producers.
- Agent B: `ResidentGroupAgg` state, expression/predicate lowering, aggregate
  lanes, dictionary keys, and reusable output materialization.
- Agent C: GPU-resident joins, retained dimension/build-side reuse, and PreAgg
  consumers over the same resident source/groupagg contracts.
- Agent D: reusable top-k, sort, segmented window, and time-series rollup
  primitives that consume/produce resident batches.
- Agent E: variable-output H3/PostGIS/raster GPU functions lowered to masks,
  keys, or CSR-like buffers consumable by generic joins/aggregates.
- Agent F: planner admission, EXPLAIN/report gates, duplicate-path detection,
  threshold matrix, and PostgreSQL/PG-Strom comparison artifacts by operator
  class.

Current slice (2026-07-02): generic operator consolidation outranks new
focused benchmark lanes. Q1/Q2/Q3/Q4, H3 grouped count, scalar/grouped FP64
reductions, and dense grouped aggregate wins prove that resident GPU OLAP can
beat PG parallel, but too many wins still live in query-family-specific
cache/spec/kernel surfaces. The next broad hunk is to collapse those paths into
reusable GPU patterns: resident source descriptors, expression/predicate IR,
aggregate state/scratch, dictionary grouping, bounded materialization, planner
proof, and benchmark classification. Canonical SQL for `grouped_agg`,
`grouped_agg_high_card`, `hashagg_10g`, `hashagg_100g`, `hashagg_1kg`,
`hashagg_10kg`, `hashagg_256g`, `filtered_grouped_agg`,
`expression_grouped_agg`, `predicate_filter_expression_grouped_agg`,
`case_when_expression_grouped_agg`, `dictionary_grouped_agg`, SSBM Q1/Q2/Q3/Q4,
time-series rollups, and geo/H3 rollups should route through shared operator
classes wherever the semantics match. Focused kernels may remain only as
measured specializations under those shared specs.

Current ship-gate remediation (2026-07-04):

1. Continue the generic GPU-resident hashjoin/join-aggregate path. Count-only
   resident hashjoin now has a reusable resident build/probe/final-count path
   for `COUNT(*)` equijoins and no longer misses the canonical winner lanes:
   `hash_join @ 1M`, `hashjoin_10k_1m @ 10K/100K/1M/10M`, and
   `gpu_hashjoin_large_build @ 10K` all select `GpuAccelJoin` with resident
   proof and clean correctness. The remaining broad join work is the
   join-filter-groupagg family (`gpu_hashjoin_filter @ 1M`,
   `hashjoin_filter_groupagg @ 1M`): collapse that into the same resident
   build/probe contract plus generic predicate/grouped-aggregate output, then
   delete or demote any duplicate focused path once the generic route wins.
   Progress: `gpu_hashjoin_filter` now uses a generic one-dimension resident
   star groupagg path with resident fact key/value columns, resident dimension
   match/group-code maps, GPU compaction of selected `(group_code, value)`
   pairs, and the shared dense resident f64 grouped aggregate over the compacted
   rows. `mixed_join_agg` now reuses the same resident star groupagg operator
   for canonical `JOIN -> GROUP BY -> SUM, COUNT(*)` SQL. The remaining broad
   work is to finish and benchmark the fused resident star
   join/filter/groupagg path for SUM/COUNT, move dense grouped scratch/sort
   state to persistent device-pointer-safe storage, and lift guarded
   large-build cells into GPU winners instead of native declines.
2. Rerun the full PG18 benchmark suite after the planner admission fixes for
   small H3 grouped-count rows and small selective `WHERE` filtered groupagg
   rows. Those lanes should now be honest native declines at 10K with
   `h3_rows_below_grouped_agg_min` and
   `resident_groupagg_filtered_rows_below_selective_min` evidence, while
   100K+ rows remain GPU winner candidates.
3. Regenerate full-suite artifacts so `benchmark_failure_ledger.json` and
   `benchmark_failure_ledger.md` become the canonical work queue for every
   ship-gate failure and every measured row still below PG-parallel parity.
4. Capture cache-mode `both` H3 evidence once the 10K grouped-count admission
   floor is verified, so H3 winner rows have warm/cold cache proof without
   weakening the resident-only policy.

Progress (2026-07-03 resident hashjoin count): landed a childless,
GPU-resident `GpuHashJoin` count path backed by resident key caches, device
build/probe/count kernels, planner unwrapping for PG18
`AggPath -> GatherPath -> AggPath -> HashPath`, resident proof metadata, and
benchmark cache priming. Fixed the Metal crash by removing the unsupported
64-bit atomic from the resident count kernel. Verified crash-free:
`ctest --test-dir pgaccel-kernels/build -R test_hash_join --output-on-failure`,
`cargo test -p pg_accel_bench`, and `cargo test -p pg_accel`.

Fresh PG18 benchmark evidence:
`benchmarks/artifacts/resident-hashjoin-smoke-20260703-102000` shows
`hashjoin_10k_1m` dispatching on all four scales with **2.41x**, **3.16x**,
**5.29x**, and **6.96x** median speedups, zero stock fallback, resident proof
reported, and clean correctness diffs. `benchmarks/artifacts/resident-hashjoin-hash-join-20260703-102200`
shows `hash_join @ 1M` dispatching and winning **4.95x** while the below-floor
and over-cap cells decline natively as expected. `benchmarks/artifacts/resident-hashjoin-large-build-20260703-102400`
shows `gpu_hashjoin_large_build @ 10K` dispatching and winning **2.56x** while
larger build-side cells decline natively as expected.

Progress (2026-07-04 compacted resident star groupagg): the
`gpu_hashjoin_filter` join-filter-groupagg lane now dispatches a compacting
GPU projection before the shared dense grouped f64 aggregate, so rejected fact
rows no longer flow through the grouped sort. Final guarded artifact
`benchmarks/artifacts/gpu-hashjoin-filter-compact-ladder-guarded-20260704`
passed the ship gate with `crashes: []`, clean correctness, resident proof,
stock fallback 0, and median speedups of **3.95x** at 10K, **3.45x** at 100K,
and **1.52x** at 1M. The 10M cell is now an honest native decline with
`hashjoin_build_side_too_large` evidence and zero pg_accel kernel dispatch,
removing the prior crash without counting native PostgreSQL execution as a GPU
win.

Progress (2026-07-04 generic mixed join aggregate): `mixed_join_agg` now routes
through the generic resident star groupagg planner/executor path instead of a
native/planner-declined mixed-workload row. The recognizer admits canonical
`dim_group, SUM(fact_value), COUNT(*)` targets without requiring artificial
fact or dimension filters, the cache loader supports `always_true` dimension
filters without reading a fake filter column, and the executor uses a
row-aligned GPU projection fast path for unfiltered joins before the shared
dense grouped f64 aggregate. Fresh PG18 release artifact
`benchmarks/artifacts/validate-mixed-join-agg-resident-star-shared-copy` passed
with `crashes: []`, zero ship-gate failures, clean correctness diffs, resident
pipeline proof, stock fallback 0, kernel class `resident_star_groupagg`, and
median speedups of **1.28x** at 10K, **1.89x** at 100K, **6.32x** at 1M, and
**13.04x** at 10M versus forced PostgreSQL parallel. This supersedes the July
2 full-suite `mixed_join_agg @ 10M` planner-decline gap; the next full-suite
run should remove that row from the release ledger.

Immediate broad priority: make `ResidentGroupAgg` predicate/expression IR and
resident star groupagg the single planner, executor, EXPLAIN, and
benchmark-report contract for grouped OLAP before adding more focused workload
lanes. Dense grouped aggregates, SSBM fact/date predicates, retained-dimension
membership filters, H3/geo rollups, star-join filters, and future time-series
rollups should all publish the same descriptor family; specialized kernels can
still win, but they must be selected from that shared spec and deleted or
demoted once the generic path wins. The next large implementation target is a
fused resident star join/filter/groupagg primitive that keeps compacted keys,
aggregate state, and sorted/segmented scratch GPU-resident across high-row and
high-build-cardinality cells.

Progress (2026-07-04 fused resident star groupagg): added a reusable fused
one-dimension resident star join/filter/grouped SUM/COUNT ABI under the
existing resident star groupagg operator. Unfiltered star joins use the fused
path from the 8K one-pass band; selective filtered joins keep the compacted
path until 1M rows because the direct reducer regressed the 10K/100K bands.
The compact/project fallbacks stay in place until the fused path has a
high-cardinality/sorted fallback and a stronger selective-row cost model.

Fresh PG18 artifacts:
`benchmarks/artifacts/validate-gpu-hashjoin-filter-fused-selector-20260704-200753`
passed with `crashes: []`, zero ship-gate failures, resident-boundary
`failed_rows=0`, stock fallback 0, kernel class `resident_star_groupagg`, and
median speedups of **1.85x** at 10K, **1.00x** at 100K, and **2.23x** at 1M;
10M remains an honest native decline with `hashjoin_build_side_too_large`.
`benchmarks/artifacts/validate-mixed-join-agg-fused-star-20260704-200915`
passed with `crashes: []`, zero ship-gate failures, resident-boundary
`failed_rows=0`, stock fallback 0, one fused dispatch per timed query, and
median speedups of **1.52x** at 10K, **2.52x** at 100K, **6.02x** at 1M, and
**15.52x** at 10M.

Progress (2026-07-05 resident dense groupagg 1K radix route): `hashagg_1kg`
now has a GPU-resident route split across the 10K/100K transition band and the
large-row sorted/segmented path. The sorted path no longer copies signed group
keys through host vectors: the C ABI includes device-pointer i32 key/value sort
entry points, i32 sign conversion runs on GPU, the radix histogram prefix step
uses device chunk scans, and dense group keys are normalized to `0..N` plus a
sentinel so 1K-group rows use low-bit radix passes instead of a full signed
sort. The direct SUM/COUNT route remains the small-row specialization under the
same `ResidentGroupAgg` operator class, with a narrow 100K transition shape
selected only where it beats sort setup.

Fresh PG18 release artifact
`benchmarks/artifacts/run-1783266524-2773-847007000` passed the ship gate for
`hashagg_1kg` with `crashes: []`, selected resident CustomScan on all four
scales, `GPU Resident Pipeline: true`, stock fallback 0, clean correctness
diffs, kernel dispatch on every timed row, and median speedups of **1.31x** at
10K, **1.17x** at 100K, **1.44x** at 1M, and **2.47x** at 10M versus forced
PostgreSQL parallel. This supersedes the July 4 hybrid-route artifacts and
closes the known 1K dense groupagg crash/performance blocker.

Next broad ResidentGroupAgg work: validate the same resident radix/sorted dense
route across adjacent grouped OLAP families (`hashagg_10kg`,
`gpu_hashagg_med_card`, `grouped_agg_high_card`, and `hashagg_f64_aggs`), then
lift any wins into shared threshold logic instead of workload-name branches.
The next implementation hunk should also add a predicate-aware compact/mask
producer for OR/IN/NOT/BETWEEN/CASE families and a direct-column MIN/MAX/AVG
specialization for time-series-style rollups. These are operator-class fixes,
not workload-name paths.

Progress (2026-07-06 ResidentGroupAgg high-cardinality route repair): the
medium/high-cardinality dense COUNT/SUM lane is crash-free again and no longer
routes the 100K-row/10K-group band through the tiled simple-wide full-rescan
shape. The crash in `gpu_hashagg_med_card` was traced to resident groupagg
scratch initialization through Metal `queue::memset`/`queue::fill`; those
initializers now use explicit GPU init kernels for the resident dense, segment,
star, and v9 scratch paths. The follow-up performance failure was route
selection, not correctness: simple-wide remains a valid small-row
specialization, but for >=2049 groups it now stops at 65,536 rows and hands the
larger high-cardinality transition band to the sorted resident route. This is a
shared `ResidentGroupAgg` threshold, mirrored in cache-owned partial scratch
sizing, not a workload-name branch.

Fresh PG18 evidence after rebuilding/reinstalling the extension: native
`test_olap_ssbm` and `test_expr_templates` passed; serial SQL repros for
`hashagg_1kg`, `hashagg_10kg`, `grouped_agg_high_card`, and
`gpu_hashagg_med_card` were crash-free after discarding the intentionally
invalid concurrent harness run that collided on shared setup tables. The
failing `gpu_hashagg_med_card @ 100K` cell moved from **0.50x** (34.92 ms vs
17.45 ms, simple-wide route, artifact
`benchmarks/artifacts/resident-med-card-ladder-restored`) to **1.42x** in the
full ladder (13.07 ms vs 18.54 ms, sorted route, artifact
`benchmarks/artifacts/resident-med-card-ladder-sorted-threshold`). The same
artifact shows `gpu_hashagg_med_card` passing all default scales with
**1.20x**, **1.42x**, **2.10x**, and **3.46x**, resident proof on every row,
kernel dispatch on every row, zero stock fallback, clean correctness diffs, and
no ship-gate failures. Adjacent guard ladders also passed:
`hashagg_10kg` at **1.74x**, **1.35x**, **3.16x**, and **3.28x**
(`benchmarks/artifacts/resident-hashagg-10kg-ladder-sorted-threshold`) and
`hashagg_1kg` at **2.11x**, **1.24x**, **2.22x**, and **1.87x**
(`benchmarks/artifacts/resident-hashagg-1kg-threshold-guard`). Remaining work:
run the broader grouped-OLAP/full-suite gate so any non-hashagg losses or
crashes become the next ledger items, then continue the predicate-mask and
MIN/MAX/AVG generic operator work above.

Predicate-wide experiment note (2026-07-04): a first one-scan local-array
SUM/COUNT ABI for interval CASE predicates was added and covered in native
`test_olap_ssbm`, but executor selection is deliberately disabled because the
design lost badly on canonical OR CASE SQL. Artifacts:
`benchmarks/artifacts/validate-case-or-predicate-wide16-20260704-212140` and
`benchmarks/artifacts/validate-case-or-predicate-wide16-block1k-20260704-212538`
were interrupted after 1M rows showed ~40-44 ms pg_accel timings versus
~27-28 ms PG parallel, and 10M warmups were also below parity. Do not mark
OR/IN/NOT/BETWEEN/CASE complete from this ABI. The next design should avoid
large per-workgroup local arrays: likely a reusable predicate mask/compact
producer plus separate grouped COUNT, or a warp/subgroup segmented reducer with
enough row-level parallelism and no repeated group-tile scans.

Next broad join-aggregate work: replace the fixed one-dimension,
predicate-specialized star cache with a v2 reusable resident fact/dimension
source descriptor plus runtime predicate/join/group spec, then extend the
fused ABI with a high-cardinality sorted/segmented fallback so focused
project/compact buffers can be deleted rather than kept as permanent paths.

Progress (2026-07-01 crash-free benchmark sweep): SSBM Q4 grouped-profit
dispatch now uses a shared kernel-parameter slab instead of a large Metal
lambda capture list, removing the `newArgumentEncoderWithBufferIndex`
assertion that aborted the backend during Q4 correctness checks. The selected
GPU path stays mandatory and resident; the change only reduces the Metal kernel
ABI surface for the existing Q4 specialization under `ResidentGroupAgg`.

Fresh PG18 local evidence after rebuilding `pgaccel_kernels` and reinstalling
the extension: targeted Q4 repros for `ssbm_q4_1` at 10K/100K/1M, `ssbm_q4_2`
at 1M, and `ssbm_q4_3` at 1M all passed with `crashes: []` and clean
correctness. The full SSBM category then passed 52 workloads with zero crashes,
clean correctness diffs, and zero ship-gate failures:
`benchmarks/artifacts/current-ssbm-crashfree2-20260701`. The broader stability
sweep also passed `regression` with 38 workloads and zero crashes
(`benchmarks/artifacts/current-regression-crashfree3-20260701`) and
`fp64_matrix` with 8 workloads, zero crashes, and clean correctness
(`benchmarks/artifacts/current-fp64-matrix-crashfree-20260701`). That older
FP64 matrix was stability evidence only: the cells were honest planner
declines with no GPU dispatch, so it did not count as FP64 GPU-win proof. It
is now superseded by the resident scalar/grouped FP64 wins below for
`reduce_f64_sum`, `reduce_f64_minmax`, `reduce_f64_stats`, and
`hashagg_f64_aggs`; the remaining FP64 matrix rows are tracked in the
high-priority soft-fp64 closure item above.

Progress (2026-07-01 FP64 resident scalar reduce): canonical
`SELECT SUM(v_f64) FROM bench_fp64_num` and
`SELECT MIN(v_f64), MAX(v_f64) FROM bench_fp64_num` now run through
`ResidentGroupAgg` as single-group resident FP64 reductions. The loader
`pg_accel_load_resident_f64_reduce_cache(table, value_col, nullable)` builds a
backend-local resident value column plus a synthetic all-zero group column, the
planner recognizes no-GROUP-BY direct FP64 aggregate target lists from the
cache shape, and the executor emits scalar SQL aggregate rows directly while
still refusing CPU fallback. The same `resident_groupagg` proof vocabulary is
used: EXPLAIN shows `GPU Resident GroupAgg Key: single_group`, measure
`direct_column`, filter/predicate `none`, nonzero stage/device-column proof,
and `GPU Resident Boundary: none`.

Fresh PG18 evidence after reinstalling the extension and refreshing the local
extension catalog: `reduce_f64_sum @ 100K` passed with `crashes: []`, clean
correctness, resident-boundary audit `failed_rows=0`, stock fallback 0, and
2.49x median speedup vs forced PG parallel at
`benchmarks/artifacts/current-fp64-resident-reduce-sum-final-20260701`.
`reduce_f64_minmax @ 100K` passed with `crashes: []`, clean correctness,
resident-boundary audit `failed_rows=0`, stock fallback 0, and 1.34x median
speedup at
`benchmarks/artifacts/current-fp64-resident-reduce-minmax-final-20260701`.

Progress (2026-07-01 native resident scalar FP64 reduce): single-group FP64
reductions now dispatch a dedicated resident scalar `pgaccel_expr_template_reduce_f64_usm`
kernel instead of the dense grouped kernel. The scalar ABI consumes one
resident `pgaccel_expr_usm_col`, accepts a SUM/MIN/MAX/COUNT/SUMSQ lane mask,
skips NULL values on device, and copies back only final scalar lanes. Planner
recognition now covers `AVG(v_f64), STDDEV(v_f64), VAR_POP(v_f64)` as
`ResidentDenseGroupedF64Layout::SingleStats`, with proof mask
SUM|COUNT|SUMSQ. Benchmark cache priming and threshold metadata now include
`reduce_f64_stats` as `resident_f64_reduce_single_stats`.

Progress (2026-07-01 resident grouped FP64 stats): the scalar stats pattern is
now lifted into `ResidentGroupAgg` for grouped two-measure FP64 aggregates.
`hashagg_f64_aggs` plans from the generic resident cache shape using the
`stats_pair` measure op, emits the reusable logical proof
`two_measure_stats`, and materializes `gk, SUM(v_f64), AVG(w_f64),
STDDEV(v_f64)` from resident GPU lanes only. The native implementation uses a
scratch-backed sort/segment grouped reducer instead of the rejected tiled
full-rescan shape, with independent NULL handling for the primary SUM/SUMSQ
lane and the secondary AVG lane. The benchmark contract now runs this lane at
100K and 1M so grouped FP64 stats must prove the same larger-row scaling rule
as the rest of grouped OLAP.

Fresh PG18 evidence after rebuilding and reinstalling the extension:
`hashagg_f64_aggs` passed at both 100K and 1M with selected resident
CustomScan, `GPU Resident Pipeline: true`, resident-boundary audit
`failed_rows=0`, exact correctness diffs, `crashes: []`, kernel delta 20 per
scale, rows dispatched/processed equal to 10 timed iterations, and stock
fallback 0. Median speedups vs forced PG parallel improved with scale: 1.42x
at 100K (7.68 ms vs 10.94 ms) and 2.07x at 1M (16.93 ms vs 35.06 ms).
Artifact root:
`benchmarks/artifacts/current-fp64-grouped-stats-2scale-20260701`.
Verification: `cargo fmt --all --check`, `git diff --check`, `cargo check -p
pg_accel --features pg18 --all-targets`, `cargo check -p pg_accel_bench`,
`cargo test -p pg_accel --features pg18 --lib -- --nocapture` (298 passed),
`cargo test -p pg_accel_bench -- --nocapture` (327 passed), native
`test_olap_ssbm` warm run (555 passed), `just install-pg-accel 18`, extension
catalog refresh, and the two-scale benchmark above.

Progress (2026-07-01 ResidentGroupAgg predicate proof): resident grouped
aggregate logical proof now exposes a generic predicate descriptor in addition
to the family-specific filter label. EXPLAIN emits `GPU Resident GroupAgg
Predicate Guard` and `GPU Resident GroupAgg Value Predicate` for dense, H3, and
SSBM resident groupagg plans, and the benchmark ship gate requires these fields
for groupagg winner lanes. This makes the existing boolean guard plus value/RHS
range predicate support visible as shared operator evidence instead of another
implicit dense-only enum case.

Fresh PG18 evidence after reinstalling the extension: live plan-shape tests for
renamed direct and expression resident groupagg tables assert the new predicate
fields and pass; `grouped_agg` benchmark artifacts across 10K/100K/1M/10M
passed with `crashes: []`, clean correctness, zero ship-gate failures, and
monotonic median speedups of 2.53x, 2.75x, 3.00x, and 3.69x. Artifact root:
`benchmarks/artifacts/current-grouped-agg-predicate-proof-20260701`. The next
broad implementation hunk should lift this descriptor into a serialized
`ResidentGroupAggPredicateExpr` or a v10 mask-producing path so arbitrary
boolean/comparison/IN/null predicates are shared by dense, geo/H3, SSBM, and
future FP64 single-group groupagg lanes instead of adding more filter enums.

Progress (2026-07-02 ResidentGroupAgg predicate IR proof gate): resident
grouped aggregate EXPLAIN now emits `GPU Resident GroupAgg Predicate IR` for
the same shared operator class across dense grouped FP64, scalar FP64,
H3 grouped count, and SSBM Q1/Q2/Q3/Q4 paths. The label records the guard,
predicate scope, value/RHS range source, and range count where the current
resident dense predicate descriptor can prove it. Benchmark ship gates now
require this field for resident groupagg winner lanes, so a new duplicate path
cannot count as a GPU win by mimicking only the older key/measure/filter fields.

Fresh PG18 evidence after reinstalling the extension and refreshing the local
catalog: `hashagg_f64_aggs` passed at 100K and 1M through the selected
resident CustomScan with `GPU Resident Pipeline: true`, the new predicate IR
proof (`guard=none;value=none`), resident-boundary audit `failed_rows=0`,
exact correctness diffs, `crashes: []`, kernel delta 20 per scale, rows
dispatched/processed equal to 10 timed iterations, and stock fallback 0. Median
speedups vs forced PG parallel were 1.17x at 100K (9.37 ms vs 10.94 ms,
noisy/non-significant in this run) and 1.35x at 1M (22.79 ms vs 30.80 ms,
p=6.55e-6). Artifact root:
`benchmarks/artifacts/current-groupagg-predicate-ir-20260702`. Verification:
`cargo fmt --all --check`, `git diff --check`, `cargo check -p pg_accel
--features pg18 --all-targets`, `cargo check -p pg_accel_bench`, focused
predicate/report artifact tests, `cargo test -p pg_accel --features pg18 --lib
-- --nocapture` (298 passed), `cargo test -p pg_accel_bench -- --nocapture`
(328 passed), `just install-pg-accel 18`, extension catalog refresh, and the
two-scale benchmark above.

Progress (2026-07-02 ResidentGroupAgg predicate IR reuse + resident copy
stability): dense resident groupagg now reuses the same predicate descriptor
for row `WHERE active AND value/rhs range` clauses and aggregate
`FILTER (WHERE active AND value/rhs range)` clauses, instead of limiting range
predicate IR to `CASE WHEN` measure expressions. The planner normalizer accepts
PostgreSQL's implicit AND qual lists as well as `BoolExpr(AND)`, so canonical
SQL `BETWEEN` predicates lower into the shared `ResidentGroupAgg` predicate IR
without benchmark-only query rewrites. EXPLAIN now proves
`where_bool_and_rhs_ranges` and `aggregate_filter_bool_and_rhs_ranges` with
`guard=resident_bool_column;scope=...;value=rhs_ranges;ranges=1`.

The shared resident copy helper was also hardened for macOS/Metal:
`pgaccel_expr_device_alloc_copy` now places host-built resident columns in
shared USM on Apple/Metal and copies into them with host `memcpy`, while
scratch/output allocations continue to use device USM. This avoids both
AdaptiveCpp Metal's cold `queue::memcpy` blit/JIT path and the follow-up copy
kernel path that crashed forked PostgreSQL backends in Apple's telemetry/logging
helper thread. The fix applies to every `ExprDeviceBuffer::copy_from_slice`
resident cache family: dense groupagg, SSBM, H3/geo, hash join, and future
time-series resident sources.

Fresh PG18 evidence after rebuilding kernels, reinstalling the extension, and
refreshing the local catalog: same-backend SQL cache loads for both no-filter
and RHS/filter resident groupagg caches returned 100K rows without a backend
crash; same-backend EXPLAIN selected `Custom Scan (GpuAccelAgg)` for
`SUM(price * discount), COUNT(*) ... WHERE active AND discount BETWEEN ...`
with `GPU Resident GroupAgg Predicate IR:
guard=resident_bool_column;scope=row;value=rhs_ranges;ranges=1`; and the live
integration test
`plan_shape_resident_groupagg_reuses_predicate_ir_for_where_and_filter_ranges`
passed, including native/GPU result comparison and GPU kernel dispatch.
Verification: `just gpu-build`, native `test_olap_ssbm` (555 passed),
native `test_fork_warmed` (passed), `cargo fmt --all --check`,
`git diff --check`, `cargo check -p pg_accel --features pg18 --all-targets`,
`cargo check -p pg_accel_bench`, focused resident groupagg logical/EXPLAIN
unit tests, `cargo test -p pg_accel --features pg18 resident_dense_grouped --
--nocapture`, `cargo test -p pg_accel_bench benchmark_ship_gate --
--nocapture`, `just install-pg-accel 18`, and extension catalog refresh. The
long native `test_expr_templates` smoke was interrupted after repeated Metal
archive compilation and is not counted as passing evidence.

Progress (2026-06-28 cache-shape ResidentGroupAgg consolidation): dense
resident grouped aggregation is now planned from the loaded resident cache
shape instead of benchmark table names. The cache records rel OID, group/value
attnos, optional RHS/filter attnos, and measure op; the planner derives the
shared `ResidentGroupAggLogicalSpec` from those descriptors, CustomScan private
data carries source attnos, and the executor validates the loaded source shape
before dispatch. Table/column rename integration tests prove the path survives
renames after cache load, and the old benchmark-named SQL cache loaders were
deleted in favor of one generic
`pg_accel_load_resident_groupagg_cache(table, group, key_type, value, rhs,
measure_op, filter, nullable)` entrypoint. Focused C++ kernels remain only as
ABI specializations selected under the shared resident groupagg spec.

Filtered WHERE and aggregate `FILTER` lanes now use compact resident filtered
columns when semantics allow it, so low-selectivity filters dispatch selected
resident rows instead of rereading the whole cache. PG18 local warm-cache matrix
evidence with realistic GUCs, correctness diffs, selected resident CustomScan,
nonzero kernel deltas, stock fallback = 0, and `GPU Resident Pipeline: true`:
`hashagg_256g` 1.22x @ 100K and 2.14x @ 1M;
`filtered_grouped_agg` 1.23x @ 100K and 5.58x @ 1M with 99,040 / 998,350
dispatched rows over 10 iterations; `predicate_filter_expression_grouped_agg`
4.67x @ 100K and 5.25x @ 1M with compact FILTER dispatch;
`expression_grouped_agg` 2.15x @ 100K and 1.87x @ 1M;
`case_when_expression_grouped_agg` 3.01x @ 100K and 2.63x @ 1M;
`dictionary_grouped_agg` 2.74x @ 100K and 3.40x @ 1M;
`timeseries_sensor_rollup` 2.00x @ 100K and 2.15x @ 1M;
`grouped_agg_high_card` 1.31x @ 100K and 3.52x @ 1M. Artifact root:
`benchmarks/artifacts/resident-groupagg-generic-matrix-20260629-000135`.
Verification: `cargo fmt --all`, `cargo check -p pg_accel --features pg18
--all-targets`, `cargo check -p pg_accel_bench --features integration_tests`,
`just install-pg-accel 18`, and the matrix above.

Progress (2026-06-28 resident-only planner evidence repair): the hard
GPU-resident-only admission policy now preserves specific shape evidence before
falling back to generic `no_gpu_resident_pipeline` counters. Base-relation,
join, and GROUP_AGG hooks run cheap read-only observers for sort/top-k
blockers, H3 scalar-predicate blockers, PostGIS predicate blockers, NLJ
host-boundary blockers, PreAgg missing-child blockers, and parallel fused-count
crash gates before recording the final generic resident-pipeline decline. This
keeps the selected-plan policy strict while making the benchmark/report gates
show the actionable reason a native PostgreSQL plan won.

The H3 protection suite now primes the same backend-local resident H3 grouped
cache that the benchmark runner loads before timing, so H3 winner lanes prove
real resident GPU dispatch instead of failing from an unprimed cache fixture.
Plan-shape tests assert that the specific reason was observed in the planner
counter stream rather than depending on whichever decline happened to be the
last global reason. Local PG18 verification after reinstall:
`cargo fmt --all`, `cargo check -p pg_accel --features pg18 --all-targets`,
`cargo check -p pg_accel_bench --features integration_tests`,
`just install-pg-accel 18`, `cargo test -p pg_accel_bench --features
integration_tests h3_protection_test -- --nocapture --test-threads=1` (17
passed), `cargo test -p pg_accel_bench --features integration_tests
plan_shape_ -- --nocapture --test-threads=1` (22 passed, 1 ignored), and
`cargo test -p pg_accel_bench --features integration_tests -- --nocapture
--test-threads=1` (337 passed, 1 ignored).

Progress (2026-06-28 H3 ResidentGroupAgg consolidation): grouped H3 count is
now treated as a specialization of the shared resident grouped-aggregate
operator class instead of a standalone variable-output island. The selected H3
grouped-count path reports `GPU Resident Operator Class: resident_groupagg`,
keeps the `h3` stage in the resident proof stage mask, and EXPLAIN emits shared
groupagg logical evidence: `GPU Resident GroupAgg Key: h3index`, `Measure:
count_star`, `Filter: none`, and `Aggregate Mask: 8`. The benchmark ship gate
now requires those groupagg logical fields for H3 grouped winner lanes before a
speedup can count.

The four benchmark-name H3 cache loader functions were removed from the runner
contract and replaced by the generic
`pg_accel_load_resident_h3_groupagg_cache(table, input_col, input_kind,
resolution)` entrypoint. The H3 protection matrix now loads a resident cache
for an arbitrary temp table and resolutions 0/7/9/15, requires a selected
resident `GpuAgg` plan, proves nonzero GPU kernel counters, and diffs exactly
against stock h3-pg output. Local PG18 verification after refreshing the
extension SQL: `cargo fmt --all`, `cargo check -p pg_accel --features pg18
--all-targets`, `cargo check -p pg_accel_bench --features integration_tests`,
`just install-pg-accel 18`, `DROP EXTENSION IF EXISTS pg_accel CASCADE; CREATE
EXTENSION pg_accel;`, `cargo test -p pg_accel --lib --features pg18 --
--nocapture --test-threads=1` (292 passed), `cargo test -p pg_accel_bench
--features integration_tests h3_protection_test -- --nocapture
--test-threads=1` (17 passed), and `cargo test -p pg_accel_bench --features
integration_tests -- --nocapture --test-threads=1` (337 passed, 1 ignored).

Progress (2026-06-28 SSBM star-schema ResidentGroupAgg consolidation): SSBM
Q1/Q2/Q3/Q4 selected plans now expose the same shared grouped-aggregate proof
contract as dense groupagg and H3. EXPLAIN emits `GPU Resident GroupAgg` logical
fields for the star-schema variants: Q1 uses `single_group`,
`ssbm_discounted_revenue`, and `ssbm_date_fact_predicate`; Q2 uses
`ssbm_year_brand`, `ssbm_revenue_column`, and `ssbm_star_join_membership`; Q3
uses `ssbm_customer_supplier_year`, `ssbm_revenue_column`, and
`ssbm_star_join_membership`; Q4 uses `ssbm_year_geo_part`,
`ssbm_profit_revenue_minus_supplycost`, and `ssbm_star_join_membership`. The
SSBM planner hook now routes Q1-Q4 through one resident SSBM CustomPath/proof
builder while preserving the existing specialized kernels as ABI
specializations under `ResidentGroupAgg`.

The benchmark threshold matrix now registers all canonical SSBM Q1/Q2/Q3/Q4
workloads as GPU-winner OLAP lanes instead of only Q1, labels their summary
kernel class as `resident_star_groupagg`, and requires `resident_groupagg`
logical proof before a SSBM winner can pass the ship gate or render as a
threshold-matrix pass. Local PG18 verification after reinstall and refreshing
the extension SQL: Q4.3 EXPLAIN showed `GPU Resident GroupAgg Key:
ssbm_year_geo_part`, `Measure: ssbm_profit_revenue_minus_supplycost`, `Filter:
ssbm_star_join_membership`, and `Aggregate Mask: 1`; Q1.1 EXPLAIN showed
`Key: single_group`, `Measure: ssbm_discounted_revenue`, `Filter:
ssbm_date_fact_predicate`, and `Aggregate Mask: 1`. Verification:
`cargo fmt --all`, `cargo check -p pg_accel --features pg18 --all-targets`,
`cargo check -p pg_accel_bench --features integration_tests`, `just
install-pg-accel 18`, `DROP EXTENSION IF EXISTS pg_accel CASCADE; CREATE
EXTENSION pg_accel;`, `cargo test -p pg_accel_bench --features
integration_tests -- --nocapture --test-threads=1` (340 passed, 1 ignored),
`cargo test -p pg_accel --lib --features pg18 -- --nocapture
--test-threads=1` (293 passed), and single-cell SSBM Q4.3 crash-repro at 10K
with 5 warmups / 10 timed warm iterations: selected resident CustomScan,
correctness artifact, kernel delta 10, stock fallback 0, threshold-matrix
status `pass`, median speedup 5.15x vs forced PG parallel. Artifact root:
`benchmarks/artifacts/crash-repro-1782714565`.

Progress (2026-06-29 SSBM resident dimension-cache consolidation): SSBM Q2,
Q3, and Q4 no longer hand-build separate dimension membership and group-code
maps. They now compile their star-dimension filters through one reusable
resident builder with three modes: match-only dimensions, sorted-label grouped
dimensions, and fixed-code grouped dimensions for synthetic or unused grouping
sides. The shared builder preserves the old kernel contract: negative and
out-of-range keys are ignored, labels are sorted/deduped before dense code
assignment, grouped keys default to `-1`, matching fixed-code rows use group
code `0`, Q3.1 uses nation labels while Q3.2-Q3.4 use city labels, and Q4 keeps
customer-nation, supplier nation/city, category, brand, and synthetic part
label behavior behind the same helper. Q2/Q3/Q4 still dispatch the existing
specialized C++ kernels, but Rust cache construction now routes through a
generic star-dimension descriptor pattern that can be reused by future retained
dimension joins.

Verification after reinstalling the PG18 extension: `cargo fmt --all`, `cargo
test -p pg_accel --lib --features pg18 olap_cache -- --nocapture` (8 passed),
`cargo test -p pg_accel --lib --features pg18 -- --nocapture --test-threads=1`
(298 passed), `cargo check -p pg_accel_bench --features integration_tests`,
`just install-pg-accel 18`, `cargo test -p pg_accel_bench --features
integration_tests -- --nocapture --test-threads=1` (340 passed, 1 ignored),
`cargo fmt --all --check`, `git diff --check`, and live crash-repro cells with
correctness diffs, plan capture, resident proof, kernel deltas, and stock
fallback = 0:
`ssbm_q2_3` 4.16x at 10K
(`benchmarks/artifacts/ssbm-q2-cache-generic-20260629`), `ssbm_q3_4` 4.01x at
10K (`benchmarks/artifacts/ssbm-q3-cache-generic-20260629`), and `ssbm_q4_3`
4.94x at 10K (`benchmarks/artifacts/ssbm-q4-cache-generic-20260629`).

Progress (2026-06-18 resident grouped aggregate engine): canonical SQL for the
hashagg sweep, original grouped aggregate, high-card grouped aggregate,
filtered grouped aggregate, and medium-cardinality grouped aggregate now
selects a childless resident `GpuAgg` CustomScan when the backend-local
resident cache is loaded. The implementation owns resident benchmark table
caches for group/value/filter columns, carries a `ResidentDenseGroupedF64`
OLAP spec through CustomScan private data, dispatches from resident buffers
only, refuses CPU fallback on missing cache or kernel failure, and materializes
only grouped final output. `filtered_grouped_agg` now recognizes PostgreSQL's
singleton-list `WHERE active` qual and exact positive boolean forms (`active`,
`active IS TRUE`, `active = true`) without accepting false/negated predicates.
The high-cardinality dense kernel now uses a tiled local-memory reducer for
short high-cardinality batches instead of a count-plus-collision-rescan path;
this cuts high-card server dispatch from about 6.6 ms to about 5.2 ms in
captured plans on the 10K/10K group lane. The benchmark harness preloads the
right resident cache for every grouped aggregate workload, rounds resident
grouped f64 SUM correctness projections to 3 decimal places and AVG/MIN/MAX
to 5 decimal places for reduction-order drift, and tracks the whole family in
the threshold matrix as resident GPU winners.

Live PG18 smoke evidence, all with selected CustomScan, `GPU Resident Pipeline:
true`, correctness diffs, nonzero kernel deltas, stock fallback = 0, and
final-only output: `hashagg_10g @ 10K` 2.38x
(`benchmarks/artifacts/crash-repro-1781854242`), `hashagg_100g @ 10K` 1.23x
(`benchmarks/artifacts/crash-repro-1781854263`), `hashagg_1kg @ 10K` 1.18x
(`benchmarks/artifacts/crash-repro-1781854270`), and `filtered_grouped_agg @
10K` 1.28x (`benchmarks/artifacts/crash-repro-1781854221`). The
output-heavy high-cardinality lanes now also pass 10-iteration/5-warmup hard
gates: `hashagg_10kg @ 10K` 1.08x, p=5.362162e-5, d=2.97
(`benchmarks/artifacts/crash-repro-1781887509`) and `gpu_hashagg_med_card @
10K` 1.12x, p=3.193723e-6, d=3.62
(`benchmarks/artifacts/crash-repro-1781887519`). The original grouped
workloads now pass the same resident proof gate: `grouped_agg @ 10K` 4.05x,
p=4.316217e-13, d=25.57 (`benchmarks/artifacts/crash-repro-1782535186`) and
`grouped_agg_high_card @ 10K` 2.13x, p=4.312288e-4, d=1.40
(`benchmarks/artifacts/crash-repro-1782535210`). The same resident grouped
engine now handles dictionary-coded text keys: `dictionary_grouped_agg @ 10K`
wins by 3.27x, p=2.114461e-5, d=3.82
(`benchmarks/artifacts/crash-repro-1782546532`). Local evidence: `cargo check
-p pg_accel`, `cargo check -p pg_accel_bench`, `cargo test -p pg_accel_bench
resident_groupagg -- --nocapture`, `cmake --build pgaccel-kernels/build
--target test_olap_ssbm -j 8`, `ctest --test-dir pgaccel-kernels/build -R
'^test_olap_ssbm$' --output-on-failure`, `just install-pg-accel 18`, SQL smoke
for the new cache loaders, and the crash-repro artifacts above.

Progress (2026-06-28 resident grouped aggregate scaling pass): the
large-row diminishing-return issue was traced to two shared bottlenecks:
per-dispatch GPU partial-buffer allocation in blocked dense group aggregation
and repeated row scans per 16-group tile for low-cardinality SUM/COUNT. The
resident cache now owns reusable row-block partial SUM/MIN/MAX/COUNT buffers
and passes them through a v9 dense-grouped f64 ABI; v8 remains as a
compatibility wrapper with internal allocation. Direct-column no-filter
SUM/AVG/COUNT and SUM/COUNT with up to 128 dense groups now use a reusable
one-pass row-block kernel with 8192-row blocks, eliminating the per-tile row
rescan. The generic filtered/expression path still uses the tiled path, but
benefits from cache-owned partials at large rows. A rejected 32-group MIN/MAX
tile experiment was not kept after PG benchmark failures; the safe fused
MIN/MAX/AVG path stays on the 16-group geometry.

Local PG18 cache-mode-both evidence now shows row scaling in the desired
direction across representative grouped OLAP classes, with correctness diffs,
resident proof, nonzero kernel deltas, and stock fallback = 0:
`timeseries_sensor_rollup @ 100K` 1.89x
(`benchmarks/artifacts/crash-repro-1782665245`) and `@ 1M` 1.99x
(`benchmarks/artifacts/crash-repro-1782665203`); `grouped_agg @ 100K` 2.70x
(`benchmarks/artifacts/crash-repro-1782666303`) and `@ 1M` 3.15x
(`benchmarks/artifacts/crash-repro-1782666336`); `hashagg_100g @ 100K` 2.57x
(`benchmarks/artifacts/crash-repro-1782666355`) and `@ 1M` 2.97x
(`benchmarks/artifacts/crash-repro-1782666374`); `expression_grouped_agg @
100K` 1.26x (`benchmarks/artifacts/crash-repro-1782666399`) and `@ 1M` 1.81x
(`benchmarks/artifacts/crash-repro-1782666419`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`,
`cargo test -p pg_accel_bench
test_correctness_projection_rounds_resident_groupagg_float_lanes -- --nocapture`,
`cmake --build pgaccel-kernels/build --target test_olap_ssbm -j 8`,
`ctest --test-dir pgaccel-kernels/build -R '^test_olap_ssbm$'
--output-on-failure`, and `just install-pg-accel 18`.

Progress (2026-06-28 256-group resident SUM/COUNT one-pass expansion): the
one-pass dense row-block reducer now has two execution shapes. Direct-column
no-filter SUM/COUNT keeps the stripped-down simple row loop. Generic
expression, predicate, WHERE, and aggregate-FILTER SUM/COUNT uses the same
resident semantics as the tiled path, but runs as 128-group chunks over
8192-row blocks and supports up to 256 dense groups without changing the v9
ABI. Rust cache sizing now treats <=256 groups as cache-owned partial-scratch
eligible. Native C++ coverage forces the 256-group one-pass path with
`SUM(lhs * rhs)`, RHS range predicates, aggregate-filter semantics, nulls, and
resident partial scratch.

Local PG18 focused ladder evidence, all with correctness diffs, resident proof,
nonzero kernel deltas, and stock fallback = 0:
`grouped_agg` 2.60x @ 100K and 3.18x @ 1M
(`benchmarks/artifacts/olap-onepass256-tiled-20260628-103649`);
`hashagg_100g` 2.51x @ 100K and 3.61x @ 1M (same artifact root);
`expression_grouped_agg` 1.76x @ 100K and 1.83x @ 1M (same root);
`predicate_filter_expression_grouped_agg` 3.26x @ 100K and 2.38x @ 1M (same
root); `case_when_expression_grouped_agg` 2.77x @ 100K and 2.64x @ 1M (same
root); `filtered_grouped_agg` 1.85x @ 100K and 1.88x @ 1M
(`benchmarks/artifacts/olap-more-sentinels-20260628-103842`);
`dictionary_grouped_agg` 2.53x @ 100K and 3.35x @ 1M (same root);
`timeseries_sensor_rollup` 1.89x @ 100K and 1.92x @ 1M (same root).
Verification: `cargo fmt --all`, `cargo check -p pg_accel`, `cargo check -p
pg_accel_bench`, `cmake --build pgaccel-kernels/build --target test_olap_ssbm
-j 8`, `ctest --test-dir pgaccel-kernels/build -R '^test_olap_ssbm$'
--output-on-failure`, `just install-pg-accel 18`, and the crash-repro ladders
above.

Follow-up rejected one-scan experiments (2026-06-28): the obvious local
group-slot reducer is blocked on this Metal/AdaptiveCpp backend because local
f64 atomic add lowers to missing `__acpp_sscp_atomic_fetch_add_f64`. A
32-lane full-width 256-group local stripe compiled but failed the native
selected/count/sum checks, consistent with excessive threadgroup local memory.
A bounded 20-lane stripe then hit Metal resource-location assignment failure
(`id 19, minimum 20`) in the generated argument block. Even a reduced 8-lane
full-width stripe for the simple direct-column SUM/COUNT branch hit the same
resource-location failure and caused other v9 dense-grouped kernels in the
native test process to fail at JIT time. Do not keep any of those shapes in
the overloaded v9 production path; the safe 128-group tiled one-pass kernel
remains the supported generic/filter/expression 129-256 group path. Rollback
verification: `cmake --build
pgaccel-kernels/build --target test_olap_ssbm -j 8` and `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure` pass on the
restored tiled implementation.

Progress (2026-06-28 narrow direct SUM/COUNT 256-group ABI): the direct-column
no-filter SUM/COUNT lane now has a separate resident dense grouped f64 C ABI
instead of another branch inside v9. The narrow ABI carries only group/value
columns, final SUM/COUNT scratch, cache-owned row-block partial SUM/COUNT
scratch, output buffers, and counts; Metal therefore sees a much smaller
argument block and accepts an 8-lane full-width 256-group one-scan row-block
reducer. The executor routes only exact direct-column SUM/COUNT-compatible
shapes through it: column measure, no RHS, no filter, BOOL_ONLY predicate,
SUM/COUNT or SUM/AVG/COUNT layout, 129-256 dense groups, and rows >= 65,536.
Generic expression/FILTER/CASE lanes stay on the safe v9 128-group tiled path.

The new canonical benchmark lane `hashagg_256g` fills the previous gap between
`hashagg_100g` and `hashagg_1kg`. Local PG18 warm crash-repro evidence, all
with correctness diffs, resident proof, nonzero kernel deltas, stock fallback =
0, and selected `Custom Scan`: 1.20x @ 100K
(`benchmarks/artifacts/hashagg256-simplewide-100k-20260628`), 1.86x @ 1M
(`benchmarks/artifacts/hashagg256-simplewide-1m-20260628`), and 3.30x @ 10M
(`benchmarks/artifacts/hashagg256-simplewide-10m-20260628`). This restores the
desired "more rows = faster" slope for the 256-group direct SUM/COUNT problem
class. Verification: `cargo fmt --all`, `cargo check -p pg_accel`, `cargo
check -p pg_accel_bench`, `cargo test -p pg_accel_bench resident_groupagg --
--nocapture`, `cargo test -p pg_accel_bench threshold_matrix -- --nocapture`,
`cmake --build pgaccel-kernels/build --target test_olap_ssbm -j 8`, `ctest
--test-dir pgaccel-kernels/build -R '^test_olap_ssbm$'
--output-on-failure`, `just install-pg-accel 18`, and the three `hashagg_256g`
crash-repro artifacts above.

Next broad grouped-OLAP hunk: remove the remaining 129-256 group read
amplification for generic expression/FILTER/CASE lanes without local f64
atomics or oversized local memory. The likely architecture is a reusable
resident row-selection/measure-evaluation stage that compacts qualifying rows
once per row block into device scratch, followed by group-local reductions over
the compacted row list, or additional narrow specialized ABIs that split
expression/filter evaluation from aggregation to reduce captured kernel
argument pressure. Rerun the expression/FILTER/CASE resident grouped ladder at
100K/1M/10M and require row-count scaling to be nondecreasing except where PG's
own selectivity scaling makes the comparison structurally noisy.

Progress (2026-06-27 resident time-series MIN/MAX/AVG): the dense resident
grouped aggregate path now owns `MIN(float8)`, `MAX(float8)`, and `AVG(float8)`
lanes in the same reusable scratch/state layout as SUM/COUNT. The new canonical
SQL workload `timeseries_sensor_rollup` loads `sensor_data(sensor_id, value)`
into the backend-local resident cache, selects a childless resident `GpuAgg`
CustomScan for `SELECT sensor_id, min(value), max(value), avg(value) FROM
sensor_data GROUP BY sensor_id`, keeps the scan/group/reduction GPU-resident,
and materializes only the final grouped output. Local PG18 smoke returned
correct rows through `Custom Scan (GpuAccelAgg)` with `GPU Resident Pipeline:
true`; the crash-repro benchmark passed correctness and beat forced PG parallel
at 10K by 1.88x median, p=2.497457e-4, d=2.32
(`benchmarks/artifacts/crash-repro-1782545384`). The local PG bench blocker was
environmental, not a resident execution bug: AdaptiveCpp's Metal JIT needed the
Metal Toolchain component, fixed locally by
`xcodebuild -downloadComponent MetalToolchain`.

Progress (2026-06-27 resident dictionary GROUP BY): resident grouped aggregate
caches now lower non-dense group domains into GPU-resident int4 codes while
preserving the original key domain for final-only materialization. Dense int4
keys still use their natural group range; sparse int4 keys and text labels use
sorted dictionary metadata. The canonical SQL workload
`dictionary_grouped_agg` loads `bench_dictionary_sales(region, amount)` into
the backend-local resident cache, dispatches the existing grouped f64 SUM/COUNT
kernel over dictionary codes, emits the original text `region` labels at the
bounded output boundary, and keeps the scan/group/reduction GPU-resident. Direct
PG18 smoke selected `Custom Scan (GpuAccelAgg)` with `GPU Resident Pipeline:
true` and returned ordered `region_###` rows. The 10-iteration/5-warmup
crash-repro benchmark passed correctness and beat forced PG parallel at 10K by
3.27x median, p=2.114461e-5, d=3.82
(`benchmarks/artifacts/crash-repro-1782546532`).

Progress (2026-06-27 resident H3 grouped rollups): canonical grouped H3 SQL now
selects childless resident `GpuAgg` plans for lat/lng-to-cell grouped count at
resolutions 7, 9, and 15, plus cell-to-parent grouped count. The planner admits
only when the backend-local resident H3 cache matches the relation, input kind,
and resolution; EXPLAIN reports `GPU Resident Pipeline: true`; execution keeps
the timed scan/group/reduction resident and materializes only the bounded H3
grouped output. The lat/lng lanes cache exact h3-pg cell keys at load time,
sort the device-resident key column once, and run a GPU sorted compact-count at
query time; the parent lane uses the resident parent-key grouped-count path.
The benchmark harness now reloads resident caches after per-measurement
`DISCARD ALL`, so cache loads stay outside warm timed queries. Local PG18
cache-mode-both artifacts passed correctness, resident proof, nonzero kernel
deltas, stock fallback = 0, and forced PG parallel speedup gates: `h3_bulk @
100K` 1.51x (`benchmarks/artifacts/crash-repro-1782549984`),
`h3_resolution_sweep @ 100K` 3.31x
(`benchmarks/artifacts/crash-repro-1782550002`), `h3_latlng_res15 @ 100K`
1.55x (`benchmarks/artifacts/crash-repro-1782550014`), and
`h3_cell_to_parent @ 100K` 3.38x
(`benchmarks/artifacts/crash-repro-1782550031`).

Progress (2026-06-27 resident expression aggregate measures): the reusable
resident dense grouped aggregate engine now supports expression-defined f64
measure lanes for `SUM(a*b)` and `SUM(a-b)` instead of only direct
`SUM(value)` columns. The cache layer can load a second resident measure column,
the planner recognizes matching aggregate expression trees, and the kernel
evaluates the arithmetic measure inside the grouped reducer without
materializing rows through PostgreSQL. The new canonical SQL workload
`expression_grouped_agg` runs
`SELECT product_id, SUM(price * discount), COUNT(*) FROM bench_expression_sales
GROUP BY product_id` through a childless resident `GpuAgg` CustomScan.

The resident grouped kernel now also receives a layout-derived aggregate-lane
mask. `SUM/COUNT` and `SUM/AVG/COUNT` layouts dispatch a no-MIN/MAX fast path,
and the low-cardinality dense SUM/COUNT path uses tiled local-memory reduction
so a 256-group workload scans row data by group tiles instead of once per
group. Local PG18 cache-mode-both evidence passed correctness, resident proof,
nonzero kernel deltas, stock fallback = 0, and forced PG parallel speedup:
`expression_grouped_agg @ 100K` 1.25x, p=4.220905e-4, d=1.30
(`benchmarks/artifacts/crash-repro-1782578079`). Verification:
`cargo fmt`, `cargo check -p pg_accel`, `cargo check -p pg_accel_bench`,
`cargo test -p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test
-p pg_accel --lib resident_groupagg -- --nocapture`, `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `cmake --build
pgaccel-kernels/build --target test_olap_ssbm -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, and
`just install-pg-accel 18`.

Progress (2026-06-27 resident aggregate FILTER expression measures):
aggregate-level predicate filters now lower into the same resident expression
measure/grouped aggregate path. The planner distinguishes unfiltered,
base-`WHERE`, and per-aggregate `FILTER` semantics, requires every admitted
aggregate filter to match the same positive resident bool column, and keeps
rejecting `DISTINCT`, ordered aggregates, mixed filters, and unrelated quals.
The resident OLAP spec carries filter mode through CustomScan private data, and
the executor emits PostgreSQL-correct aggregate-FILTER zero-count groups
(`SUM/AVG/MIN/MAX = NULL`, `COUNT = 0`) instead of treating them as
`WHERE`-filtered groups. The custom path-to-plan OLAP trailer length was
updated for the new filter-mode field; the old length corrupted the copied
private-data trailer and crashed during execution.

The new canonical SQL workload `predicate_filter_expression_grouped_agg` runs
`SELECT product_id, SUM(price * discount) FILTER (WHERE active), COUNT(*)
FILTER (WHERE active) FROM bench_predicate_expression_sales GROUP BY
product_id` through a childless resident `GpuAgg` CustomScan with four resident
device columns (`product_id`, `price`, `discount`, `active`). Local PG18
cache-mode-both evidence passed correctness, resident proof, nonzero kernel
deltas, stock fallback = 0, and forced PG parallel speedup:
`predicate_filter_expression_grouped_agg @ 100K` 2.03x, p=7.953411e-13,
d=4.84 (`benchmarks/artifacts/crash-repro-1782579033`). Manual semantic smoke
also matched PostgreSQL for an inactive-only product group emitting `SUM =
NULL`, `COUNT = 0`. Verification after the final fix: `cargo fmt`, `cargo
check -p pg_accel`, `cargo check -p pg_accel_bench`, `cargo test -p
pg_accel_bench resident_groupagg -- --nocapture`, `cargo test -p pg_accel
--lib resident_groupagg -- --nocapture`, `just install-pg-accel 18`, and the
manual PG18 smoke above.

Progress (2026-06-27 resident CASE conditional expression measures):
conditional aggregate measures now lower into the reusable resident dense
grouped aggregate engine without reusing aggregate-FILTER semantics. The
kernel ABI added a v5 filter mode: existing callers keep row-filter semantics,
while CASE-mode gates only the measure lanes and keeps `COUNT(*)` on the full
grouped row domain. The planner now recognizes the canonical searched CASE
shape `SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*)` over
resident expression-measure caches, requiring a single positive bool predicate,
`ELSE 0`, no aggregate `FILTER`, no query quals, and an unfiltered count. The
new canonical SQL workload `case_when_expression_grouped_agg` runs
`SELECT product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END),
COUNT(*) FROM bench_case_when_expression_sales GROUP BY product_id` through a
childless resident `GpuAgg` CustomScan with four resident device columns
(`product_id`, `price`, `discount`, `active`).

Manual PG18 smoke selected `Custom Scan (GpuAccelAgg)` with `GPU Resident
Pipeline: true` and matched PostgreSQL on the critical inactive-only group
case (`SUM = 0`, `COUNT(*) = all rows`) rather than aggregate-FILTER's `SUM =
NULL`, `COUNT = 0` semantics. Local cache-mode-both evidence passed
correctness, resident proof, nonzero kernel deltas, stock fallback = 0, and
forced PG parallel speedup: `case_when_expression_grouped_agg @ 100K` 4.39x,
p=6.135617e-12, d=2.76
(`benchmarks/artifacts/crash-repro-1782580182`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `cmake --build
pgaccel-kernels/build --target test_olap_ssbm -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, `cargo test
-p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test -p
pg_accel_bench test_threshold_matrix_expected_names_are_registered --
--nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `cargo test
-p pg_accel --lib resident_groupagg -- --nocapture`, `just install-pg-accel
18`, and the manual PG18 smoke above.

The same conditional-measure path now supports a compact resident
measure-predicate descriptor for canonical boolean/range CASE predicates. The
v6 dense grouped f64 kernel ABI carries predicate op plus bounds, so
`SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price *
discount ELSE 0 END), COUNT(*)` runs as one resident grouped kernel while
`COUNT(*)` still covers the full grouped row domain. The CustomScan private
OLAP spec now also carries planned `measure_op` and rhs requirements, and
execution revalidates the backend-local resident cache shape before dispatch so
stale cached plans cannot run against a mismatched resident column layout.

The new canonical SQL workload `case_when_range_expression_grouped_agg` passed
same-backend PG18 smoke with `Custom Scan (GpuAccelAgg)`, `GPU Resident
Pipeline: true`, one kernel dispatch, zero result diff versus PostgreSQL, and
zero-sum groups still reporting all rows. Cache-mode-both crash-repro evidence
passed correctness, resident proof, kernel delta 20, stock fallback = 0, and
forced PG parallel speedup: `case_when_range_expression_grouped_agg @ 100K`
2.23x, p=5.998618e-14, d=5.39
(`benchmarks/artifacts/crash-repro-1782581865`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, `cargo test
-p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test -p
pg_accel_bench test_threshold_matrix_expected_names_are_registered --
--nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `cargo test
-p pg_accel --lib resident_groupagg -- --nocapture`, `just install-pg-accel
18`, extension refresh, and manual PG18 smoke.

Progress (2026-06-27 resident CASE predicate normalization): conditional
measure predicates now normalize supported SQL boolean trees into a reusable
resident RHS interval-set descriptor instead of matching one hardcoded range.
The planner derives the descriptor from the actual searched `CASE WHEN`
expression, requires the positive `active` guard to dominate all RHS terms, and
normalizes comparison trees by treating `AND` as interval intersection and
`OR` as interval union. Supported RHS forms now include `<`, `<=`, `=`, `>=`,
`>`, flipped constant/column comparisons, PostgreSQL-lowered `BETWEEN`, and
ORs of up to four merged intervals. Open float8 endpoints are represented with
next-up/next-down inclusive bounds so GPU semantics match PostgreSQL boundary
rows. The v7 dense grouped f64 ABI carries fixed interval slots while v6/v5
remain compatibility wrappers.

The new canonical SQL workload `case_when_or_expression_grouped_agg` runs
`SELECT product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount
BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END),
COUNT(*) FROM bench_case_when_or_expression_sales GROUP BY product_id` through
the same childless resident `GpuAgg` CustomScan. Manual PG18 smoke with
boundary discounts selected a resident plan, dispatched one kernel, and
matched PostgreSQL exactly. Cache-mode-both crash-repro evidence passed
correctness, resident proof, kernel delta 20, stock fallback = 0, and forced
PG parallel speedup: `case_when_or_expression_grouped_agg @ 100K` 1.33x,
p=1.179075e-6, d=1.96
(`benchmarks/artifacts/crash-repro-1782623140`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, `cargo test
-p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test -p
pg_accel_bench test_threshold_matrix_expected_names_are_registered --
--nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `just
install-pg-accel 18`, extension refresh, manual PG18 smoke, and the
crash-repro benchmark above.

Progress (2026-06-27 resident CASE IN-list predicates): conditional measure
predicates now normalize equality `ScalarArrayOpExpr` forms into the same
resident RHS interval-set descriptor. The planner accepts `IN (...)`,
`= ANY(ARRAY[...]::numeric[])`, and folded constant numeric arrays such as
`= ANY('{...}'::float8[])` when the expression is equality, OR/ANY semantics,
guarded by the positive `active` boolean, and the merged point intervals fit the
fixed resident descriptor. Array constants are decoded through the existing
PostgreSQL `ArrayType` walker rather than a second ad-hoc parser, and duplicate
constants may collapse before the four-range descriptor limit is enforced.

The new canonical SQL workload `case_when_in_expression_grouped_agg` runs
`SELECT product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25,
0.45) THEN price * discount ELSE 0 END), COUNT(*) FROM
bench_case_when_in_expression_sales GROUP BY product_id` through the childless
resident `GpuAgg` CustomScan. Manual PG18 smoke selected `Custom Scan
(GpuAccelAgg)`, reported `GPU Resident Pipeline: true`, and dispatched one
kernel for both the canonical `IN (...)` predicate and an explicit folded
`= ANY('{0.05,0.15,0.25,0.45}'::float8[])` predicate. Cache-mode-both
crash-repro evidence passed correctness (`accel_minus_baseline_count = 0`,
`baseline_minus_accel_count = 0`), resident proof, kernel delta 20, stock
fallback = 0, and forced PG parallel speedup:
`case_when_in_expression_grouped_agg @ 100K` 1.22x, p=1.549546e-5, d=1.54
(`benchmarks/artifacts/crash-repro-1782624610`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, `cargo test
-p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test -p
pg_accel_bench test_threshold_matrix_expected_names_are_registered --
--nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `cargo test -p
pg_accel --lib resident_groupagg -- --nocapture`, `just install-pg-accel 18`,
manual PG18 smoke, and the crash-repro benchmark above.

Progress (2026-06-27 resident CASE negated predicates): conditional measure
predicates now support negation by complementing the normalized RHS interval
set. The planner accepts `NOT (...)` boolean wrappers, `<>` / `!=` comparisons
against the measure RHS column, PostgreSQL-lowered `NOT BETWEEN`, and `NOT IN`
/ `<> ALL` scalar-array forms when the positive `active` guard remains a
sibling predicate and the complemented interval set fits the fixed four-range
resident descriptor. Resident f64 group-aggregate cache loading now also
rejects NaN measure/RHS values so interval comparisons do not claim PostgreSQL
float8 NaN semantics that the kernel does not implement.

The new canonical SQL workload `case_when_not_expression_grouped_agg` runs
`SELECT product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25,
0.35) THEN price * discount ELSE 0 END), COUNT(*) FROM
bench_case_when_not_expression_sales GROUP BY product_id` through the same
childless resident `GpuAgg` CustomScan. Manual PG18 smoke selected resident
plans and dispatched GPU kernels for `NOT IN`, direct `discount <> 0.25`, and
explicit `NOT (discount BETWEEN 0.10 AND 0.35)` spellings. The cache-mode-both
crash-repro plan shows PostgreSQL lowered the canonical query to
`discount <> ALL ('{0.1,0.25,0.35}'::double precision[])`, and evidence passed
correctness (`accel_minus_baseline_count = 0`, `baseline_minus_accel_count =
0`), resident proof, kernel delta 20, stock fallback = 0, and forced PG
parallel speedup: `case_when_not_expression_grouped_agg @ 100K` 1.13x,
p=2.056310e-4, d=1.32
(`benchmarks/artifacts/crash-repro-1782628686`). Verification: `cargo fmt
--check`, `cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cmake
--build pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest
--test-dir pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`,
`cargo test -p pg_accel_bench resident_groupagg -- --nocapture`, `cargo test
-p pg_accel_bench test_threshold_matrix_expected_names_are_registered --
--nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `cargo test -p
pg_accel --lib resident_groupagg -- --nocapture`, `just install-pg-accel 18`,
manual PG18 smoke, and the crash-repro benchmark above.

Progress (2026-06-27 resident CASE source-aware measure predicates): the
conditional measure predicate descriptor now carries the predicate source
through planner normalization, CustomScan private data, Rust dispatch, and the
C++ grouped f64 kernel ABI. The same resident expression-measure engine can now
predicate CASE measures on either the left/value column or the RHS expression
operand without adding query-specific kernels. The planner normalizes
source-consistent comparison, range, IN/ANY, OR, and NOT trees, rejects mixed
source interval sets, and the executor/kernel only require resident RHS buffers
when the predicate source actually uses them.

The new canonical SQL workload `case_when_value_predicate_expression_grouped_agg`
runs `SELECT product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price *
discount ELSE 0 END), COUNT(*) FROM
bench_case_when_value_predicate_expression_sales GROUP BY product_id` through
the childless resident `GpuAgg` CustomScan. A local PG18 smoke after
`pg_accel_stats()` priming selected `Custom Scan (GpuAccelAgg)`, reported
`GPU Resident Pipeline: true`, dispatched the grouped kernel over 100K resident
rows, and returned no scan rows to PostgreSQL. Cache-mode-both crash-repro
evidence passed correctness, resident proof, kernel delta 20, stock fallback =
0, and forced PG parallel speedup:
`case_when_value_predicate_expression_grouped_agg @ 100K` 2.07x,
p=2.371229e-15, d=5.68
(`benchmarks/artifacts/crash-repro-1782631408`) and `@ 1M` 1.44x,
p=3.449721e-15, d=6.52
(`benchmarks/artifacts/crash-repro-1782631462`). The local PG bench crash path
was fixed by adding a backend-local planner-hook suspension guard for resident
cache SPI reads, so loader scans for grouped, H3, and SSBM resident caches
cannot self-intercept after `pg_accel_stats()` initializes backend state.
Verification: `cargo fmt`, `cargo check -p pg_accel`, `cargo check -p
pg_accel_bench`, `cargo test -p pg_accel --lib private_data -- --nocapture`,
`cargo test -p pg_accel_bench resident_groupagg -- --nocapture`, `cmake
--build pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest
--test-dir pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`,
`just install-pg-accel 18`, manual PG18 smoke, and the crash-repro benchmarks
above.

Progress (2026-06-28 resident nullable CASE measure predicates): resident
dense grouped f64 caches can now opt in to nullable value/RHS measure columns
and carry device-resident null masks through the existing `PgaccelExprUsmCol`
ABI. Existing grouped aggregate lanes keep the old non-null cache contract, so
nullable measure loading cannot silently change legacy `COUNT(*)` semantics.
The CASE predicate normalizer now accepts source-consistent `IS NOT NULL`
`NullTest` nodes by lowering them to the same resident source interval-domain
machinery used by comparison/range predicates; NULL source rows fail the
measure predicate on GPU while `COUNT(*)` still covers the full grouped row
domain in CASE measure-filter mode.

The new canonical SQL workload
`case_when_null_predicate_expression_grouped_agg` runs `SELECT product_id,
SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price *
discount ELSE 0 END), COUNT(*) FROM
bench_case_when_null_predicate_expression_sales GROUP BY product_id` over a
nullable `price` column through the childless resident `GpuAgg` CustomScan. A
manual PG18 smoke after `pg_accel_stats()` priming loaded 100K nullable rows,
selected `Custom Scan (GpuAccelAgg)`, reported `GPU Resident Pipeline: true`,
and dispatched the grouped kernel with zero scan rows returned to PostgreSQL.
Cache-mode-both crash-repro evidence passed correctness, resident proof, kernel
delta 20, stock fallback = 0, and forced PG parallel speedup:
`case_when_null_predicate_expression_grouped_agg @ 100K` 2.02x,
p=1.590312e-18, d=8.65
(`benchmarks/artifacts/crash-repro-1782632072`) and `@ 1M` 1.37x,
p=5.868807e-10, d=4.08
(`benchmarks/artifacts/crash-repro-1782632085`). Verification: `cargo fmt`,
`cargo check -p pg_accel`, `cargo check -p pg_accel_bench`, `cargo test -p
pg_accel --lib private_data -- --nocapture`, `cargo test -p pg_accel_bench
resident_groupagg -- --nocapture`, `cargo test -p pg_accel_bench
test_classify_kernel_separates_h3_parent_count -- --nocapture`, `cmake
--build pgaccel-kernels/build --target pgaccel_kernels -j 8`, `ctest
--test-dir pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`,
`just install-pg-accel 18`, manual PG18 smoke, and the crash-repro benchmarks
above.

Progress (2026-06-28 resident dense SUM/COUNT row-block aggregation): the
bad 100K-to-1M scaling curve was architectural, not planner noise. The
SUM/COUNT-only low-cardinality dense grouped path had only one workgroup per
group tile and scanned the full row domain per tile, so 256 dense groups meant
16 serial-ish full-table passes with very little GPU occupancy. A wider tile
alone reduced passes but also reduced workgroups, so it did not fix 1M. The
large-row path now uses a two-stage row-block aggregate: many workgroups scan
row blocks by group tile into device partial `(block, group)` sums/counts, then
a compact per-group reducer folds those partials into the existing resident
output scratch. This avoids fp64 atomics, which Metal reports unsupported, and
keeps the old low-row tile path unchanged.

Local C++ coverage now forces this branch with a 270K-row v8 dense f64
SUM/COUNT test that checks nullable measure predicates, expression measures,
and CASE measure-only `COUNT(*)` semantics against a CPU reference. PG18
cache-mode-both crash-repro artifacts now show larger row counts getting
relatively faster for the affected CASE predicate lanes: nullable value
predicate `case_when_null_predicate_expression_grouped_agg` is 2.12x at 100K
(`benchmarks/artifacts/crash-repro-1782633346`) and 3.08x at 1M
(`benchmarks/artifacts/crash-repro-1782633299`); non-null value predicate
`case_when_value_predicate_expression_grouped_agg` is 2.19x at 100K
(`benchmarks/artifacts/crash-repro-1782633401`) and 2.84x at 1M
(`benchmarks/artifacts/crash-repro-1782633369`). Each artifact passed
correctness, resident proof, nonzero kernel deltas, stock fallback = 0, and
forced PostgreSQL parallel speedup gates. Verification: `cmake --build
pgaccel-kernels/build --target pgaccel_kernels -j 8`, `cmake --build
pgaccel-kernels/build --target test_olap_ssbm -j 8`, `ctest --test-dir
pgaccel-kernels/build -R '^test_olap_ssbm$' --output-on-failure`, `just
install-pg-accel 18`, and the crash-repro benchmarks above.

Progress (2026-06-28 PG18-only pgrx baseline and expression SUM/COUNT ABI):
the local extension toolchain is now PG18-first instead of trying to preserve a
PG17 install path. `pg_accel/Cargo.toml` defaults to `pg18`, depends on
`pgrx`/`pgrx-tests` `0.18.1`, drops the stale `pgrx_embed_pg_accel` bin target,
and `scripts/pg_versions.sh` defaults supported pgrx builds to PG18 with PG19
kept as source-smoke preview until pgrx exposes a real `pg19` feature. The
active install path is the local PG18.4 pgrx cluster on port 28818.

The resident dense grouped expression path now has a direct 129-256 group
multiply SUM/COUNT ABI for `SUM(lhs * rhs)` plus `COUNT(*)`, with filter modes
for no filter, aggregate `FILTER (WHERE bool_col)`, and CASE measure predicates
where the predicate gates the SUM but `COUNT(*)` still covers all grouped rows.
Planner admission is deliberately broad for this OLAP class but still guarded:
float8 multiplication measures, optional bool filter/predicate columns,
SUM/COUNT-compatible layouts, 129-256 dense groups, and 1M+ rows use the direct
ABI; smaller rows keep the existing resident tiled path because the first 100K
wide-kernel trial was only 1.11x and not statistically useful.

Local PG18 verification: `PG_CONFIG=.pgaccel/postgres/install/18.4/bin/pg_config
PG_ACCEL_PG_MAJOR=18 cargo check -p pg_accel --lib --no-default-features
--features pg18`, `cargo check -p pg_accel_bench`, and `just install-pg-accel
18` all pass. Live warm crash-repro artifacts, all with correctness diffs,
resident proof, nonzero kernel deltas, stock fallback = 0, and
`server_version = 18.4`: `expression_grouped_agg` is 2.17x at 100K through the
gated resident path (`benchmarks/artifacts/expression-mulwide-gated-100k-20260628`),
1.90x at 1M through the multiply ABI
(`benchmarks/artifacts/expression-mulwide-gated-1m-20260628`), and 2.91x at
10M (`benchmarks/artifacts/expression-mulwide-10m-20260628`);
`predicate_filter_expression_grouped_agg` is 2.38x at 1M
(`benchmarks/artifacts/predicate-filter-mulwide-1m-20260628`) and 4.47x at 10M
(`benchmarks/artifacts/predicate-filter-mulwide-10m-20260628`);
`case_when_expression_grouped_agg` is 2.71x at 1M
(`benchmarks/artifacts/case-when-mulwide-1m-20260628`) and 5.00x at 10M
(`benchmarks/artifacts/case-when-mulwide-10m-20260628`). This closes the
plain/FILTER/CASE bool-expression SUM/COUNT lane for 256 dense groups with the
desired larger-row speedup curve; remaining work is the broader predicate
expression language, dictionary/text group keys, and PG-Strom sidecar evidence.

Next resident grouped-aggregate hunk: generic consolidation before more
specialization.

Progress (2026-06-28 generic operator-class proof gate): resident pipeline
proofs now carry a stable reusable `ResidentOperatorClass` in addition to stage
masks. Version-2 CustomScan proof trailers serialize the class, EXPLAIN emits
`GPU Resident Operator Class`, and benchmark/report audits require a
non-`unspecified` class before `GPU Resident Pipeline: true` counts as resident
proof. Existing resident dense groupagg, SSBM revenue/profit paths, and
resident H3 grouped rollups classify as `resident_groupagg`; H3 still reports
its `h3` stage inside the same grouped-aggregate operator proof. Scan/expression
batch-fabric tests classify as `resident_expression` or `resident_source`. This
makes duplicate-path consolidation enforceable in artifacts instead of only
documented in TODO.
Verification: `PG_CONFIG=.pgaccel/postgres/install/18.4/bin/pg_config
PG_ACCEL_PG_MAJOR=18 cargo check -p pg_accel --lib --no-default-features
--features pg18`, `cargo check -p pg_accel_bench`, resident proof trailer
unit tests, resident boundary audit tests, GPU-resident proof report tests,
resident groupagg harness tests, artifact audit generation test, and
`scripts/pg_version_audit.sh`.

Progress (2026-06-28 resident groupagg logical spec): resident dense grouped
f64 plans now carry a reusable `ResidentGroupAggLogicalSpec` alongside the
specialized dense-kernel ABI. The descriptor names resident i32 group keys,
direct-column and binary arithmetic measures, WHERE/FILTER/CASE boolean
filters, value/RHS range CASE predicates, and aggregate lane masks. CustomScan
OLAP private data writes this as v8, while legacy v5/v7 dense grouped payloads
decode by inferring the same logical descriptor from their ABI fields. The
planner recognizer now emits the logical spec at admission time instead of
leaving measure/filter semantics implicit in table-specific code. EXPLAIN now
emits `GPU Resident GroupAgg Key`, `Measure`, `Filter`, and `Aggregate Mask`,
and the benchmark ship gate requires dense/dictionary resident groupagg winner
artifacts to carry those logical fields before speedups count. The pgrx
integration tests now reflect the resident-only admission policy: native
nonresident candidates assert `no_gpu_resident_pipeline`, and plain reductions
assert native execution until they have a proven resident producer. Benchmark:
`grouped_agg` on local PG18 passed at 3.08x (10K), 2.49x (100K), 3.39x (1M),
and 5.54x (10M) with artifact proof in
`benchmarks/artifacts/groupagg-logical-spec-20260628160817`. Verification:
`just test`, `PG_CONFIG=.pgaccel/postgres/install/18.4/bin/pg_config
PG_ACCEL_PG_MAJOR=18 cargo test -p pg_accel --lib --no-default-features
--features pg18 -- --nocapture`, `cargo test -p pg_accel_bench -- --nocapture`,
v8/v7 resident dense grouped private-data unit tests, resident groupagg EXPLAIN
label tests, benchmark ship-gate logical-spec tests, resident groupagg benchmark
harness tests, release-audit artifact generation tests, and
`scripts/pg_version_audit.sh`.

1. Extend the reusable resident expression/predicate IR for grouped aggregate
   measures and filters beyond the current descriptor. It must cover full
   expression trees for boolean columns, comparison predicates,
   `AND`/`OR`/`NOT`, `IS NULL`/`IS NOT NULL`, parameter-safe constants, and
   larger or parameterized `IN`/`ANY` sets without adding another ad-hoc parser.
2. Route the remaining grouped-aggregate families through that IR where
   semantics match: SSBM revenue/profit, geo rollups, retained dimension
   filters, remaining H3 SRF/variable-output aggregate consumers, and any
   non-dense/dictionary key sources. The C++ ABI can keep specialized entry
   points, but Rust planner/executor routing should see shared specs and shared
   proof fields.
3. Collapse remaining duplicate state and scratch ownership into
   `ResidentGroupAgg`: SSBM fact/dimension source descriptors, H3/geo key
   producers, predicate/filter masks, aggregate lane descriptors, and
   final-output materialization must be reusable across grouped workloads.
4. Add duplicate-path audit coverage. The benchmark/report layer should fail or
   warn when a new resident grouped workload bypasses the shared expression IR,
   creates a private cache/source shape without a generic extraction plan, or
   cannot report its operator class.
5. Lift SSBM Q1/Q2/Q3/Q4 revenue/profit paths onto the generic expression
   measure layer while preserving their benchmark wins, then delete or demote
   query-family-specific planner/executor surfaces that the generic path
   replaces.
6. Run full publication ladders across 10K/100K/1M/10M and cache-mode-both
   artifacts, with output-emitter profiling for result-heavy grouped lanes and
   PG-Strom sidecar artifacts generated from already-passing resident reports.

Resident grouped-aggregate engine shape:

1. Resident table/source layer for benchmark OLAP tables, with relation
   identity/snapshot validation and device-owned columns for group keys,
   values, filters, dictionary/group-code metadata, and reusable predicate
   columns.
2. One `ResidentGroupAgg` logical spec that describes grouping, predicate IR,
   measure expressions, aggregate lanes, null policy, output ordering, and
   final materialization independently of the chosen specialized kernel ABI.
3. Device-resident scratch/state reused across queries: dense direct tables,
   dictionary ids for text keys, row-block partial buffers, predicate masks,
   aggregate lane buffers, and a two-phase local-to-global reduction path that
   avoids high-contention global atomics where possible.
4. Specialization boundary: C++/SYCL kernels may specialize for dense 10/100/256
   groups, dictionary keys, row-block reductions, or boolean-filtered measures,
   but those kernels must be selected from the shared `ResidentGroupAgg` spec
   and publish the same correctness/proof counters.
5. Final-only materialization: PostgreSQL receives bounded grouped output after
   all scan/filter/group/aggregate work stays in resident GPU memory.
6. Planner admission only when the exact resident source, expression IR, and
   aggregate shape are proven; otherwise record a native decline instead of
   selecting a host-staged `GpuAgg`.
7. Benchmark/report proof for PG parallel and PG-Strom-class comparisons:
   correctness diff, nonzero kernel deltas, stock fallback zero, resident proof,
   speedup threshold, operator-class classification, and artifacts that can line
   up PostgreSQL parallel, pg_accel, and PG-Strom runs for the same canonical
   SQL.

Progress (2026-06-16 SSBM Q1 resident OLAP path): canonical Q1 planning now
has a real resident-GPU path instead of a recognize-and-decline stub. A
backend-local `SsbmQ1ResidentCache` explicitly loads `ssbm_lineorder`
`lo_orderdate`, `lo_discount`, `lo_quantity`, and `lo_extendedprice` into
device-owned buffers and keeps date dimension metadata for predicate-to-date-key
folding. The cache now owns reusable device scratch reductions, caches
non-contiguous date-key membership buffers, folds contiguous date keys to ranges,
and clears old backend-local cache state before reload. `GpuAgg` has a childless
OLAP submode carried by an append-only `AGG_OLAP` private-data trailer; the
executor dispatches the SSBM Q1 revenue kernel directly from resident buffers
and refuses CPU fallback on missing cache, kernel failure, or uncertain rows.
The planner recognizes canonical Q1 SQL through PG18 hash/merge join clauses and
common wrapper paths, resolves the actual fact/date relation OIDs, injects a
proof-backed resident aggregate path only when the matching cache is loaded, and
otherwise records the precise native decline. The benchmark runner primes every
accel-side Q1 backend with `pg_accel_load_ssbm_q1_cache()` before plan capture,
correctness diff, and timed execution, while keeping that load outside the
measured query loop.

Live PG18 warm evidence now covers every standard Q1 scale with selected
CustomScan, `GPU Resident Pipeline: true`, correctness diffs, kernel deltas,
rows returned to CPU = 3, stock fallback = 0, and threshold-matrix `pass`:
`ssbm_q1_1` wins at 10K 3.68x
(`benchmarks/artifacts/crash-repro-1781676306`), 100K 5.69x
(`benchmarks/artifacts/ssbm-q1-1-resident-100k-20260616232213`), 1M 18.24x
(`benchmarks/artifacts/ssbm-q1-1-resident-1m-20260616230842`), and 10M 77.42x
(`benchmarks/artifacts/ssbm-q1-1-resident-10m-20260616231027`);
`ssbm_q1_2` wins at 10K 3.26x
(`benchmarks/artifacts/ssbm-q1-2-resident-10k-20260616230735`), 100K 7.27x
(`benchmarks/artifacts/ssbm-q1-2-resident-100k-20260616232234`), 1M 21.28x
(`benchmarks/artifacts/ssbm-q1-2-resident-1m-20260616230914`), and 10M 150.37x
(`benchmarks/artifacts/ssbm-q1-2-resident-10m-20260616231303`);
`ssbm_q1_3` wins at 10K 8.20x
(`benchmarks/artifacts/ssbm-q1-3-resident-10k-20260616230749`), 100K 3.92x
(`benchmarks/artifacts/ssbm-q1-3-resident-100k-20260616232257`), 1M 10.39x
(`benchmarks/artifacts/ssbm-q1-3-resident-1m-20260616230941`), and 10M 106.38x
(`benchmarks/artifacts/ssbm-q1-3-resident-10m-20260616231827`). Local evidence:
`just gpu-build`, `ctest --test-dir pgaccel-kernels/build -R test_olap_ssbm
--output-on-failure`, `cargo test -p pg_accel --lib ssbm_q1 -- --nocapture`,
and `cargo test -p pg_accel --lib olap_cache -- --nocapture` pass. Remaining
release hardening: run the full 10-iteration/5-warmup publication ladder,
capture cache-mode-both artifacts, add relation invalidation or
relfilenode/snapshot validation for the cache, and lift the cache/source shape
into the reusable resident grouped/star-schema operators for Q2-Q4.

Progress (2026-06-17 SSBM Q2 resident grouped star-join OLAP path): canonical
Q2.1/Q2.2/Q2.3 SQL now selects a resident `GpuAgg` CustomScan instead of the
old text-group-key decline. The SSBM resident cache now stages fact
`lo_partkey`, `lo_suppkey`, and `lo_revenue` alongside the Q1 columns, keeps
date-key-to-year lookup buffers, loads part/supplier dimension metadata, and
lazily builds device-resident membership maps for Q2 category, brand-range,
exact-brand, and supplier-region predicates. The Q2 kernel runs one
row-parallel grouped revenue pass over resident fact columns and dimension
maps, using 32-bit atomic lanes for Metal-compatible 64-bit revenue sums, and
returns only bounded `(SUM(lo_revenue), d_year, p_brand1)` groups to
PostgreSQL. The planner recognizes the full four-table star join, including
date/supplier joins pushed into parameterized `IndexPath` clauses, injects a
proof-backed resident grouped aggregate path before the generic text group-key
blocker, and refuses CPU fallback on missing cache or kernel failure. The
benchmark runner primes Q2 backends with the same resident cache loader used
by Q1.

Live PG18 smoke evidence covers every Q2 variant through selected CustomScan,
`GPU Resident Pipeline: true`, correctness diffs, kernel deltas, stock fallback
= 0, and final-only CPU materialization. These are one-iteration smoke cells,
not publication statistics: `ssbm_q2_1` wins at 10K 1.22x
(`benchmarks/artifacts/ssbm-q2-1-resident-10k-smoke-20260617`), 100K 1.36x
(`benchmarks/artifacts/ssbm-q2-1-resident-100k-smoke-20260617`), and 1M 4.15x
(`benchmarks/artifacts/ssbm-q2-1-resident-1m-smoke-20260617`); `ssbm_q2_2`
wins at 10K 1.41x
(`benchmarks/artifacts/ssbm-q2-2-resident-10k-smoke-20260617`), 100K 1.38x
(`benchmarks/artifacts/ssbm-q2-2-resident-100k-smoke-20260617`), 1M 4.84x
(`benchmarks/artifacts/ssbm-q2-2-resident-1m-smoke-20260617`), and 10M 13.86x
(`benchmarks/artifacts/ssbm-q2-2-resident-10m-smoke-20260617`); `ssbm_q2_3`
wins at 10K 1.33x
(`benchmarks/artifacts/ssbm-q2-3-resident-10k-smoke-20260617b`), 100K 1.49x
(`benchmarks/artifacts/ssbm-q2-3-resident-100k-smoke-20260617`), and 1M 3.41x
(`benchmarks/artifacts/ssbm-q2-3-resident-1m-smoke-20260617`). Local evidence:
`just gpu-build`, `ctest --test-dir pgaccel-kernels/build -R test_olap_ssbm
--output-on-failure`, `cargo test -p pg_accel --lib ssbm_q1 -- --nocapture`,
`cargo test -p pg_accel --lib olap_cache -- --nocapture`, and
`cargo fmt --check` pass. Remaining release hardening: run full
10-iteration/5-warmup Q2 publication ladders across all canonical scales,
capture cache-mode-both artifacts, add relation invalidation or
relfilenode/snapshot validation for the broader cache, compact per-query brand
dictionaries when moving beyond Q2, and lift the Q2 row-parallel grouped
aggregate into the generic `hashagg_*`/SSBM Q3/Q4 operator class.

Progress (2026-06-18 SSBM Q3 resident customer/supplier geography OLAP path):
canonical Q3.1/Q3.2/Q3.3/Q3.4 SQL now selects a resident `GpuAgg` CustomScan
for the full lineorder/date/customer/supplier star-join class. The SSBM
resident cache now stages `lo_custkey`, loads customer city/nation/region,
loads supplier city/nation/region, and keeps `d_yearmonth` text alongside the
date year lookup. Q3 variants compile into resident date/customer/supplier
match maps plus compact customer/supplier group-code maps: Q3.1 uses
region-to-nation grouping, Q3.2 uses United States nation-to-city grouping,
Q3.3 handles the canonical OR city-set predicates, and Q3.4 handles the
`Dec1997` yearmonth text predicate. The Q3 kernel runs one row-parallel
resident grouped revenue pass over fact columns and dimension maps, reusing the
Q2 32-bit low/high atomic revenue lanes so the path stays Metal/SYCL-safe
without 64-bit atomics. The executor materializes canonical Q3 slot order
`(c_geo, s_geo, d_year, revenue)` and emits deterministic year/revenue/text
ordering. The planner recognizes the three-table join predicates through the
existing PG18 join/path collectors, adds a Q3-only OR-aware text predicate
walker, validates exact group keys, and refuses CPU fallback on missing cache
or kernel failure. The benchmark runner now primes Q3 backends with the
resident SSBM cache loader.

Live PG18 smoke evidence covers every Q3 variant through selected CustomScan,
`GPU Resident Pipeline: true`, correctness diffs, kernel deltas, stock fallback
= 0, and final-only CPU materialization. These are one-iteration smoke cells,
not publication statistics. At 10K rows: `ssbm_q3_1` wins 1.32x
(`benchmarks/artifacts/crash-repro-1781799587`), `ssbm_q3_2` wins 1.24x
(`benchmarks/artifacts/crash-repro-1781799613`), `ssbm_q3_3` wins 1.32x
(`benchmarks/artifacts/crash-repro-1781799614`), and `ssbm_q3_4` wins 1.29x
(`benchmarks/artifacts/crash-repro-1781799614`). At 1M rows: `ssbm_q3_1` wins
6.68x (`benchmarks/artifacts/crash-repro-1781799648`), `ssbm_q3_2` wins 4.75x
(`benchmarks/artifacts/crash-repro-1781799663`), `ssbm_q3_3` wins 4.10x
(`benchmarks/artifacts/crash-repro-1781799675`), and `ssbm_q3_4` wins 4.31x
(`benchmarks/artifacts/crash-repro-1781799685`). 1M correctness artifacts all
passed with zero accel-minus-baseline and baseline-minus-accel rows. Local
evidence: `just gpu-build`, `ctest --test-dir pgaccel-kernels/build -R
test_olap_ssbm --output-on-failure`, `cargo check -p pg_accel`,
`cargo test -p pg_accel --lib ssbm_q1 -- --nocapture`, `cargo test -p
pg_accel --lib olap_cache -- --nocapture`, `cargo fmt --check`, and
`just install-pg-accel 18` pass. Remaining release hardening: run full
10-iteration/5-warmup Q3 publication ladders across all canonical scales,
capture cache-mode-both artifacts, add relation invalidation or
relfilenode/snapshot validation for customer/supplier/date caches, and lift the
Q2/Q3 dense grouped revenue kernel family into a reusable resident star-schema
operator for Q4 and non-SSBM OLAP templates.

Progress (2026-06-18 SSBM Q4 resident grouped profit OLAP path): canonical
Q4.1/Q4.2/Q4.3 SQL now selects a resident `GpuAgg` CustomScan for the full
lineorder/date/customer/supplier/part star-join class. The SSBM resident cache
now stages `lo_supplycost` and `p_mfgr` in addition to the Q1-Q3 columns, and
prebuilds Q2/Q3/Q4 dimension filter/group maps during cache load so timed
queries do not pay first-use device allocation. Q4 variants compile into
resident date/customer/supplier/part match maps plus compact geo/part group
codes: Q4.1 groups by `(d_year, c_nation)` with part manufacturer membership,
Q4.2 groups by `(d_year, s_nation, p_category)` with OR year/manufacturer
membership, and Q4.3 groups by `(d_year, s_city, p_brand1)` with
United-States supplier and MFGR#14 category filters. The Q4 kernel runs one
row-parallel resident grouped `SUM(lo_revenue - lo_supplycost)` pass and uses
resident scratch buffers for low/high two's-complement accumulation, avoiding
per-query device malloc/free and staying portable without 64-bit atomics. The
executor materializes canonical Q4 slot order and deterministic ORDER BY
output. The planner recognizes all five star joins, OR-aware year/manufacturer
predicates, exact group keys, and the exact profit subtraction aggregate, then
refuses CPU fallback on missing cache or kernel failure.

Live PG18 smoke evidence covers every Q4 variant through selected CustomScan,
`GPU Resident Pipeline: true`, correctness diffs, kernel deltas, stock fallback
= 0, and final-only CPU materialization. These are one-iteration smoke cells,
not publication statistics. At 10K rows: `ssbm_q4_1` wins 1.01x
(`benchmarks/artifacts/crash-repro-1781840912`), `ssbm_q4_2` wins 1.19x
(`benchmarks/artifacts/crash-repro-1781840932`), and `ssbm_q4_3` wins 1.15x
(`benchmarks/artifacts/crash-repro-1781840942`). At 1M rows: `ssbm_q4_1` wins
4.28x (`benchmarks/artifacts/crash-repro-1781840955`), `ssbm_q4_2` wins 4.45x
(`benchmarks/artifacts/crash-repro-1781840979`), and `ssbm_q4_3` wins 3.66x
(`benchmarks/artifacts/crash-repro-1781841007`). Correctness artifacts all
passed with zero accel-minus-baseline and baseline-minus-accel rows. Local
evidence: `just gpu-build`, `ctest --test-dir pgaccel-kernels/build -R
test_olap_ssbm --output-on-failure`, `cargo check -p pg_accel`, `cargo test -p
pg_accel --lib ssbm_q1 -- --nocapture`, `cargo test -p pg_accel --lib
olap_cache -- --nocapture`, `cargo fmt --check`, `git diff --check`, and
`just install-pg-accel 18` pass. Remaining release hardening: run full
10-iteration/5-warmup Q4 publication ladders across all canonical scales,
capture cache-mode-both artifacts, add relation invalidation or
relfilenode/snapshot validation for part/customer/supplier/date caches, and
lift the Q2/Q3/Q4 resident grouped kernels into a reusable star-schema OLAP
operator for non-SSBM grouped aggregate workloads.

Progress (2026-06-14 resident fabric ABI): the first broad resident-dataflow
spine landed without reopening planner admission. `pg_accel/src/engine/residency.rs`
now owns the shared proof vocabulary (`ResidentPipelineProof`,
`ResidentOperatorStage`, `BatchResidency`, `CpuBoundaryReason`,
`MaterializationBoundary`, `DeviceBufferRef`), resident column/batch views, and
device variable-output CSR views for H3/PostGIS/raster/join-pair style outputs.
The legacy host `PgaccelBatch` ABI remains untouched. A parallel C/Rust ABI was
added beside it: `pgaccel_resident_column_view`, `pgaccel_resident_batch`, and
`pgaccel_device_var_output` in `pgaccel-kernels/include/pgaccel_expr.h`, mirrored
by `PgaccelResidentColumnView`, `PgaccelResidentBatch`, and
`PgaccelDeviceVarOutput` in `pg_accel/src/gpu/types.rs`, with ABI version and
layout tests. Current host producers in scan and aggregate executors now mark
their blocking CPU boundary explicitly (`HostInputStaging` or
`HostTupleReconstruction`) instead of relying on ambiguous batch metadata. This
does not select any SQL plan yet.

Progress (2026-06-14 resident proof trailer): `custom_private` now carries an
append-only, versioned resident-proof trailer (`RPRF`) backed by a zero-safe
`ResidentProofSnapshot`. All CustomScan plan constructors append conservative
host-staged proof snapshots without shifting existing strategy payload offsets;
PreAgg generic decode now recognizes its three-field header deliberately instead
of accidentally interpreting depth count as an `AccelStrategy`. Executor init
decodes and stores the proof snapshot in `GpuAccelState`, and EXPLAIN emits
`GPU Resident Pipeline`, `GPU Resident Proof Version`, `GPU Resident Stage Mask`,
`GPU Resident Device Columns`, and a proof-backed boundary string from that
snapshot. Benchmark report and explain-audit consumers now require the version,
stage-mask, and device-column evidence before accepting `GPU Resident Pipeline:
true`; a bare string no longer proves residency. Artifact generation now emits
`no_dispatch_audit` and `resident_boundary_audit` JSON/Markdown, and report
finalization fails selected CustomScan rows that lack proof-backed resident
pipeline evidence. Local evidence: `cargo test -p pg_accel_bench -- --nocapture`
passed all 289 tests after the audit artifact status was updated to
`reported_resident_pipeline`. The next hunk is planner admission from real
proof producers: add proof construction to GPU-native operator builders,
require `proof.gpu_resident_pipeline()` before `add_path`, and keep host-staged
candidates declined with precise `no_gpu_resident_pipeline` boundary evidence.

Progress (2026-06-14 resident admission chokepoint): planner CustomPath
insertion now has proof-aware helpers for normal and partial paths. All
pg_accel CustomPath insertion sites either go through that helper or, for
nested Gather/Finalize scaffolds, attach proof before wrapping the child
CustomPath. Current executors still pass conservative host-staged proof, so
the hard resident-only policy continues to decline them; future GPU-native
operator builders must pass a nonzero-stage, device-column proof through the
same chokepoint. `PlanCustomPath` now propagates the path proof trailer into
the plan trailer instead of manufacturing strategy defaults at the last
moment. Resident proof decode only accepts a tail `RPRF` trailer, avoiding
payload integer collisions, and snapshot residency now requires a nonzero stage
mask. Report classification now scopes resident evidence to direct
`Custom Scan (GpuAccel...)` blocks and parses one-line JSON proof metadata, so
a resident child cannot satisfy a nonresident parent.

Progress (2026-06-11): the first substrate cleanup landed in
`pg_accel/src/engine/columnar.rs`. `ColumnarBatchOwner` now keeps typed vectors
as the owned FFI backing storage instead of copying them into byte buffers and
leaking the original typed allocations. The batch contract now records host vs
device residency and CPU-boundary reason metadata, asserts fully populated
batches before FFI handoff, preserves native pointer alignment, normalizes bool
columns to bytes, and has focused `pg_test` coverage for typed pointer reads,
alignment, null masks, and tag layout. `ColumnarBatchOwner` now represents
no-null columns as `ColumnarNulls::AllValid`, emits a null per-column mask
pointer for all-valid columns, and only allocates byte-per-row masks when real
nulls are present. `pgaccel_batch` expression consumers document that null
per-column mask pointers mean all rows are valid, and expression staging now
fails closed on data/null-mask staging OOM instead of converting OOM into
all-NULL or all-valid columns. H3 grouped-count, fused
`h3_cell_to_parent(cell, const), COUNT(*)`, and direct template-safe numeric
`GpuExpr` work proved useful lower-level `ColumnarBatchOwner` and kernel
machinery, but those SQL lanes are now held behind the hard resident-only
admission gate while their input path is still `residency=HostColumnar` /
`cpu_boundary=HostInputStaging`. PG18 live evidence now proves representative
host-staged direct/fused shapes stay native, record `no_gpu_resident_pipeline`,
dispatch zero pg_accel kernels, and retain exact PostgreSQL parity. Broad
bytecode/bitmap `GpuExpr`, projection masks, generic `GpuScan`, and downstream
GPU-resident consumers are still open. Timing smoke on the old 1M-row
host-staged direct `GpuExpr` count shape showed why it is not a release lane:
native serial count was about 48-50 ms, direct `GpuExpr` was about 292 ms at
8192-row batches and about 150 ms at the 65536-row GUC cap. The next
performance-critical step is to consume GPU
qual masks inside count/reduce/grouped-aggregate batch consumers so selected
filter+aggregate queries do not emit row-by-row PostgreSQL slots.

Progress (2026-06-12): fused `COUNT(*) WHERE template_predicate` now has a
real GPU count path instead of the row-returning direct `GpuExpr` shape:
`GpuAgg` owns the heap walk, stages only referenced predicate columns through
typed `ColumnarBatchOwner` batches, and dispatches a single count kernel for
one- and two-predicate numeric filters. Correctness hardening from the current
critical-path pass: comparison opcode inversion is centralized and applied to
`Const op Var` template extraction, stale PreAgg/dimension-filter opcode
literals were replaced with the real `expr_compiler::opcode` constants,
row-returning direct `GpuExpr` planner admission still declines `int8`
templates, while fused count admits `bigint` only when constants are exactly
representable in the current f64-erased template ABI. The fused-count executor
independently gates source column types, C++ float32 typed count
kernels only run when the constant round-trips exactly to float32, integer
typed count kernels fall back for non-integral constants, and the CPU fused
fallback now uses PostgreSQL-style NaN ordering/equality. Live PG18 coverage
now proves direct `GpuExpr`, fused one- and two-predicate count, nullable
prefix heap tuples, and `float4 NaN = NaN` all match native PostgreSQL and
dispatch GPU work. Warm 1M smoke on the current M-series/Metal dev box:
single-predicate fused GPU count was 46.82 ms vs 84.63 ms native serial
(1.81x), two-predicate fused GPU count was 51.34 ms vs 89.22 ms native serial
(1.74x). Forced PostgreSQL parallel still wins at 28.43 ms and 26.93 ms, so
release admission stays cost-honest and the fused count lane now compares
against PostgreSQL's cheapest total path including `Gather`. Next critical
path: remove the remaining heap-to-Rust-Vec-to-USM staging by filling the
kernel input buffer directly, then make scan qual masks and count/reduce
consumers GPU-resident or parallel-aware enough to beat PostgreSQL parallel.

Progress (2026-06-12 follow-up): fused filter-count staging now keeps null
masks lazy. All-valid predicate columns no longer write one zero byte per row;
they flow into `ColumnarBatchOwner::*_all_valid` and C++ count kernels see a
null mask pointer of `NULL`. If a real SQL NULL appears late in a predicate
column, the mask is allocated once and backfilled for the already-staged valid
prefix. Unit tests cover both paths, C++ count tests now cover `col_nulls[col]
== NULL` for one- and two-predicate count kernels, and the PG18 live nullable
fixture now has NULLs in the predicate column itself. Warm 1M all-valid smoke
after this cleanup: single-predicate fused GPU count was 36.49 ms vs 64.21 ms
native serial (1.76x), two-predicate fused GPU count was 38.29 ms vs 60.75 ms
native serial (1.59x). Forced PostgreSQL parallel still wins at 20.39 ms and
19.50 ms.

Progress (2026-06-12 direct shared-USM): the shared-USM fused-count substrate
is now implemented end-to-end. C++ exposes `pgaccel_expr_shared_alloc/free`,
borrowed `pgaccel_expr_usm_col { values, nulls, type }` views, and direct
`*_count_usm` entry points for single- and two-predicate count kernels. Rust
owns those buffers through RAII `ExprSharedBuffer<T>` wrappers and the fused
`GpuAgg` executor can fill kernel-readable shared USM directly. The direct ABI
is intentionally scoped to the planner-supported template set (`int2/int4`
staged as int32, exact-constant `int8`, `float4`, `float8`); broad
row-returning `GpuExpr` still declines `int8` until typed constants replace
the f64-erased template payload end-to-end. New gates cover all-valid and
late-null USM staging, C++ shared alloc/free, direct-USM null-mask semantics,
the exact `int8` constant envelope,
`float4 = NaN` on one- and two-predicate templates, exact PG18 counter
assertions, and a 4.3M-row two-batch lazy-null-mask transition. AdaptiveCpp
`fork-safe-metal` has since been rebased onto upstream `develop`; the current
pin is recorded in the integration section at the top of this file. Its Metal
event wait path now
avoids `MTLSharedEvent::waitUntilSignaledValue()` and has CTest coverage for a
cold forked child creating a Metal queue, allocating shared USM, submitting a
kernel, and waiting through `event.wait_and_throw()`. The same pin also keeps
Metal kernels flat-bound up to 29 user arguments, avoiding the fragile
`newArgumentEncoder()` path that aborted concurrent cold pg_accel backends on
two-predicate direct-USM count kernels. Local fork evidence:
`ctest --test-dir tests/build -R fork_safety_metal --output-on-failure`
passed, and `ctest --test-dir tests/build -L metal --output-on-failure`
passed all five Metal tests. pg_accel evidence with the direct-USM macOS gate
open: `test_expr_templates` cold passed 137/137, `test_fork_cold` passed,
serial PG18 fused-count plan-shape tests passed, and concurrent PG18
nullable-prefix, multibatch-null-transition, and NaN fused-count tests passed
without backend aborts. Focused isolated-fixture evidence now covers the
two-predicate direct-USM fused count path without touching the shared
`bench_gpuexpr_direct_gate` table:
`PG_ACCEL_TEST_PG_MAJOR=18 ACPP_PREFIX=/Users/contra/local cargo test -p pg_accel_bench --features integration_tests plan_shape_fused_gpuexpr_count_two_predicate_isolated_pg18_evidence -- --nocapture`
passed before the 2026-06-14 hard resident-only gate, using a
backend-PID-suffixed fixture to prove lower-level fused-kernel semantics. The
selected SQL-plan guard is now the corresponding
`*_stays_native_until_resident` coverage that records
`no_gpu_resident_pipeline` and dispatches no pg_accel kernels. Live PG18 smoke
after reinstalling the rebased fork:
two-predicate direct-USM fused GPU count on 2,000,000 rows planned as
`GpuAccelAgg -> GpuAccelScan`, cold-dispatched 2 batches/2,000,000 rows without
abort, then warmed to 53.54 ms vs 93.12 ms native serial (1.74x) with
`rows_dispatched=2000000`, `gpu_rows_processed=2000000`, and no stock fallback.
Direct-USM count kernels now use 32-bit per-workgroup scratch counters with
`size_t` final accumulation; focused C++ coverage passed 149/149 including
multi-workgroup direct-USM single- and two-predicate count cases.
Last warm 1M all-valid smoke before the gate was reopened: single-predicate
direct-USM fused GPU count was 28.78 ms vs 44.54 ms native serial (1.55x),
two-predicate direct-USM fused GPU count was 24.88 ms vs 45.80 ms native serial
(1.84x), with one kernel, one batch, 1,000,000 GPU rows processed, zero
uncertain rows, and zero stock fallback. Forced PostgreSQL parallel still wins
at 13.86 ms and 14.47 ms, so the next critical path is
parallel-aware fused count, or a true GPU-resident scan-to-count handoff that
removes the remaining single-backend heap walk bottleneck.

Progress (2026-06-12 bigint fused-count): fused `COUNT(*) WHERE ...` now
supports `bigint` predicate columns on the direct shared-USM count path when
the constants fit the exact 53-bit integer envelope of the current template
ABI. The planner keeps row-returning direct `GpuExpr` `int8` scans native, and
serial fused `GpuAgg` can now self-scan qualified `COUNT(*)` tables by carrying
the compiled scan expression in `custom_private`. Live PG18 evidence:
`plan_shape_fused_gpuexpr_count_supports_bigint_exact_constants` passed with a
two-predicate `bigint` + `int4` filter, exact native count parity, one fused
GPU batch over 1,000,000 rows, zero uncertain rows, and zero stock fallback.
Manual serial smoke on 2,000,000 rows measured native PostgreSQL serial at
119.945 ms vs fused `GpuAccelAgg` at 68.971 ms (1.74x), with
`rows_dispatched=2000000`, `gpu_rows_processed=2000000`, two GPU kernels, and
no stock fallback.

Progress (2026-06-12 serial fused-count hot path and parallel crash gate):
serial fused `COUNT(*) WHERE template_predicate` now reuses executor-local
direct shared-USM value buffers across synchronous batches, keeps previously
allocated null masks hidden from later all-valid batches, and only reuses the
scratch path outside partial/parallel execution. The C++ direct-USM count
kernels now process four rows per work-item before the existing local reduction,
cutting partial counter groups while preserving the Metal-safe reduction shape;
focused C++ coverage now passes 183/183 with strided row/null tests that cover
rows beyond the first per-work-item lane. PG18 serial fused-count coverage
passes the six live plan-shape tests for one-predicate, two-predicate,
nullable-prefix, multibatch-null-mask, `float4 NaN = NaN`, and exact-constant
`bigint` cases. Serial fused-count target sizing now uses the relation row
estimate up to the existing 4,194,304-row cap instead of forcing the old
1,048,576-row fallback. Warm 2M serial smoke on the current M-series/Metal dev
box now runs in one batch: single-predicate pg_accel was 46.634 ms vs
105.528 ms native serial (2.26x); two-predicate pg_accel was 52.063 ms vs
98.981 ms native serial (1.90x), with 2M rows dispatched, zero uncertain rows,
and zero stock fallback.

The parallel fused-count substrate remains open but is now fail-closed. A fresh
PG18 GUC-on run after the direct-USM/kernel changes crashed a PostgreSQL
parallel worker with SIGSEGV after entering the Metal worker path, so
`pg_accel.parallel_fused_count` is now a roadmap knob only: even when true, the
planner records `parallel_fused_count_unstable` and leaves the query on native
`Gather -> Partial Aggregate -> Parallel Seq Scan`. GUC-off still records
`parallel_fused_count_disabled`. Non-ignored PG18 tests now assert both native
fallback modes, zero GPU kernel delta, zero stock fallback, and exact native
count parity. The ignored evidence test also runs safely with the GUC on and
records native crash-gated fallback on 2M rows: native parallel 40.362 ms,
GUC-on native fallback 41.330 ms, kernel delta 0, stock fallback 0. The next
parallel critical path is worker stability/no-crash proof before any
performance gate or planner admission can return.

Progress (2026-06-13 fused float4 filtered reduce): template-predicate
`float4` filtered `SUM`/`MIN`/`MAX`/`COUNT(value)` now has a single direct-USM
GPU kernel that evaluates the predicate and reduces the value column in one
pass while separately returning selected rows for `COUNT(*)` and non-null
value rows for `COUNT(value)`. C++ coverage verifies one- and two-predicate
NULL behavior for predicate and value columns; PG18 live plan-shape coverage
now asserts one fused f32 reduce kernel per batch. The existing mask+reduce
path remains the fallback for `bigint`, multi-value-column, and unsupported
template-constant cases. Live 2M probe on the current M-series/Metal dev box:
native serial was 99.804 ms, forced PostgreSQL parallel was 19.683 ms, and the
new pg_accel fused f32 reduce was 61.321 ms with one batch, one GPU kernel,
2,000,000 rows dispatched, zero uncertain rows, and zero stock fallback. The
next release-critical performance gate is still removing or parallelizing the
single-backend heap staging path; this fused aggregate-consumer work reduces
GPU launch/pass overhead but does not solve the scan bottleneck.

Progress (2026-06-13 batch selection and masked reduce substrate): the shared
batch contract now has an explicit optional selection mask with `1 = selected`
and `0 = filtered`, collapsing all-selected batches to a null selection
pointer. `ColumnBatch` descriptors validate the same mask shape and expose
selected-row counts for future aggregate, join, H3, sort, window, PostGIS, and
raster consumers. Direct `GpuExpr` scan evaluation now has a reusable
three-valued-result-to-selection conversion helper: all true results become an
all-rows batch, mixed true/false results become a binary selection mask, and
uncertain rows still fail closed before batch consumers can rely on the mask.
C++ now exposes masked fused `SUM`/`MIN`/`MAX`/`COUNT` kernels for `f32`,
`f64`, and `i64`; Rust has staged wrappers that validate optional value-null
and selection masks before dispatch. Metal keeps the f64 masked path
fail-closed until the soft-fp64 struct-return path is made safe. Focused
evidence passed for Rust columnar selection, descriptor validation, scan mask
conversion, reduce wrapper validation, C++ masked f32/i64 runtime suites, and
the existing minimal reduce binary. The next executor step is to wire this into
filtered `COUNT`/`SUM`/`MIN`/`MAX` batch consumers so qualifying scans stop
materializing selected rows back through PostgreSQL slots.

Progress (2026-06-13 fused filtered reduce): filtered aggregate queries can
now consume GPU template-selection masks inside `GpuAgg` instead of returning
passing rows through PostgreSQL slots for the admitted vertical slice.
Planner admission uses one shared fused-filter candidate for self-scan,
cost-baseline, threshold, and no-value-reduce gates. It admits ungrouped,
non-partial template filters for `float4` value `COUNT`/`SUM`/`MIN`/`MAX`, and
for `bigint` value `COUNT`/`MIN`/`MAX`; mixed `COUNT(*)` is folded from the
selection mask once a supported value aggregate admits the lane.
`SUM(bigint)` remains native until the device path can return a wider exact
state. The executor now heap-stages predicate columns and value columns into
direct shared-USM batches, aliases value columns that are already present as
predicate columns, dispatches the template predicate to a binary selection
mask, then dispatches masked multi-reduce over each distinct value column.
`float4` results use PostgreSQL NaN ordering for min/max merge, and `bigint`
min/max/count fold into the existing exact integer finalization state. C++
masked reduce now has special-value coverage for infinities and PG NaN
ordering, using bit-pattern NaN detection so Metal fast-math cannot erase the
check. Focused evidence passed for the masked f32/i64 Rust wrappers, planner
admission/bookkeeping units, C++ `test_reduce_minimal` masked suites, and PG18
plan-shape compilation for both `float4` and `bigint` filtered reduce. Live
PG18 evidence now passes for fused `float4` filtered reduce and the `bigint`
mask+reduce fallback after reinstalling the extension. A 2M local probe for
`SUM`/`MIN`/`MAX`/`COUNT(val) WHERE val > 500::float4 AND category < 50`
measured native serial 100.492 ms, forced PostgreSQL parallel 22.941 ms, and
pg_accel 59.660 ms with one batch, one fused f32 reduce kernel, 2,000,000 rows
dispatched/processed, zero uncertain rows, and zero stock fallback. The
remaining performance blocker is the same one: a single backend still
heap-walks and fills USM buffers, so beating PostgreSQL parallel across
OLAP/time-series/geo requires the GPU-resident or parallel-safe
scan-to-aggregate handoff.

Progress (2026-06-14 hard GPU-only gate): scan, join, aggregate, window,
function-scan, and SRF-target-list planner hooks now fail closed before
injecting any host-staged CustomScan path and record the stable reason
`no_gpu_resident_pipeline`. There is no GUC or user-facing opt-out: if a
candidate cannot keep the pipeline GPU-resident, PostgreSQL plans it natively.
Live PG18 evidence:
`plan_shape_gpu_only_declines_host_staged_fused_reduce` passed and proved the
fused filtered-reduce SQL stays on PostgreSQL native aggregate/scan, dispatches
zero pg_accel kernels, and records no stock fallback.

## Release Mission

These are pre-release gates, not aspirational stretch goals:

- Cover every PostgreSQL workload family that can plausibly become more
  efficient on GPU: scan, expression/filter/projection, aggregate, join,
  sort/top-k/rank, window, H3, PostGIS geometry, raster, and Geo analytics.
- Cover the execution surface PG-Strom supports, using AdaptiveCpp kernels
  and pg_accel runtime plumbing: `GpuScan`, `GpuJoin`, `GpuPreAgg`,
  expression pushdown, join-side reuse, pre-aggregation, pruning, and
  GPU-resident intermediate data.
- Reach at least 90% test coverage for pg_accel-owned Rust, C++/SYCL, and
  SQL-extension behavior, with artifacts that state which layer they measure.
- Stress test Metal with zero backend crashes.
- Stress test CUDA with zero backend crashes on NVIDIA hardware.
- Beat PostgreSQL parallel execution across the selected benchmark matrix on
  both M-series and NVIDIA hardware.
- Match or beat PG-Strom for the benchmarked use cases PG-Strom supports.

## Phase 0 - Evidence, Provenance, And GPU-Only Guardrails

Nothing below this phase counts as fixed until the benchmark and runtime
evidence can prove which binary ran, which plan was selected, which GPU
kernels dispatched, and which rows returned to PostgreSQL.

### Benchmark mission and winning-lane policy

- Mission: pg_accel should win by offloading compute-dense PostgreSQL work
  to GPU where the GPU actually beats PostgreSQL parallel execution.
- Planner rule: decline cases where launch, JIT, materialization, soft-fp64,
  data transfer, per-worker duplication, or output reconstruction makes GPU
  execution slower.
- Current benchmark state from the 2026-05-18/19 focused GPU-path pass,
  refreshed on 2026-06-09 with a release-mode benchmark harness:
  the earlier `gpu_nlj_between @ 50K` selected Custom Scan win
  (`benchmarks/artifacts/crash-repro-1779092055`) is stale for release
  credit. A fresh selected-path rerun closed the backend connection during
  correctness diff
  (`benchmarks/artifacts/gpu-nlj-between-release-50k-cacheboth-20260609`), so
  `GpuNestedLoopIneq` is now planner-gated until the host tuple
  reconstruction boundary is replaced or reproven. Current NLJ release proof
  is native-decline, not a GPU win:
  `benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609`
  records `planner_declined`, kernel delta 0, `nlj_between_host_boundary_unsafe`
  threshold evidence, a passing threshold-matrix row, and no ship-gate
  failures. `h3_bulk` selects the
  grouped H3 aggregate path and currently runs the direct/pure-GPU route for
  resolution < 8: one GPU lat/lng-to-cell kernel writes H3 keys into a
  USM-accessible slab, then `pgaccel_hash_count_i64_device_execute` sorts
  and count-compacts those keys without staging host cell/key vectors.
  The C++ bridge no longer recasts f64 input arrays for this path; the direct
  point extractor produces f32 staging arrays in the heap pass while retaining
  f64 originals for exact boundary fixups. Remaining host work is heap
  extraction into f64/f32 arrays, conservative exact fixups, per-block prefix
  metadata, and final PostgreSQL result materialization. The direct point
  extractor now preallocates its f64/f32 staging buffers from relation stats
  with a bounded cap, avoiding avoidable allocator churn on the live heap pass.
  Current cache-mode-both pure-route evidence from the source tree after that
  cleanup: res7 `h3_bulk` is 47.81 ms vs 367.34 ms, 7.68x at 100K
  (`benchmarks/artifacts/h3-res7-prealloc-release-100k-cacheboth-1780992523`)
  and 345.39 ms vs 6364.24 ms, 18.43x at 1M
  (`benchmarks/artifacts/h3-res7-prealloc-release-1m-cacheboth-1780992542`).
  The broadened grouped H3 release matrix also covers res9 at 52.13 ms vs
  344.89 ms, 6.62x at 100K
  (`benchmarks/artifacts/h3-res9-release-100k-cacheboth-1780993419`) and
  389.90 ms vs 5847.52 ms, 15.00x at 1M
  (`benchmarks/artifacts/h3-res9-release-1m-cacheboth-1780993439`), plus
  res15 at 89.31 ms vs 388.43 ms, 4.35x at 100K
  (`benchmarks/artifacts/h3-res15-release-100k-cacheboth-1780994043`) and
  774.97 ms vs 6540.21 ms, 8.44x at 1M
  (`benchmarks/artifacts/h3-res15-release-1m-cacheboth-1780994068`). These
  artifacts record `harness_profile=release`, no crashes, dispatch counters,
  stock fallback delta 0, consumed output rows, warmup summaries, and
  deterministic aggregate diffs returning zero mismatches against h3-pg.
  The canonical `h3_cell_to_parent` workload is now a narrower winning lane:
  fused `h3_cell_to_parent(cell, const), COUNT(*) GROUP BY 1` selects
  `GpuAccelAgg`, dispatches the parent-cell kernel plus bounded device-hash
  count, and keeps standalone scalar `h3_cell_to_parent` quarantined. Stable
  cache-mode-both evidence is 5.74 ms vs 7.93 ms, 1.38x at 100K
  (`benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-100k-cacheboth-20260610011605`)
  and 27.49 ms vs 33.18 ms, 1.21x at 1M
  (`benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-1m-cacheboth-20260610011614`);
  both artifacts have correctness diff `pass`, Custom Scan dispatch evidence,
  stock fallback delta 0, consumed output rows, and threshold-matrix status
  `pass`.
  Hash join, grouped generic hash aggregation, top-k, generic GpuExpr scans,
  SSBM Q2.3, and the default H3 SRF benchmark remain stable no-crash
  planner-decline or not-yet-winning rows on M2/Metal.
- Ship bar: no selected GPU benchmark cell may crash; every selected GPU
  cell in the release matrix must be at `speedup_x >= 1.0` against
  PostgreSQL parallel, with explicit exceptions only for
  regression/no-overhead workloads that intentionally prove planner decline.
- Acceptance: benchmark reports separate `plan_selected`,
  `gpu_kernel_dispatched`, `gpu_resident_pipeline`, `function_kernel_count`,
  `rows_returned_to_cpu`, and `planner_declined`.

### Planner pure-GPU audit

- Scope: every planner-selected `GpuStrategy` must be audited as a
  GPU-resident pipeline. "Pure GPU" means the selected pg_accel path does not
  consume rows through `ExecProcNode`, `heap_getnext`, `ExecCopySlotMinimalTuple`,
  host `HashMap`/`Vec` staging, CPU result reordering, or row-by-row tuple
  reconstruction except for the final unavoidable PostgreSQL output boundary.
  Current audit found these non-pure paths:
- `GpuScan` (`GpuSpatial`, `GpuRaster`, `GpuH3`, `GpuExpr`): not pure GPU.
  Base-rel paths wrap seq/bitmap child paths
  (`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:454-473`), child
  mode pulls `ExecProcNode` and copies `MinimalTuple`s
  (`pg_accel/src/engine/executor/scan/arena_scan.rs:108-177`), direct mode
  still heap-walks on CPU
  (`pg_accel/src/engine/executor/scan/arena_scan.rs:66-93`), and output drains
  `MinimalTuple`s back into slots
  (`pg_accel/src/engine/executor/scan/exec.rs:456-472`). Broad standalone
  `GpuExpr` exposure is disabled from normal base-rel planning, and the narrow
  direct template-safe numeric scan-to-columnar path now stays native under the
  hard resident-only gate because it still heap-walks and emits PostgreSQL
  slots.
  Work: replace scan input with GPU-resident columnar batches, GPU qual and
  projection output masks, and downstream batch handoff; keep bitmap/index
  pruning only as GPU batch producers or explicit CPU-boundary declines.
- `GpuSort`: not pure GPU. Planner exposes bounded single-key top-k only and
  mirrors partial wrappers
  (`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:690-697`,
  `:1313-1450`); executor materializes all input tuples, sends only key
  vectors to GPU, then reorders/trims host `MinimalTuple`s and emits through
  PostgreSQL slots (the deleted sort executor). Work: make sort consume and produce GPU batch
  handles, keep payload columns on device, implement GPU gather/top-k payload
  compaction, and decline standalone heap `ORDER BY` until final transfer is
  bounded and winning.
- `GpuAgg` / `GpuReduce`: not pure GPU. Non-grouped agg drains child tuples and
  builds host value vectors before GPU reduce
  (`pg_accel/src/engine/executor/agg/execute.rs (deleted in the 2026-07 Phase 3 demolition)-1538`); grouped paths use
  direct heap/slot scans and host key/value/null buffers before
  `gpu::hash_agg_execute` (`:1976-2204`, `:3030-3204`); grouped emit reads GPU
  handles into PostgreSQL tuples one group at a time (`:2874-2970`). H3 bulk is
  closest, but still stages lat/lng/null inputs on host and materializes final
  groups (`:2227-2252`). Work: define GPU scan-to-reduce/grouped-agg batch
  contracts, device-side null/key/value staging, device prefix/compaction, and
  GPU-to-GPU handoff for joins, sorts, and windows.
- `GpuJoin` / `GpuHashJoin`: not pure GPU. Planner caps row-returning output
  because the executor reconstructs joined rows into PostgreSQL slots
  (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:19-26`,
  `:238-268`); build/probe paths still collect child rows through
  `ExecProcNode`, host key/null vectors, and `MinimalTuple` buffers
  (`pg_accel/src/engine/executor/join/probe.rs:327-554`, `:584-911`).
  Partial paths mirror the injection but rebuild work per worker
  (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:434-452`). Work:
  retain device build-side buffers, probe directly from GPU/columnar child
  batches, emit device pair buffers or aggregate counts without host tuple
  reconstruction, and share/reuse build-side state across workers where safe.
- `GpuNestedLoopIneq`: crash-gated, not pure GPU. The pair kernel exists, but
  planner exposure is disabled after the 2026-06-09 release-harness selected
  run closed the backend connection during correctness diff
  (`benchmarks/artifacts/gpu-nlj-between-release-50k-cacheboth-20260609`);
  the gate now returns `false`
  (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:497-507`) and the
  benchmark matrix expects `nlj_between_host_boundary_unsafe`
  (`pg_accel_bench/src/workloads/mod.rs:1535-1555`). The executor still
  collects both children into host `MinimalTuple`s, extracts/remaps host key
  arrays, copies matched tuples into pending pairs, and reconstructs joined
  slots (`pg_accel/src/engine/executor/join/nlj.rs:83-96`, `:127-140`,
  `:227-239`, `:292-319`, `:323-427`). Work: GPU-resident input batches,
  device pair buffers with overflow metadata, GPU gather/projection or fused
  count/preagg consumers, and no host pair remap except final bounded output.
- Spatial/raster/H3 joins and merge joins: planner-visible opportunities still
  decline rather than selecting pure GPU paths. Spatial/raster/H3 joins are
  skipped (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:196-207`);
  merge joins are observation-only (`:134-149`). Work: GPU spatial join
  kernels, GPU merge join, and planner admission only when downstream output
  remains GPU-resident or cardinality-reduced.
- `GpuPreAgg`: not pure GPU and not active from normal upper planning. The
  executor materializes dimension tables into host `HashMap`s and scans/probes
  fact rows through `ExecProcNode`/materialized slots
  (the deleted preagg executor); partial preagg scaffolds explicitly avoid CPU child
  wrappers (`preagg_partial.rs` (deleted in the 2026-07 Phase 3 demolition)),
  and normal upper planning does not call serial PreAgg until the child is
  GPU-resident (`pg_accel/src/engine/ffi/planner_hooks/mod.rs:203-207`).
  Work: device dimension hash tables, GPU fact scan/probe/filter/grouping, GPU
  partial/final aggregate state, and shared scan-state for parallel workers.
- `GpuWindow`: not pure GPU. Planner admits upper window paths, but executor
  buffers all input `MinimalTuple`s, extracts host columns, stores host result
  vectors, and emits one virtual PostgreSQL tuple per row
  (`pg_accel/src/engine/executor/window/mod.rs:61-87`, `:126-253`,
  `:409-575`, `:577-760`). Work: GPU-resident partition/order metadata,
  segmented kernels, batch-boundary frame state, and downstream GPU batch
  handoff; parallel windows need partition-aware worker ownership
  (`planner_hooks/window.rs` (deleted in the 2026-07 Phase 3 demolition)).
- `GpuFunctionScan`: not a pure GPU pipeline. The legacy path dispatches
  registered SRFs once, but only for constant arguments
  (`planner_hooks/projectset.rs` (deleted in the 2026-07 Phase 3 demolition)), buffers
  emitted host `Datum`s (`pg_accel/src/engine/ffi/custom_scan/function_scan.rs:35-68`),
  and drains rows through PostgreSQL slots one at a time (`:337-452`). Work:
  keep normal SQL admission behind `no_gpu_resident_pipeline`, produce GPU
  batch handles for SRF outputs, support table-correlated argument batches, and
  materialize only final bounded outputs.
- `GpuAccelSrfTargetList`: not pure GPU. It wraps a `ProjectSet` child, drives
  it through `ExecProcNode`, dispatches the SRF per input batch, and emits every
  expanded row as a PostgreSQL tuple
  (`srf_target_list.rs` (deleted in the 2026-07 Phase 3 demolition),
  `pg_accel/src/engine/ffi/custom_scan/srf_target_list.rs:321-453`,
  `:475-519`). Large outputs are capped because CPU materialization dominates
  (`srf_target_list.rs` (deleted in the 2026-07 Phase 3 demolition)).
  Normal SQL admission is now held behind `no_gpu_resident_pipeline`. Work:
  batched variable-output SRF kernels with row-id/offset tables on GPU,
  GPU-resident downstream aggregate/sort consumers, and multi-SRF ProjectSet
  lockstep/NULL-padding semantics before broad admission.
- Partial/Gather and partition/Append surfaces: not pure GPU. Partial
  `GpuSort`, partial `GpuHashJoin`, and partial aggregate scaffolds currently
  wrap PostgreSQL partial children or duplicate per-worker host work unless the
  child is already GPU-producing
  (`rel_pathlist.rs` (section removed in the 2026-07 Phase 3 demolition),
  `join_pathlist.rs` (section removed in the 2026-07 Phase 3 demolition),
  `partial_agg.rs` (deleted in the 2026-07 Phase 3 demolition),
  `preagg_partial.rs` (deleted in the 2026-07 Phase 3 demolition)). Partition
  children can receive per-child scan paths, but there is no GPU-resident
  append/merge batch operator; PostgreSQL combines child outputs on CPU. Work:
  only select partial paths when child input and inter-worker state are
  GPU-resident, add GPU batch append/merge handoff where needed, or keep
  explicit planner-decline counters.
- Acceptance: add/maintain planner audit coverage that walks every selected
  `GpuStrategy` and fails unless it reports `GPU Resident Pipeline: true`. A
  path can be marked pure GPU only when benchmark artifacts show GPU kernel
  dispatch, correctness diff, no crashes, `rows_returned_to_cpu` bounded to
  final output, and no `ExecProcNode`, heap-walk, or host `HashMap` staging
  between selected pg_accel operators.
- Progress (2026-06-09, superseded by 2026-06-14 hard GPU-only gate):
  `EXPLAIN` and the report fallback classifier learned CPU-boundary strings
  for all eight EXPLAIN strategy labels (`GpuScan`, `GpuJoin`, `GpuAgg`,
  `GpuSort`, `GpuWindow`, `GpuPreAgg`, `GpuFunctionScan`,
  `GpuAccelSrfTargetList`). Those strings remain useful forensic data for
  lower-level executor/kernel tests, but selected SQL-plan admission now fails
  closed before such host-staged CustomScans are injected. The live
  `explain-audit` resident check now treats any selected pg_accel Custom Scan
  without `GPU Resident Pipeline: true` as a failure. Historical selected H3
  release artifacts for res7/res9/res15 carry exact boundary evidence in
  `plan_snippets`, `pre_risk_contexts`, `report.json`, and `report.md`; that
  evidence is superseded for SQL admission by the hard resident-only gate. The
  audit parser scopes `Strategy`/resident properties to the current Custom Scan
  node, so a nested child pg_accel node cannot satisfy its parent by accident.
  Fresh post-gate release evidence:
  `benchmarks/artifacts/h3-res7-resident-gate-release-100k-cacheboth-20260609`
  records `h3_bulk @ 100K` at 41.61 ms vs 358.00 ms (8.60x), stock fallback
  delta 0, no ship-gate failures, and the exact selected `GpuAgg`
  resident-boundary reason in the dispatch-evidence row. The artifact writer
  now also emits `resident_boundary_audit.json` and
  `resident_boundary_audit.md` from normalized report data, indexes them in
  `artifact_index.json`, and lists them as completed evidence in
  `resume_audit_manifest.json`; fresh evidence
  `benchmarks/artifacts/h3-res7-resident-audit-artifact-release-100k-cacheboth-20260609`
  records `h3_bulk @ 100K` at 42.04 ms vs 363.44 ms (8.65x) with
  `failed_rows=0` and status `boundary_recorded`. Runner finalization now
  propagates final crash-inventory and report/audit artifact write failures
  instead of downgrading them to stderr-only warnings; focused test coverage
  proves a selected Custom Scan missing resident-boundary evidence writes the
  failing audit files and returns an error. Final post-propagation evidence:
  `benchmarks/artifacts/h3-res7-finalize-failclosed-release-100k-cacheboth-20260609`
  records `h3_bulk @ 100K` at 42.86 ms vs 365.09 ms (8.52x), stock fallback
  delta 0, `resident_boundary_audit.json` with `failed_rows=0`, status
  `boundary_recorded`, and indexed/listed audit artifacts. This item remains
  open until a full release benchmark artifact set carries the boundary reason
  for every selected strategy row.

### Benchmark harness and artifact hygiene

- Evidence: older runner classification could mark a Custom Scan as
  dispatched even when `EXPLAIN ANALYZE` reported `GPU Dispatched: false`,
  and undercount H3 function/SRF GPU work because it is not a Custom Scan.
- Evidence: several repros are PostgreSQL-native in both modes but still
  show large raw timing gaps, for example `hashjoin_100k_1m @ 100K`
  (`0.40x`), `spatial_sel_90pct @ 100K` (`0.68x`), and
  `reduce_sum_i64 @ 1M` (`0.48x`) after the release-install rerun. The
  harness must explain cache, connection, GUC, plan, and ordering
  differences before using non-dispatch timings as benchmark conclusions.
- Evidence: the 2026-05-14 1M diagnosis found that fresh benchmark
  backends treated `SET pg_accel.enabled = on` as a placeholder GUC until a
  pg_accel SQL function was called. Warmups for many generic workloads could
  therefore run as plain PostgreSQL, while the measured accel phase loaded
  pg_accel via `pg_accel_stats()` immediately before counter capture. Plan
  capture had the same issue and could record native plans from a backend
  where planner hooks were never installed.
- Evidence: after preloading pg_accel before accel-side warmup and plan
  capture, `ssbm_q2_3 @ 1M` remained a planner-declined no-dispatch row:
  `plan_selected=false`, `gpu_kernel_dispatched=false`, zero dispatch
  counters, and identical native `GroupAggregate` plans. The accel-side
  `EXPLAIN ANALYZE` showed planning time around 37-40 ms with hooks enabled,
  versus about 0.7 ms in the previous not-loaded capture and about 0.2 ms
  with `pg_accel.enabled=off` in the same loaded backend. No-dispatch 1M
  losses are therefore planner-hook/admission overhead, not GPU kernel
  losses.
- Evidence: after repairing the SSBM part generator so Q2.3 is no longer an
  empty dimension-filter query, the focused `ssbm_q2_3 @ 1M` repro still
  classified as `planner_declined` with zero GPU counters and measured about
  53 ms accelerated vs 22 ms PostgreSQL parallel. That is the real 1M SSBM
  blocker: no GPU-resident star-schema path is selected, while enabled
  planner hooks add overhead before declining.
- Evidence: the 2026-05-14 full-run pass showed the default 10x/5x suite
  still contains proof lanes that dominate wall time even when they are
  stable. `h3_bulk @ 10M` spent about 76-101s per PostgreSQL baseline sample
  while the accelerated path was about 4.9-5.7s; `h3_resolution_sweep @ 10M`
  spent about 72-88s baseline vs 1.0-1.6s accelerated; and
  `h3_latlng_res15 @ 10M` spent about 64-66s baseline vs 11-12s
  accelerated. Several spatial repro and full-sort parity cells also spend
  multiple seconds per sample at 10M rows.
- Evidence: the 2026-05-18 focused pass recorded no benchmark crashes for
  several still-useful rows: `hash_join @ 100K` (`crash-repro-1779092044`),
  `hashagg_100g @ 1M`
  (`crash-repro-1779092148`), `h3_bulk @ 100K`
  (`crash-repro-1779093355`), `h3_srf_grid_disk @ 10K`
  (`crash-repro-1779093366`), `topk_wide @ 1M`
  (`crash-repro-1779093376`), and `ssbm_q2_3 @ 100K`
  (`crash-repro-1779093387`). The no-dispatch rows are not GPU performance
  conclusions; they prove planner decline and stability.
- Evidence: `gpu_nlj_between @ 50K` is no longer counted as a selected
  no-crash row. The current 2026-06-09 selected-path artifact crashed during
  correctness diff
  (`benchmarks/artifacts/gpu-nlj-between-release-50k-cacheboth-20260609`);
  the fixed release proof is a no-crash native-decline row with threshold
  evidence
  (`benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609`).
- Current state: benchmark artifacts now include `manifest.json`,
  `artifact_index.json`, `artifact_checklist.md`,
  `resume_audit_manifest.json`, `no_dispatch_audit.json`,
  `no_dispatch_audit.md`, `resident_boundary_audit.json`, and
  `resident_boundary_audit.md`, with bounded log policy, run-start log
  offsets, no-dispatch timing/plan evidence, selected-plan
  resident-boundary evidence, and generated evidence inventory captured for
  audit/retry workflows.
- Work: attach correctness diff artifacts to every release benchmark cell and
  wire the generated resume/audit manifest into an actual resume/retry
  entrypoint.
- Acceptance: a full benchmark cannot create unbounded logs, can be resumed
  or audited from saved artifacts, reports every crash/skip without relying
  on terminal scrollback, and keeps the default suite bounded while preserving
  rigorous coverage for long winning/proof lanes.
- Progress (2026-06-05): correctness diff artifacts are captured before
  timing and now propagate into each `WorkloadResult` as
  `correctness_diff_artifact`, with JSON, CSV, and markdown report coverage
  linking the per-cell `correctness_diffs/<workload>-<rows>.json` file. The
  crash inventory and crash-context artifacts also link correctness diff files
  when pre-timing diff capture fails, and correctness scratch tables are
  confined to `pg_temp` so artifact capture cannot drop permanent tables with
  matching names. The resume entrypoint reads `resume_audit_manifest.json`
  plus pre-risk contexts and writes a retry source artifact before rerunning
  crashed cells. This item remains open until a full saved benchmark run
  proves end-to-end resume/audit behavior across crashes, skips, correctness
  failures, and bounded logs.
- Progress (2026-06-09): resume/audit validation now fails closed on stale or
  incomplete artifact directories before retrying crashed cells. The loader
  validates `manifest.json`, `artifact_index.json`, `artifact_checklist.md`,
  `crashes.json`, `crashes.md`, every path listed by
  `resume_audit_manifest.json`, every crash-linked plan/correctness/log
  artifact, and the per-crash pre-risk context manifest entry. Crash context
  files are now categorized as crash evidence in the resume inventory and
  artifact README. Focused tests cover missing pre-risk context, no log
  evidence, missing linked crash artifacts, stale manifest inventory, and
  missing manifest-listed files; `cargo test -p pg_accel_bench` and
  `cargo clippy -p pg_accel_bench --all-targets -- -D warnings` pass. This
  item remains open until a full saved benchmark run proves the validated
  resume path against real crash/skip/correctness-failure artifacts and
  bounded logs.
- Progress (2026-06-10): no-dispatch rows now have durable
  `no_dispatch_audit.json` and `no_dispatch_audit.md` artifacts written next
  to every benchmark report, indexed in `artifact_index.json`, and listed as
  completed evidence in `resume_audit_manifest.json`. The audit uses the same
  normalized dispatch classifier as the report/ship gate, counts clean
  native-decline rows separately from warnings, and records timing skew,
  native plan mismatch, selected Custom Scan no-dispatch, and missing plan
  evidence as machine-readable statuses so native-vs-native timing gaps
  cannot be mistaken for GPU speedups. Focused unit coverage verifies the
  report classification matrix plus artifact generation, indexing, README
  documentation, and resume inventory categorization. The plan-shape
  comparator now ignores pg_accel planner/threshold diagnostic footer lines
  so native-decline proof text does not create false no-dispatch plan
  mismatch warnings. Fresh live release-harness evidence:
  `benchmarks/artifacts/gpu-nlj-between-no-dispatch-audit-crashrepro-50k-cacheboth-20260610`
  records `gpu_nlj_between @ 50K` as `planner_declined` at 1.00x,
  `no_dispatch_audit.json` with one evaluated row, `clean_rows=1`,
  `warning_rows=0`, `plan_mismatch_rows=0`, status `comparable_native`,
  `resident_boundary_audit.json` with `failed_rows=0`, and all audit files
  indexed/listed in the resume manifest. This item remains open until a
  fresh full saved benchmark run proves the new no-dispatch audit artifacts
  on the real release matrix.

## Phase 1 - Stop All Backend Crashes Before Re-Entry

Selected GPU plans that can disconnect PostgreSQL are release blockers. All
previously-known crash families are gated at the planner or kernel layer:

- Grouped aggregation Metal argument-buffer crashes — slab pattern applied
  to all four hashagg kernel lambdas
  (`pgaccel-kernels/src/hash_agg.cpp:393-805`); sort-based path
  Metal-gated off (`hash_agg.cpp:331-334`); grouped `AVG` finalize
  preemptively rejected (host-staged finalize path (deleted in the 2026-07 Phase 3 demolition)).
- Hash join Metal host-pointer probe crashes — host-pointer SYCL probe path
  deleted; kernel is a fail-closed stub
  (`pgaccel-kernels/src/hash_join.cpp:14-46`); planner gate at
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:153-168`.
- Spatial bulk point-in-polygon high-capture lambda crashes — slab pattern
  applied to both simple and cooperative kernels
  (`pgaccel-kernels/src/spatial_dispatch.cpp:295-622`); cold-fork
  regression coverage in `pgaccel-kernels/test/test_fork_cold.cpp:233-264`.
- Parallel partial `SUM(bigint)` reduce worker crash — planner gate at
  `partial_agg.rs` (deleted in the 2026-07 Phase 3 demolition)
  (`parallel_partial_sum_bigint_rejected`) and mirror in
  `preagg_partial.rs` (deleted in the 2026-07 Phase 3 demolition).
- Broad generic `GpuExpr` scan exposure — the 2026-06-09 live plan-shape
  filter found that an earlier selected `GpuAccelScan`/`GpuExpr` path for
  `count(*) FROM bench_gpuexpr_direct_gate WHERE val > ... AND category < ...`
  closed the backend and forced PostgreSQL recovery. The narrow direct
  template-safe scan-to-columnar path is now held behind the hard resident-only
  gate and is covered by
  `plan_shape_direct_gpuexpr_template_scan_stays_native_until_resident`; broad
  standalone and bitmap-adjacent generic GpuExpr candidates still decline with
  `standalone_gpuexpr_no_gpu_pipeline` or `bitmap_heap_gpuexpr_no_gpu_pipeline`
  until their input/output protocol is GPU-resident and crash-free.

If a new backend-crashing shape appears, add it here. Repair work that is
necessary to unlock GPU dispatch for a gated shape (rather than to stop a
crash) lives in the feature phase that owns the shape, not here.

## Phase 2 - AdaptiveCpp Runtime, Metal, CUDA, And Fork Stability

Runtime instability blocks every higher-level feature. This phase turns
AdaptiveCpp/Metal/CUDA behavior into explicit pass/fail evidence instead of
incidental log noise.

### Metal runtime instability, cold-start, and warning noise

- Evidence: native GPU tests passed but emitted repeated AdaptiveCpp JIT
  warnings, large cold first-dispatch spikes, Metal shader unused-variable
  warnings from soft-fp64/SLEEF generated code, and archive-size skips for
  large spatial/H3 kernels.
- Evidence: `just gpu-test` passed during the 2026-05-13 work session, but
  took more than 20 minutes on the cold path and produced no durable
  artifact. Standalone `test_h3` later spent roughly four minutes in
  AdaptiveCpp's Metal emitter for `pgaccel_h3_lat_lng_to_cell_bulk` after
  the source hash changed, then passed cleanly.
- Evidence: the 2026-05-24 noise cleanup added a quiet GPU test runner and
  routed `just gpu-test`, `just gpu-test-cold`, and
  `just gpu-stress-archive` through raw-log-preserving summaries under
  `.pgaccel/logs`. Escalated `just gpu-test-cold h3 300` cleared 161 JIT
  cache entries and passed `test_h3` with `650 passed, 0 failed`; known
  Metal/JIT warning spam was folded into a summary while the raw log was
  preserved.
- Progress (2026-06-09): CTest now registers standalone GPU tests through
  `scripts/filter_gpu_output.py` when Python is available, keeping direct
  `ctest --output-on-failure` runs quiet while preserving raw logs under
  `.pgaccel/logs`. The wrapper now supports timestamped `--log-dir` output,
  and `ctest --test-dir pgaccel-kernels/build -R '^test_h3$'
  --output-on-failure` passed with only CTest pass/fail output; the saved
  raw log `.pgaccel/logs/test_h3-20260609-002306-84090.log` ended with
  `662 passed, 0 failed`.
- Progress (2026-06-09): benchmark reports now retain warmup timing evidence
  instead of hiding first-dispatch/JIT behavior behind measured-only
  statistics. Each `WorkloadResult` carries raw `warmup_iterations` plus
  first/max/post-first accel summaries in `report.json`, matching warmup
  columns in `report.csv`; Markdown emits a `Warmup/JIT Audit` table when
  post-first warmups exceed the recurring-latency thresholds. Focused tests
  cover warmup summary derivation and audit rendering, and
  `cargo test -p pg_accel_bench` plus
  `cargo clippy -p pg_accel_bench --all-targets -- -D warnings` pass.
- Progress (2026-06-09): raw wall-clock reports now carry the benchmark
  harness build profile in `methodology.harness_profile` and Markdown
  methodology output. Debug-mode raw/both runs print an explicit warning
  because high-output workloads pay debug client-side row-drain overhead;
  the refreshed H3 100K/1M artifacts above use `harness_profile=release`.
- Progress (2026-06-09): live PostgreSQL integration tests now share a
  process-local guard through `integration_connection::live_pg_test_lock()`.
  This preserves the real benchmark SQL and permanent fixture names while
  making the default Rust test scheduler safe for stateful live-PG modules.
  Evidence: `PG_ACCEL_TEST_CONNECTION='host=localhost port=28818 dbname=postgres'
  cargo test -p pg_accel_bench --features integration_tests h3_protection_test
  -- --nocapture` passed with 16/16 tests and no `--test-threads=1`; the
  plan-shape live filter now passes with 8/8 tests after the GpuExpr and NLJ
  crash gates were installed; and `parallel_stress_test` passed 4/4 live
  stress cases in 97.77s with no backend disconnects.
- Progress (2026-07-04): current native Metal verification passes after the
  H3 aggregate-construction compiler fix. Local evidence: `cmake --build
  pgaccel-kernels/build --parallel`, `test_h3` (`851 passed, 0 failed`),
  `test_correctness` (`340/340`), `test_hash_join` (`PASS=23 FAIL=0`),
  `test_olap_ssbm` (`570 passed, 0 failed`), and `test_expr_templates`
  (`264 passed, 0 failed`). The release blocker is no longer native
  correctness failure in this slice; it is still cold Metal JIT duration,
  warning noise, archive-size skips for large kernels, and durable stress
  artifacts.
- Progress (2026-07-04 soft-fp64/Metal root-cause pass): fixed the compiler
  failures that were making fp64 look like a workload issue. AdaptiveCpp now
  prunes unreachable soft-fp64 helpers/globals, keeps only live
  emitter-implicit primitives, emits MSL `__attribute__((noinline))` for
  LLVM `noinline` helpers, specializes SLEEF helper pointer address spaces, and
  treats null/undef pointer constants as unknown in PHI address-space
  inference. Evidence: cold fp64 probe matrix (`add`, `mul`, `sqrt`, `sin`,
  `cos`, `asin`, `atan2`, `haversine`) completed with zero mismatches;
  `test_spatial` passed `162/162`; `test_h3` passed `856/856`. Known remaining
  issue is unused generated-MSL builtin-string warnings, not fp64 dispatch
  failure.
- Work: raise or tune `ACPP_METAL_ARCHIVE_MAX_BYTES` for known large
  kernels, fix generated-MSL warning noise at the AdaptiveCpp/soft-fp64
  source where possible, and track first-dispatch latency per kernel in
  benchmark artifacts.
- Acceptance: GPU tests are quiet except for intentional diagnostics,
  benchmark warmup no longer hides recurring multi-second JIT, and no
  resource-leak messages appear in passing Metal runs.

### SLEEF helper address-space specialization

- Scope: outlining SLEEF helpers exposes pointer parameters that need
  per-call-site address-space specialization in MetalEmitter.
- Done (2026-07-04): clone helper functions per observed address-space
  combination in `Emitter.cpp`, including constant-table and thread-local fenv
  call sites.
- Acceptance evidence: the `SF64_DISABLE_SLEEF_INLINE` path builds; fp64
  trig/transcendental probes (`sin`, `cos`, `asin`, `atan2`, `haversine`) pass;
  `test_spatial` and `test_h3` no longer fail with pointer address-space
  mismatches.

### Per-fork JIT latency

- Scope: first dispatch after fork can spend hundreds of milliseconds in
  JIT/cache work.
- Work: diff `kernel_configuration` hash inputs pre- and post-fork; if
  stable, investigate mmap or parent-loaded metallib reuse.
- Acceptance: 10-child fork stress shows first-dispatch JIT wall time at or
  below 50 ms, or the limiting cost is conclusively explained.

### Out-of-order executor overlap

- Scope: sort and window execution currently use in-order Metal queues.
- Work: add per-DAG dependency tracking with `MTLSharedEvent` /
  `submit_queue_wait_for`.
- Acceptance: trace spans show overlapping GPU work and measured wall-time
  improvement.

### AdaptiveCpp emitter polish

- Scope: remaining fork-maintenance items include forward-declaration
  volume, removal/suppression of unused builtin-string constants, fine-grained
  replacement for soft-fp64 `optnone`, ReplaceIntrinsics fixpoint validation,
  and robust soft-fp64 preservation matching.
- Acceptance: each item has a focused AdaptiveCpp commit plus shader-size,
  compile-time, or correctness evidence.

### soft-fp64 adapter coverage matrix

- Scope: every `__acpp_sscp_*_f64` forwarder needs a positive test that
  reaches generated MSL source.
- Acceptance: AdaptiveCpp has a coverage-matrix test for all fp64
  forwarders.

### soft-fp64 math precision validation

- Scope: cross-check GPU-dispatched soft-fp64 math against CPU soft-fp64 and
  MPFR at the tolerances documented in soft-fp64 `v1.3.0`.
- Acceptance: arithmetic / compare are bit-exact, u10 functions are within
  4 ULP, u35 functions are within 8 ULP, and failures block `fp64_matrix`.

### Metal shader warning sweep

- Scope: emitted MSL should compile cleanly under stricter warnings.
- Acceptance: `-Wall` / `-Wextra` warning classes are triaged or suppressed
  with justification.

### Metal runtime debug knobs

- Scope: settle `ACPP_METAL_KEEP_SOURCE`, `ACPP_METAL_DUMP_IR`, fast-math
  semantics for fp64 bodies, and buffer-argument scale testing.
- Acceptance: debug env vars have a documented owner or are removed, fp64
  fast-math behavior is verified, and buffer-index limits are tested.

### Cross-backend parity

- Scope: Metal-specific AdaptiveCpp changes must not regress CUDA, ROCm, or
  Level Zero.
- Acceptance: `test_reduce_stats` or an equivalent parity suite passes on
  representative native-fp64 hardware for each backend.

### AdaptiveCpp `DEFAULT_TARGETS` JSON serialization

- Scope: CMake list values such as `omp;metal` serialize incorrectly in the
  generated AdaptiveCpp JSON config.
- Acceptance: AdaptiveCpp accepts multi-target defaults without the
  `ompmetal` concatenation bug.

## Phase 3 - GPU-Resident Execution Substrate

This phase creates the common substrate needed for PG-Strom-class plans:
columnar batches, GPU expression evaluation, GPU-resident intermediate data,
retained buffers, and truthful EXPLAIN/runtime counters.

### PG-Strom-shaped execution model

- Goal: implement GPU-resident `GpuScan -> GpuJoin -> GpuPreAgg` pipelines
  with expression/filter/projection pushdown, join-side reuse, pruning,
  final merge/rank/top-k pushdown, and reduced result transfer back to
  PostgreSQL.
- Keep and build around this shape: `GpuScan`, `GpuJoin`, `GpuPreAgg`, GPU
  expression evaluation, GPU hash/group aggregation, GPU sort as an internal
  primitive, H3/PostGIS/raster kernels, BRIN/GiST-style pruning, spatial
  joins, GPU cache / retained inner buffers, and a columnar batch format that
  feeds multiple operators without round-tripping through heap tuples.
- Planner admission rule: a path may enter normal planning only if it
  consumes GPU/columnar batches, keeps intermediate data GPU-resident,
  substantially reduces output cardinality, or performs genuinely
  compute-heavy Geo/H3/raster work.
- Acceptance: EXPLAIN output and benchmark artifacts distinguish
  `plan_selected`, `gpu_kernel_dispatched`, `gpu_resident_pipeline`, and
  `rows_returned_to_cpu`.

### GpuScan expression/filter/projection pushdown

- Scope: build generic numeric, boolean, date/time, and supported PostGIS/H3
  predicate/projection pushdown for scan batches.
- Rule: standalone expression wrappers over PostgreSQL-native child plans
  remain unavailable.
- Acceptance: scan predicates and projections dispatch GPU expression
  kernels, match PostgreSQL semantics for NULLs and supported operator
  classes, and decline unsupported shapes visibly.
- Progress (2026-06-05): scalar `h3_latlng_to_cell` base-relation predicates
  now stay native with explicit planner-decline evidence instead of reaching
  standalone scan-filter exposure. Bad argument shapes, including invalid
  resolutions, non-constant resolutions, and non-point-column arguments,
  record `h3_latlng_unsupported_shape`; valid scalar predicate wrappers,
  including equality, `AND`, `IS TRUE`, `CASE`, and `COALESCE`, record
  `h3_latlng_scalar_predicate_no_gpu_pipeline`. The detector now runs before
  the generic min-batch row gate, the stale standalone H3 scan admission helper
  was removed, a PG18 pgrx regression covers the visible declines, and the H3
  protection integration guard compiles behind `integration_tests`. This item
  remains open until fused GPU expression filtering owns H3 scalar outputs and
  the surrounding comparison/null-test semantics.
- Progress (2026-06-12, superseded by 2026-06-14 hard GPU-only gate): direct
  template-safe numeric `GpuExpr` scans were rebuilt for the narrow
  scan-to-columnar template path, but selected SQL plans now require a proven
  GPU-resident pipeline. The PG18 guard
  `plan_shape_direct_gpuexpr_template_scan_stays_native_until_resident` proves a
  representative `val > const::float4` query stays native, records
  `no_gpu_resident_pipeline`, matches PostgreSQL, and dispatches no GPU work.
  Broad standalone and bitmap-adjacent `GpuExpr` remain declined with
  `standalone_gpuexpr_no_gpu_pipeline` or `bitmap_heap_gpuexpr_no_gpu_pipeline`
  until the input/output protocol is GPU-resident and crash-free.

### Fused scan plus partial aggregate

- Scope: build real GPU scan+partial-reduce lanes for `parallel_sum`,
  `parallel_avg_stddev`, and typed multi-reduce workloads.
- Rule: aggregate wrappers over PostgreSQL-native child plans remain
  unavailable.
- Acceptance: aggregate audit rows report GPU Custom Scan plans selected by
  PostgreSQL, EXPLAIN ANALYZE shows actual GPU dispatch, and corresponding
  benchmark cells are at or above PostgreSQL parallel parity.
- Progress (2026-06-05): parallel aggregate shapes over PostgreSQL CPU
  partial scans now stay native with exact
  `partial_agg_no_gpu_producing_child` planner-decline evidence. This item
  remains open until partial aggregate CustomPaths consume GPU-producing
  children or direct GPU-owned scans without wrapping CPU parallel scans.
- Progress (2026-06-12): serial fused `COUNT(*) WHERE template_predicate` now
  has a real `GpuAgg`-owned scan/count path for `int2`/`int4`, exact-constant
  `int8`, `float4`, and `float8` template predicates; row-returning `GpuExpr`
  still declines `int8` until typed constants replace the f64-erased template
  ABI. Direct shared-USM staging is enabled on Metal with the rebased
  AdaptiveCpp fork and now has single-/two-predicate, multi-workgroup,
  null-mask, NaN, strided count, and exact-constant bigint C++/PG18 coverage.
  Serial direct-USM staging now reuses executor-local value buffers across
  batches and hides stale null masks on later all-valid batches. Serial target
  sizing now uses the relation row estimate up to the existing 4,194,304-row
  cap, so the current 2M smoke runs in one GPU batch: single-predicate fused
  `GpuAccelAgg` 46.634 ms vs 105.528 ms native serial (2.26x);
  two-predicate fused `GpuAccelAgg` 52.063 ms vs 98.981 ms native serial
  (1.90x), with 2M GPU rows processed, zero uncertain rows, and no stock
  fallback.
  Parallel partial fused count has DSM/`ParallelTableScanDesc` plumbing and
  EXPLAIN DSM-total counters, but it is crash-gated after a fresh PG18 GUC-on
  run produced a Metal worker SIGSEGV. `pg_accel.parallel_fused_count` now
  records `parallel_fused_count_unstable` and keeps the query native even when
  enabled; GUC-off records `parallel_fused_count_disabled`. Non-ignored tests
  assert both native fallback modes, zero GPU kernel delta, zero stock
  fallback, and exact native count parity. Re-enable planner admission only
  after worker no-crash proof, then reinstate active-participant and speedup
  gates against PostgreSQL parallel.

### GPU-resident join build/probe and retained inner reuse

- Scope: build real GPU join build/probe with GPU-resident retained inner
  buffers and batched probe output.
- Rule: selected `GpuHashJoin` must use GPU-resident buffers and must not
  depend on unsafe Metal host-pointer sort-merge probes.
- Acceptance: join audit rows report GPU Custom Scan plans with dispatch,
  build-side reuse evidence, correct results, and benchmark parity or better.

### GPU sort as an internal primitive

- Scope: keep GPU sort available for GPU-resident top-k, rank filters,
  grouped finalization, merge/join support, and final result ordering after
  cardinality reduction.
- Rule: full-output standalone heap sort stays unavailable until it wins
  end-to-end.
- Acceptance: top-k and internal sort consumers dispatch the intended GPU
  algorithm and prove that output materialization cost is included.

### Reduce per-batch dispatch cost

- Scope: cheap reduce and grouped aggregation still risk losing to
  PostgreSQL parallel execution when per-batch Metal dispatch dominates.
- Preferred fixes, in order: command-buffer reuse across a worker batch
  stream; scan+reduce kernel fusion; executor-side buffering into fewer,
  larger batches.
- Constraint: do not hide failures by raising `min_batch_size`.
- Acceptance: the reduce / grouped-agg row-count matrix
  `[100k, 1M, 10M, 100M, 1B]` is at or above PostgreSQL parallel via GPU plan
  selection, with trace spans proving fewer or cheaper dispatches.

## Phase 4 - Core OLAP Coverage And PG-Strom Parity

This phase covers the relational execution surface that must exist before
public release: scan, join, pre-aggregation, aggregate semantics, sort, and
window work.

### Real GPU `GpuPreAgg`

- Scope: build real GPU `PreAgg` from the star-schema recognizer.
- Evidence: the 2026-05-14 1M diagnosis shows SSBM is not a GPU performance
  result yet. `ssbm_q2_3 @ 1M` selects no pg_accel path, dispatches zero GPU
  kernels, and still loses because planner hooks add tens of milliseconds
  before declining to the same native PostgreSQL plan.
- Evidence: before the SSBM part-generator repair, the synthetic fixture was
  invalid for Q2.3: `p_brand1 = 'MFGR#2239'` matched zero `ssbm_part` rows
  because mfgr/category/brand were correlated by `i % 5` and `i % 40`.
  Q2.1 and Q4.3 category filters had the same risk. The generator now varies
  mfgr, category, and brand independently enough for the benchmark constants,
  but cardinality checks still need to be enforced in the harness.
- Work: support dimension joins, group keys, partial aggregates,
  cardinality reduction, GPU-resident fact batches, and finalization without
  heap walking under a pg_accel plan name.
- Work: add report sanity checks that flag zero-row dimension filters before
  timing, then keep SSBM work focused on the missing GPU-resident `GpuPreAgg`
  path rather than treating no-dispatch rows as GPU losses.
- Acceptance: star-schema benchmark queries select `GpuPreAgg`, dispatch GPU
  kernels, match PostgreSQL output, and beat PostgreSQL parallel plans.
- Progress (2026-06-05): grouped join aggregate shapes now have PG18
  regression coverage that keeps the disabled serial PreAgg scaffold out of
  normal planning and exposes the exact `preagg_no_gpu_resident_pipeline`
  native-decline reason. This item remains open until a real GPU-resident
  star-schema PreAgg path dispatches kernels, passes correctness diffs, and
  wins benchmark cells.

### Grouped hash aggregation

- Existing state: the remaining Metal `agg_hash` kernel
  (`pgaccel-kernels/src/hash_agg.cpp:2467`) is O(n*g) — one
  work-item per group scanning all n rows. At 1M rows × 4096 groups
  that is ~4 billion ops, at 10M × 10K it is ~100 billion ops. The
  100K-row planner gate (`formulas.rs:120-122
  hashagg_input_rows_safe`) and 4096-group cap (`hash_agg.cpp:1247
  HASH_AGG_MAX_LARGE_UNSORTED_GROUPS`) are protective.
- Evidence: the H3 count fast-path work on 2026-05-18 tried two
  Metal-targeted open-addressing kernels: a claimed/full state table and
  an owner-row-index table using only 32-bit atomics. AdaptiveCpp's Metal
  output generated uninitialized MSL around `atomic_compare_exchange` and
  produced no valid groups (`first_owner=UINT32_MAX`, `first_count=rows`),
  so Metal remains on the GPU sort-backed count path while CAS lowering is
  fixed or avoided.
- Progress (2026-06-10): fused H3 parent grouped count now uses a
  row-parallel device hash-count entry point over USM-resident parent keys,
  with a domain-specific max-distinct hint (`2 + 120 * 7^parent_res`) so the
  hash table is not sized as if every input row can be distinct. The parent
  kernel precomputes the child-digit mask once per call and uses a single
  device invalid flag instead of a host-side per-row validity scan. A more
  aggressive "record occupied slots during insert" compaction was tested and
  rejected after live PG correctness closed the backend connection; do not
  resurrect it without a device-race proof and backend crash artifact.
- Evidence: the no-CAS direct H3/grouped-count route is correct and wired
  for resolution < 8, but it does not beat the earlier staged best. Forced
  direct H3/grouped count measured roughly 111-118 ms at 100K
  (`benchmarks/artifacts/crash-repro-1779148975`,
  `benchmarks/artifacts/crash-repro-1779149150`,
  `benchmarks/artifacts/crash-repro-1779149310`,
  `benchmarks/artifacts/crash-repro-1779149487`); after restoring it as the
  default pure route, adding a values-only `u64` sort, skipping the H3
  `VectorizedScan` arena copy, tuning resolution-7 exact fixups, moving
  f32 staging into the direct heap point extraction pass, dropping the
  unused direct-extraction null-mask Vec, and preallocating the direct
  extractor staging buffers from relation stats, current release-harness
  cache-mode-both evidence is 47.81 ms at 100K
  (`benchmarks/artifacts/h3-res7-prealloc-release-100k-cacheboth-1780992523`)
  and 345.39 ms at 1M
  (`benchmarks/artifacts/h3-res7-prealloc-release-1m-cacheboth-1780992542`)
  versus
  the earlier staged best of 38.55 ms and 270.30 ms.
- Work: replace `agg_hash` with a real parallel hash-table kernel —
  open-addressing in shared memory, atomic CAS for slot acquisition,
  `atomic_ref<double>` accumulators (supported on Metal via
  AdaptiveCpp), one work-item per row instead of one per group.
  Cover `SUM`, `COUNT`, `MIN`, `MAX`, and the typed integer/floating
  variants.
- Work: reduce a minimal AdaptiveCpp/Metal CAS repro from the owner-row
  count kernel or design a no-CAS count/grouping primitive that beats the
  staged H3 path before making it selectable. Keep high-cardinality
  duplicate-count tests green before reopening CAS-backed Metal hashagg
  admission.
- Work: fix grouped `AVG` finalize. Either route grouped `AVG`
  through partial-mode always (kernel already emits `[N, sum]` lanes
  at `hash_agg.cpp:911-920`), or extend `emit_grouped_tuple`
  (the deleted host-staged grouped emit path) to read the per-group counts buffer
  (`gr.result.counts()`, `hash_agg.cpp:67`) and divide.
- Work: remove the planner gate and the 4096-group cap only after
  the new kernel benchmarks well at 1M/10M for
  `num_groups ∈ {10, 100, 1K, 10K}`, and rewrite the gating tests
  in `planner_hooks/tests.rs:1315-1345`.
- Acceptance: `grouped_agg`, `grouped_agg_high_card`,
  `gpu_hashagg_med_card`, `hashagg_10g`, `hashagg_100g`,
  `hashagg_1kg`, and `hashagg_10kg` complete at 1M and 10M with GPU
  dispatch, correctness diffs against PostgreSQL, and speedup at or
  above 1.0 where selected.

### GPU-resident hash join build/probe

- Current state: selected `GpuHashJoin` is wired to a real INT32/INT64 GPU
  build/probe kernel, and `selected_gpu_hashjoin_kernel_available()` returns
  `true` (`pgaccel-kernels/src/hash_join.cpp:1-12`,
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:455-458`). The path
  is still not pure GPU: row-returning joins are capped because the executor
  reconstructs joined rows into PostgreSQL slots
  (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:19-26`, `:238-268`),
  build/probe still stages child rows and key/null buffers on the host
  (`pg_accel/src/engine/executor/join/probe.rs:327-554`, `:584-911`), and
  partial injection duplicates work per worker (`join_pathlist.rs:434-452`,
  `:1121-1255`).
- Work: move build-side keys and payloads into retained device buffers, probe
  directly from GPU/columnar child batches, emit device pair buffers or
  aggregate counts without host `MinimalTuple` reconstruction, and share/reuse
  build-side state across workers where safe. Row-returning plans should remain
  capped or declined until GPU gather/projection or join->preagg keeps output
  device-resident.
- Acceptance: join sweep has no crashes; selected GPU plans have correctness
  diffs and speedup at or above 1.0; `HashJoinTelemetry.redundant_inner_builds`
  proves the intended reuse model; high-output joins either feed GPU-resident
  preagg/semi/anti paths or decline with `hashjoin_heap_output_too_large`.
- Progress (2026-06-05): high-output row-returning hash joins now stay
  PostgreSQL-native with exact `hashjoin_heap_output_too_large`
  planner-decline evidence, protecting release runs from selecting a
  row-reconstructing heap `GpuHashJoin` where output materialization dominates.
  This item remains open until joined rows feed GPU-resident preagg/projection
  or benchmark artifacts prove selected row-returning joins win.

### GPU semi/anti join and Bloom prefilters

- Scope: common `IN`, `EXISTS`, `NOT EXISTS`, and semi/anti join shapes can
  be cheaper GPU wins because they do not need full joined-row
  materialization.
- Work: build GPU-resident membership filters or Bloom filters from the
  inner side, push them into `GpuScan` / `GpuJoin` pipelines, and return only
  qualifying outer rows or counts.
- Acceptance: representative semi/anti join queries dispatch GPU membership
  work, avoid full join-output reconstruction, match PostgreSQL semantics for
  NULLs and anti joins, and beat PostgreSQL parallel plans where selected.
- Progress (2026-06-05): representative `EXISTS` and `NOT EXISTS` shapes now
  stay PostgreSQL-native with exact `semianti_no_gpu_membership_filter`
  planner-decline evidence instead of routing through row-returning
  `GpuHashJoin` heap reconstruction. This item remains open until GPU
  membership/Bloom-filter semantics are implemented and benchmark-proven.
- Progress (2026-07-15): the resident-v2 release matrix now covers `EXISTS`,
  `IN`, `NOT EXISTS`, and NULL-poisoned `NOT IN` with deterministic duplicate
  and NULL fixtures. The first three lanes decline with
  `no_gpu_resident_pipeline`; `NOT IN` reaches the sublink shape gate and
  declines with `shape_sublink`. Every cell is native-decline-only and
  requires its typed exact result oracle, no CustomScan, and a captured zero
  GPU-kernel counter delta.

### NestedLoop inequality pure-GPU follow-up

- Current state: selected `GpuNestedLoopIneq` BETWEEN is disabled for release.
  The previous `gpu_nlj_between @ 50K` win
  (`benchmarks/artifacts/crash-repro-1779092055`) is superseded by a 2026-06-09
  release-harness crash during selected-path correctness diff
  (`benchmarks/artifacts/gpu-nlj-between-release-50k-cacheboth-20260609`).
  The planner gate now returns `false`
  (`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:497-507`), and the
  historical benchmark threshold matrix pinned `gpu_nlj_between @ 50K` as
  native-decline with `nlj_between_host_boundary_unsafe`. Current passing proof:
  `benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609`
  records no crash, zero GPU dispatch, `planner_declined`, and a passing
  threshold-matrix row.
- Gap: the path is not pure GPU or release-safe. The executor still collects
  both child inputs through `ExecProcNode`, host-extracts/remaps key arrays,
  copies matched tuples into pending pairs, and reconstructs joined rows from
  `MinimalTuple` pairs
  (`pg_accel/src/engine/executor/join/nlj.rs:83-96`, `:127-140`, `:227-239`,
  `:292-319`, `:323-427`).
- Work: add GPU-resident input batches, a device pair buffer and
  projection/gather path, fused count/preagg consumers, benchmark rows at
  10K/100K/1M, and planner admission by measured output/cardinality.
- Acceptance: selected NLJ either stays row-count bounded with no crash and
  speedup at or above 1.0, or feeds a GPU-resident downstream consumer without
  host pair materialization.
- Progress (2026-06-05): oversized `BETWEEN` NLJ shapes now stay
  PostgreSQL-native with exact `nlj_between_output_too_large`
  planner-decline evidence. This item remains open until selected
  `GpuNestedLoopIneq` output is bounded by benchmarked admission or feeds a
  GPU-resident downstream consumer without host pair materialization.
- Progress (2026-06-09): selected `GpuNestedLoopIneq` BETWEEN is crash-gated.
  The release harness now classifies `gpu_nlj_between @ 50K` as a
  native-decline threshold-matrix row with `nlj_between_host_boundary_unsafe`
  instead of selecting the host-boundary Custom Scan.
- Progress (2026-06-09): live plan-shape integration now pins the crash gate:
  `plan_shape_nlj_between_host_boundary_stays_native` builds the BETWEEN
  fixture, verifies the native count matches with pg_accel enabled, rejects
  `Custom Scan`/`GpuAccelJoin` plan text, accepts the native-decline reason,
  and asserts the GPU kernel counter remains flat.
- Progress (2026-07-15): the current resident-v2 contract supersedes the
  historical host-boundary reason with `shape_unsupported_predicate` at every
  preserved `gpu_nlj_between` scale. Its deterministic nullable, duplicated
  BETWEEN fixture has an exact match-count digest, and both report and runner
  ship gates reject CustomScan selection or any GPU-kernel counter delta.
  Live coverage is named
  `plan_shape_nlj_between_unsupported_predicate_stays_native`.

### Aggregate FILTER / DISTINCT / ordered semantics

- Scope: aggregate paths currently reject `FILTER`, `DISTINCT`, and
  aggregate-local `ORDER BY`; these are common analytics shapes and should
  become GPU lanes where the semantics fit existing expression, hash, sort,
  or selection primitives.
- Work: fuse `FILTER` predicates into the aggregate input mask; implement
  `COUNT(DISTINCT)`, `SUM(DISTINCT)`, and related forms through GPU hashset
  or sort-unique primitives; evaluate ordered aggregates through GPU
  sort/select where they reduce output cardinality enough to pay for staging.
- Planner rule: do not expose these shapes as selected GPU plans until the
  executor owns the full semantic path and can prove dispatch.
- Acceptance: filtered, distinct, and ordered aggregate regression tests
  match PostgreSQL for NULLs, duplicates, collations/order-sensitive cases
  where applicable, and benchmark-selected cells are at or above PostgreSQL
  parallel parity.
- Progress (2026-06-05): aggregate `FILTER`, `DISTINCT`, and aggregate-local
  `ORDER BY` shapes stay PostgreSQL-native and expose
  `agg_semantic_modifier_no_gpu_kernel` instead of selecting a partial
  `GpuAgg` path that would ignore the modifier semantics. This item remains
  open until those modifiers have implemented GPU semantic paths and
  correctness/performance evidence.
- Progress (2026-07-15): the resident-v2 disposition is
  `shape_aggregate_modifier`. Deterministic workloads cover combined
  `FILTER`/`DISTINCT`/aggregate-local `ORDER BY` and actual
  `percentile_disc(...) WITHIN GROUP`, including NULLs and duplicates. Both
  are native-decline-only and require exact result digests, no CustomScan,
  and zero GPU-kernel counter delta.

### Full sort algorithm and cost gating

- Evidence: full scalar sorts lost consistently, and 10M rows lost severely:
  integer and float variant sorts were about 21-22s accelerated vs about
  2.1-2.8s PostgreSQL parallel. `large_sort @ 10M` was about 28s vs
  5.5-5.9s. Top-K and multikey cases were closer to parity.
- Current evidence: C++ `test_sort_bench 100000` passed radix edge cases for
  f32/f64 and key-value sort, but cold scalar int sort still took about
  10.8s for 100K rows versus about 1.7ms for `std::sort`; key-value int sort
  was much closer at 4.5-7.1ms.
- Work: keep bounded single-key top-k eligible only where benchmark cells
  prove parity or better; add cost-model terms for row width, limit, key
  type, algorithm, chunk count, cold JIT, and full-output materialization.
  Multi-key top-k waits for cascaded stable GPU sort.
- Acceptance: full sorts either produce PostgreSQL-native plans or beat
  PostgreSQL in the benchmark matrix with real GPU dispatch; top-k remains
  independently measured.
- Progress (2026-06-05): full-output standalone `ORDER BY` shapes now stay
  PostgreSQL-native with exact `sort_heap_full_output` planner-decline
  evidence instead of exposing heap-backed `GpuSort` for known loser lanes.
  This item remains open until full-output sorts either benchmark as native
  declines or dispatch a GPU-resident sort/gather path that beats PostgreSQL.

### Window executor partial path

- Scope: `ROW_NUMBER` / `RANK` over a Gather child currently runs on the
  leader after collecting worker output.
- Work: add a parallel-safe hook per window spec; inject a partial-window
  CustomPath when `PARTITION BY` aligns with worker distribution.
- Acceptance: EXPLAIN shows eligible partitioned window work running inside
  workers rather than only on the leader.
- Progress (2026-06-05): parallel window input shapes now stay native with
  the exact `window_partial_path_no_parallel_hook` planner-decline reason.
  This item remains open until a worker-local/partition-aware window hook is
  implemented and EXPLAIN/runtime artifacts show eligible window work running
  inside workers.

### Segmented window kernels

- Scope: running `COUNT`, `SUM`, `AVG`, `RANK`, and `DENSE_RANK` should use
  GPU algorithms with linear or near-linear work per partition rather than
  one work item scanning from the partition start for every output row.
- Work: implement segmented prefix scans for additive windows, transition
  flag prefix scans for rank/dense-rank, and partition-aware batch handling
  for rows whose frame crosses a batch boundary.
- Planner rule: keep large-partition window paths gated unless the selected
  kernel is the segmented implementation and the benchmark matrix proves
  parity or better against PostgreSQL parallel execution.
- Acceptance: large single-partition and many-partition window benchmarks
  dispatch segmented kernels, match PostgreSQL output for NULLs, peer groups,
  frame bounds, and ordering ties, and show measured speedups at selected
  thresholds.
- Progress (2026-06-05): representative `ROW_NUMBER` window shapes now stay
  PostgreSQL-native with exact `window_function_no_segmented_kernel`
  planner-decline evidence. This item remains open until segmented window
  kernels are implemented, selected, correctness-proven, and benchmarked.
- Progress (2026-07-04): Metal now fails closed for the legacy non-segmented
  `COUNT`, `SUM`, `RANK`, and `DENSE_RANK` C++ kernels instead of submitting
  O(N^2) large-partition work that can trip the command-buffer interactivity
  watchdog. The planner also declines Metal `GpuWindow` paths with
  `window_function_no_segmented_kernel` until segmented kernels replace the
  legacy prefix scans. Focused evidence: `test_window` passes with row_number
  still dispatching and the non-segmented Metal paths returning
  `PGACCEL_ERROR_NO_DEVICE`.
- Progress (2026-07-15): the release breadth matrix now captures deterministic
  `ROW_NUMBER`, peer-sensitive `RANK`/`DENSE_RANK`, running `COUNT`/`SUM`/`AVG`,
  and combined reducing-window fixtures at every preserved workload scale.
  The current resident-v2 planner disposition is
  `no_gpu_resident_pipeline`; exact peer/NULL/order digests, no CustomScan,
  and zero GPU-kernel counter delta are mandatory before these cells count as
  captured declines.

## Phase 5 - Geo, H3, Raster, And PostGIS Coverage

This phase covers the non-relational compute-heavy lanes that should be
pg_accel strengths: H3, spatial predicates/joins, geometry constructors,
raster map algebra, and prepared geometry structures.

### H3 bulk aggregation

- Scope: keep the `h3_bulk` grouped aggregate lane on a faithful H3 Core
  implementation. The lat/lng-to-cell kernel must be generated from or kept
  traceable to the H3 C source that h3-pg wraps; do not reintroduce
  approximate face/base-cell rewrites for this path.
- Current state: resolution 7 `h3_bulk` is correct against h3-pg on a
  deterministic aggregate diffs and wins the focused benchmark on M2/Metal.
  The executor no longer uses the Rust `HashMap` grouping path or intermediate
  host cell/key vectors for resolution < 8: it calls fused
  `pgaccel_h3_lat_lng_count_bulk_f32_exact`, which runs GPU
  lat/lng-to-cell into a shared key slab and then a GPU sort-backed grouped
  count through `pgaccel_hash_count_i64_device_execute`. The 100K timing
  regression was recovered by direct heap point extraction that skips the
  `VectorizedScan` arena-copy pass for H3 grouped count. A resolution-7 fp32
  edge detector gap found at 1M rows is covered by native h3-pg regression
  coordinates and a tuned conservative exact-fixup margin, so large random
  samples no longer leak one-cell H3 boundary flips.
- Progress (2026-06-09): refreshed release-harness cache-mode-both evidence
  after f32 staging, direct null-mask removal, and relation-stat preallocation
  is 7.68x at 100K, 47.81 ms vs 367.34 ms, and 18.43x at 1M, 345.39 ms vs
  6364.24 ms, with zero correctness diff mismatches, GPU dispatch evidence,
  consumed output rows, warmup summaries, `harness_profile=release`, and stock
  fallback delta 0
  (`benchmarks/artifacts/h3-res7-prealloc-release-100k-cacheboth-1780992523`,
  `benchmarks/artifacts/h3-res7-prealloc-release-1m-cacheboth-1780992542`).
  The correctness artifacts report `pass`, matching row counts, and zero
  accel-minus-baseline / baseline-minus-accel rows at both scales. A
  debug-harness rerun reproduced the old ~113 ms 100K band because raw
  wall-clock timing includes client-side row draining; direct `psql` and the
  release harness stayed in the 45-50 ms band. The six deterministic res7
  edge coordinates are now covered in the C++ H3 bulk, fused grouped-count,
  and f32/exact grouped-count harnesses, and by the live PG integration
  regression `h3_grouped_count_resolution_matrix_matches_native_h3`, which now
  keeps the grouped SQL lane native under the hard resident-only gate, records
  `no_gpu_resident_pipeline`, dispatches no pg_accel kernels, consumes every
  fixture row, and matches grouped counts against stock h3-pg for resolutions
  0, 7, 9, and 15.
- Progress (2026-06-09): the release benchmark/report-level grouped H3 matrix
  now includes res9 and res15 seed artifacts in addition to res7. Res9
  `h3_resolution_sweep` records 6.62x at 100K, 52.13 ms vs 344.89 ms, and
  15.00x at 1M, 389.90 ms vs 5847.52 ms
  (`benchmarks/artifacts/h3-res9-release-100k-cacheboth-1780993419`,
  `benchmarks/artifacts/h3-res9-release-1m-cacheboth-1780993439`). The
  high-resolution res15 grouped workload was repaired to use the
  planner-supported `geom point` group-key shape; it now dispatches
  `GpuAccelAgg` and records 4.35x at 100K, 89.31 ms vs 388.43 ms, and 8.44x
  at 1M, 774.97 ms vs 6540.21 ms
  (`benchmarks/artifacts/h3-res15-release-100k-cacheboth-1780994043`,
  `benchmarks/artifacts/h3-res15-release-1m-cacheboth-1780994068`). All four
  artifacts have `harness_profile=release`, cache-mode both, correctness
  `pass` with zero diffs, runtime dispatch counters, consumed output rows, and
  stock fallback delta 0. The separate fp64 calibration row `h3_fp64_ops`
  remains classified native/no-dispatch until expression-aggregate H3 dispatch
  exists, so it no longer pollutes the grouped H3 winner lane.
- Progress (2026-06-10): `h3_cell_to_parent(cell, const), COUNT(*) GROUP BY 1`
  is now a selected H3 winner lane, distinct from standalone scalar
  `h3_cell_to_parent`. The planner recognizes a Var h3index input plus const
  parent resolution, serializes the synthetic parent group key through Custom
  Scan private data, and the executor direct-scans the h3index column into the
  fused parent-count kernel. The stable release artifacts record 1.38x at
  100K and 1.21x at 1M against stock h3-pg with correctness diff `pass`,
  kernel counter deltas, consumed output rows, and threshold-matrix pass:
  `benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-100k-cacheboth-20260610011605`
  and
  `benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-1m-cacheboth-20260610011614`.
  The standalone scalar projection guard remains native with no GPU counter
  delta, and the benchmark/report classifier now labels this kernel as
  `h3_cell_to_parent` rather than the lat/lng family.
- Progress (2026-06-09): the f32/exact grouped-count API now has
  all-resolution standalone coverage over duplicated pole, antimeridian,
  face-center, face-edge midpoint, and deterministic random inputs. The new
  `test_lat_lng_count_bulk_f32_exact_all_res_edge_randomized` case compares
  `pgaccel_h3_lat_lng_count_bulk_f32_exact` against the fp64 cell reference
  for resolutions 0 through 15 and preserves duplicate group counts. Focused
  wrapper evidence passed with `177 passed, 0 failed`
  (`.pgaccel/logs/test_h3-f32-allres-refactor-20260609-003343-84484.log`),
  and full quiet CTest H3 evidence passed with `839 passed, 0 failed`
  (`.pgaccel/logs/test_h3-20260609-003356-84515.log`).
- Work: keep the pure route honest while broadening admission beyond the
  current benchmarked grouped lanes. The current all-GPU-ish sort/reduce still pays
  for eight-pass radix sorting, host-scanned per-block histogram/prefix
  metadata, and high-cardinality result materialization; the work-group count
  compactor did not improve 100K or 1M wall time. Next candidates are a real
  GPU prefix/scan primitive for radix/group offsets, an H3-specialized
  duplicate detector that avoids full count compaction when nearly all cells
  are unique, or a Metal-safe hash aggregate once the CAS issue in grouped hash
  aggregation is resolved. Live PG grouped-count coverage now spans
  representative resolutions 0, 7, 9, and 15, and the release benchmark/report
  seed matrix covers the current benchmarked grouped H3 winner lanes at res7,
  res9, and res15. Add a res0 release artifact only if res0 becomes a normal
  benchmark lane. Standalone C++ coverage already covers resolutions 0-15, face
  edges, poles, antimeridian-adjacent points, and duplicate preservation.
- Acceptance: no crashes, zero h3-pg diff rows, and warm benchmark evidence
  for each enabled H3 bulk resolution. The planner should select this path
  only where end-to-end output cardinality and grouping cost still beat
  PostgreSQL parallel execution.

### H3 LATERAL SRF expansion

- Scope: accelerate table-correlated variable-output H3 functions such as
  `h3_grid_disk`, `h3_cell_to_children`, `h3_polyfill`, and boundary /
  multipolygon emitters when used through `CROSS JOIN LATERAL` or equivalent
  per-row expansion.
- Work: add a planner/executor path that batches outer-row arguments, runs
  the variable-output H3 kernel once per batch, emits a row-id/offset table
  for expansion, and preserves PostgreSQL SRF semantics for empty, NULL, and
  multi-output cases.
- Correctness gate: prove every exposed variable-output H3 operation against
  h3-pg on randomized inputs, edge cells, pentagons, polar coordinates,
  antimeridian polygons, and NULL-heavy batches before counting the lane as a
  GPU win.
- Acceptance: representative LATERAL H3 expansion queries dispatch H3
  kernels, match h3-pg output including ordering/NULL semantics where
  PostgreSQL requires them, and beat PostgreSQL native execution at measured
  thresholds.
- Progress (2026-06-05): correlated `h3_grid_disk` LATERAL shapes now stay
  PostgreSQL-native with exact `h3_lateral_srf_no_batched_expansion`
  planner-decline evidence. This item remains open until table-correlated
  variable-output H3 expansion is batched, correctness-diffed against h3-pg,
  and benchmark-proven.

### H3 target-list and multi-SRF semantics

- Scope: complete H3 SRF planning for target-list cases that include more
  than one SRF or mix SRF output with ordinary projected columns.
- Evidence: before the large-input planner gate, the 2026-05-14 focused
  `h3_srf_grid_disk @ 100K` repro selected the per-row target-list SRF path
  and ran around 93-95s accelerated vs about 4.1s through h3-pg. The current
  mitigation declines variable-length SRF target-list CustomPaths for large
  estimated inputs and caps the default benchmark scales for this workload,
  but that is a gate, not a batched SRF implementation.
- Work: implement PostgreSQL-compatible row multiplication, NULL handling,
  and output ordering for multi-SRF target lists; add a batched
  variable-output SRF executor before re-enabling GPU dispatch for large
  `h3_grid_disk` target-list shapes. Unsupported multi-SRF shapes must keep
  visible planner-decline reasons.
- Acceptance: multi-SRF H3 target-list queries either dispatch with
  correctness diffs against PostgreSQL/h3-pg or decline without selected
  pg_accel plan labels.
- Progress (2026-06-05, superseded by 2026-06-14 hard GPU-only gate): large
  variable-output target-list SRF scans learned exact
  `srf_tlist_cpu_output_too_large` evidence, and multi-SRF target lists learned
  `srf_tlist_multi_srf_semantics` instead of selecting a Custom Scan that
  cannot implement ProjectSet lockstep/NULL-padding semantics. Normal SQL
  planning now hits the hard `no_gpu_resident_pipeline` gate before those
  SRF-specific admission checks. This item remains open until target-list SRF
  expansion has bounded CPU output or GPU-resident downstream consumers, plus
  multi-SRF correctness coverage.

### Spatial cost model and geometry staging

- Evidence: most non-crashing spatial polygon/selectivity cells lost by
  10-60%, even when stable. 10K warmup also showed visible cold
  JIT/dispatch spikes.
- Evidence: the 2026-05-14 full-run pass still had sub-parity spatial cells
  after the crash gates: simple 90% worker-4 repros at 100K were about
  35-66 ms accelerated vs 20-22 ms PostgreSQL, cooperative 1024 90%
  worker-4 repros at 1M were about 155-188 ms vs 140-158 ms, and
  high-selectivity/polygon-heavy 10M cells were mostly parity while taking
  multi-second samples.
- Root cause target: geometry serialization/staging and selectivity likely
  dominate for simple or moderately selective predicates; polygon vertex
  count alone is not enough to predict break-even.
- Work: add cost terms for polygon complexity, selectivity, result count,
  index/recheck shape, and batch count; keep only compute-heavy spatial cases
  eligible; improve geometry staging after the 100K crash repro is fixed.
- Acceptance: simple spatial filters route to PostgreSQL-native plans when
  faster, while high-compute spatial cases produce stable GPU wins.
- Progress (2026-06-05): `st_intersects(geometry, geometry)` is registered,
  but selected scan admission is closed until exact fp64/PostGIS semantics are
  implemented and proved. Direct and wrapped `ST_Intersects` scan predicates
  now stay native with `postgis_intersects_unsupported_shape`, and that reason
  is recorded before the generic min-batch row gate. The closed planner branch
  skips full polygon validation to avoid quadratic planning cost while no
  PostGIS `ST_Intersects` scan can be selected. Generic geometry columns,
  LineString columns, unknown-SRID typmods, dynamic polygon Vars,
  missing/wrong-SRID constants, polygons with holes, self-intersecting
  polygons, extra top-level quals, boolean wrappers, and the future
  point-column/simple-polygon candidate all stay native with
  `postgis_intersects_unsupported_shape`. The point-in-polygon kernels now
  classify polygon edge/vertex and interior-hole boundary points as
  `ST_Intersects = true`; the integration plan-shape fixture includes explicit
  boundary rows and compiles coverage for both argument orders as native
  declines. The focused `test_spatial` binary rebuilt, but its cold Metal
  runtime stalled after JIT and was interrupted, so this item remains open
  until selected point/polygon cells have durable runtime artifacts and the
  remaining spatial cost terms are calibrated.

### Prepared spatial geometry acceleration

- Scope: spatial GPU dispatch needs algorithmic acceleration for
  polygon/line-heavy workloads, not only row-count and vertex-count cost
  gates. Current point-in-polygon style work scans polygon edges directly,
  and several line/polygon and polygon/polygon predicate combinations remain
  unsupported.
- Work: build GPU-side prepared geometry structures such as edge grids,
  interval tables, bounding-volume filters, or simple BVH-like layouts that
  can be retained across batches and reused by spatial joins or repeated
  predicate scans.
- Planner rule: distinguish prepared-geometry setup cost from per-row probe
  cost, and select GPU only when reuse, vertex count, selectivity, and batch
  size cross measured break-even thresholds.
- Acceptance: prepared point-in-polygon, line/polygon, and polygon/polygon
  benchmarks match PostGIS predicate semantics on exact GPU paths and
  produce stable GPU wins below the current very-high-vertex break-even zone.

### PostGIS remaining predicate coverage

- Scope: mixed-geometry distance cases still need dispatch/kernel support.
- Acceptance: mixed supported distance shapes route through GPU with
  PostgreSQL-native correctness diffs, and unsupported shapes have visible
  planner decline counters.
- Progress (2026-06-05): unsupported PostGIS predicates, distance calls, and
  geometry constructors now recurse through boolean/null/case/coalesce-style
  wrappers, `GREATEST`/`LEAST` nodes, and array/scalar-array forms before row
  gating, so wrapped `ST_Contains`/`ST_Within`, `ST_Distance`, and
  `ST_Buffer` filter shapes stay PostgreSQL-native with stable
  planner-decline reasons instead of disappearing behind generic row gates.
  PostGIS-name decline detection also requires `postgis` extension membership
  plus extension-owned `geometry`/`geography` scalar or array argument types,
  so user-defined overloads with the same SQL names stay on the generic
  native path while PostGIS installs outside `public` keep specific decline
  visibility.
  The shared constant-geometry vertex extractor now walks the same wrappers,
  preserving spatial cost reasons such as `spatial_vertices_below_break_even`
  for wrapped predicate forms. This item remains open until mixed distance
  predicates have exact GPU kernels and correctness-diff artifacts.

### PostGIS geometry constructors

- Scope: `st_buffer`, `st_union`, and `st_intersection` need variable-size
  geometry output from GPU kernels.
- Work: design the output protocol: sizing pass plus emission pass, bounded
  preallocation, or streaming append.
- Acceptance: each constructor has a GPU kernel, adapter registration, and
  golden-diff coverage against PostGIS native output.

### Raster map algebra and multi-band fusion

- Scope: raster dispatch should execute real `ST_MapAlgebra` expressions and
  multi-band formulas instead of identity-style band extraction, and should
  avoid returning large intermediate rasters to CPU when the next operation
  is also raster-local.
- Work: parse supported map-algebra expressions into a GPU expression IR,
  support multi-band inputs such as NDVI, preserve nodata and pixel-type
  semantics, and fuse follow-on operations such as `ST_SummaryStats` when the
  SQL shape permits.
- Benchmark rule: raster benchmark queries must consume the computed raster
  or statistic output so planner pruning cannot turn a GPU-looking query into
  a no-op expression.
- Acceptance: raster expression and multi-band workloads dispatch GPU
  kernels, match PostGIS raster output within documented pixel tolerances,
  and beat PostgreSQL/PostGIS at selected raster sizes.

## Phase 6 - Coverage Closure For GPU-Efficient Work

Before public release, every remaining potentially profitable workload family
needs either an implemented GPU path or an explicit measured decline reason.
Do not move items out of this phase merely because they are hard; move them
only when benchmarks prove no release-relevant GPU opportunity.

### NUMERIC multi-limb accumulator kernel

- Scope: accelerate NUMERIC aggregation where fixed-width integer/floating
  accumulators are insufficient.
- Acceptance: NUMERIC aggregate lanes either dispatch a correct multi-limb
  GPU accumulator with PostgreSQL-compatible overflow/scale behavior or have
  measured evidence that release workloads should decline them.
- Progress (2026-06-05): built-in NUMERIC `SUM`, `AVG`, `MIN`, `MAX`,
  `STDDEV`, and `VAR_SAMP` aggregate shapes now remain PostgreSQL-native with
  generic shape evidence: `SUM`/`AVG` report
  `shape_numeric_accumulator_unavailable`, while the unsupported comparator
  and statistics families report `shape_unsupported_aggregate`. This item
  remains open until the release matrix has either measured native-decline
  artifacts or a correct multi-limb GPU accumulator/comparator implementation.
- Progress (2026-07-15): the bounded `numeric_agg_decline` workload is now a typed
  reduce-lane native-decline contract at every preserved scale. Its exact
  nullable-input digest and `shape_numeric_accumulator_unavailable` reason are
  enforced together with no CustomScan and zero GPU-kernel counter delta.

### Integer / NUMERIC AVG variants

- Scope: AVG for integer, NUMERIC, and interval inputs needs correct
  accumulator semantics and finalization.
- Acceptance: supported AVG variants dispatch GPU kernels and match
  PostgreSQL, or planner decline reasons explain why the variant is outside
  the release matrix.
- Progress (2026-06-05): unsupported integer, NUMERIC, and interval AVG
  variants now stay native with the generic shape decline
  `shape_numeric_accumulator_unavailable`. This item remains open until AVG
  variants either gain PostgreSQL-compatible GPU accumulator/finalization
  support or have release benchmark artifacts documenting native decline.
- Progress (2026-07-15): focused regression coverage now exercises `AVG` over
  `int2`, `int4`, `int8`, `numeric`, and `interval`. The bounded
  `avg_nonfloat_decline` workload remains native-decline-only with exact nullable
  output and `shape_numeric_accumulator_unavailable` at each preserved scale.

### Cascaded multi-key GPU sort

- Scope: multi-key sort and IncrementalSort opportunities need stable
  cascaded GPU sort when they can reduce or order data more efficiently than
  PostgreSQL.
- Acceptance: production-style multikey/top-k traces either dispatch a
  stable GPU implementation with speedup or decline with benchmark evidence.
- Progress (2026-06-05): multi-key `ORDER BY` regression coverage now resets
  planner stats and asserts the exact `sort_multikey_no_gpu_kernel` rejection
  count, proving the current single-key GPU sort executor does not claim
  cascaded multi-key support. This item remains open until production-style
  multi-key and IncrementalSort traces are backed by benchmark artifacts or a
  correct cascaded GPU sort implementation.
- Progress (2026-07-15): `gpu_sort_multikey` now uses deterministic nullable
  keys, duplicate peer groups, and an explicit final tie-breaker. Every
  preserved threshold remains a native decline with
  `sort_multikey_no_gpu_kernel`, exact row/order semantics, no CustomScan, and
  zero GPU-kernel counter delta.

### GPU merge-join kernel

- Scope: merge join can be strictly optimal for some ordered workloads where
  hash join regresses.
- Acceptance: representative merge-join workloads either dispatch a GPU
  merge join and beat PostgreSQL, or the planner records why hash/semi/scan
  alternatives are preferred.
- Progress (2026-06-05): merge-join-shaped equijoin regressions now reset
  planner stats and assert the exact `mergejoin_no_gpu_kernel` rejection
  count, rather than only proving that some planner rejection occurred. This
  item remains open until representative merge-join workloads have benchmark
  artifacts documenting native decline or a correct GPU merge-join kernel is
  implemented and proven.
- Progress (2026-07-15): `mergejoin_decline` now exercises nullable duplicate
  pair keys and asserts an exact join-count digest at every preserved scale.
  It remains native-decline-only with `mergejoin_no_gpu_kernel`, no
  CustomScan, and zero GPU-kernel counter delta.

### GpuExpr+Scan for BitmapHeapScan

- Scope: measured cases may benefit from preserving bitmap predicates while
  pushing expression work into GPU scan batches.
- Acceptance: BitmapHeapScan-adjacent GPU plans either beat the current
  BitmapHeapPath wrapping approach or decline explicitly.
- Progress (2026-06-05): bitmap-prefiltered generic `GpuExpr` candidates now
  reset planner stats and assert the exact
  `bitmap_heap_gpuexpr_no_gpu_pipeline` rejection count. This protects the
  current release behavior: BitmapHeapPath-adjacent scalar-expression scans
  stay PostgreSQL-native until GPU scan input, expression evaluation, and
  output handoff are GPU-resident instead of wrapping a CPU child plan.
- Progress (2026-06-09): direct seq-scan generic `GpuExpr` candidates now
  follow the same release rule as bitmap-adjacent GpuExpr: decline explicitly
  rather than selecting a CPU-boundary scan path. The live plan-shape guard
  covers the two-predicate template case that previously reached selected
  execution and crashed.

### Shared hashtable for parallel GpuHashJoin

- Scope: large-inner benchmarks may show per-worker inner builds dominating.
- Acceptance: parallel GpuHashJoin either shares/reuses a GPU-resident inner
  structure or declines large-inner plans where duplicated work loses.
- Progress (2026-06-05): large-inner parallel hash join shapes now have PG18
  regression evidence that the planner records the exact
  `hashjoin_parallel_inner_rebuild_too_large` native-decline reason before
  exposing per-worker private GPU hash-table rebuilds. This item remains open
  until the release matrix has benchmark artifacts proving native decline is
  intentional, or shared GPU-resident build-side state is implemented and
  correctness/performance proven.

### SetOp / RecursiveUnion GPU handling

- Scope: SetOp and RecursiveUnion are only release-relevant if concrete
  workloads show them as bottlenecks with GPU-friendly shapes.
- Acceptance: benchmarked shapes either dispatch GPU work with correctness
  proof or are documented as planner-declined.
- Progress (2026-06-05): upper-planner SetOp and RecursiveUnion stages now
  remain PostgreSQL-native with explicit `setop_no_gpu_kernel` and
  `recursiveunion_no_gpu_kernel` planner-decline reasons. PG18 regression
  coverage verifies representative `INTERSECT` and recursive CTE shapes do
  not get pg_accel plan labels and expose the exact missing-lane reason. This
  item remains open until benchmarked SetOp/RecursiveUnion shapes are either
  release-documented as native declines with artifacts or implemented with
  correctness-proof GPU kernels.
- Progress (2026-07-15): deterministic bounded workloads now preserve
  duplicate/NULL semantics for `INTERSECT ALL` and duplicate elimination for
  recursive `UNION`. Their exact ordered digests are tied to
  `setop_no_gpu_kernel` and `recursiveunion_no_gpu_kernel`, with no CustomScan
  and zero GPU-kernel counter delta required for every preserved cell.

### AdaptiveCpp rebase and upstream PRs

- Scope: upstream AdaptiveCpp may gain needed fixes before release, or the
  fork burden may become the main maintenance cost.
- Acceptance: either the fork is rebased/upstreamed enough for public
  installation confidence, or the release notes clearly pin the fork and
  setup path.

### soft-fp64 fenv read-back / ABI attributes

- Scope: pg_accel or another consumer may need GPU-side IEEE flag read-back
  or additional Metal ABI annotations.
- Acceptance: required ABI/fenv behavior is implemented before release, or
  documented as unnecessary for the release semantics.

## Phase 7 - Cost Models, Performance Ratchets, And Comparative Benchmarks

Once the GPU-resident implementation exists, lock planner admission to
measured break-even points and prove the release claims on M-series, NVIDIA,
PostgreSQL native execution, and PG-Strom-supported workloads.

### Benchmark win plan

- Step 1, evidence integrity: every benchmark run writes durable JSON,
  markdown, plans, crash logs, GUC snapshots, device metadata, telemetry
  limits, correctness diffs, and dispatch counters.
- Step 2, safety gates: cost out every crashing Custom Scan family before
  optimizing it.
- Step 3, planner honesty: add per-lane threshold matrices for row count,
  type, cardinality, selectivity, row width, and output size. GPU path
  selection must be justified by measured break-even points, not by generic
  "large input" assumptions.
- Step 4, lane focus: protect H3 as the current winning lane; make reduce
  typed and cheap; expose sort only for winning shapes; make hash join
  share/reuse the build side; make spatial shape/selectivity-aware.
- Step 5, ratchet: add benchmark assertions that fail CI if a selected GPU
  cell regresses below parity, crashes, silently misses GPU dispatch, or loses
  GPU plan selection for a lane that is supposed to win.

### Calibrate `pg_accel.soft_fp64_cost_multiplier`

- Scope: run the full `fp64_matrix` benchmark and pick the multiplier that
  maximizes geomean speedup while keeping every workload/size cell at
  `speedup_x >= 1.0` through an actual GPU plan.
- Work: sweep `{16, 24, 32, 40, 48, 56, 64}`; disqualify any multiplier with
  a sub-parity cell; tie-break toward the smallest multiplier.
- Acceptance: the selected multiplier, runner-up, parity-close cells
  (`<= 1.1x`), GPU-dispatch evidence, cost-threshold explanation, and
  unsupported-shape planner-decline evidence are recorded in `CHANGELOG.md` /
  `CLAUDE.md`.

### Spatial and geometry benchmark thresholds

- Scope: spatial benchmarks need threshold matrices for geometry complexity,
  selectivity, result count, index/pruning shape, retained prepared
  geometry, and batch count.
- Acceptance: GPU spatial plans are selected only for stable wins, and
  PostgreSQL-native plans are selected for simple or high-output cases where
  GPU staging cost dominates.

### H3 and raster benchmark thresholds

- Scope: H3 and raster lanes need operation-specific thresholds, not generic
  "function dispatch" claims.
- Acceptance: H3/raster functions and SRFs show consumed outputs,
  correctness diffs, dispatch counters, warm-run speedup thresholds, and
  bounded cold-start cost.
- Progress (2026-06-10): H3 thresholds are operation-specific: lat/lng
  grouped-count winners retain the 1.50x floor, while the cheap h3-pg parent
  scalar gets a separate fused parent grouped-count floor of 1.10x backed by
  the 100K/1M cache-mode-both artifacts above. Parity lanes still require
  native decline rather than speedup.

### PostgreSQL native comparison

- Scope: release benchmarks must beat PostgreSQL parallel execution across
  the selected matrix on both M-series and NVIDIA hardware.
- Acceptance: every selected GPU cell has `speedup_x >= 1.0`; every
  non-selected cell has a visible planner-decline reason and no pg_accel plan
  label.

### PG-Strom comparison

- Scope: use PG-Strom-supported OLAP/Geo cases as the comparative bar for
  PostgreSQL workloads PG-Strom already accelerates.
- Acceptance: pg_accel matches or beats PG-Strom for the benchmarked
  PG-Strom-supported use cases, or the release blocks until the gap is fixed.

## Phase 8 - Test Coverage, CI, And Stress Gates

This phase proves the implementation can survive repeated use before public
release. It is not enough for single benchmark cells to pass once.

### 90% test coverage

- Scope: reach at least 90% coverage for pg_accel-owned Rust, C++/SYCL, and
  SQL-extension behavior.
- Work: add coverage measurement for planner hooks, executor state,
  private-data encoding/decoding, GPU dispatch adapters, SQL extension
  surfaces, C++ kernels, H3/PostGIS/raster semantics, and benchmark
  classification.
- Evidence (2026-07-04): the current automated coverage gate is a Rust
  `cargo llvm-cov` gate. It runs the Rust workspace with `pg_test` enabled,
  `RUST_TEST_THREADS=1`, and an explicit `--test-threads=1` harness argument,
  so it covers Rust code reached by pgrx extension tests, but it does not
  instrument `pgaccel-kernels` C++/SYCL sources,
  standalone SQL harness files, shell scripts, benchmark artifacts, or GPU
  runtime/toolchain code. The artifact now writes `coverage-scope.txt` so the
  scope is explicit.
- Current result (2026-07-04): after the coverage-only direct-USM fused-filter
  null-mask allocation fix, `artifacts/coverage/coverage-summary.txt` completes
  but fails the threshold with 40.23% Rust line coverage and 38.36% Rust region
  coverage versus the 90% release gate. The largest Rust gaps are still
  runtime/planner/executor paths that need live-PG or focused harness coverage,
  including custom scan execution, resident/group aggregate planners, OLAP
  cache, GPU dispatch adapters, H3/PostGIS/raster dispatch, and benchmark
  CLI/integration paths.
- Acceptance: CI publishes honest Rust coverage artifacts, fails below 90% Rust
  line coverage, and publishes separate C++/SYCL and SQL-extension coverage or
  equivalent release evidence before this item is checked.

### Metal stress gate

- Scope: repeated stress on M-series hardware with mixed scan, aggregate,
  join, sort, H3, PostGIS, raster, fork, and cancellation workloads.
- Evidence (2026-07-04): `just metal-stress 18` passes on Apple silicon with
  artifact directory `benchmarks/artifacts/metal-stress-20260704-161735`.
  The run covered install provenance, extension smoke, 52/52 SQL files,
  clean PostgreSQL/panic logs, standalone GPU tests, `gpu-stress-archive`
  8x20 fork stress with zero XPC/pipeline/archive failures, benchmark crash
  artifacts for reduce/NLJ/sort/H3/spatial/raster cells, and the statement
  timeout cancellation probe. Fixes included Metal-safe fp64 reduce min/max,
  bbox fp64 correctness fallback, hash-join host-build/device-probe on Metal,
  specialized spatial PIP kernels, fail-closed Metal window planning, and the
  archive stress expected-code-object count update.
- Evidence (2026-07-04 AdaptiveCpp upstream sync): local fork commit
  `456ae6910720810f5fe59f160e6707d46bb8e5f0` rebuilds and installs with
  `ACPP_COMPILER_FEATURE_PROFILE=full`, reports `plugin-with-sscp-compiler:
  true`, and passes cold fp64 probes for
  add/mul/sqrt/sin/cos/asin/atan2/haversine with zero mismatches and no
  generated-MSL unused-selector warnings. Standalone post-sync checks also
  pass `test_spatial` `162/162`, `test_h3` `856/0`, `test_correctness`
  `340/340`, fork/cold-fork/warmed-fork smoke tests, and the 8x20 Metal
  archive fork stress with zero archive build/load failures.
- Evidence (2026-07-04 clean setup pin): `ACPP_BACKEND=metal
  ./scripts/setup_acpp.sh` uses
  `456ae6910720810f5fe59f160e6707d46bb8e5f0`, does not apply the old
  `DEFAULT_TARGETS` patch, installs `.pgaccel/acpp/metal`, and leaves
  `.pgaccel/src/AdaptiveCpp` clean.
- Acceptance: zero backend crashes, zero kernel failures, zero panic-log
  entries, zero resource-leak messages, and stable repeat artifacts.

### CUDA stress gate

- Scope: repeated stress on NVIDIA hardware using the same matrix as Metal,
  adjusted only for backend-specific device metadata.
- Acceptance: zero backend crashes, zero kernel failures, zero panic-log
  entries, and benchmark results that meet PostgreSQL/PG-Strom comparison
  gates.

### Enforce the CI ship bar

- Scope: GitHub Actions now defines the Apple Silicon GPU, Linux x86_64
  no-GPU, and optional self-hosted CUDA smoke jobs.
- Release-readiness blocker: required jobs pass on the release candidate commit
  with artifacts for the release matrix.
- Repository policy follow-up: require those jobs in branch protection. Track
  that separately from implementation, correctness, crash, and benchmark
  blockers.
- Acceptance: release checklist cites the passing CI artifacts and does not
  list branch-protection configuration as an implementation blocker.

### Run the release verification matrix

- Scope: EXPLAIN audit, correctness diff, benchmark sweep, fork stress,
  deferred-site audit, and `pg_accel_stats()` sanity.
- Progress (2026-07-04): the local PG18 SQL release gate passes after the
  strict SQL harness fix and H3/window-plan updates: `just sql-test 18`
  completed 52/52 SQL files with the live extension installed. The shell
  harness now checks PASS markers through a here-string so `pipefail` cannot
  turn a successful test with early `grep -q` exit into a false failure.
- Progress (2026-07-04): core local gates pass on the current tree:
  `cargo fmt -- --check`, `git diff --check`, `bash -n` over release/SQL/setup
  scripts, `scripts/doc_parity.sh`, `scripts/pg_version_audit.sh`,
  `cargo metadata --locked`, package file listing for `pg_accel` and
  `pg_accel_bench`, `just check 18`, `just lint 18`, `cargo test -p
  pg_accel --no-default-features --features pg18 --lib`, `cargo test -p
  pg_accel_bench --locked`, `just audit`, and `cargo deny check`.
- Acceptance: every matrix item passes with artifacts that prove GPU path
  selection, zero kernel failures, zero fork crashes, and no selected
  benchmark cell below PostgreSQL parallel parity.

### Release checklist synchronization

- Scope: keep `docs/release-checklist-1.0.md` aligned with this TODO.
- Acceptance: every release-gate item links to the commit or artifact that
  proves it, and the tag PR includes the checklist.
- Checklist wording must separate real release blockers (crashes, correctness
  diffs, missing stress/benchmark artifacts, selected rows below PG-parallel
  parity) from policy follow-up such as branch-protection configuration.
- Progress (2026-06-09): checklist Phase 0, Phase 2, and Phase 5 rows now
  reference current interim evidence for resume/audit manifests, warmup/JIT
  report fields, harness build-profile metadata, quiet CTest logs, and the
  release-harness H3 benchmark artifacts. The boxes remain unchecked until
  final release artifacts or tag-PR evidence exist.

## Phase 9 - real to hackerne.ws, public repo, make installable by anyone

This is the final phase. Do not start it until the release mission and every
prior phase gate above is satisfied.

### Fresh-machine smoke

- Scope: clean clone, install prerequisites, `just setup-gpu-acpp`, package,
  install, create extension, and run a representative benchmark without
  manual fixes.
- Acceptance: the sequence passes on a fresh M-series environment from the
  public README instructions.

### Public repository readiness

- Scope: public README, architecture docs, benchmark docs, release notes,
  license files, contribution guide, security policy, issue templates, and
  reproducible benchmark artifacts.
- Acceptance: a new user can understand what pg_accel accelerates, what it
  declines, which hardware is supported, how to install it, how to run proof
  benchmarks, and how to report failures.

### Installable by anyone

- Scope: package the PostgreSQL extension, AdaptiveCpp fork setup, kernel
  build, SQL/control files, source PostgreSQL/pgrx install path, native macOS
  notes, Linux CUDA notes, and verification command.
- Acceptance: install docs work from a clean machine; install provenance
  confirms the live backend loads the just-built extension; failures produce
  actionable diagnostics.

### Release candidate and final tag

- Scope: cut `v1.0.0-rc1`, monitor for one week, then promote to `v1.0.0`
  if no critical bugs surface.
- Acceptance: tag, release notes, source archive, SQL artifacts, checksums,
  benchmark artifacts, and install docs are published.

### Hacker News launch

- Scope: publish the repo and launch post only after the project is
  installable, benchmark-backed, and crash-free on the release matrix.
- Acceptance: the public post links to the repo, install docs, benchmark
  evidence, PG-Strom comparison, supported hardware, limitations, and issue
  tracker.

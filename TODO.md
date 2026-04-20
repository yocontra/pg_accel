# TODO

Open work, derived from current code comments and recent commits. Not
exhaustive — add items as they come up.

## Correctness

- **Fix & re-enable GPU bytecode expression evaluator.** Disabled in
  `engine/ffi/custom_scan/mod.rs` and `engine/executor/scan.rs` because the
  interpreter returns wrong results. Compilation still runs; only execution
  falls back to PG scalar qual. Template-matched predicates (simple cmp,
  BETWEEN, IN, IS NULL, two-cmp AND) are unaffected.

## Scale limits (verify gates match current `DeviceLimits`)

- Defaults in `engine/cost/device_limits.rs`: `gpu_reduce_min_rows=25_000`,
  `gpu_sort_max_elements=2_000_000`, `gpu_join_max_output_rows=100_000`.
  The platform-aware `from_profile()` path scales these by CU count /
  memory / unified — spot-check against the hardware being benchmarked
  before chasing "regressions."

## Parallel-path coverage (broader scope from merry-riding-torvalds plan)

Partial-agg parallel execution works today for plain `SUM`/`MIN`/`MAX`/`COUNT`
on scalar types (transtype == rtype). The emitter/accumulator machinery in
`executor/agg/partial/{accumulator,emitter}.rs` is ready; the round-trip
that is missing is carrying per-column `aggserialfn` / full `PartialAggSpec`
through path-level `custom_private` into the cscan at plan time.

- **AVG / STDDEV / VAR parallel.** `planner_hooks/partial_agg.rs:170-178`
  bails on `INTERNAL` transtype and `AggOp::Avg`. `Float8StatsEmitter`
  (bytea via `aggserialfn`) is implemented in `executor/agg/partial/emitter.rs`
  and `build_partial_emitters` in `ffi/custom_scan/mod.rs:790` already picks
  it when `op ∈ {Avg, StddevSamp, StddevPop, VarSamp, VarPop}` AND
  `serialize_fn_oid.is_some()`. The gap is the **path-level serialization**:
  `partial_agg.rs` writes `(op, attno, rtype)` triples into path_priv, and
  `build_partial_spec_from_path` (`ffi/custom_scan/mod.rs:825`) reconstructs
  the spec from triples with `serialize_fn_oid=None`. To unlock AVG/STDDEV:
  1. In `partial_agg.rs`, build a full `PartialAggSpec` using
     `classify_aggref` + `syscache::agg_serialize_fn`.
  2. Append the spec to path_priv using the `PAAG` sentinel block (via
     `append_partial_spec`).
  3. In `plan_custom_path_agg` (`ffi/custom_scan/mod.rs:611`), switch the
     `is_partial` reader from `path_len - 1` to a structural lookup, and
     replace `build_partial_spec_from_path` with a call to
     `deserialize_partial_spec` reading the sentinel block.
  4. Remove the `INTERNAL` / `AggOp::Avg` early-bail gates in `partial_agg.rs`.

- **Grouped HashAgg parallel.** `planner_hooks/partial_agg.rs:82` bails on
  `query.groupClause`. To unlock: propagate the groupClause through the
  partial path (groupClause, HashAgg strategy selection, group-key attno
  remap), build `Partial Gather + Final HashAgg` — PG's
  `add_paths_to_grouping_rel` path for parallel grouped agg.

- **Preagg parallel.** `planner_hooks/mod.rs:1016` hardcodes
  `parallel_safe=false`. Preagg today emits final-state Datums that would
  double-count under Gather. Fix: add a `partial_emitters`-style path to
  `executor/preagg/partial.rs` (stub exists) and flip the flag when all
  fused Aggrefs classify into the partial-capable set.

- **Sort/window/hashjoin `add_partial_path`.** `create_custom_path`
  already propagates `base_path.parallel_safe`, so wrapped CustomPaths
  inherit parallel_safe=true when wrapping a parallel child. But today
  scan-level GPU injection (`rel_pathlist.rs:414`) only calls `add_path`,
  not `add_partial_path`. For GPU sort / hashjoin / window to run inside
  a Gather, each injector needs to iterate `rel->partial_pathlist` and
  call `add_partial_path(rel, cpath)` in addition to the non-parallel
  `add_path`.

## Verification matrix (plan §Verification, not yet run)

- `EXPLAIN (VERBOSE)` for: plain SUM, AVG + STDDEV, GROUP BY, ORDER BY,
  `ROW_NUMBER() OVER ...`, plain JOIN, JOIN + GROUP BY. Each must show
  `Gather` (or `Gather Merge`) with pg_accel CustomScan inside.
- Correctness diff: pg_accel on vs off — identical rows for every query
  above, float aggregates to fp tolerance.
- `cargo run -p pg_accel_bench --release -- run --iterations 5 --warmup 2`
  at 100K / 1M / 10M. Monotonic perf curve; no regressions.
- 8-worker × 20-iteration fork stress on `bench_f32_10m` across
  {SUM, AVG, STDDEV, grouped HashAgg, sort, window, hashjoin}. Zero crashes.
- `grep -r "CPU fallback\|Deferred"` check — zero kernel-failure Deferred
  results, input-gate Deferred for unsupported types is OK.

## Perf (known gaps, deferred)

- **Per-batch GPU dispatch overhead dominates parallel SUM.** 10M `SELECT
  SUM(v) FROM bench_f32_10m`: pg_accel parallel 177ms vs PG parallel 88ms
  (baseline 2026-04-20 after fork-safe-metal landed). Each worker runs
  ~52 batches × 65k rows × ~5.5ms dispatch. JIT cache IS populated and
  reused (`~/.acpp/apps/global/jit-cache/` has .metallib + .metalar);
  this is pure per-dispatch cost, not compilation. Directions to try:
  raise `pg_accel.min_batch_size` (currently caps at 65536), command
  buffer reuse across batches, kernel fusion of scan+reduce, or
  fewer-bigger batches via buffered accumulation at the executor layer.
- **Per-fork JIT still ~290ms cold+warm on isolated queries** per
  `project_metal_fork_issue.md`. Separate from the dispatch-overhead
  issue above. Needs investigation into why the kernel_configuration
  hash lookup misses the on-disk cache on some code paths.

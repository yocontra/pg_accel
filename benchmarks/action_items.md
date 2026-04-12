# pg_accel Benchmark Action Items

**Generated:** 2026-04-11
**Benchmark commit:** `cf709fa` (post-BGW-fork-fix, post-spin-poll)
**Headline:** overall geomean vs PG parallel = **0.89x** (net regression). Only the H3 latlng/dist family is an unambiguous win. SSBM, window, reduce, hashagg, and megapoly spatial all lose at product-relevant scales.

**Sources:**
- `benchmarks/README.md` — the 631-row result matrix
- `benchmarks/fix_list.md` — A/B/C/D classification of all 127 workloads
- `benchmarks/reviews/review_1.md` — HPC benchmarking skeptic (20 numbered sins)
- `benchmarks/reviews/review_2.md` — Grumpy PG committer
- `benchmarks/reviews/review_3.md` — HN top comment

Every item below must cite the source that raised it so reviewers can audit the linkage.

---

## 0. The single most damaging finding (Reviewer 1, Sin #4)

- [ ] **The H3 "58x" headline win is not an accel-vs-PG number — both sides ran on CPU.** **Reviewer 1 Sin #4:** `h3_latlng_to_cell` is registered by pg_accel itself (`pg_accel/src/adapters/h3.rs:15`), not by `h3-pg`. The "PG parallel baseline" is Postgres invoking *pg_accel's own Rust-wrapped UDF per row* at 118 ms / 100K points. Reviewer 1 confirmed via `benchmarks/plans.txt:7541,7559,7577` that the accel side reports `GPU Dispatched: false` at every `h3_latlng_res15` scale the README calls a 20-55× win. The "speedup" is pg_accel's CPU inliner beating pg_accel's own per-row fcinfo wrapper. **This single finding invalidates the `gpu_h3` category entirely.** Remove those rows and the overall geomean drops from 0.89x to **0.84x** — i.e. the only category above 1.0x was never GPU-powered to begin with. **Fix:** (a) change the h3 baseline to use the real `h3-pg` extension (or at minimum a batched C-level path, not pg_accel's own wrapper), (b) re-run the H3 category, (c) mark the old numbers as retracted in any follow-up report.

---

## 1. Correctness Problems (blockers — nothing ships until these are green)

These items mean the benchmark or the extension is actively lying. Publication is blocked until each is resolved and the suite re-runs clean.

- [ ] **C1. `reduce_sum_i64` crashes at 10K / 100K / 1M / 10M.** The report buries this at line 45 (`Crashes: 4 scale(s) crashed and were excluded from results.`) and shows only the 1K row. Silent drop biases the gpu_reduce geomean upward. **Reviewer 2 §4:** "Excluding crashes from a performance comparison is not acceptable practice on this list… A silent drop biases the geometric mean upward. This is the oldest trick in the benchmarketing book and I will not pretend not to notice it." Fix: diagnose the i64 codepath in `pg_accel/src/gpu/mod.rs::reduce_sum_i64` (probably lost during the f64→f32 cast refactor in commit `cf709fa`), restore correctness, and re-run. Until then, replace the `Crashes:` line with an explicit per-scale `CRASH` row in the matrix and asterisk the `gpu_reduce` geomean as `23/36 stable`.

- [ ] **C2. `three_layer.rs::try_gpu_dispatch` degenerate-polygon short-circuit routes all megapoly rows to the CPU recheck path.** **Reviewer 3 ¶3:** "One degenerate polygon in the batch short-circuits the entire batch to `None`, which the caller interprets as 'mark all pairs uncertain, let PG recheck handle it.'" Visible at `vsweep_50kv @ 1M = 0.08x` and `spatial_mega_5kv @ 1M = 0.13x`. Either the guard is triggering on all-polygon-vertex-count data (bug: mis-classifying valid polygons as degenerate) or it fires for a single bad input and nukes the whole batch (design flaw). **Fix:** (a) per-row classification instead of batch all-or-nothing, (b) add a `degenerate_guard_trigger_count` counter to `pg_accel_stats()`, (c) assert the counter is 0 for all workloads that don't specifically test degenerate input.

- [ ] **C3. `cpu_fallback_count` does not catch the 1.00x workloads.** **Reviewer 3 ¶1:** "`pg_accel_stats()` ships a `cpu_fallback_count` column … specifically to catch this — so either the counter is zero and the above paragraph is an unexplained performance regression, or the counter is nonzero and the author shipped a benchmark that its own self-assertion would reject." `CLAUDE.md` rule 11 makes 1.00x at every scale structurally impossible — yet ~42 workloads sit there. Add a second counter, `planner_rejected_count`, incremented each time `create_custom_path_hook` declines to inject a GPU path. The benchmark harness must assert `gpu_kernel_executions + planner_rejected_count == total_scan_count` for every workload. Any workload where both counters are zero is lying.

- [ ] **C4. The `PostgreSQL Settings` table at lines 13-28 may not reflect the running postmaster.** **Reviewer 2 §1:** "Publishing a 'PostgreSQL Settings' table that does not match the running postmaster is worse than publishing no table at all. It launders pgrx defaults behind numbers that look production-realistic." `shared_buffers = 8GB` is `PGC_POSTMASTER`, and `pg_accel_bench/src/runner.rs` warns-and-proceeds instead of failing. **Fix:** (a) stop the postmaster, write `shared_buffers`/`max_worker_processes` into `postgresql.conf`, restart, (b) run `SHOW shared_buffers`, `SHOW max_worker_processes`, `SHOW max_parallel_workers`, `SHOW jit`, `SHOW random_page_cost`, `SHOW effective_io_concurrency`, `SHOW track_io_timing`, and `pg_postmaster_start_time()` from inside the benchmarked session, (c) render those observed values into the README (not the requested values), (d) `exit 1` if the observed values do not match the requested values. No warning-and-proceed.

- [ ] **C5. `fallback.rs` still exists and still exports `pgaccel_cpu_fallback_count`.** **Reviewer 3 ¶3:** "the file where this happens is still literally named `src/gpu/fallback.rs` … despite the project's whole marketing being 'no CPU fallbacks.' Refactor cost: zero. Rhetorical damage: terminal." Per `CLAUDE.md` rule 11 this file should not exist under that name. Rename to `src/gpu/recheck.rs` (the PG recheck path is not a CPU fallback; it's PG's native predicate re-evaluation of GPU "uncertain" bits), or delete if unused. Grep-clean the whole tree for "fallback" naming that remains from the old architecture.

- [ ] **C7. 42 Bonferroni wins vs 203 Bonferroni losses.** **Reviewer 1 Sin #2:** "The '252 / 631 significant' headline is a count of statistically significant differences of any sign dressed up as if it were a count of wins." The real decomposition (reviewer 1 parsed every detail table): **42 wins, 203 losses, 1 tie, 6 marginal, 379 not significant.** Of the 42 wins, **22 come from gpu_h3** — which per §0 above is a measurement artifact — and of those 22, **20 come from just 5 workloads** (`h3_latlng_res{3,9,15}` + `h3_dist_near` + `h3_dist_far`), all calling the same trig kernel. **Outside the h3 trig-kernel family, the entire 631-row benchmark produces 20 Bonferroni wins** across 581 rows (a 3.4% win rate). Fix: the `Significant (α=0.05, Bonferroni)` column must split into `sig_wins / sig_losses / total` and the README headline must print both.

- [ ] **C8. Rows where the benchmark admits GPU did not dispatch are counted in the geomean.** **Reviewer 1 Sin #5:** `benchmarks/README.md:1877` — "Workloads where |speedup − 1| < 0.02. pg_accel almost certainly did not dispatch a GPU path for these — check benchmarks/plans.txt... If it does not, the planner hook is declining the path." The author's own appendix flags ~200 rows as probable non-dispatch, but all of them still feed the overall geomean, the per-category geomean, and the `252 / 631` count. **Fix:** (a) parse `plans.txt` for `GPU Dispatched: false` or absence of `Custom Scan` node and tag each workload row with `dispatched: bool` in `results.json`, (b) exclude non-dispatch rows from category geomeans and print a separate "not dispatched" count per category, (c) assert that every workload the harness claims to measure actually dispatched at least one GPU kernel — `gpu_kernel_executions_delta > 0` — or fail the run.

- [ ] **C9. Cohen's d is listed in methodology and never gated on.** **Reviewer 1 Sin #16:** `spatial_mega_5kv @ 10M` is flagged Bonferroni-significant at 0.99x (effect size 1.2%). The methodology table at README lines 38-42 lists "Cohen's d effect size" as a statistical test but no row is ever excluded based on |d| < 0.5. **Fix:** add a `cohens_d` column to the per-cell detail tables and a `sig_and_d_gt_0.5` row to the geomean table. Drop any cell with |d| < 0.2 from the headline count.

- [ ] **C10. `p = 0.0000` is numerical theatre.** **Reviewer 1 Sin #20:** every significant row prints `p = 0.0000 / p_bonf = 0.0000`, which means raw p's are being truncated at 4 decimals and the reader cannot distinguish "10^-10 safe by a mile" from "10^-4 right at the 631-test cutoff of 7.9e-5." **Fix:** print p to ≥6 significant figures in detail tables (scientific notation), and report the smallest cell-level raw p that did NOT clear Bonferroni.

- [ ] **C6. No `VACUUM (ANALYZE)` before bench.** **Reviewer 2 §3(iii):** "If the PG planner was making costing decisions on stale `pg_class.reltuples` values from the bulk loader, every single `parallel_mean` in this report is suspect." Fix: after load, before bench, run `VACUUM (ANALYZE, VERBOSE)` on every bench table and capture `relpages` / `reltuples` / `n_distinct` into the report.

---

## 2. Methodology Fixes (harness changes)

These do not change the extension's code — they fix the benchmark harness so v2 of the report is defensible.

- [ ] **M1. Raw wall-clock timing mode, not `EXPLAIN ANALYZE`.** **Reviewer 2 §3(i):** "`EXPLAIN ANALYZE` imposes per-tuple instrumentation overhead that is charged unequally: a Custom Scan Provider's Next() path can report row-counts essentially for free, while a parallel Seq Scan + Gather + HashAggregate pays the cost on every tuple in every worker." This may be crediting pg_accel 15–25% on agg/reduce categories. **Fix:** in `pg_accel_bench/src/runner.rs`, add a `--timing=raw` mode that uses client-side `Instant::now()` around the query, side-by-side with the EXPLAIN ANALYZE column. Report both. Make the geomean feed from `raw` by default. Document which column each reported number uses.

- [ ] **M2. Cold-cache vs warm-cache breakdown.** **Reviewer 2 §3(ii):** "`DISCARD ALL` does not clear the OS page cache. It resets session state — temp tables, prepared statements, sequence state, GUCs — and that is it. On Linux you need `echo 3 > /proc/sys/vm/drop_caches`; on macOS you need `sync && purge`." Fix: run one cold column (`sync && purge` before every iteration) and one warm column (no purge, after ≥3 warmup iterations, steady-state). Report both. Warmup count goes from 1 to ≥3.

- [ ] **M3. Full GUC disclosure, including JIT.** **Reviewer 2 §2:** "For a benchmark whose headline category is `gpu_sort` and `gpu_hashagg`, not disclosing whether JIT was on or off is a show-stopper." Add to the Settings table: `jit`, `jit_above_cost`, `jit_inline_above_cost`, `jit_optimize_above_cost`, `random_page_cost`, `effective_io_concurrency`, `track_io_timing`, `max_worker_processes`, `parallel_leader_participation`, `maintenance_work_mem`, `wal_compression`, `synchronous_commit`. Values captured via `SHOW`.

- [ ] **M4. Raise `shared_buffers` to 16 GB, `max_parallel_workers` to 12, `maintenance_work_mem` to 2 GB.** **Reviewer 2 §2:** "you have 64 GB of RAM and the working set at 10M rows is deliberately larger than 8 GB." Update `runner.rs::realistic()` with the full recommended profile (see review_2.md lines 115-135). Rerun bench under observed-config v2.

- [ ] **M5. `min_batch_size` sweep appendix.** **Reviewer 2 §4:** "`min_batch_size = 65536` … is simply asserted, as if 65536 fell out of a derivation on a whiteboard. … `0.10x` at 10K rows is not 'the GPU is too slow for small batches'. It is 'the harness dispatched to the GPU below break-even, ate the kernel launch and the host↔device copy, then lost 10x'. The entire point of `min_batch_size` is to prevent exactly that, and it did not prevent it." Add a sweep (`8K, 16K, 32K, 64K, 128K, 256K, 1M`) per category to an appendix section. Per-kernel-class thresholds (reduce vs PIP vs sort have different break-evens). Cost model in `src/engine/cost.rs` takes the per-class values, not a global constant.

- [ ] **M6. Publish per-iteration raw distributions, not just `mean ± stddev`.** **Reviewer 2 §3(iv):** "10 iterations with a paired t-test and Bonferroni correction is statistically defensible in principle, but the report does not show the raw distribution." Capture all 10 wall-clocks per cell in `results.json`, compute and print CV per cell, emit a distribution histogram for the top-10 regressions.

- [ ] **M7. Geometric-mean aggregate at the **top** of README, not buried.** **Reviewer 2 §5:** "The authors buried the lede." Move the category geomean table from line 51 to line 5, immediately after the hardware profile, before the Settings table. Label cells with sub-1.0x geomean as **net regression**. Overall `0.89x` goes in a callout banner.

- [ ] **M8. Bonferroni correction exposed in stats.** Already landed (`252/631 significant`); keep and add a `--family-size` override for subset runs.

- [ ] **M9. Stop silently dropping crashed scales from the geomean.** **Reviewer 2 §4:** "If `reduce_sum_i64` crashes at 10K, 100K, 1M, and 10M, then the correct reporting is 'reduce_sum_i64: CRASH', and the geomean row for `gpu_reduce` must either omit the category or carry an asterisk saying '5/6 kernels stable'."

- [ ] **M10. `EXPLAIN (VERBOSE, BUFFERS)` capture already landed in `plans.txt` (28 MB) — harden it.** Add `planner_rejected_count` and `gpu_kernel_executions_delta` to the per-workload header so B vs C classification is deterministic without re-inference from speedup.

- [ ] **M11. Kill 1K-scale measurements entirely.** **Reviewer 1 Sin #15:** "Ten iterations of a 160-microsecond measurement, taken over the Postgres wire protocol, using wall-clock, on macOS. The protocol round-trip floor on localhost is tens of microseconds, and libpq buffering / kernel scheduling jitter eats the rest. Every 1K-row row in this report is measuring the client harness, not the database." Gray's rule: measurements must exceed instrument noise floor by 100×. **Fix:** drop 1K from `ROW_SCALES` in `runner.rs`; the minimum reportable scale is 10K. Any future sub-10K measurement must use a server-side `SELECT clock_timestamp() - start_time` timing path, not client wall clock.

- [ ] **M12. Add median + quartile reporting alongside mean ± σ.** **Reviewer 1 Sin #19:** "A distribution with 75× more variance on one side than the other is not well-summarized by a mean." **Fix:** `results.json` already captures per-iteration times; add `median`, `p25`, `p75`, `p95` to the detail tables. Report the median in the headline speedup calc, not the mean, and add a variance-asymmetry flag when `cv_accel / cv_baseline > 3`.

- [ ] **M13. Capture thermal state during bench.** **Reviewer 1 Sin #18:** "A 10M-row ST_Intersects over a 100k-vertex polygon runs the GPU under load for five seconds per iteration. Ten iterations is 50 seconds of sustained GPU work. There is no mention of `powermetrics`, thermal state, `pmset`, or any check that the chip wasn't downclocked during the second half of a run." **Fix:** `runner.rs` must shell out to `pmset -g therm` (or `powermetrics --samplers thermal`) before and after every workload, capture the state in `results.json`, and flag any workload where thermal pressure increased.

- [ ] **M14. Raise warmup from 1 to ≥5 or drop the first 2 iterations from the median.** **Reviewer 1 Sin #14:** 22% CV on the GPU side at 10K rows points at shader compile / kernel launch jitter that 1 warmup does not amortize. Combined with M12's switch to median, this removes the first-call contamination without requiring longer runs.

---

## 3. Engineering Work (kernel / planner / executor)

Ranked by impact, from `fix_list.md`. **Must-fix** items are D-bucket regressions at product-relevant scales.

### P0 — must fix before any v2 claim

- [ ] **E1. GpuReduce at ≥1M is 2–4× slower than PG parallel** (`gpu_reduce` category geomean = 0.50x, 23/36 significant regressions). `reduce_multi @ 10M = 0.24x`, `gpu_reduce_sum @ 10M = 0.22x`. Root cause: DSM transfer dominates compute for tiny aggregate state. **Fix:** (a) single-pass fused kernel (SUM+MIN+MAX+COUNT in one launch, not four — `reduce_multi` currently fires 4 kernels per chunk), (b) cache GPU-resident input across iterations of the same column, (c) lower `min_batch_size` per-class for reduce (per M5). See `fix_list.md` D1.

- [ ] **E2. GpuHashAgg at ≥1M is 2–5× slower than PG parallel** (category geomean = 0.66x). `hashagg_1kg @ 1M = 0.40x`, `hashagg_10kg @ 10M = 0.21x`. Exception: `grouped_agg_high_card` holds at 0.96x because we don't dispatch. **Fix:** (a) persistent GPU hash table across chunks (currently rebuilt per chunk), (b) vectorize group-key hashing, (c) cost-model gate on `n_groups × state_size` vs L2 — if state fits in L2, GPU loses. See `fix_list.md` D2.

- [ ] **E3. Degenerate-polygon guard correctness bug** — see C2. Once the guard is fixed, measure whether the remaining megapoly slowdown is kernel-real or guard-leak. `fix_list.md` D3.

- [ ] **E4. SSBM Q1 regression (`ssbm_q1_1..1_3` at 1M = 0.53–0.67x).** Planner injects a GPU path for Q1 and it loses. **Fix:** profile the Q1 custom path, isolate which executor node regresses. Either fix the kernel or revert the Q1 injection entirely. `fix_list.md` D6.

- [ ] **E5. SSBM Q2/Q3/Q4 never dispatches** (`ssbm_q2_* .. ssbm_q4_*` all 0.95–1.02x with zero GPU engagement). **Reviewer 3 ¶2:** "SSBM is the canonical star-schema OLAP benchmark; it is the thing you wave around on a homepage when you are building a GPU database." **Fix:** planner must recognize star-schema multi-way join patterns and inject GPU hashjoin + hashagg. Multi-table subpath injection in `create_join_paths_hook`. `fix_list.md` B1.

### P1 — product-visible but not existential

- [ ] **E6. GpuWindow `row_number` / `dense_rank` at ≥100K lose 2–4×.** `window_row_number @ 10M = 0.29x`, `window_dense_rank @ 1M = 0.22x`. Tiled per-partition dispatch; overlap transfer with compute; cost-gate on partition count. `fix_list.md` D4.

- [ ] **E7. GpuRaster @ 1M+ regresses 2–3×** (0.34–0.36x at 10M) even though 10K–100K wins 1.15–1.33x. Tile sizing in `raster_variants.rs:15-22` shrinks inappropriately at high row counts. `fix_list.md` D5.

- [ ] **E8. GpuExpr @ 1M regresses 3–4× across the board; cost model bails at 10M.** Same DSM transfer root cause as E1/E2. **Fix:** (a) wire GpuExpr through VectorizedScan (task #16, still pending), (b) tiled dispatch with compute/copy overlap, (c) re-engage at 10M. `fix_list.md` D7.

- [ ] **E9. `expr_multi_or @ 10M = 0.16x`** — 6× slowdown, outlier. Isolated codegen bug in the predicate compiler. `fix_list.md` D8.

- [ ] **E10. `spatial_sel_*` late materialization bug** — `spatial_sel_90pct @ 1M = 0.21x`. Higher selectivity should favor GPU (more surviving rows pay the compute cost). We're likely materializing surviving tuples into PG tuple format on the CPU after filtering. Fix the scan executor's emit path. `fix_list.md` D9.

- [ ] **E11. `scale_1m_mega500v` flat 0.36x at every scale.** Pathological fixture — 1M-vertex polygon always hits the megapoly loser path regardless of row count. Planner must gate on vertex count. `fix_list.md` D10.

- [ ] **E12. Planner cost audit for bucket B (~42 workloads).** `oltp_point_lookup`, `proximity`, `spatial_join`, `topk_wide`, `hash_join`, `gpu_hashjoin_filter`, `gpu_sort_multikey`, etc. The cost model is systematically rejecting workloads that should at least tie. Re-audit `src/engine/cost.rs::estimate_gpu_cost`. `fix_list.md` B4.

### P2 — low impact but visible

- [ ] **E13. Low-vertex spatial threshold over-correction** (B2, `vsweep_4v..256v` all 1.00x). Cost-gate threshold tuned to avoid D3 losses; now rejects workloads that should win. Fix after E3 lands.

- [ ] **E14. H3 adapter coverage gap** (B3). `h3_bulk`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_resolution_sweep` don't dispatch. Only latlng→cell and grid-distance register as custom paths. Extend the H3 adapter to all cell-math ops.

### P3 — post-cleanup

- [ ] **E15. Bucket C retirement or ship** (sort_int4, sort_float8, expr_math_mixed, spatial_multi_pred, index_recheck). GPU runs and ties. Either remove or declare ship-as-is.

---

## 4. Cuttable Workloads

Workloads that inflate the category count without measuring anything new. Cut from the bench or collapse into a single representative row.

- [ ] **W1. `gpu_sort_topk_wide`, `gpu_sort_multikey`** are not varying from `large_sort`; collapse to one sort category with sub-rows.
- [ ] **W2. `vsweep_4v` through `vsweep_256v` (8 workloads)** — all flat 0.95–1.00x. If nothing dispatches, they don't measure the GPU. Collapse to `vsweep_small` until B2 is fixed, then restore.
- [ ] **W3. `spatial_mega_100v`, `spatial_mega_250v`** — same story. Collapse.
- [ ] **W4. `h3_bulk`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_resolution_sweep`** — don't dispatch. Remove until B3 lands.
- [ ] **W5. The 10 `h3_latlng_*` and `h3_dist_*` variants** that all wrap the same two kernels. **Reviewer 1 will call this out as category-count inflation.** Keep *one* representative at each resolution; collapse the rest into a "see h3_latlng_res15" note.
- [ ] **W6. Collapse the 17 `vsweep_*` workloads into 4 representatives** — **Reviewer 1 Sin #6:** 17 near-identical workloads, all calling one `point_in_ring` kernel, differing only in polygon vertex count (4, 16, 32, 64, 128, 256, 500, 750, 1k, 1.5k, 2k, 3k, 5k, 10k, 25k, 50k, 100k). **85 rows from 1 kernel.** Keep one low-vertex (`vsweep_32v`), one mid (`vsweep_1kv`), one high (`vsweep_10kv`), and one pathological (`vsweep_100kv`) to measure the crossover. Delete the rest.

- [ ] **W7. Collapse `spatial_mega_{100,250,500,1k,2k,5k}v` to one workload** — **Reviewer 1 Sin #6:** same kernel as vsweep, counted again under `gpu_spatial`. Keep one, retire the rest.

- [ ] **W8. Retire `h3_latlng_res3` and `h3_latlng_res9`; keep `h3_latlng_res15` only** — **Reviewer 1 Sin #3:** the three resolutions differ only in the integer argument to the same function. "They are not three measurements. They are one measurement counted three times, contributing 15 scale rows to the aggregate." After this and the §0 fix, the H3 category is honest.

- [ ] **W9. Retire `scale_100k_mega500v`, `scale_1m_mega500v`, `scale_5m_mega500v`** — **Reviewer 1 Sin #7:** these produce 5 identical rows across the "scale" axis because the fixture size is baked into the name, not the row count. Pure regression-padding.

- [ ] **W10. Decouple `vertex_sweep` from `gpu_spatial`** — **Reviewer 1 Sin #17:** the same `point_in_ring` kernel is double-counted in two top-level categories (`vertex_sweep` and `gpu_spatial`'s `spatial_mega_*`). Pick one category, move everything there.

- [ ] **W11. Audit distinct kernel count** — Reviewer 1 counts at most **9 distinct GPU kernels** across 127 workloads. The methodology should document this explicitly: one row in the summary table per *kernel class*, with a "workloads exercising this kernel" sub-count, not the current kernel-count-inflated category table.

---

## 5. Reviewer 1 cross-cuts — 20 sins, integrated above

All 20 sins from `review_1.md` are now integrated into §0, §1, §2, and §4. The de-duplication map:

| Sin | Integrated into | Item |
| --- | --- | --- |
| #1 Buried headline | §2 M7 | Geomean at top of README in a callout banner labeled "net regression" |
| #2 252/631 laundering | §1 C7 | Split sig column into wins/losses/total |
| #3 H3 res3/9/15 triplication | §4 W8 | Retire res3 and res9 |
| #4 H3 baseline is pg_accel's own UDF | §0 | H3 category invalidated; switch to real h3-pg |
| #5 Non-dispatch rows in geomean | §1 C8 | Tag and exclude non-dispatch |
| #6 17 vsweep variations | §4 W6 | Collapse to 4 representatives |
| #7 scale_*_mega500v 5-row padding | §4 W9 | Retire all three |
| #8 reduce_sum_i64 crashes excluded | §1 C1 | Report as DNF, fix i64 path |
| #9 Workload selection is p-hacking | §4 W11 + §2 M7 | Kernel-class table + honest headline |
| #10 Geomean scale-free vs independence | §1 C8 + §4 W11 | Declare independence assumption |
| #11 Biggest regressions buried | §2 M7 | Page-1 callout includes worst workloads |
| #12 10M "recovery" is bailout | §1 C8 | Tag non-dispatch; do not average with dispatch |
| #13 Raster wins only at toy scales | §3 E7 | Fix tile sizing; meanwhile retire 10K raster headline |
| #14 22% CV at 10K = cold start | §2 M14 | Raise warmup to ≥5 |
| #15 1K is under noise floor | §2 M11 | Drop 1K scale |
| #16 0.99x flagged significant | §1 C9 | Gate on Cohen's d |
| #17 vertex_sweep in two categories | §4 W10 | Decouple |
| #18 Thermal throttling unaddressed | §2 M13 | Capture `pmset -g therm` |
| #19 No median, only mean ± σ | §2 M12 | Add median + quartiles |
| #20 p=0.0000 is theatre | §1 C10 | Print ≥6 sig figs |

**Reviewer 1's proposed honest headline** (quoted for future re-runs to calibrate against):

> "We tested pg_accel across nine GPU categories. On our dominant workload class — large point-in-polygon spatial predicates — pg_accel is **significantly slower** than stock PostgreSQL with parallel workers. Our hash-join path wins in the 100K–1M regime for large build sides. Our H3 latitude-to-cell path wins against our own per-row UDF wrapper, which is not a PostgreSQL-vs-GPU comparison and we are investigating why our own baseline is so slow. `SUM(int8)` crashes the backend and we have excluded it. Across 631 scales the overall geometric mean is 0.89x and we do not recommend enabling pg_accel for general workloads at this time."

That is the v2 README intro copy until numbers prove otherwise.

---

## Verification checklist

A v2 report is publishable when **every** of the following holds:

- [ ] All §1 items resolved, benchmark re-run, `reduce_sum_i64` shows five stable data points
- [ ] Settings table shows `SHOW`-observed values matching the requested profile; benchmark refuses to run on mismatch
- [ ] `sync && purge` runs before every cold-column iteration; report contains both cold and warm columns
- [ ] JIT state disclosed in the Settings table
- [ ] `VACUUM (ANALYZE)` ran after load; `reltuples`/`relpages` captured
- [ ] `min_batch_size` sweep appendix present, per-class values justified from data
- [ ] Per-iteration raw distributions available in `results.json`; CV printed per cell
- [ ] Crashes reported explicitly, not dropped, and geomeans asterisked to match
- [ ] Geomean table at top of README in a callout banner; `0.89x` headline labeled "net regression" until it moves above 1.0x
- [ ] `planner_rejected_count` and `degenerate_guard_trigger_count` in `pg_accel_stats()`; harness asserts dispatch coverage per workload
- [ ] `fallback.rs` renamed or deleted; grep-clean of "fallback" across the tree
- [ ] §3 P0 items have landed code, and the re-run geomean is > 1.0x for at least `gpu_reduce`, `gpu_hashagg`, and `ssbm`
- [ ] §0 H3 baseline switched to real `h3-pg`; if numbers stay above 1.0x they're real, if they don't the old marketing is retracted
- [ ] §1 C7: `Significant` column prints `sig_wins / sig_losses / total` not a single conflated number
- [ ] §1 C9: Cohen's d column present; no row with |d| < 0.5 flagged as significant
- [ ] §2 M11: 1K scale removed from `ROW_SCALES`; minimum reportable scale is 10K
- [ ] §2 M12: median, p25/p75/p95 in detail tables; headline speedup uses median
- [ ] §2 M13: `pmset -g therm` captured per workload; thermal-flagged workloads labeled
- [ ] §4 W6–W10: kernel-class table replaces category table; workload count drops from 127 to ≤60

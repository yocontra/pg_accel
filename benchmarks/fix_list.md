# pg_accel Workload Classification

Generated from `benchmarks/README.md` (2026-04-11 re-run, post-BGW-fork-fix, post-spin-poll perf fix `cf709fa`).

Scope: 127 workloads, 631 (workload × scale) data points. Overall geomean vs PG parallel = **0.89x** — we are, on average, **slower** than PG parallel. Only **252 / 631** data points cleared the Bonferroni-corrected significance bar, and of those many are *losses*, not wins.

## Bucket definitions

| Bucket | Rule | Interpretation |
| --- | --- | --- |
| **A. Working** | speedup ≥ 1.10x at ≥ 2 scales with Bonferroni p < 0.05 | GPU path is a real win. Ship it. |
| **B. Not dispatching** | |speedup − 1.0| < 0.02 at every scale | Planner/cost model never injected the custom path. Zero engagement. |
| **C. Dispatching but slow** | speedup ∈ [0.85, 1.10] at any scale with GPU delta > 0 | GPU ran and tied or mildly lost. Either the kernel is wrong or the workload is CPU-favorable. |
| **D. Regression** | speedup < 0.90 at any scale with Bonferroni p < 0.05 | Active harm — custom scan is injected and measurably worse than the PG path it replaces. |

A workload can fall into multiple buckets across scales (e.g. A at 10M, D at 100K). Primary bucket is chosen by the worst scale at which the workload dispatches; the fix-priority ranking uses the bucket that matters most for product.

---

## Summary

| Bucket | Workload count | Notes |
| --- | --- | --- |
| **A. Working** | 9 | Almost entirely the H3 latlng/dist family + 4 hashjoin scales. Two raster workloads squeak in at mid-scale. |
| **B. Not dispatching** | ~42 | SSBM q2/q3/q4 (12), all `spatial_mega_*≤250v`, all `vsweep_*≤256v`, `h3_bulk`/`h3_cell_to_parent`/`h3_grid_distance`/`h3_resolution_sweep`, `oltp_point_lookup`, `proximity`, `spatial_join`, `topk_wide`, `small_table_scan`, `large_sort`/`gpu_sort_multikey`/`gpu_sort_topk_wide`/`hash_join`/`gpu_hashjoin_filter` — the BGW round-trip is live but the planner still never picks these up. |
| **C. Dispatching but slow** | ~18 | `sort_int4`/`sort_int8`/`sort_float*` except one fluke, most `spatial_sel_*`, `spatial_contains`, `spatial_multi_pred`, `index_recheck`, `expr_math_mixed`, `window_analytics`. Kernel actually runs and still loses. |
| **D. Regression** | ~58 | The meat. Every `gpu_reduce_*` @ ≥10K, every `gpu_hashagg_*` @ ≥1M, all `vsweep_*≥500v` @ 100K–1M, all `spatial_mega_*≥500v` @ 100K–1M, half the `gpu_expr_*` family @ 1M, `window_row_number`/`window_dense_rank` @ ≥100K, `raster_*` @ 1M–10M, `ssbm_q1_*` @ 1M. These are where we are actively hurting the user. |
| **Crashed** | 4 | `reduce_sum_i64` at 10K/100K/1M/10M — "connection closed". Never even reached the stats row. |

---

## Bucket A — Working (9 workloads)

| Workload | 1K | 10K | 100K | 1M | 10M | Primary driver |
| --- | --- | --- | --- | --- | --- | --- |
| `h3_latlng_res3` | 0.99x | **15.45x** | **22.91x** | **8.49x** | **5.01x** | H3 indexing loop moved off per-row plpgsql UDF |
| `h3_latlng_res9` | 0.99x | **26.89x** | **40.18x** | **14.41x** | **8.70x** | same |
| `h3_latlng_res15` | 1.00x | **36.75x** | **55.78x** | **20.39x** | **13.78x** | same, deepest res |
| `h3_dist_near` | 1.00x | **13.77x** | **19.83x** | **5.36x** | **3.55x** | H3 grid-distance SIMD |
| `h3_dist_far` | 1.00x | **11.56x** | **15.41x** | **4.18x** | **2.72x** | same |
| `h3_parent_deep` | 0.99x | **2.35x** | **2.96x** | **1.04x** | 0.62x | Regressing at 10M — kernel bound, A→D at high scale |
| `hashjoin_100k_1m` | **1.37x** | **1.48x** | **1.61x** | **1.16x** | 0.69x | Build-side hashing on GPU; 10M regression at 0.69x is a D |
| `gpu_hashjoin_large_build` | 1.02x | 1.00x | **2.08x** | **1.49x** | 1.00x | Narrow A — A only at mid scales |
| `raster_algebra_deep` | 0.97x | **1.18x** | **1.33x** | 0.52x | 0.36x | A at 10K–100K, D at 1M–10M |

**Verdict:** the *only* category that wins across the board is H3 latlng+dist. Everything else is either a narrow A with a cliff at 10M, or a mid-scale-only A.

---

## Bucket D — Regressions (priority-ordered fix list)

Ranked by `row_count × (1.0 − speedup) × probability_user_hits_it`. Top of list is where the damage is worst.

### D1 — `gpu_reduce_*` multi-agg at 1M+ (ALL of them)

| Workload | 1M | 10M |
| --- | --- | --- |
| `reduce_multi` | 0.46x | 0.24x |
| `gpu_reduce_sum` | 0.43x | 0.22x |
| `gpu_reduce_scaling` | 0.58x | 0.30x |
| `reduce_sum_f32` | 0.58x | 0.30x |
| `reduce_sum_f64` | 0.60x | 0.32x |
| `reduce_min_f64` | 0.60x | 0.32x |
| `reduce_max_f64` | 0.60x | 0.32x |

**Diagnosis:** simple reductions are ~2–4× *slower* than PG parallel at the scales that matter. The BGW IPC spin-poll fix took 1M-row multi-agg from 833 ms → 68 ms warm, but PG parallel does it in 26 ms. Root cause: our reduce path transfers data to GPU via DSM chunks (PG→shared→GPU) then reduces 64k-row tiles in a loop. PG parallel just divides the heap across 8 workers and hits L2. For small-state aggregates (one f64 per agg), the memory-copy tax is unrecoverable unless we overlap it with GPU compute *and* skip deserialization.

**Fixes required:**
1. **Single-pass fused reduce** — do sum+min+max+count in one kernel launch, not four. `reduce_multi` fires 4 kernels per chunk.
2. **Cache GPU input on repeat access** — many bench workloads query the same column 10× (warmup+iterations). We re-copy to GPU every time.
3. **Drop the 65k min_batch_size for reduce** — at 10K we are losing to CPU by 4–10× because we *do* dispatch but with DSM setup cost larger than the scan. The cost model says "big enough", but it isn't for reduce.
4. **Fix `reduce_sum_i64` crash** — 4/5 scales crash with "connection closed". The spin-poll path lost the i64 codepath during the f64→f32 cast refactor. See `pg_accel/src/gpu/mod.rs:reduce_sum_i64`.

**Priority: P0.** Reduce is table-stakes. If a GPU extension can't beat PG at `SUM(col)` at 10M rows we have no product.

### D2 — `gpu_hashagg_*` at 1M+ (ALL of them)

| Workload | 1M | 10M |
| --- | --- | --- |
| `grouped_agg` | 0.44x | 0.20x |
| `gpu_hashagg_med_card` | 0.47x | 0.21x |
| `hashagg_10g` | 0.42x | 0.23x |
| `hashagg_100g` | 0.43x | 0.21x |
| `hashagg_1kg` | 0.40x | 0.19x |
| `hashagg_10kg` | 0.47x | 0.21x |

Exception: `grouped_agg_high_card` holds at 0.96x–1.00x — our path is skipped there.

**Diagnosis:** same root cause as D1 — transfer dominates compute for small aggregate state. We pay DSM setup for every chunk and gain nothing because CPU hashagg also fits in L2.

**Fixes required:**
1. **Persistent GPU hash table across chunks** — currently rebuilt per chunk.
2. **Vectorize the group-key computation** — we're re-hashing per tuple.
3. **Cost model needs a term for `state_size_bytes × n_groups`** — if state is tiny, GPU loses. Only dispatch when `n_groups × state_size > L2_size`.

**Priority: P0.**

### D3 — `vsweep_*` and `spatial_mega_*` at 100K–1M, vertex count ≥ 500

| Workload | 100K | 1M |
| --- | --- | --- |
| `vsweep_500v` | 0.84x | 0.36x |
| `vsweep_1kv` | 0.64x | 0.27x |
| `vsweep_5kv` | 0.34x | 0.13x |
| `vsweep_50kv` | 0.22x | 0.08x |
| `vsweep_100kv` | 0.22x | 0.08x |
| `spatial_mega_500v` | 0.83x | 0.36x |
| `spatial_mega_5kv` | 0.35x | 0.13x |

**Suspected cause:** Reviewer 3 is going to hammer this. The `three_layer.rs` degenerate-input guard likely short-circuits megapoly tests to `None` → CPU recheck path runs on the CPU but pg_accel takes the credit/blame. Either:
- The GPU kernel genuinely sees the data and is slower than CPU (unlikely at this gap),
- Or the guard is triggering on all-polygon-vertex-count data and we're measuring CPU-recheck overhead plus IPC.

**Fixes required:**
1. **Audit `three_layer.rs::classify_*` for degenerate-guard trigger conditions.** Add a counter for "all-uncertain-returned" per workload, surface in `pg_accel_stats()`.
2. **Planner must gate on `max_vertex_count > threshold`** — at 50kv a single point-in-polygon test does 50k cross products. GPU can't amortize IPC.
3. **Stop using megapoly fixtures for "spatial" benchmarks** — if no real user ships 50k-vertex polygons in a table, these workloads are padding. Reviewer 1 will call this out.

**Priority: P0** for correctness (the guard bug), **P2** for the kernel work.

### D4 — `window_row_number` / `window_dense_rank` at 100K+

| Workload | 100K | 1M | 10M |
| --- | --- | --- | --- |
| `window_row_number` | 0.46x | 0.58x | 0.29x |
| `window_dense_rank` | 0.46x | 0.22x | 0.45x |

**Diagnosis:** window operators dispatch (per `gpu.window.*` spans) but lose. Likely: we materialize the entire partition to GPU, do a prefix-sum, transfer back. PG with 8 parallel workers sorts-and-scans much faster.

**Fixes required:**
1. **Tiled per-partition dispatch** — only transfer one partition at a time; overlap transfer with compute.
2. **Skip GPU if partition count > threshold** — many small partitions means IPC amortizes badly.

**Priority: P1.**

### D5 — `gpu_raster_*` at 1M+

| Workload | 1M | 10M |
| --- | --- | --- |
| `raster_ndvi` | 0.55x | 0.34x |
| `raster_slope` | 0.50x | 0.31x |
| `raster_reclass` | 0.50x | 0.31x |
| `raster_algebra_deep` | 0.52x | 0.36x |

Note that raster *wins* at 10K–100K (1.15–1.33x). It's the large-table tile scaling that breaks.

**Diagnosis:** `raster_variants.rs:15-22` shrinks tile size at high row counts so per-tile GPU launch overhead wins. Fix the tile sizing heuristic.

**Priority: P1.**

### D6 — `ssbm_q1_*` at 1M

| Workload | 1M |
| --- | --- |
| `ssbm_q1_1` | 0.53x |
| `ssbm_q1_2` | 0.63x |
| `ssbm_q1_3` | 0.67x |

`ssbm_q2_*`, `q3_*`, `q4_*` all sit at 0.95–1.02x — they are **not dispatching** (bucket B). The `q1_*` regression means we *do* inject for q1 and we're slower. **This is bucket D for q1 only, bucket B for q2–q4.**

**Priority: P0** for `q1_*` regression (canonical OLAP benchmark), **P1** for getting `q2_*`–`q4_*` to dispatch at all.

### D7 — `gpu_expr_*` at 1M

| Workload | 100K | 1M | 10M |
| --- | --- | --- | --- |
| `gpu_expr_complex` | 0.84x | 0.27x | 1.00x |
| `expr_3pred` | 0.99x | 0.30x | 1.00x |
| `expr_4pred` | 0.87x | 0.28x | 1.00x |
| `expr_arith_chain` | 0.77x | 0.24x | 1.00x |
| `expr_deep_arith` | 0.78x | 0.24x | 1.00x |
| `expr_multi_or` | 0.93x | 0.29x | 0.16x |
| `expr_sqrt_heavy` | 0.94x | 0.30x | 1.00x |
| `expr_pow_chain` | 0.74x | 0.24x | 1.00x |

Pattern: dispatches at 1M, gives up at 10M (no dispatch → 1.00x). Almost every expr workload loses 3–4× at 1M, then the cost model bails out at 10M.

**Diagnosis:** likely same root cause as reduce — per-chunk DSM copy cost. Expression evaluation is tuple-at-a-time inside PG; we batch it to GPU but pay transfer for every chunk.

**Fixes required:**
1. **GpuExpr should use VectorizedScan** (existing task #16, still pending).
2. **Tiled dispatch that overlaps compute with next-chunk copy.**
3. **Re-engage cost model at 10M** — the current threshold bails out when it should win.

**Priority: P1.**

### D8 — `expr_multi_or` at 10M = 0.16x

Outlier: expr_multi_or loses **6×** at 10M. Probably a codegen bug in the predicate compiler. Needs isolation.

**Priority: P1.**

### D9 — `spatial_sel_*` at 1M

| Workload | 1M |
| --- | --- |
| `spatial_sel_1pct` | 0.43x |
| `spatial_sel_10pct` | 0.35x |
| `spatial_sel_50pct` | 0.24x |
| `spatial_sel_90pct` | 0.21x |

Pattern: selectivity-sweep workloads lose *harder* as selectivity grows. That's the wrong direction — high selectivity should favor GPU because more rows pay the compute cost.

**Diagnosis:** we're likely materializing the surviving rows into PG tuple format on the CPU after the GPU filter. Late materialization is broken somewhere in the scan executor.

**Priority: P1.**

### D10 — `scale_1m_mega500v` = 0.36x everywhere

| Workload | 1K | 10K | 100K | 1M | 10M |
| --- | --- | --- | --- | --- | --- |
| `scale_1m_mega500v` | 0.35x | 0.36x | 0.36x | 0.36x | 0.36x |

Flat 0.36× at every scale is pathological — this workload has a 1M-vertex polygon fixture that *always* triggers the megapoly loser path regardless of row count. Same root cause as D3 (degenerate guard / no planner gate).

**Priority: P1.**

---

## Bucket B — Not dispatching (~42 workloads)

Speedup sits at 1.00x ± 0.02 at every scale. Either the planner cost function rejects GPU or there is no adapter registered. `cpu_fallback_count` should be 0 (per CLAUDE.md rule 11) — if that holds, then the planner skipped, not the executor.

### B1. SSBM q2 / q3 / q4 (12 workloads)

`ssbm_q2_1`…`ssbm_q4_3` all clustered at 0.98–1.02x. Star-schema joins with date/customer/supplier/part filters. The entire point of an OLAP GPU extension. Reviewer 3 will lead with this. **Fix: planner must inject GPU hashjoin + hashagg paths for multi-way star joins.**

**Priority: P0 (product-existential).**

### B2. `spatial_mega_100v`, `spatial_mega_250v` and `vsweep_4v`…`vsweep_256v` (13 workloads)

Low-vertex spatial: planner must be rejecting these because `vertex_count × row_count` is too low. Fine logic, wrong threshold — at vsweep_256v × 100K rows we should at least break even. Probably a cost-model constant that got tuned to avoid the D3 losses and over-corrected.

**Priority: P2.** (Not hurting, just not helping.)

### B3. `h3_bulk`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_resolution_sweep` (4 workloads)

H3 *indexing* wins (that's A1–A5). H3 ops on *existing* cells tie. The latlng→cell kernel is our star; everything else in the H3 category isn't dispatching. Likely: the adapter only classifies `ST_H3_Latlng` + `ST_H3_Distance`; the other ops fall back to PG.

**Priority: P2.**

### B4. Scan / join / OLTP idle-dispatch (B4.x)

`oltp_point_lookup`, `proximity`, `spatial_join`, `topk_wide`, `small_table_scan`, `large_sort`, `gpu_sort_multikey`, `gpu_sort_topk_wide`, `hash_join`, `gpu_hashjoin_filter`, `spatial_filter`, `spatial_complex_poly`, `spatial_zigzag`, `spatial_star_1kv`, `mixed_join_agg`, `mixed_spatial_sort`, `filtered_grouped_agg`, `spatial_agg`, `spatial_sort` — all at 0.96–1.04x across scales.

Most of these **should** dispatch and tie or win. Something in the planner's cost function is rejecting them systematically.

**Priority: P1 collectively.** Split into individual investigations after the cost model is re-audited.

---

## Bucket C — Dispatching but slow (~18 workloads)

These actually fire a GPU kernel and still lose, but by < 15%. Either the kernel is mediocre or the workload is just CPU-favorable.

| Workload | Worst scale | Speedup | Notes |
| --- | --- | --- | --- |
| `sort_int4` | 1M | 0.97x | GPU sort ties PG merge sort on int4. Ship or remove. |
| `sort_int8` | 10M | 0.97x | same |
| `sort_float4` @ 1M | 1M | **3.33x** | One-off huge win — possibly noise, re-run to confirm |
| `sort_float8` | 10M | 1.00x | tied |
| `index_recheck` | 1M | 0.90x | boundary case |
| `expr_math_mixed` | all | ~1.00x | tied |
| `window_analytics` | 100K/10M | 1.15x/1.16x | mild A, mild C |
| `spatial_contains` | 1M | 0.93x | close but losing |
| `spatial_multi_pred` | 100K | 0.96x | close but losing |
| `spatial_sel_90pct` @ 10K | 10K | 0.95x | tied |
| `mixed_megapoly_agg` @ 100K | 100K | 0.86x | close to D |

**Priority: P3.** None of these are bleeding. Revisit after D-bucket is fixed.

---

## Cross-cutting correctness issues

1. **`reduce_sum_i64` crash** — 4 scales "connection closed". Blocks any claim the bench was honest. **Must fix before re-run is credible.**
2. **`cpu_fallback_count` vs 1.00x workloads** — if the planner is silently skipping B-bucket workloads, `cpu_fallback_count` should still be 0 (because we never entered the custom scan at all). But we need a *second* counter, `planner_rejected_count`, to prove the planner intentionally skipped. Without it, Reviewer 3's cut stands.
3. **Degenerate-guard short-circuit** — the `three_layer.rs` guard may be turning the D3 workloads into CPU-recheck measurements. Add a guard-trigger counter to `pg_accel_stats()`.
4. **Per-workload `gpu_kernel_executions` delta not captured in `results.json`** — we need this to split B from C definitively. The harness currently only records timings.

---

## Fix priority rollup

| Priority | Items |
| --- | --- |
| **P0 (must-fix for credible product)** | D1 reduce, D2 hashagg, D3 degenerate-guard audit, D6 ssbm_q1, B1 ssbm_q2-q4, `reduce_sum_i64` crash |
| **P1** | D4 window, D5 raster tiles, D7 gpu_expr + task #16, D8 expr_multi_or codegen, D9 late materialization, D10 scale_1m_mega500v, B4 planner cost audit |
| **P2** | B2 low-vertex spatial thresholds, B3 H3 ops adapter coverage, D3 kernel (after guard fix) |
| **P3** | Bucket C cleanup / retire cruft workloads |

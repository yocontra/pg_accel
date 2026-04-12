# Review 1 — The HPC Benchmarking Skeptic

**Reviewer:** Patterson/Gray composite — the one who rejected your paper at SIGMOD
**Report under review:** `/Users/contra/Projects/pg_accel/benchmarks/README.md`
**Verdict:** *Reject. Major revision required. The headline numbers are not defensible.*

---

## Mandatory questions, answered in the report's own words

**Q1. Geometric mean speedup across all workloads.**
The report discloses it directly in its own summary table:

> `| **overall** | **631** | **0.89x** | **252 / 631** |`  — `benchmarks/README.md:66`

**0.89x overall.** The product under review is, on average, *slower than stock PostgreSQL with parallel workers*. Only one category exceeds 1.0x: `gpu_h3` at 2.85x, and — see Sin #3 — that number is a single-kernel artifact. Six of the nine GPU categories are below 1.0x (`gpu_expr 0.77x`, `gpu_hashagg 0.66x`, `gpu_raster 0.76x`, `gpu_reduce 0.50x`, `gpu_spatial 0.76x`, `gpu_window 0.83x`), and the vertex-sweep padding category sits at 0.69x.

**Q2. Wins vs losses after Bonferroni.**
The report advertises "252 / 631 significant." I parsed every detailed result table in the README. The decomposition the author chose not to print:

- Bonferroni-significant **wins** (speedup > 1.0, `Significant? = YES`): **42**
- Bonferroni-significant **losses** (speedup < 1.0, `Significant? = YES`): **203**
- Raw α=0.05 count claimed: 252

**Nearly five times as many scales are statistically slower than statistically faster** after the multiple-testing correction the author himself applied. The "252 / 631" headline is a count of *statistically significant differences of any sign* dressed up as if it were a count of wins. This is the exact rhetorical trick Jim Gray called out in 1993.

**Q3. Padding in spatial workloads; H3 weighting.**
Of the 85 `vertex_sweep` rows, **every single one** is `ST_Intersects(point, polygon)` with a different polygon vertex count (4, 16, 32, 64, 128, 256, 500, 750, 1k, 1.5k, 2k, 3k, 5k, 10k, 25k, 50k, 100k). Seventeen near-identical workloads calling the identical `point_in_ring` kernel. Add `spatial_mega_{100,250,500,1k,2k,5k}v` inside `gpu_spatial`: **23 vertex-sweep variants of the same kernel**. For H3, the category geomean of **2.85x** is almost entirely driven by 3 `h3_latlng_res{3,9,15}` workloads (geomean ≈ **10.1x**). Remove them and the `gpu_h3` geomean collapses to **1.66x**; of `gpu_h3`'s 22 Bonferroni-significant wins, **20 come from just 5 workloads** (res3/res9/res15/dist_near/dist_far) all exercising the same trig-kernel family. The other five H3 workloads produce two wins between them.

**Q4. Is `h3_latlng_res15` at 58.33x a real speedup or a baseline pathology?**
The claimed speedup row:

> `| 100K | 2.13 +/- 0.10 | 118.73 +/- 0.75 | **55.78x** | 0.0000 | 0.0000 | YES |` — `benchmarks/README.md:1034`

Two facts make this number indefensible:

1. The `h3_latlng_to_cell` function is **registered by pg_accel itself** (`pg_accel/src/adapters/h3.rs:15`). It is not the `h3-pg` extension. When the baseline runs with `gpu_enabled=off`, Postgres per-row invokes the Rust `#[pg_extern]` SQL function wrapper through the function-call interface. That is not "PostgreSQL h3" — it is pg_accel's *own slow path*, used as the "baseline." 118.73 ms to hash 100K points to H3 cells is pathological; DuckDB and HeavyDB hit this in single-digit ms on a laptop with actual h3-pg.
2. `benchmarks/plans.txt` confirms the accel side is not even on the GPU:

   > `GPU Dispatched: false` — `benchmarks/plans.txt:7541,7559,7577` (h3_latlng_res15 at 100K / 1M / 10M)

   The pg_accel path reports `GPU Dispatched: false` for *every* h3_latlng_res15 scale that the README calls a 20-55x win. The "58x" number is not an accel-vs-PG comparison; it is *pg_accel's expression-interpretation path vs pg_accel's own per-row fcinfo path*, both running on CPU, one inlined and one not. That is a bug report about your own UDF wrapper, not a GPU benchmark.

**Q5. Workloads users run, or workloads engineered to produce a winner?**
See Sin #9. The distribution of wins is not consistent with "we tested what users run." It is consistent with "we wrote 127 queries until one won."

---

# Numbered list of benchmark-methodology sins

### 1. The headline number is buried and the product is, on average, slower than the thing it accelerates.

> `| **overall** | **631** | **0.89x** | **252 / 631** |` — `benchmarks/README.md:66`

A product whose geometric mean across its own hand-picked workload suite is **0.89x** should not be shipping a benchmark document; it should be fixing the regressions. Burying the only number that matters at line 66, then filling the next 2,140 lines with tables the reader has to aggregate themselves, is the oldest trick in the Gray playbook.

### 2. "252 / 631 significant" is statistical laundering of losses into wins.

> `| gpu_reduce | 36 | 0.50x | 23 / 36 |` — `benchmarks/README.md:58`

`gpu_reduce` reports "23 / 36 significant" next to a **0.50x geomean**. Of course they are significant — they are significantly *slower*. Twenty-three out of thirty-six scales of GpuReduce are Bonferroni-significant regressions against stock Postgres. Printing that count in the same column where an honest report would print "wins" is indistinguishable from fraud. By my parse of every detailed table, it is **42 wins vs 203 losses** after Bonferroni. Print the sign.

### 3. The one category that exceeds 1.0x is inflated by three near-identical workloads built on the same kernel.

> `| h3_latlng_res3 | 0.99x | **15.45x** | **22.91x** | **8.49x** | **5.01x** |`
> `| h3_latlng_res9 | 0.99x | **26.89x** | **40.18x** | **14.41x** | **8.70x** |`
> `| h3_latlng_res15 | 1.00x | **36.75x** | **55.78x** | **20.39x** | **13.78x** |`
> — `benchmarks/README.md:141–143`

Three workloads differ only in the literal integer argument (`3`, `9`, `15`) to the same function. They hit the same `h3_latlng_to_cell` kernel; the resolution argument affects a bit mask, not the work done per call. They are not three measurements. They are one measurement counted three times, contributing *fifteen* scale rows to the aggregate. Drop them and the overall geomean moves from 0.89x to **0.84x**; drop the five h3 trig-kernel workloads and the entire `gpu_h3` category geomean collapses to 1.66x. A VLDB reviewer would reject this as a single data point.

### 4. The "58x" headline is an artifact of baseline UDF dispatch, not GPU speed.

> `h3_latlng_to_cell(point(lng, lat), 15)` — `pg_accel_bench/src/workloads/h3_variants.rs:87`
> `GPU Dispatched: false` — `benchmarks/plans.txt:7541`

`h3_latlng_to_cell` is registered by the pg_accel extension itself, not by `h3-pg`. When the baseline runs, the "PG parallel" comparand is Postgres evaluating a per-row call to pg_accel's own Rust-wrapped UDF — 118 ms for 100K rows. The accel side reports `GPU Dispatched: false` in `plans.txt` for h3_latlng_res{3,9,15} at every winning scale. The speedup is pg_accel's CPU inliner beating pg_accel's per-row fcinfo wrapper. Whatever the GPU is doing here, the benchmark does not measure it.

### 5. The report admits it cannot tell whether many of its workloads even ran on the GPU.

> `Workloads where |speedup − 1| < 0.02. pg_accel almost certainly did not dispatch a GPU path for these — check benchmarks/plans.txt (or run with --capture-plans) to confirm whether a Custom Scan node appears in the plan. If it does not, the planner hook is declining the path.` — `benchmarks/README.md:1877`

Two hundred-plus rows are in a "Non-Dispatching Workloads" appendix where the *author himself* says pg_accel "almost certainly did not dispatch a GPU path." Every one of those rows is nevertheless counted in the 631-row geometric mean, in every category's headline count, and in the "252 / 631 significant" number. You do not get to include rows where the product under test didn't run and then average them as if it did. Either drop them or mark the category with a giant "N/A."

### 6. The mega-polygon vertex sweep is 17 variations of one kernel, counted as 17 independent workloads.

> `| vsweep_100kv | 1.00x | 1.00x | 0.22x | 0.08x | 1.00x |` — `benchmarks/README.md:128`

The `vertex_sweep` category is 17 near-identical workloads (`vsweep_4v` through `vsweep_100kv`), all executing `ST_Intersects(point, polygon)` with a differently-sized polygon. One kernel, 85 rows, 48 of them Bonferroni-flagged. Adding `spatial_mega_{100,250,500,1k,2k,5k}v` makes it 23 restatements of the same kernel. Reporting this as a dense grid of results is selection bias dressed as thoroughness — and the grid reveals a catastrophic crossover where accel collapses to **0.08x at 1M rows on 50k-vertex polygons**. A category-count inflation that makes the numbers look better also, by symmetry, makes the regressions *worse* once someone aggregates them honestly.

### 7. Scale-sweep workloads report five identical numbers across five different scales.

> `| scale_1m_mega500v | 0.35x | 0.36x | 0.36x | 0.36x | 0.36x |` — `benchmarks/README.md:187`

The three `scale_*_mega500v` workloads each produce five rows that are functionally identical (the timing variance is noise), because the "scale" axis in these workloads is the *table size baked into the query name*, not the row-count axis swept per iteration. Every row is counted as an independent observation in the 631-row geomean. This inflates *regressions* (3 × 5 rows all at 0.35–0.97x) the same way H3 inflates wins.

### 8. A workload that crashes the backend at four of five scales gets one good row in the headline table.

> `**Crashes:** 4 scale(s) crashed and were excluded from results.` — `benchmarks/README.md:45`
> `| reduce_sum_i64 | 1.00x | crash | crash | crash | crash |` — `benchmarks/README.md:78`

`SUM(bigint)` — one of the three most common aggregations in any OLAP workload — crashes pg_accel at 10K, 100K, 1M, and 10M rows. The workload appears in the headline Results table as "1.00x / crash / crash / crash / crash" and its one surviving 1K row is included in the gpu_reduce category geomean. "Excluded from results" is doing a lot of work here: a product that cannot survive a 10K-row `SUM(int8)` has no business reporting a geometric mean at all. Jim Gray's rule: **if it crashed, the benchmark score is DNF, not "excluded."**

### 9. Workload selection is consistent with p-hacking, not with "what users run."

> `| gpu_h3 | 50 | 2.85x | 23 / 50 |` — `benchmarks/README.md:54`
> `| ssbm | 65 | 0.95x | 4 / 65 |` — `benchmarks/README.md:64`

Standard OLAP workload (SSBM, 65 rows): 4/65 Bonferroni wins, geomean 0.95x. Custom H3 workload the author designed around his own kernel: 23/50 wins, geomean 2.85x. The ratio of win-rates is 7.5x. The H3 category represents the case where the author controls both the query *and* the baseline function (see Sin #4); SSBM represents the case where neither is under his control. The correlation between "author controls the baseline" and "pg_accel wins" is the only signal in this benchmark. In the entire 631-row corpus, Bonferroni-significant wins outside `gpu_h3` total **20 across 581 rows** — a 3.4% win rate that would be consistent with false positives if Bonferroni were not applied. It was applied, so the 20 are real — but they are real wins on `hashjoin_*_1m` and `sort_float4 @ 1M` and a handful of window queries. That is the honest headline: "pg_accel wins large-build hash joins at medium scale and one float4 sort outlier." Say that.

### 10. The geomean is reported as the category summary, and the category-count inflation is therefore load-bearing.

> `Geometric mean of per-workload speedups (parallel_mean / accel_mean), broken out by category.` — `benchmarks/README.md:49`

A geometric mean is *supposed* to be scale-free, which is precisely why adding 17 `vsweep_Nv` rows does not inflate it on a per-row basis. But the category column uses these rows to justify the category's sample size ("Workloads | 50 | 35 | 85"), and the implicit message is "this many workloads agree." They do not agree; they are not independent. You cannot claim a 50-row sample size while also claiming 10 of those rows are three parameter values of one kernel. Either drop to one row per kernel (then gpu_h3 is 5 workloads, not 10; vertex_sweep is 1 workload, not 17) or state explicitly on every category line that rows are not independent. Gray's rule: **declare your independence assumption; never assume it silently.**

### 11. The biggest regressions are buried in the per-workload section; the front-page table only shows 1.00x rounding.

> `| spatial_mega_5kv | 1.01x | 0.99x | 0.35x | **0.13x** | 0.99x |` — `benchmarks/README.md:111`
> `| vsweep_50kv | 0.99x | 1.00x | 0.22x | **0.08x** | 1.00x |` — `benchmarks/README.md:127`

**0.08x** means pg_accel is **12.5× slower** than stock Postgres on a 1M-row `ST_Intersects` with a 50k-vertex polygon. The headline Results table at line 66 shows an "overall 0.89x" that understates this by two orders of magnitude, because the geomean washes it out against dozens of 1.00x rows where pg_accel silently declined the query. If a user runs a single one of these, pg_accel costs them an order of magnitude. That regression should be on page one, not page eight.

### 12. The 10M scale mysteriously "recovers" to ~1.00x for exactly the workloads that crater at 1M — and nobody asks why.

> `| vsweep_50kv | 0.99x | 1.00x | 0.22x | **0.08x** | 1.00x |` — `benchmarks/README.md:127`

At 10M rows, accel is back to 1.00x — which means *pg_accel declined the query entirely*, because declining it makes it run at PG speed. The author's own "Non-Dispatching Workloads" section (Sin #5) confirms the pattern. So the data actually says: at 1M rows pg_accel *tries* and fails catastrophically; at 10M rows it gives up and hits its no-op path. That is not "recovery"; that is the planner hook bailing out under the cost model's own weight. Reporting 10M as a separate data point and averaging it with 1M turns "catastrophic crash at the only scale that matters" into a respectable 0.54x geomean for the workload. This is cherry-picking via the cost model.

### 13. `raster_*` wins only at toy scales and loses everywhere real.

> `| raster_algebra_deep | 0.97x | **1.18x** | **1.33x** | 0.52x | 0.36x |` — `benchmarks/README.md:192`

All four `gpu_raster` workloads show the same curve: ~1.2–1.33x at 10K–100K, then **0.3–0.5x at 1M and 10M**. The headline bolding is on the small-scale numbers. A raster operation at 10K pixels (a 100×100 tile) is not "raster processing"; it is a unit test. At 1M pixels — still a small drone frame — pg_accel is **2× slower** than Postgres. Shipping the 1.33x number as evidence of GPU raster acceleration is selection bias across the scale axis, even within one workload.

### 14. Variance in the GPU timings points at cold-start contamination that the "randomized ordering" note does not address.

> `| reduce_sum_f32 | 1K | 0.16 +/- 0.02 | 0.15 +/- 0.02 | **0.97x** | 0.2870 | 1.0000 | no |`
> `| reduce_sum_f32 | 10K | 1.61 +/- 0.35 | 0.42 +/- 0.02 | **0.26x** | 0.0000 | 0.0015 | YES |` — `benchmarks/README.md:265–266`

At 10K rows, the GPU side has σ=0.35ms on a 1.61ms mean — a **22% coefficient of variation**. The baseline has 5% CV at the same scale. This is the fingerprint of kernel-launch or shader-compile jitter on the GPU side, not steady-state throughput. The methodology section promises "randomized ordering per iteration" — that does not address first-call shader compilation. With only 10 iterations and 1 warmup, the compile-once-amortize cost is still bleeding into every measurement at small N. Either run 100 iterations, or report a median after dropping the top two, or state the confound explicitly.

### 15. The "timing raw" methodology is not defended for microsecond measurements.

> `| Iterations | 10 |` — `benchmarks/README.md:34`
> `| 1K | 0.16 +/- 0.01 | 0.16 +/- 0.01 | **0.98x** | 0.3852 | 1.0000 | no |` — `benchmarks/README.md:209`

Ten iterations of a 160-microsecond measurement, taken over the Postgres wire protocol, using wall-clock, on macOS. The protocol round-trip floor on localhost is **tens of microseconds**, and `libpq` buffering / kernel scheduling jitter eats the rest. Every 1K-row row in this report is measuring the client harness, not the database. Jim Gray's rule: **report only measurements that exceed the noise floor of the instrument by 100×**. Every 1K row in this report should be struck.

### 16. `spatial_mega_5kv @ 10M` takes 809 ms on GPU vs 800 ms on Postgres — and is flagged YES/significant.

> `| 10M | 809.59 +/- 1.47 | 800.20 +/- 1.35 | **0.99x** | 0.0000 | 0.0001 | YES |` — `benchmarks/README.md:921`

The report flags a **0.99x** result as Bonferroni-significant. It is significant because the variances are tiny — a t-test on tight distributions finds any mean difference. The *effect size* is 1.2%, i.e., meaningless. That the methodology table lists "Cohen's d effect size" as a statistical test and then never gates on it is a documentation lie. A serious report would drop any row with |d| < 0.5 regardless of p.

### 17. Per-workload "category" assignments do double duty as both partition and padding.

> `| gpu_spatial | 100 | 0.76x | 60 / 100 |` — `benchmarks/README.md:60`
> `| vertex_sweep | 85 | 0.69x | 48 / 85 |` — `benchmarks/README.md:65`

`vertex_sweep` is its own category — 85 rows of point-in-ring at different vertex counts — and *also* the dominant content of `gpu_spatial` (which has its own 6 `spatial_mega_*v` variants plus others). The same kernel is double-counted in two top-level categories, and the aggregate "Workloads" column sums to **631 rows from 127 workloads**, neither of which corresponds to a count of distinct GPU kernels under test. By my count, the entire benchmark exercises at most **9 distinct GPU kernels**. Reporting 631 row-measurements as if they were 631 independent experiments is the precise definition of category-count inflation.

### 18. Ordering-randomization is claimed; warmup is one iteration; no temperature/throttling discussion for an M2 Max.

> `**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias.` — `benchmarks/README.md:43`
> `| Warmup iterations | 1 |` — `benchmarks/README.md:35`

Apple Silicon laptops throttle. A 10M-row `ST_Intersects` over a 100k-vertex polygon — `vsweep_100kv @ 10M` at 5,499 ms — runs the GPU under load for five seconds per iteration. Ten iterations is 50 seconds of sustained GPU work. There is no mention of `powermetrics`, thermal state, `pmset`, or any check that the chip wasn't downclocked during the second half of a run. The randomized ordering addresses *cache*, not *thermal*, and on M2 Max thermal is the dominant noise source above 100 ms. One warmup iteration does not flush this.

### 19. The report never publishes a single median, only means ± σ.

> `| 10M | 175.99 +/- 12.84 | 2424.70 +/- 2.13 | **13.78x** | 0.0000 | 0.0000 | YES |` — `benchmarks/README.md:1038`

175.99 ± 12.84 ms is a 7% CV on the winning side, and 0.09% CV on the baseline. A distribution with 75× more variance on one side than the other is not well-summarized by a mean. The Jim Gray rule: **when the variance is asymmetric, publish the distribution**. Histograms or at least quartiles. The report publishes neither — only mean and σ, which the t-test then turns into a p-value the author trusts to six decimals.

### 20. "p (Bonferroni) = 0.0000" on a single-tailed-looking family is numerically impossible and indicates the family size is mis-declared.

> `| 10M | 2.13 +/- 0.10 | 118.73 +/- 0.75 | **55.78x** | 0.0000 | 0.0000 | YES |` — `benchmarks/README.md:1034`

Every "significant" row in the report carries `p = 0.0000 / p_bonf = 0.0000`. A Bonferroni-corrected p-value of exactly zero implies the raw p was below the numerical precision of a 32-bit float *and* survived multiplication by 631 without underflow — which it cannot, unless `p_bonf` is being computed as `min(1.0, p × N)` and then truncated at four decimals, hiding whether the correction is even binding. Without the underlying p, the reader cannot tell a 1e-10 raw p (trivially Bonferroni-safe) from a 1e-4 raw p (right at the 631-test cutoff of 7.9e-5). Publish p to at least six significant figures or this whole column is theatre.

---

## What an honest version of this report would say

> "We tested pg_accel across nine GPU categories. On our dominant workload class — large point-in-polygon spatial predicates — pg_accel is **significantly slower** than stock PostgreSQL with parallel workers. Our hash-join path wins in the 100K–1M regime for large build sides. Our H3 latitude-to-cell path wins against our own per-row UDF wrapper, which is not a PostgreSQL-vs-GPU comparison and we are investigating why our own baseline is so slow. `SUM(int8)` crashes the backend and we have excluded it. Across 631 scales the overall geometric mean is 0.89x and we do not recommend enabling pg_accel for general workloads at this time."

That is the truth. The current report is the marketing version of it. Fix.

---

**Signed,**
Reviewer 1 — *The HPC Benchmarking Skeptic*
*"A benchmark that cannot defend itself is a benchmark that lies."*

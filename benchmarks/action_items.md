# pg_accel Benchmark Action Items

Generated: 2026-04-11
Benchmark commit: <fill after re-run>

<!--
  This file is populated AFTER the three reviews in benchmarks/reviews/ are written.
  Each section below maps to a Phase 4 bucket from plans/zippy-wandering-allen.md.
  Items must cite the reviewer(s) that raised them and the specific workload or
  harness file they affect.
-->

## 1. Correctness Problems

<!--
  Anything that implies the benchmark is lying. Populated after reviews.
  These BLOCK publication. Examples:
    - GPU never actually runs for a workload we claim it accelerates
    - Degenerate-input guard short-circuiting an entire workload to CPU recheck
    - cpu_fallback_count > 0 during a run labeled "GPU vs CPU"
-->

- [ ] _TBD from reviewer 1_
- [ ] _TBD from reviewer 2_
- [ ] _TBD from reviewer 3_

## 2. Methodology Fixes

<!--
  Harness changes. Populated after reviews. Expected items from the plan:
    - Bonferroni correction option in pg_accel_bench/src/stats.rs
    - Realistic-PG GUC profile in runner.rs (shared_buffers=8GB, work_mem=256MB,
      max_parallel_workers_per_gather=8)
    - plan_capture mode that saves EXPLAIN (VERBOSE, BUFFERS) alongside timings
    - Geometric-mean aggregate reported at the top of README.md, not buried
    - Raw wall clock mode that does not use EXPLAIN ANALYZE
    - Cold-cache vs warm-cache breakdown
    - min_batch_size sweep / hardware-derived default
-->

- [ ] _TBD_

## 3. Engineering Work

Ranked by impact, worst first.

### D. Regressions (must fix or revert injection)

<!--
  speedup < 0.90 at any scale with p < 0.05. These actively harm users.
  Fix the GPU path or remove the planner hook for this pattern.
-->

- [ ] _TBD from fix_list.md bucket D_

### B. Non-dispatching paths (planner / cost gaps)

<!--
  |speedup - 1.0| < 0.02 at every scale AND gpu_kernel_executions delta == 0.
  Root cause is planner/cost model never injecting the custom path.
-->

- [ ] _TBD from fix_list.md bucket B_

### C. Dispatching but slow (kernel work)

<!--
  speedup in [0.85, 1.05] BUT gpu_kernel_executions > 0 AND cpu_fallback_count == 0.
  GPU actually ran and lost. Kernel work, batch-size tuning, or genuine
  CPU-favorable workload that should be dropped.
-->

- [ ] _TBD from fix_list.md bucket C_

## 4. Cuttable Workloads

<!--
  Removed to reduce category-count inflation. Reviewer 1 (HPC Skeptic) will
  identify these. Anything that exists only to pad the matrix or that measures
  the same kernel as another workload under a new name.
-->

- [ ] _TBD from reviewer 1_

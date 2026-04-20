---
name: Benchmark Methodology
description: How to run and report pg_accel benchmarks — two-arm Accel vs PG-parallel comparison, statistical rigor, workload layout, honest reporting rules.
---

# pg_accel Benchmark Methodology

## Hard Rule: Two Arms, Never Three

The only modes the harness measures are:

- `BenchMode::Accel` — extension ON, `max_parallel_workers_per_gather = DEFAULT`
- `BenchMode::PgParallel` — extension OFF (`SET pg_accel.enabled = off`), `max_parallel_workers_per_gather = DEFAULT`

See `pg_accel_bench/src/runner.rs:891` for the enum and `:910` for the per-mode `SET`s. Both arms let PG choose its default worker count — the comparison is always pg_accel vs parallel PG.

There is no `BenchMode::PgSingle`, no `single_ms` field, no "vs Single" column. The rule in top-level `CLAUDE.md` (Benchmark Rule #11) is absolute: comparing against `max_parallel_workers_per_gather = 0` is deceptive because 100% of production PG uses parallel query. Any PR adding a single-threaded arm must be rejected.

## Harness Entrypoints

```bash
just bench                 # iterations=10 warmup=5, toy GUCs, setup+run
just bench-rigorous        # iterations=30 warmup=5, realistic GUCs, plan capture
cargo run -p pg_accel_bench --release -- <subcommand>
```

Subcommands (`pg_accel_bench/src/main.rs:29`): `Setup`, `Run`, `Report`, `Validate`.

Key `Run` flags (`main.rs:54`):
- `--iterations N` (default 10) / `--warmup N` (default 5, min 5 for warm cache)
- `--timing raw|explain|both` (default `raw` — `Instant::now()` wall clock)
- `--cache-mode warm|cold|both` (default `warm`)
- `--speedup-from median|mean` (default `median`)
- `--realistic-gucs` — apply `GucProfile::realistic()` (`runner.rs:163`)
- `--capture-plans` — dump `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)` to `benchmarks/plans.txt`
- `--skip-guc-verify` — dev-only override for `PGC_POSTMASTER` mismatch hard-fail
- `--workload NAME` / `--category CSV` — filter

Default connection: `host=localhost port=28817 dbname=postgres` (pgrx default for PG17).

## Row Scales

Fixed constant, not configurable: `ROW_SCALES = [10_000, 100_000, 1_000_000, 10_000_000]` (`runner.rs:23`). Minimum reportable scale is 10K — lower measurements fall below the libpq round-trip noise floor.

## Timing Model

`TimingMode` (`runner.rs:43`):
- `RawWallClock` (default) — `client.simple_query()` wrapped in `Instant::now()`. No `EXPLAIN` instrumentation. Use for any published number.
- `ExplainAnalyze` — parses `Execution Time:` from `EXPLAIN ANALYZE`. Penalizes parallel plans ~15-25% vs Custom Scan because per-tuple instrumentation fires in every worker. Historical default, kept for audit.
- `Both` — runs each iteration twice; stats use the raw column.

`CacheMode` (`runner.rs:70`):
- `Warm` (default) — measure after warmup. `DISCARD ALL` between iterations, but OS page cache is retained.
- `Cold` — `sync && purge` (macOS) or `echo 3 > /proc/sys/vm/drop_caches` (Linux) between every timed iteration; warmup disabled.
- `Both` — side-by-side columns.

## Order Randomization

Each iteration randomly chooses Accel-first or Parallel-first (`runner.rs:794`). Kills systematic bias from shared-buffer priming and plan-cache carry. `DISCARD ALL` runs between the two timings within an iteration.

## Statistical Methodology

Implemented in `pg_accel_bench/src/stats.rs`:

| Fn | Line | What |
|---|---|---|
| `mean` | `:38` | arithmetic mean |
| `median` | `:49` | = `percentile(xs, 50.0)` |
| `percentile` | `:57` | linear interpolation (NumPy inclusive) |
| `stddev` | `:91` | sample stddev |
| `cv_percent` | `:80` | coefficient of variation (%) |
| `confidence_interval_95` | `:120` | t-distribution 95% CI |
| `speedup` | `:140` | `baseline_mean / experiment_mean` |
| `welch_t_test_p` | `:159` | two-sample Welch's t-test p-value |
| `paired_t_test_p` | `:201` | paired t-test (uses iteration pairing) |
| Cohen's d | `:236` | effect size |

Report policy:
- Headline speedup uses **median-of-parallel / median-of-accel** by default (`SpeedupSource::Median`, `runner.rs:89`) — robust to cold-start jitter.
- Flag significance when Welch's p < 0.05 AND |Cohen's d| ≥ 0.8.
- CV > 15% on a cell is surfaced in the report, not silently smoothed.
- All iterations are kept; no outlier removal. 3σ points are labeled, not dropped.

## Workload Layout

Workloads live in `pg_accel_bench/src/workloads/` and implement the `Workload` trait (`workloads/mod.rs:104`):

```rust
pub trait Workload: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn category(&self) -> &'static str { "gpu" }
    fn setup_sql(&self, rows: usize) -> Vec<String>;
    fn pre_query_sql(&self) -> Vec<String> { vec![] }
    fn query_sql(&self) -> String;
    fn baseline_query_sql(&self) -> Option<String> { None }  // override when accel/baseline SQL must differ
    fn cleanup_sql(&self) -> Vec<String>;
}
```

Registration: `all_workloads()` in `workloads/mod.rs:159`.

Categories (used for `--category`):

- `gpu_reduce`, `gpu_hashagg`, `gpu_sort`, `gpu_hashjoin`
- `gpu_spatial`, `gpu_window`, `gpu_expr`, `gpu_raster`
- `ssbm` (Star Schema Benchmark Q1.1–Q4.3)
- `mixed`
- `regression` — workloads expected to be ~1.00x (e.g. `OltpPoint`, `SmallTable`). They prove the cost model correctly declines to dispatch. Slowdown here = overhead regression to investigate.

`baseline_query_sql()` exists because some function names (e.g. `public.h3_latlng_to_cell`) are intercepted by pg_accel's adapters — the baseline arm must call a path pg_accel cannot hook, often a schema-qualified alias or the underlying `h3-pg` function by a different symbol.

## GUC Profiles

`GucProfile` (`runner.rs:111`) has two built-ins:

- `toy()` (`:138`) — pgrx dev defaults: 128MB `shared_buffers`, 4MB `work_mem`, 2 parallel workers. Not publishable.
- `realistic()` (`:163`) — 16GB `shared_buffers`, 512MB `work_mem`, 48GB `effective_cache_size`, 8 parallel-per-gather / 12 max-parallel, 2GB `maintenance_work_mem`, `jit=off`, `random_page_cost=1.1`.

`POSTMASTER_SETTINGS` (`runner.rs:132`) lists `shared_buffers` and `max_worker_processes`: these are `PGC_POSTMASTER`, so `ALTER SYSTEM` + `pg_reload_conf()` won't apply them. The harness hard-fails if observed values drift from requested values unless `--skip-guc-verify` is passed. Reported GUCs are always the observed `SHOW` values, not the requested ones.

## Accel Threading Note

pg_accel does **not** use rayon. The accel arm's parallelism is bounded by `src/engine/thread_budget.rs` (shared-memory LWLock budget) plus whatever the GPU dispatch path does internally. Parallel workers in the baseline arm are normal PG parallel query (separate backend processes, not threads). The benchmark does not configure `pg_accel.workers` — it is auto-derived from the hardware profile, same as `DeviceLimits` (`src/engine/cost.rs`).

## Data Generation

Deterministic via `--seed` (default 42). Seed is forwarded to each workload's `setup_sql`. Without a seed flag, reproducibility is not guaranteed because some workloads use PG's `random()` for bulk data.

## Report Outputs

`--format markdown|json|csv`:
- Markdown — human report with hardware profile, observed GUCs, per-workload rows.
- JSON — full structured output, suitable for replay via `pg_accel_bench report`.
- CSV — per-(workload × scale × mode × iteration) row for external analysis.

Plan capture writes `benchmarks/plans.txt` (referenced in `report.rs:879` and `runner.rs:1183`). SQL snippets under `benchmarks/*.sql` are legacy hand-run scripts — they are not what `just bench` executes; the canonical path is the Rust harness above.

## Honesty Rules

1. Baseline is always PG parallel with extension loaded but `pg_accel.enabled = off` — measures acceleration, not load overhead.
2. No single-threaded arm (`BenchMode::PgSingle` does not exist; do not reintroduce it).
3. Publish observed GUCs, not requested ones.
4. Keep regression workloads in the headline table — hiding ~1.00x cases is cherry-picking.
5. Report median-based speedups by default; mean-based only for backwards-compat.
6. Flag CV > 15% instead of smoothing.
7. Cold-cache numbers (`--cache-mode cold` or `both`) are required for any externally-published report — `DISCARD ALL` does not clear the OS page cache.
8. If a regression workload shows speedup, the cost model is wrong, not the win — investigate before publishing.

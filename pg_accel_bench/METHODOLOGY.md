# Benchmark Methodology

## Design Principles

1. **Compare against PG with extension loaded, not unloaded.** The baseline is
   `SET pg_accel.enabled = off`, not a PostgreSQL instance without the extension.
   This measures the acceleration itself, not extension loading overhead.

2. **Randomize measurement order.** Each iteration randomly chooses whether to
   run accel-first or baseline-first. This eliminates systematic bias from
   shared buffer warming, plan cache state, or OS page cache effects.

3. **Flush between measurements.** `DISCARD PLANS` runs between the accel and
   baseline measurements within each iteration to prevent plan cache carryover.

4. **Include regression workloads.** Two workloads (`oltp_point_lookup` and
   `small_table_scan`) are designed to show ~1.00x speedup. These prove the
   cost model correctly avoids acceleration when it would not help. If these
   show speedup, something is wrong. If they show slowdown, the extension has
   overhead that needs investigation.

5. **Statistical rigor.** Every result includes:
   - Welch's t-test p-value (two-tailed) for significance
   - Cohen's d effect size for practical significance
   - 95% confidence intervals via t-distribution
   - Outlier detection at 3 sigma
   - Min/max/mean/median/stddev

6. **Self-documenting reports.** Every report includes hardware profile
   (auto-detected), PostgreSQL GUC settings (queried from server), and full
   methodology parameters.

## Default Parameters

| Parameter | Default | Rationale |
|-----------|---------|-----------|
| Iterations | 30 | Sufficient for Welch's t-test with reasonable power |
| Warmup | 5 | Allows JIT compilation, shared buffer warming, plan stabilization |
| Rows | 100,000 | Large enough to trigger batching, small enough to run quickly |
| Seed | 0 (random) | Set non-zero for deterministic reproduction |

## Interpreting Results

**Speedup > 1.0** means pg_accel is faster than baseline PostgreSQL.

**p-value < 0.05** means the difference is statistically significant (unlikely
to be due to random variation alone).

**Cohen's d** measures the practical size of the effect:
- |d| < 0.2: negligible (even if p < 0.05, the effect is too small to matter)
- 0.2 <= |d| < 0.5: small effect
- 0.5 <= |d| < 0.8: medium effect
- |d| >= 0.8: large effect

A result with p < 0.05 AND |d| >= 0.8 is both statistically and practically
significant.

## Known Limitations

1. **Same connection for both measurements.** Shared buffers, connection state,
   and backend memory persist across measurements. The plan cache flush
   mitigates this but does not eliminate all state.

2. **EXPLAIN ANALYZE overhead.** Both measurements use `EXPLAIN ANALYZE`, which
   adds instrumentation overhead. This overhead is present in both arms, so it
   does not affect the speedup ratio, but absolute timings are slightly higher
   than real query execution.

3. **Single-backend measurement.** Benchmarks run on a single connection. They
   do not measure behavior under concurrent load or thread budget contention.

4. **Data generation randomness.** Without `--seed`, data is generated with
   PostgreSQL's `random()`, which varies between runs. Use `--seed` for
   reproducible results.

5. **No PG parallel worker comparison.** The current suite compares accel ON vs
   OFF, not vs PostgreSQL's built-in parallel query. A separate comparison
   would require configuring `max_parallel_workers_per_gather` and is planned
   for a future release.

## Reproducing Results

```bash
# Build the benchmark binary
cargo build -p pg_accel_bench --release

# Run all workloads with deterministic seed
./target/release/pg_accel_bench run \
  --connection "host=localhost port=5432 user=postgres dbname=bench" \
  --iterations 30 \
  --warmup 5 \
  --rows 100000 \
  --seed 42 \
  --format json > results.json

# Convert to markdown
cat results.json | ./target/release/pg_accel_bench report --format markdown

# Run a single workload
./target/release/pg_accel_bench run --workload spatial_join --iterations 50

# Available workloads
# Acceleration: simple_agg, aggregate, spatial_join, proximity, large_sort,
#               topk_sort, h3_bulk, join_residual, index_recheck, fts_rank
# Regression:   oltp_point_lookup, small_table_scan
```

# Exact NUMERIC aggregation: local characterization, 2026-09-29

Normal planner admission now selects exact resident NUMERIC SUM/AVG, with
shared COUNT states, zero to three dictionary grouping keys and up to four
value states (three when COUNT(*) occupies the last state). These measurements
use production binaries, not the pg_test force switch. Unsupported shapes
still decline to PostgreSQL before execution.

## Evidence and scope

The final reports are in [numeric-20260929-pg18-final](../numeric-20260929-pg18-final/).
They preserve individual paired samples, exact-result differences, plans,
resident loading and first-use timings, runtime mode counters, and binary/GUC
provenance. Each case has ten balanced randomized measured pairs after five
warmups, using client wall-clock timing and consuming all output rows.

This is an Apple M2 Max, 64 GB, Metal, PostgreSQL 18.4 warm-resident
characterization on a shared development host. It is not a cold-query,
whole-system, full-year taxi, or concurrent-throughput claim. NUMERIC stays
exact; AVG finalization uses PostgreSQL decimal arithmetic, never float64.
CUDA qualification remains owner-deferred.

Final paired-sample medians (loading excluded):

| Workload | Rows | pg_accel | PostgreSQL parallel | Ratio |
| --- | ---: | ---: | ---: | ---: |
| Taxi fare/distance rollup | 1,000,000 | 7.98 ms | 36.95 ms | 4.63x |
| Taxi fare/distance rollup | 12,748,986 | 60.58 ms | 566.03 ms | 9.34x |
| Global SUM/AVG/counts | 250,000 | 5.42 ms | 15.39 ms | 2.84x |
| Global SUM/AVG/counts | 1,000,000 | 8.85 ms | 21.99 ms | 2.49x |
| Three grouping keys, two decimal values | 250,000 | 3.17 ms | 14.76 ms | 4.66x |
| Three grouping keys, two decimal values | 1,000,000 | 4.77 ms | 37.25 ms | 7.81x |
| Wide decimal scales | 250,000 | 2.90 ms | 11.47 ms | 3.95x |
| Wide decimal scales | 1,000,000 | 5.77 ms | 25.56 ms | 4.43x |

All eight cells have exact-result matches, verified `parallel_dense_numeric`
runtime counters consistent with their plans, consumed result rows and zero
stock executor fallback. Full-month resident loading took 7.31 seconds in the
final run. The earlier run measured 34.38/289.30 ms query medians (8.41x) and
4.21 seconds loading. This substantial absolute-time variation is visible
evidence of the shared-host limit, not grounds to select the fastest run or
claim a stable release latency. Both runs show the warm-resident benefit;
neither makes loading free.

The taxi workload retains all 12,748,986 source rows in the pinned January
2015 file. Its SUM(fare_amount), AVG(trip_distance), COUNT(*) query groups by
vendor and passenger count. The million-row prefix is diagnostic, not a
substitute for the complete month. See the [corpus manifest](../../corpora/nyc-yellow-2015-01/manifest.json).

Initial reports in [numeric-20260929-pg18](../numeric-20260929-pg18/) retain
correctness and actual dispatch evidence but omit NUMERIC from the per-mode
counter breakdown. The runner was corrected and the workloads rerun; use the
final reports for complete mode evidence, not whichever run happens to be
faster. The [initial preflight](nyc_taxi_fare_rollup/) failed because the local
build was PG19 while the installed backend was PG18. No provenance check was
bypassed, and no timing was taken from that failed preflight.

## Verification

[Saved logs](../numeric-20260929-pg18/verification/) include:

- 1,135 standalone core tests and 571 benchmark-runner tests passing.
- Seven live NUMERIC tests on each of PG18.4 and PG19beta1 passing.
- All 32 native CTests passing, including NUMERIC and allocation-failure tests.
- Coverage/safety audits and documentation parity passing; coverage threshold
  remains 90%, with 80 SQL files and 395 semantic assertions.
- Full production SQL runs on both majors: 78/80 initially. Tests 03 and 104
  passed their correctness assertions but failed the strict warning check
  after synchronous dispatch exceeded the unchanged five-second warning
  threshold. Both tests then passed without warnings on each major. Original
  logs and retries are retained; this is not an uninterrupted 80/80 claim.
- Production shared-ledger tests on both majors: two live backends each held
  14,301,015 bytes of NUMERIC raw/derived data and retained execution buffers,
  in addition to 4,000,000 bytes for the existing integer probe. Exact results,
  buffer reuse without double charging, forced backend exit, normal exit and
  return to the zero-byte baseline passed. GPU submissions were sequential;
  simultaneous throughput and cross-backend budget-pressure qualification
  remain separate work.

## Reproduce

Prepare the pinned corpus using its documented importer, build/install the
PG18 production extension, and build `pg_accel_bench` in release mode. The local
extension build must match the installed PG18 binary; building PG19 last will
correctly fail the benchmark provenance check.

For each of `nyc_taxi_fare_rollup`, `numeric_global_sum_avg`,
`numeric_grouped_three_keys`, and `numeric_grouped_wide_scale`, run:

```sh
PG_CONFIG=/path/to/pg18/bin/pg_config target/release/pg_accel_bench run \
  --workload WORKLOAD --connection 'host=127.0.0.1 port=28818 dbname=postgres' \
  --cache-mode warm --timing raw --iterations 10 --warmup 5 --capture-plans \
  --artifacts-dir /new/output/directory
```

The runner owns and recreates `bench_nyc_taxi` or `bench_numeric_rollup` in the
target database. Use a development database with no unrelated tables under
those names. Loading is measured separately; the warm-query speedup must not
be represented as a one-shot end-to-end speedup.

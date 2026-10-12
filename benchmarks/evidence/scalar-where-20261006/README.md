# Typed query-level WHERE in dense aggregates

Fact-only resident integer/scalar aggregates now accept Boolean SPG1 WHERE
programs over INT4, BOOL and DATE: multi-column comparisons/arithmetic, lazy
CASE/COALESCE and bounded scalar Boolean/membership expressions. PostgreSQL
implicit-AND qualification lists have a dedicated lowering entry point with
ExecQual's FALSE/NULL short circuit. Direct Boolean masks and the existing
INT4 interval fast path are retained.

This is distinct from aggregate FILTER: rejected WHERE rows cannot create
groups, contribute COUNT(*), or evaluate aggregate arguments. An identical
unfiltered Boolean scalar state carries the predicate, consuming one of four
states unless it already exists. Additive ABI predicate-source tag 2 names
that state; only WHERE accepts it. There is no new pointer field, outer ABI
version, predicate buffer, host row interpreter or fallback executor.
The carrier reuses its already-proved TRUE result rather than evaluating
twice. Every other value-state worker still evaluates WHERE independently;
costing charges each full longest path, without assuming selectivity.
COUNT(*) uses the existing row counter. No cost coefficients, thresholds or
memory limits were relaxed. Filter-program clones are included in retained
program-payload accounting; this is not a complete host-allocation proof.

## Verification

- 1,201 pure Rust tests, 583 benchmark tests, strict PG18/19 and benchmark
  Clippy, formatting and diff checks pass.
- All 33 native suites pass, including 51,980 grouped checks, 3,899 scalar
  host/device cases and all five OOM families. New WHERE fixtures cover
  FALSE/UNKNOWN group suppression, protected argument division, predicate
  errors, one-shot/bounded variants and invalid selector contracts.
- Normal installed PG18.4 and PG19beta1 each pass SQL94, 96, 97, 98, 119,
  121, 122, 123, 125, 126, 128, 129, 130, 131 and 132: 30 file executions,
  118 assertion groups, no dispatch warnings. SQL132 compares native results
  and independent fixture predicates: 24/28 shapes select on each major.
  Three grouped NULL-OR shapes and one single-key AND shape explicitly decline
  as `generic_cost_not_competitive`; their results still match both oracles.
  Prepared predicate-only DML, rollback, all-NULL/empty/reinsert, and errors
  in a later chunk preserve SQLSTATE 22003/22012/22008 with zero stock fallback.
- Documentation tests: 21; coverage helpers: 104. Inventory: 95 SQL files,
  439 assertions, 40 semantic families, 222 Rust files, 16 C++ sources and
  33 native suites, at unchanged 90% thresholds. This is not a fresh
  instrumented coverage or full upstream regression/isolation run.

Retained failures include calling exprType on PostgreSQL's implicit List
(fixed by the WHERE entry point), initial over-strict selection expectations,
and obsolete negative probes in SQL96/97. Those negative probes now include
unsupported abs()/INT8 FILTER cases while keeping the same no-dispatch guards.
The first coverage-helper run overlapped those test edits and rejected stale
source hashes; regeneration and the final frozen-source run pass. No assertion
threshold was reduced. Final source comments/documentation were updated after
the behaviorally identical native/module builds.

## Public-table benchmark: no GPU speedup established

Thirty balanced randomized paired samples after ten warmups, separate persistent
backends with DISCARD ALL, same installed extension, all outputs consumed.
No concurrent project tests, compilers or GPU jobs ran during measurement.
This remains the shared Apple M2 Max / 64 GB / macOS 26.4.1 host, not an isolated
performance machine. The native plans request seven workers at 250K and eight
at 1M/2M; worker counts were not reduced.

All three public-table scales remain native. The recorded final reason is
`no_gpu_resident_pipeline`; diagnostic probes also record zero explicit generic
cost, missing-estimate or unsupported-predicate rejections. This observation
does not prove a GPU latency loss: no GPU candidate was executed. Do not label
these enabled-arm timings as GPU measurements. The normal planner's final
choice still needs to improve before claiming a parallel-PostgreSQL win.

| Rows | Enabled/native median ms | Baseline median ms | Ratio | Enabled CV |
|---:|---:|---:|---:|---:|
| 250,000 | 8.301 | 7.984 | 0.962x | 4.25% |
| 1,000,000 | 16.887 | 16.396 | 0.971x | 2.31% |
| 2,000,000 | 27.236 | 26.752 | 0.982x | 1.73% |

All three independent/native correctness checks and provenance pass, with zero
GPU dispatch, zero hidden stock fallback and 990 consumed enabled output rows
per scale. The ledger retains all three below-parity native rows; crashes and
ship-gate failures are zero. These are new-capability characterizations, not
matched before/after speedup evidence or WINNER promotion.

Residency loads are 47.669/94.286/159.402 ms. Refresh-inclusive lifecycle samples
are 27.938/83.489/156.946 ms versus native 7.766/16.734/28.722 ms: all lose,
even though these queries do not select a GPU plan. Refresh costs must not be
hidden inside a warm-query claim or added twice to the inclusive sample.

Command:

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_scalar_where \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/scalar-where-20261006/grouped_scalar_where
```

Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty worktree;
the source archive and SHA256SUMS identify this milestone independently of HEAD.
Installed module hashes:

- PG18: `25fbe947d3867cc0938a2c8fa8b1adca0eb436c9f033a9fd6a8f46319bf22669`
- PG19: `0da52e160005e0cc2a2469b400fa21413a3987d7add0a45ca91c2151901676cf`

Joins, hash/NUMERIC scalar WHERE, broader types/functions, generalized spatial
conjunctions and predicate sharing across value workers remain open. No fresh
upstream, CUDA, hosted CI, fresh-machine or release certification is claimed.

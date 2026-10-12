# Constant-list membership in OLAP and geo aggregates

The scalar lowerer now accepts planner-simplified constant INT4/Boolean `IN`
and `NOT IN` arrays (`= ANY` and `<> ALL`). This extends membership beyond the
existing raw-column predicate path into computed aggregate inputs, CASE and
Boolean programs, including qualified spatial rollups.

Only exact built-in equality/inequality and optional hash/negator identities
are accepted. The input evaluates once, including retained expressions feeding
empty/NULL arrays. NULL elements, NULL inputs, empty arrays, duplicate constants,
multidimensional arrays and nondefault array lower bounds keep PostgreSQL's
three-valued semantics. PostgreSQL's prior simplification remains authoritative.
Dynamic arrays, mixed types/domains and other ANY/ALL comparisons still decline.

The compiler sorts/deduplicates constants and compacts consecutive INT4 sets
into direct range comparisons. It never subtracts a bound, so INT4 extrema
cannot overflow during this optimization. Arrays are limited to 256 elements
before compaction. A 256-element consecutive set compiles to ten instructions;
smaller sparse sets can exceed the unchanged 256-instruction/32-stack limits
and decline. This uses existing SPG1 operations; no native kernel, wire framing,
descriptor layout, row threshold, cost coefficient or budget rule changed.

## Verification

- Final-source core: 1197 tests pass. New pure compiler tests compare membership
  programs against an independent three-valued set oracle across NULL/INT4
  extrema/Boolean inputs, duplicate/reordered sets, negation and array bounds.
- PG18.4 and PG19beta1 each pass ten focused live tests. They cover native
  analyzed-tree parity, errors and laziness, actual GPU aggregate execution,
  once-only input emission, sparse/dense limits and invalidation. The final
  helper also runs PostgreSQL's hashed-SAOP conversion; dedicated tests verify
  valid hashed IN/NOT IN and reject corrupted operator/hash/negator identities
  and array representation declarations before Datum decoding.
- Both normal builds pass SQL128/129: 18 assertion groups total. PG18 selects
  209 of 216 OLAP matrix queries (155 grouped); PG19 selects 208 (154 grouped).
  Both select all 64 spatial matrix queries. Every new unfiltered membership
  matrix query must dispatch; the 7/8 remaining selective queries explicitly
  decline on `generic_cost_not_competitive`. No gate was relaxed.
- Prepared global/grouped membership measures survive value/dictionary changes,
  rollback, all-NULL inputs and deletion. Spatial cases include geometry changes,
  independent coordinate/widened-arithmetic oracles, and a division error in
  the membership input: masked/FILTER-excluded rows suppress it, included rows
  raise the native SQLSTATE with zero fallback. Both-major SQL runs have no
  dispatch warnings.
- Benchmark tests: 580 pass. Strict extension Clippy passes both majors;
  benchmark Clippy passes. The initial Clippy style failure (missing semicolon)
  is retained alongside corrected successful runs.
- Documentation parity and scope audit pass. The reviewed SQL128 guard change
  adds required membership selection; it retains seven guards/fourteen tokens.
  Scope remains 222 Rust files, 16 C++ sources, 33 native suites, 92 SQL files,
  427 semantic assertions and 38 families, with the unchanged 90% threshold.
  This is not a fresh instrumented coverage run. Native source is unchanged
  from the preceding fusion milestone's 33-suite qualification; the new
  programs are exercised through the live/normal-build SQL tests above.

## Benchmarks

The new `grouped_membership_integer` workload shares SUM/AVG/COUNT/MIN/MAX
over 33 nullable region groups, plus COUNT(*), using:

```sql
CASE WHEN adjustment IN (-4,-3,-2,-1,0,1,2,3)
     THEN price*2 ELSE price-adjustment END
```

`spatial_grouped_membership_integer` adds the existing full-scan constant
point/polygon-with-holes predicate. It is not an indexed PostGIS comparison.
Both retain independent widened-arithmetic/nullable-value oracles; the spatial
oracle additionally uses independent integer-coordinate membership.

Apple M2 Max, 64 GB, macOS 26.4.1, normal PG18.4; 30 raw timed pairs after ten
warmups per scale, balanced randomized AB/BA order on separate persistent
backends, configured parallel PostgreSQL (maximum eight workers per gather).
No builds, tests or other repository GPU workloads overlap timing. The shared
host is noisy; these are local within-run warm ratios, not universal speedups
or release/WINNER promotion. Both bundles pass provenance without errors or
warnings. All five scales select normally, pass exact native and independent
oracles, consume 990 output rows and record zero fallback/selected-no-dispatch.
Kernel deltas are 30 at 250K/1M and 90 at 2M.

| Workload | Rows | GPU median ms | Parallel PG ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| Membership OLAP | 250K | 3.587 | 24.779 | 6.91x | 19.0% |
| Membership OLAP | 1M | 5.887 | 62.973 | 10.70x | 16.2% |
| Membership OLAP | 2M | 9.733 | 147.645 | 15.17x | 26.9% |
| Membership geo | 1M | 5.378 | 87.538 | 16.28x | 12.1% |
| Membership geo | 2M | 7.640 | 176.006 | 23.04x | 19.6% |

### Loading and lifecycle are separate

| Workload / rows | Initial load ms | First warmup ms | Refresh-inclusive ms | Native lifecycle probe ms |
|---|---:|---:|---:|---:|
| OLAP / 250K | 53.292 | 38.950 | 24.233 | 25.251 |
| OLAP / 1M | 100.925 | 42.495 | 86.024 | 98.859 |
| OLAP / 2M | 165.493 | 47.190 | 150.577 | 101.123 |
| Geo / 1M | 268.484 | 127.112 | 315.872 | 84.307 |
| Geo / 2M | 496.013 | 213.552 | 637.076 | 139.520 |

The two smaller OLAP lifecycle observations are faster, but each is only one
warm-cache refresh probe, not evidence of a general cold-query win. The 2M OLAP
and both geo lifecycle probes lose. Refresh alone costs 19.327/65.119/124.248 ms
for OLAP and 228.228/470.680 ms for geo. Initial loading is excluded from warm
medians and reported above. No OS cache purge was requested.

## Upstream compatibility

Fresh PG18 and PG19 pristine/loaded regression and isolation schedules all pass:
PG18 runs 231 regression plus 119 isolation tests in each mode (700 executions);
PG19 runs 245 plus 129 in each mode (748 executions). All 1448 scheduled
executions pass with the exact normal module hashes below. Both artifact SHA256
seals verify; no expected-output edits or new exclusions were introduced.
These preload-only upstream runs exercise
hook compatibility without installing extension SQL objects into the regression
database. They do not prove all upstream queries accelerate; installed-extension
dispatch is proved separately above.

Complete manifests, source/diff provenance, commands, results, expected/actual
diffs and server logs are retained in `upstream-pg18.tar.gz` and
`upstream-pg19.tar.gz`. The original local directories are
`.codex/scratch/scalar-membership-upstream{18,19}-20261006`. The SHA256 values
of their verified `SHA256SUMS` files are:

```text
PG18 5a182d3df566e4ea0f5777e849abd69ba957d4d390631338070085d70585e4f6
PG19 6eaef410f59ab885b12daaa4efeadda209565bca1d20ff3b063279124e747dbf
```

This is local dirty-tree compatibility evidence, not hosted release CI or
qualification of future source/module changes.

## Reproduction and source identity

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_membership_integer \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat for `spatial_grouped_membership_integer` with a fresh directory.
The full objective remains open; this milestone does not complete general
scalar/WHERE expressions, dynamic arrays, joins/windows, shared ownership,
streaming/spill, cold latency, broad corpora or release qualification.

The dirty worktree includes prior milestones. Base HEAD is
`5c7fbd0a6381f823befd823ec47137a8a602e94d`, not a reproducible standalone candidate.
`source-snapshot.tar.gz` contains the implementation/tests/build inputs and
benchmark sources; subsequent evidence/progress prose is outside this snapshot.

```text
Source archive SHA256 aaa257365c30e78da2d98be8933c9f52603db2739bcf96e3c41d9f3f3c09a1f6
Normal PG18 module   2d8563454deffd81ed79ec17105f2925d3496dbd5260b88a364f505a6ddaae5f
Normal PG19 module   12a31dbf056a94e825415579e9f9fc1bd3348a808ddb3c483a62c5155ab14356
```

Normal modules were restored after the final live tests and retain these same
hashes; the final local release artifact matches PG18. Each benchmark bundle
also records actual installed/loaded provenance, GUCs, limits, plans and samples.

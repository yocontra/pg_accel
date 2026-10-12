# Fused scalar instructions for CASE and COALESCE

The scalar compiler now fuses two common instruction pairs, preserving input
evaluation order, PostgreSQL NULL semantics and lazy arithmetic errors:

- Equality plus `JumpIfNotTrue` becomes `EqJumpIfNotTrue` (SPG1 opcode 55).
  It consumes two equal-typed INT4/Boolean values and branches on false/NULL.
- `JumpIfNotNull` over an INT4 literal becomes `CoalesceInt4` (opcode 6).
  It retains a non-NULL INT4 or substitutes the literal, without a branch.

Fusion is planning-only. It never removes a branch entry, relocates every
surviving forward edge and the program exit, and revalidates the whole CFG.
Both wire validators, the non-NULL proof and the device evaluator implement
the same effects. Existing opcodes, descriptor layouts, stack/input/program
bounds, cost coefficients and admission/budget gates are unchanged. Old
consumers fail closed on unknown opcodes. No intermediate resident column or
host expression execution is introduced. The optimization applies to the
existing OLAP and qualified spatial scalar paths, not arbitrary SQL types.

## Why the complex CASE had stayed native

The earlier public-table benchmark used a modulo test, two WHEN arms and two
COALESCE fallbacks. A temporary planner diagnostic on the previous code found
the 250K-row GPU candidate was admitted at cost 5505.96 against the serial
admission comparator 23844.40, but PostgreSQL's live parallel aggregate cost
4567.823 and won final path choice. The simpler CASE cost 3255.96 versus its
live native alternative 3674.973 and selected. The generic last rejection
label was left over from another hook; it was not the reason for final choice.

The diagnostic instrumentation was removed after capturing
`verification/case-cost-diagnostic.log`. It was not a production cost change.
Fusion reduces the compiled program's longest executed instruction path;
the existing work model accounts for that reduction automatically.

## Verification

- Core: 1195 passing tests, including branch-entry preservation, relocation,
  Boolean/INT4 equality, NULL/literal bounds, errors, wire round trips and
  equivalence of fused/unfused programs across the shared corpus.
- Native: all 33 CTests pass. The scalar suite passes 790 host/device cases,
  including 128 shared Rust-oracle cases and 18 rejected full-CFG proofs.
  Grouped aggregation passes 51598 checks.
- Strict extension Clippy passes on PG18 and PG19.
- PG18.4 and PG19beta1 each pass all nine focused live tests, including actual
  analyzed-node lowering, descriptor execution, input invalidation/reload,
  native error states, custom equality rejection and bounded programs.
- Normal production builds on both majors pass SQL128/129: 18 assertion groups
  total, with all 144 OLAP and 48 spatial matrix queries selected per major.
  Prepared DML/rollback/empty/NULL cases, independent FILTERs, spatial masking
  and independent arithmetic/coordinate oracles pass. PG19 SQL128 records one
  first-use dispatch warning at 5.199918 seconds (unchanged five-second limit).
  PG18 and both spatial files have no warnings. No first-use latency fix is
  claimed; the warning is retained in the verification logs.
- Documentation parity and coverage scope audit pass. Scope remains 222 Rust
  files, 16 C++ sources, 33 CTests, 92 SQL files, 427 assertions and 38 families
  with the unchanged 90% threshold. No fresh instrumented coverage or complete
  upstream PostgreSQL regression run is claimed for this follow-up.

## Warm results and remaining native scales

Apple M2 Max, 64 GB, macOS 26.4.1, normal PG18.4. Each workload uses 30 raw timed
pairs after 10 warmups per scale, balanced randomized AB/BA order on separate
persistent backends, with the configured parallel PostgreSQL baseline (maximum
eight workers per gather). No builds, tests or other repository GPU workloads
overlap timing. The shared host is noisy. These are within-run warm ratios,
not universal speedups, a matched old/new kernel experiment, or WINNER promotion.
Every bundle passes provenance with no errors or warnings.

| Workload | Rows | GPU median ms | Parallel PG ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| Complex CASE | 250K | 2.381 | 18.311 | 7.69x | 32.1% |
| Single-WHEN CASE | 250K | 2.450 | 17.798 | 7.26x | 21.4% |
| Single-WHEN CASE | 1M | 3.494 | 55.825 | 15.98x | 31.2% |
| Single-WHEN CASE | 2M | 7.098 | 77.253 | 10.88x | 15.2% |
| COALESCE | 250K | 2.185 | 15.690 | 7.18x | 30.7% |
| COALESCE | 1M | 3.000 | 37.529 | 12.51x | 18.0% |
| COALESCE | 2M | 6.177 | 57.771 | 9.35x | 26.9% |
| Grouped geo scalar | 1M | 5.514 | 104.272 | 18.91x | 15.9% |
| Grouped geo scalar | 2M | 9.125 | 183.054 | 20.06x | 24.0% |

All these rows select normally, pass native equivalence and independent Rust
oracles, consume 990 result rows per scale, and report zero stock fallback.
Kernel deltas are 30 for 250K/1M and 90 for 2M; selected-without-dispatch is zero.
The geo workload is the existing resident full-scan point/polygon-with-holes
rollup, not a comparison against a PostGIS spatial index.

The newly selected complex CASE is:

```sql
CASE adjustment%3 WHEN 0 THEN price WHEN 1 THEN price*2
ELSE COALESCE(price,0)-COALESCE(adjustment,0) END
```

At 250K its GPU plan cost falls from the diagnostic's 5505.96 to 4505.96.
However, its 1M and 2M scales still choose native plans with eight workers:
79.653/122.050 ms with the extension enabled versus 78.506/120.727 ms disabled,
and zero GPU dispatches. Both below-parity native rows remain in the failure
ledger. These are not GPU timings or hidden execution fallback. Larger-scale
complex CASE path choice is still unfinished work.

The single-WHEN medians are lower than the previous characterization's
3.508/6.816/14.102 ms, but the runs were not interleaved old/new binaries and
host variance prevents attributing the entire difference to fusion. COALESCE
is approximately unchanged at 1M. The 2M geo median is 9.125 ms versus the
earlier bounded-kernel result's 8.418 ms; this does not establish a geo latency
improvement or across-the-board non-inferiority.

## Loading and refresh still lose

| Workload / rows | Initial load ms | First warmup ms | Refresh-inclusive ms | Native lifecycle probe ms |
|---|---:|---:|---:|---:|
| Complex CASE / 250K | 53.304 | 34.032 | 24.716 | 22.280 |
| Single-WHEN / 250K | 61.198 | 34.184 | 25.447 | 21.558 |
| Single-WHEN / 1M | 114.212 | 47.238 | 100.476 | 42.219 |
| Single-WHEN / 2M | 180.170 | 47.936 | 163.169 | 81.818 |
| COALESCE / 250K | 55.885 | 30.136 | 22.401 | 16.291 |
| COALESCE / 1M | 105.455 | 36.455 | 76.272 | 36.726 |
| COALESCE / 2M | 172.873 | 47.769 | 152.680 | 56.018 |
| Geo / 1M | 294.344 | 165.632 | 381.018 | 127.745 |
| Geo / 2M | 585.763 | 385.286 | 844.321 | 212.889 |

Every listed refresh-inclusive probe loses to native. Refresh alone is recorded
separately in each report; geo refresh takes 252.518/522.934 ms. No OS cache
purge was requested, and these first warmups are not machine-cold certification.
Warm execution gains must not be presented as one-shot acceleration.

## Reproduction

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_simple_case \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat with `grouped_simple_case_linear`, `grouped_coalesced_integer` and
`spatial_grouped_scalar_program`, using a fresh artifact directory each time.

## Source identity

The worktree includes the preceding uncommitted milestones. Base HEAD is
`5c7fbd0a6381f823befd823ec47137a8a602e94d`; it alone cannot reproduce this work.
`source-snapshot.tar.gz` contains the implementation, tests and relevant build
inputs, including the unchanged benchmark workload/harness sources. Its SHA256:

```text
9dbdb50988bab188d829a96d2cececba538094d9bea5bbf03adb405c6901c2f4
```

Subsequent evidence/progress documentation is outside that snapshot. Benchmark
bundles additionally record the actual loaded module identity and environment.

Verified normal module SHA256 values (PG18 also matches the final local release
artifact):

```text
PG18 f50a3e7135ad671b465189b23232d039ecb7ba13a0ce2468c3a9ee4b3122ce85
PG19 a5e59d6e8148e2ad71fb3e5e7cbc3769df738f99c60b9cb832acba4b712a811c
```

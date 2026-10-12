# Atomic integer counts and rejected row fusion — local characterization

The retained candidate eliminates the high-word atomic update from group and
nonnull row counts in the one-shot integer path. Preparation resets both words;
each row contributes at most one unit per counter; the existing one-million-row
cap prevents carry. A compile-time assertion guards this bound. SUM still uses
two-word atomic addition, and scratch/output representations are unchanged.

An earlier experiment shared dictionary-key, membership and WHERE evaluation
across all measures of a row. It did not establish a latency benefit, so row
fusion was removed. The original per-row/per-measure work-item mapping remains.
Each measure independently evaluates FILTER, NULLs and checked arithmetic;
errors prevent output publication and never cause stock fallback.

The existing atomic eligibility limits are unchanged: at most one million rows,
more than eight dense groups, and the existing descriptor/device eligibility
checks. Hierarchical and bounded-chunk reducers remain per-measure. No production
costs, thresholds, ABI layouts, wire versions or workspace sizes were changed.

## Method and provenance

Warm PostgreSQL 18 runs use five warmups and ten raw paired iterations at 250,000
and 1,000,000 rows, normal parallel PostgreSQL settings, captured plans and exact
native/Rust-oracle comparisons. Builds, tests and other GPU workloads do not
overlap benchmark timing. Every attempt is retained, including failed benchmark
gates. Results are local development measurements, not release qualification.

- Baseline PG18 module: `55b959c365fe0e81a412635aa768d46ff296cdf9b8d24c71d4f3978367183eea`.
- Rejected fused PG18 module: `8785d7ba7e5dd17bbc1cf8c765b1ca117dedc1660fd2f249360c23f2796d93f5`.
- Rejected fused PG19 module: `4407a8ac978315e1d00909fd23758b81ddd4fe55c61210a3f4f6df7f8eda50a1`.
- Unfused PG18 recheck (measure-helper extraction retained):
  `613d6e525dee0cf6dd19c5bdfd1e6aaea1facd8632dfd3c5fe53c75014e17a69`.
- Final count-optimized PG18 module:
  `ee374bbbed4c131d1ac4257732eb1de4b4344db81faceb68b1b9bb362a2b1ce1`.
- Final count-optimized PG19 module:
  `5752d033ad2730d22ba1f2796e1f834a218e8b9937cdf4353bc29315dd53aeb3`.

The grouped computed-expression workload remains a characterization, not a GPU
winner. Its initial baseline failed a stale expected-winner annotation even
though the preceding milestone documented native selection. The annotation was
removed from the threshold envelope as well as the released-path contract;
registration, exact oracles and the independently verified global-expression
winner contract remain. A regression test prevents accidentally restoring this
unsupported winner claim. Original failure ledgers are retained. Native execution
receives no GPU speedup credit, and native parity checks are not disabled.

## Correctness scope

`127_fused_integer_rows.sql` compares all six orders of three arithmetic states
under unrestricted, stored-Boolean, NULL-or-range and spatial predicates. It also
checks a late COUNT-expression overflow after an all-NULL state and a fully
rejected FILTER, no stock fallback, prepared-plan recovery, and transition from
1,000,000 to 1,000,001 rows with verified GPU dispatch after recovery.

Native tests separately check state-order permutations, independent NULL/filter
semantics and unpublished output after late overflow. Broader existing tests
exercise atomic, hierarchical and bounded execution. Coverage now declares 90
SQL files and 418 assertions; this inventory is not a claim that the entire SQL
suite was rerun on this build.

The final native test also concentrates 1,000,000 and 1,000,001 rows into a
single group, with independent nullable/nonnullable COUNT and INT32_MAX SUM
inputs. This checks maximum count contention, exact high-word SUM carry, empty
groups and the atomic/bounded row boundary.

## Rejected fusion measurements

All numbers below are individual-run warm medians in milliseconds. The two
initial baseline runs and two fused runs have high accelerator CVs (roughly
17–26%); these are not statistical improvement claims.

| Workload / rows | Initial unfused | Repeat unfused | Fused | Repeat fused | Later unfused recheck |
| --- | ---: | ---: | ---: | ---: | ---: |
| Three measures / 250K | 9.536 | 8.972 | 9.386 | 8.552 | 10.367 |
| Three measures / 1M | 10.618 | 9.642 | 11.520 | 10.372 | 10.740 |
| WHERE ranges / 250K | — | — | 9.720 | — | 9.409 |
| WHERE ranges / 1M | — | — | 11.286 | — | 10.538 |

The first fused run's PostgreSQL medians were also much slower (20.508/50.879ms
versus initial 14.662/35.745ms), so its larger speedup ratio is not a fusion win.
The single-measure control was essentially unchanged at 1M (4.731 to 4.744ms).
Grouped computed expressions remained native. All attempted runs, lifecycle
probes, dispatch counters and correctness differences are retained in their
individual directories.

The rejected implementation is archived as
`verification/rejected-fused-grouped_agg.cpp.txt`. The fused native-selected
control still records parity warnings (0.964x/0.974x); correcting its winner
annotation did not erase those warnings or grant GPU credit.

## Retained bounded-count optimization

Warm medians in milliseconds; A/B are independent candidate runs. The reference
is the later unfused recheck for multi-measure/WHERE workloads and the original
unfused baseline for the single-measure control.

| Workload / rows | Unfused reference | Count A | Count B | Parallel PG A / B | Accelerator CV A / B |
| --- | ---: | ---: | ---: | ---: | ---: |
| Three measures / 250K | 10.367 | 9.349 | 6.102 | 25.151 / 25.392 | 25.84% / 10.07% |
| Three measures / 1M | 10.740 | 6.822 | 7.081 | 54.085 / 45.823 | 8.68% / 27.05% |
| WHERE ranges / 250K | 9.409 | 5.712 | 6.013 | 16.454 / 17.708 | 9.52% / 5.54% |
| WHERE ranges / 1M | 10.538 | 6.532 | 7.556 | 43.996 / 39.742 | 7.64% / 10.90% |
| Single measure / 250K | 4.226 | 3.540 | — | 13.170 / — | 17.81% / — |
| Single measure / 1M | 4.731 | 2.766 | — | 24.592 / — | 11.83% / — |

At 1M, the multi-measure runs are 34–36% below the later unfused reference, and
the WHERE runs are 28–38% below it. These are observed local median reductions,
not confidence-qualified general gains. The initial unfused multi-measure
medians were 9.642–10.618ms, also above both count-optimized 1M medians. The 250K
multi-measure case is inconsistent. PostgreSQL timing drift and CVs as high as
27% prohibit a publication/release-wide performance claim.

All ten final measured cells select and dispatch `parallel_dense_integer` ten
times, process exactly ten times the fixture row count, and record zero stock
fallback. All ten correctness differences pass with zero rows in either
direction. The five final failure ledgers have no rows and zero ship-gate
failures. Expected, installed and loaded module hashes match the final PG18
hash above. No admission costs or PostgreSQL parallel settings were changed.

### First-use costs remain

The resident preload and separate lifecycle refresh/preparation probe are not
included in warm medians. These probes are not disk-cold benchmark evidence.
Times are milliseconds; A/B match the warm runs above.

| Workload / rows | Preload A / B | First-use accel A / B | First-use PG A / B |
| --- | ---: | ---: | ---: |
| Three measures / 250K | 274.430 / 83.009 | 73.098 / 51.162 | 41.374 / 23.533 |
| Three measures / 1M | 187.760 / 172.821 | 182.912 / 160.183 | 52.018 / 42.057 |
| WHERE ranges / 250K | 67.348 / 70.368 | 45.041 / 43.713 | 14.977 / 14.943 |
| WHERE ranges / 1M | 162.703 / 160.253 | 146.284 / 150.109 | 37.462 / 40.989 |
| Single measure / 250K | 52.464 / — | 23.254 / — | 10.588 / — |
| Single measure / 1M | 103.952 / — | 82.944 / — | 31.078 / — |

Every final first-use probe loses to PostgreSQL. This kernel change does not
solve loading, dictionary preparation or refresh overhead.

## Verification

- Final native suite: 32/32, 91.76 seconds. Focused grouped test: 48,430 checks,
  zero failures, including maximum-skew count/SUM and boundary cases.
- Final production builds: eight targeted SQL files on each of PG18 and PG19
  pass: 90, 95, 97, 110, 111, 115, 123 and 127. The earlier rejected-fusion build
  passed these plus 125 and 126 on both majors; those earlier runs are not
  attributed to the final module. No fresh full 90-file run is claimed.
- Unchanged Rust implementation: 1,153 standalone core tests; both PG18/PG19
  Clippy builds. Benchmark metadata/oracles: 575 tests and benchmark Clippy pass.
- Coverage helpers: 154 tests; scope audit: 90 files, 418 SQL assertions, 37
  semantic families; documentation parity: 21 tests. Formatting and diff checks
  pass. The initial intermediate benchmark-metadata test failure is retained
  alongside the corrected passing run.

Raw logs are in `verification/`; each benchmark directory includes its report,
correctness differences, plans, GUC snapshot, loaded-module provenance, lifecycle
costs and failure ledger. This is not fresh upstream PostgreSQL/PostGIS/H3
regression qualification, CUDA validation, concurrency qualification or a full
release-matrix result. Broader fusion, cost modeling and first-use work remain.

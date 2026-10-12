# Once-evaluated simple CASE for OLAP and geo

The dense scalar lane now lowers `CASE value WHEN ...` with INT4/Boolean
test values, nested cases and built-in equality. It evaluates the test once,
retains its value across unsuccessful comparisons and evaluates only the
chosen result. NULL never matches NULL. Casts, domains, arbitrary functions,
custom equality and unsupported local references still decline.

SPG1 gains `DROP` (opcode 5, required zero operand), with matching Rust/native
control-flow validation, non-NULL analysis and device evaluation. Underflow,
noncanonical operands and invalid final stack shapes fail closed. Existing
wire framing, bounds and descriptor layouts are unchanged; old consumers
reject the unknown opcode. This adds no resident intermediate column.

The final WHEN consumes the saved value directly. A single built-in WHEN
therefore has bytecode identical to its searched-CASE equivalent, verified
against analyzed PG18/19 trees. This removes a duplicate/discard pair from
each executed path without changing any cost coefficient, threshold or
budget. Earlier successful arms still discard the retained value correctly.

## Verification

- Rust core: 1,192 tests pass. Strict extension Clippy passes on PG18/19.
- Native: all 33 CTests pass; 768 scalar host/device cases include 114 shared
  Rust-oracle cases, plus 10 rejected full-program proofs. Grouped aggregation
  passes 51,598 checks. The later final-WHEN optimization changes only Rust
  lowering; native source/binaries are unchanged from this full suite.
- PG18.4 and PG19beta1 each pass nine focused live tests. Coverage includes
  integer/Boolean/nested cases, once-only test emission, bytecode equivalence,
  WHEN NULL, skipped errors, exact SQLSTATEs, custom equality rejection,
  representation boundaries and actual descriptor aggregate execution.
- Final normal-build SQL128/129 reruns pass 18 assertion groups across both
  majors without warnings. Each selects all 144 temporary-table OLAP matrix
  queries (108 grouped) and all 48 temporary-table spatial matrix queries: 72 additional selected shapes
  per major. Tests cover independent FILTERs, prepared DML/rollback/NULL/empty
  inputs, spatial boundaries/holes and independent coordinate/widened
  arithmetic oracles. A CASE test-value division error is suppressed outside
  the spatial mask/FILTER and raised as 22012 when included; zero budget declines.
- Initial SQL126/128/129 qualification passed 26 assertion groups after two
  fixture corrections described below. SQL126 recorded a 19.101-second
  PG19 dispatch warning and an 8.465-second PG18 warning, above the unchanged
  five-second threshold. Retained logs are not erased by later warning-free
  runs. No first-use latency or asynchronous-cancellation improvement is claimed.
- Benchmark tests: 579 pass, with strict Clippy. Documentation parity: 21
  tests. Coverage tooling: 104 tests; scope remains 222 Rust files, 16 C++
  sources, 33 CTests, 92 SQL files, 427 assertions and 38 families at 90%.
  This is not a fresh instrumented coverage or upstream-PG regression run.

## Measured selected workload

`grouped_simple_case_linear` computes shared SUM/AVG/COUNT/MIN/MAX and COUNT(*)
over 33 nullable region groups using:

```sql
CASE adjustment WHEN 0 THEN price*2 ELSE price-adjustment END
```

The independent Rust oracle uses widened arithmetic and explicit nullable
inputs. All scales pass it and native PostgreSQL equivalence. The final
`grouped_simple_case_linear-fold-qualified/` bundle records normal selection,
`parallel_dense_integer`, 990 consumed output rows per scale, kernel deltas
30/30/90 and zero stock fallback or selected-without-dispatch rows.

Apple M2 Max / 64 GB / macOS 26.4.1, normal PG18.4; 30 raw timed pairs after
10 warmups per scale, balanced randomized AB/BA on separate persistent
backends, configured parallel PostgreSQL (max workers per gather 8). No
repository builds, tests or other GPU workloads overlap timing. The host is
shared and noisy; all samples are retained and no WINNER metadata is promoted.

| Rows | GPU median ms | Parallel PG median ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|
| 250K | 3.508 | 18.404 | 5.25x | 27.4% |
| 1M | 6.816 | 47.657 | 6.99x | 15.8% |
| 2M | 14.102 | 78.380 | 5.56x | 18.2% |

These are within-run warm ratios, not a universal speedup or a matched
before/after GPU-kernel comparison. Before the final-WHEN reduction this same
workload stayed native at all scales (zero kernel delta); its results remain
in `grouped_simple_case_linear/`.

| Rows | Resident load ms | First warmup ms | Refresh-inclusive GPU ms | Native probe ms |
|---|---:|---:|---:|---:|
| 250K | 52.194 | 34.865 | 25.347 | 18.724 |
| 1M | 106.012 | 47.020 | 78.410 | 43.464 |
| 2M | 179.637 | 59.213 | 165.389 | 94.433 |

Refresh alone takes 18.660/64.288/136.500 ms. These lifecycle probes lose to
native execution. No OS cache purge was requested; first warmups and lifecycle
probes are not machine-cold certification.

## Native probes, control and retained failures

The more complex `grouped_simple_case` uses a modulo test, two WHEN arms and
COALESCE. Both before and after the optimization it remains native under the
configured public-table parallel planner. Final medians are 21.132/57.685/
98.082 ms versus native 20.026/55.872/94.090 ms, with zero GPU dispatches.
Its final failure ledger retains all three native-below-parity rows. Do not
interpret these as accelerated timings. A diagnostic EXPLAIN returned the
generic last label `no_gpu_resident_pipeline`; it does not isolate the full
path-choice cause. Broader admission/cost diagnosis remains open.

The existing COALESCE control still selects at every scale: 2.355/2.999/
7.513 ms versus 23.486/49.321/68.640 ms, with zero fallback. Its GPU CV is
19.5–46.7%; it does not establish non-inferiority to an earlier run. Its
refresh-inclusive probes also lose. The control precedes the final-WHEN
optimization, which does not change COALESCE bytecode or native code.

Retained qualification history:

- Initially, three selective nested CASE matrix rows lost on the explicit
  `generic_cost_not_competitive` gate. The new test initially demanded all
  filtered forms win; it now requires unfiltered forms to dispatch and permits
  only that explicit cost reason for selective forms. No production gate was
  relaxed. After the compiler optimization all 144 actually select on both majors.
- The new Boolean geo fixture exposed the oracle generator replacing the `a`
  inside `false`. Column widening now uses whole-word tokens. The initial
  `numericlse` fixture error and corrected successful runs are retained.
- One Clippy run caught a missing stylistic semicolon; final runs pass.
- `grouped_simple_case-fold/` aborted at the provenance gate before measurement:
  the shared local build artifact was PG19 while the target backend was PG18.
  A normal PG18 reinstall restored the matching local artifact. Both
  `*-fold-qualified/` bundles pass provenance with no errors or warnings.
  The aborted run is not benchmark evidence. All five completed bundles pass
  provenance; the original native probes and control remain alongside them.

## Reproduction and source identity

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_simple_case_linear \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Use a fresh output directory. The local release artifact must match the target
PostgreSQL major; the runner verifies the live loaded module, not just a filename.

Base HEAD: `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty shared worktree.
Source archives include the pre-existing implementation changes and are sealed
in `SOURCE_SHA256.txt`: initial code, initial code plus the linear probe, and
final qualified code. Raw verification logs are in `verification/`.

Final normal module hashes:

- PG18: `bd0e4f069fe450f06c4b6e55171e177160d84bf3e0cf2d6d127d17224bdd59a4`
- PG19: `136e607d3cbc5d8513c5dc60cadf9684d976321efa569354b5300339c3e2cd49`

Initial pre-optimization modules were PG18
`12e988ca9f8210cb3872e7e102ae8d89a722f053307175068a231cddc99682bd`
and PG19 `f3342678d81a50c4715580e085483c52c7d74636e7a25084465c4bcbf42830e8`.

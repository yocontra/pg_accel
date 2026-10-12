# Arithmetic fusion makes larger complex CASE queries selectable

The existing complex CASE workload now selects and dispatches normally at all
three registered scales, including the previously native 1M/2M cases. No cost
coefficient, threshold, budget, workload, or PostgreSQL parallelism setting changed.

The compiler fuses checked INT4 arithmetic with a literal RHS (SPG1 opcodes
60–64), or with an immediately preceding literal-fallback RHS COALESCE (65–69).
Both native forms share ordinary arithmetic's checked implementation. They
retain strict NULL propagation, overflow, division by zero, PostgreSQL's
`INT_MIN % -1 = 0`, and evaluation errors in either input. The nullable LHS
still determines successful-result nullability for both forms.

Fusion repeats to a fixed point because removing a COALESCE branch can expose
a safe arithmetic pair. Each productive pass shortens the bounded program;
each pass protects branch entries, relocates every surviving forward edge and
exit, and revalidates the whole CFG. Rust/native validators, the independent
Rust oracle and non-NULL analysis agree on stack effects. Existing opcode
meanings, SPG1 framing, descriptor layouts and program bounds are unchanged;
older consumers reject the additive opcodes. There is no host query execution
or intermediate result column.

For this unchanged expression:

```sql
CASE adjustment%3 WHEN 0 THEN price WHEN 1 THEN price*2
ELSE COALESCE(price,0)-COALESCE(adjustment,0) END
```

the longest instruction path loses two dispatches. Its selected GPU plan costs
are 4005.96 / 16005.96 / 32015.96 at 250K/1M/2M. Previously 250K cost 4505.96
and selected, while 1M/2M lost to PostgreSQL's parallel paths. The actual
executed program is shorter; the existing instruction-work model accounts
for that reduction without new calibration constants.

## Verification

- Core: 1198 tests pass, including fused/unfused equality across arithmetic
  boundaries and NULLs, fixed-point idempotence, branch-entry protection,
  non-NULL proofs, wire round trips and invalid type/stack rejection.
- Native: all 33 CTests pass; 3875 scalar host/device cases include 148 shared
  Rust-oracle fixtures. Full CFG checks reject 58 malformed programs; generated
  fused boundary programs are also validated before evaluation. The grouped
  aggregate suite passes 51598 checks.
- Strict extension Clippy passes for PostgreSQL 18 and 19.
- All ten focused live tests pass on each major, including native analyzed
  trees/error states, descriptor lanes, invalidation/reload and catalog guards.
  The first PG18 run found a typo in the new test SQL (`a%-1` parsed as a `%-`
  operator); spacing was fixed and both final runs pass. The initial log remains.
- Normal production modules on PG18.4/PG19beta1 pass SQL128/129, all 18 assertion
  groups combined. Both select 208/216 OLAP and 64/64 geo matrix shapes. The
  remaining selective shapes explicitly decline on cost; required unfiltered
  cases dispatch. Prepared mutations, rollback, NULL/empty inputs and errors
  suppressed outside spatial/FILTER masks pass. No dispatch warnings occur.
- Documentation tests (21), formatting and scope audit pass. Scope remains
  222 Rust files, 16 C++ sources, 33 CTests, 92 SQL files, 427 assertions and
  38 families, at the unchanged 90% threshold. This turn does not claim a fresh
  instrumented coverage or full upstream PostgreSQL regression run. The prior
  membership milestone's 1448 upstream executions belong to its earlier module.

## Warm measurements

Apple M2 Max, 64 GB, macOS 26.4.1, normal PostgreSQL 18.4. Each scale has 30 raw
timed pairs after ten warmups, balanced randomized AB/BA order on separate
persistent backends. Native plans use seven workers at 250K and eight at 1M/2M.
No repository builds, tests or other GPU workloads overlap timing. This shared
host remains noisy; these are within-run warm ratios, not matched old/new
binary comparisons, universal non-inferiority, or a WINNER/release promotion.

| Workload | Rows | GPU median ms | Parallel PG ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| Complex CASE | 250K | 2.551 | 13.552 | 5.31x | 16.3% |
| Complex CASE | 1M | 4.700 | 38.644 | 8.22x | 19.5% |
| Complex CASE | 2M | 8.460 | 66.751 | 7.89x | 21.1% |
| Single-WHEN CASE | 250K | 2.538 | 13.068 | 5.15x | 59.6% |
| Single-WHEN CASE | 1M | 3.489 | 34.032 | 9.75x | 17.0% |
| Single-WHEN CASE | 2M | 6.915 | 55.032 | 7.96x | 21.3% |
| COALESCE | 250K | 2.205 | 10.815 | 4.90x | 17.4% |
| COALESCE | 1M | 3.128 | 25.687 | 8.21x | 19.2% |
| COALESCE | 2M | 5.279 | 42.390 | 8.03x | 23.4% |
| Membership | 250K | 2.692 | 17.931 | 6.66x | 15.8% |
| Membership | 1M | 5.266 | 60.402 | 11.47x | 20.2% |
| Membership | 2M | 8.609 | 93.323 | 10.84x | 19.1% |
| Geo scalar | 1M | 4.954 | 67.888 | 13.70x | 9.8% |
| Geo scalar | 2M | 7.536 | 121.462 | 16.12x | 13.8% |
| Geo membership | 1M | 4.996 | 74.293 | 14.87x | 9.7% |
| Geo membership | 2M | 6.681 | 131.058 | 19.62x | 16.4% |

Every row selects, passes native equivalence and its independent Rust oracle,
consumes 990 result rows, and has zero stock fallback and zero selected-without-
dispatch. Kernel deltas are 30 at 250K/1M and 90 at 2M. All six provenance checks
pass with no errors/warnings, and all failure ledgers have zero failures or
below-parity rows. Geo is the existing full-scan point/polygon-with-holes rollup,
not a comparison against a PostGIS spatial index.

The 250K complex CASE median is slightly above the prior 2.381 ms, and the
single-WHEN 250K CV is 59.6%. Do not infer a universal kernel speedup from these
separate runs. The substantive gain is normal selection plus measured
acceleration of the two formerly native complex CASE scales.

## Loading and refresh remain expensive

| Workload / rows | Initial load ms | First warmup ms | Refresh-inclusive ms | Native lifecycle ms |
|---|---:|---:|---:|---:|
| Complex CASE / 250K | 51.077 | 30.302 | 23.554 | 13.624 |
| Complex CASE / 1M | 100.892 | 38.089 | 75.805 | 34.917 |
| Complex CASE / 2M | 164.450 | 49.678 | 152.602 | 62.673 |
| Single-WHEN / 250K | 50.408 | 29.745 | 21.552 | 13.103 |
| Single-WHEN / 1M | 100.946 | 38.953 | 75.942 | 33.338 |
| Single-WHEN / 2M | 164.150 | 45.796 | 146.038 | 53.809 |
| COALESCE / 250K | 47.783 | 28.426 | 21.738 | 11.748 |
| COALESCE / 1M | 101.441 | 35.983 | 72.658 | 25.025 |
| COALESCE / 2M | 164.984 | 46.849 | 146.425 | 44.126 |
| Membership / 250K | 51.001 | 31.940 | 23.173 | 19.379 |
| Membership / 1M | 97.857 | 35.724 | 79.651 | 73.236 |
| Membership / 2M | 163.617 | 47.059 | 151.539 | 93.343 |
| Geo scalar / 1M | 268.674 | 129.443 | 312.621 | 67.689 |
| Geo scalar / 2M | 492.321 | 193.723 | 614.942 | 122.803 |
| Geo membership / 1M | 269.145 | 125.873 | 320.684 | 81.156 |
| Geo membership / 2M | 502.704 | 197.988 | 627.404 | 136.303 |

Every refresh-inclusive probe loses. Refresh alone is separately recorded in
the reports, including 228.537/461.227 ms for geo scalar and 230.685/466.240 ms
for geo membership. No OS-cache purge was requested; first warmups are not
machine-cold certification. This is not a one-shot-query acceleration claim.

## Reproduction and source identity

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_simple_case \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat with `grouped_simple_case_linear`, `grouped_coalesced_integer`,
`spatial_grouped_scalar_program`, `grouped_membership_integer`, and
`spatial_grouped_membership_integer`, each in a fresh directory, sequentially.
Workload definitions and benchmark executable sources are unchanged.

Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`; the worktree also contains
preceding uncommitted milestones. `source-snapshot.tar.gz` includes complete
implementation/tests/build inputs and the benchmark source. Subsequent
evidence/progress prose is outside that snapshot. SHA256:

```text
27c2032c32f41f1a23ea11e729c26f2944c8afb19e57ca6dffb94f2d3e766a7c
```

Verified normal modules (PG18 also equals the local release artifact and every
benchmark backend's loaded module):

```text
PG18 c3cc08bddb2861935a37201ffe2581e5977d1bfc50a174db9679ff5cd1ace2d1
PG19 9fdc6b02036ee32c6e02aaa042e1f2697f91e60a4444fc819ca3855debc8fbde
```

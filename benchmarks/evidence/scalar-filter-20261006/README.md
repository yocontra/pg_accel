# Multi-column aggregate FILTER on the typed scalar path

Qualified INT4/Boolean aggregate FILTER expressions now compose with the dense
scalar producer, including multi-column comparisons/arithmetic, Boolean/NULL
expressions, CASE and constant IN/NOT IN. This also supports filtered COUNT(*)
and the existing point/polygon spatial aggregate path. General WHERE, other
scalar types/functions, joined scalar execution, DISTINCT and excess aggregate
states remain outside this expansion.

The existing single-column INT4 interval FILTER representation is tried first
and is unchanged. Otherwise one bounded lowerer compiles the predicate before
the aggregate argument. False/UNKNOWN yields typed NULL without evaluating the
argument; filtered COUNT(*) supplies literal 1 for accepted rows. Rejected rows
still establish their groups. One lowerer deduplicates predicate/argument
columns and rejects cross-occurrence references across both expressions.

Identical complete programs share aggregate states. Predicate-only columns
participate in logical identity, loaded input proof, residency dependencies,
metadata budgeting and longest-path costs through existing scalar bindings.
No descriptor, SPG1 opcode, device evaluator, kernel, cost coefficient,
threshold or memory limit changes were needed. No intermediate mask or host
row filtering was introduced.

## Verification

- Core: 1198 tests pass. Both PostgreSQL-major strict Clippy checks pass.
- All ten focused live tests pass on PG18.4/PG19beta1, including direct
  comparison with independently lowered CASE expressions, filtered Boolean
  COUNT/COUNT(*), interval fast-path preservation and unsupported
  type/function/cross-occurrence rejection.
- Eight production SQL files pass on each major: SQL98, 115, 121, 125, 126,
  128, 129 and new SQL130; 52 assertion groups across 16 file executions.
  There are no dispatch warnings in the final runs.
- SQL130 selects all 32 global/one-to-three-key FILTER matrix shapes and
  three additional compositions per major. Native and widened CASE oracles
  cover NULL/false/true predicates, all-rejected groups, argument NULLs versus
  filtered COUNT(*), and independent states. Two retained composition probes
  explicitly decline on cost rather than unsupported semantics.
- Generic prepared plans cover predicate-only column changes, dictionary
  changes, rollback, all-NULL rows, deletion to empty and reinsertion. The
  selected lifecycle query shares SUM/AVG/COUNT/MIN/MAX, alongside filtered
  COUNT(*) and total COUNT(*). More than one million rows verify FILTER-before-
  argument error suppression, selected native SQLSTATE equivalence for
  predicate/argument division by zero and overflow, and zero stock fallback.
- SQL129 additionally dispatches independent multi-column filters and Boolean
  filtered COUNT above the spatial mask, and suppresses a rejected error row
  with a multi-column FILTER. All 64 existing spatial matrix shapes still
  dispatch; SQL128 selects 208/216 existing OLAP shapes per major.
- SQL98's old adjacent unsupported probes now explicitly use INT8 predicates;
  their modifier declines remain required. Their newly supported INT4 forms
  are represented in the new multi-column tests rather than kept falsely
  labeled unsupported.
- Benchmark tests: 581 pass, strict Clippy passes, and the release executable
  was rebuilt. Documentation tests (21), coverage-helper tests (104), formatting
  and scope audit pass. The fixed SQL manifest grows to 93 files / 431 assertions;
  scope remains 222 Rust files, 16 C++ sources, 33 CTests, 38 SQL families and
  an unchanged 90% threshold. This is not a fresh instrumented coverage run.
- Native evaluator/grouped-kernel sources are byte-identical to the preceding
  arithmetic-fusion snapshot (whose 33 CTests passed). No fresh full native
  CTest or upstream PostgreSQL regression run is claimed for this follow-up.

Initial SQL130 attempts required every composition to dispatch. Diagnostics
proved `generic_cost_not_competitive` for the three-independent-program query,
the selective WHERE composition, and a smaller three-output lifecycle query.
The first two remain explicit native cost/parity probes; selected lifecycle
coverage uses the complete shared aggregate output family. No admission
coefficient was changed. Initial logs and diagnostic SQL/plans are retained.
An initial coverage-helper run encountered the source/manifest transition;
the final synchronized manifest passes all 104 tests.

## Warm benchmark results, including native scales

Apple M2 Max, 64 GB, macOS 26.4.1, normal PG18.4. Each scale uses 30 raw timed
pairs after ten warmups, balanced randomized AB/BA order on separate persistent
backends. Native plans use seven workers at 250K and eight at 1M/2M. No builds,
tests or other repository GPU workloads overlap timing. The shared host is
noisy; this is local characterization, not matched old/new binaries or
WINNER/release promotion.

Both new workloads use:

```sql
sum(price) FILTER (WHERE price>adjustment OR adjustment IS NULL)
```

with shared AVG/COUNT/MIN/MAX, a separate unfiltered COUNT(*) and nullable region
grouping. The spatial variant adds the existing full-scan point/polygon-with-
holes restriction, not a comparison with a PostGIS spatial index. Independent
Rust oracles implement nullable comparison/acceptance, retained groups and exact
arithmetic; spatial membership has a separate coordinate oracle.

| Workload | Rows | Selected path | Enabled median ms | Parallel PG ms | Ratio | Enabled CV |
|---|---:|---|---:|---:|---:|---:|
| Aggregate FILTER | 250K | Native | 13.994 | 13.504 | 0.965x | 7.4% |
| Aggregate FILTER | 1M | Native | 30.234 | 29.261 | 0.968x | 10.0% |
| Aggregate FILTER | 2M | Native | 49.286 | 48.399 | 0.982x | 4.1% |
| Spatial FILTER | 1M | GPU | 4.910 | 66.596 | 13.56x | 14.2% |
| Spatial FILTER | 2M | GPU | 6.453 | 117.867 | 18.27x | 17.6% |

All five scales pass native equivalence and independent oracles and consume
990 result rows. Both provenance checks pass with no errors/warnings. Stock
fallback and selected-without-dispatch are zero. Spatial kernel deltas are
30/90; the three native scales have zero kernel executions. Their below-parity
rows remain in the failure ledger and are not GPU timings. The nonspatial
public-table workload still loses final selection against parallel PostgreSQL;
broader predicate-work sharing/cost qualification remains unfinished. This
does not contradict selected SQL130 evidence on its distinct temporary-table
queries, and that evidence must not be substituted for these native results.

## Loading and refresh

| Workload / rows | Load ms | First enabled warmup ms | Refresh-inclusive ms | Native lifecycle ms |
|---|---:|---:|---:|---:|
| Aggregate FILTER / 250K | 49.930 | 11.986 | 38.742 | 11.182 |
| Aggregate FILTER / 1M | 102.752 | 27.610 | 96.106 | 30.024 |
| Aggregate FILTER / 2M | 164.360 | 52.732 | 176.763 | 49.376 |
| Spatial FILTER / 1M | 267.582 | 125.931 | 306.609 | 66.180 |
| Spatial FILTER / 2M | 501.870 | 191.645 | 623.023 | 141.317 |

Every refresh-inclusive probe loses. Nonspatial pins/refreshes are harness setup
costs for queries that stay native, not GPU cold-execution measurements. Spatial
refresh alone costs 231.037/466.649 ms. No OS-cache purge was requested; these
first warmups are not machine-cold certification or one-shot acceleration.

## Reproduction and identity

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_scalar_filter \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat sequentially for `spatial_grouped_scalar_filter` in a fresh directory.
The baseline workload is retained even though it does not select.

Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`; preceding uncommitted
milestones are part of this worktree. The complete implementation, tests,
benchmark source and build inputs are in `source-snapshot.tar.gz`; later
evidence/progress prose is outside it. SHA256:

```text
69a9b8a99f630c624aa3a3c4ce8e1eeb33d7d99e034e5885d2b9b0805c08b98c
```

Verified normal modules (PG18 also equals the local release artifact and both
benchmark bundles' loaded module):

```text
PG18 0c6f06e151cededb42076e97bfd6c7799f76e555da74d2eea97b18980a694ae4
PG19 8ec4c3aa434a1ae0805a7d9b779720ebade57874ae2c874abfff0a6fa4ce4fe5
```

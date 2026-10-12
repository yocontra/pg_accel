# Remove per-row geometry staging allocations

Geometry ingestion now passes a borrowed slice of the live, flat detoasted
Datum directly to the validated resident builder. The builder copies the exact
bytes once into retained column storage; it no longer creates an intermediate
per-row byte Vec. Distinct PostgreSQL detoast allocations are freed after the
synchronous copy/validation call, including when it returns an error. No source
pointer escapes. The same borrowed entry point handles spatial constants.

The geometry parser keeps an ordinary point's two coordinates inline rather
than allocating a two-element Vec. Empty points require no coordinate
allocation; larger geometries retain their checked heap allocations. Every
existing type/flag/SRID, finite-coordinate, topology, length, per-value and
vertex-limit validation remains in force. Resident arrays, retained exact
bytes, device code/ABI, planner admission and memory-accounting contracts are
unchanged. No buffer sharing across PostgreSQL processes is introduced.

This removes one temporary byte allocation per non-NULL geometry and a second
coordinate allocation per nonempty ordinary point. It does not remove retained
exact bytes, PostgreSQL's required detoast copies, table scans, device transfers
or spatial artifact construction. Loading and refresh can still dominate a
one-shot query.

## Checks

The 1,207 standalone Rust tests pass. New tests prove the inline point variant,
both supported serialization versions with/without bounding boxes, signed-zero
preservation, truncation/nonfinite/size rejection, and independent retained
ownership after source mutation/release. Rejected malformed values do not append
rows. Existing polygon/multipolygon, NULL/empty, byte-budget and storage checks
continue to pass.

The new live loader test explicitly stages inline, compressed and external-
TOAST geometries. It verifies the compressed/out-of-line fixture, compares
retained bytes against the former owned-copy boundary after dropping the source
table, and repeatedly rejects unsupported toasted 3D geometry. The first test
build caught a SPI tuple-entry versus raw-Datum accessor mismatch in the test
oracle; the corrected test uses the tuple table's raw-Datum accessor. That
failed build is retained.

The live loader test and strict Clippy pass on both PG18.4 and PG19beta1.
Documentation tests pass 21/21; formatting, diff and coverage-scope checks pass.
The inventory remains 97 SQL files / 446 assertions / 40 semantic families,
222 Rust files, 16 C++ sources and 33 native suites, with unchanged thresholds.
Normal installed release modules also pass SQL112, 114, 118, 129 and 134 on
both majors: 10 file executions / 30 assertion groups with no warnings. These
cover polygon/hole boundaries, multipolygons, exact NUMERIC spatial rollups,
scalar programs, combined WHERE, prepared invalidation and typed error paths.

## Matched workload observations

The previous spatial/scalar milestone provides the before-change source archive
and normal PG18 module (`f4c358a7d14c509ad21df20a37053d07c2190b140016e99d4415ce2e77d79cd0`).
The same benchmark binary, query, fixture, sizes and settings were run twice
afterward. The only production-source changes in this milestone are geometry
staging and its private parser. Tests, compilers and other project GPU jobs
were finished before measurement. This is still the shared Apple M2 Max / 64 GB
/ macOS 26.4.1 machine, not an isolated performance host.

| Rows | Before load ms | After A / B load ms | Before refresh-inclusive ms | After A / B refresh-inclusive ms |
|---:|---:|---:|---:|---:|
| 1,000,000 | 250.173 | 214.290 / 211.361 | 306.208 | 254.912 / 263.432 |
| 2,000,000 | 468.704 | 390.349 / 382.577 | 573.533 | 490.442 / 478.639 |

Load probes are approximately 14–18% lower; refresh-inclusive probes are
approximately 14–17% lower. These are one before and two after lifecycle probes,
not distributions or a statistically qualified/general improvement claim.
The fixture's NULL/empty frequencies imply 1,922,215 / 3,844,430 eliminated
temporary allocations per scan. This is allocation churn avoided, not retained
memory saved. Required exact bytes and resident accounting are unchanged.

Each candidate run contains 30 balanced randomized warm pairs after ten
warmups, separate persistent backends, full output consumption and PostgreSQL
baseline plans requesting eight workers. No GPU forcing or cost tuning was
used. Candidate A/B GPU warm medians are 6.103/5.626 ms at 1M and 8.123/8.281 ms
at 2M, versus parallel PostgreSQL 38.182/38.533 and 59.007/58.923 ms. The preceding
warm medians were 6.001/8.304 ms; GPU CV remains 9.09–14.05%. No warm-kernel
improvement is attributed to this ingestion-only change.

Both repetitions select and dispatch at both sizes (30/90 measured kernel
executions), with zero stock fallback and 990 consumed enabled output rows per
size. Native and independent fixture oracles pass. Provenance passes without
errors/warnings; both ledgers show zero crashes, ship-gate failures and
below-parity measured rows.

First warm executions are 121.617/121.204 ms at 1M and 186.344/185.744 ms at 2M,
versus preceding 118.647/182.811 ms: these do not improve. Every candidate
refresh-inclusive probe still loses to native (37.752–38.658 ms at 1M and
59.117–59.492 ms at 2M). Inclusive numbers already include refresh and must
not be double-counted. This is progress on ingestion overhead, not a claim
that cold/one-shot queries are now competitive or that a release gate passed.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload spatial_grouped_scalar_where \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/geometry-staging-20261006/spatial_grouped_scalar_where-a
```

The second repetition uses the `-b` artifact directory. Installed normal-module
SHA-256:

- PG18.4: `2d2c0c0381935f30249264795422d4e4276db317ec8d3a24838d2e55b77302ad`
- PG19beta1: `419264227c677dec571c16915f6fb053265c78c44e34bb99f73bfb57850b48b6`

The PG18 release build matches its installed module and remains restored after
the live pg_test builds. Source and evidence checksums identify the milestone
independently of HEAD.
Source archive SHA-256:
`15a09b6068b9a6b9083d98448b3504a2f6ceb0c1a0452ec7d96634fda7cb0edf`.

Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty worktree.
The matched preceding source/module and benchmark are retained in
`benchmarks/evidence/spatial-where-20261006/`. No full upstream-suite,
instrumented-coverage, native-kernel rerun, CUDA or release certification is
claimed for this Rust-only staging change.

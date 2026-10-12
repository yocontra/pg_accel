# Compatibility after the scalar and geometry expansion

This checkpoint qualifies the normal modules from
[`null-safe-scalar-20261006`](../null-safe-scalar-20261006/README.md), including
the preceding scalar FILTER/DATE/WHERE, qualifier-ordering, spatial-WHERE and
geometry-staging changes. No production source or module changed during this
checkpoint. These are local dirty-worktree results, not hosted release CI,
CUDA qualification or proof that every PostgreSQL query accelerates.

## PostgreSQL's own suites

| Version | Regression, each mode | Isolation, each mode | Total executions |
|---|---:|---:|---:|
| PG18.4 | 231 | 119 | 700 |
| PG19beta1 | 245 | 129 | 748 |

All eight schedules pass: pristine and pg_accel-preloaded/enabled regression
and isolation for each major. No expected-output changes or new exclusions.
Both complete artifact SHA256 seals verify. Upstream test databases do not
install pg_accel SQL objects; this verifies hook/default-path compatibility,
not GPU dispatch of the upstream corpus. Installed-extension tests are separate.

Both PostgreSQL builds use `--enable-debug --enable-cassert --without-icu` on
Apple M2 Max, 64 GB, macOS 26.4.1. The full upstream bundles retain source/diff
provenance, configuration, commands, results and server logs:
`upstream-pg18.tar.gz` and `upstream-pg19.tar.gz`. Their inner SHA256SUMS hashes:

```text
PG18 e1c8762e5617632c8cc2c74065b2b80aae0461ba4da078ec3c369a31c2fb034d
PG19 40d40daa4a6b27d9243956c82073f355aeb1f812d73f4cc1550910d9eeb281f7
```

## Full SQL suite: original failure and correction

The initial PG18 run completes 97/98 files, with SQL111 the only failure and
no dispatch timeout warnings. Its old negative matrix still required native
execution for `amount NOT IN (1,NULL)`, now supported by scalar aggregate
FILTER lowering. This is a stale admission expectation, not an observed
result mismatch. The failed run and original SQL/manifest/baseline inputs
are retained separately; they are not replaced with successful follow-ups.

SQL111 now tests five formerly negative predicates as selected differential
cases: NULL-containing NOT IN, NULL/empty constant ANY arrays, cross-column
OR, and NULL-accepting cross-column OR. They compare SUM/AVG/COUNT and group
activity against PostgreSQL with acceleration off and a widened numeric CASE
oracle; GPU availability requires actual dispatch and an increasing kernel
counter. The mixed INT2 interval matrix remains unchanged. Interval-capacity,
dynamic-array, cast and volatile-function decline checks remain. The existing
end-of-file zero-stock-fallback check still applies.

Assertion `111_independent_aggregate_filters.assert_003` grows from three
failure guards/eight guard tokens to five/nine. Its reviewed guard fingerprint
and source manifest are updated; the other two guard fingerprints are unchanged.
Inventory remains 98 files, 451 assertions and 41 families, at the unchanged
90% threshold. Semantic assertion completion is not instrumented Rust/C++ line
coverage.

Both complete corrected runs pass 98/98 files and 451/451 declared assertions
per major, with no warnings/skips/caught-exception evidence. Each SQL135 log
explicitly reports 48/48 NULL-safe WHERE shapes and 24/24 FILTER/CASE/count
shapes selected. The corrected focused PG18 SQL111 run also passes. Coverage
tooling passes 104 tests, documentation parity 21 tests, and the scope audit
passes.

`sql-pg18-original.tar.gz` includes the failed run and its original source,
manifest and baseline inputs. `sql-pg18-final.tar.gz` and `sql-pg19-final.tar.gz`
include all corrected logs, results and semantic inventories.
`sql111-correction.patch` records the exact test change; it does not alter
production behavior.

## Extension catalog lifecycle

The isolated catalog integration command passes all five checkpoints on both
majors: preload-only with spoofed helper names, CREATE rollback, installed
execution, DROP rollback, and DROP/recreate. Generic prepared plans decline
without the extension and resume normal selected GPU execution after restoring
it; exact aggregate outputs remain unchanged. Both commands successfully remove
only their newly created test databases. Logs are `nullsafe-catalog18.log` and
`nullsafe-catalog19.log`; there are no warnings/errors.

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
PG_ACCEL_PG_MAJOR=18 bash scripts/extension_catalog_integration.sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/19beta1/bin/pg_config" \
PG_ACCEL_PG_MAJOR=19 bash scripts/extension_catalog_integration.sh
```

## Identity and reproduction

Base HEAD: `5c7fbd0a6381f823befd823ec47137a8a602e94d` (dirty).
The prior implementation source archive is
`../null-safe-scalar-20261006/source-snapshot.tar.gz`, SHA256
`3adc1f263abb51f8774682ea2a268d513282546d28d8036b571eba2fcfa43976`.
This checkpoint's `source-snapshot.tar.gz` additionally captures the corrected
test, coverage metadata and TODO entry, SHA256
`dcd5ca8010fe294ddff8823450bb7c9e26786c180533975941599bda01f2e52c`.
The SQL correction changes tests and coverage metadata only. Normal modules:

```text
PG18 602118cf09e2ee07b4b50a6dd5eefd2b7d8d0630959f1274a3675c01cee2e787
PG19 b2879402a962a23da4acea063d3f83ec7cbee98deb7b01dd86e34f1fcd725f39
```

Upstream commands, run sequentially with `ulimit -n 4096`:

```sh
scripts/upstream_pg_tests.sh 18 NEW_PG18_ARTIFACT_DIRECTORY
scripts/upstream_pg_tests.sh 19 NEW_PG19_ARTIFACT_DIRECTORY
```

For the installed-extension matrix, use the corresponding installed `psql`,
port 28818/28819 and a fresh output directory, for example:

```sh
PG_ACCEL_PSQL="$PWD/.pgaccel/postgres/install/18.4/bin/psql" \
PG_ACCEL_PG_MAJOR=18 PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 \
PG_ACCEL_SQL_TEST_ARTIFACT_DIR=NEW_SQL_ARTIFACT_DIRECTORY \
sql/tests/run_all.sh 'host=localhost port=28818 dbname=postgres'
```

Both normal servers retain the 5-second kernel timeout. PG18 has PostGIS 3.6.3
and h3/h3_postgis marked `unreleased`; PG19 has PostGIS 3.6.4 and H3 4.2.3.
The saved profile logs identify those limits rather than claiming identical
dependency versions or released H3 qualification on PG18.

This milestone does not close broader concurrency/owner failure, shared
residency, streaming/spill, joins/windows, full benchmark corpora or release
gates. No new performance measurements or performance claims are made here.

# Occurrence-aware resident join binding

JQS1 now represents a two-input integer equijoin stage independently of the
physical table identities. It supports four equality keys, sixteen ordered
INT4/INT8/BOOL/DATE consumer columns, and inner/left/semi/anti semantics. The
logical validator rejects invalid orientations, unknown occurrences, conflicting
physical-column types and inner outputs from semi/anti joins. Duplicate output
slots are preserved. The strict, bounded integer-word codec retains alias
identities; physical dependencies are separately deduplicated by OID/attribute.
This is not an arbitrary pipeline graph or PostgreSQL CustomPath admission.

The binder loads physical dependencies through the existing residency manager
and captures generation/global-generation/relfilenode stamps. A single immutable
store borrow revalidates the complete set, types, row domains and NULL lengths.
No raw input pointers persist between calls. Same-generation refresh resolves
new buffers safely; changed dependencies fail before exposing any inputs.
The bound stage executes its own join and gather together, preventing accidental
use of a different join's row mapping. Output columns own their device buffers
and charges independently of both the source store and temporary pair buffer.
The binder is backend-local, non-Send/non-Sync, and is not an ACL authorization
layer. SQL permission, shape and bounded-work proof remain caller requirements.

## Live evidence

Both PG18.4 and PG19beta1 pass two focused live tests:

- The new binding test roundtrips JQS1 and uses actual resident table storage
  for 40 cases: five relation pairings, one/two keys and four join kinds. Self
  joins use different attributes on the two aliases (`l.a = r.p`), so erasing
  occurrence orientation cannot accidentally pass. Results include reordered
  and repeated columns, NULLs, dates, booleans, signed INT8 key endpoints,
  duplicate multiplicity, empty sides and insufficient-capacity rejection.
- Additional lifecycle cases reject stale inputs after UPDATE and rollback,
  rebind after same-generation refresh, reject wrong DATE/INT4 identities and
  eviction, respect disabled auto-load, and preserve already-gathered output
  across source invalidation. Temporary device charges return to baseline.
- The existing primitive join/gather/scalar-aggregate test still passes,
  including direct resident SUM/COUNT composition and ownership/budget checks.

These tests call the internal execution path; SQL joins themselves remain
native. No performance, CUDA, concurrency, spill or release claim is made.

Final standalone suites pass 1,221 tests on each PostgreSQL major. All 34 native
CTests pass; strict all-target pg_test Clippy passes for both majors. The
coverage-tool suite passes 104 tests, documentation checks pass 21 tests, and
the safety-contract check passes. The coverage inventory is 226 Rust files,
17 C++ sources, 34 CTests and unchanged 100 SQL files / 462 assertions / 43
families at the existing 90% gate. This is inventory evidence, not a fresh
line-coverage measurement or a full PostgreSQL SQL-suite run.

Normal non-pg_test extension builds were restored after live testing. Their
module SHA-256 values are:

- PG18: `e61a7518818eb95845ab4c3ee0381ef4e8832a1d7433b1303eb000bbb622a115`
- PG19: `6b59a8b1a4641e4399720f41ab4593eda8351d71b438a2db0f2d2522f88b172c`

After restoration, SQL21, SQL55, SQL56 and SQL137 pass on both majors: four
files and 26 explicit assertions per major, with no skip markers. These retain
normal join-decline behavior and existing selected OLAP/geo report execution;
they do not claim new join admission. The corresponding `join-binding-sql*`
logs contain every assertion/file marker.

## Failures retained and addressed

The first live run aborted inside Metal's `newArgumentEncoderWithBufferIndex`
while dispatching the row producer. The crash report identifies the native
join `execute` call, not the gather. Earlier live fixtures always supplied NULL
sidecars; real residency omits them when all values are non-NULL. The producer's
pointer-rich by-value request capture was replaced with a device metadata
pointer. Its exact 352-byte request is copied into the caller's queried,
charged workspace for nonempty outer inputs. All row work remains on device;
the copy contains fixed metadata only. No cache purge, runtime patch, weakened
limit or fabricated sidecar was used. Public native layouts/signatures and
AQS6/AOP2 are unchanged; the workspace query reports the additional bytes.

Native coverage adds present, absent and asymmetric NULL sidecars at each of
the four key counts, through all join kinds. The focused native binary passes
6,509 checks. The failed run is published; the original crash report is retained
privately under the publication exception below.

The next live run passed the join matrix but failed the update fixture: direct
Rust loading used the outer pg_test function's snapshot, which preceded SPI
fixture writes. The test now mirrors the existing direct-loader test helper:
push a private snapshot copy, update its command ID, and pop through a guard.
Production snapshot handling is unchanged. The failed run is retained.

The conservative CPU-work audit still reports three unproved join exports.
Nothing excludes or waives those findings. SQL admission, complete pipeline
serialization, planner/cost/permission proof, bounded/skew-aware dispatch and
CustomScan lifecycle remain unfinished.

## Reproduction

Build and live-test shells used `ulimit -n 4096`. The PG19 combined live run
explicitly sets `RUST_TEST_THREADS=1`. Each cargo invocation uses matching
`PG_CONFIG` and `PGRX_PG_CONFIG_PATH` from the repository's PG installation.

```sh
just test-standalone 18
just test-standalone 19
cargo pgrx test --package pg_accel pg18 pg_resident_join
RUST_TEST_THREADS=1 cargo pgrx test --package pg_accel pg19 pg_resident_join
just gpu-build
ctest --test-dir pgaccel-kernels/build --output-on-failure --timeout 300
cargo clippy -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings
cargo clippy -p pg_accel --no-default-features --features pg19,pg_test --all-targets -- -D warnings
just safety-contract-audit
python3 -m unittest scripts.tests.test_doc_parity
python3 -m unittest scripts.tests.test_coverage_tools
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
python3 scripts/cpu_cheat_audit.py pgaccel-kernels/src/join_rows.cpp
just install-pg-accel 19
just install-pg-accel 18
# After normal installation, each major's psql runs these with ON_ERROR_STOP=1:
# sql/tests/{21_join_types,55_hash_join,56_hash_join_edge_cases,137_pulled_up_reports}.sql
```

Local Apple M2 Max / Metal evidence on macOS 26.4.1, October 9, 2026. Base commit:
`5c7fbd0a6381f823befd823ec47137a8a602e94d`. The dirty-tree source snapshot and
checksums identify the tested implementation; the commit alone does not.

Source archive SHA-256:
`1e948b47b28dba27939118eb1b5454de843aa1c90400a8bb3db963deb25765bf`.

## Public artifact exception — 2026-10-12

The original `initial-metal-abort.ips` is withheld from publication because it
contains host, user and process identifiers. Its exact repository-relative path
is `benchmarks/evidence/resident-join-binding-20261009/initial-metal-abort.ips`;
its original SHA-256 is
`2969b7b33c1b1b8804b5f9f40b163112d4ed311b454a3204177383b9eb5695e0`.
The local original remains unmodified and unpublished. No sanitized substitute
is presented as the original crash evidence.

`SHA256SUMS` remains the unmodified historical manifest, including the withheld
file and the earlier version of this README. It is not a verification manifest
for this public subset. `PUBLIC_SHA256SUMS` covers every published file in this
directory, including the historical manifest and updated README, but excludes
itself and the private crash report. From the repository root, verify with:

```sh
shasum -a 256 -c benchmarks/evidence/resident-join-binding-20261009/PUBLIC_SHA256SUMS
```

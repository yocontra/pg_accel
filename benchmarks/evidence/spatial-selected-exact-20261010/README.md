# Chunk-bounded selective spatial recheck snapshots — 2026-10-10

Spatial aggregate preparation no longer copies exact GSERIALIZED bytes for
every source row before classification. It reserves one reusable chunk-sized
selection buffer. Preflight examines only fixed chunk-boundary offsets to find
the largest possible payload; source geometry bytes are not scanned. The
offset table covers the largest chunk, not the entire relation.

After a chunk's evaluation/compaction/failure checks, only uncertain rows are
copied under a fresh immutable store borrow. Generation, global generation,
relfilenode, type and row count must still match preparation. Indices must be
strictly increasing, in range and relative to the same fixed chunk used for
preflight. The borrow ends before any PostGIS function is called. Rechecks use
owned selected bytes; NULL exact values are rejected. Failed refills make old
selection contents inaccessible. Existing final catalog/dependency publication,
private-mask ownership, exact predicates and cancellation cleanup remain.

The full scratch capacity remains charged throughout finalization. No device
buffer, kernel, C ABI or SQL admission rule changes. Resident source exact
geometry remains fully retained; this is not streaming or spill support.

## Verification scope

PG18 and PG19 standalone suites each pass 1,306 tests. Strict production and
pg_test Clippy pass on both majors. Documentation parity passes 21 tests;
safety-contract, coverage-scope, formatting and whitespace checks pass without
changing coverage floors. Existing host suites contain no-device guarded cases;
the four new host tests below run unconditionally.

Four new unconditional host tests cover exhaustive small selection subsets,
NULL/variable lengths, strict index/layout validation, retry, source ownership,
eviction/stamp mismatch and processing pending invalidations before borrowing.
Successful refills allocate zero times. In a 1,000,017-row fixture with 32-byte
values and 65,536-row chunks, scratch is 2,621,448 bytes instead of 40,000,688.
Selecting two rows copies 64 payload bytes and leaves the unused payload tail
untouched. A zero-selection fill copies no payload.

A real-catalog pg_test fixture spans 131,073 points in three chunks, with sparse
boundaries, NULL/EMPTY geometry, source-byte-layout changes, warm reuse and
all-NULL/empty inputs. This is compile-only evidence here. Existing spatial
error/cancellation and DML/rollback fixtures remain in place. No new live/GPU
execution or query-performance result is claimed.

The initial test compile caught an incorrect test helper name; production lint
caught a missing test-only semicolon. Both failed logs are retained.
Native code is unchanged from the sealed narrow-key bundle, whose full linked
audit has 124 passes and seven unresolved findings across 131 exports. Those
findings and all broader release/runtime TODOs remain open. Installed PostgreSQL
extension modules were not replaced.

Verification logs and a source snapshot accompany this report; SHA256SUMS seals
the bundle.
The snapshot excludes Git metadata, builds and evidence bundles. Branch main
remains uncommitted: this session has read-only `.git` access and cannot push.

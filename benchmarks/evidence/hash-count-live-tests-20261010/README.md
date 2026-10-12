# Live qualification fixture for staged H3 COUNT(column)

2026-10-10. Continues `hash-count-binding-20261010`. **The live fixture is
implemented and compile-checked, not executed or qualified on GPU.** Normal
SQL admission and runtime capability still reject this candidate.

## What changed

`pg_accel/src/engine/executor/agg/h3_count_test.rs` adds one pg_test matrix:

- Nine SQL types (BOOL, INT2/4/8, FLOAT4/8, DATE, TIMESTAMP, TIMESTAMPTZ), each
  through cached-parent and fused-parent execution. FALSE, signed minima,
  NaN and infinities ensure count semantics do not depend on value payloads.
- Real catalog-resolved H3/column identities and 4,097 rows spanning three
  base cells plus a NULL-key group, including a nonempty zero-count group.
- Complete tuple materialization compared with PostgreSQL's native H3 and
  COUNT result, not only intermediate buffers. COUNT outputs must be non-NULL.
- Cold/warm execution, value-only NULL/non-NULL updates, key-only updates,
  truncate and empty-to-nonempty input. The same prepared plan must rebuild
  after each mutation and must not rebuild again during its warm execution.
- A real internal subtransaction loads and executes changed data before
  rollback. The retained plan must rebuild afterward and reproduce the
  original results, not the rows cached inside the aborted scope.

The test-only entry proves the typed-count contract, logical spec/projection,
and remaining canonical H3 shape restrictions before entering the shared
catalog-validated constructor. Tests explicitly assert that ordinary constructor
and normal-admission gates reject the same nullable-count shape. No test GUC
or SQL path is introduced. The policy override and its clearly labeled evidence
reason compile only with `pg_test`; production builds retain ordinary policy
selection. Fresh private snapshots expose nested SPI writes without mutating
the caller's snapshot. A missing GPU fails the fixture rather than returning
a passing no-op.

## Verification

- PG18 and PG19 core suites: **1,267 passed each**, zero failures/ignored tests.
- Strict workspace Clippy passes for both ordinary major-version builds.
- Strict Clippy with `pg_test` and all targets passes for PG18 and PG19;
  `clippy-pg*-pg-test-final.log` covers the final loaded-rollback fixture.
- Safety contracts, documentation checks (21 tests), formatting, whitespace
  and coverage-scope audit pass. Scope remains 229 production Rust files,
  18 C++ sources, 36 CTests, 100 SQL files, 462 SQL assertions, 43 families,
  and the unchanged 90% threshold. The test file uses the existing `_test.rs`
  exclusion convention; no scope rule was changed.
- Initial compile/lint failures (missing test import, wildcard import and
  redundant iterator conversion) are retained alongside the corrected logs.

No live PostgreSQL/GPU or performance result is claimed. Existing host limits
and the seven unresolved native execution-export audit findings remain open;
the audit was not rerun because native source is unchanged in this increment.
No installed extension or database server was modified. TODO and planning docs
remain because the overall objective is unfinished. Git metadata is read-only
in this session, so no commit or push was possible.

## Reproduction

```sh
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
# For each major, bind PG_CONFIG and PGRX_PG_CONFIG_PATH to its real pg_config:
cargo clippy -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings
cargo clippy -p pg_accel --no-default-features --features pg19,pg_test --all-targets -- -D warnings
```

On a GPU-enabled PostgreSQL/H3 host, execute the pgrx test
`pg_h3_nullable_hash_count_resident_currentness_and_materialization` for both
majors. Compilation alone must not close the qualification TODO.

`source-snapshot.tar.gz` captures the accumulated dirty source worktree;
`SHA256SUMS` binds this bundle and its raw check logs.

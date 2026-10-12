# Fused H3 mask and dispatch interrupt handling

2026-10-10. Extends the cached-preparation implementation in
`hash-preparation-cancel-20261010`. SQL admission remains unchanged.

Query-local Boolean masks now poll before reservation/allocation, before each
bounded conversion and after its final call. Interruption discards the buffer
before releasing its charge and returns a distinct result, never a partial mask.
Empty/unfiltered binding also polls without device work. Fused H3 execution polls
around the grouped call and discards completed-but-unpublished output on a pending
signal. In-flight synchronous calls are not asynchronously cancellable.

The outer H3 driver services PostgreSQL only after `execute_once` returns: both
the ephemeral-artifact RefCell guard and resident-input borrow have ended.
Captured PostgreSQL errors retain their original SQLSTATE/message; temporary
owners drop before rethrow. Benign wakeups retry with fresh input resolution.
Stale-input errors still use the existing single-artifact-rebuild limit. Cached
H3 dispatch also gains interrupt servicing before/after its one-shot call.
Other aggregate execution paths retain their existing orchestration.

## Evidence

- PG18 and PG19 standalone suites each pass **1,279 tests**. Added coverage checks
  query-local interruption before reservation/FFI and shape validation before
  polling; empty fused binding asserts the typed interruption signal. Existing
  bounded-driver cleanup/retry and mask-boundary tests remain green.
- Strict production workspace Clippy and pg_test/all-target Clippy pass on both.
- The live filtered fixture now tests every fused polling boundary, including
  after grouped execution, original SQLSTATE 57014/message, exact charge cleanup
  in the error handler, released ephemeral borrow before retry, exact output on
  retry, and a benign late interrupt that repeats both mask and grouped work.
  These assertions compile but **have not run on GPU/PostgreSQL here**.
- Safety contracts, doc parity (21 tests), unchanged 90% coverage-scope audit,
  formatting and whitespace checks pass.
- Retained initial failures: strict lint identified the obsolete non-polling
  converter wrapper (removed); live-fixture compilation rejected a non-unwind-safe
  plan capture in its error handler (borrow assertion moved after capture).
- No native source/library, ABI manifest or installed extension module changed.
  No new native audit result, runtime qualification or speedup is claimed; the
  prior seven unresolved native audit findings remain open.

## Reproduction

```sh
ulimit -n 4096
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
just safety-contract-audit
just doc-parity
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
cargo fmt --all -- --check
git diff --check
```

Live compilation uses `cargo clippy -p pg_accel --no-default-features --features
pg18,pg_test --all-targets -- -D warnings`, with PG_CONFIG and
PGRX_PG_CONFIG_PATH targeting `.pgaccel/postgres/install/18.4/bin/pg_config`;
PG19 substitutes pg19 and the 19beta1 installation.

SHA256SUMS binds logs and an implementation-source/documentation snapshot,
excluding Git metadata, generated builds and evidence directories. No commit
or push was possible because session permissions make Git metadata read-only.
TODO.md and planning docs remain: live/GPU, performance and broader goal work
are unfinished.

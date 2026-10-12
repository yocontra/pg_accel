# Cached H3 preparation interrupt boundaries

2026-10-10. Cached parent and Boolean FILTER preparation now polls read-only
PostgreSQL signal flags between synchronous device calls and after each lane's
final call. The existing holdoff/critical-section/process-death rules apply.
No PostgreSQL API runs under the resident-store borrow.

An interrupted callback returns a typed `PreparationInterrupted` result. Its
unfinished device lanes, store borrow and whole-artifact reservation release
before the outer driver services PostgreSQL. The original captured error is
rethrown, not converted into a generic loader error or fabricated cancellation.
A benign wakeup retries through the reserve-first resolver with fresh dependency
and input binding. Failed/partial artifacts are not published. A signal arriving
after successful publication can leave a complete valid cache entry; it cannot
publish unfinished device output. In-flight synchronous calls remain uninterruptible.

The fused-parent query-local mask still uses the non-polling conversion wrapper.
Its cancellation integration remains open, as do GPU/live execution qualification
and the rest of the conditional-H3 TODO. SQL admission and the native ABI are
unchanged. No performance improvement is claimed.

## Verification

- PG18 and PG19 each pass 1,278 standalone tests, including four new tests.
- Chunk-driver tests cover empty input, aligned boundaries, a partial last chunk,
  initial/middle/final interruption, and typed launch failure. A prelaunch test
  proves cancellation returns before any device FFI or buffer access.
- The actual reserve-first store API is tested for interruption under an active
  borrow/charge, zero publication, full reservation rollback, and successful
  rebinding after an outside-borrow generation change.
- Strict production workspace Clippy and pg_test/all-target Clippy pass on both
  versions. Live fixtures now assert real SQLSTATE 57014 and its original user
  cancellation message, exact charge cleanup before rethrow, interruption at
  every parent/mask boundary, output parity on retry, and benign wakeup retry.
  These fixtures were compiled, **not executed**: no GPU is visible on this host.
- Safety contracts, documentation parity (21 tests), coverage scope at its
  unchanged 90% floor, formatting and whitespace checks pass.
- The first new-test compile failed on an ambiguous integer literal type; the
  retained initial log records it. The corrected full suites passed afterward.
- No native source/library, installed PostgreSQL module, or ABI manifest was
  changed. No fresh native audit or GPU execution success is claimed. The prior
  seven unresolved native audit findings remain open.

## Reproduction and provenance

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

Live-fixture compilation uses `cargo clippy -p pg_accel --no-default-features
--features pg18,pg_test --all-targets -- -D warnings`, with PG_CONFIG and
PGRX_PG_CONFIG_PATH set to the repository's PG18.4 installation; PG19 uses
pg19 and the PG19beta1 installation.

Logs and the implementation-source/documentation snapshot are bound by
SHA256SUMS. The snapshot excludes Git metadata, generated builds and evidence
directories. Git metadata is read-only in this session: no commit or push was
performed. TODO.md and planning documents remain because the goal is unfinished.

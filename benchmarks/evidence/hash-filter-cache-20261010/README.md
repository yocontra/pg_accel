# Cached H3 predicate-mask reuse

2026-10-10. Continues `hash-filter-binding-20261010`. The cached-parent path now
retains its Boolean FILTER mask instead of allocating and converting it during
every query. **SQL admission and GPU qualification remain unchanged/closed.**

## Implementation and tradeoff

The cached artifact owns parent keys and its optional mask under one reservation:
eight bytes per row without FILTER, nine with FILTER. The constructor checks
both lane lengths against the logical filter and row domain. Forecasts and
actual accounting use the same checked arithmetic; overflow rejects the build.
The mask is populated only inside the existing reserve-before-build callback.
It receives no second execution charge. A failed allocation/conversion cannot
publish a partial artifact, and the owners drop before the outer charge.

Predicate columns now participate in artifact construction as well as selected
and dispatch dependencies. The mask and parent lane share the canonical query
identity, dependency generations and snapshot validation. Warm binding verifies
current input types/lengths and borrows the retained mask. DML, rollback, refresh
or other dependency invalidation must rebuild the whole artifact.

The fused-parent path remains query-local: its mask still gets an execution
charge and is converted for each dispatch. Policy estimates now include mask
bytes and conversion submissions on both paths. Empty inputs allocate no mask;
unfiltered behavior, count partition checks and ABI layouts are unchanged.

For N filtered rows, cached reuse removes an N-byte transient allocation and
ceil(N / 65,536) conversion submissions per warm execution. It adds N retained
bytes between queries; **peak mask storage is not reduced**. At 12,748,986 rows,
that is 12,748,986 retained bytes and 195 avoided warm conversions. These are
source-derived work/storage counts, not measured GPU/query speedups. First-use
conversion work is moved into artifact preparation, not eliminated.

## Verification

- PG18 and PG19 standalone suites each pass **1,274 tests**.
- Strict production workspace Clippy and pg_test/all-target Clippy pass on both.
- New host tests cover exact parent/mask bytes, lane mismatches, overflow,
  predicate dependency union, chunk-boundary policy estimates and preallocated
  output validation before FFI or any charge. Existing budget/partition and
  empty cached/fused binding tests remain green.
- The live filtered-H3 fixture now requires nine-byte cached accounting and
  one warm grouped submission versus three for the two-chunk fused fixture.
  Exact PostgreSQL tuples, predicate-only DML, rollback of loaded rows, truncate
  and reload remain required. These assertions compile but were **not executed**
  on this no-GPU host. Counters alone are not correctness or device-work proof.
- Safety contracts, documentation parity (21 tests), coverage scope and
  formatting/whitespace checks pass without lowering thresholds.
- No native source, binary, metadata receipt or ABI manifest changed. Their
  checksums match the preceding bundle. Its linked audit still has seven
  unresolved exports out of 131, and its GPU CTest failed at initialization.
  Those results are prior evidence, not fresh runtime qualification in this turn.
- Installed PostgreSQL modules were not replaced; their checksums are retained.

## Remaining work

Run the live/GPU differential and lifecycle fixtures, including cache-hit
submission assertions. Qualify cancellation and cold/concurrent behavior;
measure the retention-versus-recomputation tradeoff and calibrate admission.
General expression filters and mask fusion remain unfinished. No TODO parent
item or release gate is marked complete by this increment.

## Reproduction

```sh
ulimit -n 4096
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
just safety-contract-audit
just doc-parity
cargo fmt --all -- --check
git diff --check
```

The snapshot captures implementation source and repository docs, excluding Git
metadata and generated build/evidence directories. SHA256SUMS binds the report,
logs and snapshot. Git metadata remains read-only, so commit/push to main could
not be performed. TODO and planning docs are retained while the full goal is open.

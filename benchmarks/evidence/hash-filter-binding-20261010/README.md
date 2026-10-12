# Resident Boolean FILTER binding for hash/H3 counts

2026-10-10. Continues `hash-filter-20261010`: connects exact Boolean resident
predicates to both cached-parent and fused-parent hash count descriptors.
**Normal SQL admission remains closed. This is not GPU or release qualification.**

## Implementation

- The additive `pgaccel_bool_sql_mask` native export converts resident BOOL
  values plus an independent NULL sidecar into SQL TRUE/FALSE/UNKNOWN on-device.
  A call is limited to 65,536 rows; output is four-byte aligned and disjoint
  from inputs, and one work item owns each four-byte word. Partial final words
  never write beyond the declared byte span. No host row processing occurs.
- Invalid evaluated Boolean/NULL bytes become marker 2, not SQL UNKNOWN. The
  consuming hash FILTER rejects that marker before publishing any result.
  NULL rows do not inspect value payloads. Conversion success is not proof that
  the input contained only canonical bytes.
- Rust's `SqlBoolMask` reserves exactly one execution byte per row before device
  allocation, constructs the mask in aligned chunks, and releases the buffer
  before its charge. The bound descriptor retains the owner through synchronous
  dispatch. No device mask pointer is serialized or stored after its owner dies.
- The counted column, grouping key and direct fact-BOOL predicate are resolved
  under one resident borrow, with exact types/lengths and unambiguous column
  identities. Selected and dispatch dependencies include the predicate. The
  query-local mask is recomputed, not cached across mutations. Empty inputs
  allocate no mask and use the equivalent disabled native filter.
- Filtered COUNT(*) permits zero counts and a total no greater than selected
  rows. Exact unfiltered COUNT(*) partition checks remain unchanged. Counted
  column NULLs remain independent of predicate NULLs; all groups survive FILTER.
- This adds an export (131 total), not a C struct layout or logical wire change.
  The literal ABI count/hash anchors were updated together. No audit rule or
  admission/performance threshold was weakened.

## Verification

The shared library and native test executable build. Protected-memory and
pre-device boundary checks pass **1,909 tests**, including null/overlapping/
misaligned pointers, oversize chunks and address overflow. Device tests compile
for all 65,536 Boolean/NULL byte pairs, absent NULL sidecars, partial words and
output guards. The GPU CTest **fails** at initialization because no SYCL GPU is
visible; none of these new device assertions is credited as executed.

Standalone tests cover filtered count-total/zero-group bounds, unchanged
unfiltered partition failures, predicate dependencies, empty cached/fused
ownership, wrong/missing predicate types and budget rejection before allocation
or pointer access. PG18 and PG19 each pass **1,271 tests**. Strict production
workspace lint and pg_test/all-target lint pass for both majors. Safety,
documentation (21 tests), scope and whitespace checks pass without changing
coverage thresholds.

The live qualification fixture uses the real H3 catalog and final PostgreSQL
tuple materialization. Both count forms and cached/fused policies are compared
with native PostgreSQL using 65,537 rows, nullable predicates, warm reuse,
predicate-only changes, rollback of rows loaded inside a subtransaction, value
changes, truncate and empty-to-nonempty reload. The ordinary constructor must
still reject these shapes. These fixtures are compiled, not executed here.

The initial Rust compile failures (missing enum/type annotation and view field)
are retained. All **180 analyzer tests pass**, including the expanded ABI
inventory, literal integrity anchors and hostile production-name witnesses.
The linked audit and analyzer suite use the expanded ABI inventory;
raw failures are retained without waivers. The linked audit exits 1 with
**124 passing exports and the same seven unresolved findings across 131**.
The new converter passes the existing device-output proof without an exemption.
The remaining findings are the two grouped-aggregate execution exports and
five join execution/count/projection exports. The existing metadata-object macOS
deployment-target link warning remains, not package qualification.

## Remaining work

Run GPU and live PG18/19 differential/lifecycle qualification. General
expression predicates, mask reuse/fusion, cancellation qualification, calibrated
costs and performance evidence are unfinished. No query speedup or production
SQL capability is claimed. The full TODO objective remains active.

## Reproduction

```sh
ulimit -n 4096
cmake --build pgaccel-kernels/build --target pgaccel_kernels_shared test_grouped_agg_hash --parallel 2
pgaccel-kernels/build/test_grouped_agg_hash --contract-only
ctest --test-dir pgaccel-kernels/build -R '^test_grouped_agg_hash$' --output-on-failure
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
python3 -m unittest scripts.tests.test_cpu_cheat_audit
just safety-contract-audit
just doc-parity
```

The source snapshot captures the accumulated dirty worktree. SHA256SUMS binds
this report, logs and snapshot. Git metadata is read-only in this session, so
the requested commit/push to main could not be performed. TODO/planning docs
remain until the complete objective is achieved.

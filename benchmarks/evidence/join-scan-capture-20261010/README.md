# Controlled join scan: explicit scalar metadata capture

2026-10-10. Continues the reference-capture obstacle identified in
`workspace-input-proof-20261010`. **No new SQL admission or GPU qualification.**

## Runtime change

The bounded upward prefix scan's host callback now explicitly captures
`level_rows`, an immutable uint64 snapshot of `level.count`, by value. The
nested SYCL closure uses that scalar for the same final-block bound rather
than referring to the complete host `Level` record through the outer callback.
`ControlledRun::ranges` still invokes its callback synchronously; `launch`
still awaits each event and polls cancellation before continuing. This is an
ownership/representation simplification, not a claim that the previous
synchronous reference capture was a demonstrated dangling-pointer bug.

Input/offset/sum pointers, workspace layouts, scan ranges, arithmetic, join
semantics, cancellation points and output publication are unchanged. No ABI
export, logical wire, planner gate or cost coefficient changed. No speedup or
measured device-argument-byte reduction is claimed.

## Verification

- Native shared library, join executable and host layout executable build.
  The existing link warning about the metadata object's newer macOS deployment
  target is retained in `build.log`; this is not package/release qualification.
- The protected-row-buffer host layout matrix passes **40,389 checks**. Its
  CTest entry passes. These tests cover layout behavior, not GPU scan results.
- The real join GPU CTest **fails**, not skips/passes: no visible SYCL GPU,
  executable exit 77 and CTest exit 8. Raw initialization output is retained.
  Existing device fixtures compile, including controlled multilevel prefixes,
  exact count-only/emitted multiplicities, cancellation and retry, but their
  GPU assertions were not executed on this host.
- PG18 and PG19 standalone suites each pass **1,268 tests**. Strict workspace
  Clippy passes for each major. Installed PostgreSQL modules retain their
  previous checksums; no server or extension installation was modified.
- **40 focused proof/indexed-escape tests pass.** No checker, test expectation
  or approval rule changed in this increment. All 59 current-source helper
  calls now pass the existing stricter root/initializer/update checks: grouped
  callers 32/1/1, ordinary join 11, controlled join 14. The formerly rejected
  reference-capture form remains rejected by the checker regression tests.
- Safety contracts, documentation parity (21 tests), scope and whitespace
  checks are recorded. Scope/thresholds and the 130-export ABI manifest remain
  unchanged.

The fresh linked release audit uses a marker created before the source edit
and rebuild, the new shared library and its generated metadata-binding receipt.
It exits 1 with **seven unresolved findings across 130 ABI exports**, unchanged
in scope: both grouped-aggregate execution exports, ordinary/controlled join
execution, controlled join counting, and ordinary/controlled join projection.
Its raw output/report are included. Workspace call facts still are not linked
device-output provenance or full callback/control-flow proofs. No failed
release finding is waived.

## Reproduction

```sh
cmake --build pgaccel-kernels/build --target pgaccel_kernels_shared test_join_rows test_join_rows_layout --parallel 2
ctest --test-dir pgaccel-kernels/build -R '^test_join_rows_layout$' --output-on-failure
ctest --test-dir pgaccel-kernels/build -R '^test_join_rows$' --output-on-failure
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
```

Use the actual-source workspace checker commands from the preceding bundle;
the controlled join now returns 14 facts rather than a LambdaExpr decline.
The complete accumulated dirty worktree is captured by the source snapshot.
SHA256SUMS binds this report, raw evidence and snapshot. Git metadata remains
read-only, so no commit or push was possible. TODO/planning docs are retained
because the overall objective is unfinished.

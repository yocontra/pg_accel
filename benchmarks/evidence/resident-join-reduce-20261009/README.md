# Resident join → global SUM/COUNT — live qualification pending

2026-10-09, PostgreSQL 18.4 and 19beta1. This checkpoint implements a typed
consumer of resident join output. It does not enable a SQL planner path,
establish a speedup, or qualify the pipeline on a GPU.

## Implemented

- JRS1 is a bounded, pointer-free reduction specification over ordered JQS1
  projection slots. It supports one to four INT4 scalar measures, exact
  BOOL/INT4/DATE input types, repeated input slots and lazy filtered measures.
  Each measure produces SUM and non-NULL COUNT; constant 1 supplies COUNT(*).
  Decoding validates all sections and scalar programs before owned allocation.
- `ResidentJoinInputs::execute_reduced` composes the existing interrupt-aware
  join/gather producer with bounded parallel dense integer aggregation. Joined
  rows remain resident; only up to four final SUM/COUNT state pairs return to
  the host. This is three composed operators, not a fused join/reduce kernel.
- Scalar serialization now supports exact-length caller-owned buffers without
  changing SPG1. Reduction uses bounded stack metadata and the existing charged
  plan/session snapshots. It preserves NULL SUM for empty/all-NULL inputs,
  checks cardinality and drops device owners before rethrowing captured
  PostgreSQL interrupts. Capacity failure never enters the consumer.
- The existing 40-case live occurrence-binding matrix now also compares four
  reductions with PostgreSQL: nullable INT4, COUNT(*), boolean-filtered INT4
  and DATE differences. Cases exercise all four join kinds, one/two keys,
  aliases and empty inputs, plus capacity/type/chunk rejection and accounting.
  These live test bodies were type-checked, **not executed** here.

## Verification

- Standalone Rust core: **1,239 tests passed on each PostgreSQL major**.
- Strict Clippy: PG18 and PG19, all targets including `pg_test`, passed.
- Coverage-tool tests: 104 passed; documentation-parity tests: 21 passed.
- Safety-contract audit, documentation parity, formatting and diff checks
  passed. Coverage scope passes with 228 Rust files, 17 C++ sources, 34 CTests,
  100 SQL files, 462 SQL assertions and 43 semantic families. This inventory
  audit is not a measured runtime coverage claim; thresholds were not lowered.
- Native `test_join_rows` failed with exit 77 because no GPU is visible in
  this sandbox. This is not counted as a passing skip. No live pgrx tests were
  installed or run, and normal installed extension modules were not replaced.

Logs include initial lint/compilation failures and the successful checks after
their fixes. `core18-verified.log`, `core19-verified.log` and
`clippy{18,19}-verified.log` are the final cross-version results.

Native join implementation, header and 129-export ABI manifest remain unchanged
from the [bounded projection checkpoint](../resident-join-bounded-project-20261009/README.md).
Its six unresolved native audit findings remain open. This checkpoint neither
reruns that unchanged audit nor claims to resolve its findings. Installed module
SHA-256 values remain PG18
`e61a7518818eb95845ab4c3ee0381ef4e8832a1d7433b1303eb000bbb622a115`
and PG19
`6b59a8b1a4641e4399720f41ab4593eda8351d71b438a2db0f2d2522f88b172c`.

Still required: live GPU parity and cancellation/lifecycle tests, calibrated
DeviceLimits and performance measurements, planner permissions/shape/cost
admission, CustomScan integration, SQL output projection, grouping, broader
expressions and the remaining TODO.md work. SQL row joins remain native.
Planning documents remain in place. The source snapshot captures the dirty
worktree; SHA256SUMS records integrity, not release qualification.

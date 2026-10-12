# Uniform grouped execution budgeting — 2026-10-10

Removed the NUMERIC/scalar/hash-only allocation-charge predicate. Every Rust
grouped workspace and result buffer now reserves the execution ledger before
allocation, including ordinary dense integer states. Charges transfer with
pooled buffers and release on reclamation; active results remain owned.

Dense/spatial one-shot setup forecasts combined native scratch and full result
bytes before allocation. Bounded sessions forecast chunk-sized scratch plus
the eventual output before accumulation, after independently charged session
metadata is pinned. Forecasts only credit this backend's charged idle grouped
pools. They neither evict nor reserve: per-allocation reservations remain the
authoritative check if concurrent pressure changes. Hash forecasting retains
its existing physical-mode gate and shares the combined-budget validation.

## Validation scope

Final standalone suites pass 1,299 tests on each of PG18 and PG19. Production
and pg_test strict Clippy pass on both versions. Documentation parity passes
21 tests; safety-contract and scope audits, formatting and whitespace checks
pass. Scope remains 229 Rust files, 18 C++ sources, 36 CTests, 100 SQL files,
462 semantic assertions and 43 families; the coverage floor remains 90%.
The first strict-lint attempt rejected eight bare unwraps in the new host tests.
They now carry explicit expected-result messages; the failed lint log is kept.

Three new host tests cover:

- Exact charges for COUNT(*), COUNT(column), SUM/COUNT; one and four states;
  dense and compact layouts; capacities 1, 17 and 65,537. Pool checkout does
  not charge again, and clearing idle pools cannot release active ownership.
- Native metadata forecasts without pool/ledger effects; one-byte-short
  combined-budget rejection before bounded workspace allocation; exact-fit
  output/scratch reservation; exhausted-budget output/workspace rejection;
  forecasts remaining available with no free budget.
- Idle-output reclamation under pressure, no reclamation of active output,
  and exact ledger restoration after cleanup.

The new live PG fixture additionally requires real workspace allocation and
empty-input native finalization, verifies warm pool reuse, and rejects at zero
budget. It has not executed here. Missing GPU is a test failure, never a skip.
The pg_test ledger is process-local; it is not cross-backend qualification.

No native source, ABI, SQL admission or installed module changes are part of
this increment. No benchmark or performance claim is made. Full live SQL/GPU,
cross-backend and release qualification remain open, as do the broader TODOs.

Validation logs and source snapshot accompany this file; SHA256SUMS seals the
bundle. The snapshot excludes Git metadata, builds and evidence bundles.
The branch is main, but read-only `.git` prevents committing or pushing.

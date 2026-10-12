# Hash execution memory accounting and forecasting — 2026-10-10

Fixed a Rust accounting gap: grouped workspace/output charging previously
covered NUMERIC/scalar measures but not hash aggregation. Hash allocations now
reserve the execution ledger before allocation, including COUNT-only and cached
H3 execution. Idle pooled allocations retain their charges; checkout transfers
ownership without double charging; reclamation releases the exact charge.
Foreign-process charge handles report no locally reclaimable bytes.

`ResolvedGroupedAggPlan::hash_memory_requirements()` queries the native scratch
layout and host output footprint without buffer allocation, pool checkout,
reservation or device work. Cached H3 and fused/raw execution preflight the
combined workspace/output working set before those allocations. The read-only
forecast credits only locally owned, charged idle grouped buffers; raw inputs
and any FILTER mask are already charged. It does not evict pools or promise a
reservation. Per-allocation atomic reservations still enforce the budget under
concurrent pressure, reclaiming idle pools on reservation failure as before.

## Verification

- PG18 and PG19 standalone suites: 1,292 passed each.
- Production strict lint and pg_test compilation/lint: passed on both versions.
- Documentation parity: 21 passed. Safety contracts, scope, formatting and
  whitespace checks passed; coverage scope and 90% floor unchanged.
- Host tests verify exact nullable-key output footprints for COUNT(*),
  COUNT(column) and SUM/COUNT at capacities through 100,000; forecast pool/ledger
  neutrality; exact combined-budget boundaries; output reservation denial;
  workspace charges; pool retention/reuse/release; arithmetic failures; and
  foreign-owner byte reporting.
- A compiled live fixture checks pre-dispatch budget denial with no GPU calls,
  successful retry, learned-capacity warm reuse, and exact pool-charge release.
  It has not executed on this host. GPU/cross-backend qualification remains open.

The initial new metadata fixtures failed native validation because they used
zero comparison opcodes instead of canonical disabled-filter ALWAYS_TRUE.
Fixing the fixtures produced the passing rerun; the initial failure log is kept.
Native sources and ABI exports are unchanged. Earlier GPU execution failures
and seven native audit findings remain unresolved; no new native audit,
benchmark, GPU execution or speedup claim is made.

This change affects existing cached H3 budgeting too; it is not solely dormant
raw-key support. SQL shape admission remains unchanged. TODO.md records the
remaining qualification work.

Final logs and the initial fixture failure accompany this file.
`source-snapshot.tar.gz` contains implementation/documentation inputs, excluding
Git metadata, generated builds and evidence. `SHA256SUMS` seals the bundle.
The branch is main; read-only `.git` still prevents commits and pushes.

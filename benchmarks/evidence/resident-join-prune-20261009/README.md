# Consumer-driven resident join projection — live qualification pending

2026-10-09, PostgreSQL 18.4 and 19beta1. Optimizes the staged join/WHERE/reduction
pipeline; it does not enable normal SQL selection or establish a measured speedup.

## Implementation

Reduction execution now selects the union of declared measure and WHERE input
slots before gathering joined columns. A fixed-capacity metadata map preserves
the original JRS1 slots without copying programs or altering wire contracts.
Repeated references to the same occurrence/attribute/type share one gathered
lane. Different occurrences remain separate, even when self aliases share the
same physical storage, because their joined row positions can differ.

The existing binder still revalidates all prepared physical dependencies and
source types under its immutable borrow. Only its gather slice is narrowed;
interrupt retries rebind and apply the same selection again. Source loading,
dependency membership, join probing and pair production are not pruned here.

Constant-only programs use a zero-column projection retaining the successful
device producer's row count. This path makes no gather allocation or launch,
but retains its entry cancellation poll and the existing outer interrupt/owner
cleanup behavior. Aggregation still runs on-device. Ordinary projection and
the unchanged native gather ABI continue to require one or more columns.
Empty metadata does not fabricate rows or execute a CPU aggregate.

Results now report actual projected column count and allocated value/NULL-sidecar
bytes. The original live fixture schemas require 30 bytes per SEMI/ANTI output
row and 39 per INNER/LEFT row; their four-measure consumer needs just three
columns totaling 12 bytes per row. Most three-measure WHERE variants need
7 bytes per row; the DATE predicate needs 12. Constant-only consumers need zero.
These are exact layout calculations and pending live assertions, not timing or
measured GPU-memory claims. Join pairs and aggregate scratch remain separate.

## Verification

- Standalone core: **1,246 tests passed on each major**.
- Strict Clippy, all targets including live `pg_test` bodies: passed on both.
- Safety contracts, documentation parity (21 tests), formatting, diff check
  and coverage scope passed. Inventory remains 228 Rust files, 17 C++ sources,
  34 CTests, 100 SQL files and 462 assertions across 43 SQL families; no coverage
  threshold or release inventory change was needed.
- Portable tests exercise slot reorder without overwrite, invalid selection
  rejection before mutation, repeated-slot deduplication, alias separation,
  WHERE-only inputs, maximum bindings and zero-column cancellation/panic with
  no buffers, charge or device launches.
- Expanded live tests are **type-checked, not executed**. The 40 join shapes
  now check exact gathered bytes, constant SUM/COUNT and duplicate-column
  arithmetic against PostgreSQL, including distinct self-alias values. Direct
  row-domain tests add cancellation/panic and unchanged ledger assertions.

Core runs again report no visible Metal/SYCL GPU. There is no fresh native
runtime pass, live pgrx result, performance qualification or SQL support claim.
No native source/header/ABI change or installed-module replacement was needed.
The existing six static native export proof findings remain unresolved; no
audit was waived or weakened. See the preceding
[WHERE checkpoint](../resident-join-where-20261009/README.md) for the unchanged
qualification boundaries.

Live GPU parity/lifecycle/performance, planner admission, calibrated limits,
source-load pruning, avoiding unnecessary pair materialization, grouping and
the remaining TODO work are still open. Planning docs are retained. SHA256SUMS
seals the dirty-tree source snapshot and logs, not release qualification.

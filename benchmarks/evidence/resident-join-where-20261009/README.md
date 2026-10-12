# Resident join → WHERE → aggregate — live qualification pending

2026-10-09, PostgreSQL 18.4 and 19beta1. Extends the staged
[join reduction consumer](../resident-join-reduce-20261009/README.md), without
enabling SQL planner selection or claiming a runtime speedup.

## Implementation

JRS1 wire version 2 adds an optional BOOL post-join predicate. Version 1 still
decodes as unfiltered. Both forms validate every section and scalar program
before allocating owned metadata. Predicate inputs use the producer's exact
BOOL/INT4/DATE slots; malformed types, external predicate-mask context and
excess physical lanes fail before producer execution. Maximum wire size is
2,585 words, including the extra header field, with at most four physical lanes.

The consumer binds the existing parallel scalar-WHERE contract to an internal
BOOL carrier. Filtering occurs on-device, without a separate mask allocation
or host row evaluation. Only TRUE reaches aggregate expressions and independent
aggregate FILTERs. LEFT join padding precedes WHERE; this is not residual ON
support or predicate pushdown. Joined and selected row counts are distinct.
The internal carrier is not exposed as a requested aggregate state. WHERE uses
one physical lane, leaving up to three aggregate measures; unfiltered consumers
still support four. Existing charged metadata snapshots, bounded execution and
cleanup-before-PostgreSQL-error propagation remain in use.

## Checks

- Standalone core suites: **1,242 tests passed on each major**. New wire tests
  cover legacy/current versions, exact maximum filtered/unfiltered size,
  malformed trailing sections, types, predicate context and carrier capacity.
- Strict Clippy, including all `pg_test` bodies: passed on both majors.
- Safety contracts, documentation parity (21 tests), formatting, diff check
  and coverage scope passed. Scope remains 228 Rust files, 17 C++ sources,
  34 CTests, 100 SQL files and 462 semantic assertions across 43 families.
  No inventory or threshold change; this is not measured LCOV coverage.
- Live test expansion is **type-checked, not executed**: eight independent
  PostgreSQL WHERE oracles for each of the existing 40 join shapes, including
  nullable flags, right-side NULL tests, cross-side conditions, dates, CASE and
  constant TRUE/FALSE/NULL. Further cases check rejected-row division guards,
  predicate division errors, allocation cleanup and successful retry.

Core runs again report no visible Metal/SYCL GPU. No fresh native runtime pass,
live pgrx execution, SQL admission or speedup is claimed. Installed normal
extension modules were not changed. Native join implementation/header and the
129-export ABI manifest are unchanged; six static native export proof findings
remain open from the bounded-projection checkpoint. No audit exemption was added.

GPU qualification, real cancellation/termination, DeviceLimits calibration,
planner permissions/shape/cost admission, CustomScan lifecycle, grouping,
residual ON predicates, partitioning/spill and the rest of TODO.md remain open.
Planning documents are retained. SHA256SUMS seals these logs and the dirty-tree
source snapshot for reproducibility; it is not release qualification.

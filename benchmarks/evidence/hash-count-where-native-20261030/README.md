# Native partition-v2 WHERE COUNT foundation

This qualifies the additive native contract on this host, not SQL admission,
Rust ownership integration, PostgreSQL behavior, performance or a release.
Baseline: `0af1949e3ef5b1e4c3dd8802f3a5011c4164e0bc`.

## Scope

Partition-v2 adds direct SQL WHERE masks to adaptive integer/fused-H3 COUNT,
including optional aggregate FILTER. It publishes separate device-computed
WHERE and partition populations before FILTER. Existing v1 layouts, validation,
workspace sizes and execution paths remain unchanged. New storage is conditional
on v2; common kernel/completion layouts are not enlarged.

Validation covers controls and aliases, empty inputs, signed-prefix partitions,
full key/count association, capacity and semantic errors, both shared/device USM
publication, exact transfer accounting and no partial successful publication.
As with existing execution, callers must ignore outputs on any non-OK return;
transport failures do not promise rollback of copies already issued.
Empty v2 SQL masks retain their identity with canonical null pointers without
relaxing legacy validation. Invalid H3 rows rejected by WHERE remain unobserved;
accepted invalid H3 preserves its error instead of a secondary population error.

The two added exports remain explicitly unproved in the source-proof inventory.
No exemption, CPU fallback, threshold adjustment or planner expansion is included.

## Verified execution

- Final isolated build/metadata contract: **60,424 passed, zero failed**.
- Full native GPU hash suite: **10,367,205 passed, zero failed**.
- Standalone H3 suite: **1,176 passed, zero failed**, plus its deliberate
  no-device child with 23 passing checks. That child is not a skipped GPU suite.
- Independent review verified current source, build/GPU binary identities,
  harnesses and before/after seals. All v2 test suites execute unconditionally
  after successful initialization.
- Final-source focused inventory, real-ABI, unproved-export and forwarding
  audit tests: **11 passed**. Older audit2's eight tests cover an earlier source
  revision and are retained as such. Passing these tests does not close the
  explicitly unproved device/source obligations or pass the release gate.

Final source identities:

| File | SHA256 |
| --- | --- |
| `pgaccel_olap.h` | `583afd9286c7af56cb4fd22ebad39624a04b78bd44d6d14c7dca82f92ae5d8f0` |
| `grouped_agg.cpp` | `0248d571691899ba671b823c23e1c498b026b0f9792e75a2d28aa9de237b90cd` |
| `test_grouped_agg_hash.cpp` | `8276013bd06d72daf716ce31a4f4cbd6430d433f230af78bc79937af6cea840c` |

The final shared module is
`ffd86536480025d7ec1a5e019854ea7913f394e20139fa48741bf486173f2cc1`.
SDK/deployment-target warnings and cold-JIT diagnostics remain in the logs.
These are correctness results, not timings suitable for performance claims.

## Retained failures

1. Audit1 found four mutation subcase failures because a new branch duplicated
   text whose uniqueness the regression requires. The v2 branch now forwards its
   validated snapshot directly, preserving the original v1 forwarding guard.
2. Build-contract2 found one incorrect large-layout fixture. Capacity4097 was
   below the unchanged16384 threshold. The corrected fixture uses16385 and
   tests the intended alignment; the small unrounded-layout assertion remains.
3. GPU1 failed62 assertions:55 from incorrectly narrowed INT32 HASH output
   fixtures, four from noncanonical H3 fixture digits, and three from a real
   error-priority defect. The production fix applies population invariants only
   to successful attempts. Fixtures use the established INT64 output contract
   and canonical H3 values; all failed assertions remain in the tests.

Failed receipts and the exact GPU1 source snapshot are retained. Nothing here
claims that an earlier failed run passed. Runtime commands contain original
local paths; seals identify tested bytes, not a relocatable build environment.

## Remaining work

Rust ABI/ownership and population conservation across retries, default planner
admission/costing, PG18/19 correctness, fixed WHERE performance cells and unchanged
released controls remain required. Static device/source proofs, hosted CI,
other hardware/backends and full PostgreSQL regression qualification remain open.
The broader TODO and planning documents must not be removed for this milestone.

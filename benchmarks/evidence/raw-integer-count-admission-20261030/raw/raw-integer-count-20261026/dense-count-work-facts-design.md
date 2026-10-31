# Proposed additive dense COUNT work-facts ABI (not implemented)

Function: `pgaccel_grouped_agg_count_work_requirements_v1(const pgaccel_grouped_agg_desc*, pgaccel_grouped_agg_count_work_v1*)` returning existing `pgaccel_status`.

Output/input envelope:144bytes, alignment8. Four u32 fields at0/4/8/12:
`abi_version=1,size_bytes=144,reserved0=0,reserved1=0`. Sixteen u64 facts
follow in this order (offset16 through136):

1. `rows`
2. `group_capacity`
3. `row_counter_updates`
4. `global_counter_clear_entries`
5. `local_counter_init_entries`
6. `local_reduction_additions`
7. `partial_counter_writes`
8. `serial_reset_group_visits`
9. `serial_merge_group_visits`
10. `serial_partial_counter_reads`
11. `serial_finalize_group_visits`
12. `persistent_counter_lanes`
13. `key_columns`
14. `nullable_key_columns`
15. `value_null_checks`
16. `sql_mask_checks`

No prices, pointers, queue, allocation, device initialization, resident payload
read, or host aggregation. These are conservative full-population work facts
for ONE actual descriptor call, not observed selected-row counts. NULLs and
FALSE/NULL predicates never discount work at planning time.

Before generic validation, inspect only the fixed descriptor envelope and
bounded inline tags to reject pointer-rich scalar/NUMERIC, weighted and
dimension shapes. Then use existing `validate_desc`, `parallel_dense_count_shape`, `make_layout`,
`parallel_dense_count_column_shape` and the same topology constants/functions
as execution. Restrict to dimension-free, ordinary unweighted direct COUNTstar
or COUNTcolumn dense descriptors; reject scalar expressions, weighted counts,
other measures/modes and dimensions. Require the actual native kernel-mode
selector to return ParallelDenseCount, including its UINT32_MAX row-domain
guard (grouped_agg.cpp:5394 and5629), not merely the shape predicate.
Preserve existing native accepted mask
semantics; this query does not expand planner or execution eligibility.

Execution flags are the phase input. Native accepts only actual valid nonzero
flags, just like validate_desc (grouped_agg.cpp:1673). Rust uses its existing
workspace_query_descriptor (gpu/grouped_agg.rs:1320) to normalize canonical
zero flags into one-shot RESET|ACCUMULATE|FINALIZE BEFORE calling this query.
Explicit validated flags retain their actual phases. Data and tail queries must use their actual row counts;
FINALIZE uses zero rows. Rust sums RESET/ACCUMULATE/tail/FINALIZE facts from
the selected lifecycle, without duplicating8/1024/32 topology thresholds.

For m=1 COUNTstar or m=2 COUNTcolumn counter arrays, and actual layout
hierarchical group/chunk counts H:

- ACCUMULATE row_counter_updates=N*m (worst case both column counters update).
- ACCUMULATE value_null_checks=N for COUNTcolumn; sql_mask_checks=N when
  the native count specialization reads a SQL mask. No selectivity discount.
- ACCUMULATE global clears=m*(hierarchical?G*H:G), exactly the queue.fill lengths.
- Hierarchical local initialization=m*G*H*actual workgroup size.
- Hierarchical local reduction additions=m*G*H*(workgroup size-1).
- Hierarchical partial writes=m*G*H; nonhierarchical uses row atomics instead.
- RESET/ACCUMULATE/FINALIZE serial group visits are respectively G or0 byphase.
- ACCUMULATE serial partial reads=m*(hierarchical?G*H:G), counting distinct
  stored counters; COUNTstar selected-row sum shares its same count array.
- Persistent counter lanes come from actual layout count/nonnull lanes, not
  SQL type width; key/null-key counts come from validated actual key metadata.
- Empty ACCUMULATE still clears m*G global counters and performs G serial
  merge visits and m*G counter reads: grouped_agg.cpp:5567/5577 fills whenever
  ACCUMULATE, and commit at3260 is also phase-gated, not positive-row-gated.
  Only row updates/checks/local work/partial writes are zero. Empty FINALIZE
  without ACCUMULATE has no clears/merge reads, but retains G finalize visits;
  RESET retains its G visits. Tests must distinguish these exact cases.

All arithmetic checked before publication. Compute into a stack-local result,
publish only after full validation. Validate descriptor/output alignment,
version/size/reserved fields and output disjointness from both input metadata
and ALL declared fixed-width payload/null/mask/scratch ranges before success
publication. Range checking uses checked address/length arithmetic only, never
reads payload bytes. Overflow or unprovable range fails closed. To avoid
failure-path corruption before alias safety is known, ALL failures leave the
entire output unchanged; callers initialize zero facts and must reject a failed
status. No zeroing an aliased output and no partial estimates. This simpler
uniform no-write-on-failure contract supersedes the initial conditional-zero
proposal. Caller-readable metadata/output remains the ordinary C ABI premise.

Tests: header/layout pins, malformed envelopes/alignment/aliases with unchanged
input bytes, protected/unreadable payload sentinels, zero GPU call/ledger deltas,
COUNTstar and all supported fixed-width COUNTcolumn shapes, masks/NULL sidecars,
phaseflags,0/1/1023/1024/1025rows,7/8/9capacities and boundarytails. Cross-check
facts against the actual shared layout/topology; values in tests are boundary
assertions, not separately maintained planner policy. Weighted/dimension/scalar
and nonCOUNT rejection, overflow fail-closed, unchanged output on EVERY failure,
no partial publication. Include output aliasing values/null/mask buffers and
zero-row ACCUMULATE vs FINALIZE, plus UINT32_MAX+1 mode rejection.

Expected implementation files: pgaccel-kernels/include/pgaccel_olap.h,
src/grouped_agg.cpp,test/test_grouped_agg_hash.cpp;
pg_accel/src/engine/spec/abi.rs,gpu/{bridge,grouped_agg}.rs,
engine/executor/agg/descriptor.rs,planner_hooks/{generic_groupagg,shape/cost}.rs;
engine/cost/{device_limits,model}.rs and existing cost tests/validation as needed;
scripts/cpu_cheat_abi_manifest.txt. Native export inventory gains exactly1;
source-proof owner must rebind changed native bytes and new query remains
conservatively unproved until audited, not silently metadata-whitelisted.

Cost equation and calibration: preserve current raw hash coefficient and all
mask/expression/launch/output/load terms. Dense parallel row+counter work uses
existing reduction coefficient. A single new DeviceLimits-derived serial work
coefficient prices a frozen Rust combination of the exposed reset/merge/finalize
visits, persistent lanes, partial reads and key staging operations. Freeze that
combination and fitting procedure before measurements; do not fit distinct
clear/merge coefficients from collinear predictors. Nullable columns and mask
alias/distinct cases validate the full new COUNT envelope, not just controls.

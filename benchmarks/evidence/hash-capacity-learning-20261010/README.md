# Bounded raw-hash capacity and warm cardinality learning — 2026-10-10

Raw BIGINT hash aggregation no longer requires a group slot for every input
row. Initial capacity is min(max(1, rows), configured groups, 2^30), with the
native UINT32_MAX row limit preserved. Capacity is a bounded allocation budget,
not a promised distinct count: native insertion/finalization rejects overflow
before output publication. No CPU distinct scan, fallback, automatic unbounded
growth or SQL admission change was added.

After successful compact validation and output setup, the query-owned raw-key
artifact records the exact group count for warm workspace sizing. Its existing
dependency stamps require fresh metadata on generation changes. Invalid counts
do not mutate the learned bound; failed/interrupted attempts do not publish one.
The interrupt wrapper also restores the prior bound if PostgreSQL rejects a
completed result during post-dispatch interrupt service. It retains only a
scalar across service, never a metadata/store borrow. Fused H3 behavior is
unchanged; cached H3 shares the renamed existing bound-validation helper.

## Verification

- PG18 and PG19 standalone suites: 1,289 passed each.
- Production strict lint and pg_test compilation/lint: passed for both versions.
- Documentation parity: 21 passed. Safety contracts, coverage scope, formatting
  and whitespace checks passed; scope and 90% floor unchanged.
- Host checks cover bounded capacity through UINT32_MAX rows, learned-bound
  validation, failed updates, fresh-owner reset and unchanged fused H3 behavior.
- The new live fixture compiles but has not executed here. It compares
  PostgreSQL SUM/COUNT results for 1,000,017 rows and eight keys using a 16-group
  budget; checks learning to eight; forces failure with a four-group budget;
  retries; inserts a ninth key; collapses keys in a rolled-back subtransaction;
  and truncates. It also injects real PG cancellation after completed output
  validation to verify restoration of the initial bound before rethrow.

Native sources are unchanged. Prior GPU execution failures and seven native
audit failures remain unresolved. No new benchmark, native audit, GPU/live run,
speedup or end-to-end memory measurement is claimed. Qualification and planner
admission remain open in TODO.md.

Raw final logs accompany this file. `source-snapshot.tar.gz` contains
implementation/documentation inputs, excluding Git metadata, generated builds
and evidence; `SHA256SUMS` seals this bundle. The branch remains main, but
read-only `.git` prevents commits and pushes in this session.

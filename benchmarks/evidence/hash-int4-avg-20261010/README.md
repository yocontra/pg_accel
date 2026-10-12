# Staged INT4 hash SUM/AVG output subsets — 2026-10-10

Cached and fused Rust bindings now accept every nonempty, duplicate-free subset
of INT4 SUM, AVG and COUNT, in any order. SUM-only and AVG-only queries retain
native SUM|COUNT internally without exposing an extra SQL output. COUNT-only
queries retain their existing specialized kernel. Other types, duplicate or
unsupported outputs, joins, WHERE and invalid FILTER shapes remain rejected.
Normal SQL admission is unchanged; this is not released or GPU-qualified support.

Existing output validation checks exact sum widths, count/validity agreement
and INT4 sum bounds. Existing materialization computes AVG from finalized exact
sum/count through PostgreSQL NUMERIC division and handles zero-count SQL NULLs.
No resident input aggregation or CPU execution fallback was added.

The new host test exercises all output subsets and permutations, both FILTER
states and both cached/fused bindings. It verifies native lane selection and
keeps SQL admission closed. A real-catalog pg_test fixture compares exact text
(including NUMERIC scale) to native PostgreSQL, with signed extremes, ±1/3,
NULL keys, all-NULL measures, nullable filters, warm reuse and rollback. The
fixture covers all seven output subsets under both execution policies.

## Verification

- PG18 and PG19 standalone suites: 1,285 passed each.
- Production strict lint and pg_test fixture compilation/lint: passed on both
  PostgreSQL configurations.
- Documentation parity: 21 passed. Safety contracts, scope audit, formatting
  and whitespace checks passed; coverage scope/floor unchanged.
- Live PostgreSQL/GPU execution and performance: not qualified on this host.
  Native sources are unchanged from `hash-int4-sum-20261010`, whose GPU failures
  and seven unresolved native audit exports remain open. No gate was waived.

An initial host-test build hit a Rust borrow-check error in duplicate-output
test construction; storing the first output before appending fixed it. The
initial failure and successful rerun logs are both retained.

`source-snapshot.tar.gz` contains implementation/documentation inputs, excluding
Git metadata, generated builds and evidence. `SHA256SUMS` seals this bundle.
No benchmark or new native audit ran for this Rust-only change. The TODO remains
unchecked. The branch is main; read-only `.git` still prevents commits/pushes.

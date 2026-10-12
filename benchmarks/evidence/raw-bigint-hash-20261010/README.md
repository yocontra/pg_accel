# Staged direct BIGINT hash aggregation — 2026-10-10

The Rust descriptor executor now has a query-owned RawHash path for one ordinary
BIGINT fact key. It binds the resident values and canonical NULL sidecar directly
to the native INT64 hash lane. No H3 extension, parent transform, key copy,
dictionary construction or host distinct scan is involved.

The path shares exact COUNT and INT4 SUM/AVG states, optional Boolean FILTER,
workspace/output accounting, compact validation and materialization with the
existing hash implementation. Raw-key dependencies are explicitly included in
the dispatch borrow; ordinary dictionary dispatch dependency pruning is unchanged.
Generation changes rebuild the ephemeral metadata. FILTER masks remain
query-local and cancellation is delivered after residency/metadata owners unwind.

The worst-case capacity is max(1, rows), rejected if it exceeds the configured
or native limit. There is no estimated distinct bound or CPU fallback. Native
unsigned key order preserves BIGINT signed bits and is not an SQL ORDER BY
guarantee. Compact decoding checks the key type/shape and both signed extremes.

Normal SQL planning still selects its established dictionary route. Only the
pg_test qualification entry requests this new physical path. Planner/cost
admission, GPU/live correctness and performance qualification remain open.

## Verification

- PG18 and PG19 standalone suites: 1,288 passed each. New host checks cover raw
  binding/dependencies, transform rejection, cancellation polling, capacity
  bounds, native shape selection and signed-key decoding.
- Production strict lint and pg_test fixture compilation/lint: passed on both
  PostgreSQL configurations.
- Documentation parity: 21 passed. Safety contracts, scope audit, formatting
  and whitespace checks passed. Coverage scope/floor unchanged.
- A real-catalog live fixture (compiled, not executed here) creates 16,388
  BIGINT/NULL groups, checks native PostgreSQL SUM/COUNT parity with FILTER,
  warm reuse, key updates, rollback and every fused cancellation boundary.
  It requires a GPU and does not install or resolve H3.

Native sources are unchanged from the preceding SUM implementation. This host
still lacks GPU execution evidence, and the prior seven native audit failures
remain unresolved. No new native audit, benchmark or live PostgreSQL execution
is claimed, and no gate was waived.

Retained initial failures: a String conversion compile error; a test whose H3
artifact resolution disagreed with its specification (the new binder rejects
that mismatch); a missing raw key in dispatch dependencies, fixed by the
compact-hash dependency builder; and one strict-lint semicolon issue. Successful
final reruns are retained alongside these logs.

`source-snapshot.tar.gz` contains implementation/documentation inputs, excluding
Git metadata, generated builds and evidence. `SHA256SUMS` seals the bundle.
The TODO remains open. The branch is main; read-only `.git` prevents commits
and pushes in this session.

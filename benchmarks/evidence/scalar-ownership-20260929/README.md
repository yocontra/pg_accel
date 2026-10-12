# Owned Rust SPG1 bindings and live fused-aggregate parity

This connects the native fused evaluator to the Rust grouped-aggregate facade.
It does not yet admit these expressions through the planner or executor's
logical-spec descriptor builder.

Unsafe plan/chunk adoption now validates the complete SPG1 program and its
typed bindings, then owns separate snapshots of the host view, words and input
descriptors. Caller metadata may be changed or dropped after adoption. Resident
payloads remain borrowed under the constructor's input lifetime; the facade
does not read resident rows on the host. Invalid view/op combinations, scalar
RHS views and noncanonical inactive scalar metadata are rejected before pointer
traversal. Length, alignment, NULL, input/result type and lane constraints are
checked before constructing a usable owner.

Bounded row chunks copy and advance nested INT32/BOOL/NULL pointers, never the
host view pointer. Zero-length end slices remain valid. Sessions independently
snapshot scalar metadata, so a plan may be dropped and re-resolved between
accumulation calls. Full program contents participate in shape equality and
workspace-pool keys; matching sizes alone cannot permit changed expressions.
The existing NUMERIC row-chunk path now retains its first metadata snapshot
instead of copying it again through unsafe chunk adoption.

## Verification

- Rust core: 1,170 tests pass. New tests cover metadata independence, INT32 and
  Boolean byte strides, NULL sidecars, empty/end/out-of-range chunks, plan-drop
  session snapshots, changed programs/pool identity, malformed headers and
  programs, binding type/length/overflow rejection, inactive-pointer rejection,
  and Boolean COUNT/result-type constraints.
- One live test passes on each PG18.4 and PG19beta1. It lowers real analyzed
  PostgreSQL expressions, binds uploaded resident inputs through the new owner,
  asserts `ParallelDenseInteger` execution, and compares all non-NULL aggregate
  lanes and counts with native PostgreSQL over 4,097 rows. Three expressions
  cover searched CASE/COALESCE, nested arithmetic, nullable Boolean conditions
  and guarded division. One/three/seventeen groups exercise hierarchical,
  ordered-partial and atomic shapes. Each case repeats in 113-row chunks with
  the original plan dropped and re-resolved, then finalizes/resets the session.
  Unguarded division inside COUNT returns the typed division-by-zero error.
- Both-major Clippy with `pg_test`, formatting and documentation checks pass.
  Scope remains 222 owned Rust files, 33 native tests and 90 SQL files/418
  assertions. The preceding milestone's 33/33 native suite remains separate
  evidence; this milestone did not alter native source or rerun that suite.

The live tests keep `pg_accel.enabled=off` for their SQL oracle and dispatch
the GPU facade directly. They are not selected Custom Scan SQL tests. No CPU
fallback, force-admission setting, timeout or planner threshold was added.
The initial Clippy log is retained: test `unwrap`/implicit raw-pointer-borrow
lint failures were corrected without suppressing those lints.

## Remaining integration

`BoundDescriptor` still rejects Bytecode measures. Executor/resident binding,
program/input memory charges and currentness checks, planner/cost admission,
selected SQL error/result parity and measured performance remain unfinished.
The host program/binding snapshots are bounded but not yet ledger-charged or
cached/shared across chunks. Neither a speedup nor general SQL expression
support is claimed. Prior full-SQL NUMERIC timing warnings remain unresolved;
focused reruns cannot replace or explain that evidence.

Verification logs and the source snapshot are retained alongside this note.

Normal non-`pg_test` extension builds were restored, PG19 then PG18:

- PG18 module SHA-256: `09bac2fe85381656b02b27a1951666ae42b4de610ef12aa998153af90f8ccda5`.
- PG19 module SHA-256: `106d5f89fa441397fe4d892d6c7c376b64368ffc263e03cf7d9ef56103a7e2ce`.
- Source snapshot SHA-256: `2963ad46eb8a3668ad259c1681d09df1fd6f190ea52709b09ba3d06f4065caac`.

On each restored build, isolated SQL117 (NUMERIC), SQL126 (computed integer and
geo combinations) and all five extension-catalog lifecycle phases pass without
warnings. These are focused regressions, not a fresh full SQL/upstream run.
The initial catalog invocation tried to execute a non-executable script; its
permission-error log is retained. Invoking the existing script with `bash`
completed the checks without changing file permissions or test requirements.

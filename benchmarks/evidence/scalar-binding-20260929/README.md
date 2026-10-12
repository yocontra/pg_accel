# Executor SPG1 resident binding — runtime admission still disabled

The production descriptor builder now binds recognized SPG1 measure programs
to resident input views. This replaces the missing Bytecode binding branch;
it does not enable the runtime capability backstop or planner admission.

Binding independently decodes the complete program, verifies exact INT4/BOOL
input and result OIDs, fact relation/attribute identities and supported output
lanes, then resolves inputs by identity rather than request order. Unknown
opaque programs are never executable just because the logical envelope accepts
them. Missing/duplicate requests, mismatched view bundles, wrong resident OIDs,
row counts and NULL-sidecar lengths fail before dispatch. INT2 is not silently
widened into the SPG1 INT4 language. Boolean results support COUNT only.

`BoundDescriptor` owns each scalar view, word array and input descriptor array
until the GPU facade snapshots them. It never retains a borrowed pointer into
the logical spec's program storage. The surrounding resolver still owns the
resident payload. Independent exact INT4 aggregate predicates bind through
RHS, including NULL-accepting predicates. The Rust physical workspace-shape
classifier now mirrors native scalar dense states without dereferencing their
host metadata, preserving bounded-partial sizing for subsequent integration.

## Evidence

- Rust core: 1,174 tests pass. Four added tests exercise the real descriptor
  builder with one to four scalar states, independent predicates, owned view
  lifetimes, exact/empty INT4 and BOOL bindings, literal NULL/Boolean COUNT,
  malformed programs/OIDs/attributes/request bundles and explicit continued
  rejection by runtime and normal planner admission.
- The live PG18/PG19 test now uses the production scalar binder for its
  uploaded resident BOOL/INT4 inputs, instead of relying on hand-written ABI
  input pointers. Native PostgreSQL aggregate parity passes over 4,097 rows,
  three expressions and one/three/seventeen groups, both one-shot and in
  113-row chunks across plan re-resolution. An undersized resident NULL
  sidecar is rejected before execution. COUNT division-by-zero also passes
  through the production binder and returns the distinct GPU error.
- Both-major Clippy with `pg_test`, formatting, documentation parity (21 parser
  tests) and scope audit pass. The inventories remain 222 owned Rust files,
  33 native tests and 90 SQL files/418 assertions. No native source changed.

The live test calls the GPU facade directly while SQL oracle execution has
acceleration disabled. It does not prove selected Custom Scan support,
resident-store invalidation, ledger accounting, performance or planner costs.
No force-admission switch or host evaluation/fallback was added. Existing
runtime/normal capability rejection is tested explicitly so unaccounted
program metadata cannot silently become a selected SQL execution path.

Initial compile/lint logs are retained: request deduplication needed the numeric
OID value because `pg_sys::Oid` is not ordered, and redundant `pub(crate)` inside
the private module was corrected without suppressing lints.

## Next required integration

Before enabling capability, charge program/view/binding snapshots and temporary
validation memory correctly across binding, plan, session and chunk lifetimes;
prove dependency/currentness behavior through the resident resolver; introduce
an accurate scalar specialization/cost model; connect planner lowering; and run
selected SQL result/error parity plus performance qualification. Native
workspace sizing already includes staged program metadata, but it does not
account for these host owners. Earlier full-SQL NUMERIC timing warnings remain
unresolved and are not superseded by this milestone.

Normal production modules were restored (PG19 then PG18):

- PG18 SHA-256: `d863fda7a55a5f3a1335b105c752ffb12f55a8728181ac2517e7e41fe8939088`.
- PG19 SHA-256: `858a806f2c3ba197803532129678c420168a66cd0312d9d7d77137a240c36e75`.

SQL126's computed-integer/geo contract passes on both restored builds without
warnings. This is a focused regression, not a new full SQL or upstream run.

Source snapshot SHA-256:
`aa36432247b9458fdcd7b60a720334b5b5d78d68f31ded7cf80fd6d513d0e76b`.

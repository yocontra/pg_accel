# SPG1 fused native aggregation — SQL admission remains disabled

SPG1 programs now execute inside the native dense integer aggregation kernels,
after WHERE and each qualified aggregate FILTER. This connects the earlier
standalone device evaluator to real SUM/MIN/MAX/COUNT states without allocating
an intermediate expression column or evaluating rows on the host. Dedicated
scalar template variants keep interpreter code out of legacy arithmetic kernels.

The additive C operation is SCALAR (6), with an explicitly versioned host view
flag (2). Outer ABI version, sizes, offsets and existing discriminants are
unchanged. The view carries SPG1 words and typed resident BOOL/INT4 bindings.
Native validation proves the full control-flow graph, including inactive paths,
before staging words/input descriptors into an explicitly sized workspace
segment. Rust and C++ assert the nested view layouts. This is not yet integrated
with Rust execution-memory charges or resident dependency/currentness proofs.

Programs preserve lazy searched CASE/COALESCE/Boolean semantics, SQL NULLs and
checked INT4 operations. COUNT evaluates the program too; its top-level view
having no NULL sidecar cannot imply a non-NULL result. Division by zero receives
a distinct native detail, Rust error and SQLSTATE mapping (22012); overflow
retains 22003. These mappings are not evidence of selected SQL execution.
Failures do not publish result lanes and require lifecycle RESET for recovery.
No ordering claim is made for different errors across concurrent input rows.

Supported native shapes are dense integer states, optional final COUNT(*),
SQL WHERE masks and the qualified raw INT4 range filters, including independent
FILTER_INPUT RHS. Per-measure SQL masks, HASH, dimensions and descriptors beyond
the parallel partial-workspace bound are rejected. Scalar descriptors cannot
fall through to generic serial execution.

## Verification

- Final native suite: 33/33 tests pass, including 48,900 grouped-aggregate
  checks. This is correctness evidence, not a timed performance comparison.
- The shared 108-case semantic corpus passes the new full CFG validator;
  seven malformed CFG cases fail with no proof published. The existing 759
  scalar evaluator host/device cases continue to pass.
- Native grouped tests cover fused CASE/COALESCE with nullable INT4 and Boolean
  resident inputs; SUM/MIN/MAX/COUNT; one, three and seventeen groups; 4,097-row
  inputs; atomic/hierarchical/ordered-partial paths; advanced chunk inputs;
  zero-row finalization; literal/all-NULL/Boolean counts; lazy division errors;
  overflow versus division-by-zero details; program mutation; maximum 64-input,
  four-state descriptors; owned device-USM workspace staging; malformed metadata,
  non-USM and noncanonical payload rejection; alias rejection; RESET recovery;
  empty global output; unsupported dimensions and partial-workspace limits.
- Rust core: 1,166 tests pass. Tests pin new view layouts, typed error mappings,
  and fail-closed facade rejection. PG18/PG19 Clippy with `pg_test` passes.
- Documentation parity (including 21 parser tests), formatting and scope audit
  pass. Inventory remains 222 Rust files, 33 native tests and 90 SQL files with
  418 semantic assertions. No SQL or upstream suite was run for this milestone.

Logs are retained in `verification/`, including the first failed focused run.
Its 18 failures were fixture expectations: MIN/MAX compared on an all-NULL
group, per-measure SQL masks expected to execute despite being outside the
qualified grammar, and malformed descriptors expected to return the wrong
status. Corrections preserve the narrower native admission contract. The full
suite and final native totals are recorded in the retained logs.

## Still unfinished

The safe Rust grouped facade rejects scalar operations/view flags before
reading nested metadata. Its existing row slicer treats ordinary VALUE pointers
as columns, which is not valid for a host scalar view. Owning/copying/slicing
program metadata, checked runtime bindings, exact budget and currentness proofs,
planner/cost admission and selected SQL parity/performance remain required.
The native view must not be enabled there just by removing the guard.

No new SQL support, measured speedup, cold/concurrent performance, CUDA coverage,
installation qualification or release readiness is claimed. Production extension
installations were not overwritten in this milestone. Prior full-SQL NUMERIC
timing warnings remain unresolved and preserved in earlier evidence.

Source snapshot SHA-256:
`d8d2b4178f682685ac7061eb04ab7e3c66b5f2066bc1a7fb19f25bd0de6eca53`.

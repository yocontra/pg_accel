# Native COUNT foundation: isolated main compatibility

This evidence qualifies a native-only change against unchanged main Rust at
`f1e0241b8befa6e0e9c1b310c000e3d0ed6c26eb`, plus the six-file `source.patch`.
It does **not** qualify the unfinished raw-integer planner admission or dense
cost-model changes in the development checkout.

## Changes

- Validated partition-v1 COUNT calls already using the large-capacity parallel
  branch keep parallel finalization for small emitted populations instead of
  falling back to a serial device heap sort. Legacy calls and the genuinely
  small-capacity branch are unchanged.
- Raw partition metadata accepts INT32 as well as INT64 keys using the existing
  sign-extending load. H3 retains its INT64 validation requirement.
- The additive, versioned `pgaccel_grouped_agg_count_work_requirements_v1` API
  reports dense COUNT work metadata without reading payloads or dispatching a
  device call. Existing ABI layouts and function signatures are unchanged.
- The immutable source-ABI inventory pins acknowledge 134 entries. The new
  query remains explicitly **unproved** by the source audit; it receives no
  whitelist or lifecycle exemption. This inventory is not a dynamic-library
  export count.

## Results

| Check | Result |
|---|---|
| Native metadata contracts | 60,310 passed, 0 failed |
| Native hash suite | 10,366,112 passed, 0 failed |
| Native H3 suite | 1,176 passed, 0 failed |
| Old-main Rust release build/install | PG18 and PG19 passed |
| Live H3 COUNT integration | PG18 and PG19 passed |
| Actual dense result materialization | GPU dispatch, no stock fallback, exact 65-group native parity on both majors |
| ABI inventory/proof regressions | 4 passed; new query stays unproved |

`qualification1/` contains the build, native, PostgreSQL, source, toolchain and
protected-artifact receipts. Its original dense smoke verified an accelerated
plan but did not separately count the result-producing CTAS. The independently
sealed `dense-materialization1/` follow-up closes that gap: it brackets the
accelerated CTAS with GPU/stock counters, brackets the disabled native CTAS with
zero counters, and checks both directions of `EXCEPT ALL` and cardinality.
Both runs ended successfully, with all owned PostgreSQL clusters stopped and
source/module/toolchain/harness checks intact.

`completion.json` binds the qualified module and source identities. `harness/`
preserves the exact bounded host-specific commands, including their original
absolute paths. Reproduction requires the pinned local toolchain and equivalent
isolated paths; these are evidence scripts, not a portable installer.
`SHA256SUMS` seals the evidence files, excluding itself.

## Limits

Validation is local Apple Silicon Metal, not CUDA qualification. No performance
benchmark was run for this isolated old-Rust/new-native combination. Historical
diagnostics from the broader development checkout are not a release headline
for this slice.

The broader raw-count warm matrix passed, but two dense old/new no-slowdown
controls failed; its planner and cost changes remain unshipped. Full upstream
compatibility, hosted CI, source-audit and release gates remain open. This
milestone is not full release qualification or completion of `TODO.md`.

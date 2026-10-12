# Indexed output escape regression

2026-10-10. This checkpoint repairs a false approval in the static device-work
audit. It does not add SQL support or improve runtime performance.

## Defect and change

A compiler-valid witness dispatched a kernel writing an ABI output, waited,
stored the output pointer in a global array, then called an argument-free host
helper which overwrote the result. The analyzer previously approved the export:
its local alias tracking did not follow indexed container contents.

The audit now rejects host indexed assignments and array initializers containing
ABI-output-derived state with `output_container_escape`. This is deliberately
conservative, including scalar/aggregate copies, because container contents are
not modeled. The finding is fatal on success and failure-only paths. Stores
inside recognized device lambdas retain the existing device provenance checks.

The parser also distinguishes `int* slots[1]{out}` from a lambda capture/body;
previously array initializers could hide host effects under the lambda rule.
No workspace-helper approval, export exemption or device-proof waiver was added.

The compiler-valid tests cover global/member arrays, casts, pointer arithmetic,
local aliases, host lambdas, copy/direct initializers and nested arrays. Positive
controls retain unrelated host arrays and actual device-lambda stores. These
fixtures use a compile-only SYCL stand-in: they test the analyzer, not GPU runtime.
Both the local audit recipe and the no-GPU CI configuration run the new suite.

## Verification

- All 180 existing analyzer tests pass, including the production-source
  witnesses and 130-export ABI inventory checks.
- All four new indexed-escape tests pass (multiple compiler-valid subcases).
- All 46 typed metadata, build-binding and link-binding tests pass.
- Safety contracts, 21 documentation-parity tests, new-test lint/format and
  `git diff --check` pass. Coverage scope remains unchanged at 228 Rust files,
  18 C++ sources, 35 CTests, 100 SQL files, 462 assertions and 43 families,
  with a 90% threshold.

Logs and the source snapshot are sealed in SHA256SUMS. CI wiring was changed;
no hosted CI run is claimed.

## Scope and remaining work

The accompanying helper diagnostic identifies existing grouped-aggregate gaps:
scalar-program staging loops, workspace-pointer helpers, specialization dispatch
and output lineage. They are not resolved here. The prior complete release
checkpoint still records five unresolved join exports and two grouped-aggregate
exports; this checkpoint does not claim a new complete release audit.

No Rust/native runtime code was changed for this repair, and PostgreSQL/native
runtime tests were not rerun. There is no GPU-runtime or performance claim.
Main remains uncommitted: this session cannot write `.git` or push a commit.

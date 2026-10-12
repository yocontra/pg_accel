# Workspace address helper body proofs

2026-10-10. A typed, source-only proof now separates workspace address formation
from device work. It does not approve exports in the release analyzer.

## What is proved

`scripts/workspace_effects.py` preprocesses the actual translation unit, then
passes that expanded text to Clang for compiler-resolved, filtered JSON nodes.
The second pass does not reopen mutable project headers. The result records the
expanded-source SHA-256, not a linked-object attestation.

Each selected helper definition must have an explicit object-pointer return,
exactly one void-pointer parameter and one builtin integral offset, and exactly
one return statement. The expression walker admits base-derived pointer casts
and addition, integral comparisons, and an optional null arm. Compiler-marked
non-ODR constant uses can supply a sentinel; ordinary const, volatile and
dynamically initialized global/TLS loads cannot. Unknown AST forms fail closed.

Calls, row dereferences, writes, integer-to-pointer reconstruction, class
conversions, local lifecycle work, defaults, attributes and side-effecting
specializations are rejected. This is an effects/provenance check, not proof
of offset bounds, alignment, lifetime, arithmetic validity or memory ownership.

The canonical Clang source view passes for both actual helpers:

- `grouped_agg.cpp`: `arena_ptr`, 11 definitions/instantiations, nullable result.
- `join_rows.cpp`: `at`, six definitions/instantiations, non-null arm only.

These counts describe the filtered source view, not every possible future
specialization. A result does not establish overload/call-site identity.

## Reproduce the real-source checks

From the repository root (with the existing local AdaptiveCpp headers):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/workspace_effects.py \
  --source pgaccel-kernels/src/grouped_agg.cpp --helper arena_ptr \
  --filter '(anonymous namespace)::arena_ptr' \
  --include pgaccel-kernels/include \
  --include .pgaccel/acpp/current/include \
  --include .pgaccel/acpp/current/include/AdaptiveCpp

PYTHONDONTWRITEBYTECODE=1 python3 scripts/workspace_effects.py \
  --source pgaccel-kernels/src/join_rows.cpp --helper at \
  --filter '(anonymous namespace)::at' \
  --include pgaccel-kernels/include \
  --include .pgaccel/acpp/current/include \
  --include .pgaccel/acpp/current/include/AdaptiveCpp
```

## Integration boundary

The checker is intentionally not imported by `cpu_cheat_audit.py`. A compiler-
valid regression demonstrates a safe helper called with a class conversion
that overwrites the ABI output: the helper BODY proof passes, but does not
justify approving that caller. Returned-pointer aliases must also remain
visible to later host-write and container-escape checks.

Before this proof can influence release approval, bind it to the audit's exact
compiler/preprocessor/build inputs and the selected call targets, prove argument
evaluation, and propagate returned pointer provenance. The current source-only
profile is not the production AdaptiveCpp object-generation profile. Neither
helper proof supplies device-work evidence or relaxes the seven unresolved
grouped/join export findings from the prior release checkpoint.

## Verification

Ten new compiler-backed regression tests pass, along with the 46 existing
metadata-effect/build/link tests, four indexed-escape tests, five release-wiring
tests and 21 documentation-parity tests. Lint, format, safety contracts, coverage
scope and `git diff --check` pass. The unchanged scope still gates 228 Rust files,
18 C++ sources, 35 CTests, 100 SQL files, 462 assertions and 43 families at 90%.
Both the local audit recipe and no-GPU CI configuration run the new tests; no
hosted CI execution is claimed. The core analyzer implementation did not change
in this checkpoint, and its full 180-test suite was not rerun here.

`grouped-proof.json` and `join-proof.json` contain the real-source results. The
logs and source snapshot are sealed by SHA256SUMS; the snapshot is working-tree
evidence, not a commit or release qualification.

No runtime source changed, no native/PostgreSQL/GPU runtime test was rerun, and
there is no performance claim. Git remains read-only, so changes are not
committed or pushed. The overall TODO objective remains incomplete.

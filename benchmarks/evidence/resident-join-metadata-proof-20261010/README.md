# Typed metadata-effect verifier — release integration pending

2026-10-10 UTC. This checkpoint implements a conservative Clang-AST verifier
for the join-query metadata boundary. It is **not** connected to the production
CPU-cheat audit's approval path and does not qualify SQL joins or GPU execution.

## Implementation

`scripts/metadata_effects.py` follows compiler-resolved declarations through the
complete helper call graph. Root descriptor objects are read-only; the single
uint64 output is write-only; local plain records have bounded storage. Pointers
loaded from descriptor fields remain opaque even if their pointee type has the
same spelling as a metadata record. Unknown calls/AST forms, callback invocation,
pointer reconstruction, nonlocal writes and user-defined object lifecycle fail
closed. The narrow memcmp case requires exact fixed metadata objects and rejects
linkage attributes that can redirect the call.

Loops require a zero-based, fixed-limit induction variable that cannot be
modified through a helper or alias. Total analyzed work, array extents, nested
record cells and call depth have independent bounds. Potential success returns
require scalar output publication; a failure guard cannot be reused after its
status is reassigned. A verifier instance cannot reuse another export's state.
The canonical compiler driver rejects missing returns and all VLA extensions.
Raw AST JSON is not accepted as proof input: Clang omits evaluated parameter
array bounds from that representation. A compiled probe confirms such a bound
really calls its function at helper entry. C linkage is checked structurally,
not by assuming platform-independent symbol prefixes.

The native layout helpers now use a builtin fixed Level array, a bounded hash
capacity loop and a four-level prefix loop. Repeated key alias validation is a
shared fixed-metadata helper. Explicit exhaustion checks fail closed. The query
ABI and supported workspace byte forecasts are unchanged; no row computation,
device pointer serialization, hidden allocation or CPU join fallback was added.

## Evidence and limits

- **19 verifier tests pass**, including compiler-valid adversarial fixtures and
  mutations in the real transitive helper source. Both real query bodies pass
  the staged typed effect check with loop limits **4, 32, 4, 4**.
- The matrix covers row reads/writes, descriptor-shaped row pointers, callbacks,
  unknown/recursive helpers, scalar pointer round-trips, output overruns, stale
  guards, counter aliases, VLA sizeof side effects, object lifecycle, oversized
  metadata, total work, globals/statics and memcmp substitution.
- The verifier tests are wired into `just audit-cpu-cheats-test` and the hosted
  no-GPU CI job. CI configuration changes are not evidence of a hosted run.
- Native build and the metadata host CTest pass. The **40,389-check** protected
  memory matrix also passes under ASan/UBSan and against the rebuilt shared
  library. PG18 and PG19 core suites each pass **1,249 tests**.
- Python formatting/lint, Rust formatting, safety contracts and coverage scope
  pass. Scope remains 228 Rust files, 18 C++ sources, 35 CTests, 100 SQL files and
  462 assertions across 43 semantic families, at the unchanged 90% threshold.
- The existing analyzer's **180 tests pass**. The actual production join audit
  still **fails on seven exports**; the staged verifier has approved none of
  them in that audit. ABI inventory remains 130 exports, unchanged.
- The GPU join CTest again **fails with exit 77** because no GPU is visible.
  This is not a passing skip or a runtime join qualification. Installed PG18/19
  module hashes are unchanged.

The effect checker is specific to the translation unit Clang saw. A regression
explicitly compiles both sides of a conditional helper: its metadata branch
passes and its row-reading branch fails. Therefore source/header dependencies,
preprocessing configuration and actual audited build evidence must be bound
before using these results to approve a public export. No metadata/lifecycle
exemption was added to the release audit. The check also does not prove integer
arithmetic, address-span validation, alias rejection or general C++ correctness;
those contracts retain their runtime and ABI tests.

The full TODO objective remains open. No extension module was installed and no
server was restarted. This is an uncommitted working-tree checkpoint because
`.git` is read-only; it is not a pushed change or release certification.

The initial symbol-prefix check rejected Darwin's C symbols; the retained
development failure log records that mistake. The final
`metadata-tests-profile-bound.log` passes after structural linkage checking and
compiler-profile binding. `source-snapshot.tar.gz` and `SHA256SUMS` record this
checkpoint's integrity, not a release attestation.

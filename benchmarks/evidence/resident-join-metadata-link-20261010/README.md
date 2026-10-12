# Linked metadata proof integration

2026-10-10 UTC. This checkpoint connects the typed metadata checker to the
release audit for the two join workspace-query exports. It does not approve
device execution, admit new SQL plans, or qualify GPU runtime behavior.

## Binding and proof boundary

CMake emits a linker map for the actual shared-library link. A post-link check
requires both query exports to be strong text definitions at the map's recorded
addresses and to belong to the proved metadata object. It records hashes of
the library, map, object, build receipt and link-checker implementation. Changed
artifacts during ownership validation fail closed.

The release audit first requires its complete source/header/compiler/object ABI
inventory and current rebuild marker to pass. It then independently reruns the
typed proof and compiles another metadata object from the frozen current inputs.
Those object bytes must exactly match the linked input. It rechecks actual
library symbol addresses and map ownership, source/header/compiler/flag freshness,
artifact hashes, expected build paths and current-link timestamps. Receipt proof
booleans are not trusted. A receipt cannot select an executable: the compiler
must be the same file as the independently selected ABI-audit compiler. For an
explicit alternate toolchain, `PGACCEL_ABI_CLANG` selects that audit compiler.

Only after those checks does a final-report overlay classify the two query
exports as `bounded_metadata` / `build_bound_metadata`. It cannot remove parser,
preprocessor or ABI errors, approve another source/export, or populate GPU-output
parameter provenance. It is applied after cross-file GPU-proof resolution.
The external GPU-proof index explicitly rejects metadata classifications, even
if an entry also accidentally carries a device-dispatch tag.

The initial complete run found that the overlay was too strict about the
informational `macro_expanded_body` source-view tag. That tag is now distinguished
from expansion failures, with a real-production-source regression. The independent
typed proof still checks the actual compiler-expanded frozen translation unit.
The development failure log is retained separately from the final report.

## Verification scope

- The final complete release scan approves **both workspace-query exports**
  with an `independently_verified` metadata-link binding and empty GPU-output
  provenance. The fresh-library, compiler and combined object ABI checks pass
  for **130 exports**. Overall status remains **FAIL on seven device-work
  findings**: five join execution/count/projection exports and
  `pgaccel_grouped_agg_execute_ex` plus its wrapper. The earlier join-only scan
  did not cover those two grouped-aggregate failures. No device-work findings
  were waived. `release-audit-final.json` is the authoritative final inventory.
- The release recipe passed all **180 existing analyzer regressions**, **20
  typed-effect tests**, **15 metadata-build tests**, and the initial **10 linkage
  tests** before rebuilding and auditing the library. The final linkage suite
  has **11 passing tests**, including the production-source tag regression.
- Real shared-library fixtures pass independently rebuilt object comparison in
  both optimized and debug configurations. Negative cases cover changed source,
  object/library bytes, forged object hashes, stale links, wrong map ownership,
  invalid build roots, receipt-selected executables and metadata proof borrowing
  from a separate translation unit. Altered receipt proof booleans do not affect
  the newly computed proof results.
- Apple ld, GNU ld and LLD map formats have parser regressions. Apple shared
  libraries have live end-to-end tests. A real cross-compiled x86-64 ELF library
  and LLD map also agree with `llvm-nm` addresses and object ownership. This is
  artifact-format validation, not a Linux runtime or CUDA qualification.
- The native build and metadata host CTest pass. The rebuilt shared library
  passes **40,389 protected-memory checks**. The GPU join CTest still **fails
  with exit 77** because no GPU is visible; this is not a passing skip.
- Coverage scope remains 228 Rust files, 18 C++ sources, 35 CTests, 100 SQL files,
  462 assertions and 43 semantic families, with the unchanged 90% threshold.
  Safety contracts, 21 documentation-parity tests, formatting/lint and diff
  checks pass. No Rust or native runtime implementation changed in this turn;
  PostgreSQL core/runtime suites were not rerun here.

## Limits

The installed compiler, linker, symbol reader and checked-in build/audit drivers
remain trusted components. Receipts are reproducible freshness/ownership evidence,
not signatures or protection against replacement of the entire trusted toolchain.
This proof checks bounded metadata effects, not general arithmetic/alias-contract
correctness. The existing runtime matrix retains that separate responsibility.

The complete TODO objective remains open. No extension was installed, no server
was restarted, and no commit or push was possible because `.git` is read-only.
The source snapshot and SHA256SUMS seal this working-tree checkpoint, not a
release certification.

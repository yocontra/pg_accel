# Build-bound join metadata checks

2026-10-10 UTC. The metadata-query object is now proved during the actual CMake
build, before either native library or the host matrix links it. This closes the
standalone-proof/build-input gap. It does **not** close release-audit integration,
the remaining device-work proofs, or GPU/normal-SQL qualification.

## What changed

`scripts/metadata_build.py` captures the source and complete included-header
contents in a closed Clang virtual filesystem. The frozen preprocessing output
must equal the original compiler-expanded input. Both the typed AST check and
object generation then use those same files, compiler and flags. Missing files
cannot fall through to the live filesystem. An uncaptured `__has_include` input
that changes preprocessing semantics fails closed.

CMake builds one PIC metadata object with this driver and links it into the
static library, shared library, and host layout test. Both proof-driver files
and the compiler-generated header depfile participate in rebuild dependencies.
A receipt records compiler identity/version, flags and mandatory diagnostics,
source/header digests, preprocessed semantics, verifier/driver digests, root
contracts, proof results and object digest. CMake replays receipt freshness on
every build, including an otherwise up-to-date build. Replay uses caller-supplied
expected paths and flags, not receipt-selected inputs.

The helpers now have internal static linkage, preventing a separate SYCL
translation unit from replacing a proved helper through weak symbol coalescing.
The driver rejects unknown flags, external helper linkage, global startup work
and top-level assembly. It does not implement any CPU join execution or fallback.

A regression uncovered a real profile bypass: `#pragma`/`_Pragma` can suppress
`-Werror=vla`, hiding an evaluated parameter bound from Clang's JSON AST. Both
metadata compiler paths now check compiler-expanded diagnostic directives and
enable mandatory diagnostics in system headers. Unknown diagnostic suppressions
fail closed; the SDK's macro-redefinition suppression is narrowly permitted.

The initial stdin/preprocessed-object experiment collapsed coverage filenames
to `<stdin>`. The final closed-filesystem build preserves actual source/header
identity instead. An executable coverage regression checks that property; no
coverage source or threshold was removed. The collector includes the new host
object alongside the 16 shared-target objects, retaining the 17-object inventory.

## Verification

- **35 metadata tests pass:** 20 typed-effect tests and 15 build/receipt tests.
  They cover actual object execution, row-reading optimization/define branches,
  transitive header changes, a header mutated after proof but before codegen,
  helper replacement by another object, startup hooks, VLA/diagnostic escapes,
  unsupported flags, receipt mismatches, dependencies and real coverage mapping.
- Native build and host CTest pass. The protected-memory matrix reports
  **40,389 passed / 0 failed** against the rebuilt shared library, under
  ASan/UBSan, and with instrumented metadata code.
- Instrumented metadata coverage reports **100% query-source lines** and
  **94.07% helper-header lines**, under their original filenames. This is not
  evidence that the complete GPU/C++ release coverage gate passed.
- The proved object and its static-library member have the same SHA-256:
  `7409cf40ac5a10a7da7d4ee135e23e13638ba6c96073b60d817f77fddebec295`.
  Build link commands include that object in both libraries and the host test.
- PG18 and PG19 core suites each pass **1,249 tests**. Coverage-tool tests pass
  **104**. Documentation parity, safety contracts, Rust formatting, Python
  formatting/lint, shell syntax and diff whitespace checks pass.
- Coverage scope remains 228 Rust files, 18 C++ sources, 35 CTests, 100 SQL files,
  462 assertions and 43 semantic families, with the unchanged 90% threshold.
- The actual production source audit still **fails on seven exports**. It does
  not consume these receipts or grant a metadata exemption. The pre-existing
  analyzer implementation was not changed in this checkpoint.
- The GPU join CTest still **fails with exit 77**: no GPU is visible. There is
  no new live SQL join qualification, planner admission or performance claim.
- Installed PG18/19 modules and the 130-export ABI manifest are unchanged.

## Remaining boundary

A receipt is freshness evidence, not a signature, GPU-work proof or independent
release approval. Release integration must replay the typed proof and bind the
expected object to the audited library's link/symbol inventory. Metadata results
must not enter the device-output proof cache. Arithmetic and alias-contract
correctness retain their independent runtime tests. The full TODO objective
remains unfinished.

No extension was installed and no server was restarted. No commit or push was
possible because `.git` is read-only. The source snapshot and SHA256SUMS seal
this working-tree checkpoint, not a release certification.

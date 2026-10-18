# Task 0a recovered and validated, 2026-10-17

Three candidate repository files are frozen. No production setup/build/CI
wiring, analyzer acceptance, SQL admission or export qualification changed.
No Git command was issued by this worker. Initial upstream CMake unexpectedly
ran read-only Git version discovery; its output and failed Ninja configure are
retained in `build.log`. Retried source-export configuration blocks Git via an
owned PATH shim and uses Unix Makefiles. No tracked source change for this was
needed. Original /private/tmp evidence had been cleaned to directories only;
this run uses persistent scratch. Original nondeterminism evidence still lives
at `.codex/scratch/projection-binding-20261012/`.

## Frozen source SHA-256

- patches/adaptivecpp/sscp-replay-controls.patch:
  62ab0c63521b6aea3d8dc19b8114d55384a6c7af46c7d894f713ac69414d4871
- scripts/tests/fixtures/acpp_hcf_replay.cpp:
  b2180daed5a73e5535d5a94e88a11933d449688235e51e376aa8acc1ad44b791
- scripts/tests/test_acpp_hcf_replay.py:
  2d19748e79dd048a29cd8b79f714e10e92acfae6ecfff5d06cc2fbb88334d731

Compact exact copies are in `source-snapshot/`; paths are recorded in
`logs/final-inputs.sha256`, verified after the final suite.

## Implementation and actual fixes found

- Explicit canonical nonzero 64-bit HCF object IDs; default random IDs preserved.
- LLVM 20's Optional setting did not reject repeated options. Actual compiler
  tests caught both identical and distinct duplicate acceptance (`tests-1.log`);
  `O.getNumOccurrences() > 1` now rejects these before value assignment.
- Insertion-ordered deduplication retains all twenty separate extraction
  functions, including annotation/name duplicate discovery.
- Native register constant decoder now rejects unsupported writes and control
  flow. Its narrow allowed windows are not a general instruction interpreter.
- HCF extraction validates either raw bitcode or its bounded LLVM wrapper;
  the entire payload, including wrapper, is retained and compared. Original
  tests incorrectly rejected valid native wrappers. `tests-1.log` and
  `tests-2.log` preserve failures and decoder call-boundary corrections.

## Final validation

`final-run.sh` completed exit 0 (worker session 62155):

- Complete clean-first plugin target rebuild: all 37 compiler objects built,
  then linked. `final-build.log`; no old objects used for final qualification.
- Eight tests passed, zero skipped, 60.622 seconds (`final-tests.log`).
- 31 compiler cases / 38 actual invocations; exact argv, cwd, exit and hashes
  retained in `final-tests/*/command-*.json` and `final-matrix.json`.
- Seven complete native object replay comparisons passed with **no byte or
  section normalization**: three fixture IDs and four real join_rows variants.
- Shared test-hook and static non-hook real join_rows, each O0 and O3/DNDEBUG,
  use all other actual launcher/compiler options. Matrix includes original and
  replay hashes and sizes. O0 objects: 1,890,712 bytes; O3: 600,040 bytes.
- Fixture IDs 1, 2^63 and 2^64-1 retain matching HCF and native unregister ID,
  registration buffer reference/extent, twenty names exactly once, unchanged
  entire embedded device payload. Four ordinary default compiles from patched
  and unpatched compilers match that device payload and have distinct IDs.
- Fifteen malformed values, identical/different duplicates and the unpatched
  plugin unknown-option invocation reject. Actual 32-bit target default input
  compiles; the explicit option fails the intended target-width guard.
- Actual 32-bit **compiler host** and unknown LLVM DataLayout are not qualified
  by this host's runtime tests; guard code is present but do not claim those
  environments were exercised.
- Ruff clean (`logs/final-ruff.log`).
- 998 installed-prefix file hashes, 766 installed-source file hashes and the
  `current` symlink remained unchanged. Before manifests and final checks are
  under `logs/`; source-prefix check scope is installed `include/` and `src/`.
- Before replay patching, all seven previously patched source files compared
  exactly with the installed source. Clean pin export was PM-supplied at
  456ae6910720810f5fe59f160e6707d46bb8e5f0; registered soft-fp and host-coverage
  patches applied before the new replay patch.

## Publication suggestion

Publish the three source files and compact raw logs/command records/matrix/
source snapshots/version and hash manifests, including initial failures. Do
not publish object binaries, full copied prefixes or the entire toolchain tree.
`final-matrix.json` records full object equality and complete payload hashes;
actual native object files remain locally in `final-tests/` for review.

This is a reproducible compiler prerequisite only, not a projection/device
proof. No replay object was linked or loaded. Independent object/variant ID
uniqueness remains the future binding/link consumer's responsibility. Seven
production proof findings remain open; no performance or release-ready claim.

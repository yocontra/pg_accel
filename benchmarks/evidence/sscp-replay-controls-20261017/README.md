# Isolated AdaptiveCpp exact replay prerequisite

2026-10-17, Darwin arm64, Homebrew LLVM 20.1.8. This qualifies the opt-in
compiler patch and its tests, **not** production build wiring, a projection
proof, SQL admission, runtime execution, performance or release readiness.
The seven production device/address proof findings remain open.

## Result

- A clean rebuild compiled all 37 plugin objects from an owned export of
  AdaptiveCpp pin `456ae6910720810f5fe59f160e6707d46bb8e5f0`, with the existing
  registered patches and the new replay patch. The installed plugin/source and
  `current` symlink were preserved; hash-check logs record the checked scope.
- Eight tests passed, zero skipped: 31 compiler cases, 38 invocations.
- Seven **complete object** replay comparisons passed without byte/section
  normalization: three fixture IDs and real `join_rows.cpp` shared/test-hook
  and static/non-hook variants at both O0 and O3/DNDEBUG. Independent review
  also compared every retained pair with `cmp`.
- Native registration-buffer/extent and unregister-ID inspection covers the
  O3 fixtures, four ordinary defaults and two O3 join variants. O0 qualification
  is whole-object equality plus embedded HCF ID, not native call inspection.
- IDs 1, 2^63 and 2^64-1 agree with embedded HCF and native unregister constants.
  Twenty distinct, duplicate-discovered extraction functions retain twenty
  initialized names exactly once. Complete embedded device payloads, including
  LLVM wrappers, agree across these fixture IDs and patched/unpatched defaults.
- Fifteen malformed IDs, identical/different repeated options and the unknown
  option on the unpatched plugin reject. A real i386 ELF default compilation
  succeeds; its explicit-ID counterpart rejects at the target-width guard.
  The 32-bit **compiler-host** and unknown-DataLayout paths were not exercised.

Default mode still generates random compiler IDs; four sampled IDs are distinct.
This is not a proof of global uniqueness. Replay objects were never linked or
loaded. A future binding/link consumer must allocate distinct IDs to independently
loadable objects and reject duplicate inventories or changed-input receipt reuse.

## Evidence and boundaries

`source-snapshot/` contains exactly the three reviewed repository files;
`logs/final-inputs.sha256` records their original paths and frozen hashes.
`final-matrix.json` records object sizes/hashes, equality and payload hashes.
`commands/` preserves every final compiler argv, cwd, exit status and raw output.
`logs/final-tests.log` is the final raw test result; `HANDOFF.md` records the
worker's terminal exit-0 attestation. `build.sh`, `rebuild.sh`, and `final-run.sh`
retain the original local commands and absolute paths, not portable installers.

`native-excerpts/` deliberately contains excerpts, not complete disassemblies:
source line numbers identify the unregister and registration windows. The
`*-initialized-symbols.txt` files are filtered `llvm-nm` output for the twenty
name symbols. Full disassemblies, retained/replay objects and toolchain trees
remain local under `.codex/scratch/projection-replay-20261017/`; no binaries
or copied prefixes are published. Exact-build dependency/environment binding
remains a future consumer obligation, not a claim of this experiment.

`logs/build.log` preserves the initial missing-Ninja configuration failure and
upstream read-only Git version discovery. The retry used Makefiles and an owned
Git-blocking PATH shim. `logs/tests-1.log` preserves actual duplicate-option
acceptance and initial decoder failures; `logs/tests-2.log` preserves the next
decoder failure. Final tests passed after explicit occurrence rejection and
bounded wrapper/native-window decoding; failures were not deleted or relabeled.

`original-nondeterminism/` preserves compact 2026-10-12 command records, unequal
whole-object hashes, failed save-temps output and the corrected device-extraction
explanation. Its README references larger artifacts retained in the original
scratch directory, not additional published files. Diagnostic normalization in
that investigation is explicitly **not** an acceptance rule.

`FILES.txt` inventories all payload files other than itself and `SHA256SUMS`.
`SHA256SUMS` seals that inventory and every payload file. Raw logs retain their
original formatting; source snapshots and command records remain unmodified.

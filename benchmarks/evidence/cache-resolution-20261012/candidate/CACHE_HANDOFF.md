# Cache-path candidate handoff

Isolated baseline: `/private/tmp/pgaccel-main-review.XVd2mz` (parent-exported main
5f4b165). Actual repository source and installed modules were not modified.
Exact integration scope: `cache-source-paths.txt` (15 files), bound by
`CACHE_SOURCE_SHA256SUMS`. Preserve other cumulative edits when integrating
Justfile, CMakeLists.txt and existing source/tests; new files can be copied whole.
Exception: `.github/workflows/ci.yml` was refreshed from actual main 644680e
before adding its two-line host-test invocation. Apply just those two lines;
do not restore any old baseline workflow content.

## Behavior

Native observation uses the pinned read-only settings accessor and pure
executable/hash helpers. The apps layer, XDG/HOME/passwd/cwd defaults, modern
and legacy override precedence, and runtime token semantics are respected.
It does not construct persistent storage or initialize a GPU. Missing caches
remain zero-success; unreadable/non-directory caches fail with cleared output.

One Python helper implements pinned Darwin discovery and explicit tool targets.
Artifacts, diagnostic capture and clear-jit consume it. Other platforms require
the actual target process cache explicitly. Native Linux application hashing is
retained; macOS algorithm tests are not live Linux qualification. The recursive
resident-concurrency benchmark fingerprint is unchanged.

Clearing is fd-relative, no-follow, exact apps/application/jit-cache only,
regular files only, with complete unsafe-entry preflight and replacement checks.
Only owned temporary test fixtures were cleared. No runtime/user cache was
purged. Exclusive quiet-window coordination remains required; already completed
unlinks cannot be rolled back on concurrent mutation.

## Terminal verification

- `native-before.log`: initial path regression failed against the old library.
- `native-final-before.log`: final expanded host test also failed against the
  old archive implementation. Actual old archive source was byte-compared with
  the parent baseline before this run. This executable links the old shared
  library, not the candidate archive object.
- `native-after.log`: final host test passed, using candidate archive_stats.o
  and pinned acpp-common; the existing shared library supplies only unchanged
  error-handoff/dependency symbols. Both FFI definitions are in the executable.
  No pgaccel_init or device dispatch is called.
- `native-compile.log` and `native-consumer-syntax.log`: clean terminal success;
  the latter checks all three modified fork/hash native consumers, not execution.
- `cache-python-native.log`: 11 tests pass, no skips. Native parity comprises
  12 fresh subprocess environments, including empty/legacy/whitespace/XDG/HOME,
  NBSP and literal tilde. Passwd-missing fallback and deterministic target-swap
  coordination use narrow test hooks; real temporary files and descriptors are
  used. Canonical just recipes run only until explicit target rejection, before
  any GPU command. One valid clear-jit run deletes an owned temporary file.
- `metal-artifact-path-tests.log`: 22 affected existing/new tests pass. The
  unrelated CandidateProvenanceTests class is excluded from this final run.
- Ruff for the new Python helper/tests, shell syntax and just parsing pass.

Initial `metal-artifact-tests.log` is preserved: two normalization regressions
were fixed by retaining read-only Path.resolve() normalization while removing
expanduser(). That initial full run also invoked pre-existing Git fixture tests
in owned temporary repositories. This was disclosed to the parent; they did not
modify the actual repository, and no further worker runs include that class.

## Review/qualification boundaries

No full native build, GPU fork execution, production module installation, live
Linux runtime, SQL or broad release audit was run for this candidate. Focused
host checks are deliberately separate from the prior engine SQL qualification.
The optional native/Python parity test is skipped unless its executable path is
provided; this candidate's recorded run supplied it and had no skips.

Independent simplification/correctness/security/performance review, minimal
integration, and integration-appropriate validation remain the parent's next
steps. No TODO item was changed and no publication is claimed here.

## Clearing-protection review fix batch

The initial review requested changes for three real destructive-target
protection bugs. `clearing-protection-before.log` records terminal failure of
three new regression methods with four failing cases: modern and legacy
runtime-base tokenization, HOME alias identity, and a relative runtime base
misinterpreted under a distinct client's cwd. Every fixture was owned temporary
data; no live cache was touched.

The fixed helper shares `_runtime_base` between discovery and protection.
Protected runtime/home/XDG roots are compared by path and existing identity
against the opened target. Alias following is confined to the protection
comparison; target traversal still uses fd-relative O_NOFOLLOW. Relative
implicit paths may be observed but cannot be cleared without an explicit
absolute tool target. Race wording now says detected changes abort, checks are
not atomic, and an exclusive quiet window is required.

Final terminal commands (cwd is this candidate):

```sh
ruff format scripts/acpp_cache.py scripts/tests/test_acpp_cache.py
ruff check scripts/acpp_cache.py scripts/tests/test_acpp_cache.py
PYTHONDONTWRITEBYTECODE=1 PGACCEL_ARCHIVE_PATH_TEST=/private/tmp/pgaccel-cache-resolution.PtRG1h/archive-path-after python3 -m unittest scripts/tests/test_acpp_cache.py -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_metal_stress_artifacts.CacheMeasurementTests scripts.tests.test_metal_stress_artifacts.ArchiveParserTests scripts.tests.test_metal_stress_artifacts.ArtifactIndexTests scripts.tests.test_metal_stress_artifacts.CrashArtifactTests scripts.tests.test_metal_stress_artifacts.LogAuditTests scripts.tests.test_metal_stress_artifacts.ReleaseWorkflowTests -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_acpp_cache.py -v
bash -n scripts/capture_metal_ci_diagnostics.sh
just --list
actionlint -config-file /Users/contra/Projects/pg_accel/.github/actionlint.yaml -shellcheck='' .github/workflows/ci.yml
```

Results: `clearing-protection-after.log` has 14/14 passing with native parity;
`clearing-fix-artifact-tests.log` has 22/22 passing, provenance fixtures excluded;
`clearing-fix-ci-tests.log` has 13 passing and one explicitly skipped native
parity test (probe absent, matching lightweight CI). Ruff, shell syntax, just
parsing, and actionlint all returned zero. No GPU test was run. Native source
was unchanged by this fix batch, so the cleanup host regression remains the
native implementation evidence. Independent re-review is required before
integration/publication.

## Scoped simplification follow-up — 2026-10-12

The independent three-file simplification pass removed exactly three redundant
lines from `archive_stats.cpp`: its unused `<cstdlib>` include and two buffer
clears made redundant by the entry clear. The native path header and Python
clearer were unchanged. The `resolve_jit_cache_dir` forwarding helper is retained
because the existing static lifecycle contract requires that call marker.
Correctness/security/performance review remains separate and pending.

The candidate object and focused host executable were rebuilt after cleanup.
The following commands completed with exit 0 from this candidate directory:

```sh
clang++ -std=c++17 -I /Users/contra/Projects/pg_accel/.pgaccel/acpp/current/include/AdaptiveCpp -I pgaccel-kernels/include -c pgaccel-kernels/src/archive_stats.cpp -o archive_stats.o
clang++ -std=c++17 -I /Users/contra/Projects/pg_accel/.pgaccel/acpp/current/include/AdaptiveCpp -I pgaccel-kernels/include pgaccel-kernels/test/test_archive_cache_path.cpp archive_stats.o -L /Users/contra/Projects/pg_accel/pgaccel-kernels/build -lpgaccel_kernels_shared -L /Users/contra/Projects/pg_accel/.pgaccel/acpp/current/lib -lacpp-common -Wl,-rpath,/Users/contra/Projects/pg_accel/pgaccel-kernels/build -Wl,-rpath,/Users/contra/Projects/pg_accel/.pgaccel/acpp/current/lib -o archive-path-after
./archive-path-after
nm archive-path-after | rg ' T _pgaccel_archive_(stats_snapshot|jit_cache_dir)$'
```

There were no compiler/linker diagnostics. `cleanup-native.log` records the
passing host result and both executable-defined FFI symbols. No CMake target,
full native build, Python suite, GPU dispatch or installed module was exercised
or changed by this cleanup validation. `CACHE_SOURCE_SHA256SUMS` now binds the
cleaned source. Edits are stopped for independent review.

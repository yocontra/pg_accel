# Pinned AdaptiveCpp cache resolution — 2026-10-12

Local functional qualification on Apple M2 Max / Metal. The reviewed cache fix
was integrated as 15 scoped files into coordinator-identified main `644680e`.
This bundle is evidence for cache observation and safe targeted clearing, not
SQL admission, performance certification, or a release-proof waiver.

## Result

- Fresh isolated Release native build: exit 0. All **37 registered CTests pass**,
  serially, in **337.36 seconds**. Four tests have the host label. Existing
  per-test timeouts and workload floors are unchanged. This duration is not a
  performance benchmark. All 37 raw logs retain exit-0 PASS attestations.
- Integrated Python cache tests: **14/14**, including fresh-process parity with
  the newly built native executable. Affected artifact tests: **22/22**. No
  skips in that combined 36-test run; unrelated Git provenance fixtures were
  excluded by exact test-class selection.
- Shell syntax, Justfile parsing and actionlint: exit 0.
- Fresh-library production audit: **exit 1, seven findings remain**, across
  131 ABI exports (124 pass). The finding `(path, entrypoint)` identities and
  classification counts match both historical references below. This is a
  historical identity comparison, not a claim that either old report audited
  main `644680e`. No exemption, assertion weakening or new finding was added.
- Final source checks: 148 captured native/header/test/script inputs, five
  configuration inputs and all 15 integrated cache files match their hashes.

The original runner completed at 23:36:14 UTC. Its exit 0 means its runtime and
focused validation stages completed successfully; `exit-statuses.txt` and the
audit report separately retain the audit's nonzero release-blocking result.

## Scope and regression evidence

Native resolution now uses pinned read-only settings, executable and hash
helpers and selects `<base>/apps/<application>/jit-cache`. It never constructs
persistent storage or initializes a device. Python tooling shares the pinned
Darwin contract and supports an explicit actual-target override elsewhere.
Fork fixtures use the same native resolver. The resident-concurrency recursive
fingerprint remains unchanged.

The isolated candidate began from exported `5f4b165`; its workflow was refreshed
from main `644680e` before adding two host-regression invocation lines. Actual
integration preserved intervening engine, metadata and analyzer additions:
only reviewed hunks were applied to CMake/Justfile/workflow. The other 13 scoped
files match the final reviewed candidate exactly. `source/` retains all 15
integrated text files; candidate and integrated manifests show the intended
CMake/Justfile differences. Their paths are repository-relative.
`source-snapshot.tar.gz` additionally retains the captured native/header/test/
script inputs and configuration files, including the exact audit implementation
used here, before any subsequent metadata-checker change. Its member inventory
is `source-snapshot-paths.txt`; source hash manifests use the repository root
(or the extracted snapshot root) as their base, not this evidence directory.

Retained candidate logs show the old native implementation failing the actual
layout regression and the fixed implementation passing. Review additionally
found tokenized modern/legacy base protection, HOME alias identity and relative
client-cwd clearing defects. `clearing-protection-before.log` records four
failing cases across three regression methods; the fixed run passes. Protection
now shares runtime base parsing, compares protected filesystem identity, and
rejects implicit relative deletion without an explicit absolute target.

Independent correctness/completeness and filesystem-boundary re-reviews
approved the final fixes; performance review reported no findings. Additional
review-only owned fixtures checked modern/legacy whitespace tokens, HOME/base/
XDG aliases and defined-empty modern suppression. No real cache was cleared.
The clearer remains non-atomic: detected changes abort, but exclusive quiet-window
coordination is required and completed unlinks cannot be rolled back.

## Reproduction and isolation

`run.sh`, `commands.log` and `run.log` retain exact commands and stage exits.
Build/test output was confined to `.codex/scratch/cache-resolution-live-20261012`.
`ACPP_APPDB_DIR` was an absolute fresh owned `runtime-base` under that directory.
FD limit was 4096; SDKROOT was the installed CommandLineTools macOS 27.0 SDK.
No installed native library, PostgreSQL module or PostgreSQL server was replaced
or stopped. No cloud resources or concurrent benchmarks were used.

The existing linker warning about a macOS-27 metadata object and macOS-18 shared
target is retained in `build.log`; it did not fail linking or execution.
Metadata binding and fresh shared-library hashes are retained without binaries.

CI now invokes the Python filesystem regressions in its Linux no-GPU job. Native
parity explicitly skips when its probe is unavailable: the candidate lightweight
run records 13 passes and one skip. The integrated local run supplied the fresh
probe and skipped nothing. Native Linux per-executable path/hash coverage here
is algorithm coverage executed on macOS, **not live Linux runtime qualification**.

Historical audit references (repository paths):

- `benchmarks/evidence/resident-join-metadata-link-20261010/release-audit-final.json`
- `benchmarks/evidence/hash-filter-binding-20261010/release-audit.json`

The seven open identities are grouped execute/execute_ex and join rows execute,
execute_controlled, count_controlled, project and project_controlled. No live
Linux/CUDA, new PostgreSQL SQL-suite run, cold-latency guarantee, speedup or broad
release certification is claimed by this cache-only bundle.

`FILES.txt` inventories retained files; `SHA256SUMS` seals them (both exclude the
seal itself; the inventory also excludes itself). Verify from this directory
with `shasum -a 256 -c SHA256SUMS`. No binaries, runtime cache or broad environment
dump is included. Independent publication review and coordinator Git operations
remain separate from this evidence preparation.

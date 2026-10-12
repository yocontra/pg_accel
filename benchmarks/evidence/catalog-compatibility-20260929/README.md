# Database-local installation and upstream PostgreSQL compatibility

The fresh upstream gate exposed a production bug: preloading `pg_accel`
installed planner hooks in databases that had never run `CREATE EXTENSION`.
Newly supported aggregate shapes could win planning there, then fail in resident
loading because the extension-owned invalidation trigger function did not exist.
This broke ordinary PostgreSQL queries before any GPU dispatch.

The original PG18 run failed `select_parallel` and `temp` (229/231 regression
tests passed). Its pristine regression and both isolation runs passed. The
failure was not a timing mismatch: results were replaced by a dispatch error
and subsequent aborted-transaction errors. The original evidence is retained;
no PostgreSQL expected output or suite setting was modified to hide the failure.

## Fix

The common upper-hook gate checks PostgreSQL's database-local extension syscache
before either production aggregate or raster path injection. Missing installation
and an active extension installation/update script leave SQL native. This uses
PostgreSQL's CREATE/DROP/rollback invalidation rather than a new presence cache;
the absence is not placed in the structural query-decline cache. Normal installed
admission, costs, device limits and kernel code are unchanged. Base/join hooks
already add no production paths and were not changed.

- Original failing PG18 module:
  `ee374bbbed4c131d1ac4257732eb1de4b4344db81faceb68b1b9bb362a2b1ce1`.
- Fixed PG18 module:
  `e1f3462cb05e53ae4a0ffe5d8ae752397f39b8472fb36765ffe6d37868f41b3c`.
- Fixed PG19 module:
  `4ebe48656208ee07c2112a7f764d87134cf07176d9a3f0656a0ecee9c1a1f97a`.

## Catalog lifecycle regression test

`just extension-catalog-test 18` (or `19`) creates an owned, uniquely named
database from `template0`, runs all phases in one backend, and removes only that
test database. It does not drop the extension from the working database.

The test checks preload-only native execution, a same-named non-member trigger
function on the search path, CREATE EXTENSION with rollback, successful install,
DROP with rollback, and committed DROP/recreate. Generic prepared plans are
executed across DROP/rollback. On GPU hosts, installed phases require normal
`parallel_dense_integer` dispatch and exact aggregate results; no-device hosts
must remain native. Absence phases execute EXPLAIN ANALYZE and forbid any
pg_accel Custom Scan. There is no force-admission GUC.

Both majors passed the initial lifecycle run and the final script rerun (all
five phase markers, no warnings). The new command and its logs are
wired into the PostgreSQL-matrix CI job; that wiring is not a claim that hosted
CI has run. Standalone core tests pass 1,154; both production Clippy builds,
five upstream-tool contract tests, formatting and 21 documentation checks pass.

The broader scalar-expression implementation was paused when this compatibility
failure appeared. No new expression language, wire version or SQL admission is
claimed by this change.

## Upstream results

| Version | Mode | Regression | Isolation |
| --- | --- | ---: | ---: |
| PG18.4 | pristine | 231/231 | 119/119 |
| PG18.4 | pg_accel preloaded/enabled | 231/231 | 119/119 |
| PG19beta1 | pristine | 245/245 | 129/129 |
| PG19beta1 | pg_accel preloaded/enabled | 245/245 | 129/129 |

All eight fixed runs pass: 1,448 scheduled executions. The original failed PG18
bundle and both fixed bundles are archived here as `upstream-*.tar.gz`.
Their complete internal `SHA256SUMS` seals were verified before archiving;
verification transcripts and driver logs are in `verification/`. Each archive
retains the source/build/module manifest, dirty-tree patch, results, command and
server logs, expected/actual differences and source-file hash manifest.

These are local debug/assertion, no-ICU source builds. The preloaded upstream
databases deliberately do not install extension SQL objects, so this proves
hook/default-path compatibility, not GPU execution of PostgreSQL's test corpus.
Catalog lifecycle tests supply separate installed-path evidence. No hosted CI,
CUDA, other-host, full upstream PostGIS/H3 or owner-recovery qualification is
claimed.

## Installed-extension matrix — completed with a warning failure

Both strict 90-file runs finished at **89 passed / 1 failed**. SQL117 reached
all correctness/completion markers but failed the warning gate: a NUMERIC
accumulate dispatch took 6,708,305 microseconds on PG18 and 5,339,724 microseconds
on PG19 against the unchanged 5,000ms warning threshold. The semantic inventory
therefore fails both runs at 416/418 assertions (99.52%); passing the percentage
threshold does not excuse a failed file. Complete logs, session profiles,
result tables and inventories are archived as `catalog-fullsql{18,19}.tar.gz`.

After both matrices finished, the unchanged SQL117 fixture passed on both
majors in separate, serial reruns, with both assertion markers, one completion
marker and no warnings. Catalog lifecycle tests then passed on both majors.
The isolated runs do not replace the original matrix failures or establish
their cause. No timeout, planner cost or admission threshold was changed.
CPU-only development/checks could run alongside these functional tests; no
other GPU suite or benchmark was overlapped. This is not a controlled cold,
concurrent or performance qualification.

Final rerun and inventory logs are in `verification/`. The installed module
hashes above are unchanged. Subsequent unbound SPG1 expression work is tested
separately and is not represented by these installed-module results.

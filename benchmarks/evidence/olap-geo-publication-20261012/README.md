# OLAP/geo evidence publication — 2026-10-12

This is an explicit publication inventory for the accumulated OLAP/geo source
milestone, not a new benchmark or a declaration that all release gates pass.
Historical artifacts support their captured, dated scope. Preserved failures,
qualification limits and unfinished TODO items remain part of the record.

## Exact selection and source boundary

`publication-paths.txt` lists every selected evidence file by repository-relative
path: 10,318 existing files in 101 bundles plus this directory's five files.
The existing selection totals 419,353,320 bytes before this publication metadata.
Use that file list, not a recursive directory add. `bundle-manifests.json`
records the directories, exclusions and each available top-level checksum
manifest's required working directory. The associated source changes are
published separately; `.gitignore` and the narrow current-status corrections in
`TODO.md` are auxiliary changes for this inventory.

The numeric bundle's transitive `numeric-20260929-pg18` and
`numeric-20260929-pg18-final` reports are included. The already committed
`corpus-importer-20261012` and `workspace-control-proof-20261012` bundles remain
available but are not included again. Unreferenced `resident-join-rows-20261006`
is excluded. The now-sealed `sql-live-20261012` bundle is included.

The historical source archives identify their own tested revisions; they are
not assertions that today's entire working tree matches every old snapshot.
Current source/module identity checks are recorded in the qualification bundles.
The independent workspace-checker milestone is already committed as
`a6542708aa431abc3580775ff039d270420ae9c3`; it is not new production admission or
a waiver of the seven unresolved grouped/join static proofs.

## Current qualification, with limits

- [Native Metal](../native-live-20261012/README.md): all 36 registered CTests
  pass, including three host-labelled tests. This supersedes earlier no-device
  initialization blockers, not SQL/planner/performance obligations.
- [Hash live tests](../hash-live-20261012/README.md): focused PostgreSQL 18/19
  execution coverage for the captured implementation.
- [Uniform budgeting](../uniform-budget-live-20261012/README.md): the exact live
  ownership/budget fixture passes on both majors; its ledger is process-local.
- [Production SQL](../sql-live-20261012/README.md): PostgreSQL 18.4 and 19beta1
  each pass 100 files and 462 assertions. The earlier PG19 warning failure is
  preserved. The successful fresh-cache rerun does not guarantee cold-start
  latency reliability or prove the original warning's exact cause.

These results do not establish all PostgreSQL regression compatibility, CUDA
qualification, new SQL join admission, cross-backend accounting, performance
milestone completion or a clean release gate.

A confirmed, separately tracked cache-path bug affects historical tools that
treated an explicit `ACPP_APPDB_DIR` base as containing `global/jit-cache`
instead of `apps/global/jit-cache`. Such cache-count/clearance observations are
not fresh cold-cache qualification merely because their artifacts are sealed.
This does not blanket-invalidate semantic or warm-run results, nor establish
that default-HOME observations were wrong. The current fresh-cache SQL run used
a new private base and the actual `apps/global/jit-cache` path. Its README
distinguishes valid query evidence from the earlier incorrect cache snapshots.
The resolver fix is separate unfinished work at this publication checkpoint.

## Bounded archive hygiene

`archive-hygiene.json` records a complete, capped streaming inspection of the
107 selected historical/current `.tar.gz` files without extraction: 99,932
members, 99,754 regular files and 1,448,540,739 expanded bytes. No unsafe paths,
links/special entries, duplicate member names or matches in four bounded
credential-pattern families were found. This is not a general security audit
or a guarantee that no secret could exist.

The archives are not pure source text: they retain 49,966 standard 163-byte
AppleDouble sidecars containing only the `com.apple.provenance` attribute, with
two payload hashes, plus 18 identical 225-byte PostgreSQL binary COPY
`stud_emp.data` regression fixtures. Those classifications and hashes are
recorded; no executable or unclassified binary payload remains in the scan.
Original checksum-bound archives were not rewritten. An earlier verbose scan
was truncated in transport; its displayed 807 binary findings were incomplete
and are superseded by this compact counter-based rerun.

The bounded earlier scan of uncompressed text found no credential-pattern
matches. This archive pass does not scan arbitrary nested compressed payloads.
The separately reviewed SQL bundle retains exact-owned-process derived trace
events, not raw system-wide traces; see its explicit trace exclusions.

## Private exception and verification

`resident-join-binding-20261009/initial-metal-abort.ips` is withheld, retained
locally unchanged and protected by an exact `.gitignore` rule. Its bundle README
records the original hash. The original `SHA256SUMS` remains historical: it
includes the private report and the old README and must not be presented as a
public-subset seal. Use that bundle's `PUBLIC_SHA256SUMS` instead.

From the repository root, verify this inventory's four metadata files:

```sh
shasum -a 256 -c benchmarks/evidence/olap-geo-publication-20261012/SHA256SUMS
shasum -a 256 -c benchmarks/evidence/resident-join-binding-20261009/PUBLIC_SHA256SUMS
shasum -a 256 -c benchmarks/evidence/native-live-20261012/SHA256SUMS
(cd benchmarks/evidence/sql-live-20261012 && shasum -a 256 -c SHA256SUMS)
(cd benchmarks/evidence/uniform-budget-live-20261012 && shasum -a 256 -c SHA256SUMS)
```

For other bundles, use the exact `base` and `manifest` recorded in
`bundle-manifests.json`: `.` means repository root; a directory value means
change into that directory first. A null manifest means the historical bundle
has no top-level `SHA256SUMS`, not that it has been newly sealed or requalified.
This directory's seal covers publication metadata only, not every historical
artifact's bytes. No manifest recursively hashes itself.

# Scoped evidence copy proposal

Destination: benchmarks/evidence/raw-integer-count-admission-20261030/raw/

Exact frozen list: package-files.tsv SHA256 bf70e12cee12a0fd033dbfd1fc2657b886fc941573bf7deecebaefc079e584a9. Each line is bytes TAB scratch-relative source path; destination mirrors the path below raw/. Total: 5,044 files, 62,610,977 bytes (59.71 MiB). No copy performed yet.

| Group | Files | Bytes |
|---|---:|---:|
| current/build-receipts | 2 | 5289 |
| current/other | 128 | 1574389 |
| current/controls | 564 | 3492049 |
| current/matrix | 502 | 3314697 |
| current/pg-tests | 159 | 141622 |
| current/planner-diagnostics | 181 | 8176720 |
| prior/other | 150 | 1956139 |
| prior/build-receipts | 247 | 1348583 |
| prior/qualification | 167 | 821263 |
| prior/controls | 587 | 3906292 |
| prior/calibration-training | 706 | 16596404 |
| prior/calibration-validation | 522 | 13056516 |
| prior/G4096 | 101 | 2148922 |
| prior/matrix | 1028 | 6072092 |

## Inclusion

Current 22-cell matrix and all 12 ABBA runs: every report, artifact JSON/plan/settings/parity/lifecycle receipt, terminal, source/module/harness and evidence seals. Run1–5 planner diagnostics including failed run2 and all raw samples. Both-major correctness and positive mixed-union proof, failed silent-skip proof1, failed sparse assertion receipt. Original cost-final3 and earlier-final3 matrices, all original cost ABBA reports (including two strict failures), original failed qualification/control build/observer runs. Frozen training2 failure/training3 fit/validation measurements, both historical and current G4096 receipts. Build source/module seals and logs, all checker/driver/calibration/proposal sources at both task roots. Existing native-only foundation evidence remains a referenced committed package, not duplicated here.

Selection is produced read-only by package-inventory.py; all databases (pgdata), caches, installed prefixes, targets, detached source checkout, compiled modules and executables are pruned. Source patches currently present in scratch are included; root must supply final authoritative Git patches/base and commit links for the raw-count, shared-input and lazy-estimator changes. Root owns README/TODO/Git and final manifest. This list intentionally excludes mutable PG database state, including run2 pgstat.stat; its previously audited hash/decoded maintenance evidence is documented in the diagnostic proposal, not copied as binary.

## Sensitive-file checks

All selected files were scanned: no NUL/binary files; no matches for credential tokens, private-key headers, long credential assignments or Authorization bearer/basic headers. No full environment dumps are selected. Eight machine-wide process-list logs were additionally excluded to avoid publishing unrelated application command lines; none is referenced by the selected calibration JSON. Originals remain untouched in scratch. The scan is a heuristic, not a guarantee; final reviewer should inspect scope and sensitive exclusions before publication.

## Finalization

After copy authorization, copy only exact listed files, verify byte sizes and content hashes against sources, then prepare package completion/integrity receipt. Do not mutate historical receipts or normalize embedded absolute paths. README must explain canonical original paths and replay requirements. Root adds authoritative source patches and README; final SHA256SUMS is generated last and excludes itself. No binaries, databases or caches will be copied.

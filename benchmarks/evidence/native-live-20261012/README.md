# Full native Metal qualification — 2026-10-12

All **36 registered CTests pass**, with no failed tests, after a fresh native
reconfiguration and complete build. The serial run took 294.59 seconds on the
Apple M2 Max/Metal device. This duration is test-run evidence, not a benchmark
or query speedup claim.

## Commands and scope

```sh
ulimit -n 4096
just gpu-build
ctest --test-dir pgaccel-kernels/build --output-on-failure --timeout 300 -j 1
```

Both commands completed successfully. The existing repository timeouts were
unchanged: 300 seconds by default, with the registered longer spatial, raster,
fork-barrier and OOM overrides intact. No test was excluded or restarted after
an observation timeout. No benchmarks ran concurrently.

The suite covers host contracts, device lifecycle, bounding boxes, spatial
dispatch/pairwise/resident kernels, H3, raster, correctness/execution, fork
lifecycle, scalar/reduction families, grouped/hash aggregation and workspace
contracts, partial/key aggregation, joins, domain/spatial matrices, expression
memory/VM, OOM invariants, FP64 multiplication and induced kernel failures.
Three registered tests carry the host label; the suite total is not a claim
that all 36 tests require GPU execution. `ctest-manifest.json` records the
actual registered commands and test properties.

`build.log` proves the fresh build. `ctest.log` and `LastTest.log` retain the
suite results and individual output. The `raw/` directory contains all 36 raw
logs; each has a terminal `PGACCEL_TEST_RESULT` attestation with
`exit_code=0 result=PASS` and its output/binding hashes. These are actual runs,
not successful skips for an unavailable device.

## Source identity and earlier failures

The source archive is copied unchanged from the sealed
`benchmarks/evidence/hash-live-20261012/source-snapshot.tar.gz`. Before copying,
all 544 source files under `pg_accel`, `pg_accel_bench`, `pgaccel-kernels`,
`scripts`, `sql` and `coverage` were compared by SHA-256 with the current tree;
every hash matched. The current and archived source inventories also matched.
Eight root/build configuration files were compared separately and matched.
The resulting file hashes are retained in `source-identity-check.log` and
`build-config-identity-check.log`. The original bundle's checksum manifest
verified before reuse. The archive includes its earlier documentation state;
this comparison claims source/build identity, not identity of subsequent
evidence prose or Git metadata. Engine work was still uncommitted when tested.

Earlier focused runs found two test-fixture errors: an empty hash output
nullability mismatch and signed-minimum SQL cast precedence. Those failures
and corrections are preserved in the separate hash-live bundle. This complete
native run includes the corrected native fixture and passes it. CTest's old
`LastTestsFailed.log` was left untouched: its timestamp predates this run and
records the earlier focused failure, not a failure in this 36-test result.

## Limits

This proves the registered native Metal suite for the captured sources. It
does not prove CUDA support, new normal PostgreSQL planner admission, query
performance, all PostgreSQL regression suites, broader concurrency/release
gates or full TODO completion. The previous linked-object static audit's seven
unresolved grouped/join proofs remain open; no static audit was waived or
reclassified by these runtime passes. The separately registered archive-stress
release gate and unregistered overlap diagnostic are not part of these 36
CTest results.

`SHA256SUMS` seals this document, the source archive, manifest and logs after
all verification processes completed. No original PostgreSQL installation or
server was changed by the native run.

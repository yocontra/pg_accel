# Production SQL qualification, 2026-10-12

Current complete production runs pass on PostgreSQL 18.4 and 19beta1:
**100/100 files and 462/462 fixed semantic assertions on each**. No files were
skipped and warning thresholds were unchanged. An earlier PG19 warning failure
is retained below; cold-start latency reliability is not claimed.

## Completed initial matrix

Fresh optimized production release modules, without `pg_test`, were installed
only into copied PostgreSQL prefixes. PG18.4 passed all 100 SQL files and all 462
fixed semantic assertions. PG19beta1 attempted all 100 files: 99 passed and 460/462
assertions qualified. Its SQL117 query completed semantic assertions but
emitted a 5,924,664us synchronous grouped-accumulate warning at the unchanged
5000ms threshold. Strict inventory correctly failed it. There were no skipped
files or other failures. The initial matrix process 36837 is terminal and both
owned clusters stopped successfully.

`pg18/` and `pg19-initial/` preserve all per-file raw logs/results, inventories,
build/install logs, module SHA256 identities, runtime profiles and server logs.
`fixed-run.log` records both runner/inventory/stop outcomes. A passing semantic
result does not erase a warning failure.

## Complete fresh-cache PG19 rerun

Process 94720 reached terminal exit 0: all 100 files passed, all 462 assertions
qualified, strict runner/inventory returned 0, and the owned cluster stopped
successfully. `pg19-fresh-cache/` preserves all logs and inventories.

This run used a new disposable cluster and a newly created private
`ACPP_APPDB_DIR`, not a prewarmed subset. `module-identity.log` confirms the
optimized release module/control/SQL hashes match the earlier complete matrix.
No production module, SQL fixture, cap or 5000ms threshold was changed between
the failing PG19 run and this full rerun. Other agents' compiler/test workloads
had ended before launch; the short scan overlap noted below is retained.
Passing this run does not establish the original warning's exact cause or
guarantee first-use latency on future hosts.

## Release packaging fix

The preceding attempt could not preload either optimized release module:
macOS dyld rejected a misaligned Mach-O LINKEDIT string pool. No SQL files ran
in that failed attempt. `release-load-failure/` preserves errors, exact artifact
hashes, raw linker commands and the optimized `strip=none` diagnostic.

The raw cargo artifact reproduced the installed failure, excluding pgrx's
schema/copy step. A fixup-chains ablation was ineffective because the later
existing no_fixup_chains flag overrode it; it is not positive evidence.
Disabling post-link stripping retained optimized release compilation and all
existing linker flags, produced an aligned artifact, and passed isolated PG19
preload/extension/device checks. This matches upstream rust-lang/rust#157750.
Cargo.toml now explicitly sets `[profile.release] strip = "none"`; the initial
full SQL matrix above uses that manifest setting, not an environment override.
The separate regression verifies the release-profile contract.

Original failing binaries remain under
`.codex/scratch/sql-live-20261012/pg{18,19}/initial-release-failed.dylib`;
they are identified by SHA256 here, not overwritten or bundled redundantly.

## Controlled latency diagnosis

`diagnostic/` contains exact-query stock-parity/GPU-delta probes with unchanged
1,000,000-row caps and 5000ms warning threshold. Shared-disk-cache first/repeat
CTAS timings were 43.685/13.698/13.740ms; a fresh private-cache run was
2794.129/14.102/9.907ms. A separately traced private-cache run was
4330.042/13.341/10.044ms. These are host wall timings, not GPU timings.
The large repeated cache-inventory JSONL is losslessly gzip-compressed; the
uncompressed original also remains in scratch.

Only owned backend 11458 was explicitly attached by Metal System Trace. Its
derived process-filtered events show 23.995128ms total/union GPU activity across
the entire four-execution traced workload. This places the observed cold
multi-second overhead outside GPU execution, consistent with first-use JIT and
archive preparation. It does not retroactively determine the exact cause of
the earlier 5.924664s warning. Cold-start deadline reliability remains open.
No cap or threshold was lowered/raised to obtain a pass.

Raw trace/TOC/broad XML contain system metadata despite target attachment and
remain scratch-only. Only exact-PID 11458 derived events are included here.
The first private-cache probe used an incorrect snapshot directory; its
query/timing/parity evidence is valid, but its cache snapshots are not. The
actual runtime path is `$ACPP_APPDB_DIR/apps/global/jit-cache`, as used by the
traced probe and subsequent full rerun. See diagnostic README for full limits.

## Scope and reproduction

`run.sh` records original matrix commands and SDK overrides. The release-only
toolchain fix does not substitute debug tests for production SQL. `ulimit -n
4096` is used. All installations and clusters are owned scratch copies; the
original PostgreSQL 17 installation/server and shared JIT cache were preserved.

Source binding: `source-baseline-sha256.log` and `source-identity-verify.log`
compare all 544 baseline source inputs. All Rust, native and SQL sources match
the source snapshot sealed in `../uniform-budget-live-20261012`; the only
difference is the independently updated Python proof checker
`scripts/workspace_calls.py`, not a compiled module input. Overlay the supplied
current Cargo.toml for the release stripping fix when reproducing that source
snapshot. Current build/config hashes, SQL runner, fixed manifest and inventory
tool are supplied here. The independent checker/wiring publication changed no
engine or installed module during qualification.

The repository's semantic suite is not the upstream PostgreSQL regression
suite, CUDA qualification, a benchmark speedup claim, or a waiver of static
device/address proof obligations. The original PG19 warning remains part of
the qualification history even if a later complete run passes.

Host activity caveat for the rerun: another worker reported an unintended
approximately 30-second, one-core read-only artifact scan, then terminated only
its own scan. Exact start/end were not recorded; the conservatively reported
overlap interval is 2026-10-12 22:44–22:47 UTC. No concurrent GPU workload was
reported. The rerun is not represented as a perfectly idle-host benchmark.

All qualification and diagnostic processes were terminal before this bundle's
`SHA256SUMS` was generated and verified. No Git actions were performed by the
verification worker; publication is handled by the coordinator.

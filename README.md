# pg_accel

`pg_accel` is a PostgreSQL Custom Scan extension for GPU-resident query
execution. The current production planner surface is intentionally narrower
than the kernel library: it can select a childless resident aggregate plan for
covered reducing, grouped-aggregate, and star-aggregate SQL shapes. Other
kernel families remain unavailable to normal planning until they have a
complete resident pipeline.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yocontra/pg_accel/ci.yml?label=CI)](https://github.com/yocontra/pg_accel/actions)

## Build from source

There is no prebuilt-package promise in this repository. A development build
requires Rust, pgrx, CMake, a C/C++ toolchain, and the AdaptiveCpp commit pinned
by [`.acpp-version`](.acpp-version). The setup and CI recipes read that file
directly; a branch name alone is not sufficient provenance.

The currently validated GPU development target is Metal on Apple Silicon:

```bash
brew install llvm@20 lld@20 libomp boost postgis
just setup-system-deps
just setup-tools
just setup-pg-source 18
ACPP_BACKEND=metal just setup-gpu-metal-headers
ACPP_BACKEND=metal LLVM_PREFIX=/path/to/llvm ACPP_LLD_PATH=/path/to/ld64.lld just setup-gpu
just setup-pgrx
cargo pgrx install --no-default-features --features pg18
```

PostgreSQL 18 is the default extension target. PostgreSQL 19 beta is also a
build/test target, not a package-availability claim. A build without a usable
GPU can load the extension, but the planner keeps queries on PostgreSQL-native
plans. CUDA/NVIDIA validation is owner-deferred in [TODO.md](TODO.md); this
document makes no support claim for it.

### AdaptiveCpp provenance and installation burden

The Metal toolchain is not an upstream binary dependency. The setup path clones
`https://github.com/yocontra/AdaptiveCpp.git`, checks out branch
`fork-safe-metal` at exact commit
`456ae6910720810f5fe59f160e6707d46bb8e5f0`, and builds it from source into the
repository-local `.pgaccel/acpp/metal` prefix. The commit recorded in
`.acpp-version` remains authoritative if this prose ever drifts.

The pinned history contains an upstream `develop` snapshot at
`9a91272169733bdfa6780362e7a2c94cd7580ffd`, merged by
`44948428654b8785d464d11544b0a845b52917af` on 2026-07-04. Release notes list
the non-merge commits unique to the pinned side of that locally available
history. This is a historical rebase/sync statement only: pg_accel does not
claim that the fork changes have been accepted upstream or that the snapshot is
the current upstream tip.

On Apple Silicon, source setup requires Xcode command-line tools, Apple
`metal-cpp` headers, and the canonical Homebrew formula set `llvm@20`,
`lld@20`, `libomp`, `boost`, and `postgis`. The release package retains only
LLVM 20 and `libomp` as explicit host runtime prerequisites; its installer
fails before writing files when either runtime is absent. Setup also clones
`yocontra/soft-fp` tag
`v1.3.0` and applies the tracked patches under `patches/adaptivecpp/` plus a
conditional `metal-cpp` compatibility edit. The result is therefore the exact
fork commit plus reviewed repository patches, not a claim that a pristine
upstream checkout is sufficient. `scripts/setup_acpp.sh` records the resolved
commit, backend, compiler paths, CMake arguments, soft-fp64 revision, and
post-patch Git status in
`${ACPP_PREFIX}/pg_accel-acpp-provenance.txt`. That file from the exact release
candidate is required evidence; these instructions alone do not close a
release gate.

Add the extension to `postgresql.conf`:

```conf
shared_preload_libraries = 'pg_accel'
```

On macOS, pg_accel sets `OS_ACTIVITY_MODE=disable` in the postmaster before it
forks backends. This works around an Apple unified logging/CoreAnalytics crash
during lazy Metal initialization on affected Tahoe builds. An explicit
`OS_ACTIVITY_MODE` value in the postmaster environment is preserved, but an
override other than `disable` may re-expose that fork-time instability. This
is a targeted runtime workaround, not a broader macOS support claim.

Restart PostgreSQL and create the extension in the target database:

```sql
CREATE EXTENSION pg_accel;
```

## Quick start

This example uses the same resident grouped-aggregate shape asserted by the
extension test suite at
`pg_accel/src/tests/mod.rs:569-626`.

```sql
CREATE TABLE pg_accel_quickstart AS
SELECT (i % 64)::int4 AS g, (i % 1000)::int4 AS v
FROM generate_series(1, 500000) AS i;

ANALYZE pg_accel_quickstart;

-- Load and pin only the columns required by the selected plan.
SELECT pg_accel_pin('pg_accel_quickstart', ARRAY['g', 'v']);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
SET pg_accel.auto_load = off;

EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
SELECT g, sum(v), count(*)
FROM pg_accel_quickstart
GROUP BY g
ORDER BY g;
```

On a capable configured device, a selected plan contains this exact node and
evidence (additional descriptor and residency properties are expected):

```text
Custom Scan (GpuAccelAgg)
  Strategy: GpuAgg
  Plan Selected: true
  GPU Resident Pipeline: true
  GPU Resident Operator Class: resident_groupagg
  GPU Kernel Dispatched: true
```

`GPU Kernel Dispatched` is present only under `EXPLAIN ANALYZE`. The property
names are emitted by
`pg_accel/src/engine/ffi/custom_scan/explain.rs:44-150`; the aggregate and raster
method tables are defined and registered at
`pg_accel/src/engine/ffi/custom_scan/mod.rs:251-309`.

To prove the native decline boundary with the same table:

```sql
SELECT pg_accel_reset_stats();

EXPLAIN (VERBOSE, COSTS OFF)
SELECT * FROM pg_accel_quickstart WHERE v > 500;

SELECT pg_accel_last_planner_rejection_reason();
-- no_gpu_resident_pipeline
```

A base scan/filter plan does not contain a pg_accel Custom Scan. The production
base-relation hook records the decline and leaves the PostgreSQL path intact at
`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:607-627`.

## Capability matrix

Kernel or bridge presence is not equivalent to production planner selection.
The normal upper-path hook injects only the generic aggregate candidate at
`pg_accel/src/engine/ffi/planner_hooks/mod.rs:205-243`. Only aggregate and raster
Custom Scan method tables are registered. Normal production planning selects
the aggregate path; raster admission is test-only. Base scans, row-returning
joins, sorts, windows, and standalone function/SRF shapes remain native and
have no registered executor.

| Capability | Implementation surface | Production planner | Current boundary |
|---|---|---|---|
| Resident reducing or grouped aggregate | Present | Selectable | Covered `AggQuerySpec` shapes become `Custom Scan (GpuAccelAgg)` after shape, type, residency, device, and cost gates. Stored generated columns are physical base attributes and remain selectable when their types and aggregate shape pass those same gates. |
| Resident star join plus aggregate | Present | Selectable | The join is represented inside one childless aggregate descriptor; this is not a row-returning join path. |
| H3-derived group key inside a resident aggregate | Present | Selectable | Covered `h3_cell_to_parent` and `h3_latlng_to_cell` group expressions can be encoded in the aggregate descriptor. |
| PostGIS spatial filter inside a resident aggregate | Present | Test-only | The descriptor lane is dark in normal planning and is admitted only by a test GUC. |
| Standalone PostGIS or H3 function/SRF | Aggregate primitives and adapter registry metadata remain; standalone executor removed | Not selectable | PostgreSQL executes standalone calls. Function and target-list SRF hooks record `no_gpu_resident_pipeline`. |
| Base scan, WHERE filter, or projection | No registered Custom Scan executor; host-staged implementation retired | Not selectable | PostgreSQL executes the base path. The production hook observes and records declines but injects no scan CustomPath. |
| Row-returning hash or inequality join | No registered row-returning executor; host-staged implementation retired | Not selectable | PostgreSQL executes the join. Resident join membership exists only inside childless `GpuAccelAgg` descriptors. |
| Sort or top-k | Kernel or descriptor code may remain; no registered executor | Not selectable | Sort opportunities remain PostgreSQL-native without a complete resident producer/consumer path. |
| Window | Kernel source may remain; no registered executor | Not selectable | Planner-visible full and reducing window SQL remains PostgreSQL-native and records `no_gpu_resident_pipeline`. |
| Raster | Registered childless resident executor | Test-only | Production planning is observation-only; a test GUC can force the resident raster path. |

The extension adapters currently register these names for OID discovery. This
table is registry metadata, not a standalone SQL support promise:

| Adapter | Registered functions |
|---|---|
| PostGIS | `st_intersects` |
| H3 scalar | `h3_latlng_to_cell` |
| H3 variable/record output | `h3_cell_to_children` |

The adapter constructors are the source of truth at
`pg_accel/src/adapters/postgis.rs:14-22` and
`pg_accel/src/adapters/h3.rs:53-135`.

**Correctness boundary:** production resident aggregate execution has no CPU
fallback; an uncertain aggregate result or GPU failure raises an error after
selection. The non-production spatial descriptor test lane is the one explicit
exception: it retains exact GSERIALIZED values and rechecks uncertain rows with
PostGIS on the PostgreSQL backend thread before patching the device mask. See
`pg_accel/src/engine/executor/agg/output.rs:66-80` and
`pg_accel/src/engine/executor/agg/spatial.rs:1067-1115`.

## Residency

Residency is backend-local; the byte budget is enforced by a cluster-wide
ledger. A selected plan can load missing columns when `pg_accel.auto_load` is
on. Turning it off requires the current backend to have the needed data already
resident, normally through `pg_accel_pin`.

| Function | Result | Operation |
|---|---|---|
| `pg_accel_pin(regclass, text[] DEFAULT NULL)` | loaded row count | Load the named columns (or all supported columns) and keep the relation pinned against local LRU eviction. |
| `pg_accel_unpin(regclass)` | boolean | Remove the pin; resident data may remain as evictable cache state. |
| `pg_accel_refresh(regclass)` | loaded row count | Reload the pinned columns, or the columns already resident for an unpinned relation. |
| `pg_accel_evict(regclass)` | boolean | Remove this backend's resident entry. |
| `pg_accel_resident_status()` | set of rows | Show relation OID, attribute numbers, raw/derived bytes, pin state, generation, timestamps, and load time. |
| `pg_accel_resident_live_bytes()` | bigint | Show the exact cluster-wide resident byte ledger at that instant. |

The SQL wrappers and status tuple are defined at
`pg_accel/src/engine/residency/store.rs:3352-3427`. Pins do not bypass
`pg_accel.resident_memory_budget_mb`.

## Configuration

The table below is the complete released GUC inventory. `user` means
`PGC_USERSET`; `superuser` means `PGC_SUSET`. Test-only `pg_accel.test_*`
settings are intentionally excluded.

| Parameter | Type | Default | Context | Range | Effect |
|---|---|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | user | - | Planning master switch. While off, new pg_accel paths are not added; an already-planned Custom Scan fails closed if it reaches execution while off. |
| `pg_accel.min_batch_size` | int | `65536` | user | `1..16777216` | Fill target only for legacy row-fed Custom Scans. Resident descriptor calls use device-limit chunk caps and independent admission costs. |
| `pg_accel.gpu_enabled` | bool | `on` | user | - | Planning GPU-path switch. It adds no new GPU path while off and does not rewrite an already-planned path. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | user | `100..60000` | Millisecond warning threshold measured after a synchronous dispatch returns. Bounded dense aggregation checks interrupts between calls; it does not asynchronously cancel an in-flight call. |
| `pg_accel.max_workers_total` | int | `0` | superuser | `0..4096` | Cluster host-thread ledger cap; `0` is unlimited. Current executors request no host threads, and PostgreSQL parallel worker processes are not counted. |
| `pg_accel.resident_memory_budget_mb` | int | `-1` | superuser | `-1..1048576` | Cluster-wide MiB cap for all charged residency bytes. `-1` derives the cap from `DeviceLimits`; pins never bypass it. |
| `pg_accel.auto_load` | bool | `on` | user | - | Authorizes a selected resident plan to load missing columns synchronously. Explicit pin and reload of an existing pin remain authorized while off. |
| `pg_accel.cost_multiplier` | float | `1.0` | user | `0.1..10.0` | Cost multiplier only for resident generic aggregate candidates; values above one are more conservative. |
| `pg_accel.log_level` | enum | `notice` | user | `debug,info,notice,warning,error` | Initial per-backend trace filter, sampled when the first Custom Scan executes. Later changes do not rebuild the subscriber; `notice` and `warning` both map to WARN. |
| `pg_accel.assert_dispatch` | bool | `off` | user | - | Reserved no-op compatibility setting; it changes neither planning nor execution. |
| `pg_accel.parallel_fused_count` | bool | `off` | superuser | - | Reserved no-op roadmap setting; the parallel fused-count shape remains native. |
| `pg_accel.otel_log_max_mb` | int | `256` | user | `1..65536` | Per-file trace cap in MiB, sampled at trace initialization. A valid `PG_ACCEL_TRACE_FILE_MAX_BYTES` environment value takes precedence. |
| `pg_accel.otel_log_max_rotations` | int | `4` | user | `0..32` | Retained rotated trace files, sampled at trace initialization; `0` discards rotated copies. |
| `pg_accel.fp64_enabled` | bool | `on` | user | - | Deprecated no-op compatibility flag; it does not disable fp64 planning or execution. |
| `pg_accel.soft_fp64_cost_multiplier` | float | `32.0` | user | `1.0..64.0` | Extra planner cost for fp64 work when the device lacks native fp64. |

Registration and authoritative descriptions live at
`pg_accel/src/engine/gucs.rs:105-265` and
`pg_accel/src/lib.rs:260-283`.

## Diagnostics

Run diagnostics in the same backend that planned and executed the query because
most counters and residency entries are backend-local:

```sql
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits() ORDER BY name;
SELECT * FROM pg_accel_resident_status();
SELECT pg_accel_resident_live_bytes();

SELECT * FROM pg_accel_stats();
SELECT * FROM pg_accel_gpu_failures();
SELECT pg_accel_kernel_executions();
SELECT pg_accel_planner_overhead_us();
SELECT pg_accel_planner_fast_decline_count();
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_planner_rejection_count('no_gpu_resident_pipeline');
```

`pg_accel_reset_stats()` resets the resettable per-backend counters and planner
rejection state. The kernel-execution counter is monotonic and should be proved
by a before/after delta; see `pg_accel/src/engine/stats.rs:487-525`.

## Benchmarks

The repository does not publish candidate performance numbers as current
results. Use the harness to produce evidence for the installed commit:

```bash
cargo run -p pg_accel_bench -- validate --workload grouped_agg
cargo run -p pg_accel_bench -- setup --workload grouped_agg --rows 1000000 --seed 42 \
  --connection "host=localhost dbname=postgres"
cargo run -p pg_accel_bench -- run --workload grouped_agg --iterations 10 --warmup 5 \
  --seed 42 --timing both --cache-mode both --capture-plans \
  --connection "host=localhost dbname=postgres"
```

A benchmark workload can intentionally prove native decline; its existence is
not evidence that the production planner selects it. See
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) for the evidence contract and
[docs/EXPLAIN_EXAMPLES.md](docs/EXPLAIN_EXAMPLES.md) for plan interpretation.

## Development references

- [ARCHITECTURE.md](ARCHITECTURE.md): resident-v2 planner, descriptor, and
  residency contracts.
- [docs/ADAPTER_GUIDE.md](docs/ADAPTER_GUIDE.md): current adapter metadata and
  the work required beyond registration.
- [docs/olap-abi.md](docs/olap-abi.md): grouped-aggregate kernel ABI notes.
- [CONTRIBUTING.md](CONTRIBUTING.md): contribution workflow.

## License

Released under the [PostgreSQL License](LICENSE).

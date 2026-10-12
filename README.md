# pg_accel

`pg_accel` is a PostgreSQL Custom Scan extension for GPU-resident query
execution. The current production planner surface is intentionally narrower
than the kernel library: it can select a childless resident aggregate plan for
covered reducing, grouped-aggregate, and star-aggregate SQL shapes, plus the
qualified childless resident raster transform. Other kernel families remain
unavailable to normal planning until they have a complete resident pipeline.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yocontra/pg_accel/ci.yml?label=CI)](https://github.com/yocontra/pg_accel/actions)

## Build from source

Tagged releases publish validated tar archives for PostgreSQL 18 and the
PostgreSQL 19 beta compatibility target; pg_accel is not distributed through an
OS package manager. A development build requires Rust, pgrx, CMake, a C/C++
toolchain, and the AdaptiveCpp commit pinned by [`.acpp-version`](.acpp-version).
The setup and CI recipes read that file directly; a branch name alone is not
sufficient provenance.

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

PostgreSQL 18 is the default extension target. PostgreSQL 19 beta archives are
preview compatibility artifacts, not a stable-PostgreSQL support promise. A
build without a usable GPU can load the extension, but the planner keeps queries
on PostgreSQL-native plans. CUDA/NVIDIA validation is owner-deferred in
[issue #4](https://github.com/yocontra/pg_accel/issues/4); this document makes
no support claim for it.

### Install a release archive

Download the archive and adjacent `.sha256` file for the target PostgreSQL
major and platform from the GitHub release. For example, an Apple Silicon PG18
release uses `pg_accel-pg18-macos-arm64.tar.gz`; the generic Linux archive uses
`pg_accel-pg18-linux-x86_64.tar.gz`. Verify both the outer archive and its
complete inner payload before installing:

```bash
archive=pg_accel-pg18-macos-arm64.tar.gz
shasum -a 256 -c "$archive.sha256"       # use sha256sum -c on Linux
tar -xzf "$archive"
cd pg_accel-pg18
shasum -a 256 -c SHA256SUMS              # use sha256sum -c on Linux
./install.py --pg-config /absolute/path/to/pg_config
```

The installer validates the PostgreSQL major, package topology, runtime
dependencies, checksums, and destination paths before writing. Use an account
with permission to write the directories reported by `pg_config`; do not copy
individual files out of the archive by hand. On macOS, install the documented
`llvm@20` and `libomp` runtime prerequisites first.

Add `pg_accel` to `shared_preload_libraries`, restart PostgreSQL, and verify the
loaded package in the target database:

```sql
CREATE EXTENSION pg_accel;
SELECT pg_accel_version();
SELECT count(*) FROM pg_accel_stats();
```

Maintainers can exercise the same checksum/staged-install/isolated-cluster path
against an already-built archive with `just package-smoke 18`; the hosted
release matrix runs it for PG18 and PG19 on both macOS arm64 and Linux x86_64.
Each archive's platform-qualified `.smoke.txt` result is attached to the tagged
release, avoiding ambiguous evidence between operating systems.

For source development, `just install-pg-accel` starts its local PostgreSQL
server with a soft open-file limit of at least 4096 and records it in install
provenance. Larger limits are preserved; an insufficient hard limit stops
installation before changing the server. Raising only the client shell's limit
does not change an already-running server's inherited limit. Separately managed
PostgreSQL services need their own resource-limit configuration.

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
`yocontra/soft-fp` tag `v2.0.1`, discovers its unified `soft_fp` package
with the binary64 compatibility target, and feeds AdaptiveCpp the generated
feature contract, semantic definitions, and deterministic source manifest. The
Metal configuration
retains soft-fp's subnormal-preserving compatibility/native ABI surface and
zero-cost disabled exception environment; binary128 and binary256 are
deliberately omitted because pg_accel's PostgreSQL `float8` ABI has no
wider-format carrier. Soft-fp 2.0.1 natively emits its integer-derived infinity
without C++ global constructors, so pg_accel consumes a pristine tagged
checkout. Setup applies the tracked patches under `patches/adaptivecpp/` plus a
conditional `metal-cpp` compatibility edit; the result is the exact AdaptiveCpp
fork commit plus reviewed repository patches.
`scripts/setup_acpp.sh` records the resolved commit, backend, compiler paths,
CMake arguments, configured soft-fp package contract, soft-fp64 revision, and
dependency Git status in
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
`pg_accel/src/tests/mod.rs:599-654`.

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
`pg_accel/src/engine/ffi/custom_scan/explain.rs:44-151`; the aggregate and raster
method tables are defined and registered at
`pg_accel/src/engine/ffi/custom_scan/mod.rs:377-448`.

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
`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:594-639`.

## Capability matrix

Kernel or bridge presence is not equivalent to production planner selection.
The normal upper-path hook injects the generic aggregate and qualified raster
candidates at `pg_accel/src/engine/ffi/planner_hooks/mod.rs:221-251`. Only
aggregate and raster Custom Scan method tables are registered. Base scans,
row-returning joins, sorts, windows, and standalone function/SRF shapes remain
native and have no registered executor.

| Capability | Implementation surface | Production planner | Current boundary |
|---|---|---|---|
| Resident reducing or grouped aggregate | Present | Selectable | Covered `AggQuerySpec` shapes become `Custom Scan (GpuAccelAgg)` after shape, type, residency, device, and cost gates. This includes qualified 250K–1M nullable bool-key / distinct nullable bool, int2, int8, float4, float8, date, timestamp, or timestamptz `COUNT(column)` envelopes, plus exact SUM/AVG/COUNT for up to four independent nullable int2/int4 fact values (up to three with `COUNT(*)`), globally or grouped by up to three supported nullable dictionary keys (including text, date, integer, floating-point and boolean keys). Dense dictionary capacity remains bounded. Integer SUM widens to int8 and AVG is finalized with PostgreSQL NUMERIC division. Independent int4 interval FILTER predicates can reference a raw value column already represented by an integer measure state, including single-column AND/OR/NOT and constant IN/NOT IN/ANY/ALL predicates when their exact NULL-preserving normalization fits one to four intervals. Filtered COUNT(*) and COUNT(int2/int4) can share these states or execute independently, including global conditional aggregates. Broader aggregate-local predicates and Boolean filtered counts use the separate typed scalar lane described below; other types and joins remain outside this expansion. Nonvolatile `HAVING` can execute in PostgreSQL after GPU aggregation, including native ordering/limits. Hidden HAVING aggregates and grouping/sort keys are materialized internally when the complete shape passes the same kernel gates; they need not appear in SQL output. Stored generated columns are physical base attributes and remain selectable when their types and aggregate shape pass those same gates. |
| Scalar row or aggregate-local predicate inside a resident aggregate | Present | Selectable for exact qualified shapes | Single-column INT4 predicates retain their interval fast path. The typed scalar lane accepts multi-column INT4/Boolean/DATE WHERE and aggregate FILTER expressions, including CASE/IN and filtered COUNT(*), with lazy argument evaluation. WHERE removes rows before group activity; aggregate FILTER does not. A typed WHERE carrier consumes a state within the four-state cap. Supported SQL HAVING is evaluated separately by PostgreSQL. Joined, NUMERIC-state and broader-type scalar predicates remain native. |
| Resident star join plus aggregate | Present | Selectable | The join is represented inside one childless aggregate descriptor; this is not a row-returning join path. Qualified counted-dimension global `COUNT(*)` converts duplicate dimension matches into exact device-side weights and preserves unmatched/NULL fact-key semantics. |
| H3-derived group key inside a resident aggregate | Present | Selectable | `h3_cell_to_parent(cell, resolution), COUNT(*)` supports constant resolutions 0–15 under the normal type, residency, cost and memory gates. Compact COUNT capacity is bounded by input rows, the H3 resolution universe, hardware-derived memory budgets and a 16Mi-group ceiling. Large results use parallel device finalization; reusable parent artifacts retain validated group counts to right-size later allocations. Native HAVING/order/limits can compose with this lane. `h3_latlng_to_cell` descriptor support is not yet reachable from production planning. |
| PostGIS spatial filter inside a resident aggregate | Present | Selectable | Resident point/Polygon or MultiPolygon `ST_Intersects` (either orientation), `ST_Contains(area, point)`, `ST_Within(point, area)`, `ST_Covers(area, point)` and `ST_CoveredBy(point, area)` inside ungrouped `COUNT(*)` or the exact integer/decimal/mixed rollups described below, including per-component holes and exact PostGIS boundary rechecks. Requires matching fixed nonzero SRIDs, a nonempty exact resident snapshot, and cost/memory/work gates. Total coordinate complexity bounds chunk size. Covers/CoveredBy include boundaries; their uncertain rows use the original PostGIS predicate for exact rechecks. Spatial joins are not yet selectable. |
| Standalone PostGIS or H3 function/SRF | Aggregate primitives and adapter registry metadata remain; standalone executor removed | Not selectable | PostgreSQL executes standalone calls. Function and target-list SRF hooks record `no_gpu_resident_pipeline`. |
| Base scan, WHERE filter, or projection | No registered Custom Scan executor; host-staged implementation retired | Not selectable | PostgreSQL executes the base path. The production hook observes and records declines but injects no scan CustomPath. |
| Row-returning hash or inequality join | No registered row-returning executor; host-staged implementation retired | Not selectable | PostgreSQL executes the join. Resident join membership exists only inside childless `GpuAccelAgg` descriptors. |
| Standalone sort or top-k | Kernel and executor removed; numeric strategy tag and descriptor retained only for fail-closed wire decoding | Not selectable | All standalone sort shapes remain PostgreSQL-native. Full-output sorts record `sort_heap_full_output`; bounded top-k records `sort_standalone_topk_no_gpu_kernel`. |
| Window | Kernel and executor removed; numeric strategy tag and descriptor retained only for fail-closed wire decoding | Not selectable | Full-output and reducing window SQL remains PostgreSQL-native and records `no_gpu_resident_pipeline`. |
| Raster | Registered childless resident executor | Selectable | Production selection is limited to the qualified three-argument resident `ST_Reclass` shape, canonical singular integer rules, band one, `8BUI` output, and the released pixel-count envelope; other raster shapes remain native. |

### 1.0 scope and explicit deferrals

The capability matrix above is the complete 1.0 planner-support claim. General
expression, aggregate/type, membership, H3, spatial, raster, and
cardinality-reducing sort/window expansion is tracked in
[issue #1](https://github.com/yocontra/pg_accel/issues/1). Those adjacent shapes
remain PostgreSQL-native until each exact lane has correctness, failure,
dispatch, and matched-performance evidence; the presence of adapter metadata,
wire tags, test scaffolding, or a kernel is not support.

The release also makes no 1B-row scale claim; that optional certification is
tracked in [issue #2](https://github.com/yocontra/pg_accel/issues/2). A
privileged OS page-cache-purge certification is separately tracked in
[issue #3](https://github.com/yocontra/pg_accel/issues/3) and is not inferred
from the mandatory warm matrix or project-owned JIT/archive cold-start proof.
CUDA/NVIDIA and PG-Strom remain owner-deferred and unvalidated as described in
[issue #4](https://github.com/yocontra/pg_accel/issues/4).

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

NUMERIC columns can be pinned losslessly as native-endian decimal digits,
including arbitrary precision/scale, NaN, infinities and NULLs. Warm resident
SUM/AVG now use exact GPU decimal accumulation, globally or with up to three
ordinary dictionary grouping keys. Up to four value states can share
SUM/AVG/COUNT outputs (three with COUNT(*)). PostgreSQL finalizes AVG without a
floating-point conversion. Admission checks exact digit metadata, memory,
dense capacity, row count and cost; the current decimal row floor uses the
grouped-aggregate threshold even for global queries. Independent decimal
COUNT states read NULL status without computing an unused SUM or allocating digit buckets,
and can compose with SUM/AVG states. Exact NUMERIC and int2/int4 SUM/AVG/COUNT
states can share one dispatch, including independent filters and conditional
COUNT(*). NUMERIC grouping, joins and expressions remain native. Aggregates
in this family may use independent FILTER predicates normalized to
exact ranges over a fact-table INT4 column; other FILTER shapes remain native.
Query-level WHERE also accepts bounded single-column INT4 Boolean/IN ranges
and stored Boolean columns. Rejected rows do not create groups or contribute
to COUNT(*). A new integer WHERE input uses one hidden value state within the
same four-state limit; Boolean masks do not consume an aggregate state.
Qualified point/constant-Polygon or MultiPolygon predicates can also feed
these decimal/mixed rollups and integer-only SUM/AVG/COUNT, with exact PostGIS boundary rechecks before
aggregation. Intersects, Contains/Within and Covers/CoveredBy retain their
existing orientation, SRID, topology, residency and work limits. Spatial joins
remain outside this expansion. Integer-only rollups, including spatial rollups,
accept independent INT4 aggregate FILTER inputs even when those columns appear
in no aggregate. Predicate-only FILTER columns consume no aggregate state.
Integer-only rollups also accept query-level WHERE using bounded single-column
INT4 ranges or stored Boolean masks. Predicate-only WHERE columns consume a
hidden state within the same four-state limit; masks need no extra state.
WHERE rejection excludes rows from groups and COUNT(*) before aggregate FILTER.
Integer-only rollups also support MIN/MAX independently or alongside
SUM/AVG/COUNT, including independent FILTER inputs and spatial restrictions.
Extrema retain int2/int4 result types, return NULL for empty/all-NULL inputs,
and share a state with other outputs over the same expression and filter.
Atomic and bounded partial execution support these combinations; INT8/NUMERIC
extrema and extrema mixed with decimal states remain native.
Two direct INT4 columns can feed checked `+`, `-` and `*` measures across
SUM/AVG/COUNT/MIN/MAX, including mixed measures and supported FILTER/WHERE/geo
restrictions. Overflow remains an error even for `COUNT(a*b)`; a NULL operand
makes the expression NULL. Independent arithmetic FILTER inputs may need a
hidden raw-column state within the four-state cap. A separate fact-only dense
scalar lane supports nested INT4 arithmetic (including division/remainder),
searched/simple CASE, COALESCE, literals, Boolean/NULL expressions and bounded
constant INT4/Boolean IN/NOT IN lists (including NULL/empty-array semantics).
Dynamic arrays and other ANY/ALL comparisons remain native. INT4 results
feed SUM/AVG/COUNT/MIN/MAX; Boolean results feed COUNT. Typed DATE comparisons,
DATE±INT4, INT4+DATE and DATE-DATE also compose with CASE/COALESCE and FILTER.
DATE results feed COUNT/MIN/MAX; differences feed integer aggregates. Strict
NULL propagation, infinity and finite date bounds follow PostgreSQL; date
errors retain SQLSTATE 22008. DATE arrays, timestamp/interval operations and
casts remain native in this lane. Identical expression and
FILTER combinations share one evaluation state. Lazy branches preserve error
timing; division by zero/overflow propagate even for COUNT when PostgreSQL
retains its expression evaluation. PostgreSQL's own COUNT simplifications are
honored. This lane
also supports aggregate FILTER with multi-column INT4 comparisons/arithmetic,
Boolean/NULL expressions, CASE and constant IN/NOT IN. The filter runs before
the argument; false/UNKNOWN skips argument evaluation without removing its
group. Filtered COUNT(*) counts accepted rows independently of argument NULLs.
The existing interval fast path is retained, and the four-state, type, memory,
cost and relation-occurrence gates still apply. Fact-only dense integer/scalar
rollups also accept typed Boolean WHERE programs over INT4/BOOL/DATE columns,
including multi-column comparisons, CASE/COALESCE and bounded constant membership.
NULL-safe `IS [NOT] DISTINCT FROM` over INT4/BOOL/DATE and all six Boolean
`IS [NOT] TRUE/FALSE/UNKNOWN` tests are supported in typed expressions, FILTER
and WHERE, including qualified spatial conjunctions. Comparison operands still
execute before the non-NULL result is formed; NULL does not hide their errors.
PostgreSQL's own earlier simplifications remain authoritative—for example,
PG19 can replace COUNT of a provably non-NULL expression with COUNT(*).
Typed INT4/BOOL/DATE `NULLIF`, `GREATEST` and `LEAST` also compose with
aggregates, FILTER/WHERE, CASE and qualified spatial rollups. This includes
zero-safe ratios such as `price/NULLIF(quantity,0)` and row-wise bounds.
Extrema ignore NULL operands (all NULL yields NULL), but every argument still
executes; neither a NULL nor an earlier extremal value hides later errors.
These rollups also work through derived tables, inline CTEs and views that
PostgreSQL fully flattens into supported base-table operations, including geo
filters and nested report aliases. Live subplans, materialized CTEs, security
barriers and RLS remain native. Definer views accelerate only when their base
permissions use the current role; changing roles invalidates that planning
assumption. This is flattened-query support, not general subquery execution.
WHERE runs before group activity and aggregate-argument evaluation; FALSE/NULL
removes the row entirely. Implicit conjuncts follow PostgreSQL's stable
per-tuple-cost order, preserving cheap guards before throwing expressions;
nested CASE/Boolean order is unchanged. A matching Boolean scalar state carries
the predicate,
consuming one of four states unless it already exists. When another value state
exists, the predicate carrier has no row worker: its COUNT is derived from
selected rows during the checked merge. A sole carrier retains its worker.
Costs include predicate evaluation by each remaining value-state worker.
Qualified point/polygon conjunctions also accept these typed predicates: the
existing exact SQL spatial mask is read inside the WHERE program in native
qualifier order. Scalar errors outside the polygon are not silently suppressed;
cheap guards and CASE retain their short circuits. Spatial OR, scalar joins
and NUMERIC states remain outside this lane.
This lane
also shares COUNT(*) with a requested scalar expression's COUNT lane when every
successful path is proved non-NULL and the FILTER is identical. Expression
evaluation and errors are retained; nullable expressions keep a separate count.
This can fit four distinct scalar expressions plus COUNT(*) in four states. It
requires exact loaded resident evidence before normal cost admission; refresh
after invalidating DML when planning a new query. Existing selected prepared
plans revalidate and reload at execution. Qualified resident point/polygon
spatial masks also compose with these scalar expressions, retaining exact
boundary/hole rechecks before expression evaluation. Simple CASE evaluates its
INT4/Boolean/DATE test once, including nested cases. Casts, domains,
arbitrary functions, scalar join compositions and floating/decimal
arithmetic remain outside this scalar lane.
Spatial WHERE can also be conjoined with exact single-column INT4 range/IN/NOT
predicates in these integer/decimal rollups. Both predicates execute before
grouping, using the spatial mask and scalar range in one aggregate dispatch.
Predicate-only scalar inputs remain within the four-state bound. Spatial OR,
and predicates involving multiple scalar columns remain native; single-column
NULL-accepting predicates are supported.
Cold decimal inputs require an explicit pin before admission.

**Correctness boundary:** production resident aggregate execution has no CPU
fallback; an uncertain aggregate result or GPU failure raises an error after
selection. The spatial predicate producer separately retains exact GSERIALIZED
values and rechecks uncertain classifications with PostGIS on the PostgreSQL
backend thread before patching the device mask consumed by aggregation. This
is an explicit predicate stage, not an aggregate-error fallback. See
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
| `pg_accel_resident_representation_status()` | set of rows | Show source, encoded, retained-exact, transient, construction-peak, and lossless compact-representation byte projections for each loaded relation. |
| `pg_accel_resident_ingestion_status()` | set of rows | Show table-AM scan batches, preflight/scan/encode/copy timing, total load time, and the publication strategy. |
| `pg_accel_resident_representation_decisions()` | set of rows | Show which compact encodings and ingestion changes were evaluated and why they remain enabled or deferred. |
| `pg_accel_resident_live_bytes()` | bigint | Show the exact cluster-wide resident byte ledger at that instant. |

The SQL wrappers and status tuple are defined at
`pg_accel/src/engine/residency/store.rs:3595-3715`. Pins do not bypass
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
| `pg_accel.planner_profiling` | bool | `off` | user | - | Enables planner-hook monotonic-clock reads and elapsed-time counters; call and decline counters remain active while off. |
| `pg_accel.execution_profiling` | bool | `off` | user | - | Enables descriptor-stage monotonic-clock reads for plan, artifact, binding, allocation, kernel, and tuple-materialization attribution. |
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
SELECT * FROM pg_accel_resident_representation_status();
SELECT * FROM pg_accel_resident_ingestion_status();
SELECT * FROM pg_accel_resident_representation_decisions();
SELECT pg_accel_resident_live_bytes();

SELECT * FROM pg_accel_stats();
SELECT * FROM pg_accel_gpu_failures();
SELECT pg_accel_kernel_executions();
SELECT pg_accel_planner_overhead_us();
SELECT pg_accel_planner_fast_decline_count();
SELECT * FROM pg_accel_planner_stage_stats() ORDER BY stage;
SELECT * FROM pg_accel_descriptor_stage_stats() ORDER BY stage;
SELECT * FROM pg_accel_artifact_lifecycle_stats();
SELECT pg_accel_last_artifact_policy();
SELECT * FROM pg_accel_grouped_kernel_mode_stats() ORDER BY mode;
SELECT * FROM pg_accel_grouped_workspace_pool_stats();
SELECT * FROM pg_accel_grouped_output_pool_stats();
SELECT * FROM pg_accel_grouped_runtime_stats();
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_planner_rejection_count('no_gpu_resident_pipeline');
```

`pg_accel_planner_stage_stats().elapsed_us` and
`pg_accel_planner_overhead_us()` collect elapsed samples only after
`SET pg_accel.planner_profiling = on`. Planner-stage call and decline counters
remain available while profiling is off.

`pg_accel_descriptor_stage_stats().elapsed_us` and its call counters collect
only after `SET pg_accel.execution_profiling = on`; the disabled path performs
no clock reads or atomic updates.

`pg_accel_reset_stats()` resets the resettable per-backend counters and planner
rejection state. The kernel-execution counter is monotonic and should be proved
by a before/after delta; see `pg_accel/src/engine/stats.rs:487-525`.

## Testing against PostgreSQL itself

The extension test suite is supplemented by PostgreSQL's own regression and
isolation schedules. Run the compatibility gate for one supported major with a
new evidence directory:

```bash
just upstream-pg-tests 18 artifacts/upstream-postgresql-pg18
```

Or run every supported major:

```bash
just upstream-pg-tests-matrix artifacts/upstream-postgresql
```

Each gate builds the configured official PostgreSQL source and runs both
schedules twice: once pristine and once with the exact installed `pg_accel`
module preloaded into every temporary-server backend. Any test diff, crash,
hang, or nonzero exit fails the command. The evidence bundle retains command
logs, server logs, expected/actual diffs and results, PostgreSQL tarball and
configure identity, source and module hashes, Git status/diff, and a complete
`SHA256SUMS` seal. CI runs this gate independently for PostgreSQL 18 and 19.

`just extension-catalog-test 18` (or `19`) separately exercises preload-only,
CREATE/DROP EXTENSION, rollback and prepared plans in an owned temporary
database. Preloading alone does not authorize GPU paths in a database without
the extension's SQL installation. The catalog test also requires normal GPU
dispatch when a device is available and the extension is installed.

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

For real-data characterization, the opt-in [NYC taxi corpus](benchmarks/corpora/nyc-yellow-2015-01/README.md)
preserves original pickup coordinates and exact monetary values. It includes
H3 grouping, complete JFK multipolygon queries with and without GiST, and fare
rollups; unsupported queries remain visible rather than being omitted.

The [local exact-NUMERIC characterization](benchmarks/evidence/numeric-20260929/README.md)
records ordinary-planner taxi and decimal rollups, exact-result and dispatch
evidence, loading costs, and shared-host limitations. These warm-resident
measurements are not a cold-query or whole-system performance claim.
The [independent decimal FILTER characterization](benchmarks/evidence/numeric-filter-20260929/README.md)
adds conditional rollups with exact results and verified GPU execution, while
retaining the initial native-only run and separate loading costs.
The [independent decimal COUNT characterization](benchmarks/evidence/numeric-count-20260929/README.md)
adds count-only states without decimal sum arithmetic, including conditional
counts composed with SUM/AVG, and records matched warm measurements and tests.
The [mixed integer/decimal characterization](benchmarks/evidence/numeric-mixed-20260929/README.md)
adds independent integer measures and conditional counts alongside decimal
aggregates, including a complete-month NYC passenger/fare/distance rollup.
The [filtered decimal/mixed characterization](benchmarks/evidence/numeric-where-20260929/README.md)
adds query-level integer ranges and stored Boolean masks, composed with
independent aggregate filters, plus a filtered NYC fare/distance rollup.
The [spatial decimal/mixed characterization](benchmarks/evidence/spatial-numeric-20260929/README.md)
combines exact area predicates with these rollups, including a JFK
fare/distance/passenger query and prepared-query empty-input verification.
The [global integer characterization](benchmarks/evidence/global-integer-20260929/README.md)
adds SUM/AVG and conditional counts without grouping keys, with exact empty-input
semantics, verified GPU execution and separate first-use costs.
The [spatial integer characterization](benchmarks/evidence/spatial-integer-20260929/README.md)
adds area-filtered integer-only rollups and conditional measures, including
complete-month JFK passenger totals and exact averages without decimal inputs.
The [integer WHERE characterization](benchmarks/evidence/integer-where-20260929/README.md)
adds query-level ranges and Boolean masks, including hidden predicate columns,
conditional rollups and a complete-month vendor-filtered NYC passenger query.
The [independent integer FILTER characterization](benchmarks/evidence/integer-filter-inputs-20260929/README.md)
removes the requirement to aggregate predicate columns, including conditional
NYC passenger totals with and without a JFK spatial restriction.
The [spatial plus scalar WHERE characterization](benchmarks/evidence/spatial-scalar-20260929/README.md)
combines exact area predicates with INT4 ranges, including complete-month
JFK/vendor passenger and fare/distance queries and separate first-use costs.
The [spatial scalar-program characterization](benchmarks/evidence/scalar-geo-20261005/README.md)
adds nested CASE/COALESCE arithmetic and Boolean COUNT inside qualified spatial
rollups. Global/grouped full-scan benchmarks win warm at 1M/2M rows; substantial
host variance, slower bounded grouping, and loading/refresh losses are retained.
The [bounded aggregate optimization](benchmarks/evidence/scalar-bounded-20261006/README.md)
then reduces the 2M grouped spatial median from 66.28 to 8.42 ms by reusing atomic
batches under an exact prefix-safety proof. A non-spatial 2M grouped COALESCE
rollup measures 6.49 ms versus native 64.01 ms. Variance and cold-path losses
remain explicit; these are local warm-workload results, not universal ratios.
The [simple CASE characterization](benchmarks/evidence/simple-case-20261006/README.md)
adds once-evaluated INT4/Boolean cases to OLAP and geo rollups. Removing redundant
stack operations enables a conditional-pricing benchmark to select normally:
5.2–7.0x warm speedups at 250K–2M rows. At that stage a more complex CASE probe remained native;
host variance, first-use dispatch warnings and loading/refresh losses remain
explicit in the evidence.
The [scalar instruction fusion follow-up](benchmarks/evidence/scalar-fusion-20261006/README.md)
removes intermediate equality/branch and literal-COALESCE work across OLAP and
geo. At that stage complex CASE selects at 250K rows (2.38 vs 18.31 ms); its 1M/2M
scales remain native. Single-WHEN CASE measures 3.49 vs 55.83 ms at 1M, and the
2M full-scan geo rollup measures 9.12 vs 183.05 ms. These noisy local warm ratios
are not a matched old/new comparison; refresh still loses and a 5.2-second
first-use dispatch warning remains recorded.
The [constant-membership characterization](benchmarks/evidence/scalar-membership-20261006/README.md)
adds bounded INT4/Boolean IN/NOT IN inside aggregate expressions, including
computed inputs, NULL/empty arrays and geo composition. At 2M rows the new
conditional-membership OLAP query measures 9.73 vs 147.65 ms, and its full-scan
geo counterpart 7.64 vs 176.01 ms. All registered scales select normally with
exact oracles and no fallback. These are local warm ratios; loading and slower
large/spatial refresh paths remain separate. Fresh pristine/loaded PostgreSQL
18/19 regression and isolation schedules pass all 1448 scheduled executions.
The [arithmetic fusion follow-up](benchmarks/evidence/scalar-arithmetic-20261006/README.md)
removes literal-RHS and coalesced-RHS interpreter work. The formerly native
complex CASE now selects at 1M/2M: 4.70/8.46 ms versus parallel PostgreSQL
38.64/66.75 ms, without changing cost coefficients or admission thresholds.
All 16 measured OLAP/geo scales select with exact oracles and zero fallback;
2M geo scalar and membership rollups measure 7.54/6.68 ms versus
121.46/131.06 ms. These are noisy local warm ratios, not matched old/new binary
comparisons; every refresh-inclusive probe still loses.
The [typed aggregate FILTER follow-up](benchmarks/evidence/scalar-filter-20261006/README.md)
adds multi-column INT4/Boolean predicates, CASE/IN and filtered COUNT(*), with
lazy argument evaluation and preserved rejected-row groups. At 2M rows its
spatial benchmark selects and measures 6.45 vs 117.87 ms. The standalone
public-table OLAP benchmark remains native at every scale; its below-parity
native results are retained. Warm geo gains do not remove loading/refresh
losses, and predicate-work sharing/cost qualification remains open.
The [typed DATE follow-up](benchmarks/evidence/scalar-date-20261006/README.md)
adds checked date arithmetic/comparisons to scalar OLAP and geo aggregates,
including DATE COUNT/MIN/MAX and date-difference integer rollups. At 2M rows,
the new date-span workloads measure 6.40 vs parallel PostgreSQL 41.97 ms for
OLAP and 6.85 vs 100.76 ms for geo. All five registered scales select normally
with exact oracles and zero fallback. These are local warm-resident results;
every refresh-inclusive probe loses, and date bucketing/casts remain open.
The [typed WHERE follow-up](benchmarks/evidence/scalar-where-20261006/README.md)
adds multi-column INT4/BOOL/DATE predicates to fact-only dense rollups, with
row rejection before group activity and lazy argument evaluation. PG18/19
each select 24/28 semantic-matrix shapes with four explicit cost declines.
The public-table benchmark remains native at 250K/1M/2M; no GPU speedup is
claimed. Predicate work is still repeated across value-state workers.
The [WHERE carrier optimization](benchmarks/evidence/where-carrier-20261006/README.md)
removes the redundant predicate-state row worker when another value state exists.
Both PostgreSQL versions now select all 28 semantic-matrix shapes, with checked
COUNT derivation across chunks. The same public-table benchmark still stays
native at all three scales; this is not yet a measured GPU speedup.
The [integer extrema characterization](benchmarks/evidence/integer-extrema-20260929/README.md)
adds independent MIN/MAX and combined MIN/MAX/SUM/AVG/COUNT, including
complete-month passenger summaries with and without a JFK restriction.
The [bigint aggregation characterization](benchmarks/evidence/bigint-numeric-20260929/README.md)
extends exact SUM/AVG beyond int64 range using raw bigint resident buffers,
including mixed decimal/integer states, independent filters and geo rollups.
The [NULL-aware predicate characterization](benchmarks/evidence/null-predicates-20260929/README.md)
adds INT4 IS NULL and NULL-or-range filters, correct conditional COUNT(*) and
their composition with integer/decimal aggregates and exact spatial WHERE.
Warm measurements and slower first-use refresh/classification are separate.
The [device-mask publication characterization](benchmarks/evidence/spatial-mask-publication-20260929/README.md)
removes the geo mask's host round trip and records matched preparation costs,
memory/transfer savings and the remaining first-use losses.
The [computed integer characterization](benchmarks/evidence/computed-integer-20260929/README.md)
adds checked two-column arithmetic across SUM/AVG/COUNT/MIN/MAX, including
filters and geo restrictions. It records global warm wins, first-use losses,
and a grouped query that remains native under normal cost-based planning.
The [bounded atomic-count characterization](benchmarks/evidence/fused-integer-20260929/README.md)
records fewer contended atomic updates, local warm improvements and remaining
first-use losses. An unsuccessful row-fusion experiment is retained as evidence,
not as a production optimization.
The [Covers/CoveredBy characterization](benchmarks/evidence/covers-20260929/README.md)
records boundary-inclusive point/area counts, prepared-plan lifecycle checks,
and warm-mask measurements alongside their slower fresh-classification costs.

## Development references

- [ARCHITECTURE.md](ARCHITECTURE.md): resident-v2 planner, descriptor, and
  residency contracts.
- [docs/ADAPTER_GUIDE.md](docs/ADAPTER_GUIDE.md): current adapter metadata and
  the work required beyond registration.
- [docs/olap-abi.md](docs/olap-abi.md): grouped-aggregate kernel ABI notes.
- [CONTRIBUTING.md](CONTRIBUTING.md): contribution workflow.
- [SUPPORT.md](SUPPORT.md): supported environments and failure-reporting data.
- [SECURITY.md](SECURITY.md): private vulnerability-reporting policy.

## License

Released under the [PostgreSQL License](LICENSE).

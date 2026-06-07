# pg_accel 1.0 Release Checklist

Use this as the tag PR gate for `v1.0.0-rc1` and `v1.0.0`. The tag PR must
include this checklist in its body, with every required item checked and linked
to the commit SHA, CI run, benchmark artifact, release artifact, or documented
deferral that proves it. Do not cut the tag while any required item is
unchecked.

## Evidence Rules

- Evidence SHA: commit that closed the item, not the commit that checked this box.
- CI evidence: link to the GitHub Actions run for the release candidate commit.
- Bench evidence: link to the committed or uploaded benchmark report artifact.
- Correctness evidence: link to the diff report, SQL output, kernel test log, or CI artifact.
- Hardware evidence: name the runner or host class used for Metal/CUDA/ROCm/L0.
- Deferrals: link to the release-note section and issue that explicitly scopes the item out of 1.0.

## Phase 0 - Evidence, Provenance, And GPU-Only Guardrails

- [ ] Benchmark artifacts prove the exact binary, extension version, GUC snapshot, device metadata, selected plan, dispatched GPU kernels, returned row counts, correctness diffs, telemetry limits, and crash logs for every release benchmark run. Evidence artifact: `<sha-or-url>`.
- [ ] Reports separate `plan_selected`, `gpu_kernel_dispatched`, `gpu_resident_pipeline`, `function_kernel_count`, `rows_returned_to_cpu`, and `planner_declined`; no benchmark conclusion is based on placeholder-GUC or unloaded-extension runs. Evidence artifact: `<sha-or-url>`.
- [ ] GPU-only planner guardrails decline cases where launch, JIT, materialization, soft-fp64, transfer, duplicate worker work, or output reconstruction makes GPU execution slower; EXPLAIN shows the decline reason and no pg_accel plan label for declined cells. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Current winning and no-dispatch lanes are represented in durable artifacts, including `gpu_nlj_between @ 50K` (`benchmarks/artifacts/crash-repro-1779092055`) and H3 bulk artifacts (`benchmarks/artifacts/crash-repro-1779159425`, `benchmarks/artifacts/crash-repro-1779159454`). Release evidence artifact: `<sha-or-url>`.

## Phase 1 - Stop All Backend Crashes Before Re-Entry

- [ ] Previously known crash families are either planner-gated or repaired: grouped aggregation Metal argument-buffer, hash join Metal host-pointer probe, spatial high-capture lambda, and parallel partial `SUM(bigint)`. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] No selected GPU benchmark cell disconnects PostgreSQL or records backend crashes; known no-crash repro rows remain covered (`crash-repro-1779092055`, `1779092044`, `1779092148`, `1779093355`, `1779093366`, `1779093376`, `1779093387`). Release evidence artifact: `<sha-or-url>`.
- [ ] Any new backend-crashing shape discovered before the tag is added to TODO Phase 1 and either gated before re-entry or blocks the release. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 2 - AdaptiveCpp Runtime, Metal, CUDA, And Fork Stability

- [ ] fp64 soft path uses the pinned AdaptiveCpp fork or newer accepted upstream commits, with fork-local commits listed in release notes. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] fp64 ULP budget, special-value behavior, documented no-fenv-read-back release semantics, ABI value-buffer contract, and `pg_accel.soft_fp64_cost_multiplier` calibration are verified for every shipped fp64 kernel family. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Cold-start, fork, archive-size, SSCP debug, and warning-noise behavior are captured as pass/fail artifacts, including first-dispatch latency per kernel. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Metal and CUDA runtime setup paths are documented with backend metadata and any ROCm/L0 deferrals. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 3 - GPU-Resident Execution Substrate

- [ ] PG-Strom-shaped execution model is represented in EXPLAIN and benchmark artifacts: GPU scan, join, pre-aggregation, expression pushdown, join-side reuse, pruning, and GPU-resident intermediate data where selected. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Expression/filter/projection wrappers over PostgreSQL-native child plans are declined unless GPU dispatch is real, output semantics match PostgreSQL, and selected cells meet benchmark parity. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Aggregate wrappers require attached fact child plans, visible GPU dispatch, correct PostgreSQL semantics, and benchmark parity for selected grouped aggregate cells. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 4 - Core OLAP Coverage And PG-Strom Parity

- [ ] HashAgg, grouped aggregate, COUNT/DISTINCT, reduce, hash join, nested-loop join, sort/top-k/rank, window, SSBM, and expression lanes either dispatch real GPU work with correctness proof or visibly decline. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Sort/top-k/rank/window selected shapes match PostgreSQL semantics for NULLs, duplicates, collation/order-sensitive cases, peer groups, and frames. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Join selected shapes show build-side reuse evidence, correct NULL/anti/semi semantics, and no redundant full join-output reconstruction where the benchmark gate depends on it. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 5 - Geo, H3, Raster, And PostGIS Coverage

- [ ] H3 bulk aggregation has zero h3-pg diff rows, no crashes, consumed outputs, dispatch counters, warm-run threshold evidence, and bounded cold-start cost for every enabled resolution. Existing seed artifacts: `benchmarks/artifacts/crash-repro-1779120959`, `benchmarks/artifacts/crash-repro-1779120976`, `benchmarks/artifacts/crash-repro-1779159425`, `benchmarks/artifacts/crash-repro-1779159454`. Release evidence artifact: `<sha-or-url>`.
- [ ] H3 LATERAL SRF and target-list/multi-SRF paths either dispatch with PostgreSQL-compatible row multiplication, NULL handling, ordinality, and zero h3-pg diffs or visibly decline. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] PostGIS geometry predicates, joins, and constructors match PostGIS native output on exact GPU paths, document uncertain-row recheck behavior, and decline unsupported shapes. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Raster map algebra, clip/reclass, summaries, and multi-band workloads consume computed raster output, match PostGIS raster output within documented pixel tolerances, and dispatch only winning selected sizes. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 6 - Feature Completion And Deferrals

- [ ] SQL operator/function support matrix, unsupported JSON/JSONB or deferred type coverage, type coercion, NULL edge cases, and released GUC docs match the implementation. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] SetOp, RecursiveUnion, merge join, NUMERIC behavior, generated columns, partition/pruning behavior, and any remaining PG-Strom-surface gaps either have GPU/correctness/performance proof or documented planner-decline deferrals. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] AdaptiveCpp rebase/upstream status and any fork-pinned installation burden are explicitly documented for public release. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 7 - Cost Models, Performance Ratchets, And Comparative Benchmarks

- [ ] Benchmark ratchets fail CI when a selected GPU cell regresses below PostgreSQL parallel parity, crashes, silently misses GPU dispatch, or loses expected GPU plan selection. Evidence SHA/CI: `<sha-or-url>`.
- [ ] Per-lane threshold matrices exist for row count, type, cardinality, selectivity, row width, output size, geometry complexity, batch count, H3 operation, and raster operation. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] PostgreSQL native comparison passes: every selected GPU cell in the release matrix is `speedup_x >= 1.0` on M-series and NVIDIA hardware, and every non-selected cell has a visible planner-decline reason. Evidence artifact: `<sha-or-url>`.
- [ ] PG-Strom comparison passes: pg_accel matches or beats PG-Strom for benchmarked PG-Strom-supported OLAP/Geo use cases, or the release is blocked. Evidence artifact: `<sha-or-url>`.

## Phase 8 - Test Coverage, CI, And Stress Gates

- [ ] Coverage reaches at least 90% for pg_accel-owned Rust/C++/SQL behavior; CI publishes coverage artifacts and fails below 90%. Evidence CI/artifact: `<sha-or-url>`.
- [ ] Coverage includes planner hooks, executor state, private-data encoding/decoding, GPU dispatch adapters, SQL extension surfaces, C++ kernels, H3/PostGIS/raster semantics, and benchmark classification. Evidence CI/artifact: `<sha-or-url>`.
- [ ] Metal stress gate passes on M-series hardware with mixed scan, aggregate, join, sort, H3, PostGIS, raster, fork, and cancellation workloads: zero backend crashes, kernel failures, panic-log entries, and resource-leak messages. Evidence artifact: `<sha-or-url>`.
- [ ] CUDA stress gate passes on NVIDIA hardware using the same matrix adjusted only for backend metadata: zero backend crashes, kernel failures, and panic-log entries, with benchmark results satisfying PostgreSQL/PG-Strom gates. Evidence artifact: `<sha-or-url>`.
- [ ] Required CI ship-bar jobs pass on `main` and branch protection requires them: macOS arm64 GPU, Linux x86_64 no-GPU, and optional self-hosted CUDA smoke or documented hardware skip. Evidence CI/settings: `<sha-or-url>`.
- [ ] Release verification matrix passes with artifacts for EXPLAIN audit, correctness diff, benchmark sweep, fork stress, deferred-site audit, and `pg_accel_stats()` sanity. Evidence artifact: `<sha-or-url>`.
- [ ] Release checklist synchronization is complete: this checklist matches TODO release gates, every item has evidence, and the tag PR includes the checked checklist. Evidence PR: `<url>`.

## Phase 9 - Public Release And Installability

- [ ] Fresh-machine smoke passes from a clean clone using public README instructions: install prerequisites, `just setup-gpu-acpp`, package, install, `CREATE EXTENSION`, and run a representative benchmark without manual fixes. Evidence artifact: `<sha-or-url>`.
- [ ] Installable-by-anyone gate passes: PostgreSQL extension package, AdaptiveCpp fork setup, kernel build, SQL/control files, source PostgreSQL/pgrx path, native macOS notes, Linux CUDA notes, and verification command all work from clean machines. Evidence artifact: `<sha-or-url>`.
- [ ] Install provenance confirms the live PostgreSQL backend loads the just-built extension binary and failures produce actionable diagnostics. Evidence artifact: `<sha-or-url>`.
- [ ] Public repository readiness is complete: README, architecture docs, benchmark docs, release notes, license files, contribution guide, security policy, issue templates, reproducible benchmark artifacts, supported hardware, limitations, and failure-reporting docs are published. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Release candidate and final tag artifacts are published: `v1.0.0-rc1`, one-week monitoring notes, `v1.0.0`, release notes, source archive, SQL artifacts, checksums, benchmark artifacts, and install docs. Evidence release: `<release-url>`.
- [ ] Hacker News launch is blocked until the repo is public, installable, benchmark-backed, crash-free on the release matrix, and the post links to install docs, benchmark evidence, PG-Strom comparison, supported hardware, limitations, and issue tracker. Evidence URL: `<url>`.

## Maintainer Sign-Off

- [ ] Release owner: `<name>`, Evidence SHA: `<sha>`.
- [ ] Reviewer: `<name>`, Evidence SHA: `<sha>`.
- [ ] Tag PR includes this completed checklist. Evidence PR: `<url>`.
- [ ] Final tag command recorded in PR body. Evidence PR: `<url>`.

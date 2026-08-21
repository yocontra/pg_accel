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
- Hardware evidence: name the Apple Silicon runner or host class used for Metal.
- Deferrals: link to the release-note section and issue that explicitly scopes the item out of 1.0.

Rows marked **External/publication** cannot be closed by a local repository
check. They require hosted CI, an independent clean-machine run, public-repository
or release artifacts, publication evidence, or named human sign-off as stated.

CUDA, NVIDIA, and PG-Strom are owner-deferred until a CUDA device is available.
They remain tracked in
[issue #4](https://github.com/yocontra/pg_accel/issues/4), are excluded from
this Metal release gate, and must not be presented as validated or supported by
this release. Adding any CUDA/NVIDIA support claim requires completing that
deferred gate with real hardware evidence first.

The unprivileged local gate proves warm performance and separately clears only
the project-owned AdaptiveCpp JIT/archive cache for cold first-dispatch
evidence. It does not prove an OS page-cache purge. Privileged OS page-cache
certification is optional manual evidence whose absence must remain explicit;
it is tracked in [issue #3](https://github.com/yocontra/pg_accel/issues/3).
The full 1B-row gate is likewise environment-deferred until sufficient storage
is available; no smaller fixture can substitute for it and no 1B-scale claim
is permitted without it. That certification is tracked in
[issue #2](https://github.com/yocontra/pg_accel/issues/2).

GitHub's `macos-26` hosted runner is an Apple virtual M1 with a paravirtual
Metal device. It is required for clean-host compatibility, source/package
installation, and three-layer coverage evidence, but it is not qualified
performance hardware. In that lane the Basic expression tier still dispatches
on Metal; Common/Extended expression semantics use an explicitly recorded
test-only host reference because their metallibs exceed the fixed 900 KiB OOM
guard. The execution-mode artifact is ineligible for performance claims. Warm
ratchets, native parity, system characterization, and Metal stress must pass on
the exact candidate on qualified physical M-series hardware. The tag workflow
creates a draft release; those qualified bundles and human sign-off must be
attached before publication.

### Local Evidence Ledger (Not Gate Completion)

The entries below preserve completed local checks without satisfying the
exact-candidate, CI, publication, or external-hardware rules above. They do not
justify checking any release item.

- Phase 6 domain-gate implementation commits: `37a9571d`, `868e0e89`, and
  `2f85c059`. A pre-integration PG18/Metal run passed, but its machine-local
  artifact was not retained as durable release evidence. The exact-candidate
  rerun remains open.
- Phase 9 operator-gate implementation commits: `b3261c86`, `64948b96`, and
  `d58db14f`. A pre-integration PG18/Metal run passed, but its machine-local
  artifact was not retained as durable release evidence. The exact-candidate
  rerun remains open.
- PostgreSQL 19 source/build commits: `3fd3d743` and `4ae5e337`. A local build
  recorded the `pg19` feature and binary digest
  `sha256:685cc1cf01c82b659965a1a3078a9ca993c47a5ee5ddb23e0f411da5deb78f5f`,
  but the build output was machine-local. Candidate package, install, and test
  gates remain open.
- Safety-documentation and strict-Clippy closure: `a0b98dd`. The local
  strict-Clippy command passed:
  `cargo clippy -p pg_accel --features pg18 --all-targets -- -D clippy::undocumented_unsafe_blocks`.
  No durable CI artifact was produced, so exact-candidate CI remains open.
- Commit `3a0bcd7` has a machine-local PG18/Metal 37-cell precursor at
  `.codex/scratch/final-warm-benchmark-3a0bcd73-PREPARED-20260811T072357Z`
  (`20` selected winners and `17` verified native declines). The full-matrix
  native-parity gate passed only `3/17` decline cells. Its `SHA256SUMS` seal hashes to
  `5c9f50243bbe0141fa23e8bd0dd5a84a577f95a1646bcc927553fb77c1ced70c`.
  Focused local repeats are sealed at
  `.codex/scratch/supplemental-warm-diagnostics-3a0bcd73-PREPARED-20260811T074540Z`
  with `SHA256SUMS`
  `4a838b7c3dae5f393ef648c2b47d0f47ae88e8b60779dc1b039ea26565b92d58`:
  all `11` repeated winner cells cleared their warm floors, but native parity
  passed `0/10` cells. These local seals therefore remain a non-publication
  precursor, do not close any checklist row, and leave the native-parity and
  hosted exact-candidate gates open.

## Phase 0 - Evidence, Provenance, And GPU-Only Guardrails

- [ ] Benchmark artifacts prove the exact binary, extension version, GUC snapshot, device metadata, selected plan, dispatched GPU kernels, returned row counts, correctness diffs, no-dispatch timing/plan audits, telemetry limits, warmup/JIT timings, and crash logs for every release benchmark run. Interim evidence: resume/audit manifests and fail-closed loader tests exercise this contract. Final evidence artifact: `<sha-or-url>`.
- [ ] Reports separate `plan_selected`, `gpu_kernel_dispatched`, `gpu_resident_pipeline`, `function_kernel_count`, `rows_returned_to_cpu`, and `planner_declined`; no benchmark conclusion is based on placeholder-GUC, unloaded-extension, no-dispatch timing skew, or debug-harness raw-wallclock runs. Reports also retain `warmup_iterations` plus first/max/post-first warmup summaries in JSON/CSV, record `methodology.harness_profile`, render recurring warmup spikes in Markdown, and hard-fail selected GPU-dispatched Custom Scan rows that lack resident-pipeline/boundary evidence. Interim live audit evidence: `target/release/pg_accel_bench explain-audit --connection 'host=localhost port=28818 dbname=postgres'` passed on 2026-06-09 and prints/fails a resident-boundary audit for selected pg_accel Custom Scans. Historical pre-reclassification H3 artifacts carry the selected `GpuAgg` boundary in plan snippets, pre-risk contexts, JSON, and Markdown reports: `benchmarks/artifacts/h3-res7-resident-gate-release-100k-cacheboth-20260609` (`h3_bulk @ 100K`, 8.60x, stock fallback 0, no ship-gate failures), `benchmarks/artifacts/h3-res7-resident-audit-artifact-release-100k-cacheboth-20260609` (`h3_bulk @ 100K`, 8.65x, `resident_boundary_audit.json` failed_rows=0, `boundary_recorded`, audit files indexed/listed in resume manifest), and `benchmarks/artifacts/h3-res7-finalize-failclosed-release-100k-cacheboth-20260609` (`h3_bulk @ 100K`, 8.52x, stock fallback 0, `resident_boundary_audit.json` failed_rows=0, `boundary_recorded`, audit files indexed/listed). Those artifacts do not establish current ship-gate eligibility: `h3_bulk` now fails closed with `shape_unsupported_rte`. New report artifacts `no_dispatch_audit.json` and `no_dispatch_audit.md` classify native no-dispatch rows as clean evidence or warning rows for timing skew, native plan mismatch, selected Custom Scan no-dispatch, and missing plan evidence; `benchmarks/artifacts/gpu-nlj-between-no-dispatch-audit-crashrepro-50k-cacheboth-20260610` proves the corrected audit on `gpu_nlj_between @ 50K` with `clean_rows=1`, `warning_rows=0`, status `comparable_native`, and indexed/listed audit files. Runner finalization propagates final crash-inventory and report/audit artifact write failures, with focused test coverage for missing resident-boundary evidence and no-dispatch audit artifacts. Evidence artifact: `<sha-or-url>`.
- [ ] GPU-only planner guardrails decline cases where launch, JIT, materialization, soft-fp64, transfer, duplicate worker work, or output reconstruction makes GPU execution slower; EXPLAIN shows the decline reason and no pg_accel plan label for declined cells. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] The proposed replacement nineteen-cell Metal matrix is represented in durable exact-candidate artifacts: `grouped_agg_int4`, `grouped_count_bool_candidate`, `grouped_count_date_candidate`, `grouped_count_float4_candidate`, `grouped_count_float8_candidate`, `grouped_count_timestamp_candidate`, `grouped_count_timestamptz_candidate`, `grouped_count_int2_candidate`, `grouped_count_int8_candidate`, `grouped_int2_sum_avg_candidate`, `grouped_int4_sum_avg_candidate`, `predicate_expression_grouped_agg_int4`, `and_range_predicate_expression_grouped_agg_int4`, `aggregate_filter_grouped_agg_int4`, `mixed_join_agg_int4`, `ssbm_resident_int4_star`, `ssbm_resident_int8_star`, `hashjoin_10k_1m`, and `h3_cell_to_parent`, all at 1M rows. Because the candidate SQL/population changed after commit `418b312`, that predecessor's randomized selection remains immutable transition history but does not verify the replacement population. Before any new release execution, record a fresh exact SHA/tree/population freeze and a fresh independent write-once random selection; do not relabel or overwrite the 418 evidence. Current no-dispatch lanes must also be represented, including crash-gated `gpu_nlj_between @ 50K` native-decline evidence (`benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609`, superseding stale selected-win artifact `benchmarks/artifacts/crash-repro-1779092055` after failed selected rerun `benchmarks/artifacts/gpu-nlj-between-release-50k-cacheboth-20260609`) plus refreshed no-dispatch audit evidence (`benchmarks/artifacts/gpu-nlj-between-no-dispatch-audit-crashrepro-50k-cacheboth-20260610`). Retained grouped-H3 res7/res9/res15 artifacts and fused-parent artifacts document historical measurements only. Release evidence artifact: `<sha-or-url>`.

## Phase 1 - Stop All Backend Crashes Before Re-Entry

- [ ] Previously known crash families are repaired or removed behind an explicit structural decline: grouped aggregation Metal argument-buffer, hash join Metal host-pointer probe, spatial high-capture lambda, parallel partial `SUM(bigint)`, direct generic expression-template scan, and the former row-emitting inequality-NLJ BETWEEN path. The expression-template and NLJ kernels/bridges no longer ship; exact-candidate plans must stay native, dispatch counters must remain flat, and planner rejection evidence must identify the missing resident pipeline or inequality-join decline. Historical NLJ evidence at `benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609` is transition evidence only. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] No selected GPU benchmark cell disconnects PostgreSQL or records backend crashes; known no-crash repro rows remain covered (`1779092044`, `1779092148`, `1779093355`, `1779093366`, `1779093376`, `1779093387`) and stale `gpu_nlj_between @ 50K` selected evidence is replaced by crash-gated native-decline artifact `benchmarks/artifacts/gpu-nlj-between-native-decline-50k-cacheboth-pass-20260609`. Release evidence artifact: `<sha-or-url>`.
- [ ] Any new backend-crashing shape discovered before the tag is recorded in the remaining-work register and either gated before re-entry or blocks the release. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 2 - AdaptiveCpp Runtime, Metal, And Fork Stability

- [ ] The fp64 soft path uses the exact AdaptiveCpp fork commit in `.acpp-version` plus the tracked repository patches. Release notes list the 33 non-merge commits unique to the pinned side relative to its embedded upstream `develop` snapshot and explicitly make no claim of later rebase or upstream acceptance. Exact-candidate setup provenance must record the resolved commit, patch status, soft-fp64 revision, compiler paths, and CMake arguments. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] fp64 ULP budget, special-value behavior, documented no-fenv-read-back release semantics, ABI value-buffer contract, and `pg_accel.soft_fp64_cost_multiplier` calibration are verified for every shipped fp64 kernel family. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] AdaptiveCpp JIT/archive cold-start, fork, archive-size, SSCP debug, and warning-noise behavior are captured as pass/fail artifacts, including first-dispatch latency per representative kernel class. The exact-candidate Metal stress artifact must index before/after project-owned JIT/archive-cache snapshots with file counts and total bytes, integer-microsecond cold first-dispatch records for reduction, H3, and spatial pipeline classes, and per-class warm-cache sample counts, totals, averages, and maxima for regression review. Missing, malformed, zero, or contradictory measurements fail closed; warm-cache metrics remain visibility-only unless an existing release policy supplies a threshold. This artifact is not OS page-cache purge evidence. Interim evidence: quiet CTest H3 log `.pgaccel/logs/test_h3-20260609-003356-84515.log`, focused f32/all-res log `.pgaccel/logs/test_h3-f32-allres-refactor-20260609-003343-84484.log`, and warmup/JIT report fields required by the benchmark evidence contract. Final evidence SHA/artifact: `<sha-or-url>`.
- [ ] The Metal runtime setup path is documented with backend metadata, and unvalidated backends are explicitly identified as unsupported. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 3 - GPU-Resident Execution Substrate

- [ ] PG-Strom-shaped execution model is represented in EXPLAIN and benchmark artifacts: GPU scan, join, pre-aggregation, expression pushdown, join-side reuse, pruning, and GPU-resident intermediate data where selected. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Expression/filter/projection wrappers over PostgreSQL-native child plans are declined unless GPU dispatch is real, output semantics match PostgreSQL, and selected cells meet benchmark parity. Current direct generic `GpuExpr` template scans decline with `standalone_gpuexpr_no_gpu_pipeline`; live plan-shape evidence keeps the GPU counter flat and matches native `count(*)`. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Selected aggregate plans preserve the Resident v2 childless boundary: one `Custom Scan (GpuAccelAgg)` carries a strict logical `AggQuerySpec`, reads every fact/dimension dependency from the residency store, and materializes only bounded final aggregate output. `BeginCustomScan` processes invalidations, revalidates relation generation and physical identity, and binds only current dependency-stamped artifacts; rescan repeats that validation. Exact-candidate plan, dispatch-counter, correctness-diff, invalidation, and benchmark artifacts must prove visible GPU dispatch, PostgreSQL semantics, and parity for every selected grouped aggregate cell. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 4 - Core OLAP Coverage

- [ ] Grouped aggregate, reduce, resident grouped COUNT, count-only hash join, H3, expression VM, and spatial/bbox survivors dispatch real GPU work with semantic correctness proof. Retired host hash aggregation, row-emitting joins, inequality NLJ, expression templates, standalone sort/top-k, and window surfaces must instead produce native plans, zero GPU dispatch, and explicit structural-decline evidence. The exact release sentinels are `grouped_agg_int4`, `grouped_count_bool_candidate`, `grouped_count_date_candidate`, `grouped_count_float4_candidate`, `grouped_count_float8_candidate`, `grouped_count_timestamp_candidate`, `grouped_count_timestamptz_candidate`, `grouped_count_int2_candidate`, `grouped_count_int8_candidate`, `grouped_int2_sum_avg_candidate`, `grouped_int4_sum_avg_candidate`, `predicate_expression_grouped_agg_int4`, `and_range_predicate_expression_grouped_agg_int4`, `aggregate_filter_grouped_agg_int4`, `mixed_join_agg_int4`, `ssbm_resident_int4_star`, `ssbm_resident_int8_star`, `hashjoin_10k_1m`, and `h3_cell_to_parent`. The typed-count sentinels group one nullable boolean fact column and count one distinct nullable bool, int2, int8, float4, float8, date, timestamp, or timestamptz fact column; adjacent COUNT shapes remain native. The widened-integer sentinels group one nullable boolean fact key, project one distinct nullable int2 or int4 value as exact SUM and NUMERIC AVG, and pair it with COUNT(*); adjacent variants remain native. The range sentinel fuses exactly two bounds over the nullable product lhs, dispatches `parallel_dense_integer`, and keeps one-sided, RHS-input, joined, degenerate, and third-bound variants native. The aggregate-FILTER sentinel applies one proper bounded same-column int4 interval only to SUM while COUNT(*) remains unfiltered. The two SSBM sentinels join lineorder to both date and part, group by `d_year, p_size`, compute exact `SUM(lo_revenue), COUNT(*)`, pin all three relation dependencies, and must expose the matching two-dimension descriptor; the INT8 sentinel additionally requires exact INT8 membership keys. A legacy Q1/Q4 proof does not satisfy either sentinel. Legacy `grouped_agg` and `mixed_join_agg` decline with `shape_floating_accumulator_semantics`; `predicate_filter_expression_grouped_agg` declines with `shape_aggregate_modifier`. The canonical 13 SSBM queries stay native: Q1.1-Q1.2 report `shape_multiple_range_predicates`, Q1.3 reports `shape_multi_filter_relation`, Q3.3-Q3.4 report `shape_unsupported_predicate`, and the remainder report `shape_unsupported_filter_type`. Historical device logs containing `test_window` or segmented window dispatch predate the surface reduction and do not verify this candidate. Release evidence SHA/artifact: `<sha-or-url>`.
- [ ] Standalone sort/top-k/rank/window shapes remain structural declines on every backend. Exact-candidate SQL tests cover NULLs, duplicates, collation/order-sensitive cases, peer groups, and frames through PostgreSQL-native execution while proving no `GpuSort`/`GpuWindow` plan, zero GPU dispatch, and the expected rejection reason (`sort_heap_full_output`, `sort_standalone_topk_no_gpu_kernel`, or `no_gpu_resident_pipeline`). No removed `test_window` binary or historical kernel artifact may satisfy this row. Release evidence SHA/artifact: `<sha-or-url>`.
- [ ] Join selected shapes show build-side reuse evidence, correct NULL/anti/semi semantics, and no redundant full join-output reconstruction where the benchmark gate depends on it. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 5 - Geo, H3, Raster, And PostGIS Coverage

- [ ] The current H3 release winner, fused `h3_cell_to_parent(cell, const), COUNT(*)` parent grouped count, has zero h3-pg diff rows, no crashes, consumed outputs, dispatch counters, `>=1.15x` warm-run threshold evidence, and bounded JIT/archive cold-start cost. `h3_bulk` fails closed with `shape_unsupported_rte`; `h3_resolution_sweep` and `h3_latlng_res15` fail closed with `shape_group_expression`, zero kernel evidence, and exact-candidate native parity at both 100K and 1M rows. Historical pre-reclassification seed artifacts remain recorded for grouped H3 res7 (`benchmarks/artifacts/h3-res7-prealloc-release-100k-cacheboth-1780992523`, `benchmarks/artifacts/h3-res7-prealloc-release-1m-cacheboth-1780992542`), res9 (`benchmarks/artifacts/h3-res9-release-100k-cacheboth-1780993419`, `benchmarks/artifacts/h3-res9-release-1m-cacheboth-1780993439`), res15 (`benchmarks/artifacts/h3-res15-release-100k-cacheboth-1780994043`, `benchmarks/artifacts/h3-res15-release-1m-cacheboth-1780994068`), and fused parent grouped count (`benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-100k-cacheboth-20260610011605`, 1.38x; `benchmarks/artifacts/h3-parent-count-boundedhash-stable-release-1m-cacheboth-20260610011614`, 1.21x); those historical measurements are unchanged and do not close the exact-candidate gate. Standalone all-resolution f32/exact correctness evidence remains `.pgaccel/logs/test_h3-20260609-003356-84515.log`. Release evidence artifact: `<sha-or-url>`.
- [ ] H3 LATERAL SRF and target-list/multi-SRF paths either dispatch with PostgreSQL-compatible row multiplication, NULL handling, ordinality, and zero h3-pg diffs or visibly decline. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] PostGIS geometry predicates, joins, and constructors match PostGIS native output on exact GPU paths, document uncertain-row recheck behavior, and decline unsupported shapes. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Raster map algebra, clip/reclass, summaries, and multi-band workloads consume computed raster output, match PostGIS raster output within documented pixel tolerances, and dispatch only winning selected sizes. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 6 - Feature Completion And Deferrals

- [ ] SQL operator/function support matrix, unsupported JSON/JSONB or deferred type coverage, type coercion, NULL edge cases, and released GUC docs match the implementation. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] SetOp, RecursiveUnion, merge join, NUMERIC behavior, generated columns, partition/pruning behavior, and any remaining accelerator-surface gaps either have GPU/correctness/performance proof or documented planner-decline deferrals. The checked-in `test_stored_generated_columns_dispatch_and_match_native` integration test covers stored generated group keys and measures after a base-column update, including selected-plan shape, real GPU dispatch, and native-result parity; a fresh exact-candidate artifact remains required. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] AdaptiveCpp rebase/upstream status and fork-pinned installation burden match the public documentation: exact fork commit, embedded upstream snapshot and merge, no unproved upstream-acceptance claim, source-build requirement, Metal/LLVM/lld and soft-fp64 prerequisites, tracked post-checkout patches, and generated setup provenance. A fresh clean-machine artifact must prove those instructions against the exact candidate. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 7 - Cost Models, Performance Ratchets, And Comparative Benchmarks

- [ ] Benchmark ratchets fail when a selected GPU cell falls below `1.15x` PostgreSQL parallel, crashes, silently misses GPU dispatch, or loses expected GPU plan selection. After the replacement candidate is frozen and independently selected as required in Phase 0, the unprivileged exact-candidate warm matrix runs the write-once nineteen-cell 1M-row contract in release mode with seed 42, raw timing, ten measurements, five warmups, and complete plan/dispatch/correctness artifacts. All nineteen proposed cells, `grouped_agg_int4`, `grouped_count_bool_candidate`, `grouped_count_date_candidate`, `grouped_count_float4_candidate`, `grouped_count_float8_candidate`, `grouped_count_timestamp_candidate`, `grouped_count_timestamptz_candidate`, `grouped_count_int2_candidate`, `grouped_count_int8_candidate`, `grouped_int2_sum_avg_candidate`, `grouped_int4_sum_avg_candidate`, `predicate_expression_grouped_agg_int4`, `and_range_predicate_expression_grouped_agg_int4`, `aggregate_filter_grouped_agg_int4`, `mixed_join_agg_int4`, `ssbm_resident_int4_star`, `ssbm_resident_int8_star`, `hashjoin_10k_1m`, and `h3_cell_to_parent`, require `>=1.15x` warm median. The qualified physical-M-series release run executes `metal-warm-ship-gate`; the hosted virtual-M1 workflows run compatibility/coverage only. `metal-ship-gate` remains a separate optional manual cache-mode-`both` certification and does not replace the warm matrix. Candidate/source changes require a fresh freeze and selection rather than rerunning or relabeling predecessor evidence. The commands reject incomplete or altered evidence. Workflow wiring is not execution evidence: successful exact-candidate hosted artifacts and separate qualified-hardware performance artifacts remain required. Evidence SHA/CI: `<sha-or-url>`.
- [ ] Per-lane threshold matrices exist for row count, type, cardinality, selectivity, row width, output size, geometry complexity, batch count, H3 operation, and raster operation. Every selected lane, including fused H3 parent grouped count, has a current `>=1.15x` warm floor; `h3_bulk`, `h3_resolution_sweep`, and `h3_latlng_res15` are native-decline guards with the structural reasons recorded in Phase 5, while standalone scalar H3 parent/distance lanes remain native-decline parity guards. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] PostgreSQL native comparison passes: every selected GPU cell in the release matrix has `speedup_x >= 1.15` on the validated M-series hardware, and every non-selected cell has a visible planner-decline reason. Evidence artifact: `<sha-or-url>`.

## Phase 8 - Test Coverage, CI, And Stress Gates

- [ ] **External/publication:** Coverage reaches at least 90% for pg_accel-owned Rust, C++/SYCL, and SQL-extension behavior, and CI publishes the exact-candidate bundle while failing below any required threshold. The local full-device gate and hosted compatibility gate each seal independent Rust source-line, C++/SYCL source-line (host instrumentation plus real Metal device counters), and fixed-manifest SQL semantic-assertion layers at 90% each. The hosted bundle must retain its virtual-M1 execution mode and cannot close any performance row. The current SQL scope contains 67 files and 361 assertions. Evidence CI/artifact: `<sha-or-url>`.
- [ ] Coverage includes planner hooks, executor state, private-data encoding/decoding, GPU dispatch adapters, SQL extension surfaces, C++ kernels, H3/PostGIS/raster semantics, and benchmark classification. The coverage scope and gate require compiler-derived Rust production mapping, CTest plus manual out-of-order C++/SYCL evidence, and the external SQL integration suite. A fresh exact-candidate artifact must prove the required surfaces and percentages. Evidence CI/artifact: `<sha-or-url>`.
- [ ] Metal stress gate passes on M-series hardware with mixed scan, aggregate, join, sort, H3, PostGIS, raster, fork, and cancellation workloads: zero backend crashes, kernel failures, panic-log entries, and resource-leak messages. The next exact-candidate run must also produce the top-level stress `artifact_index.json` covering the fail-closed archive/cache and cold/warm latency artifacts required by Phase 2. Current local evidence: `just metal-stress 18` passed on 2026-07-04 with artifact directory `benchmarks/artifacts/metal-stress-20260704-161735`, including install, extension smoke, 52/52 SQL files, clean logs, standalone GPU tests, `gpu-stress-archive` 8x20 with zero XPC/pipeline/archive failures, benchmark crash-artifact checks, cancellation probe, and final clean-log assertion; that historical artifact predates the enriched contract and does not satisfy it. Release evidence artifact: `<sha-or-url>`.
- [ ] **External/publication:** Required hosted CI ship-bar jobs pass on the exact release-candidate commit: macOS arm64 virtual-M1 Metal compatibility/coverage plus public clean-install evidence, and Linux x86_64 no-GPU behavior for PG18 and PG19. Qualified physical-M-series performance/stress evidence is a separate mandatory gate. Branch protection is recommended repository policy but is not a tag blocker. Evidence CI/settings: `<sha-or-url>`.
- [ ] Release verification matrix passes with artifacts for EXPLAIN audit, correctness diff, the unprivileged warm benchmark sweep, fork stress, deferred-site audit, and `pg_accel_stats()` sanity. Project-owned JIT/archive-cache cold-start evidence is recorded by Metal stress; the optional privileged OS page-cache arm remains a separate certification claim and may not be inferred from this row. Interim live-PG evidence: H3 protection passes under the default Rust test scheduler without `--test-threads=1`, the plan-shape filter passes 8/8 after the direct-GpuExpr and NLJ crash gates, and `parallel_stress_test` passes 4/4 with no backend disconnects. Current local PG18 evidence on 2026-07-04: `just sql-test 18` passes 52/52 SQL files; `just metal-stress 18` passes with artifacts at `benchmarks/artifacts/metal-stress-20260704-161735`; native Metal tests pass (`test_bbox` 27/27, `test_spatial` 162/162, `test_reduce_stats` PASS, `test_correctness` 340/340, `test_hash_join` 23/23); Rust format, diff whitespace, shell syntax, lint, unit tests, audit, package, and doc parity complete. Those historical checks are not exact-candidate evidence and do not close this row. Release evidence artifact: `<sha-or-url>`.
- [ ] **External/publication:** Release checklist synchronization is complete: this checklist matches the remaining release gates, every item has evidence, and the tag PR includes the checked checklist. Evidence PR: `<url>`.

## Phase 9 - Public Release And Installability

- [ ] **External/publication:** Fresh-machine smoke passes from a clean clone using public README instructions: install prerequisites, `just setup-gpu-acpp`, package, install, `CREATE EXTENSION`, and run a representative benchmark without manual fixes. Evidence artifact: `<sha-or-url>`.
- [ ] **External/publication:** Installable-by-anyone gate passes: PostgreSQL extension package, AdaptiveCpp fork setup, kernel build, SQL/control files, source PostgreSQL/pgrx path, native macOS notes, Linux no-GPU behavior, and verification command all work from clean machines. Evidence artifact: `<sha-or-url>`.
- [ ] Install provenance confirms the live PostgreSQL backend loads the just-built extension binary and failures produce actionable diagnostics. Evidence artifact: `<sha-or-url>`.
- [ ] **External/publication:** Public repository readiness is complete: README, architecture docs, benchmark docs, release notes, license files, contribution guide, security policy, issue templates, reproducible benchmark artifacts, supported hardware, limitations, and failure-reporting docs are published. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] **External/publication:** Release candidate and final tag artifacts are published: `v1.0.0-rc1`, one-week monitoring notes, `v1.0.0`, release notes, source archive, SQL artifacts, checksums, benchmark artifacts, and install docs. Evidence release: `<release-url>`.
- [ ] **External/publication:** Hacker News launch is blocked until the repo is public, installable, benchmark-backed, crash-free on the release matrix, and the post links to install docs, benchmark evidence, supported hardware, limitations, and issue tracker. Evidence URL: `<url>`.

## Maintainer Sign-Off

- [ ] **External/publication:** Release owner: `<name>`, Evidence SHA: `<sha>`.
- [ ] **External/publication:** Reviewer: `<name>`, Evidence SHA: `<sha>`.
- [ ] **External/publication:** Tag PR includes this completed checklist. Evidence PR: `<url>`.
- [ ] **External/publication:** Final tag command recorded in PR body. Evidence PR: `<url>`.

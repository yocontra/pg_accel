# pg_accel 1.0 Release Checklist

Use this as the tag PR gate for `v1.0.0-rc1` and `v1.0.0`. Every checked item
must include a commit SHA, artifact URL, or both. Do not cut the tag while any
required item is unchecked.

## Evidence Rules

- Evidence SHA: commit that closed the item, not the commit that checked this box.
- CI evidence: link to the GitHub Actions run for the release candidate commit.
- Bench evidence: link to the committed or uploaded benchmark report artifact.
- Hardware evidence: name the runner or host class used for Metal/CUDA/ROCm/L0.

## Phase 1 - fp64 Correctness And Costing

- [ ] fp64 soft path uses AdaptiveCpp fork `4f3cde11a302eebac28aa1ccc79ad3399cb8183c` or newer. Evidence SHA: `<sha>`.
- [ ] fp64 ULP budget and special-value behavior are verified for every shipped fp64 kernel family. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] `soft_fp64_cost_multiplier` calibration is rerun after Phase 6 probe-cost work. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 2 - Kernel Dispatch And Bytecode

- [ ] Expression bytecode dispatch is enabled and covered by correctness tests. Evidence SHA: `<sha>`.
- [ ] HashAgg, expression, H3, sort, spatial, window, and raster kernels have no known silent wrong-result paths. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Cold-cache fork dispatch passes for the required kernel matrix. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 3 - Parallel Planner Coverage

- [ ] PreAgg executor refactor and `preagg_parallel_safe` default are merged. Evidence SHA: `<sha>`.
- [ ] HashAgg per-group partial-state kernel, bridge, and executor are merged. Evidence SHA: `<sha>`.
- [ ] Grouped HashAgg planner-side wiring is merged and tested. Evidence SHA: `<sha>`.
- [ ] Parallel COUNT/DISTINCT and representative grouped aggregates pick the intended GPU paths. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 4 - Operator And Type Coverage

- [ ] SQL operator/function support matrix matches the released implementation. Evidence SHA: `<sha>`.
- [ ] Unsupported JSON/JSONB or other deferred type coverage is documented as post-1.0. Evidence SHA: `<sha>`.
- [ ] Type coercion and NULL edge-case tests pass in the release candidate. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 5 - Planner Model And Fallback Discipline

- [ ] GPU bridge dead-code cleanup is merged. Evidence SHA: `<sha>`.
- [ ] Worker-side spatial recheck status is documented or implemented. Evidence SHA: `<sha>`.
- [ ] Zero-overhead OLTP and passthrough tests show no regression versus PostgreSQL parallel plans. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Planner rejection counters and EXPLAIN output are consistent with released GUCs. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 6 - Performance Gates

- [ ] Probe-cost amortization is calibrated through `DeviceLimits`, not hard-coded planner literals. Evidence SHA: `<sha>`.
- [ ] Small-N HashAgg and sort cold-cache regressions remain closed. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Benchmark matrix never falls below PostgreSQL parallel parity for shipped workload/size cells. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Bench harness asserts no GPU kernel errors occurred during the sweep. Evidence SHA/artifact: `<sha-or-url>`.

## Phase 7 - AdaptiveCpp Upstream Burden

- [ ] Required AdaptiveCpp fork-local commits are listed with SHAs and status. Evidence SHA: `<sha>`.
- [ ] SLEEF/multi-address-space pointer specialization status is documented. Evidence SHA: `<sha>`.
- [ ] Cross-backend CUDA/ROCm/L0 parity is either green or explicitly deferred with hardware rationale. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Metal SSCP debug knobs and fork-safety stress tests are documented. Evidence SHA: `<sha>`.

## Phase 8 - Build And Packaging

- [ ] Fresh-machine setup path succeeds on Apple Silicon: `just setup-gpu-acpp`, `just package`, install, `CREATE EXTENSION`. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Required cargo tools are pinned or installed by setup scripts. Evidence SHA: `<sha>`.
- [ ] Extension package includes fresh install SQL and any upgrade SQL scripts. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] README installation requirements match the actual build. Evidence SHA: `<sha>`.

## Phase 9 - Verification Matrix

- [ ] `just ci` is green on the release candidate commit. Evidence SHA/CI: `<sha-or-url>`.
- [ ] pgrx PG17 test suite passes with PostGIS, postgis_raster, h3, and h3_postgis available. Evidence SHA/CI: `<sha-or-url>`.
- [ ] Standalone kernel tests pass on Metal. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Representative benchmark smoke and full benchmark sweep artifacts are attached. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Documentation parity check passes. Evidence SHA/CI: `<sha-or-url>`.

## Phase 10 - Release Prep

- [ ] CLAUDE.md and public docs match released GUCs, supported kernels, and safety rules. Evidence SHA: `<sha>`.
- [ ] GitHub Actions required jobs `macOS arm64 just ci` and `Linux x86_64 check/test` pass. Evidence CI: `<url>`.
- [ ] Optional CUDA smoke is either green on a labeled self-hosted runner or explicitly skipped for lack of hardware. Evidence CI: `<url-or-rationale>`.
- [ ] `pg_accel.control` and crate version resolve to `1.0.0`. Evidence SHA: `<sha>`.
- [ ] `cargo pgrx schema -p pg_accel pg17 --features pg17` generates `pg_accel--1.0.0.sql` with the expected SQL entities. Evidence artifact: `<url>`.
- [ ] Upgrade script `pg_accel--0.1.0--1.0.0.sql` is tested against an installed 0.1.0 cluster. Evidence SHA/artifact: `<sha-or-url>`.
- [ ] Historical 0.1.0 SQL baseline is identified or the release notes state that 0.1.0 was unreleased and unsupported for in-place upgrades. Evidence SHA: `<sha>`.
- [ ] `v1.0.0-rc1` is tagged and monitored for one week. Evidence tag: `<tag-url>`.
- [ ] `v1.0.0` release notes and binary/SQL artifacts are published. Evidence release: `<release-url>`.

## Maintainer Sign-Off

- [ ] Release owner: `<name>`, SHA: `<sha>`.
- [ ] Reviewer: `<name>`, SHA: `<sha>`.
- [ ] Final tag command recorded in PR body. Evidence: `<url>`.

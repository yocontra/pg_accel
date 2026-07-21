# Remaining Work

This file contains unfinished work only. Completed implementation history is in
Git and [CHANGELOG.md](CHANGELOG.md). A selected pg_accel plan must dispatch
real GPU work; unsupported or unprofitable shapes must remain PostgreSQL-native.

## Local Release Gates

- Complete the production GPU-only source audit and repair every real
  host-compute violation. Do not whitelist, hide, or relabel CPU execution as a
  pg_accel kernel path.
- Complete the sealed Rust, C++/SYCL, and SQL coverage bundle and reach the
  release threshold for owned executable code without synthetic inputs or
  mutable scope.
- Run the full Phase 9 live workload matrix against the exact release-candidate
  binary. Every cell needs an exact result oracle plus either selected resident
  GPU dispatch or the expected visible native-decline reason with zero kernel
  delta.
- Run the unprivileged default-admission warm performance matrix, including
  automatic load, explicit pinning, randomized nearby shapes, concurrency,
  memory pressure, and stress. Retain only selected lanes whose warm median is
  at least `1.15x` PostgreSQL-native with full correctness, dispatch, and
  provenance evidence.
- Run the final PostgreSQL 18 and PostgreSQL 19 beta build, test, package, and
  clean-install matrix with binary provenance and backend-log checks.
- Produce fresh Metal release artifacts for correctness, fork/archive stress,
  cancellation, invalidation, residency budget, benchmark evidence, and final
  clean shutdown. The exact-candidate stress artifact must index fail-closed
  before/after project-owned AdaptiveCpp JIT/archive-cache file-count and
  byte-size snapshots, per-class cold first-dispatch latency, and warm-cache
  sample totals/maxima for regression review. This is runtime/JIT-cache cold
  evidence, not an OS page-cache purge claim. Do not substitute older local
  logs or infer a timing pass from a measurement that has no approved release
  threshold.

## Environment-Deferred Evidence

- Run the full 1B-row evidence gate when sufficient local storage is
  available. No smaller fixture may be substituted, and no 1B-scale
  correctness or performance claim may be made until the exact gate passes.
- Optional manual OS page-cache certification may run the privileged cold arm
  of the benchmark/release matrix later. Its purpose is to distinguish true
  cold-I/O behavior from PostgreSQL shared-buffer and operating-system cache
  reuse. It is not required for unprivileged local engineering completion, and
  its absence must remain explicit rather than being inferred from JIT/archive
  cache clearing or warm evidence.

## Public Release Gates

- Verify the public source-build instructions from a clean checkout on a fresh
  Apple Silicon environment without undocumented manual fixes.
- Finish repository-host settings that cannot be proven from source alone:
  private vulnerability reporting, branch protection, required CI checks, and
  release permissions.
- Fill the release checklist only with durable commit, CI, package, and
  benchmark evidence from the exact candidate. Unchecked or placeholder rows
  continue to block a tag.
- Cut `v1.0.0-rc1` only after all non-deferred gates pass, monitor that exact
  candidate for one week, and promote it only if no release-blocking issue is
  found. Publish checksums and source/package evidence with the release.

## OWNER-DEFERRED: CUDA, NVIDIA, and PG-Strom

The owner has explicitly deferred this block until an NVIDIA CUDA device is
available. Never claim CUDA support, correctness, packaging, or performance
from the current Metal/no-GPU environments.

- Provision a CUDA host and build the exact AdaptiveCpp commit recorded in
  `.acpp-version` with its CUDA backend.
- Verify cross-backend ABI compatibility and run CUDA correctness, fp64,
  cold/warm, fork, cancellation, memory-pressure, crash-band, packaging, and
  `just cuda-stress` gates.
- Add a CUDA device-counter lowering/runtime path equivalent to the sealed
  Metal `.proftext` evidence before applying the C++ coverage gate on NVIDIA.
- Tune CUDA admission only from correctness-clean measurements; keep losing or
  unstable cells PostgreSQL-native with an exact decline reason.
- Install PG-Strom on the same PostgreSQL/CUDA host and capture like-for-like
  workload, configuration, correctness, and timing evidence.
- Add CUDA CI and release artifacts before advertising NVIDIA as validated.

## Versioned Toolchain

- PostgreSQL 18 is the default target; PostgreSQL 19 beta remains a required
  preview build/test target, not a package-availability promise.
- AdaptiveCpp is pinned exactly by `.acpp-version`; scripts and documentation
  must not carry a second independent pin.
- soft-fp64 is sourced from `yocontra/soft-fp` tag `v1.3.0` through the pinned
  AdaptiveCpp toolchain.

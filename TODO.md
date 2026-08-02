# Remaining Work

This file contains unfinished work only. Completed implementation history is in
Git and [CHANGELOG.md](CHANGELOG.md). A selected pg_accel plan must dispatch
real GPU work; unsupported or unprofitable shapes must remain PostgreSQL-native.

## Post-Rebuild Performance Program

- Close the native-decline parity gate in
  [docs/PERFORMANCE_PLAN.md](docs/PERFORMANCE_PLAN.md). The final Resident v2
  matrix proved every selected GPU cell above the `1.15x` floor, but the
  stricter extension-enabled native comparison passed only 5 of 15 cells.
  Capture the existing planner-overhead counter per query, remove measurable
  hook/classification overhead, and repeat the paired gate before release.
- Recheck only the flagged 10M `hash_join` and `hashjoin_10k_1m` resident
  grouped-aggregate cells under an exact-SHA, interleaved run. Their active
  kernel is unchanged and the first run does not establish a source
  regression; optimize the grouped lifecycle only if the focused run
  reproduces the loss.
- Execute the remaining measured optimization sequence in
  [docs/PERFORMANCE_PLAN.md](docs/PERFORMANCE_PLAN.md). Production admission
  must remain fail-closed, and no new lane becomes selectable until it proves
  correctness, real GPU dispatch, zero fallback, and at least `1.15x` warm
  speedup over the matched PostgreSQL plan.

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

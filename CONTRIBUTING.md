# Contributing to pg_accel

The current production surface is a GPU-resident reducing/grouped aggregate
Custom Scan plus the qualified resident raster transform. Read
[README.md](README.md#capability-matrix) and
[ARCHITECTURE.md](ARCHITECTURE.md) before changing planner or execution claims.
A kernel, bridge, executor, adapter, or benchmark workload alone is not
production planner support.

## Development setup

Prerequisites include the pinned Rust toolchain, CMake, a C/C++ toolchain,
pgrx, repository-built PostgreSQL, and the exact AdaptiveCpp commit in
[`.acpp-version`](.acpp-version) for kernel builds. Setup scripts and CI read
that file directly; do not substitute the head of `fork-safe-metal`. The
currently validated GPU development target is Metal on Apple Silicon.
CUDA/NVIDIA validation is owner-deferred in
[issue #4](https://github.com/yocontra/pg_accel/issues/4); do not claim it from
an unverified build.

```bash
brew install llvm@20 lld@20 libomp boost postgis
just setup-system-deps
just setup-tools
just setup-pg-source 18
ACPP_BACKEND=metal just setup-gpu-metal-headers
ACPP_BACKEND=metal LLVM_PREFIX=/path/to/llvm ACPP_LLD_PATH=/path/to/ld64.lld just setup-gpu
just setup-pgrx
```

That Homebrew command is the canonical Apple Silicon source-build prerequisite
set. Xcode command-line tools and the repository-installed `metal-cpp` headers
are also required. The packaged extension's runtime prerequisite set is
narrower: LLVM 20 and `libomp` only.

PostgreSQL 18 is the default extension target. The configured PostgreSQL 19
beta target is a build/test target, not a release-package promise.

## Build and test commands

All commands are defined in the [Justfile](Justfile):

| Command | Purpose |
|---|---|
| `just fmt` | Format Rust source. |
| `just fmt-check` | Verify Rust formatting. |
| `just lint` | Run clippy for the active configured PostgreSQL major. |
| `just check` | Type-check the active configured major. |
| `just check-matrix` | Type-check every configured supported major. |
| `just deny` | Check dependency license/advisory policy. |
| `just audit` | Run cargo-audit with the repository policy. |
| `just doc-parity` | Validate exact citations, released GUC semantics, adapters, and capability docs. |
| `just pg-version-audit` | Validate centralized PostgreSQL-version plumbing. |
| `just test` | Run the pgrx test matrix. |
| `just gpu-build` | Build the AdaptiveCpp/SYCL kernel library. |
| `just gpu-test` | Run registered standalone kernel tests. |
| `just ci` | Run pre-commit gates and the pgrx test matrix. |

Use `make clear-jit` when a test requires an empty Metal JIT cache.

## Making a change

Keep a patch within the existing ownership boundaries. Add abstractions only
when they remove real cross-module complexity or match an established pattern.

For planner or execution work:

1. Identify the exact SQL semantics and PostgreSQL planner/executor boundary.
2. Add a stable native-decline reason before exposing an incomplete shape.
3. Keep AQS3/AOP2 logical data free of device pointers.
4. Prove relation/column dependencies, generation currentness, budget, and
   final-only materialization.
5. Bind the frozen descriptor ABI only after capability and residency checks.
6. Add EXPLAIN selection/dispatch evidence and native-equivalence coverage.
7. Test failure, invalidation, rescan, cleanup, and budget behavior as relevant.

Normal production path injection currently occurs through the qualified
aggregate and raster arms at
`pg_accel/src/engine/ffi/planner_hooks/mod.rs:221-251`. Changing the public
capability matrix requires normal-GUC selection and dispatch evidence, not a
test-only force path.

## Adding adapter metadata

Adapter registration is OID/operation metadata, not the whole feature. Do not
register a desired extension function until the complete kernel, bridge,
resident producer/consumer, planner gates, output contract, EXPLAIN, and tests
exist. Follow [docs/ADAPTER_GUIDE.md](docs/ADAPTER_GUIDE.md), which documents
the current `FunctionAccelEntry` fields and both registry discovery lists.

The only wired adapter constructors are visible at
`pg_accel/src/engine/registry.rs:85-109`. New metadata must also participate in
the deferred re-resolution path at
`pg_accel/src/engine/registry.rs:194-235`.

## Code style and safety

- Format with `cargo fmt`; do not hand-format around rustfmt.
- Treat clippy warnings as errors under the repository configuration.
- Every `unsafe` block needs a specific `// SAFETY:` explanation.
- Do not use `unwrap()` outside tests.
- Call PostgreSQL C APIs only on the backend main thread.
- Check interrupts between bounded synchronous device calls.
- Never put device pointers in plan wire data or shared process state.
- Never convert a selected GPU error into hidden PostgreSQL executor
  passthrough.
- Remember that PostgreSQL parallel workers are forked processes, not Rust
  threads.
- Pins alter eviction eligibility, not resident budget accounting.

## Tests and evidence

Run focused tests first, then the broadest available static/test gates. State
which hardware-dependent tests were not run.

For a production selection claim, capture all of:

- exact `Custom Scan (GpuAccelAgg)` plan label;
- `Plan Selected: true` and resident proof/operator class;
- `GPU Kernel Dispatched: true` under `EXPLAIN ANALYZE`;
- positive `pg_accel_kernel_executions()` delta in the same backend;
- completely consumed output and PostgreSQL-native correctness comparison;
- stable native declines for unsupported neighboring shapes;
- commit, PostgreSQL/device provenance, released GUCs, and effective device
  limits.

Do not use `pg_accel.test_*` settings as production evidence.

## Commits and pull requests

Use a scoped conventional subject:

```text
feat(scope): add a production capability
fix(scope): correct behavior
perf(scope): improve measured execution
test(scope): add or update coverage
docs(scope): correct documentation
refactor(scope): restructure without behavior change
chore(scope): maintain tooling or dependencies
```

Pull requests should state the motivation, affected ownership boundaries,
behavioral contract, validation performed, hardware-dependent gaps, and any
native-decline or public-capability changes.

Use [GitHub issues](https://github.com/yocontra/pg_accel/issues) for ordinary
bugs and feature requests. [SUPPORT.md](SUPPORT.md) lists the diagnostic data
needed for actionable failure reports. Security reports follow
[SECURITY.md](SECURITY.md) and must not be posted as public issues.

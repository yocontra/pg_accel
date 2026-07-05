# Contributing to pg_accel

PostgreSQL extension for GPU-accelerated spatial predicates, H3 cell ops, raster
operations, and batched executor nodes. Rust (pgrx 0.18) + C++/SYCL (AdaptiveCpp).

## Development Setup

### Prerequisites

- Rust stable (via asdf or rustup)
- Repo-local PostgreSQL built from source via `just setup-pg-source`.
  PG18 is the default supported extension target; PG19 source smoke testing is
  opt-in until pgrx exposes a real `pg19` feature.
- cmake (for GPU kernels)
- [cargo-pgrx](https://github.com/pgcentralfoundation/pgrx) 0.18
- [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)

### Quick Start

```bash
just setup-system-deps  # Print distro/toolchain prerequisites
just setup              # Build source PostgreSQL and initialize pgrx
ACPP_BACKEND=cuda just setup-gpu   # Linux/NVIDIA
# or: ACPP_BACKEND=metal just setup-gpu
```

### Manual Setup

```bash
just setup-pg-source 18
just setup-pgrx 18
ACPP_BACKEND=cuda just setup-gpu
```

## Build Commands

All commands are in the [Justfile](Justfile):

| Command | Description |
|---|---|
| `just fmt` | Format code (`cargo fmt`) |
| `just lint` | Lint (`cargo clippy -- -D warnings`) |
| `just check` | Type check (`cargo check --all-features`) |
| `just deny` | License + advisory check (`cargo deny check`) |
| `just test` | Unit tests across the supported PostgreSQL matrix |
| `just ci` | Full local CI: lint + test |
| `just package` | Build installable `.so` (`cargo pgrx package`) |
| `just gpu-build` | cmake build for GPU kernels (requires AdaptiveCpp) |
| `just gpu-test` | Run standalone GPU kernel tests |

## Adding a new adapter

Adapters teach pg_accel which SQL functions can be accelerated and what strategy
to use. To add support for a new extension (e.g., pgvector):

1. Create a new file in `src/adapters/` (e.g., `pgvector.rs`).

2. Define an `ExtensionAdapter` with a name and function list. The name must
   match `pg_extension.extname`; the registry uses `pg_extension` for activation.

```rust
use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "pgvector",
        functions: vec![
            FunctionAccelEntry {
                schema: "public",
                name: "vector_l2_squared_distance",
                strategy: AccelStrategy::GpuSpatial,
            },
            FunctionAccelEntry {
                schema: "public",
                name: "vector_cosine_distance",
                strategy: AccelStrategy::GpuSpatial,
            },
            FunctionAccelEntry {
                schema: "public",
                name: "vector_ip_distance",
                strategy: AccelStrategy::GpuSpatial,
            },
        ],
    }
}
```

3. Register the adapter in `src/adapters/mod.rs` by adding it to the adapter
   list that gets passed to `AdapterRegistry::register_adapter`.

4. If the function requires a new strategy, add a variant to `AccelStrategy`
   in `src/engine/registry.rs` and implement the corresponding dispatch path
   in `src/engine/dispatch.rs`.

5. Write tests that exercise the new functions under acceleration.

That is the complete process. Most adapters are under 50 lines.

## Running tests

```bash
# Unit tests (pgrx-managed PG)
just test

# Full CI suite (lint + test)
just ci
```

GPU kernel tests require AdaptiveCpp:

```bash
just gpu-build
just gpu-test
```

## Code style

- **Format**: `cargo fmt` using the project `.rustfmt.toml` (style edition 2024,
  100 char max width).
- **Lint**: `cargo clippy -- -D warnings`. The project enforces
  `deny(clippy::all)`, `warn(clippy::pedantic, clippy::nursery)`, and
  `deny(unwrap_used)`.
- **License check**: `cargo deny check` validates dependencies against the
  allowlist in `deny.toml`.
- **SAFETY comments**: Every `unsafe` block must have a `// SAFETY:` comment
  explaining why the invariants hold. No exceptions.
- **No `unwrap()`**: Use `unwrap_or`, `?`, or explicit error handling outside
  of test code.

## Commit convention

This project uses conventional commits. The CI enforces the format.

```
feat(scope): add new capability
fix(scope): correct a bug
perf(scope): improve performance without changing behavior
test(scope): add or update tests
docs(scope): documentation only
refactor(scope): restructure without behavior change
chore(scope): build, CI, dependency updates
```

Common scopes: `adapter`, `engine`, `executor`, `gpu`, `cost`.

Examples:

```
feat(adapter): add pgvector distance function support
fix(executor): handle NULL geometry in join recheck
perf(gpu): skip redundant bbox checks in point_in_ring kernel
test(correctness): add degenerate polygon edge cases
```

## Architecture overview

Before diving in, understand the four layers:

1. **Adapters** (`src/adapters/`) -- declare which functions can be accelerated.
2. **Dispatch** (`src/engine/dispatch.rs`) -- accumulate batches, route to strategy.
3. **Executor Nodes** (`src/engine/executor/`) -- Custom Scan: scan, join, agg, sort.
4. **GPU Kernels** (`pgaccel-kernels/src/`) -- C++/SYCL spatial, H3, raster kernels.

Read `CLAUDE.md` for the full safety rules and architecture details.

## Safety Rules

1. **All PG C functions on main backend thread only.** Never call PG functions from rayon.
2. **rayon is only for**: GPU kernel orchestration, sort-key extraction, top-k merge.
3. **GPU strategies only**: registered acceleration paths must run a GPU kernel; uncertain rows reject/error.
4. **Custom Scan has three vtables**: `CustomPathMethods`, `CustomScanMethods`, `CustomExecMethods`. Never confuse them.
5. **No `unwrap()` outside tests.** Use `unwrap_or`, `?`, or explicit error handling.
6. **`CHECK_FOR_INTERRUPTS()` between batches** on main thread.
7. **Thread budget via shared memory LWLock.** Always release in `before_shmem_exit`.
8. **`PARALLEL SAFE` != thread-safe.** PG parallel = forked processes, not threads.

## Coordination

For larger changes, split work by ownership boundary and keep each patch focused.
Do not let two concurrent changes edit the same file unless the integration
owner has explicitly planned the merge. Public pull requests should include the
motivation, affected subsystems, test coverage, and any runtime behavior notes.

# Contributing to pg_accel

PostgreSQL extension for GPU-accelerated spatial predicates, H3 cell ops, raster
operations, and batched executor nodes. Rust (pgrx 0.17) + C++/SYCL (AdaptiveCpp).

## Development Setup

### Prerequisites

- Rust stable (via asdf or rustup)
- PostgreSQL 17
- cmake (for GPU kernels)
- [cargo-pgrx](https://github.com/pgcentralfoundation/pgrx) 0.17
- [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)

### Quick Start

```bash
just setup          # Install deps, init pgrx for PG 17
just setup-gpu      # Optional: install AdaptiveCpp with Metal backend
```

### Manual Setup

```bash
brew install postgresql@17
cargo install cargo-pgrx --locked
cargo install cargo-deny --locked
cargo pgrx init --pg17 $(brew --prefix postgresql@17)/bin/pg_config
```

## Build Commands

All commands are in the [Justfile](Justfile):

| Command | Description |
|---|---|
| `just fmt` | Format code (`cargo fmt`) |
| `just lint` | Lint (`cargo clippy -- -D warnings`) |
| `just check` | Type check (`cargo check --all-features`) |
| `just deny` | License + advisory check (`cargo deny check`) |
| `just test` | Unit tests (`cargo pgrx test pg17`) |
| `just ci` | Full local CI: lint + test + integration |
| `just package` | Build installable `.so` (`cargo pgrx package`) |
| `just gpu-build` | cmake build for GPU kernels (requires AdaptiveCpp) |
| `just gpu-test` | Run standalone GPU kernel tests |
| `just dev-up` | Start Docker PG container (port 5488) |
| `just dev-watch` | Hot-reload watcher (run in separate terminal) |
| `just dev-test agent=N` | Run integration tests for agent N |
| `just dev-psql agent=N` | Connect to agent N's database |
| `just dev-reset agent=N` | Reset agent N's DB to clean fixtures |

## Adding a new adapter

Adapters teach pg_accel which SQL functions can be accelerated and what strategy
to use. To add support for a new extension (e.g., pgvector):

1. Create a new file in `src/adapters/` (e.g., `pgvector.rs`).

2. Define an `ExtensionAdapter` with a version query and function list:

```rust
use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "pgvector",
        version_query: "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
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

5. Write tests. Add SQL test queries in `docker/tests/` that exercise the new
   functions under acceleration.

That is the complete process. Most adapters are under 50 lines.

## Running tests

```bash
# Unit tests (no PG required)
just test

# Full CI suite (lint + test + integration)
just ci

# Integration tests against Docker PG
just dev-up              # start container
just dev-test agent=0    # run tests for agent 0
just dev-psql agent=0    # interactive psql session
just dev-reset agent=0   # reset to clean fixtures
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

Common scopes: `adapter`, `engine`, `executor`, `gpu`, `cost`, `docker`.

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
3. **Two strategies only**: `BatchedEval` (main thread) and `Gpu*` (GPU kernel + CPU recheck).
4. **Custom Scan has three vtables**: `CustomPathMethods`, `CustomScanMethods`, `CustomExecMethods`. Never confuse them.
5. **No `unwrap()` outside tests.** Use `unwrap_or`, `?`, or explicit error handling.
6. **`CHECK_FOR_INTERRUPTS()` between batches** on main thread.
7. **Thread budget via shared memory LWLock.** Always release in `before_shmem_exit`.
8. **`PARALLEL SAFE` != thread-safe.** PG parallel = forked processes, not threads.

## Agent Coordination

pg_accel uses a multi-agent development model:

- **10 agents per phase.** Each owns specific files — no two agents edit the same file.
- **Plans live in `plans/`.** Each agent updates their checklist and implementation log.
- **Phase gates are binary.** All items must pass before the next phase starts.
- All test queries go in `docker/tests/`. The runner always runs ALL of them (cumulative, no regressions).

### Per-Agent Databases

The Docker dev environment provides isolated databases for concurrent work:

- Single PG container with per-agent databases (`pgaccel_a0` through `pgaccel_a9`).
- Template cloning via `CREATE DATABASE ... TEMPLATE pgaccel_shared`.
- Use `just dev-test agent=N`, `just dev-psql agent=N`, `just dev-reset agent=N`.

### Flock-Based Reload Safety

`flock` coordinates hot-reloads with test runs:

- **Test scripts** (`run_integration_tests.sh`): acquire a **shared** lock — multiple agents can test concurrently.
- **Reload watcher** (`dev_reload.sh`): acquires an **exclusive** lock — waits for all running tests to finish before rebuilding the `.so` and restarting PG.

This prevents test failures caused by mid-test extension reloads.

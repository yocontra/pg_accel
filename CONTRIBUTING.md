# Contributing to pg_accel

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

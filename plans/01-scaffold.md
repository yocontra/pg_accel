# Phase 0: Bootstrap & Infrastructure

**Depends on:** Nothing (greenfield)
**Parallelism:** All 10 agents independent — no shared files

This phase produces a repo that compiles, tests against a real PostgreSQL
instance, passes CI, and is installable from day one. Every subsequent phase
gate runs against this infrastructure — no phase advances without green
Docker integration tests on a real database with real data.

---

## Agent Assignments

### A0 — Cargo Workspace + pgrx Crate + Tooling Config
**Status:** Not Started
**Owns:** `Cargo.toml` (workspace root), `pg_accel/Cargo.toml`, `pg_accel/src/lib.rs`,
`.rustfmt.toml`, `clippy.toml`, `deny.toml`, `.editorconfig`

**Tasks:**
- [ ] Create workspace with two members: `pg_accel` (the extension) and `pg_accel_bench` (benchmark CLI)
- [ ] Configure pgrx crate with feature flags `pg15`, `pg16`, `pg17`, `pg18`
- [ ] Stub `lib.rs` with `pg_module_magic!()`
- [ ] Stub `lib.rs` with `#[pg_guard] pub extern "C" fn _PG_init()` that logs "pg_accel loaded, version X.Y.Z"
- [ ] Stub `lib.rs` with empty module declarations for `core`, `adapters`, `gpu`
- [ ] Create `.rustfmt.toml` with `style_edition = "2024"`, `max_width = 100`, `use_field_init_shorthand = true`
- [ ] Create `clippy.toml` with workspace-level clippy configuration
- [ ] Add workspace `Cargo.toml` lint section: `unsafe_op_in_unsafe_fn = "deny"` under `[workspace.lints.rust]`
- [ ] Add workspace `Cargo.toml` lint section: `all = { level = "deny", priority = -1 }`, `pedantic = { level = "warn", priority = -1 }`, `unwrap_used = "deny"`, `expect_used = "warn"`, `nursery = { level = "warn", priority = -1 }` under `[workspace.lints.clippy]`
- [ ] Create `deny.toml` with allowed licenses: `["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "PostgreSQL"]`
- [ ] Configure `deny.toml` bans: `multiple-versions = "warn"`
- [ ] Configure `deny.toml` advisories: `vulnerability = "deny"`, `unmaintained = "warn"`
- [ ] Create `.editorconfig` for consistent whitespace across Rust + C++ files

**Agent gate:**
- [ ] `cargo pgrx run pg17` then `CREATE EXTENSION pg_accel;` succeeds
- [ ] `SELECT pg_accel.version();` returns version string
- [ ] `cargo fmt -- --check` passes (no formatting issues)
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo deny check` passes (licenses + advisories clean)

**Implementation log:**
_(no deviations)_

### A1 — GPU Kernel Library Scaffold
**Status:** Not Started
**Owns:** `pgaccel-kernels/CMakeLists.txt`, `pgaccel-kernels/include/pgaccel_ffi.h`,
`pgaccel-kernels/src/device_manager.cpp`

**Tasks:**
- [ ] Create `CMakeLists.txt` that finds AdaptiveCpp (acpp) via `find_package(AdaptiveCpp)` or falls back gracefully
- [ ] Auto-detect available backends in CMake (Metal on macOS, CUDA/ROCm/Level Zero on Linux)
- [ ] Build `libpgaccel_kernels.so` / `.dylib` from CMake
- [ ] Stub `pgaccel_init()` in `device_manager.cpp`
- [ ] Stub `pgaccel_shutdown()` in `device_manager.cpp`
- [ ] Stub `pgaccel_get_device_info()` in `device_manager.cpp`
- [ ] Create standalone test binary (`test_device`) that prints device name, backend, memory model, fp64 support

**Agent gate:**
- [ ] `cmake --build build/` produces shared lib (on machine with AdaptiveCpp)
- [ ] `./build/test_device` prints device info (Metal on Mac, CUDA on NVIDIA Linux, etc.)
- [ ] On machine without AdaptiveCpp: cmake configure fails gracefully with clear message
- [ ] On machine without GPU: test_device shows CPU fallback device

**Implementation log:**
_(no deviations)_

### A2 — build.rs + Feature Gating
**Status:** Not Started
**Owns:** `pg_accel/build.rs`, `pg_accel/src/gpu/mod.rs`, `pg_accel/src/gpu/bridge.rs`,
`pg_accel/src/gpu/fallback.rs`

**Tasks:**
- [ ] Create `build.rs` that, when `--features gpu` is set, invokes cmake and links `pgaccel_kernels`
- [ ] Ensure `build.rs` without `gpu` feature has no C++ dependency and no cmake invocation
- [ ] Create `gpu/bridge.rs` with `extern "C"` declarations behind `#[cfg(feature = "gpu")]`
- [ ] Create `gpu/fallback.rs` with stub functions that return `GpuUnavailable` for every GPU op
- [ ] Create `gpu/mod.rs` that re-exports bridge or fallback based on feature flag

**Agent gate:**
- [ ] `cargo build` succeeds without `--features gpu` (pure Rust, no C++ deps)
- [ ] `cargo build --features gpu` succeeds when AdaptiveCpp is installed
- [ ] `cargo build --features gpu` fails clearly when AdaptiveCpp is missing

**Implementation log:**
_(no deviations)_

### A3 — Docker Test Infrastructure
**Status:** Not Started
**Owns:** `docker/docker-compose.test.yml`, `docker/Dockerfile.test`,
`docker/fixtures/`, `docker/scripts/run_integration_tests.sh`

**Tasks:**
- [ ] Create `docker/docker-compose.test.yml` with PostgreSQL 17 with PostGIS 3.5 + h3-pg + pg_accel pre-loaded
- [ ] Configure `shared_preload_libraries = 'pg_accel'` in postgresql.conf
- [ ] Configure persistent volume for test data (avoids re-loading between runs)
- [ ] Configure health check: `pg_isready` every 5s + extension verification
- [ ] Configure SHM size for shared_buffers headroom
- [ ] Map port to non-default (e.g. 5488) to avoid collisions with host PG
- [ ] Create `Dockerfile.test` based on `imresamu/postgis:17-3.5-alpine` (Alpine + PostGIS, small image)
- [ ] In Dockerfile, build h3 C library from source (v4.2.0)
- [ ] In Dockerfile, build h3-pg from source with custom patches (GiST + SP-GiST index fixes)
- [ ] In Dockerfile, group build deps into virtual `.build-deps` package, clean up after
- [ ] In Dockerfile, install pg_accel from local build (`cargo pgrx package`)
- [ ] In Dockerfile, configure shared_preload_libraries
- [ ] Create `docker/autotune.sh` that detects container memory/CPU from cgroup v2 (fallback v1 / `/proc/meminfo`)
- [ ] Autotune: `shared_buffers` = 25% RAM, capped 4GB (test container, not prod)
- [ ] Autotune: `work_mem` = RAM/128, capped 256MB
- [ ] Autotune: `max_parallel_workers_per_gather` = CPUs/2
- [ ] Autotune: `random_page_cost = 1.1` (SSD assumption)
- [ ] Autotune: `jit = off` (deterministic, avoids JIT noise in test results)
- [ ] Autotune: make all parameters overridable via `PG_*` env vars
- [ ] Autotune script replaces default entrypoint, logs detected hardware and calculated settings, then execs `docker-entrypoint.sh`
- [ ] Create per-agent databases: one PG instance with `pgaccel_a{0-9}` databases plus `pgaccel_shared` (canonical fixtures, read-only reference)
- [ ] Each agent database has its own copy of all fixtures; agents can INSERT/UPDATE/DELETE freely with no cross-talk
- [ ] Create reload coordination via reader-writer flock at `/tmp/.pgaccel_reload.lock`
- [ ] SHARED lock (read): agents acquire before running tests (multiple agents test concurrently; watcher blocked from reloading while tests run)
- [ ] EXCLUSIVE lock (write): watcher acquires before reload (waits for all running tests to finish; no agent can start tests during reload; agents that try to test block until reload done)
- [ ] Create `docker/scripts/dev_reload.sh` watcher process that watches `pg_accel/src/` via cargo-watch or fswatch
- [ ] Watcher: on change (debounce 5s to coalesce rapid agent edits), acquire EXCLUSIVE flock, `cargo pgrx package`, `docker cp pg_accel.so` into container, `docker exec pg_ctl restart -D /var/lib/postgresql/data -m fast` (~1s fast restart, container stays up), wait for `pg_isready`, release EXCLUSIVE flock
- [ ] Create `docker/scripts/run_agent_tests.sh` (usage: `run_agent_tests.sh <agent_id> [test_glob]`)
- [ ] Agent test runner: acquire SHARED flock (blocks if reload in progress), run tests against `pgaccel_a${agent_id}`, for each .sql in `docker/tests/`: run with `SET pg_accel.enabled = on` (capture), run with `SET pg_accel.enabled = off` (capture), diff results (PASS/FAIL), release SHARED flock
- [ ] Create `docker/scripts/init_agent_dbs.sh` that creates all 10 databases from template using PG's `CREATE DATABASE ... TEMPLATE pgaccel_shared` (instant COW at filesystem level)
- [ ] Add Justfile commands: `dev-up` (docker compose up + init_agent_dbs.sh), `dev-watch` (dev_reload.sh), `dev-test agent="0"` (run_agent_tests.sh), `dev-test-all` (loop seq 0-9), `dev-psql agent="0"` (psql to pgaccel_a{agent}), `dev-reset agent="0"` (DROP + CREATE DATABASE from template)
- [ ] Create test data fixtures in `docker/fixtures/`: `01_schema.sql` (tables for all test patterns), `02_spatial_data.sql` (100K random points, 1K polygons, deterministic seed), `03_h3_data.sql` (100K h3 cells at various resolutions, GiST + SP-GiST indexes), `04_raster_data.sql` (sample rasters 10x10 to 1000x1000), `05_analytics_data.sql` (1M row employees/events table for aggregate tests), `06_indexes.sql` (GiST, SP-GiST, B-tree indexes on all test tables)
- [ ] All fixture data generated with fixed seeds for deterministic, reproducible results
- [ ] All fixture SQL uses `CREATE TABLE IF NOT EXISTS` and `INSERT ... ON CONFLICT DO NOTHING` (idempotent, safe to re-run after restart)
- [ ] Create `docker/scripts/run_integration_tests.sh`: for each .sql in `docker/tests/`, run with `pg_accel.enabled = on` (capture), run with `pg_accel.enabled = off` (capture), diff (any difference = FAIL), exit 0 = all pass, non-zero = failures listed
- [ ] Create `docker/tests/00_smoke.sql` with: `SELECT * FROM pg_accel_device_info();`, `SELECT ST_AsText(ST_MakePoint(0, 0));` (PostGIS verification), `SELECT h3_lat_lng_to_cell(POINT(40.7128, -74.0060), 7);` (h3-pg verification), `SELECT COUNT(*) FROM analytics_events WHERE value > 0.5;` (basic scan, vanilla path)
- [ ] Each subsequent phase adds test queries to `docker/tests/`; the runner always runs ALL of them, ensuring no regressions

**Agent gate:**
- [ ] `just dev-up` brings PG ready in < 30s, 10 agent databases + shared template created
- [ ] All fixtures load without error into `pgaccel_shared`, cloned to `pgaccel_a{0-9}`
- [ ] `just dev-test agent=0` exits 0 (all smoke tests pass for agent 0)
- [ ] pg_accel extension loaded in all databases (shows in pg_extension catalog)
- [ ] PostGIS + h3-pg both functional alongside pg_accel in all agent databases
- [ ] Per-agent isolation: agent 0 can INSERT/DELETE without affecting agent 1's data
- [ ] `just dev-reset agent=0` restores agent 0's database to clean fixtures in < 1s
- [ ] Hot-reload simulation: start `just dev-watch` in background
- [ ] Hot-reload simulation: launch `just dev-test agent=0` through `agent=4` concurrently (5 parallel tests)
- [ ] Hot-reload simulation: while tests are running, touch a src file to trigger reload
- [ ] Hot-reload simulation: verify watcher waits for all 5 test runs to complete before restarting PG
- [ ] Hot-reload simulation: verify a 6th test run started during reload blocks until reload finishes
- [ ] Hot-reload simulation: verify all test results are correct (no partial reload, no connection errors)
- [ ] Teardown: `docker compose down -v` cleans up completely
- [ ] Works on macOS (Docker Desktop) and Linux (native Docker)

**Implementation log:**
_(no deviations)_

### A4 — CI/CD Pipeline
**Status:** Not Started
**Owns:** `.github/workflows/ci.yml`, `.github/workflows/release.yml`

**Tasks:**
- [ ] Create CI workflow (`.github/workflows/ci.yml`) that runs on every push + PR
- [ ] CI `lint` job (fast, fails fast): `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo deny check`
- [ ] CI `test-unit` job with matrix across PG versions [15, 16, 17, 18]: `cargo pgrx test pg${matrix.pg}`
- [ ] CI `test-integration` job (needs lint): `cargo pgrx package --pg-config $(which pg_config)`, `docker compose -f docker/docker-compose.test.yml up -d`, `docker/scripts/run_integration_tests.sh`, `docker compose down -v`
- [ ] CI `test-gpu-kernels` job: runs only on `macos-14` runner (has Metal), gated on `gpu` label on PR, steps: `cmake --build build/`, `./build/test_device`, `./build/test_kernels`
- [ ] Create release workflow (`.github/workflows/release.yml`) triggered on tag push
- [ ] Release workflow: build `cargo pgrx package` for PG 15-18
- [ ] Release workflow: create GitHub Release with artifacts
- [ ] Release workflow: trigger Homebrew tap update (Phase 11 sets up the tap itself)
- [ ] Configure caching: Rust target dir (`~/.cargo/registry`, `target/`), pgrx PG installs, Docker layers, AdaptiveCpp build

**Agent gate:**
- [ ] CI passes on a clean PR (all jobs green)
- [ ] Lint job completes in < 2 minutes
- [ ] Unit test job completes in < 10 minutes per PG version
- [ ] Integration test job completes in < 5 minutes
- [ ] Failure in any job blocks merge
- [ ] Release workflow produces installable artifacts

**Implementation log:**
_(no deviations)_

### A5 — Justfile + Developer Experience
**Status:** Not Started
**Owns:** `Justfile`, `CLAUDE.md` (project-level)

**Tasks:**
- [ ] Create `Justfile` using [just](https://github.com/casey/just) as the task runner (Rust-native, not Make)
- [ ] Add development commands: `fmt` (`cargo fmt`), `lint` (`cargo clippy -- -D warnings`), `check` (`cargo check --all-features`), `deny` (`cargo deny check`), `test` (`cargo pgrx test pg17`)
- [ ] Add Docker dev environment commands: `dev-up` (docker compose up), `dev-down` (docker compose down -v), `dev-watch` (`docker/scripts/dev_reload.sh`, hot-reload watcher for separate terminal), `dev-test` (`docker/scripts/run_integration_tests.sh`), `dev-psql` (`psql -h localhost -p 5488 -U postgres pgaccel_test`)
- [ ] Add `ci` command: `just fmt lint test dev-up dev-test dev-down` (full CI locally)
- [ ] Add `package` command: `cargo pgrx package --pg-config $(pg_config)` (build installable package)
- [ ] Add GPU kernel commands: `gpu-build` (`cmake -B build pgaccel-kernels && cmake --build build`), `gpu-test` (`./build/test_kernels`)
- [ ] Create `CLAUDE.md` documenting: Rust edition 2024, MSRV, pgrx version
- [ ] Document in `CLAUDE.md`: `just ci` runs everything
- [ ] Document in `CLAUDE.md`: all PRs must pass `just lint` and `just db-test`
- [ ] Document in `CLAUDE.md`: unsafe blocks require `// SAFETY:` comments
- [ ] Document in `CLAUDE.md`: no `unwrap()` outside tests

**Agent gate:**
- [ ] `just ci` runs full lint + test + integration cycle
- [ ] `just db-psql` connects to test database
- [ ] All commands documented via `just --list`
- [ ] CLAUDE.md provides accurate project context

**Implementation log:**
_(no deviations)_

### A6 — Type Extractor + Function Matcher
**Status:** Not Started
**Owns:** `pg_accel/src/core/type_extractor.rs`, `pg_accel/src/core/function_matcher.rs`

**Tasks:**
- [ ] Define `TypeExtractor` trait with methods: `oid(&self) -> pg_sys::Oid`, `extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr`, `pack(&self, repr: &GpuRepr) -> pg_sys::Datum`; trait must be `Send + Sync`
- [ ] Define `GpuRepr` enum with variants: `Float8(f64)`, `Float4(f32)`, `Int8(i64)`, `Int4(i32)`, `Bool(bool)`, `Timestamp(i64)`, `Text(Vec<u8>)`, `Null`, `Bytes(Vec<u8>)` (opaque passthrough)
- [ ] Implement extractors for: float8, float4, int8, int4, bool, timestamp, text, bytea
- [ ] Add `#[cfg(test)]` round-trip test for each of the 8 extractors
- [ ] Define `FunctionPattern` struct with fields: schema, name, arg_types (optional), return_type (optional)
- [ ] Implement `discover_functions()` via SPI query against `pg_proc JOIN pg_type`
- [ ] Read `proparallel` column in `discover_functions()`
- [ ] Handle PG 15-18 differences in `discover_functions()` via compat module
- [ ] Return `Vec<MatchedFunction>` with fields: oid, name, arg_oids, return_oid, is_parallel_safe, is_strict, fmgr_info pointer

**Agent gate:**
- [ ] `cargo test type_extractor` -- all 8 extractors round-trip: extract(datum) -> GpuRepr -> pack() -> identical datum
- [ ] Null handling: extract(_, true) -> GpuRepr::Null for all types
- [ ] `#[pg_test]`: `discover_functions("abs", "int4")` -> 1 match, parallel_safe = true
- [ ] `discover_functions("nonexistent", _)` -> 0 matches

**Implementation log:**
_(no deviations)_

### A7 — Cost Model + GUCs + Adapter Registry
**Status:** Not Started
**Owns:** `pg_accel/src/core/cost.rs`, `pg_accel/src/core/gucs.rs`,
`pg_accel/src/core/registry.rs`

**Tasks:**
- [ ] Define `PlatformProfile` struct with fields: cpu_cores, has_gpu, unified_memory (bool), estimated_gpu_gflops
- [ ] Detect platform profile at init via `sysctl` (macOS) / `/proc/cpuinfo` (Linux) + GPU probe
- [ ] Implement `should_batch(estimated_rows, per_row_cost) -> bool`
- [ ] Implement `should_use_gpu(estimated_rows, operation) -> bool`
- [ ] Implement `optimal_batch_size(estimated_rows) -> usize` with min 256, max 8192
- [ ] Implement `estimate_threads(available_budget) -> usize`
- [ ] Ensure all decision functions are pure functions (no global state, fully testable)
- [ ] Register GUC `pg_accel.enabled` (bool, default on, SET reload)
- [ ] Register GUC `pg_accel.workers` (int, default 0 = auto, SET reload)
- [ ] Register GUC `pg_accel.max_workers_total` (int, default 0 = unlimited, SIGHUP reload)
- [ ] Register GUC `pg_accel.min_batch_size` (int, default 256, SET reload)
- [ ] Register GUC `pg_accel.gpu_enabled` (bool, default on, SET reload)
- [ ] Register GUC `pg_accel.log_level` (enum, default notice, SET reload)
- [ ] Register GUC `pg_accel.kernel_timeout_ms` (int, default 5000, SET reload)
- [ ] Define `ExtensionAdapter` struct with fields: `name: &'static str`, `version_query: &'static str`, `functions: Vec<FunctionAccelEntry>`
- [ ] Define `FunctionAccelEntry` struct with fields: `pattern: FunctionPattern`, `strategy: AccelStrategy` (enum: BatchedEval, GpuSpatial, GpuSort, GpuReduce)
- [ ] Implement `init_adapters()`: check `pg_extension` catalog, register adapters if found
- [ ] Implement `lookup(oid) -> Option<&FunctionAccelEntry>` with O(1) HashMap

**Agent gate:**
- [ ] Unit tests: `should_batch(50, _)` -> false for all profiles
- [ ] `#[pg_test]`: `SET pg_accel.workers = 4; SHOW pg_accel.workers;` -> "4"
- [ ] `#[pg_test]` with PostGIS: `init_adapters()` finds PostGIS, registers functions
- [ ] `#[pg_test]` without PostGIS: `init_adapters()` returns empty, no errors

**Implementation log:**
_(no deviations)_

### A8 — PG Version Compat + Adapter Stubs
**Status:** Not Started
**Owns:** `pg_accel/src/core/ffi/pg_compat.rs`, `pg_accel/src/adapters/mod.rs`,
`postgis.rs`, `postgis_raster.rs`, `h3.rs`, `pg_builtins.rs`

**Tasks:**
- [ ] Create version-gated shims for `prokind` (PG 15+) vs `proisagg`/`proiswindow`
- [ ] Create version-gated shims for Custom Scan method struct layout differences
- [ ] Create version-gated shims for `proparallel` access patterns
- [ ] Create version-gated shims for shared memory API differences
- [ ] Gate all shims behind `#[cfg(feature = "pgXX")]` conditional compilation
- [ ] Create `postgis.rs` adapter stub returning `ExtensionAdapter` with 20+ function entries classified as `BatchedEval` (C functions on main thread) or `GpuSpatial` (spatial predicates with GPU kernels + CPU recheck)
- [ ] Create `postgis_raster.rs` adapter stub returning `ExtensionAdapter` with 8+ function entries classified as `GpuRaster` (raster per-pixel operations with GPU kernels)
- [ ] Create `h3.rs` adapter stub returning `ExtensionAdapter` with 8+ function entries classified as `GpuH3` (h3 cell operations with GPU kernels)
- [ ] Create `pg_builtins.rs` adapter stub returning `ExtensionAdapter` with 10+ function entries classified as `BatchedEval`, `GpuSort`, or `GpuReduce` (GPU-accelerated sort and aggregation)
- [ ] Create `adapters/mod.rs` that re-exports all adapter stubs
- [ ] All adapter stubs contain names + patterns + strategy classification only (no implementation yet)

**Agent gate:**
- [ ] `cargo check --features pg15` through `pg18` -- no warnings (one at a time)
- [ ] `postgis_adapter().functions.len() >= 20`
- [ ] `postgis_raster_adapter().functions.len() >= 8`
- [ ] `h3_adapter().functions.len() >= 8`
- [ ] `pg_builtins_adapter().functions.len() >= 10`

**Implementation log:**
_(no deviations)_

### A9 — Release Infrastructure
**Status:** Not Started
**Owns:** `release-plz.toml`, `.github/workflows/release-plz.yml`,
`cliff.toml` (changelog generation)

**Tasks:**
- [ ] Create `release-plz.toml` with: `changelog_config = "cliff.toml"`, `publish_timeout = "10m"`, `git_tag_enable = true` under `[workspace]`
- [ ] Create `cliff.toml` for git-cliff changelog generation from conventional commits
- [ ] Configure git-cliff to handle: `feat:` / `fix:` / `perf:` / `breaking:` -> structured CHANGELOG.md
- [ ] Configure git-cliff scoped commits: `feat(gpu):`, `fix(postgis):`, `perf(h3):`
- [ ] Validate `cargo pgrx package` via `just package` produces installable `.so` + SQL files
- [ ] Verify install path matches PG's `SHAREDIR` and `PKGLIBDIR`
- [ ] Test: install package into Docker container, `CREATE EXTENSION pg_accel` succeeds
- [ ] Add conventional commit enforcement job in `.github/workflows/ci.yml`: validate PR title follows conventional commits
- [ ] Create `.github/workflows/release-plz.yml` for automated releases

**Agent gate:**
- [ ] `just package` produces valid pgrx package
- [ ] Package installs into Docker PG and `CREATE EXTENSION` succeeds
- [ ] Conventional commit check rejects "fix stuff" but accepts "fix(gpu): handle timeout"
- [ ] `release-plz` config validates cleanly
- [ ] git-cliff generates sample changelog from test commits

**Implementation log:**
_(no deviations)_

---

## Phase Gate

All of the following must pass:

- [ ] cargo pgrx test pg17 -- all tests pass
- [ ] cargo check --features pg15,pg16,pg17,pg18 -- no errors (one at a time)
- [ ] cargo fmt -- --check -- zero formatting issues
- [ ] cargo clippy -- -D warnings -- zero warnings
- [ ] cargo deny check -- licenses + advisories clean
- [ ] cargo build (no gpu feature) -- succeeds, pure Rust
- [ ] cargo build --features gpu -- succeeds on machine with AdaptiveCpp
- [ ] CREATE EXTENSION pg_accel; -- loads without error on PG 17
- [ ] Extension log shows "pg_accel loaded"
- [ ] GUC SET/SHOW works for all 7 GUCs
- [ ] Type extractors round-trip all 8 types
- [ ] Function matcher finds known PG builtins
- [ ] Docker: `just dev-up` -> PG 17 + PostGIS + h3-pg + pg_accel running, 10 agent DBs created
- [ ] Docker: all fixtures load, smoke test queries pass in each agent database
- [ ] Docker: `just dev-test agent=0` exits 0 (ON == OFF for all queries)
- [ ] Docker: per-agent isolation verified (mutations in agent 0 don't affect agent 1)
- [ ] Docker: `just dev-reset agent=0` restores to clean fixtures via TEMPLATE clone (< 1s)
- [ ] Docker: hot-reload watcher rebuilds + reloads PG in < 5s on src change
- [ ] Docker: concurrent agent simulation -- 5 parallel test runs + reload trigger:
- [ ] Watcher waits for running tests before restart (flock exclusive blocks)
- [ ] Tests started during reload block until reload complete (flock shared blocks)
- [ ] All test results correct, zero connection errors, zero partial reloads
- [ ] CI: all GitHub Actions jobs pass on clean PR
- [ ] CI: lint + unit + integration pipeline completes in < 20 minutes
- [ ] just ci -- full local cycle passes
- [ ] just package -- produces installable pgrx package
- [ ] Package installs into Docker PG and CREATE EXTENSION succeeds

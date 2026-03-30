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
**Status:** Complete
**Owns:** `Cargo.toml` (workspace root), `pg_accel/Cargo.toml`, `pg_accel/src/lib.rs`,
`.rustfmt.toml`, `clippy.toml`, `deny.toml`, `.editorconfig`

**Tasks:**
- [x] Create workspace with two members: `pg_accel` (the extension) and `pg_accel_bench` (benchmark CLI)
- [x] Configure pgrx crate with feature flags `pg15`, `pg16`, `pg17`, `pg18`
- [x] Stub `lib.rs` with `pg_module_magic!()`
- [x] Stub `lib.rs` with `#[pg_guard] pub extern "C" fn _PG_init()` that logs "pg_accel loaded, version X.Y.Z"
- [x] Stub `lib.rs` with empty module declarations for `core`, `adapters`, `gpu`
- [x] Create `.rustfmt.toml` with `style_edition = "2024"`, `max_width = 100`, `use_field_init_shorthand = true`
- [x] Create `clippy.toml` with workspace-level clippy configuration
- [x] Add workspace `Cargo.toml` lint section: `unsafe_op_in_unsafe_fn = "deny"` under `[workspace.lints.rust]`
- [x] Add workspace `Cargo.toml` lint section: `all = { level = "deny", priority = -1 }`, `pedantic = { level = "warn", priority = -1 }`, `unwrap_used = "deny"`, `expect_used = "warn"`, `nursery = { level = "warn", priority = -1 }` under `[workspace.lints.clippy]`
- [x] Create `deny.toml` with allowed licenses: `["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "PostgreSQL"]`
- [x] Configure `deny.toml` bans: `multiple-versions = "warn"`
- [x] Configure `deny.toml` advisories: `vulnerability = "deny"`, `unmaintained = "warn"`
- [x] Create `.editorconfig` for consistent whitespace across Rust + C++ files

**Agent gate:**
- [x] `cargo pgrx run pg17` then `CREATE EXTENSION pg_accel;` succeeds
- [x] `SELECT pg_accel.version();` returns version string
- [x] `cargo fmt -- --check` passes (no formatting issues)
- [x] `cargo clippy -- -D warnings` passes
- [x] `cargo deny check` passes (licenses + advisories clean)

**Implementation log:**
Phase 0 completed — all items passing.

### A1 — GPU Kernel Library Scaffold
**Status:** Complete
**Owns:** `pgaccel-kernels/CMakeLists.txt`, `pgaccel-kernels/include/pgaccel_ffi.h`,
`pgaccel-kernels/src/device_manager.cpp`

**Tasks:**
- [x] Create `CMakeLists.txt` that finds AdaptiveCpp (acpp) via `find_package(AdaptiveCpp)` or falls back gracefully
- [x] Auto-detect available backends in CMake (Metal on macOS, CUDA/ROCm/Level Zero on Linux)
- [x] Build `libpgaccel_kernels.so` / `.dylib` from CMake
- [x] Stub `pgaccel_init()` in `device_manager.cpp`
- [x] Stub `pgaccel_shutdown()` in `device_manager.cpp`
- [x] Stub `pgaccel_get_device_info()` in `device_manager.cpp`
- [x] Create standalone test binary (`test_device`) that prints device name, backend, memory model, fp64 support

**Agent gate:**
- [x] `cmake --build build/` produces shared lib (on machine with AdaptiveCpp)
- [x] `./build/test_device` prints device info (Metal on Mac, CUDA on NVIDIA Linux, etc.)
- [x] On machine without AdaptiveCpp: cmake configure fails gracefully with clear message
- [x] On machine without GPU: test_device shows CPU fallback device

**Implementation log:**
Phase 0 completed — all items passing.

### A2 — build.rs + Feature Gating
**Status:** Complete
**Owns:** `pg_accel/build.rs`, `pg_accel/src/gpu/mod.rs`, `pg_accel/src/gpu/bridge.rs`,
`pg_accel/src/gpu/fallback.rs`

**Tasks:**
- [x] Create `build.rs` that, when `--features gpu` is set, invokes cmake and links `pgaccel_kernels`
- [x] Ensure `build.rs` without `gpu` feature has no C++ dependency and no cmake invocation
- [x] Create `gpu/bridge.rs` with `extern "C"` declarations behind `#[cfg(feature = "gpu")]`
- [x] Create `gpu/fallback.rs` with stub functions that return `GpuUnavailable` for every GPU op
- [x] Create `gpu/mod.rs` that re-exports bridge or fallback based on feature flag

**Agent gate:**
- [x] `cargo build` succeeds without `--features gpu` (pure Rust, no C++ deps)
- [x] `cargo build --features gpu` succeeds when AdaptiveCpp is installed
- [x] `cargo build --features gpu` fails clearly when AdaptiveCpp is missing

**Implementation log:**
Phase 0 completed — all items passing.

### A3 — Docker Test Infrastructure
**Status:** Complete
**Owns:** `docker/docker-compose.test.yml`, `docker/Dockerfile.test`,
`docker/fixtures/`, `docker/scripts/run_integration_tests.sh`

**Tasks:**
- [x] Create `docker/docker-compose.test.yml` with PostgreSQL 17 with PostGIS 3.5 + h3-pg + pg_accel pre-loaded
- [x] Configure `shared_preload_libraries = 'pg_accel'` in postgresql.conf
- [x] Configure persistent volume for test data (avoids re-loading between runs)
- [x] Configure health check: `pg_isready` every 5s + extension verification
- [x] Configure SHM size for shared_buffers headroom
- [x] Map port to non-default (e.g. 5488) to avoid collisions with host PG
- [x] Create `Dockerfile.test` based on `imresamu/postgis:17-3.5-alpine` (Alpine + PostGIS, small image)
- [x] In Dockerfile, build h3 C library from source (v4.2.0)
- [x] In Dockerfile, build h3-pg from source with custom patches (GiST + SP-GiST index fixes)
- [x] In Dockerfile, group build deps into virtual `.build-deps` package, clean up after
- [x] In Dockerfile, install pg_accel from local build (`cargo pgrx package`)
- [x] In Dockerfile, configure shared_preload_libraries
- [x] Create `docker/autotune.sh` that detects container memory/CPU from cgroup v2 (fallback v1 / `/proc/meminfo`)
- [x] Autotune: `shared_buffers` = 25% RAM, capped 4GB (test container, not prod)
- [x] Autotune: `work_mem` = RAM/128, capped 256MB
- [x] Autotune: `max_parallel_workers_per_gather` = CPUs/2
- [x] Autotune: `random_page_cost = 1.1` (SSD assumption)
- [x] Autotune: `jit = off` (deterministic, avoids JIT noise in test results)
- [x] Autotune: make all parameters overridable via `PG_*` env vars
- [x] Autotune script replaces default entrypoint, logs detected hardware and calculated settings, then execs `docker-entrypoint.sh`
- [x] Create per-agent databases: one PG instance with `pgaccel_a{0-9}` databases plus `pgaccel_shared` (canonical fixtures, read-only reference)
- [x] Each agent database has its own copy of all fixtures; agents can INSERT/UPDATE/DELETE freely with no cross-talk
- [x] Create reload coordination via reader-writer flock at `/tmp/.pgaccel_reload.lock`
- [x] SHARED lock (read): agents acquire before running tests (multiple agents test concurrently; watcher blocked from reloading while tests run)
- [x] EXCLUSIVE lock (write): watcher acquires before reload (waits for all running tests to finish; no agent can start tests during reload; agents that try to test block until reload done)
- [x] Create `docker/scripts/dev_reload.sh` watcher process that watches `pg_accel/src/` via cargo-watch or fswatch
- [x] Watcher: on change (debounce 5s to coalesce rapid agent edits), acquire EXCLUSIVE flock, `cargo pgrx package`, `docker cp pg_accel.so` into container, `docker exec pg_ctl restart -D /var/lib/postgresql/data -m fast` (~1s fast restart, container stays up), wait for `pg_isready`, release EXCLUSIVE flock
- [x] Create `docker/scripts/run_agent_tests.sh` (usage: `run_agent_tests.sh <agent_id> [test_glob]`)
- [x] Agent test runner: acquire SHARED flock (blocks if reload in progress), run tests against `pgaccel_a${agent_id}`, for each .sql in `docker/tests/`: run with `SET pg_accel.enabled = on` (capture), run with `SET pg_accel.enabled = off` (capture), diff results (PASS/FAIL), release SHARED flock
- [x] Create `docker/scripts/init_agent_dbs.sh` that creates all 10 databases from template using PG's `CREATE DATABASE ... TEMPLATE pgaccel_shared` (instant COW at filesystem level)
- [x] Add Justfile commands: `dev-up` (docker compose up + init_agent_dbs.sh), `dev-watch` (dev_reload.sh), `dev-test agent="0"` (run_agent_tests.sh), `dev-test-all` (loop seq 0-9), `dev-psql agent="0"` (psql to pgaccel_a{agent}), `dev-reset agent="0"` (DROP + CREATE DATABASE from template)
- [x] Create test data fixtures in `docker/fixtures/`: `01_schema.sql` (tables for all test patterns), `02_spatial_data.sql` (100K random points, 1K polygons, deterministic seed), `03_h3_data.sql` (100K h3 cells at various resolutions, GiST + SP-GiST indexes), `04_raster_data.sql` (sample rasters 10x10 to 1000x1000), `05_analytics_data.sql` (1M row employees/events table for aggregate tests), `06_indexes.sql` (GiST, SP-GiST, B-tree indexes on all test tables)
- [x] All fixture data generated with fixed seeds for deterministic, reproducible results
- [x] All fixture SQL uses `CREATE TABLE IF NOT EXISTS` and `INSERT ... ON CONFLICT DO NOTHING` (idempotent, safe to re-run after restart)
- [x] Create `docker/scripts/run_integration_tests.sh`: for each .sql in `docker/tests/`, run with `pg_accel.enabled = on` (capture), run with `pg_accel.enabled = off` (capture), diff (any difference = FAIL), exit 0 = all pass, non-zero = failures listed
- [x] Create `docker/tests/00_smoke.sql` with: `SELECT * FROM pg_accel_device_info();`, `SELECT ST_AsText(ST_MakePoint(0, 0));` (PostGIS verification), `SELECT h3_latlng_to_cell(POINT(40.7128, -74.0060), 7);` (h3-pg verification), `SELECT COUNT(*) FROM analytics_events WHERE value > 0.5;` (basic scan, vanilla path)
- [x] Each subsequent phase adds test queries to `docker/tests/`; the runner always runs ALL of them, ensuring no regressions

**Agent gate:**
- [x] `just dev-up` brings PG ready in < 30s, 10 agent databases + shared template created
- [x] All fixtures load without error into `pgaccel_shared`, cloned to `pgaccel_a{0-9}`
- [x] `just dev-test agent=0` exits 0 (all smoke tests pass for agent 0)
- [x] pg_accel extension loaded in all databases (shows in pg_extension catalog)
- [x] PostGIS + h3-pg both functional alongside pg_accel in all agent databases
- [x] Per-agent isolation: agent 0 can INSERT/DELETE without affecting agent 1's data
- [x] `just dev-reset agent=0` restores agent 0's database to clean fixtures in < 1s
- [x] Hot-reload simulation: start `just dev-watch` in background
- [x] Hot-reload simulation: launch `just dev-test agent=0` through `agent=4` concurrently (5 parallel tests)
- [x] Hot-reload simulation: while tests are running, touch a src file to trigger reload
- [x] Hot-reload simulation: verify watcher waits for all 5 test runs to complete before restarting PG
- [x] Hot-reload simulation: verify a 6th test run started during reload blocks until reload finishes
- [x] Hot-reload simulation: verify all test results are correct (no partial reload, no connection errors)
- [x] Teardown: `docker compose down -v` cleans up completely
- [x] Works on macOS (Docker Desktop) and Linux (native Docker)

**Implementation log:**
Phase 0 completed — all items passing.

### A4 — CI/CD Pipeline
**Status:** Complete
**Owns:** `.github/workflows/ci.yml`, `.github/workflows/release.yml`

**Tasks:**
- [x] Create CI workflow (`.github/workflows/ci.yml`) that runs on every push + PR
- [x] CI `lint` job (fast, fails fast): `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo deny check`
- [x] CI `test-unit` job with matrix across PG versions [15, 16, 17, 18]: `cargo pgrx test pg${matrix.pg}`
- [x] CI `test-integration` job (needs lint): `cargo pgrx package --pg-config $(which pg_config)`, `docker compose -f docker/docker-compose.test.yml up -d`, `docker/scripts/run_integration_tests.sh`, `docker compose down -v`
- [x] CI `test-gpu-kernels` job: runs only on `macos-14` runner (has Metal), gated on `gpu` label on PR, steps: `cmake --build build/`, `./build/test_device`, `./build/test_kernels`
- [x] Create release workflow (`.github/workflows/release.yml`) triggered on tag push
- [x] Release workflow: build `cargo pgrx package` for PG 15-18
- [x] Release workflow: create GitHub Release with artifacts
- [x] Release workflow: trigger Homebrew tap update (Phase 11 sets up the tap itself)
- [x] Configure caching: Rust target dir (`~/.cargo/registry`, `target/`), pgrx PG installs, Docker layers, AdaptiveCpp build

**Agent gate:**
- [x] CI passes on a clean PR (all jobs green)
- [x] Lint job completes in < 2 minutes
- [x] Unit test job completes in < 10 minutes per PG version
- [x] Integration test job completes in < 5 minutes
- [x] Failure in any job blocks merge
- [x] Release workflow produces installable artifacts

**Implementation log:**
Phase 0 completed — all items passing.

### A5 — Justfile + Developer Experience
**Status:** Complete
**Owns:** `Justfile`, `CLAUDE.md` (project-level)

**Tasks:**
- [x] Create `Justfile` using [just](https://github.com/casey/just) as the task runner (Rust-native, not Make)
- [x] Add development commands: `fmt` (`cargo fmt`), `lint` (`cargo clippy -- -D warnings`), `check` (`cargo check --all-features`), `deny` (`cargo deny check`), `test` (`cargo pgrx test pg17`)
- [x] Add Docker dev environment commands: `dev-up` (docker compose up), `dev-down` (docker compose down -v), `dev-watch` (`docker/scripts/dev_reload.sh`, hot-reload watcher for separate terminal), `dev-test` (`docker/scripts/run_integration_tests.sh`), `dev-psql` (`psql -h localhost -p 5488 -U postgres pgaccel_test`)
- [x] Add `ci` command: `just fmt lint test dev-up dev-test dev-down` (full CI locally)
- [x] Add `package` command: `cargo pgrx package --pg-config $(pg_config)` (build installable package)
- [x] Add GPU kernel commands: `gpu-build` (`cmake -B build pgaccel-kernels && cmake --build build`), `gpu-test` (`./build/test_kernels`)
- [x] Create `CLAUDE.md` documenting: Rust edition 2024, MSRV, pgrx version
- [x] Document in `CLAUDE.md`: `just ci` runs everything
- [x] Document in `CLAUDE.md`: all PRs must pass `just lint` and `just db-test`
- [x] Document in `CLAUDE.md`: unsafe blocks require `// SAFETY:` comments
- [x] Document in `CLAUDE.md`: no `unwrap()` outside tests

**Agent gate:**
- [x] `just ci` runs full lint + test + integration cycle
- [x] `just db-psql` connects to test database
- [x] All commands documented via `just --list`
- [x] CLAUDE.md provides accurate project context

**Implementation log:**
Phase 0 completed — all items passing.

### A6 — Type Extractor + Function Matcher
**Status:** Complete
**Owns:** `pg_accel/src/core/type_extractor.rs`, `pg_accel/src/core/function_matcher.rs`

**Tasks:**
- [x] Define `TypeExtractor` trait with methods: `oid(&self) -> pg_sys::Oid`, `extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr`, `pack(&self, repr: &GpuRepr) -> pg_sys::Datum`; trait must be `Send + Sync`
- [x] Define `GpuRepr` enum with variants: `Float8(f64)`, `Float4(f32)`, `Int8(i64)`, `Int4(i32)`, `Bool(bool)`, `Timestamp(i64)`, `Text(Vec<u8>)`, `Null`, `Bytes(Vec<u8>)` (opaque passthrough)
- [x] Implement extractors for: float8, float4, int8, int4, bool, timestamp, text, bytea
- [x] Add `#[cfg(test)]` round-trip test for each of the 8 extractors
- [x] Define `FunctionPattern` struct with fields: schema, name, arg_types (optional), return_type (optional)
- [x] Implement `discover_functions()` via SPI query against `pg_proc JOIN pg_type`
- [x] Read `proparallel` column in `discover_functions()`
- [x] Handle PG 15-18 differences in `discover_functions()` via compat module
- [x] Return `Vec<MatchedFunction>` with fields: oid, name, arg_oids, return_oid, is_parallel_safe, is_strict, fmgr_info pointer

**Agent gate:**
- [x] `cargo test type_extractor` -- all 8 extractors round-trip: extract(datum) -> GpuRepr -> pack() -> identical datum
- [x] Null handling: extract(_, true) -> GpuRepr::Null for all types
- [x] `#[pg_test]`: `discover_functions("abs", "int4")` -> 1 match, parallel_safe = true
- [x] `discover_functions("nonexistent", _)` -> 0 matches

**Implementation log:**
Phase 0 completed — all items passing.

### A7 — Cost Model + GUCs + Adapter Registry
**Status:** Complete
**Owns:** `pg_accel/src/core/cost.rs`, `pg_accel/src/core/gucs.rs`,
`pg_accel/src/core/registry.rs`

**Tasks:**
- [x] Define `PlatformProfile` struct with fields: cpu_cores, has_gpu, unified_memory (bool), estimated_gpu_gflops
- [x] Detect platform profile at init via `sysctl` (macOS) / `/proc/cpuinfo` (Linux) + GPU probe
- [x] Implement `should_batch(estimated_rows, per_row_cost) -> bool`
- [x] Implement `should_use_gpu(estimated_rows, operation) -> bool`
- [x] Implement `optimal_batch_size(estimated_rows) -> usize` with min 256, max 8192
- [x] Implement `estimate_threads(available_budget) -> usize`
- [x] Ensure all decision functions are pure functions (no global state, fully testable)
- [x] Register GUC `pg_accel.enabled` (bool, default on, SET reload)
- [x] Register GUC `pg_accel.workers` (int, default 0 = auto, SET reload)
- [x] Register GUC `pg_accel.max_workers_total` (int, default 0 = unlimited, SIGHUP reload)
- [x] Register GUC `pg_accel.min_batch_size` (int, default 256, SET reload)
- [x] Register GUC `pg_accel.gpu_enabled` (bool, default on, SET reload)
- [x] Register GUC `pg_accel.log_level` (enum, default notice, SET reload)
- [x] Register GUC `pg_accel.kernel_timeout_ms` (int, default 5000, SET reload)
- [x] Define `ExtensionAdapter` struct with fields: `name: &'static str`, `version_query: &'static str`, `functions: Vec<FunctionAccelEntry>`
- [x] Define `FunctionAccelEntry` struct with fields: `pattern: FunctionPattern`, `strategy: AccelStrategy` (enum: BatchedEval, GpuSpatial, GpuSort, GpuReduce)
- [x] Implement `init_adapters()`: check `pg_extension` catalog, register adapters if found
- [x] Implement `lookup(oid) -> Option<&FunctionAccelEntry>` with O(1) HashMap

**Agent gate:**
- [x] Unit tests: `should_batch(50, _)` -> false for all profiles
- [x] `#[pg_test]`: `SET pg_accel.workers = 4; SHOW pg_accel.workers;` -> "4"
- [x] `#[pg_test]` with PostGIS: `init_adapters()` finds PostGIS, registers functions
- [x] `#[pg_test]` without PostGIS: `init_adapters()` returns empty, no errors

**Implementation log:**
Phase 0 completed — all items passing.

### A8 — PG Version Compat + Adapter Stubs
**Status:** Complete
**Owns:** `pg_accel/src/core/ffi/pg_compat.rs`, `pg_accel/src/adapters/mod.rs`,
`postgis.rs`, `postgis_raster.rs`, `h3.rs`, `pg_builtins.rs`

**Tasks:**
- [x] Create version-gated shims for `prokind` (PG 15+) vs `proisagg`/`proiswindow`
- [x] Create version-gated shims for Custom Scan method struct layout differences
- [x] Create version-gated shims for `proparallel` access patterns
- [x] Create version-gated shims for shared memory API differences
- [x] Gate all shims behind `#[cfg(feature = "pgXX")]` conditional compilation
- [x] Create `postgis.rs` adapter stub returning `ExtensionAdapter` with 20+ function entries classified as `BatchedEval` (C functions on main thread) or `GpuSpatial` (spatial predicates with GPU kernels + CPU recheck)
- [x] Create `postgis_raster.rs` adapter stub returning `ExtensionAdapter` with 8+ function entries classified as `GpuRaster` (raster per-pixel operations with GPU kernels)
- [x] Create `h3.rs` adapter stub returning `ExtensionAdapter` with 8+ function entries classified as `GpuH3` (h3 cell operations with GPU kernels)
- [x] Create `pg_builtins.rs` adapter stub returning `ExtensionAdapter` with 10+ function entries classified as `BatchedEval`, `GpuSort`, or `GpuReduce` (GPU-accelerated sort and aggregation)
- [x] Create `adapters/mod.rs` that re-exports all adapter stubs
- [x] All adapter stubs contain names + patterns + strategy classification only (no implementation yet)

**Agent gate:**
- [x] `cargo check --features pg15` through `pg18` -- no warnings (one at a time)
- [x] `postgis_adapter().functions.len() >= 20`
- [x] `postgis_raster_adapter().functions.len() >= 8`
- [x] `h3_adapter().functions.len() >= 8`
- [x] `pg_builtins_adapter().functions.len() >= 10`

**Implementation log:**
Phase 0 completed — all items passing.

### A9 — Release Infrastructure
**Status:** Complete
**Owns:** `release-plz.toml`, `.github/workflows/release-plz.yml`,
`cliff.toml` (changelog generation)

**Tasks:**
- [x] Create `release-plz.toml` with: `changelog_config = "cliff.toml"`, `publish_timeout = "10m"`, `git_tag_enable = true` under `[workspace]`
- [x] Create `cliff.toml` for git-cliff changelog generation from conventional commits
- [x] Configure git-cliff to handle: `feat:` / `fix:` / `perf:` / `breaking:` -> structured CHANGELOG.md
- [x] Configure git-cliff scoped commits: `feat(gpu):`, `fix(postgis):`, `perf(h3):`
- [x] Validate `cargo pgrx package` via `just package` produces installable `.so` + SQL files
- [x] Verify install path matches PG's `SHAREDIR` and `PKGLIBDIR`
- [x] Test: install package into Docker container, `CREATE EXTENSION pg_accel` succeeds
- [x] Add conventional commit enforcement job in `.github/workflows/ci.yml`: validate PR title follows conventional commits
- [x] Create `.github/workflows/release-plz.yml` for automated releases

**Agent gate:**
- [x] `just package` produces valid pgrx package
- [x] Package installs into Docker PG and `CREATE EXTENSION` succeeds
- [x] Conventional commit check rejects "fix stuff" but accepts "fix(gpu): handle timeout"
- [x] `release-plz` config validates cleanly
- [x] git-cliff generates sample changelog from test commits

**Implementation log:**
Phase 0 completed — all items passing.

---

## Phase Gate

All of the following must pass:

- [x] cargo pgrx test pg17 -- all tests pass
- [x] cargo check --features pg15,pg16,pg17,pg18 -- no errors (one at a time)
- [x] cargo fmt -- --check -- zero formatting issues
- [x] cargo clippy -- -D warnings -- zero warnings
- [x] cargo deny check -- licenses + advisories clean
- [x] cargo build (no gpu feature) -- succeeds, pure Rust
- [x] cargo build --features gpu -- succeeds on machine with AdaptiveCpp
- [x] CREATE EXTENSION pg_accel; -- loads without error on PG 17
- [x] Extension log shows "pg_accel loaded"
- [x] GUC SET/SHOW works for all 7 GUCs
- [x] Type extractors round-trip all 8 types
- [x] Function matcher finds known PG builtins
- [x] Docker: `just dev-up` -> PG 17 + PostGIS + h3-pg + pg_accel running, 10 agent DBs created
- [x] Docker: all fixtures load, smoke test queries pass in each agent database
- [x] Docker: `just dev-test agent=0` exits 0 (ON == OFF for all queries)
- [x] Docker: per-agent isolation verified (mutations in agent 0 don't affect agent 1)
- [x] Docker: `just dev-reset agent=0` restores to clean fixtures via TEMPLATE clone (< 1s)
- [x] Docker: hot-reload watcher rebuilds + reloads PG in < 5s on src change
- [x] Docker: concurrent agent simulation -- 5 parallel test runs + reload trigger:
- [x] Watcher waits for running tests before restart (flock exclusive blocks)
- [x] Tests started during reload block until reload complete (flock shared blocks)
- [x] All test results correct, zero connection errors, zero partial reloads
- [x] CI: all GitHub Actions jobs pass on clean PR
- [x] CI: lint + unit + integration pipeline completes in < 20 minutes
- [x] just ci -- full local cycle passes
- [x] just package -- produces installable pgrx package
- [x] Package installs into Docker PG and CREATE EXTENSION succeeds

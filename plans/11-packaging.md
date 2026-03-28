# Phase 11: Packaging + Documentation

**Depends on:** Phase 10 (benchmarks complete, numbers finalized)
**Parallelism:** All 10 agents

**Note:** Phase 0 already established: Docker test image (Alpine + PostGIS + h3-pg),
basic CI pipeline, release-plz config, `cargo pgrx package`, and Justfile.
This phase builds the **production** packaging: Homebrew tap, production Docker
image, user documentation, and contributor guides.

---

## Agent Assignments

### A0 — Homebrew Tap
**Status:** Not Started
**Owns:** `homebrew-pg-accel/` repo structure, `Formula/pg_accel.rb`

**Tasks:**
- [ ] Create Homebrew tap repo structure (`homebrew-pg-accel` or within main repo for now)
- [ ] Write formula so `brew tap pg-accel/tap && brew install pg_accel` works
- [ ] Add formula dependency on `postgresql@17`, optional dependencies on `postgis` and `h3`
- [ ] Configure formula to build via `cargo pgrx install` pointing at Homebrew's `pg_config`
- [ ] Add post-install caveat telling user to add to `shared_preload_libraries` and run `CREATE EXTENSION`
- [ ] Add bottle support for Apple Silicon (pre-built binary)

**Agent gate:**
- [ ] Clean macOS (only Homebrew PG installed): `brew install pg_accel` works
- [ ] After install: `psql -c "CREATE EXTENSION pg_accel;"` succeeds
- [ ] `pg_accel_device_info()` returns correct data
- [ ] Post-install message clearly states `shared_preload_libraries` requirement

**Implementation log:**
_(no deviations)_

### A1 — Production Docker Image
**Status:** Not Started
**Owns:** `Dockerfile`, `docker-compose.yml`

**Tasks:**
- [ ] Evolve Phase 0's test image into a production-ready user-facing image
- [ ] Base on `imresamu/postgis:17-3.5-alpine` (same base as test image)
- [ ] Build h3-pg from source (same as test image, with custom patches)
- [ ] Configure as CPU-only (no GPU in container -- Metal doesn't work in Docker)
- [ ] Include autotune script from Phase 0 (detects cgroup memory/CPU, configures PG)
- [ ] Make `pg_accel.workers` configurable via `PGACCEL_WORKERS` env var
- [ ] Pre-configure `shared_preload_libraries`
- [ ] Add health check: `pg_isready` + extension verification
- [ ] Create docker-compose with sample data + example queries
- [ ] Minimize image: clean up `.build-deps` after compilation

**Agent gate:**
- [ ] `docker run pg-accel:latest -c "SELECT * FROM pg_accel_device_info()"` works
- [ ] `docker-compose up` ready with sample data in < 30s
- [ ] Autotune logs show detected hardware + calculated settings
- [ ] CPU-only mode: all workloads correct
- [ ] Image size < 300MB (Alpine base)

**Implementation log:**
_(no deviations)_

### A2 — README.md
**Status:** Not Started
**Owns:** `README.md`

**Tasks:**
- [ ] Write one-line description + badge row
- [ ] Write 3-line quickstart (brew install, add to shared_preload_libraries, CREATE EXTENSION)
- [ ] Write "What it does" section -- 1 paragraph, no jargon
- [ ] Create benchmark highlight table (3 key workloads, 3 columns)
- [ ] Include EXPLAIN ANALYZE example showing Custom Scan node
- [ ] Write "How it works" section -- 3 bullet points (batch-parallel, executor nodes, GPU spatial)
- [ ] Create configuration table of GUCs with when-to-change guidance
- [ ] Write "Supported extensions" section -- PostGIS, h3-pg, stock PG
- [ ] Write "Adding your own adapter" section -- link to CONTRIBUTING.md
- [ ] Write FAQ (5 items: "Does this work without GPU?", "Does this slow down OLTP?", "How is this different from PG-Strom?", "Which PG versions?", "How do I turn it off?")

**Agent gate:**
- [ ] All install commands verified working
- [ ] Benchmark numbers match BENCHMARKS.md
- [ ] FAQ answers are accurate
- [ ] No dead links
- [ ] Renders correctly on GitHub

**Implementation log:**
_(no deviations)_

### A3 — ARCHITECTURE.md
**Status:** Not Started
**Owns:** `ARCHITECTURE.md`

**Tasks:**
- [ ] Document the four layers: adapters -> dispatch -> executor nodes -> GPU kernels
- [ ] Document data flow: query -> planner hook -> Custom Scan path -> executor -> batch dispatch -> results
- [ ] Document thread model: rayon threads, signal masking, thread budget, interaction with PG parallel
- [ ] Document three-layer GPU model: bbox -> geometric -> CPU recheck
- [ ] Document adapter pattern: how functions are discovered, matched, dispatched
- [ ] Document Custom Scan FFI: how we bridge Rust <-> PG's C planner/executor
- [ ] Document testing philosophy: correctness first, every result must match vanilla PG

**Agent gate:**
- [ ] A developer unfamiliar with the project can read this and understand the architecture
- [ ] Diagrams (ASCII art) for data flow and three-layer model
- [ ] Covers the "why" not just the "what"

**Implementation log:**
_(no deviations)_

### A4 — CONTRIBUTING.md
**Status:** Not Started
**Owns:** `CONTRIBUTING.md`

**Tasks:**
- [ ] Write step-by-step guide to adding a new adapter: create `adapters/myext.rs`
- [ ] Document how to define `FunctionAccelEntry` for each function
- [ ] Document how to implement `TypeExtractor` if extension has custom types
- [ ] Document how to add to `all_adapters()`
- [ ] Document how to write tests
- [ ] Document how to run benchmarks
- [ ] Create worked example: pgvector adapter (distance functions)
- [ ] Create decision tree: "Should my function use BatchedEval or GpuSpatial?"

**Agent gate:**
- [ ] Guide is complete and followable
- [ ] pgvector example is correct and tested
- [ ] Links to relevant source files

**Implementation log:**
_(no deviations)_

### A5 — pg_accel_device_info / Stats Documentation
**Status:** Not Started
**Owns:** docs section on monitoring

**Tasks:**
- [ ] Document `pg_accel_device_info()` with what each field means and example output on different platforms
- [ ] Document `pg_accel_stats()` with what each counter means and when to check them
- [ ] Document `pg_accel_reset_stats()`
- [ ] Write troubleshooting guide: "pg_accel is not accelerating my query" decision tree

**Agent gate:**
- [ ] Example outputs for macOS Metal, macOS CPU-only, Linux CPU-only
- [ ] Troubleshooting guide covers top 5 common issues
- [ ] All documented behavior matches actual behavior

**Implementation log:**
_(no deviations)_

### A6 — GUC Configuration Guide
**Status:** Not Started
**Owns:** docs section on configuration

**Tasks:**
- [ ] Document `pg_accel.workers`: when to change, examples for OLTP (keep low) vs analytics (raise)
- [ ] Document `pg_accel.max_workers_total`: how to size for your workload
- [ ] Document `pg_accel.min_batch_size`: when to adjust
- [ ] Document `pg_accel.gpu_enabled`: when to disable GPU
- [ ] Document `pg_accel.kernel_timeout_ms`: when to adjust
- [ ] Write worked example: Mac Mini with 4 connections doing analytics -> workers = 6
- [ ] Write worked example: Production server with 200 connections -> workers = 1, max_workers_total = 16
- [ ] Write worked example: Dev laptop -> leave defaults
- [ ] Explain interaction with PG's own parallel settings

**Agent gate:**
- [ ] Each GUC has clear guidance with numeric examples
- [ ] Examples cover 3+ common deployment scenarios
- [ ] Interaction with PG's own parallel settings explained

**Implementation log:**
_(no deviations)_

### A7 — pgvector Example Adapter
**Status:** Not Started
**Owns:** `examples/pgvector_adapter.rs`

**Tasks:**
- [ ] Implement type extractor for `vector` type (flat float32 array)
- [ ] Implement `l2_distance(vector, vector)` -> BatchedEval
- [ ] Implement `cosine_distance(vector, vector)` -> BatchedEval
- [ ] Implement `inner_product(vector, vector)` -> BatchedEval
- [ ] Write tests: 10K vectors, ON == OFF
- [ ] Ensure code serves as both a working example and proof that the adapter pattern works for third-party extensions

**Agent gate:**
- [ ] All 3 distance functions: results identical to vanilla pgvector
- [ ] Tests pass
- [ ] Code is clean enough to serve as documentation
- [ ] < 100 lines total (demonstrating adapter simplicity)

**Implementation log:**
_(no deviations)_

### A8 — CHANGELOG + SECURITY.md
**Status:** Not Started
**Owns:** `CHANGELOG.md`, `SECURITY.md`

**Tasks:**
- [ ] Write CHANGELOG: v0.1.0 initial release notes
- [ ] Write SECURITY.md: catalogue every `unsafe` block with justification
- [ ] Document shared memory access patterns in SECURITY.md
- [ ] Document thread safety model in SECURITY.md
- [ ] Document how to report security issues in SECURITY.md
- [ ] Document known limitations (rayon threads access PG function pointers) in SECURITY.md

**Agent gate:**
- [ ] Every unsafe block listed
- [ ] Shared memory access via LWLock documented
- [ ] Report process specified

**Implementation log:**
_(no deviations)_

### A9 — License + Repo Setup
**Status:** Not Started
**Owns:** `LICENSE`, `.github/ISSUE_TEMPLATE/`, `.github/DISCUSSION_TEMPLATE/`

**Tasks:**
- [ ] Pick appropriate OSS license (PostgreSQL license recommended for PG extensions) and create LICENSE file
- [ ] Create bug report issue template (requires pg_accel_device_info + pg_accel_stats output)
- [ ] Create feature request issue template
- [ ] Create new adapter proposal issue template
- [ ] Enable GitHub Discussions
- [ ] Create `.gitignore` for Rust + C++ + pgrx artifacts

**Agent gate:**
- [ ] License file present
- [ ] Issue templates render correctly on GitHub
- [ ] .gitignore covers: target/, build/, *.so, *.dylib, pgrx artifacts

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] brew install pg_accel works on clean macOS
- [ ] Docker image works, < 500MB
- [ ] README quickstart verified end-to-end
- [ ] ARCHITECTURE.md reviewed for clarity
- [ ] CONTRIBUTING.md: pgvector adapter buildable from guide alone
- [ ] GUC guide has worked examples for 3+ scenarios
- [ ] All documentation matches actual behavior
- [ ] SECURITY.md catalogues all unsafe blocks
- [ ] License + repo infrastructure complete

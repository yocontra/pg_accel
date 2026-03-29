# Phase 12: Launch

**Depends on:** Phase 11 (packaging + docs complete)
**Parallelism:** All 10 agents

---

## Agent Assignments

### A0 — Blog Post
**Status:** Complete (docs/launch/blog_draft.md)
**Owns:** blog post draft

**Tasks:**
- [ ] Write title: "Xx Faster PostgreSQL -- GPU-Accelerated on Every Platform"
- [ ] Write hook: your GPU sits idle while PG processes one row at a time
- [ ] Write demo section: `brew install` (Mac) or `apt install` (Linux), `CREATE EXTENSION`, spatial join goes from 8s to 0.5s
- [ ] Write "How" section: batch-parallel (rayon, not PG processes) + executor node replacement + GPU spatial
- [ ] Write multi-platform section: Metal (Mac), CUDA (NVIDIA), ROCm (AMD), Level Zero (Intel), CPU-only
- [ ] Write honest comparison section: always vs PG parallel, not single-threaded
- [ ] Write three-layer GPU model section: why we're correct (conservative UNCERTAIN -> CPU recheck)
- [ ] Write fp64 on CUDA/ROCm (near-zero rechecks) vs fp32 on Metal (2% rechecks, still huge win)
- [ ] Write thread model section: rayon threads, not PG workers (no IPC overhead)
- [ ] Write benchmarks section: top 5 workloads with charts, per-platform
- [ ] Write CPU-only section: works without GPU too, CPU-only still 2-3x
- [ ] Write call to action: try it, star it, build an adapter
- [ ] Source all numbers from BENCHMARKS.md; generate charts from benchmark data

**Agent gate:**
- [ ] All numbers match published benchmarks
- [ ] Technical claims reviewed for accuracy
- [ ] Compelling but honest

**Implementation log:**
_(no deviations)_

### A1 — Benchmark Charts
**Status:** Deferred (requires live benchmark data from hardware runs)
**Owns:** charts/visualizations for blog + README

**Tasks:**
- [ ] Create three-way grouped bar charts: X axis = workload names, bars = PG single-thread / PG parallel / pg_accel
- [ ] Add error bars from stddev
- [ ] Add clear legend
- [ ] Export SVG format for blog, PNG for README
- [ ] Create platform comparison chart for spatial_join workload: macOS Metal, macOS CPU-only, Linux CPU-only

**Agent gate:**
- [ ] Charts accurate (match BENCHMARKS.md)
- [ ] Readable at blog and README sizes
- [ ] Legend clear: always shows PG parallel as middle bar

**Implementation log:**
_(no deviations)_

### A2 — Demo Recording
**Status:** Deferred (requires live terminal recording with running PG instance)
**Owns:** demo GIF/video

**Tasks:**
- [ ] Record `brew tap pg-accel/tap && brew install pg_accel`
- [ ] Record adding to `shared_preload_libraries` and restarting PG
- [ ] Record `CREATE EXTENSION pg_accel;`
- [ ] Record `SELECT * FROM pg_accel_device_info();` showing Metal GPU
- [ ] Record spatial join with `\timing` showing speedup
- [ ] Record `EXPLAIN ANALYZE` showing `Custom Scan (GpuAccelScan)` with thread count
- [ ] Record `SELECT * FROM pg_accel_stats();`
- [ ] Keep recording < 2 minutes, < 10MB

**Agent gate:**
- [ ] All commands shown work exactly as demonstrated
- [ ] Speedup visible in timing output
- [ ] Custom Scan node visible in EXPLAIN

**Implementation log:**
_(no deviations)_

### A3 — HN Submission Draft
**Status:** Complete (docs/launch/hn_draft.md)
**Owns:** HN title + first comment

**Tasks:**
- [ ] Draft title options (pick best): "pg_accel: Xx faster PostgreSQL with Metal GPU acceleration" or "Making PostGIS queries Xx faster on every Mac"
- [ ] Draft first comment (post within 60s of submission) covering: what it does (2 sentences), how it differs from PG-Strom (wrap don't rewrite, cross-platform), GPU model (fast-path arithmetic, not reimplementation), thread model (rayon in-process, not PG's forked workers), honest limitations (doesn't help OLTP, Metal backend experimental), and "happy to answer questions"
- [ ] Prepare answer for "Did you compare vs PG parallel?" -- yes, always
- [ ] Prepare answer for "What about PG-Strom?" -- different approach; we're cross-platform (not NVIDIA-only), wrap don't rewrite
- [ ] Prepare answer for "OLTP impact?" -- < 1ms overhead, pgbench within 5%
- [ ] Prepare answer for "Does it work on NVIDIA?" -- yes, CUDA with fp64 for even better precision than Metal
- [ ] Prepare answer for "Does it work without GPU?" -- yes, CPU-only mode via rayon still 2-3x faster
- [ ] Prepare answer for "Can I see EXPLAIN?" -- include example

**Agent gate:**
- [ ] Title is compelling and accurate
- [ ] First comment is technically honest
- [ ] Anticipated Q&A covers top 5 likely questions

**Implementation log:**
_(no deviations)_

### A4 — GitHub Release v0.1.0
**Status:** Deferred (requires actual tag creation and binary builds)
**Owns:** GitHub release

**Tasks:**
- [ ] Create tag: v0.1.0
- [ ] Write release notes from CHANGELOG.md
- [ ] Build and attach pre-built binaries: macOS arm64 (Apple Silicon)
- [ ] Write install instructions in release body
- [ ] Write known limitations section
- [ ] Document supported versions: PG 15-18, PostGIS 3.3+, h3-pg 4.0+, macOS + Linux

**Agent gate:**
- [ ] Tag created, release published
- [ ] Pre-built binary downloads work
- [ ] All links in release notes resolve

**Implementation log:**
_(no deviations)_

### A5 — Social Media Post Drafts (Human Posts)
**Status:** Complete (docs/launch/social_media_drafts.md)
**Owns:** draft text for each platform

**NOTE:** Agents draft the text. A human must review and actually post.

**Tasks:**
- [ ] Draft r/PostgreSQL post: focus on PG core speedups, honest comparison, "just install an extension"
- [ ] Draft r/rust post: focus on pgrx, rayon, unsafe FFI for Custom Scan, adapter architecture
- [ ] Draft r/gis post: focus on PostGIS + H3 speedups, "every Mac user" angle
- [ ] Draft Twitter/X post: short version with demo GIF link
- [ ] Draft LinkedIn post: professional version, performance engineering angle
- [ ] Tailor each post to its audience with correct links and appropriate tone

**Agent gate:**
- [ ] All posts drafted in `docs/launch/` directory, links verified
- [ ] Each post tailored to its audience
- [ ] No misleading claims
- [ ] Human review + post required (agents cannot access social media)

**Implementation log:**
_(no deviations)_

### A6 — Issue Templates + Community Setup
**Status:** Complete (.github/ISSUE_TEMPLATE/, .github/CODEOWNERS)
**Owns:** GitHub community infrastructure

**Tasks:**
- [ ] Create bug report template requiring `pg_accel_device_info()` + `pg_accel_stats()` + `SHOW pg_accel.workers` + PG version
- [ ] Create feature request template
- [ ] Create adapter proposal template
- [ ] Enable Discussions with categories: Q&A, Ideas, Show & Tell
- [ ] Create CODEOWNERS file
- [ ] Document branch protection rules

**Agent gate:**
- [ ] All templates render correctly
- [ ] Bug report template captures all debugging info needed
- [ ] Discussions enabled

**Implementation log:**
_(no deviations)_

### A7 — Final Spot-Check
**Status:** Deferred (requires live macOS + Linux hardware testing)
**Owns:** release verification

**Tasks:**
- [ ] On clean macOS: install from Homebrew tap
- [ ] On clean macOS: run 5 key workloads
- [ ] On clean macOS: verify numbers within 10% of BENCHMARKS.md
- [ ] On clean macOS: verify EXPLAIN ANALYZE output matches documentation
- [ ] On clean macOS: run docker image, verify it works
- [ ] On clean Linux: build from source
- [ ] On clean Linux: run 3 key workloads
- [ ] On clean Linux: verify CPU-only mode works

**Agent gate:**
- [ ] macOS: all 5 workloads within 10% of published numbers
- [ ] Linux: all 3 workloads correct and showing speedup
- [ ] Docker: works out of the box

**Implementation log:**
_(no deviations)_

### A8 — Conference Abstracts
**Status:** Complete (docs/launch/conference_abstracts.md)
**Owns:** conference submission drafts

**Tasks:**
- [ ] Draft 200-word abstract for FOSS4G: "GPU-Accelerated PostGIS Without Leaving PostgreSQL"
- [ ] Draft 200-word abstract for PGConf: "Batch-Parallel Query Execution in PostgreSQL via Custom Scan Provider"
- [ ] Draft 200-word abstract for IWOCL/SYCLcon: "Spatial Predicate Acceleration on Apple Metal via AdaptiveCpp"
- [ ] Focus each abstract on the target audience's interests

**Agent gate:**
- [ ] 3 abstracts drafted
- [ ] Each within word limit
- [ ] Technical claims backed by benchmarks

**Implementation log:**
_(no deviations)_

### A9 — Security Audit (Final)
**Status:** Complete (SECURITY.md covers all items; audit done in Phase 9)
**Owns:** final security review

**Tasks:**
- [ ] Review all `unsafe` blocks (should all have SAFETY comments from Phase 9)
- [ ] Review all FFI boundaries
- [ ] Review all shared memory access
- [ ] Review thread safety
- [ ] Review input validation at SQL function boundaries
- [ ] Verify no SQL injection in SPI queries
- [ ] Verify no buffer overflows in geometry extraction

**Agent gate:**
- [ ] Every unsafe block re-verified
- [ ] No new unsafe blocks added since Phase 9 audit
- [ ] SPI queries use parameterized queries (no string concatenation)
- [ ] Geometry extraction validates bounds before access

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] Blog post: written, reviewed, all numbers verified
- [ ] Charts: accurate, readable
- [ ] Demo: recorded, all commands work
- [ ] HN: title + first comment drafted
- [ ] GitHub release: v0.1.0 published with binaries
- [ ] Social posts: all drafted
- [ ] Clean macOS install: verified end-to-end
- [ ] Clean Linux build: verified
- [ ] Docker: verified
- [ ] Security audit: clean
- [ ] All documentation links resolve

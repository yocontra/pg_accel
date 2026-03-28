# Phase 9: Production Hardening + AdaptiveCpp Upstream PRs

**Depends on:** Phase 8 (correctness proven)
**Parallelism:** All 10 agents. A0-A5 on hardening, A6-A9 on upstream PRs.

---

## Hardening (A0-A5)

### A0 — Signal Safety Hardening
**Status:** Not Started
**Owns:** signal handling + cancellation paths

**Tasks:**
- [ ] Test `pg_cancel_backend()` during spatial join: clean cancel, no segfault
- [ ] Test `pg_cancel_backend()` during aggregate: clean cancel
- [ ] Test `pg_cancel_backend()` during GPU kernel: kernel completes (or times out), clean cancel
- [ ] Test `statement_timeout` triggered during batch: clean cancel
- [ ] Test `idle_in_transaction_session_timeout`: clean cleanup
- [ ] Ensure `CHECK_FOR_INTERRUPTS()` is called between every batch on main thread
- [ ] Ensure that if cancel arrives during batch, batch completes first, then cancel is processed
- [ ] Run 100 consecutive cancels at random points and verify zero segfaults

**Agent gate:**
- [ ] 100 `pg_cancel_backend()` during various query types: zero segfaults
- [ ] `statement_timeout = '100ms'` with long spatial join: clean timeout
- [ ] Thread budget released after every cancel
- [ ] GPU resources freed after every cancel

**Implementation log:**
_(no deviations)_

### A1 — Process Exit Cleanup
**Status:** Not Started
**Owns:** `before_shmem_exit` + `on_shmem_exit` callbacks

**Tasks:**
- [ ] On normal disconnect: rayon pool dropped, thread budget released, GPU freed
- [ ] On `pg_terminate_backend()`: same cleanup via exit callback
- [ ] On `kill -9` on backend: shared memory thread budget counter must recover (other backends detect stale entry on next request)
- [ ] Implement per-backend slot in shared memory with PID + thread count
- [ ] On `request_threads()`, scan for dead PIDs (`kill(pid, 0)` returns ESRCH) and reclaim their budget

**Agent gate:**
- [ ] `pg_terminate_backend()` during query: clean exit, thread budget decremented
- [ ] `kill -9` on one backend: other backends recover budget within next request
- [ ] No zombie rayon threads after backend exit
- [ ] GPU queue released after backend exit

**Implementation log:**
_(no deviations)_

### A2 — GPU Timeout + Fallback
**Status:** Not Started
**Owns:** GPU timeout path

**Tasks:**
- [ ] Implement `pg_accel.kernel_timeout_ms` GUC (default 5000)
- [ ] If GPU kernel doesn't complete within timeout, fall back to CPU for entire batch
- [ ] Implement timeout detection via SYCL event wait with timeout
- [ ] Log warning on timeout: "pg_accel: GPU kernel timeout after Xms, falling back to CPU"
- [ ] Test with simulated slow kernel (artificial delay): verify fallback triggers correctly
- [ ] Handle GPU device lost (driver crash): graceful CPU fallback + logged warning
- [ ] Handle out of GPU memory: graceful CPU fallback + logged warning
- [ ] Handle SYCL exception: graceful CPU fallback + logged warning
- [ ] Support `kernel_timeout_ms = 0` to disable timeout (infinite wait, for debugging)

**Agent gate:**
- [ ] Artificial timeout: fallback within 2x timeout, correct results via CPU
- [ ] GPU OOM simulation: fallback to CPU, correct results
- [ ] After GPU error: subsequent queries still work (GPU or CPU)
- [ ] kernel_timeout_ms = 0: disables timeout (infinite wait, for debugging)

**Implementation log:**
_(no deviations)_

### A3 — unsafe Audit
**Status:** Not Started
**Owns:** all `unsafe` blocks across entire codebase

**Tasks:**
- [ ] Ensure every `unsafe` block has a `// SAFETY:` comment explaining what invariant is being relied upon
- [ ] Ensure every `// SAFETY:` comment explains why it's safe in this specific context
- [ ] Ensure every `// SAFETY:` comment explains what could go wrong if the invariant is violated
- [ ] Eliminate all `unwrap()` outside test code (use `unwrap_or`, `?`, or explicit error handling)
- [ ] Run `cargo clippy -- -D warnings` and fix until clean
- [ ] Verify no raw pointer dereference without null check
- [ ] Verify all FFI boundaries have inputs validated before crossing

**Agent gate:**
- [ ] `grep -r "unsafe" src/ | wc -l` -- each has corresponding SAFETY comment
- [ ] `grep -r "unwrap()" src/ | grep -v test | grep -v "unwrap_or"` -- 0 hits
- [ ] `cargo clippy -- -D warnings` -- 0 warnings
- [ ] Manual review: every unsafe block is justified

**Implementation log:**
_(no deviations)_

### A4 — CI Hardening (Expand Phase 0 Pipeline)
**Status:** Not Started
**Owns:** `.github/workflows/ci.yml` (expand existing from Phase 0)

**Tasks:**
- [ ] Expand OS matrix: macOS-14 (Apple Silicon), ubuntu-latest, ubuntu-gpu (NVIDIA runner if available)
- [ ] Expand PG matrix: 15, 16, 17, 18 (full matrix, not just 17)
- [ ] Expand GPU features matrix: none, metal (macOS only), cuda (Linux NVIDIA only)
- [ ] Add PostGIS matrix dimension: with and without
- [ ] Add CI job: `cargo pgrx test pgXX` for EACH PG version (15, 16, 17, 18)
- [ ] Add CI job: short benchmark (3 workloads, 1 iteration) as regression check, not perf claim
- [ ] Add CI job: correctness subset (fuzz 10K instead of 100K for faster CI)
- [ ] Add CI job: GPU kernel standalone tests (on GPU runners)
- [ ] Add CI job: Docker integration tests with PostGIS `make check` subset
- [ ] Configure caching: Rust target dir, pgrx PG installs, AdaptiveCpp build, Docker layers

**Agent gate:**
- [ ] All matrix entries pass (green)
- [ ] CI completes in < 30 minutes total
- [ ] Matrix covers: macOS + Linux x PG versions x GPU variants
- [ ] GPU tests run on at least Metal (macOS) and optionally CUDA (Linux)
- [ ] Failure on any matrix entry blocks merge

**Implementation log:**
_(no deviations)_

### A5 — 24-Hour Stress Test
**Status:** Not Started
**Owns:** stress test runner

**Tasks:**
- [ ] Set up 32 concurrent connections for 24-hour run
- [ ] Configure 8 connections running spatial joins (varying sizes)
- [ ] Configure 8 connections running analytical aggregates
- [ ] Configure 8 connections running sort + limit queries
- [ ] Configure 4 connections running DDL (CREATE/DROP INDEX, VACUUM)
- [ ] Configure 4 connections running short OLTP queries (should use vanilla path)
- [ ] Monitor thread budget counter continuously (never corrupted)
- [ ] Monitor RSS per backend continuously (stable)
- [ ] Monitor GPU memory continuously (stable)
- [ ] Monitor error log continuously (no unexpected errors)
- [ ] Sample 1% of results and verify correctness

**Agent gate:**
- [ ] 24 hours: zero crashes
- [ ] Zero wrong results in sampled checks
- [ ] Zero deadlocks
- [ ] Thread budget: never corrupted (counter = 0 when all backends idle)
- [ ] RSS: within 2x initial for any backend
- [ ] Error log: no PANIC, no FATAL except intentional terminations

**Implementation log:**
_(no deviations)_

---

## AdaptiveCpp Upstream PRs (A6-A9)

### A6 — Bitonic Sort Kernel PR
**Status:** Not Started
**Owns:** AdaptiveCpp PR: parallel sort for Metal

**Tasks:**
- [ ] Implement bitonic sort as a SYCL kernel that works on Metal
- [ ] Support fp32, i32, i64 key types
- [ ] Implement key-value variant (sort keys, permute values)
- [ ] Test thoroughly against std::sort reference
- [ ] Benchmark on Metal and include timing comparison in PR description
- [ ] Write documentation: usage example, performance characteristics
- [ ] Test edge cases: empty, single, already sorted, reverse sorted, all equal
- [ ] Submit as PR to AdaptiveCpp with proper description explaining use case (pg_accel)
- [ ] Ensure code follows AdaptiveCpp style/conventions

**Agent gate:**
- [ ] PR submitted with tests, benchmarks, documentation
- [ ] Tests pass on Metal (Apple Silicon)
- [ ] Tests pass on CPU fallback
- [ ] Matches std::sort results for all test cases
- [ ] PR description explains motivation (pg_accel use case)
- [ ] Code follows AdaptiveCpp style/conventions

**Implementation log:**
_(no deviations)_

### A7 — llvm.minnum/maxnum Intrinsic PR
**Status:** Not Started
**Owns:** AdaptiveCpp PR: fix minnum/maxnum for Metal MSL backend

**Tasks:**
- [ ] Fix the Metal MSL emitter to handle `llvm.minnum`/`llvm.maxnum` intrinsics (currently breaks `-ffast-math` compilation, reported in PR #1961 comments)
- [ ] Expand these intrinsics to MSL's `min()`/`max()` builtins during the LLVM IR -> MSL translation pass
- [ ] Add test case that previously failed without the fix
- [ ] Verify `-ffast-math` compilation works after fix
- [ ] Verify no regression on existing tests
- [ ] Write clear commit message referencing the original bug report

**Agent gate:**
- [ ] PR submitted with test case that previously failed
- [ ] `-ffast-math` compilation works after fix
- [ ] No regression on existing tests
- [ ] Clear commit message referencing the original bug report

**Implementation log:**
_(no deviations)_

### A8 — atomic64 Support PR
**Status:** Not Started
**Owns:** AdaptiveCpp PR: 64-bit atomic operations for Metal

**Tasks:**
- [ ] Implement `atomic_fetch_add` for 64-bit integers on Metal (Apple Silicon A14+ / M1+)
- [ ] Implement `atomic_fetch_sub` for 64-bit integers on Metal
- [ ] Implement `atomic_load` for 64-bit integers on Metal
- [ ] Implement `atomic_store` for 64-bit integers on Metal
- [ ] Add tests for all 64-bit atomic operations
- [ ] Document hardware requirements (A14+ / M1+)
- [ ] Verify no regression on existing tests
- [ ] Note in PR that this enables 64-bit reduction counters on GPU (used for accurate counting in reduce kernel)

**Agent gate:**
- [ ] PR submitted with tests for all 64-bit atomic operations
- [ ] Tests pass on Apple Silicon (M1+)
- [ ] Clear documentation of hardware requirements (A14+ / M1+)
- [ ] No regression on existing tests

**Implementation log:**
_(no deviations)_

### A9 — AdaptiveCpp Metal Test Suite Improvements
**Status:** Not Started
**Owns:** AdaptiveCpp PR: expanded Metal test coverage

**Tasks:**
- [ ] Contribute test cases for edge cases discovered building pg_accel
- [ ] Add regression tests for any bugs found and worked around during Phases 4+6
- [ ] Add performance benchmarks for Metal vs CPU fallback
- [ ] Write short writeup documenting experience building on the Metal backend (GitHub Discussion or doc PR)
- [ ] Submit at least 1 PR with new test cases

**Agent gate:**
- [ ] At least 1 PR submitted with new test cases
- [ ] Tests cover scenarios encountered during Phases 4+6
- [ ] Writeup of Metal backend experience shared with AdaptiveCpp maintainers

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] 100 cancel tests: zero segfaults
- [ ] Process exit cleanup: thread budget always recovered
- [ ] GPU timeout: fallback works within 2x timeout
- [ ] Every unsafe block has SAFETY comment
- [ ] cargo clippy -- -D warnings: 0 warnings
- [ ] CI matrix: all entries green, < 30 minutes
- [ ] 24-hour stress test: zero crashes, zero wrong results
- [ ] At least 1 AdaptiveCpp PR submitted (bitonic sort kernel -- P0 blocker)
- [ ] Additional upstream PRs (minnum/maxnum, atomic64) prepared if time permits
- [ ] All submitted PRs have tests and documentation
- [ ] Docker integration: all cumulative tests pass after hardening changes
- [ ] Docker integration: cancel/timeout tests run against real PG in Docker

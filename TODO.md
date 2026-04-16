# pg_accel Phase 3 — Remaining Crashes & Issues

## Benchmark Status (v3 partial run — 2026-04-14)

Commit `2db6ac9` fixes: window table_endscan + projection, bytecode eval disabled, stale refs purged.

**Previous run (v2):** 110 crashes across all categories
**Current run (v3, partial):** ~30 crashes observed before disk full halted the run

The run completed through: reduce, agg, hash_agg, sort, hash_join, spatial, h3.
**NOT yet run:** expr_*, window_*, ssbm_*, raster_*, gpu_expr_*.

### Crash-free categories (0 crashes, all scales 10K-10M)
- `gpu_reduce_sum`, `gpu_reduce_scaling` — all scales
- `reduce_sum_f32`, `reduce_sum_f64`, `reduce_sum_i64` — all scales
- `reduce_min_f64`, `reduce_max_f64`, `reduce_multi` — all scales
- `grouped_agg`, `grouped_agg_high_card` — all scales
- `gpu_hashagg_med_card` — all scales
- `hash_join`, `hashjoin_100_1m`, `hashjoin_1k_1m` — all scales
- `gpu_hashjoin_filter` — all scales
- `sort_int4`, `sort_int8`, `sort_float4`, `sort_float8` — all scales
- `gpu_sort_topk_wide` — all scales
- All spatial workloads (spatial_filter, spatial_concentric, spatial_complex_poly [10K-1M], spatial_multihole, spatial_mega_1kv, spatial_star_1kv, spatial_zigzag, spatial_selectivity, vsweep_*, spatial_sel_*) — all scales
- `h3_bulk` — 10K and 100K (7.8x speedup at 100K!)
- `h3_cell_to_parent` — 10K-1M

---

## Remaining Crashes

### Category 1: Sort/Join at 10M scale only (3 crashes)

| Workload | Scale | Type |
|----------|-------|------|
| `large_sort` | 10M | db error |
| `gpu_sort_multikey` | 10M | db error |
| `gpu_hashjoin_large_build` | 10M | db error |

**Root cause:** GPU runtime buffer allocation fails at 10M rows. The SYCL sort kernel (bitonic) and hash join hash table exceed available GPU buffer limits.

**Fix plan:**
1. In `pg_accel/src/engine/cost.rs` — `DeviceLimits::gpu_sort_max_elements` and `gpu_join_max_output_rows`: lower from current values to ~5M
2. In `pg_accel/src/engine/executor/sort.rs` — the existing `SORT_MAX_ELEMENTS` gate should catch this; verify the gate value matches `DeviceLimits`
3. In `pg_accel/src/engine/ffi/planner_hooks.rs` — hash join injection: add row estimate gate at 5M
4. These are scale-limit issues, not logic bugs. PG handles these natively when our path isn't injected.

**Priority:** Low — only affects 10M scale, PG recovers gracefully

### Category 2: Spatial at 10M (1 crash)

| Workload | Scale | Type |
|----------|-------|------|
| `spatial_complex_poly` | 10M | db error |

**Root cause:** Complex polygon with many vertices at 10M rows exceeds GPU buffer or timeout.

**Fix plan:** Same as Category 1 — scale limit. Lower `gpu_spatial_max_rows` or increase `kernel_timeout_ms` for spatial ops.

**Priority:** Low — 10K-1M all pass

### Category 3: H3 bulk at large scale (2 crashes)

| Workload | Scale | Type |
|----------|-------|------|
| `h3_bulk` | 1M | db error (after 2 warmups) |
| `h3_bulk` | 10M | db error (at setup) |

**Root cause:** Resource accumulation — h3_bulk at 100K runs all 10 iterations perfectly (7.8x speedup). At 1M, crashes after 2 warmups. SYCL USM buffer pool or h3 kernel has a leak at scale.

**Fix plan:**
1. Check `pgaccel-kernels/src/h3_ops.cpp` for USM buffer leaks in `pgaccel_h3_lat_lng_to_cell_bulk`
2. Check `pgaccel-kernels/src/mem_pool.cpp` — ensure SYCL USM allocations are freed after each dispatch
3. Add explicit `mem_pool_reset()` call after each H3 bulk dispatch
4. May also need scale gate in cost model for H3 at >500K rows

**Priority:** Medium — 100K works with excellent speedup, but 1M+ fails

### Category 4: H3 variant workloads (24 crashes — all scales)

| Workload | Scales | Type |
|----------|--------|------|
| `h3_resolution_sweep` | all 4 | db error at setup |
| `h3_latlng_res15` | all 4 | db error at setup |
| `h3_dist_near` | all 4 | db error at setup |
| `h3_dist_far` | all 4 | db error at setup |
| `h3_parent_deep` | all 4 | db error at setup |
| `h3_grid_distance` | 1M, 10M | db error at setup |

**Root cause:** These crash during setup SQL, NOT during the benchmark query. The setup uses `h3_lat_lng_to_cell` (the h3-pg alias) and `h3index` type. Two likely issues:
1. The `h3index` column type in ALTER TABLE may fail if h3-pg extension isn't fully loaded
2. The `public.h3_lat_lng_to_cell` call in UPDATE may fail if pg_accel intercepts it despite the alias (adapter lists `h3_latlng_to_cell`, not the underscored variant, but planner hooks may still match)
3. Alternatively: the setup SQL creates data using h3 functions, and if the h3 extension's C functions crash on certain inputs (e.g. resolution 15 on edge cases), that's an h3-pg bug not a pg_accel bug

**Fix plan:**
1. Run the setup SQL manually with `pg_accel.enabled = off` to verify h3-pg works standalone
2. If setup works with pg_accel off: the planner hook is incorrectly intercepting h3-pg alias functions → add exclusion in `function_matcher.rs` for `h3_lat_lng_to_cell`
3. If setup fails with pg_accel off: this is an h3-pg compatibility issue → skip these workloads or fix the test data generator
4. Check PG logs for the actual error message: `tail -50 ~/.pgrx/data-17/pg.log` (or Homebrew PG log)

**Priority:** Medium — need to diagnose whether this is pg_accel or h3-pg

### Category 5: Expr workloads (NOT YET RUN — expected ~0 crashes)

The bytecode evaluator was disabled in commit `2db6ac9`. Complex expressions now defer to PG's native qual evaluation. Expected result: all 11 expr_* workloads should run without crashes (they'll show ~1.0x speedup since PG handles them natively).

**Verification needed:** Run `--category gpu_expr` to confirm

### Category 6: Window workloads (NOT YET RUN — expected improvement)

Fixed in commit `2db6ac9`:
- `table_endscan()` added for Window strategy (resource leak fix)
- Projection via `ps_ProjInfo` extended to Window strategy

The planner gate already rejects ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD (benchmarked as GPU-losing). Only SUM/COUNT window functions are injected.

**Expected result:** window_running_sum should work. Other window workloads may not inject GPU path at all (gated out).

**Verification needed:** Run `--category window` to confirm

### Category 7: SSBM workloads (NOT YET RUN — expected improvement)

SSBM Q1 queries have 3+ predicates in WHERE clause. Previously hit the broken bytecode evaluator. Now deferred to PG.

**Expected result:** If the crash was purely from bytecode eval → 0 crashes (PG handles natively). If the crash involves hash join tlist mismatch → still crashes.

**Verification needed:** Run `--workload ssbm_q1_1` to confirm

---

## Non-Crash Issues

### Disk space exhaustion
`/dev/disk3s5` at 100% (926GB used, <200MB free). Benchmark suite generates large output files and cargo build artifacts fill the disk.

**Fix:** Before benchmark run:
```bash
cargo clean                          # ~5-10 GB
rm -rf /private/tmp/claude-501       # Claude temp files
rm -f /tmp/bench_*.md                # Old results
```
Build bench binary in release mode only, then clean intermediate artifacts.

### Backend consolidation (resolved)
Native Metal backend and AdaptiveCpp/SYCL coexisted briefly; SYCL is now the sole
GPU path (targets CUDA / ROCm / Level Zero / Metal / CPU from one source tree).
All `metal_backend.mm`, `.metal` shaders, and `MTLBinaryArchive` tooling removed.

---

## Final Benchmark Run Plan

### Prerequisites
1. Free ≥5GB disk space (see commands above)
2. Rebuild: `cargo build --package pg_accel_bench --release`
3. Restart PG: `brew services restart postgresql@17`

### Run order (targeted, not full suite)
```bash
# 1. Verify expr fixes (previously 11 crashes)
./target/release/pg_accel_bench run \
  --workload expr_2pred \
  --connection "host=localhost port=5432 dbname=postgres" \
  --skip-guc-verify

# 2. Verify window fixes (previously 6-9 crashes)
./target/release/pg_accel_bench run \
  --workload window_running_sum \
  --connection "host=localhost port=5432 dbname=postgres" \
  --skip-guc-verify

# 3. Verify SSBM fixes (previously 3 crashes + 1 SIGSEGV)
./target/release/pg_accel_bench run \
  --workload ssbm_q1_1 \
  --connection "host=localhost port=5432 dbname=postgres" \
  --skip-guc-verify

# 4. If all pass, full suite
./target/release/pg_accel_bench run \
  --connection "host=localhost port=5432 dbname=postgres" \
  --skip-guc-verify > /tmp/bench_v3_final.md 2>&1
```

### Success criteria
- 0 crashes at 10K-1M scales
- ≤5 crashes at 10M (scale limits only, not logic bugs)
- No SIGSEGV / server crashes
- H3 bulk at 100K maintains 7x+ speedup
- Spatial workloads at parity or better
- Reduce/sort/agg workloads at parity or better

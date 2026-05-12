# TODO

This is the pre-1.0 punchlist for pg_accel — a PostgreSQL 17 extension that offloads
spatial, h3, reduce, sort, hashagg, and raster workloads to Metal GPU on Apple Silicon
(CUDA / ROCm / Level Zero elsewhere) via AdaptiveCpp. AdaptiveCpp fork pin:
`yocontra/AdaptiveCpp` branch `fork-safe-metal` @ `4f3cde11a302eebac28aa1ccc79ad3399cb8183c`.
"Done" / ship-ready means: every planner-injected GPU path is bit-correct, benchmarks
never fall below PG-parallel parity on any (workload, size) cell, zero crashes on the
verification matrix, clean `just ci`, and documentation matches reality. File-line
citations below are against `pg_accel/src/...` unless noted; grep the symbol if
numbers drift. CI enforces citation freshness via `just doc-parity`.

When an item ships, drop it from this file — `git log` and the
CHANGELOG `[Unreleased]` section carry the audit trail. Don't leave
"Resolved" entries behind.

## Phases

The shape of the punchlist: Phase 1 finishes fp64 (the AdaptiveCpp i128
mul lowering bug landed at SHA `4f3cde11`; ULP budget tightened;
calibration blocked on Phase 6 dispatch perf, not on more multiplier
tuning). **Phase 2 closed** — the SYCL kernel rewrites
(`expr_templates.cpp` `5f1c3db`, `hash_agg.cpp` `8938269`,
`expr_eval.cpp` `923739f`) and bytecode dispatch re-enable (`f37c67b`)
all landed; one open follow-up (`hash_agg`'s 2-level pointer kernel
hits a Metal SSCP edge case on small batches) is tracked in Phase 6.
Phase 3 extends parallel-path coverage (HashAgg, preagg, window).
Phase 4 widens operator/type coverage. Phase 5 tunes the planner
(GPU-bridge dead-code cleanup `b231ce0` landed). Phase 6 chases perf
ceilings + the small-N hash_agg kernel bug. Phase 7 is fork-local
AdaptiveCpp maintenance burden. Phase 8 polishes the build. Phases
9-10 gate the 1.0 tag. The "Post-1.0 (deferred)" section catches
items explicitly descoped from ship.

## Next Up

### Round 3 (2026-05-03 follow-ups) — closed

3 final agents merged: B5a planner-side PreAgg refactor (commit `7c6d355`,
GUC-gated), K kernel-bug fix (commit `0319d93`, ported 2 SSCP-failing H3
kernels to host), SRF executor wiring (commit `a57cadb`, full custom-scan
plumbing for SRF-in-target-list). `audit-cpu-cheats: PASS`.

### Round 4 (2026-05-04/05) — landed on `main`

7 commits past `a57cadb`. P0 punchlist now empty. Highlights:
- ✅ SRF passthrough wrong-result (`4c2687b`) — DISTINCT/ORDER/FILTER reject in agg planner
- ✅ h3 multi-cell SRF Record-arm slot fix (`b09c22f`) — virtual slot + `construct_array`
- ✅ `make clear-jit` canonicalised (`0449662`)
- ✅ ACPP A1+A2 emitter fixes upstream (`c2092425`, `160728fd`); pg_accel revert blocked on SLEEF JIT compile-time (see P2)
- ✅ hash_agg small-N + sort_kv cold-cache (`5076e08`) — closed by upstream A1+A2
- ✅ h3 lat_lng_to_cell argbuffer-flatten (`b17c588`) — captured params 9→4, dodges Metal SSCP encoder reflection bug
- ✅ pgrx pg_test schema-rendering bug (`a3f27e1`) — `cargo pgrx test pg17 preagg` 47/48→48/48

### Round 5 (2026-05-05) — landed on worktree branches, **NOT YET MERGED to main**

Six parallel agents shipped P1/P2 work in disjoint worktrees. Each branch
is independently mergeable; recommended order = chronological (B5b first
since it touches the broadest scope, then 3B, then P6D; ACPP cherry-picks
last since they're upstream).

| Branch | Status | Net effect |
|---|---|---|
| `agent-b5b-preagg-exec` | ✅ ready | PreAgg slot-based scan refactor; `preagg_parallel_safe` GUC default flipped on (commits `b4940e4`, `058baf7`); 47/48 → 48/48 preagg tests |
| `worktree-agent-a5b484b0e375d86d0` (Phase 3B) | ✅ ready | HashAgg per-group partial-state kernel + bridge + executor (commits `be4d2c2`, `46726d8`, `13797a0`, `a0296d2`); 64/64 partial-correctness tests, 10/10 hash_agg_keys cold-cache. Unblocks Phase 3A (planner-side). |
| `worktree-agent-a77f649bdfa546315` (Phase 6 perf) | ✅ ready | Probe-cost amortisation: 4 hard-coded literals (`0.01`/`0.005`) moved into `DeviceLimits`, calibrated from measured GPU throughput (commit `cf88435`). EXPLAIN evidence: AVG+STDDEV and plain JOIN now pick `Custom Scan (GpuAccelAgg/Join)` instead of PG's stock plans. |
| `~/local/src/AdaptiveCpp-acpp45/agent-acpp45` | ✅ ready | MetalEmitter polish bundle (4 commits): forward-decl volume reduction, ReplaceIntrinsics fixpoint, soft-fp64 classifier centralisation, `__args` argument-buffer `constant` qualifier (ACPP4) |
| `~/local/src/AdaptiveCpp-acpp67/agent-acpp67` | ✅ partial ready | cmake JSON list-separator fix (verified end-to-end), fp64 fork-safety stress test, `ACPP_METAL_DUMP_IR` debug logging. 4 sub-items deferred (forwarder coverage matrix, MPFR ULP, `-Wall` triage, cross-backend parity — needs CUDA/ROCm hardware not available locally). |
| `~/local/src/AdaptiveCpp-sleef/agent-sleef` | ⚠️ partial; **blocked** | Pre-O3 reachability prune for soft-fp64 (commit `b6a4c2ee`) reduces bitcode 5.4 MB → 605 KB AND clears the cold-cache `test_h3` xcrun-metal hang documented in P2 below. **But**: outlining SLEEF helpers exposes a multi-AS pointer-param emitter limitation (228/336 tests fail with `cannot pass pointer to default address space as a pointer to address space 'device'`). Flag `SF64_DISABLE_SLEEF_INLINE` is OFF by default so behaviour is preserved. Next-step in P2. |

### Blocking relationships at-a-glance

```
   AdaptiveCpp upstream    ┌─ ACPP4 (`__args` constant qualifier) ✅ on `agent-acpp45`
   `fork-safe-metal`       ├─ ACPP5-2/4/5 (emitter polish) ✅ on `agent-acpp45`
                           ├─ A1+A2 (i33 / PHI literal) ✅ on `fork-safe-metal` HEAD
                           └─ SLEEF surface reduction ⚠️ partial on `agent-sleef`
                                                          (multi-AS pointer specialization
                                                           in MetalEmitter is the next 200-400 LOC
                                                           of upstream work; flag-OFF preserves status quo)

   pg_accel main           ┌─ B5b PreAgg exec refactor ✅ merged locally from `agent-b5b-preagg-exec`
                           ├─ Phase 3B HashAgg parallel ✅ merged locally from `worktree-agent-a5b484b0e375d86d0`
                           ├─ Phase 6 probe-cost ✅ on `worktree-agent-a77f649bdfa546315` — ready
                           └─ Phase 3A planner-side — UNBLOCKED by 3B; ~50 LOC follow-up

   Geometry-array extractor ──→ unlocks st_value(rast, geometry[]) end-to-end smoke
   for st_value             (PostGIS doesn't expose this SQL surface today; lower priority)

   Phase 6 dispatch perf  ──→ unblocks Phase 1 cost-multiplier calibration
                              unblocks AVG+STDDEV / plain-JOIN audit rows

   Phase 3a grouped HashAgg parallel    ──→ unblocks parallel COUNT(DISTINCT) etc.
   (planner side; 3B done via              (3B: kernel + bridge + executor — `be4d2c2`,
    be4d2c2/46726d8/13797a0)                 `46726d8`, `13797a0`. 3A: planner-side
                                             groupClause threading + partial_emitters
                                             wiring. ~200-400 LOC remaining for 3A.)
```

### Open items by priority (P0 = correctness blocker, P1 = perf blocker, P2 = nice-to-have)

**P0 — correctness blockers (silent wrong-results or crashes possible)**:
- _(none open — see Closed in this round below for the
  `pgaccel_h3_lat_lng_to_cell_bulk` argbuffer-reflection fix.)_
- _(small-N hash_agg silent zero verified closed by upstream fixes — cold-cache
  `test_fork` shows `hashagg_f64 OK` (total=2048) and `h3_f64 OK` end-to-end;
  `test_hash_agg_keys` = 10/10 cold-cache.)_
- _(`sort_kv_i32`/`sort_kv_i64` cold-cache fork dispatch verified working via
  standalone harness; in-suite `test_fork` now reaches all six fp64 matrix
  entries since the h3 fix unblocked the chain.)_

**P1 — perf blockers (correctness fine, GPU acceleration off in some cases)**:
- _(B5b PreAgg exec-side refactor — merged locally from
  `agent-b5b-preagg-exec`. See Round 5 table above.)_
- _(Phase 3B HashAgg parallel kernel/bridge/executor — merged locally from
  `worktree-agent-a5b484b0e375d86d0`; planner-side 3A remains.)_
- _(Phase 6 dispatch perf / probe-cost amortisation — done on
  `worktree-agent-a77f649bdfa546315`, ready to merge.)_
- **Phase 3A grouped HashAgg planner-side** — UNBLOCKED by 3B kernel/bridge.
  ~50 LOC: (i) drop `groupClause` bail at `partial_agg.rs:84`, (ii) extend
  PAAG with `group_keys: Vec<(attno, typoid)>`, (iii) thread groupClause into
  `plan_custom_path_agg` in `custom_scan/mod.rs`, (iv) pass `groupClause` +
  `numGroups` to `create_agg_path`. 3B's `partial_emitters` field stays
  dormant until 3A wires emitters on for grouped paths.

**P2 — open follow-ups (genuinely lower priority)**:
- **SLEEF JIT compile-time + multi-AS pointer specialization** —
  upstream work on `~/local/src/AdaptiveCpp-sleef/agent-sleef` (commit
  `b6a4c2ee`) addressed item (i) from the prior Round 4 entry: pre-O3
  reachability prune drops unreachable soft-fp64 functions and SLEEF
  coefficient globals, shrinking bitcode 5.4 MB → 605 KB and eliminating
  the cold-cache `test_h3` xcrun-metal hang. **However**, outlining the
  SLEEF helpers (which is what removes the hang on the kernel side too —
  the only step that would unblock K's host-port revert) exposes a
  per-call-site address-space specialization gap in `MetalEmitter`:
  helpers like `poly_array(double, const double*, i32, sf64_internal_fe_acc&)`
  take TWO pointer params with different runtime ASes (`constant` +
  `thread`) but emit as `ptr addrspace(0)` for both. With the
  `SF64_DISABLE_SLEEF_INLINE` flag ON, 228/336 tests fail with
  `cannot pass pointer to default address space as a pointer to address
  space 'device'`. Flag is OFF by default so default behaviour is
  preserved. Next-step: implement per-AS function specialization in
  `MetalEmitter` (clone helpers per AS combination, ~200-400 LOC in
  `Emitter.cpp` + `LLVMToMetal.cpp`). Until then, K's host-port at
  `pgaccel-kernels/src/h3_ops.cpp` (commit `51b5353`/`0319d93`) stays.
  ACPP A1+A2 fixes themselves (`c2092425` + `160728fd`) remain on
  `fork-safe-metal` HEAD, verified via standalone repros.
- **Geometry-array extractor for `st_value`** — `pgaccel_raster_value`
  kernel works; `extract_geometry_array` walker for PG `geometry[]` arg
  is missing. PostGIS doesn't expose `ST_Value(rast, ARRAY[...])` at the
  SQL surface today, so impact is low.
- **Phase 1 cost-multiplier calibration** — UNBLOCKED by Phase 6 probe-cost
  work in `worktree-agent-a77f649bdfa546315`. Once Phase 6 lands, the
  `fp64_matrix` 2-cell shortfall (which was dispatch overhead, not
  multiplier tuning) should resolve naturally; spot-check the audit and
  close.
- **Phase 4 type coverage — JSON / JSONB** — substantial; needs a JSONB
  binary-format parser kernel.
- **Phase 5 worker-side spatial recheck** — DSM plumbing.
- **AdaptiveCpp polish — partial work landed on worktree branches**:
  - `agent-acpp45` (4 commits, ready to cherry-pick): forward-decl volume
    reduction, ReplaceIntrinsics fixpoint, soft-fp64 classifier
    centralisation, `__args` argument-buffer `constant` qualifier (ACPP4).
    Dropped: ACPP5-1 (uint4 shift fast paths — already implemented),
    ACPP5-3 (`optnone` removal — needs custom InstCombine recognizer-
    suppression, infeasible in one session).
  - `agent-acpp67` (3 commits): cmake JSON list-separator fix
    (verified end-to-end), fp64 fork-safety stress test (auto-skips
    without install), `ACPP_METAL_DUMP_IR` debug logging.
  - **Deferred from `agent-acpp67`**: forwarder coverage matrix
    (ACPP6.2), MPFR ULP cross-check (ACPP6.3 — depends on 6.2),
    `-Wall`/`-Wextra` triage (ACPP7.4), fenv flag readback API +
    `ACPP_METAL_FP64_EXPORT` ABI audit (ACPP7.5), `ACPP_METAL_KEEP_SOURCE`
    decommission + `-fno-fast-math` semantics + 31-buffer scale test
    (ACPP7.6 mostly), cross-backend parity verification (ACPP7.7 —
    needs CUDA/ROCm/L0 hardware not available locally; per anti-cheat
    ban #1 not testable here).

### Closed in this round (no longer in Open list)

- ✅ `test_fork` cold-cache crash on `pgaccel_h3_lat_lng_to_cell_bulk` (P0) —
  fixed in commit `b17c588`. Both kernels (fp64 res>=12 and fp32 res<12)
  collapsed from 6 typed device pointers to a single `uint8_t*` shared-
  memory slab + `{count, res, deg2rad}` scalars. Capture count drops from
  ~9 to 4, so AdaptiveCpp Metal SSCP no longer packs them into the
  `Input_0` argbuffer struct that Metal's runtime reflection refused to
  index past slot 0 of in forked children. Cold-cache `test_fork` now
  PASSES twice in a row with `[child] fp64 matrix: h3_f64 (lat_lng_to_cell) OK`
  followed by `hashagg_f64 OK` and `bbox_f64 OK`. MSL kernel signature
  is now `(constant ulong& t0, device void* t1, constant uint& t2,
  constant ulong& t3)` — same shape as the always-working spatial_f64
  / sort_kv_f64 kernels. No CPU fallback.
- ✅ small-N `hash_agg` silent zero (P0) — AdaptiveCpp upstream fixes
  `c2092425` + `160728fd` lift the SSCP edge case that was causing
  `agg_hash`'s `run_unsorted_accum_kernel` (`pgaccel-kernels/src/hash_agg.cpp:371-439`)
  to return 0 at N=64. Cold-cache `test_fork` reports
  `[child] fp64 matrix: hashagg_f64 OK` (total=2048, 4 groups). No
  pg_accel kernel changes were required — the bug was in
  AdaptiveCpp's MetalEmitter, not the SYCL kernel.
- ✅ `sort_kv_i32`/`i64` cold-cache fork dispatch (P1) — verified via
  ad-hoc fork harness (parent loads .dylib, child calls `pgaccel_init()`,
  cold-dispatches `pgaccel_sort_kv_i32` then `_i64`). Both succeed,
  `.metalar` archives are produced for the radix-sort u32 / u64 kernels.
  Same upstream fixes as above; warmup-at-init is no longer needed.
- ✅ fp64 sphere_distance + st_length fork-safety — AdaptiveCpp runtime
  acknowledges helper exit 9 (skip-too-large), kernel gates dropped,
  test_spatial 109/109 cold-cache (`5e84560`)
- ✅ Registry-init ordering for pg_test — `resolve_oids_again` API +
  auto-retry on lookup miss (`0289679`)
- ✅ FunctionScan executor crash (TTS_FLAG_FIXED + pfree heap corruption) —
  `custom_scan_tlist` built upstream so `ExecSetSlotDescriptor` is never
  called (`bedda75`)
- ✅ ArrayType walker (1-D `bigint[]` / `geometry[]`) — generic 788-LOC
  walker handles fixed-width + varlena elements (`6215160`)
- ✅ h3_cell_to_boundary + h3_cells_to_multi_polygon shape — both ops now
  emit PG built-in polygon (was: GSERIALIZED); npoints() returns 6 for
  hexagon as hard verification (`c53ae15`)
- ✅ B5a PreAgg planner-side fact-path attach (GUC-gated) — `7c6d355`
- ✅ K kernel bugs (h3_grid_disk single-cell None + h3_cell_to_boundary_emit
  multi-cell SIGABRT) — root-caused to AdaptiveCpp emitter bugs, host-ported
  affected kernels (`0319d93`)
- ✅ SRF executor wiring — full custom-scan plumbing, EXPLAIN shows
  `Custom Scan (GpuAccelSrfTargetList)`, 2/3 tests pass (passthrough
  SIGABRT documented as separate P0 above) (`a57cadb`)
- ✅ pg_test schema-name unblock (`pg_test_explain` → `preagg_explain`,
  PG forbids `pg_` schema prefix) — `fb64b1a`

The 2026-05-02 cheat-audit (7 host-loop kernels in spatial /
raster / window) is closed — point_in_ring fp32, segment_intersects
fp32+fp64, map_algebra small-N, raster_clip small-N, and
window_rank/dense_rank/sum/count are all real SYCL kernels now. Use
`just audit-cpu-cheats` to re-validate after every kernel-layer
change (PASS = clean, FAIL = lists violating symbols with
`file:line`).

## Phase 1 — Close remaining fp64 gaps

fp64 on Metal works end-to-end. AdaptiveCpp `fork-safe-metal` SHA
`4f3cde11` fixes the i128 add/sub/mul lowering bug that was silently
returning 0 for every soft-fp64 mantissa multiply on Metal — see the
commit message and `pgaccel-kernels/test/test_fp64_mul_probe.cpp` for
the kernel-boundary repro. Phase 1 ship-gate criteria post-fix:
`test_reduce_stats` var/stddev PASS, `test_spatial` 58/58, `test_h3`
112/112 (incl. fp64 res 12-15), `test_oom_invariant` all 5 fp64
families PASS, `test_correctness` 329/329 (tree-reduce-aware
ULP budget landed in `aab2ead` at `pgaccel-kernels/test/test_correctness.cpp:1414`).
The only open Phase 1 item is the cost-multiplier calibration below,
blocked on Phase 6 dispatch perf.

### Cost-model empirical calibration of `soft_fp64_cost_multiplier`

- **What**: `pg_accel.soft_fp64_cost_multiplier` seed default is `32.0` (the
  micro-bench throughput ratio). Real-query ratios differ due to cache /
  memory / dispatch overhead and need per-workload calibration. Hard cap
  `64.0` enforced at the GUC registration site. MAJOR.
- **Why**: Mis-tuned multiplier either over-routes to GPU (fp64 query
  regresses vs PG parallel — violates Benchmark Rule #11) or under-routes
  (PG wins queries GPU should win). Parity-floor rule forbids the former.
- **How**:
  - After `just gpu-test` green, run `just bench fp64_matrix` — 7 workloads
    × 5 sizes (100k / 1M / 10M / 100M / 1B) with `speedup_x ≥ 1.0` via
    Custom Scan selection (not planner-decline-as-parity).
  - **Sweep methodology.** Fixed grid: `{16, 24, 32, 40, 48, 56, 64}`. For
    each multiplier value, run the full 7×5 matrix and record `speedup_x`
    per cell. **Objective**: maximise `geomean(speedup_x)` across the
    35-cell matrix, **subject to the constraint** that no cell has
    `speedup_x < 1.0`. **Tie-break**: on ties, pick the smallest multiplier
    (minimises false-negative GPU declines as hardware evolves).
    Multipliers that violate the `< 1.0` constraint on any cell are
    disqualified before the geomean tie-break runs.
  - Document final value in CLAUDE.md and CHANGELOG.md with the winning
    geomean, the runner-up, and which cells were parity-close (≤ 1.1×).
  - Verify `pg_accel.fp64_enabled=false` cleanly routes all fp64 strategies
    to PG native with no Custom Scan injection (escape-valve GUC check
    happens in `planner_hooks.rs` before Custom Scan path creation — verify
    via EXPLAIN).
- Depends on: `just gpu-test` green (Phase 1).
- **Done when**: `just bench fp64_matrix` has `speedup_x ≥ 1.0` across every
  cell under the winning multiplier; geomean + runner-up table committed to
  CHANGELOG.md; `fp64_enabled=false` confirmed via EXPLAIN trace.

## Phase 3 — Parallel-path coverage

Partial-agg parallel execution works for plain `SUM`/`MIN`/`MAX`/`COUNT` on
scalar types and for AVG/STDDEV/VAR on float4/float8 (see git log for
`AVG/STDDEV/VAR parallel path`). Remaining work adds GROUP BY, preagg, and
window coverage.

### Grouped HashAgg parallel path — blocked on executor grouped partial emit

Investigation identified the real blocker as executor-side, not planner-side.
The `groupClause` bail at `planner_hooks/partial_agg.rs:84` and the PAAG
sentinel extension to carry group keys would be ~80 + 30 lines, but inert
without an executor path that emits
`[gk_bytes…, partial_state_0, partial_state_1, …]` per group.

Executor gap:
- `engine/executor/agg/execute.rs:1393-1409` `finalize_partial` iterates a
  single `ColumnAccumulator` per column — one tuple total, not one per
  group. Only reached from `finalize_result` on the non-grouped path.
- Grouped path (`execute.rs:2008-2196` → `emit_grouped_tuple` at `:1886` →
  `gpu::hash_agg_execute`) returns **finalized** per-group f64 scalars via
  `HashAggResult::results()` (`gpu/mod.rs:1040-1058`) — not the partial
  transition state. For AVG/STDDEV/VAR, PG's `float8_accum` transtype is
  float8[3] `[N, sum, sum_sq]`; a single f64 per group is not a valid
  partial to pass to `numeric_avg_combine`.
- `ColumnAccumulator` (`partial/accumulator.rs:17-43`) is a single unkeyed
  state; no group-keyed accumulator type exists.

Split into 3a (planner) and 3b (executor, gating):

**Phase 3b (gating) — Executor grouped partial emit path** ✅ **DONE**
(commits `be4d2c2` + `46726d8` + `13797a0`).
- Kernel (`pgaccel-kernels/src/hash_agg.cpp`):
  `pgaccel_hash_agg_execute_partial` C entry point + parallel
  `agg_hash_partial` / `agg_sort_based_partial` host orchestration +
  `run_unsorted_partial_kernel` / `run_sorted_partial_kernel` SYCL
  kernels. Per-agg per-group lanes: 1 for SUM/MIN/MAX/COUNT, 2 for
  AVG (`[N, sum]`), 3 for STDDEV/VAR (`[N, sum, sum_sq]`).
- Bridge (`pg_accel/src/gpu/`): `PgaccelAggFunc::Avg/Stddev/Var`
  variants + `partial_width()` helper; `HashAggResult::partial_results()`
  / `partial_width()` accessors; `hash_agg_execute_partial()` wrapper.
- Executor (`pg_accel/src/engine/executor/agg/execute.rs`):
  `execute_grouped_agg_partial` + `emit_grouped_tuple_partial` —
  emits `float8[2]` (AVG) / `float8[3]` (STDDEV/VAR) Datums via
  `construct_array`, with `Sxx = sum_sq − sum²/N` conversion at emit
  time matching `Float8StatsEmitter`. Width-1 funcs keep finalize-mode
  Datum bit shape. Path is dormant until `partial_emitters` is set
  for the grouped case (Phase 3A's job).
- Tests:
  - Kernel: `test_hash_agg_partial` — 6 tests / 64 assertions PASS
    cold-cache (SUM/AVG/STDDEV/MIN+MAX/COUNT*/AVG-bool, both small-N
    hash and large-N sort paths).
  - Rust: 4 `partial_ffi_*` lib tests PASS under `--features pg_test`
    (bridge round-trip + finalize-state width-1 fallback).
- `test_hash_agg_keys` 10/10 cold-cache — finalize-mode regression
  baseline preserved.

**Phase 3a (planner) — UNBLOCKED, ready to land**
- Remove the `groupClause` bail at `partial_agg.rs:84`.
- Extend PAAG (or new PGGK block) with `group_keys: Vec<(attno, typoid)>`.
- Thread groupClause into `plan_custom_path_agg` in `custom_scan/mod.rs`.
- Pass `groupClause` + `numGroups` to `create_agg_path` alongside
  `AGG_PLAIN`/`numGroups=1` currently hardcoded at `partial_agg.rs:377,382`.
- For grouped paths, populate `AggExecState::partial_emitters` so
  `next_grouped` routes through the new partial-mode path (3B).

**Gates verified intact (must not regress)**:
- `partial_agg.rs:170-203` precise AVG/STDDEV/VAR transtype gate.
- `agg_common.rs:79-93` NUMERIC classification gate.
- `mod.rs` + `hashjoin.rs` soft-fp64 multiplier.

**Done when**: `SELECT k, SUM(v) FROM t GROUP BY k` with parallel workers
shows GPU Custom Scan inside a Gather and produces identical results.

### Preagg parallel path — Agg-strategy chain shipped, true PreAgg parallelisation deferred

- **What** (refreshed 2026-05-03 after Agent P1 commits `e386720` +
  `5599067`): The Finalize Agg → Gather → CustomPath chain is built
  and `add_path(grouped_rel, finalize_path)` is called. Mirrors
  `partial_agg::try_inject` structurally (uses
  `agg_path_methods()` + `is_partial=1` + PAAG sentinel layout).
  GROUP BY propagation works via `root.processed_groupClause` +
  `parse.havingQual` into `pg_sys::create_agg_path` (mirrors PG's
  own `add_paths_to_grouping_rel`, `planner.c:7253-7263`).
  `AGG_HASHED` strategy when GROUP BY present, `AGG_PLAIN`
  otherwise. 16/16 unit tests pass.
- **Compromise (anti-cheat ban #9)**: P1's chain uses the existing
  `Agg` strategy (not `PreAgg`) because parallelising the actual
  `PreAggExecState` would over-aggregate N-fold under workers —
  see "PreAgg executor refactor for parallel safety" entry below.
  Agent 1B's `PreAggPrivData::partial` round-trip
  (`pg_accel/src/engine/ffi/custom_scan/private_data.rs`) is left
  in place but NOT exercised by the new chain; it activates once
  the executor refactor lands. So the chain accelerates standard
  parallel grouped aggregation today but doesn't yet exploit
  star-join fusion in parallel.
- **Done when**: A standard parallel-aggregation plan with GROUP BY
  uses the new chain; EXPLAIN confirms `Finalize Agg → Gather →
  CustomScan(GpuAccel)` with `parallel_safe=true` on the partial
  CustomPath. (PreAgg-strategy parallelisation is its own entry.)

### PreAgg executor refactor for parallel safety — B5a + B5b — DONE 2026-05-03

- **What** (refreshed 2026-05-03 after Agent B5 escalated per ban #9):
  `PreAggExecState` opened the fact table directly via
  `table_open(scan_oid)` (see `pg_accel/src/engine/executor/preagg/mod.rs`
  and `partial_emit.rs`). Under PG's parallel workers each worker would
  re-open the same fact relation and re-scan it from row 0 — N workers
  means N× duplicate input rows, which over-aggregates by exactly N.
  P1's planner chain fell back to standard `Agg` strategy rather than
  `PreAgg` (see "Preagg parallel path" entry above) as the workaround.
  MAJOR (correctness — under parallel, PreAgg as previously coded
  would silently produce wrong sums).
- **Why escalated** (Agent B5 ban-#9 evaluation): single-shot refactor
  surfaced 3 distinct mismatches that aren't safely committable in one
  atomic chain:
  1. PreAgg CustomPath has no `lefttree` to wire — `pgaccel_inject_gpu_preagg`
     (`pg_accel/src/engine/ffi/planner_hooks/mod.rs:1069-1074`) attaches
     only `inner_paths` (dimensions) to `custom_paths`. There is no
     fact-side path attached. Adding one requires re-indexing
     `materialize_dimensions` to skip it.
  2. **42 heap-direct call sites** across `preagg/mod.rs` + `preagg/partial.rs`
     use `HeapTupleHeader` + `try_fast_read_heap_pub` (heap offset math,
     NULL bitmap walking). All of `heap_read_i64`, `heap_read_f64`,
     `fused_eval_cmp`, `apply_fact_filter`, `build_group_key`,
     `extract_fact_group_key`, `scan_and_accumulate`'s extractors, and
     lazy-init `AttExtractInfo::new` need slot-based equivalents
     (`slot_getallattrs` + `tts_values[i]` decoding). ~150-line read-side
     rewrite per call-site type.
  3. Cost gate at `planner_hooks/mod.rs:1018-1034` compares against PG
     serial best, not parallel — biases against parallel paths. Verifying
     parallel correctness requires a GUC override path or carefully sized
     fixture; easy to ship a test that passes for the wrong reason (PG
     falling back to its own parallel HashAgg).
- **How — split into 2 independently-verifiable phases**:
  - **B5a (planner-side, no exec changes)** — **DONE 2026-05-02
    (branch `agent-b5a-preagg-planner`)**. The
    `pg_accel.preagg_parallel_safe` boolean GUC (default `false`) is
    registered in `pg_accel/src/engine/gucs.rs`. When `true`,
    `pgaccel_inject_gpu_preagg`
    (`pg_accel/src/engine/ffi/planner_hooks/mod.rs`) attaches the
    cheapest fact-side base Path as `custom_paths[0]`, sets
    `path.parallel_safe = true`, and inherits `parallel_workers` from the
    fact path. The wire layout adds an optional
    `PREAGG_PARALLEL_ATTACHED_SENTINEL` block (round-tripped via
    `PreAggPrivData::parallel_safe_planner_attached`) that the executor
    side reads in `custom_scan/mod.rs::begin_custom_scan` to skip slot 0
    of `(*node).custom_ps` when present — keeping `child_states[i]`
    aligned with `depths[i]`. Default-off plans emit no sentinel and are
    byte-identical to pre-B5a on the wire. Round-trip + GUC tests under
    `engine::ffi::custom_scan::tests::b5a_round_trip`.
  - **B5b (exec-side, gated by the flag)** — **DONE 2026-05-03**. Added
    `PreAggExecState::set_fact_child(child_ps)` and a slot-based
    `scan_and_accumulate_slot` path that consumes the fact-side
    `PlanState` at `custom_ps[0]` via `ExecProcNode`. `scan_and_accumulate`
    dispatches to slot-path when `fact_child` is set, falls back to
    heap-path otherwise (preserved byte-identical to pre-B5a for
    `preagg_parallel_safe = off`). Critical fix: the child plan's
    targetlist may project the base scan to fewer columns than the
    relation has, so the slot's `tts_values` indexes do **not** match
    relation attnos; the executor walks `(*plan).targetlist` on the
    first row to build `fact_slot_attno_map: HashMap<rel_attno,
    slot_attno>`, and `AttExtractInfo::new` is called against the
    translated slot attno. The 47-test preagg suite (sans 1
    pre-existing pgrx schema-rendering failure unrelated to this work)
    passes cold-cache. **GUC default is now `on`**; operators can flip
    it `off` for A/B regression vs the legacy heap-direct path.
- Depends on: Nothing — pure pg_accel work, no upstream dependency.
- **Closed when**: A star-join + GROUP BY query runs PreAgg inside
  parallel workers without N-fold over-aggregation; the
  `pg_accel.preagg_parallel_safe = on` GUC is the default;
  heap-direct fallback path can be removed in a separate cleanup PR.

### Window executor has no partial path

- **What**: `executor/window/mod.rs` doesn't expose a parallel-safe
  wrapping; `ROW_NUMBER` / `RANK` over a Gather child currently runs
  the window on the leader after collecting worker output. MINOR
  (downgraded 2026-05-02). The PreAgg partial-emit scaffolding has
  partially landed (`PreAggPrivData.partial`, `enable_partial(spec)`
  in `begin_custom_scan` — commit `be493db`); the Window executor
  doesn't yet hook into the same round-trip.
- **Why**: Partitioned window functions (PARTITION BY aligned with
  worker distribution) are naturally parallel; leaving them leader-
  only wastes cores.
- **How**:
  - Add an explicit parallel_safe hook per-spec.
  - Inject a partial-window CustomPath when PARTITION BY aligns
    with the underlying parallel scan; mirror
    `partial_agg::try_inject` for the Finalize → Gather →
    CustomPath chain (~100 LOC; same shape as the preagg follow-up
    in Phase 3 above).
- **Done when**: `ROW_NUMBER() OVER (PARTITION BY k ORDER BY v)`
  runs inside workers per EXPLAIN, not on the leader.

## Phase 4 — Operator coverage expansion

pg_accel today injects at `set_rel_pathlist_hook` (seqscan / indexscan) and
at upper paths (agg, sort, hashjoin, window). The following PG nodes never
reach a GPU path — each unblocks a class of real-world queries.

IncrementalSort and MergeJoin recognition landed this round as
detect-and-decline with `planner_rejected` counters; full injection is
tracked in Post-1.0 (deferred) because both require kernels that don't
exist yet (cascaded multi-key sort; merge-join kernel).

### NestedLoop (scalar) recognition

- **What**: Only spatial nested-loop is wired (via bbox + predicate
  fusion). Scalar nested loops with indexable quals are not accelerated.
  MINOR.
- **Why**: Correlated inequality joins PG can't hash still appear in real
  workloads.
- **How**: Consider a GpuHashJoin rewrite for correlated inequality joins.
- **Done when**: A correlated inequality join measurably accelerates under
  GPU injection.

### Combined `AVG + STDDEV`: partial-agg path injected but discarded

- **What**: Investigation (commit pending) shows pgaccel's
  `partial_agg::try_inject` DOES inject a `Finalize(Agg) → Gather →
  GpuAccel(partial)` chain for combined `SELECT AVG(v), STDDEV(v)`.
  Debug log line: `pg_accel partial_agg: injected Finalize(Agg) ->
  Gather -> GpuAccel(partial) chain (n_aggs=2, ...)`. PG's
  `add_path()` then discards the path because the GpuAccel-side
  per-row Custom-Scan yield cost dominates. Single STDDEV alone
  wins (its arith cost amortises yield); single AVG alone loses
  (cheap in PG); combined falls in the lose camp. Same root cause
  as Phase 4 plain-JOIN entry below. MAJOR.
- **Why**: One of the most common analytics shapes (mean+spread).
- **How**: Same as plain JOIN — measure real per-row yield cost or
  restructure cost shape to amortise over batch yield instead of
  per-tuple. The audit row was reclassified
  `RequiredToday` → `RequiredAfterPhase("6 yield-cost reduction
  (partial-agg path injected but add_path discards on cost)")`.
- **Done when**: `parallel_avg_stddev` row in
  `pg_accel_bench explain-audit` reports `[PASS]`.

### `ORDER BY v LIMIT 100` correctly defers — fixture issue, not gap

- **What**: EXPLAIN audit `parallel_orderby` row was misframed.
  pgaccel's sort injector deliberately defers to PG's top-N
  heapsort when LIMIT is small relative to N (debug:
  `pg_accel sort: LIMIT 100 << 10000000 rows, deferring to PG
  top-N heapsort`). This is correct cost-aware behavior — top-N
  heapsort is O(N log K) for K=100, dominating a full GPU sort.
  No planner gap; the audit fixture is GPU-unfavorable by design.
- **Why**: We don't want to over-inject onto small-K top-K shapes
  where PG's algorithm is genuinely better.
- **How**: Either (a) keep the audit row deferred and rewrite the
  fixture to use a larger LIMIT (e.g. LIMIT N/10) so the GpuSort
  path is genuinely the planner's preferred choice, or (b) drop
  the row and add a separate row for the bare `ORDER BY v` (no
  LIMIT) shape that exercises full GpuSort. The audit row was
  reclassified to
  `RequiredAfterPhase("audit fixture rewrite (LIMIT 100 << 10M
  defers to PG top-N heapsort by design — small-K is GPU-
  unfavorable)")`.
- **Done when**: A new audit row exercising the full-sort path
  reports `[PASS]`, and the LIMIT-100 row is either deleted or
  documented as expected-deferred.

### Plain JOIN: GpuHashJoin path injected but discarded by add_path()

- **What**: EXPLAIN audit's "no pg_accel injection on plain JOIN" was
  half-right. Investigation confirmed the
  `set_join_pathlist_hook` DOES fire and pgaccel's GpuHashJoin path
  IS injected via `add_path()`. PG's `add_path()` still discards the
  path on cost. After yield calibration `0.03 → 0.01` in `4144ac8`
  (`pg_accel/src/engine/cost/constants.rs:184` —
  `CUSTOM_SCAN_YIELD_COST = 0.01`), the yield term dropped from 300K
  to ~100K cost units on a 10M-output join, but the path is still
  discarded — the per-row build + probe cost (`0.01/row each`,
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:239`) is now
  the dominant pgaccel-side cost. MAJOR (cost-model + Phase 6
  dispatch-perf intersection).
- **Why**: Joins are the backbone of analytics queries. The hook
  is correct; the cost is honest. Closing the per-row probe cost
  needs Phase 6 dispatch-perf reductions or a cost-shape change that
  amortises over batch yield instead of per-tuple.
- **How**:
  - Phase 6 dispatch-perf wins (command-buffer reuse, kernel fusion)
    will lower the per-row probe contribution naturally; calibrate
    once those land.
  - OR change the cost model to charge build/probe per-batch (not
    per-row) once batch dispatch is amortised by Phase 6 work.
  - The audit row in `pg_accel_bench/src/explain_audit.rs` was
    re-classified from `RequiredToday` to
    `RequiredAfterPhase("6 yield-cost reduction ...")` so the
    harness ratchets cleanly when Phase 6 lands.
- **Done when**: `parallel_join` row in `pg_accel_bench explain-audit`
  reports `[PASS]` (i.e. PG picks pgaccel's GpuHashJoin path over
  its native parallel hash join for this workload).

### PostGIS operator registrations — kernels + dispatch closed for the 11 routine predicates

- **What** (refreshed 2026-05-02 after the 4 algorithmic predicates and
  st_distance polygonal kernel land): Predicates with functionally-
  complete GPU paths today:
  - `st_intersects` — `pgaccel_spatial_intersects` →
    `three_layer::spatial_intersects`.
  - `st_dwithin` — `pgaccel_sphere_distance_bulk` (fp32 SYCL) via
    `three_layer::spatial_dwithin`. Point × Point only; non-Point
    short-circuits to UNCERTAIN. fp64 returns NO_DEVICE — see
    Phase 7 "Metal SSCP soft-fp64 trig" for the fork-safety reason.
    Per-row threshold (3rd arg) needs the multi-arg dispatch carrier
    work tracked under Phase 4 "Multi-arg dispatch carrier" below.
  - `st_contains` / `st_within` — `pgaccel_point_in_ring_bulk` (fp32
    SYCL) via `three_layer::spatial_contains`. Polygon ⊇ Point only;
    constant-polygon batches collapse to one dispatch.
  - `st_disjoint` — inversion of `st_intersects` (no extra kernel).
  - `st_covers` / `st_coveredby` — alias of `st_contains` / `st_within`
    at the kernel level; PG Layer-3 recheck handles boundary semantics.
  - `st_distance` Point × Point — SHIPPED in `fed07d8` reusing
    `pgaccel_sphere_distance_bulk`.
  - `st_distance` Polygon × Polygon — SHIPPED 2026-05-02 in
    `pgaccel-kernels/src/spatial_predicates.cpp` via
    `pgaccel_st_distance_polygon_polygon_bulk` (commit `676a95d`,
    Agent 2A). fp32 vertex-pair minimum.
  - `st_area`, `st_length` (fp32) — SHIPPED in `0429586` / `14dc649`.
  - `st_equals` / `st_touches` / `st_crosses` / `st_overlaps` —
    SHIPPED 2026-05-02 in commits `b5e546a` (kernel),
    `2c08296` (`SpatialPredicate` enum + three-layer dispatch),
    `433bc21` (adapter registration) — Agent 2A. All 4 wired
    end-to-end through `three_layer::spatial_eval` with UNCERTAIN
    fall-through for non-supported geometry shapes.
- **Invariant locked**: Negative-assertion tests at
  `pg_accel/src/adapters/postgis.rs` for the predicates that are
  *still* not registered (notably the `st_buffer` / `st_union` /
  `st_intersection` constructors which need the variable-output
  protocol — see "PostGIS geometry constructors" below).
- **Done when**: Multi-arg dispatch carrier (Phase 4) lands so
  `st_dwithin` consumes per-row 3rd-arg thresholds; constructor
  output-allocation protocol designed (separate item).

### Geometry-array extractor for st_value

- **What** (surfaced 2026-05-03 by F1's escalation): The
  `pgaccel_raster_value(rast, point_array)` kernel is shipped
  (`pgaccel-kernels/src/raster_ops.cpp`) with full SYCL paths +
  test coverage. The blocker is purely on the Rust extractor side:
  PG's `geometry[]` arg type is wrapped in `ArrayType` varlena,
  and the existing `extract_geometry` (single-geom) doesn't
  iterate array bodies. MINOR (one of 27 raster ops; can be
  added independently).
- **Why**: `ST_Value(rast, ARRAY[point1, point2, ...])` is a
  recognised PostGIS pattern for pixel-lookup of multiple points
  in one call (avoids per-point round-trip). Without the
  extractor we can't dispatch.
- **How**:
  - Add `extract_geometry_array(datum) -> Result<Vec<ExtractedGeom>,
    ExtractError>` to `pg_accel/src/adapters/extractors/geometry/`
    walking the ArrayType layout: header (ndim + dataoffset +
    elemtype) → dim sizes → null bitmap → packed payload (each
    element is itself a varlena geometry, since geometry is
    variable-length).
  - Wire `dispatch_st_value` (currently a tracing::debug!
    placeholder in `dispatch/raster.rs`) to use the new extractor
    plus the existing `gpu::raster_value` wrapper (which has
    `#[allow(dead_code)]` to drop once consumed).
  - Roundtrip test: `SELECT ST_Value(rast, ARRAY[ST_Point(1,1),
    ST_Point(2,2)])` against a known 4×4 raster.
- Depends on: Nothing — extractor is local to pg_accel.
- **Done when**: `st_value` dispatches end-to-end; `gpu::raster_value`
  loses its `#[allow(dead_code)]`; integration test green.

### PostGIS geometry constructors: output-allocation kernel design

- **What**: Geometry constructors `st_buffer`, `st_union`, `st_intersection`
  need output-allocation plumbing that doesn't exist yet — GPU kernels that
  return variable-sized `GSERIALIZED` output to the executor. Currently no
  registration; no kernel design. MINOR.
- **Why**: These are the three most common constructors in real PostGIS
  workloads after the predicate set above. Without them, any query that
  constructs new geometry falls back to CPU.
- **How**:
  - Design the output-allocation protocol: two-pass kernel (sizing pass +
    emission pass) vs bounded-worst-case preallocation vs streaming append
    with `MTLHeap`. Shared design with the H3 variable-output work below.
  - Extend `pgaccel-kernels/src/spatial_*.cpp` with constructor kernels
    following the chosen protocol.
  - Register in `src/adapters/postgis.rs` with a `GpuSpatialConstructor`
    variant (new strategy tag).
  - Write adapter-level tests that exercise varying output geometry sizes.
- **Done when**: Each of `st_buffer` / `st_union` / `st_intersection` has a
  working GPU kernel, registered adapter, and golden-diff test against
  PostGIS native.

### PostGIS raster — st_value array extractor remains; other 4 dispatched

- **What** (refreshed 2026-05-03 after Phase II Agent F1 commit
  `1795e55`): All 9 raster ops are registered in
  `pg_accel/src/adapters/postgis_raster.rs`. Wired end-to-end:
  `st_mapalgebra` (existing), `st_clip`, `st_reclass`,
  `st_summarystats` (`OutputShape::Record` returning
  `count + sum + mean + stddev + min + max`), plus the 4 dispatched
  via the multi-arg carrier in Phase II: `st_resample`,
  `st_slope`, `st_aspect`, `st_hillshade`. Only `st_value` remains
  dispatch-pending — it takes a `geometry[]` ArrayType varlena and
  the existing extractors don't walk PG ArrayType bodies. MINOR.
- **Concrete fix**: see "Geometry-array extractor for st_value"
  entry above.
- **Invariant locked**: Two regression tests at
  `pg_accel/src/adapters/postgis_raster.rs`
  (`does_not_register_kernelless_raster_candidates`,
  `registered_set_matches_kernel_set`) assert the registered set
  matches the kernel set exactly.
- **Done when**: The geometry-array extractor lands; `st_value`
  dispatches per-row; golden-diff test against PostGIS raster
  output.

### H3 var-output ops — wiring landed; 3 follow-up blockers (registry-init / shape / array walker)

- **What** (refreshed 2026-05-03 after Agent F3 partial completion):
  All 15 H3 ops are registered. Wired end-to-end (dispatch + kernel)
  today: 14 total (9 fixed-1:1-output + 5 var-output: `grid_disk`,
  `grid_ring_unsafe`, `cell_to_children`, `cell_to_boundary`,
  `polyfill`). The 1 remaining var-output op —
  `cells_to_multi_polygon` — has a working SYCL kernel at
  `pgaccel-kernels/src/h3_ops.cpp` and a wrapper at
  `pg_accel/src/gpu/mod.rs:h3_cells_to_multi_polygon_bulk`, but the
  dispatch arm requires per-row `bigint[]` ArrayType extraction
  (same blocker as `st_value`'s `geometry[]`).
  - **GSERIALIZED v2 encoder: SHIPPED** at
    `pg_accel/src/adapters/extractors/geometry/polygon_encoder.rs`
    (Agent F2, commit `3abaf50`).
  - **F3 dispatch arms shipped** (commit `b5702e6`):
    `h3_cell_to_boundary` now produces `AcceleratedVarLen` of
    GSERIALIZED varlena Datums via the F2 encoder; `h3_polyfill`
    extracts per-row polygons via existing `extract_geometry` and
    runs the two-pass kernel.
  - **F3 FunctionScan plumbing landed** (commits `30a9b6e`,
    `da6b054`, branch `agent-f3-finish-v2`): the `projectset.rs`
    planner hook walks `RTE_FUNCTION` rels, looks up the funcexpr
    in the registry, and builds a `CustomPath` carrying a
    `FunctionScanPrivData` payload; new `FUNCTION_PATH_METHODS` /
    `FUNCTION_SCAN_METHODS` vtables are registered; the
    `function_scan` submodule
    (`pg_accel/src/engine/ffi/custom_scan/function_scan.rs`)
    builds a TupleDesc (resolving sentinel OID 0 via
    `get_func_rettype`), dispatches the SRF once via
    `dispatch::dispatch`, and emits one row per dispatch output via
    `ExecStoreVirtualTuple` (Scalar / VarLen) or `heap_form_tuple` +
    `ExecStoreHeapTuple` (Record). The defensive arm at
    `scan/exec.rs:451` is correctly left in place — it covers
    planner mis-routing of `AcceleratedVarLen` / `AcceleratedRecord`
    onto a per-row predicate filter shape, which doesn't apply to
    FunctionScan's one-shot dispatch.
  - **Registry-init blocker closed 2026-05-02 by Agent B2** (option (a)
    shipped): registry now exposes `AdapterRegistry::resolve_oids_again()`
    and the global `registry::resolve_oids_again()` wrapper at
    `pg_accel/src/engine/registry.rs:340`. `AdapterRegistry::lookup`
    auto-retries via this on a miss (guarded by the `RETRYING_LOOKUP`
    thread-local at `pg_accel/src/engine/registry.rs:464` to bound
    work to one retry per planner pass), so a `CREATE EXTENSION h3`
    that runs *after* `lazy_init` no longer leaves the FunctionScan
    injection chain dark. Option (b) — `pgrx.toml extra_extensions` —
    was checked and does not exist in pgrx 0.17 (`pgrx-tests` 0.17
    only exposes `pg_test::setup` + `postgresql_conf_options`, no
    extension-pre-install knob); the lookup-retry path is sufficient.
    Verified via the PG log line `pg_accel: registry re-resolve:
    activating new adapter 'h3'` followed by `resolved 17 function
    OIDs across 1 adapters` on the first FunctionScan lookup miss.
  - **Resolved 2026-05-03 (B2.5): GPU FunctionScan executor crash at
    `pg_accel/src/engine/ffi/custom_scan/function_scan.rs:279`.** Root
    cause confirmed via
    `~/Library/Logs/DiagnosticReports/postgres-2026-05-03-122131.ips`
    (frame: panic at the `pg_sys::ExecSetSlotDescriptor` call) +
    PG17 source review:
    `src/backend/executor/nodeCustom.c:ExecInitScanTupleSlot` builds
    the scan slot from `ExecTypeFromTL(custom_scan_tlist)`, and
    `src/backend/executor/execTuples.c:MakeTupleTableSlot` sets
    `TTS_FLAG_FIXED` on any slot constructed with a non-NULL tupdesc —
    so `ExecSetSlotDescriptor` (which asserts `!TTS_FIXED`) cannot be
    used from `BeginCustomScan` to swap the descriptor.
    Fix landed (candidate (a)): build `custom_scan_tlist` in
    `plan_custom_path_function` from the registry's
    `output_field_types` / `output_field_names` so PG's
    `ExecTypeFromTL` produces the correct tupdesc on first
    construction. The `init_state` flow now only validates the slot's
    existing tupdesc against the registry shape and bails cleanly on
    mismatch. Verified: 3 integration tests
    (`function_scan_h3_cell_to_boundary_emits_one_row`,
    `function_scan_st_summarystats_emits_six_field_record`,
    `function_scan_explain_does_not_crash_for_cell_to_boundary`) all
    PASS individually post-fix. The 4th test
    (`function_scan_h3_grid_disk_emits_seven_cells_for_k1`) now fails
    on a *different* signature — `pgrx::error!` from
    `pg_accel/src/engine/dispatch/h3.rs:412`
    ("h3_grid_disk GPU kernel failed; refusing CPU fallback") — not
    the executor crash. That is a kernel-layer bug
    (`gpu::h3_grid_disk_bulk` returning `None`); see new TODO entry
    "h3_grid_disk SYCL kernel returns None on single-cell input"
    below.
  - **Closed (2026-05-03, Agent K Round 3):** `h3_grid_disk` single-cell
    `None` and `h3_cell_to_boundary` multi-cell SIGABRT root-caused to
    AdaptiveCpp Metal SSCP emitter bugs:
    - `pgaccel_h3_grid_disk_output_size` JIT-failed with
      `LLVMToMetal: MetalEmitter failed: Error: Unsupported integer bit
      width: 33` for ALL counts (the count=1 case surfaced first via
      `function_scan_h3_grid_disk_emits_seven_cells_for_k1`).
    - `pgaccel_h3_cell_to_boundary_emit` JIT-failed with `use of
      undeclared identifier 't_double__1_000000e_00__double__0_000000e_00_'`
      (a `[2 x double] {1.0, 0.0}` PHI literal referenced but never
      declared in the emitted Metal). Multi-cell input then
      SIGABRT'd via the delegated `pgaccel_h3_cells_to_multi_polygon_emit`.
    Fix at `pgaccel-kernels/src/h3_ops.cpp` ports the size pass for
    `grid_disk` / `grid_ring_unsafe` / `cell_to_boundary` and the entire
    boundary emit kernel to host code (mirrors existing host-only pattern
    in `cells_to_multi_polygon_output_size` and `polyfill_*`). Pinned by
    new regression tests in `pgaccel-kernels/test/test_h3.cpp`
    (`test_grid_disk_single_cell_input`, `test_cell_to_boundary_multi_cell_emit`).
    The pgrx test `function_scan_h3_grid_disk_emits_seven_cells_for_k1`
    now PASSES (verified cold-cache).
  - **CLOSED 2026-05-03 by Agent BUG1**: the multi-cell crash root-caused
    to the FunctionScan executor's Record arm calling
    `pg_sys::ExecStoreHeapTuple` against a virtual scan slot
    (`TTSOpsVirtual` — the default for Custom Scan), which triggers
    `elog(ERROR, "trying to store a heap tuple into wrong type of slot")`
    in `src/backend/executor/execTuples.c::ExecStoreHeapTuple` and
    propagates as `panic_cannot_unwind`. Fix at
    `pg_accel/src/engine/ffi/custom_scan/function_scan.rs:413` switches
    the Record path to populate `tts_values` / `tts_isnull` directly and
    promote with `ExecStoreVirtualTuple`, mirroring the Scalar / VarLen
    arms. Hand-rolled `polygon[]` ArrayType layout was *also* replaced
    with PG's `construct_array` via `build_polygon_array_datum`
    (`pg_accel/src/engine/dispatch/h3.rs`) for defence-in-depth — the
    earlier `encode_pg_polygon_array` path is retained only as a tested
    encoder reference. Verified cold-cache: `h3_cells_to_multi_polygon_emits_one_row`
    + `h3_cells_to_multi_polygon_npoints` + `h3_cell_to_boundary_npoints`
    all PASS (3/3 in the dispatch/h3 tests submodule).
  - **CLOSED 2026-05-03 by Agent B4 (commit `c53ae15`)**: dispatch shape
    mismatch for `h3_cell_to_boundary` — kernel previously emitted
    GSERIALIZED but h3-pg declares `RETURNS polygon` (PG built-in).
    Fix landed: new `polygon_encoder::encode_pg_polygon` +
    `encode_pg_polygon_array` helpers; dispatch arm switched to
    PG-polygon emission. Hard verification: `npoints()` returns 6 for
    a hexagon. Same fix sweeps `h3_cells_to_multi_polygon` (kernel
    emits exterior + holes as `(polygon, polygon[])` Record per row)
    — single-cell verified via `h3_cells_to_multi_polygon_npoints`.
    Multi-cell verified via `h3_cells_to_multi_polygon_emits_one_row`
    after Agent BUG1's executor fix above.
- **Invariant locked**: Two regression tests at
  `pg_accel/src/adapters/h3.rs`
  (`unimplemented_ops_are_not_registered`,
  `registered_ops_match_kernel_set_exactly`) assert the registered
  set matches the kernel set exactly.
- **Done when** (refreshed 2026-05-03 after B5a / K / SRF executor / BUG1
  / BUG2):
  CLOSED — FunctionScan plumbing landed (`30a9b6e`, `da6b054`); registry
  re-resolve landed (`3827b24`); FunctionScan executor crash fixed
  (`bedda75`); h3_cell_to_boundary + h3_cells_to_multi_polygon shape fix
  landed (`c53ae15`); h3_grid_disk single-cell None + cell_to_boundary
  multi-cell SIGABRT kernel-layer fixes landed (`0319d93`); SRF executor
  wiring landed (`a57cadb`); multi-cell `h3_cells_to_multi_polygon`
  Record-arm `ExecStoreHeapTuple` SIGABRT fixed by Agent BUG1; SRF
  passthrough column wrong-result fixed by Agent BUG2 (root cause was
  `gpu_agg` planner accepting `count(DISTINCT)` and silently dropping
  the DISTINCT; SRF executor itself emitted correct schema + data the
  whole time — see fix commit).

### h3_cells_to_multi_polygon multi-cell dispatch encoder bug (P0)

- **What** (surfaced 2026-05-03 by Agent K when un-ignoring the smoke):
  Kernel-layer SIGABRT IS fixed (`pgaccel_h3_cell_to_boundary_emit` ported
  to host code in commit `0319d93`; `test_cell_to_boundary_multi_cell_emit`
  returns 24/36/60 finite doubles for N=2/3/5 cells). A SEPARATE
  dispatch-layer bug now surfaces: `heap_form_tuple` validation fails on
  the `polygon[]` holes varlena, causing `panic_cannot_unwind` at
  `pg_accel/src/engine/ffi/custom_scan/function_scan.rs:445`
  (`ExecStoreHeapTuple`) → SIGABRT for multi-cell input.
- **Repro**: `h3_cells_to_multi_polygon_emits_one_row` test (currently
  `#[ignore]`'d in `pg_accel/src/engine/ffi/custom_scan/function_scan.rs`
  with multi-cell `ARRAY['8a..', '8a..']::h3index[]` input).
- **Where**: encoder pipeline `encode_pg_polygon_array` →
  `varlena_from_bare_bytes` → `heap_form_tuple`. Single-cell paths
  (where `holes` is empty) work — verified by
  `h3_cells_to_multi_polygon_npoints` smoke. Likely: ArrayType header
  for an empty-or-single-element `polygon[]` has subtle layout that
  PG's `heap_form_tuple` rejects (vl_len_, dataoffset, dim sizes for
  the 0/1-element case).
- **Done when**: `h3_cells_to_multi_polygon_emits_one_row` un-ignored
  and passes for multi-cell input; the holes varlena round-trips
  through `array_in` / `array_out` cleanly via PG's catalog functions.
- Depends on: nothing (pure encoder layer fix; kernel side already
  correct).

### Type coverage expansion

- **What**: GPU path handles int2 / int4 / int8 / float4 / float8 /
  bool / date (filter + arith, since Phase 2 bytecode commit
  `f37c67b`), UUID (`243fa1f`, classifier re-enabled `639f6f1` after
  Agent 4A's `309f8c7` flat-buffer kernel-staging fix) and INET /
  CIDR (`c4134d5` + `3fbc03d`, classifier re-enabled `639f6f1`) on
  hash_agg group keys. Extracts but doesn't GPU-process text /
  timestamp / timestamptz. Forcing CPU qual eval: INTERVAL (no
  extractor), JSON / JSONB (no extractor — GPU `jsonb_path_exists`
  / `->>` would be a major win), ARRAY types (no GPU support —
  forces per-row unnest on CPU), custom types (domains, composites
  — immediate reject in the classifier). NUMERIC / DECIMAL are
  already handled by the Phase 2 classification gate plus int128
  NumericSumEmitter (commit `dcd0cce`, 38-digit precision). MINOR.
- **Why**: Any workload touching one of the unsupported types falls
  back to PG's scalar qual evaluator. That's correct (results match
  PG bit-for-bit) but silently reduces GPU coverage on JSON / array
  / interval workloads.
- **Status note**: After Phase 2 bytecode dispatch landed, DATE
  arithmetic in WHERE compiles correctly to bytecode and produces
  bit-identical results to PG (verified with
  `WHERE dt + 30 > '2026-06-01'` on a 1M-row fixture). The plan may
  show the filter on `Parallel Seq Scan` rather than `GpuAccelScan`
  because PG's parallel scan + scalar qual is cheaper than 1-thread
  `GpuAccelScan` at this size — that's a planner cost decision, not
  a coverage gap.
- **How**:
  - UUID / INET / CIDR hash_join keys: deferred. The hash_join
    build / probe path is templated (`build_cpu<T>` / `probe_cpu<T>`)
    and assumes T is an arithmetic type with `keys_equal<T>`. UUID
    needs either a `__uint128_t` slot type or a struct-based
    instantiation; INET needs the same plus the canonical 24-byte
    layout. Planner classifier today only emits Uuid / Inet for
    hash_agg, so executor stub at `executor/join/probe.rs` raises
    `pgrx::error!` if a join key ever arrives wrong-typed.
  - JSON / JSONB last (analytics win): the GPU side needs a JSONB
    binary-format parser kernel; substantial work.
  - ARRAY: unnest-on-GPU is a separate kernel-design item.
  - Custom types (domains, composites): document as explicit policy
    rather than a silent skip.
- **Done when**: Each type has an extractor + test, or an explicit
  documented rejection; no silent CPU fallbacks.

## Phase 5 — Cost model / planner tuning

Core cost-model items landed this round (multi-key sort limit, Window fp64
multiplier, HashJoin fp64 multiplier, DeviceLimits docs + SRF, GUC hard-cap
tests). One item remains.

### Worker-side `ExecCustomScanRecheck` for spatial

- **What**: DSM callbacks in `engine/ffi/custom_scan/dsm.rs` are all present
  (`EstimateDSMCustomScan`, `InitializeDSMCustomScan`,
  `ReInitializeDSMCustomScan`, `InitializeWorkerCustomScan`,
  `ShutdownCustomScan`) but the spatial three-layer pipeline (bbox → GPU
  predicate → CPU recheck) always runs recheck on the leader. MAJOR.
- **Why**: For parallel spatial scans, the leader becomes the bottleneck —
  workers should recheck their own candidate tuples.
- **How**: Implement a `ExecCustomScanRecheck` that runs on the worker;
  plumb the state through DSM.
- **Done when**: Parallel spatial EXPLAIN ANALYZE shows recheck time
  distributed across workers, not pinned to leader.

## Phase 6 — Performance investigation

### `hash_agg` 2-level pointer kernel returns 0 on small batches

- **What**: `agg_hash`'s 2-level pointer kernel reads as 0 instead of the
  expected accumulation on tiny batches (`test_fork hashagg_f64` N=64
  reads 0 instead of 2048). The sort-based path
  (`pgaccel-kernels/src/hash_agg.cpp:790`) is correct for N ≥ 100k —
  verified at 10M. Only affects small-N (≪ `gpu_hash_agg_min_rows`),
  which is below the planner's GPU dispatch floor on a GPU-equipped
  machine, but the kernel-level bug is real. MAJOR (correctness).
- **Why**: Wrong-result class at any size violates the ship bar; the
  sort-based path masks it in real workloads but the underlying kernel
  is bit-corrupting on small input.
- **How**: Tried flat-buffer + packed-args refactor; each angle
  uncovered a deeper Metal SSCP edge case (argbuffer rejection, 9-arg
  capture limit). Reverted. Same root-cause class as the cold-cache
  fork crash on `sort_kv` below — needs the SSCP investigation, not
  a workaround in `hash_agg.cpp`.
- **Done when**: `test_fork hashagg_f64` N=64 returns 2048; SSCP
  investigation lands a root-cause fix (in either the kernel pointer
  chain or upstream SSCP emitter) and a regression test pins the
  small-N case.

### `reduce_multi_f32` / `reduce_multi_i64` consumer wiring

- **What** (added 2026-05-02): Bridge wrappers `reduce_multi_f32`
  (`pg_accel/src/gpu/mod.rs:508`) and `reduce_multi_i64`
  (`pg_accel/src/gpu/mod.rs:583`) ship behind `#[allow(dead_code)]`
  with a doc note pointing at the Phase B Agent 1B consumer wiring
  task. The agg executor today routes everything through
  `Vec<f64>` (`reduce_multi_f64`); the f32 / i64 wrappers stay
  unconsumed pending an executor refactor that picks the right
  width per column type. MINOR.
- **Why**: Removing the `#[allow(dead_code)]` annotations is
  blocked on an executor consumer; until a code path calls them,
  `cargo clippy -- -D warnings` would fail.
- **How**: Wire the agg executor's per-column accumulator to pick
  the matching `reduce_multi_*` kernel based on `AggColumn`
  result_type; drop the `#[allow(dead_code)]` annotations in the
  same diff.
- **Done when**: Executor calls `reduce_multi_f32` for f32-typed
  columns and `reduce_multi_i64` for i64-typed columns; the dead-
  code annotations are gone; `just lint` clean.

### Per-batch GPU dispatch dominates parallel SUM / GROUP BY

- **What**: 10M `SUM(v) FROM bench_f32_10m`: pg_accel parallel 177 ms vs PG
  parallel 88 ms. Each worker runs ~52 batches × 65k rows × ~5.5 ms
  dispatch. JIT cache is populated (`~/.acpp/apps/global/jit-cache/` has
  .metallib + .metalar). Pure dispatch cost. **Same class** observed in
  `fp64_matrix` first calibration run
  (`benchmarks/runs/fp64_matrix_1m_post-i128-fix.md`):
  `hashagg_f64_keys @ 1M` 0.41x, `@ 10M` 0.20x — single-thread
  `GpuAccelAgg` losing to PG's parallel HashAggregate. Same root
  cause: per-batch dispatch dominates at large N when the GPU work
  per batch is cheap (count(*), simple agg). MAJOR (performance-parity
  risk; blocks Phase 1 cost-multiplier calibration).
- **Why**: Current 10M reduce loses to PG parallel. Benchmark Rule #11 says
  this must never happen in a released (workload, size) cell.
- **How** (directions in priority order; each must beat or match PG
  parallel across the full sweep before the next is considered):
  1. **Command-buffer reuse across batches in the Metal bridge.** Highest
     expected value; the per-batch cost is dominated by command-buffer
     setup / submit, not GPU compute. Reuse across a worker's batch stream.
  2. **Kernel fusion: scan + reduce as a single dispatch.** Collapses the
     batch boundary entirely for common patterns.
  3. **Buffered accumulation at executor layer** so the GPU sees fewer,
     larger batches per worker.
  4. **Do NOT raise `min_batch_size` to skip the failing sizes.** Per
     Benchmark Rule #11 / feedback_dont_disable_gpu.md / anti-cheat
     ban #9, raising the threshold is a bug-hiding pattern, not a fix.
     If options 1–3 are exhausted, escalate to user with measured
     dispatch costs — do not silently downgrade GPU coverage.
- **Done when**: The full sweep (`benches/reduce_sum_bench.rs` row-count
  matrix `[100k, 1M, 10M, 100M, 1B]`) shows every cell ≥ PG parallel via
  Custom Scan selection, trace spans confirm reduced batch count per
  worker, and `min_batch_size` default is unchanged vs prior release.

### Per-fork JIT ~290 ms cold+warm

- **What**: Per `project_metal_fork_issue.md`, per-fork JIT is ~290 ms.
  The `kernel_configuration` hash misses the on-disk cache on some paths.
  MAJOR.
- **Why**: Adds latency to every first-dispatch after fork; compounds per
  parallel worker startup.
- **Hypothesis**: Metal SSCP jit-cache hash miss because
  pointer-width-dependent hash keys (pointer values baked into the config
  hash, TLS addresses, `_PG_init`-time globals that differ between parent
  and fork child) change across forks. Secondary hypothesis: hash is
  stable but metallib reload from disk is what costs ~290ms regardless of
  hit/miss.
- **How**:
  1. Dump `kernel_configuration` hash inputs pre-fork (parent) and
     post-fork (child) for the same logical kernel dispatch; diff
     byte-for-byte.
  2. If hash differs, fix the inputs to be fork-stable (strip
     pointer-width-dependent fields; canonicalise TLS-sensitive values).
  3. If hash is stable but metallib reload is the cost, investigate
     memory-mapped metallib cache (load once in `_PG_init` on the parent,
     rely on CoW-mapped pages post-fork).
- **Done when**: 10-child-fork stress test shows per-fork JIT wall time
  `≤ 50ms` (vs 290ms baseline), measured via `otel-tui` span durations on
  the first post-fork dispatch; hypothesis confirmed or falsified in the
  landing commit message.

### Metal pipeline-state XPC edge case

- **What**: Per `project_metal_fork_issue` memory, rare forks still hit
  `MTLCompilerService` after the `.metalar` archive path landed.
  INVESTIGATE.
- **Why**: Flaky crashes in parallel workers — hard to reproduce, hard to
  debug.
- **How**:
  - Instrument `acpp-metal-archive-build` return codes under stress.
  - Log every archive-build-and-load cycle to isolate the miss.
- **Done when**: Either the edge case is reproducible on demand (then
  fixable), or the stress run over 8 workers × 20 iterations shows zero
  `MTLCompilerService` errors.

### Cold-cache fork crash on sort_kv_i32 / sort_kv_i64

- **What**: Fresh backend + fresh JIT cache crashes in
  `-[_MTLFunction newArgumentEncoderWithBufferIndex:]` the first time the
  int-kv radix kernel is dispatched post-fork (`asi: crashed on child side
  of fork pre-exec`). Second run warms the `.metalar` archive and the
  dispatch succeeds (sort_int4 @ 100K = 1.70x WIN, sort_int8 @ 100K = 1.47x
  WIN after warmup). The f32 / f64 paths compile into the same
  `sycl_radix_sort_kv_u32` kernel but their SSCP caller-hash differs, so
  each type needs its own archive build. MAJOR (fork-safety regression).
- **Why**: Violates the "zero crashes on verification matrix" ship bar.
- **How**: Warmup-at-`_PG_init` dry-run dispatch was tried and made things
  worse (reverted). Same root cause as the small-N hash_agg kernel bug
  (Next Up #6) — a Metal SSCP edge case that needs investigation in either
  the kernel pointer chain or the upstream SSCP emitter, not a workaround.
- **Done when**: 100 iterations × fresh JIT cache × every radix
  specialisation (u32 / i32 / i64 / u64 / f32 / f64); zero crashes, zero
  `MTLCompilerService` errors across the matrix.

### Out-of-order executor overlap for sort / window

- **What**: `src/engine/executor/sort.rs` and `window.rs` force in-order
  Metal queues; per-DAG dispatch could overlap submit+exec via
  `submit_queue_wait_for` + `MTLSharedEvent`. MINOR (perf win, not
  correctness).
- **Why**: Fills GPU pipeline bubbles — incremental win.
- **How**: Build a per-DAG scheduler that tracks dependencies via
  `MTLSharedEvent`; dispatch overlapping submits.
- **Done when**: Trace spans show overlapping `gpu.sort.*` and downstream
  spans; wall-time reduction confirmed via bench.

## Phase 7 — Upstream AdaptiveCpp work

The `fork-safe-metal` branch at `4f3cde11a302eebac28aa1ccc79ad3399cb8183c`
is the ship pin. Recent fork-side fixes:
- `4f3cde11` (this round): `fix(metal-emitter): correct lowering of
  i128 add/sub/mul`. Metal SSCP was emitting lane-wise `uint4 *` for
  `mul i128`, which silently returned 0 for every soft-fp64 mantissa
  multiply. New `__acpp_i128_add/sub/mul` helpers in
  `emitEarlyFp64Helpers()` do proper schoolbook 128-bit arithmetic.
  Verified by `pgaccel-kernels/test/test_fp64_mul_probe.cpp`
  (32/32 OK bit-exact).

Items below are fork-local maintenance burden that eventually needs to
merge upstream (or rebase onto it). **Shipping 1.0 does not require
upstream merge** — the fork SHA pin is sufficient. Rebase + PR-upstream
work is tracked in the Post-1.0 (deferred) section.

### Metal Emitter performance/robustness polish

- **What**: Several Emitter items are correct-but-suboptimal after the
  Phase 1 fixes land:
  - **uint4 shift decomposition fast paths**: current code always emits a
    4-lane OR-of-shifts expression. For shift amounts that are multiples
    of 32, re-add lane-rearrangement special cases inside the new loop for
    MSL compiler efficiency (not correctness).
  - **Forward-declaration volume**: emitter emits forward decls for every
    non-kernel function, which bloats small kernels. Optimize: only emit
    forward decls for functions referenced from other functions' bodies
    (not the function that contains them).
  - **`optnone` performance cost on soft-fp64 bodies**: every
    `__acpp_sscp_soft_f64_*` / `sf64_*` function is marked `optnone` to
    prevent InstCombine pattern-matching that would create
    self-referential `llvm.copysign.f64` calls. Better alternative: find
    the specific InstCombine pass / sub-pass and disable only it via
    fine-grained attributes or a custom pre-InstCombine pass that adds
    `nobuiltin` to every call site. Unlocks the full O3 pipeline for
    soft-fp64 bodies.
  - **Third `ReplaceIntrinsics` pass explicit ordering**: the added third
    `ReplaceIntrinsics` pass runs after `AlwaysInlinerPass`. Verify no
    subsequent pass can re-introduce LLVM intrinsics. Could formalize with
    a loop: `while (ReplaceIntrinsics changed) { InstCombine; }` until
    fixpoint.
  - **Preservation-pass name-matching robustness**: matches on
    `__acpp_sscp_soft_f64_*`, `__acpp_sscp_*_f64`, `sf64_*` prefixes.
    Audit soft-fp64's `src/` for non-prefixed internal C++ names that get
    through clang's name mangling.
  - **NEW (2026-05-03 — surfaced by Agent K, host-port pattern):
    33-bit integer width unsupported.** `LLVMToMetal: MetalEmitter
    failed: Error: Unsupported integer bit width: 33` triggered by an
    LLVM-IR temporary from the `1u + ring_sum` accumulation pattern
    in `pgaccel_h3_grid_disk_output_size`. Worked around in pg_accel
    by porting the affected size-pass kernel to host code (commit
    `0319d93`). Upstream fix in `MetalEmitter` would let us revert to
    SYCL — likely needs to lower `i33` to `i64` at IR-rewrite time
    or block the temporary's promotion in the front-end.
  - **NEW (2026-05-03 — surfaced by Agent K, host-port pattern):
    PHI literal name without declaration.** Emitter generates
    `t_double__1_000000e_00__double__0_000000e_00_` for a
    `[2 x double] {1.0, 0.0}` PHI literal coming out of
    `sycl::cos(0)` / `sycl::sin(0)` in the inverse-gnomonic projection
    inside `pgaccel_h3_cell_to_boundary_emit`, but never emits the
    declaration. Worked around in pg_accel by host-porting the affected
    emit kernel (commit `0319d93`). Upstream fix would emit the literal
    declaration alongside the use; once landed, revert to SYCL.
  MINOR (now MAJOR for the two NEW items — they block shipping the
  affected kernels as SYCL on Metal until upstream-patched).
- **Why**: Each item incrementally reduces shader size / compile time /
  runtime cost. None blocks ship, all are worth landing for a clean
  upstream PR.
- **How**: Tackle one item per commit on `fork-safe-metal`; keep each
  rebaseable.
- Depends on: Phase 1 Emitter correctness fixes landed.
- **Done when**: Each item has a dedicated commit; shader size / compile
  time measured before and after.

### Metal SSCP soft-fp64 trig — RESOLVED 2026-05-03

The full chain landed:

1. **Templated emitter recursion** fixed earlier: split
   `sphere_distance_bulk_sycl<T>` and `st_length_bulk_sycl<T>` into
   non-templated `_f32` / `_f64` (mirrors `h3_ops.cpp:972-1019`).
2. **Helper OOM (exit 137)** fixed in AdaptiveCpp `fork-safe-metal`
   `348f6022`: `acpp-metal-archive-build` skips serialization when the
   metallib exceeds `ACPP_METAL_ARCHIVE_MAX_BYTES` (default 900 KiB),
   exiting 9 instead of being SIGKILL'd.
3. **Runtime consumes exit 9** in AdaptiveCpp `fork-safe-metal`
   `903442b0`: `metal_sscp_executable_object::build` distinguishes
   built / skipped / failed; `skipped` leaves `_archive == nullptr`
   and lets pipeline-state creation fall through to in-process compile
   (works on the parent backend; forked workers pay a one-time
   `MTLCompilerService` cost when they cold-dispatch this specific
   kernel, but no longer crash).
4. **Gates dropped** at `pgaccel-kernels/src/spatial_predicates.cpp:1027`
   (st_length) and `:1091` (sphere_distance). Both `use_fp64=true` paths
   now dispatch the `_f64` SYCL kernel.

Verified: `just gpu-test-cold spatial 600` → 109/109 passed (incl. the
previously-failing `test_sphere_distance_fp64` and the fp64 st_length
post-split assertions); `just gpu-test-cold fork` → PASS;
`just gpu-test-cold fork_cold` → PASS. `just check`, `just lint`,
`just audit-cpu-cheats` clean on `agent-b1-acpp-runtime`.

Residual caveat (acceptable, documented): forked workers cold-dispatching
sphere_distance/st_length fp64 incur one MTLCompilerService round-trip
the first time the kernel is hit. Raise `ACPP_METAL_ARCHIVE_MAX_BYTES`
(e.g. to 4194304) to opt back into pre-built archives if memory headroom
allows; the helper will stop skipping. Re-test on a larger-memory box
with archive builds enabled to see whether the original 17 MB .jit IR
OOM has hardware-budget headroom.

### `__args` MSL compile error on fused stats kernels at large N

- **What**: at N=1048576, `reduce_stats_f64`'s fused kernel fails
  `xcrun metal` with `cannot assign resource locations to '__args'`.
  Other reduce shapes at the same N compile fine. Surfaces from
  pg_accel as a kernel JIT failure, but the bug lives in
  `AdaptiveCpp/src/compiler/llvm-to-backend/metal/Emitter.cpp`.
- **Why**: kernels with more captures than `maxArgsForFlatMode`
  (default 6) take the argument-buffer path
  (`MetalEmitter::emitArgStruct` / `emitSignature`). The
  `[[id(N)]]` numbering inside the argbuffer struct likely collides
  with the implicit `[[threadgroup(0)]]` / dynamic-local-mem-size
  buffer bindings emitted at the top of every kernel.
- **How**: reproduce with `kernel_build_option::metal_max_args_for_flat_mode`
  bumped (so the argbuffer path is skipped) and again at default to
  diff the two emitted prototypes; fix the collision in
  `emitArgStruct` / `emitSignature`.
- **Status note (2026-05-02)**: The previously-tracked
  `pgaccel_expr_template_two_pred_and` instance of this bug class is
  closed. Agent 4A's f64-as-u64-bits capture refactor (commit
  `0c3d5d7`) made the kernel cold-cache MSL-compile, and Agent 1B's
  fusion (commit `9009ef1`) wired it as a single-kernel call from
  `scan/exec.rs` instead of the prior Rust-side double-dispatch.
  `reduce_stats_f64` at N=1048576 still has the underlying bug —
  separate instantiation, struct-pack hasn't been applied there.
- **Done when**: pg_accel's `reduce_stats_f64` MSL-compiles at every
  N; an AdaptiveCpp-side regression test asserts the prototype.

### Metal backend fork-safety with fp64 kernels

- **What**: Once Phase 1 Emitter gaps close, every fp64 kernel dispatched
  from a forked backend must cold-compile without crashing. The `.metalar`
  binary-archive compat with new `noinline`+`optnone`-attributed functions
  needs verification. MAJOR.
- **Why**: If the archive builder misses any preserved function, forked
  backends could fail pipeline-state creation — same class as the sort_kv
  crash in Phase 6.
- **How**:
  - Run `test_fork`, `test_fork_warmed`, `test_fork_cold` with each fp64
    kernel under stress after Phase 1 is green.
  - Confirm `.metalar` archives contain every preserved function — except
    metallibs the helper deliberately skipped via the
    `ACPP_METAL_ARCHIVE_MAX_BYTES` threshold (sphere_distance / st_length
    fp64 fall in this bucket post 2026-05-03; the runtime falls back
    cleanly to in-process pipeline compile on first dispatch per worker).
- Depends on: Phase 1 Emitter fixes, `just gpu-test` green.
- **Done when**: 8-worker × 20-iteration fork stress on fp64 kernels shows
  zero crashes and zero `MTLCompilerService` errors (fp64 sphere_distance
  + st_length already verified end-to-end on
  `agent-b1-acpp-runtime` against AdaptiveCpp `fork-safe-metal` SHA
  `903442b0`).

### Metal shader compile warnings under `-Wall` / `-Wextra`

- **What**: Current emitter uses `-Wno-unused-function` but doesn't pass
  `-Wall`. With `optnone` bodies carrying dead stores / redundant loads
  that the MSL compiler might flag as warnings, a `-Wall` sweep would
  surface Emitter-level polish work. MINOR.
- **Why**: Clean warnings matter for upstream submission.
- **How**: Enable `-Wall` in the MSL compile invocation; triage each
  warning class.
- **Done when**: Emitted MSL compiles clean under `-Wall`.

### soft-fp64 adapter coverage matrix

- **What**: The forwarder list in `acpp_metal_math.cpp` has dozens of
  entries (trig, exp, log, pow, erf, gamma, rounding, hypot, fmod, fract /
  frexp / modf / ldexp / ilogb, pown / rootn, classification predicates).
  MAJOR.
- **Why**: Preservation-pass bugs that escape the name-match rule silently
  break individual math forwarders; users see `NaN` or link failures at
  runtime.
- **How**:
  - Test each forwarder via a small SYCL kernel that calls it; confirm the
    body reaches MSL source.
  - Add a coverage matrix test to the AdaptiveCpp test suite.
- **Done when**: Every `__acpp_sscp_*_f64` forwarder has a
  positive-coverage test.

### SLEEF-based math precision validation

- **What**: Once every math forwarder dispatches, cross-check results
  against CPU soft-fp64 (and MPFR 200-bit oracle) at the ULP tolerances
  documented in soft-fp64 v1.0 (0 ULP bit-exact for arithmetic / compare,
  ≤4 ULP u10 transcendentals, ≤8 ULP u35 accumulations). MAJOR.
- **Why**: Gates pg_accel's fp64 matrix bench on bit-correctness.
- **How**:
  - Add ULP-diff tests driven by MPFR.
  - Gate `just bench fp64_matrix` on this suite passing.
- Depends on: soft-fp64 adapter coverage matrix (Phase 7).
- **Done when**: Every soft-fp64 math forwarder passes its ULP tolerance
  test.

### soft-fp64 polish items (non-blocking)

- **What**:
  - **fenv mode: expose `sf64_fe_*` host-side flag read-back.** Metal
    libkernel compiles with `-DSOFT_FP64_FENV_MODE=0` (disabled) so no
    TLS. Exceptions raised inside a GPU kernel are discarded. For parity
    with host soft-fp64's IEEE flag surface, consider a host-side
    `sf64_fe_*` API that reads back accumulated flags from a GPU-side
    buffer (not TLS). pg_accel doesn't need this today; soft-fp64 upstream
    may want it for other consumers.
  - **Kernel-signature ABI flags.** Check if `ACPP_METAL_FP64_EXPORT`
    needs any additional attributes for Metal-mode bitcode — e.g.
    `uniform` address-space hints, or `AS0` pointer types.
  MINOR.
- **Why**: Not blockers; relevant if other consumers of soft-fp64 emerge.
- **How**: File upstream issues; implement only when asked.
- **Done when**: Filed or explicitly deferred.

### Metal runtime pg_accel-facing polish

- **What**:
  - **Drop `ACPP_METAL_KEEP_SOURCE` env gate** once the debug story
    matures, or promote to a permanent `HIPSYCL_DEBUG_LEVEL=N` behaviour.
  - **`ACPP_METAL_DUMP_IR` env var**: introduced in the fork commit for
    dumping the Metal-flavored LLVM IR just before MetalEmitter runs. Keep
    as a permanent debug knob; consider gating by `HIPSYCL_DEBUG_LEVEL`
    instead of its own env.
  - **Metal `-fno-fast-math` semantics**: AdaptiveCpp's base class has a
    `setFastMathFunctionAttribs` call (`LLVMToBackend.cpp:411`) that the
    Metal backend inherits. Verify fp64 bodies get `fast=false` so
    soft-fp64 correctness isn't broken by contraction / reassociation.
  - **Metal buffer-argument indexing**: with arg-struct mode triggered
    above `maxArgsForFlatMode`, forwarders that chain through many args
    may hit the Metal 31-buffer limit. Not observed yet; flag for scale
    testing.
  MINOR.
- **Why**: Debug UX + scale-test resilience.
- **How**: Address one per commit; gate any env-var rename behind a
  deprecation warning.
- **Done when**: Debug env vars consolidated, fp64 fast-math verified,
  Metal buffer limit measured at scale.

### Cross-backend parity: CUDA / ROCm / L0

- **What**: The soft-fp64 integration targets Metal, but AdaptiveCpp's
  Metal-specific CMake + Emitter changes must not affect other backends.
  MAJOR (regression risk).
- **Why**: Shipping a Metal fix that silently corrupts CUDA results is a
  disaster.
- **How**: Run `test_reduce_stats` on a native-fp64 device (e.g. an NVIDIA
  or AMD box) and confirm bit-for-bit equivalence with pre-changes output.
- **Done when**: CUDA / ROCm / L0 CI (or a manual run on each) shows no
  regression vs the pre-fork-safe-metal baseline.

## Phase 8 — Build / toolchain polish

### AdaptiveCpp `default-targets` JSON generator drops list separators

- **What**: AdaptiveCpp's cmake template substitutes `${DEFAULT_TARGETS}`
  directly into a JSON string (`"default-targets" : "${DEFAULT_TARGETS}"`)
  at `AdaptiveCpp/CMakeLists.txt:672`; a CMake list value like `omp;metal`
  expands to `"ompmetal"` in `$HOME/local/etc/AdaptiveCpp/acpp-core.json`,
  which acpp then rejects with `Unknown backend: ompmetal`. Current
  workaround in pg_accel's Justfile passes `-DDEFAULT_TARGETS=generic`.
  MINOR (upstream AdaptiveCpp patch).
- **Why**: Any dev rebuilding AdaptiveCpp with a multi-backend config hits
  this; current fix works around it but the upstream bug is real.
- **How**: Patch AdaptiveCpp's template to use
  `string(REPLACE ";" "\";\"" ...)` before JSON substitution; upstream a
  fix so semicolon-lists serialize correctly.
- **Done when**: Upstream AdaptiveCpp accepts
  `-DDEFAULT_TARGETS=omp;metal` (or equivalent multi-target) without
  mis-escaping.

## Phase 9 — Verification matrix

Not yet run end-to-end; required gate before any "pg_accel accelerates all
of PG" claim. Each item here maps to one or more phases above — complete
those first, then the check here moves to PASS.

### EXPLAIN (VERBOSE) audit

- **What**: `EXPLAIN (VERBOSE)` must show `Gather` / `Gather Merge` with
  pg_accel CustomScan inside for: plain `SUM`, `AVG + STDDEV`, `GROUP BY`,
  `ORDER BY`, `ROW_NUMBER() OVER ...`, plain JOIN, JOIN + GROUP BY,
  `IncrementalSort`, `Append` over partitioned tables.
- **Why**: Without EXPLAIN confirmation, we don't know the planner is
  actually picking GPU paths.
- **How**: Build the query matrix in `pg_accel_bench`; assert CustomScan
  tag in the plan output for each.
- Depends on: Phases 3, 4 (parallel-path + operator coverage).
- **Done when**: Every query in the matrix shows CustomScan inside Gather /
  Gather Merge.

### Correctness diff sweep

- **What**: Correctness diff (pg_accel on vs off) — identical rows, float
  aggregates to fp tolerance — for every query in the EXPLAIN matrix
  above.
- **Why**: GPU-injected plans must be bit-correct vs native PG.
- **How**: Run each query twice (pg_accel off, on); diff result sets; fp
  aggregates within tolerance.
- Depends on: Phases 1, 2, 3.
- **Done when**: Every query diffs to zero (or within documented
  tolerance).

### Benchmark sweep

- **What**: `cargo run -p pg_accel_bench --release -- run --iterations 5
  --warmup 2` at 100K / 1M / 10M / 100M. Monotonic perf curve; no
  regressions vs PG parallel baseline.
- **Why**: Benchmark Rule #11 — no regression disguised as parity, no cell
  below PG parallel, no planner-decline-as-parity cheat.
- **How**: Run the bench; compare vs prior baseline; investigate any
  regression. For each cell, capture the `EXPLAIN VERBOSE` output AND the
  `pg_accel_stats()` counter delta around the query — both must confirm
  GPU dispatch.
- Depends on: Phases 1, 2, 3, 4, 5, 6.
- **Done when**: Every (workload, size) cell ≥ PG parallel **via Custom
  Scan selection**, verified by (a) `EXPLAIN VERBOSE` showing a
  `CustomScan` tag in the plan output and (b) `pg_accel_stats()`
  hook-injection counter incrementing across the query; no cell below
  baseline; no `min_batch_size` raised vs prior release; no monotonicity
  violations. Bench driver captures all three signals (wall time, EXPLAIN
  snippet, stats delta) per cell into a report artifact.

### 8-worker × 20-iteration fork stress

- **What**: 8-worker × 20-iteration fork stress on `bench_f32_10m` across
  `{SUM, AVG, STDDEV, grouped HashAgg, sort, window, hashjoin}`. Zero
  crashes, zero `MTLCompilerService` errors.
- **Why**: Validates fork-safety end-to-end.
- **How**: Build bench recipe; run; aggregate crash + XPC-error counts.
- Depends on: Phases 1, 6, 7.
- **Done when**: Zero crashes, zero XPC errors across the stress run.

### No silent kernel-failure Deferred paths

- **What**: Audit `grep -r "Deferred" pg_accel/src/` — every match must
  be an explicit *input-gate* deferral (caller passed an unsupported
  type / shape; planner declines, PG runs the scalar qual) with a
  comment explaining the gate. *Kernel-failure* Deferred (kernel
  dispatched, returned an error, executor maps to Deferred to silently
  drop the row or fall through) is banned.
- **Why**: Silent kernel-failure Deferred violates CLAUDE.md Critical
  Safety Rule #11 / anti-cheat ban #4 (no silent error swallowing on
  GPU paths). The 2026-05-02 Rust audit (see "CPU-cheat kernel
  conversions" section) confirmed all Rust `gpu/` and `executor/` GPU
  paths today either dispatch correctly or `pgrx::error!()` on kernel
  failure with a "refusing CPU fallback (rule 11)" message — no
  silent kernel-failure Deferred remains in the Rust tree. Re-run the
  audit after every kernel change.
- **How**: After kernel/bridge changes, re-run the audit grep and
  spot-check any new `Deferred` site for the input-gate-vs-failure
  distinction; require a one-line comment on each gate.
- **Done when**: Every `Deferred` in `pg_accel/src/` carries an
  input-gate justification comment; CI grep gate added so future
  kernel-failure Deferreds fail at PR time.

### `pg_accel_stats()` sanity

- **What**: After a workload run, `pg_accel_stats()` shows hook-injection
  count > skip-by-gate count; GPU failure counter == 0.
- **Why**: Cheap automated smoke test that the planner hooks fired and no
  kernels errored.
- **How**: Add to bench harness post-run assertion.
- **Done when**: Assertion passes after every bench sweep.

## Phase 10 — Release prep (1.0 tag)

### CLAUDE.md update

- **What**: Sync Skill Router, GUC table, critical safety rules with any
  changes landed during Phase 1–9. The `Effective Device Limits` section
  landed this round; other sections may drift.
- **Why**: Agents rely on CLAUDE.md; drift causes bad suggestions.
- **How**: Full pass after Phase 9 green; update GUC defaults, kernel
  lists, Skill Router entries. `just doc-parity` catches citation drift at
  commit time.
- **Done when**: CLAUDE.md reviewed top-to-bottom; every claim verifiable
  from the code.

### GitHub Actions / cross-platform CI workflow

- **What**: GHA workflow: `macos-14` arm64 runner executes full `just ci`
  (fmt, clippy, deny, audit, doc-parity, pgrx tests, gpu-test, bench
  smoke); `ubuntu-latest` x86_64 runner executes `build + check` without
  GPU tests; CUDA smoke tests gated on self-hosted-runner availability
  (not required for 1.0 green). BLOCKER.
- **Why**: The "clean `just ci`" ship bar is local-only today. Any agent
  or contributor can assert it passes without anyone else verifying.
  Cross-platform CI is the baseline gate that makes "just ci green"
  enforceable.
- **How**:
  - Add `.github/workflows/ci.yml` with two jobs:
    - `mac-arm64`: `runs-on: macos-14`, installs brew deps + AdaptiveCpp
      pin, runs `just ci`, uploads bench smoke artifact.
    - `linux-x86`: `runs-on: ubuntu-latest`, installs PG17 + Rust, runs
      `cargo check --all-features` and `cargo test` (skipping GPU tests
      via an env gate).
    - `cuda-smoke`: optional, `runs-on: [self-hosted, cuda]`, skipped if
      runner unavailable.
  - Wire branch-protection to require `mac-arm64` and `linux-x86` green.
  - Cache Rust target dir, AdaptiveCpp build, pgrx data dir between runs.
- **Done when**: `ci.yml` lives in `.github/workflows/`; a clean push to
  `main` runs both required jobs green; branch protection blocks merges on
  red.

### Extension SQL + control-file parity for 1.0.0

- **What**: Bump extension version from `0.1.0` → `1.0.0` in the control
  file, write the `pg_accel--0.1.0--1.0.0.sql` migration script, and
  verify `ALTER EXTENSION pg_accel UPDATE` works end-to-end against an
  installed 0.1.0. BLOCKER.
- **Why**: The current "smoke test" says `just package` but doesn't verify
  that installed users can upgrade in place. Shipping a breaking SQL
  schema without a migration script is a wire-protocol break for anyone
  on 0.1.0. Walk `git log -- pg_accel--0.1.0.sql` to enumerate the actual
  signature changes needing migration coverage; `pg_accel_stats()` was
  one such change in an earlier iteration but its `cpu_fallback_count`
  column was deleted before any 0.1.0 release shipped, so it does not
  need migration coverage today.
- **How**:
  - Update `pg_accel.control` with `default_version = '1.0.0'`.
  - Generate `pg_accel--1.0.0.sql` (fresh install schema) via `cargo pgrx
    schema`.
  - Hand-write `pg_accel--0.1.0--1.0.0.sql` migration covering every
    `CREATE FUNCTION` / `DROP FUNCTION` / signature change between the
    two versions. Walk `git log pg_accel--0.1.0.sql` for the diff.
  - Add a CI step: install the 0.1.0 `.so` + SQL into a throwaway
    cluster, create the extension, then install the 1.0.0 `.so` and run
    `ALTER EXTENSION pg_accel UPDATE`; assert no errors.
  - Add a matching downgrade path only if feasible (often isn't — that's
    OK, but document it).
- **Done when**: `ALTER EXTENSION pg_accel UPDATE` against a live 0.1.0
  cluster lands at `1.0.0` with no errors; fresh `CREATE EXTENSION
  pg_accel` against the 1.0.0 `.so` also succeeds; CI step runs both
  paths.

### CI infrastructure

- **What**: Ensure `just ci` is green on a fresh machine: fmt, clippy
  `-D warnings`, cargo deny, cargo audit, doc-parity, pgrx test suite,
  cargo check all-features, gpu-test.
- **Why**: The ship bar.
- **How**: Run `just ci` on a clean checkout; fix every failure.
  Cross-check via the GHA workflow.
- Depends on: Every prior phase; GitHub Actions workflow (Phase 10).
- **Done when**: `just ci` green locally AND on GHA with no warnings, no
  skips, no `#[ignore]`.

### Smoke test on fresh machine

- **What**: Clean clone → `just setup-gpu-acpp` → `just package` →
  install → `just bench`. No manual intervention, no missing-dep errors.
- **Why**: Catches environment assumptions we've been cheating on
  locally.
- **How**: Spin up a fresh M-series VM or reset `~/local`; run the
  sequence.
- Depends on: Phase 8 (Justfile / toolchain fixes); Extension SQL parity
  (Phase 10).
- **Done when**: Fresh-machine sequence runs clean end-to-end including
  `CREATE EXTENSION pg_accel` and a representative bench.

### Release checklist / pre-flight gate

- **What**: Consolidated checklist enumerating every Phase 1–10
  requirement before the tag is cut. BLOCKER (process).
- **Why**: Release discipline. Prevents accidentally shipping with a
  yellow Phase 2 or a skipped Phase 9 item.
- **How**: Maintain a checklist at `docs/release-checklist-1.0.md`
  mirroring the phase structure. Every item must be ticked with a commit
  SHA. The tag PR description pastes the checklist with each box ticked,
  linking to the commit SHA that closed it.
- Depends on: every prior phase.
- **Done when**: Every item in `docs/release-checklist-1.0.md` is ticked
  with a commit SHA; checklist pasted into the tag PR; maintainer sign-off
  in the PR body.

### Pre-1.0 tag

- **What**: Cut `v1.0.0-rc1` tag; if no critical bugs surface in 1 week,
  promote to `v1.0.0`.
- **Why**: Release discipline.
- **How**: Tag, push, announce, monitor bug tracker.
- Depends on: Release checklist (Phase 10).
- **Done when**: `v1.0.0` tag exists on `main`; release notes published;
  GHA release workflow uploads the `.so` + SQL artifacts.

### Decide on rand unsoundness advisories

- **What**: `cargo audit` surfaces two `RUSTSEC-2026-0097` `rand`
  unsoundness advisories (one via `pg_accel_bench`'s `rand 0.8.5`, one
  transitively via tokio-postgres / pgrx-tests / opentelemetry_sdk /
  proptest on `rand 0.9.2`). Currently unsuppressed — `just audit` emits
  them as warnings, exits 0. MINOR (CI noise, not a runtime issue for
  the pgrx-side dep chain).
- **Why**: Leaving unsuppressed surfaces the warning on every CI run;
  suppressing without reason erodes the signal.
- **How**: Decide per-advisory: either add to `deny.toml`
  `[advisories] ignore` list with a written justification (upstream
  tracking issue + wait-for version), OR bump the direct dependency past
  the affected range, OR wait for upstream bumps and re-check.
- **Done when**: Either both advisories are upstream-resolved, or both
  carry a `deny.toml` ignore entry with a justification comment.

## Post-1.0 (deferred)

Items explicitly descoped from the 1.0 ship bar. Tracked here so the audit
trail isn't broken; do not gate 1.0 on any of them.

### NUMERIC multi-limb accumulator kernel (Option A)

- **What**: The ship-now fix in Phase 2 routes NUMERIC columns through PG
  via a classification gate. The long-term fix is a custom multi-limb
  accumulator kernel that matches PG's NUMERIC on-disk representation.
- **Why deferred**: Correctness is already handled by the gate. Kernel is
  significant work with maintainability cost; premature for 1.0.
- **Expected trigger**: Demand from a user workload that can't afford the
  CPU-route penalty on NUMERIC aggregates.

### Integer / NUMERIC AVG variants

- **What**: `AVG(int2)` / `AVG(int4)` / `AVG(int8)` / `AVG(numeric)` /
  `AVG(interval)` parallel path. Current gate at
  `planner_hooks/partial_agg.rs:170-203` accepts only `FLOAT8ARRAYOID`
  transtype (AVG on float4/float8); the integer/numeric variants would
  need real `NumericAggState` / `PolyNumAggState` accumulators.
- **Why deferred**: Shares the multi-limb / INTERNAL-state accumulator
  work with "NUMERIC multi-limb accumulator kernel" above. Shipping
  float4/float8 AVG/STDDEV/VAR covers the common analytics cases.
- **Expected trigger**: Landed NUMERIC multi-limb work + user demand for
  integer-type AVG parallelism.

### Executor grouped partial emit path (Phase 3b)

- **What**: Per the "Grouped HashAgg parallel path" item, the executor
  must emit per-group *partial* transition states (AVG/STDDEV/VAR need
  `[N, sum, sum_sq]` per group; SUM/MIN/MAX/COUNT need raw accumulator per
  group). Route via GPU kernel output mode — NOT a CPU
  `HashMap<Box<[u8]>, Vec<ColumnAccumulator>>` scaffold, which would
  violate CLAUDE.md rule 11. ~200-400 line change across kernel + bridge +
  result accessor + emit path.
- **Why deferred**: Significant executor rework; 1.0 can ship with
  parallel plain-agg + float-stats AVG/STDDEV/VAR but leader-only grouped
  agg.
- **Expected trigger**: Real workload where the leader-only grouped agg is
  the measured bottleneck.

### Cascaded multi-key GPU sort

- **What**: Executor support for stable multi-key sort (sort by last key
  first, then by prior keys). `GPU_SORT_MAX_PATHKEYS=1` in
  `rel_pathlist.rs` is pinned to 1 by a regression test; bumping the bound
  without landing this is a regression because the executor bails on
  >1 pathkeys.
- **Why deferred**: Single-key GPU sort covers the common ORDER BY case.
  Multi-key + IncrementalSort opportunities are counted by
  `stats::increment_planner_rejected("sort_incremental_opportunity",…)`
  so priority can be data-driven.
- **Expected trigger**: Significant
  `sort_incremental_opportunity` counter hits in production traces, or
  explicit user demand for multi-key ORDER BY acceleration.

### GPU merge-join kernel + injection

- **What**: Parallel-friendly merge-join kernel for pre-sorted inputs.
  MergeJoin recognition in `join_pathlist.rs` today is detect-and-decline;
  `stats::increment_planner_rejected("mergejoin_no_gpu_kernel",…)`
  counts the opportunity.
- **Why deferred**: Kernel design + correctness test matrix + injection
  wiring is a multi-week undertaking. Hashjoin coverage is sufficient for
  most analytics.
- **Expected trigger**: Counter hits on real workloads, or specific
  query-plan classes where MergeJoin is strictly optimal and HashJoin
  regresses.

### GpuExpr+Scan for BitmapHeapScan

- **What**: Bitmap injection landed via `ba32a4a` using the
  `T_BitmapHeapPath`-wrapping approach. The alternative — emit a
  GpuExpr+Scan path with the bitmap predicate preserved — is deferred.
- **Why deferred**: Scope constraint on 1.0; the wrapping approach already
  lands the coverage win.
- **Expected trigger**: Measured cases where bitmap-predicate
  preservation outperforms the wrapping path.

### PG shared-hashtable integration for parallel GpuHashJoin

- **What**: Current scan-level GpuHashJoin partial path builds a
  per-worker hashtable (each worker rebuilds inner locally) because pgrx
  doesn't expose PG's `ParallelHashJoin` DSM APIs. Sharing the inner
  hashtable across workers would reduce memory and avoid redundant builds.
- **Why deferred**: FFI work on pgrx / PG internals; current per-worker
  model already delivers parallel speedup.
- **Expected trigger**: Benchmarks showing inner-build dominates
  hashjoin time on large inner relations.

### PostGIS predicate kernels beyond what dispatches today

- **What** (refreshed 2026-05-02 after 4 algorithmic predicates +
  st_distance polygonal landed): Wired end-to-end — st_intersects,
  st_dwithin (Point × Point), st_contains, st_within, st_disjoint,
  st_covers, st_coveredby, st_area (fp32 Polygon Shoelace),
  st_length (fp32 Polygon perimeter / LineString), st_distance
  (Point × Point fp32; Polygon × Polygon fp32 vertex-pair minimum),
  st_equals, st_touches, st_crosses, st_overlaps (all 4
  algorithmic, commits `b5e546a` + `2c08296` + `433bc21`). Still
  missing: per-row 3rd-arg `st_dwithin` thresholds (needs Phase 4
  multi-arg dispatch carrier), polygon × point and other mixed
  geometry distance combinations.
- **Why deferred**: Per-row 3rd-arg + mixed geometry types are
  carrier work + algorithmic; 1.0 ships with what's wired above.
- **Expected trigger**: PostGIS workload demand for non-constant
  `st_dwithin` thresholds or mixed-geometry distance calls.

### PostGIS raster — multi-arg dispatch carrier

- **What** (refreshed 2026-05-02 after Agent 3A landed 6 raster
  kernels and Agent 1B wired st_clip + st_reclass + st_summarystats):
  All 9 raster ops are registered + bridged + 4 dispatched today
  (st_mapalgebra, st_clip, st_reclass, st_summarystats). 5 awaiting
  the multi-arg dispatch carrier (st_resample, st_slope, st_aspect,
  st_hillshade, st_value) — see Phase 4 "Multi-arg dispatch carrier"
  for the unified plan.
- **Why deferred**: Multi-arg carrier is the unblocker (shared with
  st_dwithin and 3 H3 var-output ops); not a 1.0 ship-blocker.
- **Expected trigger**: Raster workload demand.

### H3 kernels beyond what dispatches today

- **What** (refreshed 2026-05-02 after Agent 5A landed 6 var-output
  kernels and Agent 1B wired 3): All 15 H3 ops are registered.
  Wired end-to-end (12): the 9 fixed-1:1-output ops plus
  grid_disk, grid_ring_unsafe, cell_to_children. 3 deferred
  (polyfill, cell_to_boundary, cells_to_multi_polygon) need the
  shared GSERIALIZED encoder + per-row polygon-vertex extractor —
  see Phase 4 "H3 operator registrations" entry.
- **Why deferred**: GSERIALIZED encoder + polygon extractor are
  shared with the PostGIS geometry constructors (st_buffer /
  st_union / st_intersection); not a 1.0 ship-blocker.
- **Expected trigger**: H3 workload demand for polygon-fill /
  boundary serialization paths.

### SetOp / RecursiveUnion GPU handling

- **What**: Tagged at `planner_hooks/mod.rs:3384-3385` but no GPU
  handling.
- **Why deferred**: Niche; no user demand surfaced. Low expected win.
- **Expected trigger**: Concrete user query where SetOp / RecursiveUnion
  is the bottleneck.

### AdaptiveCpp upstream rebase

- **What**: `fork-safe-metal` is based on `c86d474a` from 2026-04.
  Upstream has moved; periodic rebase needed. Blockers: upstream may have
  refactored `GlobalInliningAttributorPass` or `ReplaceIntrinsics` in
  ways that conflict with the diffs.
- **Why deferred**: 1.0 pins the fork SHA. Rebase is hygiene, not a ship
  blocker.
- **Expected trigger**: Upstream AdaptiveCpp ships a feature or fix
  pg_accel wants to pick up.

### AdaptiveCpp upstream PRs

- **What**: The struct-order fix (`emitEarlyFp64Helpers`), the arbitrary
  i128-shift support, the forward-decl emission, and the Emitter
  undef-placeholder fix are generally useful — not Metal-specific. Plus
  the HL-extraction phi-default fix once landed.
- **Why deferred**: 1.0 ships against the fork SHA. Upstreaming is a
  hygiene step that reduces long-term fork burden.
- **Expected trigger**: Post-1.0 maintenance cycle.

### soft-fp64 polish items (fenv read-back, ABI flags)

- **What**: See Phase 7 "soft-fp64 polish items (non-blocking)" —
  host-side `sf64_fe_*` flag read-back, `ACPP_METAL_FP64_EXPORT`
  attribute audit.
- **Why deferred**: pg_accel doesn't need either today; relevant only if
  other soft-fp64 consumers emerge.
- **Expected trigger**: External consumer request or documented need.

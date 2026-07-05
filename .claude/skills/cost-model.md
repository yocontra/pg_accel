---
name: Cost Model Guide
description: How pg_accel decides to inject a Custom Scan node, which GPU strategy to pick, and how DeviceLimits derives dispatch thresholds from hardware
---

# pg_accel Cost Model

GPU-only. No CPU fallback exists (see top-level CLAUDE.md rules 2, 11, 12). At runtime,
if `gpu_is_usable()` returns false the planner hooks return early and PG runs its native
plan untouched — that is a runtime no-op, not a CPU path.

## Source map

- `pg_accel/src/engine/cost/mod.rs` — re-exports the public cost API.
- `pg_accel/src/engine/cost/platform.rs` — `PlatformProfile::detect()` (GPU caps).
- `pg_accel/src/engine/cost/device_limits.rs` — `DeviceLimits` (hardware-derived thresholds).
- `pg_accel/src/engine/cost/formulas.rs` — `should_batch`, `should_use_gpu`, `self_scan_cost`, `optimal_batch_size`.
- `pg_accel/src/engine/cost/constants.rs` — per-strategy per-row cost constants.
- `pg_accel/src/engine/cost/availability.rs` — `gpu_hardware_available`, `gpu_is_usable`, `platform_has_fp64`.
- `pg_accel/src/engine/ffi/planner_hooks/mod.rs` — `install()`, `pgaccel_create_upper_paths`, `pgaccel_inject_gpu_agg`, `pgaccel_inject_gpu_window`, `pgaccel_inject_gpu_preagg`.
- `pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs` — `pgaccel_set_rel_pathlist` (scan-level gates), `try_inject_gpu_sort_path`.
- `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` — join-level injection.
- `pg_accel/src/engine/dispatch/mod.rs` — runtime `dispatch` entry after planning.

## Planner hooks installed

`install()` at `pg_accel/src/engine/ffi/planner_hooks/mod.rs:52` registers three hooks:

- `set_rel_pathlist_hook` → `pgaccel_set_rel_pathlist` (scan + scan-level sort).
- `set_join_pathlist_hook` → `pgaccel_set_join_pathlist` (GpuHashJoin).
- `create_upper_paths_hook` → `pgaccel_create_upper_paths` (agg, window, preagg).

Previous hooks are always chained first.

## Scan-level decision chain

`pgaccel_set_rel_pathlist` (`rel_pathlist.rs:42`) runs these gates in order:

1. Chain previous hook, record `stats::record_planner_hook_call()`.
2. `gucs::enabled()` — `pg_accel.enabled` master switch.
3. `parse.commandType == CMD_SELECT` — no INSERT/UPDATE/DELETE.
4. `cost::gpu_is_usable()` — GPU hardware + `pg_accel.gpu_enabled` GUC (`availability.rs:35`).
5. `reloptkind == RELOPT_BASEREL && rtekind == RTE_RELATION`.
6. Skip system catalog relids (< `FirstNormalObjectId`).
7. Early exit when no restriction clauses AND no `root.sort_pathkeys`.
8. If `sort_pathkeys` present: try `try_inject_gpu_sort_path`.
9. Early rows exit: `max(rel.tuples, rel.rows) < gucs::min_batch_size()`.
10. `try_gpu_expr_match` (standard numeric WHERE) + adapter-registry `find_accelerable_match`
    (spatial / H3 / raster). Registry match preferred.
11. Per-strategy `per_row_cost` from `cost::GPU_*_PER_ROW_COST` + `cost::should_batch`.
12. `GpuExpr`: `rows >= device_limits().gpu_expr_min_rows`.
13. `GpuRaster`: `rows >= device_limits().gpu_min_rows * 5`.
14. Spatial/Raster: defer to cheap GiST/SP-GiST index via `has_cheap_spatial_index_path`
    (selectivity < `SPATIAL_INDEX_SELECTIVITY_THRESHOLD` or cost ratio <
    `SPATIAL_INDEX_COST_RATIO_THRESHOLD` in `constants.rs`).
15. `GpuSpatial` vertex gate (`rel_pathlist.rs:269`):
    - `vertex_count >= gpu_spatial_min_vertices`.
    - `vertex_count * rows` in
      `[spatial_point_in_ring_break_even_verts_x_rows, spatial_point_in_ring_max_verts_x_rows]`.
    - Unknown vertex count → treat as 0, reject.
16. Baseline: `find_cheapest_seqscan_path` (not cheapest overall — Custom Scan always
    does a full heap scan). Null → index paths dominate, skip.
17. Build cost: `startup = base.startup + 1.0 + gpu_overhead`, `total = (base.total *
    cost_margin + batch_overhead + gpu_overhead) * gucs::cost_multiplier()`.
    - `gpu_overhead = GPU_LAUNCH_OVERHEAD` for Spatial/Raster/H3, `0.0` for `GpuExpr`
      (inline template, no kernel).
    - `cost_margin = GPU_COST_SAFETY_MARGIN * 0.5` for Spatial/Raster/H3, full
      `GPU_COST_SAFETY_MARGIN` (0.7) for `GpuExpr`.
18. Post-cost `has_cheaper_spatial_index_path(pathlist, total_cost)`.
19. `create_custom_path` + `add_path`. `add_path` handles final domination vs PG's
    parallel paths.

Note: Custom Scan runs single-threaded on the main backend. Baseline intentionally uses
the cheapest *non-parallel* seqscan (`find_cheapest_seqscan_path`) so the GPU batch
speedup isn't required to overcome PG's `serial_cost / n_workers` bookkeeping on paper.

## Upper-path decisions

`pgaccel_create_upper_paths` (`planner_hooks/mod.rs:80`) handles `UPPERREL_GROUP_AGG`:

1. `pgaccel_inject_gpu_preagg` — fused star-join + agg; gated by `gpu_preagg_min_fact_rows`,
   `gpu_preagg_max_dim_rows`, cost ratio `gpu_preagg_cost_ratio`.
2. `pgaccel_inject_gpu_agg` — reduce or hash-agg; gates on `gpu_reduce_min_rows`,
   `gpu_hash_agg_min_rows`, `gpu_hash_agg_max_groups`. Fusion with an upstream GpuScan
   requires `rows >= gpu_pipeline_fusion_min_rows`. Final cost ratio check against PG's
   cheapest non-parallel agg uses `gpu_agg_cost_ratio` (default 0.80).
3. `pgaccel_inject_gpu_window` — gated by `gpu_window_min_rows` and `gpu_window_cost_ratio`.

PG's serial baseline is computed via `find_cheapest_nonparallel_path` / `_total_cost`
(`planner_hooks/mod.rs:2191`, `:2264`), which strips top-level Gather/GatherMerge.

## DeviceLimits — hardware-derived thresholds

Defined in `pg_accel/src/engine/cost/device_limits.rs:15`. Access via the cached
`cost::device_limits()` OnceLock (`:450`). Populated on first call from
`PlatformProfile::detect()`; if `has_gpu == false`, uses the `cpu_only()` fallback
limits (no GPU paths will actually run, but keeps `device_limits()` infallible).

`from_profile` (`:177`) takes a `PlatformProfile { compute_units, gpu_max_alloc_bytes,
unified_memory, has_fp64, ... }` and scales via:

```
cu_scale(base) = (base * BASELINE_CUS / cus), halved on unified memory
BASELINE_CUS = 32  // Apple M2 Max reference
```

Unified memory halves most thresholds (no DMA copy). Memory-derived limits use
`gpu_max_alloc_bytes` directly.

### Row-count gates

- `gpu_min_rows`: generic GPU dispatch floor.
- `gpu_sort_min_rows`, `gpu_sort_planner_min_rows`: sort dispatch floor (planner tracks
  executor; earlier `* 10` mismatch removed, see comment at `:193`).
- `gpu_window_min_rows`, `gpu_reduce_min_rows`, `gpu_hash_agg_min_rows`.
- `gpu_pipeline_fusion_min_rows`: scan+agg fusion.
- `gpu_preagg_min_fact_rows`, `gpu_preagg_max_dim_rows`.
- `gpu_expr_min_rows`, `gpu_spatial_min_vertices`.
- `window_min_partition_rows`.

### Work-product gates

- `spatial_point_in_ring_break_even_verts_x_rows` (min, e.g. 10M).
- `spatial_point_in_ring_max_verts_x_rows` (max, e.g. 5e10 — megapoly reject).
- `expr_min_predicate_complexity_x_rows` (instructions × rows).

### Dispatch-chunking limits

- `gpu_reduce_max_chunk`: unified-memory uses 100M (chunking is pure loss on UMA);
  discrete uses `mem/32/8`.
- `gpu_sort_max_elements`: `mem/32/12`, clamped to ≤ 4M per chunk; executor k-way
  merges across chunks.
- `gpu_hash_agg_max_groups`: `mem/256/64`, clamp ≤ 100K.
- `gpu_hash_join_build_max_rows`, `gpu_join_max_output_rows`.
- `gpu_multi_key_sort_max_keys` (4).
- `optimal_batch_min` (256), `optimal_batch_max` (`mem/(8*1024)` clamped).
- `fused_interrupt_interval` (65_536 rows between `CHECK_FOR_INTERRUPTS`).

### Per-strategy GPU op cost (cost units / row)

`gpu_op_cost_reduce`, `gpu_op_cost_hash_agg`, `gpu_op_cost_sort`, `gpu_op_cost_window`,
`gpu_op_cost_filter`. All halved on unified memory.

### Cost-ratio gates vs PG's serial best

- `gpu_agg_cost_ratio` (default 0.80).
- `gpu_window_cost_ratio` (1.50).
- `gpu_preagg_cost_ratio` (1.50).

### Per-kernel break-even thresholds

- `reduce_f32_break_even_rows`, `reduce_f64_break_even_rows`, `reduce_i64_break_even_rows`.
- `hashagg_min_rows_per_group`, `hashagg_max_state_bytes_per_group`.
- `sort_break_even_rows_int`, `sort_break_even_rows_float`.
- `hashjoin_min_build_rows`.

### PreAgg per-row cost components

`preagg_dim_materialize_cost`, `preagg_fact_scan_cost`, `preagg_probe_cost`
(halved on UMA), `preagg_agg_cost`, `preagg_yield_cost`.

## Rule: no hardcoded thresholds

CLAUDE.md rule 10 is enforced. Any new dispatch threshold — row counts, chunk sizes,
work-product bounds, cost ratios — must be a field on `DeviceLimits`, populated in
`from_profile` from `PlatformProfile`, with a matching value in `cpu_only()`. Never
put magic numbers in planner/executor code. `gucs::min_batch_size()` is the one public
knob, intentionally exposed as a GUC for operator override.

## Rule: don't disable GPU via thresholds

CLAUDE.md rule says: on a regression, fix the kernel or dispatch bug. Do not raise
`gpu_*_min_rows` to 50M+ to hide a broken kernel. The floors exist to avoid launch
overhead on tiny inputs, not to paper over correctness or perf bugs in the GPU path.

## should_batch / should_use_gpu

`formulas.rs:13` — `should_batch(rows, per_row_cost, min_batch_size)`:
`rows >= min_batch_size && per_row_cost > 0.01`.

`formulas.rs:23` — `should_use_gpu(profile, rows, per_row_cost)`:
`profile.has_gpu && rows >= device_limits().gpu_min_rows && per_row_cost > 0.01`.

`self_scan_cost(rows, num_extract_cols, gpu_op_cost)` (`:40`) is the universal cost
for self-scanning nodes (agg/sort/window): `scan + extract + kernel_launch + per_row_gpu`.

## GUCs affecting cost decisions

From `pg_accel/src/engine/gucs.rs`:

| GUC | Default | Role |
|-----|---------|------|
| `pg_accel.enabled` | `true` | Master switch; gate 2. |
| `pg_accel.gpu_enabled` | `true` | Part of `gpu_is_usable()`; gate 4. |
| `pg_accel.min_batch_size` | `65536` | Scan-level batch floor; gate 9 + `should_batch`. |
| `pg_accel.cost_multiplier` | `1.0` | Multiplies final `total_cost`. >1 = more conservative. |
| `pg_accel.kernel_timeout_ms` | `5000` | Runtime warning threshold after synchronous GPU dispatch returns; not planner and not async cancellation. |
| `pg_accel.log_level` | `notice` | Tracing filter. |
| `pg_accel.max_workers_total` | `0` | Host-thread request budget; current executors do not spawn CPU worker threads. |
| `pg_accel.assert_dispatch` | `false` | Benchmark-only planner-decline warning guard. |
| `pg_accel.parallel_fused_count` | `false` | Roadmap knob; current PG18 parallel fused-count shape remains native with `parallel_fused_count_unstable`. |
| `pg_accel.otel_log_max_mb` | `256` | Trace JSONL rotation size cap, not costing. |
| `pg_accel.otel_log_max_rotations` | `4` | Trace JSONL rotation retention, not costing. |
| `pg_accel.fp64_enabled` | `true` | Local GUC in `src/lib.rs`; fp64 dispatch kill switch. |
| `pg_accel.soft_fp64_cost_multiplier` | `32.0` | Local GUC in `src/lib.rs`; per-row fp64 planner penalty on devices without native fp64. |

## Costing must be conservative

If the estimate is too low, PG picks our path when it shouldn't and the query gets
slower. `GPU_COST_SAFETY_MARGIN = 0.7` (`constants.rs:14`) is the margin applied to
the base seqscan total. `gucs::cost_multiplier()` is the operator dial for extra
conservatism.

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
- `pg_accel/src/engine/ffi/planner_hooks/mod.rs` — installs hooks, injects only `generic_groupagg`, and records window/SRF declines.
- `pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs` — observation-only base-scan, standalone-sort, and function declines.
- `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` — observation-only row-join declines; resident star joins are analyzed by the aggregate shape pass.
- `pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs` — resident aggregate admission and costing.

## Planner hooks installed

`install()` at `pg_accel/src/engine/ffi/planner_hooks/mod.rs:52` registers three hooks:

- `set_rel_pathlist_hook` → observe base scans, standalone sort/top-k, and functions; add no path.
- `set_join_pathlist_hook` → observe row-emitting joins; add no path.
- `create_upper_paths_hook` → consider `generic_groupagg` at `UPPERREL_GROUP_AGG`; record native window/SRF declines elsewhere.

Previous hooks are always chained first.

## Scan-level decision chain

`pgaccel_set_rel_pathlist` chains the previous hook, honors hook suspension and
`pg_accel.enabled`, requires a SELECT, and records planner-hook timing. With GPU
planning enabled it observes test-only raster eligibility, H3/PostGIS restriction
shapes, and standalone sort shapes. It records typed reasons plus
`no_gpu_resident_pipeline` and never calls `add_path`. Bounded top-k is not an
exception: it records `sort_standalone_topk_no_gpu_kernel` and stays native on
every backend.

## Upper-path decisions

At `UPPERREL_GROUP_AGG`, `generic_groupagg::try_inject` analyzes a reducing
`AggQuerySpec`, proves residency and semantics, computes device-aware cost, and
adds the sole normal-production pg_accel path when it wins. `UPPERREL_WINDOW`
records `no_gpu_resident_pipeline` and adds no path. Numeric `GpuSort` and
`GpuWindow` tags survive only in the private-data compatibility codec.

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
- `gpu_sort_*` and `gpu_window_*`: retained calibration/compatibility fields;
  they do not admit a standalone sort or window path.
- `gpu_reduce_min_rows`, `gpu_hash_agg_min_rows`.
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
- `gpu_sort_max_elements`: retained compatibility/calibration field; no standalone
  sort executor consumes it.
- `gpu_hash_agg_max_groups`: `mem/256/64`, clamp ≤ 100K.
- `gpu_hash_join_build_max_rows`, `gpu_join_max_output_rows`.
- `gpu_multi_key_sort_max_keys` (4).
- `optimal_batch_min` (256), `optimal_batch_max` (`mem/(8*1024)` clamped).
- `fused_interrupt_interval` (65_536 rows between `CHECK_FOR_INTERRUPTS`).

### Per-strategy GPU op cost (cost units / row)

`gpu_op_cost_reduce`, `gpu_op_cost_hash_agg`, `gpu_op_cost_sort`, `gpu_op_cost_window`,
`gpu_op_cost_filter`. All halved on unified memory.

### Resident first-use load cost

Generic resident aggregates cost a synchronous missing-relation load as a row
scan plus an estimated resident-byte term, amortized by
`auto_load_amortization_queries`:

- `resident_load_scan_per_row_cost` applies only when the complete selected,
  pinned, and already-resident column union is empty or catalog-proved
  fixed-width (built-in primitive or H3).
- `resident_load_per_byte_cost` applies to every estimated missing resident
  byte.
- Text, geometry, and raster loads retain the conservative
  `preagg_dim_materialize_cost` row term because their decoding and dictionary
  or domain construction work is not bounded by the compressed resident-byte
  footprint.

The early neutral shape lacks the exact type union and therefore uses the
conservative variable-width row term. Exact residency evidence replaces that
preliminary cost before production path admission. Resident-load coefficients
must not be substituted for PreAgg dimension hash-table construction costs.

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

`self_scan_cost(rows, num_extract_cols, gpu_op_cost)` remains a reusable formula;
production selection applies it only through currently reachable resident paths.

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
| `pg_accel.parallel_fused_count` | `false` | Roadmap knob; the current PG18/PG19 parallel fused-count shape remains native with `parallel_fused_count_unstable`. |
| `pg_accel.otel_log_max_mb` | `256` | Trace JSONL rotation size cap, not costing. |
| `pg_accel.otel_log_max_rotations` | `4` | Trace JSONL rotation retention, not costing. |
| `pg_accel.fp64_enabled` | `true` | Local GUC in `src/lib.rs`; fp64 dispatch kill switch. |
| `pg_accel.soft_fp64_cost_multiplier` | `32.0` | Local GUC in `src/lib.rs`; per-row fp64 planner penalty on devices without native fp64. |

## Costing must be conservative

If the estimate is too low, PG picks our path when it shouldn't and the query gets
slower. `GPU_COST_SAFETY_MARGIN = 0.7` (`constants.rs:14`) is the margin applied to
the base seqscan total. `gucs::cost_multiplier()` is the operator dial for extra
conservatism.

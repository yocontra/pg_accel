---
name: Spatial Predicate Kernel Guide
description: GPU spatial predicate kernels (point_in_ring, sphere_distance, segment_intersects, bbox_intersects) and the DEFINITE/UNCERTAIN result contract that defers ambiguous rows to PG's native PostGIS predicate
---

# Spatial Predicate Kernels

## Files

- `pgaccel-kernels/src/spatial_predicates.cpp` — templated fp32/fp64 kernels: `pgaccel_point_in_ring_bulk`, `pgaccel_sphere_distance_bulk`, `pgaccel_segment_intersects_bulk` (`spatial_predicates.cpp:200`, `:240`, `:293`).
- `pgaccel-kernels/src/bbox_ops.cpp` — SYCL bbox intersect over all (i,j) pairs: `pgaccel_bbox_intersects_bulk_f32`, `pgaccel_bbox_intersects_bulk_f64` (`bbox_ops.cpp:119`, `:149`). f64 variant returns `PGACCEL_UNSUPPORTED` on devices without `sycl::aspect::fp64`.
- `pgaccel-kernels/src/spatial_dispatch.cpp` — bulk point-in-polygon (inline bbox + PIP): `pgaccel_point_in_polygon_bulk` (`spatial_dispatch.cpp:688`), with two SYCL strategies `sycl_point_in_polygon_simple` (`:279`) and `sycl_point_in_polygon_coop` (`:363`).
- `pgaccel-kernels/include/pgaccel_ffi.h:216` — FFI result model and prototypes.
- `pg_accel/src/gpu/bridge.rs:54-127` — `extern "C"` declarations.
- `pg_accel/src/gpu/three_layer.rs` — `ExtractedGeometry`, `GeomType`, `SpatialPredicate`, `PredicateResult`, `spatial_eval` (`three_layer.rs:108`, `:131`, `:158`).
- `pg_accel/src/engine/dispatch/spatial.rs` — `GpuSpatial` strategy entry, bulk PIP fast path (`spatial.rs:42`).
- `pg_accel/src/engine/executor/scan/arena_scan.rs:211-263` — consumer that reads the `int8_t` result code and calls `cpu_recheck_tuple` for `UNCERTAIN` rows.

## Result Contract (FFI)

Defined at `pgaccel_ffi.h:216`:

```
 1 = DEFINITE TRUE   (inside / intersects / within distance)
-1 = DEFINITE FALSE  (outside / no-intersect / out of range)
 0 = UNCERTAIN       (caller must recheck via PG's native predicate)
```

For `pgaccel_sphere_distance_bulk`, the result is a `(distance, uncertain)` pair written into two parallel output buffers.

## UNCERTAIN = PG Recheck, Not CPU Fallback

There is no CPU SYCL kernel, no CPU fast-path kernel, and no CPU fallback build (rules #11, #12 in `CLAUDE.md`). "UNCERTAIN" means the GPU kernel declined to commit to a definite answer for that row. The caller materializes the row and lets PG evaluate the original qual (PostGIS C function) on it — see `arena_scan.rs:229-244` (`cpu_recheck_tuple`). This is PG's normal expression evaluator running the same operator that would have run without pg_accel. It is not a GPU→CPU port of the kernel.

Uncertainty-counting lives in `stats.rs:95` (`record_gpu_batch(rows, uncertain)`) and surfaces as `gpu_uncertain_count` in `pg_accel_stats()`.

## Correctness Rule

A false DEFINITE is a ship-blocking bug. An unnecessary UNCERTAIN is a perf miss. When in doubt, return 0.

UNCERTAIN is required when:
- Degenerate ring (< 4 vertices) or ring not closed within epsilon (`spatial_predicates.cpp:66-74`).
- Point within epsilon of any edge (`spatial_predicates.cpp:85-94`).
- Zero-length segment in `segment_intersects` (`:170-171`).
- Any cross-product magnitude below epsilon in `segment_intersects` (`:183-186`).
- NaN / Inf coordinate (checked in the bulk wrappers, e.g. `:217-220`).
- Antipodal pair in `sphere_distance` (`a > antipodal_thresh`, `:135`), sub-threshold distance (`:149`), or polar input.

## Dual Precision (fp32 / fp64)

Each kernel is a function template over `T ∈ {float, double}`. The bulk wrappers dispatch on the `bool use_fp64` argument — see `spatial_predicates.cpp:211` and the mirror `:251`, `:303`. Epsilons are tightened for fp64:

```
EPS_FP64                = 1e-12        (spatial_predicates.cpp:14)
EPS_FP32                = 1e-5         (spatial_predicates.cpp:15)
ANTIPODAL_COS_FP64      = 1 - 1e-10    (:17)
ANTIPODAL_COS_FP32      = 1 - 1e-4     (:18)
CLOSE_DIST_M_FP64       = 0.001  (1 mm) (:21)
CLOSE_DIST_M_FP32       = 1.0    (1 m)  (:22)
```

Backend selection is resolved by `device_has_fp64_cached()` in `pg_accel/src/gpu/mod.rs:117`. Metal reports `has_fp64 == false` and the Rust wrappers pick the fp32 kernel. CUDA / ROCm / Level Zero take the fp64 path. On fp32 the UNCERTAIN band is wider, which means PG runs the recheck on a higher fraction of rows — still a large net win because the GPU cleared the rest.

Bbox is exact in both paths because PostGIS `BOX2DF` is already float32; `pgaccel_bbox_intersects_bulk_f64` exists only for PG's native `box` type and explicitly refuses to run on non-fp64 devices (`bbox_ops.cpp:168-171`, `:176-178`).

## Testing

For each kernel, at every available platform:
1. Generate ≥ 100K random inputs covering degenerate cases (NaN, Inf, boundary-close, zero-length, antipodal, polar).
2. Run GPU → collect DEFINITE / UNCERTAIN.
3. Run PostGIS reference in PG as ground truth.
4. Assert: every DEFINITE_TRUE matches ground truth TRUE; every DEFINITE_FALSE matches FALSE.
5. Assert: DEFINITE results agree across platforms (CUDA fp64 answer == Metal fp32 answer when both say DEFINITE).
6. Log UNCERTAIN rate per platform — fp64 should be well under 0.5%, fp32 under a few percent. A spiking rate is a kernel regression, not a correctness failure.

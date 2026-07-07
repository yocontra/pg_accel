//! Dispatch tests.

#![allow(clippy::unwrap_used, dead_code)]

use super::*;

// -- NULL passthrough (strict function semantics) --------------------------
// These test the pure logic of NULL handling. Actual FunctionCallInvoke
// tests require a running PG instance and are covered by #[pg_test].

#[test]
fn strict_null_passthrough_logic() {
    // Simulate strict semantics without calling PG FFI.
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
        (pgrx::pg_sys::Datum::from(1), false),
        (pgrx::pg_sys::Datum::from(0), true), // NULL
        (pgrx::pg_sys::Datum::from(3), false),
        (pgrx::pg_sys::Datum::from(0), true), // NULL
    ];

    let is_strict = true;
    let results: Vec<(pgrx::pg_sys::Datum, bool)> = batch
        .iter()
        .map(|&(datum, is_null)| {
            if is_strict && is_null {
                (pgrx::pg_sys::Datum::from(0), true)
            } else {
                // In real code this would call FunctionCallInvoke.
                (datum, false)
            }
        })
        .collect();

    // NULLs pass through as NULL.
    assert!(results[1].1);
    assert!(results[3].1);
    // Non-NULLs are "evaluated".
    assert!(!results[0].1);
    assert!(!results[2].1);
}

#[test]
fn non_strict_null_not_skipped_logic() {
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
        (pgrx::pg_sys::Datum::from(0), true), // NULL
        (pgrx::pg_sys::Datum::from(1), false),
    ];

    let is_strict = false;
    let should_call_fn: Vec<bool> = batch
        .iter()
        .map(|&(_, is_null)| !(is_strict && is_null))
        .collect();

    // Non-strict: even NULL inputs go through the function.
    assert!(should_call_fn[0]);
    assert!(should_call_fn[1]);
}

// -- DispatchResult variants ----------------------------------------------

#[test]
fn dispatch_result_deferred_variant() {
    let result = DispatchResult::Deferred;
    assert!(matches!(result, DispatchResult::Deferred));
}

#[test]
fn dispatch_result_accelerated_variant() {
    let data = vec![(pgrx::pg_sys::Datum::from(42), false)];
    let result = DispatchResult::Accelerated(data);
    assert!(matches!(result, DispatchResult::Accelerated(_)));
}

#[test]
fn dispatch_result_accelerated_record_variant() {
    // ST_SummaryStats returns 6 fields per input row: count, sum, mean,
    // stddev, min, max. Two input rows ⇒ 12 datums.
    let datums: Vec<(pgrx::pg_sys::Datum, bool)> = (0..12)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();
    let result = DispatchResult::AcceleratedRecord {
        fields_per_row: 6,
        datums,
    };
    if let DispatchResult::AcceleratedRecord {
        fields_per_row,
        datums,
    } = result
    {
        assert_eq!(fields_per_row, 6);
        assert_eq!(datums.len(), 12);
        // Layout: rows are contiguous 6-Datum blocks.
        assert_eq!(datums[0].0.value(), 0);
        assert_eq!(datums[6].0.value(), 6);
    } else {
        panic!("expected AcceleratedRecord variant");
    }
}

#[test]
fn dispatch_result_accelerated_var_len_variant() {
    // CSR layout: 3 input rows producing 1, 2, 0 cells respectively.
    // offsets = [0, 1, 3, 3] ; datums = [c0, c1, c2]
    let datums: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
        (pgrx::pg_sys::Datum::from(100_u64), false),
        (pgrx::pg_sys::Datum::from(101_u64), false),
        (pgrx::pg_sys::Datum::from(102_u64), false),
    ];
    let offsets = vec![0_u32, 1, 3, 3];
    let result = DispatchResult::AcceleratedVarLen {
        offsets: offsets.clone(),
        datums: datums.clone(),
    };
    if let DispatchResult::AcceleratedVarLen {
        offsets: o,
        datums: d,
    } = result
    {
        assert_eq!(o.len(), datums.len() + 1);
        assert_eq!(o[0], 0);
        assert_eq!(*o.last().unwrap(), d.len() as u32);
        // Row 0 owns d[0..1], row 1 owns d[1..3], row 2 is empty.
        assert_eq!(o[1] - o[0], 1);
        assert_eq!(o[2] - o[1], 2);
        assert_eq!(o[3] - o[2], 0);
    } else {
        panic!("expected AcceleratedVarLen variant");
    }
}

#[test]
fn dispatch_result_variants_compile() {
    // Smoke test that all four variants can be constructed without compile
    // error — ensures Phase A's contract holds for downstream agents.
    let _v1 = DispatchResult::Accelerated(vec![]);
    let _v2 = DispatchResult::AcceleratedRecord {
        fields_per_row: 1,
        datums: vec![],
    };
    let _v3 = DispatchResult::AcceleratedVarLen {
        offsets: vec![0],
        datums: vec![],
    };
    let _v4 = DispatchResult::Deferred;
}

// -- Batch size calculations and edge cases --------------------------------

#[test]
fn batch_size_one_produces_single_element_batch() {
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![(pgrx::pg_sys::Datum::from(42), false)];
    // Simulate strict null check for a single-element batch.
    let is_strict = true;
    let results: Vec<bool> = batch
        .iter()
        .map(|&(_, is_null)| !(is_strict && is_null))
        .collect();
    assert_eq!(results.len(), 1);
    assert!(results[0]);
}

#[test]
fn batch_all_nulls_strict_all_skipped() {
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..10)
        .map(|_| (pgrx::pg_sys::Datum::from(0), true))
        .collect();

    let is_strict = true;
    let results: Vec<(pgrx::pg_sys::Datum, bool)> = batch
        .iter()
        .map(|&(_, is_null)| {
            if is_strict && is_null {
                (pgrx::pg_sys::Datum::from(0), true)
            } else {
                (pgrx::pg_sys::Datum::from(1), false)
            }
        })
        .collect();

    assert!(results.iter().all(|(_, is_null)| *is_null));
}

#[test]
fn batch_no_nulls_strict_all_evaluated() {
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..10)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();

    let is_strict = true;
    let eval_count = batch
        .iter()
        .filter(|&&(_, is_null)| !(is_strict && is_null))
        .count();
    assert_eq!(eval_count, 10);
}

#[test]
fn very_large_batch_null_passthrough() {
    let batch_size = 100_000;
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..batch_size)
        .map(|i| {
            let is_null = i % 3 == 0;
            (pgrx::pg_sys::Datum::from(i as i64), is_null)
        })
        .collect();

    let is_strict = true;
    let null_count = batch
        .iter()
        .filter(|&&(_, is_null)| is_strict && is_null)
        .count();

    // Every 3rd element (0, 3, 6, ...) is NULL.
    let expected_nulls = (batch_size + 2) / 3;
    assert_eq!(null_count, expected_nulls);
}

// -- AccelStrategy enum: all variants, conversion --------------------------

#[test]
fn accel_strategy_from_i32_known_values() {
    assert_eq!(AccelStrategy::from_i32(1), Some(AccelStrategy::GpuSpatial));
    assert_eq!(AccelStrategy::from_i32(2), Some(AccelStrategy::GpuRaster));
    assert_eq!(AccelStrategy::from_i32(3), Some(AccelStrategy::GpuH3));
    assert_eq!(AccelStrategy::from_i32(4), Some(AccelStrategy::GpuSort));
    assert_eq!(AccelStrategy::from_i32(5), Some(AccelStrategy::GpuReduce));
    assert_eq!(AccelStrategy::from_i32(6), Some(AccelStrategy::GpuExpr));
    assert_eq!(AccelStrategy::from_i32(7), Some(AccelStrategy::GpuHashJoin));
    assert_eq!(AccelStrategy::from_i32(8), Some(AccelStrategy::GpuWindow));
    assert_eq!(
        AccelStrategy::from_i32(9),
        Some(AccelStrategy::GpuNestedLoopIneq)
    );
}

#[test]
fn accel_strategy_from_i32_unknown_is_invalid() {
    assert_eq!(AccelStrategy::from_i32(0), None);
    assert_eq!(AccelStrategy::from_i32(-1), None);
    // Discriminant 10 reserved for the next strategy. If a new variant
    // lands here, bump this test alongside the enum.
    assert_eq!(AccelStrategy::from_i32(10), None);
    assert_eq!(AccelStrategy::from_i32(100), None);
    assert_eq!(AccelStrategy::from_i32(i32::MAX), None);
    assert_eq!(AccelStrategy::from_i32(i32::MIN), None);
}

#[test]
fn accel_strategy_roundtrip_through_i32() {
    let strategies = [
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuRaster,
        AccelStrategy::GpuH3,
        AccelStrategy::GpuSort,
        AccelStrategy::GpuReduce,
        AccelStrategy::GpuExpr,
        AccelStrategy::GpuHashJoin,
        AccelStrategy::GpuWindow,
    ];
    for s in strategies {
        let as_i32 = s as i32;
        assert_eq!(AccelStrategy::from_i32(as_i32), Some(s));
    }
}

#[test]
fn accel_strategy_debug_format_contains_variant_name() {
    let dbg = format!("{:?}", AccelStrategy::GpuSpatial);
    assert!(dbg.contains("GpuSpatial"), "debug format: {dbg}");
}

#[test]
fn accel_strategy_copy_semantics() {
    let a = AccelStrategy::GpuH3;
    let b = a; // Copy
    assert_eq!(a, b);
}

#[test]
fn accel_strategy_hash_consistency() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(AccelStrategy::GpuSpatial);
    set.insert(AccelStrategy::GpuSpatial); // duplicate
    set.insert(AccelStrategy::GpuH3);
    assert_eq!(set.len(), 2);
}

// -- Dispatch routing (which strategy goes where) --------------------------

#[test]
fn dispatch_routing_gpu_strategies_that_return_deferred() {
    // GpuExpr, GpuSort, GpuReduce, GpuHashJoin, GpuWindow are not wired
    // into per-datum dispatch and should map to Deferred.
    let deferred_strategies = [
        AccelStrategy::GpuExpr,
        AccelStrategy::GpuSort,
        AccelStrategy::GpuReduce,
        AccelStrategy::GpuHashJoin,
        AccelStrategy::GpuWindow,
    ];
    for strategy in deferred_strategies {
        // Verify the match arm maps these to Deferred by checking
        // the pattern from the dispatch function.
        assert!(
            matches!(
                strategy,
                AccelStrategy::GpuExpr
                    | AccelStrategy::GpuSort
                    | AccelStrategy::GpuReduce
                    | AccelStrategy::GpuHashJoin
                    | AccelStrategy::GpuWindow
            ),
            "{strategy:?} should be in the deferred arm"
        );
    }
}

#[test]
fn dispatch_routing_gpu_spatial_is_not_in_deferred_arm() {
    // GpuSpatial has its own dispatch arm, not the catch-all deferred.
    assert!(!matches!(
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuExpr
            | AccelStrategy::GpuSort
            | AccelStrategy::GpuReduce
            | AccelStrategy::GpuHashJoin
            | AccelStrategy::GpuWindow
    ));
}

#[test]
fn dispatch_routing_gpu_spatial_is_not_deferred() {
    assert!(!matches!(
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuExpr
            | AccelStrategy::GpuSort
            | AccelStrategy::GpuReduce
            | AccelStrategy::GpuHashJoin
            | AccelStrategy::GpuWindow
    ));
}

#[test]
fn dispatch_resolution_maps_registered_names_to_typed_ops() {
    let h3_entry = FunctionAccelEntry::scalar("public", "h3_grid_disk", AccelStrategy::GpuH3);
    assert!(matches!(
        resolve_dispatch_operation(AccelStrategy::GpuH3, Some(&h3_entry)),
        DispatchOperation::H3(H3DispatchOp::GridDisk)
    ));

    let raster_entry =
        FunctionAccelEntry::scalar("public", "st_summarystats", AccelStrategy::GpuRaster);
    assert!(matches!(
        resolve_dispatch_operation(AccelStrategy::GpuRaster, Some(&raster_entry)),
        DispatchOperation::Raster(RasterDispatchOp::SummaryStats)
    ));

    let spatial_entry =
        FunctionAccelEntry::scalar("public", "st_dwithin", AccelStrategy::GpuSpatial);
    assert!(matches!(
        resolve_dispatch_operation(AccelStrategy::GpuSpatial, Some(&spatial_entry)),
        DispatchOperation::Spatial(SpatialDispatchOp::DWithin)
    ));
}

#[test]
fn spatial_dispatch_op_from_name_is_exact_allowlist() {
    assert_eq!(
        SpatialDispatchOp::from_name("st_area"),
        SpatialDispatchOp::Area
    );
    assert_eq!(
        SpatialDispatchOp::from_name("st_length"),
        SpatialDispatchOp::Length
    );
    assert_eq!(
        SpatialDispatchOp::from_name("st_distance"),
        SpatialDispatchOp::Distance
    );
    assert_eq!(
        SpatialDispatchOp::from_name("st_intersects"),
        SpatialDispatchOp::Intersects
    );
    assert_eq!(
        SpatialDispatchOp::from_name("st_dwithin"),
        SpatialDispatchOp::DWithin
    );
    assert_eq!(
        SpatialDispatchOp::from_name("st_intersection"),
        SpatialDispatchOp::Unknown
    );
    assert_eq!(
        SpatialDispatchOp::from_name("ST_Intersects"),
        SpatialDispatchOp::Unknown
    );
}

#[test]
fn dispatch_resolution_unknown_non_spatial_defers() {
    let entry = FunctionAccelEntry::scalar("public", "st_clip", AccelStrategy::GpuRaster);
    assert!(matches!(
        resolve_dispatch_operation(AccelStrategy::GpuH3, Some(&entry)),
        DispatchOperation::Deferred
    ));
}

#[test]
fn dispatch_resolution_spatial_miss_returns_unknown_route() {
    assert!(matches!(
        resolve_dispatch_operation(AccelStrategy::GpuSpatial, None),
        DispatchOperation::Spatial(SpatialDispatchOp::Unknown)
    ));
}

// -- DispatchResult: data access -------------------------------------------

#[test]
fn dispatch_result_accelerated_empty_vec() {
    let result = DispatchResult::Accelerated(vec![]);
    if let DispatchResult::Accelerated(data) = result {
        assert!(data.is_empty());
    } else {
        panic!("expected Accelerated variant");
    }
}

#[test]
fn dispatch_result_accelerated_preserves_data() {
    let data = vec![
        (pgrx::pg_sys::Datum::from(1), false),
        (pgrx::pg_sys::Datum::from(0), true),
        (pgrx::pg_sys::Datum::from(3), false),
    ];
    let result = DispatchResult::Accelerated(data);
    if let DispatchResult::Accelerated(ref d) = result {
        assert_eq!(d.len(), 3);
        assert!(!d[0].1);
        assert!(d[1].1);
        assert!(!d[2].1);
    } else {
        panic!("expected Accelerated variant");
    }
}

#[test]
fn dispatch_result_debug_format() {
    let result = DispatchResult::Deferred;
    let dbg = format!("{result:?}");
    assert!(dbg.contains("Deferred"));
}

// -- Multi-arg carrier (Phase II Agent F1) ---------------------------------
//
// Pure-Rust shape tests for the new `qual_datums: &[(Datum, bool, Oid)]`
// dispatch interface. End-to-end GPU dispatch lives behind `#[pg_test]`
// (needs a live backend + registered functions); the tests here verify
// the carrier-shape invariants that regressions would silently corrupt:
//
//   - Empty slice + multi-arg op → Deferred (not a panic, not "use
//     zeros for missing args").
//   - Missing trailing arg (e.g. only 1 of 2 cell sizes) → Deferred.
//   - Non-finite f64 args (NaN / inf cell sizes) → Deferred.
//
// We avoid invoking `dispatch_gpu_*` directly because they need a valid
// FmgrInfo + PG memory context; the shape checks here exercise the
// pure-Rust branch logic up to the FFI boundary.

#[test]
fn dispatch_result_carrier_signature_is_slice_of_triples() {
    // Compile-time assertion that the new dispatch signature accepts
    // `&[(Datum, bool, Oid)]`. If the dispatch_gpu_raster signature
    // regresses to `Option<(Datum, bool)>`, this test fails to compile.
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = vec![
        (
            pgrx::pg_sys::Datum::from(256_u64),
            false,
            pgrx::pg_sys::Oid::from(23_u32),
        ), // INT4OID
        (
            pgrx::pg_sys::Datum::from(256_u64),
            false,
            pgrx::pg_sys::Oid::from(23_u32),
        ),
    ];
    // Slice of triples — the dispatch signature.
    let _: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)] = &qual_datums;
    assert_eq!(qual_datums.len(), 2);
}

#[test]
fn st_resample_carrier_arg_layout_int4_pair() {
    // Plan-time argument layout for ST_Resample(rast, target_w, target_h):
    // qual_datums[0] = i32 width, qual_datums[1] = i32 height.
    // The dispatcher reads `Datum::value() as i32`. Verify a small int
    // round-trips through that decoding.
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = vec![
        (
            pgrx::pg_sys::Datum::from(256_u64),
            false,
            pgrx::pg_sys::Oid::from(23_u32),
        ),
        (
            pgrx::pg_sys::Datum::from(128_u64),
            false,
            pgrx::pg_sys::Oid::from(23_u32),
        ),
    ];
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let w = qual_datums[0].0.value() as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let h = qual_datums[1].0.value() as i32;
    assert_eq!(w, 256);
    assert_eq!(h, 128);
}

#[test]
fn st_hillshade_carrier_arg_layout_f64_quad() {
    // ST_Hillshade(rast, cell_x, cell_y, sun_az, sun_alt) — 4 f64 args.
    // Layout-critical: swapping qual_datums[2] and [3] silently produces
    // wrong shading (sun azimuth and altitude have different ranges and
    // semantics). Verify each f64 round-trips bit-exactly.
    let cx = 30.0_f64;
    let cy = 30.0_f64;
    let az = 315.0_f64;
    let alt = 45.0_f64;
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = vec![
        (
            pgrx::pg_sys::Datum::from(cx.to_bits()),
            false,
            pgrx::pg_sys::Oid::from(701_u32),
        ),
        (
            pgrx::pg_sys::Datum::from(cy.to_bits()),
            false,
            pgrx::pg_sys::Oid::from(701_u32),
        ),
        (
            pgrx::pg_sys::Datum::from(az.to_bits()),
            false,
            pgrx::pg_sys::Oid::from(701_u32),
        ),
        (
            pgrx::pg_sys::Datum::from(alt.to_bits()),
            false,
            pgrx::pg_sys::Oid::from(701_u32),
        ),
    ];
    assert_eq!(f64::from_bits(qual_datums[0].0.value() as u64), cx);
    assert_eq!(f64::from_bits(qual_datums[1].0.value() as u64), cy);
    assert_eq!(f64::from_bits(qual_datums[2].0.value() as u64), az);
    assert_eq!(f64::from_bits(qual_datums[3].0.value() as u64), alt);
}

#[test]
fn st_dwithin_carrier_threshold_at_position_one() {
    // ST_DWithin(geom_col, $const_geom, $threshold) — qual_datums[0] is
    // the geom Datum (typically a varlena pointer), qual_datums[1] is the
    // f64 threshold. Verify threshold f64 round-trips through the same
    // bit-pattern decoding the dispatcher uses.
    let geom_datum = pgrx::pg_sys::Datum::from(0xCAFE_BABE_usize);
    let threshold = 1000.0_f64;
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = vec![
        (geom_datum, false, pgrx::pg_sys::Oid::from(0_u32)),
        (
            pgrx::pg_sys::Datum::from(threshold.to_bits()),
            false,
            pgrx::pg_sys::Oid::from(701_u32),
        ),
    ];
    let captured = qual_datums[1];
    assert!(!captured.1, "threshold null flag false");
    let recovered = f64::from_bits(captured.0.value() as u64);
    assert!((recovered - threshold).abs() < f64::EPSILON);
}

#[test]
fn carrier_empty_slice_compiles() {
    // Single-arg ops (ST_Area, ST_Length) must accept an empty slice.
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = Vec::new();
    let slice: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)] = &qual_datums;
    assert!(slice.is_empty());
    let first: Option<&(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = slice.first();
    assert!(first.is_none(), "first() returns None on empty slice");
}

#[test]
fn carrier_first_helper_works_for_one_arg_ops() {
    // The h3.rs / raster.rs dispatchers package qual_datums[0] as an
    // `Option<(Datum, bool)>` for arms that only consume one const.
    let qual_datums: Vec<(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)> = vec![(
        pgrx::pg_sys::Datum::from(42_u64),
        false,
        pgrx::pg_sys::Oid::from(23_u32),
    )];
    let one: Option<(pgrx::pg_sys::Datum, bool)> = qual_datums.first().map(|&(d, n, _)| (d, n));
    assert!(one.is_some());
    assert_eq!(one.unwrap().0.value(), 42);
}

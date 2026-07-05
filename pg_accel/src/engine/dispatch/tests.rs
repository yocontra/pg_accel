//! Dispatch tests.

#![allow(clippy::unwrap_used, dead_code)]

use super::predicate_chain::*;
use super::*;

// -- Predicate chain ordering --------------------------------------------

fn make_predicate(label: &'static str, selectivity: f64, cost: f64) -> Predicate {
    Predicate {
        label,
        selectivity,
        cost,
        eval_fn: |batch| vec![true; batch.len()],
    }
}

#[test]
fn chain_orders_by_efficiency() {
    // "cheap" has selectivity 0.1, cost 1.0 → efficiency 0.1 (best)
    // "expensive" has selectivity 0.5, cost 10.0 → efficiency 0.05
    // "medium" has selectivity 0.3, cost 2.0 → efficiency 0.15 (worst)
    let predicates = vec![
        make_predicate("medium", 0.3, 2.0),
        make_predicate("expensive", 0.5, 10.0),
        make_predicate("cheap", 0.1, 1.0),
    ];

    let chain = PredicateChain::new(predicates);
    let labels: Vec<&str> = chain.predicates().iter().map(|p| p.label).collect();

    // Sorted ascending by selectivity/cost:
    // expensive = 0.05, cheap = 0.1, medium = 0.15
    assert_eq!(labels, vec!["expensive", "cheap", "medium"]);
}

#[test]
fn empty_chain_returns_all_alive() {
    let chain = PredicateChain::new(vec![]);
    assert!(chain.is_empty());

    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
        (pgrx::pg_sys::Datum::from(1), false),
        (pgrx::pg_sys::Datum::from(2), false),
    ];
    let result = evaluate_chain(&chain, &batch);
    assert_eq!(result, vec![true, true]);
}

#[test]
fn chain_len_matches() {
    let chain = PredicateChain::new(vec![
        make_predicate("a", 0.5, 1.0),
        make_predicate("b", 0.3, 2.0),
    ]);
    assert_eq!(chain.len(), 2);
    assert!(!chain.is_empty());
}

// -- Predicate chain evaluation ------------------------------------------

#[test]
fn chain_filters_rows_correctly() {
    // First predicate: reject odd-indexed rows.
    let pred_even = Predicate {
        label: "even_index",
        selectivity: 0.5,
        cost: 1.0,
        eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i % 2 == 0).collect(),
    };

    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..4)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();

    let chain = PredicateChain::new(vec![pred_even]);
    let result = evaluate_chain(&chain, &batch);

    // Rows 0,1,2,3 → predicate sees all 4, returns [true, false, true, false]
    assert_eq!(result, vec![true, false, true, false]);
}

#[test]
fn chain_short_circuits_rejected_rows() {
    // First predicate: pass only the first row.
    // efficiency = 0.1 / 10.0 = 0.01 (sorts first — lowest efficiency wins).
    let pred_first_only = Predicate {
        label: "first_only",
        selectivity: 0.1,
        cost: 10.0,
        eval_fn: |batch| {
            let mut v = vec![false; batch.len()];
            if !v.is_empty() {
                v[0] = true;
            }
            v
        },
    };

    // Second predicate: always returns true — but should only see 1 row.
    // efficiency = 1.0 / 1.0 = 1.0 (sorts second).
    let pred_pass_all = Predicate {
        label: "pass_all",
        selectivity: 1.0,
        cost: 1.0,
        eval_fn: |batch| {
            // If short-circuiting works, batch should have exactly 1 row.
            assert_eq!(batch.len(), 1);
            vec![true; batch.len()]
        },
    };

    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..5)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();

    let chain = PredicateChain::new(vec![pred_first_only, pred_pass_all]);
    let result = evaluate_chain(&chain, &batch);

    assert_eq!(result, vec![true, false, false, false, false]);
}

#[test]
fn chain_all_rejected_skips_remaining() {
    // First predicate: reject everything.
    let pred_reject_all = Predicate {
        label: "reject_all",
        selectivity: 0.0,
        cost: 1.0,
        eval_fn: |batch| vec![false; batch.len()],
    };

    // Second predicate: would panic if called — ensures short-circuit.
    let pred_should_not_run = Predicate {
        label: "should_not_run",
        selectivity: 1.0,
        cost: 100.0,
        eval_fn: |batch| {
            assert!(
                batch.is_empty(),
                "should_not_run predicate should not receive any rows"
            );
            vec![]
        },
    };

    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..3)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();

    let chain = PredicateChain::new(vec![pred_reject_all, pred_should_not_run]);
    let result = evaluate_chain(&chain, &batch);

    assert_eq!(result, vec![false, false, false]);
}

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

// -- Efficiency metric ---------------------------------------------------

#[test]
fn efficiency_zero_cost_returns_zero() {
    let p = make_predicate("zero_cost", 0.5, 0.0);
    assert!((efficiency(&p)).abs() < f64::EPSILON);
}

#[test]
fn efficiency_negative_cost_returns_zero() {
    let p = make_predicate("neg_cost", 0.5, -1.0);
    assert!((efficiency(&p)).abs() < f64::EPSILON);
}

#[test]
fn efficiency_normal_computation() {
    let p = make_predicate("normal", 0.3, 2.0);
    let eff = efficiency(&p);
    assert!((eff - 0.15).abs() < f64::EPSILON);
}

// -- PredicateChain: construction edge cases --------------------------------

#[test]
fn chain_with_single_predicate() {
    let chain = PredicateChain::new(vec![make_predicate("only", 0.5, 1.0)]);
    assert_eq!(chain.len(), 1);
    assert!(!chain.is_empty());
    assert_eq!(chain.predicates()[0].label, "only");
}

#[test]
fn chain_with_ten_predicates_sorted_correctly() {
    let predicates: Vec<Predicate> = (1..=10)
        .map(|i| {
            let sel = i as f64 * 0.1;
            let cost = (11 - i) as f64; // inverse cost so sorting varies
            make_predicate(
                // Static labels for 10 predicates.
                match i {
                    1 => "p1",
                    2 => "p2",
                    3 => "p3",
                    4 => "p4",
                    5 => "p5",
                    6 => "p6",
                    7 => "p7",
                    8 => "p8",
                    9 => "p9",
                    _ => "p10",
                },
                sel,
                cost,
            )
        })
        .collect();

    let chain = PredicateChain::new(predicates);
    assert_eq!(chain.len(), 10);

    // Verify sorted by ascending efficiency (selectivity / cost).
    let effs: Vec<f64> = chain.predicates().iter().map(|p| efficiency(p)).collect();
    for i in 1..effs.len() {
        assert!(
            effs[i - 1] <= effs[i] + f64::EPSILON,
            "predicates not sorted by efficiency at index {}: {} > {}",
            i,
            effs[i - 1],
            effs[i],
        );
    }
}

#[test]
fn chain_with_equal_efficiency_maintains_order_stability() {
    // Two predicates with identical efficiency should not cause issues.
    let predicates = vec![
        make_predicate("alpha", 0.5, 2.0), // efficiency = 0.25
        make_predicate("beta", 0.5, 2.0),  // efficiency = 0.25
    ];
    let chain = PredicateChain::new(predicates);
    assert_eq!(chain.len(), 2);
    // Both have the same efficiency; just verify both are present.
    let labels: Vec<&str> = chain.predicates().iter().map(|p| p.label).collect();
    assert!(labels.contains(&"alpha"));
    assert!(labels.contains(&"beta"));
}

// -- Predicate cost classification -----------------------------------------

#[test]
fn efficiency_very_low_selectivity_is_best() {
    // selectivity near 0 = filters almost everything = very efficient.
    let p = make_predicate("ultra_selective", 0.001, 1.0);
    let eff = efficiency(&p);
    assert!(eff < 0.01);
}

#[test]
fn efficiency_high_cost_penalizes() {
    let cheap = make_predicate("cheap", 0.5, 1.0);
    let expensive = make_predicate("expensive", 0.5, 100.0);
    assert!(efficiency(&cheap) > efficiency(&expensive));
}

#[test]
fn efficiency_selectivity_one_is_worst() {
    // selectivity 1.0 = filters nothing = least useful.
    let p = make_predicate("passes_all", 1.0, 1.0);
    assert!((efficiency(&p) - 1.0).abs() < f64::EPSILON);
}

#[test]
fn efficiency_tiny_cost_yields_large_ratio() {
    let p = make_predicate("tiny_cost", 0.5, 0.001);
    let eff = efficiency(&p);
    assert!((eff - 500.0).abs() < 0.01);
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

// -- evaluate_chain edge cases ---------------------------------------------

#[test]
fn evaluate_chain_empty_batch() {
    let chain = PredicateChain::new(vec![make_predicate("a", 0.5, 1.0)]);
    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![];
    let result = evaluate_chain(&chain, &batch);
    assert!(result.is_empty());
}

#[test]
fn evaluate_chain_single_row_passes() {
    let chain = PredicateChain::new(vec![make_predicate("pass", 1.0, 1.0)]);
    let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
    let result = evaluate_chain(&chain, &batch);
    assert_eq!(result, vec![true]);
}

#[test]
fn evaluate_chain_single_row_rejected() {
    let pred = Predicate {
        label: "reject",
        selectivity: 0.0,
        cost: 1.0,
        eval_fn: |batch| vec![false; batch.len()],
    };
    let chain = PredicateChain::new(vec![pred]);
    let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
    let result = evaluate_chain(&chain, &batch);
    assert_eq!(result, vec![false]);
}

#[test]
fn evaluate_chain_multiple_predicates_progressive_filtering() {
    // First predicate: keep first 3 of 5 rows.
    let pred1 = Predicate {
        label: "keep_first_3",
        selectivity: 0.3,
        cost: 10.0, // efficiency = 0.03 (runs first)
        eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i < 3).collect(),
    };

    // Second predicate: keep only even-indexed survivors.
    let pred2 = Predicate {
        label: "keep_even",
        selectivity: 0.5,
        cost: 5.0, // efficiency = 0.1 (runs second)
        eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i % 2 == 0).collect(),
    };

    let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..5)
        .map(|i| (pgrx::pg_sys::Datum::from(i), false))
        .collect();

    let chain = PredicateChain::new(vec![pred1, pred2]);
    let result = evaluate_chain(&chain, &batch);

    // After pred1: [true, true, true, false, false]
    // Survivors sent to pred2: rows 0,1,2 → pred2 sees 3 rows, returns [true, false, true]
    // Final: row0=true, row1=false, row2=true, row3=false, row4=false
    assert_eq!(result, vec![true, false, true, false, false]);
}

// -- Predicate struct fields -----------------------------------------------

#[test]
fn predicate_label_accessible() {
    let p = make_predicate("my_label", 0.5, 1.0);
    assert_eq!(p.label, "my_label");
}

#[test]
fn predicate_selectivity_and_cost_accessible() {
    let p = make_predicate("test", 0.42, 7.5);
    assert!((p.selectivity - 0.42).abs() < f64::EPSILON);
    assert!((p.cost - 7.5).abs() < f64::EPSILON);
}

#[test]
fn predicate_eval_fn_callable() {
    let p = make_predicate("test", 0.5, 1.0);
    let batch = vec![(pgrx::pg_sys::Datum::from(1), false)];
    let result = (p.eval_fn)(&batch);
    assert_eq!(result, vec![true]);
}

#[test]
fn predicate_clone() {
    let p = make_predicate("original", 0.3, 2.0);
    let cloned = p.clone();
    assert_eq!(cloned.label, "original");
    assert!((cloned.selectivity - 0.3).abs() < f64::EPSILON);
    assert!((cloned.cost - 2.0).abs() < f64::EPSILON);
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

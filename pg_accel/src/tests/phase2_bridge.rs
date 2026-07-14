//! Phase 2 GPU bridge FFI safety tests (agent 2C).
//!
//! Pure unit tests — no PostgreSQL instance and no GPU dispatch required, so
//! they run under plain `cargo test -p pg_accel --lib`. Covered here:
//!
//! 1. `PgaccelStatus::from_raw` — fallible i32 → enum conversion (the extern
//!    declarations return `i32`; materialising a fieldless `#[repr(i32)]`
//!    enum from an out-of-range C value would be UB).
//! 2. `bridge::convert_status` — the single conversion point: unknown raw
//!    values are counted + mapped to an error (never assumed OK) and every
//!    non-OK status bumps the per-domain failure counter.
//! 3. Struct size **and field offset** pins for the shared FFI structs,
//!    derived from the C headers (`pgaccel_ffi.h`, `pgaccel_expr.h`,
//!    `pgaccel_hash_agg.h`, `pgaccel_fused.h`) on LP64 targets. Agent 2A
//!    mirrors the same numbers as C-side `static_assert`s.
//! 4. `expr` batch guard — caller-supplied `num_rows` that disagrees with
//!    `batch.num_rows` is rejected before any FFI call (heap-overflow
//!    foot-gun from TODO-REVIEW P1).

#![allow(clippy::unwrap_used)] // reason: test module

use std::mem::{align_of, offset_of, size_of};

use crate::gpu::bridge::convert_status;
use crate::gpu::{
    GpuFailureDomain, PgaccelBatch, PgaccelExprInstruction, PgaccelExprProgram, PgaccelExprUsmCol,
    PgaccelGeometry, PgaccelStatus, PgaccelVal, kernel_failure_count, unknown_status_count,
};

// ---------------------------------------------------------------------------
// 1. PgaccelStatus::from_raw
// ---------------------------------------------------------------------------

#[test]
fn from_raw_maps_every_known_discriminant() {
    assert_eq!(PgaccelStatus::from_raw(0), Ok(PgaccelStatus::Ok));
    assert_eq!(PgaccelStatus::from_raw(-1), Ok(PgaccelStatus::Error));
    assert_eq!(
        PgaccelStatus::from_raw(-2),
        Ok(PgaccelStatus::ErrorUnsupported)
    );
    assert_eq!(PgaccelStatus::from_raw(-3), Ok(PgaccelStatus::ErrorOom));
    assert_eq!(PgaccelStatus::from_raw(-4), Ok(PgaccelStatus::ErrorTimeout));
    assert_eq!(
        PgaccelStatus::from_raw(-5),
        Ok(PgaccelStatus::ErrorNoDevice)
    );
    assert_eq!(
        PgaccelStatus::from_raw(-6),
        Ok(PgaccelStatus::InvalidArgument)
    );
}

#[test]
fn from_raw_rejects_out_of_range_values_with_raw_payload() {
    for raw in [1, 2, -7, -100, i32::MIN, i32::MAX] {
        assert_eq!(
            PgaccelStatus::from_raw(raw),
            Err(raw),
            "raw {raw} must be rejected, not transmuted"
        );
    }
}

#[test]
fn from_raw_round_trips_every_variant_discriminant() {
    for status in [
        PgaccelStatus::Ok,
        PgaccelStatus::Error,
        PgaccelStatus::ErrorUnsupported,
        PgaccelStatus::ErrorOom,
        PgaccelStatus::ErrorTimeout,
        PgaccelStatus::ErrorNoDevice,
        PgaccelStatus::InvalidArgument,
    ] {
        assert_eq!(PgaccelStatus::from_raw(status as i32), Ok(status));
    }
}

// ---------------------------------------------------------------------------
// 2. convert_status — loud failures, counted per domain, unknown never OK
// ---------------------------------------------------------------------------

#[test]
fn convert_status_passes_ok_through_without_counting() {
    let spatial_before = kernel_failure_count(GpuFailureDomain::Spatial);
    let got = convert_status("pgaccel_spatial_intersects", 0);
    assert_eq!(got, PgaccelStatus::Ok);
    assert_eq!(
        kernel_failure_count(GpuFailureDomain::Spatial),
        spatial_before,
        "OK status must not bump the failure counter"
    );
}

#[test]
fn convert_status_counts_known_failures_per_domain() {
    let h3_before = kernel_failure_count(GpuFailureDomain::H3);
    let got = convert_status("pgaccel_h3_grid_disk_emit", -3);
    assert_eq!(got, PgaccelStatus::ErrorOom);
    assert_eq!(
        kernel_failure_count(GpuFailureDomain::H3),
        h3_before + 1,
        "non-OK status must bump the owning domain's failure counter"
    );
}

#[test]
fn convert_status_never_treats_unknown_values_as_ok() {
    let unknown_before = unknown_status_count();
    let reduce_before = kernel_failure_count(GpuFailureDomain::Reduce);
    let got = convert_status("pgaccel_reduce_sum_f32", 42);
    assert!(
        !got.is_ok(),
        "unknown raw status 42 must surface as an error, got {got:?}"
    );
    assert_eq!(got, PgaccelStatus::Error);
    assert_eq!(
        unknown_status_count(),
        unknown_before + 1,
        "unknown raw values must be counted"
    );
    assert_eq!(
        kernel_failure_count(GpuFailureDomain::Reduce),
        reduce_before + 1,
        "unknown raw values are also kernel failures for the owning domain"
    );
}

#[test]
fn failure_domain_classification_covers_every_symbol_family() {
    use GpuFailureDomain as D;
    let cases = [
        ("pgaccel_init", D::Runtime),
        ("pgaccel_shutdown", D::Runtime),
        ("pgaccel_point_in_ring_bulk", D::Spatial),
        ("pgaccel_sphere_distance_bulk", D::Spatial),
        ("pgaccel_segment_intersects_bulk", D::Spatial),
        ("pgaccel_st_area_bulk", D::Spatial),
        ("pgaccel_spatial_intersects_pairwise", D::Spatial),
        ("pgaccel_spatial_intersects", D::Spatial),
        ("pgaccel_bbox_intersects_bulk_f32", D::Spatial),
        ("pgaccel_h3_lat_lng_to_cell_bulk", D::H3),
        ("pgaccel_h3_cell_to_parent_resident_ex", D::H3),
        ("pgaccel_raster_reclass_resident_ex", D::Raster),
        ("pgaccel_sort_kv_i64", D::Sort),
        ("pgaccel_topk_kv_f32", D::Sort),
        ("pgaccel_reduce_multi_masked_i64", D::Reduce),
        ("pgaccel_fused_filter_multi_reduce_f32", D::Reduce),
        ("pgaccel_expr_template_cmp_const", D::Expr),
        ("pgaccel_grouped_agg_execute", D::GroupedAgg),
        ("pgaccel_grouped_agg_execute_ex", D::GroupedAgg),
        ("pgaccel_grouped_agg_workspace_alloc", D::GroupedAgg),
        ("pgaccel_hash_join_probe", D::HashJoin),
        ("pgaccel_hash_agg_execute", D::HashAgg),
        ("pgaccel_hash_count_i64_execute", D::HashAgg),
        ("pgaccel_agg_get_results", D::HashAgg),
        ("pgaccel_window_row_number", D::Window),
        ("pgaccel_nlj_ineq_i64", D::NestedLoop),
        ("pgaccel_pool_reset", D::Memory),
    ];
    for (symbol, expected) in cases {
        assert_eq!(
            GpuFailureDomain::classify(symbol),
            expected,
            "misclassified {symbol}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Layout pins: size + field offsets vs the C headers (LP64)
// ---------------------------------------------------------------------------
//
// Sizes are also enforced at compile time by const assertions in
// `gpu/types.rs`; the offset checks here pin the field ORDER, which sizeof
// alone cannot catch (two swapped same-width fields keep the size).

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_val_layout_matches_pgaccel_expr_h() {
    // pgaccel_expr.h: tag (enum, 4) + pad(4) + union { ..., double } (8).
    assert_eq!(size_of::<PgaccelVal>(), 16);
    assert_eq!(align_of::<PgaccelVal>(), 8);
    assert_eq!(offset_of!(PgaccelVal, tag), 0);
    assert_eq!(offset_of!(PgaccelVal, data), 8);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_expr_instruction_layout_matches_pgaccel_expr_h() {
    // pgaccel_expr.h: uint16 opcode + uint16 _pad + uint32 arg.
    assert_eq!(size_of::<PgaccelExprInstruction>(), 8);
    assert_eq!(offset_of!(PgaccelExprInstruction, opcode), 0);
    assert_eq!(offset_of!(PgaccelExprInstruction, pad), 2);
    assert_eq!(offset_of!(PgaccelExprInstruction, arg), 4);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_expr_program_layout_matches_pgaccel_expr_h() {
    assert_eq!(size_of::<PgaccelExprProgram>(), 48);
    assert_eq!(offset_of!(PgaccelExprProgram, instructions), 0);
    assert_eq!(offset_of!(PgaccelExprProgram, inst_count), 8);
    assert_eq!(offset_of!(PgaccelExprProgram, const_pool), 16);
    assert_eq!(offset_of!(PgaccelExprProgram, const_count), 24);
    assert_eq!(offset_of!(PgaccelExprProgram, max_stack), 32);
    assert_eq!(offset_of!(PgaccelExprProgram, num_cols), 40);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_batch_layout_matches_pgaccel_expr_h() {
    // pgaccel_expr.h: size_t num_rows, size_t num_cols, void** col_data,
    // uint8_t** col_nulls, pgaccel_val_tag* col_types.
    assert_eq!(size_of::<PgaccelBatch>(), 40);
    assert_eq!(offset_of!(PgaccelBatch, num_rows), 0);
    assert_eq!(offset_of!(PgaccelBatch, num_cols), 8);
    assert_eq!(offset_of!(PgaccelBatch, col_data), 16);
    assert_eq!(offset_of!(PgaccelBatch, col_nulls), 24);
    assert_eq!(offset_of!(PgaccelBatch, col_types), 32);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_expr_usm_col_layout_matches_pgaccel_expr_h() {
    // pgaccel_expr.h: const void* values, const uint8_t* nulls, tag(4) + pad(4).
    assert_eq!(size_of::<PgaccelExprUsmCol>(), 24);
    assert_eq!(offset_of!(PgaccelExprUsmCol, values), 0);
    assert_eq!(offset_of!(PgaccelExprUsmCol, nulls), 8);
    assert_eq!(offset_of!(PgaccelExprUsmCol, tag), 16);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn pgaccel_geometry_layout_matches_pgaccel_ffi_h() {
    // pgaccel_ffi.h: geom_type(4) + pad(4), const float* bbox,
    // const float* coords, size_t coord_count, const uint32_t* ring_offsets,
    // size_t ring_count.
    assert_eq!(size_of::<PgaccelGeometry>(), 48);
    assert_eq!(offset_of!(PgaccelGeometry, geom_type), 0);
    assert_eq!(offset_of!(PgaccelGeometry, bbox), 8);
    assert_eq!(offset_of!(PgaccelGeometry, coords), 16);
    assert_eq!(offset_of!(PgaccelGeometry, coord_count), 24);
    assert_eq!(offset_of!(PgaccelGeometry, ring_offsets), 32);
    assert_eq!(offset_of!(PgaccelGeometry, ring_count), 40);
}

// ---------------------------------------------------------------------------
// 4. expr batch guard — num_rows mismatch is rejected before any FFI call
// ---------------------------------------------------------------------------

#[test]
fn expr_template_cmp_const_rejects_num_rows_mismatch_before_dispatch() {
    // A batch that claims 5 rows with null column pointers. If the guard
    // failed and the wrapper dispatched, the kernel would read through the
    // null pointers — the `None` return must come from the mismatch check
    // alone (no GPU / no init in this test binary path).
    let batch = PgaccelBatch {
        num_rows: 5,
        num_cols: 0,
        col_data: std::ptr::null(),
        col_nulls: std::ptr::null(),
        col_types: std::ptr::null(),
    };
    let expr_failures_before = kernel_failure_count(GpuFailureDomain::Expr);
    let got = crate::gpu::expr_template_cmp_const(&batch, 0, 0, 1.0, 3);
    assert!(
        got.is_none(),
        "caller num_rows (3) != batch.num_rows (5) must be rejected"
    );
    assert_eq!(
        kernel_failure_count(GpuFailureDomain::Expr),
        expr_failures_before + 1,
        "the rejected mismatch must be counted as an expr failure"
    );
}

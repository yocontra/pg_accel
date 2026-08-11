//! Rust mirrors of `pgaccel-kernels/include/pgaccel_olap.h`.
//!
//! Integer fields are intentionally not Rust enums. The C entry point must
//! validate unknown discriminants before interpreting them; representing an
//! untrusted C integer as a Rust enum would itself be undefined behavior.

use std::ffi::c_void;

use crate::gpu::{PgaccelExprUsmCol, PgaccelVal};

pub const PGACCEL_OLAP_ABI_VERSION: u32 = 1;
pub const PGACCEL_GROUPED_AGG_MAX_KEYS: usize = 3;
pub const PGACCEL_GROUPED_AGG_MAX_MEASURES: usize = 4;
pub const PGACCEL_GROUPED_AGG_MAX_DIMS: usize = 4;
pub const PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES: usize = 4;

pub const PGACCEL_GROUPED_AGG_LANE_SUM: u32 = 1 << 0;
pub const PGACCEL_GROUPED_AGG_LANE_MIN: u32 = 1 << 1;
pub const PGACCEL_GROUPED_AGG_LANE_MAX: u32 = 1 << 2;
pub const PGACCEL_GROUPED_AGG_LANE_COUNT: u32 = 1 << 3;
pub const PGACCEL_GROUPED_AGG_LANE_SUMSQ: u32 = 1 << 4;
pub const PGACCEL_GROUPED_AGG_LANE_RHS_SUM: u32 = 1 << 5;
pub const PGACCEL_GROUPED_AGG_LANE_RHS_COUNT: u32 = 1 << 6;
pub const PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN: u32 = 0x7f;

pub const PGACCEL_GROUPED_AGG_EXEC_RESET: u32 = 1 << 0;
pub const PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE: u32 = 1 << 1;
pub const PGACCEL_GROUPED_AGG_EXEC_FINALIZE: u32 = 1 << 2;
pub const PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN: u32 = 0x7;

pub const PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE: i32 = i32::MIN;
pub const PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT: u32 = 1 << 0;
pub const PGACCEL_GROUPED_AGG_KEY_FLAG_ALL_KNOWN: u32 = PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT;
pub const PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE: i32 = 0;
pub const PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID: i32 = 1;
pub const PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW: i32 = 2;

pub const PGACCEL_GROUPED_AGG_MEASURE_COLUMN: i32 = 0;
pub const PGACCEL_GROUPED_AGG_MEASURE_MUL: i32 = 1;
pub const PGACCEL_GROUPED_AGG_MEASURE_SUB: i32 = 2;
pub const PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR: i32 = 3;
pub const PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR: i32 = 4;

pub const PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT: i32 = 0;
pub const PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0: i32 = 1;
pub const PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM1: i32 = 2;
pub const PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM2: i32 = 3;
pub const PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM3: i32 = 4;

pub const PGACCEL_GROUPED_AGG_FILTER_NONE: i32 = 0;
pub const PGACCEL_GROUPED_AGG_FILTER_SQL: i32 = 1;
pub const PGACCEL_GROUPED_AGG_FILTER_RECHECK: i32 = 2;

pub const PGACCEL_GROUPED_AGG_PRED_SOURCE_VALUE: i32 = 0;
pub const PGACCEL_GROUPED_AGG_PRED_SOURCE_RHS: i32 = 1;

pub const PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX: i32 = 0;
pub const PGACCEL_GROUPED_AGG_GROUPING_HASH: i32 = 1;
pub const PGACCEL_GROUPED_AGG_OUTPUT_DENSE: i32 = 0;
pub const PGACCEL_GROUPED_AGG_OUTPUT_COMPACT: i32 = 1;

pub const PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH: i32 = 1;
pub const PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT: i32 = 2;
pub const PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER: i32 = 3;
pub const PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC: i32 = 4;

pub const PGACCEL_GROUPED_AGG_ACCUM_I64: i32 = 1;
pub const PGACCEL_GROUPED_AGG_ACCUM_F64: i32 = 2;
pub const PGACCEL_GROUPED_AGG_ACCUM_NUMERIC: i32 = 3;
pub const PGACCEL_GROUPED_AGG_ACCUM_INTERVAL: i32 = 4;

pub const PGACCEL_GROUPED_AGG_PHYSICAL_INVALID: i32 = 0;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_BOOL: i32 = 1;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_INT32: i32 = 2;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_INT64: i32 = 3;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32: i32 = 4;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64: i32 = 5;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_DATE: i32 = 6;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP: i32 = 7;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC: i32 = 8;
pub const PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL: i32 = 9;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggKey {
    pub values: PgaccelExprUsmCol,
    pub lookup_by_key: *const i32,
    pub source: i32,
    pub code_min: i32,
    pub cardinality: u32,
    pub null_code: i32,
    pub flags: u32,
    pub pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggMeasureCol {
    pub values: *const c_void,
    pub nulls: *const u8,
    pub physical_type: i32,
    pub element_bytes: u32,
    pub scale: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggMeasure {
    pub value: PgaccelGroupedAggMeasureCol,
    pub rhs: PgaccelGroupedAggMeasureCol,
    pub op: i32,
    pub agg_mask: u32,
    pub accumulator_kind: i32,
    pub state_bytes: u32,
    pub flags: u32,
    pub pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggFilter {
    pub kind: i32,
    pub predicate_source: i32,
    pub predicate_measure_slot: i32,
    pub predicate_range_count: i32,
    pub predicate_lo: [PgaccelVal; PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES],
    pub predicate_hi: [PgaccelVal; PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES],
    pub value_cmp_opcode: u16,
    pub pad0: u16,
    pub flags: u32,
    pub value_cmp_const: PgaccelVal,
    pub mask: *const i8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggDim {
    pub fact_key: PgaccelExprUsmCol,
    pub match_by_key: *const u8,
    pub multiplicity_by_key: *const u64,
    pub key_min: i32,
    pub key_count: u32,
    pub flags: u32,
    pub pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggDesc {
    pub abi_version: u32,
    pub size_bytes: u32,
    pub row_count: usize,
    pub grouping_mode: i32,
    pub output_mode: i32,
    pub key_count: u32,
    pub pad0: u32,
    pub group_capacity: usize,
    pub keys: [PgaccelGroupedAggKey; PGACCEL_GROUPED_AGG_MAX_KEYS],
    pub measure_count: u32,
    pub execution_flags: u32,
    pub flags: u32,
    pub pad1: u32,
    pub measures: [PgaccelGroupedAggMeasure; PGACCEL_GROUPED_AGG_MAX_MEASURES],
    pub where_filter: PgaccelGroupedAggFilter,
    pub measure_filters: [PgaccelGroupedAggFilter; PGACCEL_GROUPED_AGG_MAX_MEASURES],
    pub dim_count: u32,
    pub pad2: u32,
    pub dims: [PgaccelGroupedAggDim; PGACCEL_GROUPED_AGG_MAX_DIMS],
    pub scratch: *mut c_void,
    pub scratch_bytes: usize,
    pub scratch_space: i32,
    pub scratch_alignment: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggWorkspaceReq {
    pub abi_version: u32,
    pub size_bytes: u32,
    pub bytes: usize,
    pub alignment: usize,
    pub space: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggMeasureOut {
    pub sum: *mut c_void,
    pub min: *mut c_void,
    pub max: *mut c_void,
    pub sumsq: *mut c_void,
    pub count: *mut u64,
    pub nonnull_count: *mut u64,
    pub rhs_sum: *mut c_void,
    pub rhs_count: *mut u64,
    pub rhs_nonnull_count: *mut u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggKeyOut {
    pub values: *mut c_void,
    pub nulls: *mut u8,
    pub value_type: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelGroupedAggOut {
    pub abi_version: u32,
    pub size_bytes: u32,
    pub group_capacity: usize,
    pub output_space: i32,
    pub flags: u32,
    pub group_codes: *mut usize,
    pub active_groups: *mut u8,
    pub keys: [PgaccelGroupedAggKeyOut; PGACCEL_GROUPED_AGG_MAX_KEYS],
    pub measures: [PgaccelGroupedAggMeasureOut; PGACCEL_GROUPED_AGG_MAX_MEASURES],
    pub emitted_group_count: usize,
    pub selected_count: u64,
    pub uncertain_count: u64,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<PgaccelGroupedAggKey>() == 56);
    assert!(std::mem::size_of::<PgaccelGroupedAggMeasureCol>() == 32);
    assert!(std::mem::size_of::<PgaccelGroupedAggMeasure>() == 88);
    assert!(std::mem::size_of::<PgaccelGroupedAggFilter>() == 176);
    assert!(std::mem::size_of::<PgaccelGroupedAggDim>() == 56);
    assert!(std::mem::size_of::<PgaccelGroupedAggDesc>() == 1712);
    assert!(std::mem::size_of::<PgaccelGroupedAggWorkspaceReq>() == 32);
    assert!(std::mem::size_of::<PgaccelGroupedAggMeasureOut>() == 72);
    assert!(std::mem::size_of::<PgaccelGroupedAggKeyOut>() == 24);
    assert!(std::mem::size_of::<PgaccelGroupedAggOut>() == 424);
};

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn grouped_agg_component_layouts_match_c_header() {
        assert_eq!(size_of::<PgaccelGroupedAggKey>(), 56);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, values), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, lookup_by_key), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, source), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, code_min), 36);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, cardinality), 40);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, null_code), 44);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, flags), 48);
        assert_eq!(offset_of!(PgaccelGroupedAggKey, pad0), 52);

        assert_eq!(size_of::<PgaccelGroupedAggMeasureCol>(), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, values), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, nulls), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, physical_type), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, element_bytes), 20);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, scale), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureCol, flags), 28);

        assert_eq!(size_of::<PgaccelGroupedAggMeasure>(), 88);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, value), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, rhs), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, op), 64);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, agg_mask), 68);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, accumulator_kind), 72);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, state_bytes), 76);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, flags), 80);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasure, pad0), 84);

        assert_eq!(size_of::<PgaccelGroupedAggFilter>(), 176);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, kind), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, predicate_source), 4);
        assert_eq!(
            offset_of!(PgaccelGroupedAggFilter, predicate_measure_slot),
            8
        );
        assert_eq!(
            offset_of!(PgaccelGroupedAggFilter, predicate_range_count),
            12
        );
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, predicate_lo), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, predicate_hi), 80);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, value_cmp_opcode), 144);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, pad0), 146);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, flags), 148);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, value_cmp_const), 152);
        assert_eq!(offset_of!(PgaccelGroupedAggFilter, mask), 168);

        assert_eq!(size_of::<PgaccelGroupedAggDim>(), 56);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, fact_key), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, match_by_key), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, multiplicity_by_key), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, key_min), 40);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, key_count), 44);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, flags), 48);
        assert_eq!(offset_of!(PgaccelGroupedAggDim, pad0), 52);
    }

    #[test]
    fn grouped_agg_descriptor_layout_matches_c_header() {
        assert_eq!(size_of::<PgaccelGroupedAggDesc>(), 1712);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, abi_version), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, size_bytes), 4);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, row_count), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, grouping_mode), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, output_mode), 20);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, key_count), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, pad0), 28);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, group_capacity), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, keys), 40);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, measure_count), 208);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, execution_flags), 212);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, flags), 216);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, pad1), 220);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, measures), 224);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, where_filter), 576);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, measure_filters), 752);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, dim_count), 1456);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, pad2), 1460);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, dims), 1464);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, scratch), 1688);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, scratch_bytes), 1696);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, scratch_space), 1704);
        assert_eq!(offset_of!(PgaccelGroupedAggDesc, scratch_alignment), 1708);
    }

    #[test]
    fn grouped_agg_output_layouts_match_c_header() {
        assert_eq!(size_of::<PgaccelGroupedAggWorkspaceReq>(), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, abi_version), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, size_bytes), 4);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, bytes), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, alignment), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, space), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggWorkspaceReq, flags), 28);

        assert_eq!(size_of::<PgaccelGroupedAggMeasureOut>(), 72);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, sum), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, min), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, max), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, sumsq), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, count), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, nonnull_count), 40);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, rhs_sum), 48);
        assert_eq!(offset_of!(PgaccelGroupedAggMeasureOut, rhs_count), 56);
        assert_eq!(
            offset_of!(PgaccelGroupedAggMeasureOut, rhs_nonnull_count),
            64
        );

        assert_eq!(size_of::<PgaccelGroupedAggKeyOut>(), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggKeyOut, values), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggKeyOut, nulls), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggKeyOut, value_type), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggKeyOut, flags), 20);

        assert_eq!(size_of::<PgaccelGroupedAggOut>(), 424);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, abi_version), 0);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, size_bytes), 4);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, group_capacity), 8);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, output_space), 16);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, flags), 20);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, group_codes), 24);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, active_groups), 32);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, keys), 40);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, measures), 112);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, emitted_group_count), 400);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, selected_count), 408);
        assert_eq!(offset_of!(PgaccelGroupedAggOut, uncertain_count), 416);
    }

    #[test]
    fn grouped_agg_discriminants_match_c_header() {
        assert_eq!(PGACCEL_OLAP_ABI_VERSION, 1);
        assert_eq!(PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR, 4);
        assert_eq!(PGACCEL_GROUPED_AGG_FILTER_RECHECK, 2);
        assert_eq!(PGACCEL_GROUPED_AGG_GROUPING_HASH, 1);
        assert_eq!(PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH, 1);
        assert_eq!(PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT, 2);
        assert_eq!(PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER, 3);
        assert_eq!(PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC, 4);
        assert_eq!(PGACCEL_GROUPED_AGG_ACCUM_INTERVAL, 4);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_INVALID, 0);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_BOOL, 1);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_INT32, 2);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_INT64, 3);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32, 4);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64, 5);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_DATE, 6);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP, 7);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC, 8);
        assert_eq!(PGACCEL_GROUPED_AGG_PHYSICAL_INTERVAL, 9);
        assert_eq!(PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN, 0x7f);
        assert_eq!(PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN, 0x7);
        assert_eq!(PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT, 0x1);
        assert_eq!(PGACCEL_GROUPED_AGG_KEY_FLAG_ALL_KNOWN, 0x1);
        assert_eq!(PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE, 0);
        assert_eq!(PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID, 1);
        assert_eq!(PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW, 2);
    }
}

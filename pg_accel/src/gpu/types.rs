//! Shared FFI types for the GPU kernel bridge.
//!
//! These `#[repr(C)]` types mirror `pgaccel-kernels/include/pgaccel_ffi.h`
//! and `pgaccel_fused.h` exactly and are the single source of truth for
//! the Rust side of the bridge (`bridge.rs`).
//!
//! **Layout is load-bearing.** Do not reorder fields, change variant
//! discriminants, or change underlying integer widths. Any drift corrupts
//! every GPU call.

use std::ffi::{c_char, c_void};

// ---------------------------------------------------------------------------
// Status code returned by every kernel-library call.
// ---------------------------------------------------------------------------

/// Status codes returned by the pgaccel C library.
///
/// Values **must** stay in sync with the `pgaccel_status` enum in
/// `pgaccel_ffi.h`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: ABI mirror of pgaccel_status; all variants must exist for FFI parity
pub enum PgaccelStatus {
    Ok = 0,
    ErrorInit = -1,
    ErrorUnsupported = -2,
    ErrorOom = -3,
    ErrorTimeout = -4,
    ErrorNoDevice = -5,
}

impl PgaccelStatus {
    /// Returns `true` when the status indicates success.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

// ---------------------------------------------------------------------------
// Structs returned by device-query functions.
// ---------------------------------------------------------------------------

/// Per-device information (mirrors `pgaccel_device_info`).
///
/// `has_native_fp64` is a pure cost signal: `true` means the device has
/// hardware fp64 (CUDA/ROCm/L0 tier-1), `false` means fp64 runs via the
/// AdaptiveCpp soft-fp64 libkernel (e.g. Apple Silicon Metal). Both paths
/// produce correct IEEE-754 double results — the flag only tells the
/// planner/cost model that one of them is noticeably slower than fp32.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelDeviceInfo {
    pub device_name: [c_char; 128],
    pub backend_name: [c_char; 64],
    pub compute_units: u32,
    pub max_alloc_bytes: usize,
    pub has_native_fp64: bool,
    pub has_atomic64: bool,
}

/// Platform-level capability summary (mirrors `pgaccel_platform_caps`).
///
/// See [`PgaccelDeviceInfo::has_native_fp64`] for the semantics of
/// `has_native_fp64`: cost hint only, not a skip-gate.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelPlatformCaps {
    pub has_native_fp64: bool,
    pub has_atomic64: bool,
    pub has_ooo_queue: bool,
    pub max_alloc_bytes: usize,
    pub compute_units: u32,
    pub backend_name: [c_char; 64],
}

// ABI pins: these sizes must match the C side (`pgaccel_ffi.h`). If either
// assert fires, the C struct drifted — do NOT bump the numbers here; fix
// the C struct to match or escalate.
const _: () = assert!(std::mem::size_of::<PgaccelPlatformCaps>() == 88);
const _: () = assert!(std::mem::size_of::<PgaccelDeviceInfo>() == 216);

// ---------------------------------------------------------------------------
// Expression evaluator types (mirrors pgaccel_expr.h).
// ---------------------------------------------------------------------------

/// Value type tag for the expression evaluator.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelValTag {
    Null = 0,
    Bool = 1,
    Int32 = 2,
    Int64 = 3,
    Float32 = 4,
    Float64 = 5,
    Date = 6,
    Timestamp = 7,
}

/// Tagged value — 16 bytes. Matches `pgaccel_val` in `pgaccel_expr.h`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelVal {
    pub tag: PgaccelValTag,
    pub data: u64,
}

impl PgaccelVal {
    #[must_use]
    pub const fn null() -> Self {
        Self {
            tag: PgaccelValTag::Null,
            data: 0,
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        Self {
            tag: PgaccelValTag::Int32,
            data: v as u64,
        }
    }

    #[must_use]
    pub const fn from_i64(v: i64) -> Self {
        Self {
            tag: PgaccelValTag::Int64,
            data: v as u64,
        }
    }

    #[must_use]
    pub fn from_f64(v: f64) -> Self {
        Self {
            tag: PgaccelValTag::Float64,
            data: v.to_bits(),
        }
    }

    #[must_use]
    pub fn from_f32(v: f32) -> Self {
        Self {
            tag: PgaccelValTag::Float32,
            data: u64::from(v.to_bits()),
        }
    }

    #[must_use]
    pub const fn from_bool(v: bool) -> Self {
        Self {
            tag: PgaccelValTag::Bool,
            data: v as u64,
        }
    }
}

/// Single bytecode instruction — 8 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelExprInstruction {
    pub opcode: u16,
    pub pad: u16,
    pub arg: u32,
}

/// Expression program (bytecode + constant pool).
#[repr(C)]
pub struct PgaccelExprProgram {
    pub instructions: *const PgaccelExprInstruction,
    pub inst_count: usize,
    pub const_pool: *const PgaccelVal,
    pub const_count: usize,
    pub max_stack: usize,
    pub num_cols: usize,
}

/// Columnar batch for expression evaluation.
#[repr(C)]
pub struct PgaccelBatch {
    pub num_rows: usize,
    pub num_cols: usize,
    pub col_data: *const *const std::ffi::c_void,
    pub col_nulls: *const *const u8,
    pub col_types: *const PgaccelValTag,
}

/// Already-staged shared-USM expression column for fused template kernels.
///
/// `nulls == NULL` means all rows are valid. `values` points to a shared-USM
/// buffer whose element type is described by `tag`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelExprUsmCol {
    pub values: *const c_void,
    pub nulls: *const u8,
    pub tag: PgaccelValTag,
}

// ---------------------------------------------------------------------------
// Resident batch fabric types (mirrors pgaccel_expr.h additive ABI).
// ---------------------------------------------------------------------------

/// ABI version for [`PgaccelResidentBatch`] and [`PgaccelDeviceVarOutput`].
///
/// Version 1 is additive and does not change the legacy [`PgaccelBatch`] shape.
pub const PGACCEL_RESIDENT_BATCH_ABI_VERSION: u32 = 1;

/// Memory space for a resident batch pointer.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: additive resident ABI mirror; consumers land incrementally
pub enum PgaccelMemSpace {
    Host = 0,
    SharedUsm = 1,
    Device = 2,
}

/// One typed input/output column view in the resident batch fabric.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: additive resident ABI mirror; no selected SQL callers until proof wiring lands
pub struct PgaccelResidentColumnView {
    pub values: *const c_void,
    pub nulls: *const u8,
    pub tag: PgaccelValTag,
    pub values_space: PgaccelMemSpace,
    pub nulls_space: PgaccelMemSpace,
    pub element_size: usize,
    pub flags: u32,
    pub pad: u32,
}

/// Resident columnar batch view for scan/expression/relational consumers.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: additive resident ABI mirror; legacy host PgaccelBatch remains unchanged
pub struct PgaccelResidentBatch {
    pub abi_version: u32,
    pub flags: u32,
    pub num_rows: usize,
    pub num_cols: usize,
    pub columns: *const PgaccelResidentColumnView,
    pub selection: *const u8,
    pub selection_space: PgaccelMemSpace,
    pub pad: u32,
    pub selected_rows: usize,
}

/// Device-resident variable-cardinality output view.
///
/// `offsets` is an exclusive prefix-sum array with `input_row_count + 1`
/// entries and `offsets[input_row_count] == output_count`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: additive resident ABI mirror for H3/PostGIS/raster/join-pair outputs
pub struct PgaccelDeviceVarOutput {
    pub abi_version: u32,
    pub flags: u32,
    pub input_row_count: usize,
    pub output_count: usize,
    pub capacity: usize,
    pub offsets: *const u64,
    pub counts: *const u64,
    pub parent_row_ids: *const u64,
    pub payload_cols: *const PgaccelResidentColumnView,
    pub payload_col_count: usize,
    pub null_mask: *const u8,
    pub unsupported_mask: *const u8,
    pub uncertain_mask: *const u8,
    pub mask_space: PgaccelMemSpace,
    pub pad: u32,
}

// ---------------------------------------------------------------------------
// Hash aggregation types (mirrors pgaccel_hash_agg.h).
// ---------------------------------------------------------------------------

/// Aggregate function tag for GPU hash aggregation.
///
/// Variants 0..3 are the *finalize-mode* funcs supported by the original
/// `pgaccel_hash_agg_execute` entry point. Variants 4..6 (`Avg`, `Stddev`,
/// `Var`) are emitted **only** by `pgaccel_hash_agg_execute_partial` —
/// they describe richer per-group transition states (`[N, sum]` for AVG,
/// `[N, sum, sum_sq]` for STDDEV/VAR, in PG's `float8_accum` layout).
///
/// Mirror of the C enum `pgaccel_agg_func` in
/// `pgaccel-kernels/include/pgaccel_hash_agg.h`. Discriminant values must
/// stay synced — `bridge.rs` asserts this in `agg_func_discriminant_values_match_c`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelAggFunc {
    Sum = 0,
    Min = 1,
    Max = 2,
    Count = 3,
    /// AVG transition state: `[N, sum]` per group (partial mode only).
    Avg = 4,
    /// `STDDEV_*` transition state: `[N, sum, sum_sq]` per group (partial mode only).
    Stddev = 5,
    /// `VAR_*` transition state: `[N, sum, sum_sq]` per group (partial mode only).
    Var = 6,
}

impl PgaccelAggFunc {
    /// Width (number of f64 lanes) the **partial-mode** kernel emits per
    /// group for this func. SUM/MIN/MAX/COUNT keep their 1-wide finalize
    /// shape; AVG widens to 2 (`[N, sum]`); STDDEV/VAR widen to 3
    /// (`[N, sum, sum_sq]`).
    ///
    /// Mirror of the C-side `pgaccel_agg_partial_width` (see
    /// `pgaccel-kernels/include/pgaccel_hash_agg.h`); the Rust copy is
    /// kept around for planner/executor code that needs the width
    /// without a kernel-state round-trip — `bridge::pgaccel_agg_get_partial_width`
    /// reads the per-state width recorded by the kernel.
    #[allow(dead_code)] // reason: Rust mirror of C-side pgaccel_agg_partial_width; planner-side users land in Phase 3A
    #[must_use]
    pub const fn partial_width(self) -> usize {
        match self {
            Self::Sum | Self::Min | Self::Max | Self::Count => 1,
            Self::Avg => 2,
            Self::Stddev | Self::Var => 3,
        }
    }
}

/// Aggregate column descriptor for GPU hash aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelAggCol {
    pub func: PgaccelAggFunc,
    pub col_idx: usize,
}

/// Opaque handle to GPU hash aggregation state.
pub enum PgaccelAggState {}

// ---------------------------------------------------------------------------
// Hash join types (mirrors pgaccel_hash_join.h).
// ---------------------------------------------------------------------------

/// Key type for hash join / hash agg operations.
///
/// Discriminant values must match the C `pgaccel_key_type` enum in
/// `pgaccel-kernels/include/pgaccel_hash_join.h`. Slot 3 is reserved
/// on the planner side for `CompositeInt4x2` (two int4 columns packed
/// into one int8); the executor unpacks composites to int8 before
/// kernel dispatch, so the kernel never sees `key_type == 3`. UUID
/// occupies slot 4 to keep the kernel-facing values one-to-one with
/// what the kernel actually receives.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelKeyType {
    Int32 = 0,
    Int64 = 1,
    Float64 = 2,
    /// 16-byte UUID, host byte order. Hashed via two `hash64()` mixes
    /// XORed together inside the kernel.
    Uuid = 4,
    /// 24-byte canonical INET / CIDR key (family + bits + 16-byte
    /// ipaddr + 6-byte u64-alignment pad). Hashed via three
    /// `hash64()` mixes XORed together. CIDR shares the slot — the
    /// planner classifier maps both INETOID (869) and CIDROID (650)
    /// to this variant.
    Inet = 5,
}

/// Opaque handle to a GPU-side hash table.
pub enum PgaccelHashTable {}

// ---------------------------------------------------------------------------
// Geometry types for the three-layer spatial pipeline.
// ---------------------------------------------------------------------------

/// Geometry type tag (mirrors `pgaccel_geom_type` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelGeomType {
    Point = 0,
    LineString = 1,
    Polygon = 2,
    Unknown = 99,
}

/// Geometry descriptor for the spatial dispatch pipeline
/// (mirrors `pgaccel_geometry` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelGeometry {
    pub geom_type: PgaccelGeomType,
    pub bbox: *const f32,
    pub coords: *const f32,
    pub coord_count: usize,
    pub ring_offsets: *const u32,
    pub ring_count: usize,
}

// ---------------------------------------------------------------------------
// Raster types for map algebra and reclassification.
// ---------------------------------------------------------------------------

/// Pixel type tag (mirrors `pgaccel_pixel_type` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: ABI mirror of pgaccel_pixel_type; discriminants must match the C enum
pub enum PgaccelPixelType {
    Int8 = 0,
    Int16 = 1,
    Int32 = 2,
    Float32 = 3,
    Float64 = 4,
}

/// Map-algebra opcode (mirrors `pgaccel_op` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: ABI mirror of pgaccel_op; discriminants must match the C enum
pub enum PgaccelOp {
    LoadBand = 0,
    LoadConst = 1,
    Add = 2,
    Sub = 3,
    Mul = 4,
    Div = 5,
    Sqrt = 6,
    Abs = 7,
    Log = 8,
    Pow = 9,
    Gt = 10,
    Lt = 11,
    Eq = 12,
    Select = 13,
}

// ---------------------------------------------------------------------------
// Fused filter+reduce types (mirrors pgaccel_fused.h).
// ---------------------------------------------------------------------------

/// Single instruction in a map-algebra expression
/// (mirrors `pgaccel_expr_inst` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelExprInst {
    pub op: PgaccelOp,
    /// Union: `band_index` (i32) or `constant` (f64). Use the larger
    /// type so the struct size matches the C layout.
    pub arg: f64,
}

/// Map-algebra expression program (mirrors `pgaccel_expr` in `pgaccel_ffi.h`).
#[repr(C)]
pub struct PgaccelExpr {
    pub instructions: *mut PgaccelExprInst,
    pub inst_count: usize,
    pub band_count: usize,
}

/// Reclassification rule (mirrors `pgaccel_reclass_rule` in `pgaccel_ffi.h`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
#[allow(dead_code)] // reason: ABI mirror of pgaccel_reclass_rule; struct layout load-bearing
pub struct PgaccelReclassRule {
    pub min_val: f64,
    pub max_val: f64,
    pub new_val: f64,
}

// ---------------------------------------------------------------------------
// Fused ops — comparison + reduce op tags and descriptor.
// ---------------------------------------------------------------------------

/// Comparison operator for fused filter predicates.
/// Values must stay in sync with `pgaccel_cmp_op` in `pgaccel_fused.h`.
#[allow(dead_code)] // reason: ABI mirror of pgaccel_cmp_op; constants must match the C enum
pub mod cmp_op {
    pub const EQ: i32 = 0;
    pub const NE: i32 = 1;
    pub const LT: i32 = 2;
    pub const LE: i32 = 3;
    pub const GT: i32 = 4;
    pub const GE: i32 = 5;
    pub const ALWAYS_TRUE: i32 = 6;
}

/// Reduce operation tag for fused multi-reduce.
/// Values must stay in sync with `pgaccel_reduce_op` in `pgaccel_fused.h`.
#[allow(dead_code)] // reason: ABI mirror of pgaccel_reduce_op; constants must match the C enum
pub mod reduce_op {
    pub const SUM: i32 = 0;
    pub const MIN: i32 = 1;
    pub const MAX: i32 = 2;
    pub const COUNT: i32 = 3;
}

/// Per-column reduce descriptor (mirrors `pgaccel_reduce_col`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: ABI mirror of pgaccel_reduce_col; struct layout load-bearing
pub struct PgaccelReduceCol {
    pub op: i32,
    pub data: *const f32,
}

#[cfg(test)]
mod resident_abi_tests {
    use super::*;

    #[test]
    fn resident_memory_space_discriminants_match_c_header() {
        assert_eq!(PgaccelMemSpace::Host as i32, 0);
        assert_eq!(PgaccelMemSpace::SharedUsm as i32, 1);
        assert_eq!(PgaccelMemSpace::Device as i32, 2);
    }

    #[test]
    fn resident_batch_abi_layout_is_pinned() {
        assert_eq!(PGACCEL_RESIDENT_BATCH_ABI_VERSION, 1);
        assert_eq!(std::mem::size_of::<PgaccelResidentColumnView>(), 48);
        assert_eq!(std::mem::size_of::<PgaccelResidentBatch>(), 56);
        assert_eq!(std::mem::size_of::<PgaccelDeviceVarOutput>(), 104);
    }
}

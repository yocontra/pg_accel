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
///
/// This enum is **never** used directly as an FFI return type: a fieldless
/// `#[repr(i32)]` enum materialised from an out-of-range C value is
/// instant undefined behaviour. The extern declarations in `bridge.rs`
/// return raw `i32` and the bridge wrapper layer converts through
/// [`PgaccelStatus::from_raw`], which rejects unknown values instead of
/// transmuting them.
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

    /// Fallible conversion from the raw `i32` a C kernel entry point
    /// returned. `Err(raw)` carries the unrecognised value so the caller
    /// can log it verbatim; it must never be treated as success.
    ///
    /// The mapping mirrors `pgaccel_status` in `pgaccel_ffi.h` exactly
    /// (the `PGACCEL_ERROR_*` names in that enum are value aliases of the
    /// short names, so each discriminant appears once here).
    pub const fn from_raw(raw: i32) -> Result<Self, i32> {
        match raw {
            0 => Ok(Self::Ok),
            -1 => Ok(Self::ErrorInit),
            -2 => Ok(Self::ErrorUnsupported),
            -3 => Ok(Self::ErrorOom),
            -4 => Ok(Self::ErrorTimeout),
            -5 => Ok(Self::ErrorNoDevice),
            other => Err(other),
        }
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
// Compile-time ABI pins for every shared FFI struct.
//
// These are `const` assertions (not `#[test]`s) so they are checked by every
// `cargo check` / `cargo build`, not just test runs. Each expected size is
// derived from the C header layout on LP64 targets:
//   - `pgaccel_expr.h`     — pgaccel_val, pgaccel_expr_instruction,
//                            pgaccel_expr_program, pgaccel_batch,
//                            pgaccel_expr_usm_col, resident batch fabric
//   - `pgaccel_ffi.h`      — pgaccel_geometry, pgaccel_expr_inst,
//                            pgaccel_expr, pgaccel_reclass_rule
//   - `pgaccel_hash_agg.h` — pgaccel_agg_col
//   - `pgaccel_fused.h`    — pgaccel_reduce_col
// If one fires, the two sides drifted — fix the drift, never the number.
// Agent 2A mirrors the same numbers as C-side static_asserts.
// ---------------------------------------------------------------------------
#[cfg(target_pointer_width = "64")]
mod abi_size_pins {
    // reason: compile-time size/align pins reference most ABI structs in this
    // file; an explicit import list would drift every time a struct is added.
    #[allow(clippy::wildcard_imports)]
    use super::*;

    // pgaccel_expr.h — tag(i32) + pad(4) + union{..., double}(8) = 16.
    const _: () = assert!(std::mem::size_of::<PgaccelVal>() == 16);
    const _: () = assert!(std::mem::align_of::<PgaccelVal>() == 8);
    // u16 + u16 + u32 = 8.
    const _: () = assert!(std::mem::size_of::<PgaccelExprInstruction>() == 8);
    // ptr + size_t + ptr + size_t + size_t + size_t = 48.
    const _: () = assert!(std::mem::size_of::<PgaccelExprProgram>() == 48);
    // size_t + size_t + ptr + ptr + ptr = 40.
    const _: () = assert!(std::mem::size_of::<PgaccelBatch>() == 40);
    // ptr + ptr + tag(i32) + pad(4) = 24.
    const _: () = assert!(std::mem::size_of::<PgaccelExprUsmCol>() == 24);
    // Resident batch fabric (pgaccel_expr.h additive ABI v1).
    const _: () = assert!(std::mem::size_of::<PgaccelResidentColumnView>() == 48);
    const _: () = assert!(std::mem::size_of::<PgaccelResidentBatch>() == 56);
    const _: () = assert!(std::mem::size_of::<PgaccelDeviceVarOutput>() == 104);

    // pgaccel_ffi.h — geom_type(4) + pad(4) + 2 ptr + size_t + ptr + size_t = 48.
    const _: () = assert!(std::mem::size_of::<PgaccelGeometry>() == 48);
    // op(4) + pad(4) + union{int, double}(8) = 16.
    const _: () = assert!(std::mem::size_of::<PgaccelExprInst>() == 16);
    // ptr + size_t + size_t = 24.
    const _: () = assert!(std::mem::size_of::<PgaccelExpr>() == 24);
    // 3 × f64 = 24.
    const _: () = assert!(std::mem::size_of::<PgaccelReclassRule>() == 24);
    const _: () = assert!(std::mem::align_of::<PgaccelReclassRule>() == 8);
}

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
#[allow(dead_code)] // reason: additive resident ABI mirror; versions the resident-batch structs kept for FFI parity
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

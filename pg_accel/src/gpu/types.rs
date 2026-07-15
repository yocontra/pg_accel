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
    /// Generic execution/validation failure (`PGACCEL_ERROR`). The C enum
    /// also exposes `PGACCEL_ERROR_INIT` as an alias of this value, so the
    /// operation context, not the discriminant, identifies init failures.
    Error = -1,
    ErrorUnsupported = -2,
    ErrorOom = -3,
    ErrorTimeout = -4,
    ErrorNoDevice = -5,
    InvalidArgument = -6,
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
            -1 => Ok(Self::Error),
            -2 => Ok(Self::ErrorUnsupported),
            -3 => Ok(Self::ErrorOom),
            -4 => Ok(Self::ErrorTimeout),
            -5 => Ok(Self::ErrorNoDevice),
            -6 => Ok(Self::InvalidArgument),
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
//   - `pgaccel_ffi.h`      — pgaccel_geometry and resident raster ABI
//   - `pgaccel_hash_agg.h` — pgaccel_agg_col
//   - `pgaccel_fused.h`    — pgaccel_reduce_col
// If one fires, the two sides drifted — fix the drift, never the number.
// The C headers mirror the same values with static assertions.
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
    // Exact resident raster Reclass ABI (pgaccel_ffi.h, LP64).
    const _: () = assert!(std::mem::size_of::<PgaccelResidentRasterRow>() == 72);
    const _: () = assert!(std::mem::align_of::<PgaccelResidentRasterRow>() == 8);
    const _: () = assert!(std::mem::size_of::<PgaccelResidentRasterBand>() == 16);
    const _: () = assert!(std::mem::align_of::<PgaccelResidentRasterBand>() == 8);
    const _: () = assert!(std::mem::size_of::<PgaccelResidentRasterView>() == 104);
    const _: () = assert!(std::mem::align_of::<PgaccelResidentRasterView>() == 8);
    const _: () = assert!(std::mem::size_of::<PgaccelResidentRasterReclassRule>() == 16);
    const _: () = assert!(std::mem::align_of::<PgaccelResidentRasterReclassRule>() == 8);
    const _: () = assert!(std::mem::size_of::<PgaccelResidentRasterValidationScratch>() == 24);
    const _: () = assert!(std::mem::align_of::<PgaccelResidentRasterValidationScratch>() == 8);
    const _: () = assert!(std::mem::size_of::<PgaccelRasterReclassResidentRequest>() == 240);
    const _: () = assert!(std::mem::align_of::<PgaccelRasterReclassResidentRequest>() == 8);

    macro_rules! offset_pin {
        ($type:ty, $field:ident, $offset:expr) => {
            const _: () = assert!(std::mem::offset_of!($type, $field) == $offset);
        };
    }

    offset_pin!(PgaccelResidentRasterRow, width, 0);
    offset_pin!(PgaccelResidentRasterRow, height, 4);
    offset_pin!(PgaccelResidentRasterRow, first_band, 8);
    offset_pin!(PgaccelResidentRasterRow, band_count, 12);
    offset_pin!(PgaccelResidentRasterRow, srid, 16);
    offset_pin!(PgaccelResidentRasterRow, flags, 20);
    offset_pin!(PgaccelResidentRasterRow, scale_x, 24);
    offset_pin!(PgaccelResidentRasterRow, scale_y, 32);
    offset_pin!(PgaccelResidentRasterRow, ip_x, 40);
    offset_pin!(PgaccelResidentRasterRow, ip_y, 48);
    offset_pin!(PgaccelResidentRasterRow, skew_x, 56);
    offset_pin!(PgaccelResidentRasterRow, skew_y, 64);

    offset_pin!(PgaccelResidentRasterBand, pixel_type, 0);
    offset_pin!(PgaccelResidentRasterBand, flags, 4);
    offset_pin!(PgaccelResidentRasterBand, nodata, 8);

    offset_pin!(PgaccelResidentRasterView, abi_version, 0);
    offset_pin!(PgaccelResidentRasterView, flags, 4);
    offset_pin!(PgaccelResidentRasterView, pixels, 8);
    offset_pin!(PgaccelResidentRasterView, pixels_bytes, 16);
    offset_pin!(PgaccelResidentRasterView, band_offsets, 24);
    offset_pin!(PgaccelResidentRasterView, band_offsets_bytes, 32);
    offset_pin!(PgaccelResidentRasterView, rows, 40);
    offset_pin!(PgaccelResidentRasterView, rows_bytes, 48);
    offset_pin!(PgaccelResidentRasterView, bands, 56);
    offset_pin!(PgaccelResidentRasterView, bands_bytes, 64);
    offset_pin!(PgaccelResidentRasterView, nulls, 72);
    offset_pin!(PgaccelResidentRasterView, nulls_bytes, 80);
    offset_pin!(PgaccelResidentRasterView, row_count, 88);
    offset_pin!(PgaccelResidentRasterView, band_count, 96);

    offset_pin!(PgaccelResidentRasterReclassRule, source, 0);
    offset_pin!(PgaccelResidentRasterReclassRule, destination, 8);

    offset_pin!(PgaccelResidentRasterValidationScratch, failures, 0);
    offset_pin!(PgaccelResidentRasterValidationScratch, pad, 4);
    offset_pin!(
        PgaccelResidentRasterValidationScratch,
        first_output_offset,
        8
    );
    offset_pin!(
        PgaccelResidentRasterValidationScratch,
        last_output_offset,
        16
    );

    offset_pin!(PgaccelRasterReclassResidentRequest, abi_version, 0);
    offset_pin!(PgaccelRasterReclassResidentRequest, flags, 4);
    offset_pin!(PgaccelRasterReclassResidentRequest, input, 8);
    offset_pin!(PgaccelRasterReclassResidentRequest, first_row, 112);
    offset_pin!(PgaccelRasterReclassResidentRequest, count, 120);
    offset_pin!(PgaccelRasterReclassResidentRequest, output_pixel_type, 128);
    offset_pin!(PgaccelRasterReclassResidentRequest, pad, 132);
    offset_pin!(PgaccelRasterReclassResidentRequest, rules, 136);
    offset_pin!(PgaccelRasterReclassResidentRequest, rules_bytes, 144);
    offset_pin!(PgaccelRasterReclassResidentRequest, rule_count, 152);
    offset_pin!(PgaccelRasterReclassResidentRequest, output_offsets, 160);
    offset_pin!(
        PgaccelRasterReclassResidentRequest,
        output_offsets_bytes,
        168
    );
    offset_pin!(PgaccelRasterReclassResidentRequest, output_pixels, 176);
    offset_pin!(
        PgaccelRasterReclassResidentRequest,
        output_pixels_bytes,
        184
    );
    offset_pin!(PgaccelRasterReclassResidentRequest, row_actions, 192);
    offset_pin!(PgaccelRasterReclassResidentRequest, row_actions_bytes, 200);
    offset_pin!(PgaccelRasterReclassResidentRequest, validation_scratch, 208);
    offset_pin!(
        PgaccelRasterReclassResidentRequest,
        validation_scratch_bytes,
        216
    );
    offset_pin!(PgaccelRasterReclassResidentRequest, max_total_pixels, 224);
    offset_pin!(PgaccelRasterReclassResidentRequest, max_chunk_pixels, 232);
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
pub enum PgaccelMemSpace {
    Host = 0,
    SharedUsm = 1,
    Device = 2,
}

/// One typed input/output column view in the resident batch fabric.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // reason: frozen resident-batch ABI member; selected executors use domain-specific views
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
// Exact resident raster Reclass ABI.
// ---------------------------------------------------------------------------

/// ABI version for the exact resident PostGIS Reclass descriptor.
pub const PGACCEL_RESIDENT_RASTER_ABI_VERSION: u32 = 1;
#[allow(dead_code)] // reason: frozen native ABI cap pinned by discriminant tests
pub const PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES: u32 = 64;
#[allow(dead_code)] // reason: frozen native ABI cap pinned by discriminant tests
pub const PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH: u32 = 65_536;
#[allow(dead_code)] // reason: frozen native ABI cap pinned by discriminant tests
pub const PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS: u32 = 4_096;

// Literal PostGIS 3.6.4 rt_pixtype tags. These stay raw u32 values instead of
// a Rust enum so validation tests can safely construct malformed descriptors.
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_BOOL: u32 = 0;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_UINT2: u32 = 1;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_UINT4: u32 = 2;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_INT8: u32 = 3;
pub const PGACCEL_RESIDENT_RASTER_UINT8: u32 = 4;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_INT16: u32 = 5;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_UINT16: u32 = 6;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_INT32: u32 = 7;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag
pub const PGACCEL_RESIDENT_RASTER_UINT32: u32 = 8;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag retained for input views
pub const PGACCEL_RESIDENT_RASTER_FLOAT32: u32 = 10;
#[allow(dead_code)] // reason: literal PostGIS resident ABI tag retained for input views
pub const PGACCEL_RESIDENT_RASTER_FLOAT64: u32 = 11;

#[allow(dead_code)] // reason: frozen native band flag; residency owns the Rust-domain mirror
pub const PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA: u32 = 1 << 0;
#[allow(dead_code)] // reason: frozen native band flag; residency owns the Rust-domain mirror
pub const PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA: u32 = 1 << 1;

#[allow(dead_code)] // reason: frozen native output tag pinned by discriminant tests
pub const PGACCEL_RASTER_ROW_NULL: u8 = 0;
#[allow(dead_code)] // reason: frozen native output tag pinned by discriminant tests
pub const PGACCEL_RASTER_ROW_PASSTHROUGH: u8 = 1;
#[allow(dead_code)] // reason: frozen native output tag pinned by discriminant tests
pub const PGACCEL_RASTER_ROW_RECLASSIFIED: u8 = 2;

pub const PGACCEL_RASTER_VALIDATION_VIEW: u32 = 1 << 0;
pub const PGACCEL_RASTER_VALIDATION_RULES: u32 = 1 << 1;
pub const PGACCEL_RASTER_VALIDATION_OFFSETS: u32 = 1 << 2;
pub const PGACCEL_RASTER_VALIDATION_CAPACITY: u32 = 1 << 3;
pub const PGACCEL_RASTER_VALIDATION_BYTE_BUDGET: u32 = 1 << 4;
pub const PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW: u32 = 1 << 5;

/// Per-row raster metadata consumed directly from resident device storage.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PgaccelResidentRasterRow {
    pub width: u32,
    pub height: u32,
    pub first_band: u32,
    pub band_count: u32,
    pub srid: i32,
    pub flags: u32,
    pub scale_x: f64,
    pub scale_y: f64,
    pub ip_x: f64,
    pub ip_y: f64,
    pub skew_x: f64,
    pub skew_y: f64,
}

/// Per-band raster metadata consumed directly from resident device storage.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PgaccelResidentRasterBand {
    pub pixel_type: u32,
    pub flags: u32,
    pub nodata: f64,
}

/// Exact device pointer/span view over one resident raster column.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelResidentRasterView {
    pub abi_version: u32,
    pub flags: u32,
    pub pixels: *const u8,
    pub pixels_bytes: usize,
    pub band_offsets: *const u64,
    pub band_offsets_bytes: usize,
    pub rows: *const PgaccelResidentRasterRow,
    pub rows_bytes: usize,
    pub bands: *const PgaccelResidentRasterBand,
    pub bands_bytes: usize,
    pub nulls: *const u8,
    pub nulls_bytes: usize,
    pub row_count: usize,
    pub band_count: usize,
}

impl Default for PgaccelResidentRasterView {
    fn default() -> Self {
        Self {
            abi_version: PGACCEL_RESIDENT_RASTER_ABI_VERSION,
            flags: 0,
            pixels: std::ptr::null(),
            pixels_bytes: 0,
            band_offsets: std::ptr::null(),
            band_offsets_bytes: 0,
            rows: std::ptr::null(),
            rows_bytes: 0,
            bands: std::ptr::null(),
            bands_bytes: 0,
            nulls: std::ptr::null(),
            nulls_bytes: 0,
            row_count: 0,
            band_count: 0,
        }
    }
}

/// One sorted, singular integer Reclass mapping.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PgaccelResidentRasterReclassRule {
    pub source: i64,
    pub destination: i64,
}

/// Caller-owned device validation output. The raw launch never copies this
/// allocation to host; the caller does so only after releasing the input-store
/// borrow and then passes the host value to the typed validation mapper.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PgaccelResidentRasterValidationScratch {
    pub failures: u32,
    pub pad: u32,
    pub first_output_offset: u64,
    pub last_output_offset: u64,
}

/// Exact resident Reclass launch descriptor. Every pointer names device or
/// shared USM; byte counts are spans/capacities, and output offsets are bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelRasterReclassResidentRequest {
    pub abi_version: u32,
    pub flags: u32,
    pub input: PgaccelResidentRasterView,
    pub first_row: usize,
    pub count: usize,
    pub output_pixel_type: u32,
    pub pad: u32,
    pub rules: *const PgaccelResidentRasterReclassRule,
    pub rules_bytes: usize,
    pub rule_count: usize,
    pub output_offsets: *const u64,
    pub output_offsets_bytes: usize,
    pub output_pixels: *mut u8,
    pub output_pixels_bytes: usize,
    pub row_actions: *mut u8,
    pub row_actions_bytes: usize,
    pub validation_scratch: *mut PgaccelResidentRasterValidationScratch,
    pub validation_scratch_bytes: usize,
    pub max_total_pixels: usize,
    pub max_chunk_pixels: usize,
}

impl Default for PgaccelRasterReclassResidentRequest {
    fn default() -> Self {
        Self {
            abi_version: PGACCEL_RESIDENT_RASTER_ABI_VERSION,
            flags: 0,
            input: PgaccelResidentRasterView::default(),
            first_row: 0,
            count: 0,
            output_pixel_type: PGACCEL_RESIDENT_RASTER_UINT8,
            pad: 0,
            rules: std::ptr::null(),
            rules_bytes: 0,
            rule_count: 0,
            output_offsets: std::ptr::null(),
            output_offsets_bytes: 0,
            output_pixels: std::ptr::null_mut(),
            output_pixels_bytes: 0,
            row_actions: std::ptr::null_mut(),
            row_actions_bytes: 0,
            validation_scratch: std::ptr::null_mut(),
            validation_scratch_bytes: 0,
            max_total_pixels: 0,
            max_chunk_pixels: 0,
        }
    }
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

    #[test]
    fn resident_raster_constants_match_c_header() {
        assert_eq!(PGACCEL_RESIDENT_RASTER_ABI_VERSION, 1);
        assert_eq!(PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES, 64);
        assert_eq!(PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH, 65_536);
        assert_eq!(PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS, 4_096);
        assert_eq!(
            [
                PGACCEL_RESIDENT_RASTER_BOOL,
                PGACCEL_RESIDENT_RASTER_UINT2,
                PGACCEL_RESIDENT_RASTER_UINT4,
                PGACCEL_RESIDENT_RASTER_INT8,
                PGACCEL_RESIDENT_RASTER_UINT8,
                PGACCEL_RESIDENT_RASTER_INT16,
                PGACCEL_RESIDENT_RASTER_UINT16,
                PGACCEL_RESIDENT_RASTER_INT32,
                PGACCEL_RESIDENT_RASTER_UINT32,
                PGACCEL_RESIDENT_RASTER_FLOAT32,
                PGACCEL_RESIDENT_RASTER_FLOAT64,
            ],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 11]
        );
        assert_eq!(PGACCEL_RASTER_ROW_NULL, 0);
        assert_eq!(PGACCEL_RASTER_ROW_PASSTHROUGH, 1);
        assert_eq!(PGACCEL_RASTER_ROW_RECLASSIFIED, 2);
        assert_eq!(PGACCEL_RASTER_VALIDATION_VIEW, 1);
        assert_eq!(PGACCEL_RASTER_VALIDATION_RULES, 2);
        assert_eq!(PGACCEL_RASTER_VALIDATION_OFFSETS, 4);
        assert_eq!(PGACCEL_RASTER_VALIDATION_CAPACITY, 8);
        assert_eq!(PGACCEL_RASTER_VALIDATION_BYTE_BUDGET, 16);
        assert_eq!(PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW, 32);
    }

    #[test]
    fn resident_raster_row_band_and_view_layouts_match_c_header() {
        assert_eq!(std::mem::size_of::<PgaccelResidentRasterRow>(), 72);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, width), 0);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, height), 4);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterRow, first_band),
            8
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterRow, band_count),
            12
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, srid), 16);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, flags), 20);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, scale_x), 24);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, scale_y), 32);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, ip_x), 40);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, ip_y), 48);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, skew_x), 56);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterRow, skew_y), 64);

        assert_eq!(std::mem::size_of::<PgaccelResidentRasterBand>(), 16);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterBand, pixel_type),
            0
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterBand, flags), 4);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterBand, nodata), 8);

        assert_eq!(std::mem::size_of::<PgaccelResidentRasterView>(), 104);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, abi_version),
            0
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterView, flags), 4);
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterView, pixels), 8);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, pixels_bytes),
            16
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, band_offsets),
            24
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, band_offsets_bytes),
            32
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterView, rows), 40);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, rows_bytes),
            48
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterView, bands), 56);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, bands_bytes),
            64
        );
        assert_eq!(std::mem::offset_of!(PgaccelResidentRasterView, nulls), 72);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, nulls_bytes),
            80
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, row_count),
            88
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterView, band_count),
            96
        );
    }

    #[test]
    fn resident_raster_rule_scratch_and_request_layouts_match_c_header() {
        assert_eq!(std::mem::size_of::<PgaccelResidentRasterReclassRule>(), 16);
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterReclassRule, source),
            0
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterReclassRule, destination),
            8
        );

        assert_eq!(
            std::mem::size_of::<PgaccelResidentRasterValidationScratch>(),
            24
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterValidationScratch, failures),
            0
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterValidationScratch, pad),
            4
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterValidationScratch, first_output_offset),
            8
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelResidentRasterValidationScratch, last_output_offset),
            16
        );

        assert_eq!(
            std::mem::size_of::<PgaccelRasterReclassResidentRequest>(),
            240
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, abi_version),
            0
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, flags),
            4
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, input),
            8
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, first_row),
            112
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, count),
            120
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, output_pixel_type),
            128
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, pad),
            132
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, rules),
            136
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, rules_bytes),
            144
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, rule_count),
            152
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, output_offsets),
            160
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, output_offsets_bytes),
            168
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, output_pixels),
            176
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, output_pixels_bytes),
            184
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, row_actions),
            192
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, row_actions_bytes),
            200
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, validation_scratch),
            208
        );
        assert_eq!(
            std::mem::offset_of!(
                PgaccelRasterReclassResidentRequest,
                validation_scratch_bytes
            ),
            216
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, max_total_pixels),
            224
        );
        assert_eq!(
            std::mem::offset_of!(PgaccelRasterReclassResidentRequest, max_chunk_pixels),
            232
        );
    }
}

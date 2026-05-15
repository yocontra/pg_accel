//! Public adapter and function contract types.
//!
//! This module is intentionally data-only. Runtime registry lifecycle,
//! PostgreSQL catalog probing, locking, and retry behavior live in
//! `registry.rs`, so callers can understand the public declaration surface
//! without reading through mutable global-state mechanics.

/// Strategy that `pg_accel` applies when accelerating a function call.
///
/// All strategies require GPU hardware. There is no CPU-only fallback path.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccelStrategy {
    /// Offload spatial predicate evaluation to the GPU.
    GpuSpatial = 1,
    /// Offload raster map-algebra and similar operations to the GPU.
    GpuRaster = 2,
    /// Offload H3 cell computation to the GPU.
    GpuH3 = 3,
    /// GPU-accelerated sorting (e.g. radix sort on numeric keys).
    GpuSort = 4,
    /// GPU-accelerated reduction / aggregate (sum, avg, min, max, count).
    GpuReduce = 5,
    /// GPU expression evaluator - general WHERE clauses and projections.
    GpuExpr = 6,
    /// GPU hash join - equi-join via hash build + probe.
    GpuHashJoin = 7,
    /// GPU window functions - currently running SUM/COUNT over numeric windows.
    GpuWindow = 8,
    /// GPU NestedLoop scalar-inequality join (BETWEEN, range overlap, `<`,
    /// `<=`, `>=`, `>` between Var-Var across rels). Variant added in the
    /// Phase 4 NLJ kernel landing — see
    /// `pg_accel/src/gpu/nested_loop_ineq.rs` and
    /// `pgaccel-kernels/src/nested_loop_ineq.cpp`. The strategy is not
    /// planner-selectable yet: the kernel + bridge + cost model are in
    /// place but the executor node (both-sides slot deformation) is
    /// pending — see
    /// `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs`
    /// `selected_gpu_nlj_kernel_available()` which returns `false` and
    /// keeps `observe_nestloop_scalar_opportunity` recording the decline.
    GpuNestedLoopIneq = 9,
}

impl AccelStrategy {
    /// Convert from raw integer. Unknown values are invalid.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            1 => Some(Self::GpuSpatial),
            2 => Some(Self::GpuRaster),
            3 => Some(Self::GpuH3),
            4 => Some(Self::GpuSort),
            5 => Some(Self::GpuReduce),
            6 => Some(Self::GpuExpr),
            7 => Some(Self::GpuHashJoin),
            8 => Some(Self::GpuWindow),
            9 => Some(Self::GpuNestedLoopIneq),
            _ => None,
        }
    }
}

/// Shape of the per-input-row output produced by an accelerated function.
///
/// Most acceleratable functions are scalar - one Datum per input row
/// (`ST_Contains`, `h3_get_resolution`, `ST_Area`, ...). A handful return
/// multiple scalars per row (`ST_SummaryStats` returns
/// `(count, sum, mean, stddev, min, max)`) or a variable-length array per row
/// (H3 `grid_disk`, `polyfill`, `cell_to_boundary`, `cells_to_multi_polygon`).
///
/// Dispatch needs to know up front which of these three shapes a function
/// produces so it can allocate the right output buffer layout and pick the
/// right `DispatchResult` variant. Defaults to `Scalar` so existing single-
/// scalar entries don't need to opt in explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputShape {
    /// One scalar Datum per input row. Existing default.
    Scalar,
    /// `field_count` fixed scalars per input row, returned as a record/composite.
    /// Used by multi-scalar returns like `ST_SummaryStats(rast)` (6 fields).
    Record {
        /// Number of scalars per input row.
        field_count: u32,
    },
    /// CSR-style variable-length output - `offsets[N+1]` indexes into a flat
    /// `values` buffer. Used by H3 grid expansions (`grid_disk`, `polyfill`,
    /// `cell_to_boundary`, `cells_to_multi_polygon`) where each input row
    /// produces a different number of output cells/coordinates.
    VarLen,
}

impl Default for OutputShape {
    /// Default to `Scalar` so existing entries continue to compile via
    /// `..Default::default()` without changes to per-row semantics.
    fn default() -> Self {
        Self::Scalar
    }
}

/// A single SQL function that `pg_accel` knows how to accelerate.
#[derive(Debug, Clone)]
pub struct FunctionAccelEntry {
    /// Schema the function lives in (e.g. `"public"`, `"pg_catalog"`).
    pub schema: &'static str,
    /// Lower-case function name as it appears in `pg_proc`.
    pub name: &'static str,
    /// Acceleration strategy to apply.
    pub strategy: AccelStrategy,
    /// Shape of the per-input-row output. Defaults to [`OutputShape::Scalar`]
    /// (1 Datum per row); set explicitly for record-returning or variable-
    /// length-output kernels (e.g. `ST_SummaryStats`, H3 grid expansions).
    pub output_shape: OutputShape,
    /// PG type OIDs of the output column(s), in tuple-desc order. Required
    /// for the FunctionScan injection path (Phase 2 F3) so the executor can
    /// build a `TupleDesc` for record / var-length outputs without having to
    /// re-derive types via `pg_proc` lookup at exec time.
    ///
    /// - For [`OutputShape::Scalar`]: a single-OID Vec is sufficient (the
    ///   per-row return type) - left empty when the F3 FunctionScan path is
    ///   not the consumer (predicate / WHERE-clause injection sites read the
    ///   return type from `pg_proc` via `fmgr_info` instead).
    /// - For [`OutputShape::Record`] `{ field_count }`: must contain exactly
    ///   `field_count` entries (e.g. ST_SummaryStats: 6 INT8/FLOAT8 OIDs).
    /// - For [`OutputShape::VarLen`]: single-entry Vec describing the
    ///   per-output element type (e.g. `INT8OID` for h3index, `GSERIALIZED`
    ///   varlena for boundary geometries).
    pub output_field_types: Vec<u32>,
    /// Column names matching `output_field_types`, in the same positional
    /// order. Used by the FunctionScan TupleDesc builder. May be empty when
    /// `output_field_types` is empty (non-FunctionScan consumers).
    pub output_field_names: Vec<&'static str>,
}

impl FunctionAccelEntry {
    /// Construct a scalar-output entry. Convenience constructor that defaults
    /// `output_shape` to [`OutputShape::Scalar`] - used by the bulk of
    /// existing adapters (`ST_Contains`, `h3_get_resolution`, etc.) where
    /// every accelerated function produces exactly one Datum per input row.
    ///
    /// `output_field_types` and `output_field_names` are left empty here;
    /// scalar predicate/qual injection sites do not consume them. Add them
    /// explicitly via the struct literal when registering an entry that
    /// participates in the FunctionScan injection path (Phase 2 F3).
    #[must_use]
    pub const fn scalar(schema: &'static str, name: &'static str, strategy: AccelStrategy) -> Self {
        Self {
            schema,
            name,
            strategy,
            output_shape: OutputShape::Scalar,
            output_field_types: Vec::new(),
            output_field_names: Vec::new(),
        }
    }
}

/// An extension adapter that declares a set of acceleratable functions.
#[derive(Debug, Clone)]
pub struct ExtensionAdapter {
    /// Human-readable adapter name (e.g. `"postgis"`, `"h3"`).
    pub name: &'static str,
    /// Functions this adapter can accelerate.
    pub functions: Vec<FunctionAccelEntry>,
}

//! Acceleration registry types and runtime OID-based lookup.
//!
//! Defines the types used by extension adapters to declare which SQL functions
//! can be accelerated and what strategy should be used, plus the
//! [`AdapterRegistry`] that maps function OIDs to their entries at runtime.
//!
//! The global registry is lazily initialised on first planner hook invocation
//! (SPI is unavailable during `_PG_init`). See [`lazy_init`] and
//! [`global_registry`].

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::engine::function_matcher::{self, FunctionPattern};

/// Global singleton registry, populated on first use via [`lazy_init`].
static GLOBAL_REGISTRY: OnceLock<AdapterRegistry> = OnceLock::new();

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
    /// GPU expression evaluator �� general WHERE clauses and projections.
    GpuExpr = 6,
    /// GPU hash join ��� equi-join via hash build + probe.
    GpuHashJoin = 7,
    /// GPU window functions — ROW_NUMBER, RANK, SUM OVER, LAG/LEAD, etc.
    GpuWindow = 8,
}

impl AccelStrategy {
    /// Convert from raw integer, defaulting to `GpuSpatial` for unknown values.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            2 => Self::GpuRaster,
            3 => Self::GpuH3,
            4 => Self::GpuSort,
            5 => Self::GpuReduce,
            6 => Self::GpuExpr,
            7 => Self::GpuHashJoin,
            8 => Self::GpuWindow,
            _ => Self::GpuSpatial,
        }
    }
}

/// Shape of the per-input-row output produced by an accelerated function.
///
/// Most acceleratable functions are scalar — one Datum per input row
/// (`ST_Contains`, `h3_get_resolution`, `ST_Area`, …). A handful return
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
    /// CSR-style variable-length output — `offsets[N+1]` indexes into a flat
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
    ///   per-row return type) — left empty when the F3 FunctionScan path is
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
    /// `output_shape` to [`OutputShape::Scalar`] — used by the bulk of
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
    /// SQL query that, when executed, returns the extension version.
    /// Used to detect whether the extension is installed.
    pub version_query: &'static str,
    /// Functions this adapter can accelerate.
    pub functions: Vec<FunctionAccelEntry>,
}

/// Global registry of acceleratable functions, keyed by OID.
///
/// Populated during extension loading or lazily on first query, by probing
/// `pg_extension` to discover which supported extensions are installed.
pub struct AdapterRegistry {
    by_oid: HashMap<pgrx::pg_sys::Oid, FunctionAccelEntry>,
    adapters: Vec<ExtensionAdapter>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_oid: HashMap::new(),
            adapters: Vec::new(),
        }
    }

    /// Initialise adapters by probing which extensions are installed.
    ///
    /// Iterates over all known adapter constructors, runs each adapter's
    /// `version_query` via SPI to determine whether the backing extension is
    /// present, and registers those that are.
    pub fn init_adapters(&mut self) {
        let all_adapters = vec![
            crate::adapters::postgis::adapter(),
            crate::adapters::postgis_raster::adapter(),
            crate::adapters::h3::adapter(),
        ];

        for adapter in all_adapters {
            if check_extension_installed(&adapter) {
                pgrx::log!("pg_accel: activated adapter '{}'", adapter.name);
                self.register_adapter(adapter);
            } else {
                pgrx::debug1!(
                    "pg_accel: skipping adapter '{}' (extension not found)",
                    adapter.name
                );
            }
        }

        // Resolve function names → OIDs via pg_proc queries.
        self.resolve_oids();
    }

    /// Register an adapter and its function entries.
    ///
    /// OID resolution is deferred; entries are stored by name until
    /// `resolve_oids` is called.
    pub fn register_adapter(&mut self, adapter: ExtensionAdapter) {
        self.adapters.push(adapter);
    }

    /// Register a single function entry with a known OID.
    pub fn register_function(&mut self, oid: pgrx::pg_sys::Oid, entry: FunctionAccelEntry) {
        self.by_oid.insert(oid, entry);
    }

    /// Resolve OIDs for all registered adapters by querying `pg_proc`.
    ///
    /// For each function declared by each adapter, builds a
    /// [`FunctionPattern`] and calls [`function_matcher::discover_functions`]
    /// via SPI. Discovered OIDs are inserted into the `by_oid` map.
    ///
    /// Must be called within a transactional context (SPI available).
    pub fn resolve_oids(&mut self) {
        for adapter in &self.adapters {
            for func in &adapter.functions {
                let pattern = FunctionPattern {
                    schema: Some(func.schema.to_string()),
                    name: func.name.to_string(),
                    arg_types: None,
                    return_type: None,
                };

                let matches = function_matcher::discover_functions(&pattern);

                for matched in matches {
                    pgrx::debug1!(
                        "pg_accel: resolved {}.{} → OID {} (strategy {:?})",
                        matched.schema,
                        matched.name,
                        matched.oid.to_u32(),
                        func.strategy,
                    );
                    self.by_oid.insert(
                        matched.oid,
                        FunctionAccelEntry {
                            schema: func.schema,
                            name: func.name,
                            strategy: func.strategy,
                            output_shape: func.output_shape,
                            output_field_types: func.output_field_types.clone(),
                            output_field_names: func.output_field_names.clone(),
                        },
                    );
                }
            }
        }

        pgrx::log!(
            "pg_accel: resolved {} function OIDs across {} adapters",
            self.by_oid.len(),
            self.adapters.len(),
        );
    }

    /// O(1) lookup by function OID.
    #[must_use]
    pub fn lookup(&self, oid: pgrx::pg_sys::Oid) -> Option<&FunctionAccelEntry> {
        self.by_oid.get(&oid)
    }

    /// Whether the registry has any resolved function entries.
    /// Used as a fast-reject in planner hooks: if no extensions are
    /// installed, we can skip clause walking entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_oid.is_empty()
    }

    /// Number of OID-resolved function entries.
    #[must_use]
    pub fn resolved_count(&self) -> usize {
        self.by_oid.len()
    }

    /// Number of registered adapters.
    #[must_use]
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    /// Iterate over registered adapters.
    #[must_use]
    pub fn adapters(&self) -> &[ExtensionAdapter] {
        &self.adapters
    }
}

// ---------------------------------------------------------------------------
// Extension detection
// ---------------------------------------------------------------------------

/// Check whether an adapter's backing extension is installed by querying
/// the `pg_extension` system catalog.
///
/// This avoids calling version functions (e.g. `postgis_version()`) that
/// would raise a PostgreSQL ERROR if the extension is not installed —
/// pgrx converts those errors to panics which abort the server when
/// they occur inside a planner hook.
///
/// Returns `true` if the adapter's extension is installed in `pg_extension`.
fn check_extension_installed(adapter: &ExtensionAdapter) -> bool {
    // Adapter name matches extension name in pg_extension.
    let ext_name = adapter.name;

    let query = format!("SELECT 1 FROM pg_extension WHERE extname = '{ext_name}'");

    pgrx::Spi::connect(|client| {
        client
            .select(&query, None, &[])
            .map(|table| !table.is_empty())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Global registry access
// ---------------------------------------------------------------------------

// Guard against re-entrant calls to `lazy_init`.
// lazy_init → SPI → planner → hook → lazy_init would deadlock or
// panic on the OnceLock. The thread-local flag breaks the cycle by
// making the recursive call a no-op.
thread_local! {
    static INITIALIZING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Ensure the global registry is initialised exactly once.
///
/// Must be called from a transactional context (e.g. a planner hook) so
/// that SPI is available for extension detection queries.
///
/// Re-entrant calls (via SPI triggering the planner hook again) are
/// detected and short-circuited to avoid deadlock.
pub fn lazy_init() {
    if GLOBAL_REGISTRY.get().is_some() {
        return;
    }
    if INITIALIZING.with(std::cell::Cell::get) {
        return;
    }
    INITIALIZING.with(|f| f.set(true));

    // Wrap in catch_unwind because init_adapters() uses SPI queries
    // (e.g. `SELECT postgis_version()`) that can trigger PostgreSQL
    // ERRORs. pgrx converts those to panics, and if the panic
    // propagates through the C planner hook frame, PG aborts with
    // "failed to initiate panic". Catching here keeps PG alive —
    // the registry will be empty (no acceleration) but queries work.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        GLOBAL_REGISTRY.get_or_init(|| {
            let mut registry = AdapterRegistry::new();
            registry.init_adapters();
            registry
        });
    }));

    if result.is_err() {
        // Initialization panicked (SPI error, missing extension, etc.).
        // Install an empty registry so is_ready() returns true and we
        // don't retry on every query.
        GLOBAL_REGISTRY.get_or_init(|| {
            pgrx::warning!("pg_accel: adapter initialisation failed, running with no acceleration");
            AdapterRegistry::new()
        });
    }

    INITIALIZING.with(|f| f.set(false));
}

/// Whether the global registry has been successfully initialised.
///
/// Use this in planner hooks to early-return if `lazy_init` was called
/// but could not complete (e.g. during re-entrant SPI initialisation).
#[must_use]
pub fn is_ready() -> bool {
    GLOBAL_REGISTRY.get().is_some()
}

/// Return a reference to the global adapter registry.
///
/// # Panics
///
/// Panics if called before [`lazy_init`]. In normal operation the planner
/// hook guarantees initialisation before any lookup.
#[must_use]
#[allow(clippy::expect_used)] // Intentional panic: programming error if _PG_init path skipped.
pub fn global_registry() -> &'static AdapterRegistry {
    GLOBAL_REGISTRY
        .get()
        .expect("pg_accel: global registry not initialised — lazy_init was not called")
}

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_entries() {
        let reg = AdapterRegistry::new();
        assert_eq!(reg.resolved_count(), 0);
        assert_eq!(reg.adapter_count(), 0);
    }

    #[test]
    fn register_and_lookup_by_oid() {
        let mut reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(42_u32);
        let entry = FunctionAccelEntry::scalar("public", "st_contains", AccelStrategy::GpuSpatial);
        reg.register_function(oid, entry);
        let found = reg.lookup(oid);
        assert!(found.is_some());
        assert_eq!(found.map(|e| e.strategy), Some(AccelStrategy::GpuSpatial));
        assert_eq!(found.map(|e| e.output_shape), Some(OutputShape::Scalar));
    }

    #[test]
    fn lookup_missing_oid_returns_none() {
        let reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(9999_u32);
        assert!(reg.lookup(oid).is_none());
    }

    #[test]
    fn register_adapter_increments_count() {
        let mut reg = AdapterRegistry::new();
        reg.register_adapter(ExtensionAdapter {
            name: "PostGIS",
            version_query: "SELECT postgis_version()",
            functions: vec![FunctionAccelEntry::scalar(
                "public",
                "st_intersects",
                AccelStrategy::GpuSpatial,
            )],
        });
        assert_eq!(reg.adapter_count(), 1);
    }

    #[test]
    fn default_is_empty() {
        let reg = AdapterRegistry::default();
        assert_eq!(reg.resolved_count(), 0);
        assert_eq!(reg.adapter_count(), 0);
    }

    #[test]
    fn register_function_overwrites_duplicate_oid() {
        let mut reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(100_u32);

        reg.register_function(
            oid,
            FunctionAccelEntry::scalar("public", "func_a", AccelStrategy::GpuSpatial),
        );
        assert_eq!(reg.resolved_count(), 1);
        assert_eq!(reg.lookup(oid).unwrap().strategy, AccelStrategy::GpuSpatial);

        // Re-register same OID with different entry
        reg.register_function(
            oid,
            FunctionAccelEntry::scalar("public", "func_b", AccelStrategy::GpuH3),
        );
        // Count stays at 1 (overwrite, not duplicate)
        assert_eq!(reg.resolved_count(), 1);
        assert_eq!(reg.lookup(oid).unwrap().name, "func_b");
        assert_eq!(reg.lookup(oid).unwrap().strategy, AccelStrategy::GpuH3);
    }

    #[test]
    fn multiple_functions_independent_lookup() {
        let mut reg = AdapterRegistry::new();
        let oid1 = pgrx::pg_sys::Oid::from(10_u32);
        let oid2 = pgrx::pg_sys::Oid::from(20_u32);
        let oid3 = pgrx::pg_sys::Oid::from(30_u32);

        reg.register_function(
            oid1,
            FunctionAccelEntry::scalar("public", "f1", AccelStrategy::GpuSpatial),
        );
        reg.register_function(
            oid2,
            FunctionAccelEntry::scalar("public", "f2", AccelStrategy::GpuRaster),
        );
        reg.register_function(
            oid3,
            FunctionAccelEntry::scalar("pg_catalog", "f3", AccelStrategy::GpuSort),
        );

        assert_eq!(reg.resolved_count(), 3);
        assert_eq!(reg.lookup(oid1).unwrap().name, "f1");
        assert_eq!(reg.lookup(oid2).unwrap().name, "f2");
        assert_eq!(reg.lookup(oid3).unwrap().name, "f3");
        assert!(reg.lookup(pgrx::pg_sys::Oid::from(999_u32)).is_none());
    }

    #[test]
    fn adapters_returns_registered_adapters() {
        let mut reg = AdapterRegistry::new();
        assert!(reg.adapters().is_empty());

        reg.register_adapter(ExtensionAdapter {
            name: "test_ext",
            version_query: "SELECT 1",
            functions: vec![],
        });
        reg.register_adapter(ExtensionAdapter {
            name: "another_ext",
            version_query: "SELECT 2",
            functions: vec![
                FunctionAccelEntry::scalar("public", "fn1", AccelStrategy::GpuReduce),
                FunctionAccelEntry::scalar("public", "fn2", AccelStrategy::GpuH3),
            ],
        });

        assert_eq!(reg.adapter_count(), 2);
        assert_eq!(reg.adapters().len(), 2);
        assert_eq!(reg.adapters()[0].name, "test_ext");
        assert_eq!(reg.adapters()[1].name, "another_ext");
        assert_eq!(reg.adapters()[1].functions.len(), 2);
    }

    #[test]
    fn adapter_with_empty_functions_list() {
        let mut reg = AdapterRegistry::new();
        reg.register_adapter(ExtensionAdapter {
            name: "empty_adapter",
            version_query: "SELECT version()",
            functions: vec![],
        });
        assert_eq!(reg.adapter_count(), 1);
        // No OIDs resolved since no functions declared
        assert_eq!(reg.resolved_count(), 0);
    }

    #[test]
    fn accel_strategy_equality() {
        assert_eq!(AccelStrategy::GpuSpatial, AccelStrategy::GpuSpatial);
        assert_ne!(AccelStrategy::GpuSpatial, AccelStrategy::GpuH3);
        assert_ne!(AccelStrategy::GpuRaster, AccelStrategy::GpuH3);
        assert_ne!(AccelStrategy::GpuSort, AccelStrategy::GpuReduce);
    }

    #[test]
    fn accel_strategy_debug() {
        assert_eq!(format!("{:?}", AccelStrategy::GpuSpatial), "GpuSpatial");
        assert_eq!(format!("{:?}", AccelStrategy::GpuRaster), "GpuRaster");
        assert_eq!(format!("{:?}", AccelStrategy::GpuH3), "GpuH3");
        assert_eq!(format!("{:?}", AccelStrategy::GpuSort), "GpuSort");
        assert_eq!(format!("{:?}", AccelStrategy::GpuReduce), "GpuReduce");
    }

    #[test]
    fn accel_strategy_clone_copy() {
        let original = AccelStrategy::GpuSpatial;
        let cloned = original.clone();
        let copied = original;
        assert_eq!(original, cloned);
        assert_eq!(original, copied);
    }

    #[test]
    fn accel_strategy_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(AccelStrategy::GpuSpatial);
        set.insert(AccelStrategy::GpuH3);
        set.insert(AccelStrategy::GpuSpatial); // duplicate
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn function_accel_entry_debug_and_clone() {
        let entry =
            FunctionAccelEntry::scalar("public", "st_intersects", AccelStrategy::GpuSpatial);
        let cloned = entry.clone();
        assert_eq!(cloned.schema, "public");
        assert_eq!(cloned.name, "st_intersects");
        assert_eq!(cloned.strategy, AccelStrategy::GpuSpatial);
        assert_eq!(cloned.output_shape, OutputShape::Scalar);

        let debug = format!("{entry:?}");
        assert!(debug.contains("st_intersects"));
    }

    // -- OutputShape variants -----------------------------------------------

    #[test]
    fn output_shape_default_is_scalar() {
        let shape: OutputShape = OutputShape::default();
        assert_eq!(shape, OutputShape::Scalar);
    }

    #[test]
    fn output_shape_scalar_variant() {
        let s = OutputShape::Scalar;
        assert_eq!(s, OutputShape::Scalar);
        assert_ne!(s, OutputShape::VarLen);
    }

    #[test]
    fn output_shape_record_variant_carries_field_count() {
        let r = OutputShape::Record { field_count: 6 };
        match r {
            OutputShape::Record { field_count } => assert_eq!(field_count, 6),
            _ => panic!("expected Record variant"),
        }
        // Different field counts are not equal.
        assert_ne!(
            OutputShape::Record { field_count: 6 },
            OutputShape::Record { field_count: 7 }
        );
    }

    #[test]
    fn output_shape_varlen_variant() {
        let v = OutputShape::VarLen;
        assert_eq!(v, OutputShape::VarLen);
        assert_ne!(v, OutputShape::Scalar);
    }

    #[test]
    fn output_shape_copy_semantics() {
        let original = OutputShape::Record { field_count: 3 };
        let copied = original;
        assert_eq!(original, copied);
    }

    #[test]
    fn output_shape_debug_format() {
        assert!(format!("{:?}", OutputShape::Scalar).contains("Scalar"));
        assert!(format!("{:?}", OutputShape::VarLen).contains("VarLen"));
        let r = format!("{:?}", OutputShape::Record { field_count: 6 });
        assert!(r.contains("Record"));
        assert!(r.contains('6'));
    }

    // -- FunctionAccelEntry constructors -------------------------------------

    #[test]
    fn function_accel_entry_default_is_scalar() {
        // The convenience constructor must default output_shape to Scalar so
        // every existing adapter entry keeps the per-row scalar contract.
        let e = FunctionAccelEntry::scalar("public", "st_contains", AccelStrategy::GpuSpatial);
        assert_eq!(e.schema, "public");
        assert_eq!(e.name, "st_contains");
        assert_eq!(e.strategy, AccelStrategy::GpuSpatial);
        assert_eq!(e.output_shape, OutputShape::Scalar);
    }

    #[test]
    fn function_accel_entry_record_output_shape() {
        // Record-shaped entries (e.g. ST_SummaryStats with 6 scalar fields)
        // are constructed via the struct literal, not the `scalar` shortcut.
        let e = FunctionAccelEntry {
            schema: "public",
            name: "st_summarystats",
            strategy: AccelStrategy::GpuRaster,
            output_shape: OutputShape::Record { field_count: 6 },
        };
        assert_eq!(e.output_shape, OutputShape::Record { field_count: 6 });
    }

    #[test]
    fn function_accel_entry_varlen_output_shape() {
        let e = FunctionAccelEntry {
            schema: "public",
            name: "h3_grid_disk",
            strategy: AccelStrategy::GpuH3,
            output_shape: OutputShape::VarLen,
            output_field_types: Vec::new(),
            output_field_names: Vec::new(),
        };
        assert_eq!(e.output_shape, OutputShape::VarLen);
    }

    // -- output_field_types / output_field_names (Phase 2 F3 metadata) -------

    #[test]
    fn function_accel_entry_scalar_constructor_leaves_field_metadata_empty() {
        // The bulk of registered entries (predicate / qual injection sites)
        // do not consume FunctionScan TupleDesc metadata. The convenience
        // constructor must therefore default both vectors to empty.
        let e = FunctionAccelEntry::scalar("public", "st_contains", AccelStrategy::GpuSpatial);
        assert!(e.output_field_types.is_empty());
        assert!(e.output_field_names.is_empty());
    }

    #[test]
    fn function_accel_entry_record_metadata_round_trips() {
        // Records the F3 metadata for ST_SummaryStats: 6 fp64 fields named
        // count / sum / mean / stddev / min / max.
        let e = FunctionAccelEntry {
            schema: "public",
            name: "st_summarystats",
            strategy: AccelStrategy::GpuRaster,
            output_shape: OutputShape::Record { field_count: 6 },
            output_field_types: vec![
                pgrx::pg_sys::INT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
            ],
            output_field_names: vec!["count", "sum", "mean", "stddev", "min", "max"],
        };
        assert_eq!(e.output_field_types.len(), 6);
        assert_eq!(e.output_field_names.len(), 6);
        // Cloning must preserve the vectors.
        let cloned = e.clone();
        assert_eq!(cloned.output_field_types, e.output_field_types);
        assert_eq!(cloned.output_field_names, e.output_field_names);
    }

    #[test]
    fn function_accel_entry_varlen_metadata_round_trips() {
        let e = FunctionAccelEntry {
            schema: "public",
            name: "h3_polyfill",
            strategy: AccelStrategy::GpuH3,
            output_shape: OutputShape::VarLen,
            output_field_types: vec![pgrx::pg_sys::INT8OID.to_u32()],
            output_field_names: vec!["cell"],
        };
        assert_eq!(e.output_field_types, vec![pgrx::pg_sys::INT8OID.to_u32()]);
        assert_eq!(e.output_field_names, vec!["cell"]);
    }
}

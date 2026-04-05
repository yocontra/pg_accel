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

/// A single SQL function that `pg_accel` knows how to accelerate.
#[derive(Debug, Clone)]
pub struct FunctionAccelEntry {
    /// Schema the function lives in (e.g. `"public"`, `"pg_catalog"`).
    pub schema: &'static str,
    /// Lower-case function name as it appears in `pg_proc`.
    pub name: &'static str,
    /// Acceleration strategy to apply.
    pub strategy: AccelStrategy,
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
        let entry = FunctionAccelEntry {
            schema: "public",
            name: "st_contains",
            strategy: AccelStrategy::GpuSpatial,
        };
        reg.register_function(oid, entry);
        let found = reg.lookup(oid);
        assert!(found.is_some());
        assert_eq!(found.map(|e| e.strategy), Some(AccelStrategy::GpuSpatial));
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
            functions: vec![FunctionAccelEntry {
                schema: "public",
                name: "st_intersects",
                strategy: AccelStrategy::GpuSpatial,
            }],
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
            FunctionAccelEntry {
                schema: "public",
                name: "func_a",
                strategy: AccelStrategy::GpuSpatial,
            },
        );
        assert_eq!(reg.resolved_count(), 1);
        assert_eq!(reg.lookup(oid).unwrap().strategy, AccelStrategy::GpuSpatial);

        // Re-register same OID with different entry
        reg.register_function(
            oid,
            FunctionAccelEntry {
                schema: "public",
                name: "func_b",
                strategy: AccelStrategy::GpuH3,
            },
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
            FunctionAccelEntry {
                schema: "public",
                name: "f1",
                strategy: AccelStrategy::GpuSpatial,
            },
        );
        reg.register_function(
            oid2,
            FunctionAccelEntry {
                schema: "public",
                name: "f2",
                strategy: AccelStrategy::GpuRaster,
            },
        );
        reg.register_function(
            oid3,
            FunctionAccelEntry {
                schema: "pg_catalog",
                name: "f3",
                strategy: AccelStrategy::GpuSort,
            },
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
                FunctionAccelEntry {
                    schema: "public",
                    name: "fn1",
                    strategy: AccelStrategy::GpuReduce,
                },
                FunctionAccelEntry {
                    schema: "public",
                    name: "fn2",
                    strategy: AccelStrategy::GpuH3,
                },
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
        let entry = FunctionAccelEntry {
            schema: "public",
            name: "st_intersects",
            strategy: AccelStrategy::GpuSpatial,
        };
        let cloned = entry.clone();
        assert_eq!(cloned.schema, "public");
        assert_eq!(cloned.name, "st_intersects");
        assert_eq!(cloned.strategy, AccelStrategy::GpuSpatial);

        let debug = format!("{entry:?}");
        assert!(debug.contains("st_intersects"));
    }
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccelStrategy {
    /// Evaluate multiple rows in a tight loop on the main backend thread,
    /// avoiding repeated executor overhead.
    BatchedEval,
    /// Offload spatial predicate evaluation to the GPU.
    GpuSpatial,
    /// Offload raster map-algebra and similar operations to the GPU.
    GpuRaster,
    /// Offload H3 cell computation to the GPU.
    GpuH3,
    /// GPU-accelerated sorting (e.g. radix sort on numeric keys).
    GpuSort,
    /// GPU-accelerated reduction / aggregate (sum, avg, min, max, count).
    GpuReduce,
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
            crate::adapters::pg_builtins::adapter(),
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

/// Run an adapter's `version_query` via SPI to check whether its backing
/// extension is installed.
///
/// Returns `true` if the query executes without error. Errors (e.g. unknown
/// function) are caught and treated as "extension not installed".
fn check_extension_installed(adapter: &ExtensionAdapter) -> bool {
    // SPI::connect executes within the current transaction context.
    pgrx::Spi::connect(|client| {
        client
            .select(adapter.version_query, None, &[])
            .map(|_table| true)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Global registry access
// ---------------------------------------------------------------------------

/// Ensure the global registry is initialised exactly once.
///
/// Must be called from a transactional context (e.g. a planner hook) so
/// that SPI is available for extension detection queries.
pub fn lazy_init() {
    GLOBAL_REGISTRY.get_or_init(|| {
        let mut registry = AdapterRegistry::new();
        registry.init_adapters();
        registry
    });
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

#[cfg(test)]
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
}

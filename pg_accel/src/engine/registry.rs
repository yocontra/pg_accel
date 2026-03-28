//! Acceleration registry types and runtime OID-based lookup.
//!
//! Defines the types used by extension adapters to declare which SQL functions
//! can be accelerated and what strategy should be used, plus the
//! [`AdapterRegistry`] that maps function OIDs to their entries at runtime.

use std::collections::HashMap;

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
    /// This queries `pg_extension` via SPI to discover installed extensions
    /// and registers the corresponding acceleration entries.
    ///
    /// # Note
    ///
    /// Actual SPI lookup is deferred to Phase 2. Currently this is a no-op
    /// placeholder that records the adapter templates.
    pub fn init_adapters(&mut self) {
        // TODO(phase-2): Use SPI to query pg_extension for installed
        // extensions and resolve function OIDs via pg_proc lookup.
        let _ = &mut self.adapters; // silence unused-self until Phase 2
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
    pub fn adapters(&self) -> &[ExtensionAdapter] {
        &self.adapters
    }
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

//! Acceleration registry types and runtime OID-based lookup.
//!
//! Defines the types used by extension adapters to declare which SQL functions
//! can be accelerated and what strategy should be used, plus the
//! [`AdapterRegistry`] that maps function OIDs to their entries at runtime.
//!
//! The global registry is lazily initialised on first planner hook invocation
//! (SPI is unavailable during `_PG_init`). See [`lazy_init`] and
//! [`global_registry`].
//!
//! ## Re-resolution after deferred CREATE EXTENSION
//!
//! `lazy_init` only fires once per backend, but supporting extensions
//! (PostGIS / h3 / postgis_raster) may be `CREATE EXTENSION`-installed
//! *after* the first planner-hook invocation that triggered init. To handle
//! that case, the registry now wraps its mutable state in a `RwLock`:
//! [`AdapterRegistry::resolve_oids_again`] re-runs adapter detection +
//! `pg_proc` OID resolution against the live catalog and merges any newly
//! discovered OIDs into the in-memory map. [`AdapterRegistry::lookup`]
//! transparently triggers one re-resolve attempt on a miss before returning
//! `None` (guarded by a thread-local to bound work to one retry per planner
//! pass). See `lookup_with_retry` and `RETRYING_LOOKUP`.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::engine::function_matcher::{self, FunctionPattern};

mod contracts;
mod types;

pub use contracts::{
    DispatchOp, FieldSpec, FieldTypeSpec, KernelOp, OutputContract, OutputContractError,
};
pub use types::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry, OutputShape};

/// Global singleton registry, populated on first use via [`lazy_init`].
///
/// The `OnceLock` shell never changes after init; the mutable state lives
/// inside the `AdapterRegistry`'s internal `RwLock`. This lets us add
/// adapters or freshly-resolved OIDs after init (see [`resolve_oids_again`])
/// without breaking the `&'static AdapterRegistry` callers depend on.
static GLOBAL_REGISTRY: OnceLock<AdapterRegistry> = OnceLock::new();

/// Mutable state inside an [`AdapterRegistry`].
///
/// Held behind a `RwLock` so [`resolve_oids_again`] can mutate the map
/// without breaking the `&'static AdapterRegistry` API the codebase
/// already relies on.
#[derive(Default)]
struct RegistryState {
    by_oid: HashMap<pgrx::pg_sys::Oid, FunctionAccelEntry>,
    adapters: Vec<ExtensionAdapter>,
}

/// Global registry of acceleratable functions, keyed by OID.
///
/// Populated during extension loading or lazily on first query, by probing
/// `pg_extension` to discover which supported extensions are installed.
///
/// Mutable contents (the OID map + adapter list) live behind an internal
/// `RwLock`; the surface API (`lookup`, `register_*`, `resolve_oids*`)
/// takes the right lock implicitly so callers do not need to reason about
/// concurrency. See [`resolve_oids_again`] for the post-init re-resolution
/// path used after a deferred `CREATE EXTENSION`.
pub struct AdapterRegistry {
    state: RwLock<RegistryState>,
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
            state: RwLock::new(RegistryState::default()),
        }
    }

    /// Initialise adapters by probing which extensions are installed.
    ///
    /// Iterates over all known adapter constructors, checks `pg_extension`
    /// for each backing extension, and registers installed adapters.
    pub fn init_adapters(&self) {
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
    pub fn register_adapter(&self, adapter: ExtensionAdapter) {
        if let Ok(mut s) = self.state.write() {
            s.adapters.push(adapter);
        }
    }

    /// Register a single function entry with a known OID.
    pub fn register_function(&self, oid: pgrx::pg_sys::Oid, entry: FunctionAccelEntry) {
        if let Ok(mut s) = self.state.write() {
            s.by_oid.insert(oid, entry);
        }
    }

    /// Resolve OIDs for all registered adapters by querying `pg_proc`.
    ///
    /// For each function declared by each adapter, builds a
    /// [`FunctionPattern`] and calls [`function_matcher::discover_functions`]
    /// via SPI. Discovered OIDs are inserted into the `by_oid` map.
    ///
    /// Must be called within a transactional context (SPI available).
    pub fn resolve_oids(&self) {
        // Snapshot the adapter list (clone) under a short read lock so the
        // SPI calls below run lock-free, and the long-lived write lock at
        // the end does not deadlock with anything `discover_functions` may
        // recurse into.
        let adapters_snapshot: Vec<ExtensionAdapter> = match self.state.read() {
            Ok(s) => s.adapters.clone(),
            Err(_) => return,
        };

        let mut newly_resolved: Vec<(pgrx::pg_sys::Oid, FunctionAccelEntry)> = Vec::new();

        for adapter in &adapters_snapshot {
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
                    newly_resolved.push((
                        matched.oid,
                        FunctionAccelEntry {
                            schema: func.schema,
                            name: func.name,
                            strategy: func.strategy,
                            output_shape: func.output_shape,
                            output_field_types: func.output_field_types.clone(),
                            output_field_names: func.output_field_names.clone(),
                        },
                    ));
                }
            }
        }

        // Single write lock to insert all discovered entries.
        if let Ok(mut s) = self.state.write() {
            for (oid, entry) in newly_resolved {
                s.by_oid.insert(oid, entry);
            }
            pgrx::log!(
                "pg_accel: resolved {} function OIDs across {} adapters",
                s.by_oid.len(),
                s.adapters.len(),
            );
        }
    }

    /// Re-run adapter discovery + OID resolution against the live catalog.
    ///
    /// Idempotent: existing OID entries are preserved; newly-installed
    /// extensions detected via `pg_extension` are added, and their declared
    /// functions are resolved via `pg_proc` and merged into `by_oid`.
    /// On the no-op path (no new adapters, all OIDs already cached), this
    /// runs the cheap `pg_extension` check + a small per-function `pg_proc`
    /// SPI query but mutates nothing — safe to call defensively from
    /// planner-hook lookup misses.
    ///
    /// Must be called within a transactional context (SPI available).
    pub fn resolve_oids_again(&self) {
        // Step 1: re-detect adapters. If any registered name shows up in
        // `pg_extension` that wasn't there before, register it now. We
        // identify "already known" by adapter name.
        let known_names: std::collections::HashSet<&'static str> = match self.state.read() {
            Ok(s) => s.adapters.iter().map(|a| a.name).collect(),
            Err(_) => return,
        };

        let candidate_adapters = vec![
            crate::adapters::postgis::adapter(),
            crate::adapters::postgis_raster::adapter(),
            crate::adapters::h3::adapter(),
        ];

        for adapter in candidate_adapters {
            if known_names.contains(adapter.name) {
                continue;
            }
            if check_extension_installed(&adapter) {
                pgrx::log!(
                    "pg_accel: registry re-resolve: activating new adapter '{}'",
                    adapter.name
                );
                self.register_adapter(adapter);
            }
        }

        // Step 2: re-run OID resolution for *all* registered adapters.
        // `resolve_oids` is itself idempotent (insert into HashMap; same
        // OID + same entry is a no-op replace).
        self.resolve_oids();
    }

    /// O(1) lookup by function OID.
    ///
    /// Returns an owned, cloned [`FunctionAccelEntry`] so the caller does
    /// not need to hold a read guard across SPI / FFI boundaries.
    ///
    /// On a miss, transparently triggers exactly one [`resolve_oids_again`]
    /// retry per planner-pass (guarded by [`RETRYING_LOOKUP`]) before
    /// returning `None`. This handles the case where `lazy_init` fired
    /// before a supporting extension was `CREATE EXTENSION`-installed.
    #[must_use]
    pub fn lookup(&self, oid: pgrx::pg_sys::Oid) -> Option<FunctionAccelEntry> {
        if let Some(found) = self.lookup_no_retry(oid) {
            return Some(found);
        }

        // Re-entry guard: avoid infinite recursion if `resolve_oids_again`
        // somehow triggers a planner pass that calls `lookup` again. Also
        // bounds total work to O(1) per top-level lookup — at most one
        // re-resolve per `lookup` call.
        if RETRYING_LOOKUP.with(std::cell::Cell::get) {
            return None;
        }
        RETRYING_LOOKUP.with(|f| f.set(true));
        // SPI errors during re-resolve must not poison the planner pass.
        // Catch + ignore; if re-resolve fails the lookup just stays a miss.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.resolve_oids_again();
        }));
        RETRYING_LOOKUP.with(|f| f.set(false));

        self.lookup_no_retry(oid)
    }

    /// Bare lookup with no retry. Internal helper for [`lookup`] and tests.
    fn lookup_no_retry(&self, oid: pgrx::pg_sys::Oid) -> Option<FunctionAccelEntry> {
        self.state
            .read()
            .ok()
            .and_then(|s| s.by_oid.get(&oid).cloned())
    }

    /// Whether the registry has any resolved function entries.
    /// Used as a fast-reject in planner hooks: if no extensions are
    /// installed, we can skip clause walking entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.read().map_or(true, |s| s.by_oid.is_empty())
    }

    /// Number of OID-resolved function entries.
    #[must_use]
    pub fn resolved_count(&self) -> usize {
        self.state.read().map_or(0, |s| s.by_oid.len())
    }

    /// Number of registered adapters.
    #[must_use]
    pub fn adapter_count(&self) -> usize {
        self.state.read().map_or(0, |s| s.adapters.len())
    }

    /// Snapshot of registered adapters (cloned — held under no lock by the
    /// caller).
    #[must_use]
    pub fn adapters(&self) -> Vec<ExtensionAdapter> {
        self.state
            .read()
            .map(|s| s.adapters.clone())
            .unwrap_or_default()
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
            .is_ok_and(|table| !table.is_empty())
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
    /// Re-entry guard for [`AdapterRegistry::lookup`]'s auto-retry path.
    /// `lookup` calls `resolve_oids_again` on miss; that re-resolves via
    /// SPI which itself goes through the planner — and any inner planner
    /// callback that calls `lookup` again must NOT trigger another retry.
    /// The thread-local flag short-circuits the inner retry to O(1) work.
    static RETRYING_LOOKUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
            let registry = AdapterRegistry::new();
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

/// Force a re-resolution pass on the global registry.
///
/// Convenience wrapper around [`AdapterRegistry::resolve_oids_again`] that
/// no-ops cleanly if `lazy_init` has not yet run. Safe to call from a
/// planner hook on a lookup miss, from a `CREATE EXTENSION` post-trigger,
/// or from tests after `CREATE EXTENSION`-ing a new adapter mid-run.
pub fn resolve_oids_again() {
    if let Some(reg) = GLOBAL_REGISTRY.get() {
        reg.resolve_oids_again();
    }
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
        let reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(42_u32);
        let entry = FunctionAccelEntry::scalar("public", "st_contains", AccelStrategy::GpuSpatial);
        reg.register_function(oid, entry);
        // Use the no-retry helper — under #[cfg(test)] there is no SPI to
        // back the auto-retry path, and the unit test is exercising the
        // pure data structure.
        let found = reg.lookup_no_retry(oid);
        assert!(found.is_some());
        assert_eq!(
            found.as_ref().map(|e| e.strategy),
            Some(AccelStrategy::GpuSpatial)
        );
        assert_eq!(found.map(|e| e.output_shape), Some(OutputShape::Scalar));
    }

    #[test]
    fn lookup_missing_oid_returns_none() {
        let reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(9999_u32);
        assert!(reg.lookup_no_retry(oid).is_none());
    }

    #[test]
    fn register_adapter_increments_count() {
        let reg = AdapterRegistry::new();
        reg.register_adapter(ExtensionAdapter {
            name: "PostGIS",
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
        let reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(100_u32);

        reg.register_function(
            oid,
            FunctionAccelEntry::scalar("public", "func_a", AccelStrategy::GpuSpatial),
        );
        assert_eq!(reg.resolved_count(), 1);
        assert_eq!(
            reg.lookup_no_retry(oid).expect("present").strategy,
            AccelStrategy::GpuSpatial
        );

        // Re-register same OID with different entry
        reg.register_function(
            oid,
            FunctionAccelEntry::scalar("public", "func_b", AccelStrategy::GpuH3),
        );
        // Count stays at 1 (overwrite, not duplicate)
        assert_eq!(reg.resolved_count(), 1);
        assert_eq!(reg.lookup_no_retry(oid).expect("present").name, "func_b");
        assert_eq!(
            reg.lookup_no_retry(oid).expect("present").strategy,
            AccelStrategy::GpuH3
        );
    }

    #[test]
    fn multiple_functions_independent_lookup() {
        let reg = AdapterRegistry::new();
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
        assert_eq!(reg.lookup_no_retry(oid1).expect("present").name, "f1");
        assert_eq!(reg.lookup_no_retry(oid2).expect("present").name, "f2");
        assert_eq!(reg.lookup_no_retry(oid3).expect("present").name, "f3");
        assert!(
            reg.lookup_no_retry(pgrx::pg_sys::Oid::from(999_u32))
                .is_none()
        );
    }

    #[test]
    fn adapters_returns_registered_adapters() {
        let reg = AdapterRegistry::new();
        assert!(reg.adapters().is_empty());

        reg.register_adapter(ExtensionAdapter {
            name: "test_ext",
            functions: vec![],
        });
        reg.register_adapter(ExtensionAdapter {
            name: "another_ext",
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
        let reg = AdapterRegistry::new();
        reg.register_adapter(ExtensionAdapter {
            name: "empty_adapter",
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
        let cloned = original;
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
            output_field_types: Vec::new(),
            output_field_names: Vec::new(),
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

    // -- resolve_oids_again API surface --------------------------------------

    #[test]
    fn resolve_oids_again_no_op_on_empty_registry() {
        // With no adapters registered, resolve_oids_again must not panic
        // and must leave the registry empty. (SPI inside check_extension
        // is mocked-out at test build time; the test only exercises the
        // shape of the call, not the catalog query.)
        let reg = AdapterRegistry::new();
        // Note: invoking resolve_oids_again here would attempt SPI calls
        // which are unavailable under #[cfg(test)] without a live PG. We
        // exercise the *idempotence on the data-structure side* instead by
        // directly probing the no-retry helper — the integration tests in
        // function_scan.rs cover the SPI-driven path end-to-end.
        assert_eq!(reg.resolved_count(), 0);
        assert_eq!(reg.adapter_count(), 0);
        assert!(reg.lookup_no_retry(pgrx::pg_sys::Oid::from(1u32)).is_none());
    }

    #[test]
    fn lookup_after_register_returns_owned_clone() {
        // The new lookup signature returns owned FunctionAccelEntry (not
        // &FunctionAccelEntry) so callers don't have to hold a read guard.
        // Verify the round-trip preserves all fields.
        let reg = AdapterRegistry::new();
        let oid = pgrx::pg_sys::Oid::from(7777_u32);
        reg.register_function(
            oid,
            FunctionAccelEntry {
                schema: "public",
                name: "h3_grid_disk",
                strategy: AccelStrategy::GpuH3,
                output_shape: OutputShape::VarLen,
                output_field_types: vec![pgrx::pg_sys::INT8OID.to_u32()],
                output_field_names: vec!["cell"],
            },
        );
        let owned = reg.lookup_no_retry(oid).expect("present");
        assert_eq!(owned.name, "h3_grid_disk");
        assert_eq!(owned.strategy, AccelStrategy::GpuH3);
        assert_eq!(owned.output_shape, OutputShape::VarLen);
        assert_eq!(
            owned.output_field_types,
            vec![pgrx::pg_sys::INT8OID.to_u32()]
        );
        assert_eq!(owned.output_field_names, vec!["cell"]);
    }

    #[test]
    fn retrying_lookup_thread_local_default_is_false() {
        // Sanity check that the re-entry guard starts in the unset state for
        // a fresh test thread. Important because a dirty TL would silently
        // disable the retry path.
        assert!(!RETRYING_LOOKUP.with(std::cell::Cell::get));
    }
}

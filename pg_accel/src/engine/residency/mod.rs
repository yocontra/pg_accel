//! GPU residency contracts and backend-local resident data management.
//!
//! The v2 manager is split by responsibility: [`ledger`] owns cluster-wide
//! accounting and relation generations, [`loader`] performs synchronous SPI
//! snapshot loads, and [`store`] owns the backend-local two-tier LRU. The
//! proof types used by planner/executor boundaries remain in [`proof`].

mod domain;
mod geometry;
mod ledger;
mod loader;
mod proof;
mod store;

pub use domain::*;
pub(crate) use geometry::validate_resident_geometry_value;
pub use geometry::{ResidentGeometryColumn, ResidentGeometryColumnView};
pub use proof::*;
pub use store::{
    ArtifactEnsureOutcome, DerivedArtifact, DerivedArtifactIdentity, PreparedDerived,
    ResidentBudgetSnapshot, ResidentColumn, ResidentColumnRef, ResidentColumnView,
    ResidentDependencyStamp, ResidentInputBundle, ResidentKeyDecoder, ResidentLoadError,
    ResidentLoadEstimate, ResidentRelationEvidence, ResidentRelationStatus, ResolvedArtifactBundle,
    ResolvedDerivedInputs, SelectedRelation, SelectedRelationsEnsureOutcome,
    ensure_derived_artifact, ensure_device_derived_artifact, ensure_selected_relations,
    estimate_selected_relation, register_derived_artifact, resident_budget_snapshot,
    resident_live_bytes, shape_digest, with_derived_artifact, with_derived_artifact_inputs,
    with_resident_column, with_resolved_artifact,
};

/// Register residency shared memory. Must be called from `_PG_init`.
pub fn init_shmem() {
    ledger::init_shmem();
}

/// Release this backend's byte-ledger slot during `before_shmem_exit`.
pub fn cleanup_backend() {
    store::cleanup_backend();
    ledger::cleanup_backend();
}

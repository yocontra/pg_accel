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

#[cfg(test)]
pub(crate) use store::tests::{begin_test_allocation_count, finish_test_allocation_count};

#[cfg(feature = "pg_test")]
pub(crate) fn test_derived_artifact_identities(relid: pgrx::pg_sys::Oid) -> Vec<(u64, Vec<i32>)> {
    store::test_derived_artifact_identities(relid)
}

pub use domain::*;
pub use geometry::{ResidentGeometryColumn, ResidentGeometryColumnView};
pub(crate) use geometry::{
    materialize_resident_geometry_constant, validate_resident_geometry_value,
};
pub use proof::*;
pub use store::{
    ArtifactEnsureOutcome, ArtifactEnsureReport, ArtifactProbeOutcome, DerivedArtifact,
    DerivedArtifactIdentity, EphemeralArtifactBuildReport, EphemeralDerivedArtifact,
    PreparedDerived, ResidentBudgetSnapshot, ResidentColumn, ResidentColumnRef, ResidentColumnView,
    ResidentDependencyStamp, ResidentDispatchBundle, ResidentGeometryExactSnapshot,
    ResidentInputBundle, ResidentKeyDecoder, ResidentLoadError, ResidentLoadEstimate,
    ResidentRelationEvidence, ResidentRelationStatus, ResolvedArtifactBundle,
    ResolvedDerivedInputs, SelectedRelation, SelectedRelationsEnsureOutcome,
    StagedDerivedPreflight, StagedTransformPreflight, StagedTransformWorkspace,
    build_ephemeral_derived_artifact_with_report,
    build_ephemeral_staged_device_transform_artifact_with_report, ensure_derived_artifact,
    ensure_derived_artifact_with_report, ensure_device_derived_artifact,
    ensure_device_derived_artifact_with_report, ensure_selected_relations,
    ensure_staged_device_derived_artifact, ensure_staged_device_derived_artifact_with_report,
    ensure_staged_device_transform_artifact, ensure_staged_device_transform_artifact_with_report,
    estimate_selected_relation, probe_derived_artifact, register_derived_artifact,
    resident_budget_snapshot, resident_geometry_exact_snapshot_words, resident_live_bytes,
    resident_selected_relation_rows, revalidate_loaded_estimates, revalidate_planner_dependencies,
    shape_digest, snapshot_resident_geometry_exact, with_derived_artifact,
    with_derived_artifact_inputs, with_ephemeral_artifact_inputs, with_resident_column,
    with_resolved_artifact,
};
pub use store::{ResidentPlannerDependency, resident_planner_dependency};

/// Register residency shared memory. Must be called from `_PG_init`.
pub fn init_shmem() {
    ledger::init_shmem();
}

/// Release this backend's byte-ledger slot during `before_shmem_exit`.
pub fn cleanup_backend() {
    crate::gpu::cleanup_grouped_allocation_pools();
    store::cleanup_backend();
    ledger::cleanup_backend();
}

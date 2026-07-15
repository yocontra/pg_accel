//! Structured planner decision scaffolding.
//!
//! The planner hooks still use their existing early-return gates. This module
//! provides pure-Rust decision records that can be threaded through those gates
//! incrementally without changing planner behavior first.

#![allow(dead_code)]

use std::collections::VecDeque;

use crate::engine::registry::AccelStrategy;

const DEFAULT_RECORDER_CAPACITY: usize = 32;

/// Stable reason codes for planner declines.
///
/// The string returned by [`Self::stats_key`] is the value existing tracing and
/// stats code should use when a rejection is surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RejectionReason {
    ExtensionDisabled,
    UnsupportedCommandType,
    GpuNotUsable,
    UnsupportedRelationKind,
    UnsupportedRteKind,
    RowsBelowMinBatch,
    NoCheapestPath,
    NoAccelerableClause,
    CheaperNativePath,
    CostModelDeclined,
    HashJoinNoSelectedGpuKernel,
    HashJoinParallelInnerRebuildTooLarge,
    HashJoinHeapOutputTooLarge,
    SemiAntiNoGpuMembershipFilter,
    MergeJoinNoGpuKernel,
    NestedLoopScalarNoGpuKernel,
    NljBetweenHostBoundaryUnsafe,
    NljBetweenOutputTooLarge,
    PartialAggNoGpuProducingChild,
    PreAggNoGpuResidentPipeline,
    NoGpuResidentPipeline,
    GroupedAvgNoGpuFinalize,
    AggSemanticModifierNoGpuKernel,
    WindowPartialPathNoParallelHook,
    H3LateralSrfNoBatchedExpansion,
    H3LatLngUnsupportedShape,
    H3LatLngScalarPredicateNoGpuPipeline,
    SrfTlistMultiSrfSemantics,
    SrfTlistCpuOutputTooLarge,
    StandaloneGpuExprNoGpuPipeline,
    BitmapHeapGpuExprNoGpuPipeline,
    SpatialNoRegisteredGpuPredicate,
    SpatialIndexCheaper,
    SpatialVerticesBelowBreakEven,
    SpatialWorkBelowBreakEven,
    SpatialWorkAboveMax,
    SpatialHighOutputFraction,
    SpatialPreparedGeometryNotAvailable,
    PostgisIntersectsUnsupportedShape,
    PostgisDistanceNoGpuKernel,
    PostgisGeometryConstructorNoGpuOutputProtocol,
    RasterUnsupportedShape,
    RasterCatalogProofFailed,
    RasterSummaryStatsBitExactUnavailable,
    RasterResidentMetadataUnavailable,
    RasterZeroGridWkbNonRoundtrippable,
    RasterSelectedBandMissing,
    RasterCostUncalibrated,
    RasterRuntimeUnavailable,
    SortIncrementalOpportunity,
    SortMultiKeyNoGpuKernel,
    SortHeapFullOutput,
    /// Single-column ORDER BY with LIMIT 1: looks like PG's MIN/MAX →
    /// IndexScan+Limit rewrite (or a legitimate user LIMIT 1 by a single
    /// column). Either way GpuSort is the wrong shape — a 1-row top-K is
    /// O(N) reduce-cheap on PG's native plan and orders of magnitude faster
    /// than a full GPU sort, so decline and let PG run native.
    MinMaxRewriteNotASort,
    UnsupportedType(&'static str),
    Other(&'static str),
}

impl RejectionReason {
    /// Return the stable stats/tracing key for this rejection.
    #[must_use]
    pub(super) const fn stats_key(self) -> &'static str {
        match self {
            Self::ExtensionDisabled => "extension_disabled",
            Self::UnsupportedCommandType => "command_type_skip",
            Self::GpuNotUsable => "gpu_not_usable",
            Self::UnsupportedRelationKind => "unsupported_relation_kind",
            Self::UnsupportedRteKind => "unsupported_rte_kind",
            Self::RowsBelowMinBatch => "rows_below_min_batch",
            Self::NoCheapestPath => "no_cheapest_path",
            Self::NoAccelerableClause => "no_accelerable_clause",
            Self::CheaperNativePath => "cheaper_native_path",
            Self::CostModelDeclined => "cost_model_declined",
            Self::HashJoinNoSelectedGpuKernel => "hashjoin_no_selected_gpu_kernel",
            Self::HashJoinParallelInnerRebuildTooLarge => {
                "hashjoin_parallel_inner_rebuild_too_large"
            }
            Self::HashJoinHeapOutputTooLarge => "hashjoin_heap_output_too_large",
            Self::SemiAntiNoGpuMembershipFilter => "semianti_no_gpu_membership_filter",
            Self::MergeJoinNoGpuKernel => "mergejoin_no_gpu_kernel",
            Self::NestedLoopScalarNoGpuKernel => "nestloop_scalar_no_gpu_kernel",
            Self::NljBetweenHostBoundaryUnsafe => "nlj_between_host_boundary_unsafe",
            Self::NljBetweenOutputTooLarge => "nlj_between_output_too_large",
            Self::PartialAggNoGpuProducingChild => "partial_agg_no_gpu_producing_child",
            Self::PreAggNoGpuResidentPipeline => "preagg_no_gpu_resident_pipeline",
            Self::NoGpuResidentPipeline => "no_gpu_resident_pipeline",
            Self::GroupedAvgNoGpuFinalize => "grouped_avg_no_gpu_finalize",
            Self::AggSemanticModifierNoGpuKernel => "agg_semantic_modifier_no_gpu_kernel",
            Self::WindowPartialPathNoParallelHook => "window_partial_path_no_parallel_hook",
            Self::H3LateralSrfNoBatchedExpansion => "h3_lateral_srf_no_batched_expansion",
            Self::H3LatLngUnsupportedShape => "h3_latlng_unsupported_shape",
            Self::H3LatLngScalarPredicateNoGpuPipeline => {
                "h3_latlng_scalar_predicate_no_gpu_pipeline"
            }
            Self::SrfTlistMultiSrfSemantics => "srf_tlist_multi_srf_semantics",
            Self::SrfTlistCpuOutputTooLarge => "srf_tlist_cpu_output_too_large",
            Self::StandaloneGpuExprNoGpuPipeline => "standalone_gpuexpr_no_gpu_pipeline",
            Self::BitmapHeapGpuExprNoGpuPipeline => "bitmap_heap_gpuexpr_no_gpu_pipeline",
            Self::SpatialNoRegisteredGpuPredicate => "spatial_no_registered_gpu_predicate",
            Self::SpatialIndexCheaper => "spatial_index_cheaper",
            Self::SpatialVerticesBelowBreakEven => "spatial_vertices_below_break_even",
            Self::SpatialWorkBelowBreakEven => "spatial_work_below_break_even",
            Self::SpatialWorkAboveMax => "spatial_work_above_max",
            Self::SpatialHighOutputFraction => "spatial_high_output_fraction",
            Self::SpatialPreparedGeometryNotAvailable => "spatial_prepared_geometry_not_available",
            Self::PostgisIntersectsUnsupportedShape => "postgis_intersects_unsupported_shape",
            Self::PostgisDistanceNoGpuKernel => "postgis_distance_no_gpu_kernel",
            Self::PostgisGeometryConstructorNoGpuOutputProtocol => {
                "postgis_geometry_constructor_no_gpu_output_protocol"
            }
            Self::RasterUnsupportedShape => "raster_unsupported_shape",
            Self::RasterCatalogProofFailed => "raster_catalog_proof_failed",
            Self::RasterSummaryStatsBitExactUnavailable => {
                "raster_summarystats_bit_exact_unavailable"
            }
            Self::RasterResidentMetadataUnavailable => "raster_resident_metadata_unavailable",
            Self::RasterZeroGridWkbNonRoundtrippable => "raster_zero_grid_wkb_non_roundtrippable",
            Self::RasterSelectedBandMissing => "raster_selected_band_missing",
            Self::RasterCostUncalibrated => "raster_cost_uncalibrated",
            Self::RasterRuntimeUnavailable => "raster_runtime_unavailable",
            Self::SortIncrementalOpportunity => "sort_incremental_opportunity",
            Self::SortMultiKeyNoGpuKernel => "sort_multikey_no_gpu_kernel",
            Self::SortHeapFullOutput => "sort_heap_full_output",
            Self::MinMaxRewriteNotASort => "min_max_rewrite_not_a_sort",
            Self::UnsupportedType(reason) | Self::Other(reason) => reason,
        }
    }
}

/// Planner facts common to either an accepted or rejected candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct DecisionFacts {
    hook: &'static str,
    candidate: &'static str,
    strategy: Option<AccelStrategy>,
    estimated_rows: Option<u64>,
    relation_oid: Option<u32>,
    function_oid: Option<u32>,
    detail: Option<&'static str>,
}

impl DecisionFacts {
    /// Start a fact record for a hook and candidate path family.
    #[must_use]
    pub(super) const fn new(hook: &'static str, candidate: &'static str) -> Self {
        Self {
            hook,
            candidate,
            strategy: None,
            estimated_rows: None,
            relation_oid: None,
            function_oid: None,
            detail: None,
        }
    }

    /// Attach the acceleration strategy being considered.
    #[must_use]
    pub(super) const fn with_strategy(mut self, strategy: AccelStrategy) -> Self {
        self.strategy = Some(strategy);
        self
    }

    /// Attach the planner row estimate for this decision.
    #[must_use]
    pub(super) const fn with_estimated_rows(mut self, estimated_rows: u64) -> Self {
        self.estimated_rows = Some(estimated_rows);
        self
    }

    /// Attach a relation OID when the decision is relation-scoped.
    #[must_use]
    pub(super) const fn with_relation_oid(mut self, relation_oid: u32) -> Self {
        self.relation_oid = Some(relation_oid);
        self
    }

    /// Attach a function OID when the decision is function-scoped.
    #[must_use]
    pub(super) const fn with_function_oid(mut self, function_oid: u32) -> Self {
        self.function_oid = Some(function_oid);
        self
    }

    /// Attach a static detail tag for a gate-specific fact.
    #[must_use]
    pub(super) const fn with_detail(mut self, detail: &'static str) -> Self {
        self.detail = Some(detail);
        self
    }

    #[must_use]
    pub(super) const fn hook(self) -> &'static str {
        self.hook
    }

    #[must_use]
    pub(super) const fn candidate(self) -> &'static str {
        self.candidate
    }

    #[must_use]
    pub(super) const fn strategy(self) -> Option<AccelStrategy> {
        self.strategy
    }

    #[must_use]
    pub(super) const fn estimated_rows(self) -> Option<u64> {
        self.estimated_rows
    }

    #[must_use]
    pub(super) const fn relation_oid(self) -> Option<u32> {
        self.relation_oid
    }

    #[must_use]
    pub(super) const fn function_oid(self) -> Option<u32> {
        self.function_oid
    }

    #[must_use]
    pub(super) const fn detail(self) -> Option<&'static str> {
        self.detail
    }
}

/// Structured planner decision for a single candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PlannerDecision {
    Accepted {
        facts: DecisionFacts,
    },
    Rejected {
        reason: RejectionReason,
        facts: DecisionFacts,
    },
}

impl PlannerDecision {
    #[must_use]
    pub(super) const fn accepted(facts: DecisionFacts) -> Self {
        Self::Accepted { facts }
    }

    #[must_use]
    pub(super) const fn rejected(reason: RejectionReason, facts: DecisionFacts) -> Self {
        Self::Rejected { reason, facts }
    }

    #[must_use]
    pub(super) const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    #[must_use]
    pub(super) const fn is_rejected(self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    #[must_use]
    pub(super) const fn facts(self) -> DecisionFacts {
        match self {
            Self::Accepted { facts } | Self::Rejected { facts, .. } => facts,
        }
    }

    #[must_use]
    pub(super) const fn rejection_reason(self) -> Option<RejectionReason> {
        match self {
            Self::Accepted { .. } => None,
            Self::Rejected { reason, .. } => Some(reason),
        }
    }

    #[must_use]
    pub(super) const fn rejection_stats_key(self) -> Option<&'static str> {
        match self.rejection_reason() {
            Some(reason) => Some(reason.stats_key()),
            None => None,
        }
    }
}

/// Bounded recorder for planner decisions gathered during one planner pass.
#[derive(Debug, Clone)]
pub(super) struct PlannerDecisionRecorder {
    capacity: usize,
    decisions: VecDeque<PlannerDecision>,
    dropped: usize,
}

impl Default for PlannerDecisionRecorder {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_RECORDER_CAPACITY)
    }
}

impl PlannerDecisionRecorder {
    /// Create a recorder with a fixed maximum number of retained decisions.
    #[must_use]
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            decisions: VecDeque::with_capacity(capacity),
            dropped: 0,
        }
    }

    /// Record a decision, dropping the oldest retained decision if full.
    pub(super) fn record(&mut self, decision: PlannerDecision) {
        if self.capacity == 0 {
            self.dropped += 1;
            return;
        }

        if self.decisions.len() == self.capacity {
            let _ = self.decisions.pop_front();
            self.dropped += 1;
        }
        self.decisions.push_back(decision);
    }

    /// Record an accepted candidate.
    pub(super) fn record_acceptance(&mut self, facts: DecisionFacts) {
        self.record(PlannerDecision::accepted(facts));
    }

    /// Record a rejected candidate.
    pub(super) fn record_rejection(&mut self, reason: RejectionReason, facts: DecisionFacts) {
        self.record(PlannerDecision::rejected(reason, facts));
    }

    /// Clear retained decisions and drop accounting.
    pub(super) fn clear(&mut self) {
        self.decisions.clear();
        self.dropped = 0;
    }

    #[must_use]
    pub(super) const fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub(super) fn len(&self) -> usize {
        self.decisions.len()
    }

    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }

    #[must_use]
    pub(super) const fn dropped_count(&self) -> usize {
        self.dropped
    }

    #[must_use]
    pub(super) fn last(&self) -> Option<PlannerDecision> {
        self.decisions.back().copied()
    }

    pub(super) fn decisions(&self) -> impl Iterator<Item = PlannerDecision> + '_ {
        self.decisions.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_facts(tag: &'static str) -> DecisionFacts {
        DecisionFacts::new("rel_pathlist", tag)
            .with_strategy(AccelStrategy::GpuExpr)
            .with_estimated_rows(42)
            .with_relation_oid(1259)
    }

    #[test]
    fn decision_facts_builder_sets_fields() {
        let facts = scan_facts("GpuScan")
            .with_function_oid(9001)
            .with_detail("top_level_gate");

        assert_eq!(facts.hook(), "rel_pathlist");
        assert_eq!(facts.candidate(), "GpuScan");
        assert_eq!(facts.strategy(), Some(AccelStrategy::GpuExpr));
        assert_eq!(facts.estimated_rows(), Some(42));
        assert_eq!(facts.relation_oid(), Some(1259));
        assert_eq!(facts.function_oid(), Some(9001));
        assert_eq!(facts.detail(), Some("top_level_gate"));
    }

    #[test]
    fn accepted_decision_exposes_facts_without_rejection_reason() {
        let facts = scan_facts("GpuScan");
        let decision = PlannerDecision::accepted(facts);

        assert!(decision.is_accepted());
        assert!(!decision.is_rejected());
        assert_eq!(decision.facts(), facts);
        assert_eq!(decision.rejection_reason(), None);
        assert_eq!(decision.rejection_stats_key(), None);
    }

    #[test]
    fn rejected_decision_exposes_reason_and_stats_key() {
        let facts = scan_facts("GpuHashJoin");
        let decision =
            PlannerDecision::rejected(RejectionReason::HashJoinNoSelectedGpuKernel, facts);

        assert!(!decision.is_accepted());
        assert!(decision.is_rejected());
        assert_eq!(decision.facts(), facts);
        assert_eq!(
            decision.rejection_reason(),
            Some(RejectionReason::HashJoinNoSelectedGpuKernel)
        );
        assert_eq!(
            decision.rejection_stats_key(),
            Some("hashjoin_no_selected_gpu_kernel")
        );
    }

    #[test]
    fn rejection_reason_pins_existing_stats_keys() {
        assert_eq!(
            RejectionReason::UnsupportedCommandType.stats_key(),
            "command_type_skip"
        );
        assert_eq!(
            RejectionReason::HashJoinNoSelectedGpuKernel.stats_key(),
            "hashjoin_no_selected_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::HashJoinParallelInnerRebuildTooLarge.stats_key(),
            "hashjoin_parallel_inner_rebuild_too_large"
        );
        assert_eq!(
            RejectionReason::HashJoinHeapOutputTooLarge.stats_key(),
            "hashjoin_heap_output_too_large"
        );
        assert_eq!(
            RejectionReason::SemiAntiNoGpuMembershipFilter.stats_key(),
            "semianti_no_gpu_membership_filter"
        );
        assert_eq!(
            RejectionReason::MergeJoinNoGpuKernel.stats_key(),
            "mergejoin_no_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::NestedLoopScalarNoGpuKernel.stats_key(),
            "nestloop_scalar_no_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::NljBetweenOutputTooLarge.stats_key(),
            "nlj_between_output_too_large"
        );
        assert_eq!(
            RejectionReason::PartialAggNoGpuProducingChild.stats_key(),
            "partial_agg_no_gpu_producing_child"
        );
        assert_eq!(
            RejectionReason::PreAggNoGpuResidentPipeline.stats_key(),
            "preagg_no_gpu_resident_pipeline"
        );
        assert_eq!(
            RejectionReason::GroupedAvgNoGpuFinalize.stats_key(),
            "grouped_avg_no_gpu_finalize"
        );
        assert_eq!(
            RejectionReason::AggSemanticModifierNoGpuKernel.stats_key(),
            "agg_semantic_modifier_no_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::NoGpuResidentPipeline.stats_key(),
            "no_gpu_resident_pipeline"
        );
        assert_eq!(
            RejectionReason::WindowPartialPathNoParallelHook.stats_key(),
            "window_partial_path_no_parallel_hook"
        );
        assert_eq!(
            RejectionReason::H3LateralSrfNoBatchedExpansion.stats_key(),
            "h3_lateral_srf_no_batched_expansion"
        );
        assert_eq!(
            RejectionReason::H3LatLngUnsupportedShape.stats_key(),
            "h3_latlng_unsupported_shape"
        );
        assert_eq!(
            RejectionReason::H3LatLngScalarPredicateNoGpuPipeline.stats_key(),
            "h3_latlng_scalar_predicate_no_gpu_pipeline"
        );
        assert_eq!(
            RejectionReason::SrfTlistMultiSrfSemantics.stats_key(),
            "srf_tlist_multi_srf_semantics"
        );
        assert_eq!(
            RejectionReason::SrfTlistCpuOutputTooLarge.stats_key(),
            "srf_tlist_cpu_output_too_large"
        );
        assert_eq!(
            RejectionReason::StandaloneGpuExprNoGpuPipeline.stats_key(),
            "standalone_gpuexpr_no_gpu_pipeline"
        );
        assert_eq!(
            RejectionReason::BitmapHeapGpuExprNoGpuPipeline.stats_key(),
            "bitmap_heap_gpuexpr_no_gpu_pipeline"
        );
        assert_eq!(
            RejectionReason::SpatialNoRegisteredGpuPredicate.stats_key(),
            "spatial_no_registered_gpu_predicate"
        );
        assert_eq!(
            RejectionReason::SpatialIndexCheaper.stats_key(),
            "spatial_index_cheaper"
        );
        assert_eq!(
            RejectionReason::SpatialVerticesBelowBreakEven.stats_key(),
            "spatial_vertices_below_break_even"
        );
        assert_eq!(
            RejectionReason::SpatialWorkBelowBreakEven.stats_key(),
            "spatial_work_below_break_even"
        );
        assert_eq!(
            RejectionReason::SpatialWorkAboveMax.stats_key(),
            "spatial_work_above_max"
        );
        assert_eq!(
            RejectionReason::SpatialHighOutputFraction.stats_key(),
            "spatial_high_output_fraction"
        );
        assert_eq!(
            RejectionReason::SpatialPreparedGeometryNotAvailable.stats_key(),
            "spatial_prepared_geometry_not_available"
        );
        assert_eq!(
            RejectionReason::PostgisIntersectsUnsupportedShape.stats_key(),
            "postgis_intersects_unsupported_shape"
        );
        assert_eq!(
            RejectionReason::PostgisDistanceNoGpuKernel.stats_key(),
            "postgis_distance_no_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::PostgisGeometryConstructorNoGpuOutputProtocol.stats_key(),
            "postgis_geometry_constructor_no_gpu_output_protocol"
        );
        assert_eq!(
            RejectionReason::SortIncrementalOpportunity.stats_key(),
            "sort_incremental_opportunity"
        );
        assert_eq!(
            RejectionReason::SortMultiKeyNoGpuKernel.stats_key(),
            "sort_multikey_no_gpu_kernel"
        );
        assert_eq!(
            RejectionReason::SortHeapFullOutput.stats_key(),
            "sort_heap_full_output"
        );
        assert_eq!(
            RejectionReason::MinMaxRewriteNotASort.stats_key(),
            "min_max_rewrite_not_a_sort"
        );
        assert_eq!(
            RejectionReason::UnsupportedType("unsupported_jsonb_type").stats_key(),
            "unsupported_jsonb_type"
        );
    }

    #[test]
    fn recorder_retains_decisions_until_capacity() {
        let mut recorder = PlannerDecisionRecorder::with_capacity(2);

        recorder.record_acceptance(scan_facts("first"));
        recorder.record_rejection(RejectionReason::RowsBelowMinBatch, scan_facts("second"));

        let decisions = recorder.decisions().collect::<Vec<_>>();
        assert_eq!(recorder.capacity(), 2);
        assert_eq!(recorder.len(), 2);
        assert_eq!(recorder.dropped_count(), 0);
        assert_eq!(decisions[0].facts().candidate(), "first");
        assert_eq!(decisions[1].facts().candidate(), "second");
    }

    #[test]
    fn recorder_drops_oldest_when_capacity_is_reached() {
        let mut recorder = PlannerDecisionRecorder::with_capacity(2);

        recorder.record_acceptance(scan_facts("first"));
        recorder.record_acceptance(scan_facts("second"));
        recorder.record_rejection(RejectionReason::CostModelDeclined, scan_facts("third"));

        let decisions = recorder.decisions().collect::<Vec<_>>();
        assert_eq!(recorder.len(), 2);
        assert_eq!(recorder.dropped_count(), 1);
        assert_eq!(decisions[0].facts().candidate(), "second");
        assert_eq!(decisions[1].facts().candidate(), "third");
        assert_eq!(recorder.last(), Some(decisions[1]));
    }

    #[test]
    fn recorder_zero_capacity_counts_drops_without_storing() {
        let mut recorder = PlannerDecisionRecorder::with_capacity(0);

        recorder.record_acceptance(scan_facts("first"));
        recorder.record_rejection(RejectionReason::NoCheapestPath, scan_facts("second"));

        assert!(recorder.is_empty());
        assert_eq!(recorder.len(), 0);
        assert_eq!(recorder.dropped_count(), 2);
        assert_eq!(recorder.last(), None);
    }

    #[test]
    fn recorder_clear_resets_retained_decisions_and_drop_count() {
        let mut recorder = PlannerDecisionRecorder::with_capacity(1);
        recorder.record_acceptance(scan_facts("first"));
        recorder.record_acceptance(scan_facts("second"));

        assert_eq!(recorder.len(), 1);
        assert_eq!(recorder.dropped_count(), 1);

        recorder.clear();

        assert!(recorder.is_empty());
        assert_eq!(recorder.dropped_count(), 0);
        assert_eq!(recorder.capacity(), 1);
    }
}

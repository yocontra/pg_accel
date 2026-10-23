//! Catalog-driven aggregate-shape extraction for the unified OLAP path.
//!
//! [`extract_shape`] emits a logical [`AggQuerySpec`], ordered output
//! projection metadata, exact relation/attribute requirements, and typed
//! cost/residency metadata. The generic upper-path hook resolves the initially
//! unknown residency facts before it can admit the childless descriptor path.

mod builder;
mod cost;
mod int4_predicate;
mod postgres;
mod postgres_scalar;

pub use postgres_scalar::{LoweredScalarExpression, lower_scalar_expression};

/// Live-test entry to the same aggregate extraction used by production hooks.
///
/// # Safety
/// Both pointers must belong to the live analyzed/simplified Query on the
/// backend main thread and remain valid throughout this synchronous call.
#[cfg(feature = "pg_test")]
pub(crate) unsafe fn test_parse_scalar_aggregate(
    node: *mut pg_sys::Node,
    query: &pg_sys::Query,
) -> Result<AggregateExpr, ShapeDecline> {
    // SAFETY: forward the caller's planner-tree/main-thread contract.
    unsafe { postgres::parse_aggregate(node, query) }
}

pub(crate) use postgres::aggregate_scan_target_list;
pub(super) use postgres::mark_permission_role_dependency;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use pgrx::pg_sys;

use crate::engine::cost::{PgCost, TypedCostModel};
use crate::engine::spec::{
    AggQuerySpec, AggregateOutput, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeySource,
    MeasureExpr,
};

pub use crate::engine::spec::{
    AggOutputSlot as ProjectionSlot, AggOutputSource as ProjectionSource,
};

pub use builder::build_shape;
pub(super) use cost::dense_atomic_fact_row_floor;
pub(super) use cost::h3_group_bound;
pub(super) use cost::numeric_fact_row_floor;
pub use cost::{ShapeCost, ShapeCostGate, estimate_shape_cost};
pub(super) use cost::{
    dense_atomic_sum_count_cost, dense_atomic_sum_count_lifecycle, dense_lifecycle_call_count,
};

/// Reject non-base range-table inputs before GPU capability discovery.
///
/// # Safety
///
/// `root` must be null or a planner-owned pointer for the current invocation.
pub unsafe fn preflight_base_relations(root: *mut pg_sys::PlannerInfo) -> Result<(), ShapeDecline> {
    // SAFETY: forwarded unchanged under this function's planner-pointer contract.
    unsafe { postgres::preflight_base_relations(root) }
}

/// Planner range-table identity paired with a catalog column identity.
///
/// `varno` disambiguates planner relations while extraction is in progress.
/// Self joins are declined before the neutral spec is built because
/// [`ColumnRef`] intentionally identifies relations by catalog OID only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerColumn {
    pub varno: pg_sys::Index,
    pub column: ColumnRef,
    pub type_modifier: i32,
    pub collation_oid: u32,
    pub collation_is_deterministic: bool,
}

/// Residency state known while the planner is extracting a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationResidency {
    /// Phase 5A has proved that the selected columns are current and resident.
    Resident,
    /// Phase 5A can load the selected columns before execution.
    AutoLoad,
    /// Shape extraction has not yet consulted the residency store.
    Unknown,
}

/// Catalog facts for one base relation participating in a candidate shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationShape {
    pub varno: pg_sys::Index,
    pub relation_oid: u32,
    pub estimated_rows: u64,
    pub unique_attnos: BTreeSet<i32>,
    /// Average or fixed-width bytes keyed by positive attribute number.
    pub column_widths: BTreeMap<i32, u32>,
    pub residency: RelationResidency,
}

/// One equality edge in the candidate relation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EquiJoin {
    pub left: PlannerColumn,
    pub right: PlannerColumn,
}

/// One SQL aggregate before equal measure expressions are coalesced into a
/// single descriptor measure slot.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateExpr {
    pub expression: MeasureExpr,
    pub output: AggregateOutput,
    pub filter: FilterSpec,
}

/// One analyzed GROUP BY key before relation orientation.
///
/// Direct columns retain planner `varno` identity so star dimensions can be
/// oriented safely. Expression keys carry only a catalog-resolved neutral
/// source; PostgreSQL extraction must prove the function/operator semantics
/// before constructing one of these variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannerGroupKey {
    Column(PlannerColumn),
    Expression {
        source: GroupKeySource,
        type_oid: u32,
        collation_oid: u32,
        collation_is_deterministic: bool,
    },
}

/// PostgreSQL output typing carried verbatim into the strict plan wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputMetadata {
    /// Canonical type of the value consumed by the aggregate/group lane.
    /// COUNT(*) uses `InvalidOid` (`0`) because it has no input expression.
    pub source_type_oid: u32,
    pub result_type_oid: u32,
    pub result_typmod: i32,
    pub result_collation_oid: u32,
    pub nullable: bool,
}

/// A non-junk output in PostgreSQL target-list order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputProjection {
    Group {
        key: PlannerGroupKey,
        output: OutputMetadata,
    },
    Aggregate {
        aggregate_index: u32,
        output: OutputMetadata,
    },
}

/// Query features that are not part of a reducing aggregate shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ShapeModifiers {
    pub has_window_functions: bool,
    pub has_target_srfs: bool,
    pub has_sublinks: bool,
    pub has_recursive_query: bool,
    pub has_modifying_cte: bool,
    pub has_row_security: bool,
    pub has_distinct: bool,
    pub has_grouping_sets: bool,
    pub has_group_distinct: bool,
    pub has_having: bool,
    pub has_set_operations: bool,
    pub has_row_marks: bool,
}

/// Catalog-normalized input to the pure shape builder.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeInput {
    pub relations: Vec<RelationShape>,
    pub joins: Vec<EquiJoin>,
    pub group_keys: Vec<PlannerGroupKey>,
    pub aggregates: Vec<AggregateExpr>,
    pub projections: Vec<InputProjection>,
    /// At most one descriptor-compatible filter per relation.
    pub relation_filters: Vec<(u32, FilterSpec)>,
    pub estimated_output_rows: u64,
    pub expected_reuses: NonZeroU32,
    pub modifiers: ShapeModifiers,
}

/// Exact selected columns for one relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredRelation {
    pub relation_oid: u32,
    pub attnos: Vec<i32>,
}

/// Per-relation residency/load estimate passed to Phase 5A.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationResidencyRequirement {
    pub relation_oid: u32,
    pub attnos: Vec<i32>,
    pub state: RelationResidency,
    pub estimated_rows: u64,
    pub estimated_bytes: Option<u64>,
}

/// Conservative residency metadata used to account for automatic loading.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidencyEstimate {
    pub relations: Vec<RelationResidencyRequirement>,
    pub total_required_bytes: Option<u64>,
    pub missing_bytes: Option<u64>,
    pub missing_rows: u64,
    pub expected_reuses: NonZeroU32,
    pub amortized_load_cost: PgCost,
}

/// One logical group key that must be dictionary-coded from current resident
/// data before a C descriptor can be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryKeyRequirement {
    pub key_index: u32,
    pub source: GroupKeySource,
    /// PostgreSQL collation used to derive dictionary equality classes.
    pub collation_oid: u32,
}

/// One collatable equijoin whose fact/dimension lanes must be correlated to
/// one shared dictionary code domain before the INT32 descriptor is bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictionaryJoinRequirement {
    pub dim_index: u32,
    pub fact_key: ColumnRef,
    pub dim_key: ColumnRef,
    pub collation_oid: u32,
}

/// One expression-backed group key that must be materialized from current
/// resident data before the generic HASH descriptor can be bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedGroupKeyRequirement {
    pub key_index: u32,
    pub source: GroupKeySource,
    pub result_type_oid: u32,
}

/// Begin-time grouping implementation selected for the logical AQS6 spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorGroupingMode {
    DenseDictionary,
    Hash,
}

/// Whether the logical spec requires resident artifacts before a descriptor
/// can be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorResolution {
    Ready,
    BeginTimeArtifacts {
        dictionary_keys: Vec<DictionaryKeyRequirement>,
        derived_keys: Vec<DerivedGroupKeyRequirement>,
        joins: Vec<DictionaryJoinRequirement>,
        grouping_mode: DescriptorGroupingMode,
        max_group_count: usize,
    },
}

/// Exact dictionary domain produced from current Phase 5A residency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDictionaryKey {
    pub key_index: u32,
    /// Includes the NULL code when `null_code` is present.
    pub cardinality: u32,
    pub null_code: Option<i32>,
}

/// Proof that one expression-backed HASH lane was built for the exact source
/// and logical type carried by the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDerivedGroupKey {
    pub key_index: u32,
    pub source: GroupKeySource,
    pub result_type_oid: u32,
}

/// Descriptor slot used to evaluate a scalar fact filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorFilterBinding {
    pub measure_index: u32,
    pub source: crate::engine::spec::AggregateSource,
    /// True when the planner appended an unprojected measure solely to make
    /// the filter column addressable by `predicate_measure_slot`.
    pub hidden: bool,
}

/// Measure-slot accounting for the C descriptor ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorMeasurePlan {
    pub projected_measure_count: u32,
    pub descriptor_measure_count: u32,
    pub fact_filter: Option<DescriptorFilterBinding>,
    /// SQL boolean column whose resident values/nulls must be converted to a
    /// tri-state descriptor mask at Begin time (`NULL` does not select).
    pub derived_fact_mask: Option<ColumnRef>,
    /// The fact filter is a spatial producer that publishes the descriptor
    /// tri-state mask after exact rechecks.
    pub derived_spatial_mask: bool,
}

/// Complete output of the shared shape pass.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapePlan {
    pub spec: AggQuerySpec,
    pub projections: Vec<ProjectionSlot>,
    pub required_relations: Vec<RequiredRelation>,
    /// Conservative PostgreSQL aggregate cardinality after any exact H3
    /// universe bound. This is the cardinality advertised by the CustomPath.
    pub estimated_output_rows: u64,
    /// Individual PostgreSQL key estimates, before resident NULL slots are
    /// added. Their product can exceed joint output groups and input rows.
    pub estimated_dense_key_cardinalities: Option<Vec<u64>>,
    /// Stable words consumed by Phase 5A's `shape_digest`.
    pub digest_words: Vec<i32>,
    pub descriptor_resolution: DescriptorResolution,
    pub descriptor_measures: DescriptorMeasurePlan,
    pub residency: ResidencyEstimate,
    pub cost: ShapeCost,
    pub cost_gate: ShapeCostGate,
}

/// Stable, source-verifiable capability decline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeDecline {
    NotSelect,
    NoAggregate,
    WindowFunctions,
    TargetSetReturningFunction,
    Sublink,
    RecursiveQuery,
    ModifyingCte,
    RowSecurity,
    Distinct,
    GroupingSets,
    GroupDistinct,
    Having,
    SetOperations,
    RowMarks,
    UnsupportedRangeTableEntry {
        varno: pg_sys::Index,
    },
    UnsupportedPermissionContext {
        varno: pg_sys::Index,
    },
    TableSample,
    UnsupportedOuterJoin,
    UnsupportedPredicate,
    SpatialFilterOutsideFactRelation,
    SpatialWorkShapeUnproved,
    InvalidSpatialConstant,
    PostgisCatalog(String),
    UnsupportedFilterType {
        type_oid: u32,
    },
    UnsupportedAggregate {
        aggregate_oid: u32,
    },
    NumericAccumulatorUnavailable {
        aggregate_oid: u32,
    },
    NumericAccumulatorTypeUnavailable {
        type_oid: u32,
    },
    UnsupportedMeasureType {
        type_oid: u32,
    },
    UnsupportedJoinKeyType {
        type_oid: u32,
    },
    NondeterministicKeyCollation {
        collation_oid: u32,
    },
    InvalidKeyCollation {
        type_oid: u32,
        collation_oid: u32,
    },
    UnsupportedGroupKeyType {
        type_oid: u32,
    },
    IntegerExpressionOverflowSemantics,
    FloatingExpressionSemantics,
    FloatingAccumulatorSemantics,
    UnsupportedAggregateInput {
        kind: crate::engine::spec::AggregateKind,
        type_oid: u32,
    },
    ProjectionSourceTypeMismatch {
        expected_type_oid: u32,
        actual_type_oid: u32,
    },
    AggregateResultTypeMismatch {
        source_type_oid: u32,
        kind: crate::engine::spec::AggregateKind,
        result_type_oid: u32,
    },
    UnsupportedBinaryMeasure,
    UnsupportedAggregateModifier,
    UnsupportedMeasureExpression,
    UnsupportedProjection,
    UnprojectedGroupKey {
        relation_oid: u32,
        attno: i32,
    },
    UnsupportedGroupExpression,
    UnsupportedColumn {
        relation_oid: u32,
        attno: i32,
    },
    TooManyRelations {
        actual: usize,
        maximum: usize,
    },
    TooManyGroupKeys {
        actual: usize,
        maximum: usize,
    },
    TooManyDimensions {
        actual: usize,
        maximum: usize,
    },
    TooManyMeasures {
        actual: usize,
        maximum: usize,
    },
    SelfJoinUsesAmbiguousRelationOid {
        relation_oid: u32,
    },
    DuplicatePlannerRelation {
        varno: pg_sys::Index,
    },
    JoinKeyTypeMismatch {
        left_type_oid: u32,
        right_type_oid: u32,
    },
    JoinKeyCollationMismatch {
        left_collation_oid: u32,
        right_collation_oid: u32,
    },
    NonEqualityJoin,
    CompositeJoinKeyUnsupported,
    DisconnectedJoinGraph,
    NonStarJoinGraph,
    AmbiguousFactRelation,
    GroupedByNonUniqueDimension {
        relation_oid: u32,
        attno: i32,
    },
    MultipleFiltersPerRelation {
        relation_oid: u32,
    },
    MultipleRangePredicates,
    InvalidFilterRange,
    InvalidProjectionReference {
        aggregate_index: u32,
    },
    DescriptorArtifactsRequireResolution,
    InvalidGroupKeyResolution,
    InvalidSpec(String),
    Codec(String),
}

impl ShapeDecline {
    /// Stable stats/artifact key. Details remain available in the enum fields.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotSelect => "shape_not_select",
            Self::NoAggregate => "shape_no_aggregate",
            Self::WindowFunctions => "shape_window_functions",
            Self::TargetSetReturningFunction => "shape_target_srf",
            Self::Sublink => "shape_sublink",
            Self::RecursiveQuery => "shape_recursive_query",
            Self::ModifyingCte => "shape_modifying_cte",
            Self::RowSecurity => "shape_row_security",
            Self::Distinct => "shape_distinct",
            Self::GroupingSets => "shape_grouping_sets",
            Self::GroupDistinct => "shape_group_distinct",
            Self::Having => "shape_having",
            Self::SetOperations => "shape_set_operations",
            Self::RowMarks => "shape_row_marks",
            Self::UnsupportedRangeTableEntry { .. } => "shape_unsupported_rte",
            Self::UnsupportedPermissionContext { .. } => "shape_permission_context",
            Self::TableSample => "shape_table_sample",
            Self::UnsupportedOuterJoin => "shape_outer_join",
            Self::UnsupportedPredicate => "shape_unsupported_predicate",
            Self::SpatialFilterOutsideFactRelation => "shape_spatial_filter_outside_fact",
            Self::SpatialWorkShapeUnproved => "shape_spatial_work_shape_unproved",
            Self::InvalidSpatialConstant => "shape_invalid_spatial_constant",
            Self::PostgisCatalog(_) => "shape_postgis_catalog",
            Self::UnsupportedFilterType { .. } => "shape_unsupported_filter_type",
            Self::UnsupportedAggregate { .. } => "shape_unsupported_aggregate",
            Self::NumericAccumulatorUnavailable { .. } => "shape_numeric_accumulator_unavailable",
            Self::NumericAccumulatorTypeUnavailable { .. } => {
                "shape_numeric_accumulator_type_unavailable"
            }
            Self::UnsupportedMeasureType { .. } => "shape_unsupported_measure_type",
            Self::UnsupportedJoinKeyType { .. } => "shape_unsupported_join_key_type",
            Self::NondeterministicKeyCollation { .. } => "shape_nondeterministic_key_collation",
            Self::InvalidKeyCollation { .. } => "shape_invalid_key_collation",
            Self::UnsupportedGroupKeyType { .. } => "shape_unsupported_group_key_type",
            Self::IntegerExpressionOverflowSemantics => {
                "shape_integer_expression_overflow_semantics"
            }
            Self::FloatingExpressionSemantics => "shape_floating_expression_semantics",
            Self::FloatingAccumulatorSemantics => "shape_floating_accumulator_semantics",
            Self::UnsupportedAggregateInput { .. } => "shape_unsupported_aggregate_input",
            Self::ProjectionSourceTypeMismatch { .. } => "shape_projection_source_type",
            Self::AggregateResultTypeMismatch { .. } => "shape_aggregate_result_type",
            Self::UnsupportedBinaryMeasure => "shape_unsupported_binary_measure",
            Self::UnsupportedAggregateModifier => "shape_aggregate_modifier",
            Self::UnsupportedMeasureExpression => "shape_measure_expression",
            Self::UnsupportedProjection => "shape_projection",
            Self::UnprojectedGroupKey { .. } => "shape_unprojected_group_key",
            Self::UnsupportedGroupExpression => "shape_group_expression",
            Self::UnsupportedColumn { .. } => "shape_unsupported_column",
            Self::TooManyRelations { .. } => "shape_too_many_relations",
            Self::TooManyGroupKeys { .. } => "shape_too_many_group_keys",
            Self::TooManyDimensions { .. } => "shape_too_many_dimensions",
            Self::TooManyMeasures { .. } => "shape_too_many_measures",
            Self::SelfJoinUsesAmbiguousRelationOid { .. } => "shape_self_join",
            Self::DuplicatePlannerRelation { .. } => "shape_duplicate_varno",
            Self::JoinKeyTypeMismatch { .. } => "shape_join_key_type_mismatch",
            Self::JoinKeyCollationMismatch { .. } => "shape_join_key_collation_mismatch",
            Self::NonEqualityJoin => "shape_non_equality_join",
            Self::CompositeJoinKeyUnsupported => "shape_composite_join_key",
            Self::DisconnectedJoinGraph => "shape_disconnected_join_graph",
            Self::NonStarJoinGraph => "shape_non_star_join_graph",
            Self::AmbiguousFactRelation => "shape_ambiguous_fact_relation",
            Self::GroupedByNonUniqueDimension { .. } => "shape_nonunique_dimension_group",
            Self::MultipleFiltersPerRelation { .. } => "shape_multi_filter_relation",
            Self::MultipleRangePredicates => "shape_multiple_range_predicates",
            Self::InvalidFilterRange => "shape_invalid_filter_range",
            Self::InvalidProjectionReference { .. } => "shape_projection_reference",
            Self::DescriptorArtifactsRequireResolution => {
                "shape_descriptor_artifacts_require_resolution"
            }
            Self::InvalidGroupKeyResolution => "shape_invalid_group_key_resolution",
            Self::InvalidSpec(_) => "shape_invalid_spec",
            Self::Codec(_) => "shape_codec",
        }
    }
}

impl std::fmt::Display for ShapeDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {self:?}", self.code())
    }
}

impl std::error::Error for ShapeDecline {}

impl ShapePlan {
    /// Budget chunk selection changes only ordinary, dimension-free dense
    /// integer/COUNT execution. Resolve grouping policy, not placeholder key
    /// encodings, and leave spatial producers and every other family alone.
    pub(crate) fn supports_budget_selected_dense(&self) -> bool {
        let hash = match &self.descriptor_resolution {
            DescriptorResolution::BeginTimeArtifacts { grouping_mode, .. } => {
                *grouping_mode == DescriptorGroupingMode::Hash
            }
            DescriptorResolution::Ready => self
                .spec
                .group_keys
                .iter()
                .any(|key| key.encoding == GroupKeyEncoding::Hash),
        };
        self.spec.star_dims.is_empty()
            && self.spec.fact_filter.spatial().is_none()
            && matches!(
                crate::engine::executor::agg::planned_descriptor_kernel_mode(&self.spec, hash),
                crate::gpu::GroupedAggKernelMode::ParallelDenseInteger
                    | crate::gpu::GroupedAggKernelMode::ParallelDenseCount
            )
    }

    /// Borrow a spec only when no Begin-time artifact is needed.
    pub fn descriptor_spec(&self) -> Result<&AggQuerySpec, ShapeDecline> {
        if self.descriptor_resolution != DescriptorResolution::Ready
            || self
                .spec
                .group_keys
                .iter()
                .any(|key| key.encoding == GroupKeyEncoding::Hash)
        {
            return Err(ShapeDecline::DescriptorArtifactsRequireResolution);
        }
        Ok(&self.spec)
    }

    /// Resolve a dense-dictionary plan using current resident domains.
    ///
    /// The returned spec has descriptor-legal key encodings; Phase 5D must
    /// still bind the resident dictionary codes/decoders and device pointers.
    /// This method never mutates the logical/digest identity stored in the
    /// plan.
    pub fn resolve_dictionary_keys(
        &self,
        resolutions: &[ResolvedDictionaryKey],
    ) -> Result<AggQuerySpec, ShapeDecline> {
        self.resolve_group_key_artifacts(resolutions, &[])
    }

    /// Validate every Begin-time group-key artifact and return the descriptor
    /// spec. Dense mode rewrites HASH keys to exact INT32 dictionary domains;
    /// generic HASH mode preserves expression/H3 encodings after proving that
    /// their resident derived lanes match the canonical source and type.
    pub fn resolve_group_key_artifacts(
        &self,
        dictionary_resolutions: &[ResolvedDictionaryKey],
        derived_resolutions: &[ResolvedDerivedGroupKey],
    ) -> Result<AggQuerySpec, ShapeDecline> {
        let DescriptorResolution::BeginTimeArtifacts {
            dictionary_keys,
            derived_keys,
            joins: _,
            grouping_mode,
            max_group_count,
        } = &self.descriptor_resolution
        else {
            if dictionary_resolutions.is_empty() && derived_resolutions.is_empty() {
                return Ok(self.spec.clone());
            }
            return Err(ShapeDecline::InvalidGroupKeyResolution);
        };
        if dictionary_resolutions.len() != dictionary_keys.len()
            || derived_resolutions.len() != derived_keys.len()
        {
            return Err(ShapeDecline::InvalidGroupKeyResolution);
        }
        let mut resolved = self.spec.clone();
        let mut group_count = 1_usize;
        for requirement in dictionary_keys {
            let domain = dictionary_resolutions
                .iter()
                .find(|domain| domain.key_index == requirement.key_index)
                .ok_or(ShapeDecline::InvalidGroupKeyResolution)?;
            let index = usize::try_from(requirement.key_index)
                .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?;
            let key = resolved
                .group_keys
                .get_mut(index)
                .ok_or(ShapeDecline::InvalidGroupKeyResolution)?;
            if key.source != requirement.source || key.encoding != GroupKeyEncoding::Hash {
                return Err(ShapeDecline::InvalidGroupKeyResolution);
            }
            let cardinality = usize::try_from(domain.cardinality)
                .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?;
            if cardinality == 0 {
                return Err(ShapeDecline::InvalidGroupKeyResolution);
            }
            group_count = group_count
                .checked_mul(cardinality)
                .filter(|count| count <= max_group_count)
                .ok_or(ShapeDecline::InvalidGroupKeyResolution)?;
            key.encoding = GroupKeyEncoding::DictionaryI32 {
                cardinality: domain.cardinality,
                null_code: domain.null_code,
            };
        }
        for requirement in derived_keys {
            let artifact = derived_resolutions
                .iter()
                .find(|artifact| artifact.key_index == requirement.key_index)
                .ok_or(ShapeDecline::InvalidGroupKeyResolution)?;
            let index = usize::try_from(requirement.key_index)
                .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?;
            let key = resolved
                .group_keys
                .get(index)
                .ok_or(ShapeDecline::InvalidGroupKeyResolution)?;
            let artifact_source_matches = artifact.source == requirement.source;
            let key_source_matches = key.source == requirement.source;
            let artifact_type_matches = artifact.result_type_oid == requirement.result_type_oid;
            let key_type_matches = key.type_oid == requirement.result_type_oid;
            if !artifact_source_matches
                || !key_source_matches
                || !artifact_type_matches
                || !key_type_matches
                || key.encoding != GroupKeyEncoding::Hash
            {
                return Err(ShapeDecline::InvalidGroupKeyResolution);
            }
        }
        match grouping_mode {
            DescriptorGroupingMode::DenseDictionary
                if !derived_keys.is_empty()
                    || resolved
                        .group_keys
                        .iter()
                        .any(|key| key.encoding == GroupKeyEncoding::Hash) =>
            {
                return Err(ShapeDecline::InvalidGroupKeyResolution);
            }
            DescriptorGroupingMode::Hash
                if derived_keys.is_empty()
                    || !resolved
                        .group_keys
                        .iter()
                        .any(|key| key.encoding == GroupKeyEncoding::Hash) =>
            {
                return Err(ShapeDecline::InvalidGroupKeyResolution);
            }
            DescriptorGroupingMode::DenseDictionary | DescriptorGroupingMode::Hash => {}
        }
        resolved
            .validate()
            .map_err(|_| ShapeDecline::InvalidGroupKeyResolution)?;
        Ok(resolved)
    }
}

/// Extract a neutral aggregate plan from PostgreSQL planner state.
///
/// # Safety
///
/// `root` and `output_rel` must be planner-owned pointers valid for the
/// current `UPPERREL_GROUP_AGG` hook. Catalog access must run on PostgreSQL's
/// main backend thread in an active transaction.
pub unsafe fn extract_shape(
    root: *mut pg_sys::PlannerInfo,
    output_rel: *mut pg_sys::RelOptInfo,
    estimated_output_rows: u64,
    model: &TypedCostModel,
) -> Result<ShapePlan, ShapeDecline> {
    // SAFETY: forwarded unchanged under this function's planner-pointer and
    // backend-thread contract.
    let input = unsafe {
        postgres::extract_input(
            root,
            output_rel,
            estimated_output_rows,
            model.planner.auto_load_amortization_queries,
        )
    }?;
    let mut shape = build_shape(input, model)?;
    if needs_dense_key_estimate(&shape.spec, &shape.descriptor_resolution) {
        let rows = shape
            .residency
            .relations
            .iter()
            .find(|relation| relation.relation_oid == shape.spec.fact_rel)
            .map_or(0, |relation| relation.estimated_rows);
        // SAFETY: root remains owned by this live planner invocation. The
        // function only obtains per-key PostgreSQL estimates, never device data.
        // Every qualified dense workspace forecast needs Cartesian dictionary
        // slots; a joint output-row estimate cannot replace this product.
        shape.estimated_dense_key_cardinalities =
            unsafe { postgres::estimate_dense_key_cardinalities(root, rows) };
    }
    Ok(shape)
}

fn needs_dense_key_estimate(spec: &AggQuerySpec, resolution: &DescriptorResolution) -> bool {
    // Preserve the existing decimal/scalar evidence contract independently of
    // the newly covered fixed-width families.
    if spec.measures.iter().any(|measure| {
        measure.requires_numeric_accumulator()
            || matches!(measure.expression, MeasureExpr::Bytecode { .. })
    }) {
        return true;
    }
    let hash_grouping = match resolution {
        DescriptorResolution::BeginTimeArtifacts { grouping_mode, .. } => {
            *grouping_mode == DescriptorGroupingMode::Hash
        }
        DescriptorResolution::Ready => spec
            .group_keys
            .iter()
            .any(|key| key.encoding == GroupKeyEncoding::Hash),
    };
    spec.star_dims.is_empty()
        && matches!(
            crate::engine::executor::agg::planned_descriptor_kernel_mode(spec, hash_grouping),
            crate::gpu::GroupedAggKernelMode::ParallelDenseInteger
                | crate::gpu::GroupedAggKernelMode::ParallelDenseCount
        )
}

#[cfg(test)]
#[test]
fn dense_key_estimate_trigger_preserves_families_and_actual_resolution() {
    use crate::engine::spec::{
        AggregateKind, AggregateSource, DimSpec, FilterSpec, GroupKeyRef, GroupKeySource,
        JoinMultiplicity, MeasureSpec,
    };
    let column = ColumnRef {
        relation_oid: 42,
        attno: 1,
        type_oid: 23,
    };
    let mut spec = AggQuerySpec {
        fact_rel: 42,
        group_keys: vec![],
        star_dims: vec![],
        having: None,
        fact_filter: FilterSpec::None,
        measures: vec![MeasureSpec {
            expression: MeasureExpr::CountStar,
            filter: FilterSpec::None,
            outputs: vec![AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }],
        }],
    };
    let dense = DescriptorResolution::BeginTimeArtifacts {
        dictionary_keys: vec![],
        derived_keys: vec![],
        joins: vec![],
        grouping_mode: DescriptorGroupingMode::DenseDictionary,
        max_group_count: 1024,
    };
    let mut hash = dense.clone();
    if let DescriptorResolution::BeginTimeArtifacts { grouping_mode, .. } = &mut hash {
        *grouping_mode = DescriptorGroupingMode::Hash;
    }
    assert!(needs_dense_key_estimate(
        &spec,
        &DescriptorResolution::Ready
    ));
    spec.group_keys.push(GroupKeyRef {
        source: GroupKeySource::FactColumn(column),
        type_oid: 23,
        collation_oid: 0,
        encoding: GroupKeyEncoding::Hash,
    });
    assert!(
        needs_dense_key_estimate(&spec, &dense),
        "unresolved Hash encoding is a future dense dictionary"
    );
    assert!(!needs_dense_key_estimate(&spec, &hash));
    assert!(!needs_dense_key_estimate(
        &spec,
        &DescriptorResolution::Ready
    ));
    spec.measures[0].expression = MeasureExpr::Column(column);
    spec.measures[0].outputs[0].kind = AggregateKind::Sum;
    assert!(needs_dense_key_estimate(&spec, &dense));
    spec.measures[0].outputs[0].kind = AggregateKind::Avg;
    assert!(needs_dense_key_estimate(&spec, &dense));
    spec.star_dims.push(DimSpec {
        relation_oid: 43,
        fact_key: column,
        dim_key: ColumnRef {
            relation_oid: 43,
            ..column
        },
        collation_oid: 0,
        multiplicity: JoinMultiplicity::Unique,
        filter: FilterSpec::None,
    });
    assert!(!needs_dense_key_estimate(&spec, &dense));
    spec.star_dims.clear();
    spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
        type_oid: 701,
        ..column
    });
    spec.measures[0].outputs[0].kind = AggregateKind::Sum;
    assert!(
        !needs_dense_key_estimate(&spec, &dense),
        "serial float SUM is not newly covered"
    );
    spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
        type_oid: 1700,
        ..column
    });
    assert!(
        needs_dense_key_estimate(&spec, &hash),
        "existing NUMERIC trigger is unchanged"
    );
    spec.measures[0].expression = MeasureExpr::Bytecode {
        inputs: vec![],
        program: vec![],
        result_type_oid: 23,
    };
    assert!(
        needs_dense_key_estimate(&spec, &hash),
        "existing scalar trigger is unchanged"
    );
}

#[cfg(test)]
mod tests;

/// Dictionary capacity is a Cartesian product, not a joint group estimate.
/// PostgreSQL ndistinct does not reserve our explicit NULL code. Extra slots
/// are conservative if an estimate already happens to include the NULL group.
pub(crate) fn dense_slot_product(cardinalities: &[u64], nullable: &[bool]) -> Option<u64> {
    if cardinalities.len() != nullable.len() {
        return None;
    }
    cardinalities
        .iter()
        .zip(nullable)
        .try_fold(1_u64, |slots, (&count, &has_null)| {
            slots.checked_mul(count.checked_add(u64::from(has_null))?.max(1))
        })
}

// Read-only live-planner observation: exercise PostgreSQL's per-key estimates
// before NUMERIC SQL admission is enabled. This never injects or forces a path.
#[cfg(feature = "pg_test")]
struct TestDenseSlotCapture {
    rows: u64,
    estimates: Vec<Option<Vec<u64>>>,
}

#[cfg(feature = "pg_test")]
thread_local! {
    static TEST_DENSE_SLOTS: std::cell::RefCell<Option<TestDenseSlotCapture>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "pg_test")]
pub(crate) fn test_capture_dense_slots(rows: u64, f: impl FnOnce()) -> Vec<Option<Vec<u64>>> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_DENSE_SLOTS.with(|state| *state.borrow_mut() = None);
        }
    }
    TEST_DENSE_SLOTS.with(|state| {
        let mut state = state.borrow_mut();
        assert!(state.is_none(), "nested dense-slot capture");
        *state = Some(TestDenseSlotCapture {
            rows,
            estimates: Vec::new(),
        });
    });
    let _reset = Reset;
    f();
    TEST_DENSE_SLOTS.with(|state| state.borrow_mut().take().expect("capture active").estimates)
}

#[cfg(feature = "pg_test")]
pub(super) unsafe fn observe_test_dense_slots(root: *mut pg_sys::PlannerInfo) {
    let rows = TEST_DENSE_SLOTS.with(|state| state.borrow().as_ref().map(|capture| capture.rows));
    if let Some(rows) = rows {
        // SAFETY: caller supplies the live GROUP_AGG root, just as extract_shape.
        let estimate = unsafe { postgres::estimate_dense_key_cardinalities(root, rows) };
        TEST_DENSE_SLOTS.with(|state| {
            if let Some(capture) = state.borrow_mut().as_mut() {
                capture.estimates.push(estimate);
            }
        });
    }
}

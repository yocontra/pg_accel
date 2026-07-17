//! Catalog-driven aggregate-shape extraction for the unified OLAP path.
//!
//! [`extract_shape`] emits a logical [`AggQuerySpec`], ordered output
//! projection metadata, exact relation/attribute requirements, and typed
//! cost/residency metadata. The generic upper-path hook resolves the initially
//! unknown residency facts before it can admit the childless descriptor path.

mod builder;
mod cost;
mod postgres;

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
pub use cost::{ShapeCost, ShapeCostGate, estimate_shape_cost};

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

/// Begin-time grouping implementation selected for the logical AQS3 spec.
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
    build_shape(input, model)
}

#[cfg(test)]
mod tests;

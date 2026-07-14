use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use pgrx::pg_sys;

use crate::engine::cost::{DeviceLimits, TypedCostModel};
use crate::engine::spec::{
    AggOutputProjection, AggregateKind, AggregateOutput, AggregateSource, BinaryMeasureOp,
    ColumnRef, FilterSpec, GroupKeySource, JoinMultiplicity, MeasureExpr, ScalarRange, ScalarValue,
};

use super::*;

fn model() -> TypedCostModel {
    TypedCostModel::from_limits(&DeviceLimits::cpu_only())
}

fn relation(varno: pg_sys::Index, relation_oid: u32, estimated_rows: u64) -> RelationShape {
    RelationShape {
        varno,
        relation_oid,
        estimated_rows,
        unique_attnos: BTreeSet::new(),
        column_widths: BTreeMap::from([(1, 4), (2, 8), (3, 8), (4, 1)]),
        residency: RelationResidency::Unknown,
    }
}

fn column(varno: pg_sys::Index, relation_oid: u32, attno: i32, type_oid: u32) -> PlannerColumn {
    PlannerColumn {
        varno,
        column: ColumnRef {
            relation_oid,
            attno,
            type_oid,
        },
        collation_oid: 0,
        collation_is_deterministic: true,
    }
}

fn output(result_type_oid: u32, nullable: bool) -> OutputMetadata {
    OutputMetadata {
        source_type_oid: result_type_oid,
        result_type_oid,
        result_typmod: -1,
        result_collation_oid: 0,
        nullable,
    }
}

fn aggregate(
    expression: MeasureExpr,
    kind: AggregateKind,
    result_type_oid: u32,
) -> (AggregateExpr, OutputMetadata) {
    let source_type_oid = match &expression {
        MeasureExpr::CountStar => 0,
        MeasureExpr::Column(column) | MeasureExpr::Binary { lhs: column, .. } => column.type_oid,
        MeasureExpr::StatsPair { value, .. } => value.type_oid,
        MeasureExpr::Bytecode {
            result_type_oid, ..
        } => *result_type_oid,
    };
    let mut metadata = output(result_type_oid, kind != AggregateKind::Count);
    metadata.source_type_oid = source_type_oid;
    (
        AggregateExpr {
            expression,
            output: AggregateOutput {
                source: AggregateSource::Value,
                kind,
            },
            filter: FilterSpec::None,
        },
        metadata,
    )
}

fn single_table_input() -> ShapeInput {
    let value = column(1, 100, 2, u32::from(pg_sys::FLOAT8OID));
    let (minimum, minimum_output) = aggregate(
        MeasureExpr::Column(value.column),
        AggregateKind::Min,
        u32::from(pg_sys::FLOAT8OID),
    );
    ShapeInput {
        relations: vec![relation(1, 100, 1_000_000)],
        joins: Vec::new(),
        group_keys: Vec::new(),
        aggregates: vec![minimum],
        projections: vec![InputProjection::Aggregate {
            aggregate_index: 0,
            output: minimum_output,
        }],
        relation_filters: Vec::new(),
        estimated_output_rows: 1,
        expected_reuses: NonZeroU32::MIN,
        modifiers: ShapeModifiers::default(),
    }
}

fn add_dimension(input: &mut ShapeInput, varno: pg_sys::Index, relation_oid: u32, group: bool) {
    let fact_attno = i32::try_from(varno).unwrap_or(i32::MAX).saturating_add(2);
    let fact_key = column(1, 100, fact_attno, u32::from(pg_sys::INT4OID));
    let dim_key = column(varno, relation_oid, 1, u32::from(pg_sys::INT4OID));
    let mut dimension = relation(varno, relation_oid, 1_000);
    dimension.unique_attnos.insert(1);
    input.relations.push(dimension);
    input.joins.push(EquiJoin {
        left: fact_key,
        right: dim_key,
    });
    if group {
        let group_column = column(varno, relation_oid, 2, u32::from(pg_sys::FLOAT8OID));
        input.group_keys.push(PlannerGroupKey::Column(group_column));
        input.projections.insert(
            input.group_keys.len() - 1,
            InputProjection::Group {
                key: PlannerGroupKey::Column(group_column),
                output: output(u32::from(pg_sys::FLOAT8OID), true),
            },
        );
    }
}

#[test]
fn single_table_reduce_emits_neutral_spec_and_typed_projection() {
    let plan = build_shape(single_table_input(), &model()).expect("shape should build");
    assert_eq!(plan.spec.fact_rel, 100);
    assert!(plan.spec.group_keys.is_empty());
    assert!(plan.spec.star_dims.is_empty());
    assert_eq!(plan.spec.measures.len(), 1);
    assert_eq!(
        plan.projections,
        vec![ProjectionSlot {
            source: ProjectionSource::Aggregate {
                measure_index: 0,
                source: AggregateSource::Value,
                kind: AggregateKind::Min,
            },
            source_type_oid: u32::from(pg_sys::FLOAT8OID),
            result_type_oid: u32::from(pg_sys::FLOAT8OID),
            result_typmod: -1,
            result_collation_oid: 0,
            nullable: true,
        }]
    );
    assert_eq!(
        plan.required_relations,
        vec![RequiredRelation {
            relation_oid: 100,
            attnos: vec![2],
        }]
    );
    assert_eq!(plan.residency.total_required_bytes, Some(8_000_000));
    assert_eq!(plan.cost_gate, ShapeCostGate::Eligible);
    assert!(plan.descriptor_spec().is_ok());
}

#[test]
fn int4_sum_carries_distinct_source_and_result_types() {
    let mut input = single_table_input();
    let (sum, sum_output) = aggregate(
        MeasureExpr::Column(ColumnRef {
            relation_oid: 100,
            attno: 1,
            type_oid: u32::from(pg_sys::INT4OID),
        }),
        AggregateKind::Sum,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![sum];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: sum_output,
    }];

    let plan = build_shape(input, &model()).expect("SUM(int4) is descriptor-representable");
    assert_eq!(
        plan.projections[0].source_type_oid,
        u32::from(pg_sys::INT4OID)
    );
    assert_eq!(
        plan.projections[0].result_type_oid,
        u32::from(pg_sys::INT8OID)
    );
}

#[test]
fn count_star_keeps_fact_residency_requirement_without_columns() {
    let mut input = single_table_input();
    let (count, count_output) = aggregate(
        MeasureExpr::CountStar,
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![count];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: count_output,
    }];
    let plan = build_shape(input, &model()).expect("COUNT(*) shape should build");
    assert_eq!(plan.required_relations[0].relation_oid, 100);
    assert!(plan.required_relations[0].attnos.is_empty());
    assert!(!plan.projections[0].nullable);
    assert_eq!(
        plan.projections[0].source_type_oid,
        u32::from(pg_sys::InvalidOid)
    );
}

#[test]
fn projection_source_and_result_type_mismatches_decline_precisely() {
    let mut wrong_source = single_table_input();
    let InputProjection::Aggregate { output, .. } = &mut wrong_source.projections[0] else {
        panic!("fixture projection is aggregate");
    };
    output.source_type_oid = u32::from(pg_sys::INT8OID);
    assert_eq!(
        build_shape(wrong_source, &model()),
        Err(ShapeDecline::ProjectionSourceTypeMismatch {
            expected_type_oid: u32::from(pg_sys::FLOAT8OID),
            actual_type_oid: u32::from(pg_sys::INT8OID),
        })
    );

    let mut wrong_result = single_table_input();
    let InputProjection::Aggregate { output, .. } = &mut wrong_result.projections[0] else {
        panic!("fixture projection is aggregate");
    };
    output.result_type_oid = u32::from(pg_sys::INT8OID);
    assert_eq!(
        build_shape(wrong_result, &model()),
        Err(ShapeDecline::AggregateResultTypeMismatch {
            source_type_oid: u32::from(pg_sys::FLOAT8OID),
            kind: AggregateKind::Min,
            result_type_oid: u32::from(pg_sys::INT8OID),
        })
    );
}

#[test]
fn groupagg_coalesces_measure_outputs_but_preserves_projection_order() {
    let mut input = single_table_input();
    let group = column(1, 100, 1, u32::from(pg_sys::INT4OID));
    let value = column(1, 100, 2, u32::from(pg_sys::FLOAT8OID));
    let (count, count_output) = aggregate(
        MeasureExpr::Column(value.column),
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.group_keys = vec![PlannerGroupKey::Column(group)];
    input.aggregates.push(count);
    input.projections = vec![
        InputProjection::Group {
            key: PlannerGroupKey::Column(group),
            output: output(u32::from(pg_sys::INT4OID), false),
        },
        InputProjection::Aggregate {
            aggregate_index: 1,
            output: count_output,
        },
        InputProjection::Aggregate {
            aggregate_index: 0,
            output: output(u32::from(pg_sys::FLOAT8OID), true),
        },
    ];
    let plan = build_shape(input, &model()).expect("group aggregate should build");
    assert_eq!(plan.spec.measures.len(), 1);
    assert_eq!(
        plan.spec.measures[0].outputs,
        vec![
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Min,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            },
        ]
    );
    assert!(matches!(
        plan.projections[1].source,
        ProjectionSource::Aggregate {
            measure_index: 0,
            kind: AggregateKind::Count,
            ..
        }
    ));
    assert!(matches!(
        plan.projections[2].source,
        ProjectionSource::Aggregate {
            measure_index: 0,
            kind: AggregateKind::Min,
            ..
        }
    ));
}

#[test]
fn grouped_plan_requires_exact_dictionary_resolution_before_descriptor_use() {
    let mut input = single_table_input();
    let group = column(1, 100, 1, u32::from(pg_sys::INT4OID));
    input.group_keys = vec![PlannerGroupKey::Column(group)];
    input.projections.insert(
        0,
        InputProjection::Group {
            key: PlannerGroupKey::Column(group),
            output: output(u32::from(pg_sys::INT4OID), false),
        },
    );
    input.estimated_output_rows = 10;
    let plan = build_shape(input, &model()).expect("group shape should build as logical IR");
    assert!(matches!(
        &plan.descriptor_resolution,
        DescriptorResolution::BeginTimeArtifacts {
            grouping_mode: DescriptorGroupingMode::DenseDictionary,
            ..
        }
    ));
    assert_eq!(
        plan.descriptor_spec(),
        Err(ShapeDecline::DescriptorArtifactsRequireResolution)
    );

    let resolved = plan
        .resolve_dictionary_keys(&[ResolvedDictionaryKey {
            key_index: 0,
            cardinality: 10,
            null_code: None,
        }])
        .expect("exact resident dictionary should resolve");
    assert!(
        resolved
            .group_keys
            .iter()
            .all(|key| !matches!(key.encoding, crate::engine::spec::GroupKeyEncoding::Hash))
    );
    resolved
        .validate()
        .expect("resolved spec must be descriptor-valid");

    let max_groups = model().memory.gpu_hash_agg_max_groups.get();
    let too_many = u32::try_from(max_groups + 1).expect("test device maximum fits u32");
    assert_eq!(
        plan.resolve_dictionary_keys(&[ResolvedDictionaryKey {
            key_index: 0,
            cardinality: too_many,
            null_code: None,
        }]),
        Err(ShapeDecline::InvalidGroupKeyResolution)
    );
}

#[test]
fn h3_group_key_requires_a_derived_hash_lane_without_dictionary_rewrite() {
    const H3INDEXOID: u32 = 90_001;
    let mut input = single_table_input();
    let source = GroupKeySource::H3CellToParent {
        cell: ColumnRef {
            relation_oid: 100,
            attno: 1,
            type_oid: H3INDEXOID,
        },
        resolution: 7,
    };
    let key = PlannerGroupKey::Expression {
        source: source.clone(),
        type_oid: H3INDEXOID,
        collation_oid: 0,
        collation_is_deterministic: true,
    };
    input.group_keys.push(key.clone());
    input.projections.insert(
        0,
        InputProjection::Group {
            key,
            output: output(H3INDEXOID, true),
        },
    );

    let plan = build_shape(input, &model()).expect("H3 logical shape should build");
    let DescriptorResolution::BeginTimeArtifacts {
        dictionary_keys,
        derived_keys,
        grouping_mode,
        ..
    } = &plan.descriptor_resolution
    else {
        panic!("H3 grouping must require a derived artifact");
    };
    assert!(dictionary_keys.is_empty());
    assert_eq!(*grouping_mode, DescriptorGroupingMode::Hash);
    assert_eq!(derived_keys.len(), 1);
    assert_eq!(derived_keys[0].source, source);

    let resolved = plan
        .resolve_group_key_artifacts(
            &[],
            &[ResolvedDerivedGroupKey {
                key_index: 0,
                source: source.clone(),
                result_type_oid: H3INDEXOID,
            }],
        )
        .expect("matching H3 artifact should resolve");
    assert_eq!(resolved.group_keys[0].encoding, GroupKeyEncoding::Hash);
    assert_eq!(resolved.group_keys[0].source, source);

    assert_eq!(
        plan.resolve_group_key_artifacts(
            &[],
            &[ResolvedDerivedGroupKey {
                key_index: 0,
                source: GroupKeySource::H3CellToParent {
                    cell: ColumnRef {
                        relation_oid: 100,
                        attno: 1,
                        type_oid: H3INDEXOID,
                    },
                    resolution: 8,
                },
                result_type_oid: H3INDEXOID,
            }],
        ),
        Err(ShapeDecline::InvalidGroupKeyResolution)
    );
}

#[test]
fn omitted_group_key_declines_before_path_construction() {
    let mut input = single_table_input();
    let group = column(1, 100, 1, u32::from(pg_sys::INT4OID));
    input.group_keys = vec![PlannerGroupKey::Column(group)];

    let decline = build_shape(input, &model()).expect_err("hidden group output must decline");
    assert_eq!(
        decline,
        ShapeDecline::UnprojectedGroupKey {
            relation_oid: 100,
            attno: 1,
        }
    );
    assert_eq!(decline.code(), "shape_unprojected_group_key");
}

#[test]
fn repeated_sql_aggregate_reuses_one_output_lane() {
    let mut input = single_table_input();
    input.aggregates.push(input.aggregates[0].clone());
    input.projections.push(InputProjection::Aggregate {
        aggregate_index: 1,
        output: output(u32::from(pg_sys::FLOAT8OID), true),
    });
    let plan = build_shape(input, &model()).expect("duplicate projection should build");
    assert_eq!(plan.spec.measures.len(), 1);
    assert_eq!(plan.spec.measures[0].outputs.len(), 1);
    assert_eq!(plan.projections.len(), 2);
}

#[test]
fn four_dimension_star_is_catalog_driven_and_deterministic() {
    let mut input = single_table_input();
    add_dimension(&mut input, 2, 204, true);
    add_dimension(&mut input, 3, 201, true);
    add_dimension(&mut input, 4, 203, true);
    add_dimension(&mut input, 5, 202, false);
    let plan = build_shape(input, &model()).expect("four-dimension star should build");
    assert_eq!(
        plan.spec
            .star_dims
            .iter()
            .map(|dimension| dimension.relation_oid)
            .collect::<Vec<_>>(),
        vec![201, 202, 203, 204]
    );
    assert_eq!(plan.spec.group_keys.len(), 3);
    for key in &plan.spec.group_keys {
        assert!(matches!(key.source, GroupKeySource::StarDimension { .. }));
    }
    assert!(
        plan.spec
            .star_dims
            .iter()
            .all(|dimension| dimension.multiplicity == JoinMultiplicity::Unique)
    );
}

#[test]
fn one_dimension_uses_unique_side_as_dimension_without_names() {
    let mut input = single_table_input();
    input.relations[0].estimated_rows = 1_000;
    add_dimension(&mut input, 2, 200, true);
    input.relations[1].estimated_rows = 2_000;
    let plan = build_shape(input, &model()).expect("unique key resolves fact orientation");
    assert_eq!(plan.spec.fact_rel, 100);
    assert_eq!(plan.spec.star_dims[0].relation_oid, 200);
}

#[test]
fn equal_two_relation_shape_without_unique_evidence_declines_ambiguously() {
    let mut input = single_table_input();
    let (count, count_output) = aggregate(
        MeasureExpr::CountStar,
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![count];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: count_output,
    }];
    let fact_key = column(1, 100, 3, u32::from(pg_sys::INT4OID));
    let dim_key = column(2, 200, 1, u32::from(pg_sys::INT4OID));
    input.relations.push(relation(2, 200, 1_000_000));
    input.joins.push(EquiJoin {
        left: fact_key,
        right: dim_key,
    });
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::AmbiguousFactRelation)
    );
}

#[test]
fn chain_join_declines_instead_of_guessing_a_fact_table() {
    let mut input = single_table_input();
    input.relations.extend([
        relation(2, 200, 1_000),
        relation(3, 300, 1_000),
        relation(4, 400, 1_000),
    ]);
    input.joins = vec![
        EquiJoin {
            left: column(1, 100, 3, 23),
            right: column(2, 200, 1, 23),
        },
        EquiJoin {
            left: column(2, 200, 2, 23),
            right: column(3, 300, 1, 23),
        },
        EquiJoin {
            left: column(3, 300, 2, 23),
            right: column(4, 400, 1, 23),
        },
    ];
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::NonStarJoinGraph)
    );
}

#[test]
fn grouping_by_counted_dimension_declines_precisely() {
    let mut input = single_table_input();
    add_dimension(&mut input, 2, 200, true);
    input.relations[1].unique_attnos.clear();
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::GroupedByNonUniqueDimension {
            relation_oid: 200,
            attno: 2,
        })
    );
}

#[test]
fn relation_filter_is_preserved_and_selected_columns_are_deduplicated() {
    let mut input = single_table_input();
    input.relation_filters.push((
        100,
        FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 100,
                attno: 1,
                type_oid: 23,
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(10),
                hi: ScalarValue::I32(20),
            }],
        },
    ));
    let plan = build_shape(input, &model()).expect("range filter should build");
    assert!(matches!(plan.spec.fact_filter, FilterSpec::Ranges { .. }));
    assert_eq!(plan.required_relations[0].attnos, vec![1, 2]);
    assert_eq!(plan.residency.total_required_bytes, Some(12_000_000));
    assert_eq!(plan.descriptor_measures.projected_measure_count, 1);
    assert_eq!(plan.descriptor_measures.descriptor_measure_count, 2);
    assert_eq!(
        plan.descriptor_measures.fact_filter,
        Some(DescriptorFilterBinding {
            measure_index: 1,
            source: AggregateSource::Value,
            hidden: true,
        })
    );
    assert_eq!(plan.descriptor_measures.derived_fact_mask, None);
}

#[test]
fn sql_boolean_fact_filter_requires_begin_time_null_aware_mask() {
    let mut input = single_table_input();
    let filter_column = ColumnRef {
        relation_oid: 100,
        attno: 4,
        type_oid: u32::from(pg_sys::BOOLOID),
    };
    input.relation_filters.push((
        100,
        FilterSpec::Mask {
            input: filter_column,
            kind: crate::engine::spec::MaskKind::Sql,
        },
    ));
    let plan = build_shape(input, &model()).expect("SQL bool mask should resolve at Begin");
    assert_eq!(plan.descriptor_measures.descriptor_measure_count, 1);
    assert_eq!(plan.descriptor_measures.fact_filter, None);
    assert_eq!(
        plan.descriptor_measures.derived_fact_mask,
        Some(filter_column)
    );
}

#[test]
fn physical_fact_range_types_create_hidden_descriptor_inputs() {
    for (type_oid, lo, hi) in [
        (
            u32::from(pg_sys::FLOAT4OID),
            ScalarValue::F32(0.0),
            ScalarValue::F32(1.0),
        ),
        (
            u32::from(pg_sys::DATEOID),
            ScalarValue::Date(-10),
            ScalarValue::Date(10),
        ),
        (
            u32::from(pg_sys::TIMESTAMPOID),
            ScalarValue::Timestamp(-20),
            ScalarValue::Timestamp(20),
        ),
        (
            u32::from(pg_sys::TIMESTAMPTZOID),
            ScalarValue::TimestampTz(-30),
            ScalarValue::TimestampTz(30),
        ),
    ] {
        let mut input = single_table_input();
        input.relation_filters.push((
            100,
            FilterSpec::Ranges {
                input: ColumnRef {
                    relation_oid: 100,
                    attno: 3,
                    type_oid,
                },
                ranges: vec![ScalarRange { lo, hi }],
            },
        ));
        let plan = build_shape(input, &model()).expect("physical fact filter should build");
        assert_eq!(plan.descriptor_measures.descriptor_measure_count, 2);
        assert_eq!(
            plan.descriptor_measures.fact_filter,
            Some(DescriptorFilterBinding {
                measure_index: 1,
                source: AggregateSource::Value,
                hidden: true,
            })
        );
    }
}

#[test]
fn float_predicate_adapter_does_not_fake_an_upper_bound_below_nan() {
    let less_equal = pg_sys::BTLessEqualStrategyNumber as i32;
    let greater_equal = pg_sys::BTGreaterEqualStrategyNumber as i32;
    let equal = pg_sys::BTEqualStrategyNumber as i32;
    assert!(super::postgres::range_for_strategy(ScalarValue::F64(1.0), less_equal).is_ok());
    assert_eq!(
        super::postgres::range_for_strategy(ScalarValue::F64(1.0), greater_equal),
        Err(ShapeDecline::UnsupportedPredicate)
    );
    assert_eq!(
        super::postgres::range_for_strategy(ScalarValue::F64(f64::NAN), equal),
        Err(ShapeDecline::UnsupportedPredicate)
    );
}

#[test]
fn temporal_predicate_ranges_preserve_temporal_scalar_identity() {
    let less_equal = pg_sys::BTLessEqualStrategyNumber as i32;
    let greater = pg_sys::BTGreaterStrategyNumber as i32;
    assert_eq!(
        super::postgres::range_for_strategy(ScalarValue::Date(7), less_equal).expect("date range"),
        ScalarRange {
            lo: ScalarValue::Date(i32::MIN),
            hi: ScalarValue::Date(7),
        }
    );
    assert_eq!(
        super::postgres::range_for_strategy(ScalarValue::Timestamp(11), greater)
            .expect("timestamp range"),
        ScalarRange {
            lo: ScalarValue::Timestamp(12),
            hi: ScalarValue::Timestamp(i64::MAX),
        }
    );
    assert_eq!(
        super::postgres::range_for_strategy(ScalarValue::TimestampTz(13), greater)
            .expect("timestamptz range"),
        ScalarRange {
            lo: ScalarValue::TimestampTz(14),
            hi: ScalarValue::TimestampTz(i64::MAX),
        }
    );
}

#[test]
fn table_sample_is_a_structural_decline() {
    assert_eq!(
        super::postgres::reject_table_sample(true),
        Err(ShapeDecline::TableSample)
    );
    assert_eq!(super::postgres::reject_table_sample(false), Ok(()));
    assert_eq!(ShapeDecline::TableSample.code(), "shape_table_sample");
}

#[test]
fn range_table_preflight_rejects_function_inputs_without_device_discovery() {
    use super::postgres::{
        PreflightRangeTableEntry, PreflightRangeTableKind, preflight_range_table_entry,
    };

    assert_eq!(
        preflight_range_table_entry(
            3,
            PreflightRangeTableEntry {
                kind: PreflightRangeTableKind::Unsupported,
                eligible_base_relation: false,
                has_table_sample: false,
            },
        ),
        Err(ShapeDecline::UnsupportedRangeTableEntry { varno: 3 })
    );
    assert_eq!(
        preflight_range_table_entry(
            1,
            PreflightRangeTableEntry {
                kind: PreflightRangeTableKind::BaseRelation,
                eligible_base_relation: true,
                has_table_sample: false,
            },
        ),
        Ok(true)
    );
    assert_eq!(
        preflight_range_table_entry(
            2,
            PreflightRangeTableEntry {
                kind: PreflightRangeTableKind::Synthetic,
                eligible_base_relation: false,
                has_table_sample: false,
            },
        ),
        Ok(false)
    );
}

#[test]
fn dimension_filter_capability_matches_begin_time_sidecar_builder() {
    let mut ranged = single_table_input();
    add_dimension(&mut ranged, 2, 200, false);
    ranged.relation_filters.push((
        200,
        FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 200,
                attno: 3,
                type_oid: u32::from(pg_sys::FLOAT4OID),
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::F32(0.0),
                hi: ScalarValue::F32(1.0),
            }],
        },
    ));
    assert!(build_shape(ranged, &model()).is_ok());

    let mut recheck = single_table_input();
    add_dimension(&mut recheck, 2, 200, false);
    recheck.relation_filters.push((
        200,
        FilterSpec::Mask {
            input: ColumnRef {
                relation_oid: 200,
                attno: 4,
                type_oid: u32::from(pg_sys::BOOLOID),
            },
            kind: crate::engine::spec::MaskKind::Recheck,
        },
    ));
    assert_eq!(
        build_shape(recheck, &model()),
        Err(ShapeDecline::UnsupportedFilterType {
            type_oid: u32::from(pg_sys::BOOLOID),
        })
    );

    let mut bytecode = single_table_input();
    add_dimension(&mut bytecode, 2, 200, false);
    bytecode.relation_filters.push((
        200,
        FilterSpec::Bytecode {
            inputs: vec![ColumnRef {
                relation_oid: 200,
                attno: 1,
                type_oid: u32::from(pg_sys::INT4OID),
            }],
            program: vec![1],
        },
    ));
    assert_eq!(
        build_shape(bytecode, &model()),
        Err(ShapeDecline::UnsupportedPredicate)
    );
}

#[test]
fn independent_fact_filter_cannot_overflow_four_descriptor_measure_slots() {
    let mut input = single_table_input();
    input.relations[0].column_widths.insert(5, 8);
    input.aggregates.clear();
    input.projections.clear();
    for attno in 1..=4 {
        let (aggregate, aggregate_output) = aggregate(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 100,
                attno,
                type_oid: 701,
            }),
            AggregateKind::Min,
            701,
        );
        let aggregate_index = u32::try_from(input.aggregates.len()).expect("four slots fit u32");
        input.aggregates.push(aggregate);
        input.projections.push(InputProjection::Aggregate {
            aggregate_index,
            output: aggregate_output,
        });
    }
    input.relation_filters.push((
        100,
        FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 100,
                attno: 5,
                type_oid: 701,
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::F64(0.0),
                hi: ScalarValue::F64(1.0),
            }],
        },
    ));
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::TooManyMeasures {
            actual: 5,
            maximum: 4,
        })
    );
}

#[test]
fn fact_filter_reuses_existing_value_slot_at_four_measure_boundary() {
    let mut input = single_table_input();
    input.aggregates.clear();
    input.projections.clear();
    for attno in 1..=4 {
        let (aggregate, aggregate_output) = aggregate(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 100,
                attno,
                type_oid: 701,
            }),
            AggregateKind::Min,
            701,
        );
        let aggregate_index = u32::try_from(input.aggregates.len()).expect("four slots fit u32");
        input.aggregates.push(aggregate);
        input.projections.push(InputProjection::Aggregate {
            aggregate_index,
            output: aggregate_output,
        });
    }
    input.relation_filters.push((
        100,
        FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 100,
                attno: 4,
                type_oid: 701,
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::F64(0.0),
                hi: ScalarValue::F64(1.0),
            }],
        },
    ));
    let plan = build_shape(input, &model()).expect("existing value slot should be reused");
    assert_eq!(plan.spec.measures.len(), 4);
    assert_eq!(plan.descriptor_measures.descriptor_measure_count, 4);
    assert_eq!(
        plan.descriptor_measures.fact_filter,
        Some(DescriptorFilterBinding {
            measure_index: 3,
            source: AggregateSource::Value,
            hidden: false,
        })
    );
}

#[test]
fn binary_rhs_fact_filter_uses_hidden_value_slot_and_respects_boundary() {
    let mut input = single_table_input();
    input.relations[0].column_widths.insert(5, 8);
    input.aggregates.clear();
    input.projections.clear();
    let (binary, binary_output) = aggregate(
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Sub,
            lhs: ColumnRef {
                relation_oid: 100,
                attno: 1,
                type_oid: 20,
            },
            rhs: ColumnRef {
                relation_oid: 100,
                attno: 5,
                type_oid: 20,
            },
        },
        AggregateKind::Min,
        20,
    );
    input.aggregates.push(binary);
    input.projections.push(InputProjection::Aggregate {
        aggregate_index: 0,
        output: binary_output,
    });
    for attno in 2..=4 {
        let (aggregate, aggregate_output) = aggregate(
            MeasureExpr::Column(ColumnRef {
                relation_oid: 100,
                attno,
                type_oid: 20,
            }),
            AggregateKind::Min,
            20,
        );
        let aggregate_index = u32::try_from(input.aggregates.len()).expect("four slots fit u32");
        input.aggregates.push(aggregate);
        input.projections.push(InputProjection::Aggregate {
            aggregate_index,
            output: aggregate_output,
        });
    }
    input.relation_filters.push((
        100,
        FilterSpec::Ranges {
            input: ColumnRef {
                relation_oid: 100,
                attno: 5,
                type_oid: 20,
            },
            ranges: vec![ScalarRange {
                lo: ScalarValue::I64(0),
                hi: ScalarValue::I64(1),
            }],
        },
    ));
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::TooManyMeasures {
            actual: 5,
            maximum: 4,
        })
    );
}

#[test]
fn auto_load_cost_is_amortized_by_explicit_reuse_evidence() {
    let mut input = single_table_input();
    input.expected_reuses = NonZeroU32::new(4).expect("four is nonzero");
    let limits = DeviceLimits::cpu_only();
    let typed = TypedCostModel::from_limits(&limits);
    let plan = build_shape(input, &typed).expect("shape should build");
    let expected = 1_000_000_f64 * limits.preagg_dim_materialize_cost / 4.0;
    assert_eq!(plan.residency.missing_rows, 1_000_000);
    assert_eq!(plan.residency.amortized_load_cost.get(), expected);
    assert_eq!(plan.cost.amortized_auto_load.get(), expected);
}

#[test]
fn resident_relation_has_no_auto_load_charge() {
    let mut input = single_table_input();
    input.relations[0].residency = RelationResidency::Resident;
    let plan = build_shape(input, &model()).expect("resident shape should build");
    assert_eq!(plan.residency.missing_rows, 0);
    assert_eq!(plan.residency.missing_bytes, Some(0));
    assert_eq!(plan.cost.amortized_auto_load.get(), 0.0);
}

#[test]
fn cost_gate_reads_device_limits_without_magic_undercut() {
    let mut input = single_table_input();
    input.relations[0].estimated_rows = 99;
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_reduce_min_rows = 100;
    let plan = build_shape(input, &TypedCostModel::from_limits(&limits))
        .expect("shape remains available for an honest cost decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::FactRowsBelowDeviceMinimum {
            estimated: crate::engine::cost::Rows::new(99),
            required: crate::engine::cost::Rows::new(100),
        }
    );
}

fn h3_parent_input(estimated_rows: u64, resolution: i32) -> ShapeInput {
    const H3INDEXOID: u32 = 90_001;
    let mut input = single_table_input();
    input.relations[0].estimated_rows = estimated_rows;
    input.relations[0].column_widths.insert(1, 8);
    let source = GroupKeySource::H3CellToParent {
        cell: ColumnRef {
            relation_oid: 100,
            attno: 1,
            type_oid: H3INDEXOID,
        },
        resolution,
    };
    let key = PlannerGroupKey::Expression {
        source,
        type_oid: H3INDEXOID,
        collation_oid: 0,
        collation_is_deterministic: true,
    };
    let (count, count_output) = aggregate(
        MeasureExpr::CountStar,
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.group_keys = vec![key.clone()];
    input.aggregates = vec![count];
    input.projections = vec![
        InputProjection::Group {
            key,
            output: output(H3INDEXOID, true),
        },
        InputProjection::Aggregate {
            aggregate_index: 0,
            output: count_output,
        },
    ];
    input
}

#[test]
fn h3_grouping_uses_its_device_floor_and_resident_transform_cost() {
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_h3_group_min_rows = 100;
    limits.gpu_hash_agg_min_rows = 1;
    limits.gpu_op_cost_h3_parent_resident = 0.000_37;
    let plan = build_shape(
        h3_parent_input(99, 4),
        &TypedCostModel::from_limits(&limits),
    )
    .expect("H3 shape remains available for an honest device-floor decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::H3RowsBelowDeviceMinimum {
            estimated: crate::engine::cost::Rows::new(99),
            required: crate::engine::cost::Rows::new(100),
        }
    );
    let expected_aggregate = 99.0 * limits.gpu_op_cost_hash_agg
        + 99.0 * limits.gpu_op_cost_h3_parent_resident
        + crate::engine::cost::GPU_LAUNCH_OVERHEAD;
    assert!((plan.cost.aggregate.get() - expected_aggregate).abs() < 1.0e-12);
    let observed_per_row =
        (plan.cost.aggregate.get() - crate::engine::cost::GPU_LAUNCH_OVERHEAD) / 99.0;
    assert!(
        (observed_per_row - (limits.gpu_op_cost_hash_agg + limits.gpu_op_cost_h3_parent_resident))
            .abs()
            < 1.0e-12
    );
    assert!(
        observed_per_row >= limits.gpu_op_cost_hash_agg,
        "resident H3 parent cost cannot undercut the generic hash baseline",
    );
}

#[test]
fn h3_parent_cost_charges_each_transform_chunk_launch() {
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_h3_group_min_rows = 1;
    limits.gpu_hash_agg_min_rows = 1;
    limits.gpu_h3_max_chunk_rows = 100;
    let per_row_cost = |rows: f64| {
        rows * limits.gpu_op_cost_hash_agg + rows * limits.gpu_op_cost_h3_parent_resident
    };

    let at_boundary = build_shape(
        h3_parent_input(100, 4),
        &TypedCostModel::from_limits(&limits),
    )
    .expect("one full H3 transform chunk should build");
    let expected = per_row_cost(100.0) + crate::engine::cost::GPU_LAUNCH_OVERHEAD;
    assert!((at_boundary.cost.aggregate.get() - expected).abs() < 1.0e-12);

    let over_boundary = build_shape(
        h3_parent_input(101, 4),
        &TypedCostModel::from_limits(&limits),
    )
    .expect("a partial second H3 transform chunk should build");
    let expected = per_row_cost(101.0) + 2.0 * crate::engine::cost::GPU_LAUNCH_OVERHEAD;
    assert!((over_boundary.cost.aggregate.get() - expected).abs() < 1.0e-12);
}

#[test]
fn h3_parent_group_capacity_uses_exact_resolution_universe_bound() {
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_h3_group_min_rows = 1;
    limits.gpu_hash_agg_min_rows = 1;
    limits.gpu_hash_agg_max_groups = 122;

    let plan = build_shape(
        h3_parent_input(1_000, 0),
        &TypedCostModel::from_limits(&limits),
    )
    .expect("H3 shape should retain its device capacity decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::GroupsExceedDeviceMaximum {
            estimated: crate::engine::cost::Rows::new(123),
            maximum: crate::engine::cost::Rows::new(122),
        },
        "resolution zero has 122 H3 cells plus one NULL group",
    );

    limits.gpu_hash_agg_max_groups = 99;
    let plan = build_shape(
        h3_parent_input(100, 15),
        &TypedCostModel::from_limits(&limits),
    )
    .expect("H3 shape should cap its group universe by fact rows");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::GroupsExceedDeviceMaximum {
            estimated: crate::engine::cost::Rows::new(100),
            maximum: crate::engine::cost::Rows::new(99),
        },
        "the exact bound is min(N, 120 * 7^r + 3)",
    );
}

#[test]
fn h3_parent_output_cost_caps_postgres_group_estimate_to_exact_universe() {
    let mut input = h3_parent_input(1_000_000, 0);
    input.estimated_output_rows = 1_000_000;
    input.relations[0].residency = RelationResidency::Resident;
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_h3_group_min_rows = 1;
    limits.gpu_hash_agg_min_rows = 1;

    let plan = build_shape(input, &TypedCostModel::from_limits(&limits))
        .expect("resolution-zero H3 shape should build");
    let bounded_output_cost = 123.0 * limits.preagg_yield_cost;
    assert!(
        (plan.cost.output_materialization.get() - bounded_output_cost).abs() < 1.0e-12,
        "resolution zero has at most 122 H3 cells plus one NULL group",
    );

    // Reproduce the PG18 admission miss: the raw expression-group estimate
    // alone exceeded the required native-path margin before applying the bound.
    let required_cost = 22_879.0 * limits.gpu_agg_cost_ratio;
    let unbounded_total =
        plan.cost.total.get() - bounded_output_cost + 1_000_000.0 * limits.preagg_yield_cost;
    assert!(unbounded_total > required_cost);
    assert!(
        crate::engine::cost::GPU_LAUNCH_OVERHEAD + plan.cost.total.get() <= required_cost,
        "the exact H3 output bound restores the native-path cost margin",
    );
}

#[test]
fn fp64_cost_uses_typed_device_multiplier() {
    let input = single_table_input();
    let mut limits = DeviceLimits::cpu_only();
    limits.has_native_fp64 = false;
    limits.soft_fp64_cost_multiplier = 7.0;
    let soft = build_shape(input.clone(), &TypedCostModel::from_limits(&limits))
        .expect("soft-fp64 shape should build");
    limits.has_native_fp64 = true;
    let native = build_shape(input, &TypedCostModel::from_limits(&limits))
        .expect("native-fp64 shape should build");
    assert_eq!(soft.cost.aggregate.get(), native.cost.aggregate.get() * 7.0);
}

#[test]
fn projection_typing_participates_in_shape_digest() {
    let input = single_table_input();
    let baseline = build_shape(input.clone(), &model()).expect("baseline should build");
    let mut changed = input;
    let InputProjection::Aggregate { output, .. } = &mut changed.projections[0] else {
        panic!("test projection should be aggregate");
    };
    output.result_typmod = 42;
    let changed = build_shape(changed, &model()).expect("changed shape should build");
    assert_ne!(baseline.digest_words, changed.digest_words);
}

#[test]
fn shape_digest_is_exact_canonical_aqs_and_aop_encoding() {
    let plan = build_shape(single_table_input(), &model()).expect("shape should build");
    let mut canonical = plan.spec.encode_i32().expect("AQS should encode");
    canonical.extend(
        AggOutputProjection {
            slots: plan.projections.clone(),
        }
        .encode_i32(&plan.spec)
        .expect("AOP should encode"),
    );
    assert_eq!(plan.digest_words, canonical);
}

#[test]
fn each_modifier_has_a_precise_decline() {
    let cases = [
        (
            ShapeModifiers {
                has_window_functions: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::WindowFunctions,
        ),
        (
            ShapeModifiers {
                has_target_srfs: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::TargetSetReturningFunction,
        ),
        (
            ShapeModifiers {
                has_sublinks: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::Sublink,
        ),
        (
            ShapeModifiers {
                has_row_security: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::RowSecurity,
        ),
        (
            ShapeModifiers {
                has_distinct: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::Distinct,
        ),
        (
            ShapeModifiers {
                has_grouping_sets: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::GroupingSets,
        ),
        (
            ShapeModifiers {
                has_having: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::Having,
        ),
        (
            ShapeModifiers {
                has_set_operations: true,
                ..ShapeModifiers::default()
            },
            ShapeDecline::SetOperations,
        ),
    ];
    for (modifiers, expected) in cases {
        let mut input = single_table_input();
        input.modifiers = modifiers;
        assert_eq!(build_shape(input, &model()), Err(expected));
    }
}

#[test]
fn numeric_transition_aggregates_are_explicitly_out_of_scope() {
    for oid in [
        pg_sys::F_SUM_INT8,
        pg_sys::F_SUM_NUMERIC,
        pg_sys::F_AVG_INT2,
        pg_sys::F_AVG_INT4,
        pg_sys::F_AVG_INT8,
        pg_sys::F_AVG_NUMERIC,
        pg_sys::F_AVG_INTERVAL,
    ] {
        assert!(super::postgres::needs_numeric_accumulator(oid));
        assert_eq!(super::postgres::classify_aggregate(oid), None);
    }
    assert!(!super::postgres::needs_numeric_accumulator(
        pg_sys::F_AVG_FLOAT8
    ));
    assert_eq!(
        super::postgres::classify_aggregate(pg_sys::F_AVG_FLOAT8),
        Some(AggregateKind::Avg)
    );
}

#[test]
fn only_sample_float8_stddev_has_a_representable_aggregate_kind() {
    for oid in [pg_sys::F_STDDEV_FLOAT8, pg_sys::F_STDDEV_SAMP_FLOAT8] {
        assert_eq!(
            super::postgres::classify_aggregate(oid),
            Some(AggregateKind::StddevSamp)
        );
    }
    for oid in [
        pg_sys::F_STDDEV_POP_FLOAT8,
        pg_sys::F_VAR_SAMP_FLOAT8,
        pg_sys::F_VAR_POP_FLOAT8,
        pg_sys::F_STDDEV_FLOAT4,
        pg_sys::F_STDDEV_SAMP_INT4,
        pg_sys::F_STDDEV_NUMERIC,
    ] {
        assert_eq!(super::postgres::classify_aggregate(oid), None);
    }
}

#[test]
fn unchecked_float_reductions_never_enter_the_exact_result_path() {
    for kind in [
        AggregateKind::Sum,
        AggregateKind::Avg,
        AggregateKind::StddevSamp,
    ] {
        let mut input = single_table_input();
        input.aggregates[0].output.kind = kind;
        assert_eq!(
            build_shape(input, &model()),
            Err(ShapeDecline::FloatingAccumulatorSemantics)
        );
    }
}

#[test]
fn float_binary_expression_declines_before_aggregate_projection() {
    let mut input = single_table_input();
    let (count, count_output) = aggregate(
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs: ColumnRef {
                relation_oid: 100,
                attno: 2,
                type_oid: u32::from(pg_sys::FLOAT8OID),
            },
            rhs: ColumnRef {
                relation_oid: 100,
                attno: 3,
                type_oid: u32::from(pg_sys::FLOAT8OID),
            },
        },
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![count];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: count_output,
    }];
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::FloatingExpressionSemantics)
    );
}

#[test]
fn aggregate_filter_waits_for_the_phase9_filter_contract() {
    let mut input = single_table_input();
    input.aggregates[0].filter = FilterSpec::Ranges {
        input: ColumnRef {
            relation_oid: 100,
            attno: 1,
            type_oid: u32::from(pg_sys::INT4OID),
        },
        ranges: vec![ScalarRange {
            lo: ScalarValue::I32(1),
            hi: ScalarValue::I32(2),
        }],
    };
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::UnsupportedAggregateModifier)
    );
}

#[test]
fn unsupported_phase4_measure_types_decline_before_descriptor_build() {
    for type_oid in [
        u32::from(pg_sys::FLOAT4OID),
        u32::from(pg_sys::DATEOID),
        u32::from(pg_sys::TIMESTAMPOID),
    ] {
        let mut input = single_table_input();
        input.aggregates[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 100,
            attno: 2,
            type_oid,
        });
        assert_eq!(
            build_shape(input, &model()),
            Err(ShapeDecline::UnsupportedMeasureType { type_oid })
        );
    }
}

#[test]
fn structured_and_extension_measure_types_decline_in_generic_shape_analysis() {
    for type_oid in [
        114,  // json
        3802, // jsonb
        1007, // int4[]
        pg_sys::FirstNormalObjectId,
    ] {
        let mut input = single_table_input();
        input.aggregates[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 100,
            attno: 2,
            type_oid,
        });
        assert_eq!(
            build_shape(input, &model()),
            Err(ShapeDecline::UnsupportedMeasureType { type_oid })
        );
    }
}

#[test]
fn int4_binary_mul_and_sub_admit_all_descriptor_outputs() {
    for op in [BinaryMeasureOp::Mul, BinaryMeasureOp::Sub] {
        for kind in [
            AggregateKind::Sum,
            AggregateKind::Count,
            AggregateKind::Min,
            AggregateKind::Max,
        ] {
            let mut input = single_table_input();
            let result_type_oid = if matches!(kind, AggregateKind::Sum | AggregateKind::Count) {
                u32::from(pg_sys::INT8OID)
            } else {
                u32::from(pg_sys::INT4OID)
            };
            let (aggregate, projection) = aggregate(
                MeasureExpr::Binary {
                    op,
                    lhs: ColumnRef {
                        relation_oid: 100,
                        attno: 1,
                        type_oid: u32::from(pg_sys::INT4OID),
                    },
                    rhs: ColumnRef {
                        relation_oid: 100,
                        attno: 2,
                        type_oid: u32::from(pg_sys::INT4OID),
                    },
                },
                kind,
                result_type_oid,
            );
            input.aggregates = vec![aggregate];
            input.projections = vec![InputProjection::Aggregate {
                aggregate_index: 0,
                output: projection,
            }];

            build_shape(input, &model()).unwrap_or_else(|error| {
                panic!("INT4 {op:?} feeding {kind:?} should be admitted: {error:?}")
            });
        }
    }
}

#[test]
fn int8_binary_mul_remains_declined_for_per_row_overflow_semantics() {
    let mut input = single_table_input();
    let (aggregate, projection) = aggregate(
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Mul,
            lhs: ColumnRef {
                relation_oid: 100,
                attno: 2,
                type_oid: u32::from(pg_sys::INT8OID),
            },
            rhs: ColumnRef {
                relation_oid: 100,
                attno: 3,
                type_oid: u32::from(pg_sys::INT8OID),
            },
        },
        AggregateKind::Count,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![aggregate];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: projection,
    }];

    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::IntegerExpressionOverflowSemantics)
    );
}

#[test]
fn int8_binary_sub_admits_count_min_max_but_not_sum() {
    for kind in [AggregateKind::Count, AggregateKind::Min, AggregateKind::Max] {
        let mut input = single_table_input();
        let (aggregate, projection) = aggregate(
            MeasureExpr::Binary {
                op: BinaryMeasureOp::Sub,
                lhs: ColumnRef {
                    relation_oid: 100,
                    attno: 2,
                    type_oid: u32::from(pg_sys::INT8OID),
                },
                rhs: ColumnRef {
                    relation_oid: 100,
                    attno: 3,
                    type_oid: u32::from(pg_sys::INT8OID),
                },
            },
            kind,
            u32::from(pg_sys::INT8OID),
        );
        input.aggregates = vec![aggregate];
        input.projections = vec![InputProjection::Aggregate {
            aggregate_index: 0,
            output: projection,
        }];

        build_shape(input, &model()).unwrap_or_else(|error| {
            panic!("INT8 SUB feeding {kind:?} should be admitted: {error:?}")
        });
    }

    let mut input = single_table_input();
    let (sum, projection) = aggregate(
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Sub,
            lhs: ColumnRef {
                relation_oid: 100,
                attno: 2,
                type_oid: u32::from(pg_sys::INT8OID),
            },
            rhs: ColumnRef {
                relation_oid: 100,
                attno: 3,
                type_oid: u32::from(pg_sys::INT8OID),
            },
        },
        AggregateKind::Sum,
        u32::from(pg_sys::INT8OID),
    );
    input.aggregates = vec![sum];
    input.projections = vec![InputProjection::Aggregate {
        aggregate_index: 0,
        output: projection,
    }];
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::NumericAccumulatorTypeUnavailable {
            type_oid: u32::from(pg_sys::INT8OID),
        })
    );
}

#[test]
fn unsupported_join_key_width_declines_before_star_orientation() {
    let mut input = single_table_input();
    input.relations.push(relation(2, 200, 1_000));
    input.relations[1].unique_attnos.insert(1);
    input.joins.push(EquiJoin {
        left: column(1, 100, 3, u32::from(pg_sys::INT8OID)),
        right: column(2, 200, 1, u32::from(pg_sys::INT8OID)),
    });
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::UnsupportedJoinKeyType {
            type_oid: u32::from(pg_sys::INT8OID),
        })
    );
}

#[test]
fn nondeterministic_text_group_key_declines_before_dictionary_coding() {
    let mut input = single_table_input();
    let mut group = column(1, 100, 1, u32::from(pg_sys::TEXTOID));
    group.collation_oid = 9_999;
    group.collation_is_deterministic = false;
    input.group_keys.push(PlannerGroupKey::Column(group));
    input.projections.insert(
        0,
        InputProjection::Group {
            key: PlannerGroupKey::Column(group),
            output: OutputMetadata {
                source_type_oid: u32::from(pg_sys::TEXTOID),
                result_type_oid: u32::from(pg_sys::TEXTOID),
                result_typmod: -1,
                result_collation_oid: 9_999,
                nullable: true,
            },
        },
    );
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::NondeterministicKeyCollation {
            collation_oid: 9_999,
        })
    );
}

#[test]
fn deterministic_text_group_key_remains_dictionary_resolvable() {
    let mut input = single_table_input();
    let mut group = column(1, 100, 1, u32::from(pg_sys::TEXTOID));
    group.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
    input.group_keys.push(PlannerGroupKey::Column(group));
    input.projections.insert(
        0,
        InputProjection::Group {
            key: PlannerGroupKey::Column(group),
            output: OutputMetadata {
                source_type_oid: u32::from(pg_sys::TEXTOID),
                result_type_oid: u32::from(pg_sys::TEXTOID),
                result_typmod: -1,
                result_collation_oid: u32::from(pg_sys::C_COLLATION_OID),
                nullable: true,
            },
        },
    );
    let plan = build_shape(input, &model()).expect("deterministic text can be dictionary-coded");
    let DescriptorResolution::BeginTimeArtifacts {
        dictionary_keys, ..
    } = &plan.descriptor_resolution
    else {
        panic!("grouped text shape must require a dictionary");
    };
    assert_eq!(
        dictionary_keys[0].collation_oid,
        u32::from(pg_sys::C_COLLATION_OID)
    );
    assert_eq!(plan.spec.group_keys[0].type_oid, u32::from(pg_sys::TEXTOID));
    assert_eq!(
        plan.spec.group_keys[0].collation_oid,
        u32::from(pg_sys::C_COLLATION_OID)
    );
}

#[test]
fn group_keys_require_a_resident_supported_logical_type() {
    let mut input = single_table_input();
    let group = column(1, 100, 1, u32::from(pg_sys::NUMERICOID));
    input.group_keys.push(PlannerGroupKey::Column(group));
    input.projections.insert(
        0,
        InputProjection::Group {
            key: PlannerGroupKey::Column(group),
            output: output(u32::from(pg_sys::NUMERICOID), true),
        },
    );
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::UnsupportedGroupKeyType {
            type_oid: u32::from(pg_sys::NUMERICOID),
        })
    );
}

#[test]
fn deterministic_text_join_requires_begin_time_shared_dictionary() {
    let mut input = single_table_input();
    input.relations.push(relation(2, 200, 1_000));
    input.relations[1].unique_attnos.insert(1);
    let mut left = column(1, 100, 1, u32::from(pg_sys::TEXTOID));
    let mut right = column(2, 200, 1, u32::from(pg_sys::TEXTOID));
    left.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
    right.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
    input.joins.push(EquiJoin { left, right });

    let plan = build_shape(input, &model()).expect("deterministic text equijoin should normalize");
    assert_eq!(
        plan.spec.star_dims[0].collation_oid,
        u32::from(pg_sys::C_COLLATION_OID)
    );
    let DescriptorResolution::BeginTimeArtifacts {
        dictionary_keys,
        joins,
        ..
    } = &plan.descriptor_resolution
    else {
        panic!("text join must require a shared dictionary artifact");
    };
    assert!(dictionary_keys.is_empty());
    assert_eq!(joins.len(), 1);
    assert_eq!(joins[0].fact_key, left.column);
    assert_eq!(joins[0].dim_key, right.column);
    assert_eq!(
        plan.descriptor_spec(),
        Err(ShapeDecline::DescriptorArtifactsRequireResolution)
    );
}

#[test]
fn varchar_and_bpchar_joins_use_the_same_dictionary_correlation_contract() {
    for type_oid in [u32::from(pg_sys::VARCHAROID), u32::from(pg_sys::BPCHAROID)] {
        let mut input = single_table_input();
        input.relations.push(relation(2, 200, 1_000));
        let mut left = column(1, 100, 1, type_oid);
        let mut right = column(2, 200, 1, type_oid);
        left.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
        right.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
        input.joins.push(EquiJoin { left, right });

        let plan = build_shape(input, &model()).expect("collatable join should normalize");
        assert!(matches!(
            &plan.descriptor_resolution,
            DescriptorResolution::BeginTimeArtifacts { joins, .. } if joins.len() == 1
        ));
    }
}

#[test]
fn text_join_collations_must_match_bit_exactly() {
    let mut input = single_table_input();
    input.relations.push(relation(2, 200, 1_000));
    let mut left = column(1, 100, 1, u32::from(pg_sys::TEXTOID));
    let mut right = column(2, 200, 1, u32::from(pg_sys::TEXTOID));
    left.collation_oid = u32::from(pg_sys::C_COLLATION_OID);
    right.collation_oid = 9_998;
    input.joins.push(EquiJoin { left, right });
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::JoinKeyCollationMismatch {
            left_collation_oid: u32::from(pg_sys::C_COLLATION_OID),
            right_collation_oid: 9_998,
        })
    );
}

#[test]
fn nondeterministic_text_join_declines_before_key_width_check() {
    let mut input = single_table_input();
    input.relations.push(relation(2, 200, 1_000));
    let mut left = column(1, 100, 1, u32::from(pg_sys::TEXTOID));
    let mut right = column(2, 200, 1, u32::from(pg_sys::TEXTOID));
    left.collation_oid = 9_999;
    right.collation_oid = 9_999;
    left.collation_is_deterministic = false;
    right.collation_is_deterministic = false;
    input.joins.push(EquiJoin { left, right });
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::NondeterministicKeyCollation {
            collation_oid: 9_999,
        })
    );
}

#[test]
fn only_immediate_ordinary_single_column_index_proves_dimension_uniqueness() {
    use super::postgres::{
        IndexUniquenessFacts, index_proves_immediate_single_key_uniqueness as proves,
    };

    let valid = IndexUniquenessFacts {
        unique: true,
        immediate: true,
        hypothetical: false,
        nkeycolumns: 1,
        key_attno: 7,
        has_predicate: false,
        ordinary_opfamily: true,
    };
    assert!(proves(valid, 7));
    assert!(!proves(
        IndexUniquenessFacts {
            unique: false,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            immediate: false,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            hypothetical: true,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            nkeycolumns: 2,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            key_attno: 8,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            has_predicate: true,
            ..valid
        },
        7
    ));
    assert!(!proves(
        IndexUniquenessFacts {
            ordinary_opfamily: false,
            ..valid
        },
        7
    ));
}

#[test]
fn only_default_hashable_equality_is_normalized() {
    use super::postgres::is_ordinary_hash_equality;

    let ordinary = pg_sys::Oid::from(96_u32);
    assert!(is_ordinary_hash_equality(ordinary, ordinary, true));
    assert!(!is_ordinary_hash_equality(
        pg_sys::Oid::from(97_u32),
        ordinary,
        true
    ));
    assert!(!is_ordinary_hash_equality(ordinary, ordinary, false));
    assert!(!is_ordinary_hash_equality(
        pg_sys::InvalidOid,
        pg_sys::InvalidOid,
        true
    ));
}

#[test]
fn binary_measure_uses_frozen_neutral_expression() {
    let mut input = single_table_input();
    let lhs = column(1, 100, 2, u32::from(pg_sys::INT8OID));
    let rhs = column(1, 100, 3, u32::from(pg_sys::INT8OID));
    input.aggregates[0].expression = MeasureExpr::Binary {
        op: BinaryMeasureOp::Sub,
        lhs: lhs.column,
        rhs: rhs.column,
    };
    let InputProjection::Aggregate { output, .. } = &mut input.projections[0] else {
        panic!("fixture projection is aggregate");
    };
    output.source_type_oid = u32::from(pg_sys::INT8OID);
    output.result_type_oid = u32::from(pg_sys::INT8OID);
    let plan = build_shape(input, &model()).expect("binary measure should build");
    assert!(matches!(
        plan.spec.measures[0].expression,
        MeasureExpr::Binary {
            op: BinaryMeasureOp::Sub,
            ..
        }
    ));
    assert_eq!(plan.required_relations[0].attnos, vec![2, 3]);
}

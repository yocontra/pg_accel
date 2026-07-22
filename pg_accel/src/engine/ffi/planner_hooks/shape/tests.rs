use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use pgrx::pg_sys;

use crate::engine::cost::{DeviceLimits, TypedCostModel};
use crate::engine::spec::{
    AggOutputProjection, AggregateKind, AggregateOutput, AggregateSource, BinaryMeasureOp,
    ColumnRef, FilterSpec, GroupKeySource, JoinMultiplicity, MeasureExpr, ScalarRange, ScalarValue,
    SpatialOperand, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
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
        type_modifier: -1,
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
    assert_eq!(plan.estimated_output_rows, 1);
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
fn generic_group_estimate_above_device_limit_declines_before_resolution() {
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
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_hash_agg_min_rows = 1;
    input.estimated_output_rows =
        u64::try_from(limits.gpu_hash_agg_max_groups).expect("group limit fits u64") + 1;

    let plan = build_shape(input, &TypedCostModel::from_limits(&limits))
        .expect("shape should retain an honest group-cap decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::GroupsExceedDeviceMaximum {
            estimated: crate::engine::cost::Rows::new(limits.gpu_hash_agg_max_groups + 1),
            maximum: crate::engine::cost::Rows::new(limits.gpu_hash_agg_max_groups),
        }
    );
    assert_eq!(
        plan.estimated_output_rows,
        u64::try_from(limits.gpu_hash_agg_max_groups).expect("group limit fits u64") + 1
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

fn point_operand() -> SpatialOperand {
    SpatialOperand::Column {
        column: ColumnRef {
            relation_oid: 100,
            attno: 3,
            type_oid: 60_001,
        },
        metadata: SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: (4_326 << 8) | (crate::engine::residency::RESIDENT_GEOMETRY_POINT as i32) << 2,
            srid: Some(4_326),
        },
    }
}

fn polygon_bytes(points: &[(f64, f64)]) -> Box<[u8]> {
    let mut bytes = vec![0, 0, 0, 0, 0, 0x10, 0xe6, 0];
    bytes.extend_from_slice(&crate::engine::residency::RESIDENT_GEOMETRY_POLYGON.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(points.len())
            .expect("test polygon count fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for (x, y) in points {
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
    }
    bytes.into_boxed_slice()
}

fn polygon_operand(coordinate_pairs: usize) -> SpatialOperand {
    assert!(coordinate_pairs >= 4);
    let unique = coordinate_pairs - 1;
    let unique_f64 = f64::from(u32::try_from(unique).expect("test vertex count fits u32"));
    let mut points = (0..unique)
        .map(|index| {
            let index = f64::from(u32::try_from(index).expect("test vertex index fits u32"));
            let angle = std::f64::consts::TAU * index / unique_f64;
            (angle.cos(), angle.sin())
        })
        .collect::<Vec<_>>();
    points.push(points[0]);
    SpatialOperand::Constant {
        metadata: SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: -1,
            srid: Some(4_326),
        },
        bytes: polygon_bytes(&points),
    }
}

fn test_spatial_filter(
    predicate: SpatialPredicateKind,
    polygon_left: bool,
    coordinate_pairs: usize,
) -> FilterSpec {
    let point = point_operand();
    let polygon = polygon_operand(coordinate_pairs);
    let (left, right) = if polygon_left {
        (polygon, point)
    } else {
        (point, polygon)
    };
    FilterSpec::Spatial {
        predicate,
        left,
        right,
        distance: (predicate == SpatialPredicateKind::DWithin).then_some(ScalarValue::F64(1.0)),
    }
}

fn add_test_spatial_filter(input: &mut ShapeInput, coordinate_pairs: usize) {
    input.relation_filters.push((
        100,
        test_spatial_filter(SpatialPredicateKind::Intersects, false, coordinate_pairs),
    ));
    input.relations[0].column_widths.insert(3, 64);
}

#[test]
fn spatial_fact_filter_requires_postgis_mask_and_exact_point_polygon_work() {
    let mut input = single_table_input();
    add_test_spatial_filter(&mut input, 4);
    let plan = build_shape(input, &model()).expect("proved spatial fact filter should build");
    assert!(matches!(plan.spec.fact_filter, FilterSpec::Spatial { .. }));
    assert!(plan.descriptor_measures.derived_spatial_mask);
    assert_eq!(plan.descriptor_measures.derived_fact_mask, None);
    assert_eq!(plan.required_relations[0].attnos, vec![2, 3]);
}

#[test]
fn column_by_column_spatial_work_declines_without_vertex_statistics() {
    let metadata = match point_operand() {
        SpatialOperand::Column { metadata, .. } => metadata,
        SpatialOperand::Constant { .. } => unreachable!(),
    };
    let mut input = single_table_input();
    input.relation_filters.push((
        100,
        FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Intersects,
            left: point_operand(),
            right: SpatialOperand::Column {
                column: ColumnRef {
                    relation_oid: 100,
                    attno: 5,
                    type_oid: 60_001,
                },
                metadata,
            },
            distance: None,
        },
    ));
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::SpatialWorkShapeUnproved),
    );
}

#[test]
fn spatial_orientation_is_part_of_the_admission_proof() {
    for (predicate, polygon_left) in [
        (SpatialPredicateKind::Intersects, false),
        (SpatialPredicateKind::Intersects, true),
        (SpatialPredicateKind::DWithin, false),
        (SpatialPredicateKind::DWithin, true),
        (SpatialPredicateKind::Contains, true),
        (SpatialPredicateKind::Within, false),
    ] {
        let mut input = single_table_input();
        input
            .relation_filters
            .push((100, test_spatial_filter(predicate, polygon_left, 4)));
        input.relations[0].column_widths.insert(3, 64);
        assert!(
            build_shape(input, &model()).is_ok(),
            "{predicate:?} orientation"
        );
    }
    for (predicate, polygon_left) in [
        (SpatialPredicateKind::Contains, false),
        (SpatialPredicateKind::Within, true),
    ] {
        let mut input = single_table_input();
        input
            .relation_filters
            .push((100, test_spatial_filter(predicate, polygon_left, 4)));
        assert_eq!(
            build_shape(input, &model()),
            Err(ShapeDecline::SpatialWorkShapeUnproved),
        );
    }
}

#[test]
fn postgis_typmod_adapter_requires_known_2d_supported_shape_and_srid() {
    let point_4326 = (4_326 << 8) | (crate::engine::residency::RESIDENT_GEOMETRY_POINT as i32) << 2;
    assert_eq!(
        super::postgres::postgis_geometry_typmod_metadata(point_4326),
        Some(SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: point_4326,
            srid: Some(4_326),
        })
    );
    assert!(super::postgres::postgis_geometry_typmod_metadata(-1).is_none());
    assert!(super::postgres::postgis_geometry_typmod_metadata(4_326 << 8).is_none());
    assert!(super::postgres::postgis_geometry_typmod_metadata(point_4326 | 1).is_none());
    let unsupported = (4_326 << 8) | 6 << 2;
    assert!(super::postgres::postgis_geometry_typmod_metadata(unsupported).is_none());
}

#[test]
fn ordinary_funcexpr_declines_before_postgis_catalog_resolution() {
    assert_eq!(
        super::postgres::require_postgis_function_member(false),
        Err(ShapeDecline::UnsupportedPredicate),
    );
}

#[test]
fn same_name_impostor_declines_without_postgis_catalog_resolution() {
    assert_eq!(
        super::postgres::require_postgis_function_member(false),
        Err(ShapeDecline::UnsupportedPredicate),
    );
    assert_eq!(
        super::postgres::require_postgis_function_member(true),
        Ok(())
    );
}

#[test]
fn self_intersecting_bowtie_constant_declines_after_postgis_validity_check() {
    let bowtie = polygon_bytes(&[(0.0, 0.0), (2.0, 2.0), (0.0, 2.0), (2.0, 0.0), (0.0, 0.0)]);
    let parsed = crate::engine::residency::validate_resident_geometry_value(
        &bowtie,
        crate::engine::spec::MAX_SPATIAL_CONSTANT_BYTES,
    )
    .expect("bowtie is structurally valid GSERIALIZED");
    assert_eq!(parsed.coordinate_pairs, 5);
    assert_eq!(
        super::postgres::require_valid_spatial_constant(false),
        Err(ShapeDecline::InvalidSpatialConstant),
    );
}

#[test]
fn spatial_filter_outside_fact_filter_declines_explicitly() {
    let mut input = single_table_input();
    input.aggregates[0].filter = test_spatial_filter(SpatialPredicateKind::Intersects, false, 4);
    assert_eq!(
        build_shape(input, &model()),
        Err(ShapeDecline::SpatialFilterOutsideFactRelation),
    );
}

#[test]
fn spatial_cost_charges_pairwise_chunks_and_reserves_exact_rechecks() {
    let mut input = single_table_input();
    add_test_spatial_filter(&mut input, 4);
    let mut limits = DeviceLimits::cpu_only();
    limits.has_native_fp64 = true;
    limits.gpu_reduce_min_rows = 1;
    limits.gpu_spatial_pairwise_chunk_rows = 100_000;
    limits.gpu_spatial_max_recheck_fraction = 0.10;
    limits.gpu_spatial_min_vertices = 4;
    limits.gpu_spatial_max_vertices_per_row = 100;
    limits.spatial_point_in_ring_break_even_verts_x_rows = 1;
    limits.spatial_point_in_ring_max_verts_x_rows = 10_000_000;
    let plan = build_shape(input, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should retain its explicit costs");
    let expected_filter = 1_000_000.0_f64.mul_add(
        limits.gpu_op_cost_filter,
        10.0 * crate::engine::cost::GPU_LAUNCH_OVERHEAD,
    );
    let expected_recheck = 100_000.0 * limits.cpu_spatial_recheck_per_row;
    assert!((plan.cost.spatial_filter.get() - expected_filter).abs() < 1.0e-12);
    assert!((plan.cost.spatial_recheck_reserve.get() - expected_recheck).abs() < 1.0e-12);
    assert!(plan.cost.spatial_recheck_reserve.get() > plan.cost.spatial_filter.get());
    assert_eq!(plan.cost_gate, ShapeCostGate::Eligible);
}

#[test]
fn spatial_vertex_cost_never_undercuts_the_generic_filter_baseline() {
    let mut limits = DeviceLimits::cpu_only();
    limits.has_native_fp64 = true;
    limits.gpu_reduce_min_rows = 1;
    limits.gpu_spatial_min_vertices = 4;
    limits.gpu_spatial_max_vertices_per_row = 100;
    limits.gpu_spatial_pairwise_chunk_rows = 1_000_000;
    limits.spatial_point_in_ring_break_even_verts_x_rows = 1;
    limits.spatial_point_in_ring_max_verts_x_rows = 10_000_000;

    let plan_for = |coordinate_pairs| {
        let mut input = single_table_input();
        add_test_spatial_filter(&mut input, coordinate_pairs);
        build_shape(input, &TypedCostModel::from_limits(&limits)).expect("spatial shape builds")
    };
    let baseline = plan_for(4).cost.spatial_filter.get();
    let twice_the_vertices = plan_for(8).cost.spatial_filter.get();
    let generic_floor = 1_000_000.0_f64.mul_add(
        limits.gpu_op_cost_filter,
        crate::engine::cost::GPU_LAUNCH_OVERHEAD,
    );
    assert!(baseline >= generic_floor);
    let expected_twice = (2.0_f64 * 1_000_000.0).mul_add(
        limits.gpu_op_cost_filter,
        crate::engine::cost::GPU_LAUNCH_OVERHEAD,
    );
    assert!((twice_the_vertices - expected_twice).abs() < 1.0e-12);
    assert!(twice_the_vertices > baseline);
}

#[test]
fn spatial_cost_uses_distinct_row_vertex_and_work_gates() {
    let mut limits = DeviceLimits::cpu_only();
    limits.gpu_reduce_min_rows = 100;
    limits.gpu_spatial_min_vertices = 4;
    limits.gpu_spatial_max_vertices_per_row = 100;
    limits.spatial_point_in_ring_break_even_verts_x_rows = 1;
    limits.spatial_point_in_ring_max_verts_x_rows = 10_000;

    let mut below_floor = single_table_input();
    below_floor.relations[0].estimated_rows = 99;
    add_test_spatial_filter(&mut below_floor, 4);
    let plan = build_shape(below_floor, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should preserve a typed row-floor decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::SpatialRowsBelowDeviceMinimum {
            estimated: crate::engine::cost::Rows::new(99),
            required: crate::engine::cost::Rows::new(100),
        },
    );

    limits.gpu_reduce_min_rows = 1;
    limits.gpu_spatial_min_vertices = 5;
    let mut too_few_vertices = single_table_input();
    too_few_vertices.relations[0].estimated_rows = 100;
    add_test_spatial_filter(&mut too_few_vertices, 4);
    let plan = build_shape(too_few_vertices, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should preserve a typed vertex-floor decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::SpatialVerticesBelowDeviceMinimum {
            estimated: crate::engine::cost::Rows::new(4),
            required: crate::engine::cost::Rows::new(5),
        },
    );

    limits.gpu_spatial_min_vertices = 4;
    limits.gpu_spatial_max_vertices_per_row = 5;
    let mut too_many_vertices = single_table_input();
    too_many_vertices.relations[0].estimated_rows = 100;
    add_test_spatial_filter(&mut too_many_vertices, 6);
    let plan = build_shape(too_many_vertices, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should preserve a typed vertex-maximum decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::SpatialVerticesExceedDeviceMaximum {
            estimated: crate::engine::cost::Rows::new(6),
            maximum: crate::engine::cost::Rows::new(5),
        },
    );

    limits.gpu_spatial_max_vertices_per_row = 100;
    limits.spatial_point_in_ring_break_even_verts_x_rows = 401;
    let mut below_work = single_table_input();
    below_work.relations[0].estimated_rows = 100;
    add_test_spatial_filter(&mut below_work, 4);
    let plan = build_shape(below_work, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should preserve a typed work-floor decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::SpatialWorkBelowDeviceMinimum {
            estimated: crate::engine::cost::WorkProduct::new(400),
            required: crate::engine::cost::WorkProduct::new(401),
        },
    );

    limits.spatial_point_in_ring_break_even_verts_x_rows = 1;
    limits.spatial_point_in_ring_max_verts_x_rows = 399;
    let mut above_work = single_table_input();
    above_work.relations[0].estimated_rows = 100;
    add_test_spatial_filter(&mut above_work, 4);
    let plan = build_shape(above_work, &TypedCostModel::from_limits(&limits))
        .expect("spatial shape should preserve a typed maximum-work decline");
    assert_eq!(
        plan.cost_gate,
        ShapeCostGate::SpatialWorkExceedsDeviceMaximum {
            estimated: crate::engine::cost::WorkProduct::new(400),
            maximum: crate::engine::cost::WorkProduct::new(399),
        },
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
    let missing_bytes = plan
        .residency
        .missing_bytes
        .expect("fixture has catalog-width byte evidence");
    let expected_row_cost = 1_000_000_f64 * limits.preagg_dim_materialize_cost;
    let expected_byte_cost = missing_bytes as f64 * limits.resident_load_per_byte_cost;
    let expected = (expected_row_cost + expected_byte_cost) / 4.0;
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
    let expected_aggregate = 99.0_f64.mul_add(
        limits.gpu_op_cost_hash_agg,
        99.0 * limits.gpu_op_cost_h3_parent_resident,
    ) + crate::engine::cost::GPU_LAUNCH_OVERHEAD;
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
        rows.mul_add(
            limits.gpu_op_cost_hash_agg,
            rows * limits.gpu_op_cost_h3_parent_resident,
        )
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
    let expected = 2.0_f64.mul_add(
        crate::engine::cost::GPU_LAUNCH_OVERHEAD,
        per_row_cost(101.0),
    );
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
    assert_eq!(plan.estimated_output_rows, 123);
    let bounded_output_cost = 123.0 * limits.preagg_yield_cost;
    assert!(
        (plan.cost.output_materialization.get() - bounded_output_cost).abs() < 1.0e-12,
        "resolution zero has at most 122 H3 cells plus one NULL group",
    );

    // Reproduce the PG18 admission miss: the raw expression-group estimate
    // alone exceeded the required native-path margin before applying the bound.
    let required_cost = 22_879.0 * limits.gpu_agg_cost_ratio;
    let unbounded_total = 1_000_000.0_f64.mul_add(
        limits.preagg_yield_cost,
        plan.cost.total.get() - bounded_output_cost,
    );
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
fn qualifier_conjunct_bound_accepts_nonempty_ordered_lists() {
    use super::postgres::bounded_qual_conjunct_indices;

    assert_eq!(
        bounded_qual_conjunct_indices(1)
            .expect("one conjunct")
            .collect::<Vec<_>>(),
        [0]
    );
    assert_eq!(
        bounded_qual_conjunct_indices(3)
            .expect("multiple conjuncts")
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(bounded_qual_conjunct_indices(64).is_ok());
}

#[test]
fn qualifier_conjunct_bound_rejects_empty_and_oversized_lists() {
    use super::postgres::bounded_qual_conjunct_indices;

    assert_eq!(
        bounded_qual_conjunct_indices(0),
        Err(ShapeDecline::UnsupportedPredicate)
    );
    assert_eq!(
        bounded_qual_conjunct_indices(65),
        Err(ShapeDecline::UnsupportedPredicate)
    );
}

#[test]
fn every_shape_decline_has_a_stable_machine_code_and_display_prefix() {
    let cases = vec![
        (ShapeDecline::NotSelect, "shape_not_select"),
        (ShapeDecline::NoAggregate, "shape_no_aggregate"),
        (ShapeDecline::WindowFunctions, "shape_window_functions"),
        (ShapeDecline::TargetSetReturningFunction, "shape_target_srf"),
        (ShapeDecline::Sublink, "shape_sublink"),
        (ShapeDecline::RecursiveQuery, "shape_recursive_query"),
        (ShapeDecline::ModifyingCte, "shape_modifying_cte"),
        (ShapeDecline::RowSecurity, "shape_row_security"),
        (ShapeDecline::Distinct, "shape_distinct"),
        (ShapeDecline::GroupingSets, "shape_grouping_sets"),
        (ShapeDecline::GroupDistinct, "shape_group_distinct"),
        (ShapeDecline::Having, "shape_having"),
        (ShapeDecline::SetOperations, "shape_set_operations"),
        (ShapeDecline::RowMarks, "shape_row_marks"),
        (
            ShapeDecline::UnsupportedRangeTableEntry { varno: 2 },
            "shape_unsupported_rte",
        ),
        (ShapeDecline::TableSample, "shape_table_sample"),
        (ShapeDecline::UnsupportedOuterJoin, "shape_outer_join"),
        (
            ShapeDecline::UnsupportedPredicate,
            "shape_unsupported_predicate",
        ),
        (
            ShapeDecline::SpatialFilterOutsideFactRelation,
            "shape_spatial_filter_outside_fact",
        ),
        (
            ShapeDecline::SpatialWorkShapeUnproved,
            "shape_spatial_work_shape_unproved",
        ),
        (
            ShapeDecline::InvalidSpatialConstant,
            "shape_invalid_spatial_constant",
        ),
        (
            ShapeDecline::PostgisCatalog("catalog".to_owned()),
            "shape_postgis_catalog",
        ),
        (
            ShapeDecline::UnsupportedFilterType { type_oid: 1 },
            "shape_unsupported_filter_type",
        ),
        (
            ShapeDecline::UnsupportedAggregate { aggregate_oid: 2 },
            "shape_unsupported_aggregate",
        ),
        (
            ShapeDecline::NumericAccumulatorUnavailable { aggregate_oid: 3 },
            "shape_numeric_accumulator_unavailable",
        ),
        (
            ShapeDecline::NumericAccumulatorTypeUnavailable { type_oid: 4 },
            "shape_numeric_accumulator_type_unavailable",
        ),
        (
            ShapeDecline::UnsupportedMeasureType { type_oid: 5 },
            "shape_unsupported_measure_type",
        ),
        (
            ShapeDecline::UnsupportedJoinKeyType { type_oid: 6 },
            "shape_unsupported_join_key_type",
        ),
        (
            ShapeDecline::NondeterministicKeyCollation { collation_oid: 7 },
            "shape_nondeterministic_key_collation",
        ),
        (
            ShapeDecline::InvalidKeyCollation {
                type_oid: 8,
                collation_oid: 9,
            },
            "shape_invalid_key_collation",
        ),
        (
            ShapeDecline::UnsupportedGroupKeyType { type_oid: 10 },
            "shape_unsupported_group_key_type",
        ),
        (
            ShapeDecline::IntegerExpressionOverflowSemantics,
            "shape_integer_expression_overflow_semantics",
        ),
        (
            ShapeDecline::FloatingExpressionSemantics,
            "shape_floating_expression_semantics",
        ),
        (
            ShapeDecline::FloatingAccumulatorSemantics,
            "shape_floating_accumulator_semantics",
        ),
        (
            ShapeDecline::UnsupportedAggregateInput {
                kind: AggregateKind::Sum,
                type_oid: 11,
            },
            "shape_unsupported_aggregate_input",
        ),
        (
            ShapeDecline::ProjectionSourceTypeMismatch {
                expected_type_oid: 12,
                actual_type_oid: 13,
            },
            "shape_projection_source_type",
        ),
        (
            ShapeDecline::AggregateResultTypeMismatch {
                source_type_oid: 14,
                kind: AggregateKind::Min,
                result_type_oid: 15,
            },
            "shape_aggregate_result_type",
        ),
        (
            ShapeDecline::UnsupportedBinaryMeasure,
            "shape_unsupported_binary_measure",
        ),
        (
            ShapeDecline::UnsupportedAggregateModifier,
            "shape_aggregate_modifier",
        ),
        (
            ShapeDecline::UnsupportedMeasureExpression,
            "shape_measure_expression",
        ),
        (ShapeDecline::UnsupportedProjection, "shape_projection"),
        (
            ShapeDecline::UnprojectedGroupKey {
                relation_oid: 16,
                attno: 1,
            },
            "shape_unprojected_group_key",
        ),
        (
            ShapeDecline::UnsupportedGroupExpression,
            "shape_group_expression",
        ),
        (
            ShapeDecline::UnsupportedColumn {
                relation_oid: 17,
                attno: 2,
            },
            "shape_unsupported_column",
        ),
        (
            ShapeDecline::TooManyRelations {
                actual: 3,
                maximum: 2,
            },
            "shape_too_many_relations",
        ),
        (
            ShapeDecline::TooManyGroupKeys {
                actual: 3,
                maximum: 2,
            },
            "shape_too_many_group_keys",
        ),
        (
            ShapeDecline::TooManyDimensions {
                actual: 3,
                maximum: 2,
            },
            "shape_too_many_dimensions",
        ),
        (
            ShapeDecline::TooManyMeasures {
                actual: 3,
                maximum: 2,
            },
            "shape_too_many_measures",
        ),
        (
            ShapeDecline::SelfJoinUsesAmbiguousRelationOid { relation_oid: 18 },
            "shape_self_join",
        ),
        (
            ShapeDecline::DuplicatePlannerRelation { varno: 3 },
            "shape_duplicate_varno",
        ),
        (
            ShapeDecline::JoinKeyTypeMismatch {
                left_type_oid: 19,
                right_type_oid: 20,
            },
            "shape_join_key_type_mismatch",
        ),
        (
            ShapeDecline::JoinKeyCollationMismatch {
                left_collation_oid: 21,
                right_collation_oid: 22,
            },
            "shape_join_key_collation_mismatch",
        ),
        (ShapeDecline::NonEqualityJoin, "shape_non_equality_join"),
        (
            ShapeDecline::CompositeJoinKeyUnsupported,
            "shape_composite_join_key",
        ),
        (
            ShapeDecline::DisconnectedJoinGraph,
            "shape_disconnected_join_graph",
        ),
        (ShapeDecline::NonStarJoinGraph, "shape_non_star_join_graph"),
        (
            ShapeDecline::AmbiguousFactRelation,
            "shape_ambiguous_fact_relation",
        ),
        (
            ShapeDecline::GroupedByNonUniqueDimension {
                relation_oid: 23,
                attno: 4,
            },
            "shape_nonunique_dimension_group",
        ),
        (
            ShapeDecline::MultipleFiltersPerRelation { relation_oid: 24 },
            "shape_multi_filter_relation",
        ),
        (
            ShapeDecline::InvalidFilterRange,
            "shape_invalid_filter_range",
        ),
        (
            ShapeDecline::InvalidProjectionReference {
                aggregate_index: 25,
            },
            "shape_projection_reference",
        ),
        (
            ShapeDecline::DescriptorArtifactsRequireResolution,
            "shape_descriptor_artifacts_require_resolution",
        ),
        (
            ShapeDecline::InvalidGroupKeyResolution,
            "shape_invalid_group_key_resolution",
        ),
        (
            ShapeDecline::InvalidSpec("spec".to_owned()),
            "shape_invalid_spec",
        ),
        (ShapeDecline::Codec("codec".to_owned()), "shape_codec"),
    ];

    for (decline, expected_code) in cases {
        assert_eq!(decline.code(), expected_code);
        assert!(decline.to_string().starts_with(expected_code));
    }
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

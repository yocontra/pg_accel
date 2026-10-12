//! Live SPG1 lowering and owned GPU-binding tests, not planner-admission tests.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use std::ffi::CString;

    use pgrx::pg_sys;
    use pgrx::prelude::*;

    use crate::engine::ffi::planner_hooks::shape::{
        LoweredScalarExpression, lower_scalar_expression,
    };
    use crate::engine::spec::abi;
    use crate::engine::spec::scalar::tests::{Value, evaluate};
    use crate::engine::spec::scalar::{ScalarInstruction as I, ScalarType};
    use crate::gpu::{
        ExprDeviceBuffer, GpuStatusDetail, GroupedAggKernelMode, GroupedAggOutputStorage,
        GroupedAggSession, GroupedAggStateLane, ResolvedGroupedAggPlan,
        execute_grouped_agg_one_shot_with_mode,
    };

    #[pg_test]
    fn pg_scalar_descriptor_plan_materializes_all_integer_lanes() {
        use crate::engine::executor::agg::test_execute_scalar_descriptor;
        use crate::engine::spec::{
            AggOutputProjection, AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind,
            AggregateOutput, AggregateSource, FilterSpec, MeasureSpec,
        };
        Spi::run(
            "SET pg_accel.enabled=on; SET pg_accel.auto_load=on; \
            CREATE TEMP TABLE scalar_plan_rows AS SELECT i a, \
            CASE WHEN i%11=0 THEN NULL ELSE i%7 END b, i%2=0 p FROM generate_series(1,4097) s(i)",
        )
        .expect("descriptor fixture");
        for mutation in [
            None,
            Some("UPDATE scalar_plan_rows SET a=NULL,b=NULL,p=NULL"),
            Some("TRUNCATE scalar_plan_rows"),
        ] {
            if let Some(sql) = mutation {
                Spi::run(sql).expect("NULL/empty fixture mutation");
            }
            for expression in [
                "CASE WHEN p THEN a+b ELSE NULL END",
                "COALESCE(b,a)",
                "(a+1)*(b-2)",
                "a+COALESCE(b,0)",
                "a-COALESCE(b,1)",
                "a*COALESCE(b,2)",
                "a/COALESCE(b+1,1)",
                "a%COALESCE(b+1,-1)",
                "CASE b WHEN 1 THEN a+b WHEN 2 THEN a-b ELSE a END",
                "CASE p WHEN true THEN a WHEN false THEN b ELSE NULL END",
                "CASE WHEN b IN (1,3,NULL) THEN a+b ELSE a-b END",
                "CASE WHEN b NOT IN (1,2,3) THEN a ELSE b END",
            ] {
                let lowered =
                    lower(&format!("SELECT {expression} FROM scalar_plan_rows")).expect("program");
                let spec = AggQuerySpec {
                    fact_rel: lowered.inputs[0].column.relation_oid,
                    group_keys: vec![],
                    star_dims: vec![],
                    fact_filter: FilterSpec::None,
                    having: None,
                    measures: vec![MeasureSpec {
                        expression: lowered.into_measure_expression(),
                        filter: FilterSpec::None,
                        outputs: [
                            AggregateKind::Sum,
                            AggregateKind::Avg,
                            AggregateKind::Count,
                            AggregateKind::Min,
                            AggregateKind::Max,
                        ]
                        .into_iter()
                        .map(|kind| AggregateOutput {
                            source: AggregateSource::Value,
                            kind,
                        })
                        .collect(),
                    }],
                };
                let projection = AggOutputProjection {
                    slots: spec.measures[0]
                        .outputs
                        .iter()
                        .map(|output| AggOutputSlot {
                            source: AggOutputSource::Aggregate {
                                measure_index: 0,
                                source: output.source,
                                kind: output.kind,
                            },
                            source_type_oid: 23,
                            result_type_oid: match output.kind {
                                AggregateKind::Avg => 1700,
                                AggregateKind::Min | AggregateKind::Max => 23,
                                _ => 20,
                            },
                            result_typmod: -1,
                            result_collation_oid: 0,
                            nullable: output.kind != AggregateKind::Count,
                        })
                        .collect(),
                };
                let expected = vec![
                    Spi::get_one::<Vec<u8>>(&format!(
                        "SELECT int8send(sum({expression})) FROM scalar_plan_rows"
                    ))
                    .expect("sum"),
                    Spi::get_one::<Vec<u8>>(&format!(
                        "SELECT numeric_send(avg({expression})) FROM scalar_plan_rows"
                    ))
                    .expect("avg"),
                    Spi::get_one::<Vec<u8>>(&format!(
                        "SELECT int8send(count({expression})) FROM scalar_plan_rows"
                    ))
                    .expect("count"),
                    Spi::get_one::<Vec<u8>>(&format!(
                        "SELECT int4send(min({expression})) FROM scalar_plan_rows"
                    ))
                    .expect("min"),
                    Spi::get_one::<Vec<u8>>(&format!(
                        "SELECT int4send(max({expression})) FROM scalar_plan_rows"
                    ))
                    .expect("max"),
                ];
                for warm in 0..2 {
                    assert_eq!(
                        test_execute_scalar_descriptor(spec.clone(), projection.clone()),
                        vec![expected.clone()],
                        "full scalar plan materialization, mutation={mutation:?}, warm={warm}: {expression}"
                    );
                }
            }
        }
    }

    #[pg_test]
    fn pg_scalar_aggregate_extraction_uses_the_typed_lowerer() {
        use crate::engine::ffi::planner_hooks::shape::test_parse_scalar_aggregate;
        use crate::engine::spec::{AggregateKind, MeasureExpr};
        setup();
        let extract = |sql: &str| {
            let text = CString::new(sql).expect("aggregate SQL");
            // SAFETY: live backend transaction, parser/analyzer/planner nodes
            // remain owned by PostgreSQL throughout this synchronous extraction.
            unsafe {
                let raw = pg_sys::pg_parse_query(text.as_ptr());
                let statement = pg_sys::list_nth(raw, 0).cast::<pg_sys::RawStmt>();
                let query = pg_sys::parse_analyze_fixedparams(
                    statement,
                    text.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                );
                let target = pg_sys::list_nth((*query).targetList, 0).cast::<pg_sys::TargetEntry>();
                let expression =
                    pg_sys::eval_const_expressions(std::ptr::null_mut(), (*target).expr.cast());
                test_parse_scalar_aggregate(expression, &*query)
            }
        };
        for (aggregate, kind, expression) in [
            ("sum", AggregateKind::Sum, "CASE WHEN p THEN a+b ELSE c END"),
            ("avg", AggregateKind::Avg, "COALESCE(a,b,c)"),
            ("count", AggregateKind::Count, "p AND (q OR a>b)"),
            ("min", AggregateKind::Min, "-(a+b)"),
            ("max", AggregateKind::Max, "a / COALESCE(b,1)"),
            ("count", AggregateKind::Count, "(a+1)*(b-2)"),
            ("sum", AggregateKind::Sum, "1"),
        ] {
            let parsed = extract(&format!(
                "SELECT {aggregate}({expression}) FROM scalar_rows"
            ))
            .expect("scalar aggregate extraction");
            assert_eq!(parsed.output.kind, kind);
            assert_eq!(
                parsed.expression,
                lower(&format!("SELECT {expression} FROM scalar_rows"))
                    .expect("independent lowerer")
                    .into_measure_expression()
            );
            assert!(matches!(parsed.expression, MeasureExpr::Bytecode { .. }));
        }
        for (sql, expected_variant) in [
            ("SELECT count(*) FROM scalar_rows", 0),
            ("SELECT sum(a) FROM scalar_rows", 1),
            ("SELECT sum(a+b) FROM scalar_rows", 2),
        ] {
            let expression = extract(sql).expect("released representation").expression;
            assert_eq!(
                match expression {
                    MeasureExpr::CountStar => 0,
                    MeasureExpr::Column(_) => 1,
                    MeasureExpr::Binary { .. } => 2,
                    _ => 3,
                },
                expected_variant
            );
        }
        for sql in [
            "SELECT sum(a::int8+1) FROM scalar_rows",
            "SELECT sum(abs(a)) FROM scalar_rows",
            "SELECT sum(a+1) FILTER (WHERE abs(a)>0) FROM scalar_rows",
            "SELECT sum(a+1) FILTER (WHERE w>0) FROM scalar_rows",
            "SELECT sum(a::int8) FILTER (WHERE p OR q) FROM scalar_rows",
            "SELECT sum(x.a) FILTER (WHERE y.a>0 OR y.p) FROM scalar_rows x,scalar_rows y",
            "SELECT sum(DISTINCT a+1) FROM scalar_rows",
            "SELECT sum(x.a+y.a+1) FROM scalar_rows x,scalar_rows y",
        ] {
            assert!(
                extract(sql).is_err(),
                "unsupported aggregate escaped: {sql}"
            );
        }
        let filtered =
            extract("SELECT count(a+1) FILTER (WHERE c IS NULL OR c=2) FROM scalar_rows")
                .expect("independent aggregate filter");
        assert!(matches!(
            filtered.filter,
            crate::engine::spec::FilterSpec::NullOrRanges { .. }
        ));
        for (value, predicate, argument) in [
            ("sum(a+1)", "p OR q", "a+1"),
            ("avg(a*b)", "a>b", "a*b"),
            ("min(a)", "CASE WHEN p THEN a>b ELSE q END", "a"),
            ("max(a)", "(a+b) IN (0,7,NULL)", "a"),
            ("count(a>b)", "p OR q", "a>b"),
            ("count(*)", "p OR q", "1"),
        ] {
            let filtered = extract(&format!(
                "SELECT {value} FILTER (WHERE {predicate}) FROM scalar_rows"
            ))
            .expect("typed aggregate FILTER");
            assert_eq!(filtered.filter, crate::engine::spec::FilterSpec::None);
            assert_eq!(
                filtered.expression,
                lower(&format!(
                    "SELECT CASE WHEN {predicate} THEN {argument} ELSE NULL END FROM scalar_rows"
                ))
                .expect("independent lazy CASE lowering")
                .into_measure_expression(),
                "FILTER producer {value} / {predicate}"
            );
        }
    }

    #[pg_test]
    fn pg_scalar_fused_owned_binding_matches_native_aggregates() {
        use crate::engine::residency::resident_live_bytes;
        crate::gpu::cleanup_grouped_allocation_pools();
        let ledger_baseline = resident_live_bytes();
        Spi::run(
            "SET pg_accel.enabled=off; CREATE TEMP TABLE scalar_fused_rows AS \
            SELECT i id, CASE WHEN i%5=0 THEN NULL ELSE i%31-15 END a, \
            i%7-3 b, i%3=0 p FROM generate_series(0,4096) s(i)",
        )
        .expect("native reference rows");
        let a: Vec<i32> = (0..4097)
            .map(|i| if i % 5 == 0 { 0 } else { i % 31 - 15 })
            .collect();
        let b: Vec<i32> = (0..4097).map(|i| i % 7 - 3).collect();
        let p: Vec<u8> = (0..4097).map(|i| u8::from(i % 3 == 0)).collect();
        let an: Vec<u8> = (0..4097).map(|i| u8::from(i % 5 == 0)).collect();
        let a = ExprDeviceBuffer::copy_from_slice(&a).expect("resident a");
        let b = ExprDeviceBuffer::copy_from_slice(&b).expect("resident b");
        let p = ExprDeviceBuffer::copy_from_slice(&p).expect("resident p");
        let an = ExprDeviceBuffer::copy_from_slice(&an).expect("resident NULLs");
        let short_nulls =
            ExprDeviceBuffer::copy_from_slice(&[0_u8]).expect("short sidecar fixture");
        for expression in [
            "CASE WHEN p THEN COALESCE(a,7)+b ELSE NULL END",
            "CASE WHEN b<>0 THEN a/b ELSE 42 END",
            "COALESCE(a,b*2)-3",
        ] {
            let lowered = lower(&format!("SELECT {expression} FROM scalar_fused_rows"))
                .expect("catalog-proved program");
            let fact_rel = lowered.inputs[0].column.relation_oid;
            let logical = lowered.clone().into_measure_expression();
            let requests: Vec<_> = [2, 3, 4]
                .into_iter()
                .map(|attno| crate::engine::residency::ResidentColumnRef {
                    relid: pg_sys::Oid::from(fact_rel),
                    attno,
                })
                .collect();
            let views = [
                crate::engine::residency::ResidentColumnView::I32 {
                    type_oid: pg_sys::INT4OID,
                    values: &a,
                    nulls: Some(&an),
                },
                crate::engine::residency::ResidentColumnView::I32 {
                    type_oid: pg_sys::INT4OID,
                    values: &b,
                    nulls: None,
                },
                crate::engine::residency::ResidentColumnView::Bool {
                    type_oid: pg_sys::BOOLOID,
                    values: &p,
                    nulls: None,
                },
            ];
            let bad_views = [
                crate::engine::residency::ResidentColumnView::I32 {
                    type_oid: pg_sys::INT4OID,
                    values: &a,
                    nulls: Some(&short_nulls),
                },
                crate::engine::residency::ResidentColumnView::I32 {
                    type_oid: pg_sys::INT4OID,
                    values: &b,
                    nulls: None,
                },
                crate::engine::residency::ResidentColumnView::Bool {
                    type_oid: pg_sys::BOOLOID,
                    values: &p,
                    nulls: None,
                },
            ];
            assert!(
                crate::engine::executor::agg::scalar_measure_column(
                    &logical,
                    fact_rel,
                    &requests,
                    &bad_views,
                    4097,
                    abi::PGACCEL_GROUPED_AGG_LANE_COUNT
                )
                .is_err()
            );
            let words = lowered.program.encode();
            let inputs: Vec<_> = lowered
                .inputs
                .iter()
                .map(|col| {
                    let (values, nulls, physical_type) = match col.column.attno {
                        2 => (a.as_ptr().cast(), an.as_ptr(), 2),
                        3 => (b.as_ptr().cast(), std::ptr::null(), 2),
                        4 => (p.as_ptr().cast(), std::ptr::null(), 1),
                        _ => panic!("unexpected scalar input"),
                    };
                    abi::PgaccelScalarInput {
                        values,
                        nulls,
                        physical_type,
                        reserved: 0,
                    }
                })
                .collect();
            for groups in [1_usize, 3, 17] {
                let keys: Vec<i32> = (0..4097).map(|i| (i % groups) as i32).collect();
                let keys = ExprDeviceBuffer::copy_from_slice(&keys).expect("resident keys");
                let view = abi::PgaccelScalarView {
                    version: 1,
                    word_count: words.len() as u32,
                    input_count: inputs.len() as u32,
                    reserved: 0,
                    row_count: 4097,
                    words: words.as_ptr(),
                    inputs: inputs.as_ptr(),
                };
                // SAFETY: zero is valid for the ABI fields; required fields follow.
                let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
                desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
                desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
                desc.row_count = 4097;
                desc.group_capacity = groups;
                desc.where_filter.value_cmp_opcode =
                    crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
                for filter in &mut desc.measure_filters {
                    filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
                }
                if groups > 1 {
                    desc.key_count = 1;
                    desc.keys[0].values.values = keys.as_ptr().cast();
                    desc.keys[0].values.tag = crate::gpu::PgaccelValTag::Int32;
                    desc.keys[0].cardinality = groups as u32;
                    desc.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
                }
                desc.measure_count = 1;
                let measure = &mut desc.measures[0];
                measure.op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
                measure.agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM
                    | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                    | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                    | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
                measure.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
                measure.state_bytes = 8;
                measure.value.values = std::ptr::from_ref(&view).cast();
                measure.value.physical_type = 2;
                measure.value.element_bytes = size_of::<abi::PgaccelScalarView>() as u32;
                measure.value.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1;
                // Exercise the production executor's logical-to-resident
                // binding, not only hand-written ABI metadata.
                let before_binding = resident_live_bytes();
                Spi::run("SET pg_accel.resident_memory_budget_mb=0").expect("zero budget");
                assert!(
                    crate::engine::executor::agg::scalar_measure_column(
                        &logical,
                        fact_rel,
                        &requests,
                        &views,
                        4097,
                        measure.agg_mask,
                    )
                    .is_err()
                );
                // Budget failure may reclaim idle pools but cannot release any
                // active owner's charge or allocate a new metadata snapshot.
                assert!(resident_live_bytes() <= before_binding);
                let before_binding = resident_live_bytes();
                Spi::run("RESET pg_accel.resident_memory_budget_mb").expect("restore budget");
                let (bound_column, _binding_owner) =
                    crate::engine::executor::agg::scalar_measure_column(
                        &logical,
                        fact_rel,
                        &requests,
                        &views,
                        4097,
                        measure.agg_mask,
                    )
                    .expect("executor scalar binding");
                let metadata_bytes = crate::gpu::scalar_metadata_bytes(words.len(), inputs.len())
                    .expect("exact metadata bytes") as u64;
                assert_eq!(resident_live_bytes(), before_binding + metadata_bytes);
                measure.value = bound_column;
                // SAFETY: all input buffers live through synchronous GPU execution;
                // the facade copies host metadata and never reads resident rows.
                let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("owned plan");
                assert_eq!(resident_live_bytes(), before_binding + 2 * metadata_bytes);
                Spi::run("SET pg_accel.resident_memory_budget_mb=0")
                    .expect("zero execution budget");
                use crate::engine::spec::{
                    AggQuerySpec, AggregateKind, AggregateOutput, AggregateSource, ColumnRef,
                    FilterSpec, GroupKeyEncoding, GroupKeyRef, GroupKeySource, MeasureSpec,
                };
                let layout_spec = AggQuerySpec {
                    fact_rel,
                    star_dims: vec![],
                    having: None,
                    fact_filter: FilterSpec::None,
                    group_keys: if groups == 1 {
                        vec![]
                    } else {
                        vec![GroupKeyRef {
                            source: GroupKeySource::FactColumn(ColumnRef {
                                relation_oid: fact_rel,
                                attno: 1,
                                type_oid: 23,
                            }),
                            type_oid: 23,
                            collation_oid: 0,
                            encoding: GroupKeyEncoding::DictionaryI32 {
                                cardinality: groups as u32,
                                null_code: None,
                            },
                        }]
                    },
                    measures: vec![MeasureSpec {
                        expression: logical.clone(),
                        filter: FilterSpec::None,
                        outputs: [
                            AggregateKind::Sum,
                            AggregateKind::Min,
                            AggregateKind::Max,
                            AggregateKind::Count,
                        ]
                        .into_iter()
                        .map(|kind| AggregateOutput {
                            source: AggregateSource::Value,
                            kind,
                        })
                        .collect(),
                    }],
                };
                let before_quote_bytes = resident_live_bytes();
                let before_quote_workspace = crate::gpu::grouped_workspace_pool_snapshot();
                let before_quote_output = crate::gpu::grouped_output_pool_snapshot();
                let forecast = crate::engine::executor::agg::scalar_descriptor_memory_requirements(
                    &layout_spec,
                    &[2],
                    groups,
                    4097,
                    113,
                    8192,
                )
                .expect("one-shot native layout forecast");
                let bounded_forecast =
                    crate::engine::executor::agg::scalar_descriptor_memory_requirements(
                        &layout_spec,
                        &[2],
                        groups,
                        4097,
                        113,
                        113,
                    )
                    .expect("bounded native layout forecast");
                assert_eq!(resident_live_bytes(), before_quote_bytes);
                assert_eq!(
                    crate::gpu::grouped_workspace_pool_snapshot(),
                    before_quote_workspace
                );
                assert_eq!(
                    crate::gpu::grouped_output_pool_snapshot(),
                    before_quote_output
                );
                assert!(plan.row_chunk(0, 1).is_err());
                assert!(GroupedAggOutputStorage::new(&plan).is_err());
                assert!(crate::gpu::GroupedAggWorkspace::allocate(&plan).is_err());
                assert!(GroupedAggSession::start(&plan, 113).is_err());
                // SAFETY: bound metadata and resident inputs are still live;
                // adoption must fail solely because its snapshot cannot fit.
                assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
                assert_eq!(resident_live_bytes(), before_binding + 2 * metadata_bytes);
                Spi::run("RESET pg_accel.resident_memory_budget_mb")
                    .expect("restore execution budget");
                for states in 1..=4 {
                    for count_star in [false, true] {
                        if count_star && states == 1 {
                            continue;
                        }
                        let mut multi_spec = layout_spec.clone();
                        multi_spec
                            .measures
                            .resize(states, multi_spec.measures[0].clone());
                        let mut multi_desc = desc;
                        multi_desc.measure_count = states as u32;
                        for slot in 1..states {
                            multi_desc.measures[slot] = multi_desc.measures[0];
                        }
                        if count_star {
                            multi_spec.measures[states - 1] = MeasureSpec {
                                expression: crate::engine::spec::MeasureExpr::CountStar,
                                filter: FilterSpec::None,
                                outputs: vec![AggregateOutput {
                                    source: AggregateSource::Value,
                                    kind: AggregateKind::Count,
                                }],
                            };
                            // SAFETY: inactive COUNT(*) inputs are canonical zero ABI fields.
                            multi_desc.measures[states - 1] = unsafe { std::mem::zeroed() };
                            let last = &mut multi_desc.measures[states - 1];
                            last.op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
                            last.agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
                            last.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
                            last.state_bytes = 8;
                        }
                        // SAFETY: scalar slots borrow the live binder's metadata and
                        // resident lanes. Adoption creates independent owned snapshots.
                        let multi_plan = unsafe { ResolvedGroupedAggPlan::from_abi(multi_desc) }
                            .expect("multi-state plan");
                        let actual = GroupedAggSession::memory_requirements(&multi_plan, 113)
                            .expect("bound native layout");
                        let predicted =
                            crate::engine::executor::agg::scalar_descriptor_memory_requirements(
                                &multi_spec,
                                &[2],
                                groups,
                                4097,
                                113,
                                113,
                            )
                            .expect("multi-state scalar forecast");
                        assert_eq!(
                            predicted, actual,
                            "{states} states, count_star={count_star}"
                        );
                    }
                }
                let mut output = GroupedAggOutputStorage::new(&plan).expect("output");
                let (_, mode) = execute_grouped_agg_one_shot_with_mode(&plan, &mut output)
                    .expect("fused device execution");
                assert_eq!(
                    crate::gpu::grouped_output_pool_snapshot().fresh_allocation_bytes
                        - before_quote_output.fresh_allocation_bytes,
                    forecast.output_bytes as u64
                );
                assert_eq!(
                    crate::gpu::grouped_workspace_pool_snapshot().fresh_allocation_bytes
                        - before_quote_workspace.fresh_allocation_bytes,
                    forecast.workspace_bytes as u64
                );
                assert_eq!(mode, GroupedAggKernelMode::ParallelDenseInteger);
                Spi::connect(|client| {
                    for row in client.select(&format!("SELECT id%{groups} g, sum(v) s, min(v)::bigint lo, \
                        max(v)::bigint hi, count(v) n FROM (SELECT id,{expression} v FROM scalar_fused_rows) t GROUP BY 1"),
                        None, &[]).expect("native aggregate oracle") {
                        let group = row.get::<i32>(1).expect("group datum").expect("group") as usize;
                        let count = row.get::<i64>(5).expect("count datum").expect("count") as u64;
                        assert_eq!(output.measure_count(0).expect("COUNT lane")[group], count);
                        assert_eq!(output.measure_nonnull_count(0).expect("validity lane")[group], count);
                        for (index, lane) in [(2,GroupedAggStateLane::Sum), (3,GroupedAggStateLane::Min),
                            (4,GroupedAggStateLane::Max)] {
                            if let Some(value) = row.get::<i64>(index).expect("native lane") {
                                assert_eq!(output.measure_i64_at(0,lane,group).expect("device lane"), value,
                                    "{expression}: group {group}");
                            }
                        }
                    }
                });
                let mut session = GroupedAggSession::start(&plan, 113).expect("bounded session");
                assert_eq!(
                    session.workspace().requirement().bytes,
                    bounded_forecast.workspace_bytes
                );
                assert_eq!(forecast.output_bytes, bounded_forecast.output_bytes);
                drop(plan);
                // SAFETY: resident buffers remain live across fresh metadata adoption.
                let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("fresh plan");
                for first in (0..4097).step_by(113) {
                    session
                        .accumulate(
                            &plan
                                .row_chunk(first, (4097 - first).min(113))
                                .expect("chunk"),
                        )
                        .expect("owned chunk execution");
                }
                session
                    .accumulate(&plan.row_chunk(4097, 0).expect("empty chunk"))
                    .expect("empty chunk");
                let mut chunked = GroupedAggOutputStorage::new(&plan).expect("chunked output");
                session
                    .finalize(&plan, &mut chunked)
                    .expect("finalize fresh plan");
                assert_eq!(chunked.measure_count(0), output.measure_count(0));
                for lane in [
                    GroupedAggStateLane::Sum,
                    GroupedAggStateLane::Min,
                    GroupedAggStateLane::Max,
                ] {
                    assert_eq!(
                        chunked.measure_state(0, lane),
                        output.measure_state(0, lane)
                    );
                }
                session.reset(&plan).expect("reset");
                session
                    .accumulate(&plan.row_chunk(0, 113).expect("reset chunk"))
                    .expect("reused session");
                // Reuse a valid typed binding with an unguarded division program.
                if expression == "CASE WHEN b<>0 THEN a/b ELSE 42 END" {
                    let failing =
                        lower("SELECT a/b FROM scalar_fused_rows").expect("division program");
                    let fault_words = failing.program.encode();
                    let fault_inputs = [
                        abi::PgaccelScalarInput {
                            values: a.as_ptr().cast(),
                            nulls: an.as_ptr(),
                            physical_type: 2,
                            reserved: 0,
                        },
                        abi::PgaccelScalarInput {
                            values: b.as_ptr().cast(),
                            nulls: std::ptr::null(),
                            physical_type: 2,
                            reserved: 0,
                        },
                    ];
                    let fault_view = abi::PgaccelScalarView {
                        word_count: fault_words.len() as u32,
                        input_count: 2,
                        words: fault_words.as_ptr(),
                        inputs: fault_inputs.as_ptr(),
                        ..view
                    };
                    desc.measures[0].value.values = std::ptr::from_ref(&fault_view).cast();
                    desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
                    let (fault_column, _fault_owner) =
                        crate::engine::executor::agg::scalar_measure_column(
                            &failing.into_measure_expression(),
                            fact_rel,
                            &requests,
                            &views,
                            4097,
                            abi::PGACCEL_GROUPED_AGG_LANE_COUNT,
                        )
                        .expect("executor error binding");
                    desc.measures[0].value = fault_column;
                    // SAFETY: both resident inputs and host metadata stay valid.
                    let fault_plan =
                        unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("fault plan");
                    let mut fault_output =
                        GroupedAggOutputStorage::new(&fault_plan).expect("fault output");
                    let error =
                        execute_grouped_agg_one_shot_with_mode(&fault_plan, &mut fault_output)
                            .expect_err("COUNT must evaluate its expression");
                    assert_eq!(error.status, GpuStatusDetail::DivisionByZero);
                }
            }
        }
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            resident_live_bytes(),
            ledger_baseline,
            "all snapshots and pooled execution buffers released"
        );
    }

    #[pg_test]
    fn pg_scalar_resident_artifact_rejects_stale_inputs_and_reloads() {
        use crate::engine::executor::agg::test_scalar_resident_aggregate;
        use crate::engine::spec::{
            AggQuerySpec, AggregateKind, AggregateOutput, AggregateSource, FilterSpec, MeasureSpec,
        };
        Spi::run(
            "SET pg_accel.enabled=off; SET pg_accel.auto_load=on; \
            CREATE TEMP TABLE scalar_current_rows AS SELECT i a, i%7 b, i%2=0 p \
            FROM generate_series(1,4097) s(i)",
        )
        .expect("currentness fixture");
        let expression = "CASE WHEN p THEN a+b ELSE NULL END";
        let lowered =
            lower(&format!("SELECT {expression} FROM scalar_current_rows")).expect("typed program");
        let spec = AggQuerySpec {
            fact_rel: lowered.inputs[0].column.relation_oid,
            group_keys: vec![],
            star_dims: vec![],
            fact_filter: FilterSpec::None,
            having: None,
            measures: vec![MeasureSpec {
                expression: lowered.into_measure_expression(),
                filter: FilterSpec::None,
                outputs: [AggregateKind::Sum, AggregateKind::Count]
                    .into_iter()
                    .map(|kind| AggregateOutput {
                        source: AggregateSource::Value,
                        kind,
                    })
                    .collect(),
            }],
        };
        let assert_current = || {
            let expected_sum = Spi::get_one::<i64>(&format!(
                "SELECT sum({expression}) FROM scalar_current_rows"
            ))
            .expect("native sum");
            let expected_count = Spi::get_one::<i64>(&format!(
                "SELECT count({expression}) FROM scalar_current_rows"
            ))
            .expect("native count")
            .expect("nonnull count") as u64;
            assert_eq!(
                test_scalar_resident_aggregate(&spec, true).expect("reload and execute"),
                (expected_sum, expected_count)
            );
            assert_eq!(
                test_scalar_resident_aggregate(&spec, false).expect("warm artifact"),
                (expected_sum, expected_count)
            );
        };
        assert_current();
        let mut different_program = spec.clone();
        different_program.measures[0].expression =
            lower("SELECT CASE WHEN p THEN a-b ELSE NULL END FROM scalar_current_rows")
                .expect("different uncached scalar program")
                .into_measure_expression();
        Spi::run("SET pg_accel.resident_memory_budget_mb=0").expect("zero artifact budget");
        assert!(
            test_scalar_resident_aggregate(&different_program, true).is_err(),
            "uncached retained program cannot bypass zero budget"
        );
        Spi::run("RESET pg_accel.resident_memory_budget_mb").expect("restore artifact budget");
        assert_current();
        for mutation in [
            "UPDATE scalar_current_rows SET b=b+100",
            "DO $$ BEGIN BEGIN UPDATE scalar_current_rows SET p=NULL; RAISE EXCEPTION USING ERRCODE='P0002'; EXCEPTION WHEN no_data_found THEN NULL; END; END $$",
            "UPDATE scalar_current_rows SET a=NULL WHERE p",
            "TRUNCATE scalar_current_rows",
            "INSERT INTO scalar_current_rows VALUES (42,3,true),(NULL,2,true),(1,1,false)",
        ] {
            Spi::run(mutation).expect("mutate only resident scalar dependencies");
            assert!(
                test_scalar_resident_aggregate(&spec, false).is_err(),
                "stale scalar artifact after {mutation}"
            );
            assert_current();
        }
        Spi::run("ALTER TABLE scalar_current_rows ALTER COLUMN a TYPE bigint")
            .expect("change input representation");
        assert!(test_scalar_resident_aggregate(&spec, false).is_err());
        assert!(
            test_scalar_resident_aggregate(&spec, true).is_err(),
            "old INT4 program cannot reinterpret reloaded INT8"
        );
        Spi::run("SELECT pg_accel_evict('scalar_current_rows'::regclass); DROP TABLE scalar_current_rows").expect("release fixture");
        crate::gpu::cleanup_grouped_allocation_pools();
    }

    fn lower(
        sql: &str,
    ) -> Result<LoweredScalarExpression, crate::engine::ffi::planner_hooks::shape::ShapeDecline>
    {
        lower_with(sql, |_| {})
    }

    fn lower_with(
        sql: &str,
        modify: impl FnOnce(*mut pg_sys::Node),
    ) -> Result<LoweredScalarExpression, crate::engine::ffi::planner_hooks::shape::ShapeDecline>
    {
        let text = CString::new(sql).expect("fixture SQL");
        // SAFETY: this pg_test runs on the backend main thread in an active
        // transaction. Parser/analyzer allocations stay alive for the whole
        // synchronous lowering call; no PostgreSQL pointers are returned.
        unsafe {
            let raw = pg_sys::pg_parse_query(text.as_ptr());
            assert_eq!(pg_sys::list_length(raw), 1);
            let statement = pg_sys::list_nth(raw, 0).cast::<pg_sys::RawStmt>();
            let query = pg_sys::parse_analyze_fixedparams(
                statement,
                text.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            );
            assert!(!query.is_null());
            let target = pg_sys::list_nth((*query).targetList, 0).cast::<pg_sys::TargetEntry>();
            let expression =
                pg_sys::eval_const_expressions(std::ptr::null_mut(), (*target).expr.cast());
            pg_sys::convert_saop_to_hashed_saop(expression);
            modify(expression);
            lower_scalar_expression(expression, &*query)
        }
    }

    fn setup() {
        Spi::run("SET pg_accel.enabled=off; CREATE TEMP TABLE scalar_rows(id int4,a int4,b int4,c int4,p bool,q bool,w int8,d date); \
            INSERT INTO scalar_rows VALUES \
            (1,7,3,-2,true,false,1,'2026-01-01'), \
            (2,-7,3,2,false,true,2,'2026-01-02'), \
            (3,NULL,3,0,NULL,false,3,'2026-01-03'), \
            (4,7,NULL,-1,true,NULL,4,'2026-01-04'), \
            (5,NULL,NULL,NULL,NULL,NULL,5,'2026-01-05');")
            .expect("native scalar fixture");
    }

    fn verify(expression: &str) {
        let lowered = lower(&format!("SELECT {expression} FROM scalar_rows")).expect(expression);
        let logical = lowered.clone().into_measure_expression();
        let crate::engine::spec::MeasureExpr::Bytecode {
            inputs,
            program,
            result_type_oid,
        } = logical
        else {
            panic!("lowering must preserve the versioned program envelope");
        };
        assert_eq!(program, lowered.program.encode());
        assert_eq!(
            inputs,
            lowered
                .inputs
                .iter()
                .map(|input| input.column)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            result_type_oid,
            if lowered.program.result_type() == ScalarType::Int4 {
                23
            } else {
                16
            }
        );
        for id in 1..=5 {
            let values = lowered
                .inputs
                .iter()
                .map(|column| {
                    let name = match column.column.attno {
                        2 => "a",
                        3 => "b",
                        4 => "c",
                        5 => "p",
                        6 => "q",
                        _ => panic!("unexpected lowered input {column:?}"),
                    };
                    let sql = format!("SELECT {name} FROM scalar_rows WHERE id={id}");
                    match column.column.type_oid {
                        23 => Spi::get_one::<i32>(&sql)
                            .expect("int4 input")
                            .map_or(Value::Null(ScalarType::Int4), Value::Int4),
                        16 => Spi::get_one::<bool>(&sql)
                            .expect("Boolean input")
                            .map_or(Value::Null(ScalarType::Bool), Value::Bool),
                        _ => panic!("unsupported input type"),
                    }
                })
                .collect::<Vec<_>>();
            let expected = match lowered.program.result_type() {
                ScalarType::Int4 => Spi::get_one::<i32>(&format!(
                    "SELECT {expression} FROM scalar_rows WHERE id={id}"
                ))
                .expect(expression)
                .map_or(Value::Null(ScalarType::Int4), Value::Int4),
                ScalarType::Bool => Spi::get_one::<bool>(&format!(
                    "SELECT {expression} FROM scalar_rows WHERE id={id}"
                ))
                .expect(expression)
                .map_or(Value::Null(ScalarType::Bool), Value::Bool),
                ScalarType::Date => panic!("this fixture has no DATE outputs"),
            };
            assert_eq!(
                evaluate(&lowered.program, &values),
                Ok(expected),
                "{expression}, row {id}"
            );
        }
    }

    #[pg_test]
    fn pg_scalar_null_safe_lowering_matches_native() {
        setup();
        for expression in [
            "a IS DISTINCT FROM b",
            "a IS NOT DISTINCT FROM b",
            "p IS DISTINCT FROM q",
            "p IS NOT DISTINCT FROM q",
            "p IS TRUE",
            "p IS NOT TRUE",
            "p IS FALSE",
            "p IS NOT FALSE",
            "p IS UNKNOWN",
            "p IS NOT UNKNOWN",
            "(a>b) IS NOT FALSE",
            "(p OR q) IS NOT TRUE",
            "CASE WHEN a IS DISTINCT FROM b THEN a ELSE b END",
            "COALESCE(p,q) IS NOT FALSE",
        ] {
            verify(expression);
        }
        for expression in [
            "a IS DISTINCT FROM b",
            "p IS NOT TRUE",
            "p IS NOT DISTINCT FROM q",
        ] {
            let lowered =
                lower(&format!("SELECT {expression} FROM scalar_rows")).expect(expression);
            assert!(
                crate::engine::spec::scalar::ScalarProgram::validate_wire(
                    &lowered.program.encode()
                )
                .expect("proof")
                .result_is_non_null()
            );
        }
        let result = lower_with("SELECT a IS DISTINCT FROM b FROM scalar_rows", |node| {
            // SAFETY: this analyzed expression is a DistinctExpr (OpExpr layout).
            unsafe {
                assert_eq!((*node).type_, pg_sys::NodeTag::T_DistinctExpr);
                (*node.cast::<pg_sys::OpExpr>()).opfuncid = pg_sys::Oid::from(pg_sys::F_INT4NE);
            }
        });
        assert!(
            result.is_err(),
            "catalog equality mismatch must be rejected"
        );
    }

    #[pg_test]
    fn pg_scalar_nullif_extrema_lowering_matches_native() {
        setup();
        for expression in [
            "NULLIF(a,b)",
            "NULLIF(a,a)",
            "NULLIF(p,q)",
            "a/NULLIF(b,0)",
            "NULLIF(a+b,c)",
            "GREATEST(a,b,c)",
            "LEAST(a,b,c)",
            "GREATEST(NULL,a,NULL,b,c,NULL)",
            "LEAST(NULL,a,NULL,b,c,NULL)",
            "GREATEST(p,q)",
            "LEAST(p,q)",
            "LEAST(GREATEST(a,-5),5)",
            "NULLIF(GREATEST(a,b),LEAST(a,b))",
            "COALESCE(NULLIF(a,b),c)",
            "CASE WHEN p THEN GREATEST(a,b) ELSE NULLIF(b,c) END",
        ] {
            verify(expression);
        }
        let lowered = lower("SELECT NULLIF(a+b,c) FROM scalar_rows").expect("NULLIF");
        assert_eq!(
            lowered
                .program
                .instructions()
                .iter()
                .filter(|op| **op == I::Add)
                .count(),
            1
        );
        assert!(
            lower_with("SELECT NULLIF(a,b) FROM scalar_rows", |node| {
                // SAFETY: analyzed NULLIF has OpExpr layout.
                unsafe {
                    (*node.cast::<pg_sys::OpExpr>()).opfuncid = pg_sys::Oid::from(pg_sys::F_INT4NE);
                }
            })
            .is_err()
        );
        assert!(
            lower_with("SELECT GREATEST(a,b) FROM scalar_rows", |node| {
                // SAFETY: analyzed GREATEST has MinMaxExpr layout.
                unsafe {
                    (*node.cast::<pg_sys::MinMaxExpr>()).inputcollid = pg_sys::Oid::from(100);
                }
            })
            .is_err()
        );
        // SAFETY: backend-local type cache, restored before any assertion. This
        // simulates a replaced default comparison without editing global catalogs.
        unsafe {
            let entry =
                pg_sys::lookup_type_cache(pg_sys::INT4OID, pg_sys::TYPECACHE_CMP_PROC as i32);
            assert!(!entry.is_null());
            let saved = (*entry).cmp_proc;
            (*entry).cmp_proc = pg_sys::Oid::from(pg_sys::F_BTINT8CMP);
            let result = lower("SELECT GREATEST(a,b) FROM scalar_rows");
            (*entry).cmp_proc = saved;
            assert!(result.is_err(), "replaced comparison must stay native");
        }
    }

    #[pg_test]
    fn pg_scalar_lowering_matches_native_simplified_trees() {
        setup();
        for expression in [
            "a+(b*c)-(-a)",
            "a/b",
            "a%b",
            "a+42",
            "a-42",
            "a*2",
            "a / -1",
            "a % -1",
            "a+COALESCE(b,0)",
            "a-COALESCE(b,1)",
            "a*COALESCE(b,2)",
            "a/COALESCE(b,1)",
            "a%COALESCE(b,-1)",
            "a=b",
            "a<>b",
            "a<b",
            "a<=b",
            "a>b",
            "a>=b",
            "p=q",
            "p<>q",
            "p<q",
            "p<=q",
            "p>q",
            "p>=q",
            "p AND q",
            "p OR q",
            "NOT p",
            "a IS NULL",
            "p IS NOT NULL",
            "p AND q AND a>0",
            "p OR q OR a>0",
            "(a>0 OR b<0) AND (p OR NOT q)",
            "CASE WHEN p THEN a*b WHEN q THEN a+b ELSE c END",
            "CASE WHEN a IS NULL THEN COALESCE(b,c,42) ELSE a+COALESCE(b,c,0) END",
            "COALESCE(a,b,c,42)",
            "COALESCE(p,q,false)",
            "CASE WHEN a IS NULL THEN NULL ELSE a*2 END",
            "CASE WHEN p THEN CASE WHEN q THEN a ELSE b END ELSE c END",
            "CASE a WHEN 7 THEN b WHEN -7 THEN c ELSE 42 END",
            "CASE a WHEN NULL THEN 99 WHEN b THEN c ELSE a+b END",
            "CASE p WHEN true THEN a WHEN false THEN b ELSE c END",
            "CASE p WHEN q THEN a ELSE b END",
            "CASE a+b WHEN 10 THEN CASE c WHEN -2 THEN 42 ELSE b END ELSE a END",
            "CASE a WHEN CASE p WHEN true THEN 7 ELSE -7 END THEN b ELSE c END",
            "CASE p WHEN true THEN q WHEN false THEN NOT q END",
            "COALESCE(CASE a WHEN 7 THEN b WHEN -7 THEN c END,42)",
            "a IN (7,-7,NULL,7)",
            "a NOT IN (7,-7,NULL,7)",
            "a IN (-7,7)",
            "a NOT IN (-7,7)",
            "(a+b) IN (7,8,9,10)",
            "a = ANY('{}'::int4[])",
            "a <> ALL('{}'::int4[])",
            "a = ANY(NULL::int4[])",
            "a <> ALL(NULL::int4[])",
            "a IN (NULL,NULL)",
            "a = ANY('[5:7]={-7,NULL,7}'::int4[])",
            "a <> ALL('{{-7,7},{NULL,42}}'::int4[])",
            "p IN (true,false)",
            "p NOT IN (false,NULL)",
            "p = ANY('{}'::bool[])",
            "p <> ALL(NULL::bool[])",
            "CASE WHEN a IN (7,-7,NULL) THEN b ELSE c END",
            "COALESCE(a NOT IN (-7,7,NULL), p)",
            "CASE a%3 WHEN 0 THEN b WHEN 1 THEN b*2 ELSE COALESCE(b,0)-COALESCE(a,0) END",
            "CASE WHEN CASE WHEN p THEN a=b ELSE q END THEN a ELSE b END",
            "42",
            "NULL::int4",
            "true",
            "NULL::boolean",
            "a+a+a",
            "ROW(a,b) IS NULL",
            "ROW(a,b) IS NOT NULL",
            "NOT (ROW(a,b) IS NULL)",
        ] {
            verify(expression);
        }
        let repeated = lower("SELECT a+a+a FROM scalar_rows").expect("deduplicated input");
        assert_eq!(repeated.inputs.len(), 1);
        for (simple, searched) in [
            (
                "CASE a WHEN 7 THEN b ELSE c END",
                "CASE WHEN a=7 THEN b ELSE c END",
            ),
            (
                "CASE p WHEN true THEN a ELSE b END",
                "CASE WHEN p THEN a ELSE b END",
            ),
            (
                "CASE p WHEN false THEN a ELSE b END",
                "CASE WHEN NOT p THEN a ELSE b END",
            ),
        ] {
            let simple = lower(&format!("SELECT {simple} FROM scalar_rows")).expect("simple CASE");
            let searched =
                lower(&format!("SELECT {searched} FROM scalar_rows")).expect("searched CASE");
            assert_eq!(
                simple.program, searched.program,
                "one WHEN needs no saved copy"
            );
            assert_eq!(simple.inputs, searched.inputs);
        }
        let once =
            lower("SELECT CASE a+b WHEN 10 THEN c WHEN -4 THEN b ELSE a END FROM scalar_rows")
                .expect("simple CASE test evaluated once");
        assert_eq!(
            once.program
                .instructions()
                .iter()
                .filter(|i| **i == I::Add)
                .count(),
            1
        );
        let membership_once = lower("SELECT (a+b) IN (7,10,42,NULL) FROM scalar_rows")
            .expect("computed membership input");
        assert_eq!(
            membership_once
                .program
                .instructions()
                .iter()
                .filter(|i| **i == I::Add)
                .count(),
            1
        );
        let fused = lower("SELECT CASE a%3 WHEN 0 THEN b WHEN 1 THEN b*2 ELSE COALESCE(b,0)-COALESCE(a,0) END FROM scalar_rows")
            .expect("fused multi-arm CASE");
        assert_eq!(
            fused
                .program
                .instructions()
                .iter()
                .filter(|i| matches!(i, I::EqJumpIfNotTrue(_)))
                .count(),
            2
        );
        assert_eq!(
            fused
                .program
                .instructions()
                .iter()
                .filter(|i| matches!(i, I::CoalesceInt4(0)))
                .count(),
            1
        );
        assert!(fused.program.instructions().contains(&I::Int4Immediate(
            crate::engine::spec::scalar::ScalarArithmetic::Mod,
            3
        )));
        assert!(fused.program.instructions().contains(&I::Int4CoalesceRhs(
            crate::engine::spec::scalar::ScalarArithmetic::Sub,
            0
        )));
        let folded =
            lower("SELECT NULL::int4+(a/0) FROM scalar_rows").expect("PG folded literal NULL");
        assert!(folded.inputs.is_empty());
        assert_eq!(folded.program.instructions(), &[I::Null(ScalarType::Int4)]);
        let row_null = lower("SELECT a+(b/0) FROM scalar_rows").expect("runtime NULL");
        assert_eq!(
            evaluate(
                &row_null.program,
                &[Value::Null(ScalarType::Int4), Value::Int4(1)]
            ),
            Err(crate::engine::spec::scalar::tests::Fault::DivisionByZero)
        );
    }

    #[pg_test]
    fn pg_scalar_lowering_preserves_lazy_error_branches() {
        setup();
        for expression in [
            "CASE WHEN a IS NULL THEN 42 ELSE CASE WHEN b IS NULL THEN 7 ELSE a/b END END",
            "CASE WHEN a IS NOT NULL THEN a ELSE COALESCE(b,c,42) END",
            "CASE WHEN p IS NULL THEN 42 ELSE CASE WHEN p THEN a ELSE b END END",
            "COALESCE(a,COALESCE(b,c,42),a/0)",
            "(p IS NULL OR p IS NOT NULL) OR a/0>0",
            "(p IS NULL AND p IS NOT NULL) AND a/0>0",
            "CASE COALESCE(a,0) WHEN COALESCE(a,0) THEN b ELSE a/0 END",
            "CASE COALESCE(a,0) WHEN COALESCE(a,0) THEN b WHEN a/0 THEN c ELSE 42 END",
            "CASE WHEN a IN (7,-7) THEN b ELSE a/0 END",
            "CASE WHEN a IS NULL THEN 42 ELSE CASE WHEN a IN (7,-7) THEN b ELSE a/0 END END",
        ] {
            verify(expression);
        }
        let lazy = lower("SELECT CASE WHEN p THEN a/0 ELSE 42 END FROM scalar_rows")
            .expect("searched CASE");
        assert_eq!(
            evaluate(&lazy.program, &[Value::Bool(false), Value::Int4(7)]),
            Ok(Value::Int4(42))
        );
        assert_eq!(
            evaluate(
                &lazy.program,
                &[Value::Null(ScalarType::Bool), Value::Int4(7)]
            ),
            Ok(Value::Int4(42))
        );
        assert_eq!(
            evaluate(&lazy.program, &[Value::Bool(true), Value::Int4(7)]),
            Err(crate::engine::spec::scalar::tests::Fault::DivisionByZero)
        );
    }

    #[pg_test]
    fn pg_scalar_date_lowering_matches_native_boundaries_and_errors() {
        use crate::engine::spec::scalar::tests::Fault;
        Spi::run("CREATE TEMP TABLE scalar_dates(id int4,d date,e date,n int4); \
            INSERT INTO scalar_dates VALUES \
            (1,'2000-01-01','2000-01-02',1), \
            (2,'4714-11-24 BC','5874897-12-31',-1), \
            (3,'5874897-12-31','4714-11-24 BC',1), \
            (4,'infinity','-infinity',-2147483648), \
            (5,'-infinity','infinity',2147483647), \
            (6,NULL,'infinity',0),(7,'infinity',NULL,NULL), \
            (8,'2000-01-01','2000-01-01',2147483647); \
            CREATE FUNCTION pg_temp.date_try(query text) RETURNS text LANGUAGE plpgsql AS $$ \
            DECLARE value text; BEGIN EXECUTE query INTO value; \
            RETURN 'ok:'||COALESCE(value,'NULL'); EXCEPTION WHEN OTHERS THEN RETURN 'error:'||SQLSTATE; END $$")
            .expect("date boundary fixture");
        for expression in [
            "d+n",
            "n+d",
            "d-n",
            "d-e",
            "d<e",
            "d<=e",
            "d>e",
            "d>=e",
            "d=e",
            "d<>e",
            "d IS DISTINCT FROM e",
            "d IS NOT DISTINCT FROM e",
            "(d=e) IS NOT FALSE",
            "COALESCE(d,e)",
            "CASE d WHEN DATE '2000-01-01' THEN e ELSE d END",
            "CASE WHEN d<e THEN d+n ELSE e END",
            "d IS NULL",
            "d IS NOT NULL",
            "CASE WHEN d='infinity'::date OR d='-infinity'::date THEN 0 ELSE d-DATE '2000-01-01' END",
            "CASE WHEN n=1 THEN d-e ELSE 0 END",
            "COALESCE(d,e+n)",
            "NULLIF(d,e)",
            "GREATEST(d,e)",
            "LEAST(d,e,DATE '2000-01-01')",
            "GREATEST(d,e+n)",
            "NULLIF(d,e+n)",
        ] {
            let lowered =
                lower(&format!("SELECT {expression} FROM scalar_dates")).expect(expression);
            let projected = if lowered.program.result_type() == ScalarType::Date {
                format!("encode(date_send({expression}),'hex')")
            } else {
                format!("({expression})::text")
            };
            for id in 1..=8 {
                let inputs = lowered
                    .inputs
                    .iter()
                    .map(|column| {
                        let name = match column.column.attno {
                            2 => "d",
                            3 => "e",
                            4 => "n",
                            _ => panic!("unexpected date input"),
                        };
                        let query = format!("SELECT {name} FROM scalar_dates WHERE id={id}");
                        if column.column.type_oid == 1082 {
                            Spi::get_one::<pgrx::datum::Date>(&query)
                                .expect("date input")
                                .map_or(Value::Null(ScalarType::Date), |date| {
                                    Value::Date(date.to_pg_epoch_days())
                                })
                        } else {
                            Spi::get_one::<i32>(&query)
                                .expect("days input")
                                .map_or(Value::Null(ScalarType::Int4), Value::Int4)
                        }
                    })
                    .collect::<Vec<_>>();
                let interpreted = match evaluate(&lowered.program, &inputs) {
                    Ok(Value::Date(date)) => {
                        format!("ok:{:08x}", u32::from_be_bytes(date.to_be_bytes()))
                    }
                    Ok(Value::Int4(value)) => format!("ok:{value}"),
                    Ok(Value::Bool(value)) => format!("ok:{value}"),
                    Ok(Value::Null(_)) => "ok:NULL".into(),
                    Err(Fault::DateOutOfRange) => "error:22008".into(),
                    Err(Fault::Overflow) => "error:22003".into(),
                    Err(Fault::DivisionByZero) => "error:22012".into(),
                };
                let native = Spi::get_one::<String>(&format!("SELECT pg_temp.date_try($date$SELECT {projected} FROM scalar_dates WHERE id={id}$date$)"))
                    .expect("native date oracle").expect("outcome");
                assert_eq!(interpreted, native, "{expression}, row {id}");
            }
        }
        for expression in [
            "d+INTERVAL '1 day'",
            "d::timestamp",
            "d IN (DATE '2000-01-01',DATE '2000-01-02')",
            "extract(year FROM d)",
        ] {
            assert!(
                lower(&format!("SELECT {expression} FROM scalar_dates")).is_err(),
                "{expression} must stay native"
            );
        }
    }

    #[pg_test]
    fn pg_scalar_lowering_matches_native_error_states() {
        setup();
        Spi::run("CREATE FUNCTION pg_temp.scalar_try(query text) RETURNS text LANGUAGE plpgsql AS $$ \
            DECLARE value text; BEGIN EXECUTE query INTO value; \
            RETURN 'ok:'||COALESCE(value,'NULL'); EXCEPTION WHEN OTHERS THEN RETURN 'error:'||SQLSTATE; END $$")
            .expect("native subtransaction error oracle");
        for expression in [
            "a+2147483647",
            "a-2147483647",
            "a*2147483647",
            "-(a+2147483647)",
            "a/(b-b)",
            "a%(b-b)",
            "a+(b/0)",
            "a/COALESCE(b-b,0)",
            "NULLIF(a,b/0)",
            "NULLIF(a/0,b)",
            "GREATEST(a,b/0)",
            "LEAST(a,b/0)",
            "GREATEST(2147483647,a,b/0)",
            "LEAST((-2147483647-1),a,b/0)",
            "a/NULLIF(b-b,0)",
            "NULLIF(a+2147483647,b)",
            "a%COALESCE(b-b,0)",
            "a+COALESCE(b,2147483647)",
            "a-COALESCE(b,(-2147483647-1))",
            "a*COALESCE(b,2147483647)",
            "CASE WHEN p THEN a/COALESCE(b-b,0) ELSE 42 END",
            "NULL::int4+(b/0)",
            "a IS DISTINCT FROM b/0",
            "a/0 IS NOT DISTINCT FROM b",
            "(a/0>0) IS UNKNOWN",
            "(a/0>0) IS NOT TRUE",
            "CASE WHEN p THEN a+2147483647 ELSE a/(b-b) END",
            "COALESCE(a,b/0)",
            "p AND a/0>0",
            "p OR a/0>0",
            "CASE a/(b-b) WHEN 1 THEN 42 ELSE 7 END",
            "CASE a WHEN b/0 THEN 42 ELSE 7 END",
            "CASE a WHEN 7 THEN b/0 ELSE 42 END",
            "CASE a WHEN 7 THEN 42 ELSE b/0 END",
            "CASE a+2147483647 WHEN 1 THEN 42 ELSE 7 END",
            "CASE p WHEN true THEN a/0 WHEN false THEN b/0 ELSE 42 END",
            "(a/0) IN (1,2,3)",
            "(a+2147483647) NOT IN (1,2,3,NULL)",
            "(a/0) = ANY('{}'::int4[])",
            "(a/0) <> ALL('{}'::int4[])",
            "(a/0) = ANY(NULL::int4[])",
            "CASE WHEN a IN (7,NULL) THEN b/0 ELSE 42 END",
            "CASE WHEN a NOT IN (7,NULL) THEN b/0 ELSE 42 END",
            "CASE a/(b-b) WHEN NULL THEN 42 ELSE 7 END",
        ] {
            let lowered =
                lower(&format!("SELECT {expression} FROM scalar_rows")).expect(expression);
            for id in 1..=5 {
                let mut values = Vec::new();
                for column in &lowered.inputs {
                    let name = match column.column.attno {
                        2 => "a",
                        3 => "b",
                        5 => "p",
                        _ => panic!("unexpected input"),
                    };
                    let sql = format!("SELECT {name} FROM scalar_rows WHERE id={id}");
                    values.push(if column.column.type_oid == 23 {
                        Spi::get_one::<i32>(&sql)
                            .expect("integer input")
                            .map_or(Value::Null(ScalarType::Int4), Value::Int4)
                    } else {
                        Spi::get_one::<bool>(&sql)
                            .expect("Boolean input")
                            .map_or(Value::Null(ScalarType::Bool), Value::Bool)
                    });
                }
                let interpreted = match evaluate(&lowered.program, &values) {
                    Ok(Value::Null(_)) => "ok:NULL".to_owned(),
                    Ok(Value::Int4(value)) => format!("ok:{value}"),
                    Ok(Value::Bool(value)) => format!("ok:{value}"),
                    Ok(Value::Date(_)) => panic!("this fixture has no DATE outputs"),
                    Err(crate::engine::spec::scalar::tests::Fault::Overflow) => {
                        "error:22003".to_owned()
                    }
                    Err(crate::engine::spec::scalar::tests::Fault::DivisionByZero) => {
                        "error:22012".to_owned()
                    }
                    Err(crate::engine::spec::scalar::tests::Fault::DateOutOfRange) => {
                        "error:22008".to_owned()
                    }
                };
                let native = Spi::get_one::<String>(&format!("SELECT pg_temp.scalar_try($scalar$SELECT {expression} FROM scalar_rows WHERE id={id}$scalar$)"))
                    .expect("native error oracle").expect("non-null outcome");
                assert_eq!(interpreted, native, "{expression}, row {id}");
            }
        }
    }

    #[pg_test]
    fn pg_scalar_lowering_rejects_unqualified_catalog_and_relation_shapes() {
        setup();
        Spi::run("CREATE DOMAIN pg_temp.scalar_positive AS int4 CHECK(VALUE>0); \
            CREATE FUNCTION pg_temp.scalar_fake_add(int4,int4) RETURNS int4 LANGUAGE plpgsql VOLATILE AS $$ BEGIN RETURN $1-$2; END $$; \
            CREATE OPERATOR pg_temp.+ (FUNCTION=pg_temp.scalar_fake_add,LEFTARG=int4,RIGHTARG=int4)")
            .expect("adversarial catalogs");
        for expression in [
            "w::int4",
            "a::int8",
            "(a::int2)::int4",
            "(a::pg_temp.scalar_positive)::int4",
            "a OPERATOR(pg_temp.+) b",
            "pg_temp.scalar_fake_add(a,b)",
            "CASE w WHEN 1 THEN b ELSE c END",
            "CASE a WHEN w THEN b ELSE c END",
            "a = ANY(ARRAY[b,c])",
            "a = ANY(ARRAY[1,2]::int8[])",
            "a < ANY(ARRAY[1,2])",
            "a = ALL(ARRAY[1,2])",
            "d = ANY(ARRAY[date '2026-01-02',date '2026-01-03'])",
            "scalar_rows IS NULL",
            "random()>0",
            "d::timestamp>timestamp '2026-01-02'",
            "a+(SELECT max(b) FROM scalar_rows)",
            "generate_series(1,a)",
        ] {
            assert!(
                lower(&format!("SELECT {expression} FROM scalar_rows")).is_err(),
                "must decline {expression}"
            );
        }
        assert!(
            lower("SELECT x.a+y.a FROM scalar_rows x CROSS JOIN scalar_rows y").is_err(),
            "same OID does not mean same occurrence"
        );
        assert!(
            lower("SELECT x.a+x.b FROM scalar_rows x CROSS JOIN scalar_rows y").is_ok(),
            "one occurrence's expression is representable; caller still rejects self join"
        );
        Spi::run("CREATE SCHEMA scalar_case_catalog; \
            CREATE FUNCTION scalar_case_catalog.eq(int4,int4) RETURNS bool LANGUAGE plpgsql AS $$ BEGIN RETURN $1<>$2; END $$; \
            CREATE OPERATOR scalar_case_catalog.= (FUNCTION=scalar_case_catalog.eq,LEFTARG=int4,RIGHTARG=int4); \
            SET search_path=scalar_case_catalog,pg_catalog,pg_temp")
            .expect("shadow simple CASE equality");
        assert!(
            lower("SELECT CASE a WHEN b THEN c ELSE 42 END FROM scalar_rows").is_err(),
            "simple CASE must not reinterpret user-defined equality as built-in equality"
        );
        assert!(
            lower("SELECT a = ANY(ARRAY[1,2,3]) FROM scalar_rows").is_err(),
            "membership must reject shadowed equality too"
        );
        assert!(
            lower("SELECT NULLIF(a,b) FROM scalar_rows").is_err(),
            "NULLIF must reject shadowed equality too"
        );
        Spi::run("RESET search_path").expect("restore catalog lookup");
    }

    #[pg_test]
    fn pg_scalar_membership_validates_hashed_operator_and_array_contracts() {
        setup();
        for sql in [
            "SELECT a IN (1,2,3,4,5,6,7,8,9,10) FROM scalar_rows",
            "SELECT a NOT IN (1,2,3,4,5,6,7,8,9,10) FROM scalar_rows",
        ] {
            assert!(
                lower_with(sql, |node| {
                    // SAFETY: lower_with supplies a live analyzed expression. The
                    // checked tag proves the exact scalar-array layout.
                    unsafe {
                        assert_eq!((*node).type_, pg_sys::NodeTag::T_ScalarArrayOpExpr);
                        assert_eq!(
                            u32::from((*node.cast::<pg_sys::ScalarArrayOpExpr>()).hashfuncid),
                            pg_sys::F_HASHINT4
                        );
                    }
                })
                .is_ok()
            );
            for field in 0..6 {
                assert!(
                    lower_with(sql, |node| {
                        // SAFETY: this private planner tree is test-owned; verify
                        // tags before mutating fields. The lowerer must reject the
                        // corrupted declaration before decoding its array Datum.
                        unsafe {
                            assert_eq!((*node).type_, pg_sys::NodeTag::T_ScalarArrayOpExpr);
                            let op = &mut *node.cast::<pg_sys::ScalarArrayOpExpr>();
                            let right = pg_sys::list_nth(op.args, 1).cast::<pg_sys::Const>();
                            assert_eq!((*right).xpr.type_, pg_sys::NodeTag::T_Const);
                            match field {
                                0 => op.opfuncid = pg_sys::Oid::from(pg_sys::F_INT4LT),
                                1 => op.hashfuncid = pg_sys::Oid::from(pg_sys::F_HASHINT8),
                                2 => op.negfuncid = pg_sys::Oid::from(pg_sys::F_INT4NE),
                                3 => (*right).constbyval = true,
                                4 => (*right).constlen = 4,
                                _ => (*right).consttype = pg_sys::INT8ARRAYOID,
                            }
                        }
                    })
                    .is_err(),
                    "malformed membership field {field}: {sql}"
                );
            }
        }
    }

    #[pg_test]
    fn pg_scalar_lowering_enforces_instruction_stack_and_input_bounds() {
        setup();
        let dense_members = (0..256)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(lower(&format!("SELECT a IN ({dense_members}) FROM scalar_rows")).is_ok());
        assert!(
            lower(&format!(
                "SELECT a IN ({dense_members},256) FROM scalar_rows"
            ))
            .is_err(),
            "array extraction remains bounded before compaction"
        );
        let expression = format!("-COALESCE({})", vec!["a"; 128].join(","));
        let exact =
            lower(&format!("SELECT {expression} FROM scalar_rows")).expect("256 instructions");
        assert_eq!(exact.program.instructions().len(), 256);
        let expression = format!("-COALESCE({})", vec!["a"; 129].join(","));
        assert!(lower(&format!("SELECT {expression} FROM scalar_rows")).is_err());
        let mut expression = "a".to_owned();
        for _ in 1..32 {
            expression = format!("a+({expression})");
        }
        assert_eq!(
            lower(&format!("SELECT {expression} FROM scalar_rows"))
                .expect("32 stack slots")
                .program
                .max_stack(),
            32
        );
        assert!(lower(&format!("SELECT a+({expression}) FROM scalar_rows")).is_err());
        Spi::run(&format!(
            "CREATE TEMP TABLE scalar_wide({})",
            (0..65)
                .map(|i| format!("c{i} int4"))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .expect("wide input fixture");
        fn balanced(columns: &[String]) -> String {
            if columns.len() == 1 {
                return columns[0].clone();
            }
            let middle = columns.len() / 2;
            format!(
                "({})+({})",
                balanced(&columns[..middle]),
                balanced(&columns[middle..])
            )
        }
        let columns = (0..65).map(|i| format!("c{i}")).collect::<Vec<_>>();
        assert_eq!(
            lower(&format!(
                "SELECT {} FROM scalar_wide",
                balanced(&columns[..64])
            ))
            .expect("64 inputs")
            .inputs
            .len(),
            64
        );
        assert!(lower(&format!("SELECT {} FROM scalar_wide", balanced(&columns))).is_err());
    }
}

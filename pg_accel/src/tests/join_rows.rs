//! Direct resident producer tests against PostgreSQL's join semantics. These
//! deliberately do not claim normal SQL planner selection of a row join.

#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    use crate::engine::executor::join::{
        ControlledJoinStageOutcome, JoinStageOutcome, ResidentJoinInputs,
    };
    use crate::engine::residency::{ExecutionMemoryCharge, resident_live_bytes};
    use crate::engine::spec::join::{
        JoinKey, JoinKind, JoinQuerySpec, OccurrenceColumn, RelationOccurrence,
    };
    use crate::gpu::ExprDeviceBuffer;
    use crate::gpu::join_project::{
        JoinProjectionColumn, JoinProjectionInput, JoinSide, ProjectedJoinColumn,
    };
    use crate::gpu::join_rows::{
        ControlledJoinCountOutcome, ControlledJoinOutcome, JOIN_NULL_ROW, JoinDispatchLimits,
        JoinRowPair, ResidentJoin, ResidentJoinKey, ResidentJoinKind, ResidentJoinOutcome,
        ResidentJoinOutput,
    };

    struct Keys {
        a: Option<ExprDeviceBuffer<i32>>,
        a_nulls: Option<ExprDeviceBuffer<u8>>,
        b: Option<ExprDeviceBuffer<i64>>,
        b_nulls: Option<ExprDeviceBuffer<u8>>,
        _charge: ExecutionMemoryCharge,
    }

    impl Keys {
        fn upload(table: &str, rows: &[(Option<i32>, Option<i64>)]) -> Self {
            Spi::run(&format!(
                "CREATE TEMP TABLE {table}(id int8, a int4, b int8)"
            ))
            .expect("join oracle table");
            for (index, (a, b)) in rows.iter().enumerate() {
                Spi::run(&format!(
                    "INSERT INTO {table} VALUES ({index},{},{})",
                    a.map_or_else(|| "NULL".into(), |x| x.to_string()),
                    b.map_or_else(|| "NULL".into(), |x| x.to_string()),
                ))
                .expect("join oracle fixture");
            }
            let charge = ExecutionMemoryCharge::reserve(
                (rows.len() * 14) as u64,
                crate::engine::gucs::resident_memory_budget_bytes(),
            )
            .expect("test input reservation");
            let a: Vec<_> = rows.iter().map(|r| r.0.unwrap_or_default()).collect();
            let b: Vec<_> = rows.iter().map(|r| r.1.unwrap_or_default()).collect();
            let a_nulls: Vec<_> = rows.iter().map(|r| u8::from(r.0.is_none())).collect();
            let b_nulls: Vec<_> = rows.iter().map(|r| u8::from(r.1.is_none())).collect();
            let result = Self {
                a: ExprDeviceBuffer::copy_from_slice(&a),
                a_nulls: ExprDeviceBuffer::copy_from_slice(&a_nulls),
                b: ExprDeviceBuffer::copy_from_slice(&b),
                b_nulls: ExprDeviceBuffer::copy_from_slice(&b_nulls),
                _charge: charge,
            };
            if !rows.is_empty() {
                assert!(result.a.is_some() && result.b.is_some());
                assert!(result.a_nulls.is_some() && result.b_nulls.is_some());
            }
            result
        }

        fn keys(&self) -> [ResidentJoinKey<'_>; 2] {
            [
                ResidentJoinKey::Int32 {
                    values: self.a.as_ref(),
                    nulls: self.a_nulls.as_ref(),
                },
                ResidentJoinKey::Int64 {
                    values: self.b.as_ref(),
                    nulls: self.b_nulls.as_ref(),
                },
            ]
        }
    }

    fn oracle(
        kind: ResidentJoinKind,
        left: &str,
        right: &str,
        composite: bool,
    ) -> Vec<JoinRowPair> {
        let equality = if composite {
            "l.a=r.a AND l.b=r.b"
        } else {
            "l.a=r.a"
        };
        let query = match kind {
            ResidentJoinKind::Inner => {
                format!("SELECT l.id,r.id FROM {left} l JOIN {right} r ON {equality}")
            }
            ResidentJoinKind::Left => {
                format!("SELECT l.id,r.id FROM {left} l LEFT JOIN {right} r ON {equality}")
            }
            ResidentJoinKind::Semi => format!(
                "SELECT l.id,NULL::int8 FROM {left} l WHERE EXISTS (SELECT FROM {right} r WHERE {equality})"
            ),
            ResidentJoinKind::Anti => format!(
                "SELECT l.id,NULL::int8 FROM {left} l WHERE NOT EXISTS (SELECT FROM {right} r WHERE {equality})"
            ),
        };
        Spi::connect(|client| {
            client
                .select(&query, None, &[])
                .expect("native join oracle")
                .map(|row| JoinRowPair {
                    outer_row: row.get::<i64>(1).expect("outer id").expect("nonnull id") as u64,
                    inner_row: row
                        .get::<i64>(2)
                        .expect("inner id")
                        .map_or(JOIN_NULL_ROW, |id| id as u64),
                })
                .collect()
        })
    }

    fn project_and_aggregate(
        mappings: &ResidentJoinOutput,
        outer: &Keys,
        inner: &Keys,
        kind: ResidentJoinKind,
        left: &str,
        right: &str,
        composite: bool,
    ) {
        use crate::engine::spec::abi;
        use crate::engine::spec::scalar::{ScalarInstruction as I, ScalarProgram, ScalarType};
        use crate::gpu::{
            GroupedAggKernelMode, GroupedAggOutputStorage, GroupedAggStateLane,
            ResolvedGroupedAggPlan, cleanup_grouped_allocation_pools,
            execute_grouped_agg_one_shot_with_mode,
        };
        let inputs = [
            JoinProjectionInput {
                side: JoinSide::Outer,
                column: JoinProjectionColumn::Int32 {
                    values: outer.a.as_ref(),
                    nulls: outer.a_nulls.as_ref(),
                },
            },
            JoinProjectionInput {
                side: JoinSide::Inner,
                column: JoinProjectionColumn::Int32 {
                    values: inner.a.as_ref(),
                    nulls: inner.a_nulls.as_ref(),
                },
            },
            JoinProjectionInput {
                side: JoinSide::Outer,
                column: JoinProjectionColumn::Int64 {
                    values: outer.b.as_ref(),
                    nulls: outer.b_nulls.as_ref(),
                },
            },
            JoinProjectionInput {
                side: JoinSide::Inner,
                column: JoinProjectionColumn::Int64 {
                    values: inner.b.as_ref(),
                    nulls: inner.b_nulls.as_ref(),
                },
            },
            JoinProjectionInput {
                side: JoinSide::Outer,
                column: JoinProjectionColumn::Byte {
                    values: outer.a_nulls.as_ref(),
                    nulls: None,
                },
            },
        ];
        let baseline = resident_live_bytes();
        assert!(mappings.project(&[]).is_err());
        assert!(mappings.project(&[inputs[0]; 17]).is_err());
        let row_domain = mappings.project_row_domain_controlled(&mut || false);
        let crate::gpu::join_project::ControlledProjectionOutcome::Complete(row_domain) =
            row_domain
        else {
            panic!("row domain");
        };
        assert_eq!(row_domain.len(), mappings.len());
        assert_eq!(row_domain.column_count(), 0);
        assert_eq!(row_domain.allocated_bytes(), 0);
        assert_eq!(row_domain.kernel_launches(), 0);
        drop(row_domain);
        assert!(matches!(
            mappings.project_row_domain_controlled(&mut || true),
            crate::gpu::join_project::ControlledProjectionOutcome::Interrupted {
                kernel_launches: 0,
                cancel_checks: 1
            }
        ));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            mappings.project_row_domain_controlled(&mut || panic!("domain boundary"))
        }));
        assert!(panic.is_err());
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "row-domain projection is allocation-free"
        );
        if outer.a.as_ref().map_or(0, ExprDeviceBuffer::len)
            != inner.a.as_ref().map_or(0, ExprDeviceBuffer::len)
        {
            let wrong_domain = JoinProjectionInput {
                side: JoinSide::Outer,
                column: inputs[1].column,
            };
            assert!(mappings.project(&[wrong_domain]).is_err());
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "bad domain rejects before reservation"
            );
        }
        let widest = mappings
            .project(&[inputs[0]; 16])
            .expect("maximum column-count projection");
        assert_eq!(widest.column_count(), 16);
        assert!(widest.column(15).is_some());
        assert!(widest.column(16).is_none());
        assert_eq!(
            resident_live_bytes(),
            baseline + (mappings.len() * 80) as u64
        );
        drop(widest);
        assert_eq!(resident_live_bytes(), baseline);
        let projection = mappings
            .project(&inputs)
            .expect("resident join column projection");
        assert_eq!(projection.column_count(), 5);
        assert_eq!(projection.len(), mappings.len());
        assert_eq!(projection.is_empty(), mappings.is_empty());
        assert!(projection.column(5).is_none());
        assert_eq!(
            resident_live_bytes(),
            baseline + (mappings.len() * 30) as u64
        );
        assert!(projection.is_empty() || projection.kernel_launches() > 0);
        let scalar_inputs = std::array::from_fn::<_, 2, _>(|index| {
            let ProjectedJoinColumn::Int32(column) =
                projection.column(index).expect("INT4 projection")
            else {
                panic!("INT4 output type");
            };
            abi::PgaccelScalarInput {
                values: column
                    .values()
                    .map_or(std::ptr::null(), |b| b.as_ptr().cast()),
                nulls: column
                    .nulls()
                    .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
                physical_type: abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
                reserved: 0,
            }
        });
        let words = ScalarProgram::new(
            vec![ScalarType::Int4; 2],
            ScalarType::Int4,
            vec![
                I::Input(0),
                I::Int4(0),
                I::Ge,
                I::JumpIfNotTrue(9),
                I::Input(0),
                I::Input(1),
                I::CoalesceInt4(10),
                I::Add,
                I::Jump(10),
                I::Null(ScalarType::Int4),
            ],
        )
        .expect("post-join aggregate FILTER program")
        .encode();
        let view = abi::PgaccelScalarView {
            version: abi::PGACCEL_SCALAR_VIEW_VERSION,
            word_count: words.len() as u32,
            input_count: 2,
            reserved: 0,
            row_count: projection.len() as u64,
            words: words.as_ptr(),
            inputs: scalar_inputs.as_ptr(),
        };
        // SAFETY: zero is valid for all ABI scalar/enum fields; active fields
        // are initialized below and inactive descriptor slots stay canonical.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
        desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
        desc.row_count = projection.len();
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
        desc.output_mode = abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
        desc.group_capacity = 1;
        desc.measure_count = 1;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        let measure = &mut desc.measures[0];
        measure.op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
        measure.agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        measure.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        measure.state_bytes = 8;
        measure.value.values = std::ptr::from_ref(&view).cast();
        measure.value.element_bytes = size_of::<abi::PgaccelScalarView>() as u32;
        measure.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        measure.value.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1;
        // SAFETY: projection retains the initialized resident value/NULL lanes
        // for the full plan/dispatch lifetime; view, words and bindings are live
        // and the facade snapshots host metadata. No row data is read on host.
        let plan =
            unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("post-join aggregate plan");
        let mut output =
            GroupedAggOutputStorage::new(&plan).expect("bounded final aggregate output");
        let (_, mode) = execute_grouped_agg_one_shot_with_mode(&plan, &mut output)
            .expect("join -> gather -> scalar aggregate");
        assert_eq!(mode, GroupedAggKernelMode::ParallelDenseInteger);
        let equality = if composite {
            "l.a=r.a AND l.b=r.b"
        } else {
            "l.a=r.a"
        };
        let native = match kind {
            ResidentJoinKind::Inner => format!(
                "SELECT l.a la,r.a ra,l.b lb,r.b rb FROM {left} l JOIN {right} r ON {equality}"
            ),
            ResidentJoinKind::Left => format!(
                "SELECT l.a la,r.a ra,l.b lb,r.b rb FROM {left} l LEFT JOIN {right} r ON {equality}"
            ),
            ResidentJoinKind::Semi => format!(
                "SELECT l.a la,NULL::int4 ra,l.b lb,NULL::int8 rb FROM {left} l WHERE EXISTS(SELECT FROM {right} r WHERE {equality})"
            ),
            ResidentJoinKind::Anti => format!(
                "SELECT l.a la,NULL::int4 ra,l.b lb,NULL::int8 rb FROM {left} l WHERE NOT EXISTS(SELECT FROM {right} r WHERE {equality})"
            ),
        };
        let expected_sum = Spi::get_one::<i64>(&format!(
            "SELECT coalesce(sum(la+coalesce(ra,10)) FILTER(WHERE la>=0),0)::int8 FROM ({native}) q"
        ))
        .expect("native sum")
        .expect("coalesced sum");
        let expected_count = Spi::get_one::<i64>(&format!(
            "SELECT count(la+coalesce(ra,10)) FILTER(WHERE la>=0) FROM ({native}) q"
        ))
        .expect("native count")
        .expect("count");
        assert_eq!(
            output
                .measure_i64_at(0, GroupedAggStateLane::Sum, 0)
                .expect("sum state"),
            expected_sum
        );
        assert_eq!(
            output.measure_nonnull_count(0).expect("nonnull count")[0],
            expected_count as u64
        );
        drop((output, plan));
        cleanup_grouped_allocation_pools();

        // Only oracle verification copies projected rows, after the complete
        // resident pipeline has executed. Compare exact INT8 extremes as well.
        let mut small = [Vec::new(), Vec::new()];
        let mut wide = [Vec::new(), Vec::new()];
        for (index, values) in small.iter_mut().enumerate() {
            let ProjectedJoinColumn::Int32(column) = projection.column(index).expect("column")
            else {
                panic!("int4");
            };
            let raw = column
                .values()
                .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final int4 copy"));
            let nulls = column
                .nulls()
                .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final null copy"));
            *values = raw
                .into_iter()
                .zip(nulls)
                .map(|(v, n)| {
                    assert!(n <= 1);
                    if n == 0 {
                        Some(v)
                    } else {
                        assert_eq!(v, 0);
                        None
                    }
                })
                .collect::<Vec<_>>();
        }
        for (index, values) in wide.iter_mut().enumerate() {
            let ProjectedJoinColumn::Int64(column) = projection.column(index + 2).expect("column")
            else {
                panic!("int8");
            };
            let raw = column
                .values()
                .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final int8 copy"));
            let nulls = column
                .nulls()
                .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final null copy"));
            *values = raw
                .into_iter()
                .zip(nulls)
                .map(|(v, n)| {
                    assert!(n <= 1);
                    if n == 0 {
                        Some(v)
                    } else {
                        assert_eq!(v, 0);
                        None
                    }
                })
                .collect::<Vec<_>>();
        }
        let ProjectedJoinColumn::Byte(bytes) = projection.column(4).expect("byte column") else {
            panic!("byte");
        };
        let raw_bytes = bytes
            .values()
            .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final byte copy"));
        let byte_nulls = bytes
            .nulls()
            .map_or_else(Vec::new, |b| b.copy_to_vec().expect("final byte nulls"));
        let mut actual = (0..projection.len())
            .map(|i| {
                assert_eq!(byte_nulls[i], 0);
                assert_eq!(raw_bytes[i], u8::from(small[0][i].is_none()));
                (small[0][i], small[1][i], wide[0][i], wide[1][i])
            })
            .collect::<Vec<_>>();
        let mut expected = Spi::connect(|client| {
            client
                .select(&native, None, &[])
                .expect("native projected rows")
                .map(|row| {
                    (
                        row.get::<i32>(1).expect("la"),
                        row.get::<i32>(2).expect("ra"),
                        row.get::<i64>(3).expect("lb"),
                        row.get::<i64>(4).expect("rb"),
                    )
                })
                .collect::<Vec<_>>()
        });
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
        drop(projection);
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "consumer and metadata ownership released"
        );
        if !mappings.is_empty() {
            let budget = crate::engine::gucs::resident_memory_budget_bytes();
            let held = ExecutionMemoryCharge::reserve(
                budget - baseline - crate::gpu::join_project::JOIN_PROJECT_WORKSPACE_BYTES as u64,
                budget,
            )
            .expect("leave only gather workspace");
            assert!(mappings.project(&inputs).is_err());
            drop(held);
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "partial gather reservation unwinds"
            );
        }
    }

    #[pg_test]
    fn pg_resident_join_occurrence_binding_and_invalidation() {
        Spi::run(
            "SET pg_accel.enabled=off; SET pg_accel.auto_load=on; \
            CREATE TABLE bound_jl(id int8,a int4,b int8,flag bool,day date,p int4); \
            INSERT INTO bound_jl VALUES \
            (10,1,-9223372036854775808,true,'2000-01-01',2), \
            (11,1,-9223372036854775808,NULL,'1999-12-31',1), \
            (12,2,9223372036854775807,false,NULL,1), \
            (13,NULL,7,true,'2030-02-03',NULL), \
            (14,8,NULL,false,'2024-02-29',8); \
            CREATE TABLE bound_jr (LIKE bound_jl); \
            INSERT INTO bound_jr VALUES \
            (90,4,-9223372036854775808,false,'2001-01-01',1), \
            (91,5,-9223372036854775808,true,NULL,1), \
            (92,6,9223372036854775807,NULL,'1999-12-31',2); \
            CREATE TABLE bound_je (LIKE bound_jl); \
            SELECT pg_accel_pin('bound_jl'); SELECT pg_accel_pin('bound_jr'); SELECT pg_accel_pin('bound_je')",
        )
        .expect("real residency fixtures");
        let oid = |table: &str| {
            u32::from(
                Spi::get_one::<pg_sys::Oid>(&format!("SELECT '{table}'::regclass::oid"))
                    .expect("OID lookup")
                    .expect("OID"),
            )
        };
        let make_spec = |left: &str, right: &str, kind, composite| {
            let col = |occurrence, attno, type_oid| OccurrenceColumn {
                occurrence,
                attno,
                type_oid,
            };
            let outer = RelationOccurrence {
                id: 3,
                relation_oid: oid(left),
            };
            let inner = RelationOccurrence {
                id: 9,
                relation_oid: oid(right),
            };
            let mut keys = vec![JoinKey {
                outer: col(3, 2, 23),
                inner: col(9, 6, 23),
            }];
            if composite {
                keys.push(JoinKey {
                    outer: col(3, 3, 20),
                    inner: col(9, 3, 20),
                });
            }
            let columns = if matches!(kind, JoinKind::Semi | JoinKind::Anti) {
                vec![
                    col(3, 1, 20),
                    col(3, 6, 23),
                    col(3, 4, 16),
                    col(3, 5, 1082),
                    col(3, 1, 20),
                ]
            } else {
                vec![
                    col(9, 1, 20),
                    col(3, 1, 20),
                    col(9, 6, 23),
                    col(3, 4, 16),
                    col(9, 5, 1082),
                    col(9, 1, 20),
                ]
            };
            JoinQuerySpec {
                outer,
                inner,
                kind,
                keys,
                columns,
            }
        };
        for (left, right) in [
            ("bound_jl", "bound_jr"),
            ("bound_jl", "bound_jl"),
            ("bound_jl", "bound_je"),
            ("bound_je", "bound_jr"),
            ("bound_je", "bound_je"),
        ] {
            for composite in [false, true] {
                for kind in [
                    JoinKind::Inner,
                    JoinKind::Left,
                    JoinKind::Semi,
                    JoinKind::Anti,
                ] {
                    let spec = make_spec(left, right, kind, composite);
                    // Real wire roundtrip, then load through the same selected-
                    // relation manager used by production aggregate execution.
                    let spec = JoinQuerySpec::decode(&spec.encode().expect("JQS1 encode"))
                        .expect("JQS1 decode");
                    let inputs = prepare_bound(spec.clone()).expect("prepare current inputs");
                    let expected = bound_oracle(left, right, kind, composite);
                    check_join_reduction(&inputs, left, right, kind, composite, expected.len());
                    check_duplicate_reduction(spec, left, right, composite, expected.len());
                    check_controlled_bound(
                        &inputs,
                        &expected,
                        matches!(kind, JoinKind::Inner | JoinKind::Left),
                    );
                    let baseline = resident_live_bytes();
                    if !expected.is_empty() {
                        let small = inputs
                            .with_bound(|bound| bound.execute(expected.len() - 1))
                            .expect("current input borrow")
                            .expect("capacity result");
                        let JoinStageOutcome::NeedsCapacity {
                            required_rows,
                            kernel_launches,
                        } = small
                        else {
                            panic!("no partial publication");
                        };
                        assert_eq!(required_rows, expected.len() as u64);
                        assert!(kernel_launches > 0);
                        assert_eq!(resident_live_bytes(), baseline);
                    }
                    let output = bound_execute(&inputs, expected.len());
                    assert_eq!(
                        bound_rows(&output),
                        expected,
                        "{left} {right} {kind:?} composite={composite}"
                    );
                    drop(output);
                    assert_eq!(
                        resident_live_bytes(),
                        baseline,
                        "pair, gather and scratch charges release"
                    );
                }
            }
        }
        let spec = make_spec("bound_jl", "bound_jl", JoinKind::Left, true);
        let prepared = prepare_bound(spec.clone()).expect("self join prepared");
        let dependencies = spec
            .physical_columns()
            .expect("canonical physical dependencies");
        assert!(
            dependencies
                .iter()
                .all(|column| column.relation_oid == spec.outer.relation_oid)
        );
        let retained = bound_execute(&prepared, 100);
        let retained_rows = bound_rows(&retained);
        let (changed, updated) = rolled_back(|| {
            Spi::run("UPDATE bound_jl SET p=2,b=7 WHERE id=11").expect("invalidate both aliases");
            assert_stale_bound(&prepared);
            let changed = prepare_bound(spec.clone()).expect("reprepare after update");
            let updated = bound_execute(&changed, 100);
            assert_eq!(
                bound_rows(&updated),
                bound_oracle("bound_jl", "bound_jl", JoinKind::Left, true)
            );
            assert_ne!(bound_rows(&updated), retained_rows);
            assert_eq!(
                bound_rows(&retained),
                retained_rows,
                "output owns its gathered data after input invalidation"
            );
            (changed, updated)
        });
        assert_stale_bound(&changed);
        let restored = prepare_bound(spec.clone()).expect("reprepare after rollback");
        assert_eq!(bound_rows(&bound_execute(&restored, 100)), retained_rows);
        Spi::run("SELECT pg_accel_refresh('bound_jl')").expect("refresh both aliases once");
        assert_eq!(
            bound_rows(&bound_execute(&restored, 100)),
            retained_rows,
            "same-generation refresh rebinds current buffers rather than retaining pointers"
        );
        let mut wrong_type = spec.clone();
        wrong_type.columns.retain(|column| column.attno != 5);
        wrong_type.columns.push(OccurrenceColumn {
            occurrence: 3,
            attno: 5,
            type_oid: 23,
        });
        assert!(
            prepare_bound(wrong_type).is_err(),
            "DATE storage cannot silently become INT4"
        );
        let fresh = prepare_bound(spec.clone()).expect("fresh before eviction");
        Spi::run("SELECT pg_accel_unpin('bound_jl'); SELECT pg_accel_evict('bound_jl'); SET pg_accel.auto_load=off")
            .expect("evict source");
        assert_stale_bound(&fresh);
        assert!(
            prepare_bound(spec.clone()).is_err(),
            "no hidden reload with auto_load off"
        );
        Spi::run("SET pg_accel.auto_load=on; TRUNCATE bound_jl").expect("empty source");
        let empty = prepare_bound(spec).expect("typed empty binding");
        assert!(bound_execute(&empty, 0).is_empty());
        drop((retained, updated));
        Spi::run("SELECT pg_accel_evict('bound_jl'); SELECT pg_accel_unpin('bound_jr'); SELECT pg_accel_unpin('bound_je'); SELECT pg_accel_evict('bound_jr'); SELECT pg_accel_evict('bound_je'); DROP TABLE bound_jl,bound_jr,bound_je").expect("fixture cleanup");
    }

    fn check_duplicate_reduction(
        mut join: JoinQuerySpec,
        left: &str,
        right: &str,
        composite: bool,
        joined_rows: usize,
    ) {
        use crate::engine::executor::join_reduce::{JoinReducedOutcome, JoinReductionState};
        use crate::engine::spec::join_reduce::{JoinReduction, JoinReductionSpec};
        use crate::engine::spec::scalar::{
            ScalarInstruction as I, ScalarProgram, ScalarType::Int4,
        };
        let existence = matches!(join.kind, JoinKind::Semi | JoinKind::Anti);
        let value = join.columns[if existence { 1 } else { 2 }];
        join.columns = vec![
            value,
            value,
            OccurrenceColumn {
                occurrence: join.outer.id,
                attno: 6,
                type_oid: 23,
            },
        ];
        let kind = join.kind;
        let inputs = prepare_bound(join).expect("duplicate logical projection");
        let reduction = JoinReductionSpec {
            row_filter: None,
            measures: vec![JoinReduction {
                inputs: vec![0, 1, 2],
                program: ScalarProgram::new(
                    vec![Int4; 3],
                    Int4,
                    vec![I::Input(0), I::Input(1), I::Add, I::Input(2), I::Sub],
                )
                .expect("distinct alias arithmetic"),
            }],
        };
        let on = if composite {
            "l.a=r.p AND l.b=r.b"
        } else {
            "l.a=r.p"
        };
        let source = if existence {
            format!(
                "FROM {left} l WHERE {}EXISTS (SELECT FROM {right} r WHERE {on})",
                if kind == JoinKind::Anti { "NOT " } else { "" }
            )
        } else {
            format!(
                "FROM {left} l {} JOIN {right} r ON {on}",
                if kind == JoinKind::Left {
                    "LEFT"
                } else {
                    "INNER"
                }
            )
        };
        let value = if existence { "l.p" } else { "r.p" };
        let query =
            format!("SELECT sum({value}+{value}-l.p)::int8,count({value}+{value}-l.p) {source}");
        let expected = Spi::connect(|client| {
            let row = client
                .select(&query, None, &[])
                .expect("native duplicate alias reduction")
                .first();
            JoinReductionState {
                sum: row.get::<i64>(1).expect("SUM"),
                nonnull_count: row.get::<i64>(2).expect("COUNT").expect("nonnull") as u64,
            }
        });
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let JoinReducedOutcome::Complete(output) = inputs
            .execute_reduced(
                &reduction,
                joined_rows,
                JoinDispatchLimits {
                    work_items: 2,
                    chain_steps: 1,
                },
                2,
            )
            .expect("deduplicated gather")
        else {
            panic!("exact capacity");
        };
        assert_eq!(output.states[0], expected, "{query}");
        assert_eq!(output.projected_columns, if existence { 1 } else { 2 });
        assert_eq!(
            output.projected_bytes,
            joined_rows * if existence { 5 } else { 10 }
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
    }

    fn check_join_reduction(
        inputs: &ResidentJoinInputs,
        left: &str,
        right: &str,
        kind: JoinKind,
        composite: bool,
        joined_rows: usize,
    ) {
        use crate::engine::executor::join_reduce::{JoinReducedOutcome, JoinReductionState};
        use crate::engine::spec::join_reduce::{JoinReduction, JoinReductionSpec, JoinRowFilter};
        use crate::engine::spec::scalar::{
            ScalarInstruction as I, ScalarProgram,
            ScalarType::{Bool, Date, Int4},
        };
        let existence = matches!(kind, JoinKind::Semi | JoinKind::Anti);
        let (value_slot, bool_slot, date_slot) = if existence { (1, 2, 3) } else { (2, 3, 4) };
        let spec = JoinReductionSpec {
            row_filter: None,
            measures: vec![
                JoinReduction {
                    inputs: vec![value_slot],
                    program: ScalarProgram::new(vec![Int4], Int4, vec![I::Input(0)])
                        .expect("value"),
                },
                JoinReduction {
                    inputs: vec![],
                    program: ScalarProgram::new(vec![], Int4, vec![I::Int4(1)])
                        .expect("count star"),
                },
                JoinReduction {
                    inputs: vec![bool_slot, value_slot],
                    program: ScalarProgram::new(
                        vec![Bool, Int4],
                        Int4,
                        vec![
                            I::Input(0),
                            I::JumpIfNotTrue(4),
                            I::Input(1),
                            I::Jump(5),
                            I::Null(Int4),
                        ],
                    )
                    .expect("FILTER"),
                },
                JoinReduction {
                    inputs: vec![date_slot],
                    program: ScalarProgram::new(
                        vec![Date],
                        Int4,
                        vec![I::Input(0), I::Date(0), I::DateDifference],
                    )
                    .expect("date difference"),
                },
            ],
        };
        let spec =
            JoinReductionSpec::decode(&spec.encode().expect("JRS1 encode")).expect("JRS1 decode");
        let on = if composite {
            "l.a=r.p AND l.b=r.b"
        } else {
            "l.a=r.p"
        };
        let source = match kind {
            JoinKind::Inner | JoinKind::Left => format!(
                "FROM {left} l {} JOIN {right} r ON {on}",
                if kind == JoinKind::Left {
                    "LEFT"
                } else {
                    "INNER"
                }
            ),
            JoinKind::Semi | JoinKind::Anti => format!(
                "FROM {left} l WHERE {}EXISTS (SELECT FROM {right} r WHERE {on})",
                if kind == JoinKind::Anti { "NOT " } else { "" }
            ),
        };
        let value = if existence { "l.p" } else { "r.p" };
        let day = if existence { "l.day" } else { "r.day" };
        let query = format!(
            "SELECT sum({value})::int8,count({value}),sum(1)::int8,count(*),sum({value}) FILTER(WHERE l.flag)::int8,count({value}) FILTER(WHERE l.flag),sum({day}-'2000-01-01'::date)::int8,count({day}) {source}"
        );
        let expected = Spi::connect(|client| {
            let row = client
                .select(&query, None, &[])
                .expect("native joined aggregate")
                .first();
            std::array::from_fn::<_, 4, _>(|index| JoinReductionState {
                sum: row.get::<i64>(1 + index * 2).expect("native SUM"),
                nonnull_count: row
                    .get::<i64>(2 + index * 2)
                    .expect("native COUNT")
                    .expect("COUNT nonnull") as u64,
            })
        });
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let limits = JoinDispatchLimits {
            work_items: 2,
            chain_steps: 1,
        };
        let JoinReducedOutcome::Complete(output) = inputs
            .execute_reduced(&spec, joined_rows, limits, 2)
            .expect("resident join -> scalar aggregate pipeline")
        else {
            panic!("exact join capacity");
        };
        assert_eq!(output.measure_count, 4);
        assert_eq!(output.joined_rows, joined_rows);
        assert_eq!(output.selected_rows, joined_rows as u64);
        assert_eq!(output.projected_columns, 3);
        assert_eq!(output.projected_bytes, joined_rows * 12);
        assert_eq!(output.aggregate_calls, joined_rows.div_ceil(2) + 1);
        assert!(joined_rows == 0 || output.join_kernel_launches > 0);
        assert_eq!(
            output.states, expected,
            "{left} {right} {kind:?} composite={composite}"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "producer/consumer and pooled charges released"
        );
        if joined_rows != 0 {
            let JoinReducedOutcome::NeedsCapacity {
                required_rows,
                kernel_launches,
            } = inputs
                .execute_reduced(&spec, joined_rows - 1, limits, 2)
                .expect("capacity detection before consumer")
            else {
                panic!("cannot truncate aggregate input");
            };
            assert_eq!(required_rows, joined_rows as u64);
            assert!(kernel_launches > 0);
            assert_eq!(resident_live_bytes(), baseline);
        }
        // WHERE follows join null-extension and precedes every measure/FILTER.
        // SEMI/ANTI are expressed by native EXISTS only in the independent oracle.
        let filters = [
            (
                "l.flag".to_owned(),
                vec![bool_slot],
                vec![Bool],
                vec![I::Input(0)],
            ),
            (
                format!("{value} IS NULL"),
                vec![value_slot],
                vec![Int4],
                vec![I::Input(0), I::IsNull],
            ),
            (
                format!("l.flag AND {value}>1"),
                vec![bool_slot, value_slot],
                vec![Bool, Int4],
                vec![I::Input(0), I::Input(1), I::Int4(1), I::Gt, I::And],
            ),
            (
                format!("{day}>'2000-01-01'::date"),
                vec![date_slot],
                vec![Date],
                vec![I::Input(0), I::Date(0), I::Gt],
            ),
            ("true".to_owned(), vec![], vec![], vec![I::Bool(true)]),
            ("false".to_owned(), vec![], vec![], vec![I::Bool(false)]),
            ("NULL::bool".to_owned(), vec![], vec![], vec![I::Null(Bool)]),
            (
                format!("CASE WHEN l.flag THEN {value}>1 ELSE false END"),
                vec![bool_slot, value_slot],
                vec![Bool, Int4],
                vec![
                    I::Input(0),
                    I::JumpIfNotTrue(6),
                    I::Input(1),
                    I::Int4(1),
                    I::Gt,
                    I::Jump(7),
                    I::Bool(false),
                ],
            ),
        ];
        for (predicate, slots, types, instructions) in filters {
            let filtered = JoinReductionSpec {
                measures: spec.measures[..3].to_vec(),
                row_filter: Some(JoinRowFilter {
                    inputs: slots,
                    program: ScalarProgram::new(types, Bool, instructions).expect("WHERE program"),
                }),
            };
            let filtered = JoinReductionSpec::decode(&filtered.encode().expect("filtered wire"))
                .expect("filtered decode");
            let query = format!(
                "SELECT sum({value})::int8,count({value}),sum(1)::int8,count(*),sum({value}) FILTER(WHERE l.flag)::int8,count({value}) FILTER(WHERE l.flag) {source} {} ({predicate})",
                if existence { "AND" } else { "WHERE" }
            );
            let expected = Spi::connect(|client| {
                let row = client
                    .select(&query, None, &[])
                    .expect("native WHERE aggregate")
                    .first();
                std::array::from_fn::<_, 4, _>(|index| {
                    if index == 3 {
                        return JoinReductionState::default();
                    }
                    JoinReductionState {
                        sum: row.get::<i64>(1 + index * 2).expect("filtered SUM"),
                        nonnull_count: row
                            .get::<i64>(2 + index * 2)
                            .expect("filtered COUNT")
                            .expect("nonnull") as u64,
                    }
                })
            });
            let JoinReducedOutcome::Complete(output) = inputs
                .execute_reduced(&filtered, joined_rows, limits, 2)
                .expect("resident join/WHERE/aggregate")
            else {
                panic!("exact capacity with WHERE");
            };
            assert_eq!(output.measure_count, 3);
            assert_eq!(output.joined_rows, joined_rows);
            assert_eq!(output.selected_rows, expected[1].nonnull_count);
            let needs_date = filtered
                .row_filter
                .as_ref()
                .expect("WHERE")
                .program
                .inputs()
                .contains(&Date);
            assert_eq!(output.projected_columns, if needs_date { 3 } else { 2 });
            assert_eq!(
                output.projected_bytes,
                joined_rows * if needs_date { 12 } else { 7 }
            );
            assert_eq!(output.states, expected, "{query}");
            assert_eq!(output.aggregate_calls, joined_rows.div_ceil(2) + 1);
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "WHERE does not retain charges"
            );
        }
        // Filtering must prevent evaluation of a rejected row's dangerous
        // measure. A predicate error must propagate, not silently drop a row.
        let mut faulting = JoinReductionSpec {
            measures: vec![JoinReduction {
                inputs: vec![],
                program: ScalarProgram::new(vec![], Int4, vec![I::Int4(1), I::Int4(0), I::Div])
                    .expect("faulting value"),
            }],
            row_filter: Some(JoinRowFilter {
                inputs: vec![],
                program: ScalarProgram::new(vec![], Bool, vec![I::Bool(false)])
                    .expect("reject all"),
            }),
        };
        for unknown in [false, true] {
            faulting.row_filter.as_mut().expect("filter").program = ScalarProgram::new(
                vec![],
                Bool,
                vec![if unknown {
                    I::Null(Bool)
                } else {
                    I::Bool(false)
                }],
            )
            .expect("rejecting predicate");
            let JoinReducedOutcome::Complete(output) = inputs
                .execute_reduced(&faulting, joined_rows, limits, 2)
                .expect("WHERE guards measure")
            else {
                panic!("capacity");
            };
            assert_eq!(output.selected_rows, 0);
            assert_eq!(output.projected_columns, 0);
            assert_eq!(output.projected_bytes, 0);
            assert_eq!(output.states, [JoinReductionState::default(); 4]);
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(resident_live_bytes(), baseline);
        }
        // These capacities must never allocate pairs: even zero suffices and
        // usize::MAX must not overflow a nonexistent row-pair allocation.
        for (measures, capacity) in [(1, 0), (2, joined_rows), (4, usize::MAX)] {
            let constant = JoinReductionSpec {
                measures: vec![spec.measures[1].clone(); measures],
                row_filter: None,
            };
            let before = crate::gpu::gpu_exec_count();
            let failures = crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin);
            let statuses = crate::gpu::bridge::test_join_status_counts();
            let JoinReducedOutcome::Complete(output) = inputs
                .execute_reduced(&constant, capacity, limits, 2)
                .expect("device cardinality without pair production")
            else {
                panic!("cardinality has no pair capacity requirement");
            };
            assert_eq!(output.projected_columns, 0);
            assert_eq!(output.projected_bytes, 0);
            assert_eq!(output.aggregate_calls, 0);
            assert_eq!(
                output.join_kernel_launches,
                crate::gpu::gpu_exec_count() - before
            );
            let after = crate::gpu::bridge::test_join_status_counts();
            assert_eq!(after.oom, statuses.oom, "COUNT has no pair-capacity OOM");
            assert_eq!(after.other, statuses.other, "COUNT has no other failure");
            let retries = after.timeouts - statuses.timeouts;
            assert_eq!(after.count_attempts - statuses.count_attempts, 1 + retries);
            assert_eq!(
                crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin) - failures,
                retries,
                "benign interruptions remain counted as non-OK native statuses"
            );
            assert_eq!(output.selected_rows, joined_rows as u64);
            assert_eq!(output.states[..measures], vec![expected[1]; measures]);
            assert_eq!(output.measure_count, measures);
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(resident_live_bytes(), baseline);
        }
        // The initial native poll precedes all kernels. Inject one interrupt,
        // then account for every retry, including independently arriving benign
        // PostgreSQL interrupts and empty-input counts.
        let statuses = crate::gpu::bridge::test_join_status_counts();
        let failures = crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin);
        let kernels = crate::gpu::gpu_exec_count();
        let mut polls = 0;
        let mut injection_writes = 0;
        let mut injection_readback = 0;
        let (rows, launches) = inputs
            .execute_cardinality_test_hook(limits, || {
                polls += 1;
                if polls == 1 {
                    // Native callback: no PostgreSQL calls or longjmp here.
                    unsafe {
                        std::ptr::write_volatile(&raw mut pgrx::pg_sys::InterruptPending, 1);
                        injection_writes += 1;
                        injection_readback =
                            std::ptr::read_volatile(&raw const pgrx::pg_sys::InterruptPending);
                    }
                }
            })
            .expect("benign interrupt retries exact device cardinality");
        let after = crate::gpu::bridge::test_join_status_counts();
        let attempts = after.count_attempts - statuses.count_attempts;
        let retries = after.timeouts - statuses.timeouts;
        let failure_delta =
            crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin) - failures;
        let kernel_delta = crate::gpu::gpu_exec_count() - kernels;
        let receipt = format!(
            "before={statuses:?} after={after:?} injection_writes={injection_writes} \
             injection_readback={injection_readback} polls={polls} rows={rows} \
             expected_rows={joined_rows} launches={launches} kernel_delta={kernel_delta} \
             failure_delta={failure_delta}"
        );
        assert_eq!(injection_writes, 1, "{receipt}");
        assert_eq!(injection_readback, 1, "{receipt}");
        assert_eq!(rows, joined_rows as u64, "{receipt}");
        assert!(retries >= 1, "{receipt}");
        assert_eq!(attempts, retries + 1, "{receipt}");
        assert_eq!(after.oom, statuses.oom, "{receipt}");
        assert_eq!(after.other, statuses.other, "{receipt}");
        assert_eq!(
            failure_delta, retries,
            "successful retry records every interrupted native attempt: {receipt}"
        );
        assert_eq!(launches, kernel_delta, "{receipt}");
        assert_eq!(
            polls,
            launches + attempts,
            "one initial poll per native attempt: {receipt}"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
        if joined_rows != 0 {
            faulting.measures = vec![spec.measures[1].clone()];
            faulting.row_filter.as_mut().expect("filter").program = ScalarProgram::new(
                vec![],
                Bool,
                vec![I::Int4(1), I::Int4(0), I::Div, I::Int4(1), I::Eq],
            )
            .expect("faulting predicate");
            let error = inputs
                .execute_reduced(&faulting, joined_rows, limits, 2)
                .expect_err("WHERE division must not disappear");
            let crate::engine::executor::join::JoinExecutionError::Device(error) = error else {
                panic!("expected device expression error");
            };
            assert_eq!(error.status, crate::gpu::GpuStatusDetail::DivisionByZero);
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "failed WHERE drops joined columns and session"
            );
            assert!(matches!(
                inputs
                    .execute_reduced(&spec, joined_rows, limits, 2)
                    .expect("retry after failed WHERE"),
                JoinReducedOutcome::Complete(_)
            ));
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(resident_live_bytes(), baseline);
        }
        let mut wrong_type = spec.clone();
        wrong_type.measures[0].inputs[0] = 0; // INT8 identity is not an INT4 value.
        assert!(
            inputs
                .execute_reduced(&wrong_type, joined_rows, limits, 2)
                .is_err()
        );
        assert!(
            inputs
                .execute_reduced(&spec, joined_rows, limits, 0)
                .is_err()
        );
        assert!(
            inputs
                .execute_reduced(&spec, joined_rows, limits, usize::MAX)
                .is_err()
        );
        assert_eq!(resident_live_bytes(), baseline);
    }

    fn check_controlled_bound(
        inputs: &ResidentJoinInputs,
        expected: &[Vec<Option<i64>>],
        stable_launch_count: bool,
    ) {
        let baseline = resident_live_bytes();
        let limits = JoinDispatchLimits {
            work_items: 2,
            chain_steps: 1,
        };
        let mut calls = 0_u64;
        let completed = inputs
            .with_bound(|bound| {
                bound.execute_controlled(expected.len(), limits, &mut || {
                    calls += 1;
                    false
                })
            })
            .expect("bounded current input borrow")
            .expect("bounded join/gather");
        let ControlledJoinStageOutcome::Finished {
            outcome:
                JoinStageOutcome::Complete {
                    columns,
                    kernel_launches,
                },
            cancel_checks,
        } = completed
        else {
            panic!("exact capacity must complete both stages");
        };
        assert_eq!(cancel_checks, calls);
        assert_eq!(cancel_checks, kernel_launches + 2);
        assert_eq!(bound_rows(&columns), expected);
        let projection_launches = columns.kernel_launches();
        drop(columns);
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "only pinned source charges survive"
        );

        if stable_launch_count {
            check_postgres_join_interrupts(inputs, expected, limits, kernel_launches, calls);
        }

        // Full-chain joins have insertion-order-independent launch counts.
        // Cancel at the stage transition, during projection, and at its final
        // boundary after every output byte may already have been written.
        let stops = if stable_launch_count {
            vec![
                1,
                calls - projection_launches,
                calls.saturating_sub(1).max(1),
                calls,
            ]
        } else {
            vec![1]
        };
        for stop in stops {
            let mut checks = 0;
            let cancelled = inputs
                .with_bound(|bound| {
                    bound.execute_controlled(expected.len(), limits, &mut || {
                        checks += 1;
                        checks == stop
                    })
                })
                .expect("input borrow after cancellation")
                .expect("stage cancellation");
            let ControlledJoinStageOutcome::Interrupted {
                kernel_launches,
                cancel_checks,
            } = cancelled
            else {
                panic!("interrupted pipeline cannot publish columns");
            };
            assert_eq!(cancel_checks, stop);
            assert_eq!(checks, stop);
            assert!(cancel_checks == kernel_launches + 1 || cancel_checks == kernel_launches + 2);
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "pair, scratch and column allocations released"
            );
        }
        if stable_launch_count {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut checks = 0;
                let _ = inputs.with_bound(|bound| {
                    bound.execute_controlled(expected.len(), limits, &mut || {
                        checks += 1;
                        if checks == calls {
                            std::panic::panic_any(113_u32);
                        }
                        false
                    })
                });
            }))
            .expect_err("panic after gather resumes outside native");
            assert_eq!(panic.downcast_ref::<u32>(), Some(&113));
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "panic releases both stages and store borrow"
            );
        }
        if !expected.is_empty() {
            let small = inputs
                .with_bound(|bound| {
                    bound.execute_controlled(expected.len() - 1, limits, &mut || false)
                })
                .expect("current inputs")
                .expect("bounded insufficient capacity");
            let ControlledJoinStageOutcome::Finished {
                outcome:
                    JoinStageOutcome::NeedsCapacity {
                        required_rows,
                        kernel_launches,
                    },
                cancel_checks,
            } = small
            else {
                panic!("capacity failure must not gather");
            };
            assert_eq!(required_rows, expected.len() as u64);
            assert_eq!(
                cancel_checks,
                kernel_launches + 1,
                "only the producer was entered"
            );
            assert_eq!(resident_live_bytes(), baseline);
        }
        let retry = inputs
            .with_bound(|bound| bound.execute_controlled(expected.len(), limits, &mut || false))
            .expect("borrow recovered after unwind")
            .expect("bounded retry");
        let ControlledJoinStageOutcome::Finished {
            outcome: JoinStageOutcome::Complete { columns, .. },
            ..
        } = retry
        else {
            panic!("retry must complete");
        };
        assert_eq!(bound_rows(&columns), expected);
        drop(columns);
        assert_eq!(resident_live_bytes(), baseline);
        let JoinStageOutcome::Complete { columns, .. } = inputs
            .execute_interruptible(expected.len(), limits)
            .expect("PostgreSQL-aware bounded executor")
        else {
            panic!("exact capacity must complete after interrupt boundaries");
        };
        assert_eq!(bound_rows(&columns), expected);
        drop(columns);
        assert_eq!(resident_live_bytes(), baseline);
    }

    fn check_postgres_join_interrupts(
        inputs: &ResidentJoinInputs,
        expected: &[Vec<Option<i64>>],
        limits: JoinDispatchLimits,
        uninterrupted_launches: u64,
        uninterrupted_checks: u64,
    ) {
        let baseline = resident_live_bytes();
        for stop in [1, uninterrupted_checks] {
            let mut checks = 0;
            // The mutable counter is diagnostic-only and deliberately inspected
            // after the captured PG error; it owns no resource or borrow guard.
            let cancelled = pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                let _ = inputs
                    .execute_interruptible_test_hook(expected.len(), limits, || {
                        checks += 1;
                        if checks == stop {
                            // SAFETY: test-only backend-main-thread injection of
                            // PostgreSQL's own user-cancel signal flags. No PG API
                            // runs under the native callback/store borrow. The real
                            // outer ProcessInterrupts boundary consumes both flags.
                            unsafe {
                                std::ptr::write_volatile(&raw mut pg_sys::QueryCancelPending, 1);
                                std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
                            }
                        }
                    })
                    .expect("dispatch before PostgreSQL cancellation");
                false
            }))
            .catch_when(PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |caught| {
                let message = match caught {
                    pg_sys::panic::CaughtError::PostgresError(report)
                    | pg_sys::panic::CaughtError::ErrorReport(report) => {
                        report.message().to_owned()
                    }
                    pg_sys::panic::CaughtError::RustPanic { ereport, .. } => {
                        ereport.message().to_owned()
                    }
                };
                assert_eq!(message, "canceling statement due to user request");
                assert_eq!(
                    resident_live_bytes(),
                    baseline,
                    "all ephemeral owners dropped before rethrow"
                );
                true
            })
            .execute();
            assert!(cancelled);
            assert_eq!(checks, stop);
            assert_eq!(resident_live_bytes(), baseline);
        }

        // A bare interrupt wake-up is deliberately non-error: PostgreSQL clears
        // it and returns. Rebind and restart rather than inventing SQLSTATE 57014.
        let mut checks = 0;
        let JoinStageOutcome::Complete {
            columns,
            kernel_launches,
        } = inputs
            .execute_interruptible_test_hook(expected.len(), limits, || {
                checks += 1;
                if checks == uninterrupted_checks {
                    // SAFETY: same single-threaded test signal injection as
                    // above, without QueryCancelPending or any PostgreSQL call.
                    unsafe {
                        std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
                    }
                }
            })
            .expect("benign interrupt retries resident execution")
        else {
            panic!("exact capacity after benign interrupt");
        };
        assert_eq!(checks, uninterrupted_checks * 2);
        assert_eq!(
            kernel_launches,
            uninterrupted_launches * 2,
            "include discarded attempt work"
        );
        assert_eq!(bound_rows(&columns), expected);
        drop(columns);
        assert_eq!(resident_live_bytes(), baseline);
        // This also proves the store remains usable after actual PG errors.
        let JoinStageOutcome::Complete { columns, .. } = inputs
            .execute_interruptible(expected.len(), limits)
            .expect("retry after native PostgreSQL errors")
        else {
            panic!("retry capacity");
        };
        assert_eq!(bound_rows(&columns), expected);
        drop(columns);
        assert_eq!(resident_live_bytes(), baseline);
    }

    fn prepare_bound(
        spec: JoinQuerySpec,
    ) -> Result<ResidentJoinInputs, crate::engine::residency::ResidentLoadError> {
        struct SnapshotGuard;
        impl Drop for SnapshotGuard {
            fn drop(&mut self) {
                // SAFETY: this helper pushes exactly one copied snapshot.
                unsafe { pg_sys::PopActiveSnapshot() };
            }
        }
        // SAFETY: the pg_test function's outer snapshot predates its SPI DML.
        // Like direct loader tests, update a private copy to the current SPI
        // command ID; never mutate the caller's snapshot or production loading.
        unsafe {
            pg_sys::PushCopiedSnapshot(pg_sys::GetActiveSnapshot());
            pg_sys::UpdateActiveSnapshotCommandId();
        }
        let _snapshot = SnapshotGuard;
        ResidentJoinInputs::prepare(spec)
    }

    fn rolled_back<R>(body: impl FnOnce() -> R) -> R {
        // SAFETY: backend main-thread test, mirroring PL/pgSQL's internal
        // subtransaction context/owner restoration. Returned test values own
        // Rust/device memory, not allocations in the aborted PG context.
        unsafe {
            let old_context = pg_sys::CurrentMemoryContext;
            let old_owner = pg_sys::CurrentResourceOwner;
            pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pg_sys::MemoryContextSwitchTo(old_context);
            let result = body();
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(old_context);
            pg_sys::CurrentResourceOwner = old_owner;
            result
        }
    }

    fn assert_stale_bound(inputs: &ResidentJoinInputs) {
        let mut called = false;
        let result = inputs.with_bound(|_| {
            called = true;
        });
        assert!(
            result.is_err(),
            "stale input must fail before exposing device buffers"
        );
        assert!(!called);
    }

    fn bound_execute(
        inputs: &ResidentJoinInputs,
        capacity: usize,
    ) -> crate::gpu::join_project::ResidentJoinProjection {
        let result = inputs
            .with_bound(|bound| bound.execute(capacity))
            .expect("current inputs")
            .expect("join/gather dispatch");
        let JoinStageOutcome::Complete {
            columns,
            kernel_launches,
        } = result
        else {
            panic!("bounded fixture capacity");
        };
        // Empty sides still use native control/status paths; nonempty results
        // require actual join and projection kernels, not merely binding.
        if !columns.is_empty() {
            assert!(kernel_launches > 0);
        }
        columns
    }

    fn bound_rows(
        output: &crate::gpu::join_project::ResidentJoinProjection,
    ) -> Vec<Vec<Option<i64>>> {
        let mut rows = vec![vec![None; output.column_count()]; output.len()];
        for index in 0..output.column_count() {
            let (values, nulls) = match output.column(index).expect("consumer lane") {
                ProjectedJoinColumn::Int32(column) => (
                    copy_bound_values(column.values(), output.len())
                        .into_iter()
                        .map(i64::from)
                        .collect::<Vec<_>>(),
                    column.nulls(),
                ),
                ProjectedJoinColumn::Int64(column) => (
                    copy_bound_values(column.values(), output.len()),
                    column.nulls(),
                ),
                ProjectedJoinColumn::Byte(column) => (
                    copy_bound_values(column.values(), output.len())
                        .into_iter()
                        .map(i64::from)
                        .collect::<Vec<_>>(),
                    column.nulls(),
                ),
            };
            let nulls = copy_bound_values(nulls, output.len());
            for (row, values_row) in rows.iter_mut().enumerate() {
                if nulls[row] == 0 {
                    values_row[index] = Some(values[row]);
                }
            }
        }
        rows.sort();
        rows
    }

    fn copy_bound_values<T: Copy + Default>(
        values: Option<&ExprDeviceBuffer<T>>,
        count: usize,
    ) -> Vec<T> {
        let mut result = vec![T::default(); count];
        if let Some(values) = values {
            values
                .copy_to_slice(&mut result)
                .expect("oracle-only final copy");
        }
        result
    }

    fn bound_oracle(
        left: &str,
        right: &str,
        kind: JoinKind,
        composite: bool,
    ) -> Vec<Vec<Option<i64>>> {
        let on = if composite {
            "l.a=r.p AND l.b=r.b"
        } else {
            "l.a=r.p"
        };
        let (sql, width) = match kind {
            JoinKind::Inner | JoinKind::Left => (
                format!(
                    "SELECT r.id,l.id,r.p::int8,l.flag::int4::int8,(r.day-'2000-01-01'::date)::int8,r.id FROM {left} l {} JOIN {right} r ON {on}",
                    if kind == JoinKind::Left {
                        "LEFT"
                    } else {
                        "INNER"
                    }
                ),
                6,
            ),
            JoinKind::Semi | JoinKind::Anti => (
                format!(
                    "SELECT l.id,l.p::int8,l.flag::int4::int8,(l.day-'2000-01-01'::date)::int8,l.id FROM {left} l WHERE {}EXISTS (SELECT FROM {right} r WHERE {on})",
                    if kind == JoinKind::Anti { "NOT " } else { "" }
                ),
                5,
            ),
        };
        let mut rows = Spi::connect(|client| {
            client
                .select(&sql, None, &[])
                .expect("native bound-join oracle")
                .map(|row| {
                    (1..=width)
                        .map(|index| row.get::<i64>(index).expect("typed oracle lane"))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        rows.sort();
        rows
    }

    fn check_controlled_join(
        plan: &ResidentJoin<'_>,
        expected: &[JoinRowPair],
        stable_launch_count: bool,
    ) {
        // Tiny round sizes deliberately exercise resumable duplicate chains;
        // they are test parameters, not production cost/admission constants.
        let limits = JoinDispatchLimits {
            work_items: 3,
            chain_steps: 1,
        };
        let baseline = resident_live_bytes();
        let mut count_polls = 0;
        let count = plan
            .count_controlled(limits, &mut || {
                count_polls += 1;
                false
            })
            .expect("bounded count");
        let ControlledJoinCountOutcome::Complete {
            rows,
            kernel_launches,
        } = count
        else {
            panic!("count completed");
        };
        assert_eq!(rows, expected.len() as u64);
        assert_eq!(count_polls, kernel_launches + 1);
        assert_eq!(resident_live_bytes(), baseline);
        let last_count_poll = if stable_launch_count {
            count_polls
        } else {
            count_polls.min(2)
        };
        for stop in [1, last_count_poll] {
            let mut calls = 0;
            let stopped = plan
                .count_controlled(limits, &mut || {
                    calls += 1;
                    calls == stop
                })
                .expect("count cancellation");
            let ControlledJoinCountOutcome::Interrupted {
                kernel_launches,
                cancel_checks,
            } = stopped
            else {
                panic!("no partial count");
            };
            assert_eq!(cancel_checks, stop);
            assert_eq!(kernel_launches + 1, stop);
            assert_eq!(resident_live_bytes(), baseline);
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plan.count_controlled(limits, &mut || panic!("count callback"))
        }));
        assert!(panic.is_err());
        assert_eq!(resident_live_bytes(), baseline);
        assert!(
            matches!(plan.count_controlled(limits, &mut || false).expect("count retry"), ControlledJoinCountOutcome::Complete { rows, .. } if rows == expected.len() as u64)
        );
        assert_eq!(resident_live_bytes(), baseline);
        let mut checks = 0;
        let completed = plan
            .execute_controlled(expected.len(), limits, &mut || {
                checks += 1;
                false
            })
            .expect("controlled dispatch");
        let ControlledJoinOutcome::Finished(ResidentJoinOutcome::Complete(output)) = completed
        else {
            panic!("exact bounded capacity must complete");
        };
        assert_eq!(checks, output.kernel_launches() + 1);
        assert_eq!(
            resident_live_bytes(),
            baseline + (expected.len() * 16) as u64
        );
        let mut actual = vec![JoinRowPair::default(); output.len()];
        output
            .copy_final_rows(&mut actual)
            .expect("controlled final copy");
        actual.sort();
        assert_eq!(actual, expected);
        drop(output);
        assert_eq!(resident_live_bytes(), baseline);

        // INNER/LEFT fully traverse every chain, giving stable round counts.
        // SEMI/ANTI may stop at different rounds when atomic insertion orders
        // colliding keys differently. Do not assume their last poll is fixed.
        let last_poll = if stable_launch_count {
            checks
        } else {
            checks.min(2)
        };
        // On full-chain joins, the final poll is after the last emit kernel:
        // written device rows must still remain private after cancellation.
        for cancel_at in [1, last_poll.div_ceil(2), last_poll] {
            let mut calls = 0;
            let interrupted = plan
                .execute_controlled(expected.len(), limits, &mut || {
                    calls += 1;
                    calls == cancel_at
                })
                .expect("cooperative cancellation outcome");
            let ControlledJoinOutcome::Interrupted {
                kernel_launches,
                cancel_checks,
            } = interrupted
            else {
                panic!("cancelled dispatch must not expose a resident output");
            };
            assert_eq!(calls, cancel_at);
            assert_eq!(cancel_checks, calls);
            assert_eq!(kernel_launches + 1, calls);
            assert_eq!(
                resident_live_bytes(),
                baseline,
                "cancellation releases all charges"
            );
        }
        if !expected.is_empty() {
            let small = plan
                .execute_controlled(expected.len() - 1, limits, &mut || false)
                .expect("controlled capacity outcome");
            let ControlledJoinOutcome::Finished(ResidentJoinOutcome::NeedsCapacity {
                required_rows,
                ..
            }) = small
            else {
                panic!("capacity cannot truncate bounded join output");
            };
            assert_eq!(required_rows, expected.len() as u64);
            assert_eq!(resident_live_bytes(), baseline);
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut calls = 0;
            let _ = plan.execute_controlled(expected.len(), limits, &mut || {
                calls += 1;
                if calls == last_poll {
                    std::panic::panic_any(97_u32);
                }
                false
            });
        }))
        .expect_err("callback panic resumes only after native returns");
        assert_eq!(panic.downcast_ref::<u32>(), Some(&97));
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "panic releases partial output and workspace"
        );
        let ControlledJoinOutcome::Finished(ResidentJoinOutcome::Complete(retry)) = plan
            .execute_controlled(expected.len(), limits, &mut || false)
            .expect("retry after cancellation and panic")
        else {
            panic!("borrowed inputs remain usable");
        };
        retry
            .copy_final_rows(&mut actual)
            .expect("retry final copy");
        actual.sort();
        assert_eq!(actual, expected);
        drop(retry);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[pg_test]
    fn pg_resident_join_rows_native_parity_and_ownership() {
        Spi::run("SET pg_accel.enabled=off").expect("native SQL oracles");
        let initial_bytes = resident_live_bytes();
        let left = Keys::upload(
            "jl",
            &[
                (Some(1), Some(i64::MIN)),
                (Some(1), Some(i64::MIN)),
                (Some(1), Some(i64::MAX)),
                (Some(-1), Some(-1)),
                (None, Some(0)),
                (Some(7), None),
                (Some(9), Some(3)),
            ],
        );
        let right = Keys::upload(
            "jr",
            &[
                (Some(1), Some(i64::MIN)),
                (Some(1), Some(i64::MIN)),
                (Some(-1), Some(-1)),
                (None, Some(0)),
                (Some(7), None),
            ],
        );
        let empty = Keys::upload("je", &[]);
        for (outer, inner, left_table, right_table) in [
            (&left, &right, "jl", "jr"),
            (&left, &left, "jl", "jl"),
            (&left, &empty, "jl", "je"),
            (&empty, &right, "je", "jr"),
            (&empty, &empty, "je", "je"),
        ] {
            for key_count in [1, 2] {
                for kind in [
                    ResidentJoinKind::Inner,
                    ResidentJoinKind::Left,
                    ResidentJoinKind::Semi,
                    ResidentJoinKind::Anti,
                ] {
                    let baseline = resident_live_bytes();
                    let mut expected = oracle(kind, left_table, right_table, key_count == 2);
                    expected.sort();
                    let plan = ResidentJoin::new(
                        kind,
                        &outer.keys()[..key_count],
                        &inner.keys()[..key_count],
                    )
                    .expect("borrowed join plan");
                    check_controlled_join(
                        &plan,
                        &expected,
                        matches!(kind, ResidentJoinKind::Inner | ResidentJoinKind::Left),
                    );
                    if !expected.is_empty() {
                        match plan
                            .execute(expected.len() - 1)
                            .expect("capacity detection")
                        {
                            ResidentJoinOutcome::NeedsCapacity {
                                required_rows,
                                kernel_launches,
                            } => {
                                assert_eq!(required_rows, expected.len() as u64);
                                assert!(kernel_launches > 0);
                            }
                            ResidentJoinOutcome::Complete(_) => {
                                panic!("capacity must not truncate duplicates")
                            }
                        }
                        assert_eq!(
                            resident_live_bytes(),
                            baseline,
                            "failed capacity releases both allocations"
                        );
                    }
                    let ResidentJoinOutcome::Complete(output) =
                        plan.execute(expected.len()).expect("exact capacity join")
                    else {
                        panic!("oracle exact capacity must fit");
                    };
                    assert_eq!(
                        resident_live_bytes(),
                        baseline + (expected.len() * 16) as u64,
                        "only output charge survives"
                    );
                    assert_eq!(output.len(), expected.len());
                    assert_eq!(output.is_empty(), expected.is_empty());
                    if outer.a.is_some() {
                        assert!(output.kernel_launches() > 0);
                    }
                    if !output.is_empty() {
                        assert!(!output.as_device_ptr().is_null());
                    }
                    project_and_aggregate(
                        &output,
                        outer,
                        inner,
                        kind,
                        left_table,
                        right_table,
                        key_count == 2,
                    );
                    let mut actual = vec![JoinRowPair::default(); output.len()];
                    output
                        .copy_final_rows(&mut actual)
                        .expect("bounded final copy");
                    actual.sort();
                    assert_eq!(actual, expected);
                    assert!(
                        output
                            .copy_final_rows(&mut vec![JoinRowPair::default(); output.len() + 1])
                            .is_err()
                    );
                    drop(output);
                    assert_eq!(resident_live_bytes(), baseline);
                }
            }
        }
        let left_keys = left.keys();
        let mismatched_rows = [left_keys[0], right.keys()[1]];
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &mismatched_rows, &left_keys).is_err());
        let short_nulls =
            ExprDeviceBuffer::copy_from_slice(&[0_u8]).expect("invalid sidecar fixture");
        let malformed = ResidentJoinKey::Int32 {
            values: left.a.as_ref(),
            nulls: Some(&short_nulls),
        };
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &[malformed], &[left_keys[0]]).is_err());
        let baseline = resident_live_bytes();
        let plan =
            ResidentJoin::new(ResidentJoinKind::Inner, &left_keys, &left_keys).expect("self join");
        assert!(plan.execute(usize::MAX).is_err());
        assert_eq!(resident_live_bytes(), baseline);
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - plan.workspace_bytes().expect("workspace forecast") as u64,
            budget,
        )
        .expect("leave only workspace budget");
        assert!(
            plan.execute(1).is_err(),
            "output budget rejects before either allocation"
        );
        drop(held);
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "partial reservation unwinds"
        );
        let limits = JoinDispatchLimits {
            work_items: 2,
            chain_steps: 1,
        };
        let count_workspace = plan
            .controlled_workspace_bytes(limits)
            .expect("count workspace");
        let held =
            ExecutionMemoryCharge::reserve(budget - baseline - count_workspace as u64, budget)
                .expect("leave exactly count scratch, no pair budget");
        let expected_count = oracle(ResidentJoinKind::Inner, "jl", "jl", true).len() as u64;
        assert!(
            matches!(plan.count_controlled(limits, &mut || false).expect("count fits scratch-only budget"), ControlledJoinCountOutcome::Complete { rows, .. } if rows == expected_count)
        );
        assert!(
            plan.execute_controlled(1, limits, &mut || false).is_err(),
            "pair-producing execution cannot fit the same budget"
        );
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
        let held = ExecutionMemoryCharge::reserve(budget - baseline, budget)
            .expect("no count scratch budget");
        let held_bytes = resident_live_bytes();
        let kernels = crate::gpu::gpu_exec_count();
        let statuses = crate::gpu::bridge::test_join_status_counts();
        let failures = crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin);
        assert_eq!(
            plan.count_controlled(limits, &mut || false)
                .expect_err("real workspace OOM must not publish a count")
                .status,
            crate::gpu::GpuStatusDetail::OutOfMemory
        );
        assert_eq!(crate::gpu::gpu_exec_count(), kernels, "no native dispatch");
        assert_eq!(crate::gpu::bridge::test_join_status_counts(), statuses);
        assert_eq!(
            crate::gpu::kernel_failure_count(crate::gpu::GpuFailureDomain::HashJoin),
            failures,
            "Rust reservation OOM is not a native status failure"
        );
        assert_eq!(
            resident_live_bytes(),
            held_bytes,
            "failed count releases scratch"
        );
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
        drop((left, right, empty));
        assert_eq!(
            resident_live_bytes(),
            initial_bytes,
            "inputs and outputs released"
        );
    }
}

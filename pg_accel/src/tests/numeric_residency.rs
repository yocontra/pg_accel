//! Live, byte-exact proofs for NUMERIC resident input preparation.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    use crate::engine::residency::{ResidentColumnView, ResidentNumericData, with_resident_column};
    use crate::engine::spec::abi;
    use crate::gpu::{
        GroupedAggKernelMode, GroupedAggOutputStorage, GroupedAggSession, ResolvedGroupedAggPlan,
        execute_grouped_agg_one_shot_with_mode,
    };

    fn numeric_sql_plan(query: &str, analyze: bool) -> String {
        let options = if analyze {
            "ANALYZE, TIMING OFF, FORMAT TEXT"
        } else {
            "FORMAT TEXT"
        };
        Spi::connect(|client| {
            client
                .select(&format!("EXPLAIN ({options}) {query}"), None, &[])
                .expect("numeric SQL plan")
                .map(|row| row.get::<String>(1).expect("plan line").expect("plan text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn verify_numeric_sql(query: &str) {
        verify_numeric_sql_with_spatial(query, false);
    }

    fn verify_numeric_sql_with_spatial(query: &str, spatial: bool) {
        Spi::run("SET pg_accel.enabled=off").expect("native baseline");
        Spi::run(&format!("CREATE TEMP TABLE numeric_expected AS {query}")).expect("native result");
        Spi::run(&format!(
            "SET pg_accel.enabled=on; SET pg_accel.test_force_numeric_groupagg={}; \
            SET pg_accel.test_force_spatial_groupagg={}",
            !spatial, spatial
        ))
        .expect("test-only numeric admission");
        let plan = numeric_sql_plan(query, true);
        assert!(
            plan.contains("Custom Scan (GpuAccelAgg)"),
            "{query}\n{plan}"
        );
        assert!(
            plan.contains("parallel_dense_numeric"),
            "must actually dispatch numeric: {plan}"
        );
        Spi::run(&format!("CREATE TEMP TABLE numeric_actual AS {query}")).expect("GPU SQL result");
        // row_to_json retains NUMERIC's textual scale, special values and NULLs,
        // unlike SQL numeric equality which would ignore display scale.
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT NOT EXISTS ((SELECT row_to_json(nr)::text FROM numeric_actual nr \
             EXCEPT ALL SELECT row_to_json(nr)::text FROM numeric_expected nr) UNION ALL \
             (SELECT row_to_json(nr)::text FROM numeric_expected nr \
             EXCEPT ALL SELECT row_to_json(nr)::text FROM numeric_actual nr))"
            )
            .expect("exact output comparison"),
            Some(true),
            "{query}"
        );
        Spi::run("DROP TABLE numeric_actual, numeric_expected").expect("oracle cleanup");
    }

    #[pg_test]
    fn pg_numeric_spatial_rollup_rechecks_and_bounded_lifecycle() {
        configure();
        Spi::run("CREATE EXTENSION IF NOT EXISTS postgis").expect("PostGIS fixture");
        Spi::run("CREATE TEMP TABLE numeric_geo(g int4,p int4,v numeric,w numeric,geom geometry(Point,4326)); \
            INSERT INTO numeric_geo SELECT i%3,i%7,CASE WHEN i%11=0 THEN NULL ELSE (i-500)::numeric/100 END, \
            CASE WHEN i%13=0 THEN NULL ELSE (i-200)::numeric/3 END, \
            CASE WHEN i%17=0 THEN NULL WHEN i%19=0 THEN 'SRID=4326;POINT EMPTY'::geometry \
            ELSE ST_SetSRID(ST_MakePoint(i%15,(i/15)%15),4326) END \
            FROM generate_series(1,1017) s(i); ANALYZE numeric_geo")
            .expect("spatial decimal fixture");
        let pin = || {
            Spi::run("SELECT pg_accel_pin('numeric_geo',ARRAY['g','p','v','w','geom'])")
                .expect("complete current geometry/decimal snapshot");
        };
        pin();
        // Shell edges, hole edges and a separate island all participate in
        // exact PostGIS rechecks; only accepted points may establish groups.
        let area = "'SRID=4326;MULTIPOLYGON(((0 0,10 0,10 10,0 10,0 0),(3 3,7 3,7 7,3 7,3 3)),((12 12,14 12,14 14,12 14,12 12)))'::geometry";
        let previous = crate::engine::executor::agg::configure_dense_dispatch_test(113, 0, 0);
        for predicate in [
            format!("ST_Intersects(geom,{area})"),
            format!("ST_Within(geom,{area})"),
            format!("ST_Covers({area},geom)"),
        ] {
            for keys in ["", "g,"] {
                let query = format!(
                    "SELECT {keys}count(*) n,sum(v) s,avg(w) a, \
                    avg(p) FILTER (WHERE p IN (1,3,5)) q FROM numeric_geo WHERE {predicate}{}",
                    if keys.is_empty() { "" } else { " GROUP BY g" }
                );
                verify_numeric_sql_with_spatial(&query, true);
            }
        }
        let query = format!(
            "SELECT g,count(*) n,sum(v) s,avg(w) a FROM numeric_geo \
            WHERE ST_Intersects(geom,{area}) GROUP BY g"
        );
        Spi::run("UPDATE numeric_geo SET geom=ST_SetSRID(ST_MakePoint(1,1),4326),v=-v*1e20 WHERE g=1; \
            DO $$ BEGIN BEGIN UPDATE numeric_geo SET geom=NULL,v='NaN'; RAISE EXCEPTION USING ERRCODE='P0002'; \
            EXCEPTION WHEN no_data_found THEN NULL; END; END $$")
            .expect("spatial/decimal invalidation and rollback");
        pin();
        verify_numeric_sql_with_spatial(&query, true);
        Spi::run(
            "TRUNCATE numeric_geo; INSERT INTO numeric_geo VALUES \
            (0,1,2.3400,NULL,'SRID=4326;POINT(1 1)'), \
            (1,1,'NaN','Infinity','SRID=4326;POINT(20 20)'), \
            (2,1,repeat('9',131072)::numeric,0,'SRID=4326;POINT(20 20)'), \
            (2,1,repeat('9',131072)::numeric,0,'SRID=4326;POINT(20 20)'); ANALYZE numeric_geo",
        )
        .expect("outside specials and overflowing sum");
        pin();
        verify_numeric_sql_with_spatial(&query, true);
        Spi::run("UPDATE numeric_geo SET geom=NULL").expect("all excluded");
        pin();
        verify_numeric_sql_with_spatial(&query, true);
        verify_numeric_sql_with_spatial(
            &format!(
                "SELECT count(*) n,sum(v) s FROM numeric_geo \
            WHERE ST_Intersects(geom,{area})"
            ),
            true,
        );
        Spi::run("DELETE FROM numeric_geo").expect("empty spatial source");
        pin();
        verify_numeric_sql_with_spatial(&query, true);
        verify_numeric_sql_with_spatial(
            &format!(
                "SELECT count(*) n,sum(v) s FROM numeric_geo WHERE ST_Intersects(geom,{area})"
            ),
            true,
        );
        crate::engine::executor::agg::configure_dense_dispatch_test(
            previous.0, previous.1, previous.2,
        );
    }

    #[pg_test]
    fn pg_numeric_filtered_sql_refresh_and_specials() {
        configure();
        Spi::run("CREATE TEMP TABLE numeric_filtered(g int4,p int4,q int4,v numeric); \
            INSERT INTO numeric_filtered SELECT i%7,CASE WHEN i%13=0 THEN NULL ELSE i%11 END, \
            CASE WHEN i%17=0 THEN NULL ELSE i%3 END,CASE WHEN i%19=0 THEN NULL ELSE (i-5000)::numeric/100 END \
            FROM generate_series(1,10017) s(i); ANALYZE numeric_filtered; \
            SELECT pg_accel_pin('numeric_filtered',ARRAY['g','p','q','v'])")
            .expect("independent decimal filter fixture");
        let outputs = "count(*) n,sum(v) FILTER (WHERE p=1) s,avg(v) FILTER (WHERE p=1) a, \
            count(v) FILTER (WHERE p=1) c,sum(v) FILTER (WHERE q IN (1,2)) t";
        let grouped = format!("SELECT g,{outputs} FROM numeric_filtered GROUP BY g");
        let global = format!("SELECT {outputs} FROM numeric_filtered");
        let mixed = "SELECT g,sum(v) FILTER (WHERE p=1) s,avg(p) FILTER (WHERE q=2) a, \
            count(p) FILTER (WHERE q=2) c,count(*) FILTER (WHERE q IN (1,2)) n \
            FROM numeric_filtered GROUP BY g";
        // Fresh decimal plans require current resident digit metadata. DML
        // invalidates it; repin before asking the planner to admit a new query.
        let refresh = || {
            Spi::run("SELECT pg_accel_pin('numeric_filtered',ARRAY['g','p','q','v'])")
                .expect("current predicate and decimal snapshot");
        };
        verify_numeric_sql(&grouped);
        verify_numeric_sql(&global);
        verify_numeric_sql(mixed);
        Spi::run("UPDATE numeric_filtered SET p=1,q=2,v=-v*1e30 WHERE g=1; \
            DO $$ BEGIN BEGIN UPDATE numeric_filtered SET p=NULL,v='NaN'; \
            RAISE EXCEPTION USING ERRCODE='P0002'; EXCEPTION WHEN no_data_found THEN NULL; END; END $$")
            .expect("predicate/value changes and rollback");
        refresh();
        verify_numeric_sql(&grouped);
        Spi::run(
            "TRUNCATE numeric_filtered; INSERT INTO numeric_filtered VALUES \
            (0,0,0,'NaN'),(0,0,0,'Infinity'),(0,1,2,2.3400),(0,1,2,NULL), \
            (1,NULL,NULL,(repeat('9',131072))::numeric), \
            (1,NULL,NULL,(repeat('9',131072))::numeric); ANALYZE numeric_filtered",
        )
        .expect("excluded specials and overflowing sum inputs");
        refresh();
        verify_numeric_sql("SELECT g,count(v) c FROM numeric_filtered GROUP BY g");
        verify_numeric_sql("SELECT count(*) n,sum(v) s FROM numeric_filtered WHERE q=2");
        verify_numeric_sql(
            "SELECT g,count(*) n,sum(v) s FROM numeric_filtered WHERE q=2 GROUP BY g",
        );
        verify_numeric_sql(
            "SELECT count(v) c,count(v) FILTER (WHERE g=1) d,sum(v) FILTER (WHERE p=1) s FROM numeric_filtered",
        );
        verify_numeric_sql(&grouped);
        verify_numeric_sql(&global);
        verify_numeric_sql(mixed);
        Spi::run("UPDATE numeric_filtered SET p=NULL,q=NULL").expect("all predicates UNKNOWN");
        refresh();
        verify_numeric_sql(&grouped);
        Spi::run("TRUNCATE numeric_filtered; ANALYZE numeric_filtered").expect("empty filters");
        refresh();
        verify_numeric_sql(&global);
        verify_numeric_sql(&grouped);
        verify_numeric_sql(mixed);
        Spi::run(
            "SELECT pg_accel_unpin('numeric_filtered'); SELECT pg_accel_evict('numeric_filtered'); \
            DROP TABLE numeric_filtered",
        )
        .expect("filtered fixture cleanup");
    }

    #[pg_test]
    fn pg_numeric_sql_planner_preserves_projection_and_results() {
        configure();
        Spi::run(
            "SET pg_accel.enabled=on; \
            CREATE TEMP TABLE numeric_sql(g int4,h int4,t text,v numeric,w numeric,b bool); \
            INSERT INTO numeric_sql SELECT CASE WHEN i%13=0 THEN NULL ELSE i%3 END, i%2, \
            CASE WHEN i%11=0 THEN NULL ELSE (i%5)::text END, \
            CASE WHEN i%7=0 THEN NULL ELSE (i-5000)::numeric/100 END, \
            CASE WHEN i%17=0 THEN NULL ELSE (i%31)::numeric/3 END, \
            CASE WHEN i%19=0 THEN NULL ELSE i%2=0 END \
            FROM generate_series(1,10017) s(i); ANALYZE numeric_sql; \
            SELECT pg_accel_pin('numeric_sql'::regclass, ARRAY['g','h','t','v','w','b'])",
        )
        .expect("SQL fixture and resident metadata");
        let query = "SELECT count(*) n,sum(v) s,avg(v) a,count(v) c,avg(w) w FROM numeric_sql";
        assert!(
            !numeric_sql_plan(query, false).contains("GpuAccelAgg"),
            "small NUMERIC queries must keep the normal row/cost gates"
        );
        for query in [
            query,
            "SELECT sum(v) s FROM numeric_sql",
            "SELECT count(*) n,sum(v) s,avg(w) a FROM numeric_sql WHERE g=1",
            "SELECT t,g,h,sum(v) s,avg(w) a FROM numeric_sql WHERE g IN (0,2) GROUP BY t,g,h",
            "SELECT g,sum(v) FILTER (WHERE h=1) s,avg(g) a,count(*) n FROM numeric_sql WHERE g>=0 AND g<=1 GROUP BY g",
            "SELECT count(*) n,sum(v) s FROM numeric_sql WHERE NOT (g=1 OR g=2)",
            "SELECT g,sum(v) s FROM numeric_sql WHERE g NOT IN (0,2) GROUP BY g",
            "SELECT sum(v) s,count(v) c FROM numeric_sql WHERE g IS NOT NULL",
            "SELECT count(*) n,sum(v) s,avg(w) a FROM numeric_sql WHERE b",
            "SELECT g,sum(v) s,avg(g) a FROM numeric_sql WHERE b GROUP BY g",
            "SELECT g,sum(v) s FROM numeric_sql WHERE g<0 GROUP BY g",
            "SELECT count(*) n,sum(v) s FROM numeric_sql WHERE g<0",
            "SELECT sum(v) s,sum(g) i,avg(g) a,count(g) c FROM numeric_sql",
            "SELECT sum(v) s,count(*) FILTER (WHERE g=1) c,avg(g) FILTER (WHERE h=1) a FROM numeric_sql",
            "SELECT t,g,h,sum(v) s,avg(h) a,count(g) c,count(*) n FROM numeric_sql GROUP BY t,g,h",
            "SELECT t,sum(v) FILTER (WHERE g=1) s,sum(g) FILTER (WHERE h=1) i,avg(g) FILTER (WHERE h=1) a,count(g) FILTER (WHERE h=1) c FROM numeric_sql GROUP BY t",
            "SELECT avg(v) a FROM numeric_sql",
            "SELECT count(v) c FROM numeric_sql",
            "SELECT g,count(v) c,count(w) d,count(*) n FROM numeric_sql GROUP BY g",
            "SELECT t,g,h,sum(v) s,count(w) FILTER (WHERE h=1) c FROM numeric_sql GROUP BY t,g,h",
            "SELECT count(v) FILTER (WHERE g=1) a,count(w) FILTER (WHERE h=0) b FROM numeric_sql",
            "SELECT g,count(*) n,sum(v) s,avg(v) a,count(v) c FROM numeric_sql GROUP BY g",
            "SELECT h,g,avg(w) a,count(*) n,sum(v) s FROM numeric_sql GROUP BY g,h",
            "SELECT t,g,h,sum(v) s,avg(w) a FROM numeric_sql GROUP BY t,g,h",
            "SELECT sum(v) FILTER (WHERE g=1) s FROM numeric_sql",
            "SELECT t,g,h,count(*) n,sum(v) FILTER (WHERE g IN (0,2)) s,avg(w) FILTER (WHERE h=1) a,count(w) FILTER (WHERE h=1) c FROM numeric_sql GROUP BY t,g,h",
            "SELECT g,sum(v) FILTER (WHERE h<0) s,avg(v) FILTER (WHERE h<0) a,count(v) FILTER (WHERE h<0) c FROM numeric_sql GROUP BY g",
        ] {
            verify_numeric_sql(query);
        }
        for unsupported in [
            "SELECT min(v) FROM numeric_sql",
            "SELECT sum(v+1) FROM numeric_sql",
            "SELECT sum(v) FILTER (WHERE v>0) FROM numeric_sql",
            "SELECT sum(v) FROM numeric_sql WHERE v>0",
            "SELECT sum(v) FROM numeric_sql WHERE g IS NULL OR g=1",
            "SELECT sum(v) FROM numeric_sql WHERE g=1 OR h=1",
            "SELECT v,sum(w) FROM numeric_sql GROUP BY v",
        ] {
            assert!(
                !numeric_sql_plan(unsupported, false).contains("GpuAccelAgg"),
                "test force must not widen semantics: {unsupported}"
            );
        }
        Spi::run("SET pg_accel.resident_memory_budget_mb=0").expect("zero budget");
        assert!(
            !numeric_sql_plan(query, false).contains("GpuAccelAgg"),
            "test force must not bypass memory admission"
        );
        Spi::run(
            "RESET pg_accel.resident_memory_budget_mb; \
            CREATE TEMP TABLE numeric_cold AS TABLE numeric_sql; ANALYZE numeric_cold; \
            SET pg_accel.auto_load=on",
        )
        .expect("cold numeric input");
        assert!(
            !numeric_sql_plan("SELECT sum(v) FROM numeric_cold", false).contains("GpuAccelAgg"),
            "exact digit evidence is required even with auto-load enabled"
        );
        Spi::run("SET pg_accel.auto_load=off; DROP TABLE numeric_cold").expect("cold cleanup");
        Spi::run(
            "UPDATE numeric_sql SET v=-v*1e12 WHERE g=1; \
            SELECT pg_accel_pin('numeric_sql'::regclass, ARRAY['g','h','t','v','w'])",
        )
        .expect("refresh changed digit extent");
        verify_numeric_sql(query);
        Spi::run(
            "TRUNCATE numeric_sql; INSERT INTO numeric_sql(v,w) VALUES \
            ('Infinity','NaN'),('-Infinity',NULL),(NULL,'-Infinity'); ANALYZE numeric_sql; \
            SELECT pg_accel_pin('numeric_sql'::regclass, ARRAY['g','h','t','v','w'])",
        )
        .expect("special values");
        verify_numeric_sql(query);
        Spi::run(
            "TRUNCATE numeric_sql; ANALYZE numeric_sql; \
            SELECT pg_accel_pin('numeric_sql'::regclass, ARRAY['g','h','t','v','w'])",
        )
        .expect("empty input");
        verify_numeric_sql(query);
        verify_numeric_sql("SELECT g,sum(v) s FROM numeric_sql GROUP BY g");
        Spi::run("INSERT INTO numeric_sql(v) VALUES ((repeat('9',131072)||'.'||repeat('9',16383))::numeric); \
            SELECT pg_accel_pin('numeric_sql'::regclass, ARRAY['g','h','t','v','w'])")
            .expect("largest NUMERIC value");
        for enabled in [false, true] {
            Spi::run(&format!("SET pg_accel.enabled={enabled}")).expect("SQLSTATE arm");
            let state = PgTryBuilder::new(|| {
                Spi::run("SELECT avg(v) FROM numeric_sql").expect("AVG execution");
                None
            })
            .catch_when(PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE, |_| {
                Some("22003")
            })
            .execute();
            assert_eq!(state, Some("22003"), "native/GPU AVG overflow SQLSTATE");
        }
        Spi::run("SET pg_accel.test_force_numeric_groupagg=off; \
            SELECT pg_accel_unpin('numeric_sql'::regclass); SELECT pg_accel_evict('numeric_sql'::regclass)")
            .expect("cleanup");
    }

    #[pg_test]
    fn pg_numeric_dense_capacity_uses_individual_postgres_domains() {
        configure();
        Spi::run(
            "CREATE TEMP TABLE numeric_domains AS SELECT \
             CASE WHEN i%11=0 THEN NULL ELSE i%10 END AS a, \
             CASE WHEN i%11=0 THEN NULL ELSE i%10 END AS b, \
             CASE WHEN i%11=0 THEN NULL ELSE i%10 END AS c, i::numeric AS v \
             FROM generate_series(1,110) g(i); ANALYZE numeric_domains",
        )
        .expect("correlated nullable keys");
        for (keys, slots) in [("", 1), ("a", 11), ("a,b", 121), ("a,b,c", 1331)] {
            let group = if keys.is_empty() {
                String::new()
            } else {
                format!(" GROUP BY {keys}")
            };
            let estimates =
                crate::engine::ffi::planner_hooks::shape::test_capture_dense_slots(110, || {
                    Spi::run(&format!(
                        "EXPLAIN SELECT sum(v) FROM numeric_domains{group}"
                    ))
                    .expect("real PostgreSQL planning");
                });
            assert_eq!(estimates.len(), 1, "one aggregate planner invocation");
            let cardinalities = estimates[0].as_ref().expect("individual key estimates");
            let nullable = vec![true; cardinalities.len()];
            assert_eq!(
                crate::engine::ffi::planner_hooks::shape::dense_slot_product(
                    cardinalities,
                    &nullable
                ),
                Some(slots),
                "dense domain for {keys}"
            );
        }
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM (SELECT a,b,c FROM numeric_domains GROUP BY a,b,c) g"
            )
            .expect("joint group count"),
            Some(11),
            "dense capacity must exceed both joint output groups and input rows"
        );
    }

    /// Exercise the native numeric ABI directly while SQL planner admission is
    /// still disabled. This is a test seam, not a CPU aggregate fallback.
    fn verify_native_sum(table: &str) {
        let expected =
            Spi::get_one::<Vec<u8>>(&format!("SELECT numeric_send(sum(v)) FROM {table}"))
                .expect("native PostgreSQL SUM");
        let expected_count = Spi::get_one::<i64>(&format!("SELECT count(v) FROM {table}"))
            .expect("native PostgreSQL COUNT")
            .expect("COUNT is nonnull");
        Spi::run(&format!("CREATE OR REPLACE FUNCTION pg_temp.numeric_avg_expected(OUT value bytea, OUT state text) \
            LANGUAGE plpgsql AS $body$ BEGIN SELECT numeric_send(avg(v)) INTO value FROM {table}; \
            EXCEPTION WHEN numeric_value_out_of_range THEN state := SQLSTATE; END $body$"))
            .expect("native AVG exception boundary");
        let expected_avg = Spi::connect(|client| {
            let row = client
                .select("SELECT * FROM pg_temp.numeric_avg_expected()", None, &[])
                .expect("native AVG")
                .next()
                .expect("one AVG row");
            let value = row.get::<Vec<u8>>(1).expect("AVG bytes");
            row.get::<String>(2)
                .expect("AVG SQLSTATE")
                .map_or(Ok(value), Err)
        });
        let oid = Spi::get_one::<pg_sys::Oid>(&format!("SELECT '{table}'::regclass::oid"))
            .expect("fixture OID")
            .expect("fixture OID");
        Spi::run(&format!(
            "SELECT pg_accel_pin('{table}'::regclass, ARRAY['v'])"
        ))
        .expect("pin exact numeric input");
        let (actual, count) = with_resident_column(oid, 1, |column| {
            let ResidentColumnView::Numeric { data, .. } = column else {
                panic!("nonempty fixture must have resident NUMERIC input");
            };
            let view = data.native_view();
            let stride =
                16 + (2 * (view.max_weight - view.min_weight + 5) as usize).next_multiple_of(8);
            // SAFETY: these ABI records contain only integers and raw pointers;
            // all-zero bit patterns are valid. Required fields are set below.
            let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
            desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
            desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
            desc.row_count = data.rows.len();
            desc.group_capacity = 1;
            desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
            for filter in &mut desc.measure_filters {
                filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
            }
            desc.measure_count = 1;
            let measure = &mut desc.measures[0];
            measure.agg_mask =
                abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
            measure.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
            measure.state_bytes = stride as u32;
            measure.value.values = std::ptr::from_ref(&view).cast();
            measure.value.nulls = data.nulls.map_or(std::ptr::null(), |nulls| nulls.as_ptr());
            measure.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
            measure.value.element_bytes = size_of::<abi::PgaccelNumericView>() as u32;
            measure.value.flags = abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW;
            // SAFETY: all row/digit/NULL buffers stay pinned by this residency
            // borrow; the metadata view is readable during construction.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("numeric plan");
            let mut output = GroupedAggOutputStorage::new(&plan).expect("numeric output");
            let (_, mode) = execute_grouped_agg_one_shot_with_mode(&plan, &mut output)
                .expect("one-shot numeric execution");
            assert_eq!(mode, GroupedAggKernelMode::ParallelDenseNumeric);
            let count = output.measure_count(0).expect("count lane")[0];
            assert_eq!(
                output.measure_nonnull_count(0).expect("nonnull lane")[0],
                count
            );
            let wire = (count != 0).then(|| {
                output
                    .measure_numeric_wire_at(0, 0)
                    .expect("numeric result")
            });

            // Re-resolved metadata and row chunks must preserve a single digit
            // arena, while RESET and FINALIZE use zero-row metadata snapshots.
            let mut session = GroupedAggSession::start(&plan, 4093).expect("numeric session");
            drop(plan);
            // SAFETY: resident payloads are still pinned, but this is a fresh
            // metadata owner as in separately borrowed executor callbacks.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }
                .expect("re-resolved numeric plan");
            for first in (0..plan.row_count()).step_by(4093) {
                let chunk = plan
                    .row_chunk(first, (plan.row_count() - first).min(4093))
                    .expect("numeric chunk");
                session.accumulate(&chunk).expect("numeric chunk execution");
            }
            session
                .accumulate(
                    &plan
                        .row_chunk(plan.row_count(), 0)
                        .expect("empty tail chunk"),
                )
                .expect("empty tail transition");
            let mut chunked = GroupedAggOutputStorage::new(&plan).expect("chunked output");
            session
                .finalize(&plan, &mut chunked)
                .expect("numeric finalize");
            assert_eq!(chunked.measure_count(0), output.measure_count(0));
            assert_eq!(
                chunked.measure_state(0, crate::gpu::GroupedAggStateLane::Sum),
                output.measure_state(0, crate::gpu::GroupedAggStateLane::Sum)
            );
            session.reset(&plan).expect("numeric reset");
            let mut empty = GroupedAggOutputStorage::new(&plan).expect("reset output");
            session
                .finalize(&plan, &mut empty)
                .expect("empty numeric finalize");
            assert_eq!(empty.measure_count(0).expect("empty count")[0], 0);
            (wire, count)
        })
        .expect("resident numeric input");
        assert_eq!(actual, expected, "byte-exact SUM for {table}");
        assert_eq!(count, expected_count as u64);
        // Exercise the production result materializer outside the residency
        // borrow, including PostgreSQL's exact division/scale selection.
        for (average_count, expected_wire) in
            [(None, Ok(expected)), (Some(expected_count), expected_avg)]
        {
            let received = PgTryBuilder::new(|| {
                Ok(actual.as_ref().map(|wire| {
                    let datum = crate::engine::executor::agg::materialize_numeric_sum(
                        wire.clone(),
                        average_count,
                    )
                    .expect("NUMERIC result materializes");
                    // SAFETY: datum is a live NUMERIC allocated by the materializer;
                    // numeric_send copies it into a binary bytea for comparison.
                    unsafe {
                        pgrx::direct_function_call::<Vec<u8>>(pg_sys::numeric_send, &[Some(datum)])
                    }
                    .expect("numeric_send result")
                }))
            })
            .catch_when(PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE, |_| {
                Err("22003".to_owned())
            })
            .execute();
            assert_eq!(received, expected_wire, "materialized SUM/AVG parity");
        }
        Spi::run(&format!(
            "SELECT pg_accel_unpin('{table}'::regclass); SELECT pg_accel_evict('{table}'::regclass)"
        ))
        .expect("release fixture");
    }

    #[pg_test]
    fn pg_numeric_native_sum_matches_postgres() {
        configure();
        Spi::run(
            "CREATE TEMP TABLE numeric_sum(v numeric); \
            INSERT INTO numeric_sum SELECT CASE WHEN i%23=0 THEN NULL ELSE \
            ((i::bigint*7919)%20000001-10000000)::numeric / power(10::numeric,i%13) END \
            FROM generate_series(1,100017) g(i); ANALYZE numeric_sum",
        )
        .expect("numeric aggregate fixture");
        verify_native_sum("numeric_sum");
        for values in [
            "(0.0000),(-0.0000),(NULL)",
            "('1e131071'),('-1e131071'),('-1e-16383')",
            "((repeat('9',131072)||'.'||repeat('9',16383))::numeric)",
            "('Infinity'),('1e131071'),(NULL)",
            "('-Infinity'),(0.000000001)",
            "('Infinity'),('-Infinity')",
            "('NaN'),(123.456)",
            "(NULL),(NULL)",
        ] {
            Spi::run(&format!(
                "TRUNCATE numeric_sum; INSERT INTO numeric_sum VALUES {values}; ANALYZE numeric_sum"
            ))
            .expect("numeric edge fixture");
            verify_native_sum("numeric_sum");
        }
        Spi::run("DROP TABLE numeric_sum").expect("fixture cleanup");
    }

    fn verify_roundtrip(table: &str) -> ResidentNumericData {
        let oid = Spi::get_one::<pg_sys::Oid>(&format!("SELECT '{table}'::regclass::oid"))
            .expect("valid NUMERIC test fixture")
            .expect("valid NUMERIC test fixture");
        let expected = Spi::connect(|client| {
            client
                .select(
                    &format!("SELECT numeric_send(v) FROM {table} ORDER BY ctid"),
                    None,
                    &[],
                )
                .expect("valid NUMERIC test fixture")
                .map(|row| row.get::<Vec<u8>>(1).expect("valid NUMERIC test fixture"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            Spi::get_one::<i64>(&format!(
                "SELECT pg_accel_pin('{table}'::regclass, ARRAY['v'])"
            ))
            .expect("valid NUMERIC test fixture")
            .expect("valid NUMERIC test fixture"),
            i64::try_from(expected.len()).expect("valid NUMERIC test fixture")
        );
        let data = with_resident_column(oid, 1, |view| match view {
            ResidentColumnView::Numeric { type_oid, data } => {
                assert_eq!(type_oid, pg_sys::NUMERICOID);
                ResidentNumericData {
                    rows: data.rows.copy_to_vec().expect("valid NUMERIC test fixture"),
                    digits: data
                        .digits
                        .map(|digits| digits.copy_to_vec().expect("valid NUMERIC test fixture"))
                        .unwrap_or_default(),
                    nulls: data
                        .nulls
                        .map(|nulls| nulls.copy_to_vec().expect("valid NUMERIC test fixture")),
                }
            }
            ResidentColumnView::Empty { type_oid } => {
                assert_eq!(type_oid, pg_sys::NUMERICOID);
                ResidentNumericData::default()
            }
            _ => panic!("NUMERIC was narrowed or loaded with a different representation"),
        })
        .expect("valid NUMERIC test fixture");
        data.validate().expect("valid NUMERIC test fixture");
        let request = crate::engine::residency::SelectedRelation {
            relid: oid,
            columns: vec![1],
        };
        let estimate = crate::engine::residency::estimate_selected_relation(&request)
            .expect("numeric planner metadata");
        let stats = &estimate
            .resident_numeric_columns
            .as_ref()
            .expect("loaded metadata")[0];
        assert_eq!(stats.attno, 1);
        assert_eq!(stats.digit_count, data.digits.len() as u64);
        assert_eq!(
            stats.bounds.min_weight,
            data.rows
                .iter()
                .filter(|row| row.ndigits != 0)
                .map(|row| i32::from(row.weight) - i32::from(row.ndigits) + 1)
                .min()
                .unwrap_or(0)
        );
        assert_eq!(
            stats.bounds.max_weight,
            data.rows
                .iter()
                .filter(|row| row.ndigits != 0)
                .map(|row| i32::from(row.weight))
                .max()
                .unwrap_or(0)
        );
        assert_eq!(
            stats.bounds.max_ndigits,
            data.rows
                .iter()
                .map(|row| u32::from(row.ndigits))
                .max()
                .unwrap_or(0)
        );
        assert!(crate::engine::residency::revalidate_loaded_estimates(
            &[request],
            &[estimate]
        ));
        assert_eq!(data.rows.len(), expected.len());
        for (index, wire) in expected.iter().enumerate() {
            assert_eq!(
                &data.wire_value(index).expect("valid NUMERIC test fixture"),
                wire,
                "row {index}"
            );
        }
        let charged = Spi::get_one::<i64>(&format!(
            "SELECT raw_bytes FROM pg_accel_resident_status() WHERE relid = {oid}"
        ))
        .expect("valid NUMERIC test fixture")
        .expect("valid NUMERIC test fixture");
        assert_eq!(
            u64::try_from(charged).expect("valid NUMERIC test fixture"),
            data.device_bytes().expect("valid NUMERIC test fixture")
        );
        data
    }

    fn configure() {
        Spi::run("SELECT pg_advisory_xact_lock(882201); SET pg_accel.auto_load=off; \
            SET pg_accel.gpu_enabled=on; SET synchronize_seqscans=off; SET effective_io_concurrency=0")
            .expect("valid NUMERIC test fixture");
    }

    fn empty_numeric_plan(groups: usize) -> ResolvedGroupedAggPlan<'static> {
        // SAFETY: ABI records contain only integers/raw pointers and have valid
        // zero representations. Required active fields are initialized below.
        let (mut desc, view): (abi::PgaccelGroupedAggDesc, abi::PgaccelNumericView) =
            unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
        desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
        desc.group_capacity = groups;
        desc.key_count = 1;
        desc.keys[0].values.tag = crate::gpu::PgaccelValTag::Int32;
        desc.keys[0].cardinality = groups as u32;
        desc.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        desc.measure_count = 1;
        let measure = &mut desc.measures[0];
        measure.agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        measure.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
        measure.state_bytes = 32;
        measure.value.values = std::ptr::from_ref(&view).cast();
        measure.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
        measure.value.element_bytes = size_of::<abi::PgaccelNumericView>() as u32;
        measure.value.flags = abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW;
        // SAFETY: construction snapshots readable metadata; this zero-row plan
        // has no device payload pointers and can therefore outlive the view.
        unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("empty numeric plan")
    }

    #[pg_test]
    fn pg_numeric_execution_memory_budget_and_pool_ownership() {
        use crate::engine::residency::{resident_budget_snapshot, resident_live_bytes};
        use crate::gpu::{
            GpuStatusDetail, GroupedAggWorkspace, cleanup_grouped_allocation_pools,
            grouped_output_pool_snapshot, grouped_workspace_pool_snapshot,
        };
        configure();
        Spi::run("SET pg_accel.resident_memory_budget_mb=64").expect("test budget");
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let plan = empty_numeric_plan(1);
        let before_output = grouped_output_pool_snapshot();
        let before_workspace = grouped_workspace_pool_snapshot();
        let forecast = GroupedAggSession::memory_requirements(&plan, 4093)
            .expect("allocation-free memory query");
        assert_eq!(resident_live_bytes(), baseline);
        assert_eq!(grouped_output_pool_snapshot(), before_output);
        assert_eq!(grouped_workspace_pool_snapshot(), before_workspace);
        let output = GroupedAggOutputStorage::new(&plan).expect("charged output");
        assert_eq!(resident_live_bytes(), baseline + 49);
        let workspace = GroupedAggWorkspace::allocate(&plan).expect("charged workspace");
        let workspace_bytes = workspace.requirement().bytes as u64;
        assert_eq!(forecast.workspace_bytes as u64, workspace_bytes);
        assert_eq!(forecast.output_bytes, 49);
        assert_eq!(forecast.total_bytes as u64, workspace_bytes + 49);
        assert_eq!(resident_live_bytes(), baseline + 49 + workspace_bytes);
        drop(output);
        drop(workspace);
        assert_eq!(
            resident_live_bytes(),
            baseline + 49 + workspace_bytes,
            "pool retains its charges"
        );
        let snapshot =
            resident_budget_snapshot(&[]).expect("store and execution charges reconcile");
        assert_eq!(snapshot.retained_execution_bytes, 49 + workspace_bytes);
        assert_eq!(
            snapshot.projected_final_bytes(0, 0),
            Some(
                snapshot.other_backend_live_bytes
                    + snapshot.pinned_unselected_raw_bytes
                    + 49
                    + workspace_bytes
            )
        );
        let output_hits = grouped_output_pool_snapshot().hits;
        let workspace_hits = grouped_workspace_pool_snapshot().hits;
        let output = GroupedAggOutputStorage::new(&plan).expect("reuse charged output");
        let workspace = GroupedAggWorkspace::allocate(&plan).expect("reuse charged workspace");
        assert_eq!(grouped_output_pool_snapshot().hits, output_hits + 1);
        assert_eq!(grouped_workspace_pool_snapshot().hits, workspace_hits + 1);
        assert_eq!(
            resident_live_bytes(),
            baseline + 49 + workspace_bytes,
            "reuse cannot double charge"
        );
        drop(output);
        drop(workspace);
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);

        Spi::run("SET pg_accel.resident_memory_budget_mb=0").expect("exhausted budget");
        assert_eq!(
            GroupedAggSession::memory_requirements(&plan, 1)
                .expect("zero-budget query must not reserve"),
            forecast
        );
        let allocations = grouped_output_pool_snapshot().fresh_allocations;
        let failure = GroupedAggOutputStorage::new(&plan)
            .err()
            .expect("output budget rejection");
        assert_eq!(failure.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(
            grouped_output_pool_snapshot().fresh_allocations,
            allocations
        );
        let allocations = grouped_workspace_pool_snapshot().fresh_allocations;
        let failure = GroupedAggWorkspace::allocate(&plan)
            .err()
            .expect("workspace budget rejection");
        assert_eq!(failure.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(
            grouped_workspace_pool_snapshot().fresh_allocations,
            allocations
        );
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "failed reservations leave no charges"
        );

        Spi::run("SET pg_accel.resident_memory_budget_mb=1").expect("tight budget");
        let active = GroupedAggOutputStorage::new(&plan).expect("small active output");
        let cached = GroupedAggWorkspace::allocate(&plan).expect("small cached workspace");
        drop(cached);
        let large = empty_numeric_plan(50000);
        assert!(GroupedAggOutputStorage::new(&large).is_err());
        assert!(GroupedAggWorkspace::allocate(&large).is_err());
        assert_eq!(
            resident_live_bytes(),
            baseline + 49,
            "only idle allocations may be reclaimed"
        );
        drop(active);
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
        Spi::run("RESET pg_accel.resident_memory_budget_mb").expect("restore budget");
    }

    #[pg_test]
    fn pg_dense_integer_execution_budget_and_bounded_pool_ownership() {
        use crate::engine::residency::{ExecutionMemoryCharge, resident_live_bytes};
        use crate::gpu::{
            GpuStatusDetail, GroupedAggOutcome, GroupedAggWorkspace,
            cleanup_grouped_allocation_pools, grouped_output_pool_snapshot,
            grouped_workspace_pool_snapshot,
        };
        configure();
        Spi::run("SET pg_accel.resident_memory_budget_mb=64").expect("test budget");
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        // SAFETY: ABI records have valid zero representations; all active
        // fields are populated below. Empty input has no payload pointers.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
        desc.size_bytes = size_of::<abi::PgaccelGroupedAggDesc>() as u32;
        desc.group_capacity = 17;
        desc.key_count = 1;
        desc.keys[0].values.tag = crate::gpu::PgaccelValTag::Int32;
        desc.keys[0].cardinality = 17;
        desc.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        desc.measure_count = 1;
        desc.measures[0].agg_mask =
            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[0].state_bytes = 8;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        // SAFETY: canonical pointer-free zero-row descriptor.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("integer plan");
        let required = GroupedAggSession::memory_requirements(&plan, 4093).expect("forecast");
        assert_eq!(required.output_bytes, 17 * 25);
        assert_eq!(
            plan.one_shot_memory_requirements()
                .expect("one-shot forecast"),
            required
        );
        let before = (
            grouped_output_pool_snapshot(),
            grouped_workspace_pool_snapshot(),
        );
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - required.total_bytes as u64 + 1,
            budget,
        )
        .expect("one byte below working set");
        let failure = GroupedAggSession::start(&plan, 4093)
            .err()
            .expect("combined rejection");
        assert_eq!(failure.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(
            before,
            (
                grouped_output_pool_snapshot(),
                grouped_workspace_pool_snapshot()
            )
        );
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);

        // Both iterations perform real native finalization; missing GPU is a
        // failure, never a successful skip. Warm owners transfer pool charges.
        for warm in [false, true] {
            let before = (
                grouped_output_pool_snapshot(),
                grouped_workspace_pool_snapshot(),
            );
            let mut session = GroupedAggSession::start(&plan, 4093).expect("charged workspace");
            let mut output = GroupedAggOutputStorage::new(&plan).expect("charged output");
            assert_eq!(
                resident_live_bytes(),
                baseline + required.total_bytes as u64
            );
            if warm {
                assert_eq!(grouped_output_pool_snapshot().hits, before.0.hits + 1);
                assert_eq!(grouped_workspace_pool_snapshot().hits, before.1.hits + 1);
            }
            let outcome = session
                .finalize(&plan, &mut output)
                .expect("real empty-input finalization");
            let GroupedAggOutcome::Complete(result) = outcome else {
                panic!("integer output cannot require spatial recheck");
            };
            assert_eq!(result.emitted_group_count, 0);
            assert_eq!(result.selected_count, 0);
            assert!(
                output
                    .active_groups()
                    .expect("active bitmap")
                    .iter()
                    .all(|byte| *byte == 0)
            );
            assert!(
                output
                    .measure_count(0)
                    .expect("COUNT lane")
                    .iter()
                    .all(|count| *count == 0)
            );
            drop(output);
            drop(session);
            assert_eq!(
                resident_live_bytes(),
                baseline + required.total_bytes as u64
            );
        }
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
        Spi::run("SET pg_accel.resident_memory_budget_mb=0").expect("zero budget");
        assert_eq!(
            plan.one_shot_memory_requirements()
                .expect("unfunded forecast"),
            required
        );
        assert_eq!(
            GroupedAggOutputStorage::new(&plan)
                .err()
                .expect("output rejected")
                .status,
            GpuStatusDetail::OutOfMemory
        );
        assert_eq!(
            GroupedAggWorkspace::allocate(&plan)
                .err()
                .expect("workspace rejected")
                .status,
            GpuStatusDetail::OutOfMemory
        );
        assert_eq!(
            GroupedAggSession::start(&plan, 4093)
                .err()
                .expect("session rejected")
                .status,
            GpuStatusDetail::OutOfMemory
        );
        assert_eq!(resident_live_bytes(), baseline);
        Spi::run("RESET pg_accel.resident_memory_budget_mb").expect("restore budget");
    }

    fn verify_descriptor(
        table: &str,
        keys: usize,
        values: usize,
        count_star: bool,
        count_values: bool,
    ) {
        use crate::engine::spec::{
            AggOutputProjection, AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind,
            AggregateOutput, AggregateSource, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeyRef,
            GroupKeySource, MeasureExpr, MeasureSpec,
        };
        let relation_oid = u32::from(
            Spi::get_one::<pg_sys::Oid>(&format!("SELECT '{table}'::regclass::oid"))
                .expect("descriptor relation lookup")
                .expect("descriptor relation OID"),
        );
        let column = |attno, type_oid| ColumnRef {
            relation_oid,
            attno,
            type_oid,
        };
        let mut spec = AggQuerySpec {
            fact_rel: relation_oid,
            group_keys: vec![],
            measures: vec![],
            fact_filter: FilterSpec::None,
            star_dims: vec![],
            having: None,
        };
        let mut projection = AggOutputProjection { slots: vec![] };
        let mut selects = Vec::new();
        let mut groups = Vec::new();
        for index in 0..keys {
            spec.group_keys.push(GroupKeyRef {
                source: GroupKeySource::FactColumn(column(index as i32 + 1, 23)),
                type_oid: 23,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            });
            projection.slots.push(AggOutputSlot {
                source: AggOutputSource::GroupKey {
                    key_index: index as u32,
                },
                source_type_oid: 23,
                result_type_oid: 23,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            });
            selects.push(format!("int4send(k{})", index + 1));
            groups.push(format!("k{}", index + 1));
        }
        for index in 0..values {
            let outputs = [AggregateKind::Sum, AggregateKind::Avg, AggregateKind::Count]
                .into_iter()
                .filter(|kind| count_values || *kind != AggregateKind::Count)
                .map(|kind| AggregateOutput {
                    source: AggregateSource::Value,
                    kind,
                })
                .collect();
            spec.measures.push(MeasureSpec {
                expression: MeasureExpr::Column(column(index as i32 + 4, 1700)),
                outputs,
                filter: FilterSpec::None,
            });
            for (kind, aggregate, sender, result_type_oid) in [
                (AggregateKind::Sum, "sum", "numeric_send", 1700),
                (AggregateKind::Avg, "avg", "numeric_send", 1700),
                (AggregateKind::Count, "count", "int8send", 20),
            ] {
                if kind == AggregateKind::Count && !count_values {
                    continue;
                }
                projection.slots.push(AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: index as u32,
                        source: AggregateSource::Value,
                        kind,
                    },
                    source_type_oid: 1700,
                    result_type_oid,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: kind != AggregateKind::Count,
                });
                selects.push(format!("{sender}({aggregate}(v{}))", index + 1));
            }
        }
        if count_star {
            spec.measures.push(MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            });
            projection.slots.push(AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: values as u32,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: 0,
                result_type_oid: 20,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            });
            selects.push("int8send(count(*))".to_owned());
        }
        let grouping = if keys == 0 {
            String::new()
        } else {
            format!(" GROUP BY {}", groups.join(","))
        };
        let query = format!("SELECT {} FROM {table}{grouping}", selects.join(","));
        let mut expected = Spi::connect(|client| {
            client
                .select(&query, None, &[])
                .expect("PostgreSQL aggregate reference")
                .map(|row| {
                    (1..=selects.len())
                        .map(|index| row.get::<Vec<u8>>(index).expect("reference wire value"))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        expected.sort();
        let request = crate::engine::residency::SelectedRelation {
            relid: pg_sys::Oid::from(relation_oid),
            columns: (1..=keys)
                .chain(4..(4 + values))
                .map(|attno| attno as i16)
                .collect(),
        };
        let estimate = crate::engine::residency::estimate_selected_relation(&request)
            .expect("resident numeric forecast metadata");
        let columns = estimate
            .resident_numeric_columns
            .as_deref()
            .expect("complete numeric metadata");
        let mut capacity = 1_usize;
        for key in &groups {
            let cardinality = Spi::get_one::<i64>(&format!("SELECT count(DISTINCT {key}) + CASE WHEN count(*)>count({key}) THEN 1 ELSE 0 END FROM {table}"))
                .expect("reference dictionary cardinality").expect("cardinality nonnull").max(1);
            capacity *= cardinality as usize;
        }
        crate::gpu::cleanup_grouped_allocation_pools();
        let before_output = crate::gpu::grouped_output_pool_snapshot();
        let before_workspace = crate::gpu::grouped_workspace_pool_snapshot();
        let forecast = crate::engine::executor::agg::numeric_descriptor_memory_requirements(
            &spec,
            columns,
            estimate
                .resident_nullable_columns
                .as_deref()
                .expect("column NULL metadata"),
            capacity,
        )
        .expect("native-backed numeric forecast");
        assert_eq!(crate::gpu::grouped_output_pool_snapshot(), before_output);
        assert_eq!(
            crate::gpu::grouped_workspace_pool_snapshot(),
            before_workspace
        );
        let actual =
            crate::engine::executor::agg::test_execute_numeric_descriptor(spec, projection);
        assert_eq!(
            crate::gpu::grouped_output_pool_snapshot().fresh_allocation_bytes
                - before_output.fresh_allocation_bytes,
            forecast.output_bytes as u64,
            "forecast versus real result allocations"
        );
        assert_eq!(
            crate::gpu::grouped_workspace_pool_snapshot().fresh_allocation_bytes
                - before_workspace.fresh_allocation_bytes,
            forecast.workspace_bytes as u64,
            "forecast versus real bounded scratch allocation"
        );
        assert_eq!(actual, expected, "exact descriptor output: {query}");
    }

    #[pg_test]
    fn pg_numeric_descriptor_binding_and_materialization() {
        configure();
        let previous = crate::engine::executor::agg::configure_dense_dispatch_test(4093, 0, 0);
        Spi::run("CREATE TEMP TABLE numeric_descriptor(k1 int, k2 int, k3 int, v1 numeric, v2 numeric, v3 numeric, v4 numeric); \
            INSERT INTO numeric_descriptor SELECT CASE WHEN i%13=0 THEN NULL ELSE i%3 END, i%2, i%5, \
            CASE WHEN i%7=0 THEN NULL ELSE (i-50000)::numeric/100 END, \
            CASE WHEN i%3=0 THEN NULL ELSE (i%31)::numeric/10000 END, \
            CASE WHEN i%11=0 THEN NULL ELSE -(i%17)::numeric/3 END, \
            CASE WHEN i%23=0 THEN NULL ELSE (i%73)::numeric*1e24 END \
            FROM generate_series(1,100017) g(i); ANALYZE numeric_descriptor; \
            SELECT pg_accel_pin('numeric_descriptor'::regclass, ARRAY['k1','k2','k3','v1','v2','v3','v4'])")
            .expect("numeric descriptor fixture");
        for keys in 0..=3 {
            for values in 1..=4 {
                verify_descriptor("numeric_descriptor", keys, values, values < 4, true);
            }
        }
        verify_descriptor("numeric_descriptor", 0, 1, false, false);
        verify_descriptor("numeric_descriptor", 3, 4, false, false);
        for values in [
            "(0.0000,NULL,0.0000,NULL)",
            "('Infinity','-Infinity','NaN',NULL),(1,2,3,NULL)",
            "('Infinity','Infinity',NULL,1),('-Infinity','NaN',NULL,2)",
            "('1e131071','1e-16383',NULL,1),('-1e131071','-1e-16383',NULL,2)",
        ] {
            Spi::run(&format!("TRUNCATE numeric_descriptor; INSERT INTO numeric_descriptor(v1,v2,v3,v4) VALUES {values}; \
                ANALYZE numeric_descriptor; SELECT pg_accel_pin('numeric_descriptor'::regclass, ARRAY['k1','k2','k3','v1','v2','v3','v4'])"))
                .expect("numeric descriptor edge fixture");
            verify_descriptor("numeric_descriptor", 0, 4, false, true);
            verify_descriptor("numeric_descriptor", 3, 3, true, true);
        }
        Spi::run("TRUNCATE numeric_descriptor; ANALYZE numeric_descriptor; \
            SELECT pg_accel_pin('numeric_descriptor'::regclass, ARRAY['k1','k2','k3','v1','v2','v3','v4'])")
            .expect("empty descriptor fixture");
        verify_descriptor("numeric_descriptor", 0, 4, false, true);
        verify_descriptor("numeric_descriptor", 0, 3, true, false);
        verify_descriptor("numeric_descriptor", 3, 3, true, true);
        Spi::run("SELECT pg_accel_unpin('numeric_descriptor'::regclass); SELECT pg_accel_evict('numeric_descriptor'::regclass); DROP TABLE numeric_descriptor")
            .expect("descriptor cleanup");
        crate::engine::executor::agg::configure_dense_dispatch_test(
            previous.0, previous.1, previous.2,
        );
    }

    #[pg_test]
    fn pg_numeric_resident_wire_roundtrip_full_precision_and_toast() {
        configure();
        Spi::run(
            "CREATE TEMP TABLE numeric_wire(v numeric); \
            ALTER TABLE numeric_wire ALTER COLUMN v SET STORAGE EXTERNAL; \
            INSERT INTO numeric_wire VALUES (NULL),(0),(0.0000),(-0.00012345), \
            (9007199254740993),('-Infinity'),('Infinity'),('NaN'),('1e131071'),('1e-16383'), \
            (12345::numeric(5,-2)),(0.00123::numeric(3,5)), \
            ((repeat('9',131072)||'.'||repeat('9',16383))::numeric); \
            INSERT INTO numeric_wire SELECT (repeat(i::text,1000)||'.12500')::numeric \
              FROM generate_series(1,37) g(i); \
            ANALYZE numeric_wire",
        )
        .expect("valid NUMERIC test fixture");
        assert!(
            Spi::get_one::<bool>(
                "SELECT pg_relation_size(reltoastrelid)>0 FROM pg_class \
            WHERE oid='numeric_wire'::regclass"
            )
            .expect("valid NUMERIC test fixture")
            .expect("valid NUMERIC test fixture")
        );
        let data = verify_roundtrip("numeric_wire");
        assert_eq!(data.rows.len(), 50);
        assert!(data.rows.iter().any(|row| row.ndigits > 32767));
        assert!(data.nulls.is_some());
        Spi::run(
            "SELECT pg_accel_unpin('numeric_wire'::regclass); \
            SELECT pg_accel_evict('numeric_wire'::regclass); DROP TABLE numeric_wire",
        )
        .expect("valid NUMERIC test fixture");

        Spi::run(
            "CREATE TEMP TABLE numeric_compressed(v numeric); \
            ALTER TABLE numeric_compressed ALTER COLUMN v SET STORAGE MAIN; \
            ALTER TABLE numeric_compressed ALTER COLUMN v SET COMPRESSION pglz; \
            INSERT INTO numeric_compressed VALUES ((repeat('1234',10000)||'.1200')::numeric); \
            ANALYZE numeric_compressed",
        )
        .expect("valid NUMERIC test fixture");
        assert!(
            Spi::get_one::<bool>("SELECT pg_column_size(v)<1000 FROM numeric_compressed")
                .expect("valid NUMERIC test fixture")
                .expect("valid NUMERIC test fixture")
        );
        assert!(verify_roundtrip("numeric_compressed").nulls.is_none());
        Spi::run(
            "SELECT pg_accel_unpin('numeric_compressed'::regclass); \
            SELECT pg_accel_evict('numeric_compressed'::regclass); DROP TABLE numeric_compressed",
        )
        .expect("valid NUMERIC test fixture");
    }

    #[pg_test]
    fn pg_numeric_resident_refresh_rollback_nulls_and_empty() {
        configure();
        Spi::run(
            "CREATE TEMP TABLE numeric_lifecycle(v numeric); \
            INSERT INTO numeric_lifecycle SELECT (i*100003)::numeric/power(10::numeric,i%31) \
                FROM generate_series(1,10017) g(i); ANALYZE numeric_lifecycle",
        )
        .expect("valid NUMERIC test fixture");
        assert!(verify_roundtrip("numeric_lifecycle").nulls.is_none());
        let request = crate::engine::residency::SelectedRelation {
            relid: Spi::get_one::<pg_sys::Oid>("SELECT 'numeric_lifecycle'::regclass::oid")
                .expect("lifecycle OID")
                .expect("lifecycle OID"),
            columns: vec![1],
        };
        let stale = crate::engine::residency::estimate_selected_relation(&request)
            .expect("initial planner snapshot");
        Spi::run("UPDATE numeric_lifecycle SET v = -v * 100000.00001 WHERE v < 1; \
            DO $$ BEGIN BEGIN UPDATE numeric_lifecycle SET v='NaN'; \
            RAISE EXCEPTION USING ERRCODE='P0002'; EXCEPTION WHEN no_data_found THEN NULL; END; END $$")
            .expect("valid NUMERIC test fixture");
        assert!(!crate::engine::residency::revalidate_loaded_estimates(
            &[request],
            &[stale]
        ));
        assert!(verify_roundtrip("numeric_lifecycle").nulls.is_none());
        Spi::run("UPDATE numeric_lifecycle SET v=NULL").expect("valid NUMERIC test fixture");
        let data = verify_roundtrip("numeric_lifecycle");
        assert!(data.digits.is_empty());
        assert_eq!(data.device_bytes(), Some(10017 * 17));
        Spi::run("DELETE FROM numeric_lifecycle").expect("valid NUMERIC test fixture");
        assert!(verify_roundtrip("numeric_lifecycle").rows.is_empty());
        Spi::run(
            "SELECT pg_accel_unpin('numeric_lifecycle'::regclass); \
            SELECT pg_accel_evict('numeric_lifecycle'::regclass); DROP TABLE numeric_lifecycle",
        )
        .expect("valid NUMERIC test fixture");
    }
}

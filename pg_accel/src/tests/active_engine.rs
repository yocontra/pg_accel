//! End-to-end coverage for selected Resident v2 descriptor execution.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn gpu_device_available() -> bool {
        Spi::get_one::<String>("SELECT DISTINCT source FROM pg_accel_device_limits()")
            .ok()
            .flatten()
            .is_some_and(|source| source == "hardware_derived")
    }

    fn begin_gpu_test() -> bool {
        if !gpu_device_available() {
            pgrx::notice!("skipping active Resident v2 execution test: no GPU device");
            return false;
        }
        Spi::run("SELECT pg_advisory_xact_lock(882201)")
            .expect("GPU test advisory lock should succeed");
        Spi::run(
            "SET LOCAL pg_accel.enabled = on; \
             SET LOCAL pg_accel.gpu_enabled = on; \
             SET LOCAL pg_accel.auto_load = off; \
             SET LOCAL pg_accel.min_batch_size = DEFAULT",
        )
        .expect("Resident v2 test settings should apply");
        // admission-audit-allow: select the real descriptor executor independent
        // of host-specific calibration; default-cost admission has separate tests.
        Spi::run("SET LOCAL pg_accel.cost_multiplier = 0.1")
            .expect("descriptor executor contract cost should apply");
        true
    }

    fn device_rows() -> i64 {
        Spi::get_one::<i64>(
            "SELECT greatest(262144::bigint, max(value::bigint) + 65536) \
             FROM pg_accel_device_limits() \
             WHERE name IN ('gpu_min_rows', 'gpu_hash_agg_min_rows')",
        )
        .expect("device-row query should succeed")
        .expect("device-row query should return a value")
    }

    fn explain_text(query: &str, analyze: bool) -> String {
        Spi::connect(|client| {
            let prefix = if analyze {
                "EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, FORMAT TEXT)"
            } else {
                "EXPLAIN (VERBOSE, COSTS OFF, FORMAT TEXT)"
            };
            client
                .select(&format!("{prefix} {query}"), None, &[])
                .expect("EXPLAIN should succeed")
                .filter_map(|row| row.get::<String>(1).ok().flatten())
                .collect::<Vec<_>>()
                .join("\n")
                .to_lowercase()
        })
    }

    fn result_rows(query: &str) -> Vec<String> {
        Spi::connect(|client| {
            client
                .select(
                    &format!("SELECT row_to_json(result_row)::text FROM ({query}) result_row"),
                    None,
                    &[],
                )
                .expect("result query should succeed")
                .map(|row| {
                    row.get::<String>(1)
                        .expect("JSON result should decode")
                        .expect("JSON result should not be NULL")
                })
                .collect()
        })
    }

    fn kernel_executions() -> i64 {
        Spi::get_one::<i64>("SELECT pg_accel_kernel_executions()")
            .expect("kernel execution query should succeed")
            .expect("kernel execution count should not be NULL")
    }

    fn stock_exec_count() -> i64 {
        Spi::get_one::<i64>("SELECT stock_exec_count FROM pg_accel_stats()")
            .expect("stock executor counter query should succeed")
            .expect("stock executor counter should not be NULL")
    }

    fn assert_planner_decline(query: &str, expected_reason: &str) {
        Spi::run("SET LOCAL pg_accel.enabled = on").expect("acceleration should be enabled");
        let plan = explain_text(query, false);
        let reason = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("planner rejection reason should be readable");
        assert!(
            !plan.contains("custom scan (gpuaccelagg)"),
            "declined query unexpectedly selected pg_accel:\n{plan}"
        );
        assert_eq!(reason.as_deref(), Some(expected_reason));
    }

    fn assert_native_decline_matches_native(query: &str, expected_reason: &str) {
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("native baseline should be selectable");
        let native = result_rows(query);

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("acceleration should be enabled");
        let plan = explain_text(query, false);
        let reason = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("planner rejection reason should be readable");
        assert!(
            !plan.contains("custom scan (gpuaccelagg)"),
            "declined query unexpectedly selected pg_accel:\n{plan}"
        );
        assert_eq!(reason.as_deref(), Some(expected_reason));

        let kernels_before = kernel_executions();
        let fallback_before = stock_exec_count();
        let enabled = result_rows(query);
        assert_eq!(
            enabled, native,
            "native decline result differs with pg_accel enabled"
        );
        assert_eq!(
            kernel_executions(),
            kernels_before,
            "native decline dispatched a kernel"
        );
        assert_eq!(
            stock_exec_count(),
            fallback_before,
            "native decline entered stock fallback"
        );
    }

    fn assert_selected_matches_native(query: &str) -> Vec<String> {
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("native baseline should be selectable");
        let native = result_rows(query);

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("acceleration should be enabled");
        let plan = explain_text(query, false);
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("planner rejection reason should be readable");
        assert!(
            plan.contains("custom scan (gpuaccelagg)")
                && plan.contains("gpu resident pipeline: true")
                && plan.contains("gpu descriptor strategy: descriptor_grouped_aggregate"),
            "query must select the childless descriptor aggregate; rejection={rejection:?}:\n{plan}"
        );
        let before = kernel_executions();
        let accelerated = result_rows(query);
        let after = kernel_executions();
        assert!(
            after > before,
            "selected descriptor aggregate did not dispatch: before={before}, after={after}"
        );
        assert_eq!(
            accelerated, native,
            "descriptor result differs from PostgreSQL"
        );
        accelerated
    }

    fn pin(table: &str, columns: &[&str], expected_rows: i64) {
        let columns = columns
            .iter()
            .map(|column| format!("'{column}'"))
            .collect::<Vec<_>>()
            .join(",");
        let loaded = Spi::get_one::<i64>(&format!(
            "SELECT pg_accel_pin('{table}'::regclass, ARRAY[{columns}])"
        ))
        .expect("pin query should succeed")
        .expect("pin query should return a row count");
        assert_eq!(loaded, expected_rows, "pin returned the wrong row count");
    }

    #[pg_test]
    fn selected_descriptor_types_binary_empty_and_lifecycle() {
        if !begin_gpu_test() {
            return;
        }
        let rows = device_rows();
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE pgaccel_active_descriptor_types (\
               id int4 NOT NULL, g_i2 int2, g_i8 int8, g_bool bool, g_date date, \
               g_ts timestamp, g_tstz timestamptz, g_f4 float4, g_f8 float8, \
               g_varchar varchar(16), g_bpchar char(6), lhs int4 NOT NULL, \
               rhs int4 NOT NULL, nullable_v int4); \
             INSERT INTO pgaccel_active_descriptor_types \
             SELECT i, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE (i % 5)::int2 END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE (i % 7)::int8 END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE i % 2 = 0 END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE DATE '2020-01-01' + (i % 4) END, \
                    CASE WHEN i % 97 = 0 THEN NULL \
                         ELSE TIMESTAMP '2020-01-01' + (i % 3) * INTERVAL '1 hour' END, \
                    CASE WHEN i % 97 = 0 THEN NULL \
                         ELSE TIMESTAMPTZ '2020-01-01 00:00:00+00' \
                              + (i % 4) * INTERVAL '1 hour' END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE ((i % 5)::float4 / 2.0) END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE ((i % 6)::float8 / 3.0) END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE ('v' || (i % 7))::varchar(16) END, \
                    CASE WHEN i % 97 = 0 THEN NULL ELSE ('c' || (i % 5))::char(6) END, \
                    (i % 1000) - 500, (i % 7) - 3, \
                    CASE WHEN i % 11 = 0 THEN NULL ELSE (i % 101) - 50 END \
             FROM generate_series(1, {rows}) AS g(i); \
             ANALYZE pgaccel_active_descriptor_types"
        ))
        .expect("typed descriptor fixture should be created");

        let columns = [
            "g_i2",
            "g_i8",
            "g_bool",
            "g_date",
            "g_ts",
            "g_tstz",
            "g_f4",
            "g_f8",
            "g_varchar",
            "g_bpchar",
            "lhs",
            "rhs",
            "nullable_v",
        ];
        pin("pgaccel_active_descriptor_types", &columns, rows);

        let dictionary_query = "SELECT g_i2, g_varchar, g_bpchar, \
                    sum(lhs * rhs) AS product_sum, count(*) AS rows, \
                    count(nullable_v) AS present, min(nullable_v) AS minimum \
             FROM pgaccel_active_descriptor_types \
             GROUP BY g_i2, g_varchar, g_bpchar \
             ORDER BY g_i2 NULLS FIRST, g_varchar NULLS FIRST, g_bpchar NULLS FIRST";

        let temporal_query = "SELECT g_bool, g_date, g_ts, \
                    max(nullable_v) AS maximum, min(g_i8) AS min_i8, \
                    max(g_i8) AS max_i8, count(*) AS rows \
             FROM pgaccel_active_descriptor_types \
             GROUP BY g_bool, g_date, g_ts \
             ORDER BY g_bool NULLS FIRST, g_date NULLS FIRST, g_ts NULLS FIRST";

        let scalar_query = "SELECT g_i8, g_tstz, g_f4, \
                    min(g_f8) AS min_f8, max(g_f8) AS max_f8, \
                    count(*) AS rows, sum(lhs) AS lhs_sum \
             FROM pgaccel_active_descriptor_types \
             GROUP BY g_i8, g_tstz, g_f4 \
             ORDER BY g_i8 NULLS FIRST, g_tstz NULLS FIRST, g_f4 NULLS FIRST";

        let float_key_query = "SELECT g_f8, max(nullable_v) AS maximum, \
                    count(*) AS rows, sum(lhs * rhs) AS product_sum \
             FROM pgaccel_active_descriptor_types \
             GROUP BY g_f8 ORDER BY g_f8 NULLS FIRST";

        let queries = [
            dictionary_query,
            temporal_query,
            scalar_query,
            float_key_query,
        ];
        for query in queries {
            assert!(!assert_selected_matches_native(query).is_empty());
        }

        Spi::run("DELETE FROM pgaccel_active_descriptor_types")
            .expect("deleting the typed fixture should succeed");
        pin("pgaccel_active_descriptor_types", &columns, 0);
        for query in queries {
            assert!(assert_selected_matches_native(query).is_empty());
        }

        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pg_accel_unpin('pgaccel_active_descriptor_types'::regclass)"
            )
            .expect("unpin should succeed"),
            Some(true)
        );
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned FROM pg_accel_resident_status() \
                 WHERE relid = 'pgaccel_active_descriptor_types'::regclass"
            )
            .expect("unpinned status should be readable"),
            Some(false)
        );
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pg_accel_evict('pgaccel_active_descriptor_types'::regclass)"
            )
            .expect("evict should succeed"),
            Some(true)
        );
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM pg_accel_resident_status() \
                 WHERE relid = 'pgaccel_active_descriptor_types'::regclass"
            )
            .expect("evicted status should be readable"),
            Some(0)
        );
    }

    #[pg_test]
    fn selected_int8_star_membership_preserves_nulls_and_multiplicity() {
        if !begin_gpu_test() {
            return;
        }
        let rows = device_rows();
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE pgaccel_active_int8_fact (\
               k int8, payload int4 NOT NULL); \
             CREATE UNLOGGED TABLE pgaccel_active_int8_dim (k int8); \
             INSERT INTO pgaccel_active_int8_fact \
             SELECT -7::int8, 1 FROM generate_series(1, {rows}); \
             INSERT INTO pgaccel_active_int8_fact VALUES \
               (NULL, 2), (42, 3), ('-9223372036854775808', 4), \
               ('9223372036854775807', 5), (-7, 6); \
             INSERT INTO pgaccel_active_int8_dim VALUES \
               ('-9223372036854775808'), (-7), (-7), \
               ('9223372036854775807'), (NULL), (NULL); \
             ANALYZE pgaccel_active_int8_fact; \
             ANALYZE pgaccel_active_int8_dim"
        ))
        .expect("INT8 star membership fixture should be created");

        pin("pgaccel_active_int8_fact", &["k", "payload"], rows + 5);
        pin("pgaccel_active_int8_dim", &["k"], 6);

        let query = "SELECT count(f.payload) AS matched_rows \
                     FROM pgaccel_active_int8_fact AS f \
                     JOIN pgaccel_active_int8_dim AS d ON f.k = d.k";
        Spi::run("SET LOCAL pg_accel.enabled = off")
            .expect("native INT8 membership baseline should be selectable");
        let expected = rows * 2 + 4;
        let native = Spi::get_one::<i64>(query)
            .expect("native INT8 membership query should succeed")
            .expect("native INT8 membership count should not be NULL");
        assert_eq!(
            native, expected,
            "native fixture must exercise duplicate fanout and reject NULL/missing keys"
        );

        Spi::run("SET LOCAL pg_accel.enabled = on")
            .expect("INT8 membership acceleration should be enabled");
        let plan = explain_text(query, false);
        assert!(
            plan.contains("custom scan (gpuaccelagg)")
                && plan.contains("gpu descriptor star dimensions:")
                && plan.contains("type=20")
                && plan.contains("multiplicity=counted"),
            "single exact INT8 join must select counted resident membership:\n{plan}"
        );
        let before = kernel_executions();
        let accelerated = Spi::get_one::<i64>(query)
            .expect("selected INT8 membership query should succeed")
            .expect("selected INT8 membership count should not be NULL");
        let after = kernel_executions();
        assert!(
            after > before,
            "selected INT8 membership did not dispatch: before={before}, after={after}"
        );
        assert_eq!(
            accelerated, native,
            "selected INT8 membership result differs from PostgreSQL"
        );
    }

    #[pg_test]
    fn one_scalar_range_selects_while_multiple_ranges_decline_with_native_parity() {
        if !begin_gpu_test() {
            return;
        }
        let rows = device_rows();
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE pgaccel_active_and_ranges (\
               product_id int4 NOT NULL, price int4, quantity int4 NOT NULL); \
             INSERT INTO pgaccel_active_and_ranges \
             SELECT (g % 256)::int4, \
                    CASE WHEN g % 97 = 0 THEN NULL ELSE (1 + (g % 1000))::int4 END, \
                    (1 + ((g / 256) % 10))::int4 \
             FROM generate_series(1, {rows}) AS g; \
             ANALYZE pgaccel_active_and_ranges"
        ))
        .expect("range predicate fixture should be created");
        pin(
            "pgaccel_active_and_ranges",
            &["product_id", "price", "quantity"],
            rows,
        );

        let selected_query = "SELECT product_id, sum(price * quantity) AS sum, count(*) AS count \
             FROM pgaccel_active_and_ranges \
             WHERE price <= 800 \
             GROUP BY product_id ORDER BY product_id";
        let fallback_before = stock_exec_count();
        assert_eq!(assert_selected_matches_native(selected_query).len(), 256);
        assert_eq!(
            stock_exec_count(),
            fallback_before,
            "selected single range execution must not enter the stock executor"
        );

        assert_native_decline_matches_native(
            "SELECT product_id, sum(price * quantity) AS sum, count(*) AS count \
             FROM pgaccel_active_and_ranges \
             WHERE price >= 200 AND price <= 800 \
             GROUP BY product_id ORDER BY product_id",
            "shape_multiple_range_predicates",
        );

        assert_planner_decline(
            "SELECT product_id, sum(price * quantity), count(*) \
             FROM pgaccel_active_and_ranges \
             WHERE price >= 200 AND quantity <= 8 GROUP BY product_id",
            "shape_multi_filter_relation",
        );
        assert_planner_decline(
            "SELECT product_id, sum(price * quantity), count(*) \
             FROM pgaccel_active_and_ranges \
             WHERE price < 200 OR price > 800 GROUP BY product_id",
            "shape_unsupported_predicate",
        );
        assert_planner_decline(
            "SELECT product_id, sum(price * quantity), count(*) \
             FROM pgaccel_active_and_ranges \
             WHERE price >= 900 AND price <= 100 GROUP BY product_id",
            "shape_multiple_range_predicates",
        );
    }
}

//! Fixed diagnostic1 fixtures, not a new headline workload.
#[pgrx::pg_schema]
mod paired_tests {
    use pgrx::prelude::*;
    #[pg_extern]
    fn run_paired_where_candidates() -> bool {
        use super::super::paired_sink::{SharedSink, Sink};
        let path = std::env::var("PGACCEL_PAIRED_RECEIPTS").expect("fixed owned receipt directory");
        let sink: SharedSink = std::rc::Rc::new(std::cell::RefCell::new(
            Sink::open_owned(std::path::Path::new(&path)).expect("fresh bounded durable sink"),
        ));
        sink.borrow_mut().record("header",&serde_json::json!({
            "schema":1,"scheduled_executions":100,"jit_probes":4,
            "file_limit":super::super::paired_sink::FILE_LIMIT,
            "total_limit":super::super::paired_sink::TOTAL_LIMIT,
            "record_limit":super::super::paired_sink::RECORD_LIMIT,
            "row_limit":super::super::paired_sink::ROW_LIMIT,
            "timer":"descriptor executor including setup/EOF/close/frees; excludes SQL planning; diagnostic capture overhead included"
        })).expect("durable header");
        super::super::paired_sink::with_sink(sink.clone(), || {
            pgrx::pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                execute_fixed_fixtures(sink.clone());
                sink.borrow_mut().record("complete",&serde_json::json!({"fixtures":2,"scheduled_executions":100,"jit_probes":4})).expect("durable complete");
            })).catch_others(|caught| {
                super::super::paired_sink::active_error_before_rethrow(&format!("fixture setup/capture/invalidation/execution: {caught:?}"));
                caught.rethrow()
            }).execute();
        });
        true
    }
    fn execute_fixed_fixtures(sink: super::super::paired_sink::SharedSink) {
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET work_mem='64kB'").expect("defaults");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE raw_where AS SELECT \
            (CASE WHEN i%97=0 THEN NULL ELSE g*3-24582 END)::smallint k, \
            CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/16388)%4<>0 END w, \
            CASE WHEN g=0 THEN false WHEN i%11=0 THEN NULL ELSE i%3<>0 END f, \
            CASE WHEN i%7<>0 THEN i%2=0 END vb, \
            (CASE WHEN i%7<>0 THEN i%1000-500 END)::smallint vi2, \
            (CASE WHEN i%7<>0 THEN i%1000-500 END)::integer vi4, \
            CASE WHEN i%7<>0 THEN i%1000-500 END vi8, \
            (CASE WHEN i%7<>0 THEN i%1000-500 END)::real vf4, \
            (CASE WHEN i%7<>0 THEN i%1000-500 END)::double precision vf8, \
            CASE WHEN i%7<>0 THEN DATE '2000-01-01'+(i%1000)::int END vd, \
            CASE WHEN i%7<>0 THEN TIMESTAMP '2000-01-01'+(i%1000)*INTERVAL '1 second' END vt, \
            CASE WHEN i%7<>0 THEN TIMESTAMPTZ '2000-01-01 UTC'+(i%1000)*INTERVAL '1 second' END vz \
            FROM (SELECT i, CASE WHEN i>16388 AND i%5=0 THEN 0 ELSE (i-1)%16388 END g \
            FROM generate_series(1,{rows}::bigint)s(i))r; ANALYZE raw_where; \
            SELECT pg_accel_pin('raw_where',ARRAY['k','w','f','vb','vi2','vi4','vi8','vf4','vf8','vd','vt','vz'])"))
            .expect("unchanged first INT2 fixture");
        super::super::paired_driver::run_fixture(
            &mut super::super::paired_backend::PgBackend::new(sink.clone(), "int2_count_star"),
            "int2_count_star",
            "SELECT k,count(*) n FROM raw_where WHERE w GROUP BY k",
        )
        .expect("fixed first candidate comparison");
        sink.borrow_mut()
            .record(
                "fixture_complete",
                &serde_json::json!({"fixture":"int2_count_star"}),
            )
            .expect("durable fixture terminal");
        Spi::run("SELECT pg_accel_unpin('raw_where'); SELECT pg_accel_evict('raw_where'); DROP TABLE raw_where").expect("release first owned fixture");
        Spi::run(&format!("CREATE TEMP TABLE where_masks AS SELECT (i%16388)*1000000-8194000000 k, CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/16388)%4<>0 END w, CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END f FROM generate_series(1,{rows}::bigint)s(i); ANALYZE where_masks; SELECT pg_accel_pin('where_masks',ARRAY['k','w','f'])"))
            .expect("unchanged independent masks fixture");
        super::super::paired_driver::run_fixture(
            &mut super::super::paired_backend::PgBackend::new(sink.clone(), "int8_count_filter"),
            "int8_count_filter",
            "SELECT k,count(f) FILTER (WHERE f) n FROM where_masks WHERE w GROUP BY k",
        )
        .expect("fixed second candidate comparison");
        sink.borrow_mut()
            .record(
                "fixture_complete",
                &serde_json::json!({"fixture":"int8_count_filter"}),
            )
            .expect("durable fixture terminal");
        Spi::run("SELECT pg_accel_unpin('where_masks'); SELECT pg_accel_evict('where_masks'); DROP TABLE where_masks; RESET work_mem").expect("release second owned fixture");
    }
}

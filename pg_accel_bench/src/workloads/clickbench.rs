//! Deterministic ClickBench-style event analytics.
//!
//! The schema is deliberately compact enough for CI while retaining the
//! high-volume event-table characteristics that matter here: low-cardinality
//! dimensions, exact timestamps, dictionary-friendly strings, grouped
//! reductions, DISTINCT, FILTER, and top-N output. Results are reported as a
//! pg_accel selection or an honest PostgreSQL-native plan per query.

use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

fn clickbench_setup_sql(rows: usize) -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS clickbench_hits".to_owned(),
        "CREATE TABLE clickbench_hits (\
            event_time timestamp NOT NULL, \
            user_id int8 NOT NULL, \
            project_id int4 NOT NULL, \
            event_type text NOT NULL, \
            revenue int4 NOT NULL, \
            duration_ms int4 NOT NULL, \
            url text NOT NULL, \
            country text NOT NULL, \
            is_mobile bool NOT NULL\
        )"
        .to_owned(),
        format!(
            "INSERT INTO clickbench_hits \
             SELECT timestamp '2024-01-01 00:00:00' + ((i::int8 * 37) % 604800) * interval '1 second', \
                    ((i::int8 * 104729) % 2000000 + 1)::int8, \
                    ((i - 1) % 128 + 1)::int4, \
                    (ARRAY['view','click','purchase','signup','logout'])[(i % 5) + 1], \
                    CASE WHEN i % 5 = 2 THEN ((i::int8 * 17) % 10000)::int4 ELSE 0 END, \
                    ((i::int8 * 19) % 5000)::int4, \
                    '/page/' || ((i::int8 * 23) % 4096), \
                    (ARRAY['US','DE','IN','JP','BR','GB','CA','AU'])[(i % 8) + 1], \
                    i % 3 <> 0 \
             FROM generate_series(1, {rows}) AS g(i)"
        ),
        "ANALYZE clickbench_hits".to_owned(),
    ]
}

fn clickbench_cleanup_sql() -> Vec<String> {
    vec!["DROP TABLE IF EXISTS clickbench_hits".to_owned()]
}

fn fixture_oracle(rows: usize) -> ResultOracle {
    ResultOracle::one_row(
        "SELECT count(*)::int8 FROM clickbench_hits".to_owned(),
        vec![Value::I64(usize_to_i64(rows))],
    )
}

pub struct ClickbenchGroupedEvents;

impl Workload for ClickbenchGroupedEvents {
    fn name(&self) -> &'static str {
        "clickbench_grouped_events"
    }

    fn description(&self) -> &'static str {
        "ClickBench-style low-cardinality project/event grouped count and exact int4 revenue sum"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        clickbench_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT project_id, event_type, COUNT(*) AS events, SUM(revenue) AS revenue \
         FROM clickbench_hits \
         WHERE project_id BETWEEN 1 AND 64 \
           AND event_type IN ('view', 'click', 'purchase') \
         GROUP BY project_id, event_type"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        clickbench_cleanup_sql()
    }
}

pub struct ClickbenchDistinctUsers;

impl Workload for ClickbenchDistinctUsers {
    fn name(&self) -> &'static str {
        "clickbench_distinct_users"
    }

    fn description(&self) -> &'static str {
        "ClickBench-style hourly/country DISTINCT users with a filtered mobile count"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        clickbench_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT date_trunc('hour', event_time) AS event_hour, country, \
                COUNT(DISTINCT user_id) AS users, \
                COUNT(*) FILTER (WHERE is_mobile) AS mobile_events \
         FROM clickbench_hits \
         GROUP BY event_hour, country"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        clickbench_cleanup_sql()
    }
}

pub struct ClickbenchTopUrls;

impl Workload for ClickbenchTopUrls {
    fn name(&self) -> &'static str {
        "clickbench_top_urls"
    }

    fn description(&self) -> &'static str {
        "ClickBench-style filtered URL aggregation with fully consumed top-N output"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        clickbench_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT url, COUNT(*) AS hits, SUM(duration_ms) AS total_duration \
         FROM clickbench_hits \
         WHERE duration_ms BETWEEN 10 AND 3000 \
           AND project_id IN (3, 7, 11, 19, 23, 31) \
         GROUP BY url \
         ORDER BY hits DESC, url \
         LIMIT 100"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        clickbench_cleanup_sql()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clickbench_family_is_deterministic_and_covers_selected_and_native_shapes() {
        for workload in [
            &ClickbenchGroupedEvents as &dyn Workload,
            &ClickbenchDistinctUsers as &dyn Workload,
            &ClickbenchTopUrls as &dyn Workload,
        ] {
            assert_eq!(workload.setup_sql(10_000), workload.setup_sql(10_000));
            assert!(workload.query_sql().contains("clickbench_hits"));
            assert_eq!(
                workload
                    .result_oracle(10_000)
                    .expect("fixture oracle")
                    .expected_row,
                vec![Value::I64(10_000)]
            );
        }
        assert!(ClickbenchGroupedEvents.query_sql().contains("SUM(revenue)"));
        assert!(ClickbenchDistinctUsers.query_sql().contains("DISTINCT"));
        assert!(ClickbenchTopUrls.query_sql().contains("LIMIT 100"));
        assert!(
            ClickbenchTopUrls
                .setup_sql(100_000)
                .iter()
                .any(|sql| sql.contains("i::int8 * 104729")),
            "fixture arithmetic must widen before multiplication"
        );
    }
}

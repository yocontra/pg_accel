use super::Workload;

/// Tests sort acceleration with an expression-based ORDER BY and LIMIT.
pub struct TopkSort;

impl Workload for TopkSort {
    fn name(&self) -> &'static str {
        "topk_sort"
    }

    fn description(&self) -> &'static str {
        "SELECT id, score, weight FROM bench_scores \
         ORDER BY ln(score) * weight DESC LIMIT 100 — tests expression sort + top-K"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_scores".to_owned(),
            "CREATE TABLE bench_scores (\
               id serial PRIMARY KEY, \
               score double precision NOT NULL, \
               weight double precision NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_scores (score, weight) \
                 SELECT \
                   1.0 + random() * 999.0, \
                   random() * 10.0 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_scores".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT id, score, weight FROM bench_scores \
         ORDER BY ln(score) * weight DESC LIMIT 100"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_scores".to_owned()]
    }
}

use super::Workload;

/// Tests batched full-text search ranking with ORDER BY + LIMIT.
pub struct FtsRank;

impl Workload for FtsRank {
    fn name(&self) -> &'static str {
        "fts_rank"
    }

    fn description(&self) -> &'static str {
        "SELECT id, ts_rank(tsv, q) AS rank FROM bench_docs, \
         to_tsquery('english', 'data') q WHERE tsv @@ q ORDER BY rank DESC LIMIT 100 \
         — tests BatchedEval on FTS ranking + sort"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_docs".to_owned(),
            "CREATE TABLE bench_docs (\
               id serial PRIMARY KEY, \
               body text NOT NULL, \
               tsv tsvector NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_docs (body, tsv) \
                 SELECT body, to_tsvector('english', body) \
                 FROM ( \
                   SELECT CASE WHEN random() < 0.1 \
                     THEN 'data analysis report with metrics and data points' \
                     ELSE 'generic document about various unrelated topics number ' || gs \
                   END AS body \
                   FROM generate_series(1, {rows}) gs \
                 ) sub"
            ),
            "CREATE INDEX ON bench_docs USING gin (tsv)".to_owned(),
            "ANALYZE bench_docs".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT id, ts_rank(tsv, q) AS rank \
         FROM bench_docs, to_tsquery('english', 'data') q \
         WHERE tsv @@ q \
         ORDER BY rank DESC LIMIT 100"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_docs".to_owned()]
    }
}

use super::Workload;

/// Fused grouped-count workload for h3_cell_to_parent.
///
/// This scalar H3 operation remains near parity as a standalone GPU path, so
/// the adapter must not expose it to normal scalar planning. The benchmarked
/// shape is narrower and cardinality-reducing:
/// `h3_cell_to_parent(cell, const), COUNT(*) GROUP BY 1`.
/// The baseline keeps `pg_accel.enabled = off` and schema-qualifies the
/// native h3-pg call.
pub struct H3CellToParent;

impl Workload for H3CellToParent {
    fn name(&self) -> &'static str {
        "h3_cell_to_parent"
    }

    fn description(&self) -> &'static str {
        "h3_cell_to_parent fused grouped COUNT(*) — standalone scalar H3 stays \
         quarantined, but parent-cell grouping can dispatch a cardinality-\
         reducing GPU aggregate. Baseline uses stock h3-pg via \
         `public.h3_cell_to_parent`."
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_parent".to_owned(),
            "CREATE TABLE bench_h3_parent (\
               id serial PRIMARY KEY, \
               cell h3index NOT NULL\
             )"
            .to_owned(),
            // Populate fixture through h3-pg alias so cell values are
            // guaranteed to come from the stock C code regardless of
            // pg_accel state during setup.
            format!(
                "INSERT INTO bench_h3_parent (cell) \
                 SELECT public.h3_lat_lng_to_cell(\
                   point(\
                     -74.0 + random() * 0.3, \
                     40.6 + random() * 0.4\
                   ), 7\
                 ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_parent".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS group_count, \
                sum(n)::bigint AS input_rows, \
                min(parent_cell::text) AS min_cell, \
                max(parent_cell::text) AS max_cell, \
                sum(hashtextextended(parent_cell::text || ':' || n::text, 0)::numeric) \
                  AS cell_count_checksum \
         FROM (\
           SELECT h3_cell_to_parent(cell, 4) AS parent_cell, COUNT(*) AS n \
           FROM bench_h3_parent GROUP BY 1\
         ) grouped"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT count(*) AS group_count, \
                    sum(n)::bigint AS input_rows, \
                    min(parent_cell::text) AS min_cell, \
                    max(parent_cell::text) AS max_cell, \
                    sum(hashtextextended(parent_cell::text || ':' || n::text, 0)::numeric) \
                      AS cell_count_checksum \
             FROM (\
               SELECT public.h3_cell_to_parent(cell, 4) AS parent_cell, COUNT(*) AS n \
               FROM bench_h3_parent GROUP BY 1\
             ) grouped"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_parent".to_owned()]
    }
}

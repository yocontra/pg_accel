use super::Workload;

/// Fused grouped-count workload for h3_cell_to_parent.
///
/// This scalar H3 operation remains near parity as a standalone GPU path, so
/// the adapter must not expose it to normal scalar planning. The benchmarked
/// shape is the Phase 6 resident capability lane:
/// `h3_cell_to_parent(cell, 0), COUNT(*) GROUP BY 1`.
/// The baseline keeps `pg_accel.enabled = off` and schema-qualifies the
/// native h3-pg call. Performance is intentionally left for Phase 7
/// re-baselining; this workload currently proves dispatch and correctness.
pub struct H3CellToParent;

impl Workload for H3CellToParent {
    fn name(&self) -> &'static str {
        "h3_cell_to_parent"
    }

    fn description(&self) -> &'static str {
        "h3_cell_to_parent resident grouped COUNT(*) at parent resolution 0 — \
         standalone scalar H3 stays quarantined. This Phase 6 capability lane \
         proves selected GPU dispatch and native equality; Phase 7 will \
         re-baseline performance."
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
        "SELECT h3_cell_to_parent(cell, 0) AS parent_cell, COUNT(*) AS n \
         FROM bench_h3_parent GROUP BY 1"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT public.h3_cell_to_parent(cell, 0) AS parent_cell, COUNT(*) AS n \
             FROM bench_h3_parent GROUP BY 1"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_parent".to_owned()]
    }
}

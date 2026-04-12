use super::Workload;

/// Tests bulk h3_cell_to_parent resolution change through the GPU H3 bit-shift kernel.
///
/// Baseline runs the same function schema-qualified as
/// `public.h3_cell_to_parent` and relies on the runner's
/// `pg_accel.enabled = off` GUC to drop through to the h3-pg C
/// implementation. h3-pg does not ship an underscored alias for
/// `h3_cell_to_parent`, so schema qualification + the disabled hook
/// are the mechanisms that isolate the baseline from pg_accel's path.
/// See `h3_variants.rs` for the full rationale.
pub struct H3CellToParent;

impl Workload for H3CellToParent {
    fn name(&self) -> &'static str {
        "h3_cell_to_parent"
    }

    fn description(&self) -> &'static str {
        "h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel. \
         Baseline uses stock h3-pg via `public.h3_cell_to_parent`."
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
        "SELECT h3_cell_to_parent(cell, 4), COUNT(*) \
         FROM bench_h3_parent GROUP BY 1"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT public.h3_cell_to_parent(cell, 4), COUNT(*) \
             FROM bench_h3_parent GROUP BY 1"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_parent".to_owned()]
    }
}

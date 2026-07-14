use super::Workload;

const H3_GRID_DISTANCE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Quarantine guard for standalone h3_grid_distance.
///
/// This scalar H3 operation is near parity as a standalone GPU path, so the
/// adapter must not expose it to normal planning. The accel-side query keeps
/// the unqualified spelling to catch accidental re-registration, while the
/// baseline keeps `pg_accel.enabled = off` and schema-qualifies the native
/// h3-pg call. See `h3_variants.rs` for the full rationale.
pub struct H3GridDistance;

impl Workload for H3GridDistance {
    fn name(&self) -> &'static str {
        "h3_grid_distance"
    }

    fn description(&self) -> &'static str {
        "pairwise h3_grid_distance native-decline guard — near-parity scalar \
         H3 must stay out of standalone GpuH3 exposure. Baseline uses stock \
         h3-pg via `public.h3_grid_distance`."
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_dist".to_owned(),
            "CREATE TABLE bench_h3_dist (\
               id serial PRIMARY KEY, \
               cell_a h3index NOT NULL, \
               cell_b h3index NOT NULL\
             )"
            .to_owned(),
            // Populate via the h3-pg alias so setup is isolated from
            // pg_accel's adapter path.
            format!(
                "INSERT INTO bench_h3_dist (cell_a, cell_b) \
                 SELECT \
                   public.h3_lat_lng_to_cell(\
                     point(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4\
                   ), \
                   public.h3_lat_lng_to_cell(\
                     point(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4\
                   ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_dist".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT AVG(h3_grid_distance(cell_a, cell_b)) \
         FROM bench_h3_dist"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT AVG(public.h3_grid_distance(cell_a, cell_b)) \
             FROM bench_h3_dist"
                .to_owned(),
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        H3_GRID_DISTANCE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_dist".to_owned()]
    }
}

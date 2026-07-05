use super::Workload;

const ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Variable-output H3 SRF guard: table-driven `h3_grid_disk` expansion.
///
/// The default benchmark scales deliberately keep the registered
/// `h3_grid_disk(cell, k)` spelling while expecting the planner to decline
/// the GPU SRF path: returning the expanded row set to PostgreSQL loses until
/// a downstream aggregate/count path can stay GPU-resident. Focused
/// integration tests cover the small selected SRF shape. The baseline calls a
/// setup-local wrapper whose name is not in pg_accel's adapter registry and
/// whose function-local GUC keeps the wrapped h3-pg call on the native path.
pub struct H3SrfGridDisk;

impl Workload for H3SrfGridDisk {
    fn name(&self) -> &'static str {
        "h3_srf_grid_disk"
    }

    fn description(&self) -> &'static str {
        "h3_grid_disk target-list SRF native-decline guard at benchmark \
         scales until GPU aggregate/count fusion can consume expanded rows. \
         Baseline uses a native h3-pg wrapper not registered by pg_accel."
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP FUNCTION IF EXISTS bench_h3_grid_disk_native(h3index, integer)".to_owned(),
            "DROP TABLE IF EXISTS bench_h3_srf_grid_disk".to_owned(),
            "CREATE TABLE bench_h3_srf_grid_disk (\
               id serial PRIMARY KEY, \
               cell h3index NOT NULL\
             )"
            .to_owned(),
            // Populate source cells through h3-pg's non-registered alias so
            // fixture generation is independent of pg_accel state.
            format!(
                "INSERT INTO bench_h3_srf_grid_disk (cell) \
                 SELECT public.h3_lat_lng_to_cell(\
                   point(\
                     -74.0 + random() * 0.3, \
                     40.6 + random() * 0.4\
                   ), 7\
                 ) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE OR REPLACE FUNCTION \
               bench_h3_grid_disk_native(origin h3index, k integer) \
             RETURNS SETOF h3index \
             LANGUAGE sql STABLE \
             SET pg_accel.enabled = off \
             AS $$ SELECT public.h3_grid_disk($1, $2) $$"
                .to_owned(),
            "ANALYZE bench_h3_srf_grid_disk".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT \
           count(*) AS expanded_rows, \
           count(DISTINCT disk_cell::text) AS distinct_cells, \
           min(disk_cell::text) AS min_cell, \
           max(disk_cell::text) AS max_cell, \
           sum(hashtextextended(disk_cell::text, 0)::numeric) AS disk_cell_checksum \
         FROM (\
           SELECT h3_grid_disk(cell, 2) AS disk_cell \
           FROM bench_h3_srf_grid_disk\
         ) expanded"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT \
               count(*) AS expanded_rows, \
               count(DISTINCT disk_cell::text) AS distinct_cells, \
               min(disk_cell::text) AS min_cell, \
               max(disk_cell::text) AS max_cell, \
               sum(hashtextextended(disk_cell::text, 0)::numeric) AS disk_cell_checksum \
             FROM (\
               SELECT bench_h3_grid_disk_native(cell, 2) AS disk_cell \
               FROM bench_h3_srf_grid_disk\
             ) expanded"
                .to_owned(),
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_srf_grid_disk".to_owned(),
            "DROP FUNCTION IF EXISTS bench_h3_grid_disk_native(h3index, integer)".to_owned(),
        ]
    }
}

use super::Workload;

/// Native-decline guard for bulk `h3_latlng_to_cell` with `GROUP BY`.
///
/// Baseline uses h3-pg's `h3_lat_lng_to_cell` alias so the PG-parallel
/// comparand runs stock h3-pg C code rather than pg_accel's expression
/// wrapper. Normal planning must report `shape_unsupported_rte` and keep the
/// kernel counter at zero for this query shape.
pub struct H3Bulk;

impl Workload for H3Bulk {
    fn name(&self) -> &'static str {
        "h3_bulk"
    }

    fn description(&self) -> &'static str {
        "h3_latlng_to_cell(geom, 7) grouped-count native-decline guard \
         (`shape_unsupported_rte`, zero GPU kernels). Baseline uses stock h3-pg \
         `h3_lat_lng_to_cell`."
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_points".to_owned(),
            "CREATE TABLE bench_h3_points (id serial PRIMARY KEY, \
             geom point NOT NULL)"
                .to_owned(),
            // Random lat/lng stored as PostgreSQL native point type.
            format!(
                "INSERT INTO bench_h3_points (geom) \
                 SELECT point(\
                   random() * 360 - 180, \
                   random() * 180 - 90) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_points".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS group_count, \
                sum(n)::bigint AS input_rows, \
                min(cell::text) AS min_cell, \
                max(cell::text) AS max_cell, \
                sum(hashtextextended(cell::text || ':' || n::text, 0)::numeric) \
                  AS cell_count_checksum \
         FROM (\
           SELECT h3_latlng_to_cell(geom, 7) AS cell, count(*) AS n \
           FROM bench_h3_points GROUP BY 1\
         ) grouped"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        // h3-pg alias `h3_lat_lng_to_cell` is not in pg_accel's adapter
        // list, so this call path
        // bypasses the pg_accel planner hook entirely and measures the
        // stock h3-pg C function.
        Some(
            "SELECT count(*) AS group_count, \
                    sum(n)::bigint AS input_rows, \
                    min(cell::text) AS min_cell, \
                    max(cell::text) AS max_cell, \
                    sum(hashtextextended(cell::text || ':' || n::text, 0)::numeric) \
                      AS cell_count_checksum \
             FROM (\
               SELECT public.h3_lat_lng_to_cell(geom, 7) AS cell, count(*) AS n \
               FROM bench_h3_points GROUP BY 1\
             ) grouped"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_points".to_owned()]
    }
}

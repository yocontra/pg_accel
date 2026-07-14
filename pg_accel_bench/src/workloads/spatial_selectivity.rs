use super::Workload;

/// Tests GPU spatial filter at moderate (~25%) selectivity.
pub struct SpatialSelectivity;

impl Workload for SpatialSelectivity {
    fn name(&self) -> &'static str {
        "spatial_selectivity"
    }

    fn description(&self) -> &'static str {
        "25% selectivity spatial filter \
         — tests GPU spatial at moderate selectivity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_ss_pts".to_owned(),
            "CREATE TABLE bench_ss_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_ss_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   random(), random()\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            // No GiST index — force seq scan to exercise GPU spatial on ALL rows.
            "ANALYZE bench_ss_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // Complex irregular polygon (20 vertices) to force real point-in-ring
        // computation. Covers roughly 25% of the [0,1]×[0,1] space.
        "SELECT COUNT(*) FROM bench_ss_pts \
         WHERE ST_Intersects(\
           geom, \
           ST_SetSRID(ST_GeomFromText(\
             'POLYGON((0.1 0.1, 0.3 0.05, 0.45 0.15, 0.5 0.3, 0.55 0.1, \
                        0.7 0.2, 0.8 0.4, 0.9 0.3, 0.85 0.5, 0.7 0.6, \
                        0.8 0.75, 0.6 0.8, 0.5 0.7, 0.4 0.85, 0.3 0.7, \
                        0.2 0.8, 0.15 0.6, 0.05 0.5, 0.1 0.3, 0.1 0.1))'\
           ), 4326)\
         )"
        .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_ss_pts".to_owned()]
    }
}

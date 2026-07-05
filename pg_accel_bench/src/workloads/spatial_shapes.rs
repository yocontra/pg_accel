use super::Workload;

const SPATIAL_SHAPE_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];

/// Parametric spatial benchmark: ST_Intersects with complex polygon shapes.
///
/// Tests point_in_ring with non-circular geometries (donut, star, multi-hole,
/// zigzag). All shapes have high vertex counts for compute-bound evaluation.
pub struct SpatialShape {
    pub name: &'static str,
    pub description: &'static str,
    pub polygon_sql: &'static str,
}

impl Workload for SpatialShape {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_spatial"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_shape_pts".to_owned(),
            "CREATE TABLE bench_shape_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_shape_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_shape_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT count(*) FROM bench_shape_pts \
             WHERE ST_Intersects(geom, {})",
            self.polygon_sql
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_SHAPE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_shape_pts".to_owned()]
    }
}

// Polygon SQL constructors (const, used by all_workloads registration)

/// Concentric donut: outer buffer minus inner buffer = ~4000 vertices
pub const CONCENTRIC_SQL: &str = "\
ST_Difference(\
  ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.15, 500), \
  ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.05, 500)\
)";

/// Star: union of two offset buffers = ~1000 vertices with concavities
pub const STAR_SQL: &str = "\
ST_SymDifference(\
  ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.12, 250), \
  ST_Buffer(ST_SetSRID(ST_MakePoint(-73.990, 40.750), 4326), 0.10, 250)\
)";

/// Multi-hole: large polygon with 10 interior holes = ~2200 vertices
pub const MULTIHOLE_SQL: &str = "\
ST_Difference(\
  ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.20, 200), \
  ST_Union(ARRAY[\
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.97, 40.74), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.98, 40.74), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.99, 40.74), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-74.00, 40.74), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.97, 40.76), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.98, 40.76), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.99, 40.76), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-74.00, 40.76), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.975, 40.75), 4326), 0.02, 20), \
    ST_Buffer(ST_SetSRID(ST_MakePoint(-73.995, 40.75), 4326), 0.02, 20)\
  ])\
)";

/// Zigzag: non-convex shape with many edge crossings = ~1000 vertices
pub const ZIGZAG_SQL: &str = "\
ST_Buffer(\
  ST_SetSRID(\
    ST_GeomFromText(\
      'LINESTRING(-74.05 40.70, -73.95 40.75, -74.05 40.72, -73.95 40.77, \
                   -74.05 40.74, -73.95 40.79, -74.05 40.76, -73.95 40.81, \
                   -74.05 40.78, -73.95 40.83)'\
    ), 4326\
  ), \
  0.02, \
  50\
)";

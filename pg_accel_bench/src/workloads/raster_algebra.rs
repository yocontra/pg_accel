use super::Workload;

/// Tests raster map algebra: ST_MapAlgebra on a raster tile grid.
pub struct RasterAlgebra;

impl Workload for RasterAlgebra {
    fn name(&self) -> &'static str {
        "raster_algebra"
    }

    fn description(&self) -> &'static str {
        "SELECT ST_MapAlgebra(rast, 1, NULL, '[rast] * 2.0') FROM bench_rasters \
         — tests GpuRaster map algebra"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_rasters".to_owned(),
            "CREATE TABLE bench_rasters (\
               id serial PRIMARY KEY, \
               rast raster NOT NULL\
             )"
            .to_owned(),
            // Generate small raster tiles (32x32 pixels) with random elevation data.
            format!(
                "INSERT INTO bench_rasters (rast) \
                 SELECT ST_AddBand(\
                   ST_MakeEmptyRaster(\
                     32, 32, \
                     -180.0 + (g % 360)::double precision, \
                     -90.0 + (g / 360)::double precision, \
                     0.01, 0.01, 0, 0, 4326), \
                   1, '32BF'::text, random() * 1000, -9999) \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_rasters".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM (\
           SELECT ST_MapAlgebra(rast, 1, NULL, '[rast] * 2.0') AS rast \
           FROM bench_rasters\
         ) sub"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_rasters".to_owned()]
    }
}

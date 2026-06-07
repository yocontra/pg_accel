use super::Workload;

const RASTER_DEFAULT_ROW_SCALES: &[usize] = &[100];

/// Parametric raster map-algebra benchmark.
///
/// Queries consume the derived raster through ST_SummaryStats and aggregate a
/// stats digest. Avoid count-only wrappers here: the benchmark proof depends
/// on the map-algebra/raster output being evaluated.
pub struct RasterVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub setup_stmts: &'static [&'static str],
    pub query: &'static str,
    pub cleanup_stmts: &'static [&'static str],
}

/// Scale tile dimensions so total pixel volume stays roughly constant.
/// Target: ~256×256 at 1K rows ≈ 65M pixels total.
/// At 1M rows we use 8×8 tiles to keep setup under a minute.
fn tile_size(rows: usize) -> usize {
    match rows {
        r if r <= 1_000 => 256,
        r if r <= 10_000 => 64,
        r if r <= 100_000 => 24,
        _ => 8,
    }
}

impl Workload for RasterVariant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_raster"
    }

    fn row_scales(&self) -> &'static [usize] {
        RASTER_DEFAULT_ROW_SCALES
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let ts = tile_size(rows);
        self.setup_stmts
            .iter()
            .map(|s| {
                s.replace("{rows}", &rows.to_string())
                    .replace("{tile}", &ts.to_string())
            })
            .collect()
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        self.cleanup_stmts.iter().map(|s| (*s).to_owned()).collect()
    }
}

/// NDVI: (B1-B2)/(B1+B2) — 3 FLOPs/pixel
pub const RASTER_NDVI: RasterVariant = RasterVariant {
    name: "raster_ndvi",
    description: "(B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_raster",
        "CREATE TABLE bench_raster (\
           id serial PRIMARY KEY, \
           rast raster NOT NULL\
         )",
        "INSERT INTO bench_raster (rast) \
         SELECT ST_AddBand(\
           ST_AddBand(\
             ST_MakeEmptyRaster({tile}, {tile}, 0, 0, 1),\
             1, '32BF'::text, random() * 255, 0\
           ),\
           '32BF'::text, random() * 255, 0\
         ) FROM generate_series(1, {rows})",
        "ANALYZE bench_raster",
    ],
    query: "SELECT COALESCE(\
              sum(\
                ((stats).count)::double precision + \
                COALESCE((stats).sum, 0.0) + \
                COALESCE((stats).mean, 0.0) + \
                COALESCE((stats).stddev, 0.0) + \
                COALESCE((stats).min, 0.0) + \
                COALESCE((stats).max, 0.0)\
              ), \
              0.0\
            ) FROM (\
              SELECT ST_SummaryStats(\
                ST_MapAlgebra(\
                  rast, 1, rast, 2, \
                  '([rast1]-[rast2])/([rast1]+[rast2]+0.001)'::text, \
                  '32BF'::text\
                ), \
                1, true\
              ) AS stats \
              FROM bench_raster\
            ) t",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_raster"],
};

/// Slope: sqrt(pow(dzdx,2)+pow(dzdy,2)) — ~35 FLOPs/pixel
pub const RASTER_SLOPE: RasterVariant = RasterVariant {
    name: "raster_slope",
    description: "ST_Slope — ~35 FLOPs/pixel",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_raster_elev",
        "CREATE TABLE bench_raster_elev (\
           id serial PRIMARY KEY, \
           rast raster NOT NULL\
         )",
        "INSERT INTO bench_raster_elev (rast) \
         SELECT ST_AddBand(\
           ST_MakeEmptyRaster({tile}, {tile}, 0, 0, 1),\
           1, '32BF'::text, random() * 1000, 0\
         ) FROM generate_series(1, {rows})",
        "ANALYZE bench_raster_elev",
    ],
    query: "SELECT COALESCE(\
              sum(\
                ((stats).count)::double precision + \
                COALESCE((stats).sum, 0.0) + \
                COALESCE((stats).mean, 0.0) + \
                COALESCE((stats).stddev, 0.0) + \
                COALESCE((stats).min, 0.0) + \
                COALESCE((stats).max, 0.0)\
              ), \
              0.0\
            ) FROM (\
              SELECT ST_SummaryStats(\
                ST_Slope(rast, 1, '32BF'::text), \
                1, true\
              ) AS stats \
              FROM bench_raster_elev\
            ) t",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_raster_elev"],
};

/// Reclassify: 5-class reclassification — 5 FLOPs/pixel
pub const RASTER_RECLASS: RasterVariant = RasterVariant {
    name: "raster_reclass",
    description: "ST_Reclass — 5-class reclassification, 5 FLOPs/pixel",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_raster_rc",
        "CREATE TABLE bench_raster_rc (\
           id serial PRIMARY KEY, \
           rast raster NOT NULL\
         )",
        "INSERT INTO bench_raster_rc (rast) \
         SELECT ST_AddBand(\
           ST_MakeEmptyRaster({tile}, {tile}, 0, 0, 1),\
           1, '32BF'::text, random() * 255, 0\
         ) FROM generate_series(1, {rows})",
        "ANALYZE bench_raster_rc",
    ],
    query: "SELECT COALESCE(\
              sum(\
                ((stats).count)::double precision + \
                COALESCE((stats).sum, 0.0) + \
                COALESCE((stats).mean, 0.0) + \
                COALESCE((stats).stddev, 0.0) + \
                COALESCE((stats).min, 0.0) + \
                COALESCE((stats).max, 0.0)\
              ), \
              0.0\
            ) FROM (\
              SELECT ST_SummaryStats(\
                ST_Reclass(\
                  rast, \
                  1, \
                  '0-50:1, 50-100:2, 100-150:3, 150-200:4, 200-255:5'::text, \
                  '32BF'::text, 0\
                ), \
                1, true\
              ) AS stats \
              FROM bench_raster_rc\
            ) t",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_raster_rc"],
};

/// Deep algebra: sqrt(pow(B1,2)+pow(B2,2))*log(B1+B2+1) — ~50 FLOPs/pixel
pub const RASTER_ALGEBRA_DEEP: RasterVariant = RasterVariant {
    name: "raster_algebra_deep",
    description: "sqrt(pow(B1,2)+pow(B2,2))*log(B1+B2+1) — deep algebra, ~50 FLOPs/pixel",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_raster_deep",
        "CREATE TABLE bench_raster_deep (\
           id serial PRIMARY KEY, \
           rast raster NOT NULL\
         )",
        "INSERT INTO bench_raster_deep (rast) \
         SELECT ST_AddBand(\
           ST_AddBand(\
             ST_AddBand(\
               ST_MakeEmptyRaster({tile}, {tile}, 0, 0, 1),\
               1, '32BF'::text, random() * 255, 0\
             ),\
             '32BF'::text, random() * 255, 0\
           ),\
           '32BF'::text, random() * 255, 0\
         ) FROM generate_series(1, {rows})",
        "ANALYZE bench_raster_deep",
    ],
    query: "SELECT COALESCE(\
              sum(\
                ((stats).count)::double precision + \
                COALESCE((stats).sum, 0.0) + \
                COALESCE((stats).mean, 0.0) + \
                COALESCE((stats).stddev, 0.0) + \
                COALESCE((stats).min, 0.0) + \
                COALESCE((stats).max, 0.0)\
              ), \
              0.0\
            ) FROM (\
              SELECT ST_SummaryStats(\
                ST_MapAlgebra(\
                  rast, 1, rast, 2, \
                  'sqrt(pow([rast1],2)+pow([rast2],2))*log([rast1]+[rast2]+1)'::text, \
                  '32BF'::text\
                ), \
                1, true\
              ) AS stats \
              FROM bench_raster_deep\
            ) t",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_raster_deep"],
};

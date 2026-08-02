use super::Workload;

const RASTER_RESIDENT_RECLASS_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];
const RECLASS_RULES: &str =
    "0:10,1:11,2:12,3:13,4:14,5:15,6:16,7:17,8:18,9:19,10:20,11:21,12:22,13:23,14:24,15:25,255:42";

/// Candidate benchmark for the exact resident three-argument ST_Reclass subset.
///
/// The registry classifies it as an exact native decline until an independent
/// warm benchmark justifies production planner promotion.
pub struct RasterResidentExactReclass;

impl Workload for RasterResidentExactReclass {
    fn name(&self) -> &'static str {
        "raster_resident_exact_reclass"
    }

    fn description(&self) -> &'static str {
        "Exact three-argument ST_Reclass with NULL, nodata, unmatched pixels, and reconstructed raster output"
    }

    fn row_scales(&self) -> &'static [usize] {
        RASTER_RESIDENT_RECLASS_SCALES
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let tile = super::raster_variants::tile_size(rows);
        vec![
            "DROP TABLE IF EXISTS bench_raster_resident_exact_reclass".to_owned(),
            "CREATE TABLE bench_raster_resident_exact_reclass (\
               id int8 PRIMARY KEY, rast raster\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_raster_resident_exact_reclass (id, rast) \
                 SELECT g::int8, \
                        CASE \
                          WHEN g % 97 = 0 THEN NULL \
                          ELSE ST_AddBand(\
                            ST_MakeEmptyRaster({tile}, {tile}, 0, 0, 1, -1, 0, 0, 4326), \
                            '8BUI'::text, \
                            CASE WHEN g % 101 = 0 THEN 255 ELSE (g % 16)::double precision END, \
                            255\
                          ) \
                        END \
                 FROM generate_series(1, {rows}) AS rows(g)"
            ),
            "ANALYZE bench_raster_resident_exact_reclass".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT ST_Reclass(rast, '{RECLASS_RULES}'::text, '8BUI'::text) AS rast \
             FROM bench_raster_resident_exact_reclass"
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_raster_resident_exact_reclass".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_and_query_stay_inside_the_exact_rqs2_subset() {
        let workload = RasterResidentExactReclass;
        let setup = workload.setup_sql(100_000).join(" ");
        let query = workload.query_sql();
        assert!(!setup.to_ascii_lowercase().contains("random()"));
        assert!(setup.contains("THEN NULL"));
        assert!(setup.contains("ST_MakeEmptyRaster(16, 16"));
        assert!(setup.contains("'8BUI'::text"));
        assert!(!setup.contains("g % 89"));
        assert!(query.starts_with("SELECT ST_Reclass(rast,"));
        assert_eq!(query.matches("ST_Reclass").count(), 1);
        assert!(!query.contains("ST_SummaryStats"));
        assert!(!query.contains(" WHERE "));
        assert_eq!(workload.row_scales(), RASTER_RESIDENT_RECLASS_SCALES);
    }
}

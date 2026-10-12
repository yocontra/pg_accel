//! Opt-in real-data characterization. Source preparation is deliberately
//! separate from timing, and no unsupported query is removed from the corpus.
use super::{ExpectedResultValue, ResultOracle, Workload};

pub const SOURCE_MANIFEST: &str =
    include_str!("../../../benchmarks/corpora/nyc-yellow-2015-01/manifest.json");
const AIRPORT: &str =
    include_str!("../../../benchmarks/corpora/nyc-yellow-2015-01/jfk-airport.ewkt");
pub const FULL_ROWS: usize = 12_748_986;

pub struct NycTaxi(pub &'static str);

impl Workload for NycTaxi {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        "Pinned real January 2015 NYC yellow-taxi coordinates; all rows/outliers retained; diagnostic prefix and complete month"
    }
    fn external_fixture_manifest(&self) -> Option<&'static str> {
        Some(SOURCE_MANIFEST)
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let manifest: serde_json::Value =
            serde_json::from_str(SOURCE_MANIFEST).expect("embedded corpus manifest");
        let sha = manifest["source_sha256"].as_str().expect("source hash");
        let columns = match self.0 {
            "nyc_taxi_passenger_where"
            | "nyc_taxi_nullable_passengers"
            | "nyc_taxi_passenger_filters"
            | "nyc_taxi_passenger_extrema" => "id, vendor_id, passenger_count",
            "nyc_taxi_airport_passengers"
            | "nyc_taxi_airport_nullable_passengers"
            | "nyc_taxi_airport_passenger_extrema"
            | "nyc_taxi_airport_passenger_filters"
            | "nyc_taxi_airport_vendor_passengers" => "id, vendor_id, passenger_count, pickup_geom",
            "nyc_taxi_airport_rollup" => {
                "id, vendor_id, passenger_count, fare_amount, trip_distance, pickup_geom"
            }
            "nyc_taxi_airport_vendor_rollup" => {
                "id, vendor_id, fare_amount, trip_distance, pickup_geom"
            }
            "nyc_taxi_h3_res9" => "id, cell",
            "nyc_taxi_fare_rollup" | "nyc_taxi_mixed_rollup" | "nyc_taxi_filtered_rollup" => {
                "id, vendor_id, passenger_count, fare_amount, trip_distance"
            }
            _ => "id, pickup_geom",
        };
        let mut sql = vec![
            format!(
                "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pgaccel_corpus.manifests WHERE dataset_id = 'nyc-yellow-2015-01-aviz-raw-v1' AND source_sha256 = '{sha}' AND source_rows = {FULL_ROWS}) THEN RAISE EXCEPTION 'Pinned NYC corpus missing or mismatched; run scripts/nyc_taxi_corpus.py import'; END IF; END $$"
            ),
            "DROP TABLE IF EXISTS bench_nyc_taxi".to_owned(),
            format!(
                "CREATE TABLE bench_nyc_taxi AS SELECT {columns} FROM pgaccel_corpus.nyc_yellow_2015_01 WHERE id <= {rows} ORDER BY id"
            ),
            format!(
                "DO $$ BEGIN IF (SELECT count(*) FROM bench_nyc_taxi) <> {rows} THEN RAISE EXCEPTION 'NYC corpus prefix is incomplete'; END IF; END $$"
            ),
        ];
        if self.0 == "nyc_taxi_airport_indexed" {
            sql.push(
                "CREATE INDEX bench_nyc_taxi_geom_gist ON bench_nyc_taxi USING gist (pickup_geom)"
                    .to_owned(),
            );
        }
        sql.push("ANALYZE bench_nyc_taxi".to_owned());
        sql
    }

    fn query_sql(&self) -> String {
        match self.0 {
            "nyc_taxi_nullable_passengers" | "nyc_taxi_airport_nullable_passengers" => {
                let spatial = if self.0.contains("airport") {
                    format!("ST_Intersects(pickup_geom,'{}'::geometry) AND ", AIRPORT.trim())
                } else { String::new() };
                format!("SELECT vendor_id,sum(passenger_count) AS passengers,avg(passenger_count) AS mean_passengers,count(*) FILTER (WHERE passenger_count IS NULL) AS missing_passengers,count(*) AS n FROM bench_nyc_taxi WHERE {spatial}(passenger_count IS NULL OR passenger_count BETWEEN 1 AND 4) GROUP BY vendor_id")
            }
            "nyc_taxi_passenger_extrema" | "nyc_taxi_airport_passenger_extrema" => {
                let condition = if self.0 == "nyc_taxi_airport_passenger_extrema" {
                    format!(" WHERE ST_Intersects(pickup_geom,'{}'::geometry)", AIRPORT.trim())
                } else { String::new() };
                format!("SELECT vendor_id,min(passenger_count) AS min_passengers,max(passenger_count) AS max_passengers,sum(passenger_count) AS passengers,avg(passenger_count) AS mean_passengers,count(*) AS n FROM bench_nyc_taxi{condition} GROUP BY vendor_id")
            }
            "nyc_taxi_airport_vendor_passengers" => format!("SELECT count(*) AS n,sum(passenger_count) AS passengers,avg(passenger_count) AS mean_passengers FROM bench_nyc_taxi WHERE ST_Intersects(pickup_geom,'{}'::geometry) AND vendor_id=1", AIRPORT.trim()),
            "nyc_taxi_airport_vendor_rollup" => format!("SELECT vendor_id,count(*) AS n,sum(fare_amount) AS fares,avg(trip_distance) AS mean_distance FROM bench_nyc_taxi WHERE vendor_id IN (1) AND ST_Intersects(pickup_geom,'{}'::geometry) GROUP BY vendor_id", AIRPORT.trim()),
            "nyc_taxi_passenger_filters" | "nyc_taxi_airport_passenger_filters" => {
                let condition = if self.0 == "nyc_taxi_airport_passenger_filters" {
                    format!(" WHERE ST_Intersects(pickup_geom,'{}'::geometry)", AIRPORT.trim())
                } else { String::new() };
                format!("SELECT sum(passenger_count) FILTER (WHERE vendor_id=1) AS passengers_1,avg(passenger_count) FILTER (WHERE vendor_id=1) AS mean_1,sum(passenger_count) FILTER (WHERE vendor_id=2) AS passengers_2,avg(passenger_count) FILTER (WHERE vendor_id=2) AS mean_2,count(*) AS n FROM bench_nyc_taxi{condition}")
            }
            "nyc_taxi_passenger_where" => "SELECT count(*) AS n,sum(passenger_count) AS passengers,avg(passenger_count) AS mean_passengers FROM bench_nyc_taxi WHERE vendor_id=1".to_owned(),
            "nyc_taxi_airport_passengers" => format!("SELECT vendor_id,count(*) AS n,sum(passenger_count) AS passengers,avg(passenger_count) AS mean_passengers,count(passenger_count) AS known_passengers FROM bench_nyc_taxi WHERE ST_Intersects(pickup_geom,'{}'::geometry) GROUP BY vendor_id", AIRPORT.trim()),
            "nyc_taxi_airport_rollup" => format!("SELECT vendor_id,count(*) AS n,sum(fare_amount) AS fares,avg(trip_distance) AS mean_distance,avg(passenger_count) AS mean_passengers FROM bench_nyc_taxi WHERE ST_Intersects(pickup_geom,'{}'::geometry) GROUP BY vendor_id", AIRPORT.trim()),
            "nyc_taxi_h3_res9" => "SELECT public.h3_cell_to_parent(cell,9) AS parent_cell, count(*) AS n FROM bench_nyc_taxi GROUP BY 1".to_owned(),
            "nyc_taxi_fare_rollup" => "SELECT vendor_id, passenger_count, count(*) AS n, sum(fare_amount) AS fares, avg(trip_distance) AS mean_distance FROM bench_nyc_taxi GROUP BY 1,2".to_owned(),
            "nyc_taxi_mixed_rollup" => "SELECT vendor_id, count(*) AS n, sum(passenger_count) AS passengers, avg(passenger_count) AS mean_passengers, sum(fare_amount) AS fares, avg(trip_distance) AS mean_distance FROM bench_nyc_taxi GROUP BY 1".to_owned(),
            "nyc_taxi_filtered_rollup" => "SELECT vendor_id,count(*) AS n,sum(fare_amount) AS fares,avg(trip_distance) AS mean_distance FROM bench_nyc_taxi WHERE vendor_id=1 GROUP BY 1".to_owned(),
            "nyc_taxi_airport_indexed" | "nyc_taxi_airport_scan" => format!("SELECT count(*) FROM bench_nyc_taxi WHERE ST_Intersects(pickup_geom, '{}'::geometry)", AIRPORT.trim()),
            _ => unreachable!("registered NYC corpus workload"),
        }
    }

    fn row_scales(&self) -> &'static [usize] {
        &[1_000_000, FULL_ROWS]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        if self.0 != "nyc_taxi_h3_res9" {
            return None;
        }
        // Independent H3 parent bit encoding: replace resolution bits and set
        // all child digits below resolution 9 to 7. NULL remains a real group.
        // All inputs were derived at resolution 15 during corpus preparation.
        Some(ResultOracle::one_row(
            format!(
                "WITH actual AS ({}), expected AS (SELECT CASE WHEN cell IS NULL THEN NULL::bigint ELSE ((cell::bigint & ~(15::bigint << 52)) | (9::bigint << 52) | ((1::bigint << 18)-1)) END AS parent_cell, count(*) n FROM bench_nyc_taxi GROUP BY 1), delta AS ((SELECT parent_cell::bigint,n FROM actual EXCEPT ALL SELECT parent_cell,n FROM expected) UNION ALL (SELECT parent_cell,n FROM expected EXCEPT ALL SELECT parent_cell::bigint,n FROM actual)) SELECT NOT EXISTS (SELECT FROM delta), (SELECT sum(n)::bigint FROM actual)",
                self.query_sql()
            ),
            vec![
                ExpectedResultValue::Bool(true),
                ExpectedResultValue::I64(i64::try_from(rows).expect("corpus count fits int8")),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_nyc_taxi".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_corpus_is_explicit_and_keeps_full_scale_and_multipolygon() {
        let indexed = NycTaxi("nyc_taxi_airport_indexed");
        assert!(indexed.external_fixture_manifest().is_some());
        assert_eq!(indexed.row_scales(), &[1_000_000, FULL_ROWS]);
        assert!(indexed.query_sql().contains("MULTIPOLYGON"));
        assert!(
            indexed
                .setup_sql(FULL_ROWS)
                .join(" ")
                .contains("USING gist")
        );
        assert!(
            !NycTaxi("nyc_taxi_airport_scan")
                .setup_sql(FULL_ROWS)
                .join(" ")
                .contains("USING gist")
        );
        assert!(
            NycTaxi("nyc_taxi_fare_rollup")
                .query_sql()
                .contains("sum(fare_amount)")
        );
        let h3 = NycTaxi("nyc_taxi_h3_res9");
        for name in [
            "nyc_taxi_nullable_passengers",
            "nyc_taxi_airport_nullable_passengers",
        ] {
            let workload = NycTaxi(name);
            let query = workload.query_sql();
            assert!(query.contains("count(*) FILTER (WHERE passenger_count IS NULL)"));
            assert!(query.contains("passenger_count IS NULL OR passenger_count BETWEEN 1 AND 4"));
            assert_eq!(query.contains("ST_Intersects"), name.contains("airport"));
            assert_eq!(workload.row_scales(), &[1_000_000, FULL_ROWS]);
            assert!(
                !workload
                    .setup_sql(FULL_ROWS)
                    .join(" ")
                    .contains("WHERE passenger_count")
            );
        }
        for name in [
            "nyc_taxi_passenger_extrema",
            "nyc_taxi_airport_passenger_extrema",
        ] {
            let workload = NycTaxi(name);
            let query = workload.query_sql();
            for aggregate in ["min", "max", "sum", "avg"] {
                assert!(query.contains(&format!("{aggregate}(passenger_count)")));
            }
            assert!(query.contains("GROUP BY vendor_id"));
            assert_eq!(query.contains("ST_Intersects"), name.contains("airport"));
            assert_eq!(workload.row_scales(), &[1_000_000, FULL_ROWS]);
        }
        let passengers = NycTaxi("nyc_taxi_airport_passengers");
        let filtered_passengers = NycTaxi("nyc_taxi_passenger_where");
        for name in [
            "nyc_taxi_airport_vendor_passengers",
            "nyc_taxi_airport_vendor_rollup",
        ] {
            let workload = NycTaxi(name);
            let query = workload.query_sql();
            assert!(query.contains("ST_Intersects") && query.contains("vendor_id"));
            assert!(query.contains(" AND ") && !query.contains("FILTER"));
            assert_eq!(workload.row_scales(), &[1_000_000, FULL_ROWS]);
            assert!(
                !workload
                    .setup_sql(FULL_ROWS)
                    .join(" ")
                    .contains("WHERE vendor_id")
            );
        }
        for name in [
            "nyc_taxi_passenger_filters",
            "nyc_taxi_airport_passenger_filters",
        ] {
            let workload = NycTaxi(name);
            let query = workload.query_sql();
            assert!(query.contains("sum(passenger_count) FILTER (WHERE vendor_id=1)"));
            assert!(query.contains("avg(passenger_count) FILTER (WHERE vendor_id=2)"));
            assert!(!query.contains("sum(vendor_id)") && !query.contains("count(vendor_id)"));
            assert_eq!(query.contains("ST_Intersects"), name.contains("airport"));
            assert_eq!(workload.row_scales(), &[1_000_000, FULL_ROWS]);
        }
        assert!(
            filtered_passengers
                .query_sql()
                .contains("WHERE vendor_id=1")
        );
        assert!(
            filtered_passengers
                .query_sql()
                .contains("sum(passenger_count)")
        );
        assert!(
            !filtered_passengers
                .setup_sql(FULL_ROWS)
                .join(" ")
                .contains("fare_amount")
        );
        assert!(passengers.query_sql().contains("sum(passenger_count)"));
        assert!(passengers.query_sql().contains("MULTIPOLYGON"));
        assert!(
            !passengers
                .setup_sql(FULL_ROWS)
                .join(" ")
                .contains("fare_amount")
        );
        let mixed = NycTaxi("nyc_taxi_mixed_rollup");
        assert!(mixed.query_sql().contains("sum(passenger_count)"));
        assert!(mixed.query_sql().contains("avg(passenger_count)"));
        assert!(
            mixed
                .setup_sql(FULL_ROWS)
                .join(" ")
                .contains("trip_distance")
        );
        assert_eq!(mixed.row_scales(), &[1_000_000, FULL_ROWS]);
        let fare = super::super::workload_metadata("nyc_taxi_fare_rollup")
            .expect("registered exact decimal workload");
        assert_eq!(fare.resident_pins.len(), 1);
        assert_eq!(
            fare.resident_pins[0].columns,
            &[
                "vendor_id",
                "passenger_count",
                "fare_amount",
                "trip_distance"
            ]
        );
        assert!(
            h3.result_oracle(FULL_ROWS)
                .expect("independent parent oracle")
                .query_sql
                .contains("EXCEPT ALL")
        );
    }
}

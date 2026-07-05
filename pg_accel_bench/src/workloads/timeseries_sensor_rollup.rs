use super::Workload;

/// Time-series grouped rollup: per-sensor min/max/avg over float8 readings.
pub struct TimeseriesSensorRollup;

impl Workload for TimeseriesSensorRollup {
    fn name(&self) -> &'static str {
        "timeseries_sensor_rollup"
    }

    fn description(&self) -> &'static str {
        "Time-series per-sensor MIN, MAX, AVG over float8 readings"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS sensor_data".to_owned(),
            "CREATE TABLE sensor_data (\
               sensor_id int4 NOT NULL, \
               ts timestamp NOT NULL, \
               value float8 NOT NULL, \
               quality int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO sensor_data (sensor_id, ts, value, quality) \
                 SELECT \
                   (random() * 100)::int4, \
                   '2024-01-01'::timestamp + (g * interval '1 second'), \
                   (random() * 100)::float8, \
                   (random() * 10)::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE sensor_data".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT sensor_id, min(value), max(value), avg(value) \
         FROM sensor_data GROUP BY sensor_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS sensor_data".to_owned()]
    }
}

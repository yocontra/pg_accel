use super::{ExpectedResultValue, ResultOracle, Workload};

const SPATIAL_RESIDENT_AGG_ROWS: &[usize] = &[1_000_000];

/// Release candidate for the resident point/simple-polygon aggregate lane.
///
/// The fixture is deterministic and deliberately has no spatial index, so both
/// sides must evaluate every row. Every 97th geometry is NULL. The independent
/// oracle derives membership from the integer coordinates used to construct
/// each point and does not invoke PostGIS.
pub struct SpatialResidentAggCandidate;

fn candidate_polygon_wkt() -> String {
    const POINTS_PER_EDGE: usize = 256;
    let mut coordinates = Vec::with_capacity(4 * POINTS_PER_EDGE + 1);
    for point in 0..POINTS_PER_EDGE {
        let offset = 10.0 * point as f64 / POINTS_PER_EDGE as f64;
        coordinates.push(format!("{:.8} 5.00000000", 5.0 + offset));
    }
    for point in 0..POINTS_PER_EDGE {
        let offset = 10.0 * point as f64 / POINTS_PER_EDGE as f64;
        coordinates.push(format!("15.00000000 {:.8}", 5.0 + offset));
    }
    for point in 0..POINTS_PER_EDGE {
        let offset = 10.0 * point as f64 / POINTS_PER_EDGE as f64;
        coordinates.push(format!("{:.8} 15.00000000", 15.0 - offset));
    }
    for point in 0..POINTS_PER_EDGE {
        let offset = 10.0 * point as f64 / POINTS_PER_EDGE as f64;
        coordinates.push(format!("5.00000000 {:.8}", 15.0 - offset));
    }
    coordinates.push(coordinates[0].clone());
    format!("SRID=4326;POLYGON(({}))", coordinates.join(","))
}

impl Workload for SpatialResidentAggCandidate {
    fn name(&self) -> &'static str {
        "spatial_resident_agg_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Resident COUNT(*) over one geometry(Point,4326) column and one exact ",
            "simple-polygon ST_Intersects constant; deterministic NULL, interior, ",
            "exterior, and boundary rows with an independent arithmetic oracle"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_spatial_resident_agg".to_owned(),
            "CREATE TABLE bench_spatial_resident_agg (\
               id int8 PRIMARY KEY, \
               geom geometry(Point, 4326)\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_spatial_resident_agg (id, geom) \
                 SELECT i, \
                        CASE WHEN i % 97 = 0 THEN NULL \
                             ELSE ST_SetSRID(ST_MakePoint(\
                                    (i % 1000)::float8 / 50.0, \
                                    ((i / 1000) % 1000)::float8 / 50.0\
                                  ), 4326)::geometry(Point, 4326) \
                        END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            // The released lane is a resident full-column transform, not an
            // index-recheck path. An index here would benchmark a different
            // PostgreSQL access shape and make the comparison misleading.
            "ANALYZE bench_spatial_resident_agg".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT count(*) \
             FROM bench_spatial_resident_agg \
             WHERE ST_Intersects(geom, '{}'::geometry)",
            candidate_polygon_wkt()
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_RESIDENT_AGG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let expected = (1..=rows)
            .filter(|row| {
                row % 97 != 0
                    && (250..=750).contains(&(row % 1000))
                    && (250..=750).contains(&((row / 1000) % 1000))
            })
            .count();
        Some(ResultOracle::one_row(
            "SELECT count(*) \
             FROM bench_spatial_resident_agg \
             WHERE id % 97 <> 0 \
               AND id % 1000 BETWEEN 250 AND 750 \
               AND (id / 1000) % 1000 BETWEEN 250 AND 750"
                .to_owned(),
            vec![ExpectedResultValue::I64(
                i64::try_from(expected).expect("candidate result count must fit int8"),
            )],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_resident_agg".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_polygon_is_closed_and_clears_the_1m_work_floor() {
        let wkt = candidate_polygon_wkt();
        let coordinates = wkt
            .strip_prefix("SRID=4326;POLYGON((")
            .and_then(|body| body.strip_suffix("))"))
            .expect("canonical EWKT wrapper")
            .split(',')
            .collect::<Vec<_>>();
        assert_eq!(coordinates.len(), 1_025);
        assert_eq!(coordinates.first(), coordinates.last());
        assert!(coordinates.len() as u64 * SPATIAL_RESIDENT_AGG_ROWS[0] as u64 >= 500_000_000);
    }

    #[test]
    fn arithmetic_oracle_covers_null_boundary_interior_and_exterior_rows() {
        let workload = SpatialResidentAggCandidate;
        let oracle = workload.result_oracle(1_000_000).expect("oracle");
        assert_eq!(oracle.expected_row.len(), 1);
        assert!(oracle.query_sql.contains("id % 97 <> 0"));
        assert!(oracle.query_sql.contains("BETWEEN 250 AND 750"));
    }
}

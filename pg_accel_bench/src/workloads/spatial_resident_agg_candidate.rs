use super::{ExpectedResultValue, ResultOracle, Workload};

const SPATIAL_RESIDENT_AGG_ROWS: &[usize] = &[1_000_000];

/// Release candidate for the resident point/simple-polygon aggregate lane.
///
/// The fixture is deterministic and deliberately has no spatial index, so both
/// sides must evaluate every row. Every 97th geometry is NULL. The independent
/// oracle derives membership from the integer coordinates used to construct
/// each point and does not invoke PostGIS.
pub struct SpatialResidentAggCandidate;

/// Synthetic topology expansion, deliberately without a spatial index.
pub struct SpatialPolygonHoles(pub &'static str);

impl SpatialPolygonHoles {
    fn inclusive(&self) -> bool {
        matches!(
            self.0,
            "spatial_holes_intersects" | "spatial_holes_covers" | "spatial_holes_coveredby"
        )
    }
}

fn rectangle_ring(lo: f64, hi: f64, subdivisions: usize) -> String {
    let mut points = Vec::new();
    for edge in 0..4 {
        for i in 0..subdivisions {
            let offset = (hi - lo) * i as f64 / subdivisions as f64;
            let (x, y) = match edge {
                0 => (lo + offset, lo),
                1 => (hi, lo + offset),
                2 => (hi - offset, hi),
                _ => (lo, hi - offset),
            };
            points.push(format!("{x:.8} {y:.8}"));
        }
    }
    points.push(points[0].clone());
    format!("({})", points.join(","))
}

pub(super) fn holes_polygon_wkt() -> String {
    format!(
        "SRID=4326;POLYGON({},{},{})",
        rectangle_ring(5.0, 15.0, 128),
        rectangle_ring(7.0, 9.0, 64),
        rectangle_ring(11.0, 13.0, 64),
    )
}

impl Workload for SpatialPolygonHoles {
    fn name(&self) -> &'static str {
        self.0
    }

    fn description(&self) -> &'static str {
        "Resident point/polygon COUNT with two holes, NULLs and exact boundary rechecks; synthetic full-scan fixture with independent integer oracle"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        SpatialResidentAggCandidate.setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        let polygon = format!("'{}'::geometry", holes_polygon_wkt());
        let predicate = match self.0 {
            "spatial_holes_intersects" => format!("ST_Intersects(geom,{polygon})"),
            "spatial_holes_contains" => format!("ST_Contains({polygon},geom)"),
            "spatial_holes_within" => format!("ST_Within(geom,{polygon})"),
            "spatial_holes_covers" => format!("ST_Covers({polygon},geom)"),
            "spatial_holes_coveredby" => format!("ST_CoveredBy(geom,{polygon})"),
            _ => unreachable!("registered topology workload"),
        };
        format!("SELECT count(*) FROM bench_spatial_resident_agg WHERE {predicate}")
    }

    fn row_scales(&self) -> &'static [usize] {
        &[1_000_000, 2_000_000]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let inclusive = self.inclusive();
        let expected = (1..=rows)
            .filter(|row| {
                let x = row % 1000;
                let y = (row / 1000) % 1000;
                let square = |lo, hi, closed| {
                    if closed {
                        (lo..=hi).contains(&x) && (lo..=hi).contains(&y)
                    } else {
                        x > lo && x < hi && y > lo && y < hi
                    }
                };
                row % 97 != 0
                    && square(250, 750, inclusive)
                    && !square(350, 450, !inclusive)
                    && !square(550, 650, !inclusive)
            })
            .count();
        let square_sql = |lo, hi, closed| {
            let (low, high) = if closed { (">=", "<=") } else { (">", "<") };
            format!(
                "(id % 1000 {low} {lo} AND id % 1000 {high} {hi} AND (id / 1000) % 1000 {low} {lo} AND (id / 1000) % 1000 {high} {hi})"
            )
        };
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*) FROM bench_spatial_resident_agg WHERE id % 97 <> 0 AND {} AND NOT {} AND NOT {}",
                square_sql(250, 750, inclusive),
                square_sql(350, 450, !inclusive),
                square_sql(550, 650, !inclusive)
            ),
            vec![ExpectedResultValue::I64(
                i64::try_from(expected).expect("count fits int8"),
            )],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        SpatialResidentAggCandidate.cleanup_sql()
    }
}

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
    fn topology_fixture_has_two_holes_and_distinct_boundary_oracles() {
        let wkt = holes_polygon_wkt();
        assert_eq!(wkt.matches("),(").count(), 2);
        assert_eq!(wkt.matches(',').count() + 1, 1027);
        let intersects = SpatialPolygonHoles("spatial_holes_intersects");
        let contains = SpatialPolygonHoles("spatial_holes_contains");
        let within = SpatialPolygonHoles("spatial_holes_within");
        for name in ["spatial_holes_covers", "spatial_holes_coveredby"] {
            let workload = SpatialPolygonHoles(name);
            assert_eq!(
                workload
                    .result_oracle(1_000_000)
                    .expect("inclusive oracle")
                    .query_sql,
                intersects
                    .result_oracle(1_000_000)
                    .expect("intersection oracle")
                    .query_sql
            );
            assert!(workload.query_sql().contains("ST_Cover"));
        }
        assert_ne!(
            intersects
                .result_oracle(1_000_000)
                .expect("Intersects oracle")
                .query_sql,
            contains
                .result_oracle(1_000_000)
                .expect("Contains oracle")
                .query_sql
        );
        assert_eq!(
            contains
                .result_oracle(1_000_000)
                .expect("Contains oracle")
                .query_sql,
            within
                .result_oracle(1_000_000)
                .expect("Within oracle")
                .query_sql
        );
    }

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

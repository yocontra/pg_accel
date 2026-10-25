use super::{ExpectedResultValue, ResultOracle, Workload};

/// Fused grouped-count workload for h3_cell_to_parent.
///
/// This scalar H3 operation remains near parity as a standalone GPU path, so
/// the adapter must not expose it to normal scalar planning. The benchmarked
/// shape is the Phase 6 resident capability lane:
/// `h3_cell_to_parent(cell, 0), COUNT(*) GROUP BY 1`.
/// The baseline keeps `pg_accel.enabled = off` and schema-qualifies the
/// native h3-pg call. The fused lane is protected by the Phase 7 warm-speedup
/// threshold while standalone scalar parent conversion remains quarantined.
pub struct H3CellToParent;

/// Repeated observations from 256 deterministic NYC-coordinate sites. This
/// synthetic workload measures useful rollup resolutions, not a taxi corpus.
pub struct H3ParentResolution<const RESOLUTION: i32>;

/// Typed COUNT lanes deliberately keep an all-NULL value group and a NULL key.
/// A COUNT(*) substitution therefore cannot satisfy even the summary oracle.
pub struct H3ParentCountColumn<const BOOLEAN: bool, const RESOLUTION: i32>;

impl<const BOOLEAN: bool, const RESOLUTION: i32> Workload
    for H3ParentCountColumn<BOOLEAN, RESOLUTION>
{
    fn name(&self) -> &'static str {
        match (BOOLEAN, RESOLUTION) {
            (false, 0) => "h3_parent_count_int4_res0",
            (false, 1) => "h3_parent_count_int4_res1",
            (true, 0) => "h3_parent_count_bool_res0",
            (true, 1) => "h3_parent_count_bool_res1",
            _ => panic!("unregistered typed H3 COUNT benchmark"),
        }
    }

    fn description(&self) -> &'static str {
        "Default-planner H3 COUNT(column): deterministic nullable values, zero-count groups and NULL keys"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let (sql_type, value) = if BOOLEAN {
            ("boolean", "i % 2 = 0")
        } else {
            ("int4", "(i % 2001 - 1000)::int4")
        };
        vec![
            "DROP TABLE IF EXISTS bench_h3_count_column".to_owned(),
            format!("CREATE TABLE bench_h3_count_column (cell h3index, value {sql_type})"),
            format!(
                "INSERT INTO bench_h3_count_column \
                 SELECT CASE i % 4 \
                   WHEN 0 THEN public.h3_lat_lng_to_cell(point(40.7484, -73.9857), 15) \
                   WHEN 1 THEN public.h3_lat_lng_to_cell(point(35.6895, 139.6917), 15) \
                   WHEN 2 THEN public.h3_lat_lng_to_cell(point(-33.8688, 151.2093), 15) \
                   ELSE NULL::h3index END, \
                 CASE WHEN i % 4 = 0 OR i % 11 = 0 THEN NULL::{sql_type} \
                      ELSE {value} END \
                 FROM generate_series(1, {rows}) AS fixture(i)"
            ),
            "ANALYZE bench_h3_count_column".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT public.h3_cell_to_parent(cell, {RESOLUTION}) AS parent_cell, \
             count(value) AS n FROM bench_h3_count_column GROUP BY 1"
        )
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(self.query_sql())
    }

    fn row_scales(&self) -> &'static [usize] {
        &[1_000_017, 2_000_017]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        // Inclusion-exclusion for the two deterministic NULL-value classes.
        let non_null = rows - rows / 4 - rows / 11 + rows / 44;
        Some(ResultOracle::one_row(
            format!(
                "SELECT sum(n)::int8, bool_or(n = 0), bool_or(parent_cell IS NULL) \
                 FROM ({}) AS actual",
                self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(non_null).expect("fixture fits int8")),
                ExpectedResultValue::Bool(rows >= 4),
                ExpectedResultValue::Bool(rows >= 3),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_count_column".to_owned()]
    }
}

impl<const RESOLUTION: i32> Workload for H3ParentResolution<RESOLUTION> {
    fn name(&self) -> &'static str {
        match RESOLUTION {
            4 => "h3_parent_res4",
            9 => "h3_parent_res9",
            15 => "h3_parent_res15",
            _ => panic!("unregistered H3 benchmark resolution"),
        }
    }

    fn description(&self) -> &'static str {
        "Resident H3 COUNT rollup over synthetic repeated NYC sites; bounded high-resolution workspace"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_parent".to_owned(),
            "CREATE TABLE bench_h3_parent (cell h3index)".to_owned(),
            format!(
                "INSERT INTO bench_h3_parent \
                SELECT CASE WHEN i % 97 = 0 THEN NULL::h3index ELSE \
                public.h3_lat_lng_to_cell(point(40.6 + (i % 16) * 0.015, \
                    -74.02 + ((i / 16) % 16) * 0.02), 15) END \
                FROM generate_series(1, {rows}) AS g(i)"
            ),
            "ANALYZE bench_h3_parent".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT public.h3_cell_to_parent(cell, {RESOLUTION}) AS parent_cell, \
                 count(*) AS n FROM bench_h3_parent GROUP BY 1"
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        &[1_000_000]
    }

    fn baseline_query_sql(&self) -> Option<String> {
        // The harness disables pg_accel for this schema-qualified h3-pg arm.
        Some(self.query_sql())
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(ResultOracle::one_row(
            format!(
                "SELECT sum(n)::int8, sum(n) FILTER (WHERE parent_cell IS NULL)::int8, \
                     bool_and(n > 0) FROM ({}) AS actual",
                self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(rows).expect("fixture fits int8")),
                ExpectedResultValue::I64(i64::try_from(rows / 97).expect("fixture fits int8")),
                ExpectedResultValue::Bool(true),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        H3CellToParent.cleanup_sql()
    }
}

impl Workload for H3CellToParent {
    fn name(&self) -> &'static str {
        "h3_cell_to_parent"
    }

    fn description(&self) -> &'static str {
        "h3_cell_to_parent resident grouped COUNT(*) at parent resolution 0 — \
         standalone scalar H3 stays quarantined. This fused lane must prove \
         selected GPU dispatch, native equality, and its warm-speedup floor."
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_parent".to_owned(),
            "CREATE TABLE bench_h3_parent (\
               id serial PRIMARY KEY, \
               cell h3index NOT NULL\
             )"
            .to_owned(),
            // Populate fixture through h3-pg alias so cell values are
            // guaranteed to come from the stock C code regardless of
            // pg_accel state during setup.
            format!(
                "INSERT INTO bench_h3_parent (cell) \
                 SELECT public.h3_lat_lng_to_cell(\
                   point(\
                     -74.0 + random() * 0.3, \
                     40.6 + random() * 0.4\
                   ), 7\
                 ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_parent".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT h3_cell_to_parent(cell, 0) AS parent_cell, COUNT(*) AS n \
         FROM bench_h3_parent GROUP BY 1"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT public.h3_cell_to_parent(cell, 0) AS parent_cell, COUNT(*) AS n \
             FROM bench_h3_parent GROUP BY 1"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_parent".to_owned()]
    }
}

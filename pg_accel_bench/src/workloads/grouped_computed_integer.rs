use std::collections::BTreeMap;

use super::{ExpectedResultValue, ResultOracle, Workload};

pub struct GroupedComputedInteger(pub bool);

pub enum ScalarProgramAggregate {
    GlobalRollup,
    GroupedRollup,
    GroupedCoalesce,
    GroupedSimpleCase,
    GroupedSimpleCaseLinear,
    GroupedMembership,
    SpatialGroupedMembership,
    GroupedScalarFilter,
    GroupedScalarWhere,
    SpatialGroupedScalarWhere,
    GroupedNullSafeWhere,
    SpatialGroupedNullSafeWhere,
    GroupedBoundedRatio,
    SpatialGroupedBoundedRatio,
    GroupedDerivedRatio,
    SpatialGroupedDerivedRatio,
    SpatialGroupedScalarFilter,
    GroupedDateSpan,
    SpatialGroupedDateSpan,
    SpatialGlobalRollup,
    SpatialGroupedRollup,
}

const SCALAR_ROLLUP: &str = "CASE WHEN adjustment IS NULL THEN price \
    WHEN price>0 THEN (price+adjustment)*(price-adjustment) \
    ELSE COALESCE(price,0)-adjustment END";

impl ScalarProgramAggregate {
    fn global(&self) -> bool {
        matches!(self, Self::GlobalRollup | Self::SpatialGlobalRollup)
    }

    fn spatial(&self) -> bool {
        matches!(
            self,
            Self::SpatialGlobalRollup
                | Self::SpatialGroupedRollup
                | Self::SpatialGroupedMembership
                | Self::SpatialGroupedScalarFilter
                | Self::SpatialGroupedScalarWhere
                | Self::SpatialGroupedNullSafeWhere
                | Self::SpatialGroupedBoundedRatio
                | Self::SpatialGroupedDerivedRatio
                | Self::SpatialGroupedDateSpan
        )
    }

    fn scalar_filter(&self) -> bool {
        matches!(
            self,
            Self::GroupedScalarFilter | Self::SpatialGroupedScalarFilter
        )
    }

    fn scalar_where(&self) -> bool {
        matches!(
            self,
            Self::GroupedScalarWhere
                | Self::SpatialGroupedScalarWhere
                | Self::GroupedNullSafeWhere
                | Self::SpatialGroupedNullSafeWhere
        )
    }

    fn null_safe_where(&self) -> bool {
        matches!(
            self,
            Self::GroupedNullSafeWhere | Self::SpatialGroupedNullSafeWhere
        )
    }

    fn scalar_where_accepts(&self, row: usize) -> bool {
        let price = (!row.is_multiple_of(11)).then_some(row % 1001);
        let adjustment = (!row.is_multiple_of(13)).then_some(row % 101 + 450);
        if self.null_safe_where() {
            price != adjustment
        } else {
            matches!((price, adjustment), (Some(a), Some(b)) if a > b)
        }
    }

    fn expression(&self) -> &'static str {
        if self.bounded_ratio() {
            "LEAST(GREATEST(price/NULLIF(adjustment,0),-100),100)"
        } else if self.date_span() {
            "ship_date-order_date"
        } else if self.scalar_filter() || self.scalar_where() {
            "price"
        } else if matches!(self, Self::GroupedCoalesce) {
            "COALESCE(price,adjustment,0)"
        } else if matches!(self, Self::GroupedSimpleCase) {
            "CASE adjustment%3 WHEN 0 THEN price WHEN 1 THEN price*2 \
             ELSE COALESCE(price,0)-COALESCE(adjustment,0) END"
        } else if matches!(
            self,
            Self::GroupedMembership | Self::SpatialGroupedMembership
        ) {
            "CASE WHEN adjustment IN (-4,-3,-2,-1,0,1,2,3) THEN price*2 ELSE price-adjustment END"
        } else if matches!(self, Self::GroupedSimpleCaseLinear) {
            "CASE adjustment WHEN 0 THEN price*2 ELSE price-adjustment END"
        } else {
            SCALAR_ROLLUP
        }
    }

    fn date_span(&self) -> bool {
        matches!(self, Self::GroupedDateSpan | Self::SpatialGroupedDateSpan)
    }

    fn bounded_ratio(&self) -> bool {
        matches!(
            self,
            Self::GroupedBoundedRatio
                | Self::SpatialGroupedBoundedRatio
                | Self::GroupedDerivedRatio
                | Self::SpatialGroupedDerivedRatio
        )
    }

    fn derived_report(&self) -> bool {
        matches!(
            self,
            Self::GroupedDerivedRatio | Self::SpatialGroupedDerivedRatio
        )
    }
}

fn bounded_ratio_value(price: Option<i64>, adjustment: Option<i64>) -> i64 {
    let value = price
        .zip(adjustment)
        .filter(|(_, a)| *a != 0)
        .map_or(-100, |(p, a)| i128::from(p) / i128::from(a));
    i64::try_from(value.clamp(-100, 100)).expect("bounded ratio")
}

impl Workload for ScalarProgramAggregate {
    fn name(&self) -> &'static str {
        match self {
            Self::GlobalRollup => "global_scalar_program",
            Self::GroupedRollup => "grouped_scalar_program",
            Self::GroupedCoalesce => "grouped_coalesced_integer",
            Self::GroupedSimpleCase => "grouped_simple_case",
            Self::GroupedSimpleCaseLinear => "grouped_simple_case_linear",
            Self::GroupedMembership => "grouped_membership_integer",
            Self::SpatialGroupedMembership => "spatial_grouped_membership_integer",
            Self::GroupedScalarFilter => "grouped_scalar_filter",
            Self::GroupedScalarWhere => "grouped_scalar_where",
            Self::SpatialGroupedScalarWhere => "spatial_grouped_scalar_where",
            Self::GroupedNullSafeWhere => "grouped_null_safe_where",
            Self::SpatialGroupedNullSafeWhere => "spatial_grouped_null_safe_where",
            Self::GroupedBoundedRatio => "grouped_bounded_ratio",
            Self::SpatialGroupedBoundedRatio => "spatial_grouped_bounded_ratio",
            Self::GroupedDerivedRatio => "grouped_derived_ratio",
            Self::SpatialGroupedDerivedRatio => "spatial_grouped_derived_ratio",
            Self::SpatialGroupedScalarFilter => "spatial_grouped_scalar_filter",
            Self::GroupedDateSpan => "grouped_date_span",
            Self::SpatialGroupedDateSpan => "spatial_grouped_date_span",
            Self::SpatialGlobalRollup => "spatial_global_scalar_program",
            Self::SpatialGroupedRollup => "spatial_grouped_scalar_program",
        }
    }

    fn description(&self) -> &'static str {
        if self.derived_report() {
            "Inline CTE and derived-table bounded-ratio report with exact independent arithmetic/coordinate oracles"
        } else if self.bounded_ratio() {
            "Zero-safe ratio with row-wise bounds, shared SUM/AVG/COUNT/MIN/MAX and independent widened division/coordinate oracles"
        } else if self.null_safe_where() {
            "NULL-safe distinct-column WHERE rollup; independent nullable-pair and optional point/polygon oracle, no spatial index"
        } else if matches!(self, Self::SpatialGroupedScalarWhere) {
            "Exact point/polygon and multi-column scalar WHERE rollup; independent accepted-row/coordinate oracle, no spatial index"
        } else if self.scalar_where() {
            "Multi-column SQL WHERE before exact integer rollup; independent accepted-row/group oracle"
        } else if self.date_span() {
            "Nullable DATE differences feeding shared SUM/AVG/COUNT/MIN/MAX, with independent epoch-offset and optional point/polygon oracles"
        } else if self.scalar_filter() {
            "Multi-column Boolean FILTER before exact SUM/AVG/COUNT/MIN/MAX, retaining rejected-row groups and independent scalar/spatial oracles"
        } else if matches!(self, Self::SpatialGroupedMembership) {
            "Resident full-scan point/polygon conditional IN rollup; exact nullable membership and independent coordinate/arithmetic oracle, no spatial index"
        } else if matches!(self, Self::GroupedMembership) {
            "Nullable constant-list IN feeding conditional integer SUM/AVG/COUNT/MIN/MAX"
        } else if self.spatial() {
            "Resident full-scan point/polygon-with-holes CASE/COALESCE rollup; independent coordinate and widened polynomial oracle, no spatial index"
        } else if matches!(self, Self::GroupedCoalesce) {
            "Nullable grouped COALESCE feeding shared exact SUM/AVG/COUNT/MIN/MAX"
        } else if matches!(
            self,
            Self::GroupedSimpleCase | Self::GroupedSimpleCaseLinear
        ) {
            "Once-evaluated simple CASE with nullable INT4 test and shared SUM/AVG/COUNT/MIN/MAX"
        } else {
            "Shared nullable CASE/COALESCE program feeding exact SUM/AVG/COUNT/MIN/MAX"
        }
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        if self.date_span() {
            let geometry = if self.spatial() {
                ",CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry \
                 ELSE ST_SetSRID(ST_MakePoint((i%1000)::float8/50,((i/1000)%1000)::float8/50),4326) \
                 END::geometry(Point,4326) geom"
            } else {
                ""
            };
            return vec![
                "DROP TABLE IF EXISTS bench_scalar_sales".into(),
                format!(
                    "CREATE TABLE bench_scalar_sales AS SELECT \
                    CASE WHEN i%97=0 THEN NULL ELSE i%32 END region, \
                    CASE WHEN i%11=0 THEN NULL ELSE DATE '2000-01-01'+(i%1001-500) END order_date, \
                    CASE WHEN i%13=0 THEN NULL ELSE DATE '2000-01-01'+(i%101-50) END ship_date \
                    {geometry} FROM generate_series(1,{rows}) s(i)"
                ),
                "ANALYZE bench_scalar_sales".into(),
            ];
        }
        if self.spatial() {
            return vec![
                "DROP TABLE IF EXISTS bench_scalar_sales".into(),
                format!(
                    "CREATE TABLE bench_scalar_sales AS SELECT \
                    CASE WHEN i%97=0 THEN NULL ELSE i%32 END region, \
                    CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END price, \
                    CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END adjustment, \
                    CASE WHEN i%37=0 THEN NULL \
                         WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry \
                         ELSE ST_SetSRID(ST_MakePoint((i%1000)::float8/50,((i/1000)%1000)::float8/50),4326) \
                    END::geometry(Point,4326) geom FROM generate_series(1,{rows}) s(i)"
                ),
                "ANALYZE bench_scalar_sales".into(),
            ];
        }
        GroupedComputedInteger(self.global())
            .setup_sql(rows)
            .into_iter()
            .map(|sql| sql.replace("bench_computed_sales", "bench_scalar_sales"))
            .collect()
    }

    fn query_sql(&self) -> String {
        let expression = self.expression();
        let filter = if self.scalar_filter() {
            " FILTER (WHERE price>adjustment OR adjustment IS NULL)"
        } else {
            ""
        };
        let restriction = if self.spatial() {
            format!(
                " WHERE ST_Intersects(geom,'{}'::geometry){}",
                super::spatial_resident_agg_candidate::holes_polygon_wkt(),
                if self.null_safe_where() {
                    " AND price IS DISTINCT FROM adjustment"
                } else if self.scalar_where() {
                    " AND price>adjustment"
                } else {
                    ""
                }
            )
        } else if self.null_safe_where() {
            " WHERE price IS DISTINCT FROM adjustment".into()
        } else if self.scalar_where() {
            " WHERE price>adjustment".into()
        } else {
            String::new()
        };
        if self.derived_report() {
            return format!(
                "WITH report AS NOT MATERIALIZED \
                (SELECT region,{expression} value FROM bench_scalar_sales{restriction}) \
                SELECT region,sum(value) total,avg(value) mean,count(value) present,\
                min(value) lo,max(value) hi,count(*) n \
                FROM (SELECT region,value FROM report) derived GROUP BY region"
            );
        }
        format!(
            "SELECT {}sum({expression}){filter} total,avg({expression}){filter} mean,\
            count({expression}){filter} present,min({expression}){filter} lo,max({expression}){filter} hi,count(*) n \
            FROM bench_scalar_sales{restriction}{}",
            if self.global() { "" } else { "region," },
            if self.global() {
                ""
            } else {
                " GROUP BY region"
            }
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        if self.spatial() {
            &[1_000_000, 2_000_000]
        } else if matches!(
            self,
            Self::GroupedCoalesce
                | Self::GroupedSimpleCase
                | Self::GroupedSimpleCaseLinear
                | Self::GroupedMembership
                | Self::GroupedScalarFilter
                | Self::GroupedScalarWhere
                | Self::GroupedNullSafeWhere
                | Self::GroupedDateSpan
                | Self::GroupedBoundedRatio
                | Self::GroupedDerivedRatio
        ) {
            &[250_000, 1_000_000, 2_000_000]
        } else {
            &[250_000, 1_000_000]
        }
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        #[derive(Default)]
        struct State {
            sum: i64,
            count: i64,
            low: Option<i64>,
            high: Option<i64>,
            total: i64,
        }
        let mut groups = BTreeMap::<Option<usize>, State>::new();
        if self.global() {
            groups.insert(Some(0), State::default());
        }
        for row in 1..=rows {
            if self.spatial() && !spatial_scalar_member(row) {
                continue;
            }
            if self.scalar_where() && !self.scalar_where_accepts(row) {
                continue;
            }
            let key = if self.global() {
                Some(0)
            } else {
                (row % 97 != 0).then_some(row % 32)
            };
            let state = groups.entry(key).or_default();
            state.total += 1;
            let price =
                (row % 11 != 0).then_some(i64::try_from(row % 1001).expect("bounded price") - 500);
            let adjustment = (row % 13 != 0)
                .then_some(i64::try_from(row % 101).expect("bounded adjustment") - 50);
            // Independently widened arithmetic, expressed as a polynomial
            // rather than replaying the SQL expression or device program.
            let value = if self.bounded_ratio() {
                Some(bounded_ratio_value(price, adjustment))
            } else if self.date_span() {
                price.zip(adjustment).map(|(p, a)| a - p)
            } else if self.scalar_where() {
                price
            } else if self.scalar_filter() {
                match (price, adjustment) {
                    (Some(p), None) => Some(p),
                    (Some(p), Some(a)) if p > a => Some(p),
                    _ => None,
                }
            } else if matches!(self, Self::GroupedCoalesce) {
                Some(price.or(adjustment).unwrap_or(0))
            } else if matches!(self, Self::GroupedSimpleCase) {
                match adjustment.map(|a| a % 3) {
                    Some(0) => price,
                    Some(1) => price.map(|p| p + p),
                    _ => Some(price.unwrap_or(0) - adjustment.unwrap_or(0)),
                }
            } else if matches!(
                self,
                Self::GroupedMembership | Self::SpatialGroupedMembership
            ) {
                price
                    .zip(adjustment)
                    .map(|(p, a)| if (-4..=3).contains(&a) { p + p } else { p - a })
            } else if matches!(self, Self::GroupedSimpleCaseLinear) {
                price
                    .zip(adjustment)
                    .map(|(p, a)| if a == 0 { p + p } else { p - a })
            } else {
                adjustment.map_or(price, |a| {
                    Some(match price {
                        Some(p) if p > 0 => p * p - a * a,
                        _ => price.unwrap_or(0) - a,
                    })
                })
            };
            if let Some(value) = value {
                state.sum += value;
                state.count += 1;
                state.low = Some(state.low.map_or(value, |previous| previous.min(value)));
                state.high = Some(state.high.map_or(value, |previous| previous.max(value)));
            }
        }
        let nullable =
            |value: Option<i64>| value.map_or_else(|| "NULL::bigint".into(), |v| v.to_string());
        let values = groups
            .iter()
            .map(|(key, state)| {
                format!(
                    "({},{},{},{},{},{})",
                    key.map_or_else(|| "NULL::int4".into(), |v| v.to_string()),
                    nullable((state.count != 0).then_some(state.sum)),
                    state.count,
                    nullable(state.low),
                    nullable(state.high),
                    state.total
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        if values.is_empty() {
            return Some(ResultOracle::one_row(
                format!("SELECT count(*)::int8 FROM ({}) q", self.query_sql()),
                vec![ExpectedResultValue::I64(0)],
            ));
        }
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8,coalesce(bool_and(\
            actual.total IS NOT DISTINCT FROM expected.total AND \
            actual.mean IS NOT DISTINCT FROM expected.total::numeric/nullif(expected.present,0)::numeric AND \
            actual.present IS NOT DISTINCT FROM expected.present AND \
            actual.lo IS NOT DISTINCT FROM expected.lo AND actual.hi IS NOT DISTINCT FROM expected.hi AND \
            actual.n IS NOT DISTINCT FROM expected.n),false) \
            FROM ({}) actual {} (VALUES {values}) expected(region,total,present,lo,hi,n) {}",
                self.query_sql(),
                if self.global() {
                    "CROSS JOIN"
                } else {
                    "FULL JOIN"
                },
                if self.global() {
                    ""
                } else {
                    "ON jsonb_build_array(actual.region)=jsonb_build_array(expected.region)"
                }
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(groups.len()).expect("bounded groups")),
                ExpectedResultValue::Bool(true),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_scalar_sales".into()]
    }
}

// Exact integer coordinates, independent of PostGIS and the generated rings.
// Intersects includes the outer and hole boundaries, but excludes hole interiors.
fn spatial_scalar_member(row: usize) -> bool {
    let x = row % 1000;
    let y = (row / 1000) % 1000;
    !row.is_multiple_of(37)
        && !row.is_multiple_of(41)
        && (250..=750).contains(&x)
        && (250..=750).contains(&y)
        && !(x > 350 && x < 450 && y > 350 && y < 450)
        && !(x > 550 && x < 650 && y > 550 && y < 650)
}

impl Workload for GroupedComputedInteger {
    fn name(&self) -> &'static str {
        if self.0 {
            "global_computed_integer"
        } else {
            "grouped_computed_integer"
        }
    }

    fn description(&self) -> &'static str {
        "Checked INT4 sales arithmetic: product revenue, net price AVG and combined-price extrema"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_computed_sales".into(),
            "CREATE TABLE bench_computed_sales (region int4, price int4, adjustment int4)".into(),
            format!(
                "INSERT INTO bench_computed_sales SELECT \
                 CASE WHEN i%97=0 THEN NULL ELSE i%32 END, \
                 CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END, \
                 CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END \
                 FROM generate_series(1,{rows}) s(i)"
            ),
            "ANALYZE bench_computed_sales".into(),
        ]
    }

    fn query_sql(&self) -> String {
        let aggregates = "sum(price*adjustment) revenue,avg(price-adjustment) net, \
         min(price+adjustment) lo,max(price+adjustment) hi,count(*) n \
         FROM bench_computed_sales";
        if self.0 {
            format!("SELECT {aggregates}")
        } else {
            format!("SELECT region,{aggregates} GROUP BY region")
        }
    }

    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        #[derive(Default)]
        struct State {
            revenue: i64,
            net: i64,
            count: i64,
            low: Option<i64>,
            high: Option<i64>,
            total: i64,
        }
        let mut groups = BTreeMap::<Option<usize>, State>::new();
        if self.0 {
            groups.insert(Some(0), State::default());
        }
        for row in 1..=rows {
            let state = groups
                .entry(if self.0 {
                    Some(0)
                } else {
                    (row % 97 != 0).then_some(row % 32)
                })
                .or_default();
            state.total += 1;
            if row % 11 == 0 || row % 13 == 0 {
                continue;
            }
            let price = i64::try_from(row % 1001).expect("bounded price") - 500;
            let adjustment = i64::try_from(row % 101).expect("bounded adjustment") - 50;
            state.revenue += price * adjustment;
            state.net += price - adjustment;
            state.count += 1;
            let combined = price + adjustment;
            state.low = Some(
                state
                    .low
                    .map_or(combined, |previous| previous.min(combined)),
            );
            state.high = Some(
                state
                    .high
                    .map_or(combined, |previous| previous.max(combined)),
            );
        }
        let nullable =
            |value: Option<i64>| value.map_or_else(|| "NULL::bigint".into(), |v| v.to_string());
        let values = groups
            .iter()
            .map(|(key, state)| {
                let key = key.map_or_else(|| "NULL::int4".into(), |v| v.to_string());
                format!(
                    "({key},{},{},{},{},{},{})",
                    nullable((state.count != 0).then_some(state.revenue)),
                    state.net,
                    state.count,
                    nullable(state.low),
                    nullable(state.high),
                    state.total
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let join = if self.0 { "CROSS JOIN" } else { "FULL JOIN" };
        let condition = if self.0 {
            ""
        } else {
            "ON jsonb_build_array(actual.region)=jsonb_build_array(expected.region)"
        };
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8,coalesce(bool_and( \
             actual.revenue IS NOT DISTINCT FROM expected.revenue AND \
             actual.net IS NOT DISTINCT FROM expected.net::numeric/nullif(expected.c,0)::numeric AND \
             actual.lo IS NOT DISTINCT FROM expected.lo AND actual.hi IS NOT DISTINCT FROM expected.hi AND \
             actual.n IS NOT DISTINCT FROM expected.n),false) \
             FROM ({query}) actual {join} (VALUES {values}) expected(region,revenue,net,c,lo,hi,n) \
             {condition}",
                query = self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(groups.len()).expect("bounded groups")),
                ExpectedResultValue::Bool(true),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_computed_sales".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_reports_keep_direct_fixture_and_independent_oracle() {
        for (report, direct) in [
            (
                ScalarProgramAggregate::GroupedDerivedRatio,
                ScalarProgramAggregate::GroupedBoundedRatio,
            ),
            (
                ScalarProgramAggregate::SpatialGroupedDerivedRatio,
                ScalarProgramAggregate::SpatialGroupedBoundedRatio,
            ),
        ] {
            assert_eq!(report.setup_sql(10_000), direct.setup_sql(10_000));
            let mut report_oracle = report.result_oracle(10_000).expect("report oracle");
            report_oracle.query_sql = report_oracle
                .query_sql
                .replace(&report.query_sql(), &direct.query_sql());
            assert_eq!(Some(report_oracle), direct.result_oracle(10_000));
            assert_eq!(report.row_scales(), direct.row_scales());
            let sql = report.query_sql();
            assert!(sql.starts_with("WITH report AS NOT MATERIALIZED"));
            assert!(sql.contains("FROM (SELECT region,value FROM report) derived"));
            assert_eq!(sql.contains("ST_Intersects"), report.spatial());
        }
    }

    #[test]
    fn bounded_ratio_oracle_handles_zero_null_sign_and_extrema() {
        for (price, adjustment, expected) in [
            (None, Some(1), -100),
            (Some(7), None, -100),
            (Some(7), Some(0), -100),
            (None, None, -100),
            (Some(-7), Some(3), -2),
            (Some(7), Some(-3), -2),
            (Some(500), Some(1), 100),
            (Some(-500), Some(1), -100),
            (Some(i64::MIN), Some(-1), 100),
        ] {
            assert_eq!(bounded_ratio_value(price, adjustment), expected);
        }
        for workload in [
            ScalarProgramAggregate::GroupedBoundedRatio,
            ScalarProgramAggregate::SpatialGroupedBoundedRatio,
        ] {
            assert!(
                workload
                    .query_sql()
                    .contains("LEAST(GREATEST(price/NULLIF(adjustment,0),-100),100)")
            );
            assert_eq!(
                workload.query_sql().contains("ST_Intersects"),
                workload.spatial()
            );
            assert_eq!(
                workload
                    .result_oracle(1_000_000)
                    .expect("oracle")
                    .expected_row,
                vec![
                    ExpectedResultValue::I64(33),
                    ExpectedResultValue::Bool(true)
                ]
            );
        }
    }

    #[test]
    fn null_safe_where_oracle_includes_one_null_but_rejects_two_nulls() {
        let workload = ScalarProgramAggregate::GroupedNullSafeWhere;
        assert!(
            workload
                .query_sql()
                .contains("price IS DISTINCT FROM adjustment")
        );
        assert!(workload.scalar_where_accepts(11));
        assert!(workload.scalar_where_accepts(13));
        assert!(!workload.scalar_where_accepts(143));
        let mut equal_non_null = 0;
        for row in 1..=100_001 {
            let price = (row % 11 != 0).then_some(i64::try_from(row % 1001).expect("price") - 500);
            let adjustment =
                (row % 13 != 0).then_some(i64::try_from(row % 101).expect("adjustment") - 50);
            assert_eq!(workload.scalar_where_accepts(row), price != adjustment);
            if price.is_some() && price == adjustment {
                equal_non_null += 1;
            }
        }
        assert!(equal_non_null > 0);
        let spatial = ScalarProgramAggregate::SpatialGroupedNullSafeWhere;
        assert!(
            spatial
                .query_sql()
                .contains("AND price IS DISTINCT FROM adjustment")
        );
        let rows = 400_001;
        let groups = (1..=rows)
            .filter(|row| spatial_scalar_member(*row) && spatial.scalar_where_accepts(*row))
            .map(|row| (row % 97 != 0).then_some(row % 32))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            spatial
                .result_oracle(rows)
                .expect("spatial oracle")
                .expected_row,
            vec![
                ExpectedResultValue::I64(i64::try_from(groups.len()).expect("bounded groups")),
                ExpectedResultValue::Bool(true)
            ]
        );
    }

    #[test]
    fn scalar_where_oracle_rejects_rows_before_creating_groups() {
        let workload = ScalarProgramAggregate::GroupedScalarWhere;
        assert!(workload.query_sql().contains("WHERE price>adjustment"));
        assert!(!workload.query_sql().contains("FILTER"));
        assert_eq!(
            workload
                .result_oracle(1)
                .expect("empty oracle")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
        let oracle = workload.result_oracle(1001).expect("accepted rows");
        assert!(oracle.query_sql.contains("FULL JOIN"));
        assert!(
            !oracle
                .query_sql
                .contains("NULL::bigint,0,NULL::bigint,NULL::bigint,1")
        );
    }

    #[test]
    fn spatial_scalar_where_oracle_combines_both_predicates_before_grouping() {
        let workload = ScalarProgramAggregate::SpatialGroupedScalarWhere;
        assert!(workload.query_sql().contains("ST_Intersects"));
        assert!(workload.query_sql().contains("AND price>adjustment"));
        assert!(!workload.query_sql().contains("FILTER"));
        assert_eq!(workload.row_scales(), &[1_000_000, 2_000_000]);
        let rows = 400_001;
        let expected_groups = (1..=rows)
            .filter(|row| {
                spatial_scalar_member(*row)
                    && row % 11 != 0
                    && row % 13 != 0
                    && i64::try_from(row % 1001).expect("bounded price") - 500
                        > i64::try_from(row % 101).expect("bounded adjustment") - 50
            })
            .map(|row| (row % 97 != 0).then_some(row % 32))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(!expected_groups.is_empty());
        let oracle = workload.result_oracle(rows).expect("spatial WHERE oracle");
        assert_eq!(
            oracle.expected_row,
            vec![
                ExpectedResultValue::I64(
                    i64::try_from(expected_groups.len()).expect("bounded groups")
                ),
                ExpectedResultValue::Bool(true),
            ]
        );
        assert_eq!(
            workload.result_oracle(1).expect("empty").expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
    }

    #[test]
    fn date_span_oracle_uses_nullable_epoch_offsets_and_spatial_membership() {
        let workload = ScalarProgramAggregate::GroupedDateSpan;
        assert!(workload.query_sql().contains("ship_date-order_date"));
        let setup = workload.setup_sql(13).join(";");
        assert!(setup.contains("DATE '2000-01-01'+(i%1001-500)"));
        for (rows, expected) in [
            (1, "(1,450,1,450,450,1)"),
            (11, "(11,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (13, "(13,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
        ] {
            assert!(
                workload
                    .result_oracle(rows)
                    .expect("date span oracle")
                    .query_sql
                    .contains(expected)
            );
        }
        let spatial = ScalarProgramAggregate::SpatialGroupedDateSpan;
        assert!(spatial.query_sql().contains("ST_Intersects"));
        assert!(
            spatial
                .setup_sql(13)
                .join(";")
                .contains("geometry(Point,4326)")
        );
        assert_eq!(
            spatial
                .result_oracle(1)
                .expect("outside polygon")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
    }

    #[test]
    fn scalar_filter_oracle_keeps_rejected_groups_and_nullable_predicates() {
        let workload = ScalarProgramAggregate::GroupedScalarFilter;
        assert!(
            workload
                .query_sql()
                .contains("FILTER (WHERE price>adjustment OR adjustment IS NULL)")
        );
        for (rows, expected) in [
            (1, "(1,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (11, "(11,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (13, "(13,-487,1,-487,-487,1)"),
        ] {
            assert!(
                workload
                    .result_oracle(rows)
                    .expect("filter oracle")
                    .query_sql
                    .contains(expected)
            );
        }
        let spatial = ScalarProgramAggregate::SpatialGroupedScalarFilter;
        assert!(spatial.query_sql().contains("ST_Intersects"));
        assert!(spatial.query_sql().contains("FILTER (WHERE"));
        assert_eq!(
            spatial
                .result_oracle(1)
                .expect("outside polygon")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
    }

    #[test]
    fn simple_case_oracle_covers_nullable_test_and_branch_results() {
        let workload = ScalarProgramAggregate::GroupedSimpleCase;
        for (rows, expected) in [
            (1, "(1,-450,1,-450,-450,1)"), // negative remainder: ELSE
            (2, "(2,-498,1,-498,-498,1)"), // zero remainder: price
            (11, "(11,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (13, "(13,-487,1,-487,-487,1)"), // NULL test: ELSE
        ] {
            assert!(
                workload
                    .result_oracle(rows)
                    .expect("CASE oracle")
                    .query_sql
                    .contains(expected)
            );
        }
        assert!(workload.query_sql().contains("CASE adjustment%3 WHEN"));
        assert_eq!(
            workload
                .result_oracle(0)
                .expect("empty groups")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
        let linear = ScalarProgramAggregate::GroupedSimpleCaseLinear;
        for (rows, expected) in [
            (1, "(1,-450,1,-450,-450,1)"),
            (11, "(11,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (13, "(13,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (50, "(18,-1350,2,-900,-450,2)"), // row 50 has zero adjustment
        ] {
            assert!(
                linear
                    .result_oracle(rows)
                    .expect("linear CASE oracle")
                    .query_sql
                    .contains(expected)
            );
        }
    }

    #[test]
    fn membership_oracle_covers_hits_misses_nulls_and_spatial_composition() {
        let workload = ScalarProgramAggregate::GroupedMembership;
        assert!(workload.query_sql().contains(" IN (-4,-3,-2,-1,0,1,2,3)"));
        for (rows, expected) in [
            (1, "(1,-450,1,-450,-450,1)"),
            (13, "(13,NULL::bigint,0,NULL::bigint,NULL::bigint,1)"),
            (46, "(14,-1358,2,-908,-450,2)"), // -4: inclusive lower edge
            (53, "(21,-1344,2,-894,-450,2)"), // 3: inclusive upper edge
            (54, "(22,-450,1,-450,-450,2)"),  // 4: miss; row 22 price is NULL
        ] {
            assert!(
                workload
                    .result_oracle(rows)
                    .expect("membership oracle")
                    .query_sql
                    .contains(expected),
                "rows={rows}"
            );
        }
        let spatial = ScalarProgramAggregate::SpatialGroupedMembership;
        assert!(spatial.query_sql().contains("ST_Intersects"));
        assert!(spatial.query_sql().contains(" IN (-4,-3,-2,-1,0,1,2,3)"));
        assert_eq!(
            spatial.result_oracle(1).expect("outside area").expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
    }

    #[test]
    fn spatial_scalar_oracle_covers_boundaries_holes_nulls_and_empty_points() {
        assert!(spatial_scalar_member(250_250)); // outer corner
        assert!(spatial_scalar_member(350_350)); // hole corner
        assert!(!spatial_scalar_member(400_400)); // first hole interior
        assert!(!spatial_scalar_member(600_600)); // second hole interior
        assert!(!spatial_scalar_member(249_500)); // outside
        assert!(!spatial_scalar_member(500_277)); // NULL (multiple of 37)
        assert!(!spatial_scalar_member(500_282)); // empty (multiple of 41)
        for workload in [
            ScalarProgramAggregate::SpatialGlobalRollup,
            ScalarProgramAggregate::SpatialGroupedRollup,
        ] {
            assert!(workload.query_sql().contains("ST_Intersects"));
            assert!(!workload.setup_sql(1).join(" ").contains("INDEX"));
            for rows in workload.row_scales() {
                assert_eq!(
                    workload
                        .result_oracle(*rows)
                        .expect("spatial oracle")
                        .expected_row,
                    vec![
                        ExpectedResultValue::I64(if workload.global() { 1 } else { 33 }),
                        ExpectedResultValue::Bool(true)
                    ]
                );
            }
        }
    }

    #[test]
    fn coalesced_rollup_oracle_covers_each_null_branch_and_null_group() {
        let workload = ScalarProgramAggregate::GroupedCoalesce;
        for (rows, expected) in [
            (1, "(1,-499,1,-499,-499,1)"),
            (11, "(11,-39,1,-39,-39,1)"),
            (13, "(13,-487,1,-487,-487,1)"),
            (143, "(15,-1748,5,-485,0,5)"),
            (143, "(NULL::int4,-403,1,-403,-403,1)"),
        ] {
            assert!(
                workload
                    .result_oracle(rows)
                    .expect("coalesced fixture")
                    .query_sql
                    .contains(expected),
                "missing golden group {expected} at {rows} rows"
            );
        }
        assert_eq!(
            workload
                .result_oracle(0)
                .expect("empty groups")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
    }

    #[test]
    fn scalar_rollup_has_independent_nullable_polynomial_oracle() {
        let grouped = ScalarProgramAggregate::GroupedRollup;
        assert!(
            grouped
                .result_oracle(1)
                .expect("single row")
                .query_sql
                .contains("(1,-450,1,-450,-450,1)")
        );
        assert_eq!(
            grouped
                .result_oracle(0)
                .expect("empty grouping")
                .expected_row,
            vec![ExpectedResultValue::I64(0)]
        );
        let global = ScalarProgramAggregate::GlobalRollup;
        assert!(
            global
                .result_oracle(0)
                .expect("empty reduction")
                .query_sql
                .contains("(0,NULL::bigint,0,NULL::bigint,NULL::bigint,0)")
        );
        for rows in grouped.row_scales() {
            assert_eq!(
                grouped
                    .result_oracle(*rows)
                    .expect("grouped oracle")
                    .expected_row,
                vec![
                    ExpectedResultValue::I64(33),
                    ExpectedResultValue::Bool(true)
                ]
            );
            assert_eq!(
                global
                    .result_oracle(*rows)
                    .expect("global oracle")
                    .expected_row,
                vec![ExpectedResultValue::I64(1), ExpectedResultValue::Bool(true)]
            );
        }
    }

    #[test]
    fn mixed_arithmetic_has_an_independent_widened_oracle() {
        let workload = GroupedComputedInteger(false);
        for rows in workload.row_scales() {
            let oracle = workload.result_oracle(*rows).expect("oracle");
            assert_eq!(
                oracle.expected_row,
                [
                    ExpectedResultValue::I64(33),
                    ExpectedResultValue::Bool(true)
                ]
            );
            assert!(oracle.query_sql.contains(&workload.query_sql()));
            assert!(
                oracle
                    .query_sql
                    .contains("expected.net::numeric/nullif(expected.c,0)::numeric")
            );
        }
        let single = workload.result_oracle(1).expect("single row");
        assert!(single.query_sql.contains("(1,24451,-450,1,-548,-548,1)"));
        let global = GroupedComputedInteger(true);
        let oracle = global.result_oracle(1_000_000).expect("global oracle");
        assert_eq!(
            oracle.expected_row,
            [ExpectedResultValue::I64(1), ExpectedResultValue::Bool(true)]
        );
        assert!(!global.query_sql().contains("GROUP BY"));
        let empty = global.result_oracle(0).expect("empty global oracle");
        assert_eq!(
            empty.expected_row,
            [ExpectedResultValue::I64(1), ExpectedResultValue::Bool(true)]
        );
        assert!(
            empty
                .query_sql
                .contains("(0,NULL::bigint,0,0,NULL::bigint,NULL::bigint,0)")
        );
    }
}

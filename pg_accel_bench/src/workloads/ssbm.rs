use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

const SSBM_Q23_EXACT_BRAND_MIN_PARTS: usize = 1_000;

fn ssbm_dimension_counts(rows: usize) -> (usize, usize, usize) {
    let part_count = (rows / 30).clamp(SSBM_Q23_EXACT_BRAND_MIN_PARTS, 800_000);
    let supp_count = (rows / 3000).clamp(20, 200_000);
    let cust_count = (rows / 200).clamp(30, 3_000_000);
    (part_count, supp_count, cust_count)
}

fn ssbm_dimension_sanity_sql() -> String {
    "DO $$ \
     DECLARE \
        missing text; \
     BEGIN \
        SELECT label INTO missing \
        FROM (VALUES \
            ('ssbm_part.p_mfgr IN (''MFGR#1'', ''MFGR#2'') (SSBM Q4.1/Q4.2)', EXISTS (SELECT 1 FROM ssbm_part WHERE p_mfgr IN ('MFGR#1', 'MFGR#2'))), \
            ('ssbm_part.p_category = ''MFGR#12'' (SSBM Q2.1)', EXISTS (SELECT 1 FROM ssbm_part WHERE p_category = 'MFGR#12')), \
            ('ssbm_part.p_category = ''MFGR#14'' (SSBM Q4.3)', EXISTS (SELECT 1 FROM ssbm_part WHERE p_category = 'MFGR#14')), \
            ('ssbm_part.p_brand1 = ''MFGR#2239'' (SSBM Q2.3)', EXISTS (SELECT 1 FROM ssbm_part WHERE p_brand1 = 'MFGR#2239')), \
            ('ssbm_part.p_brand1 BETWEEN ''MFGR#2221'' AND ''MFGR#2228'' (SSBM Q2.2)', EXISTS (SELECT 1 FROM ssbm_part WHERE p_brand1 BETWEEN 'MFGR#2221' AND 'MFGR#2228')), \
            ('ssbm_supplier.s_region = ''AMERICA'' (SSBM Q2.1/Q4)', EXISTS (SELECT 1 FROM ssbm_supplier WHERE s_region = 'AMERICA')), \
            ('ssbm_supplier.s_region = ''ASIA'' (SSBM Q2.2/Q3.1)', EXISTS (SELECT 1 FROM ssbm_supplier WHERE s_region = 'ASIA')), \
            ('ssbm_supplier.s_region = ''EUROPE'' (SSBM Q2.3)', EXISTS (SELECT 1 FROM ssbm_supplier WHERE s_region = 'EUROPE')), \
            ('ssbm_supplier.s_nation = ''UNITED STATES'' (SSBM Q3.2/Q4.3)', EXISTS (SELECT 1 FROM ssbm_supplier WHERE s_nation = 'UNITED STATES')), \
            ('ssbm_supplier.s_city IN (''UNITED ST0'', ''UNITED ST1'') (SSBM Q3.3/Q3.4)', EXISTS (SELECT 1 FROM ssbm_supplier WHERE s_city IN ('UNITED ST0', 'UNITED ST1'))), \
            ('ssbm_customer.c_region = ''AMERICA'' (SSBM Q4)', EXISTS (SELECT 1 FROM ssbm_customer WHERE c_region = 'AMERICA')), \
            ('ssbm_customer.c_region = ''ASIA'' (SSBM Q3.1)', EXISTS (SELECT 1 FROM ssbm_customer WHERE c_region = 'ASIA')), \
            ('ssbm_customer.c_nation = ''UNITED STATES'' (SSBM Q3.2)', EXISTS (SELECT 1 FROM ssbm_customer WHERE c_nation = 'UNITED STATES')), \
            ('ssbm_customer.c_city IN (''UNITED ST0'', ''UNITED ST1'') (SSBM Q3.3/Q3.4)', EXISTS (SELECT 1 FROM ssbm_customer WHERE c_city IN ('UNITED ST0', 'UNITED ST1'))), \
            ('ssbm_date.d_year = 1992 (SSBM Q3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1992)), \
            ('ssbm_date.d_year = 1993 (SSBM Q1.1/Q3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1993)), \
            ('ssbm_date.d_year = 1994 (SSBM Q1.3/Q3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1994)), \
            ('ssbm_date.d_year = 1995 (SSBM Q3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1995)), \
            ('ssbm_date.d_year = 1996 (SSBM Q3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1996)), \
            ('ssbm_date.d_year = 1997 (SSBM Q3/Q4)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1997)), \
            ('ssbm_date.d_year = 1998 (SSBM Q4)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_year = 1998)), \
            ('ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_yearmonthnum = 199401)), \
            ('ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_weeknuminyear = 6 AND d_year = 1994)), \
            ('ssbm_date.d_yearmonth = ''Dec1997'' (SSBM Q3.4)', EXISTS (SELECT 1 FROM ssbm_date WHERE d_yearmonth = 'Dec1997')) \
        ) AS checks(label, ok) \
        WHERE NOT ok \
        LIMIT 1; \
        IF missing IS NOT NULL THEN \
            RAISE EXCEPTION 'SSBM setup sanity check failed: % matched zero dimension rows. Fix ssbm_setup_sql generator constants before benchmarking GPU pre-aggregation.', missing; \
        END IF; \
     END $$"
        .to_owned()
}

/// Shared setup SQL for all SSBM workloads. Creates the star schema (date, part,
/// supplier, customer, lineorder) and populates with synthetic data scaled to `rows`.
#[allow(clippy::too_many_lines)]
pub fn ssbm_setup_sql(rows: usize) -> Vec<String> {
    let (part_count, supp_count, cust_count) = ssbm_dimension_counts(rows);

    vec![
        // -- Drop any leftover tables (idempotent) --
        "DROP TABLE IF EXISTS ssbm_lineorder".to_owned(),
        "DROP TABLE IF EXISTS ssbm_customer".to_owned(),
        "DROP TABLE IF EXISTS ssbm_supplier".to_owned(),
        "DROP TABLE IF EXISTS ssbm_part".to_owned(),
        "DROP TABLE IF EXISTS ssbm_date".to_owned(),
        // -- Create dimension tables --
        "CREATE TABLE ssbm_date (\
            d_datekey int4 PRIMARY KEY, \
            d_date text NOT NULL, \
            d_dayofweek text NOT NULL, \
            d_month text NOT NULL, \
            d_year int4 NOT NULL, \
            d_yearmonthnum int4 NOT NULL, \
            d_yearmonth text NOT NULL, \
            d_daynuminweek int4 NOT NULL, \
            d_daynuminmonth int4 NOT NULL, \
            d_daynuminyear int4 NOT NULL, \
            d_monthnuminyear int4 NOT NULL, \
            d_weeknuminyear int4 NOT NULL, \
            d_sellingseason text NOT NULL, \
            d_lastdayinweekfl int4 NOT NULL, \
            d_lastdayinmonthfl int4 NOT NULL, \
            d_holidayfl int4 NOT NULL, \
            d_weekdayfl int4 NOT NULL\
        )"
        .to_owned(),
        "CREATE TABLE ssbm_part (\
            p_partkey int4 PRIMARY KEY, \
            p_name text NOT NULL, \
            p_mfgr text NOT NULL, \
            p_category text NOT NULL, \
            p_brand1 text NOT NULL, \
            p_color text NOT NULL, \
            p_type text NOT NULL, \
            p_size int4 NOT NULL, \
            p_container text NOT NULL\
        )"
        .to_owned(),
        "CREATE TABLE ssbm_supplier (\
            s_suppkey int4 PRIMARY KEY, \
            s_name text NOT NULL, \
            s_address text NOT NULL, \
            s_city text NOT NULL, \
            s_nation text NOT NULL, \
            s_region text NOT NULL, \
            s_phone text NOT NULL\
        )"
        .to_owned(),
        "CREATE TABLE ssbm_customer (\
            c_custkey int4 PRIMARY KEY, \
            c_name text NOT NULL, \
            c_address text NOT NULL, \
            c_city text NOT NULL, \
            c_nation text NOT NULL, \
            c_region text NOT NULL, \
            c_phone text NOT NULL, \
            c_mktsegment text NOT NULL\
        )"
        .to_owned(),
        "CREATE TABLE ssbm_lineorder (\
            lo_orderkey int4 NOT NULL, \
            lo_linenumber int4 NOT NULL, \
            lo_custkey int4 NOT NULL, \
            lo_partkey int4 NOT NULL, \
            lo_suppkey int4 NOT NULL, \
            lo_orderdate int4 NOT NULL, \
            lo_orderpriority text NOT NULL, \
            lo_shippriority int4 NOT NULL, \
            lo_quantity int4 NOT NULL, \
            lo_extendedprice int4 NOT NULL, \
            lo_ordtotalprice int4 NOT NULL, \
            lo_discount int4 NOT NULL, \
            lo_revenue int4 NOT NULL, \
            lo_supplycost int4 NOT NULL, \
            lo_tax int4 NOT NULL, \
            lo_commitdate int4 NOT NULL, \
            lo_shipmode text NOT NULL, \
            PRIMARY KEY (lo_orderkey, lo_linenumber)\
        )"
        .to_owned(),
        // -- Populate dimension tables --
        "INSERT INTO ssbm_date \
         SELECT \
            19920101 + i AS d_datekey, \
            to_char(dt, 'YYYY-MM-DD') AS d_date, \
            to_char(dt, 'FMDay') AS d_dayofweek, \
            to_char(dt, 'FMMonth') AS d_month, \
            EXTRACT(YEAR FROM dt)::int4 AS d_year, \
            EXTRACT(YEAR FROM dt)::int4 * 100 + EXTRACT(MONTH FROM dt)::int4 AS d_yearmonthnum, \
            to_char(dt, 'MonYYYY') AS d_yearmonth, \
            EXTRACT(ISODOW FROM dt)::int4 AS d_daynuminweek, \
            EXTRACT(DAY FROM dt)::int4 AS d_daynuminmonth, \
            EXTRACT(DOY FROM dt)::int4 AS d_daynuminyear, \
            EXTRACT(MONTH FROM dt)::int4 AS d_monthnuminyear, \
            ((EXTRACT(DOY FROM dt)::int4 - 1) / 7 + 1) AS d_weeknuminyear, \
            CASE WHEN EXTRACT(MONTH FROM dt)::int4 IN (11, 12) THEN 'Christmas' WHEN EXTRACT(MONTH FROM dt)::int4 IN (6, 7, 8) THEN 'Summer' ELSE 'Other' END AS d_sellingseason, \
            CASE WHEN EXTRACT(ISODOW FROM dt)::int4 = 7 THEN 1 ELSE 0 END AS d_lastdayinweekfl, \
            CASE WHEN EXTRACT(DAY FROM dt + 1)::int4 = 1 THEN 1 ELSE 0 END AS d_lastdayinmonthfl, \
            CASE WHEN EXTRACT(DOY FROM dt)::int4 IN (1, 186) THEN 1 ELSE 0 END AS d_holidayfl, \
            CASE WHEN EXTRACT(ISODOW FROM dt)::int4 <= 5 THEN 1 ELSE 0 END AS d_weekdayfl \
         FROM (SELECT i, date '1992-01-01' + i AS dt FROM generate_series(0, 2555) AS g(i)) AS dates"
            .to_owned(),
        format!(
            "INSERT INTO ssbm_part \
             SELECT \
                i AS p_partkey, \
                'part_' || i AS p_name, \
                'MFGR#' || ((i - 1) % 5 + 1) AS p_mfgr, \
                'MFGR#' || ((i - 1) % 5 + 1) || (((i - 1) / 5) % 5 + 1) AS p_category, \
                'MFGR#' || ((i - 1) % 5 + 1) || (((i - 1) / 5) % 5 + 1) || (((i - 1) / 25) % 40 + 1) AS p_brand1, \
                (ARRAY['red','green','blue','yellow','black','white','orange'])[(i % 7) + 1] AS p_color, \
                'TYPE' || (i % 150 + 1) AS p_type, \
                i % 50 + 1 AS p_size, \
                (ARRAY['SM CASE','SM BOX','SM PACK','SM PKG','MED BAG','MED BOX','LG CASE','LG BOX'])[(i % 8) + 1] AS p_container \
             FROM generate_series(1, {part_count}) AS i"
        ),
        format!(
            "INSERT INTO ssbm_supplier \
             SELECT \
                i AS s_suppkey, \
                'Supplier#' || lpad(i::text, 9, '0') AS s_name, \
                'addr_' || i AS s_address, \
                (ARRAY['UNITED ST0','UNITED ST1','UNITED ST2','UNITED ST3','UNITED ST4','CHINA    0','CHINA    1','CHINA    2','CHINA    3','CHINA    4'])[(i % 10) + 1] AS s_city, \
                (ARRAY['UNITED STATES','CHINA','INDIA','JAPAN','GERMANY'])[(i % 5) + 1] AS s_nation, \
                (ARRAY['AMERICA','ASIA','ASIA','ASIA','EUROPE'])[(i % 5) + 1] AS s_region, \
                '555-' || lpad((i % 10000)::text, 4, '0') AS s_phone \
             FROM generate_series(1, {supp_count}) AS i"
        ),
        format!(
            "INSERT INTO ssbm_customer \
             SELECT \
                i AS c_custkey, \
                'Customer#' || lpad(i::text, 9, '0') AS c_name, \
                'addr_' || i AS c_address, \
                (ARRAY['UNITED ST0','UNITED ST1','UNITED ST2','UNITED ST3','UNITED ST4','CHINA    0','CHINA    1','CHINA    2','CHINA    3','CHINA    4'])[(i % 10) + 1] AS c_city, \
                (ARRAY['UNITED STATES','CHINA','INDIA','JAPAN','GERMANY'])[(i % 5) + 1] AS c_nation, \
                (ARRAY['AMERICA','ASIA','ASIA','ASIA','EUROPE'])[(i % 5) + 1] AS c_region, \
                '555-' || lpad((i % 10000)::text, 4, '0') AS c_phone, \
                (ARRAY['AUTOMOBILE','BUILDING','FURNITURE','HOUSEHOLD','MACHINERY'])[(i % 5) + 1] AS c_mktsegment \
             FROM generate_series(1, {cust_count}) AS i"
        ),
        ssbm_dimension_sanity_sql(),
        // PostgreSQL's session-local PRNG drives the canonical fact fixture.
        // Pin it so repeated native/GPU release runs start from identical data.
        "SELECT setseed(0.424242)".to_owned(),
        // -- Populate fact table --
        format!(
            "INSERT INTO ssbm_lineorder \
             SELECT \
                i / 7 + 1 AS lo_orderkey, \
                i % 7 + 1 AS lo_linenumber, \
                (random() * ({cust_count} - 1))::int4 + 1 AS lo_custkey, \
                (random() * ({part_count} - 1))::int4 + 1 AS lo_partkey, \
                (random() * ({supp_count} - 1))::int4 + 1 AS lo_suppkey, \
                19920101 + (random() * 2555)::int4 AS lo_orderdate, \
                (ARRAY['1-URGENT','2-HIGH','3-MEDIUM','4-NOT SPECIFIED','5-LOW'])[(i % 5) + 1] AS lo_orderpriority, \
                0 AS lo_shippriority, \
                (random() * 49)::int4 + 1 AS lo_quantity, \
                (random() * 55450)::int4 + 901 AS lo_extendedprice, \
                (random() * 400000)::int4 + 1 AS lo_ordtotalprice, \
                (random() * 10)::int4 AS lo_discount, \
                0 AS lo_revenue, \
                (random() * 25000)::int4 + 600 AS lo_supplycost, \
                (random() * 8)::int4 AS lo_tax, \
                19920101 + (random() * 2555)::int4 AS lo_commitdate, \
                (ARRAY['REG AIR','AIR','RAIL','SHIP','TRUCK','MAIL','FOB'])[(i % 7) + 1] AS lo_shipmode \
             FROM generate_series(1, {rows}) AS i"
        ),
        // -- Compute derived revenue column --
        "UPDATE ssbm_lineorder SET lo_revenue = lo_extendedprice * (100 - lo_discount) / 100"
            .to_owned(),
        // -- Analyze all tables --
        "ANALYZE ssbm_date".to_owned(),
        "ANALYZE ssbm_part".to_owned(),
        "ANALYZE ssbm_supplier".to_owned(),
        "ANALYZE ssbm_customer".to_owned(),
        "ANALYZE ssbm_lineorder".to_owned(),
    ]
}

/// Shared cleanup SQL for all SSBM workloads.
pub fn ssbm_cleanup_sql() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS ssbm_lineorder".to_owned(),
        "DROP TABLE IF EXISTS ssbm_customer".to_owned(),
        "DROP TABLE IF EXISTS ssbm_supplier".to_owned(),
        "DROP TABLE IF EXISTS ssbm_part".to_owned(),
        "DROP TABLE IF EXISTS ssbm_date".to_owned(),
    ]
}

// ---------------------------------------------------------------------------
// Q1 — Revenue from discounted lineorders with date/quantity filters
// ---------------------------------------------------------------------------

/// Exact current-planner sentinel over the canonical SSBM fact/date schema.
pub struct SsbmResidentInt4Star;

impl Workload for SsbmResidentInt4Star {
    fn name(&self) -> &'static str {
        "ssbm_resident_int4_star"
    }

    fn description(&self) -> &'static str {
        "SSBM lineorder/date join grouped by int4 year with exact SUM(int4) and COUNT(*)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT d_year, SUM(lo_revenue) AS sum, COUNT(*) AS count \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         GROUP BY d_year"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(ResultOracle::one_row(
            format!(
                "SELECT COALESCE(SUM(q.count), 0)::int8 AS input_rows \
                 FROM ({}) AS q",
                self.query_sql()
            ),
            vec![Value::I64(usize_to_i64(rows))],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ1_1;

impl Workload for SsbmQ1_1 {
    fn name(&self) -> &'static str {
        "ssbm_q1_1"
    }

    fn description(&self) -> &'static str {
        "SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_extendedprice * lo_discount) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         WHERE d_year = 1993 \
           AND lo_discount BETWEEN 1 AND 3 \
           AND lo_quantity < 25"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    struct GeneratedDate {
        year: i32,
        month: usize,
        day_of_year: usize,
    }

    fn part_category(partkey: usize) -> String {
        format!(
            "MFGR#{}{}",
            (partkey - 1) % 5 + 1,
            ((partkey - 1) / 5) % 5 + 1
        )
    }

    fn part_mfgr(partkey: usize) -> String {
        format!("MFGR#{}", (partkey - 1) % 5 + 1)
    }

    fn part_brand1(partkey: usize) -> String {
        format!(
            "{}{}",
            part_category(partkey),
            ((partkey - 1) / 25) % 40 + 1
        )
    }

    fn supplier_region(suppkey: usize) -> &'static str {
        ["AMERICA", "ASIA", "ASIA", "ASIA", "EUROPE"][suppkey % 5]
    }

    fn supplier_nation(suppkey: usize) -> &'static str {
        ["UNITED STATES", "CHINA", "INDIA", "JAPAN", "GERMANY"][suppkey % 5]
    }

    fn supplier_city(suppkey: usize) -> &'static str {
        [
            "UNITED ST0",
            "UNITED ST1",
            "UNITED ST2",
            "UNITED ST3",
            "UNITED ST4",
            "CHINA    0",
            "CHINA    1",
            "CHINA    2",
            "CHINA    3",
            "CHINA    4",
        ][suppkey % 10]
    }

    fn customer_region(custkey: usize) -> &'static str {
        supplier_region(custkey)
    }

    fn customer_nation(custkey: usize) -> &'static str {
        supplier_nation(custkey)
    }

    fn customer_city(custkey: usize) -> &'static str {
        supplier_city(custkey)
    }

    fn is_leap_year(year: i32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    fn days_in_year(year: i32) -> usize {
        if is_leap_year(year) { 366 } else { 365 }
    }

    fn days_in_month(year: i32, month: usize) -> usize {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap_year(year) => 29,
            2 => 28,
            _ => unreachable!("invalid month"),
        }
    }

    fn generated_date(mut offset: usize) -> GeneratedDate {
        let mut year = 1992;
        while offset >= days_in_year(year) {
            offset -= days_in_year(year);
            year += 1;
        }

        let day_of_year = offset + 1;
        let mut month = 1;
        while offset >= days_in_month(year, month) {
            offset -= days_in_month(year, month);
            month += 1;
        }

        GeneratedDate {
            year,
            month,
            day_of_year,
        }
    }

    fn generated_yearmonthnum(offset: usize) -> i32 {
        let date = generated_date(offset);
        date.year * 100 + i32::try_from(date.month).expect("generated month fits i32")
    }

    fn generated_yearmonth(offset: usize) -> String {
        let date = generated_date(offset);
        let month = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ][date.month - 1];
        format!("{month}{}", date.year)
    }

    fn generated_weeknuminyear(offset: usize) -> usize {
        (generated_date(offset).day_of_year - 1) / 7 + 1
    }

    #[test]
    fn setup_keeps_guarded_part_filters_present_at_small_scales() {
        let (part_count, _, _) = ssbm_dimension_counts(1);
        assert!(
            (1..=part_count)
                .any(|partkey| matches!(part_mfgr(partkey).as_str(), "MFGR#1" | "MFGR#2")),
            "SSBM Q4.1/Q4.2 manufacturer filter MFGR#1/MFGR#2 must exist at the smallest scale"
        );
        assert!(
            (1..=part_count).any(|partkey| part_category(partkey) == "MFGR#12"),
            "SSBM Q2.1 category MFGR#12 must exist at the smallest scale"
        );
        assert!(
            (1..=part_count).any(|partkey| part_category(partkey) == "MFGR#14"),
            "SSBM Q4.3 category MFGR#14 must exist at the smallest scale"
        );
        assert!(
            (1..=part_count).any(|partkey| part_brand1(partkey) == "MFGR#2239"),
            "SSBM Q2.3 brand MFGR#2239 must exist at the smallest scale"
        );
        assert!(
            (1..=part_count).any(|partkey| {
                let brand = part_brand1(partkey);
                brand.as_str() >= "MFGR#2221" && brand.as_str() <= "MFGR#2228"
            }),
            "SSBM Q2.2 brand range MFGR#2221..MFGR#2228 must exist at the smallest scale"
        );
    }

    #[test]
    fn setup_keeps_guarded_geography_filters_present_at_small_scales() {
        let (_, supp_count, cust_count) = ssbm_dimension_counts(1);
        for region in ["AMERICA", "ASIA", "EUROPE"] {
            assert!(
                (1..=supp_count).any(|suppkey| supplier_region(suppkey) == region),
                "guarded supplier region {region} must exist at the smallest scale"
            );
        }
        assert!(
            (1..=supp_count).any(|suppkey| supplier_nation(suppkey) == "UNITED STATES"),
            "guarded supplier nation UNITED STATES must exist at the smallest scale"
        );
        assert!(
            (1..=supp_count)
                .any(|suppkey| matches!(supplier_city(suppkey), "UNITED ST0" | "UNITED ST1")),
            "guarded supplier cities UNITED ST0/UNITED ST1 must exist at the smallest scale"
        );

        for region in ["AMERICA", "ASIA"] {
            assert!(
                (1..=cust_count).any(|custkey| customer_region(custkey) == region),
                "guarded customer region {region} must exist at the smallest scale"
            );
        }
        assert!(
            (1..=cust_count).any(|custkey| customer_nation(custkey) == "UNITED STATES"),
            "guarded customer nation UNITED STATES must exist at the smallest scale"
        );
        assert!(
            (1..=cust_count)
                .any(|custkey| matches!(customer_city(custkey), "UNITED ST0" | "UNITED ST1")),
            "guarded customer cities UNITED ST0/UNITED ST1 must exist at the smallest scale"
        );
    }

    #[test]
    fn setup_keeps_guarded_date_filters_present() {
        let mut offsets = 0..=2555;
        for year in 1992..=1998 {
            assert!(
                offsets
                    .clone()
                    .any(|offset| generated_date(offset).year == year),
                "guarded date year {year} must exist"
            );
        }
        assert!(
            offsets
                .clone()
                .any(|offset| generated_yearmonthnum(offset) == 199_401),
            "SSBM Q1.2 date yearmonthnum 199401 must exist"
        );
        assert!(
            offsets.clone().any(|offset| {
                let date = generated_date(offset);
                date.year == 1994 && generated_weeknuminyear(offset) == 6
            }),
            "SSBM Q1.3 date year 1994/week 6 must exist"
        );
        assert!(
            offsets.any(|offset| generated_yearmonth(offset) == "Dec1997"),
            "SSBM Q3.4 date yearmonth Dec1997 must exist"
        );
    }

    #[test]
    fn setup_runs_dimension_sanity_checks_before_fact_generation() {
        let setup = ssbm_setup_sql(1);
        let sanity_pos = setup
            .iter()
            .position(|sql| sql.contains("SSBM setup sanity check failed"))
            .expect("SSBM setup should include dimension sanity guard");
        let fact_pos = setup
            .iter()
            .position(|sql| sql.contains("INSERT INTO ssbm_lineorder"))
            .expect("SSBM setup should include fact generation");

        assert!(sanity_pos < fact_pos);
        assert!(setup[sanity_pos].contains("MFGR#2239"));
        assert!(setup[sanity_pos].contains("MFGR#12"));
        assert!(setup[sanity_pos].contains("MFGR#14"));
        assert!(setup[sanity_pos].contains("Dec1997"));
    }
}

pub struct SsbmQ1_2;

impl Workload for SsbmQ1_2 {
    fn name(&self) -> &'static str {
        "ssbm_q1_2"
    }

    fn description(&self) -> &'static str {
        "SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_extendedprice * lo_discount) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         WHERE d_yearmonthnum = 199401 \
           AND lo_discount BETWEEN 4 AND 6 \
           AND lo_quantity BETWEEN 26 AND 35"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ1_3;

impl Workload for SsbmQ1_3 {
    fn name(&self) -> &'static str {
        "ssbm_q1_3"
    }

    fn description(&self) -> &'static str {
        "SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_extendedprice * lo_discount) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         WHERE d_weeknuminyear = 6 AND d_year = 1994 \
           AND lo_discount BETWEEN 5 AND 7 \
           AND lo_quantity BETWEEN 26 AND 35"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

// ---------------------------------------------------------------------------
// Q2 — Revenue by year/brand with part category and supplier region filters
// ---------------------------------------------------------------------------

pub struct SsbmQ2_1;

impl Workload for SsbmQ2_1 {
    fn name(&self) -> &'static str {
        "ssbm_q2_1"
    }

    fn description(&self) -> &'static str {
        "SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_revenue), d_year, p_brand1 \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE p_category = 'MFGR#12' \
           AND s_region = 'AMERICA' \
         GROUP BY d_year, p_brand1 \
         ORDER BY d_year, p_brand1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ2_2;

impl Workload for SsbmQ2_2 {
    fn name(&self) -> &'static str {
        "ssbm_q2_2"
    }

    fn description(&self) -> &'static str {
        "SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_revenue), d_year, p_brand1 \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE p_brand1 BETWEEN 'MFGR#2221' AND 'MFGR#2228' \
           AND s_region = 'ASIA' \
         GROUP BY d_year, p_brand1 \
         ORDER BY d_year, p_brand1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ2_3;

impl Workload for SsbmQ2_3 {
    fn name(&self) -> &'static str {
        "ssbm_q2_3"
    }

    fn description(&self) -> &'static str {
        "SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(lo_revenue), d_year, p_brand1 \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE p_brand1 = 'MFGR#2239' \
           AND s_region = 'EUROPE' \
         GROUP BY d_year, p_brand1 \
         ORDER BY d_year, p_brand1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

// ---------------------------------------------------------------------------
// Q3 — Revenue by customer/supplier geography and year
// ---------------------------------------------------------------------------

pub struct SsbmQ3_1;

impl Workload for SsbmQ3_1 {
    fn name(&self) -> &'static str {
        "ssbm_q3_1"
    }

    fn description(&self) -> &'static str {
        "SSBM Q3.1: revenue by customer/supplier nation and year, Asia region"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT c_nation, s_nation, d_year, SUM(lo_revenue) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE c_region = 'ASIA' AND s_region = 'ASIA' \
           AND d_year >= 1992 AND d_year <= 1997 \
         GROUP BY c_nation, s_nation, d_year \
         ORDER BY d_year, revenue DESC, c_nation, s_nation"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ3_2;

impl Workload for SsbmQ3_2 {
    fn name(&self) -> &'static str {
        "ssbm_q3_2"
    }

    fn description(&self) -> &'static str {
        "SSBM Q3.2: revenue by customer/supplier city and year, United States"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT c_city, s_city, d_year, SUM(lo_revenue) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE c_nation = 'UNITED STATES' AND s_nation = 'UNITED STATES' \
           AND d_year >= 1992 AND d_year <= 1997 \
         GROUP BY c_city, s_city, d_year \
         ORDER BY d_year, revenue DESC, c_city, s_city"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ3_3;

impl Workload for SsbmQ3_3 {
    fn name(&self) -> &'static str {
        "ssbm_q3_3"
    }

    fn description(&self) -> &'static str {
        "SSBM Q3.3: revenue by customer/supplier city and year, specific US cities"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT c_city, s_city, d_year, SUM(lo_revenue) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE (c_city = 'UNITED ST0' OR c_city = 'UNITED ST1') \
           AND (s_city = 'UNITED ST0' OR s_city = 'UNITED ST1') \
           AND d_year >= 1992 AND d_year <= 1997 \
         GROUP BY c_city, s_city, d_year \
         ORDER BY d_year, revenue DESC, c_city, s_city"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ3_4;

impl Workload for SsbmQ3_4 {
    fn name(&self) -> &'static str {
        "ssbm_q3_4"
    }

    fn description(&self) -> &'static str {
        "SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT c_city, s_city, d_year, SUM(lo_revenue) AS revenue \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         WHERE (c_city = 'UNITED ST0' OR c_city = 'UNITED ST1') \
           AND (s_city = 'UNITED ST0' OR s_city = 'UNITED ST1') \
           AND d_yearmonth = 'Dec1997' \
         GROUP BY c_city, s_city, d_year \
         ORDER BY d_year, revenue DESC, c_city, s_city"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

// ---------------------------------------------------------------------------
// Q4 — Profit by year/geography/part with multi-table filters
// ---------------------------------------------------------------------------

pub struct SsbmQ4_1;

impl Workload for SsbmQ4_1 {
    fn name(&self) -> &'static str {
        "ssbm_q4_1"
    }

    fn description(&self) -> &'static str {
        "SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT d_year, c_nation, SUM(lo_revenue - lo_supplycost) AS profit \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         WHERE c_region = 'AMERICA' AND s_region = 'AMERICA' \
           AND (p_mfgr = 'MFGR#1' OR p_mfgr = 'MFGR#2') \
         GROUP BY d_year, c_nation \
         ORDER BY d_year, c_nation"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ4_2;

impl Workload for SsbmQ4_2 {
    fn name(&self) -> &'static str {
        "ssbm_q4_2"
    }

    fn description(&self) -> &'static str {
        "SSBM Q4.2: profit by year/nation/category, America region, 1997-1998"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT d_year, s_nation, p_category, SUM(lo_revenue - lo_supplycost) AS profit \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         WHERE c_region = 'AMERICA' AND s_region = 'AMERICA' \
           AND (d_year = 1997 OR d_year = 1998) \
           AND (p_mfgr = 'MFGR#1' OR p_mfgr = 'MFGR#2') \
         GROUP BY d_year, s_nation, p_category \
         ORDER BY d_year, s_nation, p_category"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

pub struct SsbmQ4_3;

impl Workload for SsbmQ4_3 {
    fn name(&self) -> &'static str {
        "ssbm_q4_3"
    }

    fn description(&self) -> &'static str {
        "SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        ssbm_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT d_year, s_city, p_brand1, SUM(lo_revenue - lo_supplycost) AS profit \
         FROM ssbm_lineorder \
         JOIN ssbm_date ON lo_orderdate = d_datekey \
         JOIN ssbm_customer ON lo_custkey = c_custkey \
         JOIN ssbm_supplier ON lo_suppkey = s_suppkey \
         JOIN ssbm_part ON lo_partkey = p_partkey \
         WHERE c_region = 'AMERICA' AND s_nation = 'UNITED STATES' \
           AND (d_year = 1997 OR d_year = 1998) \
           AND p_category = 'MFGR#14' \
         GROUP BY d_year, s_city, p_brand1 \
         ORDER BY d_year, s_city, p_brand1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        ssbm_cleanup_sql()
    }
}

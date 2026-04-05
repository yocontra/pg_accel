use super::Workload;

/// Shared setup SQL for all SSBM workloads. Creates the star schema (date, part,
/// supplier, customer, lineorder) and populates with synthetic data scaled to `rows`.
#[allow(clippy::too_many_lines)]
pub fn ssbm_setup_sql(rows: usize) -> Vec<String> {
    let part_count = (rows / 30).clamp(200, 800_000);
    let supp_count = (rows / 3000).clamp(20, 200_000);
    let cust_count = (rows / 200).clamp(30, 3_000_000);

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
            'date_' || i AS d_date, \
            (ARRAY['Monday','Tuesday','Wednesday','Thursday','Friday','Saturday','Sunday'])[i % 7 + 1] AS d_dayofweek, \
            (ARRAY['January','February','March','April','May','June','July','August','September','October','November','December'])[(i / 30) % 12 + 1] AS d_month, \
            1992 + i / 365 AS d_year, \
            (1992 + i / 365) * 100 + (i / 30) % 12 + 1 AS d_yearmonthnum, \
            'Jan' || (1992 + i / 365) AS d_yearmonth, \
            i % 7 + 1 AS d_daynuminweek, \
            i % 30 + 1 AS d_daynuminmonth, \
            i % 365 + 1 AS d_daynuminyear, \
            (i / 30) % 12 + 1 AS d_monthnuminyear, \
            i / 7 % 52 + 1 AS d_weeknuminyear, \
            CASE WHEN (i / 30) % 12 + 1 IN (11, 12) THEN 'Christmas' WHEN (i / 30) % 12 + 1 IN (6, 7, 8) THEN 'Summer' ELSE 'Other' END AS d_sellingseason, \
            CASE WHEN i % 7 = 6 THEN 1 ELSE 0 END AS d_lastdayinweekfl, \
            CASE WHEN i % 30 = 29 THEN 1 ELSE 0 END AS d_lastdayinmonthfl, \
            CASE WHEN i % 365 IN (0, 185) THEN 1 ELSE 0 END AS d_holidayfl, \
            CASE WHEN i % 7 < 5 THEN 1 ELSE 0 END AS d_weekdayfl \
         FROM generate_series(0, 2555) AS i"
            .to_owned(),
        format!(
            "INSERT INTO ssbm_part \
             SELECT \
                i AS p_partkey, \
                'part_' || i AS p_name, \
                'MFGR#' || (i % 5 + 1) AS p_mfgr, \
                'MFGR#' || (i % 5 + 1) || (i % 5 + 1) AS p_category, \
                'MFGR#' || (i % 5 + 1) || (i % 5 + 1) || (i % 40 + 1) AS p_brand1, \
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

pub struct SsbmQ1_1;

impl Workload for SsbmQ1_1 {
    fn name(&self) -> &'static str {
        "ssbm_q1_1"
    }

    fn description(&self) -> &'static str {
        "SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity"
    }

    fn category(&self) -> &'static str {
        "ssbm"
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

pub struct SsbmQ1_2;

impl Workload for SsbmQ1_2 {
    fn name(&self) -> &'static str {
        "ssbm_q1_2"
    }

    fn description(&self) -> &'static str {
        "SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity"
    }

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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
         ORDER BY d_year, revenue DESC"
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

    fn category(&self) -> &'static str {
        "ssbm"
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
         ORDER BY d_year, revenue DESC"
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

    fn category(&self) -> &'static str {
        "ssbm"
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
         ORDER BY d_year, revenue DESC"
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

    fn category(&self) -> &'static str {
        "ssbm"
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
         ORDER BY d_year, revenue DESC"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

    fn category(&self) -> &'static str {
        "ssbm"
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

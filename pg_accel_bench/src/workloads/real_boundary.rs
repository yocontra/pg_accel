use super::Workload;

/// Parametric workload using real geographic boundary data from
/// `real_boundaries` (loaded via `scripts/load_boundaries.py`).
pub struct RealBoundaryWorkload {
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub setup_stmts: &'static [&'static str],
    pub query: &'static str,
    pub cleanup_stmts: &'static [&'static str],
}

impl Workload for RealBoundaryWorkload {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        self.category
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let preflight = "DO $$ BEGIN \
            IF NOT EXISTS (SELECT 1 FROM information_schema.tables \
            WHERE table_name = 'real_boundaries') THEN \
            RAISE EXCEPTION 'real_boundaries table not found. \
            Run: python3 scripts/load_boundaries.py'; \
            END IF; END $$"
            .to_owned();
        let mut stmts = vec![preflight];
        stmts.extend(
            self.setup_stmts
                .iter()
                .map(|s| s.replace("{rows}", &rows.to_string())),
        );
        stmts
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        self.cleanup_stmts.iter().map(|s| (*s).to_owned()).collect()
    }
}

// ==========================================================================
// Spatial workloads — GPU wins big on complex polygon predicates
// ==========================================================================

/// Point-in-polygon against the 50 most complex real boundaries (500-15K vertices).
pub const REAL_PIP_COMPLEX: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_pip_complex",
    description: "ST_Intersects 1M pts vs 50 complex real boundaries (500-15K vertices)",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rpip_pts",
        "DROP TABLE IF EXISTS bench_rpip_polys",
        "CREATE TABLE bench_rpip_pts (id serial PRIMARY KEY, \
         geom geometry(Point, 4326) NOT NULL)",
        "INSERT INTO bench_rpip_pts (geom) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326) \
         FROM generate_series(1, {rows})",
        "CREATE TABLE bench_rpip_polys AS \
         SELECT id, name, state, geom, vertex_count \
         FROM real_boundaries \
         WHERE vertex_count >= 500 \
         ORDER BY vertex_count DESC LIMIT 50",
        "CREATE INDEX ON bench_rpip_polys USING gist (geom)",
        "ANALYZE bench_rpip_pts",
        "ANALYZE bench_rpip_polys",
    ],
    query: "SELECT count(*) FROM bench_rpip_pts p, bench_rpip_polys g \
            WHERE ST_Intersects(g.geom, p.geom)",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rpip_pts",
        "DROP TABLE IF EXISTS bench_rpip_polys",
    ],
};

/// Multi-part polygon containment (fragmented cities like Houston=28 parts).
pub const REAL_MULTIPOLY_CONTAIN: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_multipoly_contain",
    description: "ST_Contains multi-part boundaries (5+ parts) vs 1M points + GROUP BY",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rmpc_pts",
        "DROP TABLE IF EXISTS bench_rmpc_bounds",
        "CREATE TABLE bench_rmpc_pts AS \
         SELECT id, ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326) AS geom \
         FROM generate_series(1, {rows}) AS id",
        "CREATE TABLE bench_rmpc_bounds AS \
         SELECT id, name, geom, num_parts, vertex_count \
         FROM real_boundaries \
         WHERE num_parts >= 3 \
         ORDER BY num_parts DESC LIMIT 30",
        "CREATE INDEX ON bench_rmpc_bounds USING gist (geom)",
        "ANALYZE bench_rmpc_pts",
        "ANALYZE bench_rmpc_bounds",
    ],
    query: "SELECT g.name, count(*) FROM bench_rmpc_pts p, bench_rmpc_bounds g \
            WHERE ST_Contains(g.geom, p.geom) \
            GROUP BY g.name",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rmpc_pts",
        "DROP TABLE IF EXISTS bench_rmpc_bounds",
    ],
};

/// Spatial join: points against dense urban neighborhood boundaries.
pub const REAL_SPATIAL_JOIN_DENSE: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_spatial_join_dense",
    description: "Spatial join 1M pts vs real neighborhood boundaries + GROUP BY",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsjd_pts",
        "DROP TABLE IF EXISTS bench_rsjd_bounds",
        "CREATE TABLE bench_rsjd_bounds AS \
         SELECT id, name, geom, vertex_count \
         FROM real_boundaries \
         WHERE boundary_type = 'neighborhood' \
         AND vertex_count >= 30 LIMIT 500",
        "CREATE TABLE bench_rsjd_pts (id serial PRIMARY KEY, \
         geom geometry(Point, 4326) NOT NULL)",
        "INSERT INTO bench_rsjd_pts (geom) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326) \
         FROM generate_series(1, {rows})",
        "CREATE INDEX ON bench_rsjd_bounds USING gist (geom)",
        "CREATE INDEX ON bench_rsjd_pts USING gist (geom)",
        "ANALYZE bench_rsjd_bounds",
        "ANALYZE bench_rsjd_pts",
    ],
    query: "SELECT g.name, count(*) FROM bench_rsjd_pts p \
            JOIN bench_rsjd_bounds g ON ST_Intersects(g.geom, p.geom) \
            GROUP BY g.name",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsjd_pts",
        "DROP TABLE IF EXISTS bench_rsjd_bounds",
    ],
};

/// Bidirectional containment: find neighborhoods inside cities.
pub const REAL_BIDIRECTIONAL_CONTAIN: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_bidirectional_contain",
    description: "ST_Contains OR ST_Within: cities vs neighborhoods polygon-polygon",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rbidi_cities",
        "DROP TABLE IF EXISTS bench_rbidi_hoods",
        "CREATE TABLE bench_rbidi_cities AS \
         SELECT id, name, state, geom FROM real_boundaries \
         WHERE boundary_type = 'census' AND vertex_count >= 100 LIMIT 200",
        "CREATE TABLE bench_rbidi_hoods AS \
         SELECT id, name, state, geom FROM real_boundaries \
         WHERE boundary_type = 'neighborhood' LIMIT 2000",
        "CREATE INDEX ON bench_rbidi_cities USING gist (geom)",
        "CREATE INDEX ON bench_rbidi_hoods USING gist (geom)",
        "ANALYZE bench_rbidi_cities",
        "ANALYZE bench_rbidi_hoods",
    ],
    query: "SELECT c.name AS city, h.name AS neighborhood \
            FROM bench_rbidi_cities c, bench_rbidi_hoods h \
            WHERE ST_Contains(c.geom, h.geom)",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rbidi_cities",
        "DROP TABLE IF EXISTS bench_rbidi_hoods",
    ],
};

// ==========================================================================
// OLAP / Analytics workloads
// ==========================================================================

/// Window functions: RANK + running SUM partitioned by real region names.
pub const REAL_WINDOW_REGION_RANK: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_window_region_rank",
    description: "ROW_NUMBER + running SUM partitioned by 500 real city names",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rwrr_regions",
        "DROP TABLE IF EXISTS bench_rwrr",
        "CREATE TABLE bench_rwrr_regions AS \
         SELECT name AS region, row_number() OVER () - 1 AS rn \
         FROM real_boundaries \
         WHERE boundary_type = 'census' AND population > 0 LIMIT 500",
        "CREATE TABLE bench_rwrr AS \
         SELECT g.id, r.region, \
           random() * 10000 AS revenue, \
           '2024-01-01'::date + (random() * 365)::int AS sale_date \
         FROM generate_series(1, {rows}) AS g(id) \
         JOIN bench_rwrr_regions r ON r.rn = g.id % (SELECT count(*) FROM bench_rwrr_regions)",
        "ANALYZE bench_rwrr",
    ],
    query: "SELECT region, sale_date, revenue, \
            ROW_NUMBER() OVER (PARTITION BY region ORDER BY sale_date), \
            SUM(revenue) OVER (PARTITION BY region ORDER BY sale_date) \
            FROM bench_rwrr",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rwrr",
        "DROP TABLE IF EXISTS bench_rwrr_regions",
    ],
};

/// Complex grouped aggregation by real region: multi-column stats.
pub const REAL_GROUPED_REGION_STATS: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_grouped_region_stats",
    description: "6-agg GROUP BY 500 real regions + ORDER BY — hash agg + sort",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rgrs_regions",
        "DROP TABLE IF EXISTS bench_rgrs",
        "CREATE TABLE bench_rgrs_regions AS \
         SELECT name AS region, state, row_number() OVER () - 1 AS rn \
         FROM real_boundaries \
         WHERE boundary_type = 'census' AND population IS NOT NULL LIMIT 500",
        "CREATE TABLE bench_rgrs AS \
         SELECT g.id, r.region, r.state, \
           random() * 100000 AS property_value, \
           random() * 5000 AS tax_amount, \
           (random() * 4 + 1)::int AS bedrooms, \
           1950 + (random() * 74)::int AS year_built, \
           random() < 0.15 AS is_commercial \
         FROM generate_series(1, {rows}) AS g(id) \
         JOIN bench_rgrs_regions r ON r.rn = g.id % (SELECT count(*) FROM bench_rgrs_regions)",
        "ANALYZE bench_rgrs",
    ],
    query: "SELECT state, region, \
            count(*) AS n, \
            avg(property_value) AS avg_value, \
            sum(tax_amount) AS total_tax, \
            avg(CASE WHEN is_commercial THEN property_value END) AS avg_commercial, \
            stddev(property_value) AS value_stddev, \
            max(year_built) - min(year_built) AS year_range \
            FROM bench_rgrs \
            GROUP BY state, region \
            ORDER BY total_tax DESC",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rgrs",
        "DROP TABLE IF EXISTS bench_rgrs_regions",
    ],
};

/// Hash join: fact table (events) against boundary dimension table.
pub const REAL_HASHJOIN_BOUNDARY: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_hashjoin_boundary",
    description: "Hash join 1M facts vs 5K boundary dims + filter + agg + top-K",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rhjb_dims",
        "DROP TABLE IF EXISTS bench_rhjb_facts",
        "CREATE TABLE bench_rhjb_dims AS \
         SELECT id AS boundary_id, name, state, boundary_type, \
           vertex_count, num_parts, \
           COALESCE(population, 0) AS population \
         FROM real_boundaries LIMIT 5000",
        "CREATE TABLE bench_rhjb_facts ( \
           id serial PRIMARY KEY, \
           boundary_id int NOT NULL, \
           event_type int NOT NULL, \
           amount float8 NOT NULL, \
           ts timestamp NOT NULL)",
        "INSERT INTO bench_rhjb_facts (boundary_id, event_type, amount, ts) \
         SELECT (random() * 4999)::int + 1, \
           (random() * 9)::int + 1, \
           random() * 10000, \
           '2024-01-01'::timestamp + (random() * 365 * 86400)::int * interval '1 second' \
         FROM generate_series(1, {rows})",
        "ANALYZE bench_rhjb_dims",
        "ANALYZE bench_rhjb_facts",
    ],
    query: "SELECT d.state, d.boundary_type, d.name, \
            sum(f.amount) AS total_amount, \
            count(*) AS event_count, \
            avg(f.amount) AS avg_amount \
            FROM bench_rhjb_facts f \
            INNER JOIN bench_rhjb_dims d ON f.boundary_id = d.boundary_id \
            WHERE f.event_type <= 5 \
            GROUP BY d.state, d.boundary_type, d.name \
            ORDER BY total_amount DESC LIMIT 100",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rhjb_facts",
        "DROP TABLE IF EXISTS bench_rhjb_dims",
    ],
};

/// Top-K with complex computed sort keys over boundary metadata.
pub const REAL_TOPK_BOUNDARY: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_topk_boundary",
    description: "Top-K sort with ln/div/NULLIF expressions on 39K real boundaries",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rtopk",
        // Cross-join to reach target row count for scalable benchmarking
        "CREATE TABLE bench_rtopk AS \
         SELECT b.id * 1000 + g.mult AS id, b.name, b.state, b.boundary_type, \
           COALESCE(b.population, 0) AS population, \
           COALESCE(b.aland, 1) AS aland, \
           COALESCE(b.awater, 0) AS awater, \
           b.vertex_count, b.num_parts, \
           COALESCE(b.population, 0) * (0.8 + random() * 0.4) AS est_pop, \
           COALESCE(b.aland, 1) * (0.9 + random() * 0.2) AS est_area \
         FROM real_boundaries b \
         CROSS JOIN generate_series(1, GREATEST(1, {rows} / \
           (SELECT count(*) FROM real_boundaries)::int)) AS g(mult)",
        "ANALYZE bench_rtopk",
    ],
    query: "SELECT name, state, boundary_type, population, \
            est_pop / NULLIF(est_area, 0) AS density, \
            vertex_count * num_parts AS geom_complexity, \
            ln(GREATEST(est_pop, 1)) * est_area / NULLIF(vertex_count, 0) AS score \
            FROM bench_rtopk \
            WHERE est_pop > 100 AND est_area > 0 \
            ORDER BY ln(GREATEST(est_pop, 1)) * est_area \
              / NULLIF(vertex_count * vertex_count, 0) DESC \
            LIMIT 1000",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_rtopk"],
};

// ==========================================================================
// Mixed workloads — multi-strategy GPU pipelines
// ==========================================================================

/// Spatial filter + aggregation: join points to real boundaries, aggregate by region.
pub const REAL_SPATIAL_FILTER_AGG: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_spatial_filter_agg",
    description: "Spatial join to real boundaries + 3-key GROUP BY + top-K sort",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsfa_pts",
        "DROP TABLE IF EXISTS bench_rsfa_bounds",
        "CREATE TABLE bench_rsfa_bounds AS \
         SELECT id, name, state, geom FROM real_boundaries \
         WHERE boundary_type = 'census' \
         AND state IN ('NY', 'CA', 'TX', 'FL', 'IL') \
         AND vertex_count >= 50",
        "CREATE TABLE bench_rsfa_pts ( \
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           category int NOT NULL, \
           amount float8 NOT NULL)",
        "INSERT INTO bench_rsfa_pts (geom, category, amount) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326), \
           (random() * 19)::int + 1, \
           random() * 5000 \
         FROM generate_series(1, {rows})",
        "CREATE INDEX ON bench_rsfa_bounds USING gist (geom)",
        "ANALYZE bench_rsfa_bounds",
        "ANALYZE bench_rsfa_pts",
    ],
    query: "SELECT g.state, g.name, p.category, \
            count(*) AS n, \
            sum(p.amount) AS total, \
            avg(p.amount) AS avg_amount \
            FROM bench_rsfa_pts p \
            JOIN bench_rsfa_bounds g ON ST_Intersects(g.geom, p.geom) \
            GROUP BY g.state, g.name, p.category \
            ORDER BY total DESC LIMIT 200",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsfa_pts",
        "DROP TABLE IF EXISTS bench_rsfa_bounds",
    ],
};

/// Spatial join + window function pipeline.
pub const REAL_SPATIAL_JOIN_WINDOW: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_spatial_join_window",
    description: "Spatial join → ROW_NUMBER + running SUM + LAG window pipeline",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsjw_events",
        "DROP TABLE IF EXISTS bench_rsjw_bounds",
        "CREATE TABLE bench_rsjw_bounds AS \
         SELECT id AS region_id, name, geom FROM real_boundaries \
         WHERE boundary_type = 'neighborhood' \
         AND vertex_count >= 30 LIMIT 200",
        "CREATE TABLE bench_rsjw_events ( \
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           ts timestamp NOT NULL, \
           value float8 NOT NULL)",
        "INSERT INTO bench_rsjw_events (geom, ts, value) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326), \
           '2024-01-01'::timestamp + (random() * 365 * 86400)::int * interval '1 second', \
           random() * 1000 \
         FROM generate_series(1, {rows})",
        "CREATE INDEX ON bench_rsjw_bounds USING gist (geom)",
        "ANALYZE bench_rsjw_bounds",
        "ANALYZE bench_rsjw_events",
    ],
    query: "SELECT * FROM ( \
              SELECT g.name AS region, e.ts, e.value, \
                ROW_NUMBER() OVER (PARTITION BY g.name ORDER BY e.ts) AS event_seq, \
                SUM(e.value) OVER (PARTITION BY g.name ORDER BY e.ts) AS running_total, \
                LAG(e.value) OVER (PARTITION BY g.name ORDER BY e.ts) AS prev_value \
              FROM bench_rsjw_events e \
              JOIN bench_rsjw_bounds g ON ST_Intersects(g.geom, e.geom) \
            ) sub \
            WHERE event_seq <= 100",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rsjw_events",
        "DROP TABLE IF EXISTS bench_rsjw_bounds",
    ],
};

/// Complex boolean expression + spatial predicate + aggregation.
pub const REAL_EXPR_SPATIAL: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_expr_spatial",
    description: "Complex WHERE + spatial join to real boundaries + conditional agg",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_resp",
        "DROP TABLE IF EXISTS bench_resp_bounds",
        "CREATE TABLE bench_resp ( \
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           price float8 NOT NULL, \
           cost float8 NOT NULL, \
           quality_score int NOT NULL, \
           is_premium bool NOT NULL, \
           is_flagged bool NOT NULL)",
        "INSERT INTO bench_resp (geom, price, cost, quality_score, is_premium, is_flagged) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -74.3 + random() * 0.8, 40.4 + random() * 0.8), 4326), \
           random() * 1000, \
           random() * 500, \
           (random() * 10)::int, \
           random() < 0.3, \
           random() < 0.1 \
         FROM generate_series(1, {rows})",
        "CREATE TABLE bench_resp_bounds AS \
         SELECT id, name, geom FROM real_boundaries \
         WHERE boundary_type = 'neighborhood' \
         AND state = 'NY' AND vertex_count >= 50 LIMIT 50",
        "CREATE INDEX ON bench_resp_bounds USING gist (geom)",
        "ANALYZE bench_resp",
        "ANALYZE bench_resp_bounds",
    ],
    query: "SELECT g.name, count(*), \
            avg(p.price - p.cost) AS avg_margin, \
            sum(CASE WHEN p.is_premium THEN p.price ELSE 0 END) AS premium_revenue \
            FROM bench_resp p \
            JOIN bench_resp_bounds g ON ST_Intersects(g.geom, p.geom) \
            WHERE (p.price > p.cost * 1.2 OR (p.is_premium AND p.quality_score >= 7)) \
              AND NOT p.is_flagged \
            GROUP BY g.name \
            ORDER BY avg_margin DESC",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_resp",
        "DROP TABLE IF EXISTS bench_resp_bounds",
    ],
};

/// Boundary analytics with multiple window functions over real data.
pub const REAL_BOUNDARY_ANALYTICS: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_boundary_analytics",
    description: "RANK + cumulative SUM + pop_share + DENSE_RANK over real boundaries",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rba",
        // Expand to target row count
        "CREATE TABLE bench_rba AS \
         SELECT b.id * 1000 + g.mult AS id, b.name, b.state, \
           COALESCE(b.population, 0) * (0.8 + random() * 0.4) AS est_pop, \
           COALESCE(b.aland, 1) * (0.9 + random() * 0.2) AS est_area, \
           random() * 1000000 AS tax_revenue, \
           b.vertex_count \
         FROM real_boundaries b \
         CROSS JOIN generate_series(1, GREATEST(1, {rows} / \
           (SELECT count(*) FROM real_boundaries)::int)) AS g(mult) \
         WHERE b.boundary_type = 'census'",
        "ANALYZE bench_rba",
    ],
    query: "SELECT state, name, \
            RANK() OVER (PARTITION BY state ORDER BY est_pop DESC) AS pop_rank, \
            SUM(tax_revenue) OVER (PARTITION BY state ORDER BY est_pop DESC) AS cum_revenue, \
            est_pop / NULLIF(SUM(est_pop) OVER (PARTITION BY state), 0) AS pop_share, \
            DENSE_RANK() OVER (ORDER BY vertex_count DESC) AS complexity_rank \
            FROM bench_rba \
            WHERE est_pop > 100",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_rba"],
};

/// Spatial KNN aggregation: nearest points to boundary centroids.
pub const REAL_SPATIAL_KNN_AGG: RealBoundaryWorkload = RealBoundaryWorkload {
    name: "real_spatial_knn_agg",
    description: "LATERAL KNN (500 nearest) per boundary centroid + weighted avg",
    category: "real_boundary",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_rska_centers",
        "DROP TABLE IF EXISTS bench_rska_pts",
        "CREATE TABLE bench_rska_centers AS \
         SELECT id, name, state, ST_Centroid(geom) AS centroid \
         FROM real_boundaries \
         WHERE boundary_type = 'census' \
         AND state IN ('CA', 'NY', 'TX', 'FL', 'PA') \
         LIMIT 100",
        "CREATE TABLE bench_rska_pts ( \
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           value float8 NOT NULL, \
           weight float8 NOT NULL)",
        "INSERT INTO bench_rska_pts (geom, value, weight) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -125 + random() * 58, 24 + random() * 26), 4326), \
           random() * 1000, \
           0.1 + random() * 0.9 \
         FROM generate_series(1, {rows})",
        "CREATE INDEX ON bench_rska_pts USING gist (geom)",
        "ANALYZE bench_rska_centers",
        "ANALYZE bench_rska_pts",
    ],
    query: "SELECT c.name, c.state, \
            count(*) AS n_nearby, \
            sum(p.value * p.weight) / sum(p.weight) AS weighted_avg, \
            avg(ST_Distance(c.centroid::geography, p.geom::geography)) AS avg_dist_m \
            FROM bench_rska_centers c \
            JOIN LATERAL ( \
              SELECT value, weight, geom \
              FROM bench_rska_pts p \
              ORDER BY p.geom <-> c.centroid \
              LIMIT 500 \
            ) p ON true \
            GROUP BY c.name, c.state \
            ORDER BY weighted_avg DESC",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_rska_pts",
        "DROP TABLE IF EXISTS bench_rska_centers",
    ],
};

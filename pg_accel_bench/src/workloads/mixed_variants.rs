use super::Workload;

/// Parametric mixed-pipeline benchmark: combines multiple GPU strategies.
pub struct MixedVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub setup_stmts: &'static [&'static str],
    pub query: &'static str,
    pub cleanup_stmts: &'static [&'static str],
    pub cat: &'static str,
}

impl Workload for MixedVariant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        self.cat
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        self.setup_stmts
            .iter()
            .map(|s| s.replace("{rows}", &rows.to_string()))
            .collect()
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        self.cleanup_stmts.iter().map(|s| (*s).to_owned()).collect()
    }
}

/// Spatial megapoly (500v) + aggregation pipeline
pub const MIXED_MEGAPOLY_AGG: MixedVariant = MixedVariant {
    name: "mixed_megapoly_agg",
    description: "ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_mixed_mega",
        "CREATE TABLE bench_mixed_mega (\
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           val float8 NOT NULL\
         )",
        "INSERT INTO bench_mixed_mega (geom, val) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -74.3 + random() * 0.8, \
           40.4 + random() * 0.8\
         ), 4326), random() * 1000 \
         FROM generate_series(1, {rows})",
        "ANALYZE bench_mixed_mega",
    ],
    query: "SELECT count(*), sum(val) FROM bench_mixed_mega \
            WHERE ST_Intersects(geom, \
              ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.15, 125))",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_mixed_mega"],
    cat: "mixed",
};

/// Expr filter + grouped aggregation
pub const MIXED_EXPR_AGG: MixedVariant = MixedVariant {
    name: "mixed_expr_agg",
    description: "WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_mixed_expr",
        "CREATE TABLE bench_mixed_expr (\
           id serial PRIMARY KEY, \
           cat int4 NOT NULL, \
           v1 float4 NOT NULL, \
           v2 float4 NOT NULL, \
           v3 float8 NOT NULL\
         )",
        "INSERT INTO bench_mixed_expr (cat, v1, v2, v3) \
         SELECT (random() * 49)::int4 + 1, \
                (random() * 1000)::float4, \
                (random() * 1000)::float4, \
                random() * 500 \
         FROM generate_series(1, {rows})",
        "ANALYZE bench_mixed_expr",
    ],
    query: "SELECT cat, SUM(v1), COUNT(*) FROM bench_mixed_expr \
            WHERE v1 * v2 + v3 > 500.0 GROUP BY cat",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_mixed_expr"],
    cat: "mixed",
};

/// Join + aggregation pipeline
pub const MIXED_JOIN_AGG: MixedVariant = MixedVariant {
    name: "mixed_join_agg",
    description: "INNER JOIN → GROUP BY → SUM — join + agg pipeline",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_mixed_facts",
        "DROP TABLE IF EXISTS bench_mixed_dims",
        "CREATE TABLE bench_mixed_dims (\
           id serial PRIMARY KEY, \
           label int4 NOT NULL\
         )",
        "CREATE TABLE bench_mixed_facts (\
           id serial PRIMARY KEY, \
           dim_id int4 NOT NULL, \
           amount float8 NOT NULL\
         )",
        "INSERT INTO bench_mixed_dims (label) \
         SELECT (random() * 10)::int4 FROM generate_series(1, 1000)",
        "INSERT INTO bench_mixed_facts (dim_id, amount) \
         SELECT (random() * 999)::int4 + 1, random() * 1000 \
         FROM generate_series(1, {rows})",
        "ANALYZE bench_mixed_dims",
        "ANALYZE bench_mixed_facts",
    ],
    query: "SELECT d.label, SUM(f.amount), COUNT(*) \
            FROM bench_mixed_facts f \
            INNER JOIN bench_mixed_dims d ON f.dim_id = d.id \
            GROUP BY d.label",
    cleanup_stmts: &[
        "DROP TABLE IF EXISTS bench_mixed_facts",
        "DROP TABLE IF EXISTS bench_mixed_dims",
    ],
    cat: "mixed",
};

/// Spatial megapoly + sort pipeline
pub const MIXED_SPATIAL_SORT: MixedVariant = MixedVariant {
    name: "mixed_spatial_sort",
    description: "ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline",
    setup_stmts: &[
        "DROP TABLE IF EXISTS bench_mixed_spsort",
        "CREATE TABLE bench_mixed_spsort (\
           id serial PRIMARY KEY, \
           geom geometry(Point, 4326) NOT NULL, \
           val float8 NOT NULL\
         )",
        "INSERT INTO bench_mixed_spsort (geom, val) \
         SELECT ST_SetSRID(ST_MakePoint(\
           -74.3 + random() * 0.8, \
           40.4 + random() * 0.8\
         ), 4326), random() * 1000 \
         FROM generate_series(1, {rows})",
        "ANALYZE bench_mixed_spsort",
    ],
    query: "SELECT val FROM bench_mixed_spsort \
            WHERE ST_Intersects(geom, \
              ST_Buffer(ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.15, 125)) \
            ORDER BY val LIMIT 1000",
    cleanup_stmts: &["DROP TABLE IF EXISTS bench_mixed_spsort"],
    cat: "mixed",
};

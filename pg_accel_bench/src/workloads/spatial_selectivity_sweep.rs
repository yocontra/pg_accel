#![allow(dead_code)]

use super::Workload;

const SPATIAL_SELECTIVITY_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];

/// Parametric spatial benchmark: varying selectivity with fixed polygon complexity.
///
/// Uses a 500-vertex polygon but adjusts point distribution so that different
/// percentages of rows pass the spatial filter. Tests GPU efficiency at
/// different output ratios.
pub struct SpatialSelectivitySweep {
    pub name: &'static str,
    pub description: &'static str,
    /// Fraction of points concentrated inside the polygon area (0.0-1.0).
    /// Higher values = more points inside = higher selectivity.
    pub inside_fraction: f64,
}

impl Workload for SpatialSelectivitySweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        // Points near the polygon center: (-73.985, 40.748) ± 0.15
        // Points far from the polygon: wider NYC metro area
        let inside_count = (rows as f64 * self.inside_fraction) as usize;
        let outside_count = rows - inside_count;

        vec![
            "DROP TABLE IF EXISTS bench_selsweep_pts".to_owned(),
            "CREATE TABLE bench_selsweep_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            // Points clustered near the polygon center (will mostly pass)
            format!(
                "INSERT INTO bench_selsweep_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -73.985 + (random() - 0.5) * 0.20, \
                   40.748 + (random() - 0.5) * 0.20\
                 ), 4326) \
                 FROM generate_series(1, {inside_count})"
            ),
            // Points spread across wide area (will mostly fail)
            format!(
                "INSERT INTO bench_selsweep_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.5 + random() * 1.5, \
                   40.2 + random() * 1.2\
                 ), 4326) \
                 FROM generate_series(1, {outside_count})"
            ),
            "ANALYZE bench_selsweep_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // 500-vertex polygon (125 segments × 4)
        "SELECT count(*) FROM bench_selsweep_pts \
         WHERE ST_Intersects(geom, \
           ST_Buffer(\
             ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
             0.15, \
             125\
           )\
         )"
        .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_SELECTIVITY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_selsweep_pts".to_owned()]
    }
}

/// Generated single-ring polygon used by the 100K spatial crash repro matrix.
#[derive(Clone, Copy)]
pub enum ReproPolygon {
    /// ST_Buffer with ~500 vertices: below the kernel's cooperative threshold.
    Simple500v,
    /// ST_Buffer with 1024+ vertices: exercises the cooperative kernel path.
    Coop1024v,
}

impl ReproPolygon {
    const fn label(self) -> &'static str {
        match self {
            Self::Simple500v => "generated ST_Buffer simple path ~500 vertices",
            Self::Coop1024v => "generated ST_Buffer cooperative path 1024+ vertices",
        }
    }

    const fn sql(self) -> &'static str {
        match self {
            Self::Simple500v => {
                "ST_Buffer(\
                   ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
                   0.15, \
                   125\
                 )"
            }
            Self::Coop1024v => {
                "ST_Buffer(\
                   ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
                   0.15, \
                   256\
                 )"
            }
        }
    }
}

/// Per-workload JIT state for the repro matrix.
#[derive(Clone, Copy)]
pub enum ReproJit {
    Off,
    On,
}

impl ReproJit {
    const fn label(self) -> &'static str {
        match self {
            Self::Off => "jit=off",
            Self::On => "jit=on jit_above_cost=0",
        }
    }

    fn sql(self) -> Vec<String> {
        match self {
            Self::Off => vec!["SET jit = off".to_owned()],
            Self::On => vec![
                "SET jit = on".to_owned(),
                "SET jit_above_cost = 0".to_owned(),
                "SET jit_inline_above_cost = 0".to_owned(),
                "SET jit_optimize_above_cost = 0".to_owned(),
            ],
        }
    }
}

/// Focused spatial predicate regression matrix for the legacy 100K crash repro.
///
/// The harness supplies the row axis (10K, 100K, 1M, 10M), seed, timing
/// mode, cache mode, plan snippets, and crash artifacts. These variants add
/// the per-query dimensions that the existing report can carry in the
/// workload name/description and in captured GUC/plan text:
///
/// - generated polygon path: simple ~500v vs cooperative 1024+v
/// - target selectivity: deterministic inside/outside point split
/// - batch count proxy: `pg_accel.min_batch_size`
/// - worker shape: table `parallel_workers` reloption
/// - PostgreSQL JIT state
///
/// The 64K batch variants intentionally decline pg_accel at 10K rows through
/// normal planning. That should produce a native PostgreSQL plan, not a
/// pg_accel accelerator plan.
pub struct SpatialSelectivityRepro {
    pub name: &'static str,
    pub description: &'static str,
    pub polygon: ReproPolygon,
    /// Whole-number percent of rows generated inside the polygon.
    pub selectivity_pct: usize,
    /// Value for `pg_accel.min_batch_size`.
    pub min_batch_size: usize,
    /// Table reloption. `0` keeps the native side serial; values >0 request
    /// that many parallel scan workers, subject to global PG worker caps.
    pub rel_parallel_workers: usize,
    pub jit: ReproJit,
}

impl Workload for SpatialSelectivityRepro {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_spatial_repro"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let inside_count = rows.saturating_mul(self.selectivity_pct) / 100;
        let outside_count = rows.saturating_sub(inside_count);

        vec![
            "DROP TABLE IF EXISTS bench_spatial_sel_repro".to_owned(),
            "CREATE TABLE bench_spatial_sel_repro (\
               id bigint PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "ALTER TABLE bench_spatial_sel_repro \
                 SET (parallel_workers = {})",
                self.rel_parallel_workers
            ),
            format!(
                "COMMENT ON TABLE bench_spatial_sel_repro IS \
                 'spatial_sel_repro: {}; target_selectivity={}pct; \
                  min_batch_size={}; rel_parallel_workers={}; {}'",
                self.polygon.label(),
                self.selectivity_pct,
                self.min_batch_size,
                self.rel_parallel_workers,
                self.jit.label()
            ),
            // Deterministic points inside the generated ST_Buffer radius.
            // The grid is large enough to avoid full repetition up to 10M.
            format!(
                "INSERT INTO bench_spatial_sel_repro (id, geom) \
                 SELECT g::bigint, \
                        ST_SetSRID(ST_MakePoint(\
                          -73.985 + ((((g - 1) % 3163)::float8 / 3162.0) - 0.5) * 0.08, \
                          40.748 + (((((g - 1) / 3163) % 3163)::float8 / 3162.0) - 0.5) * 0.08\
                        ), 4326) \
                 FROM generate_series(1, {inside_count}) AS g"
            ),
            // Deterministic points far outside the polygon bbox.
            format!(
                "INSERT INTO bench_spatial_sel_repro (id, geom) \
                 SELECT ({inside_count} + g)::bigint, \
                        ST_SetSRID(ST_MakePoint(\
                          -74.50 + (((g - 1) % 3163)::float8 / 3162.0) * 0.20, \
                          40.20 + ((((g - 1) / 3163) % 3163)::float8 / 3162.0) * 0.20\
                        ), 4326) \
                 FROM generate_series(1, {outside_count}) AS g"
            ),
            "ANALYZE bench_spatial_sel_repro".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        let mut sql = vec![
            format!("SET pg_accel.min_batch_size = {}", self.min_batch_size),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ];
        sql.extend(self.jit.sql());
        sql
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT count(*) \
             FROM bench_spatial_sel_repro \
             WHERE ST_Intersects(geom, {})",
            self.polygon.sql()
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_sel_repro".to_owned()]
    }
}

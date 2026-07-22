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
/// - batch count under the documented `pg_accel.min_batch_size` default
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
                  min_batch_size=DEFAULT; rel_parallel_workers={}; {}'",
                self.polygon.label(),
                self.selectivity_pct,
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
            "SET pg_accel.min_batch_size = DEFAULT".to_owned(),
            "SET max_parallel_workers_per_gather = DEFAULT".to_owned(),
            "RESET min_parallel_table_scan_size".to_owned(),
            "RESET parallel_setup_cost".to_owned(),
            "RESET parallel_tuple_cost".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectivity_sweep_builds_exact_inside_and_outside_populations() {
        let workload = SpatialSelectivitySweep {
            name: "spatial_selectivity_25",
            description: "25 percent selectivity",
            inside_fraction: 0.25,
        };

        assert_eq!(workload.name(), "spatial_selectivity_25");
        assert_eq!(workload.description(), "25 percent selectivity");
        assert_eq!(workload.row_scales(), &[10_000, 100_000, 1_000_000]);
        let setup = workload.setup_sql(80);
        assert_eq!(setup.len(), 5);
        assert!(setup[2].contains("generate_series(1, 20)"));
        assert!(setup[3].contains("generate_series(1, 60)"));
        assert!(setup[4].contains("ANALYZE bench_selsweep_pts"));
        assert!(workload.query_sql().contains("ST_Buffer"));
        assert_eq!(
            workload.cleanup_sql(),
            ["DROP TABLE IF EXISTS bench_selsweep_pts"]
        );
    }

    #[test]
    fn repro_variants_encode_polygon_worker_selectivity_and_jit_axes() {
        let cases = [
            (
                SpatialSelectivityRepro {
                    name: "simple",
                    description: "simple path",
                    polygon: ReproPolygon::Simple500v,
                    selectivity_pct: 10,
                    rel_parallel_workers: 0,
                    jit: ReproJit::Off,
                },
                10,
                90,
                "125",
                "generated ST_Buffer simple path ~500 vertices",
                vec!["SET jit = off"],
            ),
            (
                SpatialSelectivityRepro {
                    name: "cooperative",
                    description: "cooperative path",
                    polygon: ReproPolygon::Coop1024v,
                    selectivity_pct: 90,
                    rel_parallel_workers: 4,
                    jit: ReproJit::On,
                },
                90,
                10,
                "256",
                "generated ST_Buffer cooperative path 1024+ vertices",
                vec![
                    "SET jit = on",
                    "SET jit_above_cost = 0",
                    "SET jit_inline_above_cost = 0",
                    "SET jit_optimize_above_cost = 0",
                ],
            ),
        ];

        for (workload, inside, outside, polygon_segments, polygon_label, jit_sql) in cases {
            assert_eq!(workload.category(), "gpu_spatial_repro");
            let setup = workload.setup_sql(100);
            assert_eq!(setup.len(), 7);
            assert!(setup[2].contains(&format!(
                "parallel_workers = {}",
                workload.rel_parallel_workers
            )));
            assert!(setup[3].contains(polygon_label));
            assert!(setup[3].contains(&format!(
                "target_selectivity={}pct",
                workload.selectivity_pct
            )));
            assert!(setup[4].contains(&format!("generate_series(1, {inside})")));
            assert!(setup[5].contains(&format!("generate_series(1, {outside})")));
            assert!(setup[5].contains(&format!("SELECT ({inside} + g)::bigint")));
            assert!(workload.query_sql().contains(polygon_segments));

            let pre_query = workload.pre_query_sql();
            assert_eq!(&pre_query[5..], jit_sql.as_slice());
            assert_eq!(workload.name(), workload.name);
            assert_eq!(workload.description(), workload.description);
            assert_eq!(
                workload.cleanup_sql(),
                ["DROP TABLE IF EXISTS bench_spatial_sel_repro"]
            );
        }
    }

    #[test]
    fn repro_setup_uses_saturating_counts_for_hostile_percentages() {
        let workload = SpatialSelectivityRepro {
            name: "hostile",
            description: "hostile percentage",
            polygon: ReproPolygon::Simple500v,
            selectivity_pct: usize::MAX,
            rel_parallel_workers: 1,
            jit: ReproJit::Off,
        };
        let setup = workload.setup_sql(2);
        assert!(setup[4].contains(&format!(
            "generate_series(1, {})",
            2usize.saturating_mul(usize::MAX) / 100
        )));
        assert!(setup[5].contains("generate_series(1, 0)"));
    }
}

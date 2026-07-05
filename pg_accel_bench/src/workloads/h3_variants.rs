// H3 workload baselines — fixed per benchmarks/action_items.md §0
// and Reviewer 1 Sin #4 (review_1.md).
//
// Why each workload in this file distinguishes between `query_sql`
// (pg_accel-enabled side) and `baseline_query_sql` (PG parallel side):
//
// Under the previous design, both the accel side and the PG baseline
// ran the *same* SQL — e.g. `SELECT count(h3_latlng_to_cell(point, 15))`.
// That produced a 55.78x "speedup" at 100K rows which Reviewer 1
// traced to `benchmarks/plans.txt:7541,7559,7577`, where the accel
// side reports `GPU Dispatched: false` at every supposedly-winning
// scale. In other words: **both sides were running on CPU**; the
// "speedup" was pg_accel's expression-interpretation path beating
// pg_accel's own per-row fcinfo wrapper over the h3-pg C function.
// Neither side measured GPU acceleration.
//
// The fix for winning workloads: the baseline query is routed through
// a path pg_accel's adapter matcher *cannot intercept* even when the
// GPU planner hook is active. Quarantined scalar workloads deliberately
// keep the unqualified spelling on the pg_accel-enabled side so accidental
// re-registration shows up in plan/speedup evidence.
//
//   1. `h3_latlng_to_cell` → `public.h3_lat_lng_to_cell`. h3-pg ships
//      both names as aliases of the same C function; pg_accel's
//      adapter only lists the concatenated spelling, so
//      `h3_lat_lng_to_cell` bypasses the planner hook entirely and falls
//      through to the h3-pg C implementation. This is the true "stock h3"
//      baseline.
//
//   2. `h3_grid_distance`, `h3_cell_to_parent` -> schema-qualified as
//      `public.h3_grid_distance` / `public.h3_cell_to_parent` only on
//      the baseline side. These names have no underscored alias in h3-pg.
//      The adapter intentionally does not register them for normal planning
//      because they are near parity as standalone scalar GPU paths. With
//      pg_accel enabled, PostgreSQL should still choose the native h3-pg
//      function unless a future fused GPU pipeline reintroduces them with
//      benchmark evidence.
//
// Retired workloads (per action_items.md §4 W8 / Reviewer 1 Sin #3):
//
//   - `h3_latlng_res3`  — same kernel as res15, only the integer
//                         argument differs.
//   - `h3_latlng_res9`  — same kernel as res15, only the integer
//                         argument differs.
//
// Keeping all three was measurement inflation: one kernel counted
// three times, contributing 15 scale rows to the aggregate. Only
// `h3_latlng_res15` is retained.
//
// Requires h3-pg (`CREATE EXTENSION h3`). Verified present in the
// benchmark environment via `$(pg_config --sharedir)/extension/h3.control`.
// If h3-pg is not installed, `runner.rs::ensure_extensions` fails the
// run before any h3 workload starts.

use crate::config::ROW_SCALES as DEFAULT_ROW_SCALES;

use super::Workload;

const H3_NATIVE_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Parametric H3 benchmark: bulk operations at various resolutions.
pub struct H3Variant {
    pub name: &'static str,
    pub description: &'static str,
    pub setup_extra: &'static str,
    /// SQL executed on the pg_accel-enabled side. Winning workloads should
    /// use the registered accelerated spelling. Quarantined workloads keep
    /// the unqualified h3-pg spelling and should remain native.
    pub query: &'static str,
    /// SQL executed on the **PG parallel baseline** side. It must call a
    /// function name or schema-qualified path that measures stock h3-pg.
    pub baseline_query: &'static str,
    pub row_scales: &'static [usize],
    pub cleanup_extra: &'static str,
}

impl Workload for H3Variant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let mut stmts = vec![
            "DROP TABLE IF EXISTS bench_h3_var".to_owned(),
            "CREATE TABLE bench_h3_var (\
               id serial PRIMARY KEY, \
               lat float8 NOT NULL, \
               lng float8 NOT NULL, \
               geom point NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_h3_var (lat, lng, geom) \
                 SELECT lat, lng, point(lng, lat) \
                 FROM (\
                   SELECT \
                     40.4 + random() * 0.8 AS lat, \
                     -74.3 + random() * 0.8 AS lng \
                   FROM generate_series(1, {rows})\
                 ) src"
            ),
        ];
        if !self.setup_extra.is_empty() {
            stmts.push(self.setup_extra.to_owned());
        }
        stmts.push("ANALYZE bench_h3_var".to_owned());
        stmts
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(self.baseline_query.to_owned())
    }

    fn row_scales(&self) -> &'static [usize] {
        self.row_scales
    }

    fn cleanup_sql(&self) -> Vec<String> {
        let mut stmts = vec!["DROP TABLE IF EXISTS bench_h3_var".to_owned()];
        if !self.cleanup_extra.is_empty() {
            stmts.push(self.cleanup_extra.to_owned());
        }
        stmts
    }
}

// H3 point → cell at resolution 15 (finest grid, most compute).
//
// Accel side:    `h3_latlng_to_cell`   — pg_accel intercepts and
//                                        routes to GpuH3 kernel.
// Baseline side: `h3_lat_lng_to_cell`  — h3-pg alias of the same C
//                                        function, not in pg_accel's
//                                        adapter list, so pg_accel's
//                                        planner hook ignores it and
//                                        PG runs stock h3-pg.
pub const H3_LATLNG_RES15: H3Variant = H3Variant {
    name: "h3_latlng_res15",
    description: "h3_latlng_to_cell at resolution 15 — finest grid, maximum compute. \
                  Baseline uses h3-pg `h3_lat_lng_to_cell` alias (stock C impl).",
    setup_extra: "",
    query: "SELECT count(*) AS group_count, \
                   sum(n)::bigint AS input_rows, \
                   min(cell::text) AS min_cell, \
                   max(cell::text) AS max_cell, \
                   sum(hashtextextended(cell::text || ':' || n::text, 0)::numeric) \
                     AS cell_count_checksum \
            FROM (\
              SELECT h3_latlng_to_cell(geom, 15) AS cell, count(*) AS n \
              FROM bench_h3_var GROUP BY 1\
            ) grouped",
    baseline_query: "SELECT count(*) AS group_count, \
                            sum(n)::bigint AS input_rows, \
                            min(cell::text) AS min_cell, \
                            max(cell::text) AS max_cell, \
                            sum(hashtextextended(cell::text || ':' || n::text, 0)::numeric) \
                              AS cell_count_checksum \
                     FROM (\
                       SELECT public.h3_lat_lng_to_cell(geom, 15) AS cell, count(*) AS n \
                       FROM bench_h3_var GROUP BY 1\
                     ) grouped",
    row_scales: DEFAULT_ROW_SCALES,
    cleanup_extra: "",
};

// H3 grid distance between nearby cells.
//
// Setup uses `h3_lat_lng_to_cell` (h3-pg alias) so the fixture is
// populated through the stock C path regardless of pg_accel state.
// The pg_accel-enabled query intentionally calls unqualified
// `h3_grid_distance`; the adapter must decline it so PostgreSQL runs native
// h3-pg unless this op is later fused into a larger GPU pipeline.
pub const H3_DIST_NEAR: H3Variant = H3Variant {
    name: "h3_dist_near",
    description: "h3_grid_distance between nearby cells — native-decline guard \
                  for near-parity scalar H3. Baseline uses stock h3-pg via \
                  `public.h3_grid_distance`.",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell_a h3index, \
                  ADD COLUMN cell_b h3index; \
                  UPDATE bench_h3_var SET \
                    cell_a = public.h3_lat_lng_to_cell(point(lng, lat), 7), \
                    cell_b = public.h3_lat_lng_to_cell(point(lng + 0.001, lat + 0.001), 7)",
    query: "SELECT sum(h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    baseline_query: "SELECT sum(public.h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    row_scales: H3_NATIVE_DECLINE_ROW_SCALES,
    cleanup_extra: "",
};

// H3 grid distance between far cells. Also a native-decline guard.
pub const H3_DIST_FAR: H3Variant = H3Variant {
    name: "h3_dist_far",
    description: "h3_grid_distance between distant cells — native-decline guard \
                  for near-parity scalar H3. Baseline uses stock h3-pg via \
                  `public.h3_grid_distance`.",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell_a h3index, \
                  ADD COLUMN cell_b h3index; \
                  UPDATE bench_h3_var SET \
                    cell_a = public.h3_lat_lng_to_cell(point(lng, lat), 5), \
                    cell_b = public.h3_lat_lng_to_cell(point(lng + 0.5, lat + 0.5), 5)",
    query: "SELECT sum(h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    baseline_query: "SELECT sum(public.h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    row_scales: H3_NATIVE_DECLINE_ROW_SCALES,
    cleanup_extra: "",
};

// H3 cell to parent (res 15 -> 3). Native-decline guard.
pub const H3_PARENT_DEEP: H3Variant = H3Variant {
    name: "h3_parent_deep",
    description: "h3_cell_to_parent res 15->3 — native-decline guard for \
                  near-parity scalar H3. Baseline uses stock h3-pg via \
                  `public.h3_cell_to_parent`.",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell h3index; \
                  UPDATE bench_h3_var SET \
                    cell = public.h3_lat_lng_to_cell(point(lng, lat), 15)",
    query: "SELECT count(*) AS group_count, \
                   sum(n)::bigint AS input_rows, \
                   min(parent_cell::text) AS min_cell, \
                   max(parent_cell::text) AS max_cell, \
                   sum(hashtextextended(parent_cell::text || ':' || n::text, 0)::numeric) \
                     AS cell_count_checksum \
            FROM (\
              SELECT h3_cell_to_parent(cell, 3) AS parent_cell, count(*) AS n \
              FROM bench_h3_var GROUP BY 1\
            ) grouped",
    baseline_query: "SELECT count(*) AS group_count, \
                            sum(n)::bigint AS input_rows, \
                            min(parent_cell::text) AS min_cell, \
                            max(parent_cell::text) AS max_cell, \
                            sum(hashtextextended(parent_cell::text || ':' || n::text, 0)::numeric) \
                              AS cell_count_checksum \
                     FROM (\
                       SELECT public.h3_cell_to_parent(cell, 3) AS parent_cell, count(*) AS n \
                       FROM bench_h3_var GROUP BY 1\
                     ) grouped",
    row_scales: H3_NATIVE_DECLINE_ROW_SCALES,
    cleanup_extra: "",
};

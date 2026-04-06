use super::Workload;

/// Parametric H3 benchmark: bulk operations at various resolutions.
pub struct H3Variant {
    pub name: &'static str,
    pub description: &'static str,
    pub setup_extra: &'static str,
    pub query: &'static str,
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
               lng float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_h3_var (lat, lng) \
                 SELECT \
                   40.4 + random() * 0.8, \
                   -74.3 + random() * 0.8 \
                 FROM generate_series(1, {rows})"
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

    fn cleanup_sql(&self) -> Vec<String> {
        let mut stmts = vec!["DROP TABLE IF EXISTS bench_h3_var".to_owned()];
        if !self.cleanup_extra.is_empty() {
            stmts.push(self.cleanup_extra.to_owned());
        }
        stmts
    }
}

// H3 lat/lng → cell at resolution 3 (coarse grid, ~100 FLOPs/row)
pub const H3_LATLNG_RES3: H3Variant = H3Variant {
    name: "h3_latlng_res3",
    description: "h3_latlng_to_cell at resolution 3 — coarse grid, trig-heavy GPU",
    setup_extra: "",
    query: "SELECT count(h3_latlng_to_cell(point(lng, lat), 3)) \
            FROM bench_h3_var",
    cleanup_extra: "",
};

// H3 lat/lng → cell at resolution 9 (medium grid)
pub const H3_LATLNG_RES9: H3Variant = H3Variant {
    name: "h3_latlng_res9",
    description: "h3_latlng_to_cell at resolution 9 — medium grid, trig-heavy GPU",
    setup_extra: "",
    query: "SELECT count(h3_latlng_to_cell(point(lng, lat), 9)) \
            FROM bench_h3_var",
    cleanup_extra: "",
};

// H3 lat/lng → cell at resolution 15 (finest grid, most compute)
pub const H3_LATLNG_RES15: H3Variant = H3Variant {
    name: "h3_latlng_res15",
    description: "h3_latlng_to_cell at resolution 15 — finest grid, maximum compute",
    setup_extra: "",
    query: "SELECT count(h3_latlng_to_cell(point(lng, lat), 15)) \
            FROM bench_h3_var",
    cleanup_extra: "",
};

// H3 grid distance between nearby cells
pub const H3_DIST_NEAR: H3Variant = H3Variant {
    name: "h3_dist_near",
    description: "h3_grid_distance between nearby cells — IJK coordinate math",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell_a h3index, \
                  ADD COLUMN cell_b h3index; \
                  UPDATE bench_h3_var SET \
                    cell_a = h3_latlng_to_cell(point(lng, lat), 7), \
                    cell_b = h3_latlng_to_cell(point(lng + 0.001, lat + 0.001), 7)",
    query: "SELECT count(h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    cleanup_extra: "",
};

// H3 grid distance between far cells
pub const H3_DIST_FAR: H3Variant = H3Variant {
    name: "h3_dist_far",
    description: "h3_grid_distance between distant cells — more IJK computation",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell_a h3index, \
                  ADD COLUMN cell_b h3index; \
                  UPDATE bench_h3_var SET \
                    cell_a = h3_latlng_to_cell(point(lng, lat), 5), \
                    cell_b = h3_latlng_to_cell(point(lng + 0.5, lat + 0.5), 5)",
    query: "SELECT count(h3_grid_distance(cell_a, cell_b)) FROM bench_h3_var",
    cleanup_extra: "",
};

// H3 cell to parent (res 15 → 3, deep traversal)
pub const H3_PARENT_DEEP: H3Variant = H3Variant {
    name: "h3_parent_deep",
    description: "h3_cell_to_parent res 15→3 — deep resolution traversal",
    setup_extra: "ALTER TABLE bench_h3_var ADD COLUMN cell h3index; \
                  UPDATE bench_h3_var SET \
                    cell = h3_latlng_to_cell(point(lng, lat), 15)",
    query: "SELECT count(h3_cell_to_parent(cell, 3)) FROM bench_h3_var",
    cleanup_extra: "",
};

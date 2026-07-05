use super::Workload;

/// Parametric sort benchmark: GPU radix sort on narrow tables by type.
pub struct SortVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub col_type: &'static str,
    pub col_gen: &'static str,
}

impl Workload for SortVariant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_sort"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_var".to_owned(),
            format!(
                "CREATE TABLE bench_sort_var (\
                   id serial PRIMARY KEY, \
                   val {} NOT NULL\
                 )",
                self.col_type
            ),
            format!(
                "INSERT INTO bench_sort_var (val) \
                 SELECT {} FROM generate_series(1, {rows})",
                self.col_gen
            ),
            "ANALYZE bench_sort_var".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT id, val FROM bench_sort_var ORDER BY val, id LIMIT 1000".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_var".to_owned()]
    }
}

pub const SORT_INT4: SortVariant = SortVariant {
    name: "sort_int4",
    description: "ORDER BY int4 — narrow-row GPU radix sort",
    col_type: "int4",
    col_gen: "(random() * 2147483647)::int4",
};

pub const SORT_INT8: SortVariant = SortVariant {
    name: "sort_int8",
    description: "ORDER BY int8 — narrow-row GPU radix sort",
    col_type: "bigint",
    col_gen: "(random() * 9223372036854775807)::bigint",
};

pub const SORT_FLOAT4: SortVariant = SortVariant {
    name: "sort_float4",
    description: "ORDER BY float4 — narrow-row GPU radix sort",
    col_type: "float4",
    col_gen: "(random() * 1000000)::float4",
};

pub const SORT_FLOAT8: SortVariant = SortVariant {
    name: "sort_float8",
    description: "ORDER BY float8 — narrow-row GPU radix sort",
    col_type: "float8",
    col_gen: "random() * 1000000",
};

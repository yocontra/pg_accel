//! Discovers PostgreSQL functions and matches them against acceleration patterns.
//!
//! Uses SPI to query `pg_proc` and `pg_namespace` system catalogs,
//! matching functions that can be offloaded to GPU execution.

use std::fmt::Write as _;

use pgrx::pg_sys;

/// A pattern describing a function we can accelerate.
///
/// At minimum, `name` must be provided. Schema and type filters
/// are optional and narrow the match when present.
#[derive(Debug, Clone)]
pub struct FunctionPattern {
    /// Schema to restrict the search to (e.g., `"public"`). `None` matches any schema.
    pub schema: Option<String>,
    /// Function name to match (required).
    pub name: String,
    /// Expected argument types. `None` matches any signature.
    pub arg_types: Option<Vec<pg_sys::Oid>>,
    /// Expected return type. `None` matches any return type.
    pub return_type: Option<pg_sys::Oid>,
}

/// A function that matched an acceleration pattern.
#[derive(Debug)]
pub struct MatchedFunction {
    /// The `pg_proc` OID of the function.
    pub oid: pg_sys::Oid,
    /// Function name.
    pub name: String,
    /// Schema the function belongs to.
    pub schema: String,
    /// Argument type OIDs.
    pub arg_oids: Vec<pg_sys::Oid>,
    /// Return type OID.
    pub return_oid: pg_sys::Oid,
    /// Whether the function is marked `PARALLEL SAFE`.
    pub is_parallel_safe: bool,
    /// Whether the function is declared `STRICT` (returns NULL on NULL input).
    pub is_strict: bool,
}

/// Build the SQL query for discovering functions from `pg_proc`.
///
/// Returns `(query_string, params)` where params are positional `$N` bindings.
fn build_discovery_query(pattern: &FunctionPattern) -> String {
    let mut sql = String::from(
        "SELECT p.oid, p.proname::text, n.nspname::text, p.proargtypes::text, \
         p.prorettype, p.proparallel::text, p.proisstrict \
         FROM pg_proc p \
         JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE p.proname = ",
    );

    // Quote the function name to prevent injection.
    sql.push('\'');
    // Escape single quotes in the name.
    for ch in pattern.name.chars() {
        if ch == '\'' {
            sql.push_str("''");
        } else {
            sql.push(ch);
        }
    }
    sql.push('\'');

    if let Some(ref schema) = pattern.schema {
        sql.push_str(" AND n.nspname = '");
        for ch in schema.chars() {
            if ch == '\'' {
                sql.push_str("''");
            } else {
                sql.push(ch);
            }
        }
        sql.push('\'');
    }

    if let Some(ref ret_type) = pattern.return_type {
        let _ = write!(sql, " AND p.prorettype = {}", ret_type.to_u32());
    }

    sql
}

/// Discover functions matching a pattern by querying `pg_proc`.
///
/// Uses SPI to query the system catalogs. Must be called within
/// a PostgreSQL transaction context (i.e., inside a `pg_extern` or hook).
///
/// # Errors
///
/// Returns an empty `Vec` if SPI fails or no functions match.
#[must_use]
pub fn discover_functions(pattern: &FunctionPattern) -> Vec<MatchedFunction> {
    let query = build_discovery_query(pattern);

    // SPI execution requires an active PostgreSQL transaction context.
    // pgrx::Spi::connect provides that bridge.
    pgrx::Spi::connect(|client| {
        let table = client.select(&query, None, &[]);

        let Ok(table) = table else {
            pgrx::debug1!("pg_accel: discover_functions SPI error for query: {query}");
            return Vec::new();
        };

        let mut results = Vec::new();

        for row in table {
            // Extract fields, skipping rows where required columns are NULL.
            let Some(func_oid) = row.get::<pg_sys::Oid>(1).ok().flatten() else {
                continue;
            };
            let Some(name) = row.get::<String>(2).ok().flatten() else {
                continue;
            };
            let Some(schema) = row.get::<String>(3).ok().flatten() else {
                continue;
            };
            // proargtypes is cast to text in the query, yielding a
            // space-separated list of OIDs (e.g. "23 25 701").
            let arg_oids: Vec<pg_sys::Oid> = row
                .get::<String>(4)
                .ok()
                .flatten()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .map(pg_sys::Oid::from)
                .collect();

            let Some(return_oid) = row.get::<pg_sys::Oid>(5).ok().flatten() else {
                continue;
            };

            // proparallel: 's' = safe, 'r' = restricted, 'u' = unsafe
            let parallel_str = row.get::<String>(6).ok().flatten().unwrap_or_default();
            let is_parallel_safe = parallel_str == "s";

            let is_strict = row.get::<bool>(7).ok().flatten().unwrap_or(false);

            // If caller specified arg_types, filter by exact match.
            if let Some(ref expected) = pattern.arg_types
                && *expected != arg_oids
            {
                continue;
            }

            results.push(MatchedFunction {
                oid: func_oid,
                name,
                schema,
                arg_oids,
                return_oid,
                is_parallel_safe,
                is_strict,
            });
        }

        pgrx::debug1!(
            "pg_accel: discover_functions('{}') found {} matches",
            pattern.name,
            results.len()
        );
        results
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_query_name_only() {
        let pattern = FunctionPattern {
            schema: None,
            name: "st_contains".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("p.proname = 'st_contains'"));
        assert!(!sql.contains("AND n.nspname ="));
    }

    #[test]
    fn build_query_with_schema() {
        let pattern = FunctionPattern {
            schema: Some("public".to_string()),
            name: "st_contains".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("n.nspname = 'public'"));
    }

    #[test]
    fn build_query_escapes_quotes() {
        let pattern = FunctionPattern {
            schema: None,
            name: "o'malley".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("'o''malley'"));
    }

    #[test]
    fn build_query_with_return_type() {
        let pattern = FunctionPattern {
            schema: None,
            name: "my_func".to_string(),
            arg_types: None,
            return_type: Some(pg_sys::BOOLOID),
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("p.prorettype = 16"));
    }

    #[test]
    fn build_query_empty_name() {
        let pattern = FunctionPattern {
            schema: None,
            name: String::new(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("p.proname = ''"));
    }

    #[test]
    fn build_query_schema_with_quotes() {
        let pattern = FunctionPattern {
            schema: Some("it's_mine".to_string()),
            name: "func".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("n.nspname = 'it''s_mine'"));
    }

    #[test]
    fn build_query_all_filters() {
        let pattern = FunctionPattern {
            schema: Some("public".to_string()),
            name: "st_contains".to_string(),
            arg_types: Some(vec![pg_sys::Oid::from(600_u32), pg_sys::Oid::from(600_u32)]),
            return_type: Some(pg_sys::BOOLOID),
        };
        let sql = build_discovery_query(&pattern);
        // arg_types are not used in the SQL query (filtered post-hoc)
        assert!(sql.contains("p.proname = 'st_contains'"));
        assert!(sql.contains("n.nspname = 'public'"));
        assert!(sql.contains("p.prorettype = 16"));
    }

    #[test]
    fn build_query_name_with_multiple_quotes() {
        let pattern = FunctionPattern {
            schema: None,
            name: "a''b".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        // Each ' becomes '', so a''b becomes a''''b
        assert!(sql.contains("'a''''b'"));
    }

    #[test]
    fn build_query_contains_required_columns() {
        let pattern = FunctionPattern {
            schema: None,
            name: "test".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(sql.contains("p.oid"));
        assert!(sql.contains("p.proname"));
        assert!(sql.contains("n.nspname"));
        assert!(sql.contains("p.proargtypes"));
        assert!(sql.contains("p.prorettype"));
        assert!(sql.contains("p.proparallel"));
        assert!(sql.contains("p.proisstrict"));
        assert!(sql.contains("pg_proc"));
        assert!(sql.contains("pg_namespace"));
    }

    #[test]
    fn build_query_no_return_type_filter_when_none() {
        let pattern = FunctionPattern {
            schema: None,
            name: "test".to_string(),
            arg_types: None,
            return_type: None,
        };
        let sql = build_discovery_query(&pattern);
        assert!(!sql.contains("prorettype ="));
    }

    #[test]
    fn function_pattern_debug() {
        let pattern = FunctionPattern {
            schema: Some("public".to_string()),
            name: "my_func".to_string(),
            arg_types: None,
            return_type: None,
        };
        let debug = format!("{pattern:?}");
        assert!(debug.contains("my_func"));
        assert!(debug.contains("public"));
    }

    #[test]
    fn matched_function_debug() {
        let mf = MatchedFunction {
            oid: pg_sys::Oid::from(123_u32),
            name: "st_contains".to_string(),
            schema: "public".to_string(),
            arg_oids: vec![pg_sys::Oid::from(600_u32)],
            return_oid: pg_sys::BOOLOID,
            is_parallel_safe: true,
            is_strict: false,
        };
        let debug = format!("{mf:?}");
        assert!(debug.contains("st_contains"));
        assert!(debug.contains("public"));
    }
}

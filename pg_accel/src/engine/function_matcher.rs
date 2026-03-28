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
        "SELECT p.oid, p.proname, n.nspname, p.proargtypes, \
         p.prorettype, p.proparallel, p.proisstrict \
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
            // proargtypes is an oidvector; extract as raw bytes and parse.
            // For now we store an empty vec; Phase 2 will parse oidvector.
            let arg_oids: Vec<pg_sys::Oid> = Vec::new();

            let Some(return_oid) = row.get::<pg_sys::Oid>(5).ok().flatten() else {
                continue;
            };

            // proparallel: 's' = safe, 'r' = restricted, 'u' = unsafe
            #[allow(clippy::cast_sign_loss)]
            let parallel_char = row.get::<i8>(6).ok().flatten().map_or(b'u', |v| v as u8);
            let is_parallel_safe = parallel_char == b's';

            let is_strict = row.get::<bool>(7).ok().flatten().unwrap_or(false);

            // If caller specified arg_types, filter by them.
            // TODO(phase2): Implement proper oidvector comparison once
            // arg_oids parsing is complete.
            if let Some(ref expected) = pattern.arg_types
                && !arg_oids.is_empty()
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
}

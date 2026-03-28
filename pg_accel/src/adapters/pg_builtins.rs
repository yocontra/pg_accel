//! PostgreSQL built-in function adapter.
//!
//! Declares core PostgreSQL functions that `pg_accel` can accelerate using
//! batched evaluation on the main backend thread via Custom Scan batching.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the PostgreSQL built-ins adapter with all supported function entries.
///
/// All functions use [`AccelStrategy::BatchedEval`] — they benefit from
/// Custom Scan batching but run on the main backend thread (palloc, text
/// manipulation, timestamp arithmetic all require backend context).
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // Math functions (int4, int8, float8 overloads resolved by PG).
    const MATH_NAMES: &[&str] = &["abs", "sqrt", "log"];

    // Text functions.
    const TEXT_NAMES: &[&str] = &["length", "lower", "upper", "btrim"];

    // Timestamp functions.
    const TS_NAMES: &[&str] = &["date_part", "age", "date_trunc"];

    // JSON functions.
    const JSON_NAMES: &[&str] = &["jsonb_extract_path_text", "jsonb_typeof"];

    let functions = MATH_NAMES
        .iter()
        .chain(TEXT_NAMES.iter())
        .chain(TS_NAMES.iter())
        .chain(JSON_NAMES.iter())
        .map(|&name| FunctionAccelEntry {
            schema: "pg_catalog",
            name,
            strategy: AccelStrategy::BatchedEval,
        })
        .collect();

    ExtensionAdapter {
        name: "pg_builtins",
        version_query: "SELECT version()",
        functions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_has_expected_function_count() {
        let a = adapter();
        assert_eq!(a.name, "pg_builtins");
        // 3 math + 4 text + 3 timestamp + 2 json = 12
        assert_eq!(a.functions.len(), 12);
    }

    #[test]
    fn all_functions_use_batched_eval() {
        let a = adapter();
        assert!(
            a.functions
                .iter()
                .all(|f| f.strategy == AccelStrategy::BatchedEval)
        );
    }

    #[test]
    fn all_functions_use_pg_catalog_schema() {
        let a = adapter();
        assert!(a.functions.iter().all(|f| f.schema == "pg_catalog"));
    }

    #[test]
    fn contains_expected_functions() {
        let a = adapter();
        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        assert!(names.contains(&"abs"));
        assert!(names.contains(&"lower"));
        assert!(names.contains(&"date_part"));
        assert!(names.contains(&"jsonb_typeof"));
    }
}

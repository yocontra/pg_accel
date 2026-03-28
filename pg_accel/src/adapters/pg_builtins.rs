//! PostgreSQL built-in function adapter.
//!
//! Declares core PostgreSQL functions that `pg_accel` can accelerate using
//! batched evaluation, GPU sort, or GPU reduction strategies.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the PostgreSQL built-ins adapter with all supported function entries.
///
/// Scalar math/string functions use [`AccelStrategy::BatchedEval`].
/// Aggregate functions (sum, avg, etc.) use [`AccelStrategy::GpuReduce`].
#[must_use]
pub fn pg_builtins_adapter() -> ExtensionAdapter {
    const BATCHED_NAMES: &[&str] = &[
        "abs",
        "sqrt",
        "cbrt",
        "ceil",
        "floor",
        "round",
        "trunc",
        "lower",
        "upper",
        "length",
        "date_trunc",
        "date_part",
    ];
    const REDUCE_NAMES: &[&str] = &["sum", "avg", "min", "max", "count"];

    let functions = BATCHED_NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "pg_catalog",
            name,
            strategy: AccelStrategy::BatchedEval,
        })
        .chain(REDUCE_NAMES.iter().map(|&name| FunctionAccelEntry {
            schema: "pg_catalog",
            name,
            strategy: AccelStrategy::GpuReduce,
        }))
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
        let adapter = pg_builtins_adapter();
        assert_eq!(adapter.name, "pg_builtins");
        // 12 batched + 5 reduce = 17
        assert_eq!(adapter.functions.len(), 17);
    }

    #[test]
    fn batched_eval_count() {
        let adapter = pg_builtins_adapter();
        let count = adapter
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::BatchedEval)
            .count();
        assert_eq!(count, 12);
    }

    #[test]
    fn gpu_reduce_count() {
        let adapter = pg_builtins_adapter();
        let count = adapter
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuReduce)
            .count();
        assert_eq!(count, 5);
    }
}

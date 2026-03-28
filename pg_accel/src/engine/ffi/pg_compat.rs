//! PostgreSQL version compatibility shims.
//!
//! Abstracts differences between PG 15-18 internal APIs so that the rest of
//! the codebase never needs version-gated logic for these checks.

/// Byte value of the `prokind` column indicating a normal (plain) function.
const PROKIND_FUNCTION: i8 = b'f'.cast_signed();

/// Byte value of `proparallel` indicating parallel-safe.
const PROPARALLEL_SAFE: i8 = b's'.cast_signed();

/// Byte value of `proparallel` indicating parallel-restricted.
const PROPARALLEL_RESTRICTED: i8 = b'r'.cast_signed();

/// Byte value of `proparallel` indicating parallel-unsafe.
const PROPARALLEL_UNSAFE: i8 = b'u'.cast_signed();

/// Byte value of `prokind` indicating an aggregate function.
const PROKIND_AGGREGATE: i8 = b'a'.cast_signed();

/// Byte value of `prokind` indicating a window function.
const PROKIND_WINDOW: i8 = b'w'.cast_signed();

/// Byte value of `prokind` indicating a procedure.
const PROKIND_PROCEDURE: i8 = b'p'.cast_signed();

/// Check if a `pg_proc.prokind` value represents a normal (plain) function.
///
/// PG 15+ encodes function kind in a single `prokind` char column.
/// Normal functions have `prokind = 'f'`.
#[must_use]
pub const fn is_normal_function(prokind: i8) -> bool {
    prokind == PROKIND_FUNCTION
}

/// Check if a `pg_proc.prokind` value represents an aggregate function.
#[must_use]
pub const fn is_aggregate_function(prokind: i8) -> bool {
    prokind == PROKIND_AGGREGATE
}

/// Check if a `pg_proc.prokind` value represents a window function.
#[must_use]
pub const fn is_window_function(prokind: i8) -> bool {
    prokind == PROKIND_WINDOW
}

/// Check if a `pg_proc.prokind` value represents a procedure.
#[must_use]
pub const fn is_procedure(prokind: i8) -> bool {
    prokind == PROKIND_PROCEDURE
}

/// Check if a `pg_proc.proparallel` value indicates parallel-safe execution.
///
/// Parallel-safe functions can run in any parallel worker without restriction.
#[must_use]
pub const fn is_parallel_safe(proparallel: i8) -> bool {
    proparallel == PROPARALLEL_SAFE
}

/// Check if a `pg_proc.proparallel` value indicates parallel-restricted
/// execution.
///
/// Parallel-restricted functions may only run in the parallel leader.
#[must_use]
pub const fn is_parallel_restricted(proparallel: i8) -> bool {
    proparallel == PROPARALLEL_RESTRICTED
}

/// Check if a `pg_proc.proparallel` value indicates parallel-unsafe execution.
///
/// Parallel-unsafe functions prevent the query from using parallel execution.
#[must_use]
pub const fn is_parallel_unsafe(proparallel: i8) -> bool {
    proparallel == PROPARALLEL_UNSAFE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_function_detected() {
        assert!(is_normal_function(b'f'.cast_signed()));
        assert!(!is_normal_function(b'a'.cast_signed()));
        assert!(!is_normal_function(b'w'.cast_signed()));
        assert!(!is_normal_function(b'p'.cast_signed()));
    }

    #[test]
    fn aggregate_window_procedure_detected() {
        assert!(is_aggregate_function(b'a'.cast_signed()));
        assert!(is_window_function(b'w'.cast_signed()));
        assert!(is_procedure(b'p'.cast_signed()));
    }

    #[test]
    fn parallel_safety_levels() {
        assert!(is_parallel_safe(b's'.cast_signed()));
        assert!(!is_parallel_safe(b'r'.cast_signed()));
        assert!(!is_parallel_safe(b'u'.cast_signed()));

        assert!(is_parallel_restricted(b'r'.cast_signed()));
        assert!(!is_parallel_restricted(b's'.cast_signed()));

        assert!(is_parallel_unsafe(b'u'.cast_signed()));
        assert!(!is_parallel_unsafe(b's'.cast_signed()));
    }
}

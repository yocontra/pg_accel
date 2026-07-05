//! Column-at-a-time deserialization helpers for late materialization.
//!
//! Classifies column types by deserialization cost so that predicate
//! ordering can evaluate cheap predicates (integer comparisons) before
//! expensive ones (geometry deserialization, JSONB parsing).

use pgrx::pg_sys;

// ---------------------------------------------------------------------------
// Column cost classification
// ---------------------------------------------------------------------------

/// Relative cost tier for deserializing a column value.
///
/// Used by the predicate chain to order evaluation: cheap columns first,
/// expensive columns last. Rows rejected by a cheap predicate skip the
/// expensive deserialization entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColumnCostTier {
    /// Fixed-width pass-by-value types: int2/4/8, float4/8, bool, oid.
    /// Deserialization is essentially free (datum *is* the value).
    Cheap = 0,
    /// Variable-length but simple types: text, varchar, bytea, name.
    /// Require length decoding but no complex parsing.
    Medium = 1,
    /// Complex structured types: geometry, geography, jsonb, arrays,
    /// composite types. Require full deserialization and possibly
    /// detoasting before any useful predicate can run.
    Expensive = 2,
}

/// Classify a PostgreSQL type OID into a deserialization cost tier.
///
/// This is a heuristic — unknown types default to [`ColumnCostTier::Expensive`]
/// to ensure we never under-estimate cost.
#[must_use]
pub fn classify_type_cost(type_oid: pg_sys::Oid) -> ColumnCostTier {
    // SAFETY: These are well-known OID constants from pg_type.h. We compare
    // against the raw u32 value since pgrx re-exports them as Oid.
    let oid_val = type_oid.to_u32();

    match oid_val {
        // Cheap: fixed-width, pass-by-value types.
        // BOOLOID | INT2OID | INT4OID | INT8OID | FLOAT4OID | FLOAT8OID
        // | OIDOID | DATEOID | TIMESTAMPOID | TIMESTAMPTZOID | TIMEOID
        // | TIMETZOID
        16 | 21 | 23 | 20 | 700 | 701 | 26 | 1082 | 1114 | 1184 | 1083 | 1266 => {
            ColumnCostTier::Cheap
        }

        // Medium: variable-length but simple types.
        // TEXTOID | BPCHAROID | VARCHAROID | BYTEAOID | NAMEOID
        // | NUMERICOID | UUIDOID
        25 | 1042 | 1043 | 17 | 19 | 1700 | 2950 => ColumnCostTier::Medium,

        // Everything else is expensive (geometry, jsonb, arrays, etc.).
        _ => ColumnCostTier::Expensive,
    }
}

/// Estimate for a single column in a batch extraction plan.
#[derive(Debug, Clone)]
pub struct ColumnCostEstimate {
    /// Zero-based attribute number.
    pub attnum: usize,
    /// Type OID of this column.
    pub type_oid: pg_sys::Oid,
    /// Deserialization cost tier.
    pub cost_tier: ColumnCostTier,
}

/// Build cost estimates for a list of `(attnum, type_oid)` pairs and return
/// them sorted cheapest-first.
///
/// This ordering is used to decide which predicate columns to extract first
/// during late materialization.
#[must_use]
pub fn plan_column_order(columns: &[(usize, pg_sys::Oid)]) -> Vec<ColumnCostEstimate> {
    let mut estimates: Vec<ColumnCostEstimate> = columns
        .iter()
        .map(|&(attnum, type_oid)| ColumnCostEstimate {
            attnum,
            type_oid,
            cost_tier: classify_type_cost(type_oid),
        })
        .collect();

    estimates.sort_by_key(|e| e.cost_tier);
    estimates
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    #[test]
    fn int4_is_cheap() {
        let tier = classify_type_cost(pg_sys::Oid::from(23_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn float8_is_cheap() {
        let tier = classify_type_cost(pg_sys::Oid::from(701_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn bool_is_cheap() {
        let tier = classify_type_cost(pg_sys::Oid::from(16_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn text_is_medium() {
        let tier = classify_type_cost(pg_sys::Oid::from(25_u32));
        assert_eq!(tier, ColumnCostTier::Medium);
    }

    #[test]
    fn numeric_is_medium() {
        let tier = classify_type_cost(pg_sys::Oid::from(1700_u32));
        assert_eq!(tier, ColumnCostTier::Medium);
    }

    #[test]
    fn unknown_type_is_expensive() {
        // Use an OID unlikely to match any known type.
        let tier = classify_type_cost(pg_sys::Oid::from(99999_u32));
        assert_eq!(tier, ColumnCostTier::Expensive);
    }

    #[test]
    fn plan_column_order_sorts_by_cost() {
        let columns = vec![
            (2, pg_sys::Oid::from(99999_u32)), // expensive (unknown)
            (0, pg_sys::Oid::from(23_u32)),    // cheap (int4)
            (1, pg_sys::Oid::from(25_u32)),    // medium (text)
        ];

        let plan = plan_column_order(&columns);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].attnum, 0); // cheap first
        assert_eq!(plan[1].attnum, 1); // medium second
        assert_eq!(plan[2].attnum, 2); // expensive last
    }

    #[test]
    fn cost_tier_ordering() {
        assert!(ColumnCostTier::Cheap < ColumnCostTier::Medium);
        assert!(ColumnCostTier::Medium < ColumnCostTier::Expensive);
    }

    #[test]
    fn empty_columns_returns_empty() {
        let plan = plan_column_order(&[]);
        assert!(plan.is_empty());
    }

    #[test]
    fn all_cheap_types() {
        // Every fixed-width pass-by-value type should be Cheap.
        let cheap_oids: &[u32] = &[16, 21, 23, 20, 700, 701, 26, 1082, 1114, 1184, 1083, 1266];
        for &oid_val in cheap_oids {
            let tier = classify_type_cost(pg_sys::Oid::from(oid_val));
            assert_eq!(tier, ColumnCostTier::Cheap, "OID {oid_val} should be Cheap");
        }
    }

    #[test]
    fn all_medium_types() {
        let medium_oids: &[u32] = &[25, 1042, 1043, 17, 19, 1700, 2950];
        for &oid_val in medium_oids {
            let tier = classify_type_cost(pg_sys::Oid::from(oid_val));
            assert_eq!(
                tier,
                ColumnCostTier::Medium,
                "OID {oid_val} should be Medium"
            );
        }
    }

    #[test]
    fn oid_type_is_cheap() {
        // OIDOID = 26
        let tier = classify_type_cost(pg_sys::Oid::from(26_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn date_is_cheap() {
        // DATEOID = 1082
        let tier = classify_type_cost(pg_sys::Oid::from(1082_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn timestamp_is_cheap() {
        // TIMESTAMPOID = 1114
        let tier = classify_type_cost(pg_sys::Oid::from(1114_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn timestamptz_is_cheap() {
        // TIMESTAMPTZOID = 1184
        let tier = classify_type_cost(pg_sys::Oid::from(1184_u32));
        assert_eq!(tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn uuid_is_medium() {
        // UUIDOID = 2950
        let tier = classify_type_cost(pg_sys::Oid::from(2950_u32));
        assert_eq!(tier, ColumnCostTier::Medium);
    }

    #[test]
    fn bytea_is_medium() {
        // BYTEAOID = 17
        let tier = classify_type_cost(pg_sys::Oid::from(17_u32));
        assert_eq!(tier, ColumnCostTier::Medium);
    }

    #[test]
    fn jsonb_is_expensive() {
        // JSONBOID = 3802
        let tier = classify_type_cost(pg_sys::Oid::from(3802_u32));
        assert_eq!(tier, ColumnCostTier::Expensive);
    }

    #[test]
    fn array_type_is_expensive() {
        // INT4ARRAYOID = 1007
        let tier = classify_type_cost(pg_sys::Oid::from(1007_u32));
        assert_eq!(tier, ColumnCostTier::Expensive);
    }

    #[test]
    fn zero_oid_is_expensive() {
        // OID 0 (InvalidOid) should fall through to Expensive.
        let tier = classify_type_cost(pg_sys::Oid::from(0_u32));
        assert_eq!(tier, ColumnCostTier::Expensive);
    }

    #[test]
    fn plan_column_order_single_column() {
        let columns = vec![(0, pg_sys::Oid::from(23_u32))];
        let plan = plan_column_order(&columns);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].attnum, 0);
        assert_eq!(plan[0].cost_tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn plan_column_order_all_same_tier() {
        // All cheap — ordering should preserve relative order (stable sort).
        let columns = vec![
            (0, pg_sys::Oid::from(23_u32)),  // int4
            (1, pg_sys::Oid::from(20_u32)),  // int8
            (2, pg_sys::Oid::from(700_u32)), // float4
        ];
        let plan = plan_column_order(&columns);
        assert_eq!(plan.len(), 3);
        // All same tier, stable sort preserves input order.
        assert_eq!(plan[0].attnum, 0);
        assert_eq!(plan[1].attnum, 1);
        assert_eq!(plan[2].attnum, 2);
        for e in &plan {
            assert_eq!(e.cost_tier, ColumnCostTier::Cheap);
        }
    }

    #[test]
    fn plan_column_order_reverse_input() {
        // Input in reverse cost order — should be reordered.
        let columns = vec![
            (0, pg_sys::Oid::from(99999_u32)), // expensive
            (1, pg_sys::Oid::from(25_u32)),    // medium
            (2, pg_sys::Oid::from(23_u32)),    // cheap
        ];
        let plan = plan_column_order(&columns);
        assert_eq!(plan[0].cost_tier, ColumnCostTier::Cheap);
        assert_eq!(plan[1].cost_tier, ColumnCostTier::Medium);
        assert_eq!(plan[2].cost_tier, ColumnCostTier::Expensive);
    }

    #[test]
    fn plan_column_order_duplicates() {
        // Multiple columns with the same type.
        let columns = vec![
            (0, pg_sys::Oid::from(23_u32)),
            (1, pg_sys::Oid::from(23_u32)),
            (2, pg_sys::Oid::from(23_u32)),
        ];
        let plan = plan_column_order(&columns);
        assert_eq!(plan.len(), 3);
        for e in &plan {
            assert_eq!(e.cost_tier, ColumnCostTier::Cheap);
        }
    }

    #[test]
    fn plan_column_order_many_columns() {
        let columns: Vec<(usize, pg_sys::Oid)> = (0..100)
            .map(|i| {
                (
                    i,
                    pg_sys::Oid::from(if i % 3 == 0 {
                        23_u32
                    } else if i % 3 == 1 {
                        25_u32
                    } else {
                        99999_u32
                    }),
                )
            })
            .collect();
        let plan = plan_column_order(&columns);
        assert_eq!(plan.len(), 100);
        // Verify sorted: each tier >= previous tier.
        for w in plan.windows(2) {
            assert!(w[0].cost_tier <= w[1].cost_tier);
        }
    }

    #[test]
    fn column_cost_estimate_fields() {
        let est = ColumnCostEstimate {
            attnum: 42,
            type_oid: pg_sys::Oid::from(23_u32),
            cost_tier: ColumnCostTier::Cheap,
        };
        assert_eq!(est.attnum, 42);
        assert_eq!(est.cost_tier, ColumnCostTier::Cheap);
    }

    #[test]
    fn cost_tier_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ColumnCostTier::Cheap);
        set.insert(ColumnCostTier::Medium);
        set.insert(ColumnCostTier::Expensive);
        assert_eq!(set.len(), 3);
        // Inserting duplicate should not increase size.
        set.insert(ColumnCostTier::Cheap);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn cost_tier_clone_and_debug() {
        let tier = ColumnCostTier::Medium;
        let cloned = tier;
        assert_eq!(tier, cloned);
        let debug = format!("{tier:?}");
        assert_eq!(debug, "Medium");
    }
}

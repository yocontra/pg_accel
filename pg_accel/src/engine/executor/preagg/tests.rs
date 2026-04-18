#![cfg(test)]

use std::collections::HashMap;

use super::partial::{AggAccum, eval_cmp};
use super::*;

mod tests {
    use super::*;

    // -- eval_cmp ---------------------------------------------------------------

    #[test]
    fn cmp_eq_exact() {
        assert!(eval_cmp(42.0, 0, 42.0));
        assert!(!eval_cmp(42.1, 0, 42.0));
    }

    #[test]
    fn cmp_ne() {
        assert!(eval_cmp(1.0, 1, 2.0));
        assert!(!eval_cmp(5.0, 1, 5.0));
    }

    #[test]
    fn cmp_lt() {
        assert!(eval_cmp(1.0, 2, 2.0));
        assert!(!eval_cmp(2.0, 2, 2.0));
        assert!(!eval_cmp(3.0, 2, 2.0));
    }

    #[test]
    fn cmp_le() {
        assert!(eval_cmp(1.0, 3, 2.0));
        assert!(eval_cmp(2.0, 3, 2.0));
        assert!(!eval_cmp(3.0, 3, 2.0));
    }

    #[test]
    fn cmp_gt() {
        assert!(eval_cmp(3.0, 4, 2.0));
        assert!(!eval_cmp(2.0, 4, 2.0));
    }

    #[test]
    fn cmp_ge() {
        assert!(eval_cmp(3.0, 5, 2.0));
        assert!(eval_cmp(2.0, 5, 2.0));
        assert!(!eval_cmp(1.0, 5, 2.0));
    }

    #[test]
    fn cmp_unknown_passes() {
        assert!(eval_cmp(999.0, 99, 0.0));
    }

    // -- AggAccum ---------------------------------------------------------------

    #[test]
    fn accum_sum() {
        let mut a = AggAccum::new(AggOp::Sum);
        a.accumulate(10.0);
        a.accumulate(20.0);
        a.accumulate(30.0);
        assert!((a.result() - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_count() {
        let mut a = AggAccum::new(AggOp::Count);
        a.accumulate(1.0);
        a.accumulate(2.0);
        assert!((a.result() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_avg() {
        let mut a = AggAccum::new(AggOp::Avg);
        a.accumulate(10.0);
        a.accumulate(20.0);
        assert!((a.result() - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_avg_empty() {
        let a = AggAccum::new(AggOp::Avg);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_min() {
        let mut a = AggAccum::new(AggOp::Min);
        a.accumulate(30.0);
        a.accumulate(10.0);
        a.accumulate(20.0);
        assert!((a.result() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_min_empty() {
        let a = AggAccum::new(AggOp::Min);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_max() {
        let mut a = AggAccum::new(AggOp::Max);
        a.accumulate(10.0);
        a.accumulate(30.0);
        a.accumulate(20.0);
        assert!((a.result() - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_max_empty() {
        let a = AggAccum::new(AggOp::Max);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_passthrough() {
        let a = AggAccum::new(AggOp::Passthrough);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    // -- DimHashTable::probe ----------------------------------------------------

    fn make_dim_col(values: &[i64]) -> DimColumn {
        DimColumn {
            type_oid: pgrx::pg_sys::INT8OID,
            values_i64: values.to_vec(),
            values_f64: Vec::new(),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: vec![false; values.len()],
        }
    }

    fn make_dim_col_with_nulls(values: &[i64], nulls: &[bool]) -> DimColumn {
        DimColumn {
            type_oid: pgrx::pg_sys::INT8OID,
            values_i64: values.to_vec(),
            values_f64: Vec::new(),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: nulls.to_vec(),
        }
    }

    #[test]
    fn probe_no_filters() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[100, 200, 300])],
            dim_filters: vec![],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(10, 0);
        ht.hash_table.insert(20, 1);
        ht.hash_table.insert(30, 2);

        assert_eq!(ht.probe(10), Some(0));
        assert_eq!(ht.probe(20), Some(1));
        assert_eq!(ht.probe(30), Some(2));
        assert_eq!(ht.probe(99), None);
    }

    #[test]
    fn probe_with_eq_filter_pass() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 2,
            columns: vec![make_dim_col(&[1993, 1994])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0, // EQ
                const_val: 1993.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);

        assert_eq!(ht.probe(1), Some(0)); // 1993 == 1993 → pass
        assert_eq!(ht.probe(2), None); // 1994 != 1993 → fail
    }

    #[test]
    fn probe_with_ge_filter() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[10, 20, 30])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 5, // GE
                const_val: 20.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);
        ht.hash_table.insert(3, 2);

        assert_eq!(ht.probe(1), None); // 10 < 20 → fail
        assert_eq!(ht.probe(2), Some(1)); // 20 >= 20 → pass
        assert_eq!(ht.probe(3), Some(2)); // 30 >= 20 → pass
    }

    #[test]
    fn probe_null_fails_filter() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 2,
            columns: vec![make_dim_col_with_nulls(&[1993, 0], &[false, true])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0, // EQ
                const_val: 1993.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);

        assert_eq!(ht.probe(1), Some(0)); // not null, 1993 == 1993
        assert_eq!(ht.probe(2), None); // null → fail
    }

    #[test]
    fn probe_multiple_filters() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[1993, 1993, 1994]), make_dim_col(&[1, 2, 1])],
            dim_filters: vec![
                DimFilter {
                    col_idx: 0,
                    cmp_opcode: 0,
                    const_val: 1993.0,
                }, // year == 1993
                DimFilter {
                    col_idx: 1,
                    cmp_opcode: 3,
                    const_val: 1.0,
                }, // quarter <= 1
            ],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);
        ht.hash_table.insert(3, 2);

        assert_eq!(ht.probe(1), Some(0)); // 1993==1993 && 1<=1 → pass
        assert_eq!(ht.probe(2), None); // 1993==1993 && 2<=1 → fail
        assert_eq!(ht.probe(3), None); // 1994!=1993 → fail
    }

    // -- JoinDepthDesc / GroupKeyDesc construction -------------------------------

    #[test]
    fn join_depth_desc_clone() {
        let d = JoinDepthDesc {
            outer_attno: 1,
            inner_attno: 1,
            key_type: 0,
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0,
                const_val: 42.0,
            }],
            group_col_attnos: vec![2, 3],
        };
        assert_eq!(d.outer_attno, 1);
        assert_eq!(d.dim_filters.len(), 1);
        assert_eq!(d.group_col_attnos.len(), 2);
    }

    #[test]
    fn group_key_desc_fact_source() {
        let gk = GroupKeyDesc {
            source: 0,
            attno: 5,
            type_oid: pgrx::pg_sys::INT4OID,
        };
        assert_eq!(gk.source, 0); // fact table
    }

    #[test]
    fn group_key_desc_dim_source() {
        let gk = GroupKeyDesc {
            source: 1,
            attno: 2,
            type_oid: pgrx::pg_sys::INT4OID,
        };
        assert!(gk.source > 0); // dimension table
    }
}

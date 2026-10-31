//! Frozen WHERE admission matrix: 24 designated cells and eight sensitivities.
//! This is separate from every released no-WHERE fixture.
use super::{ExpectedResultValue, ResultOracle, Workload};

pub const ROW_SCALES: &[usize] = &[1_000_017, 2_000_017];
pub const NAMES: &[&str] = &[
    "where_int2_count_star",
    "where_int2_count_independent",
    "where_int2_count_identical",
    "where_int4_count_star",
    "where_int4_count_independent",
    "where_int4_count_identical",
    "where_int8_count_star",
    "where_int8_count_independent",
    "where_int8_count_identical",
    "where_h3_count_star",
    "where_h3_count_independent",
    "where_h3_count_identical",
    "where_int8_sparse_control",
    "where_int8_rejected_control",
    "where_h3_sparse_control",
    "where_h3_rejected_control",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Int2,
    Int4,
    Int8,
    H3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Star,
    Independent,
    Identical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    Mixed,
    Sparse,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contract {
    pub name: &'static str,
    pub key: Key,
    pub form: Form,
    pub selection: Selection,
}
impl Contract {
    pub const fn designated(self) -> bool {
        matches!(self.selection, Selection::Mixed)
    }
    pub const fn groups(self) -> usize {
        if self.designated() { 16_388 } else { 64 }
    }
    pub const fn mask_count(self) -> usize {
        if matches!(self.form, Form::Independent) {
            2
        } else {
            1
        }
    }
    pub const fn key_sql(self) -> &'static str {
        match self.key {
            Key::H3 => "public.h3_cell_to_parent(k,3)",
            _ => "k",
        }
    }
}
pub fn contract(name: &str) -> Option<Contract> {
    let index = NAMES.iter().position(|candidate| *candidate == name)?;
    let (key, form, selection) = if index < 12 {
        (
            [Key::Int2, Key::Int4, Key::Int8, Key::H3][index / 3],
            [Form::Star, Form::Independent, Form::Identical][index % 3],
            Selection::Mixed,
        )
    } else {
        (
            if index < 14 { Key::Int8 } else { Key::H3 },
            Form::Star,
            if index % 2 == 0 {
                Selection::Sparse
            } else {
                Selection::Rejected
            },
        )
    };
    Some(Contract {
        name: NAMES[index],
        key,
        form,
        selection,
    })
}

/// Independent arithmetic oracle; slots preserve groups with zero counts.
fn counts(c: Contract, rows: usize) -> (usize, Vec<Option<i64>>) {
    let groups = c.groups();
    let mut counts = vec![None; groups + 1];
    let mut accepted = 0;
    for i in 1..=rows {
        let admitted = i % 13 != 0
            && match c.selection {
                Selection::Mixed => !((i - 1) / groups).is_multiple_of(4),
                Selection::Sparse => i % 101 == 0,
                Selection::Rejected => false,
            };
        if !admitted {
            continue;
        }
        accepted += 1;
        let g = if i > groups && i % 5 == 0 {
            0
        } else {
            (i - 1) % groups
        };
        let key = if i % 97 == 0 { groups } else { g };
        let count = counts[key].get_or_insert(0);
        if match c.form {
            Form::Star => true,
            Form::Independent => i % 7 != 0 && g != 0 && i % 11 != 0 && i % 3 != 0,
            Form::Identical => i % 7 != 0,
        } {
            *count += 1;
        }
    }
    (accepted, counts)
}

pub struct HashCountWhere(pub &'static str);
impl Workload for HashCountWhere {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        "Frozen direct Boolean WHERE COUNT matrix; signed integer/H3 keys, skew, nullable independent/folded FILTER, fixed sensitivity controls"
    }
    fn row_scales(&self) -> &'static [usize] {
        ROW_SCALES
    }
    fn setup_sql(&self, rows: usize) -> Vec<String> {
        assert!(ROW_SCALES.contains(&rows), "frozen WHERE scale");
        let c = contract(self.0).expect("registered WHERE fixture");
        let groups = c.groups();
        let sql_type = match c.key {
            Key::Int2 => "int2",
            Key::Int4 => "int4",
            Key::Int8 => "int8",
            Key::H3 => "h3index",
        };
        let key = match c.key {
            Key::Int2 => "g*3-24582",
            Key::Int4 => "g*200003-1638844582",
            Key::Int8 => "g*100000000003-819400000024582",
            Key::H3 => "(SELECT child FROM bench_hash_where_seeds WHERE ordinal=g)",
        };
        let predicate = match c.selection {
            Selection::Mixed => {
                format!("CASE WHEN i%13=0 THEN NULL::boolean ELSE ((i-1)/{groups})%4<>0 END")
            }
            Selection::Sparse => "CASE WHEN i%13=0 THEN NULL::boolean ELSE i%101=0 END".to_owned(),
            Selection::Rejected => "CASE WHEN i%2=0 THEN false ELSE NULL::boolean END".to_owned(),
        };
        let mut sql = vec![
            "DROP TABLE IF EXISTS bench_hash_where".to_owned(),
            "DROP TABLE IF EXISTS bench_hash_where_seeds".to_owned(),
        ];
        if c.key == Key::H3 {
            sql.push(format!("CREATE TABLE bench_hash_where_seeds AS SELECT row_number() OVER(ORDER BY parent)-1 AS ordinal, parent, public.h3_cell_to_center_child(parent,15) AS child FROM (SELECT DISTINCT child AS parent FROM public.h3_get_res_0_cells() r(cell) CROSS JOIN LATERAL public.h3_cell_to_children(r.cell,3) c(child) ORDER BY parent LIMIT {groups}) seeds"));
            sql.push("ALTER TABLE bench_hash_where_seeds ADD PRIMARY KEY(ordinal)".to_owned());
            sql.push(format!("DO $$ BEGIN IF (SELECT count(*)={groups} AND count(DISTINCT parent)={groups} AND bool_and(public.h3_get_resolution(parent)=3 AND public.h3_get_resolution(child)=15 AND public.h3_cell_to_parent(child,3)=parent) FROM bench_hash_where_seeds) IS DISTINCT FROM true THEN RAISE EXCEPTION 'frozen H3 seed cardinality/roundtrip'; END IF; END $$"));
        }
        sql.push(format!(
            "CREATE TABLE bench_hash_where (k {sql_type}, w boolean, f boolean, vb boolean, vi int4)"
        ));
        sql.push(format!("INSERT INTO bench_hash_where SELECT CASE WHEN i%97=0 THEN NULL::{sql_type} ELSE ({key})::{sql_type} END, {predicate}, CASE WHEN g=0 THEN false WHEN i%11=0 THEN NULL::boolean ELSE i%3<>0 END, CASE WHEN i%7=0 THEN NULL::boolean ELSE i%2=0 END, CASE WHEN i%7=0 THEN NULL::int4 ELSE (i%1000-500)::int4 END FROM (SELECT i, CASE WHEN i>{groups} AND i%5=0 THEN 0 ELSE (i-1)%{groups} END g FROM generate_series(1::bigint,{rows})s(i)) fixture"));
        sql.push("ANALYZE bench_hash_where".to_owned());
        let (accepted, expected) = counts(c, rows);
        let selected_groups = expected.iter().flatten().count();
        let zero_groups = expected.iter().flatten().filter(|n| **n == 0).count();
        let query = self.query_sql();
        sql.push(format!("DO $$ BEGIN IF (SELECT count(*)={rows} AND count(*) FILTER(WHERE w)={accepted} FROM bench_hash_where) IS DISTINCT FROM true THEN RAISE EXCEPTION 'frozen WHERE row population'; END IF; IF (SELECT count(*)={selected_groups} AND count(*) FILTER(WHERE n=0)={zero_groups} FROM ({query}) actual) IS DISTINCT FROM true THEN RAISE EXCEPTION 'frozen WHERE selected/zero group cardinality'; END IF; END $$"));
        sql
    }
    fn query_sql(&self) -> String {
        let c = contract(self.0).expect("registered WHERE fixture");
        let count = match c.form {
            Form::Star => "count(*)",
            Form::Independent => "count(vb) FILTER(WHERE f)",
            Form::Identical => "count(vi) FILTER(WHERE w)",
        };
        format!(
            "SELECT {} AS k, {count} AS n FROM bench_hash_where WHERE w GROUP BY 1",
            c.key_sql()
        )
    }
    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let c = contract(self.0).expect("registered WHERE fixture");
        let (_, counts) = counts(c, rows);
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, COALESCE(sum(n),0)::int8, count(*) FILTER(WHERE n=0)::int8, COALESCE(bool_or(k IS NULL),false) FROM ({}) actual",
                self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(super::usize_to_i64(counts.iter().flatten().count())),
                ExpectedResultValue::I64(counts.iter().flatten().sum()),
                ExpectedResultValue::I64(super::usize_to_i64(
                    counts.iter().flatten().filter(|n| **n == 0).count(),
                )),
                ExpectedResultValue::Bool(counts[c.groups()].is_some()),
            ],
        ))
    }
    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hash_where".to_owned(),
            "DROP TABLE IF EXISTS bench_hash_where_seeds".to_owned(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{Form, HashCountWhere, Key, NAMES, ROW_SCALES, contract, counts};
    use crate::workloads::Workload;
    #[test]
    fn independent_registry_and_thresholds_do_not_relabel_old_contracts() {
        for name in NAMES {
            let c = contract(name).expect("contract");
            assert!(crate::workloads::raw_integer_count::contract(name).is_none());
            assert!(crate::workloads::h3_count_contract(name).is_none());
            let metadata = crate::workloads::workload_metadata(name).expect("registered metadata");
            assert_eq!(metadata.resident_pins.len(), 1);
            assert_eq!(metadata.resident_pins[0].table, "bench_hash_where");
            for rows in ROW_SCALES {
                let gate = crate::workloads::benchmark_threshold_matrix_entry(name, *rows);
                assert_eq!(gate.is_some(), c.designated());
                if let Some(gate) = gate {
                    assert_eq!(
                        gate.expectation,
                        crate::workloads::BenchmarkLaneExpectation::GpuWinner {
                            min_warm_speedup: 1.15
                        }
                    );
                }
            }
        }
        for name in crate::workloads::raw_integer_count::NAMES {
            assert!(contract(name).is_none());
            assert!(
                !crate::workloads::raw_integer_count::RawIntegerCount(name)
                    .query_sql()
                    .contains(" WHERE ")
            );
        }
    }
    #[test]
    fn exact_32_cell_inventory_and_populations() {
        assert_eq!(NAMES.len() * ROW_SCALES.len(), 32);
        assert_eq!(
            NAMES
                .iter()
                .filter(|n| contract(n).expect("contract").designated())
                .count()
                * 2,
            24
        );
        for name in NAMES {
            let c = contract(name).expect("contract");
            for (rows, accepted) in ROW_SCALES.iter().zip([681_055, 1_377_222]) {
                let (actual, result) = counts(c, *rows);
                if c.designated() {
                    assert_eq!(actual, accepted);
                    assert_eq!(result.iter().flatten().count(), 16_389);
                    assert_eq!(
                        result.iter().flatten().filter(|n| **n == 0).count(),
                        usize::from(c.form == Form::Independent)
                    );
                }
                let sql = HashCountWhere(name).setup_sql(*rows).join("\n");
                assert!(sql.contains("frozen WHERE row population"));
                assert!(!sql.contains("SET "));
                assert_eq!(
                    sql.contains("frozen H3 seed cardinality/roundtrip"),
                    c.key == Key::H3
                );
            }
        }
    }
}

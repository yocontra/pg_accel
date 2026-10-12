//! JRS1: pointer-free global SUM/COUNT states over a JQS1 projected batch.
//! Input slots name ordered producer lanes, never fabricated relation OIDs.
//! SQL result projection, grouping and planner admission remain separate.

use super::join::{JoinQuerySpec, JoinSpecError};
use super::scalar::{
    MAX_SCALAR_INPUTS, MAX_SCALAR_INSTRUCTIONS, ScalarInstruction, ScalarProgram, ScalarType,
};

pub const MAX_JOIN_REDUCTIONS: usize = 4;
pub const MAX_REDUCTION_PROGRAM_WORDS: usize = 3 + MAX_SCALAR_INPUTS + 2 * MAX_SCALAR_INSTRUCTIONS;
const MAGIC: i32 = 0x4a52_5331;
const HEADER: usize = 5;
pub const MAX_JOIN_REDUCTION_WORDS: usize =
    HEADER + MAX_JOIN_REDUCTIONS * (2 + MAX_SCALAR_INPUTS + MAX_REDUCTION_PROGRAM_WORDS);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinReduction {
    /// Program input i borrows projected slot inputs[i]. Repeated slots are
    /// legal; their exact SQL types must match the program input declarations.
    pub inputs: Vec<u32>,
    /// An INT4 expression, including a lazy FILTER encoded as a NULL result.
    /// Both SUM and non-NULL COUNT states are produced. Constant 1 gives COUNT(*).
    pub program: ScalarProgram,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRowFilter {
    pub inputs: Vec<u32>,
    /// Post-join SQL WHERE, not an ON condition: only TRUE reaches measures.
    /// LEFT padding is already present and participates as SQL NULL inputs.
    pub program: ScalarProgram,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinReductionSpec {
    pub measures: Vec<JoinReduction>,
    /// Uses one of the four physical scalar lanes; no materialized host mask.
    pub row_filter: Option<JoinRowFilter>,
}

#[cfg_attr(not(any(test, feature = "pg_test")), allow(dead_code))] // Staged consumer wire, not a selected SQL pipeline.
impl JoinReductionSpec {
    /// Exact multiplicity identity, not host constant folding. Every requested
    /// state is SUM(1)/COUNT(1), with no WHERE or expression that can fail.
    pub(crate) fn is_cardinality_only(&self) -> bool {
        self.row_filter.is_none()
            && self.validate_shape().is_ok()
            && self.measures.iter().all(|measure| {
                measure.inputs.is_empty()
                    && measure.program.instructions() == [ScalarInstruction::Int4(1)]
            })
    }

    pub(crate) fn programs(&self) -> impl Iterator<Item = (&[u32], &ScalarProgram, ScalarType)> {
        self.measures
            .iter()
            .map(|measure| {
                (
                    measure.inputs.as_slice(),
                    &measure.program,
                    ScalarType::Int4,
                )
            })
            .chain(
                self.row_filter
                    .iter()
                    .map(|filter| (filter.inputs.as_slice(), &filter.program, ScalarType::Bool)),
            )
    }

    fn validate_shape(&self) -> Result<(), JoinSpecError> {
        if self.measures.is_empty()
            || self.measures.len() + usize::from(self.row_filter.is_some()) > MAX_JOIN_REDUCTIONS
        {
            return Err(JoinSpecError(
                "join reduction requires measures and at most four physical lanes",
            ));
        }
        for (inputs, program, result_type) in self.programs() {
            if inputs.len() != program.inputs().len()
                || inputs.len() > MAX_SCALAR_INPUTS
                || inputs
                    .iter()
                    .any(|&slot| slot >= super::join::MAX_JOIN_COLUMNS as u32)
                || program.result_type() != result_type
                || program
                    .instructions()
                    .contains(&ScalarInstruction::PredicateMask)
            {
                return Err(JoinSpecError(
                    "invalid join reduction program or input slots",
                ));
            }
        }
        Ok(())
    }

    pub fn validate(&self, join: &JoinQuerySpec) -> Result<(), JoinSpecError> {
        join.validate()?;
        self.validate_columns(&join.columns)
    }

    pub(crate) fn validate_columns(
        &self,
        columns: &[super::join::OccurrenceColumn],
    ) -> Result<(), JoinSpecError> {
        self.validate_shape()?;
        if !(1..=super::join::MAX_JOIN_COLUMNS).contains(&columns.len()) {
            return Err(JoinSpecError("invalid join reduction producer width"));
        }
        for (inputs, program, _) in self.programs() {
            for (&slot, &expected) in inputs.iter().zip(program.inputs()) {
                let column = columns.get(slot as usize).ok_or(JoinSpecError(
                    "join reduction slot is outside producer schema",
                ))?;
                let actual = match column.type_oid {
                    16 => ScalarType::Bool,
                    23 => ScalarType::Int4,
                    1082 => ScalarType::Date,
                    _ => return Err(JoinSpecError("unsupported join reduction input type")),
                };
                if actual != expected {
                    return Err(JoinSpecError("join reduction input SQL type mismatch"));
                }
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<i32>, JoinSpecError> {
        self.validate_shape()?;
        let len = HEADER
            + self
                .programs()
                .map(|(inputs, program, _)| 2 + inputs.len() + program.encoded_word_len())
                .sum::<usize>();
        let mut words = Vec::with_capacity(len);
        words.extend([
            MAGIC,
            2,
            len as i32,
            self.measures.len() as i32,
            i32::from(self.row_filter.is_some()),
        ]);
        for (inputs, program, _) in self.programs() {
            words.extend([inputs.len() as i32, program.encoded_word_len() as i32]);
            words.extend(inputs.iter().map(|&slot| slot as i32));
            let start = words.len();
            words.resize(start + program.encoded_word_len(), 0);
            program
                .encode_into(&mut words[start..])
                .map_err(|_| JoinSpecError("invalid scalar wire"))?;
        }
        Ok(words)
    }

    pub fn decode(words: &[i32]) -> Result<Self, JoinSpecError> {
        if !(4..=MAX_JOIN_REDUCTION_WORDS).contains(&words.len())
            || words[0] != MAGIC
            || !matches!(words[1], 1 | 2)
            || words[2] != words.len() as i32
            || !(1..=MAX_JOIN_REDUCTIONS as i32).contains(&words[3])
        {
            return Err(JoinSpecError("invalid join reduction header"));
        }
        // v1 has no WHERE field. Retain strict decoding for already-written
        // staged plans; v2 explicitly tags the optional trailing BOOL program.
        let has_filter = if words[1] == 2 {
            match words.get(4) {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(JoinSpecError("invalid join row filter tag")),
            }
        } else {
            false
        };
        let measure_count = words[3] as usize;
        let program_count = measure_count + usize::from(has_filter);
        if program_count > MAX_JOIN_REDUCTIONS {
            return Err(JoinSpecError("too many join reduction physical lanes"));
        }
        // Prove all section lengths, slot bounds and SPG1 programs before any
        // owned decode allocations. Reject truncation, trailing words and tags.
        let mut sections = [(0, 0, 0); MAX_JOIN_REDUCTIONS];
        let mut cursor = if words[1] == 1 { 4 } else { HEADER };
        for (index, section) in sections[..program_count].iter_mut().enumerate() {
            let header = words
                .get(cursor..cursor + 2)
                .ok_or(JoinSpecError("truncated reduction section"))?;
            if !(0..=MAX_SCALAR_INPUTS as i32).contains(&header[0])
                || !(3..=MAX_REDUCTION_PROGRAM_WORDS as i32).contains(&header[1])
            {
                return Err(JoinSpecError("invalid reduction section counts"));
            }
            let slots = header[0] as usize;
            let program_words = header[1] as usize;
            let start = cursor + 2;
            let end = start + slots + program_words;
            let body = words
                .get(start..end)
                .ok_or(JoinSpecError("truncated reduction program"))?;
            if body[..slots]
                .iter()
                .any(|&slot| !(0..super::join::MAX_JOIN_COLUMNS as i32).contains(&slot))
            {
                return Err(JoinSpecError("invalid reduction slot"));
            }
            let program = &body[slots..];
            let proof = ScalarProgram::validate_wire(program)
                .map_err(|_| JoinSpecError("invalid reduction scalar program"))?;
            if program[2] != slots as i32
                || proof.result_type()
                    != if index < measure_count {
                        ScalarType::Int4
                    } else {
                        ScalarType::Bool
                    }
                || proof.uses_predicate_mask()
            {
                return Err(JoinSpecError("invalid reduction scalar signature"));
            }
            *section = (start, slots, end);
            cursor = end;
        }
        if cursor != words.len() {
            return Err(JoinSpecError("trailing reduction words"));
        }
        let mut measures = Vec::with_capacity(measure_count);
        let mut row_filter = None;
        for (index, &(start, slots, end)) in sections[..program_count].iter().enumerate() {
            let inputs = words[start..start + slots]
                .iter()
                .map(|&slot| slot as u32)
                .collect();
            let program = ScalarProgram::decode(&words[start + slots..end])
                .map_err(|_| JoinSpecError("invalid reduction scalar program"))?;
            if index < measure_count {
                measures.push(JoinReduction { inputs, program });
            } else {
                row_filter = Some(JoinRowFilter { inputs, program });
            }
        }
        Ok(Self {
            measures,
            row_filter,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::spec::join::{JoinKey, JoinKind, OccurrenceColumn, RelationOccurrence};
    use crate::engine::spec::scalar::{
        ScalarInstruction as I,
        ScalarType::{Bool, Date, Int4},
    };

    fn producer() -> JoinQuerySpec {
        let col = |occurrence, attno, type_oid| OccurrenceColumn {
            occurrence,
            attno,
            type_oid,
        };
        JoinQuerySpec {
            outer: RelationOccurrence {
                id: 3,
                relation_oid: 42,
            },
            inner: RelationOccurrence {
                id: 9,
                relation_oid: 43,
            },
            kind: JoinKind::Left,
            keys: vec![JoinKey {
                outer: col(3, 2, 23),
                inner: col(9, 2, 23),
            }],
            columns: vec![
                col(3, 2, 23),
                col(9, 2, 23),
                col(3, 3, 16),
                col(9, 3, 1082),
                col(3, 4, 20),
            ],
        }
    }

    fn simple() -> JoinReductionSpec {
        JoinReductionSpec {
            row_filter: None,
            measures: vec![JoinReduction {
                inputs: vec![0, 1],
                program: ScalarProgram::new(
                    vec![Int4, Int4],
                    Int4,
                    vec![I::Input(0), I::Input(1), I::CoalesceInt4(10), I::Add],
                )
                .expect("program"),
            }],
        }
    }

    fn filtered() -> JoinReductionSpec {
        let mut spec = simple();
        spec.row_filter = Some(JoinRowFilter {
            inputs: vec![2],
            program: ScalarProgram::new(vec![Bool], Bool, vec![I::Input(0)]).expect("WHERE"),
        });
        spec
    }

    #[test]
    fn cardinality_specialization_accepts_only_exact_unfiltered_sum_one_states() {
        let one = JoinReduction {
            inputs: vec![],
            program: ScalarProgram::new(vec![], Int4, vec![I::Int4(1)]).expect("one"),
        };
        let mut spec = JoinReductionSpec {
            measures: vec![one.clone()],
            row_filter: None,
        };
        for count in 0..=MAX_JOIN_REDUCTIONS + 1 {
            spec.measures = vec![one.clone(); count];
            assert_eq!(
                spec.is_cardinality_only(),
                (1..=MAX_JOIN_REDUCTIONS).contains(&count)
            );
        }
        spec.measures = vec![one.clone()];
        for code in [
            vec![I::Int4(0)],
            vec![I::Int4(-1)],
            vec![I::Null(Int4)],
            vec![I::Int4(1), I::Int4(0), I::Add],
            vec![I::Int4(1), I::Int4(0), I::Div],
        ] {
            spec.measures[0].program = ScalarProgram::new(vec![], Int4, code).expect("nonidentity");
            assert!(!spec.is_cardinality_only());
        }
        spec.measures[0] = one;
        for predicate in [I::Bool(true), I::Bool(false), I::Null(Bool)] {
            spec.row_filter = Some(JoinRowFilter {
                inputs: vec![],
                program: ScalarProgram::new(vec![], Bool, vec![predicate]).expect("WHERE"),
            });
            assert!(
                !spec.is_cardinality_only(),
                "WHERE must use its own evaluator"
            );
        }
        spec.row_filter = None;
        spec.measures[0].inputs = vec![0];
        spec.measures[0].program =
            ScalarProgram::new(vec![Int4], Int4, vec![I::Int4(1)]).expect("declared binding");
        assert!(!spec.is_cardinality_only());
        spec.measures[0].inputs.clear();
        assert!(
            !spec.is_cardinality_only(),
            "invalid binding cannot specialize"
        );
    }

    #[test]
    fn row_filter_wire_retains_typed_where_and_accepts_legacy_unfiltered_v1() {
        let spec = filtered();
        spec.validate(&producer()).expect("typed WHERE");
        let wire = spec.encode().expect("v2 wire");
        assert_eq!(&wire[..5], &[MAGIC, 2, wire.len() as i32, 1, 1]);
        assert_eq!(JoinReductionSpec::decode(&wire).expect("v2 decode"), spec);
        let mut legacy = simple().encode().expect("unfiltered");
        legacy.remove(4);
        legacy[1] = 1;
        legacy[2] -= 1;
        assert_eq!(
            JoinReductionSpec::decode(&legacy).expect("v1 decode"),
            simple()
        );
        // A legacy reader must never interpret a v2 filter as a measure.
        let mut wrong_version = wire;
        wrong_version[1] = 1;
        assert!(JoinReductionSpec::decode(&wrong_version).is_err());
    }

    #[test]
    fn row_filter_rejects_nonboolean_external_context_and_wrong_producer_types() {
        let mut spec = filtered();
        spec.measures = vec![simple().measures[0].clone(); MAX_JOIN_REDUCTIONS - 1];
        spec.validate(&producer())
            .expect("three measures plus WHERE");
        assert_eq!(
            JoinReductionSpec::decode(&spec.encode().expect("max lanes")).expect("decode"),
            spec
        );
        spec.measures.push(simple().measures[0].clone());
        assert!(spec.encode().is_err());
        spec = filtered();
        for slot in [0, 3, 4, 5, u32::MAX] {
            spec.row_filter.as_mut().expect("filter").inputs[0] = slot;
            assert!(spec.validate(&producer()).is_err());
        }
        spec = filtered();
        spec.row_filter.as_mut().expect("filter").program = simple().measures[0].program.clone();
        assert!(spec.encode().is_err());
        spec.row_filter = Some(JoinRowFilter {
            inputs: vec![],
            program: ScalarProgram::new(vec![], Bool, vec![I::PredicateMask]).expect("mask"),
        });
        assert!(spec.encode().is_err());
        spec.row_filter = Some(JoinRowFilter {
            inputs: vec![],
            program: ScalarProgram::new(vec![], Bool, vec![I::Null(Bool)]).expect("unknown"),
        });
        spec.validate(&producer())
            .expect("constant NULL WHERE is legal");
    }

    #[test]
    fn row_filter_wire_rejects_malformed_trailing_program_before_owned_decode() {
        let wire = filtered().encode().expect("wire");
        for len in 0..wire.len() {
            let mut truncated = wire[..len].to_vec();
            if len >= 3 {
                truncated[2] = len as i32;
            }
            assert!(JoinReductionSpec::decode(&truncated).is_err());
        }
        let start = simple().encode().expect("measure wire").len();
        for (offset, value) in [
            (0, -1),
            (0, 65),
            (1, -1),
            (1, 580),
            (2, -1),
            (2, 16),
            (3, 0),
        ] {
            let mut bad = wire.clone();
            bad[start + offset] = value;
            assert!(
                JoinReductionSpec::decode(&bad).is_err(),
                "filter field {offset}"
            );
        }
        for tag in [-1, 0, 2, i32::MAX] {
            let mut bad = wire.clone();
            bad[4] = tag;
            assert!(JoinReductionSpec::decode(&bad).is_err());
        }
        let mut bad_signature = simple().encode().expect("INT4 measure");
        // Append an otherwise valid INT4 program in the BOOL filter position.
        let section = bad_signature[HEADER..].to_vec();
        bad_signature.extend(section);
        bad_signature[2] = bad_signature.len() as i32;
        bad_signature[4] = 1;
        assert!(JoinReductionSpec::decode(&bad_signature).is_err());
    }

    #[test]
    fn reduction_wire_roundtrip_preserves_slots_aliases_and_programs() {
        let mut spec = simple();
        spec.measures.push(JoinReduction {
            inputs: vec![],
            program: ScalarProgram::new(vec![], Int4, vec![I::Int4(1)]).expect("count star"),
        });
        spec.measures.push(JoinReduction {
            inputs: vec![3],
            program: ScalarProgram::new(
                vec![Date],
                Int4,
                vec![I::Input(0), I::Date(0), I::DateDifference],
            )
            .expect("date offset"),
        });
        spec.measures.push(JoinReduction {
            inputs: vec![2],
            program: ScalarProgram::new(
                vec![Bool],
                Int4,
                vec![
                    I::Input(0),
                    I::JumpIfNotTrue(4),
                    I::Int4(1),
                    I::Jump(5),
                    I::Null(Int4),
                ],
            )
            .expect("bool filter"),
        });
        spec.validate(&producer()).expect("typed producer");
        assert_eq!(
            JoinReductionSpec::decode(&spec.encode().expect("encode")).expect("decode"),
            spec
        );
        let mut duplicate = simple();
        duplicate.measures[0].inputs = vec![1, 1];
        duplicate
            .validate(&producer())
            .expect("repeated slots remain meaningful");
        assert_ne!(
            duplicate.encode().expect("duplicate"),
            simple().encode().expect("distinct")
        );
    }

    #[test]
    fn reduction_rejects_wrong_types_slots_predicate_context_and_counts() {
        let mut spec = simple();
        for slot in [2, 3, 4, 5, u32::MAX] {
            spec.measures[0].inputs[0] = slot;
            assert!(spec.validate(&producer()).is_err());
        }
        spec = simple();
        spec.measures[0].inputs.pop();
        assert!(spec.encode().is_err());
        spec = simple();
        spec.measures.clear();
        assert!(spec.encode().is_err());
        spec.measures = vec![simple().measures[0].clone(); 5];
        assert!(spec.encode().is_err());
        spec.measures = vec![JoinReduction {
            inputs: vec![],
            program: ScalarProgram::new(vec![], Bool, vec![I::Bool(true)]).expect("bool"),
        }];
        assert!(spec.encode().is_err());
        spec.measures[0].program = ScalarProgram::new(
            vec![],
            Int4,
            vec![
                I::PredicateMask,
                I::JumpIfNotTrue(4),
                I::Int4(1),
                I::Jump(5),
                I::Null(Int4),
            ],
        )
        .expect("mask needs external context");
        assert!(spec.encode().is_err());
        let mut bad_join = producer();
        bad_join.inner.id = bad_join.outer.id;
        assert!(simple().validate(&bad_join).is_err());
        assert!(simple().validate_columns(&[]).is_err());
    }

    #[test]
    fn reduction_decode_rejects_truncation_counts_tags_and_trailing_words() {
        let wire = simple().encode().expect("wire");
        for len in 0..wire.len() {
            assert!(JoinReductionSpec::decode(&wire[..len]).is_err());
        }
        for (index, value) in [
            (0, 0),
            (1, 3),
            (2, -1),
            (3, 0),
            (3, 5),
            (4, -1),
            (4, 2),
            (5, -1),
            (5, i32::MAX),
            (6, -1),
            (6, i32::MAX),
            (7, -1),
            (7, 16),
            (9, 0),
            (11, 1),
        ] {
            let mut invalid = wire.clone();
            invalid[index] = value;
            assert!(
                JoinReductionSpec::decode(&invalid).is_err(),
                "word {index}={value}"
            );
        }
        let mut trailing = wire;
        trailing.push(0);
        trailing[2] += 1;
        assert!(JoinReductionSpec::decode(&trailing).is_err());
    }

    #[test]
    fn reduction_wire_maximum_and_scalar_inline_encoding_are_exact() {
        let constant = ScalarProgram::new(vec![], Int4, vec![I::Int4(1)]).expect("constant");
        let mut fixed = [0; 5];
        constant.encode_into(&mut fixed).expect("fixed wire");
        assert_eq!(
            fixed,
            [0x5350_4731, 2, 0, 1, 1],
            "SPG1 wire remains byte-for-byte compatible"
        );
        let mut code = vec![I::Input(0)];
        for _ in 0..127 {
            code.extend([I::Dup, I::Drop]);
        }
        code.push(I::Neg);
        let program =
            ScalarProgram::new(vec![Int4; MAX_SCALAR_INPUTS], Int4, code).expect("max program");
        assert_eq!(program.encoded_word_len(), MAX_REDUCTION_PROGRAM_WORDS);
        let mut words = [0; MAX_REDUCTION_PROGRAM_WORDS];
        program.encode_into(&mut words).expect("inline encode");
        assert_eq!(words.as_slice(), program.encode());
        let mut short = [97; 2];
        assert!(program.encode_into(&mut short).is_err());
        assert_eq!(short, [97; 2]);
        let spec = JoinReductionSpec {
            row_filter: None,
            measures: vec![
                JoinReduction {
                    inputs: vec![0; MAX_SCALAR_INPUTS],
                    program
                };
                MAX_JOIN_REDUCTIONS
            ],
        };
        spec.validate(&producer()).expect("max schema");
        let wire = spec.encode().expect("max wire");
        assert_eq!(wire.len(), MAX_JOIN_REDUCTION_WORDS);
        assert_eq!(
            JoinReductionSpec::decode(&wire).expect("max roundtrip"),
            spec
        );
        let mut filtered = spec;
        filtered.measures.pop();
        let mut code = vec![I::Input(0)];
        for _ in 0..127 {
            code.extend([I::Dup, I::Drop]);
        }
        code.push(I::Not);
        filtered.row_filter = Some(JoinRowFilter {
            inputs: vec![2; MAX_SCALAR_INPUTS],
            program: ScalarProgram::new(vec![Bool; MAX_SCALAR_INPUTS], Bool, code)
                .expect("max BOOL filter"),
        });
        filtered.validate(&producer()).expect("max filtered schema");
        let wire = filtered.encode().expect("max filtered wire");
        assert_eq!(wire.len(), MAX_JOIN_REDUCTION_WORDS);
        assert_eq!(
            JoinReductionSpec::decode(&wire).expect("max filtered decode"),
            filtered
        );
        assert_eq!(
            MAX_JOIN_REDUCTIONS,
            crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        );
        assert_eq!(
            MAX_REDUCTION_PROGRAM_WORDS,
            crate::engine::spec::abi::PGACCEL_SCALAR_MAX_WORDS
        );
    }
}

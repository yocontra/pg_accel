//! Strict output projection contract for neutral aggregate plans.

use super::{
    AggQuerySpec, AggregateKind, AggregateOutput, AggregateSource, MeasureExpr, MeasureSpec,
};

const BOOL_OID: u32 = 16;
const INT2_OID: u32 = 21;
const INT4_OID: u32 = 23;
const INT8_OID: u32 = 20;
const FLOAT4_OID: u32 = 700;
const FLOAT8_OID: u32 = 701;
const DATE_OID: u32 = 1082;
const TIMESTAMP_OID: u32 = 1114;
const TIMESTAMPTZ_OID: u32 = 1184;
const NUMERIC_OID: u32 = 1700;

pub const AGG_OUTPUT_PROJECTION_WIRE_MAGIC: i32 = 0x5047_4F32; // "PGO2"
pub const AGG_OUTPUT_PROJECTION_VERSION: u32 = 2;
pub const AGG_OUTPUT_PROJECTION_HEADER_WORDS: usize = 4;
pub const AGG_OUTPUT_PROJECTION_SLOT_WORDS: usize = 9;
pub const MAX_AGG_OUTPUT_PROJECTION_SLOTS: usize = 1_664;
pub const AGG_OUTPUT_PROJECTION_MAX_WORDS: usize = AGG_OUTPUT_PROJECTION_HEADER_WORDS
    + MAX_AGG_OUTPUT_PROJECTION_SLOTS * AGG_OUTPUT_PROJECTION_SLOT_WORDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggOutputSource {
    GroupKey {
        key_index: u32,
    },
    Aggregate {
        measure_index: u32,
        source: AggregateSource,
        kind: AggregateKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggOutputSlot {
    /// Logical source in the associated [`AggQuerySpec`].
    pub source: AggOutputSource,
    /// PostgreSQL type of the source value before aggregate finalization.
    /// Invalid OID is canonical only for `COUNT(*)`.
    pub source_type_oid: u32,
    /// Expected PostgreSQL result type; revalidated against plan/slot metadata.
    pub result_type_oid: u32,
    /// Expected PostgreSQL typmod (`-1` means unspecified).
    pub result_typmod: i32,
    /// Expected PostgreSQL collation, or invalid OID for noncollatable types.
    pub result_collation_oid: u32,
    /// Expected SQL nullability. COUNT is false; other aggregates are true.
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggOutputProjection {
    /// Output slots in exact PostgreSQL target-list order.
    pub slots: Vec<AggOutputSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionCodecError {
    InvalidProjection(&'static str),
    Truncated {
        index: usize,
        field: &'static str,
    },
    InvalidMagic(i32),
    UnsupportedVersion(u32),
    InvalidLength {
        declared: usize,
        actual: usize,
    },
    InvalidTag {
        index: usize,
        field: &'static str,
        raw: i32,
    },
    InvalidValue {
        index: usize,
        field: &'static str,
        raw: i32,
    },
    LimitExceeded {
        index: usize,
        field: &'static str,
        declared: usize,
        maximum: usize,
    },
    NonCanonical {
        index: usize,
    },
    AllocationFailed,
    LengthOverflow,
}

impl std::fmt::Display for ProjectionCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProjection(reason) => {
                write!(f, "invalid aggregate output projection: {reason}")
            }
            Self::Truncated { index, field } => {
                write!(
                    f,
                    "truncated aggregate output projection at word {index} while reading {field}"
                )
            }
            Self::InvalidMagic(raw) => {
                write!(f, "invalid aggregate output projection magic {raw:#x}")
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "unsupported aggregate output projection version {version}"
                )
            }
            Self::InvalidLength { declared, actual } => write!(
                f,
                "aggregate output projection length is {actual}, header declares {declared}"
            ),
            Self::InvalidTag { index, field, raw } => {
                write!(f, "invalid {field} tag {raw} at word {index}")
            }
            Self::InvalidValue { index, field, raw } => {
                write!(f, "invalid {field} value {raw} at word {index}")
            }
            Self::LimitExceeded {
                index,
                field,
                declared,
                maximum,
            } => write!(
                f,
                "{field} {declared} at word {index} exceeds schema maximum {maximum}"
            ),
            Self::NonCanonical { index } => write!(
                f,
                "aggregate output projection is not canonically encoded at word {index}"
            ),
            Self::AllocationFailed => f.write_str("could not allocate aggregate output projection"),
            Self::LengthOverflow => f.write_str("aggregate output projection length overflows i32"),
        }
    }
}

impl std::error::Error for ProjectionCodecError {}

fn measure_source_type(
    measure: &MeasureSpec,
    source: AggregateSource,
) -> Result<u32, ProjectionCodecError> {
    let invalid =
        || ProjectionCodecError::InvalidProjection("aggregate source type cannot be proven");
    match (&measure.expression, source) {
        (MeasureExpr::CountStar, AggregateSource::Value) => Ok(0),
        (MeasureExpr::Column(column), AggregateSource::Value) => Ok(column.type_oid),
        (MeasureExpr::Binary { lhs, rhs, .. }, AggregateSource::Value)
            if lhs.type_oid == rhs.type_oid
                && matches!(lhs.type_oid, INT4_OID | INT8_OID | FLOAT8_OID) =>
        {
            Ok(lhs.type_oid)
        }
        (MeasureExpr::StatsPair { value, .. }, AggregateSource::Value) => Ok(value.type_oid),
        (MeasureExpr::StatsPair { rhs, .. }, AggregateSource::Rhs) => Ok(rhs.type_oid),
        (
            MeasureExpr::Bytecode {
                result_type_oid, ..
            },
            AggregateSource::Value,
        ) => Ok(*result_type_oid),
        _ => Err(invalid()),
    }
}

fn aggregate_result_type(source_type_oid: u32, kind: AggregateKind) -> Option<u32> {
    match (source_type_oid, kind) {
        (0, AggregateKind::Count) => Some(INT8_OID),
        (
            BOOL_OID | INT2_OID | INT4_OID | INT8_OID | FLOAT4_OID | FLOAT8_OID | DATE_OID
            | TIMESTAMP_OID | TIMESTAMPTZ_OID,
            AggregateKind::Count,
        ) => Some(INT8_OID),
        (INT2_OID | INT4_OID, AggregateKind::Sum) => Some(INT8_OID),
        (INT2_OID | INT4_OID, AggregateKind::Avg) => Some(NUMERIC_OID),
        (FLOAT8_OID, AggregateKind::Sum | AggregateKind::Avg | AggregateKind::StddevSamp) => {
            Some(FLOAT8_OID)
        }
        (
            source @ (INT2_OID | INT4_OID | INT8_OID | FLOAT4_OID | FLOAT8_OID | DATE_OID
            | TIMESTAMP_OID | TIMESTAMPTZ_OID),
            AggregateKind::Min | AggregateKind::Max,
        ) => Some(source),
        _ => None,
    }
}

impl AggOutputProjection {
    pub fn validate(&self, spec: &AggQuerySpec) -> Result<(), ProjectionCodecError> {
        spec.validate()
            .map_err(|_| ProjectionCodecError::InvalidProjection("query spec is invalid"))?;
        if self.slots.is_empty() {
            return Err(ProjectionCodecError::InvalidProjection(
                "projection has no output slots",
            ));
        }
        if self.slots.len() > MAX_AGG_OUTPUT_PROJECTION_SLOTS {
            return Err(ProjectionCodecError::LimitExceeded {
                index: AGG_OUTPUT_PROJECTION_HEADER_WORDS - 1,
                field: "output slot count",
                declared: self.slots.len(),
                maximum: MAX_AGG_OUTPUT_PROJECTION_SLOTS,
            });
        }
        for slot in &self.slots {
            if slot.result_type_oid == 0 {
                return Err(ProjectionCodecError::InvalidProjection(
                    "output slot has invalid result type OID",
                ));
            }
            if slot.result_typmod < -1 {
                return Err(ProjectionCodecError::InvalidProjection(
                    "output slot has invalid typmod",
                ));
            }
            match slot.source {
                AggOutputSource::GroupKey { key_index } => {
                    let Ok(index) = usize::try_from(key_index) else {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "output references a missing group key",
                        ));
                    };
                    if index >= spec.group_keys.len() {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "output references a missing group key",
                        ));
                    }
                    if slot.source_type_oid == 0 || slot.result_type_oid != slot.source_type_oid {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "group-key source/result type OIDs must be equal and nonzero",
                        ));
                    }
                    if spec.group_keys[index].type_oid != slot.source_type_oid {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "group-key source type does not match query spec",
                        ));
                    }
                    if spec.group_keys[index].collation_oid != slot.result_collation_oid {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "group-key result collation does not match query spec",
                        ));
                    }
                }
                AggOutputSource::Aggregate {
                    measure_index,
                    source,
                    kind,
                } => {
                    let Ok(index) = usize::try_from(measure_index) else {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "output references a missing measure",
                        ));
                    };
                    let Some(measure) = spec.measures.get(index) else {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "output references a missing measure",
                        ));
                    };
                    if !measure.outputs.contains(&AggregateOutput { source, kind }) {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "output references an aggregate lane not projected by its measure",
                        ));
                    }
                    let derived_source = measure_source_type(measure, source)?;
                    if slot.source_type_oid != derived_source {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "aggregate source type does not match query spec",
                        ));
                    }
                    let expected_result = aggregate_result_type(derived_source, kind).ok_or(
                        ProjectionCodecError::InvalidProjection(
                            "aggregate source/kind has no canonical result type mapping",
                        ),
                    )?;
                    if slot.result_type_oid != expected_result {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "aggregate result type does not match source/kind semantics",
                        ));
                    }
                    if slot.result_collation_oid != 0 {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "numeric aggregate result must use invalid collation OID",
                        ));
                    }
                    if (kind == AggregateKind::Count) == slot.nullable {
                        return Err(ProjectionCodecError::InvalidProjection(
                            "COUNT must be non-nullable and other aggregates must preserve SQL NULL",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn encode_i32(&self, spec: &AggQuerySpec) -> Result<Vec<i32>, ProjectionCodecError> {
        self.validate(spec)?;
        let length = AGG_OUTPUT_PROJECTION_HEADER_WORDS
            .checked_add(
                self.slots
                    .len()
                    .checked_mul(AGG_OUTPUT_PROJECTION_SLOT_WORDS)
                    .ok_or(ProjectionCodecError::LengthOverflow)?,
            )
            .ok_or(ProjectionCodecError::LengthOverflow)?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(length)
            .map_err(|_| ProjectionCodecError::AllocationFailed)?;
        words.push(AGG_OUTPUT_PROJECTION_WIRE_MAGIC);
        words.push(AGG_OUTPUT_PROJECTION_VERSION as i32);
        words.push(i32::try_from(length).map_err(|_| ProjectionCodecError::LengthOverflow)?);
        words.push(
            i32::try_from(self.slots.len()).map_err(|_| ProjectionCodecError::LengthOverflow)?,
        );
        for slot in &self.slots {
            let (tag, primary_index, aggregate_source, aggregate_kind) = match slot.source {
                AggOutputSource::GroupKey { key_index } => (0, key_index, 0, 0),
                AggOutputSource::Aggregate {
                    measure_index,
                    source,
                    kind,
                } => (
                    1,
                    measure_index,
                    match source {
                        AggregateSource::Value => 0,
                        AggregateSource::Rhs => 1,
                    },
                    match kind {
                        AggregateKind::Sum => 0,
                        AggregateKind::Count => 1,
                        AggregateKind::Min => 2,
                        AggregateKind::Max => 3,
                        AggregateKind::Avg => 4,
                        AggregateKind::StddevSamp => 5,
                    },
                ),
            };
            words.extend([
                tag,
                primary_index as i32,
                aggregate_source,
                aggregate_kind,
                slot.source_type_oid as i32,
                slot.result_type_oid as i32,
                slot.result_typmod,
                slot.result_collation_oid as i32,
                i32::from(slot.nullable),
            ]);
        }
        Ok(words)
    }

    #[allow(clippy::too_many_lines)] // fixed slot grammar is clearer as one strict decoder
    pub fn decode_i32(words: &[i32], spec: &AggQuerySpec) -> Result<Self, ProjectionCodecError> {
        let read = |index: usize, field: &'static str| {
            words
                .get(index)
                .copied()
                .ok_or(ProjectionCodecError::Truncated { index, field })
        };
        let magic = read(0, "wire magic")?;
        if magic != AGG_OUTPUT_PROJECTION_WIRE_MAGIC {
            return Err(ProjectionCodecError::InvalidMagic(magic));
        }
        let version = read(1, "wire version")? as u32;
        if version != AGG_OUTPUT_PROJECTION_VERSION {
            return Err(ProjectionCodecError::UnsupportedVersion(version));
        }
        let declared_raw = read(2, "wire length")?;
        let declared =
            usize::try_from(declared_raw).map_err(|_| ProjectionCodecError::InvalidValue {
                index: 2,
                field: "wire length",
                raw: declared_raw,
            })?;
        if declared != words.len() {
            return Err(ProjectionCodecError::InvalidLength {
                declared,
                actual: words.len(),
            });
        }
        if declared > AGG_OUTPUT_PROJECTION_MAX_WORDS {
            return Err(ProjectionCodecError::LimitExceeded {
                index: 2,
                field: "wire length",
                declared,
                maximum: AGG_OUTPUT_PROJECTION_MAX_WORDS,
            });
        }
        let count_raw = read(3, "output slot count")?;
        let count = usize::try_from(count_raw).map_err(|_| ProjectionCodecError::InvalidValue {
            index: 3,
            field: "output slot count",
            raw: count_raw,
        })?;
        if count == 0 || count > MAX_AGG_OUTPUT_PROJECTION_SLOTS {
            return Err(ProjectionCodecError::LimitExceeded {
                index: 3,
                field: "output slot count",
                declared: count,
                maximum: MAX_AGG_OUTPUT_PROJECTION_SLOTS,
            });
        }
        let expected = AGG_OUTPUT_PROJECTION_HEADER_WORDS
            .checked_add(
                count
                    .checked_mul(AGG_OUTPUT_PROJECTION_SLOT_WORDS)
                    .ok_or(ProjectionCodecError::LengthOverflow)?,
            )
            .ok_or(ProjectionCodecError::LengthOverflow)?;
        if expected != words.len() {
            return Err(ProjectionCodecError::InvalidLength {
                declared: expected,
                actual: words.len(),
            });
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| ProjectionCodecError::AllocationFailed)?;
        for slot_index in 0..count {
            let index =
                AGG_OUTPUT_PROJECTION_HEADER_WORDS + slot_index * AGG_OUTPUT_PROJECTION_SLOT_WORDS;
            let tag = read(index, "output source")?;
            let primary = read(index + 1, "output source index")? as u32;
            let source_raw = read(index + 2, "aggregate source")?;
            let kind_raw = read(index + 3, "aggregate kind")?;
            let source = match tag {
                0 => {
                    if source_raw != 0 || kind_raw != 0 {
                        return Err(ProjectionCodecError::NonCanonical { index: index + 2 });
                    }
                    AggOutputSource::GroupKey { key_index: primary }
                }
                1 => {
                    let aggregate_source = match source_raw {
                        0 => AggregateSource::Value,
                        1 => AggregateSource::Rhs,
                        raw => {
                            return Err(ProjectionCodecError::InvalidTag {
                                index: index + 2,
                                field: "aggregate source",
                                raw,
                            });
                        }
                    };
                    let kind = match kind_raw {
                        0 => AggregateKind::Sum,
                        1 => AggregateKind::Count,
                        2 => AggregateKind::Min,
                        3 => AggregateKind::Max,
                        4 => AggregateKind::Avg,
                        5 => AggregateKind::StddevSamp,
                        raw => {
                            return Err(ProjectionCodecError::InvalidTag {
                                index: index + 3,
                                field: "aggregate kind",
                                raw,
                            });
                        }
                    };
                    AggOutputSource::Aggregate {
                        measure_index: primary,
                        source: aggregate_source,
                        kind,
                    }
                }
                raw => {
                    return Err(ProjectionCodecError::InvalidTag {
                        index,
                        field: "output source",
                        raw,
                    });
                }
            };
            let nullable_raw = read(index + 8, "nullable flag")?;
            let nullable = match nullable_raw {
                0 => false,
                1 => true,
                raw => {
                    return Err(ProjectionCodecError::InvalidValue {
                        index: index + 8,
                        field: "nullable flag",
                        raw,
                    });
                }
            };
            slots.push(AggOutputSlot {
                source,
                source_type_oid: read(index + 4, "source type OID")? as u32,
                result_type_oid: read(index + 5, "result type OID")? as u32,
                result_typmod: read(index + 6, "result typmod")?,
                result_collation_oid: read(index + 7, "result collation OID")? as u32,
                nullable,
            });
        }
        let projection = Self { slots };
        projection.validate(spec)?;
        let canonical = projection.encode_i32(spec)?;
        if canonical != words {
            let index = canonical
                .iter()
                .zip(words)
                .position(|(expected, actual)| expected != actual)
                .unwrap_or(words.len().min(canonical.len()));
            return Err(ProjectionCodecError::NonCanonical { index });
        }
        Ok(projection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::spec::{
        BinaryMeasureOp, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeyRef, GroupKeySource,
        MeasureExpr, MeasureSpec,
    };

    fn spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 10,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::FactColumn(ColumnRef {
                    relation_oid: 10,
                    attno: 1,
                    type_oid: 23,
                }),
                type_oid: 23,
                collation_oid: 0,
                encoding: GroupKeyEncoding::DenseI32 {
                    code_min: 0,
                    cardinality: 8,
                    null_code: Some(7),
                },
            }],
            measures: vec![MeasureSpec {
                expression: MeasureExpr::Column(ColumnRef {
                    relation_oid: 10,
                    attno: 2,
                    type_oid: 701,
                }),
                outputs: vec![
                    AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    },
                    AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                ],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn projection() -> AggOutputProjection {
        AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: 23,
                    result_type_oid: 23,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    },
                    source_type_oid: 701,
                    result_type_oid: 701,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
            ],
        }
    }

    #[test]
    fn round_trip_is_canonical_and_ordered() {
        let spec = spec();
        let projection = projection();
        let encoded = projection.encode_i32(&spec).expect("projection encodes");
        assert_eq!(
            AggOutputProjection::decode_i32(&encoded, &spec).expect("projection decodes"),
            projection
        );
    }

    #[test]
    fn every_word_and_byte_prefix_is_rejected() {
        let spec = spec();
        let encoded = projection().encode_i32(&spec).expect("projection encodes");
        for end in 0..encoded.len() {
            assert!(AggOutputProjection::decode_i32(&encoded[..end], &spec).is_err());
        }
        let bytes = encoded
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        for end in 0..bytes.len() {
            let rejected = if end % 4 != 0 {
                true
            } else {
                let words = bytes[..end]
                    .chunks_exact(4)
                    .map(|chunk| i32::from_le_bytes(chunk.try_into().expect("four bytes")))
                    .collect::<Vec<_>>();
                AggOutputProjection::decode_i32(&words, &spec).is_err()
            };
            assert!(rejected, "byte prefix {end} unexpectedly decoded");
        }
    }

    #[test]
    fn invalid_references_tags_flags_lengths_and_trailing_words_are_rejected() {
        let spec = spec();
        let encoded = projection().encode_i32(&spec).expect("projection encodes");
        for (index, value) in [(4, 9), (5, 99), (12, 2)] {
            let mut corrupt = encoded.clone();
            corrupt[index] = value;
            assert!(AggOutputProjection::decode_i32(&corrupt, &spec).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(AggOutputProjection::decode_i32(&trailing, &spec).is_err());

        let mut noncanonical_group = projection().encode_i32(&spec).expect("projection encodes");
        noncanonical_group[6] = 1;
        assert!(matches!(
            AggOutputProjection::decode_i32(&noncanonical_group, &spec),
            Err(ProjectionCodecError::NonCanonical { index: 6 })
        ));
    }

    #[test]
    fn output_nullability_matches_sql_aggregate_semantics() {
        let spec = spec();
        let mut projection = projection();
        projection.slots[1].nullable = false;
        assert!(
            projection.validate(&spec).is_err(),
            "SUM accepted as non-nullable"
        );

        projection.slots[1] = AggOutputSlot {
            source: AggOutputSource::Aggregate {
                measure_index: 0,
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            },
            source_type_oid: 701,
            result_type_oid: 20,
            result_typmod: -1,
            result_collation_oid: 0,
            nullable: true,
        };
        assert!(
            projection.validate(&spec).is_err(),
            "COUNT accepted as nullable"
        );
    }

    #[test]
    fn source_and_result_type_semantics_are_fail_closed() {
        let base_spec = spec();
        let mut base_projection = projection();
        base_projection.slots[0].source_type_oid = INT8_OID;
        assert!(
            base_projection.validate(&base_spec).is_err(),
            "direct group-key source type drift was accepted"
        );
        let mut base_projection = projection();
        base_projection.slots[0].result_collation_oid = 100;
        assert!(
            base_projection.validate(&base_spec).is_err(),
            "group-key result collation drift was accepted"
        );
        let mut base_projection = projection();
        base_projection.slots[1].result_type_oid = INT8_OID;
        assert!(
            base_projection.validate(&base_spec).is_err(),
            "SUM(float8) accepted an int8 result"
        );
        let mut base_projection = projection();
        base_projection.slots[1].result_collation_oid = 100;
        assert!(
            base_projection.validate(&base_spec).is_err(),
            "numeric aggregate accepted a nonzero collation"
        );

        let count_star_spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let mut count_star = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: 0,
                result_type_oid: INT8_OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            }],
        };
        count_star
            .validate(&count_star_spec)
            .expect("COUNT(*) uses InvalidOid source and int8 result");
        count_star.slots[0].source_type_oid = INT8_OID;
        assert!(
            count_star.validate(&count_star_spec).is_err(),
            "COUNT(*) accepted a synthetic nonzero source type"
        );

        let int4_column = ColumnRef {
            relation_oid: 10,
            attno: 2,
            type_oid: INT4_OID,
        };
        let int4_sum_spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::Column(int4_column),
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let mut int4_sum = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                },
                source_type_oid: INT4_OID,
                result_type_oid: INT8_OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            }],
        };
        int4_sum
            .validate(&int4_sum_spec)
            .expect("SUM(int4) widens to int8");
        int4_sum.slots[0].result_type_oid = INT4_OID;
        assert!(
            int4_sum.validate(&int4_sum_spec).is_err(),
            "SUM(int4) accepted a non-widened result"
        );

        let mismatched_binary_spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::Binary {
                    op: BinaryMeasureOp::Sub,
                    lhs: int4_column,
                    rhs: ColumnRef {
                        relation_oid: 10,
                        attno: 3,
                        type_oid: INT8_OID,
                    },
                },
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let binary_count = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: INT4_OID,
                result_type_oid: INT8_OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            }],
        };
        assert!(
            binary_count.validate(&mismatched_binary_spec).is_err(),
            "binary expression with unequal input types produced an inferred source type"
        );

        let h3_spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::H3CellToParent {
                    cell: ColumnRef {
                        relation_oid: 10,
                        attno: 1,
                        type_oid: 9_999,
                    },
                    resolution: 9,
                },
                type_oid: 9_999,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: count_star_spec.measures,
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let mut h3_projection = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::GroupKey { key_index: 0 },
                source_type_oid: 9_999,
                result_type_oid: 9_999,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            }],
        };
        h3_projection
            .validate(&h3_spec)
            .expect("H3 group key carries an explicit nonzero source type");
        h3_projection.slots[0].source_type_oid = 0;
        assert!(
            h3_projection.validate(&h3_spec).is_err(),
            "H3 group key accepted an omitted source type"
        );
    }

    #[test]
    fn phase5_aggregate_result_type_matrix_is_explicit() {
        for (source, kind, result) in [
            (0, AggregateKind::Count, INT8_OID),
            (BOOL_OID, AggregateKind::Count, INT8_OID),
            (INT2_OID, AggregateKind::Count, INT8_OID),
            (INT4_OID, AggregateKind::Count, INT8_OID),
            (INT8_OID, AggregateKind::Count, INT8_OID),
            (FLOAT4_OID, AggregateKind::Count, INT8_OID),
            (FLOAT8_OID, AggregateKind::Count, INT8_OID),
            (DATE_OID, AggregateKind::Count, INT8_OID),
            (TIMESTAMP_OID, AggregateKind::Count, INT8_OID),
            (TIMESTAMPTZ_OID, AggregateKind::Count, INT8_OID),
            (INT2_OID, AggregateKind::Sum, INT8_OID),
            (INT4_OID, AggregateKind::Sum, INT8_OID),
            (INT2_OID, AggregateKind::Avg, NUMERIC_OID),
            (INT4_OID, AggregateKind::Avg, NUMERIC_OID),
            (FLOAT8_OID, AggregateKind::Sum, FLOAT8_OID),
            (INT2_OID, AggregateKind::Min, INT2_OID),
            (INT4_OID, AggregateKind::Min, INT4_OID),
            (INT8_OID, AggregateKind::Max, INT8_OID),
            (FLOAT4_OID, AggregateKind::Max, FLOAT4_OID),
            (FLOAT8_OID, AggregateKind::Min, FLOAT8_OID),
            (DATE_OID, AggregateKind::Min, DATE_OID),
            (TIMESTAMP_OID, AggregateKind::Max, TIMESTAMP_OID),
            (TIMESTAMPTZ_OID, AggregateKind::Min, TIMESTAMPTZ_OID),
            (FLOAT8_OID, AggregateKind::Avg, FLOAT8_OID),
            (FLOAT8_OID, AggregateKind::StddevSamp, FLOAT8_OID),
        ] {
            assert_eq!(aggregate_result_type(source, kind), Some(result));
        }
        for (source, kind) in [
            (0, AggregateKind::Sum),
            (INT8_OID, AggregateKind::Sum),
            (INT4_OID, AggregateKind::StddevSamp),
            (BOOL_OID, AggregateKind::Min),
            (FLOAT4_OID, AggregateKind::Sum),
            (DATE_OID, AggregateKind::Sum),
            (25, AggregateKind::Count),
        ] {
            assert_eq!(
                aggregate_result_type(source, kind),
                None,
                "unsupported lane ({source}, {kind:?}) was accepted"
            );
        }
    }

    #[test]
    fn oversized_projection_is_rejected_before_slot_allocation() {
        let count = MAX_AGG_OUTPUT_PROJECTION_SLOTS + 1;
        let length = AGG_OUTPUT_PROJECTION_HEADER_WORDS + count * AGG_OUTPUT_PROJECTION_SLOT_WORDS;
        let mut words = vec![0; length];
        words[0] = AGG_OUTPUT_PROJECTION_WIRE_MAGIC;
        words[1] = AGG_OUTPUT_PROJECTION_VERSION as i32;
        words[2] = length as i32;
        words[3] = count as i32;
        assert!(matches!(
            AggOutputProjection::decode_i32(&words, &spec()),
            Err(ProjectionCodecError::LimitExceeded {
                field: "wire length" | "output slot count",
                ..
            })
        ));
    }

    #[test]
    fn deterministic_adversarial_words_never_decode_noncanonically() {
        let spec = spec();
        let mut state = 0xBB67_AE85_84CA_A73B_u64;
        for case in 0..2_048usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let len = state as usize % 96;
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                words.push((state >> 32) as i32);
            }
            if case % 4 == 0 && words.len() >= AGG_OUTPUT_PROJECTION_HEADER_WORDS {
                words[0] = AGG_OUTPUT_PROJECTION_WIRE_MAGIC;
                words[1] = AGG_OUTPUT_PROJECTION_VERSION as i32;
                words[2] = words.len() as i32;
            }
            if let Ok(decoded) = AggOutputProjection::decode_i32(&words, &spec) {
                assert_eq!(decoded.encode_i32(&spec).expect("re-encode"), words);
            }
        }
    }

    #[test]
    fn projection_errors_report_precise_wire_context() {
        let cases = [
            (
                ProjectionCodecError::InvalidProjection("bad lane"),
                "invalid aggregate output projection: bad lane",
            ),
            (
                ProjectionCodecError::Truncated {
                    index: 7,
                    field: "result type",
                },
                "truncated aggregate output projection at word 7 while reading result type",
            ),
            (
                ProjectionCodecError::InvalidMagic(0x1234),
                "invalid aggregate output projection magic 0x1234",
            ),
            (
                ProjectionCodecError::UnsupportedVersion(9),
                "unsupported aggregate output projection version 9",
            ),
            (
                ProjectionCodecError::InvalidLength {
                    declared: 10,
                    actual: 11,
                },
                "aggregate output projection length is 11, header declares 10",
            ),
            (
                ProjectionCodecError::InvalidTag {
                    index: 4,
                    field: "source",
                    raw: -1,
                },
                "invalid source tag -1 at word 4",
            ),
            (
                ProjectionCodecError::InvalidValue {
                    index: 12,
                    field: "nullable",
                    raw: 2,
                },
                "invalid nullable value 2 at word 12",
            ),
            (
                ProjectionCodecError::LimitExceeded {
                    index: 3,
                    field: "slot count",
                    declared: 4,
                    maximum: 3,
                },
                "slot count 4 at word 3 exceeds schema maximum 3",
            ),
            (
                ProjectionCodecError::NonCanonical { index: 6 },
                "aggregate output projection is not canonically encoded at word 6",
            ),
            (
                ProjectionCodecError::AllocationFailed,
                "could not allocate aggregate output projection",
            ),
            (
                ProjectionCodecError::LengthOverflow,
                "aggregate output projection length overflows i32",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn validation_rejects_each_projection_contract_violation() {
        let base_spec = spec();

        let mut invalid_spec = base_spec.clone();
        invalid_spec.fact_rel = 0;
        assert_eq!(
            projection().validate(&invalid_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "query spec is invalid"
            ))
        );
        assert_eq!(
            AggOutputProjection { slots: Vec::new() }.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "projection has no output slots"
            ))
        );

        let mut invalid = projection();
        invalid.slots[0].result_type_oid = 0;
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "output slot has invalid result type OID"
            ))
        );
        let mut invalid = projection();
        invalid.slots[0].result_typmod = -2;
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "output slot has invalid typmod"
            ))
        );
        let mut invalid = projection();
        invalid.slots[0].source = AggOutputSource::GroupKey { key_index: 1 };
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "output references a missing group key"
            ))
        );
        let mut invalid = projection();
        invalid.slots[0].source_type_oid = INT8_OID;
        invalid.slots[0].result_type_oid = INT8_OID;
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "group-key source type does not match query spec"
            ))
        );
        let mut invalid = projection();
        invalid.slots[1].source = AggOutputSource::Aggregate {
            measure_index: 1,
            source: AggregateSource::Value,
            kind: AggregateKind::Sum,
        };
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "output references a missing measure"
            ))
        );
        let mut invalid = projection();
        invalid.slots[1].source = AggOutputSource::Aggregate {
            measure_index: 0,
            source: AggregateSource::Value,
            kind: AggregateKind::Min,
        };
        assert_eq!(
            invalid.validate(&base_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "output references an aggregate lane not projected by its measure"
            ))
        );

        let mut int8_sum_spec = base_spec.clone();
        int8_sum_spec.group_keys.clear();
        int8_sum_spec.measures[0].expression = MeasureExpr::Column(ColumnRef {
            relation_oid: 10,
            attno: 2,
            type_oid: INT8_OID,
        });
        let int8_sum = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Sum,
                },
                source_type_oid: INT8_OID,
                result_type_oid: INT8_OID,
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: true,
            }],
        };
        assert_eq!(
            int8_sum.validate(&int8_sum_spec),
            Err(ProjectionCodecError::InvalidProjection(
                "aggregate source/kind has no canonical result type mapping"
            ))
        );

        let slot = projection().slots[0];
        let oversized = AggOutputProjection {
            slots: vec![slot; MAX_AGG_OUTPUT_PROJECTION_SLOTS + 1],
        };
        assert!(matches!(
            oversized.validate(&base_spec),
            Err(ProjectionCodecError::LimitExceeded {
                field: "output slot count",
                ..
            })
        ));
    }

    #[test]
    fn source_type_and_lane_wire_matrices_are_complete() {
        let column = ColumnRef {
            relation_oid: 10,
            attno: 2,
            type_oid: FLOAT8_OID,
        };
        let rhs = ColumnRef {
            relation_oid: 10,
            attno: 3,
            type_oid: FLOAT8_OID,
        };
        let outputs = vec![
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Sum,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Min,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Max,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::Avg,
            },
            AggregateOutput {
                source: AggregateSource::Value,
                kind: AggregateKind::StddevSamp,
            },
            AggregateOutput {
                source: AggregateSource::Rhs,
                kind: AggregateKind::Sum,
            },
            AggregateOutput {
                source: AggregateSource::Rhs,
                kind: AggregateKind::Count,
            },
            AggregateOutput {
                source: AggregateSource::Rhs,
                kind: AggregateKind::Avg,
            },
        ];
        let matrix_spec = AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::StatsPair { value: column, rhs },
                outputs: outputs.clone(),
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        let matrix_projection = AggOutputProjection {
            slots: outputs
                .iter()
                .map(|output| AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: output.source,
                        kind: output.kind,
                    },
                    source_type_oid: FLOAT8_OID,
                    result_type_oid: if output.kind == AggregateKind::Count {
                        INT8_OID
                    } else {
                        FLOAT8_OID
                    },
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: output.kind != AggregateKind::Count,
                })
                .collect(),
        };
        let encoded = matrix_projection
            .encode_i32(&matrix_spec)
            .expect("encode complete aggregate lane matrix");
        assert_eq!(
            AggOutputProjection::decode_i32(&encoded, &matrix_spec)
                .expect("decode complete aggregate lane matrix"),
            matrix_projection
        );

        let count_star = MeasureSpec {
            expression: MeasureExpr::CountStar,
            outputs: Vec::new(),
            filter: FilterSpec::None,
        };
        assert_eq!(
            measure_source_type(&count_star, AggregateSource::Value),
            Ok(0)
        );
        assert!(measure_source_type(&count_star, AggregateSource::Rhs).is_err());
        let bytecode = MeasureSpec {
            expression: MeasureExpr::Bytecode {
                inputs: vec![column],
                program: vec![0],
                result_type_oid: FLOAT8_OID,
            },
            outputs: Vec::new(),
            filter: FilterSpec::None,
        };
        assert_eq!(
            measure_source_type(&bytecode, AggregateSource::Value),
            Ok(FLOAT8_OID)
        );
        assert_eq!(
            measure_source_type(&matrix_spec.measures[0], AggregateSource::Value),
            Ok(FLOAT8_OID)
        );
        assert_eq!(
            measure_source_type(&matrix_spec.measures[0], AggregateSource::Rhs),
            Ok(FLOAT8_OID)
        );
    }

    #[test]
    fn decoder_distinguishes_header_shape_tag_and_flag_failures() {
        let spec = spec();
        let encoded = projection().encode_i32(&spec).expect("encode projection");

        let mut invalid = encoded.clone();
        invalid[1] = 99;
        assert!(matches!(
            AggOutputProjection::decode_i32(&invalid, &spec),
            Err(ProjectionCodecError::UnsupportedVersion(99))
        ));
        let mut invalid = encoded.clone();
        invalid[2] = -1;
        assert!(matches!(
            AggOutputProjection::decode_i32(&invalid, &spec),
            Err(ProjectionCodecError::InvalidValue {
                index: 2,
                field: "wire length",
                ..
            })
        ));
        let mut invalid = encoded.clone();
        invalid[3] = 1;
        assert!(matches!(
            AggOutputProjection::decode_i32(&invalid, &spec),
            Err(ProjectionCodecError::InvalidLength { declared: 13, .. })
        ));
        for (index, expected_field) in [
            (13, "output source"),
            (15, "aggregate source"),
            (16, "aggregate kind"),
        ] {
            let mut invalid = encoded.clone();
            invalid[index] = 99;
            assert!(matches!(
                AggOutputProjection::decode_i32(&invalid, &spec),
                Err(ProjectionCodecError::InvalidTag { field, .. }) if field == expected_field
            ));
        }
        let mut invalid = encoded;
        invalid[21] = 2;
        assert!(matches!(
            AggOutputProjection::decode_i32(&invalid, &spec),
            Err(ProjectionCodecError::InvalidValue {
                field: "nullable flag",
                ..
            })
        ));
    }
}

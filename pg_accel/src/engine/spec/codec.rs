//! Strict versioned `i32` wire codec for [`AggQuerySpec`].

use super::{
    AGG_QUERY_SPEC_VERSION, AggQuerySpec, AggregateKind, AggregateOutput, AggregateRef,
    AggregateSource, BinaryMeasureOp, ColumnRef, DimSpec, FilterSpec, GroupKeyEncoding,
    GroupKeyRef, GroupKeySource, HavingSpec, JoinMultiplicity, MAX_AGGREGATE_OUTPUTS,
    MAX_BYTECODE_WORDS, MAX_PROGRAM_INPUTS, MAX_VALUE_AGGREGATE_OUTPUTS, MaskKind, MeasureExpr,
    MeasureSpec, ScalarRange, ScalarValue, SpecValidationError, abi,
};

pub const AGG_QUERY_SPEC_WIRE_MAGIC: i32 = 0x5047_4132; // "PGA2"
pub const AGG_QUERY_SPEC_HEADER_WORDS: usize = 3;
const MAX_COLUMN_LIST_WORDS: usize = 1 + MAX_PROGRAM_INPUTS * 3;
const MAX_BYTECODE_BLOCK_WORDS: usize = 1 + MAX_BYTECODE_WORDS;
const MAX_FILTER_WORDS: usize = 1 + MAX_COLUMN_LIST_WORDS + MAX_BYTECODE_BLOCK_WORDS;
const MAX_GROUP_KEY_WORDS: usize = 1 + MAX_COLUMN_LIST_WORDS + MAX_BYTECODE_BLOCK_WORDS + 7;
const MAX_BYTECODE_MEASURE_EXPR_WORDS: usize =
    1 + MAX_COLUMN_LIST_WORDS + MAX_BYTECODE_BLOCK_WORDS + 1;
const STATS_PAIR_MEASURE_EXPR_WORDS: usize = 1 + 3 + 3;
const MAX_VALUE_OUTPUT_LIST_WORDS: usize = 1 + MAX_VALUE_AGGREGATE_OUTPUTS * 2;
const MAX_STATS_PAIR_OUTPUT_LIST_WORDS: usize = 1 + MAX_AGGREGATE_OUTPUTS * 2;
const MAX_BYTECODE_MEASURE_WORDS: usize =
    MAX_BYTECODE_MEASURE_EXPR_WORDS + MAX_VALUE_OUTPUT_LIST_WORDS + MAX_FILTER_WORDS;
const MAX_STATS_PAIR_MEASURE_WORDS: usize =
    STATS_PAIR_MEASURE_EXPR_WORDS + MAX_STATS_PAIR_OUTPUT_LIST_WORDS + MAX_FILTER_WORDS;
const MAX_MEASURE_WORDS: usize = if MAX_BYTECODE_MEASURE_WORDS > MAX_STATS_PAIR_MEASURE_WORDS {
    MAX_BYTECODE_MEASURE_WORDS
} else {
    MAX_STATS_PAIR_MEASURE_WORDS
};
const MAX_DIM_WORDS: usize = 1 + 3 + 3 + 1 + 1 + MAX_FILTER_WORDS;
const MAX_HAVING_WORDS: usize = 1 + 1 + MAX_PROGRAM_INPUTS * 3 + MAX_BYTECODE_BLOCK_WORDS;

/// Exact maximum word length of any semantically valid v2 spec.
pub const AGG_QUERY_SPEC_MAX_WORDS: usize = AGG_QUERY_SPEC_HEADER_WORDS
    + 1
    + 1
    + abi::PGACCEL_GROUPED_AGG_MAX_KEYS * MAX_GROUP_KEY_WORDS
    + 1
    + abi::PGACCEL_GROUPED_AGG_MAX_MEASURES * MAX_MEASURE_WORDS
    + MAX_FILTER_WORDS
    + 1
    + abi::PGACCEL_GROUPED_AGG_MAX_DIMS * MAX_DIM_WORDS
    + MAX_HAVING_WORDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecCodecError {
    InvalidSpec(SpecValidationError),
    Truncated {
        index: usize,
        context: &'static str,
    },
    InvalidMagic(i32),
    UnsupportedVersion(u32),
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    InvalidTag {
        index: usize,
        context: &'static str,
        tag: i32,
    },
    InvalidValue {
        index: usize,
        context: &'static str,
    },
    TrailingWords {
        index: usize,
        total: usize,
    },
    NonCanonical {
        index: usize,
    },
    LimitExceeded {
        index: usize,
        context: &'static str,
        declared: usize,
        maximum: usize,
    },
    AllocationFailed {
        context: &'static str,
    },
    LengthOverflow,
}

impl std::fmt::Display for SpecCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSpec(error) => write!(f, "invalid aggregate spec: {error}"),
            Self::Truncated { index, context } => {
                write!(
                    f,
                    "truncated aggregate spec at word {index} while reading {context}"
                )
            }
            Self::InvalidMagic(magic) => write!(f, "invalid aggregate spec magic {magic:#x}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported aggregate spec version {version}")
            }
            Self::LengthMismatch { declared, actual } => {
                write!(
                    f,
                    "aggregate spec length is {actual}, header declares {declared}"
                )
            }
            Self::InvalidTag {
                index,
                context,
                tag,
            } => write!(f, "invalid {context} tag {tag} at word {index}"),
            Self::InvalidValue { index, context } => {
                write!(f, "invalid {context} at word {index}")
            }
            Self::TrailingWords { index, total } => {
                write!(f, "aggregate spec has trailing words at {index} of {total}")
            }
            Self::NonCanonical { index } => {
                write!(
                    f,
                    "aggregate spec is not canonically encoded at word {index}"
                )
            }
            Self::LimitExceeded {
                index,
                context,
                declared,
                maximum,
            } => write!(
                f,
                "{context} {declared} at word {index} exceeds schema maximum {maximum}"
            ),
            Self::AllocationFailed { context } => {
                write!(f, "could not allocate aggregate spec {context}")
            }
            Self::LengthOverflow => f.write_str("aggregate spec exceeds i32-list length limit"),
        }
    }
}

impl std::error::Error for SpecCodecError {}

impl From<SpecValidationError> for SpecCodecError {
    fn from(value: SpecValidationError) -> Self {
        Self::InvalidSpec(value)
    }
}

struct Encoder {
    words: Vec<i32>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            words: vec![AGG_QUERY_SPEC_WIRE_MAGIC, AGG_QUERY_SPEC_VERSION as i32, 0],
        }
    }

    fn finish(mut self) -> Result<Vec<i32>, SpecCodecError> {
        if self.words.len() > AGG_QUERY_SPEC_MAX_WORDS {
            return Err(SpecCodecError::LimitExceeded {
                index: 2,
                context: "wire length",
                declared: self.words.len(),
                maximum: AGG_QUERY_SPEC_MAX_WORDS,
            });
        }
        let len = i32::try_from(self.words.len()).map_err(|_| SpecCodecError::LengthOverflow)?;
        self.words[2] = len;
        Ok(self.words)
    }

    fn push(&mut self, value: i32) {
        self.words.push(value);
    }

    fn push_u32(&mut self, value: u32) {
        self.push(value as i32);
    }

    fn push_bool(&mut self, value: bool) {
        self.push(i32::from(value));
    }

    fn push_len(&mut self, len: usize) -> Result<(), SpecCodecError> {
        self.push(i32::try_from(len).map_err(|_| SpecCodecError::LengthOverflow)?);
        Ok(())
    }

    fn push_i64(&mut self, value: i64) {
        let bits = value as u64;
        self.push((bits >> 32) as i32);
        self.push(bits as u32 as i32);
    }

    fn push_f64(&mut self, value: f64) {
        let bits = if value == 0.0 { 0 } else { value.to_bits() };
        self.push((bits >> 32) as i32);
        self.push(bits as u32 as i32);
    }

    fn push_f32(&mut self, value: f32) {
        self.push(if value == 0.0 {
            0
        } else {
            value.to_bits() as i32
        });
    }

    fn push_aggregate_source(&mut self, source: AggregateSource) {
        self.push(match source {
            AggregateSource::Value => 0,
            AggregateSource::Rhs => 1,
        });
    }

    fn push_aggregate_kind(&mut self, kind: AggregateKind) {
        self.push(match kind {
            AggregateKind::Sum => 0,
            AggregateKind::Count => 1,
            AggregateKind::Min => 2,
            AggregateKind::Max => 3,
            AggregateKind::Avg => 4,
            AggregateKind::StddevSamp => 5,
        });
    }

    fn push_column(&mut self, column: ColumnRef) {
        self.push_u32(column.relation_oid);
        self.push(column.attno);
        self.push_u32(column.type_oid);
    }

    fn push_columns(&mut self, columns: &[ColumnRef]) -> Result<(), SpecCodecError> {
        self.push_len(columns.len())?;
        for column in columns {
            self.push_column(*column);
        }
        Ok(())
    }

    fn push_words(&mut self, words: &[i32]) -> Result<(), SpecCodecError> {
        self.push_len(words.len())?;
        self.words.extend_from_slice(words);
        Ok(())
    }

    fn push_scalar(&mut self, value: ScalarValue) {
        match value {
            ScalarValue::Bool(value) => {
                self.push(1);
                self.push_bool(value);
            }
            ScalarValue::I32(value) => {
                self.push(2);
                self.push(value);
            }
            ScalarValue::I64(value) => {
                self.push(3);
                self.push_i64(value);
            }
            ScalarValue::F32(value) => {
                self.push(4);
                self.push_f32(value);
            }
            ScalarValue::F64(value) => {
                self.push(5);
                self.push_f64(value);
            }
        }
    }

    fn push_filter(&mut self, filter: &FilterSpec) -> Result<(), SpecCodecError> {
        match filter {
            FilterSpec::None => self.push(0),
            FilterSpec::Ranges { input, ranges } => {
                self.push(1);
                self.push_column(*input);
                self.push_len(ranges.len())?;
                for range in ranges {
                    self.push_scalar(range.lo);
                    self.push_scalar(range.hi);
                }
            }
            FilterSpec::Mask { input, kind } => {
                self.push(2);
                self.push_column(*input);
                self.push(match kind {
                    MaskKind::Sql => 0,
                    MaskKind::Recheck => 1,
                });
            }
            FilterSpec::Bytecode { inputs, program } => {
                self.push(3);
                self.push_columns(inputs)?;
                self.push_words(program)?;
            }
            FilterSpec::Spatial {
                function_oid,
                left,
                right,
                distance,
            } => {
                self.push(4);
                self.push_u32(*function_oid);
                self.push_column(*left);
                self.push_column(*right);
                self.push_bool(distance.is_some());
                if let Some(distance) = distance {
                    self.push_scalar(*distance);
                }
            }
        }
        Ok(())
    }

    fn push_group_key(&mut self, key: &GroupKeyRef) -> Result<(), SpecCodecError> {
        match &key.source {
            GroupKeySource::FactColumn(column) => {
                self.push(0);
                self.push_column(*column);
            }
            GroupKeySource::StarDimension {
                dim_index,
                group_column,
            } => {
                self.push(1);
                self.push_u32(*dim_index);
                self.push_column(*group_column);
            }
            GroupKeySource::Expression { inputs, program } => {
                self.push(2);
                self.push_columns(inputs)?;
                self.push_words(program)?;
            }
            GroupKeySource::H3Cell { input, resolution } => {
                self.push(3);
                self.push_column(*input);
                self.push(*resolution);
            }
        }
        match key.encoding {
            GroupKeyEncoding::DenseI32 {
                code_min,
                cardinality,
                null_code,
            } => {
                self.push(0);
                self.push(code_min);
                self.push_u32(cardinality);
                self.push_bool(null_code.is_some());
                if let Some(null_code) = null_code {
                    self.push(null_code);
                }
            }
            GroupKeyEncoding::DictionaryI32 {
                cardinality,
                null_code,
            } => {
                self.push(1);
                self.push_u32(cardinality);
                self.push_bool(null_code.is_some());
                if let Some(null_code) = null_code {
                    self.push(null_code);
                }
            }
            GroupKeyEncoding::Hash => self.push(2),
        }
        self.push_u32(key.type_oid);
        self.push_u32(key.collation_oid);
        Ok(())
    }

    fn push_measure(&mut self, measure: &MeasureSpec) -> Result<(), SpecCodecError> {
        match &measure.expression {
            MeasureExpr::CountStar => self.push(0),
            MeasureExpr::Column(column) => {
                self.push(1);
                self.push_column(*column);
            }
            MeasureExpr::Binary { op, lhs, rhs } => {
                self.push(2);
                self.push(match op {
                    BinaryMeasureOp::Mul => 0,
                    BinaryMeasureOp::Sub => 1,
                });
                self.push_column(*lhs);
                self.push_column(*rhs);
            }
            MeasureExpr::StatsPair { value, rhs } => {
                self.push(3);
                self.push_column(*value);
                self.push_column(*rhs);
            }
            MeasureExpr::Bytecode {
                inputs,
                program,
                result_type_oid,
            } => {
                self.push(4);
                self.push_columns(inputs)?;
                self.push_words(program)?;
                self.push_u32(*result_type_oid);
            }
        }
        self.push_len(measure.outputs.len())?;
        for output in &measure.outputs {
            self.push_aggregate_source(output.source);
            self.push_aggregate_kind(output.kind);
        }
        self.push_filter(&measure.filter)
    }

    fn push_dim(&mut self, dim: &DimSpec) -> Result<(), SpecCodecError> {
        self.push_u32(dim.relation_oid);
        self.push_column(dim.fact_key);
        self.push_column(dim.dim_key);
        self.push_u32(dim.collation_oid);
        self.push(match dim.multiplicity {
            JoinMultiplicity::Unique => 0,
            JoinMultiplicity::Counted => 1,
        });
        self.push_filter(&dim.filter)
    }
}

struct Decoder<'a> {
    words: &'a [i32],
    index: usize,
}

impl<'a> Decoder<'a> {
    const fn new(words: &'a [i32]) -> Self {
        Self { words, index: 0 }
    }

    fn read(&mut self, context: &'static str) -> Result<i32, SpecCodecError> {
        let Some(value) = self.words.get(self.index).copied() else {
            return Err(SpecCodecError::Truncated {
                index: self.index,
                context,
            });
        };
        self.index += 1;
        Ok(value)
    }

    fn read_u32(&mut self, context: &'static str) -> Result<u32, SpecCodecError> {
        Ok(self.read(context)? as u32)
    }

    fn read_bool(&mut self, context: &'static str) -> Result<bool, SpecCodecError> {
        let index = self.index;
        match self.read(context)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SpecCodecError::InvalidValue { index, context }),
        }
    }

    fn read_count(
        &mut self,
        context: &'static str,
        maximum: usize,
        minimum_words_per_item: usize,
    ) -> Result<usize, SpecCodecError> {
        let index = self.index;
        let raw = self.read(context)?;
        let len =
            usize::try_from(raw).map_err(|_| SpecCodecError::InvalidValue { index, context })?;
        if len > maximum {
            return Err(SpecCodecError::LimitExceeded {
                index,
                context,
                declared: len,
                maximum,
            });
        }
        let Some(minimum_words) = len.checked_mul(minimum_words_per_item) else {
            return Err(SpecCodecError::LimitExceeded {
                index,
                context,
                declared: len,
                maximum,
            });
        };
        if minimum_words > self.words.len().saturating_sub(self.index) {
            return Err(SpecCodecError::Truncated {
                index: self.index,
                context,
            });
        }
        Ok(len)
    }

    fn empty_vec<T>(len: usize, context: &'static str) -> Result<Vec<T>, SpecCodecError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(len)
            .map_err(|_| SpecCodecError::AllocationFailed { context })?;
        Ok(values)
    }

    fn read_tag(
        &mut self,
        context: &'static str,
        max_inclusive: i32,
    ) -> Result<i32, SpecCodecError> {
        let index = self.index;
        let tag = self.read(context)?;
        if !(0..=max_inclusive).contains(&tag) {
            return Err(SpecCodecError::InvalidTag {
                index,
                context,
                tag,
            });
        }
        Ok(tag)
    }

    fn read_i64(&mut self, context: &'static str) -> Result<i64, SpecCodecError> {
        let hi = u64::from(self.read_u32(context)?);
        let lo = u64::from(self.read_u32(context)?);
        Ok(((hi << 32) | lo) as i64)
    }

    fn read_f64(&mut self, context: &'static str) -> Result<f64, SpecCodecError> {
        let hi = u64::from(self.read_u32(context)?);
        let lo = u64::from(self.read_u32(context)?);
        Ok(f64::from_bits((hi << 32) | lo))
    }

    fn read_f32(&mut self, context: &'static str) -> Result<f32, SpecCodecError> {
        Ok(f32::from_bits(self.read_u32(context)?))
    }

    fn read_aggregate_source(&mut self) -> Result<AggregateSource, SpecCodecError> {
        match self.read_tag("aggregate source", 1)? {
            0 => Ok(AggregateSource::Value),
            1 => Ok(AggregateSource::Rhs),
            _ => unreachable!(),
        }
    }

    fn read_aggregate_kind(&mut self) -> Result<AggregateKind, SpecCodecError> {
        Ok(match self.read_tag("aggregate kind", 5)? {
            0 => AggregateKind::Sum,
            1 => AggregateKind::Count,
            2 => AggregateKind::Min,
            3 => AggregateKind::Max,
            4 => AggregateKind::Avg,
            5 => AggregateKind::StddevSamp,
            _ => unreachable!(),
        })
    }

    fn read_column(&mut self) -> Result<ColumnRef, SpecCodecError> {
        Ok(ColumnRef {
            relation_oid: self.read_u32("column relation OID")?,
            attno: self.read("column attribute number")?,
            type_oid: self.read_u32("column type OID")?,
        })
    }

    fn read_columns(&mut self) -> Result<Vec<ColumnRef>, SpecCodecError> {
        let len = self.read_count("column count", MAX_PROGRAM_INPUTS, 3)?;
        let mut columns = Self::empty_vec(len, "column list")?;
        for _ in 0..len {
            columns.push(self.read_column()?);
        }
        Ok(columns)
    }

    fn read_words(&mut self, context: &'static str) -> Result<Vec<i32>, SpecCodecError> {
        let len = self.read_count(context, MAX_BYTECODE_WORDS, 1)?;
        let end = self.index + len;
        let mut values = Self::empty_vec(len, "bytecode words")?;
        values.extend_from_slice(&self.words[self.index..end]);
        self.index = end;
        Ok(values)
    }

    fn read_scalar(&mut self) -> Result<ScalarValue, SpecCodecError> {
        match self.read_tag("scalar", 5)? {
            1 => Ok(ScalarValue::Bool(self.read_bool("boolean scalar")?)),
            2 => Ok(ScalarValue::I32(self.read("i32 scalar")?)),
            3 => Ok(ScalarValue::I64(self.read_i64("i64 scalar")?)),
            4 => Ok(ScalarValue::F32(self.read_f32("f32 scalar")?)),
            5 => Ok(ScalarValue::F64(self.read_f64("f64 scalar")?)),
            tag => Err(SpecCodecError::InvalidTag {
                index: self.index.saturating_sub(1),
                context: "scalar",
                tag,
            }),
        }
    }

    fn read_filter(&mut self) -> Result<FilterSpec, SpecCodecError> {
        match self.read_tag("filter", 4)? {
            0 => Ok(FilterSpec::None),
            1 => {
                let input = self.read_column()?;
                let len =
                    self.read_count("range count", abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES, 4)?;
                let mut ranges = Self::empty_vec(len, "filter ranges")?;
                for _ in 0..len {
                    ranges.push(ScalarRange {
                        lo: self.read_scalar()?,
                        hi: self.read_scalar()?,
                    });
                }
                Ok(FilterSpec::Ranges { input, ranges })
            }
            2 => {
                let input = self.read_column()?;
                let kind = match self.read_tag("mask kind", 1)? {
                    0 => MaskKind::Sql,
                    1 => MaskKind::Recheck,
                    _ => unreachable!(),
                };
                Ok(FilterSpec::Mask { input, kind })
            }
            3 => Ok(FilterSpec::Bytecode {
                inputs: self.read_columns()?,
                program: self.read_words("bytecode word count")?,
            }),
            4 => {
                let function_oid = self.read_u32("spatial function OID")?;
                let left = self.read_column()?;
                let right = self.read_column()?;
                let distance = self
                    .read_bool("spatial distance presence")?
                    .then(|| self.read_scalar())
                    .transpose()?;
                Ok(FilterSpec::Spatial {
                    function_oid,
                    left,
                    right,
                    distance,
                })
            }
            _ => unreachable!(),
        }
    }

    fn read_group_key(&mut self) -> Result<GroupKeyRef, SpecCodecError> {
        let source = match self.read_tag("group-key source", 3)? {
            0 => GroupKeySource::FactColumn(self.read_column()?),
            1 => GroupKeySource::StarDimension {
                dim_index: self.read_u32("group-key dimension index")?,
                group_column: self.read_column()?,
            },
            2 => GroupKeySource::Expression {
                inputs: self.read_columns()?,
                program: self.read_words("group-key bytecode word count")?,
            },
            3 => GroupKeySource::H3Cell {
                input: self.read_column()?,
                resolution: self.read("H3 resolution")?,
            },
            _ => unreachable!(),
        };
        let encoding = match self.read_tag("group-key encoding", 2)? {
            0 => {
                let code_min = self.read("dense code minimum")?;
                let cardinality = self.read_u32("dense cardinality")?;
                let null_code = self
                    .read_bool("dense NULL-code presence")?
                    .then(|| self.read("dense NULL code"))
                    .transpose()?;
                GroupKeyEncoding::DenseI32 {
                    code_min,
                    cardinality,
                    null_code,
                }
            }
            1 => {
                let cardinality = self.read_u32("dictionary cardinality")?;
                let null_code = self
                    .read_bool("dictionary NULL-code presence")?
                    .then(|| self.read("dictionary NULL code"))
                    .transpose()?;
                GroupKeyEncoding::DictionaryI32 {
                    cardinality,
                    null_code,
                }
            }
            2 => GroupKeyEncoding::Hash,
            _ => unreachable!(),
        };
        Ok(GroupKeyRef {
            source,
            type_oid: self.read_u32("group-key logical type OID")?,
            collation_oid: self.read_u32("group-key collation OID")?,
            encoding,
        })
    }

    fn read_measure(&mut self) -> Result<MeasureSpec, SpecCodecError> {
        let expression = match self.read_tag("measure expression", 4)? {
            0 => MeasureExpr::CountStar,
            1 => MeasureExpr::Column(self.read_column()?),
            2 => {
                let op = match self.read_tag("binary measure operation", 1)? {
                    0 => BinaryMeasureOp::Mul,
                    1 => BinaryMeasureOp::Sub,
                    _ => unreachable!(),
                };
                MeasureExpr::Binary {
                    op,
                    lhs: self.read_column()?,
                    rhs: self.read_column()?,
                }
            }
            3 => MeasureExpr::StatsPair {
                value: self.read_column()?,
                rhs: self.read_column()?,
            },
            4 => MeasureExpr::Bytecode {
                inputs: self.read_columns()?,
                program: self.read_words("measure bytecode word count")?,
                result_type_oid: self.read_u32("measure bytecode result type OID")?,
            },
            _ => unreachable!(),
        };
        let output_count = self.read_count("aggregate output count", MAX_AGGREGATE_OUTPUTS, 2)?;
        let mut outputs = Self::empty_vec(output_count, "aggregate outputs")?;
        for _ in 0..output_count {
            outputs.push(AggregateOutput {
                source: self.read_aggregate_source()?,
                kind: self.read_aggregate_kind()?,
            });
        }
        Ok(MeasureSpec {
            expression,
            outputs,
            filter: self.read_filter()?,
        })
    }

    fn read_dim(&mut self) -> Result<DimSpec, SpecCodecError> {
        let relation_oid = self.read_u32("dimension relation OID")?;
        let fact_key = self.read_column()?;
        let dim_key = self.read_column()?;
        let collation_oid = self.read_u32("dimension join collation OID")?;
        let multiplicity = match self.read_tag("join multiplicity", 1)? {
            0 => JoinMultiplicity::Unique,
            1 => JoinMultiplicity::Counted,
            _ => unreachable!(),
        };
        Ok(DimSpec {
            relation_oid,
            fact_key,
            dim_key,
            collation_oid,
            multiplicity,
            filter: self.read_filter()?,
        })
    }
}

impl AggQuerySpec {
    pub fn encode_i32(&self) -> Result<Vec<i32>, SpecCodecError> {
        self.validate()?;
        let mut encoder = Encoder::new();
        encoder.push_u32(self.fact_rel);
        encoder.push_len(self.group_keys.len())?;
        for key in &self.group_keys {
            encoder.push_group_key(key)?;
        }
        encoder.push_len(self.measures.len())?;
        for measure in &self.measures {
            encoder.push_measure(measure)?;
        }
        encoder.push_filter(&self.fact_filter)?;
        encoder.push_len(self.star_dims.len())?;
        for dim in &self.star_dims {
            encoder.push_dim(dim)?;
        }
        encoder.push_bool(self.having.is_some());
        if let Some(having) = &self.having {
            encoder.push_len(having.inputs.len())?;
            for input in &having.inputs {
                encoder.push_u32(input.measure_index);
                encoder.push_aggregate_source(input.source);
                encoder.push_aggregate_kind(input.kind);
            }
            encoder.push_words(&having.program)?;
        }
        encoder.finish()
    }

    /// Validate an encoded spec header at the start of `words` and return the
    /// exact number of words occupied by that spec. Trailing words are allowed
    /// so planner `private_data` can length-delimit a spec before decoding it.
    pub fn encoded_i32_prefix_len(words: &[i32]) -> Result<usize, SpecCodecError> {
        let mut decoder = Decoder::new(words);
        let magic = decoder.read("wire magic")?;
        if magic != AGG_QUERY_SPEC_WIRE_MAGIC {
            return Err(SpecCodecError::InvalidMagic(magic));
        }
        let version = decoder.read_u32("wire version")?;
        if version != AGG_QUERY_SPEC_VERSION {
            return Err(SpecCodecError::UnsupportedVersion(version));
        }
        let declared_index = decoder.index;
        let declared_raw = decoder.read("wire length")?;
        let declared = usize::try_from(declared_raw).map_err(|_| SpecCodecError::InvalidValue {
            index: declared_index,
            context: "wire length",
        })?;
        if declared < AGG_QUERY_SPEC_HEADER_WORDS {
            return Err(SpecCodecError::InvalidValue {
                index: declared_index,
                context: "wire length",
            });
        }
        if declared > AGG_QUERY_SPEC_MAX_WORDS {
            return Err(SpecCodecError::LimitExceeded {
                index: declared_index,
                context: "wire length",
                declared,
                maximum: AGG_QUERY_SPEC_MAX_WORDS,
            });
        }
        if declared > words.len() {
            return Err(SpecCodecError::Truncated {
                index: words.len(),
                context: "wire payload",
            });
        }
        Ok(declared)
    }

    pub fn decode_i32(words: &[i32]) -> Result<Self, SpecCodecError> {
        let declared = Self::encoded_i32_prefix_len(words)?;
        if declared != words.len() {
            return Err(SpecCodecError::LengthMismatch {
                declared,
                actual: words.len(),
            });
        }
        let mut decoder = Decoder {
            words,
            index: AGG_QUERY_SPEC_HEADER_WORDS,
        };
        let fact_rel = decoder.read_u32("fact relation OID")?;
        let key_count =
            decoder.read_count("group-key count", abi::PGACCEL_GROUPED_AGG_MAX_KEYS, 5)?;
        let mut group_keys = Decoder::empty_vec(key_count, "group keys")?;
        for _ in 0..key_count {
            group_keys.push(decoder.read_group_key()?);
        }
        let measure_count =
            decoder.read_count("measure count", abi::PGACCEL_GROUPED_AGG_MAX_MEASURES, 5)?;
        let mut measures = Decoder::empty_vec(measure_count, "measures")?;
        for _ in 0..measure_count {
            measures.push(decoder.read_measure()?);
        }
        let fact_filter = decoder.read_filter()?;
        let dim_count =
            decoder.read_count("dimension count", abi::PGACCEL_GROUPED_AGG_MAX_DIMS, 9)?;
        let mut star_dims = Decoder::empty_vec(dim_count, "dimensions")?;
        for _ in 0..dim_count {
            star_dims.push(decoder.read_dim()?);
        }
        let having = if decoder.read_bool("HAVING presence")? {
            let input_count = decoder.read_count("HAVING input count", MAX_PROGRAM_INPUTS, 3)?;
            let mut inputs = Decoder::empty_vec(input_count, "HAVING inputs")?;
            for _ in 0..input_count {
                inputs.push(AggregateRef {
                    measure_index: decoder.read_u32("HAVING measure index")?,
                    source: decoder.read_aggregate_source()?,
                    kind: decoder.read_aggregate_kind()?,
                });
            }
            Some(HavingSpec {
                inputs,
                program: decoder.read_words("HAVING bytecode word count")?,
            })
        } else {
            None
        };
        if decoder.index != words.len() {
            return Err(SpecCodecError::TrailingWords {
                index: decoder.index,
                total: words.len(),
            });
        }
        let spec = Self {
            fact_rel,
            group_keys,
            measures,
            fact_filter,
            star_dims,
            having,
        };
        spec.validate()?;
        let canonical = spec.encode_i32()?;
        if canonical != words {
            let index = canonical
                .iter()
                .zip(words)
                .position(|(expected, actual)| expected != actual)
                .unwrap_or(words.len().min(canonical.len()));
            return Err(SpecCodecError::NonCanonical { index });
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INT4_OID: u32 = 23;
    const INT8_OID: u32 = 20;
    const FLOAT8_OID: u32 = 701;
    const BOOL_OID: u32 = 16;

    const fn column(relation_oid: u32, attno: i32, type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid,
            attno,
            type_oid,
        }
    }

    const fn output(source: AggregateSource, kind: AggregateKind) -> AggregateOutput {
        AggregateOutput { source, kind }
    }

    fn minimal_spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn dense_spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 10,
            group_keys: vec![
                GroupKeyRef {
                    source: GroupKeySource::FactColumn(column(10, 1, INT4_OID)),
                    type_oid: INT4_OID,
                    collation_oid: 0,
                    encoding: GroupKeyEncoding::DenseI32 {
                        code_min: 1992,
                        cardinality: 8,
                        null_code: Some(1999),
                    },
                },
                GroupKeyRef {
                    source: GroupKeySource::StarDimension {
                        dim_index: 0,
                        group_column: column(20, 3, INT4_OID),
                    },
                    type_oid: INT4_OID,
                    collation_oid: 0,
                    encoding: GroupKeyEncoding::DictionaryI32 {
                        cardinality: 16,
                        null_code: None,
                    },
                },
                GroupKeyRef {
                    source: GroupKeySource::Expression {
                        inputs: vec![column(10, 2, INT4_OID)],
                        program: vec![0, 2, 40],
                    },
                    type_oid: INT4_OID,
                    collation_oid: 0,
                    encoding: GroupKeyEncoding::DenseI32 {
                        code_min: 0,
                        cardinality: 2,
                        null_code: Some(1),
                    },
                },
            ],
            measures: vec![
                MeasureSpec {
                    expression: MeasureExpr::Column(column(10, 4, FLOAT8_OID)),
                    outputs: vec![
                        output(AggregateSource::Value, AggregateKind::Sum),
                        output(AggregateSource::Value, AggregateKind::Count),
                        output(AggregateSource::Value, AggregateKind::Min),
                        output(AggregateSource::Value, AggregateKind::Max),
                        output(AggregateSource::Value, AggregateKind::Avg),
                        output(AggregateSource::Value, AggregateKind::StddevSamp),
                    ],
                    filter: FilterSpec::Ranges {
                        input: column(10, 4, FLOAT8_OID),
                        ranges: vec![ScalarRange {
                            lo: ScalarValue::F64(f64::NEG_INFINITY),
                            hi: ScalarValue::F64(f64::INFINITY),
                        }],
                    },
                },
                MeasureSpec {
                    expression: MeasureExpr::Binary {
                        op: BinaryMeasureOp::Mul,
                        lhs: column(10, 5, INT4_OID),
                        rhs: column(10, 6, INT4_OID),
                    },
                    outputs: vec![
                        output(AggregateSource::Value, AggregateKind::Sum),
                        output(AggregateSource::Value, AggregateKind::Count),
                    ],
                    filter: FilterSpec::Mask {
                        input: column(10, 7, BOOL_OID),
                        kind: MaskKind::Sql,
                    },
                },
                MeasureSpec {
                    expression: MeasureExpr::StatsPair {
                        value: column(10, 8, FLOAT8_OID),
                        rhs: column(10, 9, FLOAT8_OID),
                    },
                    outputs: vec![
                        output(AggregateSource::Value, AggregateKind::StddevSamp),
                        output(AggregateSource::Rhs, AggregateKind::Avg),
                    ],
                    filter: FilterSpec::Spatial {
                        function_oid: 42,
                        left: column(10, 10, 1_000),
                        right: column(20, 4, 1_000),
                        distance: Some(ScalarValue::F64(10.5)),
                    },
                },
                MeasureSpec {
                    expression: MeasureExpr::Bytecode {
                        inputs: vec![column(10, 11, INT8_OID)],
                        program: vec![0, 1, 11],
                        result_type_oid: INT8_OID,
                    },
                    outputs: vec![output(AggregateSource::Value, AggregateKind::Sum)],
                    filter: FilterSpec::None,
                },
            ],
            fact_filter: FilterSpec::Bytecode {
                inputs: vec![column(10, 12, INT8_OID)],
                program: vec![0, 1, 40],
            },
            star_dims: vec![DimSpec {
                relation_oid: 20,
                fact_key: column(10, 13, INT4_OID),
                dim_key: column(20, 1, INT4_OID),
                collation_oid: 0,
                multiplicity: JoinMultiplicity::Unique,
                filter: FilterSpec::Mask {
                    input: column(20, 2, BOOL_OID),
                    kind: MaskKind::Sql,
                },
            }],
            having: Some(HavingSpec {
                inputs: vec![
                    AggregateRef {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    },
                    AggregateRef {
                        measure_index: 1,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                ],
                program: vec![0, 1, 44],
            }),
        }
    }

    fn h3_spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 30,
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::H3Cell {
                    input: column(30, 1, INT8_OID),
                    resolution: 9,
                },
                type_oid: INT8_OID,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
                filter: FilterSpec::None,
            }],
            fact_filter: FilterSpec::Mask {
                input: column(30, 2, BOOL_OID),
                kind: MaskKind::Recheck,
            },
            star_dims: Vec::new(),
            having: None,
        }
    }

    fn maximal_program() -> Vec<i32> {
        vec![0; MAX_BYTECODE_WORDS]
    }

    fn maximal_inputs(relation_oid: u32) -> Vec<ColumnRef> {
        vec![column(relation_oid, 1, INT4_OID); MAX_PROGRAM_INPUTS]
    }

    fn maximal_filter(relation_oid: u32) -> FilterSpec {
        FilterSpec::Bytecode {
            inputs: maximal_inputs(relation_oid),
            program: maximal_program(),
        }
    }

    fn maximal_spec() -> AggQuerySpec {
        let group_key = GroupKeyRef {
            source: GroupKeySource::Expression {
                inputs: maximal_inputs(10),
                program: maximal_program(),
            },
            type_oid: INT4_OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::DenseI32 {
                code_min: 0,
                cardinality: 1,
                null_code: Some(0),
            },
        };
        let measure = MeasureSpec {
            expression: MeasureExpr::Bytecode {
                inputs: maximal_inputs(10),
                program: maximal_program(),
                result_type_oid: INT4_OID,
            },
            outputs: vec![
                output(AggregateSource::Value, AggregateKind::Sum),
                output(AggregateSource::Value, AggregateKind::Count),
                output(AggregateSource::Value, AggregateKind::Min),
                output(AggregateSource::Value, AggregateKind::Max),
                output(AggregateSource::Value, AggregateKind::Avg),
                output(AggregateSource::Value, AggregateKind::StddevSamp),
            ],
            filter: maximal_filter(10),
        };
        let star_dims = (0..abi::PGACCEL_GROUPED_AGG_MAX_DIMS)
            .map(|index| {
                let relation_oid = 20 + u32::try_from(index).expect("dimension index fits in u32");
                DimSpec {
                    relation_oid,
                    fact_key: column(10, 1, INT4_OID),
                    dim_key: column(relation_oid, 1, INT4_OID),
                    collation_oid: 0,
                    multiplicity: JoinMultiplicity::Unique,
                    filter: maximal_filter(relation_oid),
                }
            })
            .collect();

        AggQuerySpec {
            fact_rel: 10,
            group_keys: vec![group_key; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
            measures: vec![measure; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
            fact_filter: maximal_filter(10),
            star_dims,
            having: Some(HavingSpec {
                inputs: vec![
                    AggregateRef {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Sum,
                    };
                    MAX_PROGRAM_INPUTS
                ],
                program: maximal_program(),
            }),
        }
    }

    #[test]
    fn round_trips_minimal_dense_and_hash_specs() {
        for spec in [minimal_spec(), dense_spec(), h3_spec()] {
            let encoded = spec.encode_i32().expect("valid spec encodes");
            let decoded = AggQuerySpec::decode_i32(&encoded).expect("encoded spec decodes");
            assert_eq!(decoded, spec);
        }
    }

    #[test]
    fn group_and_dimension_logical_metadata_are_strict_and_bit_exact() {
        let mut spec = dense_spec();
        spec.group_keys[2].type_oid = 9_999;
        spec.group_keys[2].collation_oid = 0xF123_4567;
        spec.star_dims[0].collation_oid = 0x8765_4321;
        let encoded = spec.encode_i32().expect("logical metadata encodes");
        assert_eq!(
            AggQuerySpec::decode_i32(&encoded).expect("logical metadata decodes"),
            spec,
            "logical type and collation OIDs did not round-trip bit-exactly"
        );

        let mut invalid = dense_spec();
        invalid.group_keys[0].type_oid = INT8_OID;
        assert!(
            invalid.validate().is_err(),
            "fact group key accepted a logical type different from its column"
        );

        let mut invalid = dense_spec();
        invalid.group_keys[1].type_oid = INT8_OID;
        assert!(
            invalid.validate().is_err(),
            "dimension group key accepted a logical type different from its column"
        );

        let mut invalid = dense_spec();
        invalid.group_keys[2].type_oid = 0;
        assert!(
            invalid.validate().is_err(),
            "expression group key accepted an omitted logical type"
        );

        let mut explicit_h3 = h3_spec();
        explicit_h3.group_keys[0].type_oid = 9_999;
        explicit_h3
            .validate()
            .expect("H3 group key accepts an explicit analyzed result type");
        explicit_h3.group_keys[0].type_oid = 0;
        assert!(
            explicit_h3.validate().is_err(),
            "H3 group key accepted an omitted logical type"
        );
    }

    #[test]
    fn exported_max_words_is_reached_by_a_valid_maximal_spec() {
        assert_eq!(AGG_QUERY_SPEC_MAX_WORDS, 1_117_547);
        let spec = maximal_spec();
        let encoded = spec.encode_i32().expect("maximal valid spec encodes");
        assert_eq!(encoded.len(), AGG_QUERY_SPEC_MAX_WORDS);
        assert_eq!(
            AggQuerySpec::decode_i32(&encoded).expect("maximal valid spec decodes"),
            spec
        );
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        for spec in [minimal_spec(), dense_spec(), h3_spec()] {
            let encoded = spec.encode_i32().expect("valid spec encodes");
            for end in 0..encoded.len() {
                assert!(
                    AggQuerySpec::decode_i32(&encoded[..end]).is_err(),
                    "prefix {end} of {} words unexpectedly decoded",
                    encoded.len()
                );
            }
        }
    }

    #[test]
    fn every_truncated_byte_prefix_is_rejected() {
        fn decode_bytes(bytes: &[u8]) -> Result<AggQuerySpec, SpecCodecError> {
            if !bytes.len().is_multiple_of(std::mem::size_of::<i32>()) {
                return Err(SpecCodecError::Truncated {
                    index: bytes.len() / std::mem::size_of::<i32>(),
                    context: "partial wire word",
                });
            }
            let words = bytes
                .chunks_exact(std::mem::size_of::<i32>())
                .map(|chunk| i32::from_le_bytes(chunk.try_into().expect("four-byte chunk")))
                .collect::<Vec<_>>();
            AggQuerySpec::decode_i32(&words)
        }

        for spec in [minimal_spec(), dense_spec(), h3_spec()] {
            let words = spec.encode_i32().expect("valid spec encodes");
            let bytes = words
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>();
            assert_eq!(decode_bytes(&bytes).expect("full byte wire decodes"), spec);
            for end in 0..bytes.len() {
                assert!(
                    decode_bytes(&bytes[..end]).is_err(),
                    "byte prefix {end} of {} bytes unexpectedly decoded",
                    bytes.len()
                );
            }
        }
    }

    #[test]
    fn noncanonical_negative_zero_is_rejected() {
        let mut spec = minimal_spec();
        spec.fact_filter = FilterSpec::Ranges {
            input: column(10, 2, FLOAT8_OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::F64(0.0),
                hi: ScalarValue::F64(1.0),
            }],
        };
        let mut encoded = spec.encode_i32().expect("valid spec encodes");
        let scalar_tag = encoded
            .windows(3)
            .position(|words| words == [5, 0, 0])
            .expect("canonical f64 zero is present");
        encoded[scalar_tag + 1] = i32::MIN;
        assert!(matches!(
            AggQuerySpec::decode_i32(&encoded),
            Err(SpecCodecError::NonCanonical { index }) if index == scalar_tag + 1
        ));
    }

    #[test]
    fn deterministic_adversarial_words_never_panic_or_decode_noncanonically() {
        let mut state = 0x6A09_E667_F3BC_C909_u64;
        for case in 0..4_096usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let len = (state as usize) % 257;
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                words.push((state >> 32) as i32);
            }
            if case % 4 == 0 && words.len() >= AGG_QUERY_SPEC_HEADER_WORDS {
                words[0] = AGG_QUERY_SPEC_WIRE_MAGIC;
                words[1] = AGG_QUERY_SPEC_VERSION as i32;
                words[2] = i32::try_from(words.len()).expect("fuzz case length fits i32");
            }
            if let Ok(spec) = AggQuerySpec::decode_i32(&words) {
                assert_eq!(
                    spec.encode_i32().expect("decoded spec re-encodes"),
                    words,
                    "case {case} decoded from a noncanonical representation"
                );
            }
        }
    }

    #[test]
    fn corrupt_headers_tags_lengths_boole_and_trailing_words_are_rejected() {
        let encoded = minimal_spec().encode_i32().expect("valid spec encodes");

        let mut bad_magic = encoded.clone();
        bad_magic[0] ^= 1;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_magic),
            Err(SpecCodecError::InvalidMagic(_))
        ));

        let mut bad_version = encoded.clone();
        bad_version[1] = 99;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_version),
            Err(SpecCodecError::UnsupportedVersion(99))
        ));

        let mut bad_length = encoded.clone();
        bad_length[2] -= 1;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_length),
            Err(SpecCodecError::LengthMismatch { .. })
        ));

        let mut negative_count = encoded.clone();
        negative_count[4] = -1;
        assert!(matches!(
            AggQuerySpec::decode_i32(&negative_count),
            Err(SpecCodecError::InvalidValue { .. })
        ));

        let mut bad_expression_tag = encoded.clone();
        bad_expression_tag[6] = 99;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_expression_tag),
            Err(SpecCodecError::InvalidTag { .. })
        ));

        let mut bad_aggregate_tag = encoded.clone();
        bad_aggregate_tag[9] = 99;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_aggregate_tag),
            Err(SpecCodecError::InvalidTag { .. })
        ));

        let mut bad_presence = encoded.clone();
        let last = bad_presence.len() - 1;
        bad_presence[last] = 2;
        assert!(matches!(
            AggQuerySpec::decode_i32(&bad_presence),
            Err(SpecCodecError::InvalidValue { .. })
        ));

        let mut trailing = encoded;
        trailing.push(123);
        trailing[2] = i32::try_from(trailing.len()).expect("test encoding length fits i32");
        assert!(matches!(
            AggQuerySpec::decode_i32(&trailing),
            Err(SpecCodecError::TrailingWords { .. })
        ));
    }

    #[test]
    fn decoded_but_semantically_invalid_spec_is_rejected() {
        let mut encoded = h3_spec().encode_i32().expect("valid spec encodes");
        // H3 resolution follows source tag + three-word column at word 9.
        encoded[9] = 16;
        assert!(matches!(
            AggQuerySpec::decode_i32(&encoded),
            Err(SpecCodecError::InvalidSpec(_))
        ));

        let mut encoded = dense_spec().encode_i32().expect("valid spec encodes");
        // The first dense key's explicit NULL code follows its presence bit.
        encoded[13] = i32::MIN;
        assert!(matches!(
            AggQuerySpec::decode_i32(&encoded),
            Err(SpecCodecError::InvalidSpec(_))
        ));
    }

    #[test]
    fn prefix_length_validates_framing_without_rejecting_following_words() {
        let encoded = dense_spec().encode_i32().expect("valid spec encodes");
        let mut framed = encoded.clone();
        framed.extend([17, 18, 19]);
        assert_eq!(
            AggQuerySpec::encoded_i32_prefix_len(&framed).expect("prefix is valid"),
            encoded.len()
        );
        assert!(matches!(
            AggQuerySpec::decode_i32(&framed),
            Err(SpecCodecError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn source_aware_outputs_and_exact_having_references_are_validated() {
        let mut spec = dense_spec();
        spec.measures[1]
            .outputs
            .push(output(AggregateSource::Rhs, AggregateKind::Sum));
        assert!(
            spec.validate().is_err(),
            "non-STATS_PAIR RHS output accepted"
        );

        let mut spec = dense_spec();
        spec.measures[2]
            .outputs
            .push(output(AggregateSource::Rhs, AggregateKind::StddevSamp));
        assert!(
            spec.validate().is_err(),
            "unsupported RHS SUMSQ lane accepted"
        );

        let mut spec = dense_spec();
        spec.having.as_mut().expect("fixture has HAVING").inputs[0].kind = AggregateKind::Min;
        assert!(
            spec.validate().is_ok(),
            "HAVING should accept another projected primary output"
        );
        spec.having.as_mut().expect("fixture has HAVING").inputs[0].source = AggregateSource::Rhs;
        assert!(
            spec.validate().is_err(),
            "HAVING accepted an output absent from the measure"
        );
    }

    #[test]
    fn ranges_must_match_the_input_oid_and_preserve_float_edges() {
        let mut spec = minimal_spec();
        spec.fact_filter = FilterSpec::Ranges {
            input: column(10, 2, FLOAT8_OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::F64(f64::NEG_INFINITY),
                hi: ScalarValue::F64(f64::INFINITY),
            }],
        };
        assert!(spec.validate().is_ok(), "ordered infinities were rejected");

        if let FilterSpec::Ranges { ranges, .. } = &mut spec.fact_filter {
            ranges[0].hi = ScalarValue::I64(i64::MAX);
        }
        assert!(spec.validate().is_err(), "mixed endpoints were accepted");
        if let FilterSpec::Ranges { ranges, .. } = &mut spec.fact_filter {
            ranges[0].lo = ScalarValue::I64(0);
        }
        assert!(
            spec.validate().is_err(),
            "endpoints mismatching the input OID were accepted"
        );
        if let FilterSpec::Ranges { ranges, .. } = &mut spec.fact_filter {
            ranges[0] = ScalarRange {
                lo: ScalarValue::F64(f64::NAN),
                hi: ScalarValue::F64(f64::INFINITY),
            };
        }
        assert!(spec.validate().is_err(), "NaN endpoint was accepted");
        if let FilterSpec::Ranges { ranges, .. } = &mut spec.fact_filter {
            ranges[0] = ScalarRange {
                lo: ScalarValue::F64(f64::INFINITY),
                hi: ScalarValue::F64(f64::NEG_INFINITY),
            };
        }
        assert!(
            spec.validate().is_err(),
            "reversed infinities were accepted"
        );
    }

    #[test]
    fn float4_ranges_round_trip_without_widening() {
        let mut spec = minimal_spec();
        spec.fact_filter = FilterSpec::Ranges {
            input: column(10, 2, 700),
            ranges: vec![ScalarRange {
                lo: ScalarValue::F32(f32::NEG_INFINITY),
                hi: ScalarValue::F32(f32::INFINITY),
            }],
        };
        let encoded = spec.encode_i32().expect("valid float4 range encodes");
        assert_eq!(
            AggQuerySpec::decode_i32(&encoded).expect("float4 range decodes"),
            spec
        );
    }

    #[test]
    fn every_wire_count_is_schema_bounded_before_allocation() {
        let mut wire_too_large = vec![
            AGG_QUERY_SPEC_WIRE_MAGIC,
            AGG_QUERY_SPEC_VERSION as i32,
            i32::try_from(AGG_QUERY_SPEC_MAX_WORDS + 1).expect("test limit fits i32"),
        ];
        assert!(matches!(
            AggQuerySpec::decode_i32(&wire_too_large),
            Err(SpecCodecError::LimitExceeded {
                context: "wire length",
                ..
            })
        ));

        let encoded = minimal_spec().encode_i32().expect("valid spec encodes");
        for (index, value, context) in [
            (4, 4, "group-key count"),
            (5, 5, "measure count"),
            (7, 10, "aggregate output count"),
            (12, 5, "dimension count"),
        ] {
            let mut oversized = encoded.clone();
            oversized[index] = value;
            assert!(matches!(
                AggQuerySpec::decode_i32(&oversized),
                Err(SpecCodecError::LimitExceeded {
                    context: actual,
                    ..
                }) if actual == context
            ));
        }

        let mut oversized_having = encoded;
        oversized_having[13] = 1;
        oversized_having.push(65);
        oversized_having[2] =
            i32::try_from(oversized_having.len()).expect("test encoding length fits i32");
        assert!(matches!(
            AggQuerySpec::decode_i32(&oversized_having),
            Err(SpecCodecError::LimitExceeded {
                context: "HAVING input count",
                ..
            })
        ));

        wire_too_large[2] = 2;
        assert!(matches!(
            AggQuerySpec::encoded_i32_prefix_len(&wire_too_large),
            Err(SpecCodecError::InvalidValue {
                context: "wire length",
                ..
            })
        ));
    }

    #[test]
    fn nested_decoder_counts_are_bounded_before_reserve_or_copy() {
        let mut bytecode_spec = minimal_spec();
        bytecode_spec.measures[0] = MeasureSpec {
            expression: MeasureExpr::Bytecode {
                inputs: vec![column(10, 2, INT4_OID)],
                program: vec![0],
                result_type_oid: INT4_OID,
            },
            outputs: vec![output(AggregateSource::Value, AggregateKind::Sum)],
            filter: FilterSpec::None,
        };
        let encoded = bytecode_spec.encode_i32().expect("valid spec encodes");

        let mut oversized_columns = encoded.clone();
        oversized_columns[7] = 65;
        assert!(matches!(
            AggQuerySpec::decode_i32(&oversized_columns),
            Err(SpecCodecError::LimitExceeded {
                context: "column count",
                ..
            })
        ));

        let mut oversized_program = encoded;
        oversized_program[11] = 65_537;
        assert!(matches!(
            AggQuerySpec::decode_i32(&oversized_program),
            Err(SpecCodecError::LimitExceeded {
                context: "measure bytecode word count",
                ..
            })
        ));

        let mut range_spec = minimal_spec();
        range_spec.fact_filter = FilterSpec::Ranges {
            input: column(10, 2, INT4_OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(0),
                hi: ScalarValue::I32(1),
            }],
        };
        let mut oversized_ranges = range_spec.encode_i32().expect("valid spec encodes");
        oversized_ranges[15] = 5;
        assert!(matches!(
            AggQuerySpec::decode_i32(&oversized_ranges),
            Err(SpecCodecError::LimitExceeded {
                context: "range count",
                ..
            })
        ));
    }

    #[test]
    fn relation_scope_join_types_and_grouped_dimension_uniqueness_are_validated() {
        let mut spec = dense_spec();
        spec.star_dims[0].dim_key.type_oid = INT8_OID;
        assert!(
            spec.validate().is_err(),
            "mismatched join-key OIDs accepted"
        );

        let mut spec = dense_spec();
        spec.star_dims[0].multiplicity = JoinMultiplicity::Counted;
        assert!(
            spec.validate().is_err(),
            "counted dimension used as a group source was accepted"
        );

        let mut spec = dense_spec();
        spec.measures[0].expression = MeasureExpr::Column(column(99, 1, FLOAT8_OID));
        assert!(
            spec.validate().is_err(),
            "measure from an unrelated relation was accepted"
        );

        let mut spec = dense_spec();
        spec.fact_filter = FilterSpec::Mask {
            input: column(99, 1, BOOL_OID),
            kind: MaskKind::Sql,
        };
        assert!(
            spec.validate().is_err(),
            "filter from an unrelated relation was accepted"
        );

        let mut self_dimension = minimal_spec();
        self_dimension.star_dims.push(DimSpec {
            relation_oid: self_dimension.fact_rel,
            fact_key: column(10, 2, INT4_OID),
            dim_key: column(10, 1, INT4_OID),
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::None,
        });
        assert!(
            self_dimension.validate().is_err(),
            "dimension sharing the fact relation OID was accepted"
        );

        let mut duplicate_dimensions = minimal_spec();
        let dimension = DimSpec {
            relation_oid: 20,
            fact_key: column(10, 2, INT4_OID),
            dim_key: column(20, 1, INT4_OID),
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::None,
        };
        duplicate_dimensions.star_dims = vec![dimension.clone(), dimension];
        assert!(
            duplicate_dimensions.validate().is_err(),
            "duplicate dimension relation OIDs were accepted"
        );
    }

    #[test]
    fn measure_filter_cannot_collapse_two_dimension_rows_to_one_fact_mask() {
        let mut spec = minimal_spec();
        spec.star_dims.push(DimSpec {
            relation_oid: 20,
            fact_key: column(10, 2, INT4_OID),
            dim_key: column(20, 1, INT4_OID),
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Counted,
            filter: FilterSpec::None,
        });
        spec.measures[0].filter = FilterSpec::Mask {
            input: column(20, 2, BOOL_OID),
            kind: MaskKind::Sql,
        };

        // One fact key may join two dimension rows with different mask values;
        // one fact-row mask cannot preserve both aggregate FILTER outcomes.
        assert!(
            spec.validate().is_err(),
            "counted-dimension measure FILTER was accepted"
        );
    }
}

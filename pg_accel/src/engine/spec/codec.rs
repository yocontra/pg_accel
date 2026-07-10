//! Strict versioned `i32` wire codec for [`AggQuerySpec`].

use super::{
    AGG_QUERY_SPEC_VERSION, AggQuerySpec, AggregateKind, BinaryMeasureOp, ColumnRef, DimSpec,
    FilterSpec, GroupKeyEncoding, GroupKeyRef, GroupKeySource, HavingSpec, JoinMultiplicity,
    MaskKind, MeasureExpr, MeasureSpec, ScalarRange, ScalarValue, SpecValidationError,
};

const WIRE_MAGIC: i32 = 0x5047_4132; // "PGA2"
const HEADER_WORDS: usize = 3;

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
            words: vec![WIRE_MAGIC, AGG_QUERY_SPEC_VERSION as i32, 0],
        }
    }

    fn finish(mut self) -> Result<Vec<i32>, SpecCodecError> {
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
        let bits = value.to_bits();
        self.push((bits >> 32) as i32);
        self.push(bits as u32 as i32);
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
            ScalarValue::F64(value) => {
                self.push(4);
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
        self.push_len(measure.aggregates.len())?;
        for aggregate in &measure.aggregates {
            self.push(match aggregate {
                AggregateKind::Sum => 0,
                AggregateKind::Count => 1,
                AggregateKind::Min => 2,
                AggregateKind::Max => 3,
                AggregateKind::Avg => 4,
                AggregateKind::Stddev => 5,
            });
        }
        self.push_filter(&measure.filter)
    }

    fn push_dim(&mut self, dim: &DimSpec) -> Result<(), SpecCodecError> {
        self.push_u32(dim.relation_oid);
        self.push_column(dim.fact_key);
        self.push_column(dim.dim_key);
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

    fn read_len(&mut self, context: &'static str) -> Result<usize, SpecCodecError> {
        let index = self.index;
        let raw = self.read(context)?;
        let len =
            usize::try_from(raw).map_err(|_| SpecCodecError::InvalidValue { index, context })?;
        if len > self.words.len().saturating_sub(self.index) {
            return Err(SpecCodecError::Truncated {
                index: self.index,
                context,
            });
        }
        Ok(len)
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

    fn read_column(&mut self) -> Result<ColumnRef, SpecCodecError> {
        Ok(ColumnRef {
            relation_oid: self.read_u32("column relation OID")?,
            attno: self.read("column attribute number")?,
            type_oid: self.read_u32("column type OID")?,
        })
    }

    fn read_columns(&mut self) -> Result<Vec<ColumnRef>, SpecCodecError> {
        let len = self.read_len("column count")?;
        let mut columns = Vec::with_capacity(len);
        for _ in 0..len {
            columns.push(self.read_column()?);
        }
        Ok(columns)
    }

    fn read_words(&mut self, context: &'static str) -> Result<Vec<i32>, SpecCodecError> {
        let len = self.read_len(context)?;
        let end = self.index + len;
        let values = self.words[self.index..end].to_vec();
        self.index = end;
        Ok(values)
    }

    fn read_scalar(&mut self) -> Result<ScalarValue, SpecCodecError> {
        match self.read_tag("scalar", 4)? {
            1 => Ok(ScalarValue::Bool(self.read_bool("boolean scalar")?)),
            2 => Ok(ScalarValue::I32(self.read("i32 scalar")?)),
            3 => Ok(ScalarValue::I64(self.read_i64("i64 scalar")?)),
            4 => Ok(ScalarValue::F64(self.read_f64("f64 scalar")?)),
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
                let len = self.read_len("range count")?;
                let mut ranges = Vec::with_capacity(len);
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
        Ok(GroupKeyRef { source, encoding })
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
        let aggregate_count = self.read_len("aggregate lane count")?;
        let mut aggregates = Vec::with_capacity(aggregate_count);
        for _ in 0..aggregate_count {
            aggregates.push(match self.read_tag("aggregate lane", 5)? {
                0 => AggregateKind::Sum,
                1 => AggregateKind::Count,
                2 => AggregateKind::Min,
                3 => AggregateKind::Max,
                4 => AggregateKind::Avg,
                5 => AggregateKind::Stddev,
                _ => unreachable!(),
            });
        }
        Ok(MeasureSpec {
            expression,
            aggregates,
            filter: self.read_filter()?,
        })
    }

    fn read_dim(&mut self) -> Result<DimSpec, SpecCodecError> {
        let relation_oid = self.read_u32("dimension relation OID")?;
        let fact_key = self.read_column()?;
        let dim_key = self.read_column()?;
        let multiplicity = match self.read_tag("join multiplicity", 1)? {
            0 => JoinMultiplicity::Unique,
            1 => JoinMultiplicity::Counted,
            _ => unreachable!(),
        };
        Ok(DimSpec {
            relation_oid,
            fact_key,
            dim_key,
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
            encoder.push_len(having.input_slots.len())?;
            for slot in &having.input_slots {
                encoder.push_u32(*slot);
            }
            encoder.push_words(&having.program)?;
        }
        encoder.finish()
    }

    pub fn decode_i32(words: &[i32]) -> Result<Self, SpecCodecError> {
        let mut decoder = Decoder::new(words);
        let magic = decoder.read("wire magic")?;
        if magic != WIRE_MAGIC {
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
        if declared != words.len() {
            return Err(SpecCodecError::LengthMismatch {
                declared,
                actual: words.len(),
            });
        }
        if declared < HEADER_WORDS {
            return Err(SpecCodecError::InvalidValue {
                index: declared_index,
                context: "wire length",
            });
        }
        let fact_rel = decoder.read_u32("fact relation OID")?;
        let key_count = decoder.read_len("group-key count")?;
        let mut group_keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            group_keys.push(decoder.read_group_key()?);
        }
        let measure_count = decoder.read_len("measure count")?;
        let mut measures = Vec::with_capacity(measure_count);
        for _ in 0..measure_count {
            measures.push(decoder.read_measure()?);
        }
        let fact_filter = decoder.read_filter()?;
        let dim_count = decoder.read_len("dimension count")?;
        let mut star_dims = Vec::with_capacity(dim_count);
        for _ in 0..dim_count {
            star_dims.push(decoder.read_dim()?);
        }
        let having = if decoder.read_bool("HAVING presence")? {
            let input_count = decoder.read_len("HAVING input count")?;
            let mut input_slots = Vec::with_capacity(input_count);
            for _ in 0..input_count {
                input_slots.push(decoder.read_u32("HAVING input slot")?);
            }
            Some(HavingSpec {
                input_slots,
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

    fn minimal_spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 10,
            group_keys: Vec::new(),
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                aggregates: vec![AggregateKind::Count],
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
                    aggregates: vec![
                        AggregateKind::Sum,
                        AggregateKind::Count,
                        AggregateKind::Min,
                        AggregateKind::Max,
                        AggregateKind::Avg,
                        AggregateKind::Stddev,
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
                    aggregates: vec![AggregateKind::Sum, AggregateKind::Count],
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
                    aggregates: vec![AggregateKind::Avg, AggregateKind::Stddev],
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
                    aggregates: vec![AggregateKind::Sum],
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
                multiplicity: JoinMultiplicity::Counted,
                filter: FilterSpec::Mask {
                    input: column(20, 2, BOOL_OID),
                    kind: MaskKind::Sql,
                },
            }],
            having: Some(HavingSpec {
                input_slots: vec![0, 1],
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
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                aggregates: vec![AggregateKind::Count],
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

    #[test]
    fn round_trips_minimal_dense_and_hash_specs() {
        for spec in [minimal_spec(), dense_spec(), h3_spec()] {
            let encoded = spec.encode_i32().expect("valid spec encodes");
            let decoded = AggQuerySpec::decode_i32(&encoded).expect("encoded spec decodes");
            assert_eq!(decoded, spec);
        }
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let encoded = dense_spec().encode_i32().expect("valid spec encodes");
        for end in 0..encoded.len() {
            assert!(
                AggQuerySpec::decode_i32(&encoded[..end]).is_err(),
                "prefix {end} unexpectedly decoded"
            );
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
        bad_aggregate_tag[8] = 99;
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
}

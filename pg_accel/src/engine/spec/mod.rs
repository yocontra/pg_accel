//! Neutral planner specification and frozen grouped-aggregate ABI.
//!
//! Planner-facing types contain relation/column identities and logical
//! operations only. Device pointers live exclusively in [`abi`]. Phase 5 can
//! therefore analyze and serialize a query before residency exists.

pub mod abi;
mod codec;

pub use codec::{
    AGG_QUERY_SPEC_HEADER_WORDS, AGG_QUERY_SPEC_MAX_WORDS, AGG_QUERY_SPEC_WIRE_MAGIC,
    SpecCodecError,
};

pub const AGG_QUERY_SPEC_VERSION: u32 = 2;

const MAX_BYTECODE_WORDS: usize = 65_536;
const MAX_PROGRAM_INPUTS: usize = 64;
const MAX_AGGREGATE_OUTPUTS: usize = 9;

const BOOLOID: u32 = 16;
const INT8OID: u32 = 20;
const INT4OID: u32 = 23;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnRef {
    pub relation_oid: u32,
    pub attno: i32,
    pub type_oid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScalarValue {
    Bool(bool),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarRange {
    pub lo: ScalarValue,
    pub hi: ScalarValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskKind {
    Sql,
    Recheck,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterSpec {
    None,
    Ranges {
        input: ColumnRef,
        ranges: Vec<ScalarRange>,
    },
    Mask {
        input: ColumnRef,
        kind: MaskKind,
    },
    Bytecode {
        inputs: Vec<ColumnRef>,
        program: Vec<i32>,
    },
    Spatial {
        function_oid: u32,
        left: ColumnRef,
        right: ColumnRef,
        distance: Option<ScalarValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKeySource {
    FactColumn(ColumnRef),
    StarDimension {
        dim_index: u32,
        group_column: ColumnRef,
    },
    Expression {
        inputs: Vec<ColumnRef>,
        program: Vec<i32>,
    },
    H3Cell {
        input: ColumnRef,
        resolution: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKeyEncoding {
    DenseI32 {
        code_min: i32,
        cardinality: u32,
        null_code: Option<i32>,
    },
    DictionaryI32 {
        cardinality: u32,
        null_code: Option<i32>,
    },
    Hash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupKeyRef {
    pub source: GroupKeySource,
    pub encoding: GroupKeyEncoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryMeasureOp {
    Mul,
    Sub,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasureExpr {
    CountStar,
    Column(ColumnRef),
    Binary {
        op: BinaryMeasureOp,
        lhs: ColumnRef,
        rhs: ColumnRef,
    },
    StatsPair {
        value: ColumnRef,
        rhs: ColumnRef,
    },
    Bytecode {
        inputs: Vec<ColumnRef>,
        program: Vec<i32>,
        result_type_oid: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Sum,
    Count,
    Min,
    Max,
    Avg,
    Stddev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateSource {
    Value,
    Rhs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateOutput {
    pub source: AggregateSource,
    pub kind: AggregateKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeasureSpec {
    pub expression: MeasureExpr,
    pub outputs: Vec<AggregateOutput>,
    pub filter: FilterSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMultiplicity {
    Unique,
    Counted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimSpec {
    pub relation_oid: u32,
    pub fact_key: ColumnRef,
    pub dim_key: ColumnRef,
    pub multiplicity: JoinMultiplicity,
    pub filter: FilterSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateRef {
    pub measure_index: u32,
    pub source: AggregateSource,
    pub kind: AggregateKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HavingSpec {
    pub inputs: Vec<AggregateRef>,
    pub program: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggQuerySpec {
    pub fact_rel: u32,
    pub group_keys: Vec<GroupKeyRef>,
    pub measures: Vec<MeasureSpec>,
    pub fact_filter: FilterSpec,
    pub star_dims: Vec<DimSpec>,
    pub having: Option<HavingSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecValidationError {
    reason: &'static str,
}

impl SpecValidationError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl std::fmt::Display for SpecValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason)
    }
}

impl std::error::Error for SpecValidationError {}

impl ColumnRef {
    fn validate(self) -> Result<(), SpecValidationError> {
        if self.relation_oid == 0 || self.attno <= 0 || self.type_oid == 0 {
            return Err(SpecValidationError::new("invalid column reference"));
        }
        Ok(())
    }
}

impl ScalarValue {
    const fn type_oid(self) -> u32 {
        match self {
            Self::Bool(_) => BOOLOID,
            Self::I32(_) => INT4OID,
            Self::I64(_) => INT8OID,
            Self::F32(_) => FLOAT4OID,
            Self::F64(_) => FLOAT8OID,
        }
    }

    fn is_nan(self) -> bool {
        matches!(self, Self::F32(value) if value.is_nan())
            || matches!(self, Self::F64(value) if value.is_nan())
    }

    fn is_nonnegative_finite(self) -> bool {
        match self {
            Self::Bool(_) => false,
            Self::I32(value) => value >= 0,
            Self::I64(value) => value >= 0,
            Self::F32(value) => value.is_finite() && value >= 0.0,
            Self::F64(value) => value.is_finite() && value >= 0.0,
        }
    }
}

impl ScalarRange {
    fn validate(self, input_type_oid: u32) -> Result<(), SpecValidationError> {
        if self.lo.type_oid() != input_type_oid
            || self.hi.type_oid() != input_type_oid
            || self.lo.is_nan()
            || self.hi.is_nan()
        {
            return Err(SpecValidationError::new(
                "range endpoint type does not match input column",
            ));
        }
        let ordered = match (self.lo, self.hi) {
            (ScalarValue::Bool(lo), ScalarValue::Bool(hi)) => !lo || hi,
            (ScalarValue::I32(lo), ScalarValue::I32(hi)) => lo <= hi,
            (ScalarValue::I64(lo), ScalarValue::I64(hi)) => lo <= hi,
            (ScalarValue::F32(lo), ScalarValue::F32(hi)) => lo <= hi,
            (ScalarValue::F64(lo), ScalarValue::F64(hi)) => lo <= hi,
            _ => false,
        };
        if !ordered {
            return Err(SpecValidationError::new(
                "range lower bound exceeds upper bound",
            ));
        }
        Ok(())
    }
}

fn validate_program(inputs: &[ColumnRef], program: &[i32]) -> Result<(), SpecValidationError> {
    if inputs.len() > MAX_PROGRAM_INPUTS || program.is_empty() || program.len() > MAX_BYTECODE_WORDS
    {
        return Err(SpecValidationError::new("invalid bytecode program shape"));
    }
    for input in inputs {
        input.validate()?;
    }
    Ok(())
}

impl FilterSpec {
    fn validate(&self) -> Result<(), SpecValidationError> {
        match self {
            Self::None => Ok(()),
            Self::Ranges { input, ranges } => {
                input.validate()?;
                if ranges.is_empty() || ranges.len() > abi::PGACCEL_GROUPED_AGG_MAX_FILTER_RANGES {
                    return Err(SpecValidationError::new("invalid filter range count"));
                }
                for range in ranges {
                    range.validate(input.type_oid)?;
                }
                Ok(())
            }
            Self::Mask { input, .. } => input.validate(),
            Self::Bytecode { inputs, program } => validate_program(inputs, program),
            Self::Spatial {
                function_oid,
                left,
                right,
                distance,
            } => {
                if *function_oid == 0 {
                    return Err(SpecValidationError::new("invalid spatial function OID"));
                }
                left.validate()?;
                right.validate()?;
                if distance.is_some_and(|value| !value.is_nonnegative_finite()) {
                    return Err(SpecValidationError::new("invalid spatial distance"));
                }
                Ok(())
            }
        }
    }

    fn validate_relation_scope(
        &self,
        relation_allowed: impl Fn(u32) -> bool,
    ) -> Result<(), SpecValidationError> {
        self.validate()?;
        let valid = match self {
            Self::None => true,
            Self::Ranges { input, .. } | Self::Mask { input, .. } => {
                relation_allowed(input.relation_oid)
            }
            Self::Bytecode { inputs, .. } => inputs
                .iter()
                .all(|input| relation_allowed(input.relation_oid)),
            Self::Spatial { left, right, .. } => {
                relation_allowed(left.relation_oid) && relation_allowed(right.relation_oid)
            }
        };
        if !valid {
            return Err(SpecValidationError::new(
                "filter references an unrelated relation",
            ));
        }
        Ok(())
    }
}

impl GroupKeyEncoding {
    fn validate(self) -> Result<(), SpecValidationError> {
        let (code_min, cardinality, null_code) = match self {
            Self::DenseI32 {
                code_min,
                cardinality,
                null_code,
            } => (code_min, cardinality, null_code),
            Self::DictionaryI32 {
                cardinality,
                null_code,
            } => (0, cardinality, null_code),
            Self::Hash => return Ok(()),
        };
        if cardinality == 0 {
            return Err(SpecValidationError::new("zero group-key cardinality"));
        }
        if null_code == Some(abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE) {
            return Err(SpecValidationError::new(
                "NULL code conflicts with the ABI no-NULL sentinel",
            ));
        }
        let end = i64::from(code_min) + i64::from(cardinality);
        if end > i64::from(i32::MAX) + 1 {
            return Err(SpecValidationError::new(
                "group-key code range overflows i32",
            ));
        }
        if let Some(null_code) = null_code
            && (i64::from(null_code) < i64::from(code_min) || i64::from(null_code) >= end)
        {
            return Err(SpecValidationError::new("NULL code is outside key range"));
        }
        Ok(())
    }
}

impl GroupKeyRef {
    fn validate(&self, fact_rel: u32, dims: &[DimSpec]) -> Result<(), SpecValidationError> {
        self.encoding.validate()?;
        match &self.source {
            GroupKeySource::FactColumn(column) => {
                column.validate()?;
                if column.relation_oid != fact_rel {
                    return Err(SpecValidationError::new(
                        "fact key belongs to another relation",
                    ));
                }
                Ok(())
            }
            GroupKeySource::StarDimension {
                dim_index,
                group_column,
            } => {
                let Ok(index) = usize::try_from(*dim_index) else {
                    return Err(SpecValidationError::new(
                        "group key references missing dimension",
                    ));
                };
                let Some(dim) = dims.get(index) else {
                    return Err(SpecValidationError::new(
                        "group key references missing dimension",
                    ));
                };
                group_column.validate()?;
                if group_column.relation_oid != dim.relation_oid {
                    return Err(SpecValidationError::new(
                        "dimension key belongs to another relation",
                    ));
                }
                if dim.multiplicity != JoinMultiplicity::Unique {
                    return Err(SpecValidationError::new("grouping dimension is not unique"));
                }
                Ok(())
            }
            GroupKeySource::Expression { inputs, program } => {
                validate_program(inputs, program)?;
                if inputs.iter().any(|input| input.relation_oid != fact_rel) {
                    return Err(SpecValidationError::new(
                        "group-key expression references a non-fact relation",
                    ));
                }
                Ok(())
            }
            GroupKeySource::H3Cell { input, resolution } => {
                input.validate()?;
                if input.relation_oid != fact_rel
                    || !(0..=15).contains(resolution)
                    || self.encoding != GroupKeyEncoding::Hash
                {
                    return Err(SpecValidationError::new("invalid H3 group key"));
                }
                Ok(())
            }
        }
    }
}

impl MeasureSpec {
    fn validate(&self, fact_rel: u32, dims: &[DimSpec]) -> Result<(), SpecValidationError> {
        if self.outputs.is_empty() || self.outputs.len() > MAX_AGGREGATE_OUTPUTS {
            return Err(SpecValidationError::new("invalid aggregate output count"));
        }
        for (index, output) in self.outputs.iter().enumerate() {
            if self.outputs[..index].contains(output) {
                return Err(SpecValidationError::new("duplicate aggregate output"));
            }
        }
        match &self.expression {
            MeasureExpr::CountStar => {
                if self.outputs.as_slice()
                    != [AggregateOutput {
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    }]
                {
                    return Err(SpecValidationError::new(
                        "COUNT(*) slot has an invalid output",
                    ));
                }
            }
            MeasureExpr::Column(column) => {
                column.validate()?;
                if column.relation_oid != fact_rel {
                    return Err(SpecValidationError::new(
                        "measure references a non-fact relation",
                    ));
                }
            }
            MeasureExpr::Binary { lhs, rhs, .. } | MeasureExpr::StatsPair { value: lhs, rhs } => {
                lhs.validate()?;
                rhs.validate()?;
                if lhs.relation_oid != fact_rel || rhs.relation_oid != fact_rel {
                    return Err(SpecValidationError::new(
                        "measure references a non-fact relation",
                    ));
                }
            }
            MeasureExpr::Bytecode {
                inputs,
                program,
                result_type_oid,
            } => {
                validate_program(inputs, program)?;
                if *result_type_oid == 0 {
                    return Err(SpecValidationError::new(
                        "bytecode measure has invalid result type",
                    ));
                }
                if inputs.iter().any(|input| input.relation_oid != fact_rel) {
                    return Err(SpecValidationError::new(
                        "measure references a non-fact relation",
                    ));
                }
            }
        }
        for output in &self.outputs {
            if output.source == AggregateSource::Rhs {
                if !matches!(self.expression, MeasureExpr::StatsPair { .. }) {
                    return Err(SpecValidationError::new(
                        "RHS aggregate output requires STATS_PAIR",
                    ));
                }
                if !matches!(
                    output.kind,
                    AggregateKind::Sum | AggregateKind::Count | AggregateKind::Avg
                ) {
                    return Err(SpecValidationError::new(
                        "aggregate kind has no RHS ABI lane",
                    ));
                }
            }
        }
        self.filter.validate_relation_scope(|relation_oid| {
            relation_oid == fact_rel || dims.iter().any(|dim| dim.relation_oid == relation_oid)
        })
    }
}

impl DimSpec {
    fn validate(&self, fact_rel: u32) -> Result<(), SpecValidationError> {
        if self.relation_oid == 0 {
            return Err(SpecValidationError::new("invalid dimension relation OID"));
        }
        self.fact_key.validate()?;
        self.dim_key.validate()?;
        if self.fact_key.relation_oid != fact_rel || self.dim_key.relation_oid != self.relation_oid
        {
            return Err(SpecValidationError::new(
                "join key belongs to another relation",
            ));
        }
        if self.fact_key.type_oid != self.dim_key.type_oid {
            return Err(SpecValidationError::new("join key type OIDs do not match"));
        }
        self.filter
            .validate_relation_scope(|relation_oid| relation_oid == self.relation_oid)
    }
}

impl HavingSpec {
    fn validate(&self, measures: &[MeasureSpec]) -> Result<(), SpecValidationError> {
        if self.program.is_empty()
            || self.program.len() > MAX_BYTECODE_WORDS
            || self.inputs.len() > MAX_PROGRAM_INPUTS
        {
            return Err(SpecValidationError::new("invalid HAVING program"));
        }
        for input in &self.inputs {
            let Ok(index) = usize::try_from(input.measure_index) else {
                return Err(SpecValidationError::new(
                    "HAVING references a missing aggregate output",
                ));
            };
            let Some(measure) = measures.get(index) else {
                return Err(SpecValidationError::new(
                    "HAVING references a missing aggregate output",
                ));
            };
            if !measure.outputs.contains(&AggregateOutput {
                source: input.source,
                kind: input.kind,
            }) {
                return Err(SpecValidationError::new(
                    "HAVING references a missing aggregate output",
                ));
            }
        }
        Ok(())
    }
}

impl AggQuerySpec {
    pub fn validate(&self) -> Result<(), SpecValidationError> {
        if self.fact_rel == 0
            || self.group_keys.len() > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
            || self.measures.is_empty()
            || self.measures.len() > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
            || self.star_dims.len() > abi::PGACCEL_GROUPED_AGG_MAX_DIMS
        {
            return Err(SpecValidationError::new("invalid aggregate query shape"));
        }
        for dim in &self.star_dims {
            dim.validate(self.fact_rel)?;
        }
        let mut dense_product = 1usize;
        let mut has_hash_key = false;
        for key in &self.group_keys {
            key.validate(self.fact_rel, &self.star_dims)?;
            match key.encoding {
                GroupKeyEncoding::DenseI32 { cardinality, .. }
                | GroupKeyEncoding::DictionaryI32 { cardinality, .. } => {
                    dense_product = dense_product
                        .checked_mul(cardinality as usize)
                        .ok_or_else(|| SpecValidationError::new("mixed-radix product overflow"))?;
                }
                GroupKeyEncoding::Hash => has_hash_key = true,
            }
        }
        if has_hash_key
            && self
                .group_keys
                .iter()
                .any(|key| key.encoding != GroupKeyEncoding::Hash)
        {
            return Err(SpecValidationError::new("mixed dense/hash key encodings"));
        }
        for measure in &self.measures {
            measure.validate(self.fact_rel, &self.star_dims)?;
        }
        self.fact_filter.validate_relation_scope(|relation_oid| {
            relation_oid == self.fact_rel
                || self
                    .star_dims
                    .iter()
                    .any(|dim| dim.relation_oid == relation_oid)
        })?;
        if let Some(having) = &self.having {
            having.validate(&self.measures)?;
        }
        let _ = dense_product;
        Ok(())
    }
}

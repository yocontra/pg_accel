//! Neutral planner specification and frozen grouped-aggregate ABI.
//!
//! Planner-facing types contain relation/column identities and logical
//! operations only. [`AggOutputProjection`] separately describes ordered
//! PostgreSQL result slots. Device pointers live exclusively in [`abi`]. Phase
//! 5 can therefore analyze and serialize a query before residency exists.

pub mod abi;
mod codec;
mod projection;

pub use codec::{
    AGG_QUERY_SPEC_HEADER_WORDS, AGG_QUERY_SPEC_MAX_WORDS, AGG_QUERY_SPEC_WIRE_MAGIC,
    SpecCodecError,
};
pub use projection::{
    AGG_OUTPUT_PROJECTION_HEADER_WORDS, AGG_OUTPUT_PROJECTION_MAX_WORDS,
    AGG_OUTPUT_PROJECTION_SLOT_WORDS, AGG_OUTPUT_PROJECTION_VERSION,
    AGG_OUTPUT_PROJECTION_WIRE_MAGIC, AggOutputProjection, AggOutputSlot, AggOutputSource,
    MAX_AGG_OUTPUT_PROJECTION_SLOTS, ProjectionCodecError,
};

pub const AGG_QUERY_SPEC_VERSION: u32 = 3;

const MAX_BYTECODE_WORDS: usize = 65_536;
const MAX_PROGRAM_INPUTS: usize = 64;
pub const MAX_SPATIAL_CONSTANT_BYTES: usize = 1024 * 1024;
const MAX_VALUE_AGGREGATE_OUTPUTS: usize = 6;
const MAX_STATS_PAIR_RHS_AGGREGATE_OUTPUTS: usize = 3;
const MAX_AGGREGATE_OUTPUTS: usize =
    MAX_VALUE_AGGREGATE_OUTPUTS + MAX_STATS_PAIR_RHS_AGGREGATE_OUTPUTS;

const BOOLOID: u32 = 16;
const INT8OID: u32 = 20;
const INT4OID: u32 = 23;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;
const DATEOID: u32 = 1082;
const TIMESTAMPOID: u32 = 1114;
const TIMESTAMPTZOID: u32 = 1184;

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
    Date(i32),
    Timestamp(i64),
    TimestampTz(i64),
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

/// Stable spatial operation carried by AQS3.
///
/// PostgreSQL function OIDs are catalog-local identifiers and are therefore
/// deliberately excluded from the serialized contract. Planner extraction
/// resolves a catalog function to one of these semantic operations first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialPredicateKind {
    Intersects,
    Contains,
    Within,
    DWithin,
    Disjoint,
    Equals,
    Touches,
    Crosses,
    Overlaps,
}

/// Catalog-resolved PostGIS value family. This is stable across installations,
/// unlike the extension-defined `geometry` and `geography` type OIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialValueKind {
    Geometry,
    Geography,
}

/// Stable metadata required to interpret a spatial operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialValueMetadata {
    pub kind: SpatialValueKind,
    /// PostgreSQL typmod, or `-1` when the expression is not typmod-constrained.
    pub typmod: i32,
    /// Catalog/extractor-proved SRID. `None` means the SRID is row-dependent.
    pub srid: Option<i32>,
}

/// A typed spatial input. Constants own their exact detoasted payload so no
/// backend `Datum`, varlena pointer, or memory-context address crosses the plan
/// wire boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpatialOperand {
    Column {
        column: ColumnRef,
        metadata: SpatialValueMetadata,
    },
    Constant {
        metadata: SpatialValueMetadata,
        bytes: Box<[u8]>,
    },
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
        predicate: SpatialPredicateKind,
        left: SpatialOperand,
        right: SpatialOperand,
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
    /// `h3_cell_to_parent(cell, resolution)` with a catalog-proved h3index
    /// input. The function/type OIDs remain planner facts, not wire tags.
    H3CellToParent {
        cell: ColumnRef,
        resolution: i32,
    },
    /// `h3_latlng_to_cell(latitude, longitude, resolution)` over two resident
    /// numeric lanes. Point extraction is intentionally not represented until
    /// the geometry resident contract is wired into planner extraction.
    H3LatLngToCell {
        latitude: ColumnRef,
        longitude: ColumnRef,
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
    /// Logical PostgreSQL type produced by this group-key expression.
    pub type_oid: u32,
    /// Logical PostgreSQL collation, or invalid OID when not collatable.
    pub collation_oid: u32,
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
    /// PostgreSQL sample standard deviation (`stddev` / `stddev_samp`).
    StddevSamp,
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
    /// Analyzed collation shared by both equijoin inputs.
    pub collation_oid: u32,
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
    #[must_use]
    pub const fn type_oid(self) -> u32 {
        match self {
            Self::Bool(_) => BOOLOID,
            Self::I32(_) => INT4OID,
            Self::I64(_) => INT8OID,
            Self::F32(_) => FLOAT4OID,
            Self::F64(_) => FLOAT8OID,
            Self::Date(_) => DATEOID,
            Self::Timestamp(_) => TIMESTAMPOID,
            Self::TimestampTz(_) => TIMESTAMPTZOID,
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
            Self::Date(_) | Self::Timestamp(_) | Self::TimestampTz(_) => false,
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
            (ScalarValue::Date(lo), ScalarValue::Date(hi)) => lo <= hi,
            (ScalarValue::Timestamp(lo), ScalarValue::Timestamp(hi))
            | (ScalarValue::TimestampTz(lo), ScalarValue::TimestampTz(hi)) => lo <= hi,
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

impl SpatialValueMetadata {
    fn validate(self) -> Result<(), SpecValidationError> {
        if self.typmod < -1 {
            return Err(SpecValidationError::new("invalid spatial typmod"));
        }
        if self.srid.is_some_and(|srid| !(0..=999_999).contains(&srid)) {
            return Err(SpecValidationError::new("invalid spatial SRID"));
        }
        Ok(())
    }
}

impl SpatialOperand {
    fn validate(&self) -> Result<(), SpecValidationError> {
        match self {
            Self::Column { column, metadata } => {
                column.validate()?;
                metadata.validate()
            }
            Self::Constant { metadata, bytes } => {
                metadata.validate()?;
                if bytes.is_empty() || bytes.len() > MAX_SPATIAL_CONSTANT_BYTES {
                    return Err(SpecValidationError::new(
                        "invalid spatial constant payload length",
                    ));
                }
                if metadata.srid.is_none() {
                    return Err(SpecValidationError::new(
                        "spatial constant is missing a stable SRID",
                    ));
                }
                Ok(())
            }
        }
    }

    #[must_use]
    pub const fn column(&self) -> Option<ColumnRef> {
        match self {
            Self::Column { column, .. } => Some(*column),
            Self::Constant { .. } => None,
        }
    }

    #[must_use]
    const fn metadata(&self) -> SpatialValueMetadata {
        match self {
            Self::Column { metadata, .. } | Self::Constant { metadata, .. } => *metadata,
        }
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
                predicate,
                left,
                right,
                distance,
            } => {
                left.validate()?;
                right.validate()?;
                if left.column().is_none() && right.column().is_none() {
                    return Err(SpecValidationError::new(
                        "spatial filter has no relation operand",
                    ));
                }
                let left_metadata = left.metadata();
                let right_metadata = right.metadata();
                if left_metadata.kind != right_metadata.kind {
                    return Err(SpecValidationError::new(
                        "spatial operand families do not match",
                    ));
                }
                if let (Some(left_srid), Some(right_srid)) =
                    (left_metadata.srid, right_metadata.srid)
                    && left_srid != right_srid
                {
                    return Err(SpecValidationError::new(
                        "spatial operand SRIDs do not match",
                    ));
                }
                if (*predicate == SpatialPredicateKind::DWithin) != distance.is_some()
                    || distance.is_some_and(|value| !value.is_nonnegative_finite())
                {
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
            Self::Spatial { left, right, .. } => [left, right]
                .into_iter()
                .filter_map(|operand| operand.column())
                .all(|column| relation_allowed(column.relation_oid)),
        };
        if !valid {
            return Err(SpecValidationError::new(
                "filter references an unrelated relation",
            ));
        }
        Ok(())
    }

    fn references_relation(&self, relation_oid: u32) -> bool {
        match self {
            Self::None => false,
            Self::Ranges { input, .. } | Self::Mask { input, .. } => {
                input.relation_oid == relation_oid
            }
            Self::Bytecode { inputs, .. } => inputs
                .iter()
                .any(|input| input.relation_oid == relation_oid),
            Self::Spatial { left, right, .. } => [left, right]
                .into_iter()
                .filter_map(|operand| operand.column())
                .any(|column| column.relation_oid == relation_oid),
        }
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
        if self.type_oid == 0 {
            return Err(SpecValidationError::new(
                "group key has invalid logical type OID",
            ));
        }
        self.encoding.validate()?;
        match &self.source {
            GroupKeySource::FactColumn(column) => {
                column.validate()?;
                if column.relation_oid != fact_rel {
                    return Err(SpecValidationError::new(
                        "fact key belongs to another relation",
                    ));
                }
                if self.type_oid != column.type_oid {
                    return Err(SpecValidationError::new(
                        "fact group-key logical type does not match its column",
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
                if self.type_oid != group_column.type_oid {
                    return Err(SpecValidationError::new(
                        "dimension group-key logical type does not match its column",
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
            GroupKeySource::H3CellToParent { cell, resolution } => {
                cell.validate()?;
                if cell.relation_oid != fact_rel
                    || cell.type_oid != self.type_oid
                    || !(0..=15).contains(resolution)
                    || self.encoding != GroupKeyEncoding::Hash
                {
                    return Err(SpecValidationError::new("invalid H3 parent group key"));
                }
                Ok(())
            }
            GroupKeySource::H3LatLngToCell {
                latitude,
                longitude,
                resolution,
            } => {
                latitude.validate()?;
                longitude.validate()?;
                if latitude.relation_oid != fact_rel
                    || longitude.relation_oid != fact_rel
                    || latitude.type_oid != longitude.type_oid
                    || !matches!(latitude.type_oid, FLOAT4OID | FLOAT8OID)
                    || !(0..=15).contains(resolution)
                    || self.encoding != GroupKeyEncoding::Hash
                {
                    return Err(SpecValidationError::new("invalid H3 lat/lng group key"));
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
        if dims.iter().any(|dim| {
            dim.multiplicity == JoinMultiplicity::Counted
                && self.filter.references_relation(dim.relation_oid)
        }) {
            return Err(SpecValidationError::new(
                "measure filter references a counted dimension",
            ));
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
        if self.relation_oid == fact_rel {
            return Err(SpecValidationError::new(
                "dimension relation OID equals the fact relation OID",
            ));
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
        for (index, dim) in self.star_dims.iter().enumerate() {
            if self.star_dims[..index]
                .iter()
                .any(|prior| prior.relation_oid == dim.relation_oid)
            {
                return Err(SpecValidationError::new("duplicate dimension relation OID"));
            }
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

#[cfg(test)]
mod validation_edge_tests {
    use super::*;

    fn column(relation_oid: u32, type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid,
            attno: 1,
            type_oid,
        }
    }

    fn metadata(kind: SpatialValueKind, srid: Option<i32>) -> SpatialValueMetadata {
        SpatialValueMetadata {
            kind,
            typmod: -1,
            srid,
        }
    }

    fn operand(relation_oid: u32, kind: SpatialValueKind, srid: Option<i32>) -> SpatialOperand {
        SpatialOperand::Column {
            column: column(relation_oid, 1),
            metadata: metadata(kind, srid),
        }
    }

    fn output(source: AggregateSource, kind: AggregateKind) -> AggregateOutput {
        AggregateOutput { source, kind }
    }

    fn count_measure() -> MeasureSpec {
        MeasureSpec {
            expression: MeasureExpr::CountStar,
            outputs: vec![output(AggregateSource::Value, AggregateKind::Count)],
            filter: FilterSpec::None,
        }
    }

    fn dimension() -> DimSpec {
        DimSpec {
            relation_oid: 2,
            fact_key: column(1, INT4OID),
            dim_key: column(2, INT4OID),
            collation_oid: 0,
            multiplicity: JoinMultiplicity::Unique,
            filter: FilterSpec::None,
        }
    }

    fn error(result: Result<(), SpecValidationError>) -> &'static str {
        result.expect_err("case must be rejected").reason
    }

    #[test]
    fn scalar_and_range_boundaries_are_total() {
        assert_eq!(
            error(column(0, INT4OID).validate()),
            "invalid column reference"
        );
        assert_eq!(ScalarValue::Bool(true).type_oid(), BOOLOID);

        assert!(!ScalarValue::Bool(true).is_nonnegative_finite());
        assert!(ScalarValue::I32(0).is_nonnegative_finite());
        assert!(!ScalarValue::I64(-1).is_nonnegative_finite());
        assert!(ScalarValue::F32(0.0).is_nonnegative_finite());
        assert!(!ScalarValue::F64(f64::INFINITY).is_nonnegative_finite());
        assert!(!ScalarValue::Date(0).is_nonnegative_finite());

        ScalarRange {
            lo: ScalarValue::Bool(false),
            hi: ScalarValue::Bool(true),
        }
        .validate(BOOLOID)
        .expect("ordered booleans");
        assert_eq!(
            error(
                ScalarRange {
                    lo: ScalarValue::I32(2),
                    hi: ScalarValue::I32(1),
                }
                .validate(INT4OID)
            ),
            "range lower bound exceeds upper bound"
        );
        assert_eq!(
            error(
                ScalarRange {
                    lo: ScalarValue::I32(1),
                    hi: ScalarValue::I64(2),
                }
                .validate(INT4OID)
            ),
            "range endpoint type does not match input column"
        );
    }

    #[test]
    fn spatial_metadata_operands_and_distance_fail_closed() {
        let mut invalid = metadata(SpatialValueKind::Geometry, Some(4326));
        invalid.typmod = -2;
        assert_eq!(error(invalid.validate()), "invalid spatial typmod");
        invalid.typmod = -1;
        invalid.srid = Some(1_000_000);
        assert_eq!(error(invalid.validate()), "invalid spatial SRID");

        let empty = SpatialOperand::Constant {
            metadata: metadata(SpatialValueKind::Geometry, Some(4326)),
            bytes: Box::new([]),
        };
        assert_eq!(
            error(empty.validate()),
            "invalid spatial constant payload length"
        );
        let unstable = SpatialOperand::Constant {
            metadata: metadata(SpatialValueKind::Geometry, None),
            bytes: Box::new([1]),
        };
        assert_eq!(
            error(unstable.validate()),
            "spatial constant is missing a stable SRID"
        );

        let constant = |kind, srid| SpatialOperand::Constant {
            metadata: metadata(kind, srid),
            bytes: Box::new([1]),
        };
        let filter = |predicate, left, right, distance| FilterSpec::Spatial {
            predicate,
            left,
            right,
            distance,
        };
        assert_eq!(
            error(
                filter(
                    SpatialPredicateKind::Intersects,
                    constant(SpatialValueKind::Geometry, Some(1)),
                    constant(SpatialValueKind::Geometry, Some(1)),
                    None,
                )
                .validate()
            ),
            "spatial filter has no relation operand"
        );
        assert_eq!(
            error(
                filter(
                    SpatialPredicateKind::Intersects,
                    operand(1, SpatialValueKind::Geometry, Some(1)),
                    constant(SpatialValueKind::Geography, Some(1)),
                    None,
                )
                .validate()
            ),
            "spatial operand families do not match"
        );
        assert_eq!(
            error(
                filter(
                    SpatialPredicateKind::Intersects,
                    operand(1, SpatialValueKind::Geometry, Some(1)),
                    constant(SpatialValueKind::Geometry, Some(2)),
                    None,
                )
                .validate()
            ),
            "spatial operand SRIDs do not match"
        );
        assert_eq!(
            error(
                filter(
                    SpatialPredicateKind::DWithin,
                    operand(1, SpatialValueKind::Geometry, Some(1)),
                    constant(SpatialValueKind::Geometry, Some(1)),
                    Some(ScalarValue::Bool(true)),
                )
                .validate()
            ),
            "invalid spatial distance"
        );
    }

    #[test]
    fn filter_program_shape_scope_and_references_are_checked() {
        assert_eq!(
            error(validate_program(&[], &[])),
            "invalid bytecode program shape"
        );
        assert_eq!(
            error(
                FilterSpec::Ranges {
                    input: column(1, INT4OID),
                    ranges: Vec::new(),
                }
                .validate()
            ),
            "invalid filter range count"
        );

        let bytecode = FilterSpec::Bytecode {
            inputs: vec![column(3, INT4OID)],
            program: vec![1],
        };
        assert!(bytecode.references_relation(3));
        assert!(!bytecode.references_relation(4));
        let spatial = FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Equals,
            left: operand(1, SpatialValueKind::Geometry, Some(0)),
            right: operand(2, SpatialValueKind::Geometry, Some(0)),
            distance: None,
        };
        assert!(spatial.references_relation(2));
        assert!(!spatial.references_relation(4));
    }

    #[test]
    fn group_encoding_and_sources_reject_invalid_contracts() {
        assert_eq!(
            error(
                GroupKeyEncoding::DictionaryI32 {
                    cardinality: 0,
                    null_code: None,
                }
                .validate()
            ),
            "zero group-key cardinality"
        );
        assert_eq!(
            error(
                GroupKeyEncoding::DenseI32 {
                    code_min: i32::MAX,
                    cardinality: 2,
                    null_code: None,
                }
                .validate()
            ),
            "group-key code range overflows i32"
        );
        assert_eq!(
            error(
                GroupKeyEncoding::DenseI32 {
                    code_min: 10,
                    cardinality: 2,
                    null_code: Some(9),
                }
                .validate()
            ),
            "NULL code is outside key range"
        );

        let key = |source| GroupKeyRef {
            source,
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        };
        assert_eq!(
            error(key(GroupKeySource::FactColumn(column(2, INT4OID))).validate(1, &[])),
            "fact key belongs to another relation"
        );
        assert_eq!(
            error(
                key(GroupKeySource::StarDimension {
                    dim_index: 0,
                    group_column: column(2, INT4OID),
                })
                .validate(1, &[])
            ),
            "group key references missing dimension"
        );
        assert_eq!(
            error(
                key(GroupKeySource::StarDimension {
                    dim_index: 0,
                    group_column: column(3, INT4OID),
                })
                .validate(1, &[dimension()])
            ),
            "dimension key belongs to another relation"
        );
        assert_eq!(
            error(
                key(GroupKeySource::Expression {
                    inputs: vec![column(2, INT4OID)],
                    program: vec![1],
                })
                .validate(1, &[])
            ),
            "group-key expression references a non-fact relation"
        );
        assert_eq!(
            error(
                key(GroupKeySource::H3LatLngToCell {
                    latitude: column(1, INT4OID),
                    longitude: column(1, INT4OID),
                    resolution: 9,
                })
                .validate(1, &[])
            ),
            "invalid H3 lat/lng group key"
        );
    }

    #[test]
    fn measure_dimension_and_having_contracts_reject_bad_references() {
        let mut measure = count_measure();
        measure.outputs.clear();
        assert_eq!(
            error(measure.validate(1, &[])),
            "invalid aggregate output count"
        );
        measure = count_measure();
        measure.outputs.push(measure.outputs[0]);
        assert_eq!(
            error(measure.validate(1, &[])),
            "duplicate aggregate output"
        );

        measure = MeasureSpec {
            expression: MeasureExpr::Binary {
                op: BinaryMeasureOp::Mul,
                lhs: column(1, INT4OID),
                rhs: column(2, INT4OID),
            },
            outputs: vec![output(AggregateSource::Value, AggregateKind::Sum)],
            filter: FilterSpec::None,
        };
        assert_eq!(
            error(measure.validate(1, &[])),
            "measure references a non-fact relation"
        );
        measure.expression = MeasureExpr::Bytecode {
            inputs: vec![column(1, INT4OID)],
            program: vec![1],
            result_type_oid: 0,
        };
        assert_eq!(
            error(measure.validate(1, &[])),
            "bytecode measure has invalid result type"
        );
        measure.expression = MeasureExpr::Bytecode {
            inputs: vec![column(2, INT4OID)],
            program: vec![1],
            result_type_oid: INT4OID,
        };
        assert_eq!(
            error(measure.validate(1, &[])),
            "measure references a non-fact relation"
        );

        let mut dim = dimension();
        dim.relation_oid = 0;
        assert_eq!(error(dim.validate(1)), "invalid dimension relation OID");
        dim = dimension();
        dim.fact_key.relation_oid = 3;
        assert_eq!(
            error(dim.validate(1)),
            "join key belongs to another relation"
        );

        let invalid_having = HavingSpec {
            inputs: Vec::new(),
            program: Vec::new(),
        };
        assert_eq!(
            error(invalid_having.validate(&[count_measure()])),
            "invalid HAVING program"
        );
        let missing_measure = HavingSpec {
            inputs: vec![AggregateRef {
                measure_index: 1,
                source: AggregateSource::Value,
                kind: AggregateKind::Count,
            }],
            program: vec![1],
        };
        assert_eq!(
            error(missing_measure.validate(&[count_measure()])),
            "HAVING references a missing aggregate output"
        );
        let missing_output = HavingSpec {
            inputs: vec![AggregateRef {
                measure_index: 0,
                source: AggregateSource::Value,
                kind: AggregateKind::Sum,
            }],
            program: vec![1],
        };
        assert_eq!(
            error(missing_output.validate(&[count_measure()])),
            "HAVING references a missing aggregate output"
        );
    }

    #[test]
    fn aggregate_shape_rejects_mixed_key_strategies() {
        let fact = column(1, INT4OID);
        let key = |encoding| GroupKeyRef {
            source: GroupKeySource::FactColumn(fact),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding,
        };
        let spec = AggQuerySpec {
            fact_rel: 1,
            group_keys: vec![
                key(GroupKeyEncoding::Hash),
                key(GroupKeyEncoding::DenseI32 {
                    code_min: 0,
                    cardinality: 2,
                    null_code: None,
                }),
            ],
            measures: vec![count_measure()],
            fact_filter: FilterSpec::None,
            star_dims: Vec::new(),
            having: None,
        };
        assert_eq!(error(spec.validate()), "mixed dense/hash key encodings");
    }
}

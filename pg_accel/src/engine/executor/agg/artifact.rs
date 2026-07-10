//! Begin-time host preparation and device ownership for generic aggregates.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use pgrx::pg_sys;

use crate::engine::residency::{
    DerivedArtifact, PreparedDerived, ResidentColumnRef, ResidentColumnView, ResidentInputBundle,
};
use crate::engine::spec::{
    AggQuerySpec, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity,
    MaskKind, ScalarRange, ScalarValue,
};
use crate::gpu::ExprDeviceBuffer;

const BOOLOID: u32 = 16;
const INT2OID: u32 = 21;
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;
const DATEOID: u32 = 1082;
const TIMESTAMPOID: u32 = 1114;
const TIMESTAMPTZOID: u32 = 1184;
const TEXTOID: u32 = 25;
const BPCHAROID: u32 = 1042;
const VARCHAROID: u32 = 1043;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum GroupDatum {
    Unused,
    Null,
    Bool(bool),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct GroupDomain {
    pub type_oid: u32,
    pub collation_oid: u32,
    pub values: Vec<GroupDatum>,
    pub null_code: Option<i32>,
}

impl GroupDomain {
    pub fn cardinality(&self) -> Result<u32, String> {
        u32::try_from(self.values.len())
            .map_err(|_| "group dictionary exceeds the descriptor u32 domain".to_owned())
    }

    pub fn decode(&self, code: i32) -> Result<&GroupDatum, String> {
        let index =
            usize::try_from(code).map_err(|_| format!("negative group dictionary code {code}"))?;
        self.values
            .get(index)
            .ok_or_else(|| format!("group dictionary code {code} is out of bounds"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalKey {
    Bool(bool),
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Text(String),
}

fn canonical_f32(value: f32) -> u32 {
    if value == 0.0 {
        0
    } else if value.is_nan() {
        0x7fc0_0000
    } else {
        value.to_bits()
    }
}

fn canonical_f64(value: f64) -> u64 {
    if value == 0.0 {
        0
    } else if value.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        value.to_bits()
    }
}

fn canonical_text(type_oid: u32, value: &str) -> String {
    if type_oid == BPCHAROID {
        value.trim_end_matches(' ').to_owned()
    } else {
        value.to_owned()
    }
}

enum HostColumn {
    Bool {
        type_oid: u32,
        values: Vec<u8>,
        nulls: Option<Vec<u8>>,
    },
    I32 {
        type_oid: u32,
        values: Vec<i32>,
        nulls: Option<Vec<u8>>,
    },
    I64 {
        type_oid: u32,
        values: Vec<i64>,
        nulls: Option<Vec<u8>>,
    },
    F32 {
        type_oid: u32,
        values: Vec<f32>,
        nulls: Option<Vec<u8>>,
    },
    F64 {
        type_oid: u32,
        values: Vec<f64>,
        nulls: Option<Vec<u8>>,
    },
    Text {
        type_oid: u32,
        codes: Vec<i32>,
        nulls: Option<Vec<u8>>,
        labels: Vec<String>,
    },
}

impl HostColumn {
    fn copy_from(view: ResidentColumnView<'_>) -> Result<Self, String> {
        let copy_nulls = |nulls: Option<&ExprDeviceBuffer<u8>>| {
            nulls
                .map(ExprDeviceBuffer::copy_to_vec)
                .transpose()
                .map_err(|error| error.to_string())
        };
        let result = match view {
            ResidentColumnView::Empty { type_oid } => match u32::from(type_oid) {
                BOOLOID => Self::Bool {
                    type_oid: BOOLOID,
                    values: Vec::new(),
                    nulls: None,
                },
                type_oid @ (INT2OID | INT4OID | DATEOID) => Self::I32 {
                    type_oid,
                    values: Vec::new(),
                    nulls: None,
                },
                type_oid @ (INT8OID | TIMESTAMPOID | TIMESTAMPTZOID) => Self::I64 {
                    type_oid,
                    values: Vec::new(),
                    nulls: None,
                },
                FLOAT4OID => Self::F32 {
                    type_oid: FLOAT4OID,
                    values: Vec::new(),
                    nulls: None,
                },
                FLOAT8OID => Self::F64 {
                    type_oid: FLOAT8OID,
                    values: Vec::new(),
                    nulls: None,
                },
                type_oid @ (TEXTOID | VARCHAROID | BPCHAROID) => Self::Text {
                    type_oid,
                    codes: Vec::new(),
                    nulls: None,
                    labels: Vec::new(),
                },
                type_oid => {
                    return Err(format!(
                        "resident empty column has unsupported type OID {type_oid}"
                    ));
                }
            },
            ResidentColumnView::Bool {
                type_oid,
                values,
                nulls,
            } => Self::Bool {
                type_oid: u32::from(type_oid),
                values: values.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
            },
            ResidentColumnView::I32 {
                type_oid,
                values,
                nulls,
            } => Self::I32 {
                type_oid: u32::from(type_oid),
                values: values.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
            },
            ResidentColumnView::I64 {
                type_oid,
                values,
                nulls,
            } => Self::I64 {
                type_oid: u32::from(type_oid),
                values: values.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
            },
            ResidentColumnView::F32 {
                type_oid,
                values,
                nulls,
            } => Self::F32 {
                type_oid: u32::from(type_oid),
                values: values.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
            },
            ResidentColumnView::F64 {
                type_oid,
                values,
                nulls,
            } => Self::F64 {
                type_oid: u32::from(type_oid),
                values: values.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
            },
            ResidentColumnView::TextDictionary {
                type_oid,
                codes,
                nulls,
                labels,
            } => Self::Text {
                type_oid: u32::from(type_oid),
                codes: codes.copy_to_vec().map_err(|error| error.to_string())?,
                nulls: copy_nulls(nulls)?,
                labels: labels.to_vec(),
            },
        };
        result.validate_nulls()?;
        Ok(result)
    }

    const fn type_oid(&self) -> u32 {
        match self {
            Self::Bool { type_oid, .. }
            | Self::I32 { type_oid, .. }
            | Self::I64 { type_oid, .. }
            | Self::F32 { type_oid, .. }
            | Self::F64 { type_oid, .. }
            | Self::Text { type_oid, .. } => *type_oid,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bool { values, .. } => values.len(),
            Self::I32 { values, .. } => values.len(),
            Self::I64 { values, .. } => values.len(),
            Self::F32 { values, .. } => values.len(),
            Self::F64 { values, .. } => values.len(),
            Self::Text { codes, .. } => codes.len(),
        }
    }

    fn nulls(&self) -> Option<&[u8]> {
        match self {
            Self::Bool { nulls, .. }
            | Self::I32 { nulls, .. }
            | Self::I64 { nulls, .. }
            | Self::F32 { nulls, .. }
            | Self::F64 { nulls, .. }
            | Self::Text { nulls, .. } => nulls.as_deref(),
        }
    }

    fn validate_nulls(&self) -> Result<(), String> {
        if let Self::Bool { values, .. } = self
            && values.iter().any(|value| *value > 1)
        {
            return Err("resident boolean column contains a noncanonical value".to_owned());
        }
        let Some(nulls) = self.nulls() else {
            return Ok(());
        };
        if nulls.len() != self.len() || nulls.iter().any(|value| *value > 1) {
            return Err("resident NULL bitmap has an invalid shape/value".to_owned());
        }
        Ok(())
    }

    fn is_null(&self, row: usize) -> Result<bool, String> {
        if row >= self.len() {
            return Err(format!("resident row {row} is out of bounds"));
        }
        Ok(self.nulls().is_some_and(|nulls| nulls[row] != 0))
    }

    fn dictionary_value(&self, row: usize) -> Result<Option<(CanonicalKey, GroupDatum)>, String> {
        if self.is_null(row)? {
            return Ok(None);
        }
        let pair = match self {
            Self::Bool { values, .. } => {
                let value = values[row];
                if value > 1 {
                    return Err("resident boolean value is not canonical".to_owned());
                }
                (CanonicalKey::Bool(value != 0), GroupDatum::Bool(value != 0))
            }
            Self::I32 { values, .. } => {
                let value = values[row];
                (CanonicalKey::I32(value), GroupDatum::I32(value))
            }
            Self::I64 { values, .. } => {
                let value = values[row];
                (CanonicalKey::I64(value), GroupDatum::I64(value))
            }
            Self::F32 { values, .. } => {
                let value = values[row];
                (
                    CanonicalKey::F32(canonical_f32(value)),
                    GroupDatum::F32(value),
                )
            }
            Self::F64 { values, .. } => {
                let value = values[row];
                (
                    CanonicalKey::F64(canonical_f64(value)),
                    GroupDatum::F64(value),
                )
            }
            Self::Text {
                type_oid,
                codes,
                labels,
                ..
            } => {
                let code = usize::try_from(codes[row])
                    .map_err(|_| "resident text code is negative".to_owned())?;
                let label = labels
                    .get(code)
                    .ok_or_else(|| "resident text code exceeds its label dictionary".to_owned())?;
                (
                    CanonicalKey::Text(canonical_text(*type_oid, label)),
                    GroupDatum::Text(label.clone()),
                )
            }
        };
        Ok(Some(pair))
    }

    fn join_key(&self, row: usize) -> Result<Option<CanonicalKey>, String> {
        match self.type_oid() {
            INT4OID | TEXTOID | VARCHAROID | BPCHAROID => {
                Ok(self.dictionary_value(row)?.map(|(key, _)| key))
            }
            type_oid => Err(format!(
                "join type OID {type_oid} is not an INT4 or deterministic text-family key"
            )),
        }
    }

    fn filter_value(&self, row: usize) -> Result<Option<ScalarValue>, String> {
        if self.is_null(row)? {
            return Ok(None);
        }
        let value = match self {
            Self::Bool { values, .. } => ScalarValue::Bool(values[row] != 0),
            Self::I32 {
                type_oid: DATEOID,
                values,
                ..
            } => ScalarValue::Date(values[row]),
            Self::I32 { values, .. } => ScalarValue::I32(values[row]),
            Self::I64 {
                type_oid: TIMESTAMPOID,
                values,
                ..
            } => ScalarValue::Timestamp(values[row]),
            Self::I64 {
                type_oid: TIMESTAMPTZOID,
                values,
                ..
            } => ScalarValue::TimestampTz(values[row]),
            Self::I64 { values, .. } => ScalarValue::I64(values[row]),
            Self::F32 { values, .. } => ScalarValue::F32(values[row]),
            Self::F64 { values, .. } => ScalarValue::F64(values[row]),
            Self::Text { .. } => {
                return Err("text columns are not scalar range filters".to_owned());
            }
        };
        Ok(Some(value))
    }
}

struct HostColumns {
    columns: BTreeMap<(u32, i16), HostColumn>,
    row_counts: BTreeMap<u32, usize>,
}

impl HostColumns {
    fn copy(
        requests: &[ResidentColumnRef],
        bundle: ResidentInputBundle<'_>,
    ) -> Result<Self, String> {
        if requests.len() != bundle.columns.len() {
            return Err("resident bulk column count changed during preparation".to_owned());
        }
        let mut row_counts = BTreeMap::new();
        for evidence in bundle.evidence {
            row_counts.insert(
                u32::from(evidence.relid),
                usize::try_from(evidence.row_count)
                    .map_err(|_| "resident row count exceeds usize".to_owned())?,
            );
        }
        let mut columns = BTreeMap::new();
        for (request, view) in requests.iter().zip(bundle.columns) {
            let column = HostColumn::copy_from(view)?;
            let expected = row_counts
                .get(&u32::from(request.relid))
                .copied()
                .ok_or_else(|| "resident column has no relation evidence".to_owned())?;
            if column.len() != expected {
                return Err(format!(
                    "resident column ({}, {}) has {} rows, relation evidence reports {expected}",
                    u32::from(request.relid),
                    request.attno,
                    column.len()
                ));
            }
            columns.insert((u32::from(request.relid), request.attno), column);
        }
        Ok(Self {
            columns,
            row_counts,
        })
    }

    fn get(&self, column: ColumnRef) -> Result<&HostColumn, String> {
        let attno = i16::try_from(column.attno)
            .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
        let value = self
            .columns
            .get(&(column.relation_oid, attno))
            .ok_or_else(|| {
                format!(
                    "artifact preparation did not request column ({}, {})",
                    column.relation_oid, column.attno
                )
            })?;
        if value.type_oid() != column.type_oid {
            return Err(format!(
                "resident column ({}, {}) type OID {} does not match planned OID {}",
                column.relation_oid,
                column.attno,
                value.type_oid(),
                column.type_oid
            ));
        }
        Ok(value)
    }

    fn row_count(&self, relation_oid: u32) -> Result<usize, String> {
        self.row_counts
            .get(&relation_oid)
            .copied()
            .ok_or_else(|| format!("relation OID {relation_oid} has no resident evidence"))
    }
}

fn collect_filter_column(
    filter: &FilterSpec,
    output: &mut BTreeSet<(u32, i16)>,
) -> Result<(), String> {
    match filter {
        FilterSpec::None => Ok(()),
        FilterSpec::Ranges { input, .. } | FilterSpec::Mask { input, .. } => {
            let attno = i16::try_from(input.attno)
                .map_err(|_| format!("attribute {} exceeds int16", input.attno))?;
            output.insert((input.relation_oid, attno));
            Ok(())
        }
        FilterSpec::Bytecode { .. } | FilterSpec::Spatial { .. } => {
            Err("derived artifact cannot evaluate bytecode/spatial filters".to_owned())
        }
    }
}

fn insert_column_ref(columns: &mut BTreeSet<(u32, i16)>, column: ColumnRef) -> Result<(), String> {
    let attno = i16::try_from(column.attno)
        .map_err(|_| format!("attribute {} exceeds int16", column.attno))?;
    columns.insert((column.relation_oid, attno));
    Ok(())
}

pub(super) fn artifact_column_refs(spec: &AggQuerySpec) -> Result<Vec<ResidentColumnRef>, String> {
    let mut columns = BTreeSet::new();
    for key in &spec.group_keys {
        match &key.source {
            GroupKeySource::FactColumn(column) => insert_column_ref(&mut columns, *column)?,
            GroupKeySource::StarDimension { group_column, .. } => {
                insert_column_ref(&mut columns, *group_column)?;
            }
            GroupKeySource::Expression { .. } | GroupKeySource::H3Cell { .. } => {
                return Err("Phase 5D supports only column group keys".to_owned());
            }
        }
    }
    for dimension in &spec.star_dims {
        insert_column_ref(&mut columns, dimension.fact_key)?;
        insert_column_ref(&mut columns, dimension.dim_key)?;
        collect_filter_column(&dimension.filter, &mut columns)?;
    }
    if matches!(spec.fact_filter, FilterSpec::Mask { .. }) {
        collect_filter_column(&spec.fact_filter, &mut columns)?;
    }
    Ok(columns
        .into_iter()
        .map(|(relid, attno)| ResidentColumnRef {
            relid: pg_sys::Oid::from(relid),
            attno,
        })
        .collect())
}

fn scalar_cmp(left: ScalarValue, right: ScalarValue) -> Result<std::cmp::Ordering, String> {
    let ordering = match (left, right) {
        (ScalarValue::Bool(left), ScalarValue::Bool(right)) => left.cmp(&right),
        (ScalarValue::I32(left), ScalarValue::I32(right)) => left.cmp(&right),
        (ScalarValue::I64(left), ScalarValue::I64(right)) => left.cmp(&right),
        (ScalarValue::Date(left), ScalarValue::Date(right)) => left.cmp(&right),
        (ScalarValue::Timestamp(left), ScalarValue::Timestamp(right))
        | (ScalarValue::TimestampTz(left), ScalarValue::TimestampTz(right)) => left.cmp(&right),
        (ScalarValue::F32(left), ScalarValue::F32(right)) => {
            if left.is_nan() {
                if right.is_nan() {
                    std::cmp::Ordering::Equal
                } else {
                    std::cmp::Ordering::Greater
                }
            } else if right.is_nan() {
                std::cmp::Ordering::Less
            } else {
                left.partial_cmp(&right)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        }
        (ScalarValue::F64(left), ScalarValue::F64(right)) => {
            if left.is_nan() {
                if right.is_nan() {
                    std::cmp::Ordering::Equal
                } else {
                    std::cmp::Ordering::Greater
                }
            } else if right.is_nan() {
                std::cmp::Ordering::Less
            } else {
                left.partial_cmp(&right)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        }
        _ => return Err("scalar filter type mismatch".to_owned()),
    };
    Ok(ordering)
}

fn in_range(value: ScalarValue, range: ScalarRange) -> Result<bool, String> {
    Ok(scalar_cmp(value, range.lo)? != std::cmp::Ordering::Less
        && scalar_cmp(value, range.hi)? != std::cmp::Ordering::Greater)
}

fn filter_accepts(filter: &FilterSpec, columns: &HostColumns, row: usize) -> Result<bool, String> {
    match filter {
        FilterSpec::None => Ok(true),
        FilterSpec::Ranges { input, ranges } => {
            let Some(value) = columns.get(*input)?.filter_value(row)? else {
                return Ok(false);
            };
            for range in ranges {
                if in_range(value, *range)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.type_oid == BOOLOID => {
            let Some(ScalarValue::Bool(value)) = columns.get(*input)?.filter_value(row)? else {
                return Ok(false);
            };
            Ok(value)
        }
        FilterSpec::Mask { .. } => {
            Err("recheck/non-boolean masks are not childless-safe".to_owned())
        }
        FilterSpec::Bytecode { .. } | FilterSpec::Spatial { .. } => {
            Err("bytecode/spatial filters are not supported by Phase 5D".to_owned())
        }
    }
}

struct DictionaryBuilder {
    type_oid: u32,
    collation_oid: u32,
    by_key: BTreeMap<CanonicalKey, i32>,
    values: Vec<GroupDatum>,
    null_code: Option<i32>,
}

impl DictionaryBuilder {
    fn new(type_oid: u32, collation_oid: u32) -> Self {
        Self {
            type_oid,
            collation_oid,
            by_key: BTreeMap::new(),
            values: Vec::new(),
            null_code: None,
        }
    }

    fn code_for(&mut self, value: Option<(CanonicalKey, GroupDatum)>) -> Result<i32, String> {
        let Some((key, datum)) = value else {
            if let Some(code) = self.null_code {
                return Ok(code);
            }
            let code = i32::try_from(self.values.len())
                .map_err(|_| "group dictionary exceeds i32 code space".to_owned())?;
            self.values.push(GroupDatum::Null);
            self.null_code = Some(code);
            return Ok(code);
        };
        if let Some(code) = self.by_key.get(&key) {
            return Ok(*code);
        }
        let code = i32::try_from(self.values.len())
            .map_err(|_| "group dictionary exceeds i32 code space".to_owned())?;
        self.values.push(datum);
        self.by_key.insert(key, code);
        Ok(code)
    }

    fn finish(mut self) -> GroupDomain {
        if self.values.is_empty() {
            self.values.push(GroupDatum::Unused);
        }
        GroupDomain {
            type_oid: self.type_oid,
            collation_oid: self.collation_oid,
            values: self.values,
            null_code: self.null_code,
        }
    }
}

struct PreparedDimension {
    fact_codes: Vec<i32>,
    row_codes: Vec<Option<usize>>,
    row_matches: Vec<bool>,
    match_by_key: Vec<u8>,
    multiplicity_by_key: Option<Vec<u64>>,
}

fn prepare_dimension(
    dimension: &crate::engine::spec::DimSpec,
    columns: &HostColumns,
    fact_rows: usize,
) -> Result<PreparedDimension, String> {
    let fact = columns.get(dimension.fact_key)?;
    let dim = columns.get(dimension.dim_key)?;
    if fact.len() != fact_rows {
        return Err("fact join key row count mismatch".to_owned());
    }
    let dim_rows = columns.row_count(dimension.relation_oid)?;
    if dim.len() != dim_rows {
        return Err("dimension join key row count mismatch".to_owned());
    }
    if fact.type_oid() != dim.type_oid() || fact.type_oid() != dimension.fact_key.type_oid {
        return Err("join key resident/logical type mismatch".to_owned());
    }

    let mut by_key = BTreeMap::<CanonicalKey, usize>::new();
    let mut row_codes = vec![None; dim_rows];
    let mut row_matches = vec![false; dim_rows];
    let mut unique_codes = BTreeSet::new();
    for row in 0..dim_rows {
        let Some(key) = dim.join_key(row)? else {
            continue;
        };
        let next = by_key.len();
        let code = *by_key.entry(key).or_insert(next);
        if dimension.multiplicity == JoinMultiplicity::Unique && !unique_codes.insert(code) {
            return Err(
                "dimension declared UNIQUE contains a duplicate non-NULL join key".to_owned(),
            );
        }
        row_codes[row] = Some(code);
        row_matches[row] = filter_accepts(&dimension.filter, columns, row)?;
    }

    let mut match_by_key = vec![0_u8; by_key.len()];
    let mut multiplicity =
        (dimension.multiplicity == JoinMultiplicity::Counted).then(|| vec![0_u64; by_key.len()]);
    for row in 0..dim_rows {
        let (Some(code), true) = (row_codes[row], row_matches[row]) else {
            continue;
        };
        match_by_key[code] = 1;
        if let Some(multiplicity) = &mut multiplicity {
            multiplicity[code] = multiplicity[code]
                .checked_add(1)
                .ok_or_else(|| "dimension multiplicity exceeds u64".to_owned())?;
        }
    }

    let mut fact_codes = Vec::with_capacity(fact_rows);
    for row in 0..fact_rows {
        let code = fact
            .join_key(row)?
            .and_then(|key| by_key.get(&key).copied())
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or(-1);
        fact_codes.push(code);
    }
    Ok(PreparedDimension {
        fact_codes,
        row_codes,
        row_matches,
        match_by_key,
        multiplicity_by_key: multiplicity,
    })
}

enum PreparedKeyInput {
    Fact(Vec<i32>),
    Dimension { dim_index: usize, lookup: Vec<i32> },
}

pub(super) struct PreparedAggArtifact {
    resolved_spec: AggQuerySpec,
    fact_rows: usize,
    group_capacity: usize,
    keys: Vec<PreparedKeyInput>,
    dimensions: Vec<PreparedDimension>,
    fact_mask: Option<Vec<i8>>,
    domains: Rc<[GroupDomain]>,
    device_bytes: u64,
}

fn checked_device_bytes(prepared: &PreparedAggArtifact) -> Result<u64, String> {
    let mut bytes = 0_usize;
    let mut add = |elements: usize, width: usize| -> Result<(), String> {
        bytes = bytes
            .checked_add(
                elements
                    .checked_mul(width)
                    .ok_or_else(|| "derived artifact byte length overflow".to_owned())?,
            )
            .ok_or_else(|| "derived artifact byte length overflow".to_owned())?;
        Ok(())
    };
    for key in &prepared.keys {
        match key {
            PreparedKeyInput::Fact(codes) => add(codes.len(), 4)?,
            PreparedKeyInput::Dimension { lookup, .. } => add(lookup.len(), 4)?,
        }
    }
    for dimension in &prepared.dimensions {
        add(dimension.fact_codes.len(), 4)?;
        add(dimension.match_by_key.len(), 1)?;
        if let Some(multiplicity) = &dimension.multiplicity_by_key {
            add(multiplicity.len(), 8)?;
        }
    }
    if let Some(mask) = &prepared.fact_mask {
        add(mask.len(), 1)?;
    }
    u64::try_from(bytes).map_err(|_| "derived artifact bytes exceed u64".to_owned())
}

pub(super) fn prepare_agg_artifact(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    bundle: ResidentInputBundle<'_>,
    max_groups: usize,
) -> Result<PreparedDerived<PreparedAggArtifact>, String> {
    let columns = HostColumns::copy(requests, bundle)?;
    let fact_rows = columns.row_count(spec.fact_rel)?;
    let mut dimensions = Vec::with_capacity(spec.star_dims.len());
    for dimension in &spec.star_dims {
        dimensions.push(prepare_dimension(dimension, &columns, fact_rows)?);
    }

    let mut resolved_spec = spec.clone();
    let mut keys = Vec::with_capacity(spec.group_keys.len());
    let mut domains = Vec::with_capacity(spec.group_keys.len());
    let mut group_capacity = 1_usize;
    for (key_index, key) in spec.group_keys.iter().enumerate() {
        let mut dictionary = DictionaryBuilder::new(key.type_oid, key.collation_oid);
        let input = match &key.source {
            GroupKeySource::FactColumn(column) => {
                let column = columns.get(*column)?;
                let mut codes = Vec::with_capacity(fact_rows);
                for row in 0..fact_rows {
                    codes.push(dictionary.code_for(column.dictionary_value(row)?)?);
                }
                PreparedKeyInput::Fact(codes)
            }
            GroupKeySource::StarDimension {
                dim_index,
                group_column,
            } => {
                let dim_index = usize::try_from(*dim_index)
                    .map_err(|_| "dimension index exceeds usize".to_owned())?;
                let dimension = dimensions
                    .get(dim_index)
                    .ok_or_else(|| "group key references a missing dimension".to_owned())?;
                if spec.star_dims[dim_index].multiplicity != JoinMultiplicity::Unique {
                    return Err("cannot group by a counted dimension".to_owned());
                }
                let group_column = columns.get(*group_column)?;
                let mut lookup = vec![0_i32; dimension.match_by_key.len()];
                for row in 0..group_column.len() {
                    let (Some(join_code), true) =
                        (dimension.row_codes[row], dimension.row_matches[row])
                    else {
                        continue;
                    };
                    lookup[join_code] = dictionary.code_for(group_column.dictionary_value(row)?)?;
                }
                PreparedKeyInput::Dimension { dim_index, lookup }
            }
            GroupKeySource::Expression { .. } | GroupKeySource::H3Cell { .. } => {
                return Err("Phase 5D cannot resolve expression/H3 group keys".to_owned());
            }
        };
        let domain = dictionary.finish();
        let cardinality = domain.cardinality()?;
        group_capacity = group_capacity
            .checked_mul(cardinality as usize)
            .filter(|capacity| *capacity <= max_groups)
            .ok_or_else(|| format!("resolved group capacity exceeds limit {max_groups}"))?;
        resolved_spec.group_keys[key_index].encoding = GroupKeyEncoding::DictionaryI32 {
            cardinality,
            null_code: domain.null_code,
        };
        keys.push(input);
        domains.push(domain);
    }

    let fact_mask = match &spec.fact_filter {
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.type_oid == BOOLOID => {
            let column = columns.get(*input)?;
            let mut mask = Vec::with_capacity(fact_rows);
            for row in 0..fact_rows {
                mask.push(
                    if matches!(column.filter_value(row)?, Some(ScalarValue::Bool(true))) {
                        1
                    } else {
                        -1
                    },
                );
            }
            Some(mask)
        }
        FilterSpec::None | FilterSpec::Ranges { .. } => None,
        _ => return Err("fact filter is not descriptor-compatible".to_owned()),
    };
    resolved_spec
        .validate()
        .map_err(|error| format!("resolved aggregate spec is invalid: {error}"))?;
    let mut prepared = PreparedAggArtifact {
        resolved_spec,
        fact_rows,
        group_capacity,
        keys,
        dimensions,
        fact_mask,
        domains: Rc::from(domains),
        device_bytes: 0,
    };
    prepared.device_bytes = checked_device_bytes(&prepared)?;
    let device_bytes = prepared.device_bytes;
    Ok(PreparedDerived {
        prepared,
        device_bytes,
    })
}

fn upload<T: Copy>(
    values: &[T],
    label: &'static str,
) -> Result<Option<ExprDeviceBuffer<T>>, String> {
    if values.is_empty() {
        return Ok(None);
    }
    ExprDeviceBuffer::copy_from_slice(values)
        .map(Some)
        .ok_or_else(|| format!("could not allocate/upload derived {label} buffer"))
}

pub(super) enum ArtifactKeyInput {
    Fact(Option<ExprDeviceBuffer<i32>>),
    Dimension {
        dim_index: usize,
        lookup: Option<ExprDeviceBuffer<i32>>,
    },
}

pub(super) struct ArtifactDimension {
    pub fact_codes: Option<ExprDeviceBuffer<i32>>,
    pub match_by_key: Option<ExprDeviceBuffer<u8>>,
    pub multiplicity_by_key: Option<ExprDeviceBuffer<u64>>,
    pub key_count: usize,
}

pub(super) struct DescriptorAggArtifact {
    pub resolved_spec: AggQuerySpec,
    pub fact_rows: usize,
    pub group_capacity: usize,
    pub keys: Vec<ArtifactKeyInput>,
    pub dimensions: Vec<ArtifactDimension>,
    pub fact_mask: Option<ExprDeviceBuffer<i8>>,
    pub domains: Rc<[GroupDomain]>,
    device_bytes: u64,
}

impl DescriptorAggArtifact {
    pub(super) fn build(prepared: PreparedAggArtifact) -> Result<Self, String> {
        let mut keys = Vec::with_capacity(prepared.keys.len());
        for key in prepared.keys {
            keys.push(match key {
                PreparedKeyInput::Fact(codes) => {
                    ArtifactKeyInput::Fact(upload(&codes, "group code")?)
                }
                PreparedKeyInput::Dimension { dim_index, lookup } => ArtifactKeyInput::Dimension {
                    dim_index,
                    lookup: upload(&lookup, "dimension group lookup")?,
                },
            });
        }
        let mut dimensions = Vec::with_capacity(prepared.dimensions.len());
        for dimension in prepared.dimensions {
            let key_count = dimension.match_by_key.len();
            dimensions.push(ArtifactDimension {
                fact_codes: upload(&dimension.fact_codes, "fact join code")?,
                match_by_key: upload(&dimension.match_by_key, "dimension match")?,
                multiplicity_by_key: dimension
                    .multiplicity_by_key
                    .as_deref()
                    .map(|values| upload(values, "dimension multiplicity"))
                    .transpose()?
                    .flatten(),
                key_count,
            });
        }
        let fact_mask = prepared
            .fact_mask
            .as_deref()
            .map(|values| upload(values, "fact filter mask"))
            .transpose()?
            .flatten();
        Ok(Self {
            resolved_spec: prepared.resolved_spec,
            fact_rows: prepared.fact_rows,
            group_capacity: prepared.group_capacity,
            keys,
            dimensions,
            fact_mask,
            domains: prepared.domains,
            device_bytes: prepared.device_bytes,
        })
    }
}

impl DerivedArtifact for DescriptorAggArtifact {
    fn device_bytes(&self) -> u64 {
        self.device_bytes
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_grouping_canonicalizes_zero_and_nan() {
        assert_eq!(canonical_f64(0.0), canonical_f64(-0.0));
        assert_eq!(canonical_f64(f64::NAN), canonical_f64(-f64::NAN));
        assert_eq!(canonical_f32(0.0), canonical_f32(-0.0));
        assert_eq!(canonical_f32(f32::NAN), canonical_f32(-f32::NAN));
    }

    #[test]
    fn bpchar_join_identity_ignores_trailing_spaces_only() {
        assert_eq!(canonical_text(BPCHAROID, "EU  "), "EU");
        assert_eq!(canonical_text(TEXTOID, "EU  "), "EU  ");
        assert_eq!(canonical_text(VARCHAROID, "EU  "), "EU  ");
    }

    #[test]
    fn dictionary_reserves_explicit_null_code() {
        let mut dictionary = DictionaryBuilder::new(INT4OID, 0);
        assert_eq!(
            dictionary
                .code_for(Some((CanonicalKey::I32(7), GroupDatum::I32(7))))
                .expect("code"),
            0
        );
        assert_eq!(dictionary.code_for(None).expect("null code"), 1);
        assert_eq!(dictionary.code_for(None).expect("stable null code"), 1);
        let domain = dictionary.finish();
        assert_eq!(domain.null_code, Some(1));
        assert_eq!(domain.values, vec![GroupDatum::I32(7), GroupDatum::Null]);
    }

    #[test]
    fn empty_dictionary_keeps_nonzero_descriptor_cardinality() {
        let domain = DictionaryBuilder::new(INT4OID, 0).finish();
        assert_eq!(domain.cardinality().expect("cardinality"), 1);
        assert_eq!(domain.values, vec![GroupDatum::Unused]);
        assert_eq!(domain.null_code, None);
    }

    #[test]
    fn postgres_float_range_order_places_nan_last() {
        let range = ScalarRange {
            lo: ScalarValue::F64(f64::NEG_INFINITY),
            hi: ScalarValue::F64(f64::INFINITY),
        };
        assert!(in_range(ScalarValue::F64(1.0), range).expect("range"));
        assert!(!in_range(ScalarValue::F64(f64::NAN), range).expect("range"));
    }

    #[test]
    fn temporal_host_filters_preserve_logical_scalar_identity() {
        let date = HostColumn::I32 {
            type_oid: DATEOID,
            values: vec![17],
            nulls: None,
        };
        let timestamp = HostColumn::I64 {
            type_oid: TIMESTAMPOID,
            values: vec![42],
            nulls: None,
        };
        let timestamptz = HostColumn::I64 {
            type_oid: TIMESTAMPTZOID,
            values: vec![84],
            nulls: None,
        };
        assert_eq!(date.filter_value(0), Ok(Some(ScalarValue::Date(17))));
        assert_eq!(
            timestamp.filter_value(0),
            Ok(Some(ScalarValue::Timestamp(42)))
        );
        assert_eq!(
            timestamptz.filter_value(0),
            Ok(Some(ScalarValue::TimestampTz(84)))
        );
        assert_eq!(
            scalar_cmp(ScalarValue::Date(1), ScalarValue::Date(2)),
            Ok(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn supported_group_type_constants_stay_distinct() {
        let supported = [
            BOOLOID,
            INT2OID,
            INT4OID,
            INT8OID,
            FLOAT4OID,
            FLOAT8OID,
            DATEOID,
            TIMESTAMPOID,
            TIMESTAMPTZOID,
            TEXTOID,
            BPCHAROID,
            VARCHAROID,
        ];
        assert_eq!(
            supported.into_iter().collect::<BTreeSet<_>>().len(),
            supported.len()
        );
    }
}

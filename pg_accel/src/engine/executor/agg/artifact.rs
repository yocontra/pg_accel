//! Begin-time host preparation and device ownership for generic aggregates.

use std::any::Any;
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use pgrx::pg_sys;

use crate::engine::residency::{
    DerivedArtifact, PreparedDerived, ResidentColumnRef, ResidentColumnView, ResidentInputBundle,
};
use crate::engine::spec::{
    AggQuerySpec, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeySource, JoinMultiplicity,
    MaskKind, MeasureExpr, ScalarRange, ScalarValue,
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

enum HostColumn<'a> {
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
        labels: Cow<'a, [String]>,
    },
}

impl<'a> HostColumn<'a> {
    /// Value/NULL lanes have already been downloaded; labels stay borrowed
    /// under the immutable resident input bundle. Final group datums still own
    /// their strings, so no resident-label borrow escapes preparation.
    fn text_from_copied_lanes(
        type_oid: u32,
        codes: Vec<i32>,
        nulls: Option<Vec<u8>>,
        labels: &'a [String],
    ) -> Self {
        Self::Text {
            type_oid,
            codes,
            nulls,
            labels: Cow::Borrowed(labels),
        }
    }

    fn copy_from(view: ResidentColumnView<'a>) -> Result<Self, String> {
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
                    labels: Cow::Borrowed(&[]),
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
            } => Self::text_from_copied_lanes(
                u32::from(type_oid),
                codes.copy_to_vec().map_err(|error| error.to_string())?,
                copy_nulls(nulls)?,
                labels,
            ),
            ResidentColumnView::H3 { type_oid, .. } => {
                return Err(format!(
                    "resident H3 type OID {} cannot enter dense host dictionary preparation",
                    u32::from(type_oid)
                ));
            }
            ResidentColumnView::Geometry { type_oid, .. } => {
                return Err(format!(
                    "resident geometry type OID {} cannot enter dense host dictionary preparation",
                    u32::from(type_oid)
                ));
            }
            ResidentColumnView::Numeric { type_oid, .. } => {
                return Err(format!(
                    "resident NUMERIC type OID {} requires exact decimal dictionary preparation",
                    u32::from(type_oid)
                ));
            }
            ResidentColumnView::Raster { type_oid, .. } => {
                return Err(format!(
                    "resident raster type OID {} requires the raster executor",
                    u32::from(type_oid)
                ));
            }
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
            INT4OID | INT8OID | TEXTOID | VARCHAROID | BPCHAROID => {
                Ok(self.dictionary_value(row)?.map(|(key, _)| key))
            }
            type_oid => Err(format!(
                "join type OID {type_oid} is not an INT4, INT8, or deterministic text-family key"
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

struct HostColumns<'a> {
    columns: BTreeMap<(u32, i16), HostColumn<'a>>,
    row_counts: BTreeMap<u32, usize>,
}

impl<'a> HostColumns<'a> {
    fn copy(
        requests: &[ResidentColumnRef],
        bundle: ResidentInputBundle<'a>,
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

    fn get(&self, column: ColumnRef) -> Result<&HostColumn<'a>, String> {
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
        FilterSpec::Ranges { input, .. }
        | FilterSpec::NullOrRanges { input, .. }
        | FilterSpec::Mask { input, .. } => {
            let attno = i16::try_from(input.attno)
                .map_err(|_| format!("attribute {} exceeds int16", input.attno))?;
            output.insert((input.relation_oid, attno));
            Ok(())
        }
        FilterSpec::Bytecode { .. }
        | FilterSpec::Spatial { .. }
        | FilterSpec::SpatialAnd { .. } => {
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
            GroupKeySource::H3CellToParent { cell, .. } => {
                insert_column_ref(&mut columns, *cell)?;
            }
            GroupKeySource::Expression { .. } | GroupKeySource::H3LatLngToCell { .. } => {
                return Err("Phase 5D supports only column group keys".to_owned());
            }
        }
    }
    for dimension in &spec.star_dims {
        insert_column_ref(&mut columns, dimension.fact_key)?;
        insert_column_ref(&mut columns, dimension.dim_key)?;
        collect_filter_column(&dimension.filter, &mut columns)?;
    }
    if spec
        .group_keys
        .iter()
        .any(|key| matches!(key.source, GroupKeySource::H3CellToParent { .. }))
        && let Some(input) = h3_count_filter(spec)?
    {
        insert_column_ref(&mut columns, input)?;
    }
    match &spec.fact_filter {
        FilterSpec::SpatialAnd { spatial, .. } => {
            let FilterSpec::Spatial { left, right, .. } = spatial.as_ref() else {
                return Err("invalid spatial conjunction".into());
            };
            for operand in [left, right] {
                if let Some(column) = operand.column() {
                    insert_column_ref(&mut columns, column)?;
                }
            }
        }
        FilterSpec::Mask { .. } => collect_filter_column(&spec.fact_filter, &mut columns)?,
        FilterSpec::Spatial { left, right, .. } => {
            for operand in [left, right] {
                if let Some(column) = operand.column() {
                    insert_column_ref(&mut columns, column)?;
                }
            }
        }
        FilterSpec::None
        | FilterSpec::Ranges { .. }
        | FilterSpec::NullOrRanges { .. }
        | FilterSpec::Bytecode { .. } => {}
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

fn filter_accepts(
    filter: &FilterSpec,
    columns: &HostColumns<'_>,
    row: usize,
) -> Result<bool, String> {
    match filter {
        FilterSpec::None => Ok(true),
        FilterSpec::Ranges { input, ranges } | FilterSpec::NullOrRanges { input, ranges } => {
            let Some(value) = columns.get(*input)?.filter_value(row)? else {
                return Ok(matches!(filter, FilterSpec::NullOrRanges { .. }));
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
        FilterSpec::Bytecode { .. }
        | FilterSpec::Spatial { .. }
        | FilterSpec::SpatialAnd { .. } => {
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

/// Reuse resident text codes and compact primitive domains while constructing
/// one grouping dictionary. Entries are filled lazily in row order: unused
/// resident labels never become groups, and the first visible SQL datum stays
/// the representative (including bpchar spellings and NULL-code placement).
enum DictionaryCodeCache {
    None,
    Text(Vec<i32>),
    Bool([i32; 2]),
    I32 { minimum: i32, codes: Vec<i32> },
}

struct ColumnDictionaryEncoder<'a> {
    column: &'a HostColumn<'a>,
    cache: DictionaryCodeCache,
}

impl<'a> ColumnDictionaryEncoder<'a> {
    fn new(column: &'a HostColumn<'a>) -> Self {
        const MAX_DIRECT_DOMAIN: i64 = 4096;
        let cache = match column {
            HostColumn::Text { labels, .. } => DictionaryCodeCache::Text(vec![-1; labels.len()]),
            HostColumn::Bool { .. } => DictionaryCodeCache::Bool([-1; 2]),
            HostColumn::I32 { values, nulls, .. } if !values.is_empty() => {
                let (minimum, maximum) = values
                    .iter()
                    .enumerate()
                    .filter(|(row, _)| nulls.as_ref().is_none_or(|nulls| nulls[*row] == 0))
                    .fold((i32::MAX, i32::MIN), |(lo, hi), (_, value)| {
                        (lo.min(*value), hi.max(*value))
                    });
                let span = i64::from(maximum) - i64::from(minimum) + 1;
                if (1..=MAX_DIRECT_DOMAIN).contains(&span) {
                    DictionaryCodeCache::I32 {
                        minimum,
                        codes: vec![
                            -1;
                            usize::try_from(span).expect("bounded positive i32 domain")
                        ],
                    }
                } else {
                    DictionaryCodeCache::None
                }
            }
            _ => DictionaryCodeCache::None,
        };
        Self { column, cache }
    }

    fn code_for(&mut self, row: usize, dictionary: &mut DictionaryBuilder) -> Result<i32, String> {
        if self.column.is_null(row)? {
            return dictionary.code_for(None);
        }
        let slot =
            match (&mut self.cache, self.column) {
                (DictionaryCodeCache::Text(remap), HostColumn::Text { codes, .. }) => {
                    let source = usize::try_from(codes[row])
                        .map_err(|_| "resident text code is negative".to_owned())?;
                    Some(remap.get_mut(source).ok_or_else(|| {
                        "resident text code exceeds its label dictionary".to_owned()
                    })?)
                }
                (DictionaryCodeCache::Bool(remap), HostColumn::Bool { values, .. }) => Some(
                    remap
                        .get_mut(usize::from(values[row]))
                        .ok_or_else(|| "resident boolean value is not canonical".to_owned())?,
                ),
                (DictionaryCodeCache::I32 { minimum, codes }, HostColumn::I32 { values, .. }) => {
                    let index = usize::try_from(i64::from(values[row]) - i64::from(*minimum))
                        .map_err(|_| "resident integer dictionary index is negative".to_owned())?;
                    Some(codes.get_mut(index).ok_or_else(|| {
                        "resident integer dictionary index exceeds domain".to_owned()
                    })?)
                }
                _ => None,
            };
        if let Some(slot) = slot {
            if *slot < 0 {
                *slot = dictionary.code_for(self.column.dictionary_value(row)?)?;
            }
            return Ok(*slot);
        }
        dictionary.code_for(self.column.dictionary_value(row)?)
    }
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

fn device_dimension_code(code: Option<usize>) -> Result<i32, String> {
    code.map_or(Ok(-1), |code| {
        i32::try_from(code).map_err(|_| "dimension dictionary exceeds i32 code space".to_owned())
    })
}

fn prepare_dimension(
    dimension: &crate::engine::spec::DimSpec,
    columns: &HostColumns<'_>,
    fact_rows: usize,
    pad_device_lanes: bool,
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

    let device_key_count = if pad_device_lanes {
        dim_rows
    } else {
        by_key.len()
    };
    let mut match_by_key = vec![0_u8; device_key_count];
    let mut multiplicity = (dimension.multiplicity == JoinMultiplicity::Counted)
        .then(|| vec![0_u64; device_key_count]);
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
    // A counted join only needs a multiplicity lane when at least one
    // filter-qualified key actually fans out. The match lane already carries
    // zero-versus-one membership, so retaining an all-zero/one multiplicity
    // buffer would needlessly force exact COUNT(*) through the serial weighted
    // aggregation path.
    if multiplicity
        .as_ref()
        .is_some_and(|values| values.iter().all(|&count| count <= 1))
    {
        multiplicity = None;
    }

    let mut fact_codes = Vec::with_capacity(fact_rows);
    for row in 0..fact_rows {
        let code = device_dimension_code(
            fact.join_key(row)?
                .and_then(|key| by_key.get(&key).copied()),
        )?;
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

/// Conservative upper bound for the derived device buffers built from a
/// logical descriptor spec. Raw resident columns are intentionally excluded.
///
/// `None` means a required relation estimate is absent, the key/filter shape
/// is outside the Phase 5D artifact subset, or checked byte arithmetic
/// overflowed. Callers must decline rather than treating `None` as zero.
#[allow(clippy::redundant_pub_crate)] // reason: re-exported crate-wide from this private module
pub(crate) fn estimate_descriptor_artifact_bytes_upper_bound(
    spec: &AggQuerySpec,
    relation_rows: &BTreeMap<u32, u64>,
) -> Option<u64> {
    let mut bytes = 0_u64;
    let mut add = |rows: u64, width: u64| -> Option<()> {
        bytes = bytes.checked_add(rows.checked_mul(width)?)?;
        Some(())
    };
    let fact_rows = || relation_rows.get(&spec.fact_rel).copied();

    for key in &spec.group_keys {
        match &key.source {
            GroupKeySource::FactColumn(column) if column.relation_oid == spec.fact_rel => {
                add(fact_rows()?, 4)?;
            }
            GroupKeySource::StarDimension { dim_index, .. } => {
                let dimension = spec.star_dims.get(usize::try_from(*dim_index).ok()?)?;
                add(*relation_rows.get(&dimension.relation_oid)?, 4)?;
            }
            GroupKeySource::H3CellToParent { cell, .. } if cell.relation_oid == spec.fact_rel => {
                add(fact_rows()?, 8)?;
            }
            GroupKeySource::FactColumn(_)
            | GroupKeySource::Expression { .. }
            | GroupKeySource::H3CellToParent { .. }
            | GroupKeySource::H3LatLngToCell { .. } => return None,
        }
    }

    if spec
        .group_keys
        .iter()
        .any(|key| matches!(key.source, GroupKeySource::H3CellToParent { .. }))
        && h3_count_filter(spec).ok()?.is_some()
    {
        add(fact_rows()?, 1)?;
    }
    for dimension in &spec.star_dims {
        let dimension_rows = *relation_rows.get(&dimension.relation_oid)?;
        add(fact_rows()?, 4)?;
        add(dimension_rows, 1)?;
        if dimension.multiplicity == JoinMultiplicity::Counted {
            add(dimension_rows, 8)?;
        }
    }

    match &spec.fact_filter {
        FilterSpec::Mask {
            kind: MaskKind::Sql,
            ..
        } => add(fact_rows()?, 1)?,
        FilterSpec::Spatial { .. } | FilterSpec::SpatialAnd { .. } => add(fact_rows()?, 1)?,
        FilterSpec::None
        | FilterSpec::Ranges { .. }
        | FilterSpec::NullOrRanges { .. }
        | FilterSpec::Bytecode { .. } => {}
        FilterSpec::Mask { .. } => {
            return None;
        }
    }
    Some(bytes)
}

/// Exact requested payload of newly cloned measure/WHERE-program vectors. Their
/// source capacity is irrelevant: cloning retains only the active elements.
#[allow(clippy::redundant_pub_crate)] // re-exported from the private artifact module
pub(crate) fn descriptor_program_clone_bytes(spec: &AggQuerySpec) -> Result<u64, String> {
    descriptor_program_bytes(spec, false)
}

fn descriptor_program_bytes(spec: &AggQuerySpec, retained: bool) -> Result<u64, String> {
    let mut programs = spec
        .measures
        .iter()
        .filter_map(|measure| match &measure.expression {
            MeasureExpr::Bytecode {
                inputs, program, ..
            } => Some((inputs, program)),
            _ => None,
        })
        .chain(match spec.fact_filter.scalar() {
            FilterSpec::Bytecode { inputs, program } => Some((inputs, program)),
            _ => None,
        });
    programs.try_fold(0_u64, |bytes, (inputs, program)| {
        let inputs = if retained {
            inputs.capacity()
        } else {
            inputs.len()
        };
        let words = if retained {
            program.capacity()
        } else {
            program.len()
        };
        inputs
            .checked_mul(size_of::<ColumnRef>())
            .and_then(|input_bytes| {
                words
                    .checked_mul(size_of::<i32>())
                    .and_then(|words| words.checked_add(input_bytes))
            })
            .and_then(|payload| u64::try_from(payload).ok())
            .and_then(|payload| bytes.checked_add(payload))
            .ok_or_else(|| "descriptor program payload bytes overflow".into())
    })
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

fn prepare_agg_artifact_impl(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    bundle: ResidentInputBundle<'_>,
    max_groups: usize,
    pad_device_lanes: bool,
) -> Result<PreparedDerived<PreparedAggArtifact>, String> {
    let columns = HostColumns::copy(requests, bundle)?;
    let fact_rows = columns.row_count(spec.fact_rel)?;
    let mut dimensions = Vec::with_capacity(spec.star_dims.len());
    for dimension in &spec.star_dims {
        dimensions.push(prepare_dimension(
            dimension,
            &columns,
            fact_rows,
            pad_device_lanes,
        )?);
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
                let mut encoder = ColumnDictionaryEncoder::new(column);
                let mut codes = Vec::with_capacity(fact_rows);
                for row in 0..fact_rows {
                    codes.push(encoder.code_for(row, &mut dictionary)?);
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
                let mut encoder = ColumnDictionaryEncoder::new(group_column);
                let lookup_len = if pad_device_lanes {
                    group_column.len()
                } else {
                    dimension.match_by_key.len()
                };
                let mut lookup = vec![0_i32; lookup_len];
                for row in 0..group_column.len() {
                    let (Some(join_code), true) =
                        (dimension.row_codes[row], dimension.row_matches[row])
                    else {
                        continue;
                    };
                    lookup[join_code] = encoder.code_for(row, &mut dictionary)?;
                }
                PreparedKeyInput::Dimension { dim_index, lookup }
            }
            GroupKeySource::Expression { .. }
            | GroupKeySource::H3CellToParent { .. }
            | GroupKeySource::H3LatLngToCell { .. } => {
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
        FilterSpec::None
        | FilterSpec::Ranges { .. }
        | FilterSpec::NullOrRanges { .. }
        | FilterSpec::Bytecode { .. } => None,
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

pub(super) fn prepare_agg_artifact(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    bundle: ResidentInputBundle<'_>,
    max_groups: usize,
) -> Result<PreparedDerived<PreparedAggArtifact>, String> {
    prepare_agg_artifact_impl(spec, requests, bundle, max_groups, false)
}

pub(super) fn prepare_spatial_base_artifact(
    spec: &AggQuerySpec,
    requests: &[ResidentColumnRef],
    bundle: ResidentInputBundle<'_>,
    spatial_column: ResidentColumnRef,
    max_groups: usize,
) -> Result<PreparedDerived<PreparedAggArtifact>, String> {
    if requests.len() != bundle.columns.len() {
        return Err("resident spatial base column count changed during preparation".to_owned());
    }
    let ResidentInputBundle { columns, evidence } = bundle;
    let mut base_requests = Vec::with_capacity(requests.len().saturating_sub(1));
    let mut base_columns = Vec::with_capacity(columns.len().saturating_sub(1));
    let mut found_spatial = false;
    for (request, column) in requests.iter().copied().zip(columns) {
        if request == spatial_column {
            if found_spatial {
                return Err("spatial source column request is duplicated".to_owned());
            }
            found_spatial = true;
        } else {
            base_requests.push(request);
            base_columns.push(column);
        }
    }
    if !found_spatial {
        return Err("spatial source column was not requested for artifact preparation".to_owned());
    }
    prepare_agg_artifact_impl(
        spec,
        &base_requests,
        ResidentInputBundle {
            columns: base_columns,
            evidence,
        },
        max_groups,
        true,
    )
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
    program_host_bytes: u64,
}

/// Device-derived H3 parent lane for the exact HASH/COMPACT grouped path.
///
/// The raw source column owns the NULL sidecar. This artifact owns only the
/// transformed parent values plus an optional exact Boolean FILTER mask.
/// Both lanes are pinned to the same dependency generations and snapshot.
pub(super) struct H3ParentArtifact {
    pub resolved_spec: AggQuerySpec,
    pub fact_rows: usize,
    group_capacity: Cell<usize>,
    pub parents: Option<ExprDeviceBuffer<u64>>,
    pub filter_mask: Option<ExprDeviceBuffer<i8>>,
    device_bytes: u64,
}

/// Physical measure FILTER after exact WHERE implication. A direct Boolean
/// predicate identical to WHERE is TRUE for every selected row, so it needs
/// neither another mask nor a per-measure device test. Validate before folding:
/// matching malformed or recheck predicates must never become unfiltered.
pub(super) fn h3_count_filter(spec: &AggQuerySpec) -> Result<Option<ColumnRef>, String> {
    let [measure] = spec.measures.as_slice() else {
        return Err("hash binding requires one measure".into());
    };
    let filter = hash_boolean_filter(spec, &measure.filter)?;
    Ok(if measure.filter == spec.fact_filter {
        None
    } else {
        filter
    })
}

pub(super) fn hash_where_filter(spec: &AggQuerySpec) -> Result<Option<ColumnRef>, String> {
    hash_boolean_filter(spec, &spec.fact_filter)
}

fn hash_boolean_filter(
    spec: &AggQuerySpec,
    filter: &FilterSpec,
) -> Result<Option<ColumnRef>, String> {
    match *filter {
        FilterSpec::None => Ok(None),
        FilterSpec::Mask {
            input,
            kind: MaskKind::Sql,
        } if input.relation_oid == spec.fact_rel
            && input.attno > 0
            && input.type_oid == BOOLOID =>
        {
            Ok(Some(input))
        }
        _ => Err("hash predicate requires an exact resident fact Boolean".into()),
    }
}

pub(super) fn h3_parent_artifact_bytes(spec: &AggQuerySpec, rows: usize) -> Result<u64, String> {
    let width = std::mem::size_of::<u64>() + usize::from(h3_count_filter(spec)?.is_some());
    rows.checked_mul(width)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| "derived H3 parent/mask byte count overflow".to_owned())
}

fn h3_artifact_lane_bytes(
    spec: &AggQuerySpec,
    rows: usize,
    parent_rows: usize,
    mask_rows: usize,
) -> Result<u64, String> {
    if parent_rows != rows {
        return Err("derived H3 parent lane length does not match fact rows".to_owned());
    }
    let expected_mask_rows = if h3_count_filter(spec)?.is_some() {
        rows
    } else {
        0
    };
    if mask_rows != expected_mask_rows {
        return Err("derived H3 FILTER mask length does not match its logical input".into());
    }
    h3_parent_artifact_bytes(spec, rows)
}

impl H3ParentArtifact {
    pub(super) fn new(
        resolved_spec: AggQuerySpec,
        fact_rows: usize,
        group_capacity: usize,
        parents: Option<ExprDeviceBuffer<u64>>,
        filter_mask: Option<ExprDeviceBuffer<i8>>,
    ) -> Result<Self, String> {
        if hash_where_filter(&resolved_spec)?.is_some() {
            return Err("H3 WHERE requires fused key evaluation after row rejection".into());
        }
        let device_bytes = h3_artifact_lane_bytes(
            &resolved_spec,
            fact_rows,
            parents.as_ref().map_or(0, ExprDeviceBuffer::len),
            filter_mask.as_ref().map_or(0, ExprDeviceBuffer::len),
        )?;
        Ok(Self {
            resolved_spec,
            fact_rows,
            group_capacity: Cell::new(group_capacity),
            parents,
            filter_mask,
            device_bytes,
        })
    }

    pub(super) fn group_capacity(&self) -> usize {
        self.group_capacity.get()
    }

    /// Call only after exact keys/count partition validation. This is a bound
    /// proved by complete GPU execution, not a planner estimate. The parent
    /// values and source NULL sidecar are immutable for this artifact's pinned
    /// dependency generations; invalidation destroys this learned bound too.
    pub(super) fn record_validated_groups(&self, emitted: usize) -> Result<(), String> {
        let bound = learned_hash_group_bound(self.fact_rows, self.group_capacity.get(), emitted)?;
        self.group_capacity.set(bound);
        Ok(())
    }
}

pub(super) fn learned_hash_group_bound(
    rows: usize,
    previous: usize,
    emitted: usize,
) -> Result<usize, String> {
    if previous == 0 || emitted > previous || emitted > rows || (rows != 0 && emitted == 0) {
        return Err("validated hash group count violates its input/capacity bounds".to_owned());
    }
    Ok(emitted.max(1))
}

impl DerivedArtifact for H3ParentArtifact {
    fn device_bytes(&self) -> u64 {
        self.device_bytes
    }

    fn retained_host_exact_bytes(&self) -> u64 {
        0
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DescriptorAggArtifact {
    pub(super) fn build(prepared: PreparedAggArtifact) -> Result<Self, String> {
        let program_host_bytes = descriptor_program_bytes(&prepared.resolved_spec, true)?;
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
            program_host_bytes,
        })
    }
}

impl DerivedArtifact for DescriptorAggArtifact {
    fn device_bytes(&self) -> u64 {
        self.device_bytes
    }

    fn retained_host_exact_bytes(&self) -> u64 {
        self.program_host_bytes
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn learned_h3_capacity_requires_a_complete_possible_group_count() {
        assert_eq!(
            super::learned_hash_group_bound(12_748_986, 12_748_986, 4467),
            Ok(4467)
        );
        assert_eq!(
            super::learned_hash_group_bound(12_748_986, 4467, 4467),
            Ok(4467)
        );
        assert_eq!(super::learned_hash_group_bound(0, 1, 0), Ok(1));
        assert_eq!(
            super::learned_hash_group_bound(2_000_019, 2_000_019, 1),
            Ok(1)
        );
        for (rows, previous, emitted) in [(100, 10, 11), (10, 100, 11), (10, 10, 0), (0, 0, 0)] {
            assert!(super::learned_hash_group_bound(rows, previous, emitted).is_err());
        }
    }

    use super::*;
    use crate::engine::spec::{
        AggregateKind, AggregateOutput, AggregateSource, GroupKeyRef, MeasureExpr, MeasureSpec,
        SpatialOperand, SpatialPredicateKind, SpatialValueKind, SpatialValueMetadata,
    };

    fn column(relation_oid: u32, type_oid: u32) -> ColumnRef {
        ColumnRef {
            relation_oid,
            attno: 1,
            type_oid,
        }
    }

    fn host_columns<'a>(fact: HostColumn<'a>, dimension: HostColumn<'a>) -> HostColumns<'a> {
        let fact_rows = fact.len();
        let dimension_rows = dimension.len();
        HostColumns {
            columns: BTreeMap::from([((100, 1), fact), ((200, 1), dimension)]),
            row_counts: BTreeMap::from([(100, fact_rows), (200, dimension_rows)]),
        }
    }

    fn dimension(type_oid: u32, multiplicity: JoinMultiplicity) -> crate::engine::spec::DimSpec {
        crate::engine::spec::DimSpec {
            relation_oid: 200,
            fact_key: column(100, type_oid),
            dim_key: column(200, type_oid),
            collation_oid: u32::from(matches!(type_oid, TEXTOID | BPCHAROID | VARCHAROID)),
            multiplicity,
            filter: FilterSpec::None,
        }
    }

    fn count_star_spec() -> AggQuerySpec {
        AggQuerySpec {
            fact_rel: 100,
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
        }
    }

    #[test]
    fn descriptor_program_host_accounting_matches_cloned_vector_payloads() {
        let mut spec = count_star_spec();
        let mut inputs = Vec::with_capacity(16);
        inputs.push(column(100, INT4OID));
        let mut program = Vec::with_capacity(128);
        program.extend([
            crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC,
            2,
            1,
            2,
            0,
            0,
        ]);
        spec.measures[0].expression = MeasureExpr::Bytecode {
            inputs,
            program,
            result_type_oid: INT4OID,
        };
        assert!(
            descriptor_program_bytes(&spec, true).expect("source capacity")
                > descriptor_program_clone_bytes(&spec).expect("clone size")
        );
        for states in 1..=4 {
            spec.measures.resize(states, spec.measures[0].clone());
            let forecast = descriptor_program_clone_bytes(&spec).expect("clone forecast");
            assert_eq!(
                forecast,
                states as u64 * (6 * 4 + size_of::<ColumnRef>() as u64)
            );
            let artifact = DescriptorAggArtifact::build(PreparedAggArtifact {
                resolved_spec: spec.clone(),
                fact_rows: 0,
                group_capacity: 1,
                keys: vec![],
                dimensions: vec![],
                fact_mask: None,
                domains: Rc::from([]),
                device_bytes: 0,
            })
            .expect("empty device artifact with real cloned program vectors");
            assert_eq!(artifact.device_bytes(), 0);
            assert_eq!(artifact.retained_host_exact_bytes(), forecast);
        }
        assert_eq!(descriptor_program_clone_bytes(&count_star_spec()), Ok(0));
    }

    fn spatial_filter() -> FilterSpec {
        let metadata = SpatialValueMetadata {
            kind: SpatialValueKind::Geometry,
            typmod: -1,
            srid: Some(4_326),
        };
        let mut spatial_column = column(100, 60_001);
        spatial_column.attno = 2;
        FilterSpec::Spatial {
            predicate: SpatialPredicateKind::Intersects,
            left: SpatialOperand::Column {
                column: spatial_column,
                metadata,
            },
            right: SpatialOperand::Constant {
                metadata,
                bytes: vec![1_u8].into_boxed_slice(),
            },
            distance: None,
        }
    }

    #[test]
    fn artifact_estimator_matches_every_runtime_device_buffer_term() {
        let mut spec = count_star_spec();
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(100, INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::StarDimension {
                dim_index: 0,
                group_column: column(200, INT4OID),
            },
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        spec.star_dims
            .push(dimension(INT4OID, JoinMultiplicity::Unique));
        let mut counted = dimension(INT4OID, JoinMultiplicity::Counted);
        counted.relation_oid = 300;
        counted.dim_key.relation_oid = 300;
        counted.fact_key.attno = 2;
        spec.star_dims.push(counted);
        spec.fact_filter = FilterSpec::Mask {
            input: column(100, BOOLOID),
            kind: MaskKind::Sql,
        };

        let rows = BTreeMap::from([(100, 100), (200, 10), (300, 20)]);
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &rows),
            Some(1_530)
        );
    }

    #[test]
    fn artifact_estimator_is_zero_for_a_bufferless_count_star() {
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&count_star_spec(), &BTreeMap::new()),
            Some(0)
        );
    }

    #[test]
    fn h3_parent_artifact_estimator_charges_exactly_eight_bytes_per_fact_row() {
        const H3OID: u32 = 90_001;
        let mut spec = count_star_spec();
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::H3CellToParent {
                cell: column(100, H3OID),
                resolution: 7,
            },
            type_oid: H3OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &BTreeMap::from([(100, 123)])),
            Some(984)
        );
        let refs = artifact_column_refs(&spec).expect("H3 source should be resident");
        assert_eq!(refs.len(), 1);
        assert_eq!(u32::from(refs[0].relid), 100);
        assert_eq!(refs[0].attno, 1);
    }

    #[test]
    fn filtered_h3_artifact_forecast_matches_lanes_and_dependency_union() {
        let mut spec = count_star_spec();
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::H3CellToParent {
                cell: column(100, 90_001),
                resolution: 0,
            },
            type_oid: 90_001,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        let unfiltered = spec.clone();
        spec.measures[0].filter = FilterSpec::Mask {
            input: ColumnRef {
                attno: 3,
                ..column(100, BOOLOID)
            },
            kind: MaskKind::Sql,
        };
        for rows in [0, 1, 65535, 65536, 65537, 12_748_986] {
            let expected = rows as u64 * 9;
            assert_eq!(h3_parent_artifact_bytes(&spec, rows), Ok(expected));
            assert_eq!(
                h3_artifact_lane_bytes(&spec, rows, rows, rows),
                Ok(expected)
            );
            assert_eq!(
                estimate_descriptor_artifact_bytes_upper_bound(
                    &spec,
                    &BTreeMap::from([(100, rows as u64)])
                ),
                Some(expected)
            );
            assert_eq!(
                h3_artifact_lane_bytes(&unfiltered, rows, rows, 0),
                Ok(rows as u64 * 8)
            );
            assert!(h3_artifact_lane_bytes(&spec, rows, rows + 1, rows).is_err());
            if rows != 0 {
                assert!(h3_artifact_lane_bytes(&spec, rows, rows, 0).is_err());
                assert!(h3_artifact_lane_bytes(&unfiltered, rows, rows, rows).is_err());
            }
        }
        assert!(h3_parent_artifact_bytes(&spec, usize::MAX / 9 + 1).is_err());
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(
                &spec,
                &BTreeMap::from([(100, u64::MAX / 9 + 1)])
            ),
            None
        );
        assert_eq!(
            artifact_column_refs(&spec)
                .expect("predicate dependency")
                .iter()
                .map(|r| r.attno)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        spec.measures[0].filter = FilterSpec::Mask {
            input: column(200, BOOLOID),
            kind: MaskKind::Sql,
        };
        assert!(h3_parent_artifact_bytes(&spec, 1).is_err());
        assert!(artifact_column_refs(&spec).is_err());
        assert!(H3ParentArtifact::new(spec, 0, 1, None, None).is_err());
    }

    #[test]
    fn spatial_artifact_estimator_adds_exact_mask_bytes_and_fails_on_overflow() {
        let mut spec = count_star_spec();
        spec.fact_filter = spatial_filter();
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &BTreeMap::from([(100, 123)])),
            Some(123)
        );
        let refs = artifact_column_refs(&spec).expect("spatial source should be resident");
        assert_eq!(refs.len(), 1);
        assert_eq!(u32::from(refs[0].relid), 100);
        assert_eq!(refs[0].attno, 2);

        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(100, INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &BTreeMap::from([(100, 123)])),
            Some(123 * 5),
            "four-byte base key plus one-byte spatial mask"
        );
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(
                &spec,
                &BTreeMap::from([(100, u64::MAX / 4)])
            ),
            None
        );
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &BTreeMap::new()),
            None
        );
    }

    #[test]
    fn artifact_estimator_fails_closed_on_missing_rows_and_overflow() {
        let mut spec = count_star_spec();
        spec.group_keys.push(GroupKeyRef {
            source: GroupKeySource::FactColumn(column(100, INT4OID)),
            type_oid: INT4OID,
            collation_oid: 0,
            encoding: GroupKeyEncoding::Hash,
        });
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(
                &spec,
                &BTreeMap::from([(100, u64::MAX)])
            ),
            None
        );

        spec.group_keys[0].source = GroupKeySource::StarDimension {
            dim_index: 0,
            group_column: column(200, INT4OID),
        };
        spec.star_dims
            .push(dimension(INT4OID, JoinMultiplicity::Unique));
        assert_eq!(
            estimate_descriptor_artifact_bytes_upper_bound(&spec, &BTreeMap::from([(100, 1)])),
            None
        );
    }

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
    fn dictionary_code_cache_matches_rowwise_reference() {
        let compare = |column: HostColumn<'_>, rows: &[usize]| {
            column.validate_nulls().expect("fixture null bitmap");
            let mut reference = DictionaryBuilder::new(column.type_oid(), 950);
            let mut optimized = DictionaryBuilder::new(column.type_oid(), 950);
            let mut encoder = ColumnDictionaryEncoder::new(&column);
            for row in rows {
                assert_eq!(
                    encoder.code_for(*row, &mut optimized),
                    reference.code_for(column.dictionary_value(*row).expect("reference value"))
                );
            }
            assert_eq!(optimized.finish(), reference.finish());
        };
        let rows = [0, 1, 2, 3, 4, 5, 6, 6, 0, 2];
        for type_oid in [TEXTOID, VARCHAROID, BPCHAROID] {
            compare(
                HostColumn::Text {
                    type_oid,
                    codes: vec![-7, 3, 1, 2, 0, 3, 5],
                    nulls: Some(vec![1, 0, 0, 0, 0, 0, 1]),
                    labels: vec![
                        "x".into(),
                        "x ".into(),
                        "z".into(),
                        "x  ".into(),
                        "unused".into(),
                    ]
                    .into(),
                },
                &rows,
            );
        }
        compare(
            HostColumn::Bool {
                type_oid: BOOLOID,
                values: vec![0, 1, 0, 1, 1, 0, 0],
                nulls: Some(vec![1, 0, 0, 0, 0, 0, 1]),
            },
            &rows,
        );
        for values in [
            vec![0, 9001, 9002, 9001, 9003, 9002, i32::MIN],
            vec![0, i32::MIN, i32::MAX, -1, 0, 1, 0],
        ] {
            compare(
                HostColumn::I32 {
                    type_oid: DATEOID,
                    values,
                    nulls: Some(vec![1, 0, 0, 0, 0, 0, 1]),
                },
                &rows,
            );
        }
        // Dimension grouping encodes only matched rows; unused labels must not
        // reserve codes, and the representative must be the first matched row.
        compare(
            HostColumn::Text {
                type_oid: BPCHAROID,
                codes: vec![0, 1, 2, 3],
                nulls: None,
                labels: vec!["unused".into(), "a ".into(), "a".into(), "b".into()].into(),
            },
            &[2, 3, 1],
        );
    }

    #[test]
    fn borrowed_labels_preserve_codes_and_owned_group_representatives() {
        for type_oid in [TEXTOID, VARCHAROID, BPCHAROID] {
            let mut labels = vec![
                "a  ".to_owned(),
                "a".to_owned(),
                String::new(),
                "λ\u{a0}".to_owned(),
                "unused".to_owned(),
            ];
            let (actual, expected) = {
                let codes = vec![-1, 2, 0, 1, 3, 0, i32::MAX];
                let nulls = Some(vec![1, 0, 0, 0, 0, 0, 1]);
                let borrowed = HostColumn::text_from_copied_lanes(
                    type_oid,
                    codes.clone(),
                    nulls.clone(),
                    &labels,
                );
                let owned = HostColumn::Text {
                    type_oid,
                    codes,
                    nulls,
                    labels: Cow::Owned(labels.clone()),
                };
                borrowed.validate_nulls().expect("valid sidecar");
                let HostColumn::Text {
                    labels: Cow::Borrowed(view),
                    ..
                } = &borrowed
                else {
                    panic!("resident labels must not be cloned");
                };
                assert!(std::ptr::eq(*view, labels.as_slice()));
                let mut actual = DictionaryBuilder::new(type_oid, 950);
                let mut expected = DictionaryBuilder::new(type_oid, 950);
                let mut encoder = ColumnDictionaryEncoder::new(&borrowed);
                for row in [2, 0, 1, 3, 4, 5, 6, 2] {
                    assert_eq!(
                        encoder.code_for(row, &mut actual),
                        expected.code_for(owned.dictionary_value(row).expect("owned reference"))
                    );
                }
                (actual.finish(), expected.finish())
            };
            // Simulate replacement/removal of the resident decoder only after
            // the immutable preparation borrow has ended. Final output owns
            // its representatives, including the first bpchar spelling.
            labels.iter_mut().for_each(String::clear);
            drop(labels);
            assert_eq!(actual, expected);
            assert_eq!(actual.values[0], GroupDatum::Text("a  ".to_owned()));
            assert_eq!(actual.null_code, Some(1));
            assert!(
                !actual
                    .values
                    .contains(&GroupDatum::Text("unused".to_owned()))
            );
        }
    }

    #[test]
    fn unused_high_cardinality_labels_stay_borrowed() {
        let labels = (0..100_000)
            .map(|index| format!("label-{index:06}-{}", "x".repeat(64)))
            .collect::<Vec<_>>();
        let clone_payload_bytes = labels.len() * std::mem::size_of::<String>()
            + labels.iter().map(String::len).sum::<usize>();
        assert_eq!(
            clone_payload_bytes,
            7_700_000 + 100_000 * std::mem::size_of::<String>()
        );
        let column =
            HostColumn::text_from_copied_lanes(TEXTOID, (0..100_000).collect(), None, &labels);
        let HostColumn::Text {
            labels: Cow::Borrowed(view),
            ..
        } = &column
        else {
            panic!("unused strings must remain in resident storage");
        };
        assert!(std::ptr::eq(*view, labels.as_slice()));
        let mut dictionary = DictionaryBuilder::new(TEXTOID, 950);
        let mut encoder = ColumnDictionaryEncoder::new(&column);
        // A dimension filter may select just the last row of a complete
        // resident column; the other labels must not become output groups.
        assert_eq!(encoder.code_for(99_999, &mut dictionary), Ok(0));
        assert_eq!(encoder.code_for(99_999, &mut dictionary), Ok(0));
        let domain = dictionary.finish();
        assert_eq!(
            domain.values,
            vec![GroupDatum::Text(labels[99_999].clone())]
        );
        // The existing per-label i32 remap is still allocated. This change
        // removes only the redundant String vector and label byte clones.
        let DictionaryCodeCache::Text(remap) = encoder.cache else {
            panic!("text remap");
        };
        assert_eq!(remap.len(), labels.len());
        assert_eq!(remap.iter().filter(|code| **code >= 0).count(), 1);
    }

    #[test]
    fn borrowed_decoder_keeps_empty_and_invalid_input_checks() {
        let labels = vec!["only".to_owned()];
        for code in [-1, 1, i32::MAX] {
            let column = HostColumn::text_from_copied_lanes(TEXTOID, vec![code], None, &labels);
            let mut dictionary = DictionaryBuilder::new(TEXTOID, 950);
            let mut encoder = ColumnDictionaryEncoder::new(&column);
            assert!(encoder.code_for(0, &mut dictionary).is_err());
            assert!(dictionary.values.is_empty());
        }
        for nulls in [vec![], vec![2], vec![0, 0]] {
            let column = HostColumn::text_from_copied_lanes(TEXTOID, vec![0], Some(nulls), &labels);
            assert!(column.validate_nulls().is_err());
        }
        let empty = HostColumn::text_from_copied_lanes(TEXTOID, Vec::new(), None, &[]);
        empty.validate_nulls().expect("empty decoder");
        assert!(empty.dictionary_value(0).is_err());
        assert_eq!(labels, ["only"]);
    }

    #[test]
    fn borrowed_join_decoders_keep_independent_domains_and_owned_outputs() {
        let prepared = {
            let fact = vec!["APAC".to_owned(), "EU   ".to_owned(), "NA".to_owned()];
            let dimension_labels = vec!["EU".to_owned(), "NA".to_owned(), "APAC".to_owned()];
            let columns = host_columns(
                HostColumn::text_from_copied_lanes(BPCHAROID, vec![1, 0, 2], None, &fact),
                HostColumn::text_from_copied_lanes(
                    BPCHAROID,
                    vec![2, 0, 1],
                    None,
                    &dimension_labels,
                ),
            );
            prepare_dimension(
                &dimension(BPCHAROID, JoinMultiplicity::Unique),
                &columns,
                3,
                false,
            )
            .expect("independent borrowed domains")
        };
        assert_eq!(prepared.fact_codes, [1, 0, 2]);
        assert_eq!(prepared.match_by_key, [1, 1, 1]);
    }

    #[test]
    fn dictionary_cache_preserves_invalid_code_and_empty_boundaries() {
        for code in [-1, 1, i32::MAX] {
            let column = HostColumn::Text {
                type_oid: TEXTOID,
                codes: vec![code],
                nulls: None,
                labels: vec!["only-label".into()].into(),
            };
            let mut dictionary = DictionaryBuilder::new(TEXTOID, 950);
            let mut encoder = ColumnDictionaryEncoder::new(&column);
            assert!(encoder.code_for(0, &mut dictionary).is_err());
            assert!(encoder.code_for(1, &mut dictionary).is_err());
            assert!(dictionary.values.is_empty());
        }
        let column = HostColumn::Text {
            type_oid: TEXTOID,
            codes: Vec::new(),
            nulls: None,
            labels: vec!["unused".into()].into(),
        };
        let mut dictionary = DictionaryBuilder::new(TEXTOID, 950);
        let mut encoder = ColumnDictionaryEncoder::new(&column);
        assert!(encoder.code_for(0, &mut dictionary).is_err());
        assert_eq!(dictionary.finish().values, vec![GroupDatum::Unused]);

        let all_null = HostColumn::I32 {
            type_oid: DATEOID,
            values: vec![i32::MIN, i32::MAX],
            nulls: Some(vec![1, 1]),
        };
        let mut dictionary = DictionaryBuilder::new(DATEOID, 0);
        let mut encoder = ColumnDictionaryEncoder::new(&all_null);
        assert!(matches!(encoder.cache, DictionaryCodeCache::None));
        assert_eq!(encoder.code_for(0, &mut dictionary), Ok(0));
        assert_eq!(encoder.code_for(1, &mut dictionary), Ok(0));
        assert_eq!(dictionary.finish().values, vec![GroupDatum::Null]);
    }

    #[test]
    fn dictionary_cache_is_bounded_and_lazy_per_resident_label() {
        let column = HostColumn::Text {
            type_oid: BPCHAROID,
            codes: (0..100_000).map(|row| row % 3).collect(),
            nulls: None,
            labels: vec!["a ".into(), "a".into(), "b".into(), "unused".into()].into(),
        };
        let mut dictionary = DictionaryBuilder::new(BPCHAROID, 950);
        let mut encoder = ColumnDictionaryEncoder::new(&column);
        for row in 0..column.len() {
            assert_eq!(
                encoder.code_for(row, &mut dictionary),
                Ok(i32::from(row % 3 == 2))
            );
        }
        let DictionaryCodeCache::Text(remap) = encoder.cache else {
            panic!("resident text cache")
        };
        assert_eq!(remap, [0, 0, 1, -1]);
        assert_eq!(
            dictionary.finish().values,
            vec![GroupDatum::Text("a ".into()), GroupDatum::Text("b".into())]
        );

        for (maximum, cached) in [(4095, true), (4096, false), (i32::MAX, false)] {
            let column = HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![0, maximum],
                nulls: None,
            };
            let encoder = ColumnDictionaryEncoder::new(&column);
            assert_eq!(
                matches!(encoder.cache, DictionaryCodeCache::I32 { .. }),
                cached
            );
        }
        let column = HostColumn::I32 {
            type_oid: DATEOID,
            values: vec![0, 9001, 9003],
            nulls: Some(vec![1, 0, 0]),
        };
        assert!(matches!(
            ColumnDictionaryEncoder::new(&column).cache,
            DictionaryCodeCache::I32 { minimum: 9001, .. }
        ));
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
    fn int4_join_correlation_never_matches_null_or_missing_keys() {
        let columns = host_columns(
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 2, 3, 0],
                nulls: Some(vec![0, 0, 0, 1]),
            },
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![3, 1, 0],
                nulls: Some(vec![0, 0, 1]),
            },
        );
        let prepared = prepare_dimension(
            &dimension(INT4OID, JoinMultiplicity::Unique),
            &columns,
            4,
            false,
        )
        .expect("unique INT4 dimension prepares");
        assert_eq!(prepared.fact_codes, vec![1, -1, 0, -1]);
        assert_eq!(prepared.row_codes, vec![Some(0), Some(1), None]);
        assert_eq!(prepared.match_by_key, vec![1, 1]);
        assert_eq!(prepared.multiplicity_by_key, None);
    }

    #[test]
    fn int8_join_correlation_preserves_extremes_nulls_and_counted_duplicates() {
        let columns = host_columns(
            HostColumn::I64 {
                type_oid: INT8OID,
                values: vec![i64::MIN, -7, i64::MAX, 42, 0],
                nulls: Some(vec![0, 0, 0, 0, 1]),
            },
            HostColumn::I64 {
                type_oid: INT8OID,
                values: vec![-7, -7, i64::MAX, i64::MIN, 0],
                nulls: Some(vec![0, 0, 0, 0, 1]),
            },
        );
        let prepared = prepare_dimension(
            &dimension(INT8OID, JoinMultiplicity::Counted),
            &columns,
            5,
            false,
        )
        .expect("counted INT8 dimension prepares losslessly");

        assert_eq!(prepared.fact_codes, vec![2, 0, 1, -1, -1]);
        assert_eq!(
            prepared.row_codes,
            vec![Some(0), Some(0), Some(1), Some(2), None]
        );
        assert_eq!(prepared.match_by_key, vec![1, 1, 1]);
        assert_eq!(prepared.multiplicity_by_key, Some(vec![2, 1, 1]));
    }

    #[test]
    fn unique_int8_dimension_rejects_duplicate_nonnull_keys() {
        let columns = host_columns(
            HostColumn::I64 {
                type_oid: INT8OID,
                values: vec![-7],
                nulls: None,
            },
            HostColumn::I64 {
                type_oid: INT8OID,
                values: vec![-7, -7],
                nulls: None,
            },
        );
        let error = match prepare_dimension(
            &dimension(INT8OID, JoinMultiplicity::Unique),
            &columns,
            1,
            false,
        ) {
            Ok(_) => panic!("duplicate UNIQUE INT8 dimension must fail closed"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            "dimension declared UNIQUE contains a duplicate non-NULL join key"
        );
    }

    #[test]
    fn bpchar_join_correlates_independent_dictionaries() {
        let columns = host_columns(
            HostColumn::Text {
                type_oid: BPCHAROID,
                codes: vec![1, 0, 2],
                nulls: None,
                labels: vec!["APAC".into(), "EU   ".into(), "NA".into()].into(),
            },
            HostColumn::Text {
                type_oid: BPCHAROID,
                codes: vec![2, 0, 1],
                nulls: None,
                labels: vec!["EU".into(), "NA".into(), "APAC".into()].into(),
            },
        );
        let prepared = prepare_dimension(
            &dimension(BPCHAROID, JoinMultiplicity::Unique),
            &columns,
            3,
            false,
        )
        .expect("independent BPCHAR dictionaries correlate");
        assert_eq!(prepared.fact_codes, vec![1, 0, 2]);
        assert_eq!(prepared.match_by_key, vec![1, 1, 1]);
    }

    #[test]
    fn counted_dimension_preserves_filtered_multiplicity() {
        let columns = host_columns(
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 2, 3, 0],
                nulls: Some(vec![0, 0, 0, 1]),
            },
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 1, 2, 0],
                nulls: Some(vec![0, 0, 0, 1]),
            },
        );
        let prepared = prepare_dimension(
            &dimension(INT4OID, JoinMultiplicity::Counted),
            &columns,
            4,
            false,
        )
        .expect("counted dimension prepares");
        assert_eq!(prepared.fact_codes, vec![0, 1, -1, -1]);
        assert_eq!(prepared.multiplicity_by_key, Some(vec![2, 1]));
    }

    #[test]
    fn filtered_counted_dimension_without_fanout_uses_match_lane_only() {
        let columns = host_columns(
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 2, 3, 4],
                nulls: None,
            },
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 2, 4],
                nulls: None,
            },
        );
        let mut counted = dimension(INT4OID, JoinMultiplicity::Counted);
        counted.filter = FilterSpec::Ranges {
            input: column(200, INT4OID),
            ranges: vec![ScalarRange {
                lo: ScalarValue::I32(1),
                hi: ScalarValue::I32(2),
            }],
        };
        let prepared = prepare_dimension(&counted, &columns, 4, false)
            .expect("filtered non-fanout counted dimension prepares");

        assert_eq!(prepared.fact_codes, vec![0, 1, -1, 2]);
        assert_eq!(prepared.match_by_key, vec![1, 1, 0]);
        assert_eq!(prepared.multiplicity_by_key, None);
    }

    #[test]
    fn unique_dimension_rejects_duplicate_nonnull_keys() {
        let columns = host_columns(
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1],
                nulls: None,
            },
            HostColumn::I32 {
                type_oid: INT4OID,
                values: vec![1, 1],
                nulls: None,
            },
        );
        assert!(
            prepare_dimension(
                &dimension(INT4OID, JoinMultiplicity::Unique),
                &columns,
                1,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn dimension_dictionary_code_overflow_is_not_a_join_miss() {
        assert_eq!(device_dimension_code(None).expect("missing key code"), -1);
        assert_eq!(
            device_dimension_code(Some(i32::MAX as usize)).expect("largest device code"),
            i32::MAX
        );
        assert_eq!(
            device_dimension_code(Some(i32::MAX as usize + 1)),
            Err("dimension dictionary exceeds i32 code space".to_owned())
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

    #[test]
    fn host_column_validation_and_value_accessors_fail_closed() {
        let invalid_bool = HostColumn::Bool {
            type_oid: BOOLOID,
            values: vec![0, 2],
            nulls: None,
        };
        assert_eq!(
            invalid_bool.validate_nulls(),
            Err("resident boolean column contains a noncanonical value".to_owned())
        );
        let invalid_null_shape = HostColumn::I32 {
            type_oid: INT4OID,
            values: vec![1, 2],
            nulls: Some(vec![0]),
        };
        assert_eq!(
            invalid_null_shape.validate_nulls(),
            Err("resident NULL bitmap has an invalid shape/value".to_owned())
        );
        let invalid_null_value = HostColumn::I32 {
            type_oid: INT4OID,
            values: vec![1],
            nulls: Some(vec![2]),
        };
        assert!(invalid_null_value.validate_nulls().is_err());

        let nullable = HostColumn::I64 {
            type_oid: INT8OID,
            values: vec![7, 8],
            nulls: Some(vec![1, 0]),
        };
        assert_eq!(nullable.dictionary_value(0), Ok(None));
        assert_eq!(
            nullable.dictionary_value(1),
            Ok(Some((CanonicalKey::I64(8), GroupDatum::I64(8))))
        );
        assert_eq!(nullable.join_key(0), Ok(None));
        assert_eq!(nullable.join_key(1), Ok(Some(CanonicalKey::I64(8))));
        assert_eq!(
            nullable.is_null(2),
            Err("resident row 2 is out of bounds".to_owned())
        );

        let bools = HostColumn::Bool {
            type_oid: BOOLOID,
            values: vec![1],
            nulls: None,
        };
        assert_eq!(
            bools.dictionary_value(0),
            Ok(Some((CanonicalKey::Bool(true), GroupDatum::Bool(true))))
        );
        assert_eq!(bools.filter_value(0), Ok(Some(ScalarValue::Bool(true))));
        assert!(bools.join_key(0).is_err());

        let int4 = HostColumn::I32 {
            type_oid: INT4OID,
            values: vec![-4],
            nulls: None,
        };
        assert_eq!(
            int4.dictionary_value(0),
            Ok(Some((CanonicalKey::I32(-4), GroupDatum::I32(-4))))
        );
        assert_eq!(int4.filter_value(0), Ok(Some(ScalarValue::I32(-4))));
        assert_eq!(int4.join_key(0), Ok(Some(CanonicalKey::I32(-4))));

        let float4 = HostColumn::F32 {
            type_oid: FLOAT4OID,
            values: vec![-0.0],
            nulls: None,
        };
        assert_eq!(
            float4.dictionary_value(0),
            Ok(Some((CanonicalKey::F32(0), GroupDatum::F32(-0.0))))
        );
        assert_eq!(float4.filter_value(0), Ok(Some(ScalarValue::F32(-0.0))));

        let float8 = HostColumn::F64 {
            type_oid: FLOAT8OID,
            values: vec![2.5],
            nulls: None,
        };
        assert_eq!(
            float8.dictionary_value(0),
            Ok(Some((
                CanonicalKey::F64(2.5_f64.to_bits()),
                GroupDatum::F64(2.5)
            )))
        );
        assert_eq!(float8.filter_value(0), Ok(Some(ScalarValue::F64(2.5))));

        let text = HostColumn::Text {
            type_oid: BPCHAROID,
            codes: vec![0],
            nulls: None,
            labels: vec!["EU  ".to_owned()].into(),
        };
        assert_eq!(
            text.dictionary_value(0),
            Ok(Some((
                CanonicalKey::Text("EU".to_owned()),
                GroupDatum::Text("EU  ".to_owned())
            )))
        );
        assert_eq!(
            text.join_key(0),
            Ok(Some(CanonicalKey::Text("EU".to_owned())))
        );
        assert_eq!(
            text.filter_value(0),
            Err("text columns are not scalar range filters".to_owned())
        );
        for codes in [vec![-1], vec![1]] {
            let invalid = HostColumn::Text {
                type_oid: TEXTOID,
                codes,
                nulls: None,
                labels: vec!["only".to_owned()].into(),
            };
            assert!(invalid.dictionary_value(0).is_err());
        }
    }

    #[test]
    fn host_column_lookup_and_scalar_comparison_cover_all_domains() {
        let columns = HostColumns {
            columns: BTreeMap::from([(
                (100, 1),
                HostColumn::I32 {
                    type_oid: INT4OID,
                    values: vec![1],
                    nulls: None,
                },
            )]),
            row_counts: BTreeMap::from([(100, 1)]),
        };
        assert_eq!(
            columns.get(column(100, INT4OID)).map(HostColumn::len),
            Ok(1)
        );
        assert!(columns.get(column(100, INT8OID)).is_err());
        assert!(columns.get(column(200, INT4OID)).is_err());
        let mut oversized_attno = column(100, INT4OID);
        oversized_attno.attno = i32::MAX;
        assert!(columns.get(oversized_attno).is_err());
        assert_eq!(columns.row_count(100), Ok(1));
        assert!(columns.row_count(200).is_err());

        for (left, right, expected) in [
            (
                ScalarValue::Bool(false),
                ScalarValue::Bool(true),
                std::cmp::Ordering::Less,
            ),
            (
                ScalarValue::I32(2),
                ScalarValue::I32(1),
                std::cmp::Ordering::Greater,
            ),
            (
                ScalarValue::I64(4),
                ScalarValue::I64(4),
                std::cmp::Ordering::Equal,
            ),
            (
                ScalarValue::Date(1),
                ScalarValue::Date(2),
                std::cmp::Ordering::Less,
            ),
            (
                ScalarValue::Timestamp(3),
                ScalarValue::Timestamp(2),
                std::cmp::Ordering::Greater,
            ),
            (
                ScalarValue::TimestampTz(3),
                ScalarValue::TimestampTz(3),
                std::cmp::Ordering::Equal,
            ),
            (
                ScalarValue::F32(f32::NAN),
                ScalarValue::F32(1.0),
                std::cmp::Ordering::Greater,
            ),
            (
                ScalarValue::F32(1.0),
                ScalarValue::F32(f32::NAN),
                std::cmp::Ordering::Less,
            ),
            (
                ScalarValue::F64(f64::NAN),
                ScalarValue::F64(f64::NAN),
                std::cmp::Ordering::Equal,
            ),
        ] {
            assert_eq!(scalar_cmp(left, right), Ok(expected));
        }
        assert_eq!(
            scalar_cmp(ScalarValue::I32(1), ScalarValue::I64(1)),
            Err("scalar filter type mismatch".to_owned())
        );
    }
}

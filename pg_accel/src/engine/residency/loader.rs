//! Synchronous MVCC table loader for residency v2.
//!
//! SPI performs an exact zero-row projection preflight so PostgreSQL enforces
//! relation and column privileges. The data path then scans the table through
//! its table AM under the active snapshot, avoiding per-row SPI conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pgrx::{pg_sys, prelude::*};

use crate::adapters::extractors::raster::parse_resident_raster;
use crate::engine::cost::device_limits;
use crate::engine::ffi::syscache;
use crate::engine::gucs;
use crate::gpu::ExprDeviceBuffer;

use super::domain::{
    RESIDENT_RASTER_BAND_HAS_NODATA, RESIDENT_RASTER_BAND_IS_NODATA, ResidentByteAccounting,
    ResidentGeometryData, ResidentRasterBand, ResidentRasterData, ResidentRasterRow,
    ResidentRepresentationEvidence, RetainedExactValues, text_dictionary_retained_host_bytes,
};
use super::geometry::{
    ResidentGeometryBuilder, ResidentGeometryColumn, ResidentGeometryReferencedBytes,
};
use super::ledger::{self, GenerationStamp, LedgerCharge};
use super::numeric::{NumericColumnBuilder, ResidentNumericColumn, ResidentNumericData};
use super::store::{
    InvalidationTriggerFingerprint, RelationFingerprint, ResidentColumn, ResidentRelation,
};

const LOAD_INTERRUPT_CHECK_ROWS: usize = super::nulls::STAGING_ROWS;
const REPRESENTATION_BLOCK_ROWS: usize = LOAD_INTERRUPT_CHECK_ROWS;

#[cfg(feature = "pg_test")]
#[derive(Clone, Copy)]
enum TestDirectScanError {
    Recoverable,
    QueryCanceled,
}

#[cfg(feature = "pg_test")]
std::thread_local! {
    static TEST_DIRECT_SCAN_ERROR_AFTER_ROWS: std::cell::Cell<Option<(u64, TestDirectScanError)>> = const { std::cell::Cell::new(None) };
    static TEST_DIRECT_SCAN_DROP_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Raw H3 Datum access is intentionally private. `ColumnBuilder::for_type`
/// proves the dynamic type before this wrapper is ever requested from SPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawH3Datum(u64);

impl FromDatum for RawH3Datum {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _type_oid: pg_sys::Oid,
    ) -> Option<Self> {
        (!is_null).then(|| Self(datum.value() as u64))
    }
}

/// Consume a catalog-proved geometry on the backend thread. The builder copies
/// directly from the live flat Datum into its retained storage, eliminating
/// the temporary per-row exact-byte Vec. Distinct detoast copies are released
/// on both successful staging and validation/allocation errors.
unsafe fn stage_geometry_datum(
    builder: &mut ResidentGeometryBuilder,
    datum: pg_sys::Datum,
    is_null: bool,
) -> Result<(), String> {
    if is_null {
        return builder.push_borrowed(None);
    }
    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
    // SAFETY: caller proved a live geometry varlena in the current table slot.
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    if detoasted.is_null() {
        return Err("could not detoast geometry".to_owned());
    }
    // SAFETY: pg_detoast_datum returned a complete flat, four-byte-header varlena.
    let length = unsafe { pgrx::varsize(detoasted.cast()) };
    // SAFETY: the flat allocation is readable for length bytes until pfree
    // below. push_borrowed retains copies, never a reference to this allocation.
    let bytes = unsafe { std::slice::from_raw_parts(detoasted.cast::<u8>(), length) };
    let result = builder.push_borrowed(Some(bytes));
    if detoasted != original {
        // SAFETY: this is our distinct palloc-owned detoast copy; all builder
        // references have expired, including when staging returned an error.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
    result
}

impl IntoDatum for RawH3Datum {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(pg_sys::Datum::from(self.0))
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::InvalidOid
    }

    fn is_compatible_with(_other: pg_sys::Oid) -> bool {
        true
    }
}

/// Borrowed PostGIS Raster Datum marker. The pointer is consumed
/// synchronously through the catalog-proved WKB exporter while its SPI row is
/// still live; no private PostGIS bytes are interpreted directly.
#[derive(Debug, Clone, Copy)]
struct RawRasterDatum(pg_sys::Datum);

impl FromDatum for RawRasterDatum {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _type_oid: pg_sys::Oid,
    ) -> Option<Self> {
        (!is_null && datum.value() != 0).then_some(Self(datum))
    }
}

impl IntoDatum for RawRasterDatum {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(self.0)
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::InvalidOid
    }

    fn is_compatible_with(_other: pg_sys::Oid) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ColumnRequest {
    pub attno: i16,
    pub type_oid: pg_sys::Oid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TriggerInstall {
    Existing,
    New,
}

enum StagedColumn {
    Numeric {
        type_oid: pg_sys::Oid,
        data: ResidentNumericData,
    },
    Empty {
        type_oid: pg_sys::Oid,
    },
    Bool {
        type_oid: pg_sys::Oid,
        values: Vec<u8>,
        nulls: Option<Vec<u8>>,
    },
    I32 {
        type_oid: pg_sys::Oid,
        values: Vec<i32>,
        nulls: Option<Vec<u8>>,
    },
    I64 {
        type_oid: pg_sys::Oid,
        values: Vec<i64>,
        nulls: Option<Vec<u8>>,
    },
    H3 {
        type_oid: pg_sys::Oid,
        values: Vec<u64>,
        nulls: Option<Vec<u8>>,
    },
    Geometry {
        type_oid: pg_sys::Oid,
        data: ResidentGeometryData,
        referenced_bytes: ResidentGeometryReferencedBytes,
        accounting: ResidentByteAccounting,
        max_exact_value_bytes: usize,
    },
    Raster {
        type_oid: pg_sys::Oid,
        data: ResidentRasterData,
        max_exact_value_bytes: usize,
    },
    F32 {
        type_oid: pg_sys::Oid,
        values: Vec<f32>,
        nulls: Option<Vec<u8>>,
    },
    F64 {
        type_oid: pg_sys::Oid,
        values: Vec<f64>,
        nulls: Option<Vec<u8>>,
    },
    TextDictionary {
        type_oid: pg_sys::Oid,
        codes: Vec<i32>,
        nulls: Option<Vec<u8>>,
        labels: Vec<String>,
    },
}

fn checked_lane_bytes(len: usize, width: usize, label: &str) -> Result<u64, String> {
    len.checked_mul(width)
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| format!("{label} byte count overflow"))
}

fn packed_bit_bytes(len: usize, bits_per_value: u32, label: &str) -> Result<u64, String> {
    if len == 0 || bits_per_value == 0 {
        return Ok(0);
    }
    let bits = u128::try_from(len)
        .ok()
        .and_then(|len| len.checked_mul(u128::from(bits_per_value)))
        .ok_or_else(|| format!("{label} bit count overflow"))?;
    let bytes = bits
        .checked_add(7)
        .map(|bits| bits / 8)
        .ok_or_else(|| format!("{label} byte count overflow"))?;
    u64::try_from(bytes).map_err(|_| format!("{label} byte count exceeds u64"))
}

fn bits_for_range(range: u128) -> u32 {
    u128::BITS - range.leading_zeros()
}

fn is_null_at(nulls: Option<&[u8]>, index: usize) -> bool {
    nulls.is_some_and(|nulls| nulls.get(index) == Some(&1))
}

fn projected_i32_bitpack_bytes(values: &[i32], nulls: Option<&[u8]>) -> Result<u64, String> {
    let mut bounds = None::<(i32, i32)>;
    for (index, value) in values.iter().copied().enumerate() {
        if is_null_at(nulls, index) {
            continue;
        }
        bounds = Some(bounds.map_or((value, value), |(min, max)| {
            (min.min(value), max.max(value))
        }));
    }
    let current = checked_lane_bytes(values.len(), std::mem::size_of::<i32>(), "int32 lane")?;
    let Some((min, max)) = bounds else {
        return Ok(0);
    };
    let range = u128::try_from(i64::from(max) - i64::from(min))
        .map_err(|_| "int32 range became negative".to_owned())?;
    let packed = packed_bit_bytes(values.len(), bits_for_range(range), "int32 bit-pack")?
        .checked_add(std::mem::size_of::<i32>() as u64)
        .ok_or_else(|| "int32 bit-pack metadata overflow".to_owned())?;
    Ok(current.min(packed))
}

fn projected_i64_bitpack_bytes(values: &[i64], nulls: Option<&[u8]>) -> Result<u64, String> {
    let mut bounds = None::<(i64, i64)>;
    for (index, value) in values.iter().copied().enumerate() {
        if is_null_at(nulls, index) {
            continue;
        }
        bounds = Some(bounds.map_or((value, value), |(min, max)| {
            (min.min(value), max.max(value))
        }));
    }
    let current = checked_lane_bytes(values.len(), std::mem::size_of::<i64>(), "int64 lane")?;
    let Some((min, max)) = bounds else {
        return Ok(0);
    };
    let range = u128::try_from(i128::from(max) - i128::from(min))
        .map_err(|_| "int64 range became negative".to_owned())?;
    let packed = packed_bit_bytes(values.len(), bits_for_range(range), "int64 bit-pack")?
        .checked_add(std::mem::size_of::<i64>() as u64)
        .ok_or_else(|| "int64 bit-pack metadata overflow".to_owned())?;
    Ok(current.min(packed))
}

fn projected_u64_bitpack_bytes(values: &[u64], nulls: Option<&[u8]>) -> Result<u64, String> {
    let mut bounds = None::<(u64, u64)>;
    for (index, value) in values.iter().copied().enumerate() {
        if is_null_at(nulls, index) {
            continue;
        }
        bounds = Some(bounds.map_or((value, value), |(min, max)| {
            (min.min(value), max.max(value))
        }));
    }
    let current = checked_lane_bytes(values.len(), std::mem::size_of::<u64>(), "uint64 lane")?;
    let Some((min, max)) = bounds else {
        return Ok(0);
    };
    let range = u128::from(max - min);
    let packed = packed_bit_bytes(values.len(), bits_for_range(range), "uint64 bit-pack")?
        .checked_add(std::mem::size_of::<u64>() as u64)
        .ok_or_else(|| "uint64 bit-pack metadata overflow".to_owned())?;
    Ok(current.min(packed))
}

fn projected_block_minmax_bytes(len: usize, width: usize) -> Result<u64, String> {
    if len == 0 {
        return Ok(0);
    }
    let blocks = len.div_ceil(REPRESENTATION_BLOCK_ROWS);
    // Two exact values plus one byte each for NULL and NaN/special-value state.
    checked_lane_bytes(
        blocks,
        width
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(2))
            .ok_or_else(|| "block min/max width overflow".to_owned())?,
        "block min/max metadata",
    )
}

fn checked_add_representation(
    total: &mut ResidentRepresentationEvidence,
    column: ResidentRepresentationEvidence,
) -> Result<(), String> {
    macro_rules! add {
        ($field:ident) => {
            total.$field = total.$field.checked_add(column.$field).ok_or_else(|| {
                concat!("resident representation ", stringify!($field), " overflow").to_owned()
            })?;
        };
    }
    add!(source_logical_bytes);
    add!(encoded_device_bytes);
    add!(retained_exact_bytes);
    add!(transient_staging_bytes);
    add!(construction_peak_bytes);
    add!(null_sidecar_bytes);
    add!(projected_null_bitmap_bytes);
    add!(bool_value_bytes);
    add!(projected_bool_bitmap_bytes);
    add!(integer_value_bytes);
    add!(projected_integer_bitpack_bytes);
    add!(dictionary_code_bytes);
    add!(projected_dictionary_bitpack_bytes);
    add!(projected_block_minmax_bytes);
    Ok(())
}

impl StagedColumn {
    fn device_bytes(&self) -> Result<u64, String> {
        let checked = |len: usize, width: usize, nulls: usize| {
            len.checked_mul(width)
                .and_then(|bytes| bytes.checked_add(nulls))
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(|| "resident column byte count overflow".to_owned())
        };
        match self {
            Self::Numeric { data, .. } => data
                .device_bytes()
                .ok_or_else(|| "resident NUMERIC byte count overflow".to_owned()),
            Self::Empty { .. } => Ok(0),
            Self::Bool { values, nulls, .. } => {
                checked(values.len(), 1, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::I32 { values, nulls, .. } => {
                checked(values.len(), 4, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::F32 { values, nulls, .. } => {
                checked(values.len(), 4, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::I64 { values, nulls, .. } => {
                checked(values.len(), 8, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::H3 { values, nulls, .. } => {
                checked(values.len(), 8, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::Geometry { accounting, .. } => Ok(accounting.device_bytes),
            Self::Raster {
                data,
                max_exact_value_bytes,
                ..
            } => data
                .accounting(*max_exact_value_bytes)
                .map(|accounting| accounting.device_bytes)
                .map_err(|error| format!("resident raster accounting failed: {error}")),
            Self::F64 { values, nulls, .. } => {
                checked(values.len(), 8, nulls.as_ref().map_or(0, Vec::len))
            }
            Self::TextDictionary { codes, nulls, .. } => {
                checked(codes.len(), 4, nulls.as_ref().map_or(0, Vec::len))
            }
        }
    }

    fn materialize(self, label: &str) -> Result<ResidentColumn, String> {
        match self {
            Self::Numeric { type_oid, data } => Ok(ResidentColumn::Numeric {
                type_oid,
                data: ResidentNumericColumn::materialize(&data)?,
            }),
            Self::Empty { type_oid } => Ok(ResidentColumn::Empty { type_oid }),
            Self::Bool {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::Bool {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::I32 {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::I32 {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::I64 {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::I64 {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::H3 {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::H3 {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::Geometry {
                type_oid,
                data,
                referenced_bytes,
                max_exact_value_bytes,
                ..
            } => Ok(ResidentColumn::Geometry {
                type_oid,
                data: ResidentGeometryColumn::materialize(
                    data,
                    referenced_bytes,
                    max_exact_value_bytes,
                    label,
                )?,
            }),
            Self::Raster {
                type_oid,
                data,
                max_exact_value_bytes,
            } => {
                data.validate(max_exact_value_bytes)
                    .map_err(|error| format!("resident raster validation failed: {error}"))?;
                let stats = data
                    .stats()
                    .map_err(|error| format!("resident raster statistics failed: {error}"))?;
                let ResidentRasterData {
                    pixels,
                    band_offsets,
                    rows,
                    bands,
                    nulls,
                    exact,
                } = data;
                Ok(ResidentColumn::Raster {
                    type_oid,
                    pixels: copy_optional_buffer(pixels, &format!("{label} raster pixels"))?,
                    band_offsets: copy_buffer(
                        &band_offsets,
                        &format!("{label} raster band offsets"),
                    )?,
                    rows: copy_buffer(&rows, &format!("{label} raster rows"))?,
                    bands: copy_optional_buffer(bands, &format!("{label} raster bands"))?,
                    nulls: copy_optional_nulls(nulls, &format!("{label} raster"))?,
                    exact,
                    stats,
                })
            }
            Self::F32 {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::F32 {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::F64 {
                type_oid,
                values,
                nulls,
            } => Ok(ResidentColumn::F64 {
                type_oid,
                values: copy_buffer(&values, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
            }),
            Self::TextDictionary {
                type_oid,
                codes,
                nulls,
                labels,
            } => Ok(ResidentColumn::TextDictionary {
                type_oid,
                codes: copy_buffer(&codes, label)?,
                nulls: copy_optional_nulls(nulls, label)?,
                labels,
            }),
        }
    }

    fn accounting(&self) -> Result<ResidentByteAccounting, String> {
        match self {
            Self::Geometry { accounting, .. } => Ok(*accounting),
            Self::Raster {
                data,
                max_exact_value_bytes,
                ..
            } => data
                .accounting(*max_exact_value_bytes)
                .map_err(|error| format!("resident raster accounting failed: {error}")),
            Self::TextDictionary { labels, .. } => Ok(ResidentByteAccounting {
                device_bytes: self.device_bytes()?,
                retained_host_exact_bytes: text_dictionary_retained_host_bytes(labels).ok_or_else(
                    || "text dictionary retained-host byte count overflow".to_owned(),
                )?,
            }),
            _ => Ok(ResidentByteAccounting {
                device_bytes: self.device_bytes()?,
                retained_host_exact_bytes: 0,
            }),
        }
    }

    fn representation_evidence(&self) -> Result<ResidentRepresentationEvidence, String> {
        let accounting = self.accounting()?;
        let transient_staging_bytes = accounting
            .checked_total()
            .map_err(|error| error.to_string())?;
        let construction_peak_bytes = transient_staging_bytes
            .checked_add(accounting.device_bytes)
            .ok_or_else(|| "resident construction peak byte count overflow".to_owned())?;
        let mut evidence = ResidentRepresentationEvidence {
            encoded_device_bytes: accounting.device_bytes,
            retained_exact_bytes: accounting.retained_host_exact_bytes,
            transient_staging_bytes,
            construction_peak_bytes,
            ..ResidentRepresentationEvidence::default()
        };
        let null_projection = |nulls: Option<&[u8]>| -> Result<(u64, u64), String> {
            let bytes = nulls.map_or(0, |nulls| nulls.len());
            Ok((
                u64::try_from(bytes).map_err(|_| "NULL sidecar bytes exceed u64".to_owned())?,
                packed_bit_bytes(bytes, 1, "NULL bitmap")?,
            ))
        };
        match self {
            Self::Numeric { data, .. } => {
                // Source logical size is the lossless send payload: eight
                // header bytes per non-NULL value and two bytes per digit.
                let nonnull = data.rows.len()
                    - data
                        .nulls
                        .as_ref()
                        .map_or(0, |nulls| nulls.iter().filter(|null| **null != 0).count());
                evidence.source_logical_bytes = checked_lane_bytes(nonnull, 8, "NUMERIC headers")?
                    .checked_add(checked_lane_bytes(data.digits.len(), 2, "NUMERIC digits")?)
                    .ok_or("NUMERIC source byte count overflow")?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(data.nulls.as_deref())?;
            }
            Self::Empty { .. } => {}
            Self::Bool { values, nulls, .. } => {
                evidence.source_logical_bytes = checked_lane_bytes(values.len(), 1, "bool source")?;
                evidence.bool_value_bytes = checked_lane_bytes(values.len(), 1, "bool values")?;
                evidence.projected_bool_bitmap_bytes =
                    packed_bit_bytes(values.len(), 1, "bool bitmap")?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::I32 {
                type_oid,
                values,
                nulls,
            } => {
                let source_width = usize::from(*type_oid != pg_sys::INT2OID) * 2 + 2;
                evidence.source_logical_bytes =
                    checked_lane_bytes(values.len(), source_width, "int32 source")?;
                evidence.integer_value_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<i32>(), "int32 values")?;
                evidence.projected_integer_bitpack_bytes =
                    projected_i32_bitpack_bytes(values, nulls.as_deref())?;
                evidence.projected_block_minmax_bytes =
                    projected_block_minmax_bytes(values.len(), std::mem::size_of::<i32>())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::I64 { values, nulls, .. } => {
                evidence.source_logical_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<i64>(), "int64 source")?;
                evidence.integer_value_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<i64>(), "int64 values")?;
                evidence.projected_integer_bitpack_bytes =
                    projected_i64_bitpack_bytes(values, nulls.as_deref())?;
                evidence.projected_block_minmax_bytes =
                    projected_block_minmax_bytes(values.len(), std::mem::size_of::<i64>())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::H3 { values, nulls, .. } => {
                evidence.source_logical_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<u64>(), "H3 source")?;
                evidence.integer_value_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<u64>(), "H3 values")?;
                evidence.projected_integer_bitpack_bytes =
                    projected_u64_bitpack_bytes(values, nulls.as_deref())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::Geometry { data, .. } => {
                evidence.source_logical_bytes = u64::try_from(data.exact.bytes.len())
                    .map_err(|_| "geometry exact source bytes exceed u64".to_owned())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(data.nulls.as_deref())?;
            }
            Self::Raster { data, .. } => {
                evidence.source_logical_bytes = u64::try_from(data.exact.bytes.len())
                    .map_err(|_| "raster exact source bytes exceed u64".to_owned())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(data.nulls.as_deref())?;
            }
            Self::F32 { values, nulls, .. } => {
                evidence.source_logical_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<f32>(), "float32 source")?;
                evidence.projected_block_minmax_bytes =
                    projected_block_minmax_bytes(values.len(), std::mem::size_of::<f32>())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::F64 { values, nulls, .. } => {
                evidence.source_logical_bytes =
                    checked_lane_bytes(values.len(), std::mem::size_of::<f64>(), "float64 source")?;
                evidence.projected_block_minmax_bytes =
                    projected_block_minmax_bytes(values.len(), std::mem::size_of::<f64>())?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
            Self::TextDictionary {
                codes,
                nulls,
                labels,
                ..
            } => {
                let mut source = 0_u64;
                for (index, code) in codes.iter().copied().enumerate() {
                    if is_null_at(nulls.as_deref(), index) {
                        continue;
                    }
                    let code = usize::try_from(code)
                        .map_err(|_| "text dictionary contains a negative code".to_owned())?;
                    let label = labels.get(code).ok_or_else(|| {
                        "text dictionary code exceeds label cardinality".to_owned()
                    })?;
                    source = source
                        .checked_add(4)
                        .and_then(|bytes| bytes.checked_add(u64::try_from(label.len()).ok()?))
                        .ok_or_else(|| "text source logical byte count overflow".to_owned())?;
                }
                evidence.source_logical_bytes = source;
                evidence.dictionary_code_bytes = checked_lane_bytes(
                    codes.len(),
                    std::mem::size_of::<i32>(),
                    "dictionary codes",
                )?;
                let cardinality_range = labels.len().saturating_sub(1) as u128;
                evidence.projected_dictionary_bitpack_bytes = packed_bit_bytes(
                    codes.len(),
                    bits_for_range(cardinality_range),
                    "dictionary bit-pack",
                )?;
                (
                    evidence.null_sidecar_bytes,
                    evidence.projected_null_bitmap_bytes,
                ) = null_projection(nulls.as_deref())?;
            }
        }
        Ok(evidence)
    }
}

fn copy_buffer<T>(values: &[T], label: &str) -> Result<ExprDeviceBuffer<T>, String> {
    ExprDeviceBuffer::copy_from_slice(values)
        .ok_or_else(|| format!("device allocation/copy failed for {label}"))
}

fn copy_optional_buffer<T>(
    values: Vec<T>,
    label: &str,
) -> Result<Option<ExprDeviceBuffer<T>>, String> {
    if values.is_empty() {
        Ok(None)
    } else {
        copy_buffer(&values, label).map(Some)
    }
}

fn copy_optional_nulls(
    nulls: Option<Vec<u8>>,
    label: &str,
) -> Result<Option<ExprDeviceBuffer<u8>>, String> {
    nulls
        .map(|values| copy_buffer(&values, &format!("{label} NULL sidecar")))
        .transpose()
}

pub(super) struct StagedRelation {
    relid: pg_sys::Oid,
    relfilenode: pg_sys::Oid,
    fingerprint: RelationFingerprint,
    generation: GenerationStamp,
    columns: BTreeMap<i16, StagedColumn>,
    row_count: u64,
    loaded_at_us: i64,
    load_ms: f64,
    representation: ResidentRepresentationEvidence,
}

impl StagedRelation {
    pub(super) const fn relid(&self) -> pg_sys::Oid {
        self.relid
    }

    pub(super) const fn row_count(&self) -> u64 {
        self.row_count
    }

    pub(super) fn accounting(&self) -> Result<ResidentByteAccounting, String> {
        let accounting = self.columns.values().try_fold(
            ResidentByteAccounting::default(),
            |mut total, column| {
                let column = column.accounting()?;
                total.device_bytes = total
                    .device_bytes
                    .checked_add(column.device_bytes)
                    .ok_or_else(|| "resident relation device byte count overflow".to_owned())?;
                total.retained_host_exact_bytes = total
                    .retained_host_exact_bytes
                    .checked_add(column.retained_host_exact_bytes)
                    .ok_or_else(|| {
                        "resident relation retained-host byte count overflow".to_owned()
                    })?;
                Ok::<_, String>(total)
            },
        )?;
        accounting
            .checked_total()
            .map_err(|error| error.to_string())?;
        Ok(accounting)
    }

    pub(super) fn materialize(
        self,
        charge: LedgerCharge,
        accounting: ResidentByteAccounting,
    ) -> Result<ResidentRelation, String> {
        let copy_started = Instant::now();
        let mut representation = self.representation;
        let mut columns = BTreeMap::new();
        for (attno, staged) in self.columns {
            let label = format!(
                "resident relation {} attribute {attno}",
                u32::from(self.relid)
            );
            columns.insert(attno, staged.materialize(&label)?);
        }
        let actual_accounting =
            columns
                .values()
                .try_fold(ResidentByteAccounting::default(), |mut total, column| {
                    let column = column
                        .accounting()
                        .ok_or_else(|| "resident column byte count overflow".to_owned())?;
                    total.device_bytes = total
                        .device_bytes
                        .checked_add(column.device_bytes)
                        .ok_or_else(|| "resident relation device byte count overflow".to_owned())?;
                    total.retained_host_exact_bytes = total
                        .retained_host_exact_bytes
                        .checked_add(column.retained_host_exact_bytes)
                        .ok_or_else(|| {
                            "resident relation retained-host byte count overflow".to_owned()
                        })?;
                    Ok::<_, String>(total)
                })?;
        if actual_accounting != accounting {
            return Err(format!(
                "resident relation accounting mismatch: staged={accounting:?}, materialized={actual_accounting:?}"
            ));
        }
        representation.copy_ms = copy_started.elapsed().as_secs_f64() * 1000.0;
        let load_ms = self.load_ms + representation.copy_ms;
        let now = now_us();
        Ok(ResidentRelation {
            relid: self.relid,
            relfilenode: self.relfilenode,
            fingerprint: self.fingerprint,
            generation: self.generation,
            columns,
            row_count: self.row_count,
            loaded_at_us: self.loaded_at_us,
            last_used_us: now,
            load_ms,
            representation,
            last_used_tick: 0,
            pinned: false,
            raw_charge: charge,
            raw_accounting: accounting,
            first_use_scope: None,
            relcache_suspect: false,
            derived: Vec::new(),
        })
    }
}

struct RasterColumnBuilder {
    type_oid: pg_sys::Oid,
    catalog: syscache::PostgisRasterCatalogIdentity,
    pixels: Vec<u8>,
    band_offsets: Vec<u64>,
    rows: Vec<ResidentRasterRow>,
    bands: Vec<ResidentRasterBand>,
    nulls: Vec<u8>,
    saw_null: bool,
    exact_offsets: Vec<u64>,
    exact_bytes: Vec<u8>,
    max_exact_value_bytes: usize,
}

impl RasterColumnBuilder {
    fn new(catalog: syscache::PostgisRasterCatalogIdentity, max_exact_value_bytes: usize) -> Self {
        Self {
            type_oid: catalog.raster_type_oid,
            catalog,
            pixels: Vec::new(),
            band_offsets: vec![0],
            rows: Vec::new(),
            bands: Vec::new(),
            nulls: Vec::new(),
            saw_null: false,
            exact_offsets: vec![0],
            exact_bytes: Vec::new(),
            max_exact_value_bytes,
        }
    }

    fn reserve_rows(&mut self, additional: usize) -> Result<(), String> {
        let reserve = |result: Result<(), std::collections::TryReserveError>| {
            result
                .map_err(|error| format!("resident raster host staging allocation failed: {error}"))
        };
        reserve(self.rows.try_reserve(additional))?;
        super::nulls::reserve(&mut self.nulls, self.saw_null, self.rows.len(), additional)?;
        reserve(self.exact_offsets.try_reserve(additional))
    }

    fn push_value(&mut self, value: Option<&[u8]>) -> Result<(), String> {
        self.reserve_rows(1)?;
        let Some(value) = value else {
            self.rows.push(ResidentRasterRow::default());
            self.nulls.push(1);
            self.saw_null = true;
            self.exact_offsets.push(
                *self
                    .exact_offsets
                    .last()
                    .ok_or("resident raster exact offsets lost their sentinel")?,
            );
            return Ok(());
        };
        if value.len() > self.max_exact_value_bytes {
            return Err(format!(
                "resident raster exact value is {} bytes, exceeding the per-value limit of {} bytes",
                value.len(),
                self.max_exact_value_bytes
            ));
        }
        let parsed = parse_resident_raster(value)
            .map_err(|error| format!("resident raster structural decline: {error}"))?;
        let first_band = u32::try_from(self.bands.len())
            .map_err(|_| "resident raster flattened band index exceeds u32".to_owned())?;
        let band_count = u32::try_from(parsed.bands.len())
            .map_err(|_| "resident raster band count exceeds u32".to_owned())?;
        let pixel_bytes = parsed.bands.iter().try_fold(0_usize, |total, band| {
            total
                .checked_add(band.pixels.len())
                .ok_or_else(|| "resident raster pixel byte count overflow".to_owned())
        })?;
        let exact_end = self
            .exact_bytes
            .len()
            .checked_add(value.len())
            .and_then(|end| u64::try_from(end).ok())
            .ok_or_else(|| "resident raster exact byte count overflow".to_owned())?;
        self.pixels
            .try_reserve(pixel_bytes)
            .map_err(|error| format!("resident raster pixel staging allocation failed: {error}"))?;
        self.bands
            .try_reserve(parsed.bands.len())
            .map_err(|error| format!("resident raster band staging allocation failed: {error}"))?;
        self.band_offsets
            .try_reserve(parsed.bands.len())
            .map_err(|error| {
                format!("resident raster offset staging allocation failed: {error}")
            })?;
        self.exact_bytes
            .try_reserve(value.len())
            .map_err(|error| format!("resident raster exact staging allocation failed: {error}"))?;

        self.rows.push(ResidentRasterRow {
            width: u32::from(parsed.header.width),
            height: u32::from(parsed.header.height),
            first_band,
            band_count,
            srid: parsed.header.srid,
            flags: 0,
            scale_x: parsed.header.scale_x,
            scale_y: parsed.header.scale_y,
            ip_x: parsed.header.ip_x,
            ip_y: parsed.header.ip_y,
            skew_x: parsed.header.skew_x,
            skew_y: parsed.header.skew_y,
        });
        for band in parsed.bands {
            let mut flags = 0;
            if band.has_nodata {
                flags |= RESIDENT_RASTER_BAND_HAS_NODATA;
            }
            if band.is_nodata {
                flags |= RESIDENT_RASTER_BAND_IS_NODATA;
            }
            self.bands.push(ResidentRasterBand {
                pixel_type: u32::from(band.pixel_type.code()),
                flags,
                nodata: band.nodata,
            });
            self.pixels.extend_from_slice(&band.pixels);
            self.band_offsets.push(
                u64::try_from(self.pixels.len())
                    .map_err(|_| "resident raster pixel byte count exceeds u64".to_owned())?,
            );
        }
        self.nulls.push(0);
        self.exact_bytes.extend_from_slice(value);
        self.exact_offsets.push(exact_end);
        Ok(())
    }

    fn finish(self) -> Result<StagedColumn, String> {
        // SAFETY: staging and publication run synchronously on the backend
        // main thread. A changed exporter/importer must invalidate the load.
        let current = unsafe { syscache::resolve_postgis_raster_catalog() }
            .map_err(|error| format!("resident raster catalog revalidation failed: {error}"))?;
        if current != self.catalog {
            return Err("PostGIS Raster catalog changed during resident staging".to_owned());
        }
        self.finish_after_catalog_proof()
    }

    fn finish_after_catalog_proof(self) -> Result<StagedColumn, String> {
        let data = ResidentRasterData {
            pixels: self.pixels,
            band_offsets: self.band_offsets,
            nulls: super::nulls::finish(self.nulls, self.saw_null, self.rows.len())?,
            rows: self.rows,
            bands: self.bands,
            exact: RetainedExactValues {
                offsets: self.exact_offsets.into_boxed_slice(),
                bytes: self.exact_bytes.into_boxed_slice(),
            },
        };
        data.validate(self.max_exact_value_bytes)
            .map_err(|error| format!("resident raster staging contract failed: {error}"))?;
        Ok(StagedColumn::Raster {
            type_oid: self.type_oid,
            data,
            max_exact_value_bytes: self.max_exact_value_bytes,
        })
    }

    #[cfg(test)]
    fn finish_for_test(self) -> Result<StagedColumn, String> {
        self.finish_after_catalog_proof()
    }
}

/// Intern text as it is scanned: retain one owned label per distinct value,
/// not one String per row. Finalization restores the established sorted-label
/// representation by remapping first-seen codes, leaving NULL payloads zero.
#[derive(Default)]
struct TextColumnBuilder {
    by_label: BTreeMap<String, i32>,
    codes: Vec<i32>,
    nulls: Vec<u8>,
    saw_null: bool,
}

fn next_text_dictionary_code(labels: usize) -> Result<i32, String> {
    if labels >= i32::MAX as usize {
        return Err("text dictionary exceeds int32 code space".to_owned());
    }
    i32::try_from(labels).map_err(|_| "text dictionary exceeds int32 code space".to_owned())
}

impl TextColumnBuilder {
    fn try_reserve(&mut self, additional: usize) -> Result<(), String> {
        self.codes
            .try_reserve(additional)
            .map_err(|error| format!("resident host staging allocation failed: {error}"))?;
        super::nulls::reserve(&mut self.nulls, self.saw_null, self.codes.len(), additional)
    }

    fn push(&mut self, value: Option<&str>) -> Result<(), String> {
        if let Some(label) = value {
            let code = if let Some(code) = self.by_label.get(label) {
                *code
            } else {
                let code = next_text_dictionary_code(self.by_label.len())?;
                self.by_label.insert(label.to_owned(), code);
                code
            };
            self.codes.push(code);
            self.nulls.push(0);
        } else {
            self.codes.push(0);
            self.nulls.push(1);
            self.saw_null = true;
        }
        Ok(())
    }

    fn finish(mut self, type_oid: pg_sys::Oid) -> Result<StagedColumn, String> {
        let nulls = super::nulls::finish(self.nulls, self.saw_null, self.codes.len())?;
        let mut remap = vec![0_i32; self.by_label.len()];
        let mut labels = Vec::with_capacity(self.by_label.len());
        for (sorted, (label, first_seen)) in self.by_label.into_iter().enumerate() {
            let first_seen = usize::try_from(first_seen)
                .map_err(|_| "text dictionary contains a negative code".to_owned())?;
            *remap
                .get_mut(first_seen)
                .ok_or("text dictionary code exceeds label cardinality")? =
                i32::try_from(sorted).map_err(|_| "text dictionary exceeds int32 code space")?;
            labels.push(label);
        }
        for (index, code) in self.codes.iter_mut().enumerate() {
            if nulls.as_ref().is_none_or(|mask| mask[index] == 0) {
                let first_seen = usize::try_from(*code)
                    .map_err(|_| "text dictionary contains a negative code".to_owned())?;
                *code = *remap
                    .get(first_seen)
                    .ok_or("text dictionary code exceeds label cardinality")?;
            }
        }
        Ok(StagedColumn::TextDictionary {
            type_oid,
            codes: self.codes,
            nulls,
            labels,
        })
    }
}

enum ColumnBuilder {
    Numeric {
        type_oid: pg_sys::Oid,
        builder: NumericColumnBuilder,
    },
    Bool {
        type_oid: pg_sys::Oid,
        values: Vec<u8>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    I32 {
        type_oid: pg_sys::Oid,
        values: Vec<i32>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    I64 {
        type_oid: pg_sys::Oid,
        values: Vec<i64>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    H3 {
        type_oid: pg_sys::Oid,
        values: Vec<u64>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    Geometry {
        type_oid: pg_sys::Oid,
        builder: ResidentGeometryBuilder,
    },
    Raster(RasterColumnBuilder),
    F32 {
        type_oid: pg_sys::Oid,
        values: Vec<f32>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    F64 {
        type_oid: pg_sys::Oid,
        values: Vec<f64>,
        nulls: Vec<u8>,
        saw_null: bool,
    },
    Text {
        type_oid: pg_sys::Oid,
        builder: TextColumnBuilder,
    },
}

fn validate_h3_column_type(type_oid: pg_sys::Oid) -> Result<(), String> {
    // SAFETY: residency resolution and load run on the PostgreSQL backend main
    // thread. Resolving the full identity also revalidates the parent function.
    let catalog = unsafe { syscache::resolve_h3_catalog() }.map_err(|detail| {
        format!(
            "type OID {} is not supported by residency v2; exact extension-owned h3index validation failed: {detail}",
            u32::from(type_oid)
        )
    })?;
    if catalog.type_oid != type_oid {
        return Err(format!(
            "type OID {} is not supported by residency v2; the validated h3index type is OID {}",
            u32::from(type_oid),
            u32::from(catalog.type_oid)
        ));
    }
    Ok(())
}

fn validate_geometry_column_type(type_oid: pg_sys::Oid) -> Result<(), String> {
    // SAFETY: residency resolution runs synchronously on the backend main
    // thread before interpreting any extension-owned varlena payload.
    let catalog = unsafe { syscache::resolve_postgis_catalog() }?;
    if type_oid != catalog.geometry_type_oid {
        return Err(format!(
            "type OID {} is not the catalog-proved PostGIS geometry type",
            u32::from(type_oid)
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExtensionColumnKind {
    H3,
    Geometry,
    Raster(syscache::PostgisRasterCatalogIdentity),
}

fn classify_extension_column_type(type_oid: pg_sys::Oid) -> Result<ExtensionColumnKind, String> {
    // SAFETY: extension membership and exact catalog proofs are inspected on
    // the PostgreSQL backend main thread before selecting a Datum reader.
    if unsafe { syscache::type_is_extension_member(type_oid, "postgis") } {
        validate_geometry_column_type(type_oid)?;
        return Ok(ExtensionColumnKind::Geometry);
    }

    // SAFETY: residency resolution and load run on the backend main thread.
    match unsafe { syscache::resolve_postgis_raster_catalog() } {
        Ok(catalog) if catalog.raster_type_oid == type_oid => {
            Ok(ExtensionColumnKind::Raster(catalog))
        }
        result => {
            let raster_detail = match result {
                Ok(catalog) => format!(
                    "the catalog-proved raster type is OID {}",
                    u32::from(catalog.raster_type_oid)
                ),
                Err(error) => error,
            };
            match validate_h3_column_type(type_oid) {
                Ok(()) => Ok(ExtensionColumnKind::H3),
                Err(h3_detail) => Err(format!(
                    "type OID {} is not supported by residency v2; exact PostGIS Raster validation failed: {raster_detail}; exact H3 validation failed: {h3_detail}",
                    u32::from(type_oid)
                )),
            }
        }
    }
}

/// Whether every requested column has fixed-width resident load work.
///
/// An empty request is the zero-column `COUNT(*)` case: it still scans rows,
/// but it does not decode or materialize any variable-width values.
pub(super) fn columns_have_fixed_width_load(requests: &[ColumnRequest]) -> Result<bool, String> {
    requests.iter().try_fold(true, |all_fixed, request| {
        let fixed = match request.type_oid {
            pg_sys::BOOLOID
            | pg_sys::INT2OID
            | pg_sys::INT4OID
            | pg_sys::INT8OID
            | pg_sys::DATEOID
            | pg_sys::TIMESTAMPOID
            | pg_sys::TIMESTAMPTZOID
            | pg_sys::FLOAT4OID
            | pg_sys::FLOAT8OID => true,
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NUMERICOID => false,
            _ => matches!(
                classify_extension_column_type(request.type_oid)?,
                ExtensionColumnKind::H3
            ),
        };
        Ok(all_fixed && fixed)
    })
}

impl ColumnBuilder {
    fn type_oid(&self) -> pg_sys::Oid {
        match self {
            Self::Raster(builder) => builder.type_oid,
            Self::Numeric { type_oid, .. }
            | Self::Bool { type_oid, .. }
            | Self::I32 { type_oid, .. }
            | Self::I64 { type_oid, .. }
            | Self::H3 { type_oid, .. }
            | Self::Geometry { type_oid, .. }
            | Self::F32 { type_oid, .. }
            | Self::F64 { type_oid, .. }
            | Self::Text { type_oid, .. } => *type_oid,
        }
    }

    fn finish_empty(self) -> Result<StagedColumn, String> {
        let type_oid = self.type_oid();
        if matches!(&self, Self::Geometry { .. }) {
            self.finish()
        } else {
            Ok(StagedColumn::Empty { type_oid })
        }
    }

    fn for_type(type_oid: pg_sys::Oid) -> Result<Self, String> {
        match type_oid {
            pg_sys::NUMERICOID => Ok(Self::Numeric {
                type_oid,
                builder: NumericColumnBuilder::default(),
            }),
            pg_sys::BOOLOID => Ok(Self::Bool {
                type_oid,
                values: Vec::new(),
                nulls: Vec::new(),
                saw_null: false,
            }),
            pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::DATEOID => Ok(Self::I32 {
                type_oid,
                values: Vec::new(),
                nulls: Vec::new(),
                saw_null: false,
            }),
            pg_sys::INT8OID | pg_sys::TIMESTAMPOID | pg_sys::TIMESTAMPTZOID => Ok(Self::I64 {
                type_oid,
                values: Vec::new(),
                nulls: Vec::new(),
                saw_null: false,
            }),
            pg_sys::FLOAT4OID => Ok(Self::F32 {
                type_oid,
                values: Vec::new(),
                nulls: Vec::new(),
                saw_null: false,
            }),
            pg_sys::FLOAT8OID => Ok(Self::F64 {
                type_oid,
                values: Vec::new(),
                nulls: Vec::new(),
                saw_null: false,
            }),
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID => Ok(Self::Text {
                type_oid,
                builder: TextColumnBuilder::default(),
            }),
            _ => match classify_extension_column_type(type_oid)? {
                ExtensionColumnKind::H3 => Ok(Self::H3 {
                    type_oid,
                    values: Vec::new(),
                    nulls: Vec::new(),
                    saw_null: false,
                }),
                ExtensionColumnKind::Geometry => {
                    let limits = device_limits();
                    Ok(Self::Geometry {
                        type_oid,
                        builder: ResidentGeometryBuilder::new(
                            limits.resident_domain_max_exact_value_bytes,
                            limits.gpu_spatial_max_vertices_per_row,
                        ),
                    })
                }
                ExtensionColumnKind::Raster(catalog) => Ok(Self::Raster(RasterColumnBuilder::new(
                    catalog,
                    device_limits().resident_domain_max_exact_value_bytes,
                ))),
            },
        }
    }

    fn try_reserve(&mut self, additional: usize) -> Result<(), String> {
        let reserve = |result: Result<(), std::collections::TryReserveError>| {
            result.map_err(|error| format!("resident host staging allocation failed: {error}"))
        };
        match self {
            Self::Numeric { builder, .. } => builder.try_reserve_rows(additional),
            Self::Bool {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::I32 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::I64 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::H3 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::Geometry { builder, .. } => builder.try_reserve_rows(additional),
            Self::Raster(builder) => builder.reserve_rows(additional),
            Self::F32 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::F64 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                reserve(values.try_reserve(additional))?;
                super::nulls::reserve(nulls, *saw_null, values.len(), additional)
            }
            Self::Text { builder, .. } => builder.try_reserve(additional),
        }
    }

    unsafe fn push_datum(
        &mut self,
        datum: pg_sys::Datum,
        is_null: bool,
        attno: i16,
    ) -> Result<(), String> {
        macro_rules! decode_datum {
            ($type:ty, $type_oid:expr) => {{
                // SAFETY: push_datum's caller proves that the slot Datum is
                // live and has the builder's freshly validated dynamic type.
                unsafe { <$type>::from_polymorphic_datum(datum, is_null, $type_oid) }
            }};
        }
        match self {
            Self::Numeric { builder, .. } => {
                if is_null {
                    builder.push(None)?;
                } else {
                    // SAFETY: ColumnBuilder::for_type accepts only the exact
                    // built-in NUMERIC OID for this branch. The slot Datum is
                    // live on the backend thread. numeric_send returns a fresh
                    // flat bytea; no private Numeric varlena layout is read.
                    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
                    // SAFETY: original is a live NUMERIC varlena. Own any
                    // detoast copy explicitly: numeric_send itself does not
                    // release a PG_GETARG_NUMERIC copy on this scan path.
                    let flat = unsafe { pg_sys::pg_detoast_datum(original) };
                    if flat.is_null() {
                        return Err("could not detoast NUMERIC".to_owned());
                    }
                    // SAFETY: built-in numeric_send accepts this flat NUMERIC
                    // Datum. Direct dispatch avoids a catalog lookup per row.
                    let wire = unsafe {
                        pgrx::direct_function_call_as_datum(
                            pg_sys::numeric_send,
                            &[Some(pg_sys::Datum::from(flat))],
                        )
                    };
                    if flat != original {
                        // SAFETY: flat is a distinct palloc-owned detoast copy;
                        // numeric_send has copied its complete digit payload.
                        unsafe { pg_sys::pfree(flat.cast()) };
                    }
                    let wire = wire.map_or(
                        std::ptr::null_mut(),
                        pg_sys::Datum::cast_mut_ptr::<pg_sys::varlena>,
                    );
                    if wire.is_null() {
                        return Err("numeric_send returned NULL".to_owned());
                    }
                    // SAFETY: numeric_send returned the complete flat bytea.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            pgrx::vardata_any(wire).cast::<u8>(),
                            pgrx::varsize_any_exhdr(wire),
                        )
                    };
                    let result = builder.push(Some(bytes));
                    // SAFETY: wire is the fresh numeric_send result and push
                    // has copied its digits; no reference escapes this call.
                    unsafe { pg_sys::pfree(wire.cast()) };
                    result?;
                }
            }
            Self::Bool {
                values,
                nulls,
                saw_null,
                ..
            } => {
                // SAFETY: the builder's catalog-proved type is bool and the
                // Datum remains live in the current table slot.
                let value = decode_datum!(bool, pg_sys::BOOLOID);
                values.push(u8::from(value.unwrap_or(false)));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::I32 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => {
                let value = match *type_oid {
                    pg_sys::INT2OID => decode_datum!(i16, *type_oid).map(i32::from),
                    pg_sys::DATEOID => decode_datum!(Date, *type_oid).map(pg_sys::DateADT::from),
                    _ => decode_datum!(i32, *type_oid),
                };
                values.push(value.unwrap_or(0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::I64 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => {
                let value = match *type_oid {
                    pg_sys::TIMESTAMPOID => {
                        decode_datum!(Timestamp, *type_oid).map(pg_sys::Timestamp::from)
                    }
                    pg_sys::TIMESTAMPTZOID => decode_datum!(TimestampWithTimeZone, *type_oid)
                        .map(pg_sys::TimestampTz::from),
                    _ => decode_datum!(i64, *type_oid),
                };
                values.push(value.unwrap_or(0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::H3 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => {
                let value = decode_datum!(RawH3Datum, *type_oid);
                values.push(value.map_or(0, |value| value.0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::Geometry { builder, .. } => {
                // SAFETY: the builder's dynamic geometry type was catalog-
                // validated; this table-slot Datum is live on the backend thread.
                unsafe { stage_geometry_datum(builder, datum, is_null) }?;
            }
            Self::Raster(builder) => {
                let datum = decode_datum!(RawRasterDatum, builder.type_oid);
                let wkb = datum
                    .map(|datum| unsafe {
                        // SAFETY: RawRasterDatum only wraps the live non-null
                        // SPI value of the catalog-proved raster type; `catalog`
                        // is the freshly resolved identity for its WKB exporter.
                        syscache::postgis_raster_datum_to_wkb(&builder.catalog, datum.0)
                    })
                    .transpose()
                    .map_err(|error| {
                        format!("column {attno} raster WKB conversion failed: {error}")
                    })?;
                builder.push_value(wkb.as_deref())?;
            }
            Self::F32 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                let value = decode_datum!(f32, pg_sys::FLOAT4OID);
                values.push(value.unwrap_or(0.0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::F64 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                let value = decode_datum!(f64, pg_sys::FLOAT8OID);
                values.push(value.unwrap_or(0.0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::Text { type_oid, builder } => {
                if is_null || datum.is_null() {
                    builder.push(None)?;
                } else {
                    // SAFETY: this is a live text-family varlena from the
                    // validated slot. Inline values can be borrowed only for
                    // this call; push owns every newly interned label. Keep
                    // pgrx's owning decoder for external/compressed values so
                    // its detoast allocation is freed exactly as before.
                    let toasted = unsafe {
                        pgrx::varlena::varatt_is_1b_e(datum.cast_mut_ptr())
                            || pgrx::varlena::varatt_is_4b_c(datum.cast_mut_ptr())
                    };
                    if toasted {
                        let value = decode_datum!(String, *type_oid);
                        builder.push(value.as_deref())?;
                    } else {
                        let value = decode_datum!(&str, *type_oid);
                        builder.push(value)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<StagedColumn, String> {
        Ok(match self {
            Self::Numeric { type_oid, builder } => StagedColumn::Numeric {
                type_oid,
                data: builder.finish()?,
            },
            Self::Bool {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::Bool {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::I32 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::I32 {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::I64 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::I64 {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::H3 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::H3 {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::Geometry { type_oid, builder } => {
                let max_exact_value_bytes =
                    crate::engine::cost::device_limits().resident_domain_max_exact_value_bytes;
                let data = builder.finish()?;
                let referenced_bytes = ResidentGeometryReferencedBytes::build(&data)?;
                let mut accounting = data
                    .accounting(max_exact_value_bytes)
                    .map_err(|error| error.to_string())?;
                accounting.retained_host_exact_bytes = accounting
                    .retained_host_exact_bytes
                    .checked_add(referenced_bytes.accounting_bytes()?)
                    .ok_or_else(|| "geometry retained-host accounting overflow".to_owned())?;
                StagedColumn::Geometry {
                    type_oid,
                    data,
                    referenced_bytes,
                    accounting,
                    max_exact_value_bytes,
                }
            }
            Self::Raster(builder) => return builder.finish(),
            Self::F32 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::F32 {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::F64 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::F64 {
                type_oid,
                nulls: super::nulls::finish(nulls, saw_null, values.len())?,
                values,
            },
            Self::Text { type_oid, builder } => return builder.finish(type_oid),
        })
    }
}

fn qualified_relation_name(relid: pg_sys::Oid) -> Result<String, String> {
    let rls_query = format!(
        "SELECT relrowsecurity OR relforcerowsecurity FROM pg_catalog.pg_class WHERE oid = {}::oid",
        u32::from(relid)
    );
    if Spi::get_one::<bool>(&rls_query)
        .map_err(|error| {
            format!(
                "failed to inspect relation OID {} RLS state: {error:?}",
                u32::from(relid)
            )
        })?
        .unwrap_or(false)
    {
        return Err(format!(
            "relation OID {} has row-level security enabled; residency is keyed by relation/column identity and cannot safely replay role- or policy-dependent row subsets",
            u32::from(relid)
        ));
    }
    let query = format!(
        "SELECT pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(c.relname) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid = {}::oid AND c.relkind = 'r'",
        u32::from(relid)
    );
    Spi::get_one::<String>(&query)
        .map_err(|error| format!("failed to resolve relation OID {}: {error:?}", u32::from(relid)))?
        .ok_or_else(|| format!("relation OID {} is missing or is not an ordinary table; partitioned tables require per-partition residency and are not yet accepted", u32::from(relid)))
}

fn relation_columns(relid: pg_sys::Oid) -> Result<Vec<(i16, String, pg_sys::Oid)>, String> {
    let query = format!(
        "SELECT attnum::int4, attname::text, atttypid::oid \
         FROM pg_catalog.pg_attribute WHERE attrelid = {}::oid AND attnum > 0 AND NOT attisdropped \
         ORDER BY attnum",
        u32::from(relid)
    );
    Spi::connect(|client| {
        let rows = client.select(&query, None, &[]).map_err(|error| {
            format!(
                "failed to inspect relation OID {} columns: {error:?}",
                u32::from(relid)
            )
        })?;
        let mut columns = Vec::new();
        for row in rows {
            let attno = row
                .get::<i32>(1)
                .map_err(|error| format!("attnum read failed: {error:?}"))?
                .ok_or("attnum is NULL")?;
            let name = row
                .get::<String>(2)
                .map_err(|error| format!("attname read failed: {error:?}"))?
                .ok_or("attname is NULL")?;
            let type_oid = row
                .get::<pg_sys::Oid>(3)
                .map_err(|error| format!("atttypid read failed: {error:?}"))?
                .ok_or("atttypid is NULL")?;
            columns.push((
                i16::try_from(attno).map_err(|_| format!("attnum {attno} exceeds int16"))?,
                name,
                type_oid,
            ));
        }
        Ok::<_, String>(columns)
    })
}

pub(super) fn resolve_attnos(
    relid: pg_sys::Oid,
    attnos: &[i16],
) -> Result<Vec<ColumnRequest>, String> {
    if attnos.is_empty() {
        // COUNT(*) still needs a relation-validated MVCC scan and generation
        // stamp, but it intentionally owns no raw device columns.
        qualified_relation_name(relid)?;
        return Ok(Vec::new());
    }
    let wanted_attnos = attnos.iter().copied().collect::<BTreeSet<_>>();
    let mut resolved = Vec::new();
    for (attno, _name, type_oid) in relation_columns(relid)? {
        if !wanted_attnos.contains(&attno) {
            continue;
        }
        let request = ColumnRequest { attno, type_oid };
        ColumnBuilder::for_type(type_oid)?;
        resolved.push(request);
    }
    if resolved.len() != attnos.len() {
        return Err(format!(
            "relation OID {} resolved {} of {} requested attributes",
            u32::from(relid),
            resolved.len(),
            attnos.len()
        ));
    }
    Ok(resolved)
}

pub(super) fn resolve_column_names(
    relid: pg_sys::Oid,
    names: Option<&[String]>,
) -> Result<Vec<ColumnRequest>, String> {
    let wanted = names.map(|items| items.iter().cloned().collect::<BTreeSet<_>>());
    if names.is_some_and(|items| items.is_empty()) {
        return Err(
            "pg_accel_pin columns must be NULL (all supported columns) or a non-empty array"
                .to_owned(),
        );
    }
    let mut resolved = Vec::new();
    let mut found = BTreeSet::new();
    for (attno, name, type_oid) in relation_columns(relid)? {
        if wanted.as_ref().is_none_or(|wanted| wanted.contains(&name)) {
            if let Err(detail) = ColumnBuilder::for_type(type_oid) {
                if wanted.is_some() {
                    return Err(format!(
                        "cannot pin {}.{}: {detail}",
                        u32::from(relid),
                        name
                    ));
                }
                continue;
            }
            found.insert(name);
            resolved.push(ColumnRequest { attno, type_oid });
        }
    }
    if let Some(wanted) = wanted {
        let missing = wanted.difference(&found).cloned().collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "relation OID {} has no live columns: {}",
                u32::from(relid),
                missing.join(", ")
            ));
        }
    }
    if resolved.is_empty() {
        return Err(format!(
            "relation OID {} has no supported columns to pin",
            u32::from(relid)
        ));
    }
    Ok(resolved)
}

pub(super) fn estimate_resident_bytes(
    relid: pg_sys::Oid,
    requests: &[ColumnRequest],
) -> Result<u64, String> {
    if requests.is_empty() {
        return Ok(0);
    }
    let row_query = format!(
        "SELECT GREATEST(reltuples, 0)::float8 FROM pg_catalog.pg_class WHERE oid = {}::oid",
        u32::from(relid)
    );
    let rows = Spi::get_one::<f64>(&row_query)
        .map_err(|error| {
            format!(
                "failed to estimate relation OID {} rows: {error:?}",
                u32::from(relid)
            )
        })?
        .ok_or_else(|| {
            format!(
                "relation OID {} disappeared during load estimate",
                u32::from(relid)
            )
        })?;
    if !rows.is_finite() || rows < 0.0 || rows > u64::MAX as f64 {
        return Err(format!(
            "relation OID {} has invalid reltuples estimate {rows}",
            u32::from(relid)
        ));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rows = rows.ceil() as u64;
    let attnos = requests
        .iter()
        .map(|request| request.attno.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let nullability_query = format!(
        "SELECT attnum::int4, attnotnull FROM pg_catalog.pg_attribute \
         WHERE attrelid = {}::oid AND attnum IN ({attnos})",
        u32::from(relid)
    );
    let not_null = Spi::connect(|client| {
        let nullability_rows = client
            .select(&nullability_query, None, &[])
            .map_err(|error| {
                format!(
                    "failed to estimate relation OID {} null sidecars: {error:?}",
                    u32::from(relid)
                )
            })?;
        let mut not_null = BTreeMap::new();
        for row in nullability_rows {
            let attno = row
                .get::<i32>(1)
                .map_err(|error| format!("attnum estimate read failed: {error:?}"))?
                .ok_or("attnum estimate is NULL")?;
            let required = row
                .get::<bool>(2)
                .map_err(|error| format!("attnotnull estimate read failed: {error:?}"))?
                .ok_or("attnotnull estimate is NULL")?;
            not_null.insert(
                i16::try_from(attno).map_err(|_| format!("attnum {attno} exceeds int16"))?,
                required,
            );
        }
        Ok::<_, String>(not_null)
    })?;
    requests.iter().try_fold(0_u64, |total, request| {
        let width = match request.type_oid {
            pg_sys::NUMERICOID => {
                // A width estimate only: staging computes the exact digit
                // arena and 16-byte row-header charge before publication.
                // SAFETY: validated attribute identity, backend main thread.
                let average = unsafe { pg_sys::get_attavgwidth(relid, request.attno) };
                u64::try_from(average.max(8)).map_err(|_| "NUMERIC width exceeds u64")? + 16
            }
            pg_sys::BOOLOID => 1_u64,
            pg_sys::INT2OID
            | pg_sys::INT4OID
            | pg_sys::DATEOID
            | pg_sys::FLOAT4OID
            | pg_sys::TEXTOID
            | pg_sys::VARCHAROID
            | pg_sys::BPCHAROID => 4,
            pg_sys::INT8OID | pg_sys::TIMESTAMPOID | pg_sys::TIMESTAMPTZOID | pg_sys::FLOAT8OID => {
                8
            }
            _ => match classify_extension_column_type(request.type_oid)? {
                ExtensionColumnKind::H3 => 8,
                ExtensionColumnKind::Geometry => {
                    // Geometry retains the exact varlena and publishes fp64
                    // coordinates plus fixed row metadata. PostgreSQL's
                    // analyzed average width is the best available pre-scan
                    // estimate; the exact post-scan accounting remains the
                    // authoritative reservation.
                    // SAFETY: relation/attribute identities were read from the
                    // active catalog on the backend main thread.
                    let average_width = unsafe { pg_sys::get_attavgwidth(relid, request.attno) };
                    let average_width = u64::try_from(average_width.max(32))
                        .map_err(|_| "geometry average width exceeds u64".to_owned())?;
                    average_width
                        .checked_mul(2)
                        .and_then(|bytes| bytes.checked_add(72))
                        .ok_or_else(|| "resident geometry width estimate overflow".to_owned())?
                }
                ExtensionColumnKind::Raster(_) => {
                    // The exact charge is computed after staging. This planner
                    // estimate uses catalog width for both retained WKB and
                    // native pixels, plus fixed row/offset/band overhead.
                    // SAFETY: estimate resolution runs on the backend thread.
                    let average = unsafe { pg_sys::get_attavgwidth(relid, request.attno) };
                    let average = u64::try_from(average.max(1))
                        .map_err(|_| "raster average width exceeds u64".to_owned())?;
                    average
                        .checked_mul(2)
                        .and_then(|bytes| bytes.checked_add(104))
                        .ok_or_else(|| "resident raster byte estimate overflow".to_owned())?
                }
            },
        };
        let width = width + u64::from(!not_null.get(&request.attno).copied().unwrap_or(false));
        let column_bytes = rows
            .checked_mul(width)
            .ok_or("resident byte estimate overflow")?;
        total
            .checked_add(column_bytes)
            .ok_or_else(|| "resident byte estimate overflow".to_owned())
    })
}

#[cfg_attr(all(test, not(feature = "pg_test")), allow(dead_code))]
pub(super) fn current_relfilenode(relid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: called on the backend main thread in planner/executor/SRF context.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::RELOID as ::core::ffi::c_int,
            relid.into(),
        )
    };
    if tuple.is_null() {
        return None;
    }
    // SAFETY: tuple is a pinned pg_class tuple until ReleaseSysCache below.
    let relfilenode = unsafe {
        let form = pg_sys::GETSTRUCT(tuple).cast::<pg_sys::FormData_pg_class>();
        (*form).relfilenode
    };
    // SAFETY: releases the SearchSysCache pin above.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    Some(relfilenode)
}

struct DirectTableScan {
    relation: pg_sys::Relation,
    scan: pg_sys::TableScanDesc,
    slot: *mut pg_sys::TupleTableSlot,
}

fn select_privilege_granted(
    relation_select: bool,
    requests: &[ColumnRequest],
    mut column_select: impl FnMut(Option<i16>) -> bool,
) -> bool {
    if relation_select {
        return true;
    }
    if requests.is_empty() {
        // PostgreSQL permits a projection without Vars, such as COUNT(*),
        // when the role has SELECT on any live user column.
        return column_select(None);
    }
    requests
        .iter()
        .all(|request| column_select(Some(request.attno)))
}

impl DirectTableScan {
    fn open_relation(relid: pg_sys::Oid, requests: &[ColumnRequest]) -> Result<Self, String> {
        // SAFETY: residency loading runs on the PostgreSQL backend main thread.
        // The lock protects the relation identity through preflight and scan.
        let relation =
            unsafe { pg_sys::try_table_open(relid, pg_sys::AccessShareLock as pg_sys::LOCKMODE) };
        if relation.is_null() {
            return Err(format!(
                "relation OID {} disappeared before resident table scan",
                u32::from(relid)
            ));
        }
        let table = Self {
            relation,
            scan: std::ptr::null_mut(),
            slot: std::ptr::null_mut(),
        };
        table.validate_locked_relation(requests)?;
        table.validate_select_privileges(requests)?;
        Ok(table)
    }

    fn validate_locked_relation(&self, requests: &[ColumnRequest]) -> Result<(), String> {
        // SAFETY: relation is open under AccessShareLock for the guard's life.
        let relation = unsafe { &*self.relation };
        if relation.rd_rel.is_null() {
            return Err("resident table scan relation has no pg_class descriptor".to_owned());
        }
        // SAFETY: rd_rel is relation-owned and protected by AccessShareLock.
        let class = unsafe { &*relation.rd_rel };
        if class.relrowsecurity || class.relforcerowsecurity {
            return Err(format!(
                "relation OID {} has row-level security enabled; residency cannot bypass role- or policy-dependent row subsets",
                u32::from(relation.rd_id)
            ));
        }
        if class.relkind as u8 != pg_sys::RELKIND_RELATION {
            return Err(format!(
                "relation OID {} is no longer an ordinary table",
                u32::from(relation.rd_id)
            ));
        }
        if relation.rd_tableam.is_null() {
            return Err(format!(
                "relation OID {} has no table access method",
                u32::from(relation.rd_id)
            ));
        }
        let tuple_desc = relation.rd_att;
        if tuple_desc.is_null() {
            return Err(format!(
                "relation OID {} has no tuple descriptor",
                u32::from(relation.rd_id)
            ));
        }
        // SAFETY: tuple_desc is live and relation-owned under the lock.
        let attribute_count = usize::try_from(unsafe { (*tuple_desc).natts })
            .map_err(|_| "resident tuple descriptor has a negative attribute count")?;
        for request in requests {
            let index = usize::try_from(request.attno.checked_sub(1).ok_or_else(|| {
                format!("resident request has invalid attribute {}", request.attno)
            })?)
            .map_err(|_| format!("resident request has invalid attribute {}", request.attno))?;
            if index >= attribute_count {
                return Err(format!(
                    "relation OID {} no longer has attribute {}",
                    u32::from(relation.rd_id),
                    request.attno
                ));
            }
            // SAFETY: index is in bounds for this live tuple descriptor.
            let attribute = unsafe { crate::engine::pg_compat::tuple_desc_attr(tuple_desc, index) };
            if attribute.is_null() {
                return Err(format!(
                    "relation OID {} attribute {} has no descriptor",
                    u32::from(relation.rd_id),
                    request.attno
                ));
            }
            // SAFETY: attribute belongs to the locked tuple descriptor.
            let attribute = unsafe { &*attribute };
            if attribute.attisdropped || attribute.atttypid != request.type_oid {
                return Err(format!(
                    "relation OID {} attribute {} changed before resident table scan",
                    u32::from(relation.rd_id),
                    request.attno
                ));
            }
        }
        Ok(())
    }

    fn validate_select_privileges(&self, requests: &[ColumnRequest]) -> Result<(), String> {
        // These OID-based ACL checks use PostgreSQL's current syscache state.
        // AccessShareLock prevents the relation or its columns from being
        // renamed or replaced between this proof and the table-AM scan.
        // SAFETY: relation is live under AccessShareLock on the backend thread.
        let relid = unsafe { (*self.relation).rd_id };
        // SAFETY: GetUserId reads backend-local effective-role state.
        let user_id = unsafe { pg_sys::GetUserId() };
        let select_mode = pg_sys::AclMode::from(pg_sys::ACL_SELECT);
        // SAFETY: relid is locked and user_id/select_mode are valid ACL inputs.
        let relation_select = unsafe {
            pg_sys::pg_class_aclcheck(relid, user_id, select_mode) == pg_sys::AclResult::ACLCHECK_OK
        };
        let granted = select_privilege_granted(relation_select, requests, |attno| {
            // SAFETY: requested attnos were checked against the locked tuple
            // descriptor above. None requests PostgreSQL's COUNT(*) rule: any
            // live user column with SELECT is sufficient.
            let result = unsafe {
                match attno {
                    Some(attno) => {
                        pg_sys::pg_attribute_aclcheck(relid, attno, user_id, select_mode)
                    }
                    None => pg_sys::pg_attribute_aclcheck_all(
                        relid,
                        user_id,
                        select_mode,
                        pg_sys::AclMaskHow::ACLMASK_ANY,
                    ),
                }
            };
            result == pg_sys::AclResult::ACLCHECK_OK
        });
        if !granted {
            return Err(format!(
                "resident ACL/type preflight for relation OID {} failed: current role lacks the SELECT privileges required by the resident projection",
                u32::from(relid)
            ));
        }
        Ok(())
    }

    fn begin(&mut self) -> Result<(), String> {
        // SAFETY: stage_relation is invoked inside an active SQL statement.
        let snapshot = unsafe { pg_sys::GetActiveSnapshot() };
        if snapshot.is_null() {
            return Err("resident table scan has no active PostgreSQL snapshot".to_owned());
        }
        // SAFETY: the relation is open under AccessShareLock and the active
        // snapshot remains valid for the surrounding statement.
        self.scan = unsafe { begin_default_resident_scan(self.relation, snapshot) };
        if self.scan.is_null() {
            return Err("PostgreSQL table AM returned a NULL scan descriptor".to_owned());
        }
        // SAFETY: rd_att and the table AM's slot callbacks remain valid while
        // the relation is open. This supports non-heap ordinary table AMs.
        self.slot = unsafe {
            pg_sys::MakeSingleTupleTableSlot(
                (*self.relation).rd_att,
                pg_sys::table_slot_callbacks(self.relation),
            )
        };
        if self.slot.is_null() {
            return Err("PostgreSQL could not allocate a resident table scan slot".to_owned());
        }
        Ok(())
    }
}

impl Drop for DirectTableScan {
    fn drop(&mut self) {
        if !self.scan.is_null() {
            // SAFETY: scan was returned by table_beginscan and is ended once.
            unsafe { pg_sys::table_endscan(self.scan) };
            self.scan = std::ptr::null_mut();
        }
        if !self.slot.is_null() {
            // SAFETY: slot was returned by MakeSingleTupleTableSlot and is
            // dropped once, before its relation descriptor is closed.
            unsafe { pg_sys::ExecDropSingleTupleTableSlot(self.slot) };
            self.slot = std::ptr::null_mut();
        }
        if !self.relation.is_null() {
            // SAFETY: relation was opened with this lock mode and is closed
            // once after all scan-owned objects have been released.
            unsafe {
                pg_sys::table_close(self.relation, pg_sys::AccessShareLock as pg_sys::LOCKMODE);
            };
            self.relation = std::ptr::null_mut();
        }
        #[cfg(feature = "pg_test")]
        TEST_DIRECT_SCAN_DROP_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    }
}

unsafe fn begin_default_resident_scan(
    relation: pg_sys::Relation,
    snapshot: pg_sys::Snapshot,
) -> pg_sys::TableScanDesc {
    #[cfg(feature = "pg18")]
    {
        // SAFETY: relation and snapshot are live; zero keys requests an
        // unqualified MVCC scan through the relation's table AM.
        unsafe { pg_sys::table_beginscan(relation, snapshot, 0, std::ptr::null_mut()) }
    }
    #[cfg(feature = "pg19")]
    {
        // SAFETY: PG19 adds a flags argument; zero selects default behavior.
        unsafe { pg_sys::table_beginscan(relation, snapshot, 0, std::ptr::null_mut(), 0) }
    }
}

fn caught_loader_error<R>(caught: pg_sys::panic::CaughtError, context: &str) -> Result<R, String> {
    use pg_sys::panic::CaughtError;
    let (level, code, message) = match &caught {
        CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => (
            error.level(),
            error.sql_error_code(),
            error.message().to_owned(),
        ),
        CaughtError::RustPanic { .. } => caught.rethrow(),
    };
    // Preserve PostgreSQL control-flow and resource failures. In particular,
    // statement_timeout and cancellation retain SQLSTATE 57014 after the scan
    // guard has unwound, rather than becoming an ordinary loader decline.
    if syscache::postgres_error_requires_rethrow(level, code) {
        caught.rethrow();
    }
    Err(format!("{context}: {message}"))
}

fn scan_relation_direct(
    relid: pg_sys::Oid,
    requests: &[ColumnRequest],
    builders: &mut [ColumnBuilder],
) -> Result<u64, String> {
    pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
        scan_relation_direct_inner(relid, requests, builders)
    }))
    .catch_others(|caught| caught_loader_error(caught, "resident direct table scan failed"))
    .execute()
}

fn scan_relation_direct_inner(
    relid: pg_sys::Oid,
    requests: &[ColumnRequest],
    builders: &mut [ColumnBuilder],
) -> Result<u64, String> {
    if requests.len() != builders.len() {
        return Err("resident scan request/builder count mismatch".to_owned());
    }
    let mut table = DirectTableScan::open_relation(relid, requests)?;
    table.begin()?;
    let max_attno = requests.iter().map(|request| request.attno).max();

    let mut row_count = 0_u64;
    let mut batch_rows = 0_usize;
    loop {
        // SAFETY: table.scan and table.slot are live and owned by the guard.
        let found = unsafe {
            pg_sys::table_scan_getnextslot(
                table.scan,
                pg_sys::ScanDirection::ForwardScanDirection,
                table.slot,
            )
        };
        if !found {
            return Ok(row_count);
        }
        if batch_rows == 0 {
            for builder in &mut *builders {
                builder.try_reserve(LOAD_INTERRUPT_CHECK_ROWS)?;
            }
        }
        if let Some(max_attno) = max_attno {
            // Deform the heap/table-AM tuple once through the highest requested
            // attribute. Individual requested columns then read the slot's
            // cached Datum/null arrays directly instead of re-entering
            // slot_getattr for every column.
            // SAFETY: the slot contains the current row; every requested attno
            // is positive and was validated against its locked descriptor.
            unsafe { pg_sys::slot_getsomeattrs(table.slot, i32::from(max_attno)) };
            // SAFETY: slot_getsomeattrs populated both arrays through max_attno.
            if unsafe { (*table.slot).tts_values.is_null() || (*table.slot).tts_isnull.is_null() } {
                return Err("resident table scan slot has no deformed value/null arrays".to_owned());
            }
        }
        for (request, builder) in requests.iter().zip(builders.iter_mut()) {
            let index = request
                .attno
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| "resident request attribute number is not positive".to_owned())?;
            // SAFETY: slot_getsomeattrs above populated arrays through the
            // maximum requested attno and this request is within that range.
            let (datum, is_null) = unsafe {
                (
                    *(*table.slot).tts_values.add(index),
                    *(*table.slot).tts_isnull.add(index),
                )
            };
            // SAFETY: the request and builder carry the freshly validated
            // dynamic type for this attribute; slot storage remains live.
            unsafe { builder.push_datum(datum, is_null, request.attno) }?;
        }
        row_count = row_count
            .checked_add(1)
            .ok_or("resident row count exceeds u64")?;
        #[cfg(feature = "pg_test")]
        inject_direct_scan_error(row_count);
        batch_rows += 1;
        if batch_rows == LOAD_INTERRUPT_CHECK_ROWS {
            pgrx::check_for_interrupts!();
            batch_rows = 0;
        }
    }
}

#[cfg(feature = "pg_test")]
fn inject_direct_scan_error(row_count: u64) {
    let error = TEST_DIRECT_SCAN_ERROR_AFTER_ROWS.with(|trigger| {
        let (target, error) = trigger.get()?;
        if target != row_count {
            return None;
        }
        trigger.set(None);
        Some(error)
    });
    match error {
        Some(TestDirectScanError::Recoverable) => pgrx::ereport!(
            pgrx::PgLogLevel::ERROR,
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
            "injected resident direct scan error"
        ),
        Some(TestDirectScanError::QueryCanceled) => pgrx::ereport!(
            pgrx::PgLogLevel::ERROR,
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "injected resident direct scan cancellation"
        ),
        None => {}
    }
}

pub(super) fn stage_relation(
    relid: pg_sys::Oid,
    requests: &[ColumnRequest],
) -> Result<StagedRelation, String> {
    for attempt in 0..2 {
        let generation_before = ledger::generation_stamp(relid);
        let fingerprint_before = RelationFingerprint::capture(relid, requests).ok_or_else(|| {
            format!(
                "relation OID {}, one of its requested columns, or its invalidation trigger contract changed before resident load",
                u32::from(relid)
            )
        })?;
        let relfilenode_before = fingerprint_before.relfilenode();
        let started = Instant::now();
        let estimated_bytes = estimate_resident_bytes(relid, requests)?;
        let budget = gucs::resident_memory_budget_bytes();
        if estimated_bytes > budget {
            return Err(format!(
                "relation OID {} resident load estimate {estimated_bytes} bytes exceeds cluster budget {budget} bytes",
                u32::from(relid)
            ));
        }
        let mut builders = requests
            .iter()
            .map(|request| ColumnBuilder::for_type(request.type_oid))
            .collect::<Result<Vec<_>, _>>()?;
        let scan_started = Instant::now();
        let row_count = scan_relation_direct(relid, requests, &mut builders)?;
        let scan_ms = scan_started.elapsed().as_secs_f64() * 1000.0;
        let generation_after = ledger::generation_stamp(relid);
        let fingerprint_after = RelationFingerprint::capture(relid, requests);
        if generation_before != generation_after
            || fingerprint_after.as_ref() != Some(&fingerprint_before)
        {
            if attempt == 0 {
                continue;
            }
            return Err(format!(
                "relation OID {} changed during two consecutive resident loads; retry after concurrent DML/DDL finishes",
                u32::from(relid)
            ));
        }
        let encode_started = Instant::now();
        let columns = if row_count == 0 {
            requests
                .iter()
                .map(|request| request.attno)
                .zip(builders.into_iter().map(ColumnBuilder::finish_empty))
                .map(|(attno, column)| column.map(|column| (attno, column)))
                .collect::<Result<BTreeMap<_, _>, _>>()?
        } else {
            requests
                .iter()
                .map(|request| request.attno)
                .zip(builders.into_iter().map(ColumnBuilder::finish))
                .map(|(attno, column)| column.map(|column| (attno, column)))
                .collect::<Result<BTreeMap<_, _>, _>>()?
        };
        let encode_ms = encode_started.elapsed().as_secs_f64() * 1000.0;
        let staged_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut representation = ResidentRepresentationEvidence {
            scan_batches: row_count.div_ceil(LOAD_INTERRUPT_CHECK_ROWS as u64),
            preflight_ms: (staged_ms - scan_ms - encode_ms).max(0.0),
            scan_ms,
            encode_ms,
            ..ResidentRepresentationEvidence::default()
        };
        for column in columns.values() {
            checked_add_representation(&mut representation, column.representation_evidence()?)?;
        }
        return Ok(StagedRelation {
            relid,
            relfilenode: relfilenode_before,
            fingerprint: fingerprint_before,
            generation: generation_after,
            columns,
            row_count,
            loaded_at_us: now_us(),
            load_ms: staged_ms,
            representation,
        });
    }
    Err("resident load retry loop exhausted".to_owned())
}

const INVALIDATION_TRIGGER_NAME: &str = "__pg_accel_residency_v2_7d9e";

struct InvalidationTriggerFunction {
    oid: pg_sys::Oid,
    catalog_xmin: u64,
    catalog_tid: String,
    qualified_name: String,
}

fn invalidation_trigger_function() -> Result<InvalidationTriggerFunction, String> {
    let function_query = "SELECT p.oid::int8, p.xmin::text::int8, p.ctid::text, \
        pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(p.proname) \
        FROM pg_catalog.pg_proc p \
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
        JOIN pg_catalog.pg_depend d ON d.classid = 'pg_catalog.pg_proc'::regclass \
          AND d.objid = p.oid AND d.refclassid = 'pg_catalog.pg_extension'::regclass \
          AND d.deptype = 'e' \
        JOIN pg_catalog.pg_extension e ON e.oid = d.refobjid \
        WHERE e.extname = 'pg_accel' AND p.proname = 'pg_accel_residency_invalidate' \
          AND p.pronargs = 0 AND p.prokind = 'f' \
          AND p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype";
    Spi::connect(|client| {
        let rows = client
            .select(function_query, Some(2), &[])
            .map_err(|error| {
                format!("failed to resolve extension-owned residency trigger function: {error:?}")
            })?;
        if rows.is_empty() {
            return Err(
                "pg_accel_residency_invalidate() is not an extension-owned function".to_owned(),
            );
        }
        if rows.len() != 1 {
            return Err(
                "pg_accel_residency_invalidate() extension identity is ambiguous".to_owned(),
            );
        }
        let row = rows.first();
        let oid = row
            .get::<i64>(1)
            .map_err(|error| format!("trigger function OID read failed: {error:?}"))?
            .ok_or("trigger function OID is NULL")?;
        let catalog_xmin = row
            .get::<i64>(2)
            .map_err(|error| format!("trigger function xmin read failed: {error:?}"))?
            .ok_or("trigger function xmin is NULL")?
            .try_into()
            .map_err(|_| "trigger function xmin is not an unsigned integer".to_owned())?;
        let name = row
            .get::<String>(4)
            .map_err(|error| format!("trigger function name read failed: {error:?}"))?
            .ok_or("trigger function name is NULL")?;
        let oid = u32::try_from(oid)
            .map(pg_sys::Oid::from)
            .map_err(|_| "trigger function OID exceeds oid range".to_owned())?;
        let catalog_tid = row
            .get::<String>(3)
            .map_err(|error| format!("trigger function ctid read failed: {error:?}"))?
            .ok_or("trigger function ctid is NULL")?;
        Ok::<_, String>(InvalidationTriggerFunction {
            oid,
            catalog_xmin,
            catalog_tid,
            qualified_name: name,
        })
    })
}

pub(super) fn invalidation_trigger_fingerprint(
    relid: pg_sys::Oid,
) -> Option<InvalidationTriggerFingerprint> {
    let function = invalidation_trigger_function().ok()?;
    let existing_query = format!(
        "SELECT oid::int8, xmin::text::int8, ctid::text, tgfoid::int8, tgtype::int4, tgenabled::text, tgnargs::int4, (tgattr::text = '') \
         FROM pg_catalog.pg_trigger \
         WHERE tgrelid = {}::oid AND tgname = '{}' AND NOT tgisinternal",
        u32::from(relid),
        INVALIDATION_TRIGGER_NAME
    );
    let existing = Spi::connect(|client| {
        let rows = client
            .select(&existing_query, Some(1), &[])
            .map_err(|error| format!("failed to inspect residency trigger: {error:?}"))?;
        if rows.is_empty() {
            return Ok::<_, String>(None);
        }
        let row = rows.first();
        Ok(Some((
            row.get::<i64>(1)
                .map_err(|error| format!("trigger oid read failed: {error:?}"))?
                .ok_or("trigger oid is NULL")?,
            row.get::<i64>(2)
                .map_err(|error| format!("trigger xmin read failed: {error:?}"))?
                .ok_or("trigger xmin is NULL")?,
            row.get::<String>(3)
                .map_err(|error| format!("trigger ctid read failed: {error:?}"))?
                .ok_or("trigger ctid is NULL")?,
            row.get::<i64>(4)
                .map_err(|error| format!("tgfoid read failed: {error:?}"))?
                .ok_or("tgfoid is NULL")?,
            row.get::<i32>(5)
                .map_err(|error| format!("tgtype read failed: {error:?}"))?
                .ok_or("tgtype is NULL")?,
            row.get::<String>(6)
                .map_err(|error| format!("tgenabled read failed: {error:?}"))?
                .ok_or("tgenabled is NULL")?,
            row.get::<i32>(7)
                .map_err(|error| format!("tgnargs read failed: {error:?}"))?
                .ok_or("tgnargs is NULL")?,
            row.get::<bool>(8)
                .map_err(|error| format!("tgattr read failed: {error:?}"))?
                .ok_or("tgattr emptiness is NULL")?,
        )))
    })
    .ok()??;
    let expected_type = i32::try_from(
        pg_sys::TRIGGER_TYPE_INSERT
            | pg_sys::TRIGGER_TYPE_DELETE
            | pg_sys::TRIGGER_TYPE_UPDATE
            | pg_sys::TRIGGER_TYPE_TRUNCATE,
    )
    .ok()?;
    let (
        trigger_oid,
        trigger_catalog_xmin,
        trigger_catalog_tid,
        existing_function,
        trigger_type,
        enabled,
        argument_count,
        all_updates,
    ) = existing;
    let trigger_oid = u32::try_from(trigger_oid).ok().map(pg_sys::Oid::from)?;
    let existing_function = u32::try_from(existing_function)
        .ok()
        .map(pg_sys::Oid::from)?;
    let trigger_catalog_xmin = u64::try_from(trigger_catalog_xmin).ok()?;
    (existing_function == function.oid
        && trigger_type == expected_type
        && enabled == "A"
        && argument_count == 0
        && all_updates)
        .then_some(InvalidationTriggerFingerprint {
            trigger_oid,
            trigger_catalog_xmin,
            trigger_catalog_tid,
            function_oid: function.oid,
            function_catalog_xmin: function.catalog_xmin,
            function_catalog_tid: function.catalog_tid,
            trigger_type,
        })
}

pub(super) fn ensure_invalidation_trigger(relid: pg_sys::Oid) -> Result<TriggerInstall, String> {
    let qualified = qualified_relation_name(relid)?;
    let ownership_query = format!(
        "SELECT pg_catalog.pg_has_role(c.relowner, 'USAGE') FROM pg_catalog.pg_class c WHERE c.oid = {}::oid",
        u32::from(relid)
    );
    let owns = Spi::get_one::<bool>(&ownership_query)
        .map_err(|error| format!("failed to check ownership for {qualified}: {error:?}"))?
        .unwrap_or(false);
    if !owns {
        return Err(format!(
            "cannot install pg_accel residency invalidation trigger on {qualified}: current role must own the table (or be a member of its owner role); run pg_accel_pin as the owner"
        ));
    }
    let function = invalidation_trigger_function()?;
    let existing_query = format!(
        "SELECT 1 FROM pg_catalog.pg_trigger \
         WHERE tgrelid = {}::oid AND tgname = '{}' AND NOT tgisinternal",
        u32::from(relid),
        INVALIDATION_TRIGGER_NAME
    );
    let existing = Spi::connect(|client| {
        client
            .select(&existing_query, Some(1), &[])
            .map(|rows| !rows.is_empty())
            .map_err(|error| {
                format!("failed to inspect residency trigger on {qualified}: {error:?}")
            })
    })?;
    if existing {
        if invalidation_trigger_fingerprint(relid).is_some() {
            return Ok(TriggerInstall::Existing);
        }
        return Err(format!(
            "cannot trust existing trigger {INVALIDATION_TRIGGER_NAME} on {qualified}: expected extension function OID {}, statement-level AFTER INSERT/UPDATE/DELETE/TRUNCATE, ENABLE ALWAYS; drop the conflicting trigger and retry",
            u32::from(function.oid)
        ));
    }
    let create_sql = format!(
        "CREATE TRIGGER {INVALIDATION_TRIGGER_NAME} \
         AFTER INSERT OR UPDATE OR DELETE OR TRUNCATE ON {qualified} \
         FOR EACH STATEMENT EXECUTE FUNCTION {}()",
        function.qualified_name
    );
    Spi::run(&create_sql).map_err(|error| format!("cannot install pg_accel residency invalidation trigger on {qualified}: {error:?}; run pg_accel_pin as the table owner"))?;
    let enable_sql =
        format!("ALTER TABLE {qualified} ENABLE ALWAYS TRIGGER {INVALIDATION_TRIGGER_NAME}");
    Spi::run(&enable_sql).map_err(|error| format!("cannot mark pg_accel residency invalidation trigger ENABLE ALWAYS on {qualified}: {error:?}"))?;
    // The statement snapshot that waited to install this trigger can predate
    // DML which committed immediately before the DDL lock was acquired. The
    // explicit loader switches to GetLatestSnapshot while this DDL lock is
    // still held. One immediate global bump invalidates older snapshots; a
    // commit-time bump would incorrectly evict the fresh explicit pin.
    ledger::note_trigger_install(relid);
    Ok(TriggerInstall::New)
}

fn now_us() -> i64 {
    #[cfg(any(test, feature = "pg_test"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_micros());
        i64::try_from(micros).unwrap_or(i64::MAX)
    }
    #[cfg(all(not(test), not(feature = "pg_test")))]
    {
        // SAFETY: backend main thread.
        unsafe { pg_sys::GetCurrentTimestamp() }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn raster_catalog_identity() -> syscache::PostgisRasterCatalogIdentity {
        syscache::PostgisRasterCatalogIdentity {
            extension_oid: pg_sys::Oid::from(60_000),
            schema_oid: pg_sys::Oid::from(60_002),
            raster_type_oid: pg_sys::Oid::from(60_001),
            summary_stats_type_oid: pg_sys::Oid::from(60_003),
            reclass_fn_oid: pg_sys::Oid::from(60_004),
            summary_stats_fn_oid: pg_sys::Oid::from(60_005),
            summary_stats_default_band_fn_oid: pg_sys::Oid::from(60_006),
            as_wkb_fn_oid: pg_sys::Oid::from(60_007),
            rast_from_wkb_fn_oid: pg_sys::Oid::from(60_008),
            reclass_impl_fn_oid: pg_sys::Oid::from(60_009),
            summary_stats_impl_fn_oid: pg_sys::Oid::from(60_010),
            fingerprint_words: vec![1, 2, 3],
        }
    }

    #[test]
    fn fixed_width_load_classifies_zero_builtin_and_text_requests() {
        assert_eq!(columns_have_fixed_width_load(&[]), Ok(true));
        assert_eq!(
            columns_have_fixed_width_load(&[
                ColumnRequest {
                    attno: 1,
                    type_oid: pg_sys::INT4OID,
                },
                ColumnRequest {
                    attno: 2,
                    type_oid: pg_sys::TIMESTAMPOID,
                },
            ]),
            Ok(true)
        );
        assert_eq!(
            columns_have_fixed_width_load(&[ColumnRequest {
                attno: 1,
                type_oid: pg_sys::TEXTOID,
            }]),
            Ok(false)
        );
        assert_eq!(
            columns_have_fixed_width_load(&[ColumnRequest {
                attno: 1,
                type_oid: pg_sys::NUMERICOID,
            }]),
            Ok(false)
        );
    }

    #[test]
    fn select_acl_matches_postgresql_projection_rules() {
        let requests = [
            ColumnRequest {
                attno: 1,
                type_oid: pg_sys::INT4OID,
            },
            ColumnRequest {
                attno: 2,
                type_oid: pg_sys::INT4OID,
            },
        ];
        assert!(select_privilege_granted(true, &requests, |_| {
            panic!("table SELECT must bypass column ACL checks")
        }));
        assert!(select_privilege_granted(false, &requests, |attno| {
            matches!(attno, Some(1 | 2))
        }));
        assert!(!select_privilege_granted(false, &requests, |attno| {
            attno == Some(1)
        }));

        let mut count_only_checks = 0;
        assert!(select_privilege_granted(false, &[], |attno| {
            count_only_checks += 1;
            assert_eq!(attno, None);
            true
        }));
        assert_eq!(count_only_checks, 1);
    }

    #[test]
    fn cancellation_resource_and_deadlock_sqlstates_are_rethrown() {
        for code in [
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            PgSqlErrorCode::ERRCODE_T_R_DEADLOCK_DETECTED,
            PgSqlErrorCode::ERRCODE_LOCK_NOT_AVAILABLE,
            PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            PgSqlErrorCode::ERRCODE_DISK_FULL,
            PgSqlErrorCode::ERRCODE_IO_ERROR,
            PgSqlErrorCode::ERRCODE_ASSERT_FAILURE,
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            PgSqlErrorCode::ERRCODE_INDEX_CORRUPTED,
        ] {
            assert!(syscache::postgres_error_requires_rethrow(
                pgrx::PgLogLevel::ERROR,
                code
            ));
        }
        for code in [
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
        ] {
            assert!(!syscache::postgres_error_requires_rethrow(
                pgrx::PgLogLevel::ERROR,
                code
            ));
        }
    }

    #[test]
    fn raw_datum_ingestion_preserves_values_and_nulls() {
        let mut builder = ColumnBuilder::I32 {
            type_oid: pg_sys::INT4OID,
            values: Vec::new(),
            nulls: Vec::new(),
            saw_null: false,
        };
        unsafe {
            // SAFETY: these are by-value int4 Datums paired with the matching
            // catalog-proved builder type; NULL prevents the second payload
            // from being interpreted.
            builder
                .push_datum(pg_sys::Datum::from((-17_i32) as usize), false, 3)
                .expect("non-null int4 stages");
            builder
                .push_datum(pg_sys::Datum::from(0_usize), true, 3)
                .expect("NULL int4 stages");
        }
        let StagedColumn::I32 { values, nulls, .. } =
            builder.finish().expect("raw Datum builder finishes")
        else {
            panic!("int4 builder changed staging representation");
        };
        assert_eq!(values, vec![-17, 0]);
        assert_eq!(nulls, Some(vec![0, 1]));
    }

    #[test]
    fn primitive_null_masks_are_lazy_and_backfill_late_nulls() {
        for oid in [
            pg_sys::BOOLOID,
            pg_sys::INT2OID,
            pg_sys::INT4OID,
            pg_sys::INT8OID,
            pg_sys::DATEOID,
            pg_sys::TIMESTAMPOID,
            pg_sys::TIMESTAMPTZOID,
            pg_sys::FLOAT4OID,
            pg_sys::FLOAT8OID,
        ] {
            for first_null in [None, Some(0), Some(16385)] {
                let mut builder = ColumnBuilder::for_type(oid).expect("builtin type");
                builder.try_reserve(8192).expect("reserve values");
                for row in 0..16387 {
                    if row % LOAD_INTERRUPT_CHECK_ROWS == 0 {
                        builder
                            .try_reserve(LOAD_INTERRUPT_CHECK_ROWS)
                            .expect("next batch");
                    }
                    let is_null = first_null == Some(row);
                    // SAFETY: zero is a valid by-value Datum for each listed
                    // builtin, and NULL suppresses payload decoding.
                    unsafe { builder.push_datum(pg_sys::Datum::from(0_usize), is_null, 1) }
                        .expect("stage builtin row");
                    let (nulls, saw_null) = match &builder {
                        ColumnBuilder::Bool {
                            nulls, saw_null, ..
                        }
                        | ColumnBuilder::I32 {
                            nulls, saw_null, ..
                        }
                        | ColumnBuilder::I64 {
                            nulls, saw_null, ..
                        }
                        | ColumnBuilder::F32 {
                            nulls, saw_null, ..
                        }
                        | ColumnBuilder::F64 {
                            nulls, saw_null, ..
                        } => (nulls, saw_null),
                        _ => panic!("unexpected primitive builder"),
                    };
                    if first_null.is_none_or(|first| row < first) {
                        assert!(!saw_null);
                        assert!(nulls.capacity() <= LOAD_INTERRUPT_CHECK_ROWS);
                    } else {
                        assert!(*saw_null);
                        assert!(nulls.len() <= row + 1);
                        assert_eq!(nulls.last(), Some(&u8::from(is_null)));
                    }
                }
                let staged = builder.finish().expect("finish primitive column");
                let nulls = match staged {
                    StagedColumn::Bool { nulls, .. }
                    | StagedColumn::I32 { nulls, .. }
                    | StagedColumn::I64 { nulls, .. }
                    | StagedColumn::F32 { nulls, .. }
                    | StagedColumn::F64 { nulls, .. } => nulls,
                    _ => panic!("unexpected primitive column"),
                };
                assert_eq!(
                    nulls,
                    first_null.map(|first| {
                        let mut mask = vec![0; 16387];
                        mask[first] = 1;
                        mask
                    })
                );
            }
        }
    }

    #[test]
    fn text_remapping_handles_absent_and_late_null_masks() {
        for late_null in [false, true] {
            let mut builder = TextColumnBuilder::default();
            builder.try_reserve(8192).expect("reserve codes only");
            for row in 0..16385 {
                if row % LOAD_INTERRUPT_CHECK_ROWS == 0 {
                    builder
                        .try_reserve(LOAD_INTERRUPT_CHECK_ROWS)
                        .expect("next text batch");
                }
                builder
                    .push(Some(if row % 2 == 0 { "z" } else { "a" }))
                    .expect("label");
            }
            assert!(builder.nulls.capacity() <= super::super::nulls::STAGING_ROWS);
            if late_null {
                builder.push(None).expect("late NULL");
            }
            let StagedColumn::TextDictionary {
                codes,
                nulls,
                labels,
                ..
            } = builder.finish(pg_sys::TEXTOID).expect("sorted dictionary")
            else {
                panic!("text representation changed");
            };
            assert_eq!(labels, ["a", "z"]);
            for (row, code) in codes[..16385].iter().enumerate() {
                assert_eq!(*code, i32::from(row % 2 == 0));
            }
            if late_null {
                assert_eq!(codes[16385], 0);
                let mask = nulls.expect("late mask");
                assert!(mask[..16385].iter().all(|null| *null == 0));
                assert_eq!(mask[16385], 1);
            } else {
                assert!(nulls.is_none());
            }
        }
    }

    #[test]
    fn raster_null_mask_backfills_without_changing_exact_payloads() {
        let value = multiband_raster();
        let mut builder = RasterColumnBuilder::new(raster_catalog_identity(), 1024);
        builder.reserve_rows(8192).expect("reserve raster rows");
        for _ in 0..16385 {
            builder.push_value(Some(&value)).expect("raster prefix");
        }
        assert!(builder.nulls.capacity() <= super::super::nulls::STAGING_ROWS);
        assert!(builder.push_value(Some(&[0])).is_err());
        builder.push_value(None).expect("late NULL");
        builder.push_value(Some(&value)).expect("raster suffix");
        let StagedColumn::Raster { data, .. } = builder
            .finish_after_catalog_proof()
            .expect("pure raster staging fixture")
        else {
            panic!("raster representation changed");
        };
        let mask = data.nulls.as_ref().expect("late mask");
        assert!(mask[..16385].iter().all(|null| *null == 0));
        assert_eq!(&mask[16385..], &[1, 0]);
        assert_eq!(data.exact.value(16384), Some(value.as_slice()));
        assert_eq!(data.exact.value(16385), Some([].as_slice()));
        assert_eq!(data.exact.value(16386), Some(value.as_slice()));
    }

    #[test]
    fn raw_h3_datum_preserves_all_unsigned_bits() {
        let bits = 0xf123_4567_89ab_cdef_u64;
        let value = unsafe {
            // SAFETY: h3index is a by-value 64-bit Datum; the explicit false
            // null flag marks these bits as a present value for the trait hook.
            <RawH3Datum as pgrx::FromDatum>::from_polymorphic_datum(
                pg_sys::Datum::from(bits),
                false,
                pg_sys::Oid::from(50_001),
            )
        };
        assert_eq!(value, Some(RawH3Datum(bits)));
        assert_eq!(
            unsafe {
                // SAFETY: the explicit true null flag means the trait hook does
                // not interpret the supplied by-value Datum as a present value.
                <RawH3Datum as pgrx::FromDatum>::from_polymorphic_datum(
                    pg_sys::Datum::from(bits),
                    true,
                    pg_sys::Oid::from(50_001),
                )
            },
            None
        );
    }

    #[test]
    fn staged_h3_uses_exact_values_plus_null_sidecar_accounting() {
        let without_nulls = StagedColumn::H3 {
            type_oid: pg_sys::Oid::from(50_001),
            values: vec![1, u64::MAX],
            nulls: None,
        };
        assert_eq!(without_nulls.device_bytes(), Ok(16));

        let with_nulls = ColumnBuilder::H3 {
            type_oid: pg_sys::Oid::from(50_001),
            values: vec![1, 0],
            nulls: vec![0, 1],
            saw_null: true,
        }
        .finish()
        .expect("valid H3 staging");
        assert_eq!(with_nulls.device_bytes(), Ok(18));
        match with_nulls {
            StagedColumn::H3 { nulls, .. } => assert_eq!(nulls, Some(vec![0, 1])),
            _ => panic!("H3 builder changed staging representation"),
        }
    }

    fn raster_header(width: u16, height: u16, band_count: u16) -> Vec<u8> {
        let mut wkb = Vec::new();
        wkb.push(1);
        wkb.extend_from_slice(&0_u16.to_le_bytes());
        wkb.extend_from_slice(&band_count.to_le_bytes());
        for value in [2.0_f64, -3.0, 10.0, 20.0, 0.25, -0.5] {
            wkb.extend_from_slice(&value.to_le_bytes());
        }
        wkb.extend_from_slice(&4_326_i32.to_le_bytes());
        wkb.extend_from_slice(&width.to_le_bytes());
        wkb.extend_from_slice(&height.to_le_bytes());
        wkb
    }

    fn multiband_raster() -> Vec<u8> {
        let mut wkb = raster_header(2, 1, 2);
        wkb.push(0x0a | 0x40);
        wkb.extend_from_slice(&f32::NAN.to_le_bytes());
        wkb.extend_from_slice(&1.5_f32.to_le_bytes());
        wkb.extend_from_slice(&2.5_f32.to_le_bytes());
        wkb.push(6 | 0x40 | 0x20);
        wkb.extend_from_slice(&u16::MAX.to_le_bytes());
        wkb.extend_from_slice(&7_u16.to_le_bytes());
        wkb.extend_from_slice(&8_u16.to_le_bytes());
        wkb
    }

    #[test]
    fn raster_staging_retains_exact_wkb_and_charges_host_plus_device_bytes() {
        let wkb = multiband_raster();
        let mut builder = RasterColumnBuilder::new(raster_catalog_identity(), 1_024);
        builder.push_value(None).expect("NULL raster stages");
        builder
            .push_value(Some(&wkb))
            .expect("valid multiband raster stages");
        let staged = builder.finish_for_test().expect("raster domain validates");
        let declared = staged
            .accounting()
            .expect("raster accounting validates")
            .checked_total()
            .expect("raster bytes fit u64");
        let StagedColumn::Raster {
            data,
            max_exact_value_bytes,
            ..
        } = staged
        else {
            panic!("raster builder changed staging representation");
        };
        let accounting = data
            .accounting(max_exact_value_bytes)
            .expect("staged raster accounting validates");
        assert_eq!(declared, accounting.checked_total().expect("total fits"));
        assert_eq!(data.nulls, Some(vec![1, 0]));
        assert_eq!(data.rows[0], ResidentRasterRow::default());
        assert_eq!(data.rows[1].first_band, 0);
        assert_eq!(data.rows[1].band_count, 2);
        assert_eq!(data.rows[1].srid, 4_326);
        assert_eq!(data.rows[1].skew_x, 0.25);
        assert_eq!(data.rows[1].skew_y, -0.5);
        assert_eq!(data.band_offsets, vec![0, 8, 12]);
        assert!(data.bands[0].nodata.is_nan());
        assert_eq!(data.bands[0].pixel_type, 10);
        assert_eq!(
            data.bands[1].flags,
            RESIDENT_RASTER_BAND_HAS_NODATA | RESIDENT_RASTER_BAND_IS_NODATA
        );
        assert_eq!(data.exact.value(0), Some(&[][..]));
        assert_eq!(data.exact.value(1), Some(wkb.as_slice()));
        assert_eq!(
            accounting.retained_host_exact_bytes,
            u64::try_from(
                3 * std::mem::size_of::<u64>()
                    + wkb.len()
                    + 2 * 8
                    + 2 * 8
                    + 2 * std::mem::size_of::<crate::engine::residency::ResidentRasterWorkRow>()
            )
            .expect("test bytes fit")
        );
        let stats = data.stats().expect("staged raster statistics validate");
        assert_eq!(stats.row_count, 2);
        assert_eq!(stats.non_null_rows, 1);
        assert_eq!(stats.total_grid_pixels, 2);
        assert_eq!(stats.total_band_pixels, 4);
        assert_eq!(
            stats.input_wkb_bytes,
            u64::try_from(wkb.len()).expect("test WKB length fits")
        );
        assert_eq!(stats.selected_band_pixels(1), Some(2));
        assert_eq!(stats.selected_band_pixels(2), Some(2));
        assert_eq!(stats.selected_band_pixels(3), Some(0));
        assert_eq!(stats.selected_band_pixels(0), None);
        assert_eq!(stats.selected_band_rows(1), Some(1));
        assert_eq!(stats.selected_band_rows(2), Some(1));
        assert_eq!(stats.selected_band_rows(3), Some(0));
        assert_eq!(stats.selected_band_rows(0), None);
        let input_bytes = u64::try_from(wkb.len()).expect("test WKB length fits");
        assert_eq!(
            stats.reclass_output_wkb_bytes(4),
            Some(input_bytes - 12 + 3)
        );
        assert_eq!(
            stats.reclass_output_wkb_bytes(5),
            Some(input_bytes - 12 + 6)
        );
        assert_eq!(stats.reclass_output_wkb_bytes(7), Some(input_bytes));
        assert_eq!(stats.reclass_output_wkb_bytes(10), None);
        assert_eq!(stats.reclass_output_pixel_bytes(4), Some(2));
        assert_eq!(stats.reclass_output_pixel_bytes(5), Some(4));
        assert_eq!(stats.reclass_output_pixel_bytes(7), Some(8));
        assert_eq!(stats.reclass_output_pixel_bytes(10), None);
    }

    #[test]
    fn raster_staging_preserves_empty_zero_band_values() {
        let wkb = raster_header(0, 0, 0);
        let mut builder = RasterColumnBuilder::new(raster_catalog_identity(), 1_024);
        builder
            .push_value(Some(&wkb))
            .expect("zero-band raster is a non-NULL value");
        let StagedColumn::Raster { data, .. } =
            builder.finish_for_test().expect("empty raster validates")
        else {
            panic!("raster builder changed staging representation");
        };
        assert!(data.pixels.is_empty());
        assert_eq!(data.band_offsets, vec![0]);
        assert!(data.bands.is_empty());
        assert_eq!(data.rows.len(), 1);
        assert_eq!(data.rows[0].width, 0);
        assert_eq!(data.rows[0].band_count, 0);
        assert_eq!(data.exact.value(0), Some(wkb.as_slice()));
    }

    #[test]
    fn raster_staging_rejects_structural_values_before_adding_rows() {
        const HEADER_BYTES: usize = 61;
        let valid = multiband_raster();
        let mut offline = valid.clone();
        offline[HEADER_BYTES] |= 0x80;
        let mut reserved = valid.clone();
        reserved[HEADER_BYTES] |= 0x10;
        let mut unknown = valid.clone();
        unknown[HEADER_BYTES] = (unknown[HEADER_BYTES] & 0xf0) | 9;

        for (value, reason) in [
            (offline, "offline raster bands cannot enter residency"),
            (reserved, "raster WKB contains invalid band flags"),
            (unknown, "raster WKB contains an unknown pixel type"),
        ] {
            let mut builder = RasterColumnBuilder::new(raster_catalog_identity(), 1_024);
            let error = builder
                .push_value(Some(&value))
                .expect_err("unsupported raster must decline during staging");
            assert!(error.contains(reason), "unexpected decline: {error}");
            assert!(builder.rows.is_empty());
            assert_eq!(builder.band_offsets, vec![0]);
            assert_eq!(builder.exact_offsets, vec![0]);
        }

        let mut capped = RasterColumnBuilder::new(raster_catalog_identity(), valid.len() - 1);
        let error = capped
            .push_value(Some(&valid))
            .expect_err("oversized exact value must decline during staging");
        assert!(error.contains("exceeding the per-value limit"));
        assert!(capped.rows.is_empty());
    }

    #[test]
    fn raw_extension_datum_wrappers_have_exact_null_and_roundtrip_contracts() {
        let h3_bits = 0xfedc_ba98_7654_3210_u64;
        let h3 = RawH3Datum(h3_bits);
        assert_eq!(
            h3.into_datum().map(pg_sys::Datum::value),
            Some(h3_bits as usize)
        );
        assert_eq!(RawH3Datum::type_oid(), pg_sys::InvalidOid);
        assert!(RawH3Datum::is_compatible_with(pg_sys::Oid::from(99)));

        let mut geometry = ResidentGeometryBuilder::new(1024, 16);
        // SAFETY: a true null flag prevents any dereference of the sentinel Datum.
        unsafe { stage_geometry_datum(&mut geometry, pg_sys::Datum::from(1_usize), true) }
            .expect("NULL geometry does not inspect sentinel");
        assert_eq!(geometry.finish().expect("NULL column").nulls, Some(vec![1]));

        let raster_datum = pg_sys::Datum::from(0x1234_usize);
        // SAFETY: RawRasterDatum only checks the by-value payload for this test.
        let raster = unsafe {
            RawRasterDatum::from_polymorphic_datum(
                raster_datum,
                false,
                raster_catalog_identity().raster_type_oid,
            )
        }
        .expect("nonzero present raster Datum is retained");
        assert_eq!(raster.into_datum(), Some(raster_datum));
        // SAFETY: null and zero payloads are rejected before any catalog access.
        assert!(
            unsafe {
                RawRasterDatum::from_polymorphic_datum(
                    raster_datum,
                    true,
                    raster_catalog_identity().raster_type_oid,
                )
            }
            .is_none()
        );
        assert!(
            unsafe {
                RawRasterDatum::from_polymorphic_datum(
                    pg_sys::Datum::from(0_usize),
                    false,
                    raster_catalog_identity().raster_type_oid,
                )
            }
            .is_none()
        );
        assert_eq!(RawRasterDatum::type_oid(), pg_sys::InvalidOid);
        assert!(RawRasterDatum::is_compatible_with(pg_sys::Oid::from(99)));
    }

    #[test]
    fn every_builtin_builder_stages_values_nulls_and_exact_byte_counts() {
        let mut boolean = ColumnBuilder::for_type(pg_sys::BOOLOID).expect("bool builder");
        let mut int2 = ColumnBuilder::for_type(pg_sys::INT2OID).expect("int2 builder");
        let mut int4 = ColumnBuilder::for_type(pg_sys::INT4OID).expect("int4 builder");
        let mut date = ColumnBuilder::for_type(pg_sys::DATEOID).expect("date builder");
        let mut int8 = ColumnBuilder::for_type(pg_sys::INT8OID).expect("int8 builder");
        let mut timestamp =
            ColumnBuilder::for_type(pg_sys::TIMESTAMPOID).expect("timestamp builder");
        let mut timestamptz =
            ColumnBuilder::for_type(pg_sys::TIMESTAMPTZOID).expect("timestamptz builder");
        let mut float4 = ColumnBuilder::for_type(pg_sys::FLOAT4OID).expect("float4 builder");
        let mut float8 = ColumnBuilder::for_type(pg_sys::FLOAT8OID).expect("float8 builder");
        let mut text = ColumnBuilder::for_type(pg_sys::TEXTOID).expect("text builder");
        let varchar = ColumnBuilder::for_type(pg_sys::VARCHAROID).expect("varchar builder");
        let bpchar = ColumnBuilder::for_type(pg_sys::BPCHAROID).expect("bpchar builder");

        for builder in [
            &mut boolean,
            &mut int2,
            &mut int4,
            &mut date,
            &mut int8,
            &mut timestamp,
            &mut timestamptz,
            &mut float4,
            &mut float8,
            &mut text,
        ] {
            builder.try_reserve(3).expect("small host reserve succeeds");
        }
        assert_eq!(boolean.type_oid(), pg_sys::BOOLOID);
        assert_eq!(int2.type_oid(), pg_sys::INT2OID);
        assert_eq!(int4.type_oid(), pg_sys::INT4OID);
        assert_eq!(date.type_oid(), pg_sys::DATEOID);
        assert_eq!(int8.type_oid(), pg_sys::INT8OID);
        assert_eq!(timestamp.type_oid(), pg_sys::TIMESTAMPOID);
        assert_eq!(timestamptz.type_oid(), pg_sys::TIMESTAMPTZOID);
        assert_eq!(float4.type_oid(), pg_sys::FLOAT4OID);
        assert_eq!(float8.type_oid(), pg_sys::FLOAT8OID);
        assert_eq!(text.type_oid(), pg_sys::TEXTOID);
        assert_eq!(varchar.type_oid(), pg_sys::VARCHAROID);
        assert_eq!(bpchar.type_oid(), pg_sys::BPCHAROID);

        // SAFETY: all present Datums are by-value values matching their builder;
        // every sentinel payload paired with is_null=true is never interpreted.
        unsafe {
            boolean
                .push_datum(true.into_datum().expect("bool Datum"), false, 1)
                .expect("bool value stages");
            boolean
                .push_datum(pg_sys::Datum::from(0_usize), true, 1)
                .expect("bool null stages");
            int2.push_datum((-7_i16).into_datum().expect("int2 Datum"), false, 2)
                .expect("int2 value stages");
            int4.push_datum((-17_i32).into_datum().expect("int4 Datum"), false, 3)
                .expect("int4 value stages");
            date.push_datum(pg_sys::Datum::from(0_usize), true, 4)
                .expect("date null stages");
            int8.push_datum((-33_i64).into_datum().expect("int8 Datum"), false, 5)
                .expect("int8 value stages");
            timestamp
                .push_datum(pg_sys::Datum::from(0_usize), true, 6)
                .expect("timestamp null stages");
            timestamptz
                .push_datum(pg_sys::Datum::from(0_usize), true, 7)
                .expect("timestamptz null stages");
            float4
                .push_datum(1.5_f32.into_datum().expect("float4 Datum"), false, 8)
                .expect("float4 value stages");
            float8
                .push_datum((-2.25_f64).into_datum().expect("float8 Datum"), false, 9)
                .expect("float8 value stages");
            text.push_datum(pg_sys::Datum::from(0_usize), true, 10)
                .expect("text null stages");
        }

        let StagedColumn::Bool { values, nulls, .. } = boolean.finish().expect("bool finishes")
        else {
            panic!("bool builder changed representation");
        };
        assert_eq!(values, vec![1, 0]);
        assert_eq!(nulls, Some(vec![0, 1]));
        let bool_stage = StagedColumn::Bool {
            type_oid: pg_sys::BOOLOID,
            values,
            nulls,
        };
        assert_eq!(bool_stage.device_bytes(), Ok(4));
        assert_eq!(
            bool_stage.accounting().map(|value| value.device_bytes),
            Ok(4)
        );

        let StagedColumn::I32 { values, nulls, .. } = int2.finish().expect("int2 finishes") else {
            panic!("int2 builder changed representation");
        };
        assert_eq!(values, vec![-7]);
        assert_eq!(nulls, None);
        assert_eq!(
            StagedColumn::I32 {
                type_oid: pg_sys::INT2OID,
                values,
                nulls,
            }
            .device_bytes(),
            Ok(4)
        );

        let StagedColumn::I32 { values, nulls, .. } = int4.finish().expect("int4 finishes") else {
            panic!("int4 builder changed representation");
        };
        assert_eq!(values, vec![-17]);
        assert_eq!(nulls, None);
        let StagedColumn::I32 { values, nulls, .. } = date.finish().expect("date finishes") else {
            panic!("date builder changed representation");
        };
        assert_eq!(values, vec![0]);
        assert_eq!(nulls, Some(vec![1]));

        let StagedColumn::I64 { values, nulls, .. } = int8.finish().expect("int8 finishes") else {
            panic!("int8 builder changed representation");
        };
        assert_eq!(values, vec![-33]);
        assert_eq!(nulls, None);
        for staged in [timestamp.finish(), timestamptz.finish()] {
            let StagedColumn::I64 { values, nulls, .. } = staged.expect("time builder finishes")
            else {
                panic!("time builder changed representation");
            };
            assert_eq!(values, vec![0]);
            assert_eq!(nulls, Some(vec![1]));
        }

        let StagedColumn::F32 { values, nulls, .. } = float4.finish().expect("float4 finishes")
        else {
            panic!("float4 builder changed representation");
        };
        assert_eq!(values, vec![1.5]);
        assert_eq!(nulls, None);
        assert_eq!(
            StagedColumn::F32 {
                type_oid: pg_sys::FLOAT4OID,
                values,
                nulls,
            }
            .device_bytes(),
            Ok(4)
        );
        let StagedColumn::F64 { values, nulls, .. } = float8.finish().expect("float8 finishes")
        else {
            panic!("float8 builder changed representation");
        };
        assert_eq!(values, vec![-2.25]);
        assert_eq!(nulls, None);
        assert_eq!(
            StagedColumn::F64 {
                type_oid: pg_sys::FLOAT8OID,
                values,
                nulls,
            }
            .device_bytes(),
            Ok(8)
        );

        let StagedColumn::TextDictionary {
            codes,
            nulls,
            labels,
            ..
        } = text.finish().expect("text finishes")
        else {
            panic!("text builder changed representation");
        };
        assert_eq!(codes, vec![0]);
        assert_eq!(nulls, Some(vec![1]));
        assert!(labels.is_empty());
        assert_eq!(
            StagedColumn::TextDictionary {
                type_oid: pg_sys::TEXTOID,
                codes,
                nulls,
                labels,
            }
            .device_bytes(),
            Ok(5)
        );
    }

    #[test]
    fn extension_builders_and_empty_columns_preserve_type_and_accounting() {
        let geometry_oid = pg_sys::Oid::from(70_001);
        let raster_oid = raster_catalog_identity().raster_type_oid;
        let mut geometry = ColumnBuilder::Geometry {
            type_oid: geometry_oid,
            builder: ResidentGeometryBuilder::new(1_024, 128),
        };
        let mut raster =
            ColumnBuilder::Raster(RasterColumnBuilder::new(raster_catalog_identity(), 1_024));
        let mut h3 = ColumnBuilder::H3 {
            type_oid: pg_sys::Oid::from(50_001),
            values: Vec::new(),
            nulls: Vec::new(),
            saw_null: false,
        };
        geometry.try_reserve(2).expect("geometry reserve");
        raster.try_reserve(2).expect("raster reserve");
        h3.try_reserve(2).expect("H3 reserve");
        assert_eq!(geometry.type_oid(), geometry_oid);
        assert_eq!(raster.type_oid(), raster_oid);
        assert_eq!(h3.type_oid(), pg_sys::Oid::from(50_001));

        // SAFETY: all three null payloads are rejected before any Datum access;
        // the raster builder carries a synthetic but internally coherent catalog identity.
        unsafe {
            geometry
                .push_datum(pg_sys::Datum::from(0_usize), true, 1)
                .expect("NULL geometry stages");
            raster
                .push_datum(pg_sys::Datum::from(0_usize), true, 2)
                .expect("NULL raster stages");
            h3.push_datum(pg_sys::Datum::from(0_usize), true, 3)
                .expect("NULL H3 stages");
        }
        let geometry = geometry.finish().expect("geometry finishes");
        let ColumnBuilder::Raster(raster) = raster else {
            panic!("raster builder changed representation");
        };
        let raster = raster
            .finish_for_test()
            .expect("synthetic raster catalog finishes without backend revalidation");
        let h3 = h3.finish().expect("H3 finishes");
        assert_eq!(
            geometry.accounting().map(|value| value.device_bytes),
            Ok(73)
        );
        assert_eq!(raster.accounting().map(|value| value.device_bytes), Ok(81));
        assert_eq!(h3.device_bytes(), Ok(9));

        for type_oid in [
            pg_sys::BOOLOID,
            pg_sys::INT2OID,
            pg_sys::INT4OID,
            pg_sys::DATEOID,
            pg_sys::INT8OID,
            pg_sys::TIMESTAMPOID,
            pg_sys::TIMESTAMPTZOID,
            pg_sys::FLOAT4OID,
            pg_sys::FLOAT8OID,
            pg_sys::TEXTOID,
            pg_sys::VARCHAROID,
            pg_sys::BPCHAROID,
        ] {
            let empty = ColumnBuilder::for_type(type_oid)
                .expect("builtin builder")
                .finish_empty()
                .expect("empty builtin finishes");
            assert_eq!(empty.device_bytes(), Ok(0));
            assert!(
                matches!(empty, StagedColumn::Empty { type_oid: actual } if actual == type_oid)
            );
        }

        let empty_geometry = ColumnBuilder::Geometry {
            type_oid: geometry_oid,
            builder: ResidentGeometryBuilder::new(1_024, 128),
        }
        .finish_empty()
        .expect("empty geometry still builds its structural domain");
        assert!(matches!(
            empty_geometry,
            StagedColumn::Geometry { type_oid, .. } if type_oid == geometry_oid
        ));
    }

    #[test]
    fn text_dictionary_is_sorted_deduplicated_and_null_aware() {
        let mut builder = TextColumnBuilder::default();
        for value in [Some("zeta"), None, Some("alpha"), Some("zeta")] {
            builder.push(value).expect("intern label");
        }
        let staged = builder
            .finish(pg_sys::TEXTOID)
            .expect("text dictionary builds");
        let StagedColumn::TextDictionary {
            codes,
            nulls,
            labels,
            ..
        } = staged
        else {
            panic!("text builder changed representation");
        };
        assert_eq!(labels, vec!["alpha", "zeta"]);
        assert_eq!(codes, vec![1, 0, 0, 1]);
        assert_eq!(nulls, Some(vec![0, 1, 0, 0]));
    }

    #[test]
    fn incremental_text_dictionary_matches_sorted_reference_and_owns_labels() {
        for type_oid in [pg_sys::TEXTOID, pg_sys::VARCHAROID, pg_sys::BPCHAROID] {
            let mut builder = TextColumnBuilder::default();
            builder.try_reserve(100_000).expect("reserve rows");
            let labels = ["", "z", "a", "a ", "a  ", "é", "e\u{301}", "東京", "😀"];
            let mut input = Vec::new();
            for row in 0..100_000 {
                let value = (row % 17 != 0).then(|| labels[(row * 13) % labels.len()].to_owned());
                builder
                    .push(value.as_deref())
                    .expect("intern borrowed text");
                // The builder cannot retain the temporary String's address.
                input.push(value.clone());
            }
            assert_eq!(builder.by_label.len(), labels.len());
            assert_eq!(builder.codes.len(), input.len());
            let expected_labels = input
                .iter()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let StagedColumn::TextDictionary {
                codes,
                nulls,
                labels,
                ..
            } = builder.finish(type_oid).expect("finish dictionary")
            else {
                panic!("dictionary representation")
            };
            assert_eq!(labels, expected_labels);
            let nulls = nulls.expect("NULL sidecar");
            for (row, value) in input.iter().enumerate() {
                assert_eq!(nulls[row], u8::from(value.is_none()));
                assert_eq!(
                    codes[row],
                    value.as_ref().map_or(0, |value| i32::try_from(
                        expected_labels.binary_search(value).expect("label")
                    )
                    .expect("code"))
                );
            }
        }
    }

    #[test]
    fn incremental_text_dictionary_empty_null_and_capacity_boundaries() {
        for values in [vec![], vec![None, None], vec![Some("same"), Some("same")]] {
            let mut builder = TextColumnBuilder::default();
            for value in &values {
                builder.push(*value).expect("row");
            }
            let StagedColumn::TextDictionary {
                codes,
                nulls,
                labels,
                ..
            } = builder.finish(pg_sys::TEXTOID).expect("finish")
            else {
                panic!("dictionary representation")
            };
            assert_eq!(codes, vec![0; values.len()]);
            assert_eq!(nulls.is_some(), values.iter().any(Option::is_none));
            assert_eq!(
                labels.len(),
                usize::from(values.iter().any(Option::is_some))
            );
        }
        assert_eq!(next_text_dictionary_code(0), Ok(0));
        assert_eq!(
            next_text_dictionary_code(i32::MAX as usize - 1),
            Ok(i32::MAX - 1)
        );
        assert!(next_text_dictionary_code(i32::MAX as usize).is_err());
        assert!(next_text_dictionary_code(usize::MAX).is_err());
    }

    #[test]
    fn representation_evidence_accounts_retained_text_and_lossless_projections() {
        let boolean = StagedColumn::Bool {
            type_oid: pg_sys::BOOLOID,
            values: vec![0; 9],
            nulls: Some(vec![0; 9]),
        };
        let boolean = boolean
            .representation_evidence()
            .expect("bool evidence is bounded");
        assert_eq!(boolean.bool_value_bytes, 9);
        assert_eq!(boolean.projected_bool_bitmap_bytes, 2);
        assert_eq!(boolean.null_sidecar_bytes, 9);
        assert_eq!(boolean.projected_null_bitmap_bytes, 2);
        assert_eq!(boolean.transient_staging_bytes, 18);
        assert_eq!(boolean.construction_peak_bytes, 36);

        let integer = StagedColumn::I32 {
            type_oid: pg_sys::INT4OID,
            values: vec![10, 11, 12, 13],
            nulls: None,
        }
        .representation_evidence()
        .expect("integer evidence is bounded");
        assert_eq!(integer.source_logical_bytes, 16);
        assert_eq!(integer.integer_value_bytes, 16);
        // Two range bits per row plus one exact int32 base value.
        assert_eq!(integer.projected_integer_bitpack_bytes, 5);
        // One block: exact min/max int32 values plus NULL/special flags.
        assert_eq!(integer.projected_block_minmax_bytes, 10);

        let text = StagedColumn::TextDictionary {
            type_oid: pg_sys::TEXTOID,
            codes: vec![1, 0, 0, 1],
            nulls: Some(vec![0, 1, 0, 0]),
            labels: vec!["alpha".to_owned(), "zeta".to_owned()],
        };
        let accounting = text.accounting().expect("text accounting is bounded");
        assert_eq!(accounting.device_bytes, 20);
        assert_eq!(
            accounting.retained_host_exact_bytes,
            u64::try_from(2 * std::mem::size_of::<String>() + 9).expect("small host charge")
        );
        let text = text
            .representation_evidence()
            .expect("text evidence is bounded");
        assert_eq!(text.source_logical_bytes, 25);
        assert_eq!(text.dictionary_code_bytes, 16);
        assert_eq!(text.projected_dictionary_bitpack_bytes, 1);
        assert_eq!(text.null_sidecar_bytes, 4);
        assert_eq!(text.projected_null_bitmap_bytes, 1);
    }
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    struct TestSnapshotGuard;

    impl Drop for TestSnapshotGuard {
        fn drop(&mut self) {
            // SAFETY: stage_for_test pushes exactly one copied snapshot.
            unsafe { pg_sys::PopActiveSnapshot() };
        }
    }

    fn stage_for_test(
        relid: pg_sys::Oid,
        requests: &[ColumnRequest],
    ) -> Result<StagedRelation, String> {
        // SAFETY: pg_test runs on the backend main thread with an active outer
        // snapshot. A private copy may be updated to the current SPI command ID.
        unsafe {
            pg_sys::PushCopiedSnapshot(pg_sys::GetActiveSnapshot());
            pg_sys::UpdateActiveSnapshotCommandId();
        }
        let _snapshot = TestSnapshotGuard;
        stage_relation(relid, requests)
    }

    fn relation_oid(name: &str) -> pg_sys::Oid {
        Spi::get_one::<pg_sys::Oid>(&format!("SELECT '{name}'::regclass::oid"))
            .expect("relation OID lookup succeeds")
            .expect("relation exists")
    }

    fn caught_error_code(caught: &pg_sys::panic::CaughtError) -> PgSqlErrorCode {
        use pg_sys::panic::CaughtError;
        match caught {
            CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
                error.sql_error_code()
            }
            CaughtError::RustPanic { ereport, .. } => ereport.sql_error_code(),
        }
    }

    #[pg_test]
    fn direct_geometry_staging_borrows_flat_compressed_and_external_values() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS postgis; \
             CREATE TABLE pgaccel_geometry_staging(p geometry, c geometry, e geometry); \
             ALTER TABLE pgaccel_geometry_staging ALTER COLUMN p SET STORAGE PLAIN; \
             ALTER TABLE pgaccel_geometry_staging ALTER COLUMN c SET STORAGE MAIN; \
             ALTER TABLE pgaccel_geometry_staging ALTER COLUMN e SET STORAGE EXTERNAL; \
             INSERT INTO pgaccel_geometry_staging \
             SELECT ST_SetSRID(ST_MakePoint(-0.0,1.25),4326), \
                    ST_MakeLine(ST_SetSRID(ST_MakePoint(1,2),4326) ORDER BY i), \
                    ST_MakeLine(ST_SetSRID(ST_MakePoint(i,sin(i)),4326) ORDER BY i) \
             FROM generate_series(1,2048) s(i); \
             INSERT INTO pgaccel_geometry_staging VALUES \
                (NULL,NULL,NULL), ('SRID=4326;POINT EMPTY', \
                                  'SRID=4326;LINESTRING EMPTY', \
                                  'SRID=4326;POLYGON EMPTY')",
        )
        .expect("create flat/compressed/external geometry fixture");
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pg_column_size(c)<2000 AND pg_column_size(e)>32000 \
                 AND pg_relation_size((SELECT reltoastrelid FROM pg_class \
                   WHERE oid='pgaccel_geometry_staging'::regclass))>0 \
                 FROM pgaccel_geometry_staging WHERE p IS NOT NULL AND NOT ST_IsEmpty(p)"
            )
            .expect("verify compressed and out-of-line fixture"),
            Some(true)
        );
        let relid = relation_oid("pgaccel_geometry_staging");
        ensure_invalidation_trigger(relid).expect("install generation tracking");
        let requests = resolve_attnos(relid, &[1, 2, 3]).expect("resolve geometry columns");
        // The independent expected bytes use the old owned-copy boundary.
        let expected = Spi::connect(|client| {
            let mut rows = client
                .select("SELECT p,c,e FROM pgaccel_geometry_staging", None, &[])
                .expect("read exact source bytes");
            let mut expected = Vec::new();
            while rows.next().is_some() {
                expected.push(
                    (1..=3)
                        .map(|column| {
                            rows.get_datum_by_ordinal(column)
                                .expect("geometry field")
                                .map(|datum| {
                                    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
                                    // SAFETY: live non-NULL geometry owned by this SPI row.
                                    let flat = unsafe { pg_sys::pg_detoast_datum(original) };
                                    assert!(!flat.is_null());
                                    // SAFETY: flat contains its complete four-byte-header payload.
                                    let bytes = unsafe {
                                        std::slice::from_raw_parts(
                                            flat.cast::<u8>(),
                                            pgrx::varsize(flat.cast()),
                                        )
                                        .to_vec()
                                    };
                                    if flat != original {
                                        // SAFETY: distinct detoast copy; bytes now own the payload.
                                        unsafe { pg_sys::pfree(flat.cast()) };
                                    }
                                    bytes
                                })
                        })
                        .collect::<Vec<_>>(),
                );
            }
            expected
        });
        let staged = stage_for_test(relid, &requests).expect("borrowed geometry table scan");
        assert_eq!(staged.row_count(), 3);
        Spi::run("DROP TABLE pgaccel_geometry_staging").expect("release source table and toast");
        for (column, request) in requests.iter().enumerate() {
            let StagedColumn::Geometry { data, .. } =
                staged.columns.get(&request.attno).expect("column")
            else {
                panic!("expected geometry column");
            };
            assert_eq!(data.nulls.as_deref(), Some([0, 1, 0].as_slice()));
            for (row, values) in expected.iter().enumerate() {
                assert_eq!(
                    data.exact.value(row),
                    Some(values[column].as_deref().unwrap_or(&[]))
                );
            }
            assert_eq!(data.coordinates.len(), if column == 0 { 2 } else { 4096 });
        }
        // A non-NULL unsupported geometry is still rejected, including when
        // rejection occurs after detoasting an out-of-line value.
        Spi::run(
            "CREATE TABLE pgaccel_geometry_invalid(g geometry); \
             ALTER TABLE pgaccel_geometry_invalid ALTER COLUMN g SET STORAGE EXTERNAL; \
             INSERT INTO pgaccel_geometry_invalid SELECT \
               ST_MakeLine(ST_MakePoint(i,sin(i),i) ORDER BY i) \
               FROM generate_series(1,2048) s(i)",
        )
        .expect("create toasted unsupported 3D geometry");
        let invalid = relation_oid("pgaccel_geometry_invalid");
        ensure_invalidation_trigger(invalid).expect("invalid fixture generation");
        let requests = resolve_attnos(invalid, &[1]).expect("invalid fixture columns");
        for _ in 0..16 {
            let error = match stage_for_test(invalid, &requests) {
                Ok(_) => panic!("3D geometry must not stage"),
                Err(error) => error,
            };
            assert!(
                error.contains("not a supported non-geodetic 2D layout"),
                "{error}"
            );
        }
        Spi::run("DROP TABLE pgaccel_geometry_invalid").expect("clean invalid fixture");
    }

    #[pg_test]
    fn resident_representation_and_ingestion_evidence_is_exact_and_queryable() {
        Spi::run(
            "CREATE TABLE pgaccel_representation_evidence(flag bool, n int4, label text); \
             INSERT INTO pgaccel_representation_evidence \
             SELECT CASE WHEN g = 3 THEN NULL ELSE g % 2 = 0 END, \
                    CASE WHEN g = 4 THEN NULL ELSE 10 + g % 4 END, \
                    CASE WHEN g = 5 THEN NULL WHEN g % 2 = 0 THEN 'alpha' ELSE 'zeta' END \
             FROM generate_series(1, 9) AS g; \
             SELECT pg_accel_pin('pgaccel_representation_evidence'::regclass, \
                                 ARRAY['flag', 'n', 'label'])",
        )
        .expect("load representation fixture");

        let exact = Spi::get_one::<bool>(
            "SELECT r.row_count = 9 \
                    AND r.encoded_device_bytes + r.retained_exact_bytes = s.raw_bytes \
                    AND r.transient_staging_bytes = s.raw_bytes \
                    AND r.construction_peak_bytes = r.transient_staging_bytes + r.encoded_device_bytes \
                    AND r.null_sidecar_bytes = 27 \
                    AND r.projected_null_bitmap_bytes = 6 \
                    AND r.bool_value_bytes = 9 \
                    AND r.projected_bool_bitmap_bytes = 2 \
                    AND r.integer_value_bytes = 36 \
                    AND r.projected_integer_bitpack_bytes < r.integer_value_bytes \
                    AND r.dictionary_code_bytes = 36 \
                    AND r.projected_dictionary_bitpack_bytes = 2 \
                    AND r.retained_exact_bytes > 0 \
             FROM pg_accel_resident_representation_status() AS r \
             JOIN pg_accel_resident_status() AS s USING (relid) \
             WHERE r.relid = 'pgaccel_representation_evidence'::regclass",
        )
        .expect("query representation evidence")
        .expect("loaded relation has representation evidence");
        assert!(exact, "representation evidence or ledger identity changed");

        let ingestion = Spi::get_one::<bool>(
            "SELECT scan_batches = 1 \
                    AND preflight_ms >= 0 AND scan_ms >= 0 \
                    AND encode_ms >= 0 AND copy_ms >= 0 \
                    AND total_load_ms + 0.000001 >= preflight_ms + scan_ms + encode_ms + copy_ms \
                    AND scan_strategy = 'table_am_single_deform_batches_8192' \
                    AND publication_strategy = 'synchronous_exact_after_copy' \
             FROM pg_accel_resident_ingestion_status() \
             WHERE relid = 'pgaccel_representation_evidence'::regclass",
        )
        .expect("query ingestion evidence")
        .expect("loaded relation has ingestion evidence");
        assert!(ingestion, "ingestion phase evidence changed");
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM pg_accel_resident_representation_decisions()"
            )
            .expect("query representation decisions"),
            Some(5)
        );

        Spi::run(
            "SELECT pg_accel_evict('pgaccel_representation_evidence'::regclass); \
             DROP TABLE pgaccel_representation_evidence",
        )
        .expect("clean representation fixture");
    }

    #[pg_test]
    fn direct_loader_acl_rls_mvcc_primitives_and_cold_timing() {
        Spi::run(
            "CREATE ROLE pgaccel_loader_reader; \
             CREATE TABLE pgaccel_loader_acl (allowed int4, denied int4); \
             INSERT INTO pgaccel_loader_acl VALUES (1, 2); \
             REVOKE ALL ON pgaccel_loader_acl FROM PUBLIC; \
             GRANT SELECT (allowed) ON pgaccel_loader_acl TO pgaccel_loader_reader",
        )
        .expect("create ACL fixture");
        let acl_relid = relation_oid("pgaccel_loader_acl");
        ensure_invalidation_trigger(acl_relid).expect("install ACL fixture invalidation trigger");
        let allowed = resolve_attnos(acl_relid, &[1]).expect("resolve allowed column");
        let denied = resolve_attnos(acl_relid, &[2]).expect("resolve denied column");
        Spi::run("SET ROLE pgaccel_loader_reader").expect("assume restricted role");
        let allowed_result = stage_for_test(acl_relid, &allowed);
        let denied_result = stage_for_test(acl_relid, &denied);
        let count_only_result = stage_for_test(acl_relid, &[]);
        Spi::run("RESET ROLE").expect("restore test role");
        assert_eq!(
            allowed_result
                .expect("per-column SELECT permits the requested projection")
                .row_count(),
            1
        );
        let denied_error = match denied_result {
            Ok(_) => panic!("missing column grant must reject load"),
            Err(error) => error,
        };
        assert!(
            denied_error.contains("ACL/type preflight"),
            "unexpected ACL rejection: {denied_error}"
        );
        assert_eq!(
            count_only_result
                .expect("SELECT on any column permits a count-only projection")
                .row_count(),
            1
        );

        Spi::run("GRANT SELECT ON pgaccel_loader_acl TO pgaccel_loader_reader")
            .expect("grant table SELECT");
        Spi::run("SET ROLE pgaccel_loader_reader").expect("assume table reader role");
        let table_grant_result = stage_for_test(acl_relid, &denied);
        Spi::run("RESET ROLE").expect("restore test role after table grant");
        assert_eq!(
            table_grant_result
                .expect("table SELECT permits every requested projection")
                .row_count(),
            1
        );

        Spi::run("ALTER TABLE pgaccel_loader_acl ENABLE ROW LEVEL SECURITY").expect("enable RLS");
        let rls_error = match stage_for_test(acl_relid, &allowed) {
            Ok(_) => panic!("RLS must reject residency"),
            Err(error) => error,
        };
        assert!(
            rls_error.contains("row-level security enabled"),
            "unexpected RLS rejection: {rls_error}"
        );

        Spi::run(
            "SET TIME ZONE 'UTC'; \
             CREATE TABLE pgaccel_loader_values ( \
               b bool, i2 int2, i4 int4, i8 int8, f4 float4, f8 float8, \
               d date, ts timestamp, tstz timestamptz, t text); \
             INSERT INTO pgaccel_loader_values VALUES \
               (true, -2, 17, -9000000000, 1.25, -2.5, \
                DATE '2000-01-02', TIMESTAMP '2000-01-01 00:00:01', \
                TIMESTAMPTZ '2000-01-01 00:00:02+00', 'alpha'), \
               (NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL), \
               (false, 9, 999, 9, 9, 9, DATE '2020-01-01', \
                TIMESTAMP '2020-01-01', TIMESTAMPTZ '2020-01-01+00', 'deleted'); \
             DELETE FROM pgaccel_loader_values WHERE i4 = 999",
        )
        .expect("create MVCC/primitive fixture");
        let values_relid = relation_oid("pgaccel_loader_values");
        ensure_invalidation_trigger(values_relid)
            .expect("install primitive fixture invalidation trigger");
        let requests = resolve_attnos(values_relid, &(1_i16..=10).collect::<Vec<_>>())
            .expect("resolve primitive columns");
        let staged = stage_for_test(values_relid, &requests).expect("direct primitive load");
        assert_eq!(staged.row_count(), 2, "dead tuple must be MVCC-invisible");
        macro_rules! assert_nullable_column {
            ($attno:expr, $variant:ident, $expected:expr) => {
                match staged.columns.get(&$attno).expect("primitive staged") {
                    StagedColumn::$variant { values, nulls, .. } => {
                        assert_eq!(values.as_slice(), $expected);
                        assert_eq!(nulls.as_deref(), Some(&[0, 1][..]));
                    }
                    _ => panic!("primitive staging representation changed"),
                }
            };
        }
        assert_nullable_column!(1, Bool, &[1_u8, 0]);
        assert_nullable_column!(2, I32, &[-2_i32, 0]);
        assert_nullable_column!(3, I32, &[17_i32, 0]);
        assert_nullable_column!(4, I64, &[-9_000_000_000_i64, 0]);
        assert_nullable_column!(5, F32, &[1.25_f32, 0.0]);
        assert_nullable_column!(6, F64, &[-2.5_f64, 0.0]);
        assert_nullable_column!(7, I32, &[1_i32, 0]);
        assert_nullable_column!(8, I64, &[1_000_000_i64, 0]);
        assert_nullable_column!(9, I64, &[2_000_000_i64, 0]);
        match staged.columns.get(&10).expect("text staged") {
            StagedColumn::TextDictionary {
                codes,
                nulls,
                labels,
                ..
            } => {
                assert_eq!(codes, &[0, 0]);
                assert_eq!(nulls.as_deref(), Some(&[0, 1][..]));
                assert_eq!(labels, &["alpha"]);
            }
            _ => panic!("text staging representation changed"),
        }

        let drops_before_error = TEST_DIRECT_SCAN_DROP_COUNT.with(std::cell::Cell::get);
        TEST_DIRECT_SCAN_ERROR_AFTER_ROWS.with(|trigger| {
            trigger.set(Some((1, TestDirectScanError::Recoverable)));
        });
        let injected = match stage_for_test(values_relid, &requests) {
            Ok(_) => panic!("injected scan ERROR must not publish a staged relation"),
            Err(error) => error,
        };
        assert!(injected.contains("injected resident direct scan error"));
        assert_eq!(
            TEST_DIRECT_SCAN_DROP_COUNT.with(std::cell::Cell::get),
            drops_before_error + 1,
            "caught scan ERROR must drop scan, slot, and relation guard exactly once"
        );

        let drops_before_cancel = TEST_DIRECT_SCAN_DROP_COUNT.with(std::cell::Cell::get);
        TEST_DIRECT_SCAN_ERROR_AFTER_ROWS.with(|trigger| {
            trigger.set(Some((1, TestDirectScanError::QueryCanceled)));
        });
        let cancellation = pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
            stage_for_test(values_relid, &requests)
                .expect("query cancellation must rethrow instead of returning a loader error");
            None
        }))
        .catch_others(|caught| Some(caught_error_code(&caught)))
        .execute();
        assert_eq!(
            cancellation,
            Some(PgSqlErrorCode::ERRCODE_QUERY_CANCELED),
            "cancellation must retain SQLSTATE 57014"
        );
        assert_eq!(
            TEST_DIRECT_SCAN_DROP_COUNT.with(std::cell::Cell::get),
            drops_before_cancel + 1,
            "rethrowing cancellation must still drop direct scan resources"
        );
        assert_eq!(
            stage_for_test(values_relid, &requests)
                .expect("scan must remain reusable after caught errors")
                .row_count(),
            2
        );

        Spi::run(
            "CREATE UNLOGGED TABLE pgaccel_loader_bench (g int4 NOT NULL, v int4); \
             INSERT INTO pgaccel_loader_bench \
             SELECT (i % 17)::int4, CASE WHEN i % 127 = 0 THEN NULL ELSE (i % 997)::int4 END \
             FROM generate_series(1, 262144) AS i; \
             ANALYZE pgaccel_loader_bench",
        )
        .expect("create cold-load fixture");
        let bench_relid = relation_oid("pgaccel_loader_bench");
        ensure_invalidation_trigger(bench_relid)
            .expect("install cold-load fixture invalidation trigger");
        let bench_requests = resolve_attnos(bench_relid, &[1, 2]).expect("resolve bench columns");
        let wall_started = Instant::now();
        let bench = stage_for_test(bench_relid, &bench_requests).expect("cold direct table load");
        let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(bench.row_count(), 262_144);
        pgrx::notice!(
            "direct resident cold load: 262144 rows x 2 int4 columns: stage={:.3}ms wall={wall_ms:.3}ms",
            bench.load_ms
        );
    }
}

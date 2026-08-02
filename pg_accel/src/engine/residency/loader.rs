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
    RetainedExactValues,
};
use super::geometry::{
    ResidentGeometryBuilder, ResidentGeometryColumn, ResidentGeometryReferencedBytes,
};
use super::ledger::{self, GenerationStamp, LedgerCharge};
use super::store::{
    InvalidationTriggerFingerprint, RelationFingerprint, ResidentColumn, ResidentRelation,
};

const LOAD_INTERRUPT_CHECK_ROWS: usize = 8192;

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

/// Owned detoasted GSERIALIZED bytes. The dynamic type is proved before this
/// wrapper is ever requested from SPI.
struct RawGeometryDatum(Vec<u8>);

impl FromDatum for RawGeometryDatum {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _type_oid: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            return None;
        }
        // SAFETY: SPI supplied a non-NULL Datum of the catalog-proved PostGIS
        // varlena type on the backend main thread.
        let original = datum.cast_mut_ptr::<pg_sys::varlena>();
        // SAFETY: `original` is the non-null catalog-proved geometry varlena
        // supplied by SPI and the conversion runs synchronously on its thread.
        let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
        if detoasted.is_null() {
            return Some(Self(Vec::new()));
        }
        // SAFETY: pg_detoast_datum returned a flat varlena whose complete
        // allocation is readable for VARSIZE bytes during this SPI callback.
        let length = unsafe { pgrx::varsize(detoasted.cast()) };
        // SAFETY: `varsize` proves exactly `length` readable bytes in the flat
        // detoasted allocation; to_vec copies them before any pfree or SPI exit.
        let bytes = unsafe { std::slice::from_raw_parts(detoasted.cast::<u8>(), length) }.to_vec();
        if detoasted != original {
            // SAFETY: a distinct pg_detoast_datum result is a palloc-owned
            // flat copy; the owned Vec above no longer borrows it.
            unsafe { pg_sys::pfree(detoasted.cast()) };
        }
        Some(Self(bytes))
    }
}

impl IntoDatum for RawGeometryDatum {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        None
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::InvalidOid
    }

    fn is_compatible_with(_other: pg_sys::Oid) -> bool {
        true
    }
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

impl StagedColumn {
    fn device_bytes(&self) -> Result<u64, String> {
        let checked = |len: usize, width: usize, nulls: usize| {
            len.checked_mul(width)
                .and_then(|bytes| bytes.checked_add(nulls))
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(|| "resident column byte count overflow".to_owned())
        };
        match self {
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
            _ => Ok(ResidentByteAccounting {
                device_bytes: self.device_bytes()?,
                retained_host_exact_bytes: 0,
            }),
        }
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
            load_ms: self.load_ms,
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
        reserve(self.nulls.try_reserve(additional))?;
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
            rows: self.rows,
            bands: self.bands,
            nulls: self.saw_null.then_some(self.nulls),
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

enum ColumnBuilder {
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
        values: Vec<Option<String>>,
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
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID => false,
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
            Self::Bool { type_oid, .. }
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
                values: Vec::new(),
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
            Self::Bool { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::I32 { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::I64 { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::H3 { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::Geometry { builder, .. } => builder.try_reserve_rows(additional),
            Self::Raster(builder) => builder.reserve_rows(additional),
            Self::F32 { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::F64 { values, nulls, .. } => {
                reserve(values.try_reserve(additional))?;
                reserve(nulls.try_reserve(additional))
            }
            Self::Text { values, .. } => reserve(values.try_reserve(additional)),
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
            Self::Geometry { type_oid, builder } => {
                let value = decode_datum!(RawGeometryDatum, *type_oid);
                builder.push(value.map(|value| value.0))?;
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
            Self::Text { type_oid, values } => {
                let value = decode_datum!(String, *type_oid);
                values.push(value);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<StagedColumn, String> {
        Ok(match self {
            Self::Bool {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::Bool {
                type_oid,
                values,
                nulls: saw_null.then_some(nulls),
            },
            Self::I32 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::I32 {
                type_oid,
                values,
                nulls: saw_null.then_some(nulls),
            },
            Self::I64 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::I64 {
                type_oid,
                values,
                nulls: saw_null.then_some(nulls),
            },
            Self::H3 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::H3 {
                type_oid,
                values,
                nulls: saw_null.then_some(nulls),
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
                values,
                nulls: saw_null.then_some(nulls),
            },
            Self::F64 {
                type_oid,
                values,
                nulls,
                saw_null,
            } => StagedColumn::F64 {
                type_oid,
                values,
                nulls: saw_null.then_some(nulls),
            },
            Self::Text { type_oid, values } => {
                let labels = values
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                if labels.len() > i32::MAX as usize {
                    return Err("text dictionary exceeds int32 code space".to_owned());
                }
                let by_label = labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        (label.as_str(), i32::try_from(index).unwrap_or(i32::MAX))
                    })
                    .collect::<BTreeMap<_, _>>();
                let saw_null = values.iter().any(Option::is_none);
                let mut codes = Vec::with_capacity(values.len());
                let mut nulls = Vec::with_capacity(values.len());
                for value in &values {
                    if let Some(label) = value {
                        codes.push(
                            *by_label
                                .get(label.as_str())
                                .ok_or("text dictionary code disappeared")?,
                        );
                        nulls.push(0);
                    } else {
                        codes.push(0);
                        nulls.push(1);
                    }
                }
                StagedColumn::TextDictionary {
                    type_oid,
                    codes,
                    nulls: saw_null.then_some(nulls),
                    labels,
                }
            }
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
        for (request, builder) in requests.iter().zip(builders.iter_mut()) {
            let mut is_null = false;
            // SAFETY: the slot contains the current row and each positive
            // attno was validated against the relation fingerprint/catalog.
            let datum = unsafe {
                pg_sys::slot_getattr(table.slot, i32::from(request.attno), &raw mut is_null)
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
        let row_count = scan_relation_direct(relid, requests, &mut builders)?;
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
        return Ok(StagedRelation {
            relid,
            relfilenode: relfilenode_before,
            fingerprint: fingerprint_before,
            generation: generation_after,
            columns,
            row_count,
            loaded_at_us: now_us(),
            load_ms: started.elapsed().as_secs_f64() * 1000.0,
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
    // DML which committed immediately before the DDL lock was acquired. Mark
    // the first load as a one-command snapshot and advance again at xact end.
    ledger::note_relation_change(relid);
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

        let geometry = RawGeometryDatum(vec![1, 2, 3]);
        assert_eq!(geometry.into_datum(), None);
        assert_eq!(RawGeometryDatum::type_oid(), pg_sys::InvalidOid);
        assert!(RawGeometryDatum::is_compatible_with(pg_sys::Oid::from(99)));
        // SAFETY: a true null flag prevents any dereference of the sentinel Datum.
        assert!(
            unsafe {
                RawGeometryDatum::from_polymorphic_datum(
                    pg_sys::Datum::from(1_usize),
                    true,
                    pg_sys::Oid::from(99),
                )
            }
            .is_none()
        );

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
        let staged = ColumnBuilder::Text {
            type_oid: pg_sys::TEXTOID,
            values: vec![
                Some("zeta".to_owned()),
                None,
                Some("alpha".to_owned()),
                Some("zeta".to_owned()),
            ],
        }
        .finish()
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

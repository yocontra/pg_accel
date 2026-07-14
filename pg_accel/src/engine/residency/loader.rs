//! Synchronous SPI full-scan loader for residency v2.
//!
//! SPI is deliberate: it preserves PostgreSQL MVCC, permissions, row-security,
//! partition routing, and type conversion semantics. Every loader query runs
//! with pg_accel planner hooks suspended to prevent recursive path injection.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use pgrx::{pg_sys, prelude::*};

use crate::engine::ffi::syscache;
use crate::engine::gucs;
use crate::gpu::ExprDeviceBuffer;

use super::domain::{ResidentByteAccounting, ResidentGeometryData};
use super::geometry::{ResidentGeometryBuilder, ResidentGeometryColumn};
use super::ledger::{self, GenerationStamp, LedgerCharge};
use super::store::{ResidentColumn, ResidentRelation};

const LOAD_INTERRUPT_CHECK_ROWS: usize = 8192;

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
        let detoasted =
            unsafe { pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pg_sys::varlena>()) };
        if detoasted.is_null() {
            return Some(Self(Vec::new()));
        }
        // SAFETY: pg_detoast_datum returned a flat varlena whose complete
        // allocation is readable for VARSIZE bytes during this SPI callback.
        let length = unsafe { pgrx::varsize(detoasted.cast()) };
        let bytes = unsafe { std::slice::from_raw_parts(detoasted.cast::<u8>(), length) };
        Some(Self(bytes.to_vec()))
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
        accounting: ResidentByteAccounting,
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
                max_exact_value_bytes,
                ..
            } => Ok(ResidentColumn::Geometry {
                type_oid,
                data: ResidentGeometryColumn::materialize(data, max_exact_value_bytes, label)?,
            }),
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
            derived: Vec::new(),
        })
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
    // thread. The complete PostGIS function/type fingerprint is revalidated by
    // planner admission; the loader independently proves extension ownership
    // and the exact geometry type before interpreting a varlena payload.
    let is_member = unsafe { syscache::type_is_extension_member(type_oid, "postgis") };
    let name = unsafe { syscache::type_name(type_oid) };
    if !is_member || name.as_deref() != Some("geometry") {
        return Err(format!(
            "type OID {} is not the extension-owned PostGIS geometry type",
            u32::from(type_oid)
        ));
    }
    Ok(())
}

impl ColumnBuilder {
    fn type_oid(&self) -> pg_sys::Oid {
        match self {
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
            _ => {
                if validate_geometry_column_type(type_oid).is_ok() {
                    Ok(Self::Geometry {
                        type_oid,
                        builder: ResidentGeometryBuilder::new(
                            crate::engine::cost::device_limits()
                                .resident_domain_max_exact_value_bytes,
                        ),
                    })
                } else {
                    validate_h3_column_type(type_oid)?;
                    Ok(Self::H3 {
                        type_oid,
                        values: Vec::new(),
                        nulls: Vec::new(),
                        saw_null: false,
                    })
                }
            }
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

    fn push(
        &mut self,
        row: &pgrx::spi::SpiHeapTupleData<'_>,
        ordinal: usize,
    ) -> Result<(), String> {
        match self {
            Self::Bool {
                values,
                nulls,
                saw_null,
                ..
            } => {
                let value = row
                    .get::<bool>(ordinal)
                    .map_err(|error| format!("column {ordinal} bool read failed: {error:?}"))?;
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
                    pg_sys::INT2OID => row.get::<i16>(ordinal).map(|value| value.map(i32::from)),
                    pg_sys::DATEOID => row
                        .get::<Date>(ordinal)
                        .map(|value| value.map(pg_sys::DateADT::from)),
                    _ => row.get::<i32>(ordinal),
                }
                .map_err(|error| format!("column {ordinal} integer read failed: {error:?}"))?;
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
                    pg_sys::TIMESTAMPOID => row
                        .get::<Timestamp>(ordinal)
                        .map(|value| value.map(pg_sys::Timestamp::from)),
                    pg_sys::TIMESTAMPTZOID => row
                        .get::<TimestampWithTimeZone>(ordinal)
                        .map(|value| value.map(pg_sys::TimestampTz::from)),
                    _ => row.get::<i64>(ordinal),
                }
                .map_err(|error| format!("column {ordinal} int8 read failed: {error:?}"))?;
                values.push(value.unwrap_or(0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::H3 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                let value = row
                    .get::<RawH3Datum>(ordinal)
                    .map_err(|error| format!("column {ordinal} h3index read failed: {error:?}"))?;
                values.push(value.map_or(0, |value| value.0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::Geometry { builder, .. } => {
                let value = row
                    .get::<RawGeometryDatum>(ordinal)
                    .map_err(|error| format!("column {ordinal} geometry read failed: {error:?}"))?;
                builder.push(value.map(|value| value.0))?;
            }
            Self::F32 {
                values,
                nulls,
                saw_null,
                ..
            } => {
                let value = row
                    .get::<f32>(ordinal)
                    .map_err(|error| format!("column {ordinal} float4 read failed: {error:?}"))?;
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
                let value = row
                    .get::<f64>(ordinal)
                    .map_err(|error| format!("column {ordinal} float8 read failed: {error:?}"))?;
                values.push(value.unwrap_or(0.0));
                nulls.push(u8::from(value.is_none()));
                *saw_null |= value.is_none();
            }
            Self::Text { values, .. } => {
                values
                    .push(row.get::<String>(ordinal).map_err(|error| {
                        format!("column {ordinal} text read failed: {error:?}")
                    })?);
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
                let accounting = data
                    .accounting(max_exact_value_bytes)
                    .map_err(|error| error.to_string())?;
                StagedColumn::Geometry {
                    type_oid,
                    data,
                    accounting,
                    max_exact_value_bytes,
                }
            }
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

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
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

fn column_names(relid: pg_sys::Oid, requests: &[ColumnRequest]) -> Result<Vec<String>, String> {
    let mut by_attno = BTreeMap::new();
    for (attno, name, type_oid) in relation_columns(relid)? {
        by_attno.insert(attno, (name, type_oid));
    }
    requests
        .iter()
        .map(|request| {
            let (name, actual_type) = by_attno.get(&request.attno).ok_or_else(|| {
                format!(
                    "relation OID {} has no live attribute {}",
                    u32::from(relid),
                    request.attno
                )
            })?;
            if *actual_type != request.type_oid {
                return Err(format!(
                    "relation OID {} attribute {} changed type from OID {} to OID {}",
                    u32::from(relid),
                    request.attno,
                    u32::from(request.type_oid),
                    u32::from(*actual_type)
                ));
            }
            Ok(name.clone())
        })
        .collect()
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

pub(super) fn estimate_device_bytes(
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
            _ => {
                if validate_geometry_column_type(request.type_oid).is_ok() {
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
                } else {
                    validate_h3_column_type(request.type_oid)?;
                    8
                }
            }
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

pub(super) fn current_relfilenode(relid: pg_sys::Oid) -> Option<pg_sys::Oid> {
    // SAFETY: called on the backend main thread in planner/executor/SRF context.
    let tuple =
        unsafe { pg_sys::SearchSysCache1(pg_sys::SysCacheIdentifier::RELOID as i32, relid.into()) };
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

fn scan_with_detached_cursor(
    query: &str,
    qualified: &str,
    builders: &mut [ColumnBuilder],
) -> Result<u64, String> {
    crate::engine::ffi::planner_hooks::with_planner_hooks_suspended(|| {
        let mut cursor_name = Spi::connect_mut(|client| {
            let cursor = client.try_open_cursor(query, &[]).map_err(|error| {
                format!("resident SPI cursor open for {qualified} failed: {error:?}")
            })?;
            Ok::<String, String>(cursor.detach_into_name())
        })?;
        let mut row_count = 0_u64;
        loop {
            let (done, detached_name, fetched) = Spi::connect_mut(|client| {
                let mut cursor = client.find_cursor(&cursor_name).map_err(|error| {
                    format!("resident SPI cursor lookup for {qualified} failed: {error:?}")
                })?;
                let rows = cursor
                    .fetch(LOAD_INTERRUPT_CHECK_ROWS as libc::c_long)
                    .map_err(|error| {
                        format!("resident SPI cursor fetch for {qualified} failed: {error:?}")
                    })?;
                let fetched = rows.len();
                for builder in &mut *builders {
                    builder.try_reserve(fetched)?;
                }
                for row in rows {
                    for (index, builder) in builders.iter_mut().enumerate() {
                        builder.push(&row, index + 1)?;
                    }
                }
                if fetched == 0 {
                    drop(cursor);
                    Ok::<_, String>((true, None, 0_usize))
                } else {
                    Ok((false, Some(cursor.detach_into_name()), fetched))
                }
            })?;
            row_count = add_fetched_rows(row_count, fetched)?;
            if done {
                return Ok(row_count);
            }
            cursor_name = detached_name.ok_or("resident SPI cursor detached without a name")?;
            pgrx::check_for_interrupts!();
        }
    })
}

fn add_fetched_rows(row_count: u64, fetched: usize) -> Result<u64, &'static str> {
    row_count
        .checked_add(u64::try_from(fetched).map_err(|_| "cursor batch length exceeds u64")?)
        .ok_or("resident row count exceeds u64")
}

fn scan_projection(names: &[String]) -> String {
    if names.is_empty() {
        "1".to_owned()
    } else {
        names
            .iter()
            .map(|name| quote_identifier(name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn exact_relation_scan_query(qualified: &str, names: &[String]) -> String {
    format!("SELECT {} FROM ONLY {qualified}", scan_projection(names))
}

pub(super) fn stage_relation(
    relid: pg_sys::Oid,
    requests: &[ColumnRequest],
) -> Result<StagedRelation, String> {
    for attempt in 0..2 {
        let generation_before = ledger::generation_stamp(relid);
        let relfilenode_before = current_relfilenode(relid).ok_or_else(|| {
            format!(
                "relation OID {} disappeared before resident load",
                u32::from(relid)
            )
        })?;
        let started = Instant::now();
        let qualified = qualified_relation_name(relid)?;
        let estimated_bytes = estimate_device_bytes(relid, requests)?;
        let budget = gucs::resident_memory_budget_bytes();
        if estimated_bytes > budget {
            return Err(format!(
                "relation OID {} resident load estimate {estimated_bytes} bytes exceeds cluster budget {budget} bytes",
                u32::from(relid)
            ));
        }
        let names = column_names(relid, requests)?;
        let query = exact_relation_scan_query(&qualified, &names);
        let mut builders = requests
            .iter()
            .map(|request| ColumnBuilder::for_type(request.type_oid))
            .collect::<Result<Vec<_>, _>>()?;
        let row_count = scan_with_detached_cursor(&query, &qualified, &mut builders)?;
        let generation_after = ledger::generation_stamp(relid);
        let relfilenode_after = current_relfilenode(relid);
        if generation_before != generation_after || relfilenode_after != Some(relfilenode_before) {
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
            generation: generation_after,
            columns,
            row_count,
            loaded_at_us: now_us(),
            load_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    Err("resident load retry loop exhausted".to_owned())
}

pub(super) fn ensure_invalidation_trigger(relid: pg_sys::Oid) -> Result<TriggerInstall, String> {
    const TRIGGER_NAME: &str = "__pg_accel_residency_v2_7d9e";
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
    let function_query = "SELECT p.oid::int8, \
        pg_catalog.quote_ident(n.nspname) || '.' || pg_catalog.quote_ident(p.proname) \
        FROM pg_catalog.pg_proc p \
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
        JOIN pg_catalog.pg_depend d ON d.classid = 'pg_catalog.pg_proc'::regclass \
          AND d.objid = p.oid AND d.refclassid = 'pg_catalog.pg_extension'::regclass \
          AND d.deptype = 'e' \
        JOIN pg_catalog.pg_extension e ON e.oid = d.refobjid \
        WHERE e.extname = 'pg_accel' AND p.proname = 'pg_accel_residency_invalidate' \
          AND p.pronargs = 0";
    let (function_oid, function) = Spi::connect(|client| {
        let rows = client
            .select(function_query, Some(1), &[])
            .map_err(|error| {
                format!("failed to resolve extension-owned residency trigger function: {error:?}")
            })?;
        if rows.is_empty() {
            return Err(
                "pg_accel_residency_invalidate() is not an extension-owned function".to_owned(),
            );
        }
        let row = rows.first();
        let oid = row
            .get::<i64>(1)
            .map_err(|error| format!("trigger function OID read failed: {error:?}"))?
            .ok_or("trigger function OID is NULL")?;
        let name = row
            .get::<String>(2)
            .map_err(|error| format!("trigger function name read failed: {error:?}"))?
            .ok_or("trigger function name is NULL")?;
        Ok::<_, String>((oid, name))
    })?;
    let existing_query = format!(
        "SELECT tgfoid::int8, tgtype::int4, tgenabled::text, tgnargs::int4, (tgattr::text = '') \
         FROM pg_catalog.pg_trigger \
         WHERE tgrelid = {}::oid AND tgname = '{}' AND NOT tgisinternal",
        u32::from(relid),
        TRIGGER_NAME
    );
    let existing = Spi::connect(|client| {
        let rows = client
            .select(&existing_query, Some(1), &[])
            .map_err(|error| {
                format!("failed to inspect residency trigger on {qualified}: {error:?}")
            })?;
        if rows.is_empty() {
            return Ok::<_, String>(None);
        }
        let row = rows.first();
        Ok(Some((
            row.get::<i64>(1)
                .map_err(|error| format!("tgfoid read failed: {error:?}"))?
                .ok_or("tgfoid is NULL")?,
            row.get::<i32>(2)
                .map_err(|error| format!("tgtype read failed: {error:?}"))?
                .ok_or("tgtype is NULL")?,
            row.get::<String>(3)
                .map_err(|error| format!("tgenabled read failed: {error:?}"))?
                .ok_or("tgenabled is NULL")?,
            row.get::<i32>(4)
                .map_err(|error| format!("tgnargs read failed: {error:?}"))?
                .ok_or("tgnargs is NULL")?,
            row.get::<bool>(5)
                .map_err(|error| format!("tgattr read failed: {error:?}"))?
                .ok_or("tgattr emptiness is NULL")?,
        )))
    })?;
    let expected_type = i32::try_from(
        pg_sys::TRIGGER_TYPE_INSERT
            | pg_sys::TRIGGER_TYPE_DELETE
            | pg_sys::TRIGGER_TYPE_UPDATE
            | pg_sys::TRIGGER_TYPE_TRUNCATE,
    )
    .map_err(|_| "PostgreSQL trigger type mask exceeds int32")?;
    if let Some((existing_function, trigger_type, enabled, argument_count, all_updates)) = existing
    {
        if existing_function == function_oid
            && trigger_type == expected_type
            && enabled == "A"
            && argument_count == 0
            && all_updates
        {
            return Ok(TriggerInstall::Existing);
        }
        return Err(format!(
            "cannot trust existing trigger {TRIGGER_NAME} on {qualified}: expected extension function OID {function_oid}, statement-level AFTER INSERT/UPDATE/DELETE/TRUNCATE, ENABLE ALWAYS; drop the conflicting trigger and retry"
        ));
    }
    let create_sql = format!(
        "CREATE TRIGGER {TRIGGER_NAME} \
         AFTER INSERT OR UPDATE OR DELETE OR TRUNCATE ON {qualified} \
         FOR EACH STATEMENT EXECUTE FUNCTION {function}()"
    );
    Spi::run(&create_sql).map_err(|error| format!("cannot install pg_accel residency invalidation trigger on {qualified}: {error:?}; run pg_accel_pin as the table owner"))?;
    let enable_sql = format!("ALTER TABLE {qualified} ENABLE ALWAYS TRIGGER {TRIGGER_NAME}");
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
mod tests {
    use super::*;

    #[test]
    fn count_only_projection_scans_one_constant_per_visible_row() {
        assert_eq!(scan_projection(&[]), "1");
        assert_eq!(scan_projection(&["a\"b".to_owned()]), "\"a\"\"b\"");
        assert_eq!(
            exact_relation_scan_query("\"s\".\"parent\"", &[]),
            "SELECT 1 FROM ONLY \"s\".\"parent\""
        );
        assert_eq!(
            exact_relation_scan_query("\"s\".\"parent\"", &["a\"b".to_owned()]),
            "SELECT \"a\"\"b\" FROM ONLY \"s\".\"parent\""
        );
    }

    #[test]
    fn cursor_batches_accumulate_an_exact_count() {
        let after_first = add_fetched_rows(0, LOAD_INTERRUPT_CHECK_ROWS).expect("first batch");
        let after_second = add_fetched_rows(after_first, 17).expect("second batch");
        assert_eq!(
            after_second,
            u64::try_from(LOAD_INTERRUPT_CHECK_ROWS + 17).expect("test count fits u64")
        );
        assert_eq!(
            add_fetched_rows(u64::MAX, 1),
            Err("resident row count exceeds u64")
        );
    }

    #[test]
    fn raw_h3_datum_preserves_all_unsigned_bits() {
        let bits = 0xf123_4567_89ab_cdef_u64;
        let value = unsafe {
            <RawH3Datum as pgrx::FromDatum>::from_polymorphic_datum(
                pg_sys::Datum::from(bits),
                false,
                pg_sys::Oid::from(50_001),
            )
        };
        assert_eq!(value, Some(RawH3Datum(bits)));
        assert_eq!(
            unsafe {
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
}

//! Pure resident-data layouts for Phase 6 domain columns.
//!
//! These types freeze buffer shapes, validation, and ledger accounting without
//! teaching the loader or executor to produce them yet. `device_bytes` counts
//! only buffers that survive as GPU-readable lanes. The legacy-named
//! `retained_host_exact_bytes` counts original varlena bytes, their offsets,
//! and minimal host-only prefix metadata needed to construct exact domain
//! requests without device readback. Both categories must be charged to the
//! residency ledger for the full lifetime of a resident domain column.

use std::fmt;

pub const RESIDENT_GEOMETRY_POINT: u32 = 1;
pub const RESIDENT_GEOMETRY_LINESTRING: u32 = 2;
pub const RESIDENT_GEOMETRY_POLYGON: u32 = 3;
/// The row's bbox lane is finite, ordered, and covers every coordinate.
/// A clear bit denotes an empty geometry and requires a zero bbox payload.
pub const RESIDENT_GEOMETRY_BBOX_VALID: u32 = 1 << 0;
const RESIDENT_GEOMETRY_ALL_FLAGS: u32 = RESIDENT_GEOMETRY_BBOX_VALID;

pub const RESIDENT_RASTER_BOOL: u32 = 0;
pub const RESIDENT_RASTER_UINT2: u32 = 1;
pub const RESIDENT_RASTER_UINT4: u32 = 2;
pub const RESIDENT_RASTER_INT8: u32 = 3;
pub const RESIDENT_RASTER_UINT8: u32 = 4;
pub const RESIDENT_RASTER_INT16: u32 = 5;
pub const RESIDENT_RASTER_UINT16: u32 = 6;
pub const RESIDENT_RASTER_INT32: u32 = 7;
pub const RESIDENT_RASTER_UINT32: u32 = 8;
// These are the literal PostGIS rt_pixtype tags. Value 9 is intentionally
// absent; PT_32BF and PT_64BF are 10 and 11 respectively.
pub const RESIDENT_RASTER_FLOAT32: u32 = 10;
pub const RESIDENT_RASTER_FLOAT64: u32 = 11;

pub const RESIDENT_RASTER_BAND_HAS_NODATA: u32 = 1 << 0;
pub const RESIDENT_RASTER_BAND_IS_NODATA: u32 = 1 << 1;
const RESIDENT_RASTER_BAND_ALL_FLAGS: u32 =
    RESIDENT_RASTER_BAND_HAS_NODATA | RESIDENT_RASTER_BAND_IS_NODATA;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentByteAccounting {
    pub device_bytes: u64,
    /// All store-owned host bytes retained alongside a resident domain lane.
    /// The field name predates small host-only request metadata.
    pub retained_host_exact_bytes: u64,
}

impl ResidentByteAccounting {
    pub fn checked_total(self) -> Result<u64, DomainContractError> {
        self.device_bytes
            .checked_add(self.retained_host_exact_bytes)
            .ok_or(DomainContractError::ByteCountOverflow)
    }

    fn checked_add(self, other: Self) -> Result<Self, DomainContractError> {
        Ok(Self {
            device_bytes: self
                .device_bytes
                .checked_add(other.device_bytes)
                .ok_or(DomainContractError::ByteCountOverflow)?,
            retained_host_exact_bytes: self
                .retained_host_exact_bytes
                .checked_add(other.retained_host_exact_bytes)
                .ok_or(DomainContractError::ByteCountOverflow)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainContractError {
    Invalid(&'static str),
    ByteCountOverflow,
}

impl fmt::Display for DomainContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::ByteCountOverflow => f.write_str("resident domain byte count overflow"),
        }
    }
}

impl std::error::Error for DomainContractError {}

fn bytes_for_len<T>(len: usize) -> Result<u64, DomainContractError> {
    len.checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(DomainContractError::ByteCountOverflow)
}

fn validate_nulls(nulls: Option<&[u8]>, row_count: usize) -> Result<(), DomainContractError> {
    if let Some(nulls) = nulls
        && (nulls.len() != row_count || nulls.iter().any(|value| *value > 1))
    {
        return Err(DomainContractError::Invalid(
            "domain NULL sidecar must contain one canonical byte per row",
        ));
    }
    Ok(())
}

fn validate_offsets(
    offsets: &[u64],
    expected_count: usize,
    final_value: usize,
    label: &'static str,
) -> Result<(), DomainContractError> {
    if offsets.len() != expected_count
        || offsets.first() != Some(&0)
        || offsets.windows(2).any(|pair| pair[0] > pair[1])
        || offsets.last().copied() != u64::try_from(final_value).ok()
    {
        return Err(DomainContractError::Invalid(label));
    }
    Ok(())
}

fn is_canonical_zero(value: f64) -> bool {
    value.to_bits() == 0
}

fn is_canonical_zero_bbox(bbox: &[f64; 4]) -> bool {
    bbox.iter().all(|value| is_canonical_zero(*value))
}

fn raster_pixel_width(pixel_type: u32) -> Option<usize> {
    match pixel_type {
        RESIDENT_RASTER_BOOL
        | RESIDENT_RASTER_UINT2
        | RESIDENT_RASTER_UINT4
        | RESIDENT_RASTER_INT8
        | RESIDENT_RASTER_UINT8 => Some(1),
        RESIDENT_RASTER_INT16 | RESIDENT_RASTER_UINT16 => Some(2),
        RESIDENT_RASTER_INT32 | RESIDENT_RASTER_UINT32 | RESIDENT_RASTER_FLOAT32 => Some(4),
        RESIDENT_RASTER_FLOAT64 => Some(8),
        _ => None,
    }
}

/// Original detoasted values retained on the host. Boxed slices make their
/// logical lengths their exact owned allocation sizes. Offsets contain one
/// sentinel and therefore always have `row_count + 1` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedExactValues {
    pub offsets: Box<[u64]>,
    pub bytes: Box<[u8]>,
}

impl RetainedExactValues {
    /// Borrow one retained row value directly from the canonical offset lane.
    /// NULL state remains the responsibility of the domain's NULL sidecar.
    #[must_use]
    pub fn value(&self, row_index: usize) -> Option<&[u8]> {
        let start = usize::try_from(*self.offsets.get(row_index)?).ok()?;
        let end = usize::try_from(*self.offsets.get(row_index.checked_add(1)?)?).ok()?;
        self.bytes.get(start..end)
    }

    pub fn validate(
        &self,
        row_count: usize,
        nulls: Option<&[u8]>,
        max_exact_value_bytes: usize,
    ) -> Result<(), DomainContractError> {
        validate_nulls(nulls, row_count)?;
        let expected = row_count
            .checked_add(1)
            .ok_or(DomainContractError::ByteCountOverflow)?;
        validate_offsets(
            &self.offsets,
            expected,
            self.bytes.len(),
            "retained exact-value offsets are invalid",
        )?;
        let max_exact_value_bytes = u64::try_from(max_exact_value_bytes)
            .map_err(|_| DomainContractError::ByteCountOverflow)?;
        for index in 0..row_count {
            let value_bytes = self.offsets[index + 1] - self.offsets[index];
            let is_null = nulls.is_some_and(|nulls| nulls[index] == 1);
            if (is_null && value_bytes != 0) || (!is_null && value_bytes == 0) {
                return Err(DomainContractError::Invalid(
                    "retained exact value does not match row NULL state",
                ));
            }
            if value_bytes > max_exact_value_bytes {
                return Err(DomainContractError::Invalid(
                    "retained exact value exceeds the per-value byte limit",
                ));
            }
        }
        Ok(())
    }

    fn accounting(&self) -> Result<ResidentByteAccounting, DomainContractError> {
        Ok(ResidentByteAccounting {
            device_bytes: 0,
            retained_host_exact_bytes: bytes_for_len::<u64>(self.offsets.len())?
                .checked_add(bytes_for_len::<u8>(self.bytes.len())?)
                .ok_or(DomainContractError::ByteCountOverflow)?,
        })
    }
}

/// Resident h3index lane. Values are the extension's catalog-proved, pass-by-
/// value 64-bit representation; no extension type OID is stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentH3Lane {
    pub values: Vec<u64>,
    pub nulls: Option<Vec<u8>>,
}

impl ResidentH3Lane {
    pub fn validate(&self) -> Result<(), DomainContractError> {
        validate_nulls(self.nulls.as_deref(), self.values.len())
    }

    pub fn accounting(&self) -> Result<ResidentByteAccounting, DomainContractError> {
        self.validate()?;
        Ok(ResidentByteAccounting {
            device_bytes: bytes_for_len::<u64>(self.values.len())?
                .checked_add(bytes_for_len::<u8>(
                    self.nulls.as_ref().map_or(0, Vec::len),
                )?)
                .ok_or(DomainContractError::ByteCountOverflow)?,
            retained_host_exact_bytes: 0,
        })
    }
}

/// Fixed ABI metadata for one flattened geometry row.
///
/// Integer tags are used instead of a Rust enum so future FFI validation can
/// reject unknown values without first constructing an invalid enum.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentGeometryRow {
    pub geom_type: u32,
    pub srid: i32,
    pub first_ring: u64,
    pub ring_count: u32,
    pub flags: u32,
}

/// Flattened fp64 geometry lanes plus retained exact GSERIALIZED values.
/// `bboxes` contains `[min_x, min_y, max_x, max_y]` for every row.
/// `geometry_offsets` and `ring_offsets` are coordinate-pair indexes, not byte
/// or scalar indexes. A non-NULL row is empty exactly when its coordinate span
/// is empty and `RESIDENT_GEOMETRY_BBOX_VALID` is clear; its bbox is then
/// canonical positive zero.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentGeometryData {
    pub coordinates: Vec<f64>,
    pub bboxes: Vec<[f64; 4]>,
    pub geometry_offsets: Vec<u64>,
    pub ring_offsets: Vec<u64>,
    pub rows: Vec<ResidentGeometryRow>,
    pub nulls: Option<Vec<u8>>,
    pub exact: RetainedExactValues,
}

impl ResidentGeometryData {
    pub fn validate(&self, max_exact_value_bytes: usize) -> Result<(), DomainContractError> {
        if !self.coordinates.len().is_multiple_of(2) {
            return Err(DomainContractError::Invalid(
                "geometry coordinate lane is not x/y paired",
            ));
        }
        let row_count = self.rows.len();
        if self.bboxes.len() != row_count {
            return Err(DomainContractError::Invalid(
                "geometry bbox lane must contain one entry per row",
            ));
        }
        let offset_count = row_count
            .checked_add(1)
            .ok_or(DomainContractError::ByteCountOverflow)?;
        validate_offsets(
            &self.geometry_offsets,
            offset_count,
            self.coordinates.len() / 2,
            "geometry row offsets are invalid",
        )?;
        validate_nulls(self.nulls.as_deref(), row_count)?;
        self.exact
            .validate(row_count, self.nulls.as_deref(), max_exact_value_bytes)?;

        let mut next_ring = 0_usize;
        for (index, row) in self.rows.iter().enumerate() {
            let coordinate_start = usize::try_from(self.geometry_offsets[index])
                .map_err(|_| DomainContractError::Invalid("geometry offset overflows usize"))?;
            let coordinate_end = usize::try_from(self.geometry_offsets[index + 1])
                .map_err(|_| DomainContractError::Invalid("geometry offset overflows usize"))?;
            let is_null = self.nulls.as_ref().is_some_and(|nulls| nulls[index] == 1);
            let bbox = &self.bboxes[index];
            if is_null {
                if *row != ResidentGeometryRow::default()
                    || coordinate_start != coordinate_end
                    || !is_canonical_zero_bbox(bbox)
                {
                    return Err(DomainContractError::Invalid(
                        "NULL geometry row is not canonical zero",
                    ));
                }
                continue;
            }
            if !matches!(
                row.geom_type,
                RESIDENT_GEOMETRY_POINT | RESIDENT_GEOMETRY_LINESTRING | RESIDENT_GEOMETRY_POLYGON
            ) || row.flags & !RESIDENT_GEOMETRY_ALL_FLAGS != 0
                || !(0..=999_999).contains(&row.srid)
            {
                return Err(DomainContractError::Invalid(
                    "geometry row metadata contains an unknown tag or flag",
                ));
            }
            let first_ring = usize::try_from(row.first_ring)
                .map_err(|_| DomainContractError::Invalid("geometry ring index overflows usize"))?;
            let ring_count = usize::try_from(row.ring_count)
                .map_err(|_| DomainContractError::Invalid("geometry ring count overflows usize"))?;
            let ring_end = first_ring
                .checked_add(ring_count)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            let bbox_valid = row.flags & RESIDENT_GEOMETRY_BBOX_VALID != 0;
            if first_ring != next_ring
                || ring_end > self.ring_offsets.len()
                || (bbox_valid && row.geom_type == RESIDENT_GEOMETRY_POLYGON) != (ring_count > 0)
            {
                return Err(DomainContractError::Invalid(
                    "geometry row ring range is invalid",
                ));
            }
            let coordinate_count = coordinate_end - coordinate_start;
            if bbox_valid != (coordinate_count > 0) {
                return Err(DomainContractError::Invalid(
                    "geometry bbox validity does not match row emptiness",
                ));
            }
            if !bbox_valid {
                if ring_count != 0 || !is_canonical_zero_bbox(bbox) {
                    return Err(DomainContractError::Invalid(
                        "empty geometry bbox and ring metadata are not canonical zero",
                    ));
                }
                next_ring = ring_end;
                continue;
            }
            let [min_x, min_y, max_x, max_y] = *bbox;
            if bbox.iter().any(|value| !value.is_finite()) || min_x > max_x || min_y > max_y {
                return Err(DomainContractError::Invalid(
                    "geometry bbox is non-finite or unordered",
                ));
            }
            let coordinate_scalars = &self.coordinates[coordinate_start * 2..coordinate_end * 2];
            if coordinate_scalars.chunks_exact(2).any(|coordinate| {
                coordinate[0] < min_x
                    || coordinate[0] > max_x
                    || coordinate[1] < min_y
                    || coordinate[1] > max_y
            }) {
                return Err(DomainContractError::Invalid(
                    "geometry bbox does not cover every row coordinate",
                ));
            }
            match row.geom_type {
                RESIDENT_GEOMETRY_POINT if coordinate_count != 1 => {
                    return Err(DomainContractError::Invalid(
                        "resident point does not contain exactly one coordinate",
                    ));
                }
                RESIDENT_GEOMETRY_LINESTRING if coordinate_count < 2 => {
                    return Err(DomainContractError::Invalid(
                        "resident linestring contains fewer than two coordinates",
                    ));
                }
                RESIDENT_GEOMETRY_POLYGON => {
                    let ring_offsets = &self.ring_offsets[first_ring..ring_end];
                    if ring_offsets.first().copied() != u64::try_from(coordinate_start).ok()
                        || ring_offsets.windows(2).any(|pair| pair[0] >= pair[1])
                    {
                        return Err(DomainContractError::Invalid(
                            "geometry polygon ring starts are not canonical",
                        ));
                    }
                    for (ring_index, start) in ring_offsets.iter().enumerate() {
                        let start = usize::try_from(*start).map_err(|_| {
                            DomainContractError::Invalid("geometry ring offset overflows usize")
                        })?;
                        let end = ring_offsets.get(ring_index + 1).copied().map_or(
                            Ok(coordinate_end),
                            |end| {
                                usize::try_from(end).map_err(|_| {
                                    DomainContractError::Invalid(
                                        "geometry ring offset overflows usize",
                                    )
                                })
                            },
                        )?;
                        if start < coordinate_start
                            || start >= end
                            || end > coordinate_end
                            || end - start < 3
                        {
                            return Err(DomainContractError::Invalid(
                                "geometry polygon ring is outside its row or degenerate",
                            ));
                        }
                    }
                }
                _ => {}
            }
            next_ring = ring_end;
        }
        if next_ring != self.ring_offsets.len()
            || self
                .coordinates
                .iter()
                .any(|coordinate| !coordinate.is_finite())
        {
            return Err(DomainContractError::Invalid(
                "geometry contains orphan rings or non-finite coordinates",
            ));
        }
        Ok(())
    }

    pub fn accounting(
        &self,
        max_exact_value_bytes: usize,
    ) -> Result<ResidentByteAccounting, DomainContractError> {
        self.validate(max_exact_value_bytes)?;
        let device = ResidentByteAccounting {
            device_bytes: bytes_for_len::<f64>(self.coordinates.len())?
                .checked_add(bytes_for_len::<[f64; 4]>(self.bboxes.len())?)
                .and_then(|bytes| {
                    bytes.checked_add(bytes_for_len::<u64>(self.geometry_offsets.len()).ok()?)
                })
                .and_then(|bytes| {
                    bytes.checked_add(bytes_for_len::<u64>(self.ring_offsets.len()).ok()?)
                })
                .and_then(|bytes| {
                    bytes.checked_add(bytes_for_len::<ResidentGeometryRow>(self.rows.len()).ok()?)
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        bytes_for_len::<u8>(self.nulls.as_ref().map_or(0, Vec::len)).ok()?,
                    )
                })
                .ok_or(DomainContractError::ByteCountOverflow)?,
            retained_host_exact_bytes: 0,
        };
        device.checked_add(self.exact.accounting()?)
    }
}

/// Fixed ABI metadata for one raster value.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResidentRasterRow {
    pub width: u32,
    pub height: u32,
    pub first_band: u32,
    pub band_count: u32,
    pub srid: i32,
    pub flags: u32,
    pub scale_x: f64,
    pub scale_y: f64,
    pub ip_x: f64,
    pub ip_y: f64,
    pub skew_x: f64,
    pub skew_y: f64,
}

/// Fixed ABI metadata for one flattened raster band.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResidentRasterBand {
    pub pixel_type: u32,
    pub flags: u32,
    pub nodata: f64,
}

pub const RESIDENT_RASTER_WORK_NULL: u8 = 0;
pub const RESIDENT_RASTER_WORK_MISSING_BAND: u8 = 1;
pub const RESIDENT_RASTER_WORK_RECLASS: u8 = 2;

/// Host-retained facts for one raster row. Executors clone this compact lane
/// in a short metadata snapshot, release the store borrow, and reserve exact
/// output/scratch bytes before borrowing device input.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentRasterWorkRow {
    pub grid_pixels: u64,
    pub exact_wkb_bytes: u64,
    pub source_pixel_width: u8,
    pub action: u8,
    pub reserved: [u8; 6],
}

/// Host-retained work metadata used by the raster planner without copying
/// GPU buffers or reparsing every exact WKB value during planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentRasterStats {
    pub row_count: u64,
    pub non_null_rows: u64,
    pub total_grid_pixels: u64,
    pub total_band_pixels: u64,
    pub input_wkb_bytes: u64,
    /// One entry per one-based band ordinal. Missing bands contribute zero.
    pub band_pixels: Box<[u64]>,
    /// Rows in which each one-based band ordinal is present. This remains
    /// distinct from pixel count for zero-dimensional rasters.
    pub band_rows: Box<[u64]>,
    /// Exact full-WKB result totals for output pixel widths 1, 2, and 4.
    reclass_output_wkb_bytes: [u64; 3],
    pub work_rows: Box<[ResidentRasterWorkRow]>,
}

impl ResidentRasterStats {
    /// Canonical work metadata for a typed raster column with zero rows.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            row_count: 0,
            non_null_rows: 0,
            total_grid_pixels: 0,
            total_band_pixels: 0,
            input_wkb_bytes: 0,
            band_pixels: Box::default(),
            band_rows: Box::default(),
            reclass_output_wkb_bytes: [0; 3],
            work_rows: Box::default(),
        }
    }

    #[must_use]
    pub fn selected_band_pixels(&self, band: u32) -> Option<u64> {
        let index = usize::try_from(band.checked_sub(1)?).ok()?;
        Some(self.band_pixels.get(index).copied().unwrap_or(0))
    }

    #[must_use]
    pub fn selected_band_rows(&self, band: u32) -> Option<u64> {
        let index = usize::try_from(band.checked_sub(1)?).ok()?;
        Some(self.band_rows.get(index).copied().unwrap_or(0))
    }

    #[must_use]
    pub const fn reclass_output_wkb_bytes(&self, pixel_type_tag: u32) -> Option<u64> {
        match pixel_type_tag {
            0..=4 => Some(self.reclass_output_wkb_bytes[0]),
            5..=6 => Some(self.reclass_output_wkb_bytes[1]),
            7..=8 => Some(self.reclass_output_wkb_bytes[2]),
            _ => None,
        }
    }

    #[must_use]
    pub fn reclass_output_pixel_bytes(&self, pixel_type_tag: u32) -> Option<u64> {
        let width = match pixel_type_tag {
            0..=4 => 1_u64,
            5..=6 => 2,
            7..=8 => 4,
            _ => return None,
        };
        self.selected_band_pixels(1)?.checked_mul(width)
    }

    fn retained_bytes(&self) -> Result<u64, DomainContractError> {
        bytes_for_len::<u64>(self.band_pixels.len())?
            .checked_add(bytes_for_len::<u64>(self.band_rows.len())?)
            .and_then(|bytes| {
                bytes
                    .checked_add(bytes_for_len::<ResidentRasterWorkRow>(self.work_rows.len()).ok()?)
            })
            .ok_or(DomainContractError::ByteCountOverflow)
    }
}

/// Flattened raster pixel bytes and metadata plus retained exact WKB values.
/// Each band keeps its native PostGIS pixel representation; `band_offsets`
/// are byte offsets, not pixel indexes.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentRasterData {
    pub pixels: Vec<u8>,
    pub band_offsets: Vec<u64>,
    pub rows: Vec<ResidentRasterRow>,
    pub bands: Vec<ResidentRasterBand>,
    pub nulls: Option<Vec<u8>>,
    pub exact: RetainedExactValues,
}

fn raster_row_is_canonical_zero(row: &ResidentRasterRow) -> bool {
    row.width == 0
        && row.height == 0
        && row.first_band == 0
        && row.band_count == 0
        && row.srid == 0
        && row.flags == 0
        && is_canonical_zero(row.scale_x)
        && is_canonical_zero(row.scale_y)
        && is_canonical_zero(row.ip_x)
        && is_canonical_zero(row.ip_y)
        && is_canonical_zero(row.skew_x)
        && is_canonical_zero(row.skew_y)
}

impl ResidentRasterData {
    pub fn stats(&self) -> Result<ResidentRasterStats, DomainContractError> {
        let max_band_count = self
            .rows
            .iter()
            .map(|row| usize::try_from(row.band_count).unwrap_or(usize::MAX))
            .max()
            .unwrap_or(0);
        if max_band_count == usize::MAX {
            return Err(DomainContractError::ByteCountOverflow);
        }
        let mut band_pixels = Vec::new();
        band_pixels.try_reserve_exact(max_band_count).map_err(|_| {
            DomainContractError::Invalid("resident raster statistics allocation failed")
        })?;
        band_pixels.resize(max_band_count, 0_u64);
        let mut band_rows = Vec::new();
        band_rows.try_reserve_exact(max_band_count).map_err(|_| {
            DomainContractError::Invalid("resident raster statistics allocation failed")
        })?;
        band_rows.resize(max_band_count, 0_u64);
        let mut work_rows = Vec::new();
        work_rows.try_reserve_exact(self.rows.len()).map_err(|_| {
            DomainContractError::Invalid("resident raster statistics allocation failed")
        })?;
        let mut non_null_rows = 0_u64;
        let mut total_grid_pixels = 0_u64;
        let mut total_band_pixels = 0_u64;
        let mut input_wkb_bytes = 0_u64;
        let mut reclass_output_wkb_bytes = [0_u64; 3];
        for (row_index, row) in self.rows.iter().enumerate() {
            let exact_wkb_bytes = u64::try_from(
                self.exact
                    .value(row_index)
                    .ok_or(DomainContractError::Invalid(
                        "resident raster exact row metadata is invalid",
                    ))?
                    .len(),
            )
            .map_err(|_| DomainContractError::ByteCountOverflow)?;
            input_wkb_bytes = input_wkb_bytes
                .checked_add(exact_wkb_bytes)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            if self
                .nulls
                .as_ref()
                .is_some_and(|nulls| nulls.get(row_index) == Some(&1))
            {
                work_rows.push(ResidentRasterWorkRow {
                    exact_wkb_bytes,
                    action: RESIDENT_RASTER_WORK_NULL,
                    ..ResidentRasterWorkRow::default()
                });
                continue;
            }
            non_null_rows = non_null_rows
                .checked_add(1)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            let pixels = u64::from(row.width)
                .checked_mul(u64::from(row.height))
                .ok_or(DomainContractError::ByteCountOverflow)?;
            total_grid_pixels = total_grid_pixels
                .checked_add(pixels)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            total_band_pixels = total_band_pixels
                .checked_add(
                    pixels
                        .checked_mul(u64::from(row.band_count))
                        .ok_or(DomainContractError::ByteCountOverflow)?,
                )
                .ok_or(DomainContractError::ByteCountOverflow)?;
            let band_count = usize::try_from(row.band_count)
                .map_err(|_| DomainContractError::ByteCountOverflow)?;
            for (pixel_count, row_count) in
                band_pixels.iter_mut().zip(&mut band_rows).take(band_count)
            {
                *pixel_count = pixel_count
                    .checked_add(pixels)
                    .ok_or(DomainContractError::ByteCountOverflow)?;
                *row_count = row_count
                    .checked_add(1)
                    .ok_or(DomainContractError::ByteCountOverflow)?;
            }
            if band_count == 0 {
                for output in &mut reclass_output_wkb_bytes {
                    *output = output
                        .checked_add(exact_wkb_bytes)
                        .ok_or(DomainContractError::ByteCountOverflow)?;
                }
                work_rows.push(ResidentRasterWorkRow {
                    grid_pixels: pixels,
                    exact_wkb_bytes,
                    action: RESIDENT_RASTER_WORK_MISSING_BAND,
                    ..ResidentRasterWorkRow::default()
                });
                continue;
            }
            let first_band = usize::try_from(row.first_band)
                .map_err(|_| DomainContractError::ByteCountOverflow)?;
            let source_pixel_width = self
                .bands
                .get(first_band)
                .and_then(|band| raster_pixel_width(band.pixel_type))
                .ok_or(DomainContractError::Invalid(
                    "resident raster band-one metadata is invalid",
                ))?;
            let serialized_values = pixels
                .checked_add(1)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            let old_band_values = serialized_values
                .checked_mul(
                    u64::try_from(source_pixel_width)
                        .map_err(|_| DomainContractError::ByteCountOverflow)?,
                )
                .ok_or(DomainContractError::ByteCountOverflow)?;
            let base_wkb_bytes = exact_wkb_bytes.checked_sub(old_band_values).ok_or(
                DomainContractError::Invalid(
                    "resident raster exact WKB is shorter than band-one payload",
                ),
            )?;
            for (index, output_width) in [1_u64, 2, 4].into_iter().enumerate() {
                let output_values = serialized_values
                    .checked_mul(output_width)
                    .ok_or(DomainContractError::ByteCountOverflow)?;
                reclass_output_wkb_bytes[index] = reclass_output_wkb_bytes[index]
                    .checked_add(
                        base_wkb_bytes
                            .checked_add(output_values)
                            .ok_or(DomainContractError::ByteCountOverflow)?,
                    )
                    .ok_or(DomainContractError::ByteCountOverflow)?;
            }
            work_rows.push(ResidentRasterWorkRow {
                grid_pixels: pixels,
                exact_wkb_bytes,
                source_pixel_width: u8::try_from(source_pixel_width)
                    .map_err(|_| DomainContractError::ByteCountOverflow)?,
                action: RESIDENT_RASTER_WORK_RECLASS,
                reserved: [0; 6],
            });
        }
        Ok(ResidentRasterStats {
            row_count: u64::try_from(self.rows.len())
                .map_err(|_| DomainContractError::ByteCountOverflow)?,
            non_null_rows,
            total_grid_pixels,
            total_band_pixels,
            input_wkb_bytes,
            band_pixels: band_pixels.into_boxed_slice(),
            band_rows: band_rows.into_boxed_slice(),
            reclass_output_wkb_bytes,
            work_rows: work_rows.into_boxed_slice(),
        })
    }

    pub fn validate(&self, max_exact_value_bytes: usize) -> Result<(), DomainContractError> {
        let band_offset_count = self
            .bands
            .len()
            .checked_add(1)
            .ok_or(DomainContractError::ByteCountOverflow)?;
        validate_offsets(
            &self.band_offsets,
            band_offset_count,
            self.pixels.len(),
            "raster band offsets are invalid",
        )?;
        validate_nulls(self.nulls.as_deref(), self.rows.len())?;
        self.exact.validate(
            self.rows.len(),
            self.nulls.as_deref(),
            max_exact_value_bytes,
        )?;

        let mut next_band = 0_usize;
        for (row_index, row) in self.rows.iter().enumerate() {
            let is_null = self
                .nulls
                .as_ref()
                .is_some_and(|nulls| nulls[row_index] == 1);
            if is_null {
                if !raster_row_is_canonical_zero(row) {
                    return Err(DomainContractError::Invalid(
                        "NULL raster row is not canonical zero",
                    ));
                }
                continue;
            }
            if row.flags != 0 {
                return Err(DomainContractError::Invalid(
                    "raster row contains unknown flags",
                ));
            }
            let first_band = usize::try_from(row.first_band)
                .map_err(|_| DomainContractError::Invalid("raster band index overflows usize"))?;
            let band_count = usize::try_from(row.band_count)
                .map_err(|_| DomainContractError::Invalid("raster band count overflows usize"))?;
            let band_end = first_band
                .checked_add(band_count)
                .ok_or(DomainContractError::ByteCountOverflow)?;
            if first_band != next_band
                || band_end > self.bands.len()
                || !(0..=999_999).contains(&row.srid)
                || !row.scale_x.is_finite()
                || !row.scale_y.is_finite()
                || !row.ip_x.is_finite()
                || !row.ip_y.is_finite()
                || !row.skew_x.is_finite()
                || !row.skew_y.is_finite()
            {
                return Err(DomainContractError::Invalid(
                    "raster row metadata or band range is invalid",
                ));
            }
            let pixel_count = usize::try_from(row.width)
                .ok()
                .and_then(|width| {
                    usize::try_from(row.height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .ok_or(DomainContractError::ByteCountOverflow)?;
            for band in first_band..band_end {
                let actual = self.band_offsets[band + 1] - self.band_offsets[band];
                let expected = raster_pixel_width(self.bands[band].pixel_type)
                    .and_then(|width| pixel_count.checked_mul(width))
                    .and_then(|bytes| u64::try_from(bytes).ok())
                    .ok_or(DomainContractError::ByteCountOverflow)?;
                if actual != expected {
                    return Err(DomainContractError::Invalid(
                        "raster band byte count does not match row dimensions and type",
                    ));
                }
            }
            next_band = band_end;
        }
        if next_band != self.bands.len() {
            return Err(DomainContractError::Invalid("raster contains orphan bands"));
        }
        for band in &self.bands {
            if raster_pixel_width(band.pixel_type).is_none()
                || band.flags & !RESIDENT_RASTER_BAND_ALL_FLAGS != 0
                || band.flags & RESIDENT_RASTER_BAND_IS_NODATA != 0
                    && band.flags & RESIDENT_RASTER_BAND_HAS_NODATA == 0
                || band.flags & RESIDENT_RASTER_BAND_HAS_NODATA == 0
                    && !is_canonical_zero(band.nodata)
            {
                return Err(DomainContractError::Invalid(
                    "raster band metadata contains an unknown tag or flag",
                ));
            }
        }
        Ok(())
    }

    pub fn accounting(
        &self,
        max_exact_value_bytes: usize,
    ) -> Result<ResidentByteAccounting, DomainContractError> {
        self.validate(max_exact_value_bytes)?;
        let stats = self.stats()?;
        let device = ResidentByteAccounting {
            device_bytes: bytes_for_len::<u8>(self.pixels.len())?
                .checked_add(bytes_for_len::<u64>(self.band_offsets.len())?)
                .and_then(|bytes| {
                    bytes.checked_add(bytes_for_len::<ResidentRasterRow>(self.rows.len()).ok()?)
                })
                .and_then(|bytes| {
                    bytes.checked_add(bytes_for_len::<ResidentRasterBand>(self.bands.len()).ok()?)
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        bytes_for_len::<u8>(self.nulls.as_ref().map_or(0, Vec::len)).ok()?,
                    )
                })
                .ok_or(DomainContractError::ByteCountOverflow)?,
            retained_host_exact_bytes: 0,
        };
        let exact = self.exact.accounting()?;
        device.checked_add(ResidentByteAccounting {
            device_bytes: exact.device_bytes,
            retained_host_exact_bytes: exact
                .retained_host_exact_bytes
                .checked_add(stats.retained_bytes()?)
                .ok_or(DomainContractError::ByteCountOverflow)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_EXACT_VALUE_BYTES: usize = 1_024;

    fn exact(offsets: Vec<u64>, bytes: Vec<u8>) -> RetainedExactValues {
        RetainedExactValues {
            offsets: offsets.into_boxed_slice(),
            bytes: bytes.into_boxed_slice(),
        }
    }

    fn point_geometry() -> ResidentGeometryData {
        ResidentGeometryData {
            coordinates: vec![1.0, 2.0],
            bboxes: vec![[1.0, 2.0, 1.0, 2.0]],
            geometry_offsets: vec![0, 1],
            ring_offsets: Vec::new(),
            rows: vec![ResidentGeometryRow {
                geom_type: RESIDENT_GEOMETRY_POINT,
                srid: 4_326,
                flags: RESIDENT_GEOMETRY_BBOX_VALID,
                ..ResidentGeometryRow::default()
            }],
            nulls: Some(vec![0]),
            exact: exact(vec![0, 4], vec![1, 2, 3, 4]),
        }
    }

    fn one_band_raster() -> ResidentRasterData {
        ResidentRasterData {
            pixels: vec![0; 4],
            band_offsets: vec![0, 4],
            rows: vec![ResidentRasterRow {
                width: 1,
                height: 1,
                first_band: 0,
                band_count: 1,
                srid: 3_857,
                scale_x: 1.0,
                scale_y: -1.0,
                ..ResidentRasterRow::default()
            }],
            bands: vec![ResidentRasterBand {
                pixel_type: RESIDENT_RASTER_FLOAT32,
                ..ResidentRasterBand::default()
            }],
            nulls: Some(vec![0]),
            exact: exact(vec![0, 64], vec![0; 64]),
        }
    }

    #[test]
    fn domain_metadata_abi_is_pinned() {
        assert_eq!(std::mem::size_of::<ResidentGeometryRow>(), 24);
        assert_eq!(std::mem::align_of::<ResidentGeometryRow>(), 8);
        assert_eq!(std::mem::offset_of!(ResidentGeometryRow, first_ring), 8);
        assert_eq!(std::mem::size_of::<ResidentRasterRow>(), 72);
        assert_eq!(std::mem::align_of::<ResidentRasterRow>(), 8);
        assert_eq!(std::mem::offset_of!(ResidentRasterRow, scale_x), 24);
        assert_eq!(std::mem::size_of::<ResidentRasterBand>(), 16);
        assert_eq!(std::mem::align_of::<ResidentRasterBand>(), 8);
        assert_eq!(std::mem::offset_of!(ResidentRasterBand, nodata), 8);
        assert_eq!(std::mem::size_of::<ResidentRasterWorkRow>(), 24);
        assert_eq!(std::mem::align_of::<ResidentRasterWorkRow>(), 8);
        assert_eq!(std::mem::size_of::<[f64; 4]>(), 32);
        assert_eq!(RESIDENT_GEOMETRY_BBOX_VALID, 1);
        assert_eq!(RESIDENT_RASTER_FLOAT32, 10);
        assert_eq!(RESIDENT_RASTER_FLOAT64, 11);
        assert_eq!(raster_pixel_width(9), None);
    }

    #[test]
    fn h3_lane_accounts_values_and_nulls_as_device_bytes() {
        let lane = ResidentH3Lane {
            values: vec![1, 2, 3],
            nulls: Some(vec![0, 1, 0]),
        };
        assert_eq!(
            lane.accounting().expect("valid H3 accounting"),
            ResidentByteAccounting {
                device_bytes: 27,
                retained_host_exact_bytes: 0,
            }
        );
        assert_eq!(
            lane.accounting()
                .expect("valid H3 accounting")
                .checked_total()
                .expect("total fits"),
            27
        );
    }

    #[test]
    fn geometry_accounts_flattened_lanes_and_exact_host_bytes() {
        let geometry = ResidentGeometryData {
            coordinates: vec![1.0, 2.0, 3.0, 4.0],
            bboxes: vec![[1.0, 2.0, 1.0, 2.0], [3.0, 4.0, 3.0, 4.0]],
            geometry_offsets: vec![0, 1, 2],
            ring_offsets: Vec::new(),
            rows: vec![
                ResidentGeometryRow {
                    geom_type: RESIDENT_GEOMETRY_POINT,
                    srid: 4_326,
                    flags: RESIDENT_GEOMETRY_BBOX_VALID,
                    ..ResidentGeometryRow::default()
                },
                ResidentGeometryRow {
                    geom_type: RESIDENT_GEOMETRY_POINT,
                    srid: 4_326,
                    flags: RESIDENT_GEOMETRY_BBOX_VALID,
                    ..ResidentGeometryRow::default()
                },
            ],
            nulls: Some(vec![0, 0]),
            exact: exact(vec![0, 3, 7], vec![1, 2, 3, 4, 5, 6, 7]),
        };
        assert_eq!(
            geometry
                .accounting(MAX_EXACT_VALUE_BYTES)
                .expect("valid geometry accounting"),
            ResidentByteAccounting {
                device_bytes: 32 + 64 + 24 + 48 + 2,
                retained_host_exact_bytes: 24 + 7,
            }
        );
        assert_eq!(
            bytes_for_len::<[f64; 4]>(geometry.bboxes.len()).expect("bbox bytes fit"),
            32 * u64::try_from(geometry.rows.len()).expect("row count fits"),
            "geometry bboxes consume exactly 32 device bytes per row"
        );
    }

    #[test]
    fn geometry_bbox_contract_rejects_every_malformed_shape() {
        let valid = point_geometry();
        valid
            .validate(MAX_EXACT_VALUE_BYTES)
            .expect("valid nonempty bbox is accepted");

        let mut malformed = point_geometry();
        malformed.bboxes.clear();
        assert!(malformed.validate(MAX_EXACT_VALUE_BYTES).is_err());

        let mut malformed = point_geometry();
        malformed.bboxes.push([0.0; 4]);
        assert!(malformed.validate(MAX_EXACT_VALUE_BYTES).is_err());

        for invalid in [
            [f64::NAN, 2.0, 1.0, 2.0],
            [1.0, 2.0, f64::INFINITY, 2.0],
            [2.0, 2.0, 1.0, 2.0],
            [1.0, 3.0, 1.0, 2.0],
            [0.0, 0.0, 0.5, 3.0],
        ] {
            let mut malformed = point_geometry();
            malformed.bboxes[0] = invalid;
            assert!(malformed.validate(MAX_EXACT_VALUE_BYTES).is_err());
        }

        let mut missing_validity = point_geometry();
        missing_validity.rows[0].flags = 0;
        assert!(missing_validity.validate(MAX_EXACT_VALUE_BYTES).is_err());

        let mut extra_validity = point_geometry();
        extra_validity.rows[0].flags |= 1 << 31;
        assert!(extra_validity.validate(MAX_EXACT_VALUE_BYTES).is_err());
    }

    #[test]
    fn geometry_layout_recovers_bbox_and_exact_bytes_as_immutable_row_views() {
        let expected = [
            ([1.25, -2.5, 1.25, -2.5], b"first-gserialized".as_slice()),
            ([10.0, 20.0, 10.0, 20.0], b"second".as_slice()),
        ];
        let first_end = u64::try_from(expected[0].1.len()).expect("fixture length fits");
        let final_end = first_end
            .checked_add(u64::try_from(expected[1].1.len()).expect("fixture length fits"))
            .expect("fixture offsets fit");
        let exact_bytes = expected
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        let geometry = ResidentGeometryData {
            coordinates: vec![1.25, -2.5, 10.0, 20.0],
            bboxes: expected.iter().map(|(bbox, _)| *bbox).collect(),
            geometry_offsets: vec![0, 1, 2],
            ring_offsets: Vec::new(),
            rows: vec![
                ResidentGeometryRow {
                    geom_type: RESIDENT_GEOMETRY_POINT,
                    flags: RESIDENT_GEOMETRY_BBOX_VALID,
                    ..ResidentGeometryRow::default()
                };
                expected.len()
            ],
            nulls: None,
            exact: exact(vec![0, first_end, final_end], exact_bytes),
        };
        geometry
            .validate(MAX_EXACT_VALUE_BYTES)
            .expect("resident geometry fixture is valid");

        let resident = &geometry;
        for (row_index, (expected_bbox, expected_exact)) in expected.iter().enumerate() {
            assert_eq!(resident.bboxes.get(row_index), Some(expected_bbox));
            assert_eq!(resident.exact.value(row_index), Some(*expected_exact));
        }
        assert_eq!(resident.exact.value(expected.len()), None);
    }

    #[test]
    fn empty_and_null_geometry_rows_have_canonical_zero_bboxes() {
        let empty = ResidentGeometryData {
            coordinates: Vec::new(),
            bboxes: vec![[0.0; 4]],
            geometry_offsets: vec![0, 0],
            ring_offsets: Vec::new(),
            rows: vec![ResidentGeometryRow {
                geom_type: RESIDENT_GEOMETRY_POINT,
                srid: 4_326,
                ..ResidentGeometryRow::default()
            }],
            nulls: Some(vec![0]),
            exact: exact(vec![0, 4], vec![1, 2, 3, 4]),
        };
        empty
            .validate(MAX_EXACT_VALUE_BYTES)
            .expect("canonical empty geometry is accepted");

        let null = ResidentGeometryData {
            coordinates: Vec::new(),
            bboxes: vec![[0.0; 4]],
            geometry_offsets: vec![0, 0],
            ring_offsets: Vec::new(),
            rows: vec![ResidentGeometryRow::default()],
            nulls: Some(vec![1]),
            exact: exact(vec![0, 0], Vec::new()),
        };
        null.validate(MAX_EXACT_VALUE_BYTES)
            .expect("canonical NULL geometry is accepted");

        for mut malformed in [empty, null] {
            malformed.bboxes[0][0] = -0.0;
            assert!(malformed.validate(MAX_EXACT_VALUE_BYTES).is_err());
        }
    }

    #[test]
    fn retained_exact_values_match_nulls_and_enforce_the_per_value_cap() {
        assert!(exact(vec![0, 0], Vec::new()).validate(1, None, 8).is_err());
        assert!(
            exact(vec![0, 1], vec![1])
                .validate(1, Some(&[1]), 8)
                .is_err()
        );
        assert!(
            exact(vec![0, 4], vec![1, 2, 3, 4])
                .validate(1, None, 3)
                .is_err()
        );
        exact(vec![0, 0], Vec::new())
            .validate(1, Some(&[1]), 0)
            .expect("NULL exact segment may be empty at a zero byte cap");
    }

    #[test]
    fn raster_accounts_flattened_lanes_and_exact_host_bytes() {
        let raster = ResidentRasterData {
            pixels: vec![0; 16],
            band_offsets: vec![0, 16],
            rows: vec![ResidentRasterRow {
                width: 2,
                height: 2,
                first_band: 0,
                band_count: 1,
                srid: 3_857,
                scale_x: 1.0,
                scale_y: -1.0,
                ..ResidentRasterRow::default()
            }],
            bands: vec![ResidentRasterBand {
                pixel_type: RESIDENT_RASTER_FLOAT32,
                flags: RESIDENT_RASTER_BAND_HAS_NODATA,
                nodata: -9_999.0,
            }],
            nulls: Some(vec![0]),
            exact: exact(vec![0, 64], vec![0; 64]),
        };
        assert_eq!(
            raster
                .accounting(MAX_EXACT_VALUE_BYTES)
                .expect("valid raster accounting"),
            ResidentByteAccounting {
                device_bytes: 16 + 16 + 72 + 16 + 1,
                retained_host_exact_bytes: 16 + 64 + 8 + 8 + 24,
            }
        );
        assert_eq!(
            raster.stats().expect("valid raster statistics"),
            ResidentRasterStats {
                row_count: 1,
                non_null_rows: 1,
                total_grid_pixels: 4,
                total_band_pixels: 4,
                input_wkb_bytes: 64,
                band_pixels: vec![4].into_boxed_slice(),
                band_rows: vec![1].into_boxed_slice(),
                reclass_output_wkb_bytes: [49, 54, 64],
                work_rows: vec![ResidentRasterWorkRow {
                    grid_pixels: 4,
                    exact_wkb_bytes: 64,
                    source_pixel_width: 4,
                    action: RESIDENT_RASTER_WORK_RECLASS,
                    reserved: [0; 6],
                }]
                .into_boxed_slice(),
            }
        );
    }

    #[test]
    fn raster_canonical_zero_rejects_negative_zero_bits() {
        let mut absent_nodata = one_band_raster();
        absent_nodata.bands[0].nodata = -0.0;
        assert!(absent_nodata.validate(MAX_EXACT_VALUE_BYTES).is_err());

        let null = ResidentRasterData {
            pixels: Vec::new(),
            band_offsets: vec![0],
            rows: vec![ResidentRasterRow::default()],
            bands: Vec::new(),
            nulls: Some(vec![1]),
            exact: exact(vec![0, 0], Vec::new()),
        };
        null.validate(MAX_EXACT_VALUE_BYTES)
            .expect("canonical NULL raster is accepted");

        let mut negative_zero = null;
        negative_zero.rows[0].scale_x = -0.0;
        assert!(negative_zero.validate(MAX_EXACT_VALUE_BYTES).is_err());
    }

    #[test]
    fn raster_stats_distinguish_zero_pixel_band_from_missing_band() {
        let raster = ResidentRasterData {
            pixels: Vec::new(),
            band_offsets: vec![0, 0],
            rows: vec![
                ResidentRasterRow {
                    first_band: 0,
                    band_count: 1,
                    scale_x: 1.0,
                    scale_y: -1.0,
                    ..ResidentRasterRow::default()
                },
                ResidentRasterRow {
                    first_band: 1,
                    scale_x: 1.0,
                    scale_y: -1.0,
                    ..ResidentRasterRow::default()
                },
            ],
            bands: vec![ResidentRasterBand {
                pixel_type: RESIDENT_RASTER_FLOAT32,
                ..ResidentRasterBand::default()
            }],
            nulls: None,
            exact: exact(vec![0, 64, 128], vec![0; 128]),
        };
        raster
            .validate(MAX_EXACT_VALUE_BYTES)
            .expect("zero-dimensional raster rows validate");
        let stats = raster.stats().expect("statistics are exact");
        assert_eq!(stats.non_null_rows, 2);
        assert_eq!(stats.selected_band_pixels(1), Some(0));
        assert_eq!(stats.selected_band_rows(1), Some(1));
        assert_eq!(stats.work_rows[0].action, RESIDENT_RASTER_WORK_RECLASS);
        assert_eq!(stats.work_rows[1].action, RESIDENT_RASTER_WORK_MISSING_BAND);
        assert_eq!(stats.reclass_output_wkb_bytes(4), Some(125));
        assert_eq!(stats.reclass_output_wkb_bytes(5), Some(126));
        assert_eq!(stats.reclass_output_wkb_bytes(7), Some(128));
    }

    #[test]
    fn malformed_offsets_tags_and_nulls_fail_closed() {
        let h3 = ResidentH3Lane {
            values: vec![1],
            nulls: Some(vec![2]),
        };
        assert!(h3.validate().is_err());

        let malformed_exact = exact(vec![0, 3], vec![1, 2]);
        assert!(
            malformed_exact
                .validate(1, None, MAX_EXACT_VALUE_BYTES)
                .is_err()
        );

        let geometry = ResidentGeometryData {
            coordinates: vec![1.0, 2.0],
            bboxes: vec![[1.0, 2.0, 1.0, 2.0]],
            geometry_offsets: vec![0, 1],
            ring_offsets: Vec::new(),
            rows: vec![ResidentGeometryRow {
                geom_type: 99,
                flags: RESIDENT_GEOMETRY_BBOX_VALID,
                ..ResidentGeometryRow::default()
            }],
            nulls: None,
            exact: exact(vec![0, 1], vec![1]),
        };
        assert!(geometry.validate(MAX_EXACT_VALUE_BYTES).is_err());

        let raster = ResidentRasterData {
            pixels: vec![0; 4],
            band_offsets: vec![0, 4],
            rows: vec![ResidentRasterRow {
                width: 1,
                height: 1,
                first_band: 0,
                band_count: 1,
                ..ResidentRasterRow::default()
            }],
            bands: vec![ResidentRasterBand {
                pixel_type: 9,
                ..ResidentRasterBand::default()
            }],
            nulls: None,
            exact: exact(vec![0, 1], vec![1]),
        };
        assert!(raster.validate(MAX_EXACT_VALUE_BYTES).is_err());
    }
}

//! Strict PostGIS geometry staging and device-resident ownership.

use crate::gpu::ExprDeviceBuffer;

use super::domain::{
    RESIDENT_GEOMETRY_BBOX_VALID, RESIDENT_GEOMETRY_LINESTRING, RESIDENT_GEOMETRY_POINT,
    RESIDENT_GEOMETRY_POLYGON, ResidentByteAccounting, ResidentGeometryData, ResidentGeometryRow,
    RetainedExactValues,
};

const GSERIALIZED_HEADER_BYTES: usize = 8;
const GSERIALIZED_BBOX_BYTES_2D: usize = 4 * std::mem::size_of::<f32>();
const GSERIALIZED_FLAG_Z: u8 = 1 << 0;
const GSERIALIZED_FLAG_M: u8 = 1 << 1;
const GSERIALIZED_FLAG_BBOX: u8 = 1 << 2;
const GSERIALIZED_FLAG_GEODETIC: u8 = 1 << 3;
const GSERIALIZED_FLAG_EXTENDED: u8 = 1 << 4;
const GSERIALIZED_FLAG_VERSION_2: u8 = 1 << 6;
const GSERIALIZED_SUPPORTED_FLAGS: u8 = GSERIALIZED_FLAG_BBOX | GSERIALIZED_FLAG_VERSION_2;

#[derive(Debug)]
struct ParsedGeometry {
    geom_type: u32,
    srid: i32,
    coordinates: Vec<f64>,
    ring_offsets: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentGeometryValueMetadata {
    pub geom_type: u32,
    pub srid: i32,
    pub coordinate_pairs: usize,
}

impl ParsedGeometry {
    fn coordinate_pairs(&self) -> usize {
        self.coordinates.len() / 2
    }

    fn bbox(&self) -> Result<[f64; 4], String> {
        if self.coordinates.is_empty() {
            return Ok([0.0; 4]);
        }
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for coordinate in self.coordinates.chunks_exact(2) {
            let [x, y] = [coordinate[0], coordinate[1]];
            if !x.is_finite() || !y.is_finite() {
                return Err("geometry contains a non-finite coordinate".to_owned());
            }
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        Ok([min_x, min_y, max_x, max_y])
    }
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32, String> {
    let end = offset
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| format!("{label} offset overflow"))?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| format!("geometry is truncated before {label}"))?;
    Ok(u32::from_le_bytes(
        raw.try_into()
            .map_err(|_| format!("invalid {label} width"))?,
    ))
}

fn read_coordinate_pairs(
    bytes: &[u8],
    offset: usize,
    pair_count: usize,
) -> Result<(Vec<f64>, usize), String> {
    let scalar_count = pair_count
        .checked_mul(2)
        .ok_or_else(|| "geometry coordinate count overflow".to_owned())?;
    let byte_count = scalar_count
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| "geometry coordinate byte count overflow".to_owned())?;
    let end = offset
        .checked_add(byte_count)
        .ok_or_else(|| "geometry coordinate end overflow".to_owned())?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| "geometry coordinate payload is truncated".to_owned())?;
    let mut coordinates = Vec::new();
    coordinates
        .try_reserve_exact(scalar_count)
        .map_err(|error| format!("geometry coordinate allocation failed: {error}"))?;
    for scalar in raw.chunks_exact(std::mem::size_of::<f64>()) {
        let value = f64::from_le_bytes(
            scalar
                .try_into()
                .map_err(|_| "invalid geometry coordinate width".to_owned())?,
        );
        if !value.is_finite() {
            return Err("geometry contains a non-finite coordinate".to_owned());
        }
        coordinates.push(value);
    }
    Ok((coordinates, end))
}

fn decode_srid(bytes: &[u8]) -> Result<i32, String> {
    let srid_bytes = bytes
        .get(4..7)
        .ok_or_else(|| "geometry is truncated before SRID".to_owned())?;
    let raw = (u32::from(srid_bytes[0]) << 16)
        | (u32::from(srid_bytes[1]) << 8)
        | u32::from(srid_bytes[2]);
    let signed = i32::try_from(if raw & 0x10_0000 == 0 {
        i64::from(raw)
    } else {
        i64::from(raw) - (1_i64 << 21)
    })
    .map_err(|_| "geometry SRID sign extension failed".to_owned())?;
    if !(0..=999_999).contains(&signed) {
        return Err(format!(
            "geometry SRID {signed} is outside the resident 2D domain"
        ));
    }
    Ok(signed)
}

fn parse_gserialized(bytes: &[u8], max_exact_value_bytes: usize) -> Result<ParsedGeometry, String> {
    if bytes.is_empty() || bytes.len() > max_exact_value_bytes {
        return Err(format!(
            "geometry exact value length {} is outside 1..={max_exact_value_bytes}",
            bytes.len()
        ));
    }
    let flags = *bytes
        .get(7)
        .ok_or_else(|| "geometry is shorter than the GSERIALIZED header".to_owned())?;
    if flags
        & (GSERIALIZED_FLAG_Z
            | GSERIALIZED_FLAG_M
            | GSERIALIZED_FLAG_GEODETIC
            | GSERIALIZED_FLAG_EXTENDED)
        != 0
        || flags & !GSERIALIZED_SUPPORTED_FLAGS != 0
    {
        return Err(format!(
            "geometry flags {flags:#04x} are not a supported non-geodetic 2D layout"
        ));
    }
    let srid = decode_srid(bytes)?;
    let geometry_start = GSERIALIZED_HEADER_BYTES
        .checked_add(if flags & GSERIALIZED_FLAG_BBOX != 0 {
            GSERIALIZED_BBOX_BYTES_2D
        } else {
            0
        })
        .ok_or_else(|| "geometry header byte count overflow".to_owned())?;
    let geom_type = read_u32(bytes, geometry_start, "geometry type")?;
    let count_offset = geometry_start
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| "geometry type offset overflow".to_owned())?;

    let (coordinates, ring_offsets, end) = match geom_type {
        RESIDENT_GEOMETRY_POINT => {
            let point_count = usize::try_from(read_u32(bytes, count_offset, "point count")?)
                .map_err(|_| "point count exceeds usize".to_owned())?;
            if point_count > 1 {
                return Err("resident POINT contains more than one coordinate".to_owned());
            }
            let coordinates_offset = count_offset
                .checked_add(std::mem::size_of::<u32>())
                .ok_or_else(|| "point coordinate offset overflow".to_owned())?;
            let (coordinates, end) = read_coordinate_pairs(bytes, coordinates_offset, point_count)?;
            (coordinates, Vec::new(), end)
        }
        RESIDENT_GEOMETRY_LINESTRING => {
            let point_count = usize::try_from(read_u32(bytes, count_offset, "line point count")?)
                .map_err(|_| "line point count exceeds usize".to_owned())?;
            if point_count == 1 {
                return Err("resident LINESTRING contains exactly one coordinate".to_owned());
            }
            let coordinates_offset = count_offset
                .checked_add(std::mem::size_of::<u32>())
                .ok_or_else(|| "line coordinate offset overflow".to_owned())?;
            let (coordinates, end) = read_coordinate_pairs(bytes, coordinates_offset, point_count)?;
            (coordinates, Vec::new(), end)
        }
        RESIDENT_GEOMETRY_POLYGON => {
            let ring_count = usize::try_from(read_u32(bytes, count_offset, "polygon ring count")?)
                .map_err(|_| "polygon ring count exceeds usize".to_owned())?;
            let ring_counts_offset = count_offset
                .checked_add(std::mem::size_of::<u32>())
                .ok_or_else(|| "polygon ring-count offset overflow".to_owned())?;
            let mut point_counts = Vec::new();
            point_counts
                .try_reserve_exact(ring_count)
                .map_err(|error| format!("polygon ring allocation failed: {error}"))?;
            let mut total_points = 0_usize;
            for ring in 0..ring_count {
                let offset = ring
                    .checked_mul(std::mem::size_of::<u32>())
                    .and_then(|offset| ring_counts_offset.checked_add(offset))
                    .ok_or_else(|| "polygon ring-count offset overflow".to_owned())?;
                let point_count = usize::try_from(read_u32(bytes, offset, "polygon point count")?)
                    .map_err(|_| "polygon point count exceeds usize".to_owned())?;
                if point_count < 4 {
                    return Err(format!(
                        "resident polygon ring {ring} contains fewer than four closed coordinates"
                    ));
                }
                total_points = total_points
                    .checked_add(point_count)
                    .ok_or_else(|| "polygon coordinate count overflow".to_owned())?;
                point_counts.push(point_count);
            }
            let ring_count_bytes = ring_count
                .checked_mul(std::mem::size_of::<u32>())
                .ok_or_else(|| "polygon ring-count byte length overflow".to_owned())?;
            let padding = if ring_count % 2 == 0 {
                0
            } else {
                std::mem::size_of::<u32>()
            };
            let coordinates_offset = ring_counts_offset
                .checked_add(ring_count_bytes)
                .and_then(|offset| offset.checked_add(padding))
                .ok_or_else(|| "polygon coordinate offset overflow".to_owned())?;
            let (coordinates, end) =
                read_coordinate_pairs(bytes, coordinates_offset, total_points)?;
            let mut ring_offsets = Vec::new();
            ring_offsets
                .try_reserve_exact(ring_count)
                .map_err(|error| format!("polygon ring-offset allocation failed: {error}"))?;
            let mut start = 0_usize;
            for (ring, point_count) in point_counts.into_iter().enumerate() {
                ring_offsets.push(start);
                let ring_end = start
                    .checked_add(point_count)
                    .ok_or_else(|| "polygon ring end overflow".to_owned())?;
                let first = coordinates
                    .get(start * 2..start * 2 + 2)
                    .ok_or_else(|| format!("polygon ring {ring} start is invalid"))?;
                let last_start = ring_end
                    .checked_sub(1)
                    .and_then(|index| index.checked_mul(2))
                    .ok_or_else(|| format!("polygon ring {ring} end is invalid"))?;
                let last = coordinates
                    .get(last_start..last_start + 2)
                    .ok_or_else(|| format!("polygon ring {ring} end is invalid"))?;
                if first != last {
                    return Err(format!("resident polygon ring {ring} is not closed"));
                }
                start = ring_end;
            }
            (coordinates, ring_offsets, end)
        }
        other => {
            return Err(format!(
                "geometry type tag {other} is not POINT, LINESTRING, or POLYGON"
            ));
        }
    };
    if end != bytes.len() {
        return Err(format!(
            "geometry has {} trailing bytes after its canonical 2D payload",
            bytes.len() - end
        ));
    }
    Ok(ParsedGeometry {
        geom_type,
        srid,
        coordinates,
        ring_offsets,
    })
}

pub fn validate_resident_geometry_value(
    bytes: &[u8],
    max_exact_value_bytes: usize,
) -> Result<ResidentGeometryValueMetadata, String> {
    let parsed = parse_gserialized(bytes, max_exact_value_bytes)?;
    Ok(ResidentGeometryValueMetadata {
        geom_type: parsed.geom_type,
        srid: parsed.srid,
        coordinate_pairs: parsed.coordinate_pairs(),
    })
}

/// Host-side builder for one exact resident geometry column.
pub(super) struct ResidentGeometryBuilder {
    coordinates: Vec<f64>,
    bboxes: Vec<[f64; 4]>,
    geometry_offsets: Vec<u64>,
    ring_offsets: Vec<u64>,
    rows: Vec<ResidentGeometryRow>,
    nulls: Vec<u8>,
    exact_offsets: Vec<u64>,
    exact_bytes: Vec<u8>,
    saw_null: bool,
    max_exact_value_bytes: usize,
    max_vertices_per_row: usize,
}

impl ResidentGeometryBuilder {
    pub(super) fn new(max_exact_value_bytes: usize, max_vertices_per_row: usize) -> Self {
        Self {
            coordinates: Vec::new(),
            bboxes: Vec::new(),
            geometry_offsets: vec![0],
            ring_offsets: Vec::new(),
            rows: Vec::new(),
            nulls: Vec::new(),
            exact_offsets: vec![0],
            exact_bytes: Vec::new(),
            saw_null: false,
            max_exact_value_bytes,
            max_vertices_per_row,
        }
    }

    pub(super) fn try_reserve_rows(&mut self, additional: usize) -> Result<(), String> {
        let reserve = |result: Result<(), std::collections::TryReserveError>| {
            result.map_err(|error| format!("resident geometry staging allocation failed: {error}"))
        };
        reserve(self.bboxes.try_reserve(additional))?;
        reserve(self.rows.try_reserve(additional))?;
        reserve(self.nulls.try_reserve(additional))?;
        reserve(self.geometry_offsets.try_reserve(additional))?;
        reserve(self.exact_offsets.try_reserve(additional))
    }

    pub(super) fn push(&mut self, exact: Option<Vec<u8>>) -> Result<(), String> {
        let Some(exact) = exact else {
            self.bboxes.push([0.0; 4]);
            self.rows.push(ResidentGeometryRow::default());
            self.nulls.push(1);
            self.saw_null = true;
            self.geometry_offsets.push(
                u64::try_from(self.coordinates.len() / 2)
                    .map_err(|_| "geometry coordinate count exceeds u64".to_owned())?,
            );
            self.exact_offsets.push(
                u64::try_from(self.exact_bytes.len())
                    .map_err(|_| "geometry exact-byte count exceeds u64".to_owned())?,
            );
            return Ok(());
        };

        let parsed = parse_gserialized(&exact, self.max_exact_value_bytes)?;
        if parsed.coordinate_pairs() > self.max_vertices_per_row {
            return Err(format!(
                "resident geometry has {} coordinate pairs; device maximum is {}",
                parsed.coordinate_pairs(),
                self.max_vertices_per_row,
            ));
        }
        let bbox = parsed.bbox()?;
        let coordinate_base = self.coordinates.len() / 2;
        let ring_base = self.ring_offsets.len();
        self.coordinates
            .try_reserve(parsed.coordinates.len())
            .map_err(|error| format!("resident geometry coordinate allocation failed: {error}"))?;
        self.ring_offsets
            .try_reserve(parsed.ring_offsets.len())
            .map_err(|error| format!("resident geometry ring allocation failed: {error}"))?;
        self.exact_bytes
            .try_reserve(exact.len())
            .map_err(|error| format!("resident geometry exact-byte allocation failed: {error}"))?;

        for offset in &parsed.ring_offsets {
            let global = coordinate_base
                .checked_add(*offset)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| "geometry global ring offset overflow".to_owned())?;
            self.ring_offsets.push(global);
        }
        let coordinate_pairs = parsed.coordinate_pairs();
        self.coordinates.extend(parsed.coordinates);
        self.bboxes.push(bbox);
        self.rows.push(ResidentGeometryRow {
            geom_type: parsed.geom_type,
            srid: parsed.srid,
            first_ring: u64::try_from(ring_base)
                .map_err(|_| "geometry ring count exceeds u64".to_owned())?,
            ring_count: u32::try_from(parsed.ring_offsets.len())
                .map_err(|_| "geometry row ring count exceeds u32".to_owned())?,
            flags: if coordinate_pairs == 0 {
                0
            } else {
                RESIDENT_GEOMETRY_BBOX_VALID
            },
        });
        self.nulls.push(0);
        let coordinate_end = coordinate_base
            .checked_add(coordinate_pairs)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| "geometry coordinate end exceeds u64".to_owned())?;
        self.geometry_offsets.push(coordinate_end);
        self.exact_bytes.extend(exact);
        self.exact_offsets.push(
            u64::try_from(self.exact_bytes.len())
                .map_err(|_| "geometry exact-byte count exceeds u64".to_owned())?,
        );
        Ok(())
    }

    pub(super) fn finish(self) -> Result<ResidentGeometryData, String> {
        let data = ResidentGeometryData {
            coordinates: self.coordinates,
            bboxes: self.bboxes,
            geometry_offsets: self.geometry_offsets,
            ring_offsets: self.ring_offsets,
            rows: self.rows,
            nulls: self.saw_null.then_some(self.nulls),
            exact: RetainedExactValues {
                offsets: self.exact_offsets.into_boxed_slice(),
                bytes: self.exact_bytes.into_boxed_slice(),
            },
        };
        data.validate(self.max_exact_value_bytes)
            .map_err(|error| error.to_string())?;
        Ok(data)
    }
}

fn copy_optional<T>(values: &[T], label: &str) -> Result<Option<ExprDeviceBuffer<T>>, String> {
    if values.is_empty() {
        Ok(None)
    } else {
        ExprDeviceBuffer::copy_from_slice(values)
            .map(Some)
            .ok_or_else(|| format!("device allocation/copy failed for {label}"))
    }
}

/// Host-only prefix sums of the native per-row referenced-byte charge.
/// Keeping this alongside the exact values avoids device-to-host reads while
/// constructing a request budget for a resident row slice.
pub(super) struct ResidentGeometryReferencedBytes {
    prefix: Box<[u64]>,
}

impl ResidentGeometryReferencedBytes {
    pub(super) fn build(data: &ResidentGeometryData) -> Result<Self, String> {
        let mut prefix = Vec::new();
        prefix
            .try_reserve_exact(data.rows.len().saturating_add(1))
            .map_err(|error| format!("geometry byte-prefix allocation failed: {error}"))?;
        prefix.push(0_u64);
        let has_null_sidecar = data.nulls.is_some();
        for (index, row) in data.rows.iter().enumerate() {
            let coordinate_pairs = data.geometry_offsets[index + 1]
                .checked_sub(data.geometry_offsets[index])
                .ok_or_else(|| "geometry byte-prefix offsets are not ordered".to_owned())?;
            let row_bytes = referenced_geometry_bytes(
                coordinate_pairs,
                u64::from(row.ring_count),
                1,
                has_null_sidecar,
            )
            .ok_or_else(|| "geometry referenced-byte prefix overflow".to_owned())?;
            let total = prefix
                .last()
                .copied()
                .and_then(|total| total.checked_add(row_bytes))
                .ok_or_else(|| "geometry referenced-byte prefix overflow".to_owned())?;
            prefix.push(total);
        }
        Ok(Self {
            prefix: prefix.into_boxed_slice(),
        })
    }

    pub(super) fn accounting_bytes(&self) -> Result<u64, String> {
        self.prefix
            .len()
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| "geometry byte-prefix accounting overflow".to_owned())
    }

    fn referenced_bytes(&self, first_row: usize, row_count: usize) -> Option<u64> {
        let end_row = first_row.checked_add(row_count)?;
        self.prefix
            .get(end_row)?
            .checked_sub(*self.prefix.get(first_row)?)
    }
}

/// Store-owned device buffers plus exact host values for a geometry column.
pub struct ResidentGeometryColumn {
    coordinates: Option<ExprDeviceBuffer<f64>>,
    bboxes: Option<ExprDeviceBuffer<[f64; 4]>>,
    geometry_offsets: ExprDeviceBuffer<u64>,
    ring_offsets: Option<ExprDeviceBuffer<u64>>,
    rows: Option<ExprDeviceBuffer<ResidentGeometryRow>>,
    nulls: Option<ExprDeviceBuffer<u8>>,
    exact: RetainedExactValues,
    referenced_bytes: ResidentGeometryReferencedBytes,
    accounting: ResidentByteAccounting,
    row_count: usize,
    coordinate_pair_count: usize,
    ring_count: usize,
}

impl ResidentGeometryColumn {
    pub(super) fn materialize(
        data: ResidentGeometryData,
        referenced_bytes: ResidentGeometryReferencedBytes,
        max_exact_value_bytes: usize,
        label: &str,
    ) -> Result<Self, String> {
        let mut accounting = data
            .accounting(max_exact_value_bytes)
            .map_err(|error| error.to_string())?;
        accounting.retained_host_exact_bytes = accounting
            .retained_host_exact_bytes
            .checked_add(referenced_bytes.accounting_bytes()?)
            .ok_or_else(|| "geometry retained-host accounting overflow".to_owned())?;
        let row_count = data.rows.len();
        let coordinate_pair_count = data.coordinates.len() / 2;
        let ring_count = data.ring_offsets.len();
        let coordinates = copy_optional(&data.coordinates, &format!("{label} coordinates"))?;
        let bboxes = copy_optional(&data.bboxes, &format!("{label} bboxes"))?;
        let geometry_offsets = ExprDeviceBuffer::copy_from_slice(&data.geometry_offsets)
            .ok_or_else(|| format!("device allocation/copy failed for {label} row offsets"))?;
        let ring_offsets = copy_optional(&data.ring_offsets, &format!("{label} ring offsets"))?;
        let rows = copy_optional(&data.rows, &format!("{label} row metadata"))?;
        let nulls = data
            .nulls
            .as_deref()
            .map(|values| copy_optional(values, &format!("{label} NULL sidecar")))
            .transpose()?
            .flatten();
        Ok(Self {
            coordinates,
            bboxes,
            geometry_offsets,
            ring_offsets,
            rows,
            nulls,
            exact: data.exact,
            referenced_bytes,
            accounting,
            row_count,
            coordinate_pair_count,
            ring_count,
        })
    }

    #[must_use]
    pub const fn accounting(&self) -> ResidentByteAccounting {
        self.accounting
    }

    #[must_use]
    pub fn view(&self) -> ResidentGeometryColumnView<'_> {
        ResidentGeometryColumnView {
            coordinates: self.coordinates.as_ref(),
            bboxes: self.bboxes.as_ref(),
            geometry_offsets: &self.geometry_offsets,
            ring_offsets: self.ring_offsets.as_ref(),
            rows: self.rows.as_ref(),
            nulls: self.nulls.as_ref(),
            exact: &self.exact,
            referenced_byte_prefix: &self.referenced_bytes,
            accounting: self.accounting,
            row_count: self.row_count,
            coordinate_pair_count: self.coordinate_pair_count,
            ring_count: self.ring_count,
        }
    }
}

/// Borrowed resident geometry lanes. Pointers may not escape the store callback.
#[derive(Clone, Copy)]
pub struct ResidentGeometryColumnView<'a> {
    pub coordinates: Option<&'a ExprDeviceBuffer<f64>>,
    pub bboxes: Option<&'a ExprDeviceBuffer<[f64; 4]>>,
    pub geometry_offsets: &'a ExprDeviceBuffer<u64>,
    pub ring_offsets: Option<&'a ExprDeviceBuffer<u64>>,
    pub rows: Option<&'a ExprDeviceBuffer<ResidentGeometryRow>>,
    pub nulls: Option<&'a ExprDeviceBuffer<u8>>,
    pub exact: &'a RetainedExactValues,
    referenced_byte_prefix: &'a ResidentGeometryReferencedBytes,
    pub accounting: ResidentByteAccounting,
    pub row_count: usize,
    pub coordinate_pair_count: usize,
    pub ring_count: usize,
}

impl ResidentGeometryColumnView<'_> {
    #[must_use]
    pub fn exact_value(&self, row_index: usize) -> Option<&[u8]> {
        self.exact.value(row_index)
    }

    #[must_use]
    pub fn referenced_bytes(&self, first_row: usize, row_count: usize) -> Option<u64> {
        self.referenced_byte_prefix
            .referenced_bytes(first_row, row_count)
    }
}

fn referenced_geometry_bytes(
    coordinate_pairs: u64,
    ring_count: u64,
    row_count: u64,
    has_null_sidecar: bool,
) -> Option<u64> {
    let coordinate_bytes = coordinate_pairs
        .checked_mul(2_u64.checked_mul(u64::try_from(std::mem::size_of::<f64>()).ok()?)?)?;
    let ring_bytes = ring_count.checked_mul(u64::try_from(std::mem::size_of::<u64>()).ok()?)?;
    let fixed_per_row = u64::try_from(std::mem::size_of::<ResidentGeometryRow>())
        .ok()?
        .checked_add(4_u64.checked_mul(u64::try_from(std::mem::size_of::<f64>()).ok()?)?)?
        .checked_add(2_u64.checked_mul(u64::try_from(std::mem::size_of::<u64>()).ok()?)?)?
        .checked_add(u64::from(has_null_sidecar))?;
    coordinate_bytes
        .checked_add(ring_bytes)?
        .checked_add(row_count.checked_mul(fixed_per_row)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(srid: u32, flags: u8) -> Vec<u8> {
        vec![
            0,
            0,
            0,
            0,
            ((srid >> 16) & 0xff) as u8,
            ((srid >> 8) & 0xff) as u8,
            (srid & 0xff) as u8,
            flags,
        ]
    }

    fn point(srid: u32, value: Option<(f64, f64)>) -> Vec<u8> {
        let mut bytes = header(srid, 0);
        bytes.extend_from_slice(&RESIDENT_GEOMETRY_POINT.to_le_bytes());
        bytes.extend_from_slice(&u32::from(value.is_some()).to_le_bytes());
        if let Some((x, y)) = value {
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        bytes
    }

    fn polygon(rings: &[&[(f64, f64)]]) -> Vec<u8> {
        let mut bytes = header(4_326, 0);
        bytes.extend_from_slice(&RESIDENT_GEOMETRY_POLYGON.to_le_bytes());
        bytes.extend_from_slice(&(rings.len() as u32).to_le_bytes());
        for ring in rings {
            bytes.extend_from_slice(&(ring.len() as u32).to_le_bytes());
        }
        if rings.len() % 2 == 1 {
            bytes.extend_from_slice(&0_u32.to_le_bytes());
        }
        for ring in rings {
            for (x, y) in *ring {
                bytes.extend_from_slice(&x.to_le_bytes());
                bytes.extend_from_slice(&y.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn parser_preserves_fp64_point_and_canonical_empty() {
        let mut encoded = point(4_326, Some((1.25, -2.5)));
        encoded[7] = GSERIALIZED_FLAG_VERSION_2;
        let parsed = parse_gserialized(&encoded, 1_024).expect("version-2 point parses");
        assert_eq!(parsed.geom_type, RESIDENT_GEOMETRY_POINT);
        assert_eq!(parsed.srid, 4_326);
        assert_eq!(parsed.coordinates, [1.25, -2.5]);
        assert_eq!(parsed.bbox().expect("bbox"), [1.25, -2.5, 1.25, -2.5]);

        let empty = parse_gserialized(&point(4_326, None), 1_024).expect("empty point parses");
        assert!(empty.coordinates.is_empty());
        assert_eq!(empty.bbox().expect("empty bbox"), [0.0; 4]);
    }

    #[test]
    fn parser_preserves_polygon_ring_ownership_and_requires_closure() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)];
        let inner = [(2.0, 2.0), (3.0, 2.0), (2.0, 3.0), (2.0, 2.0)];
        let parsed =
            parse_gserialized(&polygon(&[&outer, &inner]), 4_096).expect("closed polygon parses");
        assert_eq!(parsed.ring_offsets, [0, 4]);
        assert_eq!(parsed.coordinate_pairs(), 8);

        let single = parse_gserialized(&polygon(&[&outer]), 4_096)
            .expect("odd ring count includes serializer alignment padding");
        assert_eq!(single.ring_offsets, [0]);
        assert_eq!(single.coordinate_pairs(), 4);

        let open = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (1.0, 1.0)];
        assert!(parse_gserialized(&polygon(&[&open]), 4_096).is_err());
    }

    #[test]
    fn parser_rejects_dimensions_geography_nonfinite_and_trailing_bytes() {
        for flag in [
            GSERIALIZED_FLAG_Z,
            GSERIALIZED_FLAG_M,
            GSERIALIZED_FLAG_GEODETIC,
            GSERIALIZED_FLAG_EXTENDED,
            1 << 7,
        ] {
            let mut bytes = point(4_326, Some((1.0, 2.0)));
            bytes[7] = flag;
            assert!(parse_gserialized(&bytes, 1_024).is_err());
        }
        assert!(parse_gserialized(&point(4_326, Some((f64::NAN, 2.0))), 1_024).is_err());
        let mut trailing = point(4_326, Some((1.0, 2.0)));
        trailing.push(0);
        assert!(parse_gserialized(&trailing, 1_024).is_err());
    }

    #[test]
    fn builder_retains_exact_values_and_canonical_nulls() {
        let first = point(4_326, Some((1.0, 2.0)));
        let empty = point(4_326, None);
        let mut builder = ResidentGeometryBuilder::new(1_024, 16);
        builder.push(Some(first.clone())).expect("first point");
        builder.push(None).expect("NULL point");
        builder.push(Some(empty.clone())).expect("empty point");
        let data = builder.finish().expect("valid resident geometry");
        assert_eq!(data.geometry_offsets, [0, 1, 1, 1]);
        assert_eq!(data.nulls.as_deref(), Some([0, 1, 0].as_slice()));
        assert_eq!(data.exact.value(0), Some(first.as_slice()));
        assert_eq!(data.exact.value(1), Some([].as_slice()));
        assert_eq!(data.exact.value(2), Some(empty.as_slice()));
        assert_eq!(data.rows[1], ResidentGeometryRow::default());
        assert_eq!(data.bboxes[1], [0.0; 4]);
        assert_eq!(data.bboxes[2], [0.0; 4]);
    }

    #[test]
    fn builder_rejects_a_row_above_the_device_vertex_limit() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)];
        let mut builder = ResidentGeometryBuilder::new(1_024, 3);
        let error = builder
            .push(Some(polygon(&[&outer])))
            .expect_err("four coordinate pairs exceed the per-row limit");
        assert!(error.contains("device maximum is 3"));
    }

    #[test]
    fn referenced_byte_budget_matches_native_row_accounting() {
        assert_eq!(
            referenced_geometry_bytes(4, 1, 1, false),
            Some(24 + 32 + 16 + 64 + 8),
        );
        assert_eq!(
            referenced_geometry_bytes(9, 3, 2, true),
            Some(2 * (24 + 32 + 16 + 1) + 9 * 16 + 3 * 8),
        );
        assert_eq!(referenced_geometry_bytes(u64::MAX, 0, 1, false), None);
    }

    #[test]
    fn referenced_byte_slices_use_host_prefix_without_device_readback() {
        let outer = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)];
        let mut builder = ResidentGeometryBuilder::new(4_096, 16);
        builder.push(Some(polygon(&[&outer]))).expect("polygon row");
        builder.push(None).expect("NULL row");
        builder
            .push(Some(point(4_326, Some((1.0, 2.0)))))
            .expect("point row");
        let data = builder.finish().expect("valid geometry data");
        let referenced = ResidentGeometryReferencedBytes::build(&data)
            .expect("host prefix builds before materialization");

        assert_eq!(referenced.prefix.as_ref(), [0, 145, 218, 307]);
        assert_eq!(referenced.referenced_bytes(0, 2), Some(218));
        assert_eq!(referenced.referenced_bytes(1, 2), Some(162));
        assert_eq!(referenced.referenced_bytes(3, 1), None);
        assert_eq!(referenced.accounting_bytes(), Ok(32));
    }
}

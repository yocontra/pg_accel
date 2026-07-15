//! Pure preflight and exact WKB reconstruction for resident raster Reclass.

use crate::engine::residency::{
    RESIDENT_RASTER_WORK_MISSING_BAND, RESIDENT_RASTER_WORK_NULL, RESIDENT_RASTER_WORK_RECLASS,
    ResidentRasterStats, ResidentRasterWorkRow, RetainedExactValues,
};

use super::{RasterPixelType, RasterQuerySpec, RasterSpecError};

const RASTER_WKB_HEADER_BYTES: usize = 61;
const NATIVE_RECLASS_RULE_BYTES: u64 = 16;
const NATIVE_VALIDATION_SCRATCH_BYTES: u64 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterExecutionError {
    InvalidSpec(RasterSpecError),
    InvalidSnapshot(&'static str),
    InvalidRow {
        row: usize,
        reason: &'static str,
    },
    ByteCountOverflow,
    AllocationFailed(&'static str),
    OutputPixelLength {
        expected: usize,
        actual: usize,
    },
    RowActionLength {
        expected: usize,
        actual: usize,
    },
    RowActionMismatch {
        row: usize,
        expected: u8,
        actual: u8,
    },
}

impl std::fmt::Display for RasterExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RasterExecutionError {}

/// Owned host facts copied during the short resident-metadata borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterExecutionSnapshot {
    pub stats: ResidentRasterStats,
    pub exact: RetainedExactValues,
}

/// Exact logical allocation sizes for one execution.
///
/// Persistent and transient categories remain separate. The residency store
/// must not report the post-launch coexistence peak as published artifact
/// bytes; it reserves that peak as a temporary delta and releases the delta
/// after reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterExecutionAccounting {
    pub snapshot_host_bytes: u64,
    pub layout_host_bytes: u64,
    pub device_artifact_bytes: u64,
    pub post_launch_host_bytes: u64,
    pub reconstructed_output_bytes: u64,
    pub prelaunch_reserved_bytes: u64,
    pub peak_reserved_bytes: u64,
}

/// Inline result of the first, non-owning resident-store sizing pass. Creating
/// this value performs no allocation and copies no retained WKB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterExecutionSizing {
    pub accounting: RasterExecutionAccounting,
    output_pixel_type: RasterPixelType,
    output_pixel_width: usize,
    row_count: usize,
    total_pixels: usize,
    output_pixels_bytes: usize,
    output_wkb_bytes: usize,
    null_rows: usize,
}

impl RasterExecutionSizing {
    #[must_use]
    pub const fn output_pixel_type(self) -> RasterPixelType {
        self.output_pixel_type
    }

    #[must_use]
    pub const fn output_pixel_width(self) -> usize {
        self.output_pixel_width
    }

    #[must_use]
    pub const fn row_count(self) -> usize {
        self.row_count
    }

    #[must_use]
    pub const fn total_pixels(self) -> usize {
        self.total_pixels
    }

    #[must_use]
    pub const fn output_pixels_bytes(self) -> usize {
        self.output_pixels_bytes
    }

    #[must_use]
    pub const fn output_wkb_bytes(self) -> usize {
        self.output_wkb_bytes
    }

    #[must_use]
    pub const fn null_rows(self) -> usize {
        self.null_rows
    }
}

/// Canonical full-column layout consumed by the resident native ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterExecutionLayout {
    output_pixel_type: RasterPixelType,
    output_pixel_width: usize,
    output_offsets: Box<[u64]>,
    output_wkb_offsets: Box<[u64]>,
    total_pixels: usize,
    output_pixels_bytes: usize,
    null_rows: usize,
}

impl RasterExecutionLayout {
    #[must_use]
    pub const fn output_pixel_type(&self) -> RasterPixelType {
        self.output_pixel_type
    }

    #[must_use]
    pub const fn output_pixel_width(&self) -> usize {
        self.output_pixel_width
    }

    /// Global output byte offsets. A chunk passes its corresponding subslice,
    /// whose first entry may therefore be nonzero.
    #[must_use]
    pub fn output_offsets(&self) -> &[u64] {
        &self.output_offsets
    }

    #[must_use]
    pub fn output_wkb_offsets(&self) -> &[u64] {
        &self.output_wkb_offsets
    }

    #[must_use]
    pub const fn total_pixels(&self) -> usize {
        self.total_pixels
    }

    #[must_use]
    pub const fn output_pixels_bytes(&self) -> usize {
        self.output_pixels_bytes
    }

    #[must_use]
    pub fn output_wkb_bytes(&self) -> usize {
        usize::try_from(self.output_wkb_offsets.last().copied().unwrap_or(0)).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.output_offsets.len().saturating_sub(1)
    }

    #[must_use]
    pub const fn requires_launch(&self) -> bool {
        self.output_offsets.len() > 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterExecutionPreflight {
    pub snapshot: RasterExecutionSnapshot,
    pub layout: RasterExecutionLayout,
    pub accounting: RasterExecutionAccounting,
}

/// Packed reconstructed values. NULL rows have equal adjacent offsets and a
/// canonical `1` in the optional NULL sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterReconstructedOutput {
    pub exact: RetainedExactValues,
    pub nulls: Option<Box<[u8]>>,
}

impl RasterReconstructedOutput {
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.exact.offsets.len().saturating_sub(1)
    }

    #[must_use]
    pub fn is_null(&self, row: usize) -> Option<bool> {
        (row < self.row_count()).then(|| self.nulls.as_ref().is_some_and(|nulls| nulls[row] == 1))
    }

    #[must_use]
    pub fn value(&self, row: usize) -> Option<&[u8]> {
        if self.is_null(row)? {
            None
        } else {
            self.exact.value(row)
        }
    }
}

fn pixel_width(pixel_type: RasterPixelType) -> Option<usize> {
    match pixel_type {
        RasterPixelType::Bool
        | RasterPixelType::UInt2
        | RasterPixelType::UInt4
        | RasterPixelType::Int8
        | RasterPixelType::UInt8 => Some(1),
        RasterPixelType::Int16 | RasterPixelType::UInt16 => Some(2),
        RasterPixelType::Int32 | RasterPixelType::UInt32 => Some(4),
        RasterPixelType::Float32 | RasterPixelType::Float64 => None,
    }
}

fn pixel_width_from_tag(tag: u8) -> Option<usize> {
    match tag {
        0..=4 => Some(1),
        5..=6 => Some(2),
        7..=8 | 10 => Some(4),
        11 => Some(8),
        _ => None,
    }
}

fn bytes_for_len<T>(len: usize) -> Result<u64, RasterExecutionError> {
    len.checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(RasterExecutionError::ByteCountOverflow)
}

fn checked_sum(values: &[u64]) -> Result<u64, RasterExecutionError> {
    values.iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(*value)
            .ok_or(RasterExecutionError::ByteCountOverflow)
    })
}

fn try_vec_with_capacity<T>(
    len: usize,
    label: &'static str,
) -> Result<Vec<T>, RasterExecutionError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| RasterExecutionError::AllocationFailed(label))?;
    Ok(values)
}

fn exact_row(exact: &RetainedExactValues, row: usize) -> Result<&[u8], RasterExecutionError> {
    exact.value(row).ok_or(RasterExecutionError::InvalidRow {
        row,
        reason: "retained exact WKB offsets are invalid",
    })
}

fn read_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let pair: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if little_endian {
        u16::from_le_bytes(pair)
    } else {
        u16::from_be_bytes(pair)
    })
}

#[derive(Debug, Clone, Copy)]
struct ExactWkbShape {
    little_endian: bool,
    band_count: u16,
    grid_pixels: u64,
}

fn exact_wkb_shape(bytes: &[u8], row: usize) -> Result<ExactWkbShape, RasterExecutionError> {
    if bytes.len() < RASTER_WKB_HEADER_BYTES {
        return Err(RasterExecutionError::InvalidRow {
            row,
            reason: "retained raster WKB is shorter than its header",
        });
    }
    let little_endian = match bytes[0] {
        0 => false,
        1 => true,
        _ => {
            return Err(RasterExecutionError::InvalidRow {
                row,
                reason: "retained raster WKB has an invalid endian marker",
            });
        }
    };
    if read_u16(bytes, 1, little_endian) != Some(0) {
        return Err(RasterExecutionError::InvalidRow {
            row,
            reason: "retained raster WKB has an unsupported version",
        });
    }
    let band_count = read_u16(bytes, 3, little_endian).ok_or(RasterExecutionError::InvalidRow {
        row,
        reason: "retained raster WKB band count is truncated",
    })?;
    let width = read_u16(bytes, 57, little_endian).ok_or(RasterExecutionError::InvalidRow {
        row,
        reason: "retained raster WKB width is truncated",
    })?;
    let height = read_u16(bytes, 59, little_endian).ok_or(RasterExecutionError::InvalidRow {
        row,
        reason: "retained raster WKB height is truncated",
    })?;
    let grid_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    Ok(ExactWkbShape {
        little_endian,
        band_count,
        grid_pixels,
    })
}

fn checked_row_layout(
    row: usize,
    work: ResidentRasterWorkRow,
    exact_wkb: &[u8],
    output_width: usize,
) -> Result<(u64, u64), RasterExecutionError> {
    if work.reserved != [0; 6] || u64::try_from(exact_wkb.len()).ok() != Some(work.exact_wkb_bytes)
    {
        return Err(RasterExecutionError::InvalidRow {
            row,
            reason: "resident raster work metadata is not canonical",
        });
    }
    match work.action {
        RESIDENT_RASTER_WORK_NULL => {
            if !exact_wkb.is_empty() || work.grid_pixels != 0 || work.source_pixel_width != 0 {
                return Err(RasterExecutionError::InvalidRow {
                    row,
                    reason: "NULL raster work metadata is not canonical",
                });
            }
            Ok((0, 0))
        }
        RESIDENT_RASTER_WORK_MISSING_BAND => {
            let shape = exact_wkb_shape(exact_wkb, row)?;
            if shape.band_count != 0
                || shape.grid_pixels != work.grid_pixels
                || work.source_pixel_width != 0
                || exact_wkb.len() != RASTER_WKB_HEADER_BYTES
            {
                return Err(RasterExecutionError::InvalidRow {
                    row,
                    reason: "zero-band raster work metadata disagrees with exact WKB",
                });
            }
            Ok((0, work.exact_wkb_bytes))
        }
        RESIDENT_RASTER_WORK_RECLASS => {
            let shape = exact_wkb_shape(exact_wkb, row)?;
            if shape.band_count == 0 || shape.grid_pixels != work.grid_pixels {
                return Err(RasterExecutionError::InvalidRow {
                    row,
                    reason: "band-one raster work metadata disagrees with exact WKB",
                });
            }
            let flags = exact_wkb[RASTER_WKB_HEADER_BYTES];
            let source_width =
                pixel_width_from_tag(flags & 0x0f).ok_or(RasterExecutionError::InvalidRow {
                    row,
                    reason: "band-one raster pixel type is invalid",
                })?;
            if flags & 0x90 != 0
                || (flags & 0x20 != 0 && flags & 0x40 == 0)
                || usize::from(work.source_pixel_width) != source_width
            {
                return Err(RasterExecutionError::InvalidRow {
                    row,
                    reason: "band-one raster flags or pixel width are invalid",
                });
            }
            let serialized_values = work
                .grid_pixels
                .checked_add(1)
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            let source_values = serialized_values
                .checked_mul(u64::from(work.source_pixel_width))
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            let source_end = u64::try_from(RASTER_WKB_HEADER_BYTES + 1)
                .ok()
                .and_then(|prefix| prefix.checked_add(source_values))
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            if source_end > work.exact_wkb_bytes {
                return Err(RasterExecutionError::InvalidRow {
                    row,
                    reason: "band-one raster payload exceeds exact WKB",
                });
            }
            let output_width =
                u64::try_from(output_width).map_err(|_| RasterExecutionError::ByteCountOverflow)?;
            let output_pixel_bytes = work
                .grid_pixels
                .checked_mul(output_width)
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            let output_values = serialized_values
                .checked_mul(output_width)
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            let output_wkb_bytes = work
                .exact_wkb_bytes
                .checked_sub(source_values)
                .and_then(|base| base.checked_add(output_values))
                .ok_or(RasterExecutionError::ByteCountOverflow)?;
            Ok((output_pixel_bytes, output_wkb_bytes))
        }
        _ => Err(RasterExecutionError::InvalidRow {
            row,
            reason: "resident raster work action is invalid",
        }),
    }
}

/// Validate borrowed resident metadata and compute exact persistent and peak
/// charges without allocating or copying retained WKB.
pub fn size_raster_execution(
    spec: &RasterQuerySpec,
    stats: &ResidentRasterStats,
    exact: &RetainedExactValues,
) -> Result<RasterExecutionSizing, RasterExecutionError> {
    spec.validate().map_err(RasterExecutionError::InvalidSpec)?;
    let output_width = pixel_width(spec.reclass.output_pixel_type).ok_or(
        RasterExecutionError::InvalidSnapshot("floating raster output reached execution"),
    )?;
    let row_count =
        usize::try_from(stats.row_count).map_err(|_| RasterExecutionError::ByteCountOverflow)?;
    if stats.work_rows.len() != row_count
        || exact.offsets.len()
            != row_count
                .checked_add(1)
                .ok_or(RasterExecutionError::ByteCountOverflow)?
        || exact.offsets.first() != Some(&0)
        || exact
            .offsets
            .windows(2)
            .any(|offsets| offsets[0] > offsets[1])
        || exact.offsets.last().copied() != u64::try_from(exact.bytes.len()).ok()
        || stats.input_wkb_bytes != u64::try_from(exact.bytes.len()).unwrap_or(u64::MAX)
    {
        return Err(RasterExecutionError::InvalidSnapshot(
            "resident raster snapshot lengths or offsets are invalid",
        ));
    }
    if stats.zero_grid_present_band_rows != 0 {
        return Err(RasterExecutionError::InvalidSnapshot(
            "zero-grid raster bands cannot cross the exact PostGIS WKB importer",
        ));
    }

    let mut output_pixels_bytes = 0_u64;
    let mut output_wkb_bytes = 0_u64;
    let mut total_pixels = 0_u64;
    let mut non_null_rows = 0_u64;
    let mut total_grid_pixels = 0_u64;
    let mut selected_band_rows = 0_u64;
    let mut null_rows = 0_usize;
    for (row, work) in stats.work_rows.iter().copied().enumerate() {
        let exact_wkb = exact_row(exact, row)?;
        let (pixel_bytes, wkb_bytes) = checked_row_layout(row, work, exact_wkb, output_width)?;
        output_pixels_bytes = output_pixels_bytes
            .checked_add(pixel_bytes)
            .ok_or(RasterExecutionError::ByteCountOverflow)?;
        output_wkb_bytes = output_wkb_bytes
            .checked_add(wkb_bytes)
            .ok_or(RasterExecutionError::ByteCountOverflow)?;
        match work.action {
            RESIDENT_RASTER_WORK_NULL => {
                null_rows = null_rows
                    .checked_add(1)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
            }
            RESIDENT_RASTER_WORK_MISSING_BAND => {
                non_null_rows = non_null_rows
                    .checked_add(1)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
                total_grid_pixels = total_grid_pixels
                    .checked_add(work.grid_pixels)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
            }
            RESIDENT_RASTER_WORK_RECLASS => {
                non_null_rows = non_null_rows
                    .checked_add(1)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
                total_grid_pixels = total_grid_pixels
                    .checked_add(work.grid_pixels)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
                total_pixels = total_pixels
                    .checked_add(work.grid_pixels)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
                selected_band_rows = selected_band_rows
                    .checked_add(1)
                    .ok_or(RasterExecutionError::ByteCountOverflow)?;
            }
            _ => unreachable!("checked_row_layout rejects unknown actions"),
        }
    }
    let expected_output_pixels = total_pixels
        .checked_mul(
            u64::try_from(output_width).map_err(|_| RasterExecutionError::ByteCountOverflow)?,
        )
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    if stats.non_null_rows != non_null_rows
        || stats.total_grid_pixels != total_grid_pixels
        || stats.selected_band_pixels(1) != Some(total_pixels)
        || stats.selected_band_rows(1) != Some(selected_band_rows)
        || stats.reclass_output_wkb_bytes(spec.reclass.output_pixel_type.tag())
            != Some(output_wkb_bytes)
        || output_pixels_bytes != expected_output_pixels
    {
        return Err(RasterExecutionError::InvalidSnapshot(
            "resident raster aggregate statistics disagree with row work metadata",
        ));
    }

    let output_pixels_bytes = usize::try_from(output_pixels_bytes)
        .map_err(|_| RasterExecutionError::ByteCountOverflow)?;
    let total_pixels =
        usize::try_from(total_pixels).map_err(|_| RasterExecutionError::ByteCountOverflow)?;
    let output_wkb_bytes =
        usize::try_from(output_wkb_bytes).map_err(|_| RasterExecutionError::ByteCountOverflow)?;

    let offset_count = row_count
        .checked_add(1)
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    let exact_offsets_bytes = bytes_for_len::<u64>(exact.offsets.len())?;
    let exact_bytes =
        u64::try_from(exact.bytes.len()).map_err(|_| RasterExecutionError::ByteCountOverflow)?;
    let stats_bytes = checked_sum(&[
        bytes_for_len::<u64>(stats.band_pixels.len())?,
        bytes_for_len::<u64>(stats.band_rows.len())?,
        bytes_for_len::<ResidentRasterWorkRow>(stats.work_rows.len())?,
    ])?;
    let snapshot_host_bytes = checked_sum(&[exact_offsets_bytes, exact_bytes, stats_bytes])?;
    let layout_host_bytes = checked_sum(&[
        bytes_for_len::<u64>(offset_count)?,
        bytes_for_len::<u64>(offset_count)?,
    ])?;
    let (device_artifact_bytes, post_launch_host_bytes) = if row_count == 0 {
        (0, 0)
    } else {
        let rules_bytes = u64::try_from(spec.reclass.rules.len())
            .ok()
            .and_then(|count| count.checked_mul(NATIVE_RECLASS_RULE_BYTES))
            .ok_or(RasterExecutionError::ByteCountOverflow)?;
        let offsets_bytes = bytes_for_len::<u64>(offset_count)?;
        let output_bytes = u64::try_from(output_pixels_bytes)
            .map_err(|_| RasterExecutionError::ByteCountOverflow)?;
        let actions_bytes =
            u64::try_from(row_count).map_err(|_| RasterExecutionError::ByteCountOverflow)?;
        (
            checked_sum(&[
                rules_bytes,
                offsets_bytes,
                output_bytes,
                actions_bytes,
                NATIVE_VALIDATION_SCRATCH_BYTES,
            ])?,
            checked_sum(&[output_bytes, actions_bytes, NATIVE_VALIDATION_SCRATCH_BYTES])?,
        )
    };
    let reconstructed_output_bytes = checked_sum(&[
        u64::try_from(output_wkb_bytes).map_err(|_| RasterExecutionError::ByteCountOverflow)?,
        bytes_for_len::<u64>(offset_count)?,
        if null_rows == 0 {
            0
        } else {
            u64::try_from(row_count).map_err(|_| RasterExecutionError::ByteCountOverflow)?
        },
    ])?;
    let prelaunch_reserved_bytes = checked_sum(&[
        snapshot_host_bytes,
        layout_host_bytes,
        device_artifact_bytes,
    ])?;
    let peak_reserved_bytes = checked_sum(&[
        prelaunch_reserved_bytes,
        post_launch_host_bytes,
        reconstructed_output_bytes,
    ])?;

    Ok(RasterExecutionSizing {
        accounting: RasterExecutionAccounting {
            snapshot_host_bytes,
            layout_host_bytes,
            device_artifact_bytes,
            post_launch_host_bytes,
            reconstructed_output_bytes,
            prelaunch_reserved_bytes,
            peak_reserved_bytes,
        },
        output_pixel_type: spec.reclass.output_pixel_type,
        output_pixel_width: output_width,
        row_count,
        total_pixels,
        output_pixels_bytes,
        output_wkb_bytes,
        null_rows,
    })
}

/// Compute the exact zero-row lifecycle charges without constructing owned
/// empty snapshot lanes. This is the typed-empty branch of the first
/// resident borrow and must remain allocation-free.
pub fn size_empty_raster_execution(
    spec: &RasterQuerySpec,
) -> Result<RasterExecutionSizing, RasterExecutionError> {
    spec.validate().map_err(RasterExecutionError::InvalidSpec)?;
    let output_width = pixel_width(spec.reclass.output_pixel_type).ok_or(
        RasterExecutionError::InvalidSnapshot("floating raster output reached execution"),
    )?;
    let exact_offsets_bytes = bytes_for_len::<u64>(1)?;
    let layout_host_bytes = checked_sum(&[bytes_for_len::<u64>(1)?, bytes_for_len::<u64>(1)?])?;
    let reconstructed_output_bytes = exact_offsets_bytes;
    let prelaunch_reserved_bytes = exact_offsets_bytes
        .checked_add(layout_host_bytes)
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    let peak_reserved_bytes = prelaunch_reserved_bytes
        .checked_add(reconstructed_output_bytes)
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    Ok(RasterExecutionSizing {
        accounting: RasterExecutionAccounting {
            snapshot_host_bytes: exact_offsets_bytes,
            layout_host_bytes,
            device_artifact_bytes: 0,
            post_launch_host_bytes: 0,
            reconstructed_output_bytes,
            prelaunch_reserved_bytes,
            peak_reserved_bytes,
        },
        output_pixel_type: spec.reclass.output_pixel_type,
        output_pixel_width: output_width,
        row_count: 0,
        total_pixels: 0,
        output_pixels_bytes: 0,
        output_wkb_bytes: 0,
        null_rows: 0,
    })
}

/// Allocate and fill the owned launch layout only after the caller has
/// reserved the exact charges returned by [`size_raster_execution`].
pub fn preflight_raster_execution(
    spec: &RasterQuerySpec,
    snapshot: RasterExecutionSnapshot,
) -> Result<RasterExecutionPreflight, RasterExecutionError> {
    let sizing = size_raster_execution(spec, &snapshot.stats, &snapshot.exact)?;
    let mut output_offsets = try_vec_with_capacity(sizing.row_count + 1, "raster output offsets")?;
    let mut output_wkb_offsets =
        try_vec_with_capacity(sizing.row_count + 1, "raster WKB output offsets")?;
    output_offsets.push(0_u64);
    output_wkb_offsets.push(0_u64);
    for (row, work) in snapshot.stats.work_rows.iter().copied().enumerate() {
        let exact_wkb = exact_row(&snapshot.exact, row)?;
        let (pixel_bytes, wkb_bytes) =
            checked_row_layout(row, work, exact_wkb, sizing.output_pixel_width)?;
        output_offsets.push(
            output_offsets
                .last()
                .copied()
                .and_then(|offset| offset.checked_add(pixel_bytes))
                .ok_or(RasterExecutionError::ByteCountOverflow)?,
        );
        output_wkb_offsets.push(
            output_wkb_offsets
                .last()
                .copied()
                .and_then(|offset| offset.checked_add(wkb_bytes))
                .ok_or(RasterExecutionError::ByteCountOverflow)?,
        );
    }
    if output_offsets.last().copied() != u64::try_from(sizing.output_pixels_bytes).ok()
        || output_wkb_offsets.last().copied() != u64::try_from(sizing.output_wkb_bytes).ok()
    {
        return Err(RasterExecutionError::InvalidSnapshot(
            "owned raster layout changed after non-owning sizing",
        ));
    }
    Ok(RasterExecutionPreflight {
        snapshot,
        layout: RasterExecutionLayout {
            output_pixel_type: spec.reclass.output_pixel_type,
            output_pixel_width: sizing.output_pixel_width,
            output_offsets: output_offsets.into_boxed_slice(),
            output_wkb_offsets: output_wkb_offsets.into_boxed_slice(),
            total_pixels: sizing.total_pixels,
            output_pixels_bytes: sizing.output_pixels_bytes,
            null_rows: sizing.null_rows,
        },
        accounting: sizing.accounting,
    })
}

fn validate_narrow_output(pixel_type: RasterPixelType, pixels: &[u8]) -> bool {
    match pixel_type {
        RasterPixelType::Bool => pixels.iter().all(|value| *value <= 1),
        RasterPixelType::UInt2 => pixels.iter().all(|value| *value <= 3),
        RasterPixelType::UInt4 => pixels.iter().all(|value| *value <= 15),
        _ => true,
    }
}

fn append_reclassified_wkb(
    output: &mut Vec<u8>,
    original: &[u8],
    work: ResidentRasterWorkRow,
    output_pixels: &[u8],
    output_pixel_type: RasterPixelType,
    output_width: usize,
    row: usize,
) -> Result<(), RasterExecutionError> {
    let shape = exact_wkb_shape(original, row)?;
    let source_values = usize::try_from(
        work.grid_pixels
            .checked_add(1)
            .and_then(|values| values.checked_mul(u64::from(work.source_pixel_width)))
            .ok_or(RasterExecutionError::ByteCountOverflow)?,
    )
    .map_err(|_| RasterExecutionError::ByteCountOverflow)?;
    let source_end = (RASTER_WKB_HEADER_BYTES + 1)
        .checked_add(source_values)
        .ok_or(RasterExecutionError::ByteCountOverflow)?;
    let suffix = original
        .get(source_end..)
        .ok_or(RasterExecutionError::InvalidRow {
            row,
            reason: "band-one source payload is truncated during reconstruction",
        })?;
    if !validate_narrow_output(output_pixel_type, output_pixels) {
        return Err(RasterExecutionError::InvalidRow {
            row,
            reason: "native output exceeds the selected packed integer pixel range",
        });
    }

    output.extend_from_slice(&original[..RASTER_WKB_HEADER_BYTES]);
    output.push(u8::try_from(output_pixel_type.tag()).map_err(|_| {
        RasterExecutionError::InvalidRow {
            row,
            reason: "output raster pixel tag does not fit WKB flags",
        }
    })?);
    output.extend(std::iter::repeat_n(0_u8, output_width));
    if shape.little_endian || output_width == 1 {
        output.extend_from_slice(output_pixels);
    } else {
        for pixel in output_pixels.chunks_exact(output_width) {
            output.extend(pixel.iter().rev());
        }
    }
    output.extend_from_slice(suffix);
    Ok(())
}

/// Reconstruct exact PostGIS WKB only after native scratch and row actions have
/// been copied and validated outside the resident-input borrow.
pub fn reconstruct_raster_output(
    preflight: &RasterExecutionPreflight,
    output_pixels: &[u8],
    row_actions: &[u8],
) -> Result<RasterReconstructedOutput, RasterExecutionError> {
    let expected_pixels = preflight.layout.output_pixels_bytes;
    if output_pixels.len() != expected_pixels {
        return Err(RasterExecutionError::OutputPixelLength {
            expected: expected_pixels,
            actual: output_pixels.len(),
        });
    }
    let row_count = preflight.layout.row_count();
    if row_actions.len() != row_count {
        return Err(RasterExecutionError::RowActionLength {
            expected: row_count,
            actual: row_actions.len(),
        });
    }
    for (row, (actual, work)) in row_actions
        .iter()
        .zip(preflight.snapshot.stats.work_rows.iter())
        .enumerate()
    {
        if *actual != work.action {
            return Err(RasterExecutionError::RowActionMismatch {
                row,
                expected: work.action,
                actual: *actual,
            });
        }
    }

    let output_wkb_bytes = preflight.layout.output_wkb_bytes();
    let mut exact_bytes = try_vec_with_capacity(output_wkb_bytes, "reconstructed raster WKB")?;
    for (row, work) in preflight
        .snapshot
        .stats
        .work_rows
        .iter()
        .copied()
        .enumerate()
    {
        let original = exact_row(&preflight.snapshot.exact, row)?;
        match work.action {
            RESIDENT_RASTER_WORK_NULL => {}
            RESIDENT_RASTER_WORK_MISSING_BAND => exact_bytes.extend_from_slice(original),
            RESIDENT_RASTER_WORK_RECLASS => {
                let start = usize::try_from(preflight.layout.output_offsets[row])
                    .map_err(|_| RasterExecutionError::ByteCountOverflow)?;
                let end = usize::try_from(preflight.layout.output_offsets[row + 1])
                    .map_err(|_| RasterExecutionError::ByteCountOverflow)?;
                let pixels =
                    output_pixels
                        .get(start..end)
                        .ok_or(RasterExecutionError::InvalidRow {
                            row,
                            reason: "native output offsets exceed copied pixel bytes",
                        })?;
                append_reclassified_wkb(
                    &mut exact_bytes,
                    original,
                    work,
                    pixels,
                    preflight.layout.output_pixel_type,
                    preflight.layout.output_pixel_width,
                    row,
                )?;
            }
            _ => unreachable!("preflight rejects unknown actions"),
        }
        if u64::try_from(exact_bytes.len()).ok()
            != preflight.layout.output_wkb_offsets.get(row + 1).copied()
        {
            return Err(RasterExecutionError::InvalidRow {
                row,
                reason: "reconstructed raster WKB length disagrees with preflight",
            });
        }
    }
    if exact_bytes.len() != output_wkb_bytes {
        return Err(RasterExecutionError::InvalidSnapshot(
            "reconstructed raster output length is not exact",
        ));
    }

    let mut exact_offsets =
        try_vec_with_capacity(row_count + 1, "reconstructed raster WKB offsets")?;
    exact_offsets.extend_from_slice(&preflight.layout.output_wkb_offsets);
    let nulls = if preflight.layout.null_rows == 0 {
        None
    } else {
        let mut nulls = try_vec_with_capacity(row_count, "reconstructed raster NULL sidecar")?;
        nulls.extend(
            preflight
                .snapshot
                .stats
                .work_rows
                .iter()
                .map(|work| u8::from(work.action == RESIDENT_RASTER_WORK_NULL)),
        );
        Some(nulls.into_boxed_slice())
    };
    Ok(RasterReconstructedOutput {
        exact: RetainedExactValues {
            offsets: exact_offsets.into_boxed_slice(),
            bytes: exact_bytes.into_boxed_slice(),
        },
        nulls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::extractors::raster::parse_resident_raster;
    use crate::engine::raster::{RasterReclassRule, RasterReclassSpec};
    use crate::engine::residency::{
        RESIDENT_RASTER_BAND_HAS_NODATA, RESIDENT_RASTER_BAND_IS_NODATA, ResidentRasterBand,
        ResidentRasterData, ResidentRasterRow,
    };

    fn spec(output_pixel_type: RasterPixelType) -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 10,
            raster_attno: 1,
            raster_type_oid: 20,
            function_oid: 30,
            as_wkb_fn_oid: 31,
            rast_from_wkb_fn_oid: 32,
            catalog_fingerprint: vec![1, 2, 3].into_boxed_slice(),
            reclass: RasterReclassSpec {
                output_pixel_type,
                rules: vec![RasterReclassRule {
                    source: 1,
                    destination: 1,
                }]
                .into_boxed_slice(),
            },
        }
    }

    fn append_u16(output: &mut Vec<u8>, value: u16, little: bool) {
        let bytes = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output.extend_from_slice(&bytes);
    }

    fn append_i32(output: &mut Vec<u8>, value: i32, little: bool) {
        let bytes = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output.extend_from_slice(&bytes);
    }

    fn append_f64(output: &mut Vec<u8>, value: f64, little: bool) {
        let bytes = if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        output.extend_from_slice(&bytes);
    }

    fn append_elements(output: &mut Vec<u8>, little_bytes: &[u8], width: usize, little: bool) {
        assert_eq!(little_bytes.len() % width, 0);
        if little || width == 1 {
            output.extend_from_slice(little_bytes);
        } else {
            for value in little_bytes.chunks_exact(width) {
                output.extend(value.iter().rev());
            }
        }
    }

    fn wkb(little: bool, width: u16, height: u16, bands: &[(u8, u8, Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut output = Vec::new();
        output.push(u8::from(little));
        append_u16(&mut output, 0, little);
        append_u16(&mut output, bands.len() as u16, little);
        for value in [1.25, -2.5, 3.75, 4.5, 0.125, -0.25] {
            append_f64(&mut output, value, little);
        }
        append_i32(&mut output, 4326, little);
        append_u16(&mut output, width, little);
        append_u16(&mut output, height, little);
        assert_eq!(output.len(), RASTER_WKB_HEADER_BYTES);
        for (tag, upper_flags, nodata, pixels) in bands {
            let element_width = pixel_width_from_tag(*tag).expect("test pixel tag");
            output.push(*tag | *upper_flags);
            assert_eq!(nodata.len(), element_width);
            append_elements(&mut output, nodata, element_width, little);
            append_elements(&mut output, pixels, element_width, little);
        }
        output
    }

    fn snapshot(values: &[Option<Vec<u8>>]) -> RasterExecutionSnapshot {
        let mut pixels = Vec::new();
        let mut band_offsets = vec![0_u64];
        let mut rows = Vec::new();
        let mut bands = Vec::new();
        let mut nulls = Vec::new();
        let mut exact_offsets = vec![0_u64];
        let mut exact_bytes = Vec::new();
        let mut saw_null = false;
        for value in values {
            let Some(value) = value else {
                rows.push(ResidentRasterRow::default());
                nulls.push(1);
                exact_offsets.push(exact_bytes.len() as u64);
                saw_null = true;
                continue;
            };
            let parsed = parse_resident_raster(value).expect("test WKB is resident-compatible");
            let first_band = bands.len() as u32;
            rows.push(ResidentRasterRow {
                width: u32::from(parsed.header.width),
                height: u32::from(parsed.header.height),
                first_band,
                band_count: parsed.bands.len() as u32,
                srid: parsed.header.srid,
                scale_x: parsed.header.scale_x,
                scale_y: parsed.header.scale_y,
                ip_x: parsed.header.ip_x,
                ip_y: parsed.header.ip_y,
                skew_x: parsed.header.skew_x,
                skew_y: parsed.header.skew_y,
                ..ResidentRasterRow::default()
            });
            for band in parsed.bands {
                let flags = (u32::from(band.has_nodata) * RESIDENT_RASTER_BAND_HAS_NODATA)
                    | (u32::from(band.is_nodata) * RESIDENT_RASTER_BAND_IS_NODATA);
                bands.push(ResidentRasterBand {
                    pixel_type: u32::from(band.pixel_type.code()),
                    flags,
                    nodata: band.nodata,
                });
                pixels.extend_from_slice(&band.pixels);
                band_offsets.push(pixels.len() as u64);
            }
            nulls.push(0);
            exact_bytes.extend_from_slice(value);
            exact_offsets.push(exact_bytes.len() as u64);
        }
        let data = ResidentRasterData {
            pixels,
            band_offsets,
            rows,
            bands,
            nulls: saw_null.then_some(nulls),
            exact: RetainedExactValues {
                offsets: exact_offsets.into_boxed_slice(),
                bytes: exact_bytes.into_boxed_slice(),
            },
        };
        let stats = data.stats().expect("test resident raster stats");
        RasterExecutionSnapshot {
            stats,
            exact: data.exact,
        }
    }

    #[test]
    fn empty_snapshot_has_no_native_artifact() {
        let preflight = preflight_raster_execution(&spec(RasterPixelType::UInt8), snapshot(&[]))
            .expect("empty raster preflight");
        assert_eq!(preflight.layout.output_offsets(), &[0]);
        assert_eq!(preflight.layout.output_wkb_offsets(), &[0]);
        assert!(!preflight.layout.requires_launch());
        assert_eq!(preflight.accounting.device_artifact_bytes, 0);
        assert_eq!(preflight.accounting.post_launch_host_bytes, 0);
        let output = reconstruct_raster_output(&preflight, &[], &[]).expect("empty output");
        assert_eq!(output.row_count(), 0);
    }

    #[test]
    fn present_zero_pixel_band_fails_before_native_dispatch() {
        let zero_band = wkb(true, 2, 3, &[]);
        let zero_pixel_band = wkb(true, 0, 7, &[(4, 0x40, vec![255], vec![])]);
        let error = preflight_raster_execution(
            &spec(RasterPixelType::UInt16),
            snapshot(&[None, Some(zero_band), Some(zero_pixel_band)]),
        )
        .expect_err("zero-grid present bands cannot cross the public importer");
        assert_eq!(
            error,
            RasterExecutionError::InvalidSnapshot(
                "zero-grid raster bands cannot cross the exact PostGIS WKB importer"
            )
        );
    }

    #[test]
    fn little_endian_multiband_rewrites_only_band_one() {
        let input = wkb(
            true,
            2,
            1,
            &[
                (4, 0x40, vec![255], vec![1, 2]),
                (
                    6,
                    0x40,
                    0x1234_u16.to_le_bytes().to_vec(),
                    vec![0x78, 0x56, 0xbc, 0x9a],
                ),
            ],
        );
        let source_band_end = RASTER_WKB_HEADER_BYTES + 1 + 3;
        let preflight = preflight_raster_execution(
            &spec(RasterPixelType::UInt16),
            snapshot(&[Some(input.clone())]),
        )
        .expect("little-endian preflight");
        assert_eq!(preflight.layout.output_offsets(), &[0, 4]);
        let output = reconstruct_raster_output(&preflight, &[9, 0, 10, 0], &[2])
            .expect("little-endian reconstruction");
        let output = output.value(0).expect("output WKB");
        assert_eq!(
            &output[..RASTER_WKB_HEADER_BYTES],
            &input[..RASTER_WKB_HEADER_BYTES]
        );
        assert_eq!(output[RASTER_WKB_HEADER_BYTES], 6);
        assert_eq!(
            &output[RASTER_WKB_HEADER_BYTES + 1..RASTER_WKB_HEADER_BYTES + 3],
            &[0, 0]
        );
        assert_eq!(
            &output[RASTER_WKB_HEADER_BYTES + 3..RASTER_WKB_HEADER_BYTES + 7],
            &[9, 0, 10, 0]
        );
        assert_eq!(
            &output[RASTER_WKB_HEADER_BYTES + 7..],
            &input[source_band_end..]
        );
    }

    #[test]
    fn big_endian_output_pixels_are_serialized_per_element() {
        let input = wkb(
            false,
            2,
            1,
            &[(5, 0, 0_i16.to_le_bytes().to_vec(), vec![1, 0, 2, 0])],
        );
        let preflight =
            preflight_raster_execution(&spec(RasterPixelType::UInt32), snapshot(&[Some(input)]))
                .expect("big-endian preflight");
        let output = reconstruct_raster_output(
            &preflight,
            &[0x44, 0x33, 0x22, 0x11, 0xdd, 0xcc, 0xbb, 0xaa],
            &[2],
        )
        .expect("big-endian reconstruction");
        let band = &output.value(0).expect("output WKB")[RASTER_WKB_HEADER_BYTES..];
        assert_eq!(band[0], 8);
        assert_eq!(&band[1..5], &[0, 0, 0, 0]);
        assert_eq!(
            &band[5..],
            &[0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd]
        );
    }

    #[test]
    fn source_nodata_and_nan_flags_do_not_leak_to_output_band() {
        let input = wkb(
            true,
            1,
            1,
            &[(
                10,
                0x60,
                f32::NAN.to_le_bytes().to_vec(),
                f32::NAN.to_le_bytes().to_vec(),
            )],
        );
        let preflight =
            preflight_raster_execution(&spec(RasterPixelType::UInt8), snapshot(&[Some(input)]))
                .expect("NaN-nodata preflight");
        let output =
            reconstruct_raster_output(&preflight, &[1], &[2]).expect("NaN-nodata reconstruction");
        let band = &output.value(0).expect("output WKB")[RASTER_WKB_HEADER_BYTES..];
        assert_eq!(band, &[4, 0, 1]);
    }

    #[test]
    fn all_integer_output_widths_have_exact_layouts() {
        let input = wkb(true, 3, 1, &[(4, 0, vec![0], vec![1, 2, 3])]);
        for (pixel_type, width) in [
            (RasterPixelType::Bool, 1),
            (RasterPixelType::UInt2, 1),
            (RasterPixelType::UInt4, 1),
            (RasterPixelType::Int8, 1),
            (RasterPixelType::UInt8, 1),
            (RasterPixelType::Int16, 2),
            (RasterPixelType::UInt16, 2),
            (RasterPixelType::Int32, 4),
            (RasterPixelType::UInt32, 4),
        ] {
            let preflight =
                preflight_raster_execution(&spec(pixel_type), snapshot(&[Some(input.clone())]))
                    .expect("integer preflight");
            assert_eq!(preflight.layout.output_pixel_width(), width);
            assert_eq!(preflight.layout.output_offsets(), &[0, (3 * width) as u64]);
            assert_eq!(
                preflight.layout.output_wkb_bytes(),
                RASTER_WKB_HEADER_BYTES + 1 + 4 * width
            );
        }
    }

    #[test]
    fn accounting_separates_persistent_and_peak_bytes_exactly() {
        let zero_band = wkb(true, 2, 1, &[]);
        let band = wkb(true, 2, 1, &[(4, 0, vec![0], vec![1, 2])]);
        let spec = spec(RasterPixelType::UInt16);
        let snapshot = snapshot(&[None, Some(zero_band), Some(band)]);
        let sizing = size_raster_execution(&spec, &snapshot.stats, &snapshot.exact)
            .expect("non-owning sizing");
        let preflight = preflight_raster_execution(&spec, snapshot).expect("accounting preflight");
        assert_eq!(sizing.accounting, preflight.accounting);
        assert_eq!(sizing.output_pixel_type(), RasterPixelType::UInt16);
        assert_eq!(sizing.output_pixel_width(), 2);
        assert_eq!(sizing.row_count(), 3);
        assert_eq!(sizing.total_pixels(), 2);
        assert_eq!(sizing.output_pixels_bytes(), 4);
        assert_eq!(
            sizing.output_wkb_bytes(),
            preflight.layout.output_wkb_bytes()
        );
        assert_eq!(sizing.null_rows(), 1);
        assert_eq!(preflight.accounting.snapshot_host_bytes, 246);
        assert_eq!(preflight.accounting.layout_host_bytes, 64);
        assert_eq!(preflight.accounting.device_artifact_bytes, 79);
        assert_eq!(preflight.accounting.post_launch_host_bytes, 31);
        assert_eq!(preflight.accounting.reconstructed_output_bytes, 164);
        assert_eq!(preflight.accounting.prelaunch_reserved_bytes, 389);
        assert_eq!(preflight.accounting.peak_reserved_bytes, 584);
    }

    #[test]
    fn action_length_capacity_and_packed_range_fail_closed() {
        let input = wkb(true, 1, 1, &[(4, 0, vec![0], vec![1])]);
        let preflight =
            preflight_raster_execution(&spec(RasterPixelType::Bool), snapshot(&[Some(input)]))
                .expect("failure-case preflight");
        assert!(matches!(
            reconstruct_raster_output(&preflight, &[], &[2]),
            Err(RasterExecutionError::OutputPixelLength { .. })
        ));
        assert!(matches!(
            reconstruct_raster_output(&preflight, &[1], &[]),
            Err(RasterExecutionError::RowActionLength { .. })
        ));
        assert!(matches!(
            reconstruct_raster_output(&preflight, &[1], &[1]),
            Err(RasterExecutionError::RowActionMismatch { .. })
        ));
        assert!(matches!(
            reconstruct_raster_output(&preflight, &[2], &[2]),
            Err(RasterExecutionError::InvalidRow { .. })
        ));
    }

    #[test]
    fn corrupt_offsets_and_work_counts_fail_before_output_allocation() {
        let input = wkb(true, 1, 1, &[(4, 0, vec![0], vec![1])]);
        let mut bad_offsets = snapshot(&[Some(input.clone())]);
        bad_offsets.exact.offsets[1] += 1;
        assert!(matches!(
            preflight_raster_execution(&spec(RasterPixelType::UInt8), bad_offsets),
            Err(RasterExecutionError::InvalidSnapshot(_))
        ));

        let mut bad_work = snapshot(&[Some(input)]);
        bad_work.stats.work_rows[0].grid_pixels = u64::MAX;
        assert!(matches!(
            preflight_raster_execution(&spec(RasterPixelType::UInt32), bad_work),
            Err(RasterExecutionError::InvalidRow { .. } | RasterExecutionError::ByteCountOverflow)
        ));

        assert_eq!(
            checked_sum(&[u64::MAX, 1]),
            Err(RasterExecutionError::ByteCountOverflow)
        );
        assert_eq!(
            bytes_for_len::<u64>(usize::MAX),
            Err(RasterExecutionError::ByteCountOverflow)
        );
    }
}

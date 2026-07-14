//! Store-neutral preparation and accounting for resident spatial aggregates.

#![allow(dead_code)] // reason: descriptor dispatch consumes this bounded checkpoint next

use std::any::Any;

use super::artifact::DescriptorAggArtifact;
use crate::engine::residency::{DerivedArtifact, ResidentByteAccounting, StagedTransformWorkspace};
use crate::gpu::{ExprDeviceBuffer, PGACCEL_SPATIAL_CONTROL_BYTES, PGACCEL_SPATIAL_MAX_CHUNK_ROWS};

const GEOMETRY_ROW_BYTES: u64 = 24;
const GEOMETRY_BBOX_BYTES: u64 = 4 * 8;
const GEOMETRY_OFFSET_BYTES_PER_ROW: u64 = 2 * 8;
const POINT_COORDINATE_BYTES: u64 = 2 * 8;
const NULL_SIDECAR_BYTES_PER_ROW: u64 = 1;
const CONSTANT_OFFSET_WORDS: u64 = 2;
const CONSTANT_PREFIX_WORDS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialDimensionShape {
    pub row_count: usize,
    pub group_key_count: usize,
    pub counted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpatialBaseShape {
    pub fact_rows: usize,
    pub fact_group_key_count: usize,
    pub dimensions: Box<[SpatialDimensionShape]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialConstantShape {
    pub exact_bytes: usize,
    pub coordinate_pairs: usize,
    pub ring_count: usize,
}

impl SpatialConstantShape {
    fn referenced_bytes(self) -> Option<u64> {
        checked_sum([
            checked_mul(self.coordinate_pairs, 2 * std::mem::size_of::<f64>())?,
            checked_mul(self.ring_count, std::mem::size_of::<u64>())?,
            GEOMETRY_ROW_BYTES,
            GEOMETRY_BBOX_BYTES,
            GEOMETRY_OFFSET_BYTES_PER_ROW,
        ])
    }

    fn device_bytes(self) -> Option<u64> {
        self.referenced_bytes()
    }

    fn host_bytes(self) -> Option<u64> {
        checked_sum([
            u64::try_from(self.exact_bytes).ok()?,
            CONSTANT_OFFSET_WORDS.checked_mul(8)?,
            CONSTANT_PREFIX_WORDS.checked_mul(8)?,
        ])
    }
}

/// Host storage allocated only after the persistent and transient charges are
/// both held. `exact_snapshot_words` is filled from resident GSERIALIZED bytes
/// during the staged snapshot callback; preflight never copies source values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpatialSnapshotLayout {
    pub exact_snapshot_words: usize,
    pub prepared_base_host_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpatialPreflight {
    pub fact_rows: usize,
    pub chunk_count: usize,
    pub chunk_limit: usize,
    pub max_referenced_bytes: usize,
    pub published_accounting: ResidentByteAccounting,
    pub transient_accounting: ResidentByteAccounting,
    pub snapshot: SpatialSnapshotLayout,
    pub constant: SpatialConstantShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpatialPreflightError {
    ZeroChunkLimit,
    ChunkLimitExceedsNativeMaximum,
    AccountingOverflow,
}

fn checked_mul(elements: usize, width: usize) -> Option<u64> {
    elements
        .checked_mul(width)
        .and_then(|bytes| u64::try_from(bytes).ok())
}

fn checked_sum<const N: usize>(terms: [u64; N]) -> Option<u64> {
    terms
        .into_iter()
        .try_fold(0_u64, |total, term| total.checked_add(term))
}

fn padded_base_device_bytes(shape: &SpatialBaseShape) -> Option<u64> {
    let mut bytes = checked_mul(shape.fact_rows, shape.fact_group_key_count.checked_mul(4)?)?;
    for dimension in &shape.dimensions {
        bytes = bytes.checked_add(checked_mul(
            dimension.row_count,
            dimension.group_key_count.checked_mul(4)?,
        )?)?;
        bytes = bytes.checked_add(checked_mul(shape.fact_rows, 4)?)?;
        bytes = bytes.checked_add(checked_mul(dimension.row_count, 1)?)?;
        if dimension.counted {
            bytes = bytes.checked_add(checked_mul(dimension.row_count, 8)?)?;
        }
    }
    Some(bytes)
}

fn chunk_device_bytes(rows: usize) -> Option<u64> {
    checked_sum([
        u64::try_from(PGACCEL_SPATIAL_CONTROL_BYTES).ok()?,
        u64::try_from(std::mem::size_of::<u32>()).ok()?,
        checked_mul(rows, std::mem::size_of::<i8>())?,
        checked_mul(rows, std::mem::size_of::<i8>())?,
        checked_mul(rows, std::mem::size_of::<u64>())?,
        u64::try_from(std::mem::size_of::<u64>()).ok()?,
    ])
}

pub(super) fn spatial_preflight(
    base: &SpatialBaseShape,
    constant: SpatialConstantShape,
    snapshot: SpatialSnapshotLayout,
    chunk_limit: usize,
) -> Result<SpatialPreflight, SpatialPreflightError> {
    if chunk_limit == 0 {
        return Err(SpatialPreflightError::ZeroChunkLimit);
    }
    if chunk_limit > PGACCEL_SPATIAL_MAX_CHUNK_ROWS {
        return Err(SpatialPreflightError::ChunkLimitExceedsNativeMaximum);
    }
    let chunk_count = base.fact_rows.div_ceil(chunk_limit);

    let base_device_bytes =
        padded_base_device_bytes(base).ok_or(SpatialPreflightError::AccountingOverflow)?;
    let final_mask_bytes = checked_mul(base.fact_rows, std::mem::size_of::<i8>())
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let published_device_bytes = base_device_bytes
        .checked_add(final_mask_bytes)
        .ok_or(SpatialPreflightError::AccountingOverflow)?;

    let fixed_chunk_scratch =
        chunk_device_bytes(0).and_then(|bytes| bytes.checked_mul(u64::try_from(chunk_count).ok()?));
    let row_chunk_scratch = checked_mul(base.fact_rows, 10);
    let chunk_scratch = fixed_chunk_scratch.and_then(|fixed| fixed.checked_add(row_chunk_scratch?));
    let transient_device_bytes = constant
        .device_bytes()
        .and_then(|bytes| bytes.checked_add(chunk_scratch?))
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let transient_host_bytes = checked_sum([
        snapshot.prepared_base_host_bytes,
        checked_mul(snapshot.exact_snapshot_words, std::mem::size_of::<u64>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        constant
            .host_bytes()
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        final_mask_bytes,
        checked_mul(base.fact_rows, std::mem::size_of::<u64>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
        checked_mul(base.fact_rows, std::mem::size_of::<i8>())
            .ok_or(SpatialPreflightError::AccountingOverflow)?,
    ])
    .ok_or(SpatialPreflightError::AccountingOverflow)?;

    let largest_chunk = base.fact_rows.min(chunk_limit);
    let point_row_bytes = checked_sum([
        POINT_COORDINATE_BYTES,
        GEOMETRY_ROW_BYTES,
        GEOMETRY_BBOX_BYTES,
        GEOMETRY_OFFSET_BYTES_PER_ROW,
        NULL_SIDECAR_BYTES_PER_ROW,
    ])
    .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let column_half = u64::try_from(largest_chunk)
        .ok()
        .and_then(|rows| rows.checked_mul(point_row_bytes))
        .ok_or(SpatialPreflightError::AccountingOverflow)?;
    let max_referenced_bytes = constant
        .referenced_bytes()
        .and_then(|bytes| bytes.max(column_half).checked_mul(2))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(SpatialPreflightError::AccountingOverflow)?;

    Ok(SpatialPreflight {
        fact_rows: base.fact_rows,
        chunk_count,
        chunk_limit,
        max_referenced_bytes,
        published_accounting: ResidentByteAccounting {
            device_bytes: published_device_bytes,
            retained_host_exact_bytes: 0,
        },
        transient_accounting: ResidentByteAccounting {
            device_bytes: transient_device_bytes,
            retained_host_exact_bytes: transient_host_bytes,
        },
        snapshot,
        constant,
    })
}

/// Publishable composite: the ordinary group/dimension artifact and the final
/// exact SQL mask are retained under one derived-artifact identity.
pub(super) struct SpatialAggArtifact {
    pub base: DescriptorAggArtifact,
    pub final_mask: Option<ExprDeviceBuffer<i8>>,
    device_bytes: u64,
}

impl SpatialAggArtifact {
    pub fn new(
        base: DescriptorAggArtifact,
        final_mask: Option<ExprDeviceBuffer<i8>>,
    ) -> Result<Self, &'static str> {
        if final_mask.as_ref().map_or(0, ExprDeviceBuffer::len) != base.fact_rows {
            return Err("spatial SQL mask length does not match fact rows");
        }
        let mask_bytes = checked_mul(base.fact_rows, std::mem::size_of::<i8>())
            .ok_or("spatial SQL mask byte count overflow")?;
        let device_bytes = base
            .device_bytes()
            .checked_add(mask_bytes)
            .ok_or("spatial composite byte count overflow")?;
        Ok(Self {
            base,
            final_mask,
            device_bytes,
        })
    }
}

impl DerivedArtifact for SpatialAggArtifact {
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

/// Accounting declaration consumed by the post-charge workspace builder.
/// Buffer ownership and launch outcomes are added in the dispatch checkpoint.
pub(super) struct SpatialWorkspace {
    pub preflight: SpatialPreflight,
}

impl StagedTransformWorkspace for SpatialWorkspace {
    fn device_bytes(&self) -> u64 {
        self.preflight.transient_accounting.device_bytes
    }

    fn host_bytes(&self) -> u64 {
        self.preflight
            .transient_accounting
            .retained_host_exact_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(fact_rows: usize) -> SpatialBaseShape {
        SpatialBaseShape {
            fact_rows,
            fact_group_key_count: 1,
            dimensions: vec![SpatialDimensionShape {
                row_count: 7,
                group_key_count: 1,
                counted: true,
            }]
            .into_boxed_slice(),
        }
    }

    fn constant() -> SpatialConstantShape {
        SpatialConstantShape {
            exact_bytes: 96,
            coordinate_pairs: 5,
            ring_count: 1,
        }
    }

    #[test]
    fn preflight_chunks_at_the_native_boundary_and_reserves_all_uncertain_rows() {
        let preflight = spatial_preflight(
            &shape(PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 3),
            constant(),
            SpatialSnapshotLayout {
                exact_snapshot_words: 40,
                prepared_base_host_bytes: 128,
            },
            PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
        )
        .expect("bounded preflight");
        assert_eq!(preflight.chunk_count, 2);
        assert_eq!(preflight.chunk_limit, 65_536);
        let full_uncertainty_host = (PGACCEL_SPATIAL_MAX_CHUNK_ROWS as u64 + 3) * 9;
        assert!(preflight.transient_accounting.retained_host_exact_bytes >= full_uncertainty_host);
    }

    #[test]
    fn half_budget_is_twice_the_larger_referenced_operand() {
        let preflight = spatial_preflight(
            &shape(10),
            constant(),
            SpatialSnapshotLayout {
                exact_snapshot_words: 1,
                prepared_base_host_bytes: 0,
            },
            10,
        )
        .expect("bounded preflight");
        assert_eq!(preflight.max_referenced_bytes, 2 * 10 * 89);

        let large_constant = SpatialConstantShape {
            coordinate_pairs: 1_000,
            ..constant()
        };
        let preflight = spatial_preflight(
            &shape(1),
            large_constant,
            SpatialSnapshotLayout {
                exact_snapshot_words: 1,
                prepared_base_host_bytes: 0,
            },
            1,
        )
        .expect("bounded preflight");
        assert_eq!(
            preflight.max_referenced_bytes as u64,
            2 * large_constant.referenced_bytes().expect("constant bytes")
        );
    }

    #[test]
    fn accounting_matches_padded_base_mask_constant_and_chunk_scratch() {
        let base = shape(4);
        let snapshot = SpatialSnapshotLayout {
            exact_snapshot_words: 10,
            prepared_base_host_bytes: 20,
        };
        let preflight = spatial_preflight(&base, constant(), snapshot, 3).expect("preflight");
        // Base: fact key 16 + dimension key 28 + fact join 16 + match 7 + multiplicity 56.
        assert_eq!(preflight.published_accounting.device_bytes, 123 + 4);
        // Constant device 160; chunks are (384 + 4 + 8) + 10*n each.
        assert_eq!(preflight.transient_accounting.device_bytes, 160 + 426 + 406);
        // Base prep 20 + snapshot 80 + constant host 128 + full mask/index/result 40.
        assert_eq!(
            preflight.transient_accounting.retained_host_exact_bytes,
            268
        );
    }

    #[test]
    fn zero_or_oversized_chunk_limits_and_arithmetic_overflow_fail_closed() {
        assert_eq!(
            spatial_preflight(
                &shape(1),
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 1,
                    prepared_base_host_bytes: 0,
                },
                0,
            ),
            Err(SpatialPreflightError::ZeroChunkLimit)
        );
        assert_eq!(
            spatial_preflight(
                &shape(1),
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 1,
                    prepared_base_host_bytes: 0,
                },
                PGACCEL_SPATIAL_MAX_CHUNK_ROWS + 1,
            ),
            Err(SpatialPreflightError::ChunkLimitExceedsNativeMaximum)
        );
        let overflow = SpatialBaseShape {
            fact_rows: usize::MAX,
            fact_group_key_count: usize::MAX,
            dimensions: Box::default(),
        };
        assert_eq!(
            spatial_preflight(
                &overflow,
                constant(),
                SpatialSnapshotLayout {
                    exact_snapshot_words: 0,
                    prepared_base_host_bytes: 0,
                },
                PGACCEL_SPATIAL_MAX_CHUNK_ROWS,
            ),
            Err(SpatialPreflightError::AccountingOverflow)
        );
    }

    #[test]
    fn preflight_performs_no_allocation_or_snapshot_copy() {
        let base = shape(10);
        let constant = constant();
        let snapshot = SpatialSnapshotLayout {
            exact_snapshot_words: 10,
            prepared_base_host_bytes: 20,
        };
        crate::engine::residency::begin_test_allocation_count();
        let result = spatial_preflight(&base, constant, snapshot, 4);
        let allocation_count = crate::engine::residency::finish_test_allocation_count();
        assert!(result.is_ok());
        assert_eq!(allocation_count, 0);
    }
}

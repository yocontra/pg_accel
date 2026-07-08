//! `custom_private` serialization / deserialization.
//!
//! Plan metadata travels through PG as a `List *` of `Integer` nodes so it
//! survives plan copying and EXPLAIN output. Field order is load-bearing.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::GpuStrategy;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::executor::agg::{
    AggOp, GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE, is_h3_synthetic_group_key,
};
use crate::engine::executor::olap::{
    OlapAggSpec, RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES,
    ResidentDenseGroupedF64FilterMode, ResidentDenseGroupedF64Layout,
    ResidentDenseGroupedF64MeasurePredicate, ResidentDenseGroupedF64MeasurePredicateOp,
    ResidentDenseGroupedF64MeasurePredicateSource, ResidentDenseGroupedF64Source,
    ResidentDenseGroupedF64Spec, ResidentGroupAggLogicalSpec, ResidentH3GroupedCountSpec,
    ResidentStarDimGroupedF64Spec, SsbmQ1RevenueSpec, SsbmQ2GroupedRevenueSpec,
    SsbmQ3GroupedRevenueSpec, SsbmQ4GroupedProfitSpec,
};
use crate::engine::executor::preagg::{DimFilter, GroupKeyDesc, JoinDepthDesc, PreAggColDesc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};
use crate::engine::gucs;
use crate::engine::olap_cache::{
    ResidentH3GroupedCountKind, ResidentMeasureOp, ResidentStarDimGroupAggCacheShape,
    SsbmQ1DatePredicate, SsbmQ2Variant, SsbmQ3Variant, SsbmQ4Variant,
};
use crate::engine::registry::AccelStrategy;
use crate::engine::residency::{
    CpuBoundaryReason, ResidentMaterializationKind, ResidentOperatorClass, ResidentProofSnapshot,
};
use crate::gpu::PgaccelKeyType;

mod list_codec;

use list_codec::{IntListReader, PgListWriter};

const PATH_PRIVATE_HEADER_INTS: usize = 3;
const PLAN_PRIVATE_HEADER_INTS: usize = 6;
const PLAN_PAYLOAD_START: usize = PLAN_PRIVATE_HEADER_INTS;

/// Error returned by private-data decoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DecodeError {
    MissingField {
        layout: &'static str,
        field: &'static str,
        index: usize,
        len: usize,
    },
    InvalidGpuStrategy {
        raw: c_int,
    },
    InvalidAccelStrategy {
        raw: c_int,
    },
    InvalidBatchSize {
        raw: c_int,
    },
}

/// Decode a `GpuStrategy` from the current integer wire layout.
pub(super) const fn decode_gpu_strategy(raw: c_int) -> Result<GpuStrategy, DecodeError> {
    match raw {
        0 => Ok(GpuStrategy::Scan),
        1 => Ok(GpuStrategy::Join),
        2 => Ok(GpuStrategy::Agg),
        3 => Ok(GpuStrategy::Sort),
        4 => Ok(GpuStrategy::Window),
        5 => Ok(GpuStrategy::PreAgg),
        6 => Ok(GpuStrategy::FunctionScan),
        7 => Ok(GpuStrategy::SrfTargetList),
        _ => Err(DecodeError::InvalidGpuStrategy { raw }),
    }
}

/// Decode an `AccelStrategy` from the current integer wire layout.
pub(super) const fn decode_accel_strategy(raw: c_int) -> Result<AccelStrategy, DecodeError> {
    match raw {
        1 => Ok(AccelStrategy::GpuSpatial),
        2 => Ok(AccelStrategy::GpuRaster),
        3 => Ok(AccelStrategy::GpuH3),
        4 => Ok(AccelStrategy::GpuSort),
        5 => Ok(AccelStrategy::GpuReduce),
        6 => Ok(AccelStrategy::GpuExpr),
        7 => Ok(AccelStrategy::GpuHashJoin),
        8 => Ok(AccelStrategy::GpuWindow),
        9 => Ok(AccelStrategy::GpuNestedLoopIneq),
        _ => Err(DecodeError::InvalidAccelStrategy { raw }),
    }
}

#[inline]
fn decode_batch_size(raw: c_int) -> Result<c_int, DecodeError> {
    if raw > 0 {
        Ok(raw)
    } else {
        Err(DecodeError::InvalidBatchSize { raw })
    }
}

fn require_private_field(
    fields: &[c_int],
    layout: &'static str,
    index: usize,
    field: &'static str,
) -> Result<c_int, DecodeError> {
    fields.get(index).copied().ok_or(DecodeError::MissingField {
        layout,
        field,
        index,
        len: fields.len(),
    })
}

#[inline]
fn require_reader_field(
    reader: &IntListReader<'_>,
    layout: &'static str,
    index: usize,
    field: &'static str,
) -> Result<c_int, DecodeError> {
    reader.get(index).ok_or(DecodeError::MissingField {
        layout,
        field,
        index,
        len: reader.len(),
    })
}

/// Typed view of the planner hook's `CustomPath.custom_private` prefix.
///
/// Current path layout prefix:
/// `[fn_oid, target_attno, accel_strategy, ...strategy-specific payload]`.
#[allow(dead_code)] // reason: scaffold for migrating custom path call sites off raw indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PathPrivate {
    pub(super) fn_oid: pg_sys::Oid,
    pub(super) target_attno: i32,
    pub(super) accel_strategy: AccelStrategy,
}

#[allow(dead_code)] // reason: path metadata call sites still use raw indexes.
impl PathPrivate {
    /// Decode the path private header from its integer protocol.
    pub(super) fn decode(fields: &[c_int]) -> Result<Self, DecodeError> {
        let fn_oid_raw = require_private_field(fields, "PathPrivate", 0, "fn_oid")? as u32;
        let target_attno = require_private_field(fields, "PathPrivate", 1, "target_attno")?;
        let accel_strategy_raw = require_private_field(fields, "PathPrivate", 2, "accel_strategy")?;

        Ok(Self {
            fn_oid: pg_sys::Oid::from(fn_oid_raw),
            target_attno,
            accel_strategy: decode_accel_strategy(accel_strategy_raw)?,
        })
    }

    /// Re-encode the path header into the existing integer protocol.
    #[must_use]
    pub(super) fn to_ints(self) -> [c_int; PATH_PRIVATE_HEADER_INTS] {
        [
            u32::from(self.fn_oid) as c_int,
            self.target_attno,
            self.accel_strategy as c_int,
        ]
    }
}

/// Typed view of the executor plan's `CustomScan.custom_private` prefix.
///
/// Current plan layout prefix:
/// `[strategy, batch_size, expected_threads, fn_oid, target_attno, accel_strategy,
/// ...strategy-specific payload]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlanPrivate {
    pub(super) gpu_strategy: GpuStrategy,
    pub(super) batch_size: c_int,
    pub(super) expected_threads: c_int,
    pub(super) fn_oid: pg_sys::Oid,
    pub(super) target_attno: i32,
    pub(super) accel_strategy: AccelStrategy,
}

impl PlanPrivate {
    /// Decode the plan private header from its integer protocol.
    #[cfg(test)]
    pub(super) fn decode(fields: &[c_int]) -> Result<Self, DecodeError> {
        Self::decode_reader(&IntListReader::from_slice(fields))
    }

    fn decode_reader(reader: &IntListReader<'_>) -> Result<Self, DecodeError> {
        let gpu_strategy_raw = require_reader_field(reader, "PlanPrivate", 0, "strategy")?;
        let batch_size_raw = require_reader_field(reader, "PlanPrivate", 1, "batch_size")?;
        let expected_threads = require_reader_field(reader, "PlanPrivate", 2, "expected_threads")?;
        let fn_oid_raw = require_reader_field(reader, "PlanPrivate", 3, "fn_oid")? as u32;
        let target_attno = require_reader_field(reader, "PlanPrivate", 4, "target_attno")?;
        let accel_strategy_raw = require_reader_field(reader, "PlanPrivate", 5, "accel_strategy")?;

        Ok(Self {
            gpu_strategy: decode_gpu_strategy(gpu_strategy_raw)?,
            batch_size: decode_batch_size(batch_size_raw)?,
            expected_threads,
            fn_oid: pg_sys::Oid::from(fn_oid_raw),
            target_attno,
            accel_strategy: decode_accel_strategy(accel_strategy_raw)?,
        })
    }

    /// Re-encode the plan header into the integer protocol.
    #[allow(dead_code)] // reason: test-only serializer helper.
    #[must_use]
    pub(super) fn to_ints(self) -> [c_int; PLAN_PRIVATE_HEADER_INTS] {
        [
            self.gpu_strategy as c_int,
            self.batch_size,
            self.expected_threads,
            u32::from(self.fn_oid) as c_int,
            self.target_attno,
            self.accel_strategy as c_int,
        ]
    }
}

#[cfg(test)]
mod typed_private_tests {
    use super::*;
    use crate::engine::executor::olap::{
        ResidentGroupAggFilterExpr, ResidentGroupAggGroupKeyExpr, ResidentGroupAggMeasureExpr,
    };
    use crate::engine::residency::{ResidentOperatorStage, ResidentProofDecodeError};

    #[test]
    fn strict_gpu_strategy_decoder_accepts_current_wire_values() {
        for strategy in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
            GpuStrategy::PreAgg,
            GpuStrategy::FunctionScan,
            GpuStrategy::SrfTargetList,
        ] {
            assert_eq!(
                decode_gpu_strategy(strategy as c_int),
                Ok(strategy),
                "strategy {strategy:?} should decode strictly"
            );
        }
    }

    #[test]
    fn strict_strategy_decoders_reject_unknown_values() {
        assert_eq!(
            decode_gpu_strategy(99),
            Err(DecodeError::InvalidGpuStrategy { raw: 99 })
        );
        assert_eq!(
            decode_accel_strategy(99),
            Err(DecodeError::InvalidAccelStrategy { raw: 99 })
        );
    }

    #[test]
    fn plan_private_decodes_and_reencodes_header() {
        let raw = [
            GpuStrategy::Sort as c_int,
            2048,
            6,
            1234,
            5,
            AccelStrategy::GpuSort as c_int,
        ];

        let decoded = PlanPrivate::decode(&raw).expect("valid plan header should decode");

        assert_eq!(decoded.gpu_strategy, GpuStrategy::Sort);
        assert_eq!(decoded.batch_size, 2048);
        assert_eq!(decoded.expected_threads, 6);
        assert_eq!(u32::from(decoded.fn_oid), 1234);
        assert_eq!(decoded.target_attno, 5);
        assert_eq!(decoded.accel_strategy, AccelStrategy::GpuSort);
        assert_eq!(decoded.to_ints(), raw);
    }

    #[test]
    fn plan_private_strict_decode_reports_missing_field() {
        let err = PlanPrivate::decode(&[GpuStrategy::Scan as c_int, 256, 1])
            .expect_err("truncated plan header should fail");

        assert_eq!(
            err,
            DecodeError::MissingField {
                layout: "PlanPrivate",
                field: "fn_oid",
                index: 3,
                len: 3,
            }
        );
    }

    #[test]
    fn plan_private_decode_rejects_bad_batch_size() {
        let err = PlanPrivate::decode(&[
            GpuStrategy::Scan as c_int,
            0,
            1,
            42,
            7,
            AccelStrategy::GpuH3 as c_int,
        ])
        .expect_err("zero batch size should fail");

        assert_eq!(err, DecodeError::InvalidBatchSize { raw: 0 });
    }

    #[test]
    fn path_private_decodes_and_reencodes_header() {
        let raw = [777, 2, AccelStrategy::GpuHashJoin as c_int];

        let decoded = PathPrivate::decode(&raw).expect("valid path header should decode");

        assert_eq!(u32::from(decoded.fn_oid), 777);
        assert_eq!(decoded.target_attno, 2);
        assert_eq!(decoded.accel_strategy, AccelStrategy::GpuHashJoin);
        assert_eq!(decoded.to_ints(), raw);
    }

    #[test]
    fn resident_proof_trailer_decodes_snapshot_from_ints() {
        let stage_mask =
            ResidentOperatorStage::Scan.bit() | ResidentOperatorStage::Expression.bit();
        let raw = [
            GpuStrategy::Scan as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuExpr as c_int,
            RESIDENT_PROOF_SENTINEL,
            RESIDENT_PROOF_VERSION,
            ResidentOperatorClass::ResidentExpression as c_int,
            stage_mask as c_int,
            ResidentMaterializationKind::FinalOutput as c_int,
            2,
            1,
            1,
            CpuBoundaryReason::None as c_int,
        ];
        let fields = IntListReader::from_slice(&raw);

        let proof = deserialize_resident_proof_snapshot_from_reader(&fields)
            .expect("resident proof trailer should decode");

        assert!(proof.gpu_resident_pipeline());
        assert_eq!(
            proof.operator_class,
            ResidentOperatorClass::ResidentExpression
        );
        assert_eq!(proof.stage_mask, stage_mask);
        assert_eq!(proof.device_columns, 2);
        assert!(proof.has_device_selection);
        assert!(proof.has_device_projection);
        assert_eq!(proof.cpu_boundary, CpuBoundaryReason::None);
    }

    #[test]
    fn resident_proof_trailer_absence_returns_none() {
        let raw = [
            GpuStrategy::Scan as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuExpr as c_int,
        ];
        let fields = IntListReader::from_slice(&raw);

        assert!(deserialize_resident_proof_snapshot_from_reader(&fields).is_none());
    }

    #[test]
    fn resident_proof_trailer_ignores_payload_sentinel_collision() {
        let stage_mask = ResidentOperatorStage::Aggregate.bit();
        let raw = [
            GpuStrategy::Agg as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuReduce as c_int,
            RESIDENT_PROOF_SENTINEL,
            999,
            RESIDENT_PROOF_SENTINEL,
            RESIDENT_PROOF_VERSION,
            ResidentOperatorClass::ResidentGroupAgg as c_int,
            stage_mask as c_int,
            ResidentMaterializationKind::FinalOutput as c_int,
            1,
            0,
            0,
            CpuBoundaryReason::None as c_int,
        ];
        let fields = IntListReader::from_slice(&raw);

        let proof = deserialize_resident_proof_snapshot_from_reader(&fields)
            .expect("tail resident proof trailer should decode");

        assert!(proof.gpu_resident_pipeline());
        assert_eq!(
            proof.operator_class,
            ResidentOperatorClass::ResidentGroupAgg
        );
        assert_eq!(proof.stage_mask, stage_mask);
        assert_eq!(proof.device_columns, 1);
    }

    #[test]
    fn resident_proof_snapshot_zero_stage_mask_is_not_resident() {
        let raw = [
            GpuStrategy::Scan as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuExpr as c_int,
            RESIDENT_PROOF_SENTINEL,
            RESIDENT_PROOF_VERSION,
            ResidentOperatorClass::ResidentExpression as c_int,
            0,
            ResidentMaterializationKind::FinalOutput as c_int,
            1,
            0,
            0,
            CpuBoundaryReason::None as c_int,
        ];
        let fields = IntListReader::from_slice(&raw);

        let proof = deserialize_resident_proof_snapshot_from_reader(&fields)
            .expect("tail resident proof trailer should decode");

        assert!(!proof.gpu_resident_pipeline());
    }

    #[test]
    fn resident_proof_snapshot_unspecified_operator_class_is_not_resident() {
        let raw = [
            GpuStrategy::Scan as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuExpr as c_int,
            RESIDENT_PROOF_SENTINEL,
            RESIDENT_PROOF_VERSION,
            ResidentOperatorClass::Unspecified as c_int,
            ResidentOperatorStage::Scan.bit() as c_int,
            ResidentMaterializationKind::FinalOutput as c_int,
            1,
            0,
            0,
            CpuBoundaryReason::None as c_int,
        ];
        let fields = IntListReader::from_slice(&raw);

        let proof = deserialize_resident_proof_snapshot_from_reader(&fields)
            .expect("tail resident proof trailer should decode");

        assert!(!proof.gpu_resident_pipeline());
        assert_eq!(
            proof.to_proof(),
            Err(ResidentProofDecodeError::MissingOperatorClass)
        );
    }

    #[test]
    fn strategy_default_resident_proofs_are_host_staged() {
        for strategy in [
            GpuStrategy::Scan,
            GpuStrategy::Join,
            GpuStrategy::Agg,
            GpuStrategy::Sort,
            GpuStrategy::Window,
            GpuStrategy::PreAgg,
            GpuStrategy::FunctionScan,
            GpuStrategy::SrfTargetList,
        ] {
            let proof = resident_proof_default_for_strategy(strategy);

            assert!(!proof.gpu_resident_pipeline());
            assert_eq!(
                proof.materialization_kind,
                ResidentMaterializationKind::HostIntermediate
            );
            assert!(proof.cpu_boundary.blocks_resident_pipeline());
        }
    }

    #[test]
    fn agg_payload_decodes_group_key2_before_packed_h3_tlist_and_relid() {
        const H3_TLIST_POS: i32 = 2;
        const H3_RESOLUTION: i32 = 9;
        let packed_tlist_pos = H3_TLIST_POS | (H3_RESOLUTION << 16);
        let raw = [
            GpuStrategy::Agg as c_int,
            256,
            1,
            0,
            0,
            AccelStrategy::GpuReduce as c_int,
            1,
            AggOp::Count.to_i32(),
            0,
            u32::from(pg_sys::INT8OID) as c_int,
            1,
            2,
            u32::from(pg_sys::INT8OID) as c_int,
            H3_LATLNG_GROUP_KEY_TYPE,
            0,
            packed_tlist_pos,
            42,
            0,
        ];
        let fields = IntListReader::from_slice(&raw);
        let payload = PlanPayloadReader::new(&fields);

        assert_eq!(payload.agg_count(), 1);
        assert_eq!(payload.agg_columns(1).len(), 1);
        let (group_key, group_key_tlist_pos) = payload.agg_group_key(1);
        let group_key = group_key.expect("H3 group key should decode");
        assert_eq!(group_key.attno, 2);
        assert_eq!(group_key.type_oid, pg_sys::INT8OID);
        assert_eq!(group_key.key_type, H3_LATLNG_GROUP_KEY_TYPE);
        assert_eq!(group_key_tlist_pos, packed_tlist_pos as usize);
        let (self_scan_relid, agg_scan_expr, partial, olap) =
            payload.agg_self_scan_relid_and_trailers(1);
        assert_eq!(self_scan_relid, 42);
        assert!(agg_scan_expr.is_none());
        assert!(partial.is_none());
        assert!(olap.is_none());
    }

    #[test]
    fn resident_dense_grouped_logical_payload_decodes_v8() {
        let layout = ResidentDenseGroupedF64Layout::GroupSumCount;
        let mut raw = vec![
            OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL,
            123,
            layout.to_i32(),
            ResidentMeasureOp::Mul.to_i32(),
            1,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
            0,
        ];
        raw.extend([0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES * 4]);
        raw.extend([
            ResidentGroupAggGroupKeyExpr::ResidentI32.to_i32(),
            ResidentGroupAggMeasureExpr::BinaryMul.to_i32(),
            ResidentGroupAggFilterExpr::CaseBool.to_i32(),
            layout.aggregate_mask() as c_int,
        ]);
        let fields = IntListReader::from_slice(&raw);

        let (spec, next_idx) = deserialize_olap_agg_spec_from_reader(&fields, 0)
            .expect("v8 resident dense grouped payload should decode");

        assert_eq!(next_idx, raw.len());
        let OlapAggSpec::ResidentDenseGroupedF64(spec) = spec else {
            panic!("expected resident dense grouped f64 spec");
        };
        assert_eq!(u32::from(spec.rel_oid), 123);
        assert_eq!(
            spec.logical.group_key_expr,
            ResidentGroupAggGroupKeyExpr::ResidentI32
        );
        assert_eq!(
            spec.logical.measure_expr,
            ResidentGroupAggMeasureExpr::BinaryMul
        );
        assert_eq!(
            spec.logical.filter_expr,
            ResidentGroupAggFilterExpr::CaseBool
        );
        assert_eq!(spec.logical.aggregate_lane_mask, layout.aggregate_mask());
        assert_eq!(spec.source, ResidentDenseGroupedF64Source::UNKNOWN);
    }

    #[test]
    fn resident_dense_grouped_logical_source_payload_decodes_v9() {
        let layout = ResidentDenseGroupedF64Layout::GroupSumCount;
        let source = ResidentDenseGroupedF64Source {
            group_attno: 2,
            value_attno: 3,
            value_rhs_attno: 4,
            filter_attno: 5,
        };
        let mut raw = vec![
            OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE,
            123,
            source.group_attno,
            source.value_attno,
            source.value_rhs_attno,
            source.filter_attno,
            layout.to_i32(),
            ResidentMeasureOp::Mul.to_i32(),
            1,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
            0,
        ];
        raw.extend([0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES * 4]);
        raw.extend([
            ResidentGroupAggGroupKeyExpr::ResidentI32.to_i32(),
            ResidentGroupAggMeasureExpr::BinaryMul.to_i32(),
            ResidentGroupAggFilterExpr::CaseBool.to_i32(),
            layout.aggregate_mask() as c_int,
        ]);
        let fields = IntListReader::from_slice(&raw);

        let (spec, next_idx) = deserialize_olap_agg_spec_from_reader(&fields, 0)
            .expect("v9 resident dense grouped payload should decode");

        assert_eq!(next_idx, raw.len());
        let OlapAggSpec::ResidentDenseGroupedF64(spec) = spec else {
            panic!("expected resident dense grouped f64 spec");
        };
        assert_eq!(u32::from(spec.rel_oid), 123);
        assert_eq!(spec.source, source);
        assert_eq!(
            spec.logical.measure_expr,
            ResidentGroupAggMeasureExpr::BinaryMul
        );
        assert_eq!(
            spec.logical.filter_expr,
            ResidentGroupAggFilterExpr::CaseBool
        );
    }

    #[test]
    fn resident_dense_grouped_logical_payload_rejects_bad_logical_values() {
        let layout = ResidentDenseGroupedF64Layout::GroupSumCount;
        let mut raw = vec![
            OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL,
            123,
            layout.to_i32(),
            ResidentMeasureOp::Mul.to_i32(),
            1,
            ResidentDenseGroupedF64FilterMode::MeasurePredicate.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
            0,
        ];
        raw.extend([0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES * 4]);
        let logical_start = raw.len();
        raw.extend([
            ResidentGroupAggGroupKeyExpr::ResidentI32.to_i32(),
            ResidentGroupAggMeasureExpr::BinaryMul.to_i32(),
            ResidentGroupAggFilterExpr::CaseBool.to_i32(),
            layout.aggregate_mask() as c_int,
        ]);

        for offset in 0..3 {
            let mut bad = raw.clone();
            bad[logical_start + offset] = 999;
            let fields = IntListReader::from_slice(&bad);
            assert!(
                deserialize_olap_agg_spec_from_reader(&fields, 0).is_none(),
                "bad logical discriminant at offset {offset} should reject"
            );
        }

        let mut bad_mask = raw;
        bad_mask[logical_start + 3] = ResidentDenseGroupedF64Layout::GroupMinMaxAvg
            .aggregate_mask()
            .try_into()
            .expect("mask fits");
        let fields = IntListReader::from_slice(&bad_mask);
        assert!(
            deserialize_olap_agg_spec_from_reader(&fields, 0).is_none(),
            "logical aggregate mask must match layout"
        );
    }

    #[test]
    fn resident_dense_grouped_source_payload_infers_logical_spec() {
        let layout = ResidentDenseGroupedF64Layout::GroupSumAvgCount;
        let mut raw = vec![
            OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE,
            456,
            layout.to_i32(),
            ResidentMeasureOp::Column.to_i32(),
            0,
            ResidentDenseGroupedF64FilterMode::AggregateFilter.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly.to_i32(),
            ResidentDenseGroupedF64MeasurePredicateSource::Value.to_i32(),
            0,
        ];
        raw.extend([0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES * 4]);
        let fields = IntListReader::from_slice(&raw);

        let (spec, next_idx) = deserialize_olap_agg_spec_from_reader(&fields, 0)
            .expect("v7 resident dense grouped payload should decode");

        assert_eq!(next_idx, raw.len());
        let OlapAggSpec::ResidentDenseGroupedF64(spec) = spec else {
            panic!("expected resident dense grouped f64 spec");
        };
        assert_eq!(u32::from(spec.rel_oid), 456);
        assert_eq!(
            spec.logical.group_key_expr,
            ResidentGroupAggGroupKeyExpr::ResidentI32
        );
        assert_eq!(
            spec.logical.measure_expr,
            ResidentGroupAggMeasureExpr::DirectColumn
        );
        assert_eq!(
            spec.logical.filter_expr,
            ResidentGroupAggFilterExpr::AggregateFilterBool
        );
        assert_eq!(spec.logical.aggregate_lane_mask, layout.aggregate_mask());
        assert_eq!(spec.source, ResidentDenseGroupedF64Source::UNKNOWN);
    }
}

/// Deserialized acceleration metadata from `custom_private`.
pub(super) struct CustomPrivateData {
    pub(super) gpu_strategy: GpuStrategy,
    pub(super) batch_size: c_int,
    pub(super) fn_oid: pg_sys::Oid,
    pub(super) target_attno: i32,
    pub(super) accel_strategy: AccelStrategy,
    /// Aggregate column descriptors `(AggOp, attno, result_type_oid)`.
    /// Only meaningful when `gpu_strategy == Agg`.
    pub(super) agg_columns: Vec<(AggOp, i32, u32)>,
    /// Group key info for grouped aggregation.
    /// Only meaningful when `gpu_strategy == Agg` and GROUP BY is present.
    pub(super) group_key: Option<GroupKeyInfo>,
    /// 0-based position of group key in the output target list.
    pub(super) group_key_tlist_pos: usize,
    /// Inner relation join key attno (1-based). Only for `GpuHashJoin`.
    pub(super) hash_inner_attno: i32,
    /// Key type for hash join (0=i32, 1=i64, 2=f64). Only for `GpuHashJoin`.
    pub(super) hash_key_type: i32,
    /// True for the fused `COUNT(*)` over `GpuHashJoin` path.
    pub(super) hash_count_only: bool,
    /// True when count-only hashjoin reads preloaded device-resident key buffers.
    pub(super) hash_resident_count: bool,
    /// Expected outer relation OID for the resident hashjoin cache.
    pub(super) hash_outer_rel_oid: pg_sys::Oid,
    /// Expected inner relation OID for the resident hashjoin cache.
    pub(super) hash_inner_rel_oid: pg_sys::Oid,
    /// NLJ predicate shape. `2` = BETWEEN/range-containment.
    pub(super) nlj_shape: i32,
    /// NLJ key type (0=i32 promoted to i64, 1=i64, 2=f64).
    pub(super) nlj_key_type: i32,
    /// NLJ inequality opcode for single-predicate shapes. Reserved for the
    /// non-BETWEEN shape; decoded now so the private-data layout stays stable.
    #[allow(dead_code)]
    pub(super) nlj_op: i32,
    /// Inner lower-bound attno for BETWEEN NLJ.
    pub(super) nlj_inner_lo_attno: i32,
    /// Inner upper-bound attno for BETWEEN NLJ.
    pub(super) nlj_inner_hi_attno: i32,
    /// Window function specifications. Only meaningful when `gpu_strategy == Window`.
    pub(super) window_specs: Vec<WindowFuncSpec>,
    /// Scan relation index for direct heap scan (Window vectorized path).
    /// 0 means use child plan; > 0 means open this relation directly.
    pub(super) window_scan_relid: pg_sys::Index,
    /// Base relation index for self-scanning (vectorized pipeline).
    /// When > 0, the executor opens its own heap scan instead of pulling
    /// tuples through ExecProcNode. Used by Agg and Sort strategies.
    pub(super) self_scan_relid: u32,
    /// Partial-aggregate spec (worker-side of a Gather plan). `Some` means the
    /// executor emits transition-state tuples instead of final aggregate values.
    /// `None` on non-parallel paths. Only meaningful when `gpu_strategy == Agg`.
    pub(super) partial: Option<PartialAggSpec>,
    /// Optional scan predicate for aggregate self-scan plans. Used by the
    /// parallel fused `COUNT(*) WHERE template_predicate` path so the
    /// aggregate owns a parallel table scan and applies the predicate on GPU.
    pub(super) agg_scan_expr: Option<CompiledExpr>,
    /// Optional resident OLAP aggregate payload. Only meaningful for
    /// `gpu_strategy == Agg`; executor must not pull a child plan in this mode.
    pub(super) olap_agg: Option<OlapAggSpec>,
    /// Versioned resident-pipeline proof decoded from the plan trailer.
    pub(super) resident_proof: ResidentProofSnapshot,
}

impl CustomPrivateData {
    #[must_use]
    pub(super) fn hash_join_validation_error(&self) -> Option<&'static str> {
        if self.accel_strategy != AccelStrategy::GpuHashJoin {
            return None;
        }
        if self.gpu_strategy != GpuStrategy::Join {
            return Some("hash join accel requires join strategy");
        }
        if self.target_attno <= 0 || self.hash_inner_attno <= 0 {
            return Some("join key attno must be positive");
        }
        if !matches!(self.hash_key_type, 0..=2) {
            return Some("join key type is unsupported");
        }
        if self.hash_resident_count {
            if !self.hash_count_only {
                return Some("resident hash join requires count-only mode");
            }
            if self.hash_outer_rel_oid == pg_sys::InvalidOid
                || self.hash_inner_rel_oid == pg_sys::InvalidOid
            {
                return Some("resident hash join requires relation OIDs");
            }
        }
        None
    }

    #[must_use]
    pub(super) fn nlj_validation_error(&self) -> Option<&'static str> {
        if self.accel_strategy != AccelStrategy::GpuNestedLoopIneq {
            return None;
        }
        if self.gpu_strategy != GpuStrategy::Join {
            return Some("NLJ accel requires join strategy");
        }
        if self.nlj_shape != 2 {
            return Some("NLJ shape is unsupported");
        }
        if self.target_attno <= 0 || self.nlj_inner_lo_attno <= 0 || self.nlj_inner_hi_attno <= 0 {
            return Some("NLJ attnos must be positive");
        }
        if !matches!(self.nlj_key_type, 0..=2) {
            return Some("NLJ key type is unsupported");
        }
        None
    }
}

struct PlanPayloadReader<'reader, 'source> {
    fields: &'reader IntListReader<'source>,
}

impl<'reader, 'source> PlanPayloadReader<'reader, 'source> {
    const AGG_COUNT: usize = PLAN_PAYLOAD_START;
    const AGGS: usize = Self::AGG_COUNT + 1;
    const WINDOW_COUNT: usize = PLAN_PAYLOAD_START;
    const WINDOW_SPECS: usize = Self::WINDOW_COUNT + 1;
    const HASH_INNER_ATTNO: usize = PLAN_PAYLOAD_START;
    const HASH_KEY_TYPE: usize = PLAN_PAYLOAD_START + 1;
    const HASH_COUNT_ONLY: usize = PLAN_PAYLOAD_START + 2;
    const HASH_RESIDENT_COUNT: usize = PLAN_PAYLOAD_START + 3;
    const HASH_OUTER_REL_OID: usize = PLAN_PAYLOAD_START + 4;
    const HASH_INNER_REL_OID: usize = PLAN_PAYLOAD_START + 5;
    const NLJ_SHAPE: usize = PLAN_PAYLOAD_START;
    const NLJ_KEY_TYPE: usize = PLAN_PAYLOAD_START + 1;
    const NLJ_OP: usize = PLAN_PAYLOAD_START + 2;
    const NLJ_INNER_LO_ATTNO: usize = PLAN_PAYLOAD_START + 3;
    const NLJ_INNER_HI_ATTNO: usize = PLAN_PAYLOAD_START + 4;

    #[must_use]
    const fn new(fields: &'reader IntListReader<'source>) -> Self {
        PlanPayloadReader { fields }
    }

    #[must_use]
    fn agg_count(&self) -> usize {
        self.fields.int_at(Self::AGG_COUNT) as usize
    }

    #[must_use]
    fn agg_columns(&self, count: usize) -> Vec<(AggOp, i32, u32)> {
        let mut columns = Vec::with_capacity(count);
        for agg_index in 0..count {
            let offset = Self::AGGS + agg_index * 3;
            if !self.fields.contains_range(offset, 3) {
                break;
            }
            let mut agg = self.fields.cursor_at(offset);
            let op_raw = agg.read_int();
            let Some(op) = AggOp::from_i32(op_raw) else {
                pgrx::error!("pg_accel: invalid aggregate op tag {op_raw}");
            };
            columns.push((op, agg.read_int(), agg.read_u32()));
        }
        columns
    }

    #[must_use]
    fn agg_group_key(&self, agg_count: usize) -> (Option<GroupKeyInfo>, usize) {
        let group_key_base = Self::AGGS + agg_count * 3;
        let Some(has_group_key) = self.fields.get(group_key_base) else {
            return (None, 0);
        };
        if has_group_key == 0 || !self.fields.contains_range(group_key_base + 1, 5) {
            return (None, 0);
        }

        let mut group_key = self.fields.cursor_at(group_key_base + 1);
        let attno = group_key.read_int();
        let type_oid = group_key.read_oid();
        let key_type = group_key.read_int();
        let _group_key2_attno = group_key.read_int();
        let tlist_pos = group_key.read_int();

        if attno > 0
            && (matches!(key_type, 0 | 1 | 2 | 4 | 5) || is_h3_synthetic_group_key(key_type))
        {
            (
                Some(GroupKeyInfo {
                    attno,
                    type_oid,
                    key_type,
                }),
                tlist_pos as usize,
            )
        } else {
            (None, 0)
        }
    }

    #[must_use]
    fn agg_self_scan_relid_and_trailers(
        &self,
        agg_count: usize,
    ) -> (
        u32,
        Option<CompiledExpr>,
        Option<PartialAggSpec>,
        Option<OlapAggSpec>,
    ) {
        let relid_idx = self.agg_self_scan_relid_index(agg_count);
        let relid = self
            .fields
            .get(relid_idx)
            .map_or(0, |value| value as pg_sys::Index);
        let mut scan_expr = None;
        let mut partial = None;
        let mut olap = None;
        let mut trailer_idx = relid_idx + 1;
        while trailer_idx < self.fields.len() {
            match self.fields.int_at(trailer_idx) {
                AGG_SCAN_EXPR_SENTINEL => {
                    let Some((expr, next_idx)) =
                        deserialize_agg_scan_expr_from_reader(self.fields, trailer_idx + 1)
                    else {
                        break;
                    };
                    scan_expr = Some(expr);
                    trailer_idx = next_idx;
                }
                AGG_OLAP_SENTINEL => {
                    let Some((spec, next_idx)) =
                        deserialize_olap_agg_spec_from_reader(self.fields, trailer_idx + 1)
                    else {
                        break;
                    };
                    olap = Some(spec);
                    trailer_idx = next_idx;
                }
                PARTIAL_SENTINEL => {
                    partial = deserialize_partial_spec_from_reader(self.fields, trailer_idx + 1);
                    break;
                }
                _ => break,
            }
        }
        (relid, scan_expr, partial, olap)
    }

    #[must_use]
    fn agg_self_scan_relid_index(&self, agg_count: usize) -> usize {
        let group_key_base = Self::AGGS + agg_count * 3;
        let Some(has_group_key) = self.fields.get(group_key_base) else {
            return self.fields.len().saturating_sub(1);
        };
        if has_group_key == 0 {
            return group_key_base + 1;
        }
        group_key_base + 6
    }

    #[must_use]
    fn window_specs(&self) -> Vec<WindowFuncSpec> {
        let count = self.fields.int_at(Self::WINDOW_COUNT) as usize;
        let mut specs = Vec::with_capacity(count);
        for spec_index in 0..count {
            let offset = Self::WINDOW_SPECS + spec_index * WINDOW_SPEC_INTS;
            if !self.fields.contains_range(offset, WINDOW_SPEC_INTS) {
                break;
            }
            let mut spec = self.fields.cursor_at(offset);
            let Some(func) = WindowFunc::from_i32(spec.read_int()) else {
                break;
            };
            specs.push(WindowFuncSpec {
                func,
                partition_attno: spec.read_int(),
                order_attno: spec.read_int(),
                value_attno: spec.read_int(),
                offset: spec.read_int(),
                default_val: f64::from_bits(spec.read_int() as u64),
                result_type_oid: spec.read_u32(),
                uses_fp64: spec.read_bool(),
            });
        }
        specs
    }

    #[must_use]
    fn window_scan_relid(&self) -> pg_sys::Index {
        let spec_count = self.fields.int_at(Self::WINDOW_COUNT) as usize;
        let relid_idx = Self::WINDOW_SPECS + spec_count * WINDOW_SPEC_INTS;
        self.fields
            .get(relid_idx)
            .map_or(0, |value| value as pg_sys::Index)
    }

    #[must_use]
    fn hash_join_info(&self) -> (i32, i32, bool, bool, pg_sys::Oid, pg_sys::Oid) {
        if self.fields.contains_range(Self::HASH_INNER_ATTNO, 2) {
            (
                self.fields.int_at(Self::HASH_INNER_ATTNO),
                self.fields.int_at(Self::HASH_KEY_TYPE),
                self.fields
                    .get(Self::HASH_COUNT_ONLY)
                    .is_some_and(|value| value != 0),
                self.fields
                    .get(Self::HASH_RESIDENT_COUNT)
                    .is_some_and(|value| value != 0),
                self.fields
                    .get(Self::HASH_OUTER_REL_OID)
                    .map_or(pg_sys::InvalidOid, |value| pg_sys::Oid::from(value as u32)),
                self.fields
                    .get(Self::HASH_INNER_REL_OID)
                    .map_or(pg_sys::InvalidOid, |value| pg_sys::Oid::from(value as u32)),
            )
        } else {
            (0, 0, false, false, pg_sys::InvalidOid, pg_sys::InvalidOid)
        }
    }

    #[must_use]
    fn nlj_info(&self) -> (i32, i32, i32, i32, i32) {
        if self.fields.contains_range(Self::NLJ_SHAPE, 5) {
            (
                self.fields.int_at(Self::NLJ_SHAPE),
                self.fields.int_at(Self::NLJ_KEY_TYPE),
                self.fields.int_at(Self::NLJ_OP),
                self.fields.int_at(Self::NLJ_INNER_LO_ATTNO),
                self.fields.int_at(Self::NLJ_INNER_HI_ATTNO),
            )
        } else {
            (0, 0, 0, 0, 0)
        }
    }
}

/// Deserialize strategy, batch size, and accel context from
/// `custom_private`.
///
/// Layout: `[strategy, batch_size, expected_threads, fn_oid, target_attno,
///   accel_strategy, ...strategy-specific payload...]`
///
/// Invalid or missing private data raises a PostgreSQL ERROR.
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` emitted by this build's
/// planner hooks.
#[allow(clippy::too_many_lines)]
pub(super) unsafe fn deserialize_custom_private(
    custom_private: *mut pg_sys::List,
) -> CustomPrivateData {
    if custom_private.is_null() {
        pgrx::error!("pg_accel: missing CustomScan private data");
    }

    // SAFETY: custom_private is a valid List of Integer nodes.
    let fields = unsafe { IntListReader::from_pg_list(custom_private) };
    let gpu_strategy_raw = require_reader_field(&fields, "CustomPrivateData", 0, "strategy")
        .unwrap_or_else(|err| pgrx::error!("pg_accel: invalid CustomScan private data: {err:?}"));
    let gpu_strategy = decode_gpu_strategy(gpu_strategy_raw)
        .unwrap_or_else(|err| pgrx::error!("pg_accel: invalid CustomScan private data: {err:?}"));
    let resident_proof = unsafe { deserialize_resident_proof_snapshot(custom_private) }
        .unwrap_or_else(|| resident_proof_default_for_strategy(gpu_strategy));

    if matches!(gpu_strategy, GpuStrategy::PreAgg) {
        let batch_size_raw = require_reader_field(&fields, "PreAggPlanPrivate", 1, "batch_size")
            .unwrap_or_else(|err| {
                pgrx::error!("pg_accel: invalid PreAgg CustomScan private data: {err:?}")
            });
        let batch_size = decode_batch_size(batch_size_raw).unwrap_or_else(|err| {
            pgrx::error!("pg_accel: invalid PreAgg CustomScan private data: {err:?}")
        });
        let _expected_threads =
            require_reader_field(&fields, "PreAggPlanPrivate", 2, "expected_threads")
                .unwrap_or_else(|err| {
                    pgrx::error!("pg_accel: invalid PreAgg CustomScan private data: {err:?}")
                });
        return CustomPrivateData {
            gpu_strategy,
            batch_size,
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuReduce,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            hash_count_only: false,
            hash_resident_count: false,
            hash_outer_rel_oid: pg_sys::InvalidOid,
            hash_inner_rel_oid: pg_sys::InvalidOid,
            nlj_shape: 0,
            nlj_key_type: 0,
            nlj_op: 0,
            nlj_inner_lo_attno: 0,
            nlj_inner_hi_attno: 0,
            window_specs: vec![],
            window_scan_relid: 0,
            self_scan_relid: 0,
            partial: None,
            agg_scan_expr: None,
            olap_agg: None,
            resident_proof,
        };
    }

    let plan_private = PlanPrivate::decode_reader(&fields)
        .unwrap_or_else(|err| pgrx::error!("pg_accel: invalid CustomScan private data: {err:?}"));
    let payload = PlanPayloadReader::new(&fields);
    let batch_size = plan_private.batch_size;
    let _expected_threads = plan_private.expected_threads;
    let fn_oid = plan_private.fn_oid;
    let target_attno = plan_private.target_attno;
    let accel_strategy = plan_private.accel_strategy;

    // GpuSort was retired: no plan-time serializer emits Sort payloads, so
    // the reader no longer decodes sort keys / limit / self-scan relid.
    if matches!(gpu_strategy, GpuStrategy::Sort) {
        pgrx::error!("pg_accel: GpuSort strategy retired; refusing to decode a Sort plan payload");
    }

    // For Agg strategy, read aggregate column descriptors starting at index 6.
    // Layout: [num_aggs, op0, attno0, rtype0, op1, attno1, rtype1, ...]
    let agg_count = if matches!(gpu_strategy, GpuStrategy::Agg) {
        payload.agg_count()
    } else {
        0
    };
    let agg_columns = if matches!(gpu_strategy, GpuStrategy::Agg) {
        payload.agg_columns(agg_count)
    } else {
        vec![]
    };

    // For Agg strategy, read optional group key info after agg descriptors.
    // Layout:
    //   [...agg descs..., has_group_key,
    //    gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos, self_scan_relid]
    let (group_key, group_key_tlist_pos) = if matches!(gpu_strategy, GpuStrategy::Agg) {
        payload.agg_group_key(agg_count)
    } else {
        (None, 0)
    };

    // For Window strategy, read window function specs starting at index 6.
    // Layout: [num_specs, func0, part_attno0, order_attno0, value_attno0,
    //   offset0, default_bits0, result_type0, ...]
    let window_specs = if matches!(gpu_strategy, GpuStrategy::Window) {
        payload.window_specs()
    } else {
        vec![]
    };

    // For Window strategy, read scan_relid after the specs.
    // Plan layout: [...base 6 fields..., num_specs, spec0..., scan_relid]
    let window_scan_relid: pg_sys::Index = if matches!(gpu_strategy, GpuStrategy::Window) {
        payload.window_scan_relid()
    } else {
        0
    };

    // For Join strategy with GpuHashJoin accel, read hash join info at index 6+.
    // Layout: [...base 6 fields..., inner_attno, key_type]
    let (
        hash_inner_attno,
        hash_key_type,
        hash_count_only,
        hash_resident_count,
        hash_outer_rel_oid,
        hash_inner_rel_oid,
    ) = if accel_strategy == AccelStrategy::GpuHashJoin {
        payload.hash_join_info()
    } else {
        (0, 0, false, false, pg_sys::InvalidOid, pg_sys::InvalidOid)
    };

    // For Join strategy with GpuNestedLoopIneq accel, read NLJ info at index 6+.
    // Layout: [...base 6 fields..., shape, key_type, op, inner_lo_attno, inner_hi_attno]
    let (nlj_shape, nlj_key_type, nlj_op, nlj_inner_lo_attno, nlj_inner_hi_attno) =
        if accel_strategy == AccelStrategy::GpuNestedLoopIneq {
            payload.nlj_info()
        } else {
            (0, 0, 0, 0, 0)
        };

    // For Agg strategy, find `self_scan_relid` (immediately follows the
    // group-key block) and optionally a PartialAggSpec sentinel block.
    //
    // Layout (Agg):
    //   [..., num_aggs, (op,attno,rtype)*N,
    //    has_gk, (gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos)?,
    //    self_scan_relid,
    //    (PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols)?]
    //
    // The partial block is optional: non-parallel plans omit it entirely.
    let (self_scan_relid, agg_scan_expr, partial, olap_agg) =
        if matches!(gpu_strategy, GpuStrategy::Agg) {
            payload.agg_self_scan_relid_and_trailers(agg_count)
        } else {
            (0, None, None, None)
        };

    CustomPrivateData {
        gpu_strategy,
        batch_size,
        fn_oid,
        target_attno,
        accel_strategy,
        agg_columns,
        group_key,
        group_key_tlist_pos,
        hash_inner_attno,
        hash_key_type,
        hash_count_only,
        hash_resident_count,
        hash_outer_rel_oid,
        hash_inner_rel_oid,
        nlj_shape,
        nlj_key_type,
        nlj_op,
        nlj_inner_lo_attno,
        nlj_inner_hi_attno,
        window_specs,
        window_scan_relid,
        self_scan_relid,
        partial,
        agg_scan_expr,
        olap_agg,
        resident_proof,
    }
}

// ---------------------------------------------------------------------------
// PartialAggSpec serialization / deserialization
// ---------------------------------------------------------------------------

/// Magic marker preceding a serialized [`PartialAggSpec`] in `custom_private`.
/// Chosen to be distinct from any plausible scalar field so mistaken layouts
/// don't silently deserialize as partial-agg metadata.
pub(in crate::engine::ffi) const PARTIAL_SENTINEL: c_int = 0x5041_4147; // b"PAAG"

/// Magic marker preceding an aggregate self-scan template expression in
/// `custom_private`.
pub(in crate::engine::ffi) const AGG_SCAN_EXPR_SENTINEL: c_int = 0x4147_5850; // b"AGXP"

/// Magic marker preceding an aggregate OLAP submode payload.
pub(in crate::engine::ffi) const AGG_OLAP_SENTINEL: c_int = 0x4F4C_4150; // b"OLAP"
/// Path-level marker for the childless resident `COUNT(*)` hashjoin payload.
pub(in crate::engine::ffi) const HASH_JOIN_RESIDENT_COUNT_SENTINEL: c_int = 0x484A_5243; // b"HJRC"
const OLAP_KIND_SSBM_Q1_REVENUE: c_int = 1;
const OLAP_KIND_SSBM_Q2_GROUPED_REVENUE: c_int = 2;
const OLAP_KIND_SSBM_Q3_GROUPED_REVENUE: c_int = 3;
const OLAP_KIND_SSBM_Q4_GROUPED_PROFIT: c_int = 4;
const OLAP_KIND_RESIDENT_DENSE_GROUPED_F64: c_int = 5;
const OLAP_KIND_RESIDENT_H3_GROUPED_COUNT: c_int = 6;
const OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE: c_int = 7;
const OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL: c_int = 8;
const OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE: c_int = 9;
const OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64: c_int = 10;
const OLAP_DATE_YEAR: c_int = 1;
const OLAP_DATE_YEARMONTHNUM: c_int = 2;
const OLAP_DATE_YEAR_WEEK: c_int = 3;

/// Append a resident OLAP aggregate payload after the standard Agg fields.
///
/// # Safety
/// Must be called in a valid PostgreSQL planner memory context.
pub(in crate::engine::ffi) unsafe fn append_olap_agg_spec(
    list: *mut pg_sys::List,
    spec: &OlapAggSpec,
) -> *mut pg_sys::List {
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(AGG_OLAP_SENTINEL);
    match spec {
        OlapAggSpec::SsbmQ1Revenue(spec) => {
            writer.push_int(OLAP_KIND_SSBM_Q1_REVENUE);
            writer.push_oid(spec.fact_rel_oid);
            writer.push_oid(spec.date_rel_oid);
            match spec.date_predicate {
                SsbmQ1DatePredicate::Year(year) => {
                    writer.push_int(OLAP_DATE_YEAR);
                    writer.push_int(year);
                    writer.push_int(0);
                }
                SsbmQ1DatePredicate::YearMonthNum(yearmonthnum) => {
                    writer.push_int(OLAP_DATE_YEARMONTHNUM);
                    writer.push_int(yearmonthnum);
                    writer.push_int(0);
                }
                SsbmQ1DatePredicate::YearWeek { year, week } => {
                    writer.push_int(OLAP_DATE_YEAR_WEEK);
                    writer.push_int(year);
                    writer.push_int(week);
                }
            }
            writer.push_int(spec.discount_lo);
            writer.push_int(spec.discount_hi);
            writer.push_int(spec.quantity_lo);
            writer.push_int(spec.quantity_hi);
        }
        OlapAggSpec::SsbmQ2GroupedRevenue(spec) => {
            writer.push_int(OLAP_KIND_SSBM_Q2_GROUPED_REVENUE);
            writer.push_oid(spec.fact_rel_oid);
            writer.push_oid(spec.date_rel_oid);
            writer.push_oid(spec.part_rel_oid);
            writer.push_oid(spec.supplier_rel_oid);
            writer.push_int(spec.variant.to_i32());
        }
        OlapAggSpec::SsbmQ3GroupedRevenue(spec) => {
            writer.push_int(OLAP_KIND_SSBM_Q3_GROUPED_REVENUE);
            writer.push_oid(spec.fact_rel_oid);
            writer.push_oid(spec.date_rel_oid);
            writer.push_oid(spec.customer_rel_oid);
            writer.push_oid(spec.supplier_rel_oid);
            writer.push_int(spec.variant.to_i32());
        }
        OlapAggSpec::SsbmQ4GroupedProfit(spec) => {
            writer.push_int(OLAP_KIND_SSBM_Q4_GROUPED_PROFIT);
            writer.push_oid(spec.fact_rel_oid);
            writer.push_oid(spec.date_rel_oid);
            writer.push_oid(spec.part_rel_oid);
            writer.push_oid(spec.customer_rel_oid);
            writer.push_oid(spec.supplier_rel_oid);
            writer.push_int(spec.variant.to_i32());
        }
        OlapAggSpec::ResidentDenseGroupedF64(spec) => {
            writer.push_int(OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE);
            writer.push_oid(spec.rel_oid);
            writer.push_int(spec.source.group_attno);
            writer.push_int(spec.source.value_attno);
            writer.push_int(spec.source.value_rhs_attno);
            writer.push_int(spec.source.filter_attno);
            writer.push_int(spec.layout.to_i32());
            writer.push_int(spec.measure_op.to_i32());
            writer.push_int(i32::from(spec.requires_rhs));
            writer.push_int(spec.filter_mode.to_i32());
            writer.push_int(spec.measure_predicate.op.to_i32());
            writer.push_int(spec.measure_predicate.source.to_i32());
            writer.push_int(i32::from(spec.measure_predicate.range_count));
            for idx in 0..RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES {
                writer.push_f64_halves(spec.measure_predicate.range_los[idx]);
                writer.push_f64_halves(spec.measure_predicate.range_his[idx]);
            }
            writer.push_int(spec.logical.group_key_expr.to_i32());
            writer.push_int(spec.logical.measure_expr.to_i32());
            writer.push_int(spec.logical.filter_expr.to_i32());
            writer.push_int(
                i32::try_from(spec.logical.aggregate_lane_mask)
                    .expect("resident groupagg aggregate mask fits in private data"),
            );
        }
        OlapAggSpec::ResidentStarDimGroupedF64(spec) => {
            writer.push_int(OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64);
            writer.push_oid(spec.shape.fact_rel_oid);
            writer.push_oid(spec.shape.dim_rel_oid);
            writer.push_int(spec.shape.fact_key_attno);
            writer.push_int(spec.shape.fact_value_attno);
            writer.push_int(spec.shape.dim_key_attno);
            writer.push_int(spec.shape.dim_group_attno);
            writer.push_int(spec.shape.dim_filter_attno);
            writer.push_int(i32::from(spec.shape.fact_value_cmp_opcode));
            writer.push_f64_halves(spec.shape.fact_value_cmp_const);
            writer.push_int(i32::from(spec.shape.dim_filter_cmp_opcode));
            writer.push_f64_halves(spec.shape.dim_filter_cmp_const);
            writer.push_int(spec.logical.group_key_expr.to_i32());
            writer.push_int(spec.logical.measure_expr.to_i32());
            writer.push_int(spec.logical.filter_expr.to_i32());
            writer.push_int(
                i32::try_from(spec.logical.aggregate_lane_mask)
                    .expect("resident star groupagg aggregate mask fits in private data"),
            );
        }
        OlapAggSpec::ResidentH3GroupedCount(spec) => {
            writer.push_int(OLAP_KIND_RESIDENT_H3_GROUPED_COUNT);
            writer.push_oid(spec.rel_oid);
            writer.push_int(spec.kind.to_i32());
            writer.push_int(spec.resolution);
        }
    }
    writer.into_list()
}

fn deserialize_olap_agg_spec_from_reader(
    fields: &IntListReader<'_>,
    start_idx: usize,
) -> Option<(OlapAggSpec, usize)> {
    if !fields.contains_range(start_idx, 1) {
        return None;
    }
    let mut cursor = fields.cursor_at(start_idx);
    let kind = cursor.read_int();
    match kind {
        OLAP_KIND_SSBM_Q1_REVENUE => {
            if !fields.contains_range(start_idx, 10) {
                return None;
            }
            let fact_rel_oid = cursor.read_oid();
            let date_rel_oid = cursor.read_oid();
            let date_kind = cursor.read_int();
            let date_arg1 = cursor.read_int();
            let date_arg2 = cursor.read_int();
            let date_predicate = match date_kind {
                OLAP_DATE_YEAR => SsbmQ1DatePredicate::Year(date_arg1),
                OLAP_DATE_YEARMONTHNUM => SsbmQ1DatePredicate::YearMonthNum(date_arg1),
                OLAP_DATE_YEAR_WEEK => SsbmQ1DatePredicate::YearWeek {
                    year: date_arg1,
                    week: date_arg2,
                },
                _ => return None,
            };
            let discount_lo = cursor.read_int();
            let discount_hi = cursor.read_int();
            let quantity_lo = cursor.read_int();
            let quantity_hi = cursor.read_int();
            Some((
                OlapAggSpec::SsbmQ1Revenue(SsbmQ1RevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    date_predicate,
                    discount_lo,
                    discount_hi,
                    quantity_lo,
                    quantity_hi,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_SSBM_Q2_GROUPED_REVENUE => {
            if !fields.contains_range(start_idx, 6) {
                return None;
            }
            let fact_rel_oid = cursor.read_oid();
            let date_rel_oid = cursor.read_oid();
            let part_rel_oid = cursor.read_oid();
            let supplier_rel_oid = cursor.read_oid();
            let variant = SsbmQ2Variant::from_i32(cursor.read_int())?;
            Some((
                OlapAggSpec::SsbmQ2GroupedRevenue(SsbmQ2GroupedRevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    part_rel_oid,
                    supplier_rel_oid,
                    variant,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_SSBM_Q3_GROUPED_REVENUE => {
            if !fields.contains_range(start_idx, 6) {
                return None;
            }
            let fact_rel_oid = cursor.read_oid();
            let date_rel_oid = cursor.read_oid();
            let customer_rel_oid = cursor.read_oid();
            let supplier_rel_oid = cursor.read_oid();
            let variant = SsbmQ3Variant::from_i32(cursor.read_int())?;
            Some((
                OlapAggSpec::SsbmQ3GroupedRevenue(SsbmQ3GroupedRevenueSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    customer_rel_oid,
                    supplier_rel_oid,
                    variant,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_SSBM_Q4_GROUPED_PROFIT => {
            if !fields.contains_range(start_idx, 7) {
                return None;
            }
            let fact_rel_oid = cursor.read_oid();
            let date_rel_oid = cursor.read_oid();
            let part_rel_oid = cursor.read_oid();
            let customer_rel_oid = cursor.read_oid();
            let supplier_rel_oid = cursor.read_oid();
            let variant = SsbmQ4Variant::from_i32(cursor.read_int())?;
            Some((
                OlapAggSpec::SsbmQ4GroupedProfit(SsbmQ4GroupedProfitSpec {
                    fact_rel_oid,
                    date_rel_oid,
                    part_rel_oid,
                    customer_rel_oid,
                    supplier_rel_oid,
                    variant,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE => {
            let has_predicate_source = kind != OLAP_KIND_RESIDENT_DENSE_GROUPED_F64;
            let has_logical = matches!(
                kind,
                OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL
                    | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE
            );
            let has_source_attnos = kind == OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE;
            let expected_ints = if has_source_attnos {
                33
            } else if has_logical {
                29
            } else if has_predicate_source {
                25
            } else {
                24
            };
            if !fields.contains_range(start_idx, expected_ints) {
                return None;
            }
            let rel_oid = cursor.read_oid();
            let source = if has_source_attnos {
                ResidentDenseGroupedF64Source {
                    group_attno: cursor.read_int(),
                    value_attno: cursor.read_int(),
                    value_rhs_attno: cursor.read_int(),
                    filter_attno: cursor.read_int(),
                }
            } else {
                ResidentDenseGroupedF64Source::UNKNOWN
            };
            let layout = ResidentDenseGroupedF64Layout::from_i32(cursor.read_int())?;
            let measure_op = ResidentMeasureOp::from_i32(cursor.read_int())?;
            let requires_rhs = cursor.read_int() != 0;
            let filter_mode = ResidentDenseGroupedF64FilterMode::from_i32(cursor.read_int())?;
            if has_source_attnos {
                let group_attno_valid = if layout.is_single_group() {
                    source.group_attno == 0
                } else {
                    source.group_attno > 0
                };
                if !group_attno_valid
                    || source.value_attno <= 0
                    || source.value_rhs_attno < 0
                    || source.filter_attno < 0
                    || (source.value_rhs_attno > 0) != requires_rhs
                    || (filter_mode != ResidentDenseGroupedF64FilterMode::None
                        && source.filter_attno <= 0)
                {
                    return None;
                }
            }
            let predicate_op =
                ResidentDenseGroupedF64MeasurePredicateOp::from_i32(cursor.read_int())?;
            let predicate_source = if has_predicate_source {
                ResidentDenseGroupedF64MeasurePredicateSource::from_i32(cursor.read_int())?
            } else if predicate_op == ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly {
                ResidentDenseGroupedF64MeasurePredicateSource::Value
            } else {
                ResidentDenseGroupedF64MeasurePredicateSource::Rhs
            };
            let predicate_range_count = cursor.read_int();
            let predicate_range_count = u8::try_from(predicate_range_count).ok()?;
            if usize::from(predicate_range_count)
                > RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES
            {
                return None;
            }
            let mut predicate_range_los =
                [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES];
            let mut predicate_range_his =
                [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES];
            for idx in 0..RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES {
                predicate_range_los[idx] = cursor.read_f64_halves();
                predicate_range_his[idx] = cursor.read_f64_halves();
            }
            match predicate_op {
                ResidentDenseGroupedF64MeasurePredicateOp::BoolOnly => {
                    if predicate_source != ResidentDenseGroupedF64MeasurePredicateSource::Value
                        || predicate_range_count != 0
                    {
                        return None;
                    }
                }
                ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsBetween => {
                    if predicate_range_count != 1 {
                        return None;
                    }
                }
                ResidentDenseGroupedF64MeasurePredicateOp::BoolAndRhsRanges => {
                    if predicate_range_count == 0 {
                        return None;
                    }
                }
            }
            for idx in 0..usize::from(predicate_range_count) {
                if predicate_range_los[idx].is_nan()
                    || predicate_range_his[idx].is_nan()
                    || predicate_range_los[idx] > predicate_range_his[idx]
                {
                    return None;
                }
            }
            let measure_predicate = ResidentDenseGroupedF64MeasurePredicate {
                op: predicate_op,
                source: predicate_source,
                range_count: predicate_range_count,
                range_los: predicate_range_los,
                range_his: predicate_range_his,
            };
            let logical = if has_logical {
                let logical = ResidentGroupAggLogicalSpec::from_wire_values(
                    cursor.read_int(),
                    cursor.read_int(),
                    cursor.read_int(),
                    cursor.read_int(),
                )?;
                if logical.aggregate_lane_mask != layout.aggregate_mask() {
                    return None;
                }
                logical
            } else {
                ResidentGroupAggLogicalSpec::for_dense_grouped_f64(
                    layout,
                    measure_op,
                    requires_rhs,
                    filter_mode,
                    measure_predicate,
                )?
            };
            Some((
                OlapAggSpec::ResidentDenseGroupedF64(ResidentDenseGroupedF64Spec {
                    rel_oid,
                    source,
                    layout,
                    logical,
                    measure_op,
                    requires_rhs,
                    filter_mode,
                    measure_predicate,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64 => {
            if !fields.contains_range(start_idx, 18) {
                return None;
            }
            let fact_rel_oid = cursor.read_oid();
            let dim_rel_oid = cursor.read_oid();
            let fact_key_attno = cursor.read_int();
            let fact_value_attno = cursor.read_int();
            let dim_key_attno = cursor.read_int();
            let dim_group_attno = cursor.read_int();
            let dim_filter_attno = cursor.read_int();
            if fact_rel_oid == pg_sys::InvalidOid
                || dim_rel_oid == pg_sys::InvalidOid
                || fact_key_attno <= 0
                || fact_value_attno <= 0
                || dim_key_attno <= 0
                || dim_group_attno <= 0
                || dim_filter_attno <= 0
            {
                return None;
            }
            let fact_value_cmp_opcode = u16::try_from(cursor.read_int()).ok()?;
            let fact_value_cmp_const = cursor.read_f64_halves();
            let dim_filter_cmp_opcode = u16::try_from(cursor.read_int()).ok()?;
            let dim_filter_cmp_const = cursor.read_f64_halves();
            if fact_value_cmp_const.is_nan() || dim_filter_cmp_const.is_nan() {
                return None;
            }
            let logical = ResidentGroupAggLogicalSpec::from_wire_values(
                cursor.read_int(),
                cursor.read_int(),
                cursor.read_int(),
                cursor.read_int(),
            )?;
            if logical != ResidentGroupAggLogicalSpec::for_star_dim_grouped_f64() {
                return None;
            }
            Some((
                OlapAggSpec::ResidentStarDimGroupedF64(ResidentStarDimGroupedF64Spec {
                    shape: ResidentStarDimGroupAggCacheShape {
                        fact_rel_oid,
                        dim_rel_oid,
                        fact_key_attno,
                        fact_value_attno,
                        dim_key_attno,
                        dim_group_attno,
                        dim_filter_attno,
                        fact_value_cmp_opcode,
                        fact_value_cmp_const,
                        dim_filter_cmp_opcode,
                        dim_filter_cmp_const,
                    },
                    logical,
                }),
                cursor.position(),
            ))
        }
        OLAP_KIND_RESIDENT_H3_GROUPED_COUNT => {
            if !fields.contains_range(start_idx, 4) {
                return None;
            }
            let rel_oid = cursor.read_oid();
            let kind = ResidentH3GroupedCountKind::from_i32(cursor.read_int())?;
            let resolution = cursor.read_int();
            Some((
                OlapAggSpec::ResidentH3GroupedCount(ResidentH3GroupedCountSpec {
                    rel_oid,
                    kind,
                    resolution,
                }),
                cursor.position(),
            ))
        }
        _ => None,
    }
}

/// Magic marker for the B5a "PreAgg parallel-safe planner-attached" flag.
/// When present in the PreAgg `custom_private` layout (after `has_scan_expr`
/// and before any optional [`PARTIAL_SENTINEL`] block), the next list element
/// is a `c_int` (1 = the planner attached the fact path as `custom_paths[0]`
/// and marked the CustomPath `parallel_safe = true`; 0 = serial layout).
/// Round-tripped via [`PreAggPrivData::parallel_safe_planner_attached`].
/// Distinct from [`PARTIAL_SENTINEL`] so the deserializer can probe for
/// either marker independently. Old plans (serialized before B5a) lack this
/// block entirely; the deserializer treats absence as `false`.
pub(in crate::engine::ffi) const PREAGG_PARALLEL_ATTACHED_SENTINEL: c_int = 0x5050_5341; // b"PPSA"

/// Magic marker preceding a serialized [`ResidentProofSnapshot`] trailer.
///
/// This block is always appended after each strategy-specific payload so legacy
/// offsets stay stable: generic scan/sort/agg/window payloads still start at
/// index 6, FunctionScan/SRF sentinels still live at index 6, and PreAgg still
/// starts at index 3.
pub(in crate::engine::ffi) const RESIDENT_PROOF_SENTINEL: c_int = 0x5250_5246; // b"RPRF"
pub(in crate::engine::ffi) const RESIDENT_PROOF_VERSION: c_int = 2;
const RESIDENT_PROOF_TRAILER_INTS: usize = 9;

/// Strategy-local default proof for currently selected host-staged executors.
///
/// This is intentionally conservative: until a planner path provides a stronger
/// proof snapshot, selected CustomScans report a blocking CPU boundary.
#[must_use]
pub(in crate::engine::ffi) const fn resident_proof_default_for_strategy(
    strategy: GpuStrategy,
) -> ResidentProofSnapshot {
    ResidentProofSnapshot::host_staged(match strategy {
        GpuStrategy::Scan => CpuBoundaryReason::HostInputStaging,
        GpuStrategy::Join => CpuBoundaryReason::ExecProcNodeInput,
        GpuStrategy::Agg => CpuBoundaryReason::HostInputStaging,
        GpuStrategy::Sort => CpuBoundaryReason::HostTupleReconstruction,
        GpuStrategy::Window => CpuBoundaryReason::HostTupleReconstruction,
        GpuStrategy::PreAgg => CpuBoundaryReason::HostHashState,
        GpuStrategy::FunctionScan => CpuBoundaryReason::HostVariableOutput,
        GpuStrategy::SrfTargetList => CpuBoundaryReason::ExecProcNodeInput,
    })
}

/// Append a resident-proof snapshot as a versioned trailer.
///
/// # Safety
/// Must be called in a valid PG memory context on the main backend thread.
#[allow(clippy::cast_possible_wrap)]
pub(in crate::engine::ffi) unsafe fn append_resident_proof_snapshot(
    list: *mut pg_sys::List,
    proof: ResidentProofSnapshot,
) -> *mut pg_sys::List {
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(RESIDENT_PROOF_SENTINEL);
    writer.push_int(RESIDENT_PROOF_VERSION);
    writer.push_int(proof.operator_class.to_i32());
    writer.push_u32(proof.stage_mask);
    writer.push_int(proof.materialization_kind.to_i32());
    writer.push_u32(proof.device_columns);
    writer.push_bool(proof.has_device_selection);
    writer.push_bool(proof.has_device_projection);
    writer.push_int(proof.cpu_boundary.to_i32());
    writer.into_list()
}

/// Append the conservative host-staged proof for a strategy.
///
/// # Safety
/// Must be called in a valid PG memory context on the main backend thread.
pub(in crate::engine::ffi) unsafe fn append_host_staged_resident_proof(
    list: *mut pg_sys::List,
    strategy: GpuStrategy,
) -> *mut pg_sys::List {
    // SAFETY: delegated to the caller's valid PG memory context.
    unsafe { append_resident_proof_snapshot(list, resident_proof_default_for_strategy(strategy)) }
}

/// Decode the resident-proof trailer if this plan carries one.
///
/// Absence means "old layout"; callers should fall back to
/// [`resident_proof_default_for_strategy`]. A present but malformed trailer is
/// an ERROR, because silently treating corrupt proof data as valid would reopen
/// CPU-backed plan reporting.
///
/// # Safety
/// `list` must be a valid PG `List *` of `Integer` nodes.
pub(in crate::engine::ffi) unsafe fn deserialize_resident_proof_snapshot(
    list: *mut pg_sys::List,
) -> Option<ResidentProofSnapshot> {
    if list.is_null() {
        return None;
    }
    // SAFETY: caller guarantees a valid PG List.
    let fields = unsafe { IntListReader::from_pg_list(list) };
    deserialize_resident_proof_snapshot_from_reader(&fields)
}

fn deserialize_resident_proof_snapshot_from_reader(
    fields: &IntListReader<'_>,
) -> Option<ResidentProofSnapshot> {
    if fields.len() < RESIDENT_PROOF_TRAILER_INTS {
        return None;
    }
    let idx = fields.len() - RESIDENT_PROOF_TRAILER_INTS;
    if fields.int_at(idx) != RESIDENT_PROOF_SENTINEL {
        return None;
    }
    let mut proof = fields.cursor_at(idx + 1);
    let version = proof.read_int();
    if version != RESIDENT_PROOF_VERSION {
        pgrx::error!("pg_accel: unsupported resident proof trailer version {version}");
    }
    let operator_class_raw = proof.read_int();
    let Some(operator_class) = ResidentOperatorClass::from_i32(operator_class_raw) else {
        pgrx::error!("pg_accel: invalid resident proof operator class tag {operator_class_raw}");
    };
    let stage_mask = proof.read_u32();
    let materialization_raw = proof.read_int();
    let Some(materialization_kind) = ResidentMaterializationKind::from_i32(materialization_raw)
    else {
        pgrx::error!("pg_accel: invalid resident proof materialization tag {materialization_raw}");
    };
    let device_columns = proof.read_u32();
    let has_device_selection = proof.read_bool();
    let has_device_projection = proof.read_bool();
    let boundary_raw = proof.read_int();
    let Some(cpu_boundary) = CpuBoundaryReason::from_i32(boundary_raw) else {
        pgrx::error!("pg_accel: invalid resident proof CPU boundary tag {boundary_raw}");
    };
    Some(ResidentProofSnapshot {
        operator_class,
        stage_mask,
        materialization_kind,
        device_columns,
        has_device_selection,
        has_device_projection,
        cpu_boundary,
    })
}

/// Append a [`PartialAggSpec`] onto `list` using the sentinel-prefixed
/// layout consumed by `deserialize_partial_spec`.
///
/// Layout: `[PARTIAL_SENTINEL, n_cols, (op, attno, transtype_oid, serialize_fn_oid)*n_cols]`
/// where `serialize_fn_oid == 0` encodes `None`.
///
/// # Safety
/// Must be called in a valid PG memory context on the main backend thread.
#[allow(clippy::cast_possible_wrap)]
pub(in crate::engine::ffi) unsafe fn append_partial_spec(
    list: *mut pg_sys::List,
    spec: &PartialAggSpec,
) -> *mut pg_sys::List {
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(PARTIAL_SENTINEL);
    writer.push_len(spec.per_column.len());
    for col in &spec.per_column {
        writer.push_int(col.op.to_i32());
        writer.push_int(col.attno);
        writer.push_oid(col.transtype_oid);
        writer.push_u32(col.serialize_fn_oid.map_or(0, u32::from));
    }
    writer.into_list()
}

/// Deserialize a [`PartialAggSpec`] from `list` starting at `start_idx`
/// (the position of `n_cols`). Returns `None` when the list is too short
/// or `n_cols` is zero.
///
/// # Safety
/// `list` must be a valid PG `List` of `Integer` nodes.
#[allow(clippy::cast_sign_loss)]
pub(in crate::engine::ffi) unsafe fn deserialize_partial_spec(
    list: *mut pg_sys::List,
    start_idx: usize,
) -> Option<PartialAggSpec> {
    // SAFETY: caller guarantees a valid PG List.
    let fields = unsafe { IntListReader::from_pg_list(list) };
    deserialize_partial_spec_from_reader(&fields, start_idx)
}

#[must_use]
fn deserialize_agg_scan_expr_from_reader(
    fields: &IntListReader<'_>,
    start_idx: usize,
) -> Option<(CompiledExpr, usize)> {
    let template_type = fields.get(start_idx)?;
    let mut cursor = fields.cursor_at(start_idx + 1);
    match template_type {
        1 => {
            if !fields.contains_range(start_idx + 1, 4) {
                return None;
            }
            Some((
                CompiledExpr::Template(TemplateKernel::CmpConst {
                    col_idx: cursor.read_u32(),
                    cmp_opcode: cursor.read_int() as u16,
                    const_val: cursor.read_f64_halves(),
                }),
                start_idx + 5,
            ))
        }
        2 => {
            if !fields.contains_range(start_idx + 1, 5) {
                return None;
            }
            Some((
                CompiledExpr::Template(TemplateKernel::Between {
                    col_idx: cursor.read_u32(),
                    lo: cursor.read_f64_halves(),
                    hi: cursor.read_f64_halves(),
                }),
                start_idx + 6,
            ))
        }
        3 => {
            if !fields.contains_range(start_idx + 1, 8) {
                return None;
            }
            Some((
                CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx: cursor.read_u32(),
                    cmp1_opcode: cursor.read_int() as u16,
                    const1_val: cursor.read_f64_halves(),
                    col2_idx: cursor.read_u32(),
                    cmp2_opcode: cursor.read_int() as u16,
                    const2_val: cursor.read_f64_halves(),
                }),
                start_idx + 9,
            ))
        }
        _ => None,
    }
}

#[must_use]
fn deserialize_partial_spec_from_reader(
    fields: &IntListReader<'_>,
    start_idx: usize,
) -> Option<PartialAggSpec> {
    if start_idx >= fields.len() {
        return None;
    }
    let n_cols = fields.int_at(start_idx) as usize;
    if n_cols == 0 {
        return Some(PartialAggSpec {
            per_column: Vec::new(),
        });
    }
    let base = start_idx + 1;
    if !fields.contains_range(base, n_cols * 4) {
        return None;
    }
    let mut per_column = Vec::with_capacity(n_cols);
    for k in 0..n_cols {
        let mut column = fields.cursor_at(base + k * 4);
        let op_raw = column.read_int();
        let Some(op) = AggOp::from_i32(op_raw) else {
            pgrx::error!("pg_accel: invalid partial aggregate op tag {op_raw}");
        };
        let attno = column.read_int();
        let transtype_oid = column.read_oid();
        let ser_raw = column.read_u32();
        let serialize_fn_oid = if ser_raw == 0 {
            None
        } else {
            Some(pg_sys::Oid::from(ser_raw))
        };
        per_column.push(PartialColumn {
            op,
            attno,
            transtype_oid,
            serialize_fn_oid,
        });
    }
    Some(PartialAggSpec { per_column })
}

// ---------------------------------------------------------------------------
// PreAgg serialization / deserialization
// ---------------------------------------------------------------------------

/// Deserialized PreAgg configuration from `custom_private`.
#[allow(dead_code)]
pub(in crate::engine::ffi) struct PreAggPrivData {
    pub(super) scan_relid: pg_sys::Index,
    /// Stable relation OID for the fact table. Use this (via `table_open`)
    /// at execution time rather than `scan_relid`, because the planner's
    /// `set_plan_refs` pass may rewrite the range-table indices for upper
    /// plans (scanrelid=0 spanning a join).
    pub(super) scan_oid: pg_sys::Oid,
    pub(super) depths: Vec<JoinDepthDesc>,
    pub(super) agg_descs: Vec<PreAggColDesc>,
    pub(super) group_keys: Vec<GroupKeyDesc>,
    pub(super) scan_expr: Option<crate::engine::expr_compiler::CompiledExpr>,
    /// Partial-aggregate spec (worker-side of a Gather plan). `Some` means
    /// the executor emits transition-state tuples instead of final aggregate
    /// Datums. Mirrors the same field on `CustomPrivateData` for the Agg
    /// strategy. Round-tripped via the existing PARTIAL_SENTINEL block
    /// (`append_partial_spec` / `deserialize_partial_spec`) appended to the
    /// PreAgg layout.
    pub(super) partial: Option<PartialAggSpec>,
    /// **B5a flag.** `true` when the planner injected this PreAgg path with
    /// the fact relation's path attached as `custom_paths[0]` (and the
    /// `CustomPath.path.parallel_safe` bit set). Slot 0 of the deserialized
    /// `(*node).custom_ps` list is then a fact-side PlanState rather than a
    /// dimension; `materialize_dimensions` must skip it to keep the depth
    /// indices aligned with `depths[]`. Round-tripped via the
    /// [`PREAGG_PARALLEL_ATTACHED_SENTINEL`] block.
    pub(super) parallel_safe_planner_attached: bool,
}

/// Serialize PreAgg metadata into a PG `List` of `Integer` nodes.
///
/// Layout:
/// ```text
/// [STRATEGY=5, batch_size, expected_threads,
///  scan_relid, scan_oid, n_depths,
///  // Per depth:
///  outer_attno, inner_attno, key_type, n_dim_filters,
///  // Per dim filter: col_idx, cmp_opcode, const_val_hi, const_val_lo
///  // Per depth group cols: n_group_col_attnos, attno1, attno2, ...
///  n_agg_ops,
///  // Per agg: op_type, attno, type_oid
///  n_group_keys,
///  // Per group key: source, attno, type_oid
///  has_scan_expr, (if 1: template_type, ...template_data...),
///  // Required parallel-attached sentinel block (planner-side wiring):
///  PREAGG_PARALLEL_ATTACHED_SENTINEL, attached_flag
///  // Optional partial-agg sentinel block (worker-side parallel preagg):
///  (PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols)?
/// ]
/// ```
///
/// `partial` carries the `PartialAggSpec` for parallel partial-emit paths
/// (workers emit transition-state tuples for PG's Finalize Agg). `None`
/// means the current plan emits final aggregate Datums.
///
/// `parallel_safe_planner_attached` must be `true`: current PreAgg plans
/// attach the fact path as `custom_paths[0]` and require the slot-based
/// executor path.
///
/// # Safety
///
/// Must be called during planning on the main backend thread.
#[allow(
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
#[must_use]
pub unsafe fn serialize_preagg_private(
    scan_relid: pg_sys::Index,
    scan_oid: pg_sys::Oid,
    depths: &[JoinDepthDesc],
    agg_descs: &[PreAggColDesc],
    group_keys: &[GroupKeyDesc],
    scan_expr: Option<&crate::engine::expr_compiler::CompiledExpr>,
    partial: Option<&PartialAggSpec>,
    parallel_safe_planner_attached: bool,
) -> *mut pg_sys::List {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    if !parallel_safe_planner_attached {
        pgrx::error!("pg_accel: refusing to serialize PreAgg without attached fact path");
    }

    let batch_size = gucs::min_batch_size();
    let expected_threads = super::explain::resolve_thread_count();

    let mut writer = PgListWriter::new();
    writer.push_int(GpuStrategy::PreAgg as c_int);
    writer.push_int(batch_size);
    writer.push_int(expected_threads);
    writer.push_int(scan_relid as c_int);
    writer.push_oid(scan_oid);
    writer.push_len(depths.len());

    // Per depth.
    for depth in depths {
        writer.push_int(depth.outer_attno);
        writer.push_int(depth.inner_attno);
        writer.push_int(depth.key_type);
        writer.push_len(depth.dim_filters.len());
        for filt in &depth.dim_filters {
            writer.push_int(filt.col_idx as c_int);
            writer.push_int(filt.cmp_opcode as c_int);
            writer.push_f64_halves(filt.const_val);
        }
        writer.push_len(depth.group_col_attnos.len());
        for &attno in &depth.group_col_attnos {
            writer.push_int(attno);
        }
    }

    // Aggregates.
    writer.push_len(agg_descs.len());
    for desc in agg_descs {
        writer.push_int(desc.op.to_i32());
        writer.push_int(desc.attno);
        writer.push_oid(desc.type_oid);
    }

    // GROUP BY keys.
    writer.push_len(group_keys.len());
    for gk in group_keys {
        writer.push_u32(gk.source);
        writer.push_int(gk.attno);
        writer.push_oid(gk.type_oid);
    }

    // Serialize fact-side scan expression.
    match scan_expr {
        Some(CompiledExpr::Template(TemplateKernel::CmpConst {
            col_idx,
            cmp_opcode,
            const_val,
        })) => {
            writer.push_bool(true);
            writer.push_int(1);
            writer.push_u32(*col_idx);
            writer.push_int(*cmp_opcode as c_int);
            writer.push_f64_halves(*const_val);
        }
        Some(CompiledExpr::Template(TemplateKernel::Between { col_idx, lo, hi })) => {
            writer.push_bool(true);
            writer.push_int(2);
            writer.push_u32(*col_idx);
            writer.push_f64_halves(*lo);
            writer.push_f64_halves(*hi);
        }
        Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
            col1_idx,
            cmp1_opcode,
            const1_val,
            col2_idx,
            cmp2_opcode,
            const2_val,
        })) => {
            writer.push_bool(true);
            writer.push_int(3);
            writer.push_u32(*col1_idx);
            writer.push_int(*cmp1_opcode as c_int);
            writer.push_f64_halves(*const1_val);
            writer.push_u32(*col2_idx);
            writer.push_int(*cmp2_opcode as c_int);
            writer.push_f64_halves(*const2_val);
        }
        _ => {
            writer.push_bool(false);
        }
    }

    writer.push_int(PREAGG_PARALLEL_ATTACHED_SENTINEL);
    writer.push_bool(true);

    // Optional partial-agg sentinel block. Mirrors the Agg-strategy
    // layout (append_partial_spec writes
    // `[PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols]`)
    // so deserialize_partial_spec can read it back. Absent when the
    // plan is non-parallel (the serial preagg path).
    let mut list = writer.into_list();
    if let Some(spec) = partial {
        // SAFETY: this function is called while planning in a valid PG memory context.
        list = unsafe { append_partial_spec(list, spec) };
    }
    // SAFETY: this function is called while planning in a valid PG memory context.
    unsafe { append_host_staged_resident_proof(list, GpuStrategy::PreAgg) }
}

/// Deserialize PreAgg configuration from `custom_private`.
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` of Integer nodes.
#[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
pub(in crate::engine::ffi) unsafe fn deserialize_preagg_private(
    custom_private: *mut pg_sys::List,
) -> PreAggPrivData {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    if custom_private.is_null() {
        pgrx::error!("pg_accel: missing PreAgg private data");
    }

    // SAFETY: custom_private is a valid List.
    let fields = unsafe { IntListReader::from_pg_list(custom_private) };
    let mut cursor = fields.cursor_at(3); // skip [strategy, batch_size, expected_threads]

    let scan_relid = cursor.read_int() as pg_sys::Index;
    let scan_oid = cursor.read_oid();
    let n_depths = cursor.read_usize();

    let mut depths = Vec::with_capacity(n_depths);
    for _ in 0..n_depths {
        let outer_attno = cursor.read_int();
        let inner_attno = cursor.read_int();
        let key_type = cursor.read_int();
        let n_filters = cursor.read_usize();

        let mut dim_filters = Vec::with_capacity(n_filters);
        for _ in 0..n_filters {
            dim_filters.push(DimFilter {
                col_idx: cursor.read_usize(),
                cmp_opcode: cursor.read_int() as u16,
                const_val: cursor.read_f64_halves(),
            });
        }

        let n_group_cols = cursor.read_usize();
        let mut group_col_attnos = Vec::with_capacity(n_group_cols);
        for _ in 0..n_group_cols {
            group_col_attnos.push(cursor.read_int());
        }

        depths.push(JoinDepthDesc {
            outer_attno,
            inner_attno,
            key_type,
            dim_filters,
            group_col_attnos,
        });
    }

    // Aggregates.
    let n_aggs = cursor.read_usize();
    let mut agg_descs = Vec::with_capacity(n_aggs);
    for _ in 0..n_aggs {
        let op_raw = cursor.read_int();
        let Some(op) = AggOp::from_i32(op_raw) else {
            pgrx::error!("pg_accel: invalid PreAgg op tag {op_raw}");
        };
        agg_descs.push(PreAggColDesc {
            op,
            attno: cursor.read_int(),
            type_oid: cursor.read_oid(),
        });
    }

    // GROUP BY keys.
    let n_gkeys = cursor.read_usize();
    let mut group_keys = Vec::with_capacity(n_gkeys);
    for _ in 0..n_gkeys {
        group_keys.push(GroupKeyDesc {
            source: cursor.read_u32(),
            attno: cursor.read_int(),
            type_oid: cursor.read_oid(),
        });
    }

    // Deserialize scan_expr.
    let scan_expr = if cursor.read_bool() {
        let template_type = cursor.read_int();
        match template_type {
            1 => {
                // CmpConst
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    col_idx: cursor.read_u32(),
                    cmp_opcode: cursor.read_int() as u16,
                    const_val: cursor.read_f64_halves(),
                }))
            }
            2 => {
                // Between
                Some(CompiledExpr::Template(TemplateKernel::Between {
                    col_idx: cursor.read_u32(),
                    lo: cursor.read_f64_halves(),
                    hi: cursor.read_f64_halves(),
                }))
            }
            3 => {
                // TwoPredAnd
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx: cursor.read_u32(),
                    cmp1_opcode: cursor.read_int() as u16,
                    const1_val: cursor.read_f64_halves(),
                    col2_idx: cursor.read_u32(),
                    cmp2_opcode: cursor.read_int() as u16,
                    const2_val: cursor.read_f64_halves(),
                }))
            }
            _ => None,
        }
    } else {
        None
    };

    // Required PREAGG_PARALLEL_ATTACHED block, optionally followed by PARTIAL.
    let mut trailer_idx = cursor.position();
    if !fields.contains_range(trailer_idx, 2)
        || fields.int_at(trailer_idx) != PREAGG_PARALLEL_ATTACHED_SENTINEL
    {
        pgrx::error!("pg_accel: PreAgg private data missing attached fact child marker");
    }
    let parallel_safe_planner_attached = fields.int_at(trailer_idx + 1) != 0;
    if !parallel_safe_planner_attached {
        pgrx::error!("pg_accel: PreAgg private data requested unattached fact path");
    }
    trailer_idx += 2;

    // Optional PARTIAL_SENTINEL block — present only when the planner
    // injected a parallel partial-emit path (preagg_partial::try_inject).
    // Mirrors the Agg-strategy decode at deserialize_custom_private:344-356.
    let partial = if fields.int_at(trailer_idx) == PARTIAL_SENTINEL {
        deserialize_partial_spec_from_reader(&fields, trailer_idx + 1)
    } else {
        None
    };

    PreAggPrivData {
        scan_relid,
        scan_oid,
        depths,
        agg_descs,
        group_keys,
        scan_expr,
        partial,
        parallel_safe_planner_attached,
    }
}

// ---------------------------------------------------------------------------
// FunctionScan serialization / deserialization (Phase 2 F3)
// ---------------------------------------------------------------------------

/// Magic marker preceding a serialized [`FunctionScanPrivData`].
///
/// Distinct from `PARTIAL_SENTINEL` so the two block formats cannot be
/// silently confused if a layout regression mis-positions the cursor.
pub const FUNCTIONSCAN_SENTINEL: c_int = 0x4653_4341; // b"FSCA"

/// Plan metadata for a `FunctionScan` Custom-Scan injection (Phase 2 F3).
///
/// Carries the registered function OID and the constant arguments captured
/// from the FunctionScan's `RTE_FUNCTION` `funcexpr`. The args are stored
/// as serializable triples — pgrx Datum values that fit into a `c_int` —
/// so that the metadata can survive the planner's `List *` round-trip
/// alongside other strategies' private data.
///
/// **Note (Phase 2 F3 status):** the planner-side hook
/// (`projectset.rs::pgaccel_set_function_pathlist`) and the executor-side
/// `begin_custom_scan` arm that consume this struct are escalated per
/// anti-cheat ban #9; the type + (de)serializers are landed here so the
/// follow-up wiring agent can plug in without re-touching the
/// custom_private layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionScanPrivData {
    /// OID of the registered SRF / record-returning function.
    pub fn_oid: pg_sys::Oid,
    /// `OutputShape` discriminant: 0 = Scalar, 1 = Record, 2 = VarLen.
    /// (Mirrors `OutputShape::from_i32` semantics; serialized as a single
    /// `c_int` so the variant carries through `List *` round-trip.)
    pub output_shape_disc: i32,
    /// `field_count` payload for `Record { field_count }`. Zero for the
    /// Scalar / VarLen variants.
    pub output_shape_field_count: u32,
    /// Captured constant arguments to the FunctionScan's `funcexpr`, in
    /// positional order. Each entry is `(datum_as_i64, type_oid_as_u32)`.
    /// Datum is stored as `i64` (PG `usize` on 64-bit) so it fits into
    /// two `c_int` slots in the List layout.
    pub args: Vec<(i64, u32)>,
}

/// Append a [`FunctionScanPrivData`] onto `list` using the
/// `FUNCTIONSCAN_SENTINEL`-prefixed layout consumed by
/// [`deserialize_functionscan_priv`].
///
/// Layout (after the standard 6-element `[strategy, batch_size,
/// threads, fn_oid, target_attno, accel_strategy]` prefix that all
/// scan-strategy plans share):
///
/// ```text
/// [FUNCTIONSCAN_SENTINEL,
///  fn_oid_low_bits, output_shape_disc, output_shape_field_count,
///  n_args,
///  per arg: (datum_hi, datum_lo, type_oid)]
/// ```
///
/// The Datum value is split across two `c_int` slots (high 32 + low 32
/// bits) because PG's `Integer` node holds a single 32-bit signed int.
///
/// # Safety
///
/// Must be called in a valid PG memory context on the main backend
/// thread. `list` may be null or a valid PG `List *`.
#[allow(clippy::cast_possible_wrap)]
pub unsafe fn append_functionscan_priv(
    list: *mut pg_sys::List,
    priv_data: &FunctionScanPrivData,
) -> *mut pg_sys::List {
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(FUNCTIONSCAN_SENTINEL);
    writer.push_oid(priv_data.fn_oid);
    writer.push_int(priv_data.output_shape_disc);
    writer.push_u32(priv_data.output_shape_field_count);
    writer.push_len(priv_data.args.len());
    for &(datum, type_oid) in &priv_data.args {
        writer.push_i64_halves(datum);
        writer.push_u32(type_oid);
    }
    writer.into_list()
}

/// Deserialize a [`FunctionScanPrivData`] from `list` starting at
/// `start_idx` (the position of the `FUNCTIONSCAN_SENTINEL` marker).
///
/// Returns `None` if the sentinel does not match or the list is too
/// short to hold the declared `n_args` payload.
///
/// # Safety
///
/// `list` must be a valid PG `List *` of `Integer` nodes.
#[allow(clippy::cast_sign_loss)]
pub unsafe fn deserialize_functionscan_priv(
    list: *mut pg_sys::List,
    start_idx: usize,
) -> Option<FunctionScanPrivData> {
    if list.is_null() {
        return None;
    }
    // SAFETY: caller guarantees a valid PG List.
    let fields = unsafe { IntListReader::from_pg_list(list) };
    if !fields.contains_range(start_idx, 5) {
        return None;
    }
    let mut fixed = fields.cursor_at(start_idx);
    if fixed.read_int() != FUNCTIONSCAN_SENTINEL {
        return None;
    }
    let fn_oid = fixed.read_oid();
    let output_shape_disc = fixed.read_int();
    let output_shape_field_count = fixed.read_u32();
    let n_args = fixed.read_usize();
    let payload_base = fixed.position();
    if !fields.contains_range(payload_base, n_args * 3) {
        return None;
    }
    let mut args = Vec::with_capacity(n_args);
    let mut arg = fields.cursor_at(payload_base);
    for _ in 0..n_args {
        args.push((arg.read_i64_halves(), arg.read_u32()));
    }
    Some(FunctionScanPrivData {
        fn_oid,
        output_shape_disc,
        output_shape_field_count,
        args,
    })
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod functionscan_tests {
    use super::*;

    /// Sentinel must be distinct from `PARTIAL_SENTINEL` so the two
    /// optional-trailer blocks cannot be silently confused if a layout
    /// regression mis-positions the cursor.
    #[test]
    fn functionscan_sentinel_distinct_from_partial() {
        assert_ne!(FUNCTIONSCAN_SENTINEL, PARTIAL_SENTINEL);
    }

    #[pgrx::pg_schema]
    mod tests {
        use pgrx::pg_test;

        use super::*;

        /// Round-trip assertion via the in-memory layout: build a small list
        /// with `append_functionscan_priv`, then read it back with
        /// `deserialize_functionscan_priv` from the same offset. Uses a
        /// pgrx-managed memory context so PG `lappend` allocates safely.
        #[pg_test]
        fn functionscan_priv_roundtrip() {
            use pgrx::pg_sys;
            let original = FunctionScanPrivData {
                fn_oid: pg_sys::Oid::from(12345_u32),
                output_shape_disc: 1, // Record
                output_shape_field_count: 6,
                args: vec![
                    (0xDEAD_BEEF_DEAD_BEEFu64 as i64, pg_sys::INT8OID.to_u32()),
                    (42i64, pg_sys::INT4OID.to_u32()),
                ],
            };
            // SAFETY: pg_test runs in a real backend, so CurrentMemoryContext is
            // valid and lappend / makeInteger are safe.
            let mut list: *mut pg_sys::List = std::ptr::null_mut();
            unsafe {
                list = append_functionscan_priv(list, &original);
            }
            let decoded = unsafe { deserialize_functionscan_priv(list, 0) }
                .expect("functionscan priv must round-trip");
            assert_eq!(decoded, original);
        }
    }
}

// ---------------------------------------------------------------------------
// SrfTargetList serialization / deserialization (Phase 2 follow-up to F3)
// ---------------------------------------------------------------------------

/// Magic marker preceding a serialized [`SrfTargetListPrivData`] block.
///
/// Distinct from `FUNCTIONSCAN_SENTINEL` and `PARTIAL_SENTINEL` so the three
/// block formats cannot be silently confused if a layout regression
/// mis-positions the cursor.
pub const SRF_TARGET_LIST_SENTINEL: c_int = 0x5354_4C53; // b"STLS"

/// Plan metadata for an SRF-in-target-list Custom-Scan injection.
///
/// Captures the data needed to expand a `SELECT srf(col), passthrough_cols
/// FROM t` ProjectSet at execution time:
///
/// - `fn_oid`: registered SRF (`h3_grid_disk`, `h3_cell_to_boundary`, etc.)
/// - `output_shape_disc` / `output_shape_field_count`: same encoding as
///   `FunctionScanPrivData` so the executor can pick the right
///   `DispatchResult` arm.
/// - `srf_arg_attno`: 1-based attno of the per-row input column (Var) in
///   the child plan's targetlist. The executor reads this column from each
///   slot returned by `ExecProcNode(child)` and feeds it as `batch[0]` to
///   the dispatcher.
/// - `srf_tlist_pos`: 0-based position of the SRF result column in the
///   output tuple (the upper tlist this Custom Scan replaces).
/// - `passthrough_attnos`: for each non-SRF column in the output tlist,
///   the 1-based child attno to copy from per output row. Aligned with
///   `passthrough_tlist_positions` so position `k` in the output tuple
///   gets `passthrough_attnos[k]` from the child slot. The SRF position
///   itself is encoded as attno `0` (skipped during passthrough).
/// - `qual_args`: the constant args to the SRF (`k=1` in
///   `h3_grid_disk(cell, 1)`). Datum + type OID pairs, same encoding as
///   `FunctionScanPrivData::args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrfTargetListPrivData {
    /// OID of the registered SRF function.
    pub fn_oid: pg_sys::Oid,
    /// `OutputShape` discriminant: 0 = Scalar, 1 = Record, 2 = VarLen.
    pub output_shape_disc: i32,
    /// `field_count` for the Record variant; 0 otherwise.
    pub output_shape_field_count: u32,
    /// 1-based attno of the SRF's Var argument in the child plan's
    /// targetlist (which child column to feed per row).
    pub srf_arg_attno: i32,
    /// 0-based position of the SRF column in the output tuple.
    pub srf_tlist_pos: i32,
    /// Per output-tuple-position: 1-based child attno to passthrough,
    /// or 0 if that position is the SRF column. Length == n_output_cols.
    pub passthrough_attnos: Vec<i32>,
    /// Constant args to the SRF, in positional order. Each entry is
    /// `(datum_as_i64, type_oid_as_u32)` — same shape as
    /// `FunctionScanPrivData::args`.
    pub qual_args: Vec<(i64, u32)>,
}

/// Append an [`SrfTargetListPrivData`] onto `list` using the
/// `SRF_TARGET_LIST_SENTINEL`-prefixed layout.
///
/// Layout (after the standard 6-element header):
///
/// ```text
/// [SRF_TARGET_LIST_SENTINEL,
///  fn_oid, output_shape_disc, output_shape_field_count,
///  srf_arg_attno, srf_tlist_pos,
///  n_passthrough, passthrough_attno_0, ..., passthrough_attno_{n-1},
///  n_qual_args,
///  per qual arg: (datum_hi, datum_lo, type_oid)]
/// ```
///
/// # Safety
///
/// Must be called in a valid PG memory context on the main backend thread.
#[allow(clippy::cast_possible_wrap)]
pub unsafe fn append_srf_target_list_priv(
    list: *mut pg_sys::List,
    priv_data: &SrfTargetListPrivData,
) -> *mut pg_sys::List {
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(SRF_TARGET_LIST_SENTINEL);
    writer.push_oid(priv_data.fn_oid);
    writer.push_int(priv_data.output_shape_disc);
    writer.push_u32(priv_data.output_shape_field_count);
    writer.push_int(priv_data.srf_arg_attno);
    writer.push_int(priv_data.srf_tlist_pos);
    writer.push_len(priv_data.passthrough_attnos.len());
    for &attno in &priv_data.passthrough_attnos {
        writer.push_int(attno);
    }
    writer.push_len(priv_data.qual_args.len());
    for &(datum, type_oid) in &priv_data.qual_args {
        writer.push_i64_halves(datum);
        writer.push_u32(type_oid);
    }
    writer.into_list()
}

/// Deserialize an [`SrfTargetListPrivData`] from `list` starting at
/// `start_idx` (the position of the `SRF_TARGET_LIST_SENTINEL`).
///
/// Returns `None` if the sentinel does not match or the list is too
/// short to hold the declared payload.
///
/// # Safety
///
/// `list` must be a valid PG `List *` of `Integer` nodes.
#[allow(clippy::cast_sign_loss)]
pub unsafe fn deserialize_srf_target_list_priv(
    list: *mut pg_sys::List,
    start_idx: usize,
) -> Option<SrfTargetListPrivData> {
    if list.is_null() {
        return None;
    }
    // Need at least sentinel + 6 fixed fields + n_passthrough(0) + n_qual_args(0)
    // SAFETY: caller guarantees a valid PG List.
    let fields = unsafe { IntListReader::from_pg_list(list) };
    if !fields.contains_range(start_idx, 7) {
        return None;
    }
    let mut fixed = fields.cursor_at(start_idx);
    if fixed.read_int() != SRF_TARGET_LIST_SENTINEL {
        return None;
    }
    let fn_oid = fixed.read_oid();
    let output_shape_disc = fixed.read_int();
    let output_shape_field_count = fixed.read_u32();
    let srf_arg_attno = fixed.read_int();
    let srf_tlist_pos = fixed.read_int();
    let n_passthrough = fixed.read_usize();
    let pass_base = fixed.position();
    if !fields.contains_range(pass_base, n_passthrough + 1) {
        return None;
    }
    let mut passthrough_attnos = Vec::with_capacity(n_passthrough);
    let mut passthrough = fields.cursor_at(pass_base);
    for _ in 0..n_passthrough {
        passthrough_attnos.push(passthrough.read_int());
    }
    let n_qual_args = passthrough.read_usize();
    let qual_base = passthrough.position();
    if !fields.contains_range(qual_base, n_qual_args * 3) {
        return None;
    }
    let mut qual_args = Vec::with_capacity(n_qual_args);
    let mut qual_arg = fields.cursor_at(qual_base);
    for _ in 0..n_qual_args {
        qual_args.push((qual_arg.read_i64_halves(), qual_arg.read_u32()));
    }
    Some(SrfTargetListPrivData {
        fn_oid,
        output_shape_disc,
        output_shape_field_count,
        srf_arg_attno,
        srf_tlist_pos,
        passthrough_attnos,
        qual_args,
    })
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod srf_target_list_tests {
    use super::*;

    /// Sentinel must be distinct from `FUNCTIONSCAN_SENTINEL` and
    /// `PARTIAL_SENTINEL` so the three block formats are unambiguous.
    #[test]
    fn srf_target_list_sentinel_distinct() {
        assert_ne!(SRF_TARGET_LIST_SENTINEL, FUNCTIONSCAN_SENTINEL);
        assert_ne!(SRF_TARGET_LIST_SENTINEL, PARTIAL_SENTINEL);
    }

    #[pgrx::pg_schema]
    mod tests {
        use pgrx::pg_test;

        use super::*;

        /// Round-trip: build a serialized priv block, then deserialize and
        /// confirm equality field-by-field.
        #[pg_test]
        fn srf_target_list_priv_roundtrip() {
            use pgrx::pg_sys;
            let original = SrfTargetListPrivData {
                fn_oid: pg_sys::Oid::from(98765_u32),
                output_shape_disc: 2, // VarLen
                output_shape_field_count: 0,
                srf_arg_attno: 2,
                srf_tlist_pos: 1,
                passthrough_attnos: vec![1, 0], // pos 0 = passthrough child attno 1; pos 1 = SRF
                qual_args: vec![(7i64, pg_sys::INT4OID.to_u32())],
            };
            // SAFETY: pg_test runs in a real backend, so CurrentMemoryContext
            // is valid for lappend / makeInteger.
            let mut list: *mut pg_sys::List = std::ptr::null_mut();
            unsafe {
                list = append_srf_target_list_priv(list, &original);
            }
            let decoded = unsafe { deserialize_srf_target_list_priv(list, 0) }
                .expect("srf_target_list priv must round-trip");
            assert_eq!(decoded, original);
        }
    }
}

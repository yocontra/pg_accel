//! `custom_private` serialization / deserialization.
//!
//! Plan metadata travels through PG as a `List *` of `Integer` nodes so it
//! survives plan copying and EXPLAIN output. Field order is load-bearing.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::{GpuStrategy, OutputShapeDisc};
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
use crate::engine::spec::{
    AGG_OUTPUT_PROJECTION_MAX_WORDS, AGG_QUERY_SPEC_MAX_WORDS, AggOutputProjection, AggQuerySpec,
    ProjectionCodecError, SpecCodecError,
};
use crate::gpu::PgaccelKeyType;

mod list_codec;

use list_codec::{IntListReader, PgListWriter};

const PATH_PRIVATE_HEADER_INTS: usize = 3;
const PLAN_PRIVATE_HEADER_INTS: usize = 6;
const PLAN_PAYLOAD_START: usize = PLAN_PRIVATE_HEADER_INTS;
pub(super) const PLAN_WIRE_MAGIC: c_int = 0x5043_5732; // b"PCW2"
pub(super) const PLAN_WIRE_VERSION: c_int = 2;
pub(super) const PLAN_WIRE_FOOTER_INTS: usize = 4;
pub(in crate::engine::ffi) const AGG_QUERY_SPEC_SENTINEL: c_int = 0x4151_5332; // b"AQS2"
pub(in crate::engine::ffi) const AGG_OUTPUT_PROJECTION_SENTINEL: c_int = 0x414F_5032; // b"AOP2"
const MAX_LEGACY_AGGREGATES: usize = 64;
const MAX_WINDOW_SPECS: usize = 64;
const MAX_FUNCTION_ARGS: usize = 100;
const MAX_TUPLE_COLUMNS: usize = 1_664;
pub(super) const MAX_PLAN_BATCH_SIZE: c_int = 16_777_216;
const MAX_EXPECTED_THREADS: c_int = 4_096;
const MAX_PLAN_WIRE_INTS: usize = AGG_QUERY_SPEC_MAX_WORDS + 32_768;

/// Strategy-specific PostgreSQL `CustomExecMethods` identity serialized in
/// every v2 plan-private frame.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanExecMethod {
    Scan = 1,
    Join = 2,
    Agg = 3,
    Window = 4,
    FunctionScan = 5,
    SrfTargetList = 6,
}

impl PlanExecMethod {
    pub(super) const fn from_i32(raw: c_int) -> Option<Self> {
        match raw {
            1 => Some(Self::Scan),
            2 => Some(Self::Join),
            3 => Some(Self::Agg),
            4 => Some(Self::Window),
            5 => Some(Self::FunctionScan),
            6 => Some(Self::SrfTargetList),
            _ => None,
        }
    }

    pub(super) const fn for_strategy(strategy: GpuStrategy) -> Option<Self> {
        match strategy {
            GpuStrategy::Scan => Some(Self::Scan),
            GpuStrategy::Join => Some(Self::Join),
            GpuStrategy::Agg => Some(Self::Agg),
            GpuStrategy::Window => Some(Self::Window),
            GpuStrategy::FunctionScan => Some(Self::FunctionScan),
            GpuStrategy::SrfTargetList => Some(Self::SrfTargetList),
            GpuStrategy::Sort | GpuStrategy::PreAgg => None,
        }
    }
}

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
    InvalidExpectedThreads {
        raw: c_int,
    },
    UnsupportedGpuStrategy {
        strategy: GpuStrategy,
    },
    InvalidExecutionMethod {
        raw: c_int,
    },
    ExecutionMethodMismatch {
        expected: PlanExecMethod,
        actual: PlanExecMethod,
    },
    StrategyMethodMismatch {
        strategy: GpuStrategy,
        method: PlanExecMethod,
    },
    StrategyAccelMismatch {
        strategy: GpuStrategy,
        accel: AccelStrategy,
    },
    InvalidValue {
        index: usize,
        field: &'static str,
        raw: c_int,
    },
    LimitExceeded {
        index: usize,
        field: &'static str,
        declared: usize,
        maximum: usize,
    },
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    UnsupportedVersion {
        version: c_int,
    },
    Truncated {
        index: usize,
        field: &'static str,
    },
    TrailingPayload {
        index: usize,
        payload_end: usize,
    },
    InvalidResidentProof {
        field: &'static str,
    },
    InvalidAggQuerySpec(SpecCodecError),
    InvalidAggOutputProjection(ProjectionCodecError),
    AllocationFailed {
        field: &'static str,
    },
    ProjectionTargetMismatch {
        index: usize,
        field: &'static str,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

impl From<SpecCodecError> for DecodeError {
    fn from(value: SpecCodecError) -> Self {
        Self::InvalidAggQuerySpec(value)
    }
}

impl From<ProjectionCodecError> for DecodeError {
    fn from(value: ProjectionCodecError) -> Self {
        Self::InvalidAggOutputProjection(value)
    }
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
    if (1..=MAX_PLAN_BATCH_SIZE).contains(&raw) {
        Ok(raw)
    } else {
        Err(DecodeError::InvalidBatchSize { raw })
    }
}

#[inline]
fn decode_expected_threads(raw: c_int) -> Result<c_int, DecodeError> {
    if (1..=MAX_EXPECTED_THREADS).contains(&raw) {
        Ok(raw)
    } else {
        Err(DecodeError::InvalidExpectedThreads { raw })
    }
}

fn validate_strategy_accel(strategy: GpuStrategy, accel: AccelStrategy) -> Result<(), DecodeError> {
    let valid = match strategy {
        GpuStrategy::Scan => matches!(
            accel,
            AccelStrategy::GpuSpatial
                | AccelStrategy::GpuRaster
                | AccelStrategy::GpuH3
                | AccelStrategy::GpuReduce
                | AccelStrategy::GpuExpr
        ),
        GpuStrategy::Join => matches!(
            accel,
            AccelStrategy::GpuHashJoin | AccelStrategy::GpuNestedLoopIneq
        ),
        GpuStrategy::Agg => accel == AccelStrategy::GpuReduce,
        GpuStrategy::Window => accel == AccelStrategy::GpuWindow,
        GpuStrategy::FunctionScan | GpuStrategy::SrfTargetList => matches!(
            accel,
            AccelStrategy::GpuSpatial | AccelStrategy::GpuRaster | AccelStrategy::GpuH3
        ),
        GpuStrategy::Sort | GpuStrategy::PreAgg => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DecodeError::StrategyAccelMismatch { strategy, accel })
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

        let gpu_strategy = decode_gpu_strategy(gpu_strategy_raw)?;
        let accel_strategy = decode_accel_strategy(accel_strategy_raw)?;
        validate_strategy_accel(gpu_strategy, accel_strategy)?;
        if target_attno < 0 {
            return Err(DecodeError::InvalidValue {
                index: 4,
                field: "target attno",
                raw: target_attno,
            });
        }
        Ok(Self {
            gpu_strategy,
            batch_size: decode_batch_size(batch_size_raw)?,
            expected_threads: decode_expected_threads(expected_threads)?,
            fn_oid: pg_sys::Oid::from(fn_oid_raw),
            target_attno,
            accel_strategy,
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
    use crate::engine::residency::ResidentOperatorStage;
    use crate::engine::spec::{
        AggOutputProjection, AggOutputSlot, AggOutputSource, AggregateKind, AggregateOutput,
        AggregateSource, FilterSpec, MeasureExpr, MeasureSpec,
    };

    fn append_proof_words(words: &mut Vec<i32>, proof: ResidentProofSnapshot) {
        words.extend([
            RESIDENT_PROOF_SENTINEL,
            RESIDENT_PROOF_VERSION,
            proof.operator_class.to_i32(),
            proof.stage_mask as i32,
            proof.materialization_kind.to_i32(),
            proof.device_columns as i32,
            i32::from(proof.has_device_selection),
            i32::from(proof.has_device_projection),
            proof.cpu_boundary.to_i32(),
        ]);
    }

    fn frame(mut words: Vec<i32>, strategy: GpuStrategy, method: PlanExecMethod) -> Vec<i32> {
        append_proof_words(&mut words, resident_proof_default_for_strategy(strategy));
        let total = words.len() + PLAN_WIRE_FOOTER_INTS;
        words.extend([
            PLAN_WIRE_MAGIC,
            PLAN_WIRE_VERSION,
            total as i32,
            method as i32,
        ]);
        words
    }

    fn header(strategy: GpuStrategy, accel: AccelStrategy) -> Vec<i32> {
        vec![strategy as i32, 256, 1, 0, 0, accel as i32]
    }

    fn generic_agg_frame() -> Vec<i32> {
        let spec = AggQuerySpec {
            fact_rel: 42,
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
        };
        let projection = AggOutputProjection {
            slots: vec![AggOutputSlot {
                source: AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                },
                source_type_oid: 0,
                result_type_oid: pg_sys::INT8OID.to_u32(),
                result_typmod: -1,
                result_collation_oid: 0,
                nullable: false,
            }],
        };
        let mut words = header(GpuStrategy::Agg, AccelStrategy::GpuReduce);
        words.extend([0, 0, 0, AGG_QUERY_SPEC_SENTINEL]);
        words.extend(spec.encode_i32().expect("query spec encodes"));
        words.push(AGG_OUTPUT_PROJECTION_SENTINEL);
        words.extend(projection.encode_i32(&spec).expect("projection encodes"));
        frame(words, GpuStrategy::Agg, PlanExecMethod::Agg)
    }

    fn legacy_h3_agg_frame() -> Vec<i32> {
        let mut words = header(GpuStrategy::Agg, AccelStrategy::GpuReduce);
        words.extend([
            1,
            AggOp::Count.to_i32(),
            0,
            pg_sys::INT8OID.to_u32() as i32,
            0,
            42,
            AGG_OLAP_SENTINEL,
            OLAP_KIND_RESIDENT_H3_GROUPED_COUNT,
            42,
            ResidentH3GroupedCountKind::LatLngToCell.to_i32(),
            9,
        ]);
        frame(words, GpuStrategy::Agg, PlanExecMethod::Agg)
    }

    fn valid_plan_frames() -> Vec<(PlanExecMethod, Vec<i32>)> {
        let scan = frame(
            header(GpuStrategy::Scan, AccelStrategy::GpuSpatial),
            GpuStrategy::Scan,
            PlanExecMethod::Scan,
        );

        let mut join = header(GpuStrategy::Join, AccelStrategy::GpuHashJoin);
        join[4] = 1;
        join.extend([2, 0, 0, 0, 0, 0]);
        let join = frame(join, GpuStrategy::Join, PlanExecMethod::Join);

        let mut window = header(GpuStrategy::Window, AccelStrategy::GpuWindow);
        window.extend([1, 0, 1, 2, 0, 0, 0, pg_sys::INT8OID.to_u32() as i32, 0, 0]);
        let window = frame(window, GpuStrategy::Window, PlanExecMethod::Window);

        let mut function = header(GpuStrategy::FunctionScan, AccelStrategy::GpuH3);
        function.extend([FUNCTIONSCAN_SENTINEL, 42, 2, 0, 1, 0, 7, 23]);
        let function = frame(
            function,
            GpuStrategy::FunctionScan,
            PlanExecMethod::FunctionScan,
        );

        let mut srf = header(GpuStrategy::SrfTargetList, AccelStrategy::GpuH3);
        srf.extend([
            SRF_TARGET_LIST_SENTINEL,
            42,
            2,
            0,
            1,
            1,
            2,
            1,
            0,
            1,
            0,
            7,
            23,
        ]);
        let srf = frame(
            srf,
            GpuStrategy::SrfTargetList,
            PlanExecMethod::SrfTargetList,
        );

        vec![
            (PlanExecMethod::Scan, scan),
            (PlanExecMethod::Join, join),
            (PlanExecMethod::Agg, generic_agg_frame()),
            (PlanExecMethod::Window, window),
            (PlanExecMethod::FunctionScan, function),
            (PlanExecMethod::SrfTargetList, srf),
        ]
    }

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
    fn v2_plan_frames_validate_for_every_execution_method() {
        for (method, words) in valid_plan_frames() {
            validate_plan_wire_slice(&words, method)
                .unwrap_or_else(|error| panic!("{method:?} frame failed: {error}"));
        }
    }

    #[test]
    fn v2_frame_preserves_strict_legacy_h3_aggregate_compatibility() {
        let words = legacy_h3_agg_frame();
        validate_plan_wire_slice(&words, PlanExecMethod::Agg)
            .unwrap_or_else(|error| panic!("legacy H3 aggregate frame failed: {error}"));

        for end in 0..words.len() {
            assert!(
                validate_plan_wire_slice(&words[..end], PlanExecMethod::Agg).is_err(),
                "legacy H3 aggregate word prefix {end} unexpectedly decoded"
            );
        }
    }

    #[test]
    fn every_plan_frame_truncation_boundary_is_rejected() {
        for (method, words) in valid_plan_frames() {
            for end in 0..words.len() {
                assert!(
                    validate_plan_wire_slice(&words[..end], method).is_err(),
                    "{method:?} word prefix {end} unexpectedly decoded"
                );
            }
            let bytes = words
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect::<Vec<_>>();
            for end in 0..bytes.len() {
                let result = if end % 4 == 0 {
                    let prefix = bytes[..end]
                        .chunks_exact(4)
                        .map(|chunk| {
                            i32::from_le_bytes(chunk.try_into().expect("four-byte wire word"))
                        })
                        .collect::<Vec<_>>();
                    validate_plan_wire_slice(&prefix, method)
                } else {
                    Err(DecodeError::Truncated {
                        index: end / 4,
                        field: "partial plan-private word",
                    })
                };
                assert!(
                    result.is_err(),
                    "{method:?} byte prefix {end} unexpectedly decoded"
                );
            }
        }
    }

    #[test]
    fn execution_method_identity_mismatch_matrix_is_rejected() {
        let methods = [
            PlanExecMethod::Scan,
            PlanExecMethod::Join,
            PlanExecMethod::Agg,
            PlanExecMethod::Window,
            PlanExecMethod::FunctionScan,
            PlanExecMethod::SrfTargetList,
        ];
        for (actual, words) in valid_plan_frames() {
            for expected in methods {
                let result = validate_plan_wire_slice(&words, expected);
                assert_eq!(
                    result.is_ok(),
                    expected == actual,
                    "expected {expected:?}, serialized {actual:?}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn v2_plan_frames_reject_noncanonical_flags_counts_tags_and_trailing_payload() {
        let frames = valid_plan_frames();

        let mut bad_proof_bool = frames[0].1.clone();
        let proof_start =
            bad_proof_bool.len() - PLAN_WIRE_FOOTER_INTS - RESIDENT_PROOF_TRAILER_INTS;
        bad_proof_bool[proof_start + 6] = 2;
        assert!(validate_plan_wire_slice(&bad_proof_bool, PlanExecMethod::Scan).is_err());

        let mut negative_window_count = frames[3].1.clone();
        negative_window_count[PLAN_PAYLOAD_START] = -1;
        assert!(validate_plan_wire_slice(&negative_window_count, PlanExecMethod::Window).is_err());

        let mut bad_pair = frames[0].1.clone();
        bad_pair[5] = AccelStrategy::GpuHashJoin as i32;
        assert!(validate_plan_wire_slice(&bad_pair, PlanExecMethod::Scan).is_err());

        let mut trailing = frames[0].1.clone();
        let footer = trailing.len() - PLAN_WIRE_FOOTER_INTS;
        trailing.insert(footer - RESIDENT_PROOF_TRAILER_INTS, 99);
        let new_footer = trailing.len() - PLAN_WIRE_FOOTER_INTS;
        trailing[new_footer + 2] = trailing.len() as i32;
        assert!(validate_plan_wire_slice(&trailing, PlanExecMethod::Scan).is_err());
    }

    #[test]
    fn neutral_aggregate_rejects_all_legacy_execution_fields() {
        let mut self_scan = generic_agg_frame();
        self_scan[PLAN_PAYLOAD_START + 2] = 42;
        assert!(validate_plan_wire_slice(&self_scan, PlanExecMethod::Agg).is_err());

        let mut scan_expr = generic_agg_frame();
        scan_expr.splice(
            PLAN_PAYLOAD_START + 3..PLAN_PAYLOAD_START + 3,
            [
                AGG_SCAN_EXPR_SENTINEL,
                1,
                0,
                i32::from(crate::engine::expr_compiler::opcode::EQ),
                0,
                0,
            ],
        );
        let footer = scan_expr.len() - PLAN_WIRE_FOOTER_INTS;
        scan_expr[footer + 2] = scan_expr.len() as i32;
        assert!(validate_plan_wire_slice(&scan_expr, PlanExecMethod::Agg).is_err());

        let mut partial = generic_agg_frame();
        let proof = partial.len() - PLAN_WIRE_FOOTER_INTS - RESIDENT_PROOF_TRAILER_INTS;
        partial.splice(
            proof..proof,
            [
                PARTIAL_SENTINEL,
                1,
                AggOp::Count.to_i32(),
                1,
                pg_sys::INT8OID.to_u32() as i32,
                0,
            ],
        );
        let footer = partial.len() - PLAN_WIRE_FOOTER_INTS;
        partial[footer + 2] = partial.len() as i32;
        assert!(validate_plan_wire_slice(&partial, PlanExecMethod::Agg).is_err());
    }

    #[test]
    fn oversized_plan_frame_is_rejected_before_payload_decode() {
        let words = vec![0; MAX_PLAN_WIRE_INTS + 1];
        assert!(matches!(
            validate_plan_wire_slice(&words, PlanExecMethod::Scan),
            Err(DecodeError::LimitExceeded {
                field: "plan-private word count",
                ..
            })
        ));
    }

    #[test]
    fn deterministic_adversarial_plan_words_never_panic() {
        let mut state = 0x3C6E_F372_FE94_F82B_u64;
        for case in 0..4_096usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let len = state as usize % 257;
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                words.push((state >> 32) as i32);
            }
            if case % 4 == 0 && words.len() >= PLAN_WIRE_FOOTER_INTS {
                let footer = words.len() - PLAN_WIRE_FOOTER_INTS;
                words[footer] = PLAN_WIRE_MAGIC;
                words[footer + 1] = PLAN_WIRE_VERSION;
                words[footer + 2] = words.len() as i32;
                words[footer + 3] = PlanExecMethod::Scan as i32;
            }
            let outcome = std::panic::catch_unwind(|| {
                let _ = validate_plan_wire_slice(&words, PlanExecMethod::Scan);
            });
            assert!(outcome.is_ok(), "adversarial frame {case} panicked");
        }
    }

    #[test]
    fn plan_private_decodes_and_reencodes_header() {
        let raw = [
            GpuStrategy::Scan as c_int,
            2048,
            6,
            1234,
            5,
            AccelStrategy::GpuSpatial as c_int,
        ];

        let decoded = PlanPrivate::decode(&raw).expect("valid plan header should decode");

        assert_eq!(decoded.gpu_strategy, GpuStrategy::Scan);
        assert_eq!(decoded.batch_size, 2048);
        assert_eq!(decoded.expected_threads, 6);
        assert_eq!(u32::from(decoded.fn_oid), 1234);
        assert_eq!(decoded.target_attno, 5);
        assert_eq!(decoded.accel_strategy, AccelStrategy::GpuSpatial);
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
        for raw in [0, MAX_PLAN_BATCH_SIZE + 1] {
            let err = PlanPrivate::decode(&[
                GpuStrategy::Scan as c_int,
                raw,
                1,
                42,
                7,
                AccelStrategy::GpuH3 as c_int,
            ])
            .expect_err("out-of-range batch size should fail");

            assert_eq!(err, DecodeError::InvalidBatchSize { raw });
        }
    }

    #[test]
    fn plan_private_decode_rejects_nonpositive_thread_count_and_method_pair() {
        let mut raw = [
            GpuStrategy::Scan as c_int,
            256,
            0,
            42,
            7,
            AccelStrategy::GpuH3 as c_int,
        ];
        assert_eq!(
            PlanPrivate::decode(&raw),
            Err(DecodeError::InvalidExpectedThreads { raw: 0 })
        );
        raw[2] = MAX_EXPECTED_THREADS + 1;
        assert_eq!(
            PlanPrivate::decode(&raw),
            Err(DecodeError::InvalidExpectedThreads {
                raw: MAX_EXPECTED_THREADS + 1
            })
        );
        raw[2] = 1;
        raw[5] = AccelStrategy::GpuHashJoin as c_int;
        assert_eq!(
            PlanPrivate::decode(&raw),
            Err(DecodeError::StrategyAccelMismatch {
                strategy: GpuStrategy::Scan,
                accel: AccelStrategy::GpuHashJoin,
            })
        );
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
    fn resident_proof_snapshot_zero_stage_mask_is_rejected() {
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

        assert!(matches!(
            decode_resident_proof_at(&fields, raw.len() - RESIDENT_PROOF_TRAILER_INTS),
            Err(DecodeError::InvalidResidentProof { field: "semantics" })
        ));
    }

    #[test]
    fn resident_proof_snapshot_unspecified_operator_class_is_rejected() {
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

        assert!(matches!(
            decode_resident_proof_at(&fields, raw.len() - RESIDENT_PROOF_TRAILER_INTS),
            Err(DecodeError::InvalidResidentProof { field: "semantics" })
        ));
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
        // The trailer walker must still skip the full group-key block
        // (has_gk + 5 ints, tlist pos packed with the H3 resolution) to land
        // on self_scan_relid — that offset math is what keeps the OLAP
        // trailer reachable for the surviving resident agg decode.
        let _ = packed_tlist_pos;
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
    pub(super) expected_threads: c_int,
    pub(super) fn_oid: pg_sys::Oid,
    pub(super) target_attno: i32,
    pub(super) accel_strategy: AccelStrategy,
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
    /// Optional resident OLAP aggregate payload. Only meaningful for
    /// `gpu_strategy == Agg`; executor must not pull a child plan in this mode.
    pub(super) olap_agg: Option<OlapAggSpec>,
    /// Neutral grouped-aggregate query contract carried by the v2 wire.
    /// Residency resolves its relation/column references after decode.
    #[allow(dead_code)] // consumed by the descriptor executor landing in Phase 5D
    pub(super) agg_query_spec: Option<AggQuerySpec>,
    /// Ordered PostgreSQL result slots for the neutral aggregate contract.
    pub(super) agg_output_projection: Option<AggOutputProjection>,
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
        let relid = self.fields.int_at(relid_idx) as pg_sys::Index;
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
        let has_group_key = self.fields.int_at(group_key_base);
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
                pgrx::error!("pg_accel: truncated validated window private payload");
            }
            let mut spec = self.fields.cursor_at(offset);
            let func = WindowFunc::from_i32(spec.read_int()).unwrap_or_else(|| {
                pgrx::error!("pg_accel: invalid function in validated window private payload")
            });
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
        self.fields.int_at(relid_idx) as pg_sys::Index
    }

    #[must_use]
    fn hash_join_info(&self) -> (i32, i32, bool, bool, pg_sys::Oid, pg_sys::Oid) {
        (
            self.fields.int_at(Self::HASH_INNER_ATTNO),
            self.fields.int_at(Self::HASH_KEY_TYPE),
            self.fields.int_at(Self::HASH_COUNT_ONLY) == 1,
            self.fields.int_at(Self::HASH_RESIDENT_COUNT) == 1,
            pg_sys::Oid::from(self.fields.int_at(Self::HASH_OUTER_REL_OID) as u32),
            pg_sys::Oid::from(self.fields.int_at(Self::HASH_INNER_REL_OID) as u32),
        )
    }

    #[must_use]
    fn nlj_info(&self) -> (i32, i32, i32, i32, i32) {
        (
            self.fields.int_at(Self::NLJ_SHAPE),
            self.fields.int_at(Self::NLJ_KEY_TYPE),
            self.fields.int_at(Self::NLJ_OP),
            self.fields.int_at(Self::NLJ_INNER_LO_ATTNO),
            self.fields.int_at(Self::NLJ_INNER_HI_ATTNO),
        )
    }
}

#[derive(Debug)]
struct ValidatedPlanWire {
    plan_private: PlanPrivate,
    resident_proof: ResidentProofSnapshot,
    agg_query_spec: Option<AggQuerySpec>,
    agg_output_projection: Option<AggOutputProjection>,
}

fn strict_word(
    fields: &IntListReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<c_int, DecodeError> {
    fields
        .get(index)
        .ok_or(DecodeError::Truncated { index, field })
}

fn strict_bool(
    fields: &IntListReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<bool, DecodeError> {
    match strict_word(fields, index, field)? {
        0 => Ok(false),
        1 => Ok(true),
        raw => Err(DecodeError::InvalidValue { index, field, raw }),
    }
}

fn strict_count(
    fields: &IntListReader<'_>,
    index: usize,
    field: &'static str,
    maximum: usize,
    minimum_words_per_item: usize,
    payload_end: usize,
) -> Result<usize, DecodeError> {
    let raw = strict_word(fields, index, field)?;
    let count =
        usize::try_from(raw).map_err(|_| DecodeError::InvalidValue { index, field, raw })?;
    if count > maximum {
        return Err(DecodeError::LimitExceeded {
            index,
            field,
            declared: count,
            maximum,
        });
    }
    let needed = count
        .checked_mul(minimum_words_per_item)
        .ok_or(DecodeError::LimitExceeded {
            index,
            field,
            declared: count,
            maximum,
        })?;
    if index
        .checked_add(1)
        .and_then(|start| start.checked_add(needed))
        .is_none_or(|end| end > payload_end)
    {
        return Err(DecodeError::Truncated {
            index: payload_end,
            field,
        });
    }
    Ok(count)
}

fn require_payload_end(index: usize, payload_end: usize) -> Result<(), DecodeError> {
    if index == payload_end {
        Ok(())
    } else {
        Err(DecodeError::TrailingPayload { index, payload_end })
    }
}

fn payload_end_before_proof(fields: &IntListReader<'_>) -> usize {
    let frame_end = if fields.len() >= PLAN_WIRE_FOOTER_INTS
        && fields.get(fields.len() - PLAN_WIRE_FOOTER_INTS) == Some(PLAN_WIRE_MAGIC)
    {
        fields.len() - PLAN_WIRE_FOOTER_INTS
    } else {
        fields.len()
    };
    if frame_end >= RESIDENT_PROOF_TRAILER_INTS
        && fields.get(frame_end - RESIDENT_PROOF_TRAILER_INTS) == Some(RESIDENT_PROOF_SENTINEL)
    {
        frame_end - RESIDENT_PROOF_TRAILER_INTS
    } else {
        frame_end
    }
}

fn decode_resident_proof_at(
    fields: &IntListReader<'_>,
    index: usize,
) -> Result<ResidentProofSnapshot, DecodeError> {
    if !fields.contains_range(index, RESIDENT_PROOF_TRAILER_INTS) {
        return Err(DecodeError::Truncated {
            index,
            field: "resident proof",
        });
    }
    if strict_word(fields, index, "resident proof sentinel")? != RESIDENT_PROOF_SENTINEL {
        return Err(DecodeError::InvalidResidentProof { field: "sentinel" });
    }
    if strict_word(fields, index + 1, "resident proof version")? != RESIDENT_PROOF_VERSION {
        return Err(DecodeError::InvalidResidentProof { field: "version" });
    }
    let operator_class = ResidentOperatorClass::from_i32(strict_word(
        fields,
        index + 2,
        "resident proof operator class",
    )?)
    .ok_or(DecodeError::InvalidResidentProof {
        field: "operator class",
    })?;
    let stage_mask = strict_word(fields, index + 3, "resident proof stage mask")? as u32;
    let known_stage_mask = crate::engine::residency::ResidentOperatorStage::ALL
        .iter()
        .fold(0u32, |mask, stage| mask | stage.bit());
    if stage_mask & !known_stage_mask != 0 {
        return Err(DecodeError::InvalidResidentProof {
            field: "stage mask",
        });
    }
    let materialization_kind = ResidentMaterializationKind::from_i32(strict_word(
        fields,
        index + 4,
        "resident proof materialization",
    )?)
    .ok_or(DecodeError::InvalidResidentProof {
        field: "materialization",
    })?;
    let device_columns = strict_word(fields, index + 5, "resident proof device columns")? as u32;
    let has_device_selection = strict_bool(fields, index + 6, "resident proof selection")?;
    let has_device_projection = strict_bool(fields, index + 7, "resident proof projection")?;
    let cpu_boundary = CpuBoundaryReason::from_i32(strict_word(
        fields,
        index + 8,
        "resident proof CPU boundary",
    )?)
    .ok_or(DecodeError::InvalidResidentProof {
        field: "CPU boundary",
    })?;
    let snapshot = ResidentProofSnapshot {
        operator_class,
        stage_mask,
        materialization_kind,
        device_columns,
        has_device_selection,
        has_device_projection,
        cpu_boundary,
    };
    let canonical = snapshot
        .to_proof()
        .map_err(|_| DecodeError::InvalidResidentProof { field: "semantics" })?
        .snapshot();
    if canonical != snapshot {
        return Err(DecodeError::InvalidResidentProof {
            field: "canonical form",
        });
    }
    Ok(snapshot)
}

fn copy_wire_words(
    fields: &IntListReader<'_>,
    start: usize,
    len: usize,
    field: &'static str,
) -> Result<Vec<i32>, DecodeError> {
    if !fields.contains_range(start, len) {
        return Err(DecodeError::Truncated {
            index: fields.len(),
            field,
        });
    }
    let mut words = Vec::new();
    words
        .try_reserve_exact(len)
        .map_err(|_| DecodeError::AllocationFailed { field })?;
    for index in start..start + len {
        words.push(strict_word(fields, index, field)?);
    }
    Ok(words)
}

fn strict_f64_at(
    fields: &IntListReader<'_>,
    index: usize,
    field: &'static str,
) -> Result<f64, DecodeError> {
    let high = strict_word(fields, index, field)? as u32 as u64;
    let low = strict_word(fields, index + 1, field)? as u32 as u64;
    let value = f64::from_bits((high << 32) | low);
    if value == 0.0 && value.is_sign_negative() {
        return Err(DecodeError::InvalidValue {
            index,
            field,
            raw: i32::MIN,
        });
    }
    Ok(value)
}

fn validate_function_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<(), DecodeError> {
    if strict_word(fields, start, "FunctionScan sentinel")? != FUNCTIONSCAN_SENTINEL {
        return Err(DecodeError::InvalidValue {
            index: start,
            field: "FunctionScan sentinel",
            raw: strict_word(fields, start, "FunctionScan sentinel")?,
        });
    }
    if strict_word(fields, start + 1, "FunctionScan function OID")? == 0 {
        return Err(DecodeError::InvalidValue {
            index: start + 1,
            field: "FunctionScan function OID",
            raw: 0,
        });
    }
    let shape_raw = strict_word(fields, start + 2, "FunctionScan output shape")?;
    let shape = OutputShapeDisc::from_i32(shape_raw).ok_or(DecodeError::InvalidValue {
        index: start + 2,
        field: "FunctionScan output shape",
        raw: shape_raw,
    })?;
    let field_count = strict_word(fields, start + 3, "FunctionScan field count")? as u32;
    if match shape {
        OutputShapeDisc::Record => field_count == 0 || field_count as usize > MAX_TUPLE_COLUMNS,
        OutputShapeDisc::Scalar | OutputShapeDisc::VarLen => field_count != 0,
    } {
        return Err(DecodeError::InvalidValue {
            index: start + 3,
            field: "FunctionScan field count",
            raw: field_count as i32,
        });
    }
    let count = strict_count(
        fields,
        start + 4,
        "FunctionScan argument count",
        MAX_FUNCTION_ARGS,
        3,
        payload_end,
    )?;
    let mut index = start + 5;
    for _ in 0..count {
        let type_index = index + 2;
        if strict_word(fields, type_index, "FunctionScan argument type OID")? == 0 {
            return Err(DecodeError::InvalidValue {
                index: type_index,
                field: "FunctionScan argument type OID",
                raw: 0,
            });
        }
        index += 3;
    }
    require_payload_end(index, payload_end)
}

fn validate_srf_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<(), DecodeError> {
    let sentinel = strict_word(fields, start, "SRF target-list sentinel")?;
    if sentinel != SRF_TARGET_LIST_SENTINEL {
        return Err(DecodeError::InvalidValue {
            index: start,
            field: "SRF target-list sentinel",
            raw: sentinel,
        });
    }
    if strict_word(fields, start + 1, "SRF function OID")? == 0 {
        return Err(DecodeError::InvalidValue {
            index: start + 1,
            field: "SRF function OID",
            raw: 0,
        });
    }
    let shape_raw = strict_word(fields, start + 2, "SRF output shape")?;
    let shape = OutputShapeDisc::from_i32(shape_raw).ok_or(DecodeError::InvalidValue {
        index: start + 2,
        field: "SRF output shape",
        raw: shape_raw,
    })?;
    let field_count = strict_word(fields, start + 3, "SRF field count")? as u32;
    if match shape {
        OutputShapeDisc::Record => field_count == 0 || field_count as usize > MAX_TUPLE_COLUMNS,
        OutputShapeDisc::Scalar | OutputShapeDisc::VarLen => field_count != 0,
    } {
        return Err(DecodeError::InvalidValue {
            index: start + 3,
            field: "SRF field count",
            raw: field_count as i32,
        });
    }
    let arg_attno = strict_word(fields, start + 4, "SRF argument attno")?;
    let tlist_pos = strict_word(fields, start + 5, "SRF target-list position")?;
    if arg_attno <= 0 || tlist_pos < 0 {
        return Err(DecodeError::InvalidValue {
            index: if arg_attno <= 0 { start + 4 } else { start + 5 },
            field: if arg_attno <= 0 {
                "SRF argument attno"
            } else {
                "SRF target-list position"
            },
            raw: if arg_attno <= 0 { arg_attno } else { tlist_pos },
        });
    }
    let passthrough_count = strict_count(
        fields,
        start + 6,
        "SRF passthrough count",
        MAX_TUPLE_COLUMNS,
        1,
        payload_end,
    )?;
    let tlist_pos = usize::try_from(tlist_pos).map_err(|_| DecodeError::InvalidValue {
        index: start + 5,
        field: "SRF target-list position",
        raw: tlist_pos,
    })?;
    if passthrough_count == 0 || tlist_pos >= passthrough_count {
        return Err(DecodeError::InvalidValue {
            index: start + 5,
            field: "SRF target-list position",
            raw: tlist_pos as i32,
        });
    }
    let pass_start = start + 7;
    for offset in 0..passthrough_count {
        let raw = strict_word(fields, pass_start + offset, "SRF passthrough attno")?;
        let valid = if offset == tlist_pos {
            raw == 0
        } else {
            raw > 0
        };
        if !valid {
            return Err(DecodeError::InvalidValue {
                index: pass_start + offset,
                field: "SRF passthrough attno",
                raw,
            });
        }
    }
    let qual_count_index = pass_start + passthrough_count;
    let qual_count = strict_count(
        fields,
        qual_count_index,
        "SRF constant argument count",
        MAX_FUNCTION_ARGS,
        3,
        payload_end,
    )?;
    let mut index = qual_count_index + 1;
    for _ in 0..qual_count {
        let type_index = index + 2;
        if strict_word(fields, type_index, "SRF argument type OID")? == 0 {
            return Err(DecodeError::InvalidValue {
                index: type_index,
                field: "SRF argument type OID",
                raw: 0,
            });
        }
        index += 3;
    }
    require_payload_end(index, payload_end)
}

fn validate_window_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<(), DecodeError> {
    let count = strict_count(
        fields,
        start,
        "window spec count",
        MAX_WINDOW_SPECS,
        WINDOW_SPEC_INTS,
        payload_end,
    )?;
    if count == 0 {
        return Err(DecodeError::InvalidValue {
            index: start,
            field: "window spec count",
            raw: 0,
        });
    }
    let mut index = start + 1;
    for _ in 0..count {
        let func_raw = strict_word(fields, index, "window function")?;
        if WindowFunc::from_i32(func_raw).is_none() {
            return Err(DecodeError::InvalidValue {
                index,
                field: "window function",
                raw: func_raw,
            });
        }
        for attno_index in [index + 1, index + 2, index + 3] {
            let raw = strict_word(fields, attno_index, "window attno")?;
            if raw < 0 {
                return Err(DecodeError::InvalidValue {
                    index: attno_index,
                    field: "window attno",
                    raw,
                });
            }
        }
        if strict_word(fields, index + 6, "window result type OID")? == 0 {
            return Err(DecodeError::InvalidValue {
                index: index + 6,
                field: "window result type OID",
                raw: 0,
            });
        }
        strict_bool(fields, index + 7, "window uses-fp64 flag")?;
        index += WINDOW_SPEC_INTS;
    }
    let scan_relid = strict_word(fields, index, "window scan relid")?;
    if scan_relid < 0 {
        return Err(DecodeError::InvalidValue {
            index,
            field: "window scan relid",
            raw: scan_relid,
        });
    }
    require_payload_end(index + 1, payload_end)
}

fn validate_join_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
    plan: PlanPrivate,
) -> Result<(), DecodeError> {
    match plan.accel_strategy {
        AccelStrategy::GpuHashJoin => {
            if payload_end.saturating_sub(start) != 6 {
                return Err(DecodeError::LengthMismatch {
                    declared: 6,
                    actual: payload_end.saturating_sub(start),
                });
            }
            let inner_attno = strict_word(fields, start, "hash inner attno")?;
            let key_type = strict_word(fields, start + 1, "hash key type")?;
            let count_only = strict_bool(fields, start + 2, "hash count-only flag")?;
            let resident = strict_bool(fields, start + 3, "hash resident-count flag")?;
            let outer_oid = strict_word(fields, start + 4, "hash outer relation OID")? as u32;
            let inner_oid = strict_word(fields, start + 5, "hash inner relation OID")? as u32;
            if plan.target_attno <= 0 || inner_attno <= 0 || !matches!(key_type, 0..=2) {
                return Err(DecodeError::InvalidValue {
                    index: start,
                    field: "hash join payload",
                    raw: inner_attno,
                });
            }
            if resident {
                if !count_only || outer_oid == 0 || inner_oid == 0 {
                    return Err(DecodeError::InvalidValue {
                        index: start + 3,
                        field: "resident hash join payload",
                        raw: 1,
                    });
                }
            } else if outer_oid != 0 || inner_oid != 0 {
                return Err(DecodeError::InvalidValue {
                    index: start + 4,
                    field: "nonresident hash relation OIDs",
                    raw: outer_oid as i32,
                });
            }
            Ok(())
        }
        AccelStrategy::GpuNestedLoopIneq => {
            if payload_end.saturating_sub(start) != 5 {
                return Err(DecodeError::LengthMismatch {
                    declared: 5,
                    actual: payload_end.saturating_sub(start),
                });
            }
            let shape = strict_word(fields, start, "NLJ shape")?;
            let key_type = strict_word(fields, start + 1, "NLJ key type")?;
            let op = strict_word(fields, start + 2, "NLJ operation")?;
            let lo = strict_word(fields, start + 3, "NLJ lower attno")?;
            let hi = strict_word(fields, start + 4, "NLJ upper attno")?;
            if plan.target_attno <= 0
                || shape != 2
                || !matches!(key_type, 0..=2)
                || op != 0
                || lo <= 0
                || hi <= 0
            {
                return Err(DecodeError::InvalidValue {
                    index: start,
                    field: "NLJ payload",
                    raw: shape,
                });
            }
            Ok(())
        }
        _ => Err(DecodeError::StrategyAccelMismatch {
            strategy: plan.gpu_strategy,
            accel: plan.accel_strategy,
        }),
    }
}

fn legacy_olap_payload_words(kind: c_int) -> Option<usize> {
    match kind {
        OLAP_KIND_SSBM_Q1_REVENUE => Some(10),
        OLAP_KIND_SSBM_Q2_GROUPED_REVENUE | OLAP_KIND_SSBM_Q3_GROUPED_REVENUE => Some(6),
        OLAP_KIND_SSBM_Q4_GROUPED_PROFIT => Some(7),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64 => Some(24),
        OLAP_KIND_RESIDENT_H3_GROUPED_COUNT => Some(4),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE => Some(25),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL => Some(29),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE => Some(33),
        OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64 => Some(18),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)] // fixed legacy variants are quarantined here until Phase 5E deletion
fn validate_legacy_olap_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<usize, DecodeError> {
    let kind = strict_word(fields, start, "legacy OLAP kind")?;
    let words = legacy_olap_payload_words(kind).ok_or(DecodeError::InvalidValue {
        index: start,
        field: "legacy OLAP kind",
        raw: kind,
    })?;
    let end = start.checked_add(words).ok_or(DecodeError::LimitExceeded {
        index: start,
        field: "legacy OLAP payload",
        declared: words,
        maximum: MAX_PLAN_WIRE_INTS,
    })?;
    if end > payload_end {
        return Err(DecodeError::Truncated {
            index: payload_end,
            field: "legacy OLAP payload",
        });
    }
    let decoded_end = deserialize_olap_agg_spec_from_reader(fields, start)
        .map(|(_, next)| next)
        .ok_or(DecodeError::InvalidValue {
            index: start,
            field: "legacy OLAP payload",
            raw: kind,
        })?;
    if decoded_end != end {
        return Err(DecodeError::LengthMismatch {
            declared: end,
            actual: decoded_end,
        });
    }
    let relation_indices: &[usize] = match kind {
        OLAP_KIND_SSBM_Q1_REVENUE => &[1, 2],
        OLAP_KIND_SSBM_Q2_GROUPED_REVENUE | OLAP_KIND_SSBM_Q3_GROUPED_REVENUE => &[1, 2, 3, 4],
        OLAP_KIND_SSBM_Q4_GROUPED_PROFIT => &[1, 2, 3, 4, 5],
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64
        | OLAP_KIND_RESIDENT_H3_GROUPED_COUNT
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE => &[1],
        OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64 => &[1, 2],
        _ => &[],
    };
    for offset in relation_indices {
        if strict_word(fields, start + offset, "legacy OLAP relation OID")? == 0 {
            return Err(DecodeError::InvalidValue {
                index: start + offset,
                field: "legacy OLAP relation OID",
                raw: 0,
            });
        }
    }
    let requires_rhs_index = match kind {
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL => Some(start + 4),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE => Some(start + 8),
        _ => None,
    };
    if let Some(index) = requires_rhs_index {
        strict_bool(fields, index, "legacy OLAP requires-rhs flag")?;
    }
    if kind == OLAP_KIND_SSBM_Q1_REVENUE {
        let date_kind = strict_word(fields, start + 3, "SSBM date predicate")?;
        if !matches!(
            date_kind,
            OLAP_DATE_YEAR | OLAP_DATE_YEARMONTHNUM | OLAP_DATE_YEAR_WEEK
        ) {
            return Err(DecodeError::InvalidValue {
                index: start + 3,
                field: "SSBM date predicate",
                raw: date_kind,
            });
        }
        if date_kind != OLAP_DATE_YEAR_WEEK
            && strict_word(fields, start + 5, "SSBM unused date argument")? != 0
        {
            return Err(DecodeError::InvalidValue {
                index: start + 5,
                field: "SSBM unused date argument",
                raw: strict_word(fields, start + 5, "SSBM unused date argument")?,
            });
        }
        for (lo_offset, hi_offset) in [(6, 7), (8, 9)] {
            let lo = strict_word(fields, start + lo_offset, "SSBM range lower bound")?;
            let hi = strict_word(fields, start + hi_offset, "SSBM range upper bound")?;
            if lo > hi {
                return Err(DecodeError::InvalidValue {
                    index: start + lo_offset,
                    field: "SSBM range",
                    raw: lo,
                });
            }
        }
    }
    if kind == OLAP_KIND_RESIDENT_H3_GROUPED_COUNT {
        let resolution = strict_word(fields, start + 3, "resident H3 resolution")?;
        if !(0..=15).contains(&resolution) {
            return Err(DecodeError::InvalidValue {
                index: start + 3,
                field: "resident H3 resolution",
                raw: resolution,
            });
        }
    }
    let range_layout = match kind {
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64 => Some((start + 7, start + 8)),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_WITH_SOURCE
        | OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL => Some((start + 8, start + 9)),
        OLAP_KIND_RESIDENT_DENSE_GROUPED_F64_LOGICAL_SOURCE => Some((start + 12, start + 13)),
        _ => None,
    };
    if let Some((count_index, ranges_start)) = range_layout {
        let range_count_raw = strict_word(fields, count_index, "legacy OLAP range count")?;
        let range_count =
            usize::try_from(range_count_raw).map_err(|_| DecodeError::InvalidValue {
                index: count_index,
                field: "legacy OLAP range count",
                raw: range_count_raw,
            })?;
        for range in 0..RESIDENT_DENSE_GROUPED_F64_MEASURE_PREDICATE_MAX_RANGES {
            let range_start = ranges_start + range * 4;
            let lo = strict_f64_at(fields, range_start, "legacy OLAP range lower bound")?;
            let hi = strict_f64_at(fields, range_start + 2, "legacy OLAP range upper bound")?;
            if range < range_count {
                if lo.is_nan() || hi.is_nan() || lo > hi {
                    return Err(DecodeError::InvalidValue {
                        index: range_start,
                        field: "legacy OLAP range",
                        raw: 0,
                    });
                }
            } else if lo.to_bits() != 0 || hi.to_bits() != 0 {
                return Err(DecodeError::InvalidValue {
                    index: range_start,
                    field: "unused legacy OLAP range",
                    raw: strict_word(fields, range_start, "unused legacy OLAP range")?,
                });
            }
        }
    }
    if kind == OLAP_KIND_RESIDENT_STAR_DIM_GROUPED_F64 {
        for index in [start + 8, start + 11] {
            let opcode = strict_word(fields, index, "legacy star comparison opcode")?;
            if !(i32::from(crate::engine::expr_compiler::opcode::EQ)
                ..=i32::from(crate::engine::expr_compiler::opcode::ALWAYS_TRUE))
                .contains(&opcode)
            {
                return Err(DecodeError::InvalidValue {
                    index,
                    field: "legacy star comparison opcode",
                    raw: opcode,
                });
            }
        }
        for index in [start + 9, start + 12] {
            let value = strict_f64_at(fields, index, "legacy star comparison constant")?;
            if value.is_nan() {
                return Err(DecodeError::InvalidValue {
                    index,
                    field: "legacy star comparison constant",
                    raw: 0,
                });
            }
        }
    }
    Ok(end)
}

fn validate_partial_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<usize, DecodeError> {
    let count = strict_count(
        fields,
        start,
        "partial aggregate column count",
        MAX_LEGACY_AGGREGATES,
        4,
        payload_end,
    )?;
    if count == 0 {
        return Err(DecodeError::InvalidValue {
            index: start,
            field: "partial aggregate column count",
            raw: 0,
        });
    }
    let mut index = start + 1;
    for _ in 0..count {
        let op = strict_word(fields, index, "partial aggregate operation")?;
        if AggOp::from_i32(op).is_none() {
            return Err(DecodeError::InvalidValue {
                index,
                field: "partial aggregate operation",
                raw: op,
            });
        }
        if strict_word(fields, index + 1, "partial aggregate attno")? <= 0
            || strict_word(fields, index + 2, "partial aggregate transition type")? == 0
        {
            return Err(DecodeError::InvalidValue {
                index: index + 1,
                field: "partial aggregate column",
                raw: strict_word(fields, index + 1, "partial aggregate attno")?,
            });
        }
        index += 4;
    }
    Ok(index)
}

#[allow(clippy::too_many_lines)] // validates the complete ordered aggregate extension grammar
fn validate_agg_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<(Option<AggQuerySpec>, Option<AggOutputProjection>), DecodeError> {
    let count = strict_count(
        fields,
        start,
        "legacy aggregate count",
        MAX_LEGACY_AGGREGATES,
        3,
        payload_end,
    )?;
    let mut index = start + 1;
    for _ in 0..count {
        let op = strict_word(fields, index, "aggregate operation")?;
        if AggOp::from_i32(op).is_none() {
            return Err(DecodeError::InvalidValue {
                index,
                field: "aggregate operation",
                raw: op,
            });
        }
        let attno = strict_word(fields, index + 1, "aggregate attno")?;
        if attno < 0 || strict_word(fields, index + 2, "aggregate result type OID")? == 0 {
            return Err(DecodeError::InvalidValue {
                index: index + 1,
                field: "aggregate descriptor",
                raw: attno,
            });
        }
        index += 3;
    }
    let has_group = strict_bool(fields, index, "aggregate group-key presence")?;
    index += 1;
    if has_group {
        let attno = strict_word(fields, index, "aggregate group-key attno")?;
        let type_oid = strict_word(fields, index + 1, "aggregate group-key type OID")?;
        let key_type = strict_word(fields, index + 2, "aggregate group-key type")?;
        let attno2 = strict_word(fields, index + 3, "aggregate second group-key attno")?;
        let tlist_pos = strict_word(fields, index + 4, "aggregate group-key target position")?;
        if attno <= 0
            || type_oid == 0
            || !(matches!(key_type, 0 | 1 | 2 | 4 | 5) || is_h3_synthetic_group_key(key_type))
            || attno2 < 0
            || tlist_pos < 0
        {
            return Err(DecodeError::InvalidValue {
                index,
                field: "aggregate group-key descriptor",
                raw: attno,
            });
        }
        index += 5;
    }
    let self_scan_relid = strict_word(fields, index, "aggregate self-scan relid")?;
    if self_scan_relid < 0 {
        return Err(DecodeError::InvalidValue {
            index,
            field: "aggregate self-scan relid",
            raw: self_scan_relid,
        });
    }
    index += 1;

    let mut legacy_scan_expr = false;
    if fields.get(index) == Some(AGG_SCAN_EXPR_SENTINEL) {
        legacy_scan_expr = true;
        let template = strict_word(fields, index + 1, "aggregate scan-expression template")?;
        let width = match template {
            1 => 6,
            2 => 7,
            3 => 10,
            raw => {
                return Err(DecodeError::InvalidValue {
                    index: index + 1,
                    field: "aggregate scan-expression template",
                    raw,
                });
            }
        };
        if index + width > payload_end {
            return Err(DecodeError::Truncated {
                index: payload_end,
                field: "aggregate scan expression",
            });
        }
        let validate_column = |offset: usize| -> Result<(), DecodeError> {
            let raw = strict_word(fields, index + offset, "aggregate scan-expression column")?;
            if raw < 0 || raw as usize >= MAX_TUPLE_COLUMNS {
                return Err(DecodeError::InvalidValue {
                    index: index + offset,
                    field: "aggregate scan-expression column",
                    raw,
                });
            }
            Ok(())
        };
        let validate_opcode = |offset: usize| -> Result<(), DecodeError> {
            let raw = strict_word(fields, index + offset, "aggregate scan-expression opcode")?;
            if !(i32::from(crate::engine::expr_compiler::opcode::EQ)
                ..=i32::from(crate::engine::expr_compiler::opcode::ALWAYS_TRUE))
                .contains(&raw)
            {
                return Err(DecodeError::InvalidValue {
                    index: index + offset,
                    field: "aggregate scan-expression opcode",
                    raw,
                });
            }
            Ok(())
        };
        match template {
            1 => {
                validate_column(2)?;
                validate_opcode(3)?;
                let value = strict_f64_at(fields, index + 4, "aggregate scan constant")?;
                if value.is_nan() {
                    return Err(DecodeError::InvalidValue {
                        index: index + 4,
                        field: "aggregate scan constant",
                        raw: 0,
                    });
                }
            }
            2 => {
                validate_column(2)?;
                let lo = strict_f64_at(fields, index + 3, "aggregate scan lower bound")?;
                let hi = strict_f64_at(fields, index + 5, "aggregate scan upper bound")?;
                if lo.is_nan() || hi.is_nan() || lo > hi {
                    return Err(DecodeError::InvalidValue {
                        index: index + 3,
                        field: "aggregate scan range",
                        raw: 0,
                    });
                }
            }
            3 => {
                validate_column(2)?;
                validate_opcode(3)?;
                validate_column(6)?;
                validate_opcode(7)?;
                for constant_index in [index + 4, index + 8] {
                    let value = strict_f64_at(fields, constant_index, "aggregate scan constant")?;
                    if value.is_nan() {
                        return Err(DecodeError::InvalidValue {
                            index: constant_index,
                            field: "aggregate scan constant",
                            raw: 0,
                        });
                    }
                }
            }
            _ => unreachable!(),
        }
        index += width;
    }

    let mut legacy_olap = false;
    if fields.get(index) == Some(AGG_OLAP_SENTINEL) {
        legacy_olap = true;
        index = validate_legacy_olap_payload(fields, index + 1, payload_end)?;
    }

    let mut agg_query_spec = None;
    let mut agg_output_projection = None;
    if fields.get(index) == Some(AGG_QUERY_SPEC_SENTINEL) {
        let remaining = payload_end.saturating_sub(index + 1);
        let prefix_words = copy_wire_words(fields, index + 1, remaining, "aggregate query spec")?;
        let spec_len = AggQuerySpec::encoded_i32_prefix_len(&prefix_words)?;
        let spec_words = &prefix_words[..spec_len];
        agg_query_spec = Some(AggQuerySpec::decode_i32(spec_words)?);
        index += 1 + spec_len;
        if fields.get(index) != Some(AGG_OUTPUT_PROJECTION_SENTINEL) {
            return Err(DecodeError::Truncated {
                index,
                field: "aggregate output projection",
            });
        }
        let length_index = index + 3;
        let projection_len_raw =
            strict_word(fields, length_index, "aggregate output projection length")?;
        let projection_len =
            usize::try_from(projection_len_raw).map_err(|_| DecodeError::InvalidValue {
                index: length_index,
                field: "aggregate output projection length",
                raw: projection_len_raw,
            })?;
        if projection_len > AGG_OUTPUT_PROJECTION_MAX_WORDS {
            return Err(DecodeError::LimitExceeded {
                index: length_index,
                field: "aggregate output projection length",
                declared: projection_len,
                maximum: AGG_OUTPUT_PROJECTION_MAX_WORDS,
            });
        }
        let projection_words = copy_wire_words(
            fields,
            index + 1,
            projection_len,
            "aggregate output projection",
        )?;
        agg_output_projection = Some(AggOutputProjection::decode_i32(
            &projection_words,
            agg_query_spec.as_ref().expect("query spec assigned above"),
        )?);
        index += 1 + projection_len;
    }
    if legacy_olap && agg_query_spec.is_some() {
        return Err(DecodeError::InvalidValue {
            index,
            field: "duplicate aggregate execution contracts",
            raw: AGG_QUERY_SPEC_SENTINEL,
        });
    }

    let mut legacy_partial = false;
    if fields.get(index) == Some(PARTIAL_SENTINEL) {
        legacy_partial = true;
        index = validate_partial_payload(fields, index + 1, payload_end)?;
    }
    require_payload_end(index, payload_end)?;
    if !legacy_olap && agg_query_spec.is_none() {
        return Err(DecodeError::InvalidValue {
            index,
            field: "aggregate execution contract",
            raw: 0,
        });
    }
    if agg_query_spec.is_some() != agg_output_projection.is_some() {
        return Err(DecodeError::InvalidValue {
            index,
            field: "generic aggregate projection contract",
            raw: 0,
        });
    }
    if agg_query_spec.is_some()
        && (count != 0 || has_group || self_scan_relid != 0 || legacy_scan_expr || legacy_partial)
    {
        return Err(DecodeError::InvalidValue {
            index: start,
            field: "noncanonical generic aggregate legacy fields",
            raw: count as i32,
        });
    }
    Ok((agg_query_spec, agg_output_projection))
}

fn validate_plan_wire_from_reader(
    fields: &IntListReader<'_>,
    expected_method: Option<PlanExecMethod>,
) -> Result<ValidatedPlanWire, DecodeError> {
    let minimum = PLAN_PRIVATE_HEADER_INTS + RESIDENT_PROOF_TRAILER_INTS + PLAN_WIRE_FOOTER_INTS;
    if fields.len() < minimum {
        return Err(DecodeError::Truncated {
            index: fields.len(),
            field: "plan-private v2 frame",
        });
    }
    if fields.len() > MAX_PLAN_WIRE_INTS {
        return Err(DecodeError::LimitExceeded {
            index: 0,
            field: "plan-private word count",
            declared: fields.len(),
            maximum: MAX_PLAN_WIRE_INTS,
        });
    }
    let footer = fields.len() - PLAN_WIRE_FOOTER_INTS;
    let magic = strict_word(fields, footer, "plan-private magic")?;
    if magic != PLAN_WIRE_MAGIC {
        return Err(DecodeError::InvalidValue {
            index: footer,
            field: "plan-private magic",
            raw: magic,
        });
    }
    let version = strict_word(fields, footer + 1, "plan-private version")?;
    if version != PLAN_WIRE_VERSION {
        return Err(DecodeError::UnsupportedVersion { version });
    }
    let declared_raw = strict_word(fields, footer + 2, "plan-private word count")?;
    let declared = usize::try_from(declared_raw).map_err(|_| DecodeError::InvalidValue {
        index: footer + 2,
        field: "plan-private word count",
        raw: declared_raw,
    })?;
    if declared != fields.len() {
        return Err(DecodeError::LengthMismatch {
            declared,
            actual: fields.len(),
        });
    }
    let method_raw = strict_word(fields, footer + 3, "execution method")?;
    let method = PlanExecMethod::from_i32(method_raw)
        .ok_or(DecodeError::InvalidExecutionMethod { raw: method_raw })?;
    if let Some(expected) = expected_method
        && expected != method
    {
        return Err(DecodeError::ExecutionMethodMismatch {
            expected,
            actual: method,
        });
    }
    let plan_private = PlanPrivate::decode_reader(fields)?;
    let strategy_method = PlanExecMethod::for_strategy(plan_private.gpu_strategy).ok_or(
        DecodeError::UnsupportedGpuStrategy {
            strategy: plan_private.gpu_strategy,
        },
    )?;
    if strategy_method != method {
        return Err(DecodeError::StrategyMethodMismatch {
            strategy: plan_private.gpu_strategy,
            method,
        });
    }
    let proof_start = footer - RESIDENT_PROOF_TRAILER_INTS;
    let resident_proof = decode_resident_proof_at(fields, proof_start)?;

    let (agg_query_spec, agg_output_projection) = match method {
        PlanExecMethod::Scan => {
            require_payload_end(PLAN_PAYLOAD_START, proof_start)?;
            (None, None)
        }
        PlanExecMethod::Join => {
            validate_join_payload(fields, PLAN_PAYLOAD_START, proof_start, plan_private)?;
            (None, None)
        }
        PlanExecMethod::Agg => validate_agg_payload(fields, PLAN_PAYLOAD_START, proof_start)?,
        PlanExecMethod::Window => {
            validate_window_payload(fields, PLAN_PAYLOAD_START, proof_start)?;
            (None, None)
        }
        PlanExecMethod::FunctionScan => {
            validate_function_payload(fields, PLAN_PAYLOAD_START, proof_start)?;
            (None, None)
        }
        PlanExecMethod::SrfTargetList => {
            validate_srf_payload(fields, PLAN_PAYLOAD_START, proof_start)?;
            (None, None)
        }
    };

    Ok(ValidatedPlanWire {
        plan_private,
        resident_proof,
        agg_query_spec,
        agg_output_projection,
    })
}

#[cfg(test)]
fn validate_plan_wire_slice(
    fields: &[c_int],
    expected_method: PlanExecMethod,
) -> Result<(), DecodeError> {
    validate_plan_wire_from_reader(&IntListReader::from_slice(fields), Some(expected_method))
        .map(|_| ())
}

/// Bind neutral projection metadata to the PostgreSQL target list that defines
/// the CustomScan's output tuple descriptor.
///
/// # Safety
/// `target_list` must be null or a planner-owned `List<TargetEntry>`.
unsafe fn validate_projection_target_list(
    projection: &AggOutputProjection,
    target_list: *mut pg_sys::List,
) -> Result<(), DecodeError> {
    if target_list.is_null() {
        return Err(DecodeError::ProjectionTargetMismatch {
            index: 0,
            field: "slot count",
        });
    }
    // SAFETY: target_list was checked non-null and the caller promises a valid PG List.
    let target_count = unsafe { pg_sys::list_length(target_list) as usize };
    if target_count != projection.slots.len() {
        return Err(DecodeError::ProjectionTargetMismatch {
            index: 0,
            field: "slot count",
        });
    }
    for (index, slot) in projection.slots.iter().enumerate() {
        // SAFETY: length was checked above and target_list is planner-owned.
        let node = unsafe { pg_sys::list_nth(target_list, index as c_int) };
        if node.is_null() {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "TargetEntry node tag",
            });
        }
        // SAFETY: every PostgreSQL node begins with a NodeTag.
        let node_tag = unsafe { (*node.cast::<pg_sys::Node>()).type_ };
        if node_tag != pg_sys::NodeTag::T_TargetEntry {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "TargetEntry node tag",
            });
        }
        // SAFETY: NodeTag was checked above.
        let target = unsafe { &*node.cast::<pg_sys::TargetEntry>() };
        if i32::from(target.resno) != i32::try_from(index + 1).expect("target index fits i32")
            || target.expr.is_null()
        {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "target-list order",
            });
        }
        // SAFETY: target.expr is a non-null planner-owned expression node.
        let (result_type_oid, result_typmod, result_collation_oid) = unsafe {
            (
                pg_sys::exprType(target.expr.cast()),
                pg_sys::exprTypmod(target.expr.cast()),
                pg_sys::exprCollation(target.expr.cast()),
            )
        };
        if result_type_oid.to_u32() != slot.result_type_oid {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "result type OID",
            });
        }
        if result_typmod != slot.result_typmod {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "result typmod",
            });
        }
        if result_collation_oid.to_u32() != slot.result_collation_oid {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "result collation OID",
            });
        }
    }
    Ok(())
}

unsafe fn validate_projection_plan_lists(
    projection: &AggOutputProjection,
    cscan: *mut pg_sys::CustomScan,
) -> Result<(), DecodeError> {
    // PG18 nodeCustom.c builds the scan and result slots from different lists;
    // a neutral aggregate plan promises both are one-to-one with projection.
    // SAFETY: the caller promises cscan is a valid planner/executor-owned node.
    let custom_scan_tlist = unsafe { (*cscan).custom_scan_tlist };
    // SAFETY: custom_scan_tlist belongs to cscan and is valid for this call.
    unsafe { validate_projection_target_list(projection, custom_scan_tlist) }?;
    // SAFETY: the caller promises cscan is a valid planner/executor-owned node.
    let plan_targetlist = unsafe { (*cscan).scan.plan.targetlist };
    // SAFETY: plan_targetlist belongs to cscan and is valid for this call.
    unsafe { validate_projection_target_list(projection, plan_targetlist) }?;
    Ok(())
}

/// Re-bind projection metadata to the actual scan-slot descriptor after
/// PostgreSQL has initialized the CustomScan state.
///
/// # Safety
/// `tuple_desc` must be null or a valid PostgreSQL `TupleDesc`.
pub(super) unsafe fn validate_projection_tuple_desc(
    projection: &AggOutputProjection,
    tuple_desc: pg_sys::TupleDesc,
) -> Result<(), DecodeError> {
    if tuple_desc.is_null() {
        return Err(DecodeError::ProjectionTargetMismatch {
            index: 0,
            field: "TupleDesc slot count",
        });
    }
    // SAFETY: tuple_desc was checked non-null and the caller promises it is valid.
    let attribute_count = unsafe { (*tuple_desc).natts as usize };
    if attribute_count != projection.slots.len() {
        return Err(DecodeError::ProjectionTargetMismatch {
            index: 0,
            field: "TupleDesc slot count",
        });
    }
    for (index, slot) in projection.slots.iter().enumerate() {
        // SAFETY: natts was checked and tuple_desc is valid.
        let attribute = unsafe { &*crate::engine::pg_compat::tuple_desc_attr(tuple_desc, index) };
        if attribute.atttypid.to_u32() != slot.result_type_oid {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "TupleDesc type OID",
            });
        }
        if attribute.atttypmod != slot.result_typmod {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "TupleDesc typmod",
            });
        }
        if attribute.attcollation.to_u32() != slot.result_collation_oid {
            return Err(DecodeError::ProjectionTargetMismatch {
                index,
                field: "TupleDesc collation OID",
            });
        }
    }
    Ok(())
}

/// Validate the complete production plan-private frame before PostgreSQL is
/// allowed to allocate an executor state for it.
///
/// # Safety
/// `custom_private` must be null or a valid PostgreSQL `List *`. Every element
/// is checked for `T_Integer` before it is interpreted.
pub(super) unsafe fn validate_custom_private_wire(
    cscan: *mut pg_sys::CustomScan,
    expected_method: PlanExecMethod,
) -> Result<(), DecodeError> {
    if cscan.is_null() {
        return Err(DecodeError::Truncated {
            index: 0,
            field: "CustomScan plan",
        });
    }
    // SAFETY: cscan was checked above.
    let custom_private = unsafe { (*cscan).custom_private };
    if custom_private.is_null() {
        return Err(DecodeError::Truncated {
            index: 0,
            field: "plan-private v2 frame",
        });
    }
    // SAFETY: caller guarantees a valid PostgreSQL List pointer; the reader
    // checks each element's NodeTag before casting it to Integer.
    let fields = unsafe { IntListReader::from_pg_list(custom_private) };
    let validated = validate_plan_wire_from_reader(&fields, Some(expected_method))?;
    if let Some(projection) = &validated.agg_output_projection {
        // SAFETY: cscan is a valid planner/executor-owned CustomScan.
        unsafe { validate_projection_plan_lists(projection, cscan) }?;
    }
    Ok(())
}

/// Deserialize strategy, batch size, and accel context from
/// `custom_private`.
///
/// Layout: `[strategy, batch_size, expected_threads, fn_oid, target_attno,
///   accel_strategy, ...strategy-specific payload, resident-proof-v2,
///   plan-wire-magic, version, exact-word-count, execution-method]`.
///
/// Invalid or missing private data raises a PostgreSQL ERROR.
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` emitted by this build's
/// planner hooks.
#[allow(clippy::too_many_lines)]
pub(super) unsafe fn deserialize_custom_private(
    cscan: *mut pg_sys::CustomScan,
    expected_method: PlanExecMethod,
) -> CustomPrivateData {
    if cscan.is_null() {
        pgrx::error!("pg_accel: missing CustomScan plan");
    }
    // SAFETY: cscan was checked above.
    let custom_private = unsafe { (*cscan).custom_private };
    if custom_private.is_null() {
        pgrx::error!("pg_accel: missing CustomScan private data");
    }

    // SAFETY: custom_private is a valid List of Integer nodes.
    let fields = unsafe { IntListReader::from_pg_list(custom_private) };
    let validated = validate_plan_wire_from_reader(&fields, Some(expected_method))
        .unwrap_or_else(|err| pgrx::error!("pg_accel: invalid CustomScan private data: {err}"));
    if let Some(projection) = &validated.agg_output_projection {
        // SAFETY: cscan is a valid planner/executor-owned CustomScan.
        unsafe { validate_projection_plan_lists(projection, cscan) }.unwrap_or_else(|error| {
            pgrx::error!("pg_accel: invalid aggregate output projection: {error}");
        });
    }
    let plan_private = validated.plan_private;
    let gpu_strategy = plan_private.gpu_strategy;
    let resident_proof = validated.resident_proof;
    let payload = PlanPayloadReader::new(&fields);
    let batch_size = plan_private.batch_size;
    let expected_threads = plan_private.expected_threads;
    let fn_oid = plan_private.fn_oid;
    let target_attno = plan_private.target_attno;
    let accel_strategy = plan_private.accel_strategy;

    // For Agg strategy, read the aggregate descriptor count at index 6. The
    // count is still load-bearing for wire offsets: the OLAP trailer that the
    // resident agg executor consumes sits after the descriptor + group-key
    // blocks. The descriptors/group-key themselves fed only the retired
    // host-staged agg executors and are no longer materialized.
    // Layout: [num_aggs, op0, attno0, rtype0, op1, attno1, rtype1, ...]
    let agg_count = if matches!(gpu_strategy, GpuStrategy::Agg) {
        payload.agg_count()
    } else {
        0
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

    // For Agg strategy, walk past `self_scan_relid` (immediately after the
    // group-key block) to the sentinel trailers and keep the OLAP spec — the
    // only trailer the surviving resident agg executor consumes. The scan-expr
    // and partial trailers fed retired host-staged executors; they are still
    // parsed for wire compatibility but discarded.
    //
    // Layout (Agg):
    //   [..., num_aggs, (op,attno,rtype)*N,
    //    has_gk, (gk_attno, gk_type_oid, gk_key_type, gk2_attno, gk_tlist_pos)?,
    //    self_scan_relid, sentinel trailers...]
    let olap_agg = if matches!(gpu_strategy, GpuStrategy::Agg) {
        let (_self_scan_relid, _agg_scan_expr, _partial, olap_agg) =
            payload.agg_self_scan_relid_and_trailers(agg_count);
        olap_agg
    } else {
        None
    };

    CustomPrivateData {
        gpu_strategy,
        batch_size,
        expected_threads,
        fn_oid,
        target_attno,
        accel_strategy,
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
        olap_agg,
        agg_query_spec: validated.agg_query_spec,
        agg_output_projection: validated.agg_output_projection,
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

/// Append the neutral aggregate query and ordered output contracts consumed by
/// the Phase 5 descriptor executor. Both blocks own independent v2 framing.
///
/// # Safety
/// Must be called in a valid PostgreSQL planner memory context. `cscan` must
/// own one-to-one `custom_scan_tlist` and `plan.targetlist` lists matching the
/// ordered projection.
#[allow(dead_code)] // Cross-branch API consumed by the Phase 5C shape planner after integration.
pub(in crate::engine::ffi) unsafe fn append_agg_query_plan(
    list: *mut pg_sys::List,
    spec: &AggQuerySpec,
    projection: &AggOutputProjection,
    cscan: *mut pg_sys::CustomScan,
) -> *mut pg_sys::List {
    let spec_words = spec
        .encode_i32()
        .unwrap_or_else(|error| pgrx::error!("pg_accel: invalid aggregate query spec: {error}"));
    let projection_words = projection.encode_i32(spec).unwrap_or_else(|error| {
        pgrx::error!("pg_accel: invalid aggregate output projection: {error}")
    });
    if cscan.is_null() {
        pgrx::error!("pg_accel: aggregate output projection has no CustomScan plan");
    }
    // SAFETY: caller supplies the planner-owned CustomScan being serialized.
    unsafe { validate_projection_plan_lists(projection, cscan) }.unwrap_or_else(|error| {
        pgrx::error!("pg_accel: aggregate output projection does not match plan lists: {error}");
    });
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(AGG_QUERY_SPEC_SENTINEL);
    for word in spec_words {
        writer.push_int(word);
    }
    writer.push_int(AGG_OUTPUT_PROJECTION_SENTINEL);
    for word in projection_words {
        writer.push_int(word);
    }
    writer.into_list()
}

/// Seal a plan-private list with the required v2 footer.
///
/// # Safety
/// Must be called in a valid PostgreSQL planner memory context. `list` must be
/// a `List<Integer>` whose final section is a resident-proof trailer.
pub(super) unsafe fn append_plan_wire_footer(
    list: *mut pg_sys::List,
    method: PlanExecMethod,
) -> *mut pg_sys::List {
    let current_len = if list.is_null() {
        0usize
    } else {
        // SAFETY: caller guarantees a valid list.
        unsafe { pg_sys::list_length(list) as usize }
    };
    let total_len = current_len
        .checked_add(PLAN_WIRE_FOOTER_INTS)
        .filter(|len| *len <= MAX_PLAN_WIRE_INTS)
        .and_then(|len| c_int::try_from(len).ok())
        .unwrap_or_else(|| pgrx::error!("pg_accel: plan-private v2 frame is too large"));
    let mut writer = PgListWriter::from_existing(list);
    writer.push_int(PLAN_WIRE_MAGIC);
    writer.push_int(PLAN_WIRE_VERSION);
    writer.push_int(total_len);
    writer.push_int(method as c_int);
    writer.into_list()
}

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

/// Magic marker preceding a serialized [`ResidentProofSnapshot`] trailer.
///
/// This block is always appended after each strategy-specific payload so legacy
/// offsets stay stable: generic scan/sort/agg/window payloads still start at
/// index 6, FunctionScan/SRF sentinels still live at index 6, and PreAgg still
/// starts at index 3.
pub(in crate::engine::ffi) const RESIDENT_PROOF_SENTINEL: c_int = 0x5250_5246; // b"RPRF"
pub(in crate::engine::ffi) const RESIDENT_PROOF_VERSION: c_int = 2;
pub(super) const RESIDENT_PROOF_TRAILER_INTS: usize = 9;

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
    let canonical = proof
        .to_proof()
        .unwrap_or_else(|error| pgrx::error!("pg_accel: invalid resident proof: {error:?}"))
        .snapshot();
    if canonical != proof {
        pgrx::error!("pg_accel: resident proof is not canonically encoded");
    }
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

/// Decode the resident-proof trailer if this plan carries one.
///
/// Absence returns `None`; every selected v2 path/plan caller treats that as an
/// error. A present but malformed trailer is also an ERROR, because silently
/// treating corrupt proof data as valid would reopen CPU-backed plan reporting.
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
    let payload_end = if fields.len() >= PLAN_WIRE_FOOTER_INTS
        && fields.get(fields.len() - PLAN_WIRE_FOOTER_INTS) == Some(PLAN_WIRE_MAGIC)
    {
        fields.len() - PLAN_WIRE_FOOTER_INTS
    } else {
        fields.len()
    };
    if payload_end < RESIDENT_PROOF_TRAILER_INTS {
        return None;
    }
    let idx = payload_end - RESIDENT_PROOF_TRAILER_INTS;
    if fields.int_at(idx) != RESIDENT_PROOF_SENTINEL {
        return None;
    }
    Some(
        decode_resident_proof_at(fields, idx).unwrap_or_else(|error| {
            pgrx::error!("pg_accel: invalid resident proof trailer: {error}")
        }),
    )
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
    if spec.per_column.is_empty() || spec.per_column.len() > MAX_LEGACY_AGGREGATES {
        pgrx::error!("pg_accel: invalid partial aggregate column count");
    }
    for column in &spec.per_column {
        if column.attno <= 0 || column.transtype_oid == pg_sys::InvalidOid {
            pgrx::error!("pg_accel: invalid partial aggregate column metadata");
        }
    }
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
    let payload_end = payload_end_before_proof(fields);
    let n_cols = strict_count(
        fields,
        start_idx,
        "partial aggregate column count",
        MAX_LEGACY_AGGREGATES,
        4,
        payload_end,
    )
    .ok()?;
    if n_cols == 0 {
        return None;
    }
    let base = start_idx + 1;
    let end = base.checked_add(n_cols.checked_mul(4)?)?;
    if end > payload_end {
        return None;
    }
    let mut per_column = Vec::new();
    per_column.try_reserve_exact(n_cols).ok()?;
    for k in 0..n_cols {
        let mut column = fields.cursor_at(base + k * 4);
        let op_raw = column.read_int();
        let op = AggOp::from_i32(op_raw)?;
        let attno = column.read_int();
        let transtype_oid = column.read_oid();
        if attno <= 0 || transtype_oid == pg_sys::InvalidOid {
            return None;
        }
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
    let shape = OutputShapeDisc::from_i32(priv_data.output_shape_disc)
        .unwrap_or_else(|| pgrx::error!("pg_accel: invalid FunctionScan output shape"));
    let invalid_field_count = match shape {
        OutputShapeDisc::Record => {
            priv_data.output_shape_field_count == 0
                || priv_data.output_shape_field_count as usize > MAX_TUPLE_COLUMNS
        }
        OutputShapeDisc::Scalar | OutputShapeDisc::VarLen => {
            priv_data.output_shape_field_count != 0
        }
    };
    if priv_data.fn_oid == pg_sys::InvalidOid
        || invalid_field_count
        || priv_data.args.len() > MAX_FUNCTION_ARGS
        || priv_data.args.iter().any(|(_, type_oid)| *type_oid == 0)
    {
        pgrx::error!("pg_accel: invalid FunctionScan private data");
    }
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
    let payload_end = payload_end_before_proof(&fields);
    validate_function_payload(&fields, start_idx, payload_end).ok()?;
    let mut fixed = fields.cursor_at(start_idx);
    let _sentinel = fixed.read_int();
    let fn_oid = fixed.read_oid();
    let output_shape_disc = fixed.read_int();
    let output_shape_field_count = fixed.read_u32();
    let n_args = fixed.read_usize();
    let payload_base = fixed.position();
    let mut args = Vec::new();
    args.try_reserve_exact(n_args).ok()?;
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
    let shape = OutputShapeDisc::from_i32(priv_data.output_shape_disc)
        .unwrap_or_else(|| pgrx::error!("pg_accel: invalid SRF output shape"));
    let invalid_field_count = match shape {
        OutputShapeDisc::Record => {
            priv_data.output_shape_field_count == 0
                || priv_data.output_shape_field_count as usize > MAX_TUPLE_COLUMNS
        }
        OutputShapeDisc::Scalar | OutputShapeDisc::VarLen => {
            priv_data.output_shape_field_count != 0
        }
    };
    let srf_position = usize::try_from(priv_data.srf_tlist_pos).ok();
    let passthrough_valid = srf_position.is_some_and(|position| {
        !priv_data.passthrough_attnos.is_empty()
            && position < priv_data.passthrough_attnos.len()
            && priv_data
                .passthrough_attnos
                .iter()
                .enumerate()
                .all(|(index, attno)| {
                    if index == position {
                        *attno == 0
                    } else {
                        *attno > 0
                    }
                })
    });
    if priv_data.fn_oid == pg_sys::InvalidOid
        || invalid_field_count
        || priv_data.srf_arg_attno <= 0
        || priv_data.passthrough_attnos.len() > MAX_TUPLE_COLUMNS
        || !passthrough_valid
        || priv_data.qual_args.len() > MAX_FUNCTION_ARGS
        || priv_data
            .qual_args
            .iter()
            .any(|(_, type_oid)| *type_oid == 0)
    {
        pgrx::error!("pg_accel: invalid SRF target-list private data");
    }
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
    let payload_end = payload_end_before_proof(&fields);
    validate_srf_payload(&fields, start_idx, payload_end).ok()?;
    let mut fixed = fields.cursor_at(start_idx);
    let _sentinel = fixed.read_int();
    let fn_oid = fixed.read_oid();
    let output_shape_disc = fixed.read_int();
    let output_shape_field_count = fixed.read_u32();
    let srf_arg_attno = fixed.read_int();
    let srf_tlist_pos = fixed.read_int();
    let n_passthrough = fixed.read_usize();
    let pass_base = fixed.position();
    let mut passthrough_attnos = Vec::new();
    passthrough_attnos.try_reserve_exact(n_passthrough).ok()?;
    let mut passthrough = fields.cursor_at(pass_base);
    for _ in 0..n_passthrough {
        passthrough_attnos.push(passthrough.read_int());
    }
    let n_qual_args = passthrough.read_usize();
    let qual_base = passthrough.position();
    let mut qual_args = Vec::new();
    qual_args.try_reserve_exact(n_qual_args).ok()?;
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

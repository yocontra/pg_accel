//! `custom_private` serialization / deserialization.
//!
//! Plan metadata travels through PG as a `List *` of `Integer` nodes so it
//! survives plan copying and EXPLAIN output. Field order is load-bearing.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::{GpuStrategy, OutputShapeDisc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};
use crate::engine::gucs;
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
pub(in crate::engine::ffi) const AGG_QUERY_SPEC_SENTINEL: c_int = 0x4151_5333; // b"AQS3"
pub(in crate::engine::ffi) const AGG_OUTPUT_PROJECTION_SENTINEL: c_int = 0x414F_5032; // b"AOP2"
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
        words.push(AGG_QUERY_SPEC_SENTINEL);
        words.extend(spec.encode_i32().expect("query spec encodes"));
        words.push(AGG_OUTPUT_PROJECTION_SENTINEL);
        words.extend(projection.encode_i32(&spec).expect("projection encodes"));
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
        join.extend([2, 0, 0]);
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
    fn retired_olap_aggregate_contract_is_rejected() {
        let mut words = generic_agg_frame();
        words[PLAN_PAYLOAD_START] = i32::from_be_bytes(*b"OLAP");
        assert!(validate_plan_wire_slice(&words, PlanExecMethod::Agg).is_err());
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
    fn neutral_aggregate_rejects_prefixes_suffixes_and_retired_tags() {
        for retired_tag in [
            0,
            i32::from_be_bytes(*b"AGXP"),
            i32::from_be_bytes(*b"PAAG"),
            i32::from_be_bytes(*b"AQS2"),
        ] {
            let mut prefixed = generic_agg_frame();
            prefixed.insert(PLAN_PAYLOAD_START, retired_tag);
            let footer = prefixed.len() - PLAN_WIRE_FOOTER_INTS;
            prefixed[footer + 2] = prefixed.len() as i32;
            assert!(validate_plan_wire_slice(&prefixed, PlanExecMethod::Agg).is_err());
        }

        let mut trailing = generic_agg_frame();
        let proof = trailing.len() - PLAN_WIRE_FOOTER_INTS - RESIDENT_PROOF_TRAILER_INTS;
        trailing.insert(proof, 0);
        let footer = trailing.len() - PLAN_WIRE_FOOTER_INTS;
        trailing[footer + 2] = trailing.len() as i32;
        assert!(validate_plan_wire_slice(&trailing, PlanExecMethod::Agg).is_err());
    }

    #[test]
    fn neutral_aggregate_contract_roundtrips_to_proof_boundary() {
        let frame = generic_agg_frame();
        let reader = IntListReader::from_slice(&frame);
        let proof_start = frame.len() - PLAN_WIRE_FOOTER_INTS - RESIDENT_PROOF_TRAILER_INTS;
        assert_eq!(frame[PLAN_PAYLOAD_START], AGG_QUERY_SPEC_SENTINEL);
        let (spec, projection) =
            decode_agg_query_contract_at(&reader, PLAN_PAYLOAD_START, proof_start)
                .expect("strict path contract decodes");
        assert_eq!(spec.fact_rel, 42);
        assert_eq!(spec.measures.len(), 1);
        assert_eq!(projection.slots.len(), 1);
        projection.validate(&spec).expect("projection matches spec");
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
    const WINDOW_COUNT: usize = PLAN_PAYLOAD_START;
    const WINDOW_SPECS: usize = Self::WINDOW_COUNT + 1;
    const HASH_INNER_ATTNO: usize = PLAN_PAYLOAD_START;
    const HASH_KEY_TYPE: usize = PLAN_PAYLOAD_START + 1;
    const HASH_COUNT_ONLY: usize = PLAN_PAYLOAD_START + 2;
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
    fn hash_join_info(&self) -> (i32, i32, bool) {
        (
            self.fields.int_at(Self::HASH_INNER_ATTNO),
            self.fields.int_at(Self::HASH_KEY_TYPE),
            self.fields.int_at(Self::HASH_COUNT_ONLY) == 1,
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

fn decode_agg_query_contract_at(
    fields: &IntListReader<'_>,
    mut index: usize,
    contract_end: usize,
) -> Result<(AggQuerySpec, AggOutputProjection), DecodeError> {
    if index >= contract_end {
        return Err(DecodeError::Truncated {
            index,
            field: "aggregate query spec sentinel",
        });
    }
    let sentinel = strict_word(fields, index, "aggregate query spec sentinel")?;
    if sentinel != AGG_QUERY_SPEC_SENTINEL {
        return Err(DecodeError::InvalidValue {
            index,
            field: "aggregate query spec sentinel",
            raw: sentinel,
        });
    }
    let spec_start = index + 1;
    let remaining = contract_end
        .checked_sub(spec_start)
        .ok_or(DecodeError::Truncated {
            index: contract_end,
            field: "aggregate query spec",
        })?;
    let prefix_words = copy_wire_words(fields, spec_start, remaining, "aggregate query spec")?;
    let spec_len = AggQuerySpec::encoded_i32_prefix_len(&prefix_words)?;
    let spec = AggQuerySpec::decode_i32(&prefix_words[..spec_len])?;
    index = spec_start + spec_len;
    if index >= contract_end {
        return Err(DecodeError::Truncated {
            index,
            field: "aggregate output projection sentinel",
        });
    }
    let sentinel = strict_word(fields, index, "aggregate output projection sentinel")?;
    if sentinel != AGG_OUTPUT_PROJECTION_SENTINEL {
        return Err(DecodeError::InvalidValue {
            index,
            field: "aggregate output projection sentinel",
            raw: sentinel,
        });
    }
    let projection_start = index + 1;
    let length_index = projection_start + 2;
    if length_index >= contract_end {
        return Err(DecodeError::Truncated {
            index: contract_end,
            field: "aggregate output projection length",
        });
    }
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
    let projection_end =
        projection_start
            .checked_add(projection_len)
            .ok_or(DecodeError::LimitExceeded {
                index: length_index,
                field: "aggregate output projection length",
                declared: projection_len,
                maximum: AGG_OUTPUT_PROJECTION_MAX_WORDS,
            })?;
    if projection_end > contract_end {
        return Err(DecodeError::Truncated {
            index: contract_end,
            field: "aggregate output projection",
        });
    }
    let projection_words = copy_wire_words(
        fields,
        projection_start,
        projection_len,
        "aggregate output projection",
    )?;
    let projection = AggOutputProjection::decode_i32(&projection_words, &spec)?;
    require_payload_end(projection_end, contract_end)?;
    Ok((spec, projection))
}

/// Decode the strict AQS3/AOP2 trailer carried by a selected aggregate path.
///
/// # Safety
/// `path_private` must be a valid PostgreSQL `List<Integer>` emitted by this
/// extension in the current planner context.
pub(super) unsafe fn deserialize_agg_query_path_contract(
    path_private: *mut pg_sys::List,
) -> Result<(AggQuerySpec, AggOutputProjection), DecodeError> {
    // SAFETY: caller supplies a planner-owned list; the reader validates every
    // node before any integer is interpreted.
    let fields = unsafe { IntListReader::from_pg_list(path_private) };
    let contract_end = payload_end_before_proof(&fields);
    decode_agg_query_contract_at(&fields, 0, contract_end)
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
            if payload_end.saturating_sub(start) != 3 {
                return Err(DecodeError::LengthMismatch {
                    declared: 3,
                    actual: payload_end.saturating_sub(start),
                });
            }
            let inner_attno = strict_word(fields, start, "hash inner attno")?;
            let key_type = strict_word(fields, start + 1, "hash key type")?;
            strict_bool(fields, start + 2, "hash count-only flag")?;
            if plan.target_attno <= 0 || inner_attno <= 0 || !matches!(key_type, 0..=2) {
                return Err(DecodeError::InvalidValue {
                    index: start,
                    field: "hash join payload",
                    raw: inner_attno,
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

fn validate_agg_payload(
    fields: &IntListReader<'_>,
    start: usize,
    payload_end: usize,
) -> Result<(Option<AggQuerySpec>, Option<AggOutputProjection>), DecodeError> {
    let (spec, projection) = decode_agg_query_contract_at(fields, start, payload_end)?;
    Ok((Some(spec), Some(projection)))
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
    let (hash_inner_attno, hash_key_type, hash_count_only) =
        if accel_strategy == AccelStrategy::GpuHashJoin {
            payload.hash_join_info()
        } else {
            (0, 0, false)
        };

    // For Join strategy with GpuNestedLoopIneq accel, read NLJ info at index 6+.
    // Layout: [...base 6 fields..., shape, key_type, op, inner_lo_attno, inner_hi_attno]
    let (nlj_shape, nlj_key_type, nlj_op, nlj_inner_lo_attno, nlj_inner_hi_attno) =
        if accel_strategy == AccelStrategy::GpuNestedLoopIneq {
            payload.nlj_info()
        } else {
            (0, 0, 0, 0, 0)
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
        nlj_shape,
        nlj_key_type,
        nlj_op,
        nlj_inner_lo_attno,
        nlj_inner_hi_attno,
        window_specs,
        window_scan_relid,
        agg_query_spec: validated.agg_query_spec,
        agg_output_projection: validated.agg_output_projection,
        resident_proof,
    }
}

/// Append the neutral aggregate query and ordered output contracts consumed by
/// the Phase 5 descriptor executor. Both blocks own independent v2 framing.
///
/// # Safety
/// Must be called in a valid PostgreSQL planner memory context. `cscan` must
/// own one-to-one `custom_scan_tlist` and `plan.targetlist` lists matching the
/// ordered projection.
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

/// Magic marker preceding a serialized [`ResidentProofSnapshot`] trailer.
///
/// This block is always appended after each strategy-specific payload and before
/// the plan-wire footer.
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

// ---------------------------------------------------------------------------
// FunctionScan serialization / deserialization (Phase 2 F3)
// ---------------------------------------------------------------------------

/// Magic marker preceding a serialized [`FunctionScanPrivData`].
///
/// Distinct from the other strategy-local sentinels so a layout regression
/// cannot silently decode one payload as another.
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

    /// Strategy-local sentinels must remain unambiguous.
    #[test]
    fn functionscan_sentinel_is_distinct() {
        assert_ne!(FUNCTIONSCAN_SENTINEL, AGG_QUERY_SPEC_SENTINEL);
        assert_ne!(FUNCTIONSCAN_SENTINEL, AGG_OUTPUT_PROJECTION_SENTINEL);
        assert_ne!(FUNCTIONSCAN_SENTINEL, SRF_TARGET_LIST_SENTINEL);
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
/// Distinct from the other strategy-local sentinels so a layout regression
/// cannot silently decode one payload as another.
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

    /// Strategy-local sentinels must remain unambiguous.
    #[test]
    fn srf_target_list_sentinel_distinct() {
        assert_ne!(SRF_TARGET_LIST_SENTINEL, FUNCTIONSCAN_SENTINEL);
        assert_ne!(SRF_TARGET_LIST_SENTINEL, AGG_QUERY_SPEC_SENTINEL);
        assert_ne!(SRF_TARGET_LIST_SENTINEL, AGG_OUTPUT_PROJECTION_SENTINEL);
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

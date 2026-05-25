//! `custom_private` serialization / deserialization.
//!
//! Plan metadata travels through PG as a `List *` of `Integer` nodes so it
//! survives plan copying and EXPLAIN output. Field order is load-bearing.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::GpuStrategy;
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::executor::agg::{AggOp, GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE};
use crate::engine::executor::preagg::{DimFilter, GroupKeyDesc, JoinDepthDesc, PreAggColDesc};
use crate::engine::executor::sort::{SORT_KEY_INTS, SortKeyDesc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};
use crate::engine::gucs;
use crate::engine::registry::AccelStrategy;
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
}

/// Deserialized acceleration metadata from `custom_private`.
pub(super) struct CustomPrivateData {
    pub(super) gpu_strategy: GpuStrategy,
    pub(super) batch_size: c_int,
    pub(super) fn_oid: pg_sys::Oid,
    pub(super) target_attno: i32,
    pub(super) accel_strategy: AccelStrategy,
    pub(super) sort_keys: Vec<SortKeyDesc>,
    /// Limit for top-k sort optimization. `None` means no limit.
    pub(super) sort_limit: Option<usize>,
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
    const SORT_KEY_COUNT: usize = PLAN_PAYLOAD_START;
    const SORT_KEYS: usize = Self::SORT_KEY_COUNT + 1;
    const AGG_COUNT: usize = PLAN_PAYLOAD_START;
    const AGGS: usize = Self::AGG_COUNT + 1;
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
    fn sort_key_count(&self) -> usize {
        self.fields.int_at(Self::SORT_KEY_COUNT) as usize
    }

    #[must_use]
    fn sort_keys(&self, count: usize) -> Vec<SortKeyDesc> {
        let mut keys = Vec::with_capacity(count);
        for key_index in 0..count {
            let offset = Self::SORT_KEYS + key_index * SORT_KEY_INTS;
            if !self.fields.contains_range(offset, SORT_KEY_INTS) {
                break;
            }
            let mut key = self.fields.cursor_at(offset);
            keys.push(SortKeyDesc {
                attno: key.read_int() as i16,
                sort_op: key.read_oid(),
                collation: key.read_oid(),
                nulls_first: key.read_bool(),
            });
        }
        keys
    }

    #[must_use]
    fn sort_limit(&self, key_count: usize) -> Option<usize> {
        let limit_idx = Self::SORT_KEYS + key_count * SORT_KEY_INTS;
        self.fields
            .get(limit_idx)
            .and_then(|value| (value > 0).then_some(value as usize))
    }

    #[must_use]
    fn sort_self_scan_relid(&self, key_count: usize) -> u32 {
        let relid_idx = Self::SORT_KEYS + key_count * SORT_KEY_INTS + 1;
        self.fields
            .get(relid_idx)
            .map_or(0, |value| if value > 0 { value as u32 } else { 0 })
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
        if has_group_key == 0 || !self.fields.contains_range(group_key_base + 1, 4) {
            return (None, 0);
        }

        let mut group_key = self.fields.cursor_at(group_key_base + 1);
        let attno = group_key.read_int();
        let type_oid = group_key.read_oid();
        let key_type = group_key.read_int();
        let tlist_pos = group_key.read_int();

        if attno > 0 && matches!(key_type, 0 | 1 | 2 | 4 | 5 | H3_LATLNG_GROUP_KEY_TYPE) {
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
    fn agg_self_scan_relid_and_partial(&self, agg_count: usize) -> (u32, Option<PartialAggSpec>) {
        let relid_idx = self.agg_self_scan_relid_index(agg_count);
        let relid = self
            .fields
            .get(relid_idx)
            .map_or(0, |value| value as pg_sys::Index);
        let partial_idx = relid_idx + 1;
        let partial = if self.fields.int_at(partial_idx) == PARTIAL_SENTINEL {
            deserialize_partial_spec_from_reader(self.fields, partial_idx + 1)
        } else {
            None
        };
        (relid, partial)
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
        group_key_base + 5
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
    fn hash_join_info(&self) -> (i32, i32, bool) {
        if self.fields.contains_range(Self::HASH_INNER_ATTNO, 2) {
            (
                self.fields.int_at(Self::HASH_INNER_ATTNO),
                self.fields.int_at(Self::HASH_KEY_TYPE),
                self.fields
                    .get(Self::HASH_COUNT_ONLY)
                    .is_some_and(|value| value != 0),
            )
        } else {
            (0, 0, false)
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

/// Deserialize strategy, batch size, accel context, and sort keys from
/// `custom_private`.
///
/// Layout: `[strategy, batch_size, expected_threads, fn_oid, target_attno,
///   accel_strategy, num_sort_keys?, attno1, sort_op1, collation1,
///   nulls_first1, ...]`
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
    let plan_private = PlanPrivate::decode_reader(&fields)
        .unwrap_or_else(|err| pgrx::error!("pg_accel: invalid CustomScan private data: {err:?}"));
    let payload = PlanPayloadReader::new(&fields);
    let gpu_strategy = plan_private.gpu_strategy;
    let batch_size = plan_private.batch_size;
    let _expected_threads = plan_private.expected_threads;
    let fn_oid = plan_private.fn_oid;
    let target_attno = plan_private.target_attno;
    let accel_strategy = plan_private.accel_strategy;

    let sort_key_count = if matches!(gpu_strategy, GpuStrategy::Sort) {
        payload.sort_key_count()
    } else {
        0
    };
    let sort_keys = if matches!(gpu_strategy, GpuStrategy::Sort) {
        payload.sort_keys(sort_key_count)
    } else {
        vec![]
    };

    // For Sort strategy, read optional limit after sort keys.
    // Layout: [...sort keys..., limit_tuples, self_scan_relid]
    let sort_limit = if matches!(gpu_strategy, GpuStrategy::Sort) {
        payload.sort_limit(sort_key_count)
    } else {
        None
    };

    // For Sort strategy, read self_scan_relid for VectorizedScan.
    // It's one position after limit_tuples in the plan's custom_private.
    let sort_self_scan_relid = if matches!(gpu_strategy, GpuStrategy::Sort) {
        payload.sort_self_scan_relid(sort_key_count)
    } else {
        0
    };

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
    //    gk_attno, gk_type_oid, gk_key_type, gk_tlist_pos, self_scan_relid]
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

    // For Agg strategy, find `self_scan_relid` (immediately follows the
    // group-key block) and optionally a PartialAggSpec sentinel block.
    //
    // Layout (Agg):
    //   [..., num_aggs, (op,attno,rtype)*N,
    //    has_gk, (gk_attno, gk_type_oid, gk_key_type, gk_tlist_pos)?,
    //    self_scan_relid,
    //    (PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols)?]
    //
    // The partial block is optional: non-parallel plans omit it entirely.
    let (self_scan_relid, partial) = if matches!(gpu_strategy, GpuStrategy::Agg) {
        payload.agg_self_scan_relid_and_partial(agg_count)
    } else if matches!(gpu_strategy, GpuStrategy::Sort) {
        (sort_self_scan_relid, None)
    } else {
        (0, None)
    };

    CustomPrivateData {
        gpu_strategy,
        batch_size,
        fn_oid,
        target_attno,
        accel_strategy,
        sort_keys,
        sort_limit,
        agg_columns,
        group_key,
        group_key_tlist_pos,
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
        self_scan_relid,
        partial,
    }
}

// ---------------------------------------------------------------------------
// PartialAggSpec serialization / deserialization
// ---------------------------------------------------------------------------

/// Magic marker preceding a serialized [`PartialAggSpec`] in `custom_private`.
/// Chosen to be distinct from any plausible scalar field so mistaken layouts
/// don't silently deserialize as partial-agg metadata.
pub(in crate::engine::ffi) const PARTIAL_SENTINEL: c_int = 0x5041_4147; // b"PAAG"

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
    if let Some(spec) = partial {
        // SAFETY: this function is called while planning in a valid PG memory context.
        unsafe { append_partial_spec(writer.into_list(), spec) }
    } else {
        writer.into_list()
    }
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

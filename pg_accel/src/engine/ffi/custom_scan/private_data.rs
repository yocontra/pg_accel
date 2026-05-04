//! `custom_private` serialization / deserialization.
//!
//! Plan metadata travels through PG as a `List *` of `Integer` nodes so it
//! survives plan copying and EXPLAIN output. Field order is load-bearing.

use std::ffi::c_int;

use pgrx::pg_sys;

use super::{GpuStrategy, list_int_at};
use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
use crate::engine::executor::agg::{AggOp, GroupKeyInfo};
use crate::engine::executor::preagg::{DimFilter, GroupKeyDesc, JoinDepthDesc, PreAggColDesc};
use crate::engine::executor::sort::{SORT_KEY_INTS, SortKeyDesc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};
use crate::engine::gucs;
use crate::engine::registry::AccelStrategy;
use crate::gpu::PgaccelKeyType;

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

/// Deserialize strategy, batch size, accel context, and sort keys from
/// `custom_private`.
///
/// Layout: `[strategy, batch_size, expected_threads, fn_oid, target_attno,
///   accel_strategy, num_sort_keys?, attno1, sort_op1, collation1,
///   nulls_first1, ...]`
///
/// Falls back to GUC defaults when `custom_private` is null or malformed.
///
/// # Safety
///
/// `custom_private` must be null or a valid PG `List`.
#[allow(clippy::too_many_lines)]
pub(super) unsafe fn deserialize_custom_private(
    custom_private: *mut pg_sys::List,
) -> CustomPrivateData {
    if custom_private.is_null() {
        return CustomPrivateData {
            gpu_strategy: GpuStrategy::Scan,
            batch_size: gucs::min_batch_size().max(1),
            fn_oid: pg_sys::Oid::INVALID,
            target_attno: 0,
            accel_strategy: AccelStrategy::GpuSpatial,
            sort_keys: vec![],
            sort_limit: None,
            agg_columns: vec![],
            group_key: None,
            group_key_tlist_pos: 0,
            hash_inner_attno: 0,
            hash_key_type: 0,
            window_specs: vec![],
            window_scan_relid: 0,
            self_scan_relid: 0,
            partial: None,
        };
    }

    // SAFETY: custom_private is a valid List of Integer nodes.
    let strategy_raw = unsafe { list_int_at(custom_private, 0) };
    let batch_size = unsafe { list_int_at(custom_private, 1) };
    let gpu_strategy = GpuStrategy::from_i32(strategy_raw);

    let batch_size = if batch_size > 0 {
        batch_size
    } else {
        gucs::min_batch_size().max(1)
    };

    // SAFETY: custom_private was populated by PlanCustomPath with valid integer nodes
    // at indices 3, 4, 5.
    let fn_oid_raw = unsafe { list_int_at(custom_private, 3) } as u32;
    let fn_oid = pg_sys::Oid::from(fn_oid_raw);
    let target_attno = unsafe { list_int_at(custom_private, 4) };
    let accel_strategy_raw = unsafe { list_int_at(custom_private, 5) };
    let accel_strategy = AccelStrategy::from_i32(accel_strategy_raw);

    // For Sort strategy, read sort key descriptors starting at index 6.
    let mut sort_keys = vec![];
    if matches!(gpu_strategy, GpuStrategy::Sort) {
        // SAFETY: custom_private is a valid List (checked non-null above);
        // list_int_at and list_length handle bounds safely.
        let num_keys = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7; // first sort key starts at index 7

        for k in 0..num_keys {
            let offset = base + k * SORT_KEY_INTS;
            if offset + SORT_KEY_INTS > list_len {
                break;
            }
            // SAFETY: Indices are within bounds (checked above).
            let attno = unsafe { list_int_at(custom_private, offset as c_int) } as i16;
            let sort_op_raw = unsafe { list_int_at(custom_private, (offset + 1) as c_int) } as u32;
            let collation_raw =
                unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
            let nulls_first = unsafe { list_int_at(custom_private, (offset + 3) as c_int) } != 0;

            sort_keys.push(SortKeyDesc {
                attno,
                sort_op: pg_sys::Oid::from(sort_op_raw),
                collation: pg_sys::Oid::from(collation_raw),
                nulls_first,
            });
        }
    }

    // For Sort strategy, read optional limit after sort keys.
    // Layout: [...sort keys..., limit_tuples, self_scan_relid]
    let sort_limit = if matches!(gpu_strategy, GpuStrategy::Sort) {
        // SAFETY: custom_private is a valid List.
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let num_keys = unsafe { list_int_at(custom_private, 6) } as usize;
        let limit_idx = 7 + num_keys * SORT_KEY_INTS;
        if limit_idx < list_len {
            // SAFETY: Index is within bounds (checked above).
            let v = unsafe { list_int_at(custom_private, limit_idx as c_int) };
            if v > 0 { Some(v as usize) } else { None }
        } else {
            None
        }
    } else {
        None
    };

    // For Sort strategy, read self_scan_relid for VectorizedScan.
    // It's one position after limit_tuples in the plan's custom_private.
    let sort_self_scan_relid = if matches!(gpu_strategy, GpuStrategy::Sort) {
        // SAFETY: custom_private is a valid List.
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let num_keys = unsafe { list_int_at(custom_private, 6) } as usize;
        let relid_idx = 7 + num_keys * SORT_KEY_INTS + 1; // +1 past limit
        if relid_idx < list_len {
            // SAFETY: Index is within bounds (checked above).
            let v = unsafe { list_int_at(custom_private, relid_idx as c_int) };
            if v > 0 { v as u32 } else { 0 }
        } else {
            0
        }
    } else {
        0
    };

    // For Agg strategy, read aggregate column descriptors starting at index 6.
    // Layout: [num_aggs, op0, attno0, rtype0, op1, attno1, rtype1, ...]
    let mut agg_columns = vec![];
    if matches!(gpu_strategy, GpuStrategy::Agg) {
        // SAFETY: custom_private is a valid List; list_int_at handles bounds.
        let num_aggs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7;
        for k in 0..num_aggs {
            let offset = base + k * 3;
            if offset + 3 > list_len {
                break;
            }
            let op = AggOp::from_i32(unsafe { list_int_at(custom_private, offset as c_int) });
            let attno = unsafe { list_int_at(custom_private, (offset + 1) as c_int) };
            let rtype = unsafe { list_int_at(custom_private, (offset + 2) as c_int) } as u32;
            agg_columns.push((op, attno, rtype));
        }
    }

    // For Agg strategy, read optional group key info after agg descriptors.
    // Layout: [...agg descs..., has_group_key, gk_attno, gk_type_oid,
    //   gk_key_type, gk2_attno, gk_tlist_pos]
    let (group_key, group_key_tlist_pos) = if matches!(gpu_strategy, GpuStrategy::Agg) {
        let num_aggs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let gk_base = 7 + num_aggs * 3;
        if gk_base < list_len {
            // SAFETY: Index is within bounds (checked above).
            let has_gk = unsafe { list_int_at(custom_private, gk_base as c_int) };
            if has_gk != 0 && gk_base + 3 < list_len {
                let gk_attno = unsafe { list_int_at(custom_private, (gk_base + 1) as c_int) };
                let gk_type_oid =
                    pg_sys::Oid::from(
                        unsafe { list_int_at(custom_private, (gk_base + 2) as c_int) } as u32,
                    );
                let gk_key_type = unsafe { list_int_at(custom_private, (gk_base + 3) as c_int) };
                // gk_base + 4 = group_key2_attno, gk_base + 5 = gk_tlist_pos
                let tlist_pos = if gk_base + 5 < list_len {
                    unsafe { list_int_at(custom_private, (gk_base + 5) as c_int) }
                } else {
                    0 // default: group key at position 0
                };
                (
                    Some(GroupKeyInfo {
                        attno: gk_attno,
                        type_oid: gk_type_oid,
                        key_type: gk_key_type,
                    }),
                    tlist_pos,
                )
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        }
    } else {
        (None, 0)
    };

    // For Window strategy, read window function specs starting at index 6.
    // Layout: [num_specs, func0, part_attno0, order_attno0, value_attno0,
    //   offset0, default_bits0, result_type0, ...]
    let mut window_specs = vec![];
    if matches!(gpu_strategy, GpuStrategy::Window) {
        // SAFETY: custom_private is a valid List; list_int_at handles bounds.
        let num_specs = unsafe { list_int_at(custom_private, 6) } as usize;
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        let base = 7;
        for k in 0..num_specs {
            let offset = base + k * WINDOW_SPEC_INTS;
            if offset + WINDOW_SPEC_INTS > list_len {
                break;
            }
            // SAFETY: Indices are within bounds (checked above).
            let func_raw = unsafe { list_int_at(custom_private, offset as c_int) };
            let Some(func) = WindowFunc::from_i32(func_raw) else {
                break;
            };
            let part_attno = unsafe { list_int_at(custom_private, (offset + 1) as c_int) };
            let ord_attno = unsafe { list_int_at(custom_private, (offset + 2) as c_int) };
            let val_attno = unsafe { list_int_at(custom_private, (offset + 3) as c_int) };
            let lag_offset = unsafe { list_int_at(custom_private, (offset + 4) as c_int) };
            let default_bits = unsafe { list_int_at(custom_private, (offset + 5) as c_int) };
            let result_type = unsafe { list_int_at(custom_private, (offset + 6) as c_int) } as u32;
            let uses_fp64_raw = unsafe { list_int_at(custom_private, (offset + 7) as c_int) };
            window_specs.push(WindowFuncSpec {
                func,
                partition_attno: part_attno,
                order_attno: ord_attno,
                value_attno: val_attno,
                offset: lag_offset,
                default_val: f64::from_bits(default_bits as u64),
                result_type_oid: result_type,
                uses_fp64: uses_fp64_raw != 0,
            });
        }
    }

    // For Window strategy, read scan_relid after the specs.
    // Plan layout: [...base 6 fields..., num_specs, spec0..., scan_relid]
    let window_scan_relid: pg_sys::Index = if matches!(gpu_strategy, GpuStrategy::Window) {
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        // scan_relid is the last element: index = 7 + num_specs * WINDOW_SPEC_INTS
        let num_specs_raw = unsafe { list_int_at(custom_private, 6) } as usize;
        let relid_idx = 7 + num_specs_raw * WINDOW_SPEC_INTS;
        if relid_idx < list_len {
            // SAFETY: Index is within bounds (checked above).
            unsafe { list_int_at(custom_private, relid_idx as c_int) as pg_sys::Index }
        } else {
            0
        }
    } else {
        0
    };

    // For Join strategy with GpuHashJoin accel, read hash join info at index 6+.
    // Layout: [...base 6 fields..., inner_attno, key_type]
    let (hash_inner_attno, hash_key_type) = if accel_strategy == AccelStrategy::GpuHashJoin {
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        if list_len > 7 {
            // SAFETY: Indices 6 and 7 are within bounds (checked above).
            let inner_attno = unsafe { list_int_at(custom_private, 6) };
            let key_type = unsafe { list_int_at(custom_private, 7) };
            (inner_attno, key_type)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    // For Agg strategy, find `self_scan_relid` (immediately follows the
    // group-key block) and optionally a PartialAggSpec sentinel block.
    //
    // Layout (Agg):
    //   [..., num_aggs, (op,attno,rtype)*N,
    //    has_gk, (gk_attno, gk_type_oid, gk_key_type)?,
    //    self_scan_relid,
    //    (PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols)?]
    //
    // The partial block is optional: non-parallel plans omit it entirely.
    let (self_scan_relid, partial) = if matches!(gpu_strategy, GpuStrategy::Agg) {
        let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
        // The self_scan_relid index is derived from group-key positioning.
        let num_aggs = unsafe { list_int_at(custom_private, 6) } as usize;
        let gk_base = 7 + num_aggs * 3;
        let relid_idx = if gk_base < list_len {
            let has_gk = unsafe { list_int_at(custom_private, gk_base as c_int) };
            if has_gk != 0 {
                // has_gk + 3 payload ints + 1 (relid slot)
                gk_base + 4
            } else {
                gk_base + 1
            }
        } else {
            // Defensive: fall back to prior layout (second-to-last int).
            list_len.saturating_sub(1)
        };
        let relid = if relid_idx < list_len {
            (unsafe { list_int_at(custom_private, relid_idx as c_int) }) as pg_sys::Index
        } else {
            0
        };
        // Partial sentinel starts at relid_idx + 1 (if present).
        let partial_idx = relid_idx + 1;
        let partial_spec = if partial_idx < list_len {
            let sentinel = unsafe { list_int_at(custom_private, partial_idx as c_int) };
            if sentinel == PARTIAL_SENTINEL {
                // SAFETY: indices bounds-checked via partial_idx + needed offsets.
                unsafe { deserialize_partial_spec(custom_private, partial_idx + 1) }
            } else {
                None
            }
        } else {
            None
        };
        (relid, partial_spec)
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
        group_key_tlist_pos: group_key_tlist_pos as usize,
        hash_inner_attno,
        hash_key_type,
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
    mut list: *mut pg_sys::List,
    spec: &PartialAggSpec,
) -> *mut pg_sys::List {
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(list, pg_sys::makeInteger(PARTIAL_SENTINEL).cast());
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(spec.per_column.len() as c_int).cast(),
        );
        for col in &spec.per_column {
            list = pg_sys::lappend(list, pg_sys::makeInteger(col.op.to_i32()).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(col.attno).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(col.transtype_oid) as c_int).cast(),
            );
            let ser_oid = col
                .serialize_fn_oid
                .map_or(0_i32, |o| u32::from(o) as c_int);
            list = pg_sys::lappend(list, pg_sys::makeInteger(ser_oid).cast());
        }
    }
    list
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
    // SAFETY: list_length safe on the null-guarded list from caller.
    let list_len = unsafe { pg_sys::list_length(list) } as usize;
    if start_idx >= list_len {
        return None;
    }
    let n_cols = unsafe { list_int_at(list, start_idx as c_int) } as usize;
    if n_cols == 0 {
        return Some(PartialAggSpec {
            per_column: Vec::new(),
        });
    }
    let base = start_idx + 1;
    if base + n_cols * 4 > list_len {
        return None;
    }
    let mut per_column = Vec::with_capacity(n_cols);
    for k in 0..n_cols {
        let off = base + k * 4;
        let op = AggOp::from_i32(unsafe { list_int_at(list, off as c_int) });
        let attno = unsafe { list_int_at(list, (off + 1) as c_int) };
        let transtype_raw = unsafe { list_int_at(list, (off + 2) as c_int) } as u32;
        let ser_raw = unsafe { list_int_at(list, (off + 3) as c_int) } as u32;
        let serialize_fn_oid = if ser_raw == 0 {
            None
        } else {
            Some(pg_sys::Oid::from(ser_raw))
        };
        per_column.push(PartialColumn {
            op,
            attno,
            transtype_oid: pg_sys::Oid::from(transtype_raw),
            serialize_fn_oid,
        });
    }
    Some(PartialAggSpec { per_column })
}

// ---------------------------------------------------------------------------
// PreAgg serialization / deserialization
// ---------------------------------------------------------------------------

/// Deserialized PreAgg configuration from `custom_private`.
pub(super) struct PreAggPrivData {
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
///  // Optional partial-agg sentinel block (worker-side parallel preagg):
///  (PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols)?
/// ]
/// ```
///
/// `partial` carries the `PartialAggSpec` for parallel partial-emit paths
/// (workers emit transition-state tuples for PG's Finalize Agg). `None`
/// (the serial path) skips the sentinel block entirely so existing
/// non-parallel plans don't change shape.
///
/// # Safety
///
/// Must be called during planning on the main backend thread.
#[allow(clippy::cast_possible_wrap, clippy::too_many_lines)]
#[must_use]
pub unsafe fn serialize_preagg_private(
    scan_relid: pg_sys::Index,
    scan_oid: pg_sys::Oid,
    depths: &[JoinDepthDesc],
    agg_descs: &[PreAggColDesc],
    group_keys: &[GroupKeyDesc],
    scan_expr: Option<&crate::engine::expr_compiler::CompiledExpr>,
    partial: Option<&PartialAggSpec>,
) -> *mut pg_sys::List {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    let batch_size = gucs::min_batch_size();
    let expected_threads = super::explain::resolve_thread_count();

    let mut list: *mut pg_sys::List = std::ptr::null_mut();
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(GpuStrategy::PreAgg as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(batch_size).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(expected_threads).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(scan_relid as c_int).cast());
        // Stable OID: stored as c_int (Oid is u32 — bit-cast via `as c_int`).
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(u32::from(scan_oid) as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(depths.len() as c_int).cast());

        // Per depth.
        for depth in depths {
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.outer_attno).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.inner_attno).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(depth.key_type).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(depth.dim_filters.len() as c_int).cast(),
            );
            for filt in &depth.dim_filters {
                list = pg_sys::lappend(list, pg_sys::makeInteger(filt.col_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(filt.cmp_opcode as c_int).cast());
                // Encode f64 as two i32s (hi and lo bits).
                let bits = filt.const_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(bits as u32 as c_int).cast());
            }
            // Group col attnos for this depth.
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(depth.group_col_attnos.len() as c_int).cast(),
            );
            for &attno in &depth.group_col_attnos {
                list = pg_sys::lappend(list, pg_sys::makeInteger(attno).cast());
            }
        }

        // Aggregates.
        list = pg_sys::lappend(list, pg_sys::makeInteger(agg_descs.len() as c_int).cast());
        for desc in agg_descs {
            list = pg_sys::lappend(list, pg_sys::makeInteger(desc.op.to_i32()).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(desc.attno).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(desc.type_oid) as c_int).cast(),
            );
        }

        // GROUP BY keys.
        list = pg_sys::lappend(list, pg_sys::makeInteger(group_keys.len() as c_int).cast());
        for gk in group_keys {
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk.source as c_int).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(gk.attno).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(gk.type_oid) as c_int).cast(),
            );
        }

        // Serialize fact-side scan expression.
        match scan_expr {
            Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                col_idx,
                cmp_opcode,
                const_val,
            })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // type=CmpConst
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp_opcode as c_int).cast());
                let bits = const_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(bits as u32 as c_int).cast());
            }
            Some(CompiledExpr::Template(TemplateKernel::Between { col_idx, lo, hi })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast()); // type=Between
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col_idx as c_int).cast());
                let lo_bits = lo.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((lo_bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(lo_bits as u32 as c_int).cast());
                let hi_bits = hi.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((hi_bits >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(hi_bits as u32 as c_int).cast());
            }
            Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                col1_idx,
                cmp1_opcode,
                const1_val,
                col2_idx,
                cmp2_opcode,
                const2_val,
            })) => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has
                list = pg_sys::lappend(list, pg_sys::makeInteger(3).cast()); // type=TwoPredAnd
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col1_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp1_opcode as c_int).cast());
                let b1 = const1_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((b1 >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(b1 as u32 as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*col2_idx as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(*cmp2_opcode as c_int).cast());
                let b2 = const2_val.to_bits();
                list = pg_sys::lappend(list, pg_sys::makeInteger((b2 >> 32) as c_int).cast());
                list = pg_sys::lappend(list, pg_sys::makeInteger(b2 as u32 as c_int).cast());
            }
            _ => {
                list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // no scan_expr
            }
        }

        // Optional partial-agg sentinel block. Mirrors the Agg-strategy
        // layout (append_partial_spec writes
        // `[PARTIAL_SENTINEL, n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols]`)
        // so deserialize_partial_spec can read it back. Absent when the
        // plan is non-parallel (the serial preagg path).
        if let Some(spec) = partial {
            list = append_partial_spec(list, spec);
        }
    }

    list
}

/// Deserialize PreAgg configuration from `custom_private`.
///
/// # Safety
///
/// `custom_private` must be a valid PG `List` of Integer nodes.
#[allow(clippy::cast_sign_loss, clippy::too_many_lines)]
pub(super) unsafe fn deserialize_preagg_private(
    custom_private: *mut pg_sys::List,
) -> PreAggPrivData {
    use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

    let empty = PreAggPrivData {
        scan_relid: 0,
        scan_oid: pg_sys::InvalidOid,
        depths: vec![],
        agg_descs: vec![],
        group_keys: vec![],
        scan_expr: None,
        partial: None,
    };
    if custom_private.is_null() {
        return empty;
    }

    let mut idx: c_int = 3; // skip [strategy, batch_size, expected_threads]

    // SAFETY: custom_private is a valid List.
    let scan_relid = unsafe { list_int_at(custom_private, idx) } as pg_sys::Index;
    idx += 1;
    let scan_oid_raw = unsafe { list_int_at(custom_private, idx) } as u32;
    idx += 1;
    let scan_oid = pg_sys::Oid::from(scan_oid_raw);
    let n_depths = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;

    let mut depths = Vec::with_capacity(n_depths);
    for _ in 0..n_depths {
        let outer_attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let inner_attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let key_type = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let n_filters = unsafe { list_int_at(custom_private, idx) } as usize;
        idx += 1;

        let mut dim_filters = Vec::with_capacity(n_filters);
        for _ in 0..n_filters {
            let col_idx = unsafe { list_int_at(custom_private, idx) } as usize;
            idx += 1;
            let cmp_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
            idx += 1;
            let bits_hi = unsafe { list_int_at(custom_private, idx) } as u32;
            idx += 1;
            let bits_lo = unsafe { list_int_at(custom_private, idx) } as u32;
            idx += 1;
            let const_val = f64::from_bits(((bits_hi as u64) << 32) | bits_lo as u64);
            dim_filters.push(DimFilter {
                col_idx,
                cmp_opcode,
                const_val,
            });
        }

        let n_group_cols = unsafe { list_int_at(custom_private, idx) } as usize;
        idx += 1;
        let mut group_col_attnos = Vec::with_capacity(n_group_cols);
        for _ in 0..n_group_cols {
            group_col_attnos.push(unsafe { list_int_at(custom_private, idx) });
            idx += 1;
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
    let n_aggs = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;
    let mut agg_descs = Vec::with_capacity(n_aggs);
    for _ in 0..n_aggs {
        let op = AggOp::from_i32(unsafe { list_int_at(custom_private, idx) });
        idx += 1;
        let attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let type_oid_raw = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        agg_descs.push(PreAggColDesc {
            op,
            attno,
            type_oid: pg_sys::Oid::from(type_oid_raw),
        });
    }

    // GROUP BY keys.
    let n_gkeys = unsafe { list_int_at(custom_private, idx) } as usize;
    idx += 1;
    let mut group_keys = Vec::with_capacity(n_gkeys);
    for _ in 0..n_gkeys {
        let source = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        let attno = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        let type_oid_raw = unsafe { list_int_at(custom_private, idx) } as u32;
        idx += 1;
        group_keys.push(GroupKeyDesc {
            source,
            attno,
            type_oid: pg_sys::Oid::from(type_oid_raw),
        });
    }

    // Deserialize scan_expr.
    let has_scan_expr = unsafe { list_int_at(custom_private, idx) };
    idx += 1;
    let scan_expr = if has_scan_expr == 1 {
        let template_type = unsafe { list_int_at(custom_private, idx) };
        idx += 1;
        match template_type {
            1 => {
                // CmpConst
                let col_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let bits_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let bits_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const_val = f64::from_bits(((bits_hi as u64) << 32) | bits_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    col_idx,
                    cmp_opcode,
                    const_val,
                }))
            }
            2 => {
                // Between
                let col_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let lo = f64::from_bits(((lo_hi as u64) << 32) | lo_lo as u64);
                let hi_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let hi_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let hi = f64::from_bits(((hi_hi as u64) << 32) | hi_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::Between {
                    col_idx,
                    lo,
                    hi,
                }))
            }
            3 => {
                // TwoPredAnd
                let col1_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp1_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let b1_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let b1_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const1_val = f64::from_bits(((b1_hi as u64) << 32) | b1_lo as u64);
                let col2_idx = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let cmp2_opcode = unsafe { list_int_at(custom_private, idx) } as u16;
                idx += 1;
                let b2_hi = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let b2_lo = unsafe { list_int_at(custom_private, idx) } as u32;
                idx += 1;
                let const2_val = f64::from_bits(((b2_hi as u64) << 32) | b2_lo as u64);
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx,
                    cmp1_opcode,
                    const1_val,
                    col2_idx,
                    cmp2_opcode,
                    const2_val,
                }))
            }
            _ => None,
        }
    } else {
        None
    };

    // Optional PARTIAL_SENTINEL block — present only when the planner
    // injected a parallel partial-emit path (preagg_partial::try_inject).
    // Mirrors the Agg-strategy decode at deserialize_custom_private:344-356.
    let list_len = unsafe { pg_sys::list_length(custom_private) } as usize;
    let partial = if (idx as usize) < list_len {
        let sentinel = unsafe { list_int_at(custom_private, idx) };
        if sentinel == PARTIAL_SENTINEL {
            // SAFETY: list bounds checked; deserialize_partial_spec consumes
            // [n_cols, (op,attno,transtype_oid,serialize_fn_oid)*n_cols].
            unsafe { deserialize_partial_spec(custom_private, (idx as usize) + 1) }
        } else {
            None
        }
    } else {
        None
    };

    // Suppress unused-assignment warning for idx.
    let _ = idx;

    PreAggPrivData {
        scan_relid,
        scan_oid,
        depths,
        agg_descs,
        group_keys,
        scan_expr,
        partial,
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
    mut list: *mut pg_sys::List,
    priv_data: &FunctionScanPrivData,
) -> *mut pg_sys::List {
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(list, pg_sys::makeInteger(FUNCTIONSCAN_SENTINEL).cast());
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(u32::from(priv_data.fn_oid) as c_int).cast(),
        );
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.output_shape_disc).cast(),
        );
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.output_shape_field_count as c_int).cast(),
        );
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.args.len() as c_int).cast(),
        );
        for &(datum, type_oid) in &priv_data.args {
            // Split the i64 datum into hi / lo halves.
            let datum_u = datum as u64;
            list = pg_sys::lappend(list, pg_sys::makeInteger((datum_u >> 32) as c_int).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(datum_u as u32 as c_int).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(type_oid as c_int).cast());
        }
    }
    list
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
    // SAFETY: list_length is safe on a non-null List.
    let list_len = unsafe { pg_sys::list_length(list) } as usize;
    if start_idx + 4 >= list_len {
        return None;
    }
    let sentinel = unsafe { list_int_at(list, start_idx as c_int) };
    if sentinel != FUNCTIONSCAN_SENTINEL {
        return None;
    }
    let fn_oid_raw = unsafe { list_int_at(list, (start_idx + 1) as c_int) } as u32;
    let shape_disc = unsafe { list_int_at(list, (start_idx + 2) as c_int) };
    let shape_field_count = unsafe { list_int_at(list, (start_idx + 3) as c_int) } as u32;
    let n_args = unsafe { list_int_at(list, (start_idx + 4) as c_int) } as usize;
    let payload_base = start_idx + 5;
    if payload_base + n_args * 3 > list_len {
        return None;
    }
    let mut args = Vec::with_capacity(n_args);
    for k in 0..n_args {
        let off = payload_base + k * 3;
        let hi = unsafe { list_int_at(list, off as c_int) } as u32;
        let lo = unsafe { list_int_at(list, (off + 1) as c_int) } as u32;
        let datum = (((hi as u64) << 32) | lo as u64) as i64;
        let type_oid = unsafe { list_int_at(list, (off + 2) as c_int) } as u32;
        args.push((datum, type_oid));
    }
    Some(FunctionScanPrivData {
        fn_oid: pg_sys::Oid::from(fn_oid_raw),
        output_shape_disc: shape_disc,
        output_shape_field_count: shape_field_count,
        args,
    })
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod functionscan_tests {
    use pgrx::pg_test;

    use super::*;

    /// Sentinel must be distinct from `PARTIAL_SENTINEL` so the two
    /// optional-trailer blocks cannot be silently confused if a layout
    /// regression mis-positions the cursor.
    #[test]
    fn functionscan_sentinel_distinct_from_partial() {
        assert_ne!(FUNCTIONSCAN_SENTINEL, PARTIAL_SENTINEL);
    }

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
    mut list: *mut pg_sys::List,
    priv_data: &SrfTargetListPrivData,
) -> *mut pg_sys::List {
    // SAFETY: makeInteger + lappend allocate in CurrentMemoryContext.
    unsafe {
        list = pg_sys::lappend(list, pg_sys::makeInteger(SRF_TARGET_LIST_SENTINEL).cast());
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(u32::from(priv_data.fn_oid) as c_int).cast(),
        );
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.output_shape_disc).cast(),
        );
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.output_shape_field_count as c_int).cast(),
        );
        list = pg_sys::lappend(list, pg_sys::makeInteger(priv_data.srf_arg_attno).cast());
        list = pg_sys::lappend(list, pg_sys::makeInteger(priv_data.srf_tlist_pos).cast());
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.passthrough_attnos.len() as c_int).cast(),
        );
        for &attno in &priv_data.passthrough_attnos {
            list = pg_sys::lappend(list, pg_sys::makeInteger(attno).cast());
        }
        list = pg_sys::lappend(
            list,
            pg_sys::makeInteger(priv_data.qual_args.len() as c_int).cast(),
        );
        for &(datum, type_oid) in &priv_data.qual_args {
            let datum_u = datum as u64;
            list = pg_sys::lappend(list, pg_sys::makeInteger((datum_u >> 32) as c_int).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(datum_u as u32 as c_int).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(type_oid as c_int).cast());
        }
    }
    list
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
    // SAFETY: list_length is safe on a non-null List.
    let list_len = unsafe { pg_sys::list_length(list) } as usize;
    // Need at least sentinel + 6 fixed fields + n_passthrough(0) + n_qual_args(0)
    if start_idx + 6 >= list_len {
        return None;
    }
    let sentinel = unsafe { list_int_at(list, start_idx as c_int) };
    if sentinel != SRF_TARGET_LIST_SENTINEL {
        return None;
    }
    let fn_oid_raw = unsafe { list_int_at(list, (start_idx + 1) as c_int) } as u32;
    let shape_disc = unsafe { list_int_at(list, (start_idx + 2) as c_int) };
    let shape_field_count = unsafe { list_int_at(list, (start_idx + 3) as c_int) } as u32;
    let srf_arg_attno = unsafe { list_int_at(list, (start_idx + 4) as c_int) };
    let srf_tlist_pos = unsafe { list_int_at(list, (start_idx + 5) as c_int) };
    let n_passthrough = unsafe { list_int_at(list, (start_idx + 6) as c_int) } as usize;
    let pass_base = start_idx + 7;
    if pass_base + n_passthrough >= list_len {
        return None;
    }
    let mut passthrough_attnos = Vec::with_capacity(n_passthrough);
    for k in 0..n_passthrough {
        passthrough_attnos.push(unsafe { list_int_at(list, (pass_base + k) as c_int) });
    }
    let n_args_idx = pass_base + n_passthrough;
    if n_args_idx >= list_len {
        return None;
    }
    let n_qual_args = unsafe { list_int_at(list, n_args_idx as c_int) } as usize;
    let qual_base = n_args_idx + 1;
    if qual_base + n_qual_args * 3 > list_len {
        return None;
    }
    let mut qual_args = Vec::with_capacity(n_qual_args);
    for k in 0..n_qual_args {
        let off = qual_base + k * 3;
        let hi = unsafe { list_int_at(list, off as c_int) } as u32;
        let lo = unsafe { list_int_at(list, (off + 1) as c_int) } as u32;
        let datum = (((hi as u64) << 32) | lo as u64) as i64;
        let type_oid = unsafe { list_int_at(list, (off + 2) as c_int) } as u32;
        qual_args.push((datum, type_oid));
    }
    Some(SrfTargetListPrivData {
        fn_oid: pg_sys::Oid::from(fn_oid_raw),
        output_shape_disc: shape_disc,
        output_shape_field_count: shape_field_count,
        srf_arg_attno,
        srf_tlist_pos,
        passthrough_attnos,
        qual_args,
    })
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod srf_target_list_tests {
    use pgrx::pg_test;

    use super::*;

    /// Sentinel must be distinct from `FUNCTIONSCAN_SENTINEL` and
    /// `PARTIAL_SENTINEL` so the three block formats are unambiguous.
    #[test]
    fn srf_target_list_sentinel_distinct() {
        assert_ne!(SRF_TARGET_LIST_SENTINEL, FUNCTIONSCAN_SENTINEL);
        assert_ne!(SRF_TARGET_LIST_SENTINEL, PARTIAL_SENTINEL);
    }

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

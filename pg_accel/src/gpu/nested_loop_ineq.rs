//! Safe Rust wrappers for the GPU NestedLoop scalar-inequality kernel.
//!
//! Mirrors the C API in `pgaccel-kernels/include/pgaccel_nested_loop_ineq.h`.
//! The kernel is the implementation that backs the launchpad described in
//! `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` —
//! `observe_nestloop_scalar_opportunity` (the planner observability path
//! that increments `planner_rejected("nestloop_scalar_no_gpu_kernel", ...)`).
//!
//! ## Contract
//!
//! - Both key arrays MUST be free of SQL NULL rows before they reach this
//!   layer. PG INNER-join semantics exclude any row where either side is
//!   NULL; the executor strips them upstream and the kernel sees only
//!   valid scalars.
//! - The output slice is `[outer_idx, inner_idx, outer_idx, inner_idx, ...]`
//!   `u32` pairs. The kernel emits up to `max_pairs` pairs.
//! - The kernel returns the TOTAL number of matches it observed via
//!   `*pair_count_out`. If `pair_count_out > max_pairs`, the result is
//!   an overflow and the caller MUST reject the GPU path (no silent
//!   truncation).
//!
//! Anti-cheat note: this module deliberately surfaces overflow as
//! `NljDispatchResult::Overflow` rather than silently truncating —
//! per CLAUDE.md ban #4 ("No silent error swallowing on GPU paths").

use super::{GpuErrorDomain, GpuOperation, GpuResult, bridge, status_to_result};

/// Inequality opcode mirroring `pgaccel_nlj_ineq_op` in the C header.
#[allow(dead_code)]
// reason: kernel + bridge landed ahead of the executor node that consumes them;
// see join_pathlist.rs::selected_gpu_nlj_kernel_available for the gate.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NljIneqOp {
    /// `outer < inner`
    Lt = 0,
    /// `outer <= inner`
    Le = 1,
    /// `outer >= inner`
    Ge = 2,
    /// `outer > inner`
    Gt = 3,
}

#[allow(dead_code)] // reason: see NljIneqOp.
impl NljIneqOp {
    /// Pure Rust oracle for testing — evaluate the predicate on host values.
    #[must_use]
    pub const fn eval_i64(self, a: i64, b: i64) -> bool {
        match self {
            Self::Lt => a < b,
            Self::Le => a <= b,
            Self::Ge => a >= b,
            Self::Gt => a > b,
        }
    }

    /// Pure Rust oracle for testing — evaluate the predicate on host values.
    #[must_use]
    pub fn eval_f64(self, a: f64, b: f64) -> bool {
        match self {
            Self::Lt => a < b,
            Self::Le => a <= b,
            Self::Ge => a >= b,
            Self::Gt => a > b,
        }
    }
}

/// A matched index pair produced by the NLJ kernel.
#[allow(dead_code)] // reason: kernel + bridge landed ahead of the executor; see NljIneqOp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NljPair {
    /// Index into the outer key array.
    pub outer: u32,
    /// Index into the inner key array.
    pub inner: u32,
}

/// Result of an NLJ dispatch — either the emitted pair list, or an explicit
/// `Overflow` signal so the planner / executor can decline to PG native.
#[allow(dead_code)] // reason: kernel + bridge landed ahead of the executor; see NljIneqOp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NljDispatchResult {
    /// All matches fit inside the caller-provided `max_pairs` cap.
    Pairs(Vec<NljPair>),
    /// Kernel observed more matches than `max_pairs`. Holds the true total
    /// so cost/telemetry can record the actual selectivity. The pair list
    /// is intentionally NOT exposed — partial output is unsafe.
    Overflow {
        /// True total number of matches the kernel counted.
        observed: usize,
        /// The cap the caller passed in.
        cap: usize,
    },
}

#[allow(dead_code)] // reason: helper for the four dispatch_* functions below;
// gated alongside them.
fn build_pairs(buf: &[u32], pair_count: usize) -> Vec<NljPair> {
    let mut out = Vec::with_capacity(pair_count);
    for chunk in buf[..pair_count * 2].chunks_exact(2) {
        out.push(NljPair {
            outer: chunk[0],
            inner: chunk[1],
        });
    }
    out
}

#[allow(dead_code)] // reason: kernel name tags used by the four dispatch_* fns.
const NLJ_KERNEL_INEQ_I64: &str = "nlj_ineq_i64";
#[allow(dead_code)] // reason: kernel name tag.
const NLJ_KERNEL_INEQ_F64: &str = "nlj_ineq_f64";
#[allow(dead_code)] // reason: kernel name tag.
const NLJ_KERNEL_BETWEEN_I64: &str = "nlj_between_i64";
#[allow(dead_code)] // reason: kernel name tag.
const NLJ_KERNEL_BETWEEN_F64: &str = "nlj_between_f64";

/// Drive the GPU NLJ inequality kernel for `i64` keys.
///
/// # Errors
///
/// Returns `GpuError` on FFI failure. Returns `NljDispatchResult::Overflow`
/// (success) when the kernel observed more matches than `max_pairs`.
#[allow(dead_code)] // reason: see NljIneqOp; executor wiring pending.
pub fn dispatch_ineq_i64(
    outer_keys: &[i64],
    inner_keys: &[i64],
    op: NljIneqOp,
    max_pairs: usize,
) -> GpuResult<NljDispatchResult> {
    if outer_keys.is_empty() || inner_keys.is_empty() {
        return Ok(NljDispatchResult::Pairs(Vec::new()));
    }
    let mut buf = vec![0u32; max_pairs.saturating_mul(2).max(2)];
    let mut pair_count: usize = 0;

    // SAFETY: all pointers are derived from non-empty slices; max_pairs and
    // count buffers point to valid stack/heap memory.
    let status = unsafe {
        bridge::pgaccel_nlj_ineq_i64(
            outer_keys.as_ptr(),
            outer_keys.len(),
            inner_keys.as_ptr(),
            inner_keys.len(),
            op as i32,
            buf.as_mut_ptr(),
            max_pairs,
            std::ptr::addr_of_mut!(pair_count),
        )
    };
    status_to_result(
        status,
        GpuErrorDomain::HashJoin,
        GpuOperation::Kernel(NLJ_KERNEL_INEQ_I64),
    )?;

    if pair_count > max_pairs {
        return Ok(NljDispatchResult::Overflow {
            observed: pair_count,
            cap: max_pairs,
        });
    }
    Ok(NljDispatchResult::Pairs(build_pairs(&buf, pair_count)))
}

/// Drive the GPU NLJ inequality kernel for `f64` keys.
///
/// # Errors
///
/// Returns `GpuError` on FFI failure. Returns `NljDispatchResult::Overflow`
/// (success) when the kernel observed more matches than `max_pairs`.
#[allow(dead_code)] // reason: see NljIneqOp; executor wiring pending.
pub fn dispatch_ineq_f64(
    outer_keys: &[f64],
    inner_keys: &[f64],
    op: NljIneqOp,
    max_pairs: usize,
) -> GpuResult<NljDispatchResult> {
    if outer_keys.is_empty() || inner_keys.is_empty() {
        return Ok(NljDispatchResult::Pairs(Vec::new()));
    }
    let mut buf = vec![0u32; max_pairs.saturating_mul(2).max(2)];
    let mut pair_count: usize = 0;

    // SAFETY: see dispatch_ineq_i64; identical pointer derivation.
    let status = unsafe {
        bridge::pgaccel_nlj_ineq_f64(
            outer_keys.as_ptr(),
            outer_keys.len(),
            inner_keys.as_ptr(),
            inner_keys.len(),
            op as i32,
            buf.as_mut_ptr(),
            max_pairs,
            std::ptr::addr_of_mut!(pair_count),
        )
    };
    status_to_result(
        status,
        GpuErrorDomain::HashJoin,
        GpuOperation::Kernel(NLJ_KERNEL_INEQ_F64),
    )?;

    if pair_count > max_pairs {
        return Ok(NljDispatchResult::Overflow {
            observed: pair_count,
            cap: max_pairs,
        });
    }
    Ok(NljDispatchResult::Pairs(build_pairs(&buf, pair_count)))
}

/// Drive the GPU NLJ BETWEEN-shape kernel for `i64` keys.
/// Predicate: `inner_lo[j] <= outer[i] <= inner_hi[j]`.
///
/// `inner_lo` and `inner_hi` must have the same length.
///
/// # Errors
///
/// Returns `GpuError` on FFI failure or on mismatched lo/hi lengths.
/// Returns `NljDispatchResult::Overflow` (success) when the kernel
/// observed more matches than `max_pairs`.
#[allow(dead_code)] // reason: see NljIneqOp; executor wiring pending.
pub fn dispatch_between_i64(
    outer_keys: &[i64],
    inner_lo: &[i64],
    inner_hi: &[i64],
    max_pairs: usize,
) -> GpuResult<NljDispatchResult> {
    if inner_lo.len() != inner_hi.len() {
        return Err(super::GpuError::with_detail(
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel(NLJ_KERNEL_BETWEEN_I64),
            super::GpuStatusDetail::ShapeMismatch,
            "inner_lo/inner_hi length mismatch",
        ));
    }
    if outer_keys.is_empty() || inner_lo.is_empty() {
        return Ok(NljDispatchResult::Pairs(Vec::new()));
    }
    let mut buf = vec![0u32; max_pairs.saturating_mul(2).max(2)];
    let mut pair_count: usize = 0;

    // SAFETY: identical pointer derivation as dispatch_ineq_i64.
    let status = unsafe {
        bridge::pgaccel_nlj_between_i64(
            outer_keys.as_ptr(),
            outer_keys.len(),
            inner_lo.as_ptr(),
            inner_hi.as_ptr(),
            inner_lo.len(),
            buf.as_mut_ptr(),
            max_pairs,
            std::ptr::addr_of_mut!(pair_count),
        )
    };
    status_to_result(
        status,
        GpuErrorDomain::HashJoin,
        GpuOperation::Kernel(NLJ_KERNEL_BETWEEN_I64),
    )?;

    if pair_count > max_pairs {
        return Ok(NljDispatchResult::Overflow {
            observed: pair_count,
            cap: max_pairs,
        });
    }
    Ok(NljDispatchResult::Pairs(build_pairs(&buf, pair_count)))
}

/// Drive the GPU NLJ BETWEEN-shape kernel for `f64` keys.
///
/// # Errors
///
/// Returns `GpuError` on FFI failure or on mismatched lo/hi lengths.
/// Returns `NljDispatchResult::Overflow` (success) when the kernel
/// observed more matches than `max_pairs`.
#[allow(dead_code)] // reason: see NljIneqOp; executor wiring pending.
pub fn dispatch_between_f64(
    outer_keys: &[f64],
    inner_lo: &[f64],
    inner_hi: &[f64],
    max_pairs: usize,
) -> GpuResult<NljDispatchResult> {
    if inner_lo.len() != inner_hi.len() {
        return Err(super::GpuError::with_detail(
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel(NLJ_KERNEL_BETWEEN_F64),
            super::GpuStatusDetail::ShapeMismatch,
            "inner_lo/inner_hi length mismatch",
        ));
    }
    if outer_keys.is_empty() || inner_lo.is_empty() {
        return Ok(NljDispatchResult::Pairs(Vec::new()));
    }
    let mut buf = vec![0u32; max_pairs.saturating_mul(2).max(2)];
    let mut pair_count: usize = 0;

    // SAFETY: see dispatch_between_i64.
    let status = unsafe {
        bridge::pgaccel_nlj_between_f64(
            outer_keys.as_ptr(),
            outer_keys.len(),
            inner_lo.as_ptr(),
            inner_hi.as_ptr(),
            inner_lo.len(),
            buf.as_mut_ptr(),
            max_pairs,
            std::ptr::addr_of_mut!(pair_count),
        )
    };
    status_to_result(
        status,
        GpuErrorDomain::HashJoin,
        GpuOperation::Kernel(NLJ_KERNEL_BETWEEN_F64),
    )?;

    if pair_count > max_pairs {
        return Ok(NljDispatchResult::Overflow {
            observed: pair_count,
            cap: max_pairs,
        });
    }
    Ok(NljDispatchResult::Pairs(build_pairs(&buf, pair_count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ineq_op_oracle_i64() {
        assert!(NljIneqOp::Lt.eval_i64(1, 2));
        assert!(!NljIneqOp::Lt.eval_i64(2, 2));
        assert!(NljIneqOp::Le.eval_i64(2, 2));
        assert!(NljIneqOp::Ge.eval_i64(2, 2));
        assert!(!NljIneqOp::Gt.eval_i64(2, 2));
        assert!(NljIneqOp::Gt.eval_i64(3, 2));
    }

    #[test]
    fn ineq_op_oracle_f64() {
        assert!(NljIneqOp::Lt.eval_f64(1.0, 2.0));
        assert!(!NljIneqOp::Gt.eval_f64(2.0, 2.0));
        // NaN: any compare returns false in Rust (matches PG btree
        // inequality which treats NaN as incomparable — the kernel
        // reproduces this because GPU `<`/`>` also yield false on NaN
        // operands).
        assert!(!NljIneqOp::Lt.eval_f64(f64::NAN, 1.0));
        assert!(!NljIneqOp::Gt.eval_f64(f64::NAN, 1.0));
    }

    #[test]
    fn ineq_op_discriminants_match_c_header() {
        // ABI pin: must match pgaccel_nlj_ineq_op in the C header.
        assert_eq!(NljIneqOp::Lt as i32, 0);
        assert_eq!(NljIneqOp::Le as i32, 1);
        assert_eq!(NljIneqOp::Ge as i32, 2);
        assert_eq!(NljIneqOp::Gt as i32, 3);
    }
}

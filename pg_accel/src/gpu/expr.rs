use super::{PgaccelBatch, PgaccelExprProgram, PgaccelVal, bridge};

// ---------------------------------------------------------------------------
// Expression evaluator wrappers
// ---------------------------------------------------------------------------

/// Evaluate a predicate expression on a columnar batch via GPU.
///
/// Returns a vector of three-result values per row:
/// +1 = TRUE, -1 = FALSE, 0 = UNCERTAIN. Selected pg_accel callers reject
/// uncertain rows rather than evaluating the predicate on CPU.
/// Returns `None` if the GPU is unavailable.
pub fn expr_eval_predicate(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let mut results = vec![0i8; num_rows];

    // SAFETY: program and batch are valid references. results is pre-allocated.
    let status = unsafe {
        bridge::pgaccel_expr_eval_predicate(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

/// Evaluate a projection expression on a columnar batch via GPU.
///
/// Returns `(output_values, uncertain_mask)` or `None` if GPU unavailable.
#[allow(dead_code)] // reason: projection wrapper paired with predicate; executor only consumes predicate today
pub fn expr_eval_project(
    program: &PgaccelExprProgram,
    batch: &PgaccelBatch,
    num_rows: usize,
) -> Option<(Vec<PgaccelVal>, Vec<u8>)> {
    let mut output = vec![PgaccelVal::null(); num_rows];
    let mut uncertain = vec![0u8; num_rows];

    // SAFETY: program and batch are valid references. output/uncertain pre-allocated.
    let status = unsafe {
        bridge::pgaccel_expr_eval_project(
            std::ptr::from_ref(program),
            std::ptr::from_ref(batch),
            output.as_mut_ptr(),
            uncertain.as_mut_ptr(),
        )
    };
    status.is_ok().then_some((output, uncertain))
}

/// Template: evaluate `col <cmp> const` on a batch.
///
/// Returns three-result vector or `None` if GPU unavailable.
pub fn expr_template_cmp_const(
    batch: &PgaccelBatch,
    col_idx: u32,
    cmp_opcode: u16,
    const_val: f64,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let mut results = vec![0i8; num_rows];

    // SAFETY: batch is a valid reference. results is pre-allocated.
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const(
            std::ptr::from_ref(batch),
            col_idx,
            cmp_opcode,
            const_val,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

/// Template: evaluate `col1 <cmp1> const1 AND col2 <cmp2> const2` on a
/// batch via Agent 4A's struct-packed kernel (single dispatch, no Rust-side
/// AND combiner). Three-valued: `+1=TRUE, -1=FALSE, 0=UNCERTAIN`.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and(
    batch: &PgaccelBatch,
    col1_idx: u32,
    cmp1_opcode: u16,
    const1_val: f64,
    col2_idx: u32,
    cmp2_opcode: u16,
    const2_val: f64,
    num_rows: usize,
) -> Option<Vec<i8>> {
    let mut results = vec![0i8; num_rows];
    // SAFETY: batch is a valid reference; results is caller-owned with
    // num_rows capacity.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and(
            std::ptr::from_ref(batch),
            col1_idx,
            cmp1_opcode,
            const1_val,
            col2_idx,
            cmp2_opcode,
            const2_val,
            results.as_mut_ptr(),
        )
    };
    status.is_ok().then_some(results)
}

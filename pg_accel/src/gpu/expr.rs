use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;

use super::{
    PgaccelBatch, PgaccelExprProgram, PgaccelExprUsmCol, PgaccelStatus, PgaccelVal, PgaccelValTag,
    bridge,
};

fn checked_allocation_bytes<T>(len: usize) -> Option<usize> {
    len.checked_mul(std::mem::size_of::<T>())
        .filter(|bytes| *bytes != 0)
}

/// Shared-USM buffer for direct expression-template staging.
///
/// The pointer is host-writable and kernel-readable. It is intentionally
/// `!Send + !Sync` via `PhantomData<*mut T>` because the underlying queue and
/// PostgreSQL executor state are backend-local.
pub struct ExprSharedBuffer<T> {
    ptr: NonNull<T>,
    len: usize,
    _not_send_sync: PhantomData<*mut T>,
}

impl<T> ExprSharedBuffer<T> {
    /// Allocate `len` elements in shared USM.
    ///
    /// Returns `None` for zero-sized requests, size overflow, unavailable GPU,
    /// or allocation failure.
    pub fn new(len: usize) -> Option<Self> {
        let bytes = checked_allocation_bytes::<T>(len)?;
        let mut raw = std::ptr::null_mut::<c_void>();
        // SAFETY: `raw` is a valid out pointer. The C++ side initializes the
        // GPU queue and writes a shared-USM allocation pointer on success.
        let status = unsafe { bridge::pgaccel_expr_shared_alloc(bytes, &raw mut raw) };
        if !status.is_ok() {
            return None;
        }
        Some(Self {
            ptr: NonNull::new(raw.cast::<T>())?,
            len,
            _not_send_sync: PhantomData,
        })
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn as_mut_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: this buffer owns `len` contiguous elements of type T until Drop.
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: this buffer owns `len` contiguous elements of type T until Drop.
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }
}

impl<T> Drop for ExprSharedBuffer<T> {
    fn drop(&mut self) {
        // SAFETY: pointer was returned by pgaccel_expr_shared_alloc and is
        // freed exactly once by this owner.
        unsafe { bridge::pgaccel_expr_shared_free(self.ptr.as_ptr().cast::<c_void>()) };
    }
}

/// Device-owned buffer for resident cached columns.
///
/// The pointer is only valid for device kernels. It is intentionally not
/// exposed as a Rust slice because host dereference of device memory is
/// invalid on discrete devices and semantically wrong for resident caches.
pub struct ExprDeviceBuffer<T> {
    ptr: NonNull<T>,
    len: usize,
    _not_send_sync: PhantomData<*mut T>,
}

impl<T> ExprDeviceBuffer<T> {
    /// Allocate uninitialized device memory.
    ///
    /// Returns `None` for zero-sized requests, size overflow, unavailable GPU,
    /// or allocation failure. The host must not dereference the returned
    /// pointer.
    pub fn new(len: usize) -> Option<Self> {
        let bytes = checked_allocation_bytes::<T>(len)?;
        let mut raw = std::ptr::null_mut::<c_void>();
        // SAFETY: `raw` is a valid out pointer. The C++ side initializes the
        // GPU queue and writes a device allocation pointer on success.
        let status = unsafe { bridge::pgaccel_expr_device_alloc(bytes, &raw mut raw) };
        if !status.is_ok() {
            return None;
        }
        Some(Self {
            ptr: NonNull::new(raw.cast::<T>())?,
            len,
            _not_send_sync: PhantomData,
        })
    }

    /// Allocate device memory and copy `values` into it once.
    ///
    /// Returns `None` for empty input, size overflow, unavailable GPU,
    /// allocation failure, or device-copy failure.
    pub fn copy_from_slice(values: &[T]) -> Option<Self> {
        let bytes = checked_allocation_bytes::<T>(values.len())?;
        let mut raw = std::ptr::null_mut::<c_void>();
        // SAFETY: source slice is valid for `bytes` and `raw` is an out pointer.
        let status = unsafe {
            bridge::pgaccel_expr_device_alloc_copy(
                values.as_ptr().cast::<c_void>(),
                bytes,
                &raw mut raw,
            )
        };
        if !status.is_ok() {
            return None;
        }
        Some(Self {
            ptr: NonNull::new(raw.cast::<T>())?,
            len: values.len(),
            _not_send_sync: PhantomData,
        })
    }

    #[must_use]
    pub fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn as_mut_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }
}

impl<T> Drop for ExprDeviceBuffer<T> {
    fn drop(&mut self) {
        // SAFETY: pointer was returned by a pgaccel_expr_device_alloc* call
        // and is freed exactly once by this owner.
        unsafe { bridge::pgaccel_expr_device_free(self.ptr.as_ptr().cast::<c_void>()) };
    }
}

#[must_use]
pub fn expr_usm_col<T>(
    values: &ExprSharedBuffer<T>,
    nulls: Option<&ExprSharedBuffer<u8>>,
    tag: PgaccelValTag,
) -> PgaccelExprUsmCol {
    PgaccelExprUsmCol {
        values: values.as_ptr().cast::<c_void>(),
        nulls: nulls.map_or(std::ptr::null(), ExprSharedBuffer::as_ptr),
        tag,
    }
}

#[must_use]
pub fn expr_device_col<T>(values: &ExprDeviceBuffer<T>, tag: PgaccelValTag) -> PgaccelExprUsmCol {
    PgaccelExprUsmCol {
        values: values.as_ptr().cast::<c_void>(),
        nulls: std::ptr::null(),
        tag,
    }
}

#[must_use]
pub fn expr_device_col_with_nulls<T>(
    values: &ExprDeviceBuffer<T>,
    nulls: Option<&ExprDeviceBuffer<u8>>,
    tag: PgaccelValTag,
) -> PgaccelExprUsmCol {
    PgaccelExprUsmCol {
        values: values.as_ptr().cast::<c_void>(),
        nulls: nulls.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
        tag,
    }
}

#[must_use]
pub const fn null_usm_col() -> PgaccelExprUsmCol {
    PgaccelExprUsmCol {
        values: std::ptr::null(),
        nulls: std::ptr::null(),
        tag: PgaccelValTag::Null,
    }
}

impl From<&ExprDeviceBuffer<i32>> for PgaccelExprUsmCol {
    fn from(values: &ExprDeviceBuffer<i32>) -> Self {
        expr_device_col(values, PgaccelValTag::Int32)
    }
}

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

/// Template: count TRUE rows for `col <cmp> const` on a batch.
///
/// Returns `(true_count, uncertain_count)` or `None` if GPU dispatch fails.
pub fn expr_template_cmp_const_count(
    batch: &PgaccelBatch,
    col_idx: u32,
    cmp_opcode: u16,
    const_val: f64,
) -> Option<(usize, usize)> {
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: batch is a valid reference; output counters are valid pointers.
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const_count(
            std::ptr::from_ref(batch),
            col_idx,
            cmp_opcode,
            const_val,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Template: count TRUE rows for an already-staged shared-USM column.
pub fn expr_template_cmp_const_count_usm(
    col: PgaccelExprUsmCol,
    row_count: usize,
    cmp_opcode: u16,
    const_val: f64,
) -> Option<(usize, usize)> {
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees `col` points at shared-USM buffers with at
    // least row_count elements and matching type tag.
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const_count_usm(
            col,
            row_count,
            cmp_opcode,
            const_val,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Template: write a `1 = TRUE`, `0 = FALSE/NULL` selection mask for an
/// already-staged shared-USM column and return `(true_count, uncertain_count)`.
pub fn expr_template_cmp_const_mask_usm(
    col: PgaccelExprUsmCol,
    row_count: usize,
    cmp_opcode: u16,
    const_val: f64,
    selection: &mut ExprSharedBuffer<u8>,
) -> Option<(usize, usize)> {
    if selection.len() < row_count {
        return None;
    }

    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees `col` points at shared-USM buffers with at
    // least row_count elements. `selection` is shared USM with row_count bytes.
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const_mask_usm(
            col,
            row_count,
            cmp_opcode,
            const_val,
            selection.as_mut_ptr(),
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Result of a direct-USM template predicate fused with f32 SUM/MIN/MAX/COUNT.
#[derive(Debug, Clone, Copy)]
pub struct ExprTemplateReduceF32 {
    pub sum: f32,
    pub min: f32,
    pub max: f32,
    pub value_count: i64,
    pub true_count: usize,
    pub uncertain_count: usize,
}

/// Result of the SSBM Q1.x direct-USM filtered revenue aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: ABI wrapper lands before planner/executor integration consumes it
pub struct ExprTemplateSsbmQ1RevenueI64 {
    pub sum: i64,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Result of the SSBM Q2.x grouped revenue aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprTemplateSsbmQ2GroupedRevenueI64 {
    pub revenue_by_group: Vec<i64>,
    pub count_by_group: Vec<u32>,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Result of the SSBM Q3.x grouped revenue aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprTemplateSsbmQ3GroupedRevenueI64 {
    pub revenue_by_group: Vec<i64>,
    pub count_by_group: Vec<u32>,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Result of the SSBM Q4.x grouped profit aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprTemplateSsbmQ4GroupedProfitI64 {
    pub profit_by_group: Vec<i64>,
    pub count_by_group: Vec<u32>,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Result of the resident dense grouped f64 aggregate.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprTemplateResidentDenseGroupedF64 {
    pub sum_by_group: Vec<f64>,
    pub min_by_group: Vec<f64>,
    pub max_by_group: Vec<f64>,
    pub sumsq_by_group: Vec<f64>,
    pub rhs_sum_by_group: Vec<f64>,
    pub count_by_group: Vec<u32>,
    pub rhs_count_by_group: Vec<u32>,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Result of a resident scalar f64 reduction.
#[derive(Debug, Clone, PartialEq)]
pub struct ExprTemplateReduceF64 {
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub sumsq: f64,
    pub count: u64,
    pub selected_count: usize,
    pub uncertain_count: usize,
}

pub const RESIDENT_DENSE_GROUPED_F64_AGG_SUM: u32 = 1 << 0;
pub const RESIDENT_DENSE_GROUPED_F64_AGG_MIN: u32 = 1 << 1;
pub const RESIDENT_DENSE_GROUPED_F64_AGG_MAX: u32 = 1 << 2;
pub const RESIDENT_DENSE_GROUPED_F64_AGG_COUNT: u32 = 1 << 3;
pub const RESIDENT_F64_REDUCE_AGG_SUMSQ: u32 = 1 << 4;
pub const RESIDENT_DENSE_GROUPED_F64_AGG_ALL: u32 = RESIDENT_DENSE_GROUPED_F64_AGG_SUM
    | RESIDENT_DENSE_GROUPED_F64_AGG_MIN
    | RESIDENT_DENSE_GROUPED_F64_AGG_MAX
    | RESIDENT_DENSE_GROUPED_F64_AGG_COUNT;
pub const RESIDENT_F64_REDUCE_AGG_ALL: u32 =
    RESIDENT_DENSE_GROUPED_F64_AGG_ALL | RESIDENT_F64_REDUCE_AGG_SUMSQ;
pub const RESIDENT_DENSE_GROUPED_F64_FILTER_ROWS: i32 = 0;
pub const RESIDENT_DENSE_GROUPED_F64_FILTER_MEASURE_ONLY: i32 = 1;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_ONLY: i32 = 0;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_BETWEEN: i32 = 1;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_RANGES: i32 = 2;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_VALUE: i32 = 0;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_RHS: i32 = 1;
pub const RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES: usize = 4;
pub const RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_NONE: i32 = 0;
pub const RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_AGGREGATE: i32 = 1;
pub const RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_MEASURE_ONLY: i32 = 2;

/// Resident scratch buffers for the SSBM Q1.x revenue aggregate.
pub struct ExprTemplateSsbmQ1Scratch<'a> {
    pub revenue_a: &'a ExprDeviceBuffer<i64>,
    pub count_a: &'a ExprDeviceBuffer<i64>,
    pub revenue_b: &'a ExprDeviceBuffer<i64>,
    pub count_b: &'a ExprDeviceBuffer<i64>,
}

impl ExprTemplateSsbmQ1Scratch<'_> {
    #[must_use]
    pub fn item_capacity(&self) -> usize {
        self.revenue_a
            .len()
            .min(self.count_a.len())
            .min(self.revenue_b.len())
            .min(self.count_b.len())
    }

    #[must_use]
    pub fn has_distinct_buffers(&self) -> bool {
        let ptrs = [
            self.revenue_a.as_ptr().cast::<c_void>(),
            self.count_a.as_ptr().cast::<c_void>(),
            self.revenue_b.as_ptr().cast::<c_void>(),
            self.count_b.as_ptr().cast::<c_void>(),
        ];
        ptrs.iter()
            .enumerate()
            .all(|(idx, ptr)| ptrs.iter().skip(idx + 1).all(|other| ptr != other))
    }
}

/// Scratch item count required by the SSBM Q1.x revenue kernel for `row_count`.
#[must_use]
pub fn expr_template_ssbm_q1_scratch_items(row_count: usize) -> usize {
    // SAFETY: pure sizing helper; no pointers cross the ABI.
    unsafe { bridge::pgaccel_expr_template_ssbm_q1_scratch_items(row_count) }
}

/// Template: fuse `col <cmp> const` with f32 SUM/MIN/MAX/COUNT over TRUE rows.
///
/// `true_count` counts all predicate-TRUE rows for `COUNT(*)`; `value_count`
/// counts predicate-TRUE rows whose value column is non-NULL.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_cmp_const_reduce_f32_usm(
    pred_col: PgaccelExprUsmCol,
    cmp_opcode: u16,
    const_val: f64,
    value_col: PgaccelExprUsmCol,
    row_count: usize,
) -> Option<ExprTemplateReduceF32> {
    let mut out_sum = 0.0f32;
    let mut out_min = 0.0f32;
    let mut out_max = 0.0f32;
    let mut out_value_count = 0i64;
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees predicate/value columns point at shared-USM
    // buffers with at least row_count elements and matching type tags.
    let status = unsafe {
        bridge::pgaccel_expr_template_cmp_const_reduce_f32_usm(
            pred_col,
            cmp_opcode,
            const_val,
            value_col,
            row_count,
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_value_count,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some(ExprTemplateReduceF32 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        value_count: out_value_count,
        true_count,
        uncertain_count,
    })
}

/// Template: SSBM Q1.x filtered revenue aggregate over resident int32 columns.
///
/// The date filter uses `[orderdate_lo, orderdate_hi]` when `orderdate_keys`
/// is `None`; otherwise the key list is treated as a membership filter.
#[allow(dead_code)] // reason: SSBM Q1 executor wiring is the next OLAP lane hunk
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q1_revenue_i64_usm(
    orderdate_col: PgaccelExprUsmCol,
    discount_col: PgaccelExprUsmCol,
    quantity_col: PgaccelExprUsmCol,
    extendedprice_col: PgaccelExprUsmCol,
    row_count: usize,
    orderdate_lo: i32,
    orderdate_hi: i32,
    orderdate_keys: Option<&ExprSharedBuffer<i32>>,
    discount_lo: i32,
    discount_hi: i32,
    quantity_lo: i32,
    quantity_hi: i32,
) -> Option<ExprTemplateSsbmQ1RevenueI64> {
    let orderdate_key_count = orderdate_keys.map_or(0, ExprSharedBuffer::len);
    let orderdate_key_ptr = orderdate_keys.map_or(std::ptr::null(), ExprSharedBuffer::as_ptr);
    expr_template_ssbm_q1_revenue_i64_usm_raw_keys(
        orderdate_col,
        discount_col,
        quantity_col,
        extendedprice_col,
        row_count,
        orderdate_lo,
        orderdate_hi,
        orderdate_key_ptr,
        orderdate_key_count,
        discount_lo,
        discount_hi,
        quantity_lo,
        quantity_hi,
    )
}

/// Template: SSBM Q1.x filtered revenue aggregate with a raw resident date-key
/// pointer. The key list may live in shared or device USM.
#[allow(dead_code)] // reason: selected OLAP executor is landed incrementally with cache admission
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q1_revenue_i64_usm_raw_keys(
    orderdate_col: PgaccelExprUsmCol,
    discount_col: PgaccelExprUsmCol,
    quantity_col: PgaccelExprUsmCol,
    extendedprice_col: PgaccelExprUsmCol,
    row_count: usize,
    orderdate_lo: i32,
    orderdate_hi: i32,
    orderdate_key_ptr: *const i32,
    orderdate_key_count: usize,
    discount_lo: i32,
    discount_hi: i32,
    quantity_lo: i32,
    quantity_hi: i32,
) -> Option<ExprTemplateSsbmQ1RevenueI64> {
    let mut out_sum = 0i64;
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees all column and optional key-list pointers are
    // shared-USM/resident buffers with at least the supplied counts.
    let status = unsafe {
        bridge::pgaccel_expr_template_ssbm_q1_revenue_i64_usm(
            orderdate_col,
            discount_col,
            quantity_col,
            extendedprice_col,
            row_count,
            orderdate_lo,
            orderdate_hi,
            orderdate_key_ptr,
            orderdate_key_count,
            discount_lo,
            discount_hi,
            quantity_lo,
            quantity_hi,
            &raw mut out_sum,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some(ExprTemplateSsbmQ1RevenueI64 {
        sum: out_sum,
        selected_count,
        uncertain_count,
    })
}

/// Template: SSBM Q1.x filtered revenue aggregate using caller-owned resident
/// scratch buffers. This is the selected SQL path; it avoids per-query device
/// allocation inside the kernel.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q1_revenue_i64_usm_raw_keys_scratch(
    orderdate_col: PgaccelExprUsmCol,
    discount_col: PgaccelExprUsmCol,
    quantity_col: PgaccelExprUsmCol,
    extendedprice_col: PgaccelExprUsmCol,
    row_count: usize,
    orderdate_lo: i32,
    orderdate_hi: i32,
    orderdate_key_ptr: *const i32,
    orderdate_key_count: usize,
    discount_lo: i32,
    discount_hi: i32,
    quantity_lo: i32,
    quantity_hi: i32,
    scratch: ExprTemplateSsbmQ1Scratch<'_>,
) -> Option<ExprTemplateSsbmQ1RevenueI64> {
    let mut out_sum = 0i64;
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;
    let scratch_item_capacity = scratch.item_capacity();
    if scratch_item_capacity < expr_template_ssbm_q1_scratch_items(row_count)
        || !scratch.has_distinct_buffers()
    {
        return None;
    }

    // SAFETY: caller guarantees all column/key pointers and scratch buffers
    // are resident buffers with at least the supplied counts/capacity.
    let status = unsafe {
        bridge::pgaccel_expr_template_ssbm_q1_revenue_i64_usm_scratch(
            orderdate_col,
            discount_col,
            quantity_col,
            extendedprice_col,
            row_count,
            orderdate_lo,
            orderdate_hi,
            orderdate_key_ptr,
            orderdate_key_count,
            discount_lo,
            discount_hi,
            quantity_lo,
            quantity_hi,
            scratch.revenue_a.as_mut_ptr(),
            scratch.count_a.as_mut_ptr(),
            scratch.revenue_b.as_mut_ptr(),
            scratch.count_b.as_mut_ptr(),
            scratch_item_capacity,
            &raw mut out_sum,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some(ExprTemplateSsbmQ1RevenueI64 {
        sum: out_sum,
        selected_count,
        uncertain_count,
    })
}

/// Template: SSBM Q2.x grouped SUM(lo_revenue) over resident star-schema
/// columns and resident dimension membership maps.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q2_grouped_revenue_i64_usm(
    orderdate_col: PgaccelExprUsmCol,
    partkey_col: PgaccelExprUsmCol,
    suppkey_col: PgaccelExprUsmCol,
    revenue_col: PgaccelExprUsmCol,
    row_count: usize,
    date_key_min: i32,
    date_year_by_offset: *const i32,
    date_year_count: usize,
    part_brand_code_by_key: *const i32,
    part_match_by_key: *const u8,
    part_key_count: usize,
    supplier_match_by_key: *const u8,
    supplier_key_count: usize,
    year_min: i32,
    year_count: i32,
    brand_count: i32,
) -> Option<ExprTemplateSsbmQ2GroupedRevenueI64> {
    if year_count <= 0 || brand_count <= 0 {
        return None;
    }
    let group_count = usize::try_from(year_count)
        .ok()?
        .checked_mul(usize::try_from(brand_count).ok()?)?;
    if group_count == 0 {
        return None;
    }

    let mut revenue_by_group = vec![0i64; group_count];
    let mut count_by_group = vec![0u32; group_count];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees all column and dimension-map pointers are
    // resident buffers with at least the supplied counts. Host output vectors
    // are sized to out_group_capacity.
    let status = unsafe {
        bridge::pgaccel_expr_template_ssbm_q2_grouped_revenue_i64_usm(
            orderdate_col,
            partkey_col,
            suppkey_col,
            revenue_col,
            row_count,
            date_key_min,
            date_year_by_offset,
            date_year_count,
            part_brand_code_by_key,
            part_match_by_key,
            part_key_count,
            supplier_match_by_key,
            supplier_key_count,
            year_min,
            year_count,
            brand_count,
            revenue_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            group_count,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    status
        .is_ok()
        .then_some(ExprTemplateSsbmQ2GroupedRevenueI64 {
            revenue_by_group,
            count_by_group,
            selected_count,
            uncertain_count,
        })
}

/// Template: SSBM Q3.x grouped SUM(lo_revenue) over resident star-schema
/// columns and resident date/customer/supplier dimension maps.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q3_grouped_revenue_i64_usm(
    orderdate_col: PgaccelExprUsmCol,
    custkey_col: PgaccelExprUsmCol,
    suppkey_col: PgaccelExprUsmCol,
    revenue_col: PgaccelExprUsmCol,
    row_count: usize,
    date_key_min: i32,
    date_year_by_offset: *const i32,
    date_match_by_offset: *const u8,
    date_year_count: usize,
    customer_group_code_by_key: *const i32,
    customer_match_by_key: *const u8,
    customer_key_count: usize,
    supplier_group_code_by_key: *const i32,
    supplier_match_by_key: *const u8,
    supplier_key_count: usize,
    year_min: i32,
    year_count: i32,
    customer_group_count: i32,
    supplier_group_count: i32,
) -> Option<ExprTemplateSsbmQ3GroupedRevenueI64> {
    if year_count <= 0 || customer_group_count <= 0 || supplier_group_count <= 0 {
        return None;
    }
    let group_count = usize::try_from(year_count)
        .ok()?
        .checked_mul(usize::try_from(customer_group_count).ok()?)?
        .checked_mul(usize::try_from(supplier_group_count).ok()?)?;
    if group_count == 0 {
        return None;
    }

    let mut revenue_by_group = vec![0i64; group_count];
    let mut count_by_group = vec![0u32; group_count];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees all column and dimension-map pointers are
    // resident buffers with at least the supplied counts. Host output vectors
    // are sized to out_group_capacity.
    let status = unsafe {
        bridge::pgaccel_expr_template_ssbm_q3_grouped_revenue_i64_usm(
            orderdate_col,
            custkey_col,
            suppkey_col,
            revenue_col,
            row_count,
            date_key_min,
            date_year_by_offset,
            date_match_by_offset,
            date_year_count,
            customer_group_code_by_key,
            customer_match_by_key,
            customer_key_count,
            supplier_group_code_by_key,
            supplier_match_by_key,
            supplier_key_count,
            year_min,
            year_count,
            customer_group_count,
            supplier_group_count,
            revenue_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            group_count,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    status
        .is_ok()
        .then_some(ExprTemplateSsbmQ3GroupedRevenueI64 {
            revenue_by_group,
            count_by_group,
            selected_count,
            uncertain_count,
        })
}

/// Template: SSBM Q4.x grouped SUM(lo_revenue - lo_supplycost) over resident
/// star-schema columns and resident date/customer/supplier/part maps.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_ssbm_q4_grouped_profit_i64_usm(
    orderdate_col: PgaccelExprUsmCol,
    custkey_col: PgaccelExprUsmCol,
    suppkey_col: PgaccelExprUsmCol,
    partkey_col: PgaccelExprUsmCol,
    revenue_col: PgaccelExprUsmCol,
    supplycost: impl Into<PgaccelExprUsmCol>,
    row_count: usize,
    date_key_min: i32,
    date_year_by_offset: *const i32,
    date_match_by_offset: *const u8,
    date_year_count: usize,
    customer_group_code_by_key: *const i32,
    customer_match_by_key: *const u8,
    customer_key_count: usize,
    supplier_group_code_by_key: *const i32,
    supplier_match_by_key: *const u8,
    supplier_key_count: usize,
    part_group_code_by_key: *const i32,
    part_match_by_key: *const u8,
    part_key_count: usize,
    group_geo_source: i32,
    year_min: i32,
    year_count: i32,
    geo_group_count: i32,
    part_group_count: i32,
    scratch_profit_lo: *mut u32,
    scratch_profit_hi: *mut u32,
    scratch_count: *mut u32,
    scratch_group_capacity: usize,
) -> Option<ExprTemplateSsbmQ4GroupedProfitI64> {
    if year_count <= 0 || geo_group_count <= 0 || part_group_count <= 0 {
        return None;
    }
    let group_count = usize::try_from(year_count)
        .ok()?
        .checked_mul(usize::try_from(geo_group_count).ok()?)?
        .checked_mul(usize::try_from(part_group_count).ok()?)?;
    if group_count == 0 {
        return None;
    }
    if scratch_group_capacity < group_count
        || scratch_profit_lo.is_null()
        || scratch_profit_hi.is_null()
        || scratch_count.is_null()
    {
        return None;
    }

    let mut profit_by_group = vec![0i64; group_count];
    let mut count_by_group = vec![0u32; group_count];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;
    let supplycost_col = supplycost.into();

    // SAFETY: caller guarantees all column and dimension-map pointers are
    // resident buffers with at least the supplied counts. Host output vectors
    // are sized to out_group_capacity.
    let status = unsafe {
        bridge::pgaccel_expr_template_ssbm_q4_grouped_profit_i64_usm(
            orderdate_col,
            custkey_col,
            suppkey_col,
            partkey_col,
            revenue_col,
            supplycost_col,
            row_count,
            date_key_min,
            date_year_by_offset,
            date_match_by_offset,
            date_year_count,
            customer_group_code_by_key,
            customer_match_by_key,
            customer_key_count,
            supplier_group_code_by_key,
            supplier_match_by_key,
            supplier_key_count,
            part_group_code_by_key,
            part_match_by_key,
            part_key_count,
            group_geo_source,
            year_min,
            year_count,
            geo_group_count,
            part_group_count,
            scratch_profit_lo,
            scratch_profit_hi,
            scratch_count,
            scratch_group_capacity,
            profit_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            group_count,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    status
        .is_ok()
        .then_some(ExprTemplateSsbmQ4GroupedProfitI64 {
            profit_by_group,
            count_by_group,
            selected_count,
            uncertain_count,
        })
}

/// Template: resident dense grouped SUM/COUNT over int32 group keys and f64 values.
#[allow(dead_code)] // reason: compatibility wrapper; executor uses the Result-returning form for diagnostics
#[allow(clippy::too_many_arguments)]
pub fn expr_template_resident_dense_grouped_f64_usm(
    group_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    filter_col: Option<PgaccelExprUsmCol>,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_min: *mut f64,
    scratch_max: *mut f64,
    scratch_count: *mut u32,
    scratch_group_start: *mut u32,
    scratch_group_cursor: *mut u32,
    scratch_group_capacity: usize,
    scratch_sorted_group: *mut i32,
    scratch_row_index: *mut u32,
    scratch_row_capacity: usize,
) -> Option<ExprTemplateResidentDenseGroupedF64> {
    try_expr_template_resident_dense_grouped_f64_usm(
        group_col,
        value_col,
        None,
        0,
        filter_col,
        row_count,
        group_min,
        group_count,
        scratch_sum,
        scratch_min,
        scratch_max,
        scratch_count,
        scratch_group_start,
        scratch_group_cursor,
        scratch_group_capacity,
        scratch_sorted_group,
        scratch_row_index,
        scratch_row_capacity,
    )
    .ok()
}

/// Template: resident dense grouped SUM/MIN/MAX/COUNT over int32 group keys and f64 values.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_dense_grouped_f64_usm(
    group_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    value_rhs_col: Option<PgaccelExprUsmCol>,
    measure_op: i32,
    filter_col: Option<PgaccelExprUsmCol>,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_min: *mut f64,
    scratch_max: *mut f64,
    scratch_count: *mut u32,
    scratch_group_start: *mut u32,
    scratch_group_cursor: *mut u32,
    scratch_group_capacity: usize,
    scratch_sorted_group: *mut i32,
    scratch_row_index: *mut u32,
    scratch_row_capacity: usize,
) -> Result<ExprTemplateResidentDenseGroupedF64, PgaccelStatus> {
    try_expr_template_resident_dense_grouped_f64_usm_masked(
        group_col,
        value_col,
        value_rhs_col,
        measure_op,
        RESIDENT_DENSE_GROUPED_F64_AGG_ALL,
        RESIDENT_DENSE_GROUPED_F64_FILTER_ROWS,
        RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_ONLY,
        RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_VALUE,
        0,
        [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES],
        [0.0; RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES],
        filter_col,
        row_count,
        group_min,
        group_count,
        scratch_sum,
        scratch_min,
        scratch_max,
        scratch_count,
        scratch_group_start,
        scratch_group_cursor,
        scratch_group_capacity,
        scratch_sorted_group,
        scratch_row_index,
        scratch_row_capacity,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        0,
    )
}

/// Template: resident dense grouped aggregate over int32 group keys and f64 values.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_dense_grouped_f64_usm_masked(
    group_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    value_rhs_col: Option<PgaccelExprUsmCol>,
    measure_op: i32,
    aggregate_mask: u32,
    filter_mode: i32,
    measure_predicate_op: i32,
    measure_predicate_source: i32,
    measure_predicate_range_count: i32,
    measure_predicate_range_los: [f64; RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES],
    measure_predicate_range_his: [f64; RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES],
    filter_col: Option<PgaccelExprUsmCol>,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_min: *mut f64,
    scratch_max: *mut f64,
    scratch_count: *mut u32,
    scratch_group_start: *mut u32,
    scratch_group_cursor: *mut u32,
    scratch_group_capacity: usize,
    scratch_sorted_group: *mut i32,
    scratch_row_index: *mut u32,
    scratch_row_capacity: usize,
    scratch_partial_sum: *mut f64,
    scratch_partial_min: *mut f64,
    scratch_partial_max: *mut f64,
    scratch_partial_count: *mut u32,
    scratch_partial_capacity: usize,
) -> Result<ExprTemplateResidentDenseGroupedF64, PgaccelStatus> {
    if group_count <= 0 {
        return Err(PgaccelStatus::ErrorUnsupported);
    }
    let group_count_usize =
        usize::try_from(group_count).map_err(|_| PgaccelStatus::ErrorUnsupported)?;
    let aggregate_mask_valid = aggregate_mask != 0
        && (aggregate_mask & !RESIDENT_DENSE_GROUPED_F64_AGG_ALL) == 0
        && (aggregate_mask & RESIDENT_DENSE_GROUPED_F64_AGG_SUM) != 0
        && (aggregate_mask & RESIDENT_DENSE_GROUPED_F64_AGG_COUNT) != 0;
    let needs_min = (aggregate_mask & RESIDENT_DENSE_GROUPED_F64_AGG_MIN) != 0;
    let needs_max = (aggregate_mask & RESIDENT_DENSE_GROUPED_F64_AGG_MAX) != 0;
    let filter_mode_valid = matches!(
        filter_mode,
        RESIDENT_DENSE_GROUPED_F64_FILTER_ROWS | RESIDENT_DENSE_GROUPED_F64_FILTER_MEASURE_ONLY
    );
    let measure_predicate_valid = matches!(
        measure_predicate_op,
        RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_ONLY
            | RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_BETWEEN
            | RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_RANGES
    );
    let measure_predicate_source_valid = matches!(
        measure_predicate_source,
        RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_VALUE
            | RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_RHS
    );
    let measure_predicate_range_count_valid = measure_predicate_range_count >= 0
        && usize::try_from(measure_predicate_range_count)
            .is_ok_and(|count| count <= RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_MAX_RANGES);
    let ranges_valid = measure_predicate_range_count_valid
        && (0..usize::try_from(measure_predicate_range_count).unwrap_or(0)).all(|idx| {
            !measure_predicate_range_los[idx].is_nan()
                && !measure_predicate_range_his[idx].is_nan()
                && measure_predicate_range_los[idx] <= measure_predicate_range_his[idx]
        });
    if group_count_usize == 0
        || !aggregate_mask_valid
        || !filter_mode_valid
        || !measure_predicate_valid
        || !measure_predicate_source_valid
        || !measure_predicate_range_count_valid
        || !ranges_valid
        || (measure_predicate_op == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_ONLY
            && measure_predicate_source != RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_VALUE)
        || (measure_predicate_op == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_BETWEEN
            && measure_predicate_range_count != 1)
        || (measure_predicate_op == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_RANGES
            && measure_predicate_range_count <= 0)
        || ((measure_predicate_op == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_BETWEEN
            || measure_predicate_op == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_BOOL_AND_RHS_RANGES)
            && measure_predicate_source == RESIDENT_DENSE_GROUPED_F64_MEASURE_PRED_SOURCE_RHS
            && measure_op == 0)
        || (filter_mode == RESIDENT_DENSE_GROUPED_F64_FILTER_MEASURE_ONLY
            && (needs_min || needs_max))
        || needs_min != needs_max
        || scratch_group_capacity < group_count_usize
        || scratch_row_capacity < row_count
        || scratch_sum.is_null()
        || (needs_min && scratch_min.is_null())
        || (needs_max && scratch_max.is_null())
        || scratch_count.is_null()
        || scratch_group_start.is_null()
        || scratch_group_cursor.is_null()
        || scratch_sorted_group.is_null()
        || scratch_row_index.is_null()
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut sum_by_group = vec![0.0f64; group_count_usize];
    let mut min_by_group = if needs_min {
        vec![0.0f64; group_count_usize]
    } else {
        Vec::new()
    };
    let mut max_by_group = if needs_max {
        vec![0.0f64; group_count_usize]
    } else {
        Vec::new()
    };
    let mut count_by_group = vec![0u32; group_count_usize];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;
    let value_rhs_col = value_rhs_col.unwrap_or_else(null_usm_col);
    let filter_col = filter_col.unwrap_or_else(null_usm_col);

    // SAFETY: caller guarantees resident column pointers and scratch buffers
    // have the supplied row/group capacities. Output vectors are sized to
    // group_count.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
            group_col,
            value_col,
            value_rhs_col,
            measure_op,
            aggregate_mask,
            filter_mode,
            measure_predicate_op,
            measure_predicate_source,
            measure_predicate_range_count,
            measure_predicate_range_los[0],
            measure_predicate_range_his[0],
            measure_predicate_range_los[1],
            measure_predicate_range_his[1],
            measure_predicate_range_los[2],
            measure_predicate_range_his[2],
            measure_predicate_range_los[3],
            measure_predicate_range_his[3],
            filter_col,
            row_count,
            group_min,
            group_count,
            scratch_sum,
            scratch_min,
            scratch_max,
            scratch_count,
            scratch_group_start,
            scratch_group_cursor,
            scratch_group_capacity,
            scratch_sorted_group,
            scratch_row_index,
            scratch_row_capacity,
            scratch_partial_sum,
            scratch_partial_min,
            scratch_partial_max,
            scratch_partial_count,
            scratch_partial_capacity,
            sum_by_group.as_mut_ptr(),
            if needs_min {
                min_by_group.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            if needs_max {
                max_by_group.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            count_by_group.as_mut_ptr(),
            group_count_usize,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if !status.is_ok() {
        return Err(status);
    }
    Ok(ExprTemplateResidentDenseGroupedF64 {
        sum_by_group,
        min_by_group,
        max_by_group,
        sumsq_by_group: Vec::new(),
        rhs_sum_by_group: Vec::new(),
        count_by_group,
        rhs_count_by_group: Vec::new(),
        selected_count,
        uncertain_count,
    })
}

/// Resident scalar f64 reduction over one cached column.
pub fn try_expr_template_reduce_f64_usm(
    value_col: PgaccelExprUsmCol,
    aggregate_mask: u32,
    row_count: usize,
) -> Result<ExprTemplateReduceF64, PgaccelStatus> {
    if aggregate_mask == 0 || (aggregate_mask & !RESIDENT_F64_REDUCE_AGG_ALL) != 0 {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut sum = 0.0f64;
    let mut min = 0.0f64;
    let mut max = 0.0f64;
    let mut sumsq = 0.0f64;
    let mut count = 0u64;
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees `value_col` is a resident f64 USM column with
    // at least `row_count` elements. The C++ kernel writes host scalars after
    // copying final device reductions back to these pointers.
    let status = unsafe {
        bridge::pgaccel_expr_template_reduce_f64_usm(
            value_col,
            aggregate_mask,
            row_count,
            &raw mut sum,
            &raw mut min,
            &raw mut max,
            &raw mut sumsq,
            &raw mut count,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if !status.is_ok() {
        return Err(status);
    }
    if (aggregate_mask & RESIDENT_DENSE_GROUPED_F64_AGG_COUNT) == 0 {
        count = u64::try_from(selected_count).map_err(|_| PgaccelStatus::ErrorUnsupported)?;
    }

    Ok(ExprTemplateReduceF64 {
        sum,
        min,
        max,
        sumsq,
        count,
        selected_count,
        uncertain_count,
    })
}

/// Resident grouped stats over two f64 measures. The primary measure produces
/// SUM/COUNT/SUMSQ; the RHS measure produces SUM/COUNT for AVG.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_dense_grouped_f64_stats_pair_usm(
    group_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    value_rhs_col: PgaccelExprUsmCol,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_sumsq: *mut f64,
    scratch_rhs_sum: *mut f64,
    scratch_count: *mut u32,
    scratch_group_start: *mut u32,
    scratch_group_len: *mut u32,
    scratch_group_capacity: usize,
    scratch_sorted_group: *mut i32,
    scratch_row_index: *mut u32,
    scratch_row_capacity: usize,
) -> Result<ExprTemplateResidentDenseGroupedF64, PgaccelStatus> {
    if group_count <= 0 {
        return Err(PgaccelStatus::ErrorUnsupported);
    }
    let group_count_usize =
        usize::try_from(group_count).map_err(|_| PgaccelStatus::ErrorUnsupported)?;
    if group_count_usize == 0
        || scratch_group_capacity < group_count_usize
        || scratch_row_capacity < row_count
        || scratch_sum.is_null()
        || scratch_sumsq.is_null()
        || scratch_rhs_sum.is_null()
        || scratch_count.is_null()
        || scratch_group_start.is_null()
        || scratch_group_len.is_null()
        || scratch_sorted_group.is_null()
        || scratch_row_index.is_null()
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut sum_by_group = vec![0.0f64; group_count_usize];
    let mut sumsq_by_group = vec![0.0f64; group_count_usize];
    let mut count_by_group = vec![0u32; group_count_usize];
    let mut rhs_sum_by_group = vec![0.0f64; group_count_usize];
    let mut rhs_count_by_group = vec![0u32; group_count_usize];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller supplies resident group/value/RHS columns. Output vectors
    // are host-owned and sized to group_count.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_dense_grouped_f64_stats_pair_usm(
            group_col,
            value_col,
            value_rhs_col,
            row_count,
            group_min,
            group_count,
            scratch_sum,
            scratch_sumsq,
            scratch_rhs_sum,
            scratch_count,
            scratch_group_start,
            scratch_group_len,
            scratch_group_capacity,
            scratch_sorted_group,
            scratch_row_index,
            scratch_row_capacity,
            sum_by_group.as_mut_ptr(),
            sumsq_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            rhs_sum_by_group.as_mut_ptr(),
            rhs_count_by_group.as_mut_ptr(),
            group_count_usize,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if !status.is_ok() {
        return Err(status);
    }
    Ok(ExprTemplateResidentDenseGroupedF64 {
        sum_by_group,
        min_by_group: Vec::new(),
        max_by_group: Vec::new(),
        sumsq_by_group,
        rhs_sum_by_group,
        count_by_group,
        rhs_count_by_group,
        selected_count,
        uncertain_count,
    })
}

/// Narrow direct-column SUM/COUNT resident dense grouped aggregate.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
    group_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_count: *mut u32,
    scratch_partial_sum: *mut f64,
    scratch_partial_count: *mut u32,
    scratch_partial_capacity: usize,
) -> Result<ExprTemplateResidentDenseGroupedF64, PgaccelStatus> {
    if group_count <= 0 {
        return Err(PgaccelStatus::ErrorUnsupported);
    }
    let group_count_usize =
        usize::try_from(group_count).map_err(|_| PgaccelStatus::ErrorUnsupported)?;
    if group_count_usize == 0
        || group_count_usize > 256
        || scratch_sum.is_null()
        || scratch_count.is_null()
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut sum_by_group = vec![0.0f64; group_count_usize];
    let mut count_by_group = vec![0u32; group_count_usize];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller supplies resident group/value columns and device scratch
    // buffers with capacities implied by row_count/group_count. Output vectors
    // are host-owned and sized to group_count.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
            group_col,
            value_col,
            row_count,
            group_min,
            group_count,
            scratch_sum,
            scratch_count,
            scratch_partial_sum,
            scratch_partial_count,
            scratch_partial_capacity,
            sum_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            group_count_usize,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if !status.is_ok() {
        return Err(status);
    }
    Ok(ExprTemplateResidentDenseGroupedF64 {
        sum_by_group,
        min_by_group: Vec::new(),
        max_by_group: Vec::new(),
        sumsq_by_group: Vec::new(),
        rhs_sum_by_group: Vec::new(),
        count_by_group,
        rhs_count_by_group: Vec::new(),
        selected_count,
        uncertain_count,
    })
}

/// Narrow multiplication SUM/COUNT resident dense grouped aggregate.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
    group_col: PgaccelExprUsmCol,
    lhs_col: PgaccelExprUsmCol,
    rhs_col: PgaccelExprUsmCol,
    filter_col: Option<PgaccelExprUsmCol>,
    filter_mode: i32,
    row_count: usize,
    group_min: i32,
    group_count: i32,
    scratch_sum: *mut f64,
    scratch_count: *mut u32,
    scratch_partial_sum: *mut f64,
    scratch_partial_count: *mut u32,
    scratch_partial_capacity: usize,
) -> Result<ExprTemplateResidentDenseGroupedF64, PgaccelStatus> {
    if group_count <= 0 {
        return Err(PgaccelStatus::ErrorUnsupported);
    }
    let group_count_usize =
        usize::try_from(group_count).map_err(|_| PgaccelStatus::ErrorUnsupported)?;
    let filter_mode_valid = matches!(
        filter_mode,
        RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_NONE
            | RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_AGGREGATE
            | RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_MEASURE_ONLY
    );
    if group_count_usize == 0
        || group_count_usize > 256
        || !filter_mode_valid
        || scratch_sum.is_null()
        || scratch_count.is_null()
        || (filter_mode != RESIDENT_DENSE_GROUPED_F64_MUL_FILTER_NONE && filter_col.is_none())
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut sum_by_group = vec![0.0f64; group_count_usize];
    let mut count_by_group = vec![0u32; group_count_usize];
    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;
    let filter_col = filter_col.unwrap_or_else(null_usm_col);

    // SAFETY: caller supplies resident columns and device scratch buffers with
    // capacities implied by row_count/group_count. Output vectors are host-owned
    // and sized to group_count.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
            group_col,
            lhs_col,
            rhs_col,
            filter_col,
            filter_mode,
            row_count,
            group_min,
            group_count,
            scratch_sum,
            scratch_count,
            scratch_partial_sum,
            scratch_partial_count,
            scratch_partial_capacity,
            sum_by_group.as_mut_ptr(),
            count_by_group.as_mut_ptr(),
            group_count_usize,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if !status.is_ok() {
        return Err(status);
    }
    Ok(ExprTemplateResidentDenseGroupedF64 {
        sum_by_group,
        min_by_group: Vec::new(),
        max_by_group: Vec::new(),
        sumsq_by_group: Vec::new(),
        rhs_sum_by_group: Vec::new(),
        count_by_group,
        rhs_count_by_group: Vec::new(),
        selected_count,
        uncertain_count,
    })
}

/// Project a resident one-dimension star join into dense group codes.
#[allow(dead_code)] // reason: compatibility wrapper retained for the non-compacting ABI
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_star_dim_group_project_f64_usm(
    fact_key_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    row_count: usize,
    dim_match_by_key: *const u8,
    dim_group_code_by_key: *const i32,
    dim_key_count: usize,
    value_cmp_opcode: u16,
    value_const: f64,
    out_group_codes: *mut i32,
    out_group_capacity: usize,
) -> Result<(), PgaccelStatus> {
    if row_count == 0 {
        return Ok(());
    }
    if dim_match_by_key.is_null()
        || dim_group_code_by_key.is_null()
        || out_group_codes.is_null()
        || dim_key_count == 0
        || out_group_capacity < row_count
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    // SAFETY: caller supplies resident fact columns, resident dimension maps,
    // and an output buffer with at least row_count i32 slots.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_star_dim_group_project_f64_usm(
            fact_key_col,
            value_col,
            row_count,
            dim_match_by_key,
            dim_group_code_by_key,
            dim_key_count,
            value_cmp_opcode,
            value_const,
            out_group_codes,
            out_group_capacity,
        )
    };
    if status.is_ok() { Ok(()) } else { Err(status) }
}

pub struct ExprTemplateResidentStarDimGroupCompactF64 {
    pub selected_count: usize,
    pub uncertain_count: usize,
}

/// Project and compact a resident one-dimension star join into dense group
/// codes plus the matching f64 measure values.
#[allow(clippy::too_many_arguments)]
pub fn try_expr_template_resident_star_dim_group_compact_f64_usm(
    fact_key_col: PgaccelExprUsmCol,
    value_col: PgaccelExprUsmCol,
    row_count: usize,
    dim_match_by_key: *const u8,
    dim_group_code_by_key: *const i32,
    dim_key_count: usize,
    value_cmp_opcode: u16,
    value_const: f64,
    out_group_codes: *mut i32,
    out_values: *mut f64,
    out_capacity: usize,
) -> Result<ExprTemplateResidentStarDimGroupCompactF64, PgaccelStatus> {
    if row_count == 0 {
        return Ok(ExprTemplateResidentStarDimGroupCompactF64 {
            selected_count: 0,
            uncertain_count: 0,
        });
    }
    if dim_match_by_key.is_null()
        || dim_group_code_by_key.is_null()
        || out_group_codes.is_null()
        || out_values.is_null()
        || dim_key_count == 0
        || out_capacity < row_count
    {
        return Err(PgaccelStatus::ErrorUnsupported);
    }

    let mut selected_count = 0usize;
    let mut uncertain_count = 0usize;
    // SAFETY: caller supplies resident fact columns, resident dimension maps,
    // output buffers with at least row_count slots, and host count outputs.
    let status = unsafe {
        bridge::pgaccel_expr_template_resident_star_dim_group_compact_f64_usm(
            fact_key_col,
            value_col,
            row_count,
            dim_match_by_key,
            dim_group_code_by_key,
            dim_key_count,
            value_cmp_opcode,
            value_const,
            out_group_codes,
            out_values,
            out_capacity,
            &raw mut selected_count,
            &raw mut uncertain_count,
        )
    };
    if status.is_ok() {
        Ok(ExprTemplateResidentStarDimGroupCompactF64 {
            selected_count,
            uncertain_count,
        })
    } else {
        Err(status)
    }
}

/// Template: evaluate `col1 <cmp1> const1 AND col2 <cmp2> const2` on a
/// batch via Agent 4A's struct-packed kernel (single dispatch, no Rust-side
/// AND combiner). Three-valued: `+1=TRUE, -1=FALSE, 0=UNCERTAIN`.
#[allow(dead_code)] // reason: fused Metal launch aborts on macOS; executor uses two cmp_const launches
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

/// Template: count TRUE rows for
/// `col1 <cmp1> const1 AND col2 <cmp2> const2` on a batch.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and_count(
    batch: &PgaccelBatch,
    col1_idx: u32,
    cmp1_opcode: u16,
    const1_val: f64,
    col2_idx: u32,
    cmp2_opcode: u16,
    const2_val: f64,
) -> Option<(usize, usize)> {
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: batch is a valid reference; output counters are valid pointers.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and_count(
            std::ptr::from_ref(batch),
            col1_idx,
            cmp1_opcode,
            const1_val,
            col2_idx,
            cmp2_opcode,
            const2_val,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Template: count TRUE rows for two already-staged shared-USM columns.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and_count_usm(
    col1: PgaccelExprUsmCol,
    cmp1_opcode: u16,
    const1_val: f64,
    col2: PgaccelExprUsmCol,
    cmp2_opcode: u16,
    const2_val: f64,
    row_count: usize,
) -> Option<(usize, usize)> {
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees both columns point at shared-USM buffers with
    // at least row_count elements and matching type tags.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and_count_usm(
            col1,
            cmp1_opcode,
            const1_val,
            col2,
            cmp2_opcode,
            const2_val,
            row_count,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Template: write a `1 = TRUE`, `0 = FALSE/NULL` selection mask for
/// `col1 <cmp1> const1 AND col2 <cmp2> const2` over shared-USM columns.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and_mask_usm(
    col1: PgaccelExprUsmCol,
    cmp1_opcode: u16,
    const1_val: f64,
    col2: PgaccelExprUsmCol,
    cmp2_opcode: u16,
    const2_val: f64,
    row_count: usize,
    selection: &mut ExprSharedBuffer<u8>,
) -> Option<(usize, usize)> {
    if selection.len() < row_count {
        return None;
    }

    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees both columns and selection point at shared-USM
    // buffers with at least row_count elements.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and_mask_usm(
            col1,
            cmp1_opcode,
            const1_val,
            col2,
            cmp2_opcode,
            const2_val,
            row_count,
            selection.as_mut_ptr(),
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some((true_count, uncertain_count))
}

/// Template: fuse
/// `col1 <cmp1> const1 AND col2 <cmp2> const2` with f32 SUM/MIN/MAX/COUNT.
#[allow(clippy::too_many_arguments)]
pub fn expr_template_two_pred_and_reduce_f32_usm(
    col1: PgaccelExprUsmCol,
    cmp1_opcode: u16,
    const1_val: f64,
    col2: PgaccelExprUsmCol,
    cmp2_opcode: u16,
    const2_val: f64,
    value_col: PgaccelExprUsmCol,
    row_count: usize,
) -> Option<ExprTemplateReduceF32> {
    let mut out_sum = 0.0f32;
    let mut out_min = 0.0f32;
    let mut out_max = 0.0f32;
    let mut out_value_count = 0i64;
    let mut true_count = 0usize;
    let mut uncertain_count = 0usize;

    // SAFETY: caller guarantees all columns point at shared-USM buffers with
    // at least row_count elements and matching type tags.
    let status = unsafe {
        bridge::pgaccel_expr_template_two_pred_and_reduce_f32_usm(
            col1,
            cmp1_opcode,
            const1_val,
            col2,
            cmp2_opcode,
            const2_val,
            value_col,
            row_count,
            &raw mut out_sum,
            &raw mut out_min,
            &raw mut out_max,
            &raw mut out_value_count,
            &raw mut true_count,
            &raw mut uncertain_count,
        )
    };
    status.is_ok().then_some(ExprTemplateReduceF32 {
        sum: out_sum,
        min: out_min,
        max: out_max,
        value_count: out_value_count,
        true_count,
        uncertain_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_allocation_bytes_rejects_zero_and_overflow() {
        assert_eq!(checked_allocation_bytes::<u8>(0), None);
        assert_eq!(
            checked_allocation_bytes::<u64>(usize::MAX / std::mem::size_of::<u64>() + 1),
            None
        );
        assert_eq!(checked_allocation_bytes::<u32>(4), Some(16));
    }
}

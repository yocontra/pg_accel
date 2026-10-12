//! Resident integer equijoin row mappings. This is an execution primitive, not
//! SQL planner admission. Input residency/currentness and work-size admission
//! belong to the future pipeline caller; there is no CPU join fallback here.

use std::ffi::c_void;
use std::marker::PhantomData;

use crate::engine::residency::ExecutionMemoryCharge;

use super::{
    ExprDeviceBuffer, GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail,
    PgaccelStatus, bridge, status_to_result,
};

const VERSION: u32 = 1;
const MAX_KEYS: usize = 4;
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // ABI sentinel; live oracle caller, pending pipeline admission.
pub const JOIN_NULL_ROW: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NativeJoinKey {
    values: *const c_void,
    nulls: *const u8,
    value_bytes: u64,
    null_bytes: u64,
    kind: u32,
    reserved: u32,
}

#[repr(C)]
pub struct NativeJoinRequest {
    version: u32,
    kind: u32,
    key_count: u32,
    reserved: u32,
    outer_rows: u64,
    inner_rows: u64,
    outer_keys: [NativeJoinKey; MAX_KEYS],
    inner_keys: [NativeJoinKey; MAX_KEYS],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JoinRowPair {
    pub outer_row: u64,
    /// JOIN_NULL_ROW for unmatched LEFT rows and every SEMI/ANTI row.
    pub inner_row: u64,
}

#[repr(C)]
#[derive(Default)]
pub struct NativeJoinResult {
    pub(super) required_rows: u64,
    pub(super) emitted_rows: u64,
    pub(super) kernel_launches: u64,
}

#[repr(C)]
pub struct NativeJoinControl {
    version: u32,
    reserved: u32,
    work_items: u64,
    chain_steps: u64,
    cancel_check: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    context: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
pub struct NativeControlledJoinResult {
    pub(super) rows: NativeJoinResult,
    pub(super) cancelled: u32,
    reserved: u32,
    pub(super) cancel_checks: u64,
}

/// Bounds device work independently of output cardinality. Prefix-scan work
/// items examine at most 256 rows; probe/emission items follow at most
/// `chain_steps` links, comparing at most four keys at each link. These are
/// admission inputs, not a wall-clock deadline or a production tuning policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinDispatchLimits {
    pub work_items: usize,
    pub chain_steps: usize,
}

impl JoinDispatchLimits {
    pub(super) fn native(self) -> GpuResult<NativeJoinControl> {
        if !(1..=i32::MAX as usize).contains(&self.work_items)
            || !(1..=i32::MAX as usize).contains(&self.chain_steps)
        {
            return Err(contract_error(
                "bounded join limits must be in 1..INT32_MAX",
            ));
        }
        Ok(NativeJoinControl {
            version: 1,
            reserved: 0,
            work_items: self.work_items as u64,
            chain_steps: self.chain_steps as u64,
            cancel_check: None,
            context: std::ptr::null_mut(),
        })
    }
}

struct CancelContext<'a> {
    check: &'a mut dyn FnMut() -> bool,
    checks: u64,
    panic: Option<Box<dyn std::any::Any + Send>>,
}

unsafe extern "C" fn poll_cancel(context: *mut c_void) -> i32 {
    // SAFETY: execute_controlled passes this live, exclusive stack context
    // through a synchronous native call. Native invokes it only on this host
    // thread, never retains it and never invokes it from a device kernel.
    let state = unsafe { &mut *context.cast::<CancelContext<'_>>() };
    state.checks = state.checks.saturating_add(1);
    // A Rust panic must never unwind across the C++ frames. Ask native to stop,
    // then resume the original panic after it returns and releases C++ state.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut state.check)) {
        Ok(cancel) => i32::from(cancel),
        Err(panic) => {
            state.panic = Some(panic);
            1
        }
    }
}

/// Shared synchronous FFI boundary for producer and projection. Only the
/// internal bridge caller sees control/result references; callbacks never
/// unwind across native frames. Native must not retain either reference.
pub(super) fn controlled_call(
    limits: JoinDispatchLimits,
    check: &mut dyn FnMut() -> bool,
    call: impl FnOnce(&NativeJoinControl, &mut NativeControlledJoinResult) -> PgaccelStatus,
) -> GpuResult<(PgaccelStatus, NativeControlledJoinResult, u64)> {
    let mut context = CancelContext {
        check,
        checks: 0,
        panic: None,
    };
    let mut control = limits.native()?;
    control.cancel_check = Some(poll_cancel);
    control.context = std::ptr::from_mut(&mut context).cast();
    let mut result = NativeControlledJoinResult::default();
    let status = call(&control, &mut result);
    if let Some(panic) = context.panic.take() {
        std::panic::resume_unwind(panic);
    }
    Ok((status, result, context.checks))
}

impl NativeControlledJoinResult {
    pub(super) fn validate_progress(&self, checks: u64, interrupted: bool) -> GpuResult<()> {
        if self.reserved != 0
            || self.cancelled > 1
            || self.cancel_checks != checks
            || self.rows.kernel_launches.checked_add(1) != Some(self.cancel_checks)
            || (self.cancelled == 1) != interrupted
            || (interrupted && self.rows.emitted_rows != 0)
        {
            return Err(contract_error("invalid bounded join progress metadata"));
        }
        Ok(())
    }
}

struct JoinAllocations {
    workspace: ExprDeviceBuffer<u64>,
    output: Option<ExprDeviceBuffer<JoinRowPair>>,
    _workspace_charge: ExecutionMemoryCharge,
    output_charge: ExecutionMemoryCharge,
}

impl JoinAllocations {
    fn new(workspace_bytes: usize, capacity: usize) -> GpuResult<Self> {
        let output_bytes = capacity
            .checked_mul(size_of::<JoinRowPair>())
            .ok_or_else(|| contract_error("join output capacity overflows bytes"))?;
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let workspace_charge = ExecutionMemoryCharge::reserve(workspace_bytes as u64, budget)
            .map_err(|_| allocation_error("join workspace exceeds resident memory budget"))?;
        let output_charge = ExecutionMemoryCharge::reserve(output_bytes as u64, budget)
            .map_err(|_| allocation_error("join output exceeds resident memory budget"))?;
        let workspace = ExprDeviceBuffer::new(workspace_bytes / size_of::<u64>())
            .ok_or_else(|| allocation_error("join workspace allocation failed"))?;
        let output = if capacity == 0 {
            None
        } else {
            Some(
                ExprDeviceBuffer::new(capacity)
                    .ok_or_else(|| allocation_error("join output allocation failed"))?,
            )
        };
        Ok(Self {
            workspace,
            output,
            _workspace_charge: workspace_charge,
            output_charge,
        })
    }

    fn output_ptr(&self) -> *mut JoinRowPair {
        self.output
            .as_ref()
            .map_or(std::ptr::null_mut(), ExprDeviceBuffer::as_mut_ptr)
    }

    fn finish(self, result: NativeJoinResult, request: &NativeJoinRequest) -> ResidentJoinOutput {
        // Unmoved workspace fields drop here, in declaration order, so their
        // device memory remains charged through destruction. Only output lives.
        ResidentJoinOutput {
            buffer: self.output,
            rows: result.emitted_rows as usize,
            outer_rows: request.outer_rows as usize,
            inner_rows: request.inner_rows as usize,
            kernel_launches: result.kernel_launches,
            _charge: self.output_charge,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(u32)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Native producer kinds; live-tested before SQL wiring.
pub enum ResidentJoinKind {
    Inner = 0,
    Left = 1,
    Semi = 2,
    Anti = 3,
}

/// A typed borrow of already-resident keys. None represents an empty column,
/// not a NULL column; a missing sidecar means all values are non-NULL.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Typed native producer inputs; pending pipeline caller.
pub enum ResidentJoinKey<'a> {
    Int32 {
        values: Option<&'a ExprDeviceBuffer<i32>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    Int64 {
        values: Option<&'a ExprDeviceBuffer<i64>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
}

fn contract_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::HashJoin,
        GpuOperation::ValidateDeviceInput,
        GpuStatusDetail::ShapeMismatch,
        detail,
    )
}

fn allocation_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::HashJoin,
        GpuOperation::BuildColumnBatch,
        GpuStatusDetail::OutOfMemory,
        detail,
    )
}

fn bind_key<T>(
    values: Option<&ExprDeviceBuffer<T>>,
    nulls: Option<&ExprDeviceBuffer<u8>>,
    kind: u32,
) -> GpuResult<(NativeJoinKey, usize)> {
    let rows = values.map_or(0, ExprDeviceBuffer::len);
    if rows > i32::MAX as usize || nulls.is_some_and(|sidecar| sidecar.len() != rows) {
        return Err(contract_error("join input row/NULL lengths are invalid"));
    }
    let bytes = rows
        .checked_mul(size_of::<T>())
        .ok_or_else(|| contract_error("join input byte count overflow"))?;
    Ok((
        NativeJoinKey {
            values: values.map_or(std::ptr::null(), |buffer| buffer.as_ptr().cast()),
            nulls: nulls.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            value_bytes: bytes as u64,
            null_bytes: nulls.map_or(0, |buffer| buffer.len() as u64),
            kind,
            reserved: 0,
        },
        rows,
    ))
}

impl ResidentJoinKey<'_> {
    fn bind(self) -> GpuResult<(NativeJoinKey, usize)> {
        match self {
            Self::Int32 { values, nulls } => bind_key(values, nulls, 1),
            Self::Int64 { values, nulls } => bind_key(values, nulls, 2),
        }
    }
}

/// Borrows every input through the synchronous dispatch. Raw descriptors never
/// escape to a plan wire, another backend or a sendable owner.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Live-tested resident producer owner, not planner-selected yet.
pub struct ResidentJoin<'a> {
    request: NativeJoinRequest,
    _inputs: PhantomData<ResidentJoinKey<'a>>,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged facade exercised by live PostgreSQL oracle tests.
impl<'a> ResidentJoin<'a> {
    pub fn count_controlled(
        &self,
        limits: JoinDispatchLimits,
        check: &mut dyn FnMut() -> bool,
    ) -> GpuResult<ControlledJoinCountOutcome> {
        let workspace_bytes = self.controlled_workspace_bytes(limits)?;
        let allocations = JoinAllocations::new(workspace_bytes, 0)?;
        // SAFETY: source borrows and charged, disjoint workspace outlive the
        // synchronous count call. No pair pointer/output exists. The shared
        // shim contains callback panics until C++ returns and owners can drop.
        let (status, result, checks) = controlled_call(limits, check, |control, result| unsafe {
            bridge::pgaccel_join_rows_count_controlled(
                &raw const self.request,
                control,
                allocations.workspace.as_mut_ptr().cast(),
                workspace_bytes as u64,
                result,
            )
        })?;
        let interrupted = status == PgaccelStatus::ErrorTimeout && result.cancelled == 1;
        if !interrupted {
            status_to_result(
                status,
                GpuErrorDomain::HashJoin,
                GpuOperation::Kernel("pgaccel_join_rows_count_controlled"),
            )?;
        }
        result.validate_progress(checks, interrupted)?;
        if result.rows.emitted_rows != 0 {
            return Err(contract_error("count-only join unexpectedly emitted pairs"));
        }
        if interrupted {
            Ok(ControlledJoinCountOutcome::Interrupted {
                kernel_launches: result.rows.kernel_launches,
                cancel_checks: result.cancel_checks,
            })
        } else {
            Ok(ControlledJoinCountOutcome::Complete {
                rows: result.rows.required_rows,
                kernel_launches: result.rows.kernel_launches,
            })
        }
    }

    pub fn new(
        kind: ResidentJoinKind,
        outer: &[ResidentJoinKey<'a>],
        inner: &[ResidentJoinKey<'a>],
    ) -> GpuResult<Self> {
        if outer.is_empty() || outer.len() > MAX_KEYS || outer.len() != inner.len() {
            return Err(contract_error(
                "join requires one to four matching key pairs",
            ));
        }
        let mut request = NativeJoinRequest {
            version: VERSION,
            kind: kind as u32,
            key_count: outer.len() as u32,
            reserved: 0,
            outer_rows: 0,
            inner_rows: 0,
            outer_keys: [NativeJoinKey::default(); MAX_KEYS],
            inner_keys: [NativeJoinKey::default(); MAX_KEYS],
        };
        for (index, (left, right)) in outer.iter().zip(inner).enumerate() {
            let (left, left_rows) = left.bind()?;
            let (right, right_rows) = right.bind()?;
            if left.kind != right.kind
                || (index != 0
                    && (request.outer_rows != left_rows as u64
                        || request.inner_rows != right_rows as u64))
            {
                return Err(contract_error("join key types or column lengths disagree"));
            }
            request.outer_rows = left_rows as u64;
            request.inner_rows = right_rows as u64;
            request.outer_keys[index] = left;
            request.inner_keys[index] = right;
        }
        Ok(Self {
            request,
            _inputs: PhantomData,
        })
    }

    /// Exact device workspace bytes; does not initialize or allocate a GPU.
    pub fn workspace_bytes(&self) -> GpuResult<usize> {
        let mut bytes = 0;
        // SAFETY: request is an initialized ABI mirror with typed borrowed
        // buffers; bytes is a disjoint host scalar. The query reads no rows.
        let status = unsafe {
            bridge::pgaccel_join_rows_workspace_bytes(&raw const self.request, &raw mut bytes)
        };
        status_to_result(
            status,
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel("pgaccel_join_rows_workspace_bytes"),
        )?;
        if bytes == 0 || bytes % size_of::<u64>() as u64 != 0 {
            return Err(contract_error("native join workspace is not u64-aligned"));
        }
        usize::try_from(bytes).map_err(|_| contract_error("join workspace exceeds address space"))
    }

    /// Caller admits bounded work before dispatch. Capacity is a hard row-pair
    /// limit, not an estimate: exhaustion returns metadata without partial rows.
    /// All scratch/output bytes are reserved before either allocation. Input
    /// buffers remain charged by their residency owner, not charged twice here.
    pub fn execute(&self, capacity: usize) -> GpuResult<ResidentJoinOutcome> {
        let workspace_bytes = self.workspace_bytes()?;
        let allocations = JoinAllocations::new(workspace_bytes, capacity)?;
        let mut result = NativeJoinResult::default();
        // SAFETY: typed borrowed inputs outlive this synchronous call. Scratch
        // and output are fresh, disjoint, aligned allocations of the declared
        // sizes; stack request/result are disjoint. Empty output is canonical.
        let status = unsafe {
            bridge::pgaccel_join_rows_execute(
                &raw const self.request,
                allocations.workspace.as_mut_ptr().cast(),
                workspace_bytes as u64,
                allocations.output_ptr(),
                capacity as u64,
                &raw mut result,
            )
        };
        if status == PgaccelStatus::ErrorOom
            && result.required_rows > capacity as u64
            && result.emitted_rows == 0
        {
            return Ok(ResidentJoinOutcome::NeedsCapacity {
                required_rows: result.required_rows,
                kernel_launches: result.kernel_launches,
            });
        }
        status_to_result(
            status,
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel("pgaccel_join_rows_execute"),
        )?;
        if result.emitted_rows != result.required_rows || result.emitted_rows > capacity as u64 {
            return Err(contract_error(
                "native join returned invalid output cardinality",
            ));
        }
        Ok(ResidentJoinOutcome::Complete(
            allocations.finish(result, &self.request),
        ))
    }

    pub fn controlled_workspace_bytes(&self, limits: JoinDispatchLimits) -> GpuResult<usize> {
        let control = limits.native()?;
        let mut bytes = 0;
        // SAFETY: initialized disjoint host ABI values. The layout query does
        // not invoke a callback, read rows, allocate or initialize the device.
        let status = unsafe {
            bridge::pgaccel_join_rows_controlled_workspace_bytes(
                &raw const self.request,
                &raw const control,
                &raw mut bytes,
            )
        };
        status_to_result(
            status,
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel("pgaccel_join_rows_controlled_workspace_bytes"),
        )?;
        if bytes == 0 || bytes % size_of::<u64>() as u64 != 0 {
            return Err(contract_error("bounded join workspace is not u64-aligned"));
        }
        usize::try_from(bytes)
            .map_err(|_| contract_error("bounded join workspace exceeds address space"))
    }

    /// Poll before dispatch and after each bounded device launch. The callback
    /// must be host-only and may read a cancellation flag, but must NOT call
    /// PostgreSQL APIs, longjmp, or mutate inputs. Raise PostgreSQL interrupts
    /// only after this returns and after releasing any residency borrow.
    /// Rust callback panics resume on the Rust side after native has stopped.
    pub fn execute_controlled(
        &self,
        capacity: usize,
        limits: JoinDispatchLimits,
        check: &mut dyn FnMut() -> bool,
    ) -> GpuResult<ControlledJoinOutcome> {
        let workspace_bytes = self.controlled_workspace_bytes(limits)?;
        let allocations = JoinAllocations::new(workspace_bytes, capacity)?;
        // SAFETY: all borrowed inputs, exclusive host context/control/result
        // and charged, disjoint device allocations live through the synchronous
        // call. The callback catches Rust panics and does not unwind into C++.
        let (status, result, checks) = controlled_call(limits, check, |control, result| unsafe {
            bridge::pgaccel_join_rows_execute_controlled(
                &raw const self.request,
                control,
                allocations.workspace.as_mut_ptr().cast(),
                workspace_bytes as u64,
                allocations.output_ptr(),
                capacity as u64,
                result,
            )
        })?;
        let interrupted = status == PgaccelStatus::ErrorTimeout && result.cancelled == 1;
        let needs_capacity = status == PgaccelStatus::ErrorOom
            && result.rows.required_rows > capacity as u64
            && result.rows.emitted_rows == 0;
        if !interrupted && !needs_capacity {
            status_to_result(
                status,
                GpuErrorDomain::HashJoin,
                GpuOperation::Kernel("pgaccel_join_rows_execute_controlled"),
            )?;
        }
        result.validate_progress(checks, interrupted)?;
        if interrupted {
            return Ok(ControlledJoinOutcome::Interrupted {
                kernel_launches: result.rows.kernel_launches,
                cancel_checks: result.cancel_checks,
            });
        }
        if needs_capacity {
            return Ok(ControlledJoinOutcome::Finished(
                ResidentJoinOutcome::NeedsCapacity {
                    required_rows: result.rows.required_rows,
                    kernel_launches: result.rows.kernel_launches,
                },
            ));
        }
        if result.rows.emitted_rows != result.rows.required_rows
            || result.rows.emitted_rows > capacity as u64
        {
            return Err(contract_error(
                "bounded join returned invalid output cardinality",
            ));
        }
        Ok(ControlledJoinOutcome::Finished(
            ResidentJoinOutcome::Complete(allocations.finish(result.rows, &self.request)),
        ))
    }
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged bounded producer; no SQL admission yet.
pub enum ControlledJoinOutcome {
    Finished(ResidentJoinOutcome),
    Interrupted {
        kernel_launches: u64,
        cancel_checks: u64,
    },
}

#[derive(Debug)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged cardinality driver, pending live qualification.
pub enum ControlledJoinCountOutcome {
    Complete {
        rows: u64,
        kernel_launches: u64,
    },
    Interrupted {
        kernel_launches: u64,
        cancel_checks: u64,
    },
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Capacity outcome for staged resident producer.
pub enum ResidentJoinOutcome {
    Complete(ResidentJoinOutput),
    NeedsCapacity {
        required_rows: u64,
        kernel_launches: u64,
    },
}

/// Only successful initialized rows are exposed. Device allocation precedes
/// the charge field so drop frees memory before releasing its ledger charge.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Charged resident result, pending pipeline consumer.
pub struct ResidentJoinOutput {
    buffer: Option<ExprDeviceBuffer<JoinRowPair>>,
    rows: usize,
    outer_rows: usize,
    inner_rows: usize,
    kernel_launches: u64,
    _charge: ExecutionMemoryCharge,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Live-tested output boundary; no production caller yet.
impl ResidentJoinOutput {
    pub(super) fn origin_rows(&self) -> (usize, usize) {
        (self.outer_rows, self.inner_rows)
    }
    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn kernel_launches(&self) -> u64 {
        self.kernel_launches
    }

    /// GPU consumer input, valid for len() row pairs while this owner lives.
    /// No row data is copied to the host by the producer.
    pub fn as_device_ptr(&self) -> *const JoinRowPair {
        self.buffer
            .as_ref()
            .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr)
    }

    /// Explicit final materialization boundary, also used by oracle tests.
    /// Intermediate pipeline consumers must use the resident pointer instead.
    pub fn copy_final_rows(&self, output: &mut [JoinRowPair]) -> GpuResult<()> {
        if output.len() != self.rows {
            return Err(contract_error(
                "join final output length must equal initialized rows",
            ));
        }
        if let Some(buffer) = &self.buffer {
            buffer.copy_prefix_to_slice(output)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(ResidentJoin<'static>: Send, Sync);
    assert_not_impl_any!(ResidentJoinOutput: Send, Sync);

    #[test]
    fn resident_join_abi_layout() {
        assert_eq!(size_of::<NativeJoinKey>(), 40);
        assert_eq!(size_of::<NativeJoinRequest>(), 352);
        assert_eq!(size_of::<JoinRowPair>(), 16);
        assert_eq!(size_of::<NativeJoinResult>(), 24);
        assert_eq!(size_of::<NativeJoinControl>(), 40);
        assert_eq!(size_of::<NativeControlledJoinResult>(), 40);
        assert_eq!(align_of::<NativeJoinControl>(), 8);
        assert_eq!(align_of::<NativeControlledJoinResult>(), 8);
        assert_eq!(std::mem::offset_of!(NativeJoinKey, kind), 32);
        assert_eq!(std::mem::offset_of!(NativeJoinRequest, outer_keys), 32);
        assert_eq!(std::mem::offset_of!(NativeJoinRequest, inner_keys), 192);
        assert_eq!(std::mem::offset_of!(NativeJoinResult, kernel_launches), 16);
        assert_eq!(std::mem::offset_of!(NativeJoinControl, work_items), 8);
        assert_eq!(std::mem::offset_of!(NativeJoinControl, chain_steps), 16);
        assert_eq!(std::mem::offset_of!(NativeJoinControl, cancel_check), 24);
        assert_eq!(std::mem::offset_of!(NativeJoinControl, context), 32);
        assert_eq!(
            std::mem::offset_of!(NativeControlledJoinResult, cancelled),
            24
        );
        assert_eq!(
            std::mem::offset_of!(NativeControlledJoinResult, cancel_checks),
            32
        );
        assert_eq!(VERSION, 1);
    }

    #[test]
    fn controlled_join_limits_reject_unbounded_or_empty_dispatches() {
        for invalid in [0, i32::MAX as usize + 1, usize::MAX] {
            assert!(
                JoinDispatchLimits {
                    work_items: invalid,
                    chain_steps: 1
                }
                .native()
                .is_err()
            );
            assert!(
                JoinDispatchLimits {
                    work_items: 1,
                    chain_steps: invalid
                }
                .native()
                .is_err()
            );
        }
        for valid in [1, 17, i32::MAX as usize] {
            let control = JoinDispatchLimits {
                work_items: valid,
                chain_steps: valid,
            }
            .native()
            .expect("valid dispatch limits");
            assert_eq!(control.version, 1);
            assert_eq!(control.reserved, 0);
            assert_eq!(control.work_items, valid as u64);
            assert_eq!(control.chain_steps, valid as u64);
            assert!(control.cancel_check.is_none());
            assert!(control.context.is_null());
        }
    }

    #[test]
    fn count_only_preflight_rejects_invalid_metadata_without_gpu_or_result_writes() {
        for mutation in 0..8 {
            let mut request = NativeJoinRequest {
                version: VERSION,
                kind: 0,
                key_count: 1,
                reserved: 0,
                outer_rows: 0,
                inner_rows: 0,
                outer_keys: [NativeJoinKey::default(); MAX_KEYS],
                inner_keys: [NativeJoinKey::default(); MAX_KEYS],
            };
            request.outer_keys[0].kind = 1;
            request.inner_keys[0].kind = 1;
            let mut control = JoinDispatchLimits {
                work_items: 1,
                chain_steps: 1,
            }
            .native()
            .expect("limits");
            let mut workspace = [0_u64; 64];
            let mut bytes = size_of_val(&workspace) as u64;
            let mut result = NativeControlledJoinResult::default();
            result.rows.required_rows = 83;
            result.rows.emitted_rows = 89;
            let mut result_pointer = std::ptr::from_mut(&mut result);
            match mutation {
                0 => request.version = 2,
                1 => control.version = 2,
                2 => control.reserved = 1,
                3 => request.key_count = 0,
                4 => control.work_items = 0,
                5 => bytes = 0,
                6 => result_pointer = std::ptr::null_mut(),
                7 => result_pointer = std::ptr::from_ref(&control).cast_mut().cast(),
                _ => unreachable!(),
            }
            // SAFETY: every non-null pointer names initialized aligned host
            // storage. Each mutation violates preflight before queue access or
            // result writes; the intentionally aliased result must be rejected.
            let status = unsafe {
                bridge::pgaccel_join_rows_count_controlled(
                    &raw const request,
                    &raw const control,
                    workspace.as_mut_ptr().cast(),
                    bytes,
                    result_pointer,
                )
            };
            assert_eq!(
                status,
                if mutation < 2 {
                    PgaccelStatus::ErrorUnsupported
                } else {
                    PgaccelStatus::InvalidArgument
                }
            );
            assert_eq!(result.rows.required_rows, 83);
            assert_eq!(result.rows.emitted_rows, 89);
            assert_eq!(workspace, [0; 64]);
        }
    }

    #[test]
    fn controlled_join_callback_contains_panic_without_crossing_ffi() {
        let mut calls = 0;
        let mut check = || {
            calls += 1;
            calls == 2
        };
        let mut context = CancelContext {
            check: &mut check,
            checks: 0,
            panic: None,
        };
        let pointer = std::ptr::from_mut(&mut context).cast();
        // SAFETY: live exclusive context on this thread, exactly as in dispatch.
        unsafe {
            assert_eq!(poll_cancel(pointer), 0);
            assert_eq!(poll_cancel(pointer), 1);
        }
        assert_eq!(context.checks, 2);
        assert!(context.panic.is_none());

        let mut panic_check = || std::panic::panic_any(97_u32);
        let mut context = CancelContext {
            check: &mut panic_check,
            checks: 0,
            panic: None,
        };
        // SAFETY: same synchronous, exclusive stack-context contract as above.
        assert_eq!(
            unsafe { poll_cancel(std::ptr::from_mut(&mut context).cast()) },
            1
        );
        assert_eq!(context.checks, 1);
        let payload = context.panic.take().expect("original panic retained");
        assert_eq!(payload.downcast_ref::<u32>(), Some(&97));
    }

    #[test]
    fn resident_join_metadata_rejects_bad_key_pairs() {
        let empty = ResidentJoinKey::Int32 {
            values: None,
            nulls: None,
        };
        let wide = ResidentJoinKey::Int64 {
            values: None,
            nulls: None,
        };
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &[], &[]).is_err());
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &[empty; 5], &[empty; 5]).is_err());
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &[empty], &[empty; 2]).is_err());
        assert!(ResidentJoin::new(ResidentJoinKind::Inner, &[empty], &[wide]).is_err());
        for kind in [
            ResidentJoinKind::Inner,
            ResidentJoinKind::Left,
            ResidentJoinKind::Semi,
            ResidentJoinKind::Anti,
        ] {
            let plan =
                ResidentJoin::new(kind, &[empty, wide], &[empty, wide]).expect("empty typed plan");
            assert_eq!(
                plan.workspace_bytes().expect("empty workspace forecast"),
                64
            );
            assert_eq!(
                plan.controlled_workspace_bytes(JoinDispatchLimits {
                    work_items: 3,
                    chain_steps: 2,
                })
                .expect("empty controlled workspace forecast"),
                64
            );
        }
    }

    #[test]
    fn controlled_progress_rejects_partial_publication_and_corrupt_accounting() {
        for mutation in 0..7 {
            let mut result = NativeControlledJoinResult {
                rows: NativeJoinResult {
                    required_rows: 9,
                    emitted_rows: 0,
                    kernel_launches: 3,
                },
                cancelled: 1,
                reserved: 0,
                cancel_checks: 4,
            };
            assert!(result.validate_progress(4, true).is_ok());
            match mutation {
                0 => result.reserved = 1,
                1 => result.cancelled = 2,
                2 => result.cancelled = 0,
                3 => result.cancel_checks = 3,
                4 => result.rows.kernel_launches = 4,
                5 => result.rows.emitted_rows = 1,
                6 => result.rows.kernel_launches = u64::MAX,
                _ => unreachable!(),
            }
            assert!(result.validate_progress(4, true).is_err());
        }
        let result = NativeControlledJoinResult {
            rows: NativeJoinResult {
                required_rows: 9,
                emitted_rows: 9,
                kernel_launches: 3,
            },
            cancelled: 0,
            reserved: 0,
            cancel_checks: 4,
        };
        assert!(result.validate_progress(4, false).is_ok());
        assert!(result.validate_progress(5, false).is_err());
        assert!(result.validate_progress(4, true).is_err());
    }

    #[test]
    fn controlled_callback_panic_resumes_only_after_bridge_returns() {
        let returned = std::cell::Cell::new(false);
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = controlled_call(
                JoinDispatchLimits {
                    work_items: 1,
                    chain_steps: 1,
                },
                &mut || std::panic::panic_any(113_u32),
                |control, _| {
                    // SAFETY: controlled_call owns this exclusive stack
                    // context throughout the synchronous bridge closure.
                    let stop = unsafe {
                        control.cancel_check.expect("installed callback")(control.context)
                    };
                    assert_eq!(stop, 1);
                    returned.set(true);
                    PgaccelStatus::ErrorTimeout
                },
            );
        }))
        .expect_err("original panic resumed on Rust side");
        assert!(returned.get());
        assert_eq!(observed.downcast_ref::<u32>(), Some(&113));
    }
}

//! Device-only gather from resident join mappings into contiguous consumer
//! lanes. This connects execution primitives; it does not add a SQL path.

use std::ffi::c_void;

use crate::engine::residency::ExecutionMemoryCharge;

use super::join_rows::{JoinDispatchLimits, JoinRowPair, ResidentJoinOutput, controlled_call};
use super::{
    ExprDeviceBuffer, GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail,
    PgaccelStatus, bridge, status_to_result,
};

const MAX_COLUMNS: usize = 16;
pub const JOIN_PROJECT_WORKSPACE_BYTES: usize = 8 + MAX_COLUMNS * size_of::<NativeProjectColumn>();

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NativeProjectColumn {
    side: u32,
    width: u32,
    values: *const c_void,
    nulls: *const u8,
    value_bytes: u64,
    null_bytes: u64,
    output_values: *mut c_void,
    output_nulls: *mut u8,
    output_value_bytes: u64,
    output_null_bytes: u64,
}

#[repr(C)]
pub struct NativeProjectRequest {
    version: u32,
    column_count: u32,
    reserved: u64,
    outer_rows: u64,
    inner_rows: u64,
    row_count: u64,
    pairs: *const JoinRowPair,
    pair_bytes: u64,
    columns: [NativeProjectColumn; MAX_COLUMNS],
}

#[repr(C)]
#[derive(Default)]
pub struct NativeProjectResult {
    emitted_rows: u64,
    kernel_launches: u64,
}

#[derive(Clone, Copy)]
#[repr(u32)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged pipeline input identity, tested directly before SQL admission.
pub enum JoinSide {
    Outer = 0,
    Inner = 1,
}

/// Byte lanes also carry BOOL. INT4 lanes can carry DATE bits, but this gather
/// does not reinterpret SQL types or authorize casts; consumers retain types.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Typed borrowed source columns for the staged gather.
pub enum JoinProjectionColumn<'a> {
    Int32 {
        values: Option<&'a ExprDeviceBuffer<i32>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    Int64 {
        values: Option<&'a ExprDeviceBuffer<i64>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    Byte {
        values: Option<&'a ExprDeviceBuffer<u8>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
}

#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Pending planner binding; live pipeline tests exercise both sides.
pub struct JoinProjectionInput<'a> {
    pub side: JoinSide,
    pub column: JoinProjectionColumn<'a>,
}

fn error(status: GpuStatusDetail, detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::HashJoin,
        GpuOperation::ValidateDeviceInput,
        status,
        detail,
    )
}

fn bind<T>(
    values: Option<&ExprDeviceBuffer<T>>,
    nulls: Option<&ExprDeviceBuffer<u8>>,
    rows: usize,
) -> GpuResult<NativeProjectColumn> {
    if values.map_or(0, ExprDeviceBuffer::len) != rows || nulls.is_some_and(|n| n.len() != rows) {
        return Err(error(
            GpuStatusDetail::ShapeMismatch,
            "projection input does not match join side row domain",
        ));
    }
    let bytes = rows.checked_mul(size_of::<T>()).ok_or_else(|| {
        error(
            GpuStatusDetail::CapacityOverflow,
            "projection source bytes overflow",
        )
    })?;
    Ok(NativeProjectColumn {
        width: size_of::<T>() as u32,
        values: values.map_or(std::ptr::null(), |v| v.as_ptr().cast()),
        nulls: nulls.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
        value_bytes: bytes as u64,
        null_bytes: nulls.map_or(0, |n| n.len() as u64),
        ..NativeProjectColumn::default()
    })
}

/// Initialized device lanes owned by a successful projection. No host slice
/// is exposed; their null sidecar is canonical and includes outer-join padding.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged contiguous consumer input, verified in live aggregate tests.
pub struct ResidentProjectedColumn<T> {
    values: Option<ExprDeviceBuffer<T>>,
    nulls: Option<ExprDeviceBuffer<u8>>,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Typed resident consumers, not yet planner-bound.
impl<T> ResidentProjectedColumn<T> {
    pub fn values(&self) -> Option<&ExprDeviceBuffer<T>> {
        self.values.as_ref()
    }
    pub fn nulls(&self) -> Option<&ExprDeviceBuffer<u8>> {
        self.nulls.as_ref()
    }

    fn allocate(rows: usize, column: &mut NativeProjectColumn) -> GpuResult<Self> {
        if rows == 0 {
            return Ok(Self {
                values: None,
                nulls: None,
            });
        }
        let values = ExprDeviceBuffer::<T>::new(rows).ok_or_else(|| {
            error(
                GpuStatusDetail::OutOfMemory,
                "projection values allocation failed",
            )
        })?;
        let nulls = ExprDeviceBuffer::<u8>::new(rows).ok_or_else(|| {
            error(
                GpuStatusDetail::OutOfMemory,
                "projection nulls allocation failed",
            )
        })?;
        column.output_values = values.as_mut_ptr().cast();
        column.output_nulls = nulls.as_mut_ptr();
        column.output_value_bytes = (rows * size_of::<T>()) as u64;
        column.output_null_bytes = rows as u64;
        Ok(Self {
            values: Some(values),
            nulls: Some(nulls),
        })
    }
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Resident result variants for future pipeline consumers.
pub enum ProjectedJoinColumn {
    Int32(ResidentProjectedColumn<i32>),
    Int64(ResidentProjectedColumn<i64>),
    Byte(ResidentProjectedColumn<u8>),
}

/// Fixed-capacity metadata avoids an uncharged heap metadata allocation. The
/// output charge is dropped after all of its device buffers, never before.
#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Live-tested owner awaiting production pipeline admission.
pub struct ResidentJoinProjection {
    columns: [Option<ProjectedJoinColumn>; MAX_COLUMNS],
    column_count: usize,
    rows: usize,
    kernel_launches: u64,
    allocated_bytes: usize,
    _charge: Option<ExecutionMemoryCharge>,
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged bounded join consumer; not SQL admission.
#[allow(clippy::large_enum_variant)] // Inline, fixed-capacity metadata avoids a heap allocation under the store borrow.
pub enum ControlledProjectionOutcome {
    Complete(ResidentJoinProjection),
    Interrupted {
        kernel_launches: u64,
        cancel_checks: u64,
    },
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged consumer-facing output accessors.
impl ResidentJoinProjection {
    pub fn column(&self, index: usize) -> Option<&ProjectedJoinColumn> {
        self.columns.get(index).and_then(Option::as_ref)
    }
    pub fn column_count(&self) -> usize {
        self.column_count
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
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes
    }

    fn row_domain(rows: usize, check: &mut dyn FnMut() -> bool) -> ControlledProjectionOutcome {
        if check() {
            return ControlledProjectionOutcome::Interrupted {
                kernel_launches: 0,
                cancel_checks: 1,
            };
        }
        ControlledProjectionOutcome::Complete(Self {
            columns: std::array::from_fn(|_| None),
            column_count: 0,
            rows,
            kernel_launches: 0,
            allocated_bytes: 0,
            _charge: None,
        })
    }
}

#[cfg_attr(not(feature = "pg_test"), allow(dead_code))] // Staged join-to-consumer bridge; no host row projection.
impl ResidentJoinOutput {
    /// Constant-only consumers need no gathered lanes. The producer has already
    /// established the row count on-device; this publishes metadata, not a host
    /// join or aggregate. No scratch/output allocation or device launch occurs.
    pub(crate) fn project_row_domain_controlled(
        &self,
        check: &mut dyn FnMut() -> bool,
    ) -> ControlledProjectionOutcome {
        ResidentJoinProjection::row_domain(self.len(), check)
    }

    pub fn project(&self, inputs: &[JoinProjectionInput<'_>]) -> GpuResult<ResidentJoinProjection> {
        match self.project_impl(inputs, None)? {
            ControlledProjectionOutcome::Complete(output) => Ok(output),
            ControlledProjectionOutcome::Interrupted { .. } => Err(error(
                GpuStatusDetail::ShapeMismatch,
                "uncontrolled projection reported cancellation",
            )),
        }
    }

    /// Bound row-index validation and gather launches, polling before work and
    /// after each fill/kernel. As for execute_controlled, the callback must not
    /// call PostgreSQL APIs or longjmp; raise interrupts after releasing borrows.
    pub fn project_controlled(
        &self,
        inputs: &[JoinProjectionInput<'_>],
        limits: JoinDispatchLimits,
        check: &mut dyn FnMut() -> bool,
    ) -> GpuResult<ControlledProjectionOutcome> {
        limits.native()?; // Reject invalid limits before reservations/allocations.
        self.project_impl(inputs, Some((limits, check)))
    }

    fn project_impl(
        &self,
        inputs: &[JoinProjectionInput<'_>],
        control: Option<(JoinDispatchLimits, &mut dyn FnMut() -> bool)>,
    ) -> GpuResult<ControlledProjectionOutcome> {
        if inputs.is_empty() || inputs.len() > MAX_COLUMNS {
            return Err(error(
                GpuStatusDetail::ShapeMismatch,
                "projection requires one to sixteen columns",
            ));
        }
        let (outer_rows, inner_rows) = self.origin_rows();
        let mut request = NativeProjectRequest {
            version: 1,
            column_count: inputs.len() as u32,
            reserved: 0,
            outer_rows: outer_rows as u64,
            inner_rows: inner_rows as u64,
            row_count: self.len() as u64,
            pairs: if self.is_empty() {
                std::ptr::null()
            } else {
                self.as_device_ptr()
            },
            pair_bytes: (self.len() * size_of::<JoinRowPair>()) as u64,
            columns: [NativeProjectColumn::default(); MAX_COLUMNS],
        };
        let mut bytes_per_row = 0_usize;
        for (index, input) in inputs.iter().enumerate() {
            let rows = match input.side {
                JoinSide::Outer => outer_rows,
                JoinSide::Inner => inner_rows,
            };
            let mut column = match input.column {
                JoinProjectionColumn::Int32 { values, nulls } => bind(values, nulls, rows)?,
                JoinProjectionColumn::Int64 { values, nulls } => bind(values, nulls, rows)?,
                JoinProjectionColumn::Byte { values, nulls } => bind(values, nulls, rows)?,
            };
            column.side = input.side as u32;
            bytes_per_row += column.width as usize + 1;
            request.columns[index] = column;
        }
        let output_bytes = self.len().checked_mul(bytes_per_row).ok_or_else(|| {
            error(
                GpuStatusDetail::CapacityOverflow,
                "projected output bytes overflow",
            )
        })?;
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let _scratch_charge =
            ExecutionMemoryCharge::reserve(JOIN_PROJECT_WORKSPACE_BYTES as u64, budget).map_err(
                |_| {
                    error(
                        GpuStatusDetail::OutOfMemory,
                        "projection scratch budget exceeded",
                    )
                },
            )?;
        let charge = ExecutionMemoryCharge::reserve(output_bytes as u64, budget).map_err(|_| {
            error(
                GpuStatusDetail::OutOfMemory,
                "projection output budget exceeded",
            )
        })?;
        let scratch =
            ExprDeviceBuffer::<u64>::new(JOIN_PROJECT_WORKSPACE_BYTES / 8).ok_or_else(|| {
                error(
                    GpuStatusDetail::OutOfMemory,
                    "projection scratch allocation failed",
                )
            })?;
        let mut columns = std::array::from_fn(|_| None);
        for (index, input) in inputs.iter().enumerate() {
            let column = &mut request.columns[index];
            columns[index] = Some(match input.column {
                JoinProjectionColumn::Int32 { .. } => ProjectedJoinColumn::Int32(
                    ResidentProjectedColumn::allocate(self.len(), column)?,
                ),
                JoinProjectionColumn::Int64 { .. } => ProjectedJoinColumn::Int64(
                    ResidentProjectedColumn::allocate(self.len(), column)?,
                ),
                JoinProjectionColumn::Byte { .. } => ProjectedJoinColumn::Byte(
                    ResidentProjectedColumn::allocate(self.len(), column)?,
                ),
            });
        }
        let result = if let Some((limits, check)) = control {
            // SAFETY: initialized pair prefix and typed source borrows live
            // throughout dispatch. All output/scratch allocations are disjoint
            // and charged. controlled_call owns the synchronous host callback
            // context and catches panics before they can cross C++ frames.
            let (status, result, checks) =
                controlled_call(limits, check, |control, result| unsafe {
                    bridge::pgaccel_join_rows_project_controlled(
                        &raw const request,
                        control,
                        scratch.as_mut_ptr().cast(),
                        JOIN_PROJECT_WORKSPACE_BYTES as u64,
                        result,
                    )
                })?;
            let interrupted = status == PgaccelStatus::ErrorTimeout && result.cancelled == 1;
            if !interrupted {
                status_to_result(
                    status,
                    GpuErrorDomain::HashJoin,
                    GpuOperation::Kernel("pgaccel_join_rows_project_controlled"),
                )?;
            }
            result.validate_progress(checks, interrupted)?;
            if interrupted {
                return Ok(ControlledProjectionOutcome::Interrupted {
                    kernel_launches: result.rows.kernel_launches,
                    cancel_checks: result.cancel_checks,
                });
            }
            if result.rows.required_rows != self.len() as u64 {
                return Err(error(
                    GpuStatusDetail::ShapeMismatch,
                    "controlled projection returned invalid cardinality",
                ));
            }
            NativeProjectResult {
                emitted_rows: result.rows.emitted_rows,
                kernel_launches: result.rows.kernel_launches,
            }
        } else {
            let mut result = NativeProjectResult::default();
            // SAFETY: self owns the initialized pair prefix and origin row counts.
            // Inputs are typed borrows of exactly those domains. Fresh disjoint
            // output allocations match every span and outlive the synchronous call;
            // fixed stack metadata/error result are disjoint from device storage.
            let status = unsafe {
                bridge::pgaccel_join_rows_project(
                    &raw const request,
                    scratch.as_mut_ptr().cast(),
                    JOIN_PROJECT_WORKSPACE_BYTES as u64,
                    &raw mut result,
                )
            };
            status_to_result(
                status,
                GpuErrorDomain::HashJoin,
                GpuOperation::Kernel("pgaccel_join_rows_project"),
            )?;
            result
        };
        if result.emitted_rows != self.len() as u64 {
            return Err(error(
                GpuStatusDetail::ShapeMismatch,
                "projection emitted an invalid row count",
            ));
        }
        Ok(ControlledProjectionOutcome::Complete(
            ResidentJoinProjection {
                columns,
                column_count: inputs.len(),
                rows: self.len(),
                kernel_launches: result.kernel_launches,
                allocated_bytes: output_bytes,
                _charge: Some(charge),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(ResidentJoinProjection: Send, Sync);
    assert_not_impl_any!(JoinProjectionInput<'static>: Send, Sync);

    #[test]
    fn row_domain_projection_never_allocates_or_launches_and_preserves_cancellation() {
        for rows in [0, 1, 17, i32::MAX as usize] {
            let mut polls = 0;
            let result = ResidentJoinProjection::row_domain(rows, &mut || {
                polls += 1;
                false
            });
            let ControlledProjectionOutcome::Complete(output) = result else {
                panic!("not cancelled");
            };
            assert_eq!(polls, 1);
            assert_eq!(output.len(), rows);
            assert_eq!(output.column_count(), 0);
            assert_eq!(output.allocated_bytes(), 0);
            assert_eq!(output.kernel_launches(), 0);
            assert!(output._charge.is_none());
            assert!(output.columns.iter().all(Option::is_none));
            assert!(matches!(
                ResidentJoinProjection::row_domain(rows, &mut || true),
                ControlledProjectionOutcome::Interrupted {
                    kernel_launches: 0,
                    cancel_checks: 1
                }
            ));
        }
        let panic = std::panic::catch_unwind(|| {
            ResidentJoinProjection::row_domain(9, &mut || panic!("host boundary panic"))
        });
        assert!(
            panic.is_err(),
            "no native frame or allocated owner can intercept panic"
        );
    }

    #[test]
    fn join_projection_abi_layout() {
        assert_eq!(size_of::<NativeProjectColumn>(), 72);
        assert_eq!(size_of::<NativeProjectRequest>(), 1208);
        assert_eq!(size_of::<NativeProjectResult>(), 16);
        assert_eq!(JOIN_PROJECT_WORKSPACE_BYTES, 1160);
        assert_eq!(std::mem::offset_of!(NativeProjectColumn, output_values), 40);
        assert_eq!(std::mem::offset_of!(NativeProjectRequest, columns), 56);
    }

    #[test]
    fn controlled_projection_preflight_rejects_invalid_host_contracts_without_a_device() {
        use super::super::join_rows::NativeControlledJoinResult;

        for mutation in 0..10 {
            let control = JoinDispatchLimits {
                work_items: 2,
                chain_steps: 1,
            }
            .native()
            .expect("valid host control");
            let mut request = NativeProjectRequest {
                version: 1,
                column_count: 1,
                reserved: 0,
                outer_rows: 0,
                inner_rows: 0,
                row_count: 0,
                pairs: std::ptr::null(),
                pair_bytes: 0,
                columns: [NativeProjectColumn::default(); MAX_COLUMNS],
            };
            request.columns[0].width = 4;
            let mut workspace = [0_u64; JOIN_PROJECT_WORKSPACE_BYTES / 8];
            let mut result = NativeControlledJoinResult::default();
            result.rows.required_rows = 83;
            result.rows.emitted_rows = 89;
            result.rows.kernel_launches = 97;
            result.cancelled = 79;
            result.cancel_checks = 73;
            let mut workspace_bytes = JOIN_PROJECT_WORKSPACE_BYTES as u64;
            let mut result_pointer = std::ptr::from_mut(&mut result);
            let mut control_pointer = std::ptr::from_ref(&control);
            match mutation {
                0 => request.version = 2,
                1 => request.column_count = 0,
                2 => request.row_count = u64::MAX,
                3 => workspace_bytes -= 1,
                4 => result_pointer = std::ptr::null_mut(),
                5 => result_pointer = workspace.as_mut_ptr().cast(),
                6 => result_pointer = std::ptr::from_ref(&control).cast_mut().cast(),
                7 => control_pointer = std::ptr::null(),
                8 => request.reserved = 1,
                9 => request.columns[1].width = 4,
                _ => unreachable!(),
            }
            // SAFETY: every non-null pointer designates initialized, aligned
            // host storage. Each request violates a preflight contract checked
            // before queue access; neither GPU dispatch nor result writes are
            // permitted, especially for the deliberately aliased result spans.
            let status = unsafe {
                bridge::pgaccel_join_rows_project_controlled(
                    &raw const request,
                    control_pointer,
                    workspace.as_mut_ptr().cast(),
                    workspace_bytes,
                    result_pointer,
                )
            };
            assert_eq!(
                status,
                if mutation == 0 {
                    PgaccelStatus::ErrorUnsupported
                } else {
                    PgaccelStatus::InvalidArgument
                },
                "mutation {mutation}"
            );
            assert_eq!(result.rows.required_rows, 83);
            assert_eq!(result.rows.emitted_rows, 89);
            assert_eq!(result.rows.kernel_launches, 97);
            assert_eq!(result.cancelled, 79);
            assert_eq!(result.cancel_checks, 73);
            assert_eq!(workspace, [0; JOIN_PROJECT_WORKSPACE_BYTES / 8]);
        }
    }
}

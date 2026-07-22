//! Typed error surface for GPU facade calls.
//!
//! Domain dispatchers use this result when runtime failure must remain
//! distinct from a successful algorithmic result such as spatial UNCERTAIN.

use std::fmt;

use super::types::PgaccelStatus;

/// Result type used by typed GPU facade helpers.
pub type GpuResult<T> = Result<T, GpuError>;

/// High-level GPU subsystem associated with an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuErrorDomain {
    Runtime,
    Memory,
    Descriptor,
    Expression,
    Spatial,
    H3,
    Raster,
    Sort,
    Reduce,
    HashAgg,
    GroupedAgg,
    HashJoin,
    Window,
}

impl fmt::Display for GpuErrorDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Runtime => "runtime",
            Self::Memory => "memory",
            Self::Descriptor => "descriptor",
            Self::Expression => "expression",
            Self::Spatial => "spatial",
            Self::H3 => "h3",
            Self::Raster => "raster",
            Self::Sort => "sort",
            Self::Reduce => "reduce",
            Self::HashAgg => "hash_agg",
            Self::GroupedAgg => "grouped_agg",
            Self::HashJoin => "hash_join",
            Self::Window => "window",
        };
        f.write_str(label)
    }
}

/// Specific GPU operation being attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuOperation {
    Init,
    Shutdown,
    QueryDevice,
    ValidateDeviceInput,
    ValidateDeviceOutput,
    ValidateCsrOutput,
    BuildColumnBatch,
    Kernel(&'static str),
}

impl fmt::Display for GpuOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init => f.write_str("init"),
            Self::Shutdown => f.write_str("shutdown"),
            Self::QueryDevice => f.write_str("query_device"),
            Self::ValidateDeviceInput => f.write_str("validate_device_input"),
            Self::ValidateDeviceOutput => f.write_str("validate_device_output"),
            Self::ValidateCsrOutput => f.write_str("validate_csr_output"),
            Self::BuildColumnBatch => f.write_str("build_column_batch"),
            Self::Kernel(name) => write!(f, "kernel({name})"),
        }
    }
}

/// Normalised status detail for FFI statuses and facade validation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuStatusDetail {
    Ok,
    ExecutionFailed,
    Unsupported,
    OutOfMemory,
    Timeout,
    NoDevice,
    InvalidArgument,
    InvalidDescriptor,
    ShapeMismatch,
    CapacityOverflow,
    NumericOverflow,
}

impl GpuStatusDetail {
    /// Returns `true` when the status indicates success.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

impl From<PgaccelStatus> for GpuStatusDetail {
    fn from(status: PgaccelStatus) -> Self {
        match status {
            PgaccelStatus::Ok => Self::Ok,
            PgaccelStatus::Error => Self::ExecutionFailed,
            PgaccelStatus::ErrorUnsupported => Self::Unsupported,
            PgaccelStatus::ErrorOom => Self::OutOfMemory,
            PgaccelStatus::ErrorTimeout => Self::Timeout,
            PgaccelStatus::ErrorNoDevice => Self::NoDevice,
            PgaccelStatus::InvalidArgument => Self::InvalidArgument,
        }
    }
}

impl fmt::Display for GpuStatusDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Ok => "ok",
            Self::ExecutionFailed => "execution_failed",
            Self::Unsupported => "unsupported",
            Self::OutOfMemory => "out_of_memory",
            Self::Timeout => "timeout",
            Self::NoDevice => "no_device",
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidDescriptor => "invalid_descriptor",
            Self::ShapeMismatch => "shape_mismatch",
            Self::CapacityOverflow => "capacity_overflow",
            Self::NumericOverflow => "numeric_overflow",
        };
        f.write_str(label)
    }
}

/// Typed GPU error with domain, operation, status, and optional detail text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuError {
    pub domain: GpuErrorDomain,
    pub operation: GpuOperation,
    pub status: GpuStatusDetail,
    pub detail: Option<&'static str>,
}

impl GpuError {
    /// Build an error with no extra detail text.
    #[must_use]
    pub const fn new(
        domain: GpuErrorDomain,
        operation: GpuOperation,
        status: GpuStatusDetail,
    ) -> Self {
        Self {
            domain,
            operation,
            status,
            detail: None,
        }
    }

    /// Build an error with a static detail string.
    #[must_use]
    pub const fn with_detail(
        domain: GpuErrorDomain,
        operation: GpuOperation,
        status: GpuStatusDetail,
        detail: &'static str,
    ) -> Self {
        Self {
            domain,
            operation,
            status,
            detail: Some(detail),
        }
    }

    /// Convert a raw FFI status into a typed GPU error.
    #[must_use]
    pub fn from_status(
        domain: GpuErrorDomain,
        operation: GpuOperation,
        status: PgaccelStatus,
    ) -> Self {
        Self::new(domain, operation, status.into())
    }
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.detail {
            Some(detail) => write!(
                f,
                "GPU {} {} failed with {}: {}",
                self.domain, self.operation, self.status, detail
            ),
            None => write!(
                f,
                "GPU {} {} failed with {}",
                self.domain, self.operation, self.status
            ),
        }
    }
}

impl std::error::Error for GpuError {}

/// Convert a raw FFI status into a typed result.
pub fn status_to_result(
    status: PgaccelStatus,
    domain: GpuErrorDomain,
    operation: GpuOperation,
) -> GpuResult<()> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(GpuError::from_status(domain, operation, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgaccel_status_maps_to_typed_status_detail() {
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::Ok),
            GpuStatusDetail::Ok
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::Error),
            GpuStatusDetail::ExecutionFailed
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::ErrorUnsupported),
            GpuStatusDetail::Unsupported
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::ErrorOom),
            GpuStatusDetail::OutOfMemory
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::ErrorTimeout),
            GpuStatusDetail::Timeout
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::ErrorNoDevice),
            GpuStatusDetail::NoDevice
        );
        assert_eq!(
            GpuStatusDetail::from(PgaccelStatus::InvalidArgument),
            GpuStatusDetail::InvalidArgument
        );
        assert_eq!(
            GpuStatusDetail::NumericOverflow.to_string(),
            "numeric_overflow"
        );
    }

    #[test]
    fn status_to_result_preserves_error_context() {
        let result = status_to_result(
            PgaccelStatus::ErrorNoDevice,
            GpuErrorDomain::Runtime,
            GpuOperation::Init,
        );

        let Err(err) = result else {
            panic!("expected no-device status to become an error");
        };

        assert_eq!(err.domain, GpuErrorDomain::Runtime);
        assert_eq!(err.operation, GpuOperation::Init);
        assert_eq!(err.status, GpuStatusDetail::NoDevice);
    }

    #[test]
    fn ok_status_returns_ok_result() {
        assert!(
            status_to_result(
                PgaccelStatus::Ok,
                GpuErrorDomain::Runtime,
                GpuOperation::Init,
            )
            .is_ok()
        );
    }

    #[test]
    fn every_error_domain_and_operation_has_a_stable_label() {
        let domains = [
            (GpuErrorDomain::Runtime, "runtime"),
            (GpuErrorDomain::Memory, "memory"),
            (GpuErrorDomain::Descriptor, "descriptor"),
            (GpuErrorDomain::Expression, "expression"),
            (GpuErrorDomain::Spatial, "spatial"),
            (GpuErrorDomain::H3, "h3"),
            (GpuErrorDomain::Raster, "raster"),
            (GpuErrorDomain::Sort, "sort"),
            (GpuErrorDomain::Reduce, "reduce"),
            (GpuErrorDomain::HashAgg, "hash_agg"),
            (GpuErrorDomain::GroupedAgg, "grouped_agg"),
            (GpuErrorDomain::HashJoin, "hash_join"),
            (GpuErrorDomain::Window, "window"),
        ];
        for (domain, label) in domains {
            assert_eq!(domain.to_string(), label);
        }

        let operations = [
            (GpuOperation::Init, "init"),
            (GpuOperation::Shutdown, "shutdown"),
            (GpuOperation::QueryDevice, "query_device"),
            (GpuOperation::ValidateDeviceInput, "validate_device_input"),
            (GpuOperation::ValidateDeviceOutput, "validate_device_output"),
            (GpuOperation::ValidateCsrOutput, "validate_csr_output"),
            (GpuOperation::BuildColumnBatch, "build_column_batch"),
            (GpuOperation::Kernel("scan"), "kernel(scan)"),
        ];
        for (operation, label) in operations {
            assert_eq!(operation.to_string(), label);
        }
    }

    #[test]
    fn every_status_detail_has_a_stable_label_and_success_bit() {
        let details = [
            (GpuStatusDetail::Ok, "ok", true),
            (GpuStatusDetail::ExecutionFailed, "execution_failed", false),
            (GpuStatusDetail::Unsupported, "unsupported", false),
            (GpuStatusDetail::OutOfMemory, "out_of_memory", false),
            (GpuStatusDetail::Timeout, "timeout", false),
            (GpuStatusDetail::NoDevice, "no_device", false),
            (GpuStatusDetail::InvalidArgument, "invalid_argument", false),
            (
                GpuStatusDetail::InvalidDescriptor,
                "invalid_descriptor",
                false,
            ),
            (GpuStatusDetail::ShapeMismatch, "shape_mismatch", false),
            (
                GpuStatusDetail::CapacityOverflow,
                "capacity_overflow",
                false,
            ),
            (GpuStatusDetail::NumericOverflow, "numeric_overflow", false),
        ];
        for (detail, label, is_ok) in details {
            assert_eq!(detail.to_string(), label);
            assert_eq!(detail.is_ok(), is_ok);
        }
    }

    #[test]
    fn gpu_error_display_includes_optional_detail() {
        let plain = GpuError::new(
            GpuErrorDomain::HashJoin,
            GpuOperation::Kernel("probe"),
            GpuStatusDetail::CapacityOverflow,
        );
        assert_eq!(
            plain.to_string(),
            "GPU hash_join kernel(probe) failed with capacity_overflow"
        );

        let detailed = GpuError::with_detail(
            GpuErrorDomain::Raster,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::ShapeMismatch,
            "row count changed",
        );
        assert_eq!(
            detailed.to_string(),
            "GPU raster validate_device_output failed with shape_mismatch: row count changed"
        );
        assert!(std::error::Error::source(&detailed).is_none());
    }
}

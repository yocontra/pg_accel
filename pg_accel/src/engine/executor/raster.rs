//! Store-neutral artifact boundary for childless resident raster execution.

use crate::engine::raster::{
    RasterExecutionError, RasterExecutionPreflight, RasterQuerySpec, RasterReclassRule,
    RasterReconstructedOutput, RasterSpecCodecError, reconstruct_raster_output,
};
use crate::engine::residency::{
    DerivedArtifact, DerivedArtifactIdentity, ResidentByteAccounting, ResidentColumnView,
    ResidentLoadError, ResidentRasterBand, ResidentRasterRow,
};
use crate::gpu::{
    ExprDeviceBuffer, GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail,
    PGACCEL_RESIDENT_RASTER_ABI_VERSION, PgaccelRasterReclassResidentRequest,
    PgaccelResidentRasterBand, PgaccelResidentRasterReclassRule, PgaccelResidentRasterRow,
    PgaccelResidentRasterValidationScratch, PgaccelResidentRasterView, RasterResidentLaunchOutcome,
    prepare_raster_reclass_resident, raster_reclass_resident_launch,
    raster_reclass_resident_launch_result, raster_reclass_resident_validation,
};

const _: () = assert!(
    std::mem::size_of::<RasterReclassRule>()
        == std::mem::size_of::<PgaccelResidentRasterReclassRule>()
);
const _: () = assert!(
    std::mem::align_of::<RasterReclassRule>()
        == std::mem::align_of::<PgaccelResidentRasterReclassRule>()
);
const _: () = assert!(
    std::mem::size_of::<ResidentRasterRow>() == std::mem::size_of::<PgaccelResidentRasterRow>()
);
const _: () = assert!(
    std::mem::align_of::<ResidentRasterRow>() == std::mem::align_of::<PgaccelResidentRasterRow>()
);
const _: () = assert!(
    std::mem::size_of::<ResidentRasterBand>() == std::mem::size_of::<PgaccelResidentRasterBand>()
);
const _: () = assert!(
    std::mem::align_of::<ResidentRasterBand>() == std::mem::align_of::<PgaccelResidentRasterBand>()
);

macro_rules! assert_abi_field_offset {
    ($domain:ty, $abi:ty, $field:ident) => {
        const _: () =
            assert!(std::mem::offset_of!($domain, $field) == std::mem::offset_of!($abi, $field));
    };
}

assert_abi_field_offset!(RasterReclassRule, PgaccelResidentRasterReclassRule, source);
assert_abi_field_offset!(
    RasterReclassRule,
    PgaccelResidentRasterReclassRule,
    destination
);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, width);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, height);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, first_band);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, band_count);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, srid);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, flags);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, scale_x);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, scale_y);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, ip_x);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, ip_y);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, skew_x);
assert_abi_field_offset!(ResidentRasterRow, PgaccelResidentRasterRow, skew_y);
assert_abi_field_offset!(ResidentRasterBand, PgaccelResidentRasterBand, pixel_type);
assert_abi_field_offset!(ResidentRasterBand, PgaccelResidentRasterBand, flags);
assert_abi_field_offset!(ResidentRasterBand, PgaccelResidentRasterBand, nodata);

/// Prepared transient state. It is never publishable by itself: native output
/// and row actions must reconstruct successfully before it can become a
/// [`RasterOutputArtifact`].
pub struct PreparedRasterArtifact {
    preflight: RasterExecutionPreflight,
}

impl PreparedRasterArtifact {
    #[must_use]
    pub const fn new(preflight: RasterExecutionPreflight) -> Self {
        Self { preflight }
    }

    #[must_use]
    pub const fn preflight(&self) -> &RasterExecutionPreflight {
        &self.preflight
    }

    /// Exact final accounting declared to the derived-artifact store.
    /// Launch buffers and the source snapshot are not smuggled into this
    /// persistent category.
    #[must_use]
    pub const fn published_accounting(&self) -> ResidentByteAccounting {
        ResidentByteAccounting {
            device_bytes: 0,
            retained_host_exact_bytes: self.preflight.accounting.reconstructed_output_bytes,
        }
    }

    /// Additional bytes that coexist with the final output at peak. A staged
    /// caller reserves this exact transient delta separately, then releases it
    /// after reconstruction or on any failure.
    #[must_use]
    pub fn transient_peak_bytes(&self) -> Option<u64> {
        self.preflight
            .accounting
            .prelaunch_reserved_bytes
            .checked_add(self.preflight.accounting.post_launch_host_bytes)
    }

    /// Consume all transient source state and return the only publishable
    /// raster artifact. Reconstruction errors return no partial artifact.
    pub fn reconstruct(
        self,
        output_pixels: &[u8],
        row_actions: &[u8],
    ) -> Result<RasterOutputArtifact, RasterExecutionError> {
        let output = reconstruct_raster_output(&self.preflight, output_pixels, row_actions)?;
        Ok(RasterOutputArtifact { output })
    }
}

fn gpu_error(
    operation: GpuOperation,
    status: GpuStatusDetail,
    detail: &'static str,
) -> ResidentLoadError {
    ResidentLoadError::Gpu(GpuError::with_detail(
        GpuErrorDomain::Raster,
        operation,
        status,
        detail,
    ))
}

fn allocation_error(detail: &'static str) -> ResidentLoadError {
    gpu_error(
        GpuOperation::BuildColumnBatch,
        GpuStatusDetail::OutOfMemory,
        detail,
    )
}

fn invalid_workspace(detail: &'static str) -> ResidentLoadError {
    gpu_error(
        GpuOperation::ValidateDeviceInput,
        GpuStatusDetail::InvalidDescriptor,
        detail,
    )
}

fn invalid_output(detail: &'static str) -> ResidentLoadError {
    gpu_error(
        GpuOperation::ValidateDeviceOutput,
        GpuStatusDetail::ShapeMismatch,
        detail,
    )
}

fn try_default_box<T: Clone + Default>(
    len: usize,
    detail: &'static str,
) -> Result<Box<[T]>, ResidentLoadError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| allocation_error(detail))?;
    values.resize(len, T::default());
    Ok(values.into_boxed_slice())
}

fn checked_bytes<T>(len: usize) -> Option<u64> {
    len.checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
}

fn checked_add_bytes(total: &mut u64, bytes: u64) -> Option<()> {
    *total = total.checked_add(bytes)?;
    Some(())
}

fn optional_device_bytes<T>(buffer: Option<&ExprDeviceBuffer<T>>) -> Option<u64> {
    checked_bytes::<T>(buffer.map_or(0, ExprDeviceBuffer::len))
}

fn execution_error(error: RasterExecutionError) -> ResidentLoadError {
    ResidentLoadError::Loader(format!("resident raster execution failed: {error}"))
}

fn validate_launch_scratch(
    scratch: &PgaccelResidentRasterValidationScratch,
    output_offsets: &[u64],
    first_row: usize,
    count: usize,
) -> GpuResult<()> {
    raster_reclass_resident_validation(scratch)?;
    let last_row = first_row.checked_add(count).ok_or_else(|| {
        GpuError::with_detail(
            GpuErrorDomain::Raster,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::NumericOverflow,
            "resident raster launched row range overflowed",
        )
    })?;
    let expected_first = output_offsets.get(first_row).ok_or_else(|| {
        GpuError::with_detail(
            GpuErrorDomain::Raster,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::ShapeMismatch,
            "resident raster launch starts outside output offsets",
        )
    })?;
    let expected_last = output_offsets.get(last_row).ok_or_else(|| {
        GpuError::with_detail(
            GpuErrorDomain::Raster,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::ShapeMismatch,
            "resident raster launch ends outside output offsets",
        )
    })?;
    if scratch.first_output_offset != *expected_first
        || scratch.last_output_offset != *expected_last
    {
        return Err(GpuError::with_detail(
            GpuErrorDomain::Raster,
            GpuOperation::ValidateDeviceOutput,
            GpuStatusDetail::ShapeMismatch,
            "resident raster validation scratch has stale or corrupt output bounds",
        ));
    }
    Ok(())
}

/// Allocation-free contract failure captured while resident input pointers
/// are borrowed. It is mapped to a typed error only after that borrow ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterBorrowFailure {
    RepeatedLaunch,
    EmptyInputMismatch,
    NonRasterInput,
    InputSpanOverflow,
    InputTypeOrRowCountChanged,
    MissingRules,
    MissingOutputOffsets,
    MissingOutputPixels,
    MissingRowActions,
    MissingValidationScratch,
    WorkspaceSpanOverflow,
    WorkspaceLengthMismatch,
}

/// POD state crossing the resident-store borrow boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterBorrowOutcome {
    Pending,
    EmptyAccepted,
    Failure(RasterBorrowFailure),
    Native(RasterResidentLaunchOutcome),
}

#[derive(Clone, Copy)]
struct RasterBorrowView {
    type_oid: u32,
    pixels: *const u8,
    pixels_len: usize,
    band_offsets: *const u64,
    band_offsets_len: usize,
    rows: *const ResidentRasterRow,
    rows_len: usize,
    bands: *const ResidentRasterBand,
    bands_len: usize,
    nulls: *const u8,
    nulls_len: usize,
}

#[derive(Clone, Copy)]
enum RasterBorrowInput {
    Empty { type_oid: u32 },
    Raster(RasterBorrowView),
    Other,
}

#[derive(Clone, Copy, Default)]
struct RasterWorkspaceDeviceView {
    rules: Option<(*const RasterReclassRule, usize)>,
    output_offsets: Option<(*const u64, usize)>,
    output_pixels: Option<(*mut u8, usize)>,
    row_actions: Option<(*mut u8, usize)>,
    validation_scratch: Option<(*mut PgaccelResidentRasterValidationScratch, usize)>,
}

fn borrow_failure_error(failure: RasterBorrowFailure) -> ResidentLoadError {
    let (status, detail) = match failure {
        RasterBorrowFailure::RepeatedLaunch => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster workspace was launched more than once",
        ),
        RasterBorrowFailure::EmptyInputMismatch => (
            GpuStatusDetail::InvalidDescriptor,
            "empty resident raster workspace resolved a different input column",
        ),
        RasterBorrowFailure::NonRasterInput => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster workspace resolved a non-raster input column",
        ),
        RasterBorrowFailure::InputSpanOverflow => (
            GpuStatusDetail::NumericOverflow,
            "resident raster input byte span overflowed",
        ),
        RasterBorrowFailure::InputTypeOrRowCountChanged => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster input type or row count changed after snapshot",
        ),
        RasterBorrowFailure::MissingRules => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster rules buffer is missing",
        ),
        RasterBorrowFailure::MissingOutputOffsets => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster output-offset buffer is missing",
        ),
        RasterBorrowFailure::MissingOutputPixels => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster output-pixel buffer is missing",
        ),
        RasterBorrowFailure::MissingRowActions => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster row-action buffer is missing",
        ),
        RasterBorrowFailure::MissingValidationScratch => (
            GpuStatusDetail::InvalidDescriptor,
            "resident raster validation buffer is missing",
        ),
        RasterBorrowFailure::WorkspaceSpanOverflow => (
            GpuStatusDetail::NumericOverflow,
            "resident raster workspace byte span overflowed",
        ),
        RasterBorrowFailure::WorkspaceLengthMismatch => (
            GpuStatusDetail::ShapeMismatch,
            "resident raster workspace buffer lengths changed after construction",
        ),
    };
    gpu_error(GpuOperation::ValidateDeviceInput, status, detail)
}

/// Complete transient state for one resident Reclass build. Every heap and
/// device allocation is created after the store has reserved its exact W
/// charge. The raw store borrow may only call [`Self::launch`].
pub struct RasterLaunchWorkspace {
    prepared: PreparedRasterArtifact,
    expected_type_oid: u32,
    rules: Option<ExprDeviceBuffer<RasterReclassRule>>,
    output_offsets: Option<ExprDeviceBuffer<u64>>,
    output_pixels: Option<ExprDeviceBuffer<u8>>,
    row_actions: Option<ExprDeviceBuffer<u8>>,
    validation_scratch: Option<ExprDeviceBuffer<PgaccelResidentRasterValidationScratch>>,
    host_output_pixels: Box<[u8]>,
    host_row_actions: Box<[u8]>,
    host_validation_scratch: Box<[PgaccelResidentRasterValidationScratch]>,
    expected_rule_count: usize,
    max_chunk_pixels: usize,
    borrow_outcome: RasterBorrowOutcome,
}

impl RasterLaunchWorkspace {
    /// Prepare the process queue and construct every W-owned allocation. No
    /// device or host allocation remains for the resident dispatch borrow or
    /// post-launch device-to-host copies.
    pub fn build(
        spec: &RasterQuerySpec,
        prepared: PreparedRasterArtifact,
    ) -> Result<Self, ResidentLoadError> {
        let row_count = prepared.preflight.layout.row_count();
        let output_pixels_bytes = prepared.preflight.layout.output_pixels_bytes();
        if prepared.preflight.layout.output_pixel_type() != spec.reclass.output_pixel_type {
            return Err(invalid_workspace(
                "resident raster preflight output type disagrees with the query spec",
            ));
        }
        if row_count == 0 {
            return Self {
                prepared,
                expected_type_oid: spec.raster_type_oid,
                rules: None,
                output_offsets: None,
                output_pixels: None,
                row_actions: None,
                validation_scratch: None,
                host_output_pixels: Box::default(),
                host_row_actions: Box::default(),
                host_validation_scratch: Box::default(),
                expected_rule_count: spec.reclass.rules.len(),
                max_chunk_pixels: 1,
                borrow_outcome: RasterBorrowOutcome::Pending,
            }
            .verify_accounting();
        }

        prepare_raster_reclass_resident().map_err(ResidentLoadError::Gpu)?;
        let rules = ExprDeviceBuffer::copy_from_slice(&spec.reclass.rules)
            .ok_or_else(|| allocation_error("resident raster rule upload failed"))?;
        let output_offsets =
            ExprDeviceBuffer::copy_from_slice(prepared.preflight.layout.output_offsets())
                .ok_or_else(|| allocation_error("resident raster offset upload failed"))?;
        let output_pixels = if output_pixels_bytes == 0 {
            None
        } else {
            Some(
                ExprDeviceBuffer::new(output_pixels_bytes)
                    .ok_or_else(|| allocation_error("resident raster output allocation failed"))?,
            )
        };
        let row_actions = ExprDeviceBuffer::new(row_count)
            .ok_or_else(|| allocation_error("resident raster row-action allocation failed"))?;
        let validation_scratch = ExprDeviceBuffer::new(1).ok_or_else(|| {
            allocation_error("resident raster validation-scratch allocation failed")
        })?;
        let host_output_pixels = try_default_box(
            output_pixels_bytes,
            "resident raster output readback allocation failed",
        )?;
        let host_row_actions = try_default_box(
            row_count,
            "resident raster row-action readback allocation failed",
        )?;
        let host_validation_scratch =
            try_default_box(1, "resident raster validation readback allocation failed")?;
        let max_chunk_pixels = crate::engine::cost::device_limits()
            .gpu_raster_max_chunk_pixels
            .max(1)
            .min(prepared.preflight.layout.total_pixels().max(1));
        Self {
            prepared,
            expected_type_oid: spec.raster_type_oid,
            rules: Some(rules),
            output_offsets: Some(output_offsets),
            output_pixels,
            row_actions: Some(row_actions),
            validation_scratch: Some(validation_scratch),
            host_output_pixels,
            host_row_actions,
            host_validation_scratch,
            expected_rule_count: spec.reclass.rules.len(),
            max_chunk_pixels,
            borrow_outcome: RasterBorrowOutcome::Pending,
        }
        .verify_accounting()
    }

    fn verify_accounting(self) -> Result<Self, ResidentLoadError> {
        let declared = self
            .declared_accounting()
            .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
        let actual = self
            .accounting()
            .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
        if actual != declared {
            return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
        }
        Ok(self)
    }

    #[must_use]
    pub fn declared_accounting(&self) -> Option<ResidentByteAccounting> {
        let accounting = self.prepared.preflight.accounting;
        Some(ResidentByteAccounting {
            device_bytes: accounting.device_artifact_bytes,
            retained_host_exact_bytes: accounting
                .snapshot_host_bytes
                .checked_add(accounting.layout_host_bytes)?
                .checked_add(accounting.post_launch_host_bytes)?,
        })
    }

    /// Compute W's actual owned allocation sizes rather than echoing the
    /// preflight declaration.
    #[must_use]
    pub fn accounting(&self) -> Option<ResidentByteAccounting> {
        let mut device_bytes = 0_u64;
        checked_add_bytes(
            &mut device_bytes,
            optional_device_bytes(self.rules.as_ref())?,
        )?;
        checked_add_bytes(
            &mut device_bytes,
            optional_device_bytes(self.output_offsets.as_ref())?,
        )?;
        checked_add_bytes(
            &mut device_bytes,
            optional_device_bytes(self.output_pixels.as_ref())?,
        )?;
        checked_add_bytes(
            &mut device_bytes,
            optional_device_bytes(self.row_actions.as_ref())?,
        )?;
        checked_add_bytes(
            &mut device_bytes,
            optional_device_bytes(self.validation_scratch.as_ref())?,
        )?;

        let snapshot = &self.prepared.preflight.snapshot;
        let layout = &self.prepared.preflight.layout;
        let mut host_bytes = 0_u64;
        for bytes in [
            checked_bytes::<u64>(snapshot.stats.band_pixels.len())?,
            checked_bytes::<u64>(snapshot.stats.band_rows.len())?,
            checked_bytes::<crate::engine::residency::ResidentRasterWorkRow>(
                snapshot.stats.work_rows.len(),
            )?,
            checked_bytes::<u64>(snapshot.exact.offsets.len())?,
            checked_bytes::<u8>(snapshot.exact.bytes.len())?,
            checked_bytes::<u64>(layout.output_offsets().len())?,
            checked_bytes::<u64>(layout.output_wkb_offsets().len())?,
            checked_bytes::<u8>(self.host_output_pixels.len())?,
            checked_bytes::<u8>(self.host_row_actions.len())?,
            checked_bytes::<PgaccelResidentRasterValidationScratch>(
                self.host_validation_scratch.len(),
            )?,
        ] {
            checked_add_bytes(&mut host_bytes, bytes)?;
        }
        Some(ResidentByteAccounting {
            device_bytes,
            retained_host_exact_bytes: host_bytes,
        })
    }

    #[must_use]
    pub fn published_accounting(&self) -> ResidentByteAccounting {
        self.prepared.published_accounting()
    }

    /// Submit the already-built native request while the input column's store
    /// borrow pins all raw pointers. This method performs no allocation,
    /// device initialization, tracing, host copy, or typed result mapping.
    pub fn launch(&mut self, column: &ResidentColumnView<'_>) -> RasterBorrowOutcome {
        let input = match column {
            ResidentColumnView::Empty { type_oid } => RasterBorrowInput::Empty {
                type_oid: u32::from(*type_oid),
            },
            ResidentColumnView::Raster {
                type_oid,
                pixels,
                band_offsets,
                rows,
                bands,
                nulls,
                ..
            } => RasterBorrowInput::Raster(RasterBorrowView {
                type_oid: u32::from(*type_oid),
                pixels: pixels.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
                pixels_len: pixels.map_or(0, ExprDeviceBuffer::len),
                band_offsets: band_offsets.as_ptr(),
                band_offsets_len: band_offsets.len(),
                rows: rows.as_ptr(),
                rows_len: rows.len(),
                bands: bands.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
                bands_len: bands.map_or(0, ExprDeviceBuffer::len),
                nulls: nulls.map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
                nulls_len: nulls.map_or(0, ExprDeviceBuffer::len),
            }),
            _ => RasterBorrowInput::Other,
        };
        let device = RasterWorkspaceDeviceView {
            rules: self
                .rules
                .as_ref()
                .map(|buffer| (buffer.as_ptr(), buffer.len())),
            output_offsets: self
                .output_offsets
                .as_ref()
                .map(|buffer| (buffer.as_ptr(), buffer.len())),
            output_pixels: self
                .output_pixels
                .as_ref()
                .map(|buffer| (buffer.as_mut_ptr(), buffer.len())),
            row_actions: self
                .row_actions
                .as_ref()
                .map(|buffer| (buffer.as_mut_ptr(), buffer.len())),
            validation_scratch: self
                .validation_scratch
                .as_ref()
                .map(|buffer| (buffer.as_mut_ptr(), buffer.len())),
        };
        self.capture_launch(input, device)
    }

    fn capture_launch(
        &mut self,
        input: RasterBorrowInput,
        device: RasterWorkspaceDeviceView,
    ) -> RasterBorrowOutcome {
        macro_rules! fail {
            ($failure:expr) => {{
                self.borrow_outcome = RasterBorrowOutcome::Failure($failure);
                return self.borrow_outcome;
            }};
        }

        if self.borrow_outcome != RasterBorrowOutcome::Pending {
            fail!(RasterBorrowFailure::RepeatedLaunch);
        }
        if !self.prepared.preflight.layout.requires_launch() {
            self.borrow_outcome = match input {
                RasterBorrowInput::Empty { type_oid } if type_oid == self.expected_type_oid => {
                    RasterBorrowOutcome::EmptyAccepted
                }
                RasterBorrowInput::Raster(view)
                    if view.type_oid == self.expected_type_oid && view.rows_len == 0 =>
                {
                    RasterBorrowOutcome::EmptyAccepted
                }
                _ => RasterBorrowOutcome::Failure(RasterBorrowFailure::EmptyInputMismatch),
            };
            return self.borrow_outcome;
        }
        let RasterBorrowInput::Raster(input) = input else {
            fail!(RasterBorrowFailure::NonRasterInput);
        };
        let Some(band_offsets_bytes) = input
            .band_offsets_len
            .checked_mul(std::mem::size_of::<u64>())
        else {
            fail!(RasterBorrowFailure::InputSpanOverflow);
        };
        let Some(rows_bytes) = input
            .rows_len
            .checked_mul(std::mem::size_of::<ResidentRasterRow>())
        else {
            fail!(RasterBorrowFailure::InputSpanOverflow);
        };
        let Some(bands_bytes) = input
            .bands_len
            .checked_mul(std::mem::size_of::<ResidentRasterBand>())
        else {
            fail!(RasterBorrowFailure::InputSpanOverflow);
        };
        let row_count = self.prepared.preflight.layout.row_count();
        if input.type_oid != self.expected_type_oid || input.rows_len != row_count {
            fail!(RasterBorrowFailure::InputTypeOrRowCountChanged);
        }
        let Some((rules, rule_count)) = device.rules else {
            fail!(RasterBorrowFailure::MissingRules);
        };
        let Some((output_offsets, output_offset_count)) = device.output_offsets else {
            fail!(RasterBorrowFailure::MissingOutputOffsets);
        };
        let output_pixels = match (
            self.prepared.preflight.layout.output_pixels_bytes(),
            device.output_pixels,
        ) {
            (0, None) => (std::ptr::null_mut(), 0),
            (0, Some((pointer, 0))) => (pointer, 0),
            (_, Some(output)) => output,
            (_, None) => fail!(RasterBorrowFailure::MissingOutputPixels),
        };
        let Some((row_actions, row_action_count)) = device.row_actions else {
            fail!(RasterBorrowFailure::MissingRowActions);
        };
        let Some((validation_scratch, validation_count)) = device.validation_scratch else {
            fail!(RasterBorrowFailure::MissingValidationScratch);
        };
        let Some(rules_bytes) = rule_count.checked_mul(std::mem::size_of::<RasterReclassRule>())
        else {
            fail!(RasterBorrowFailure::WorkspaceSpanOverflow);
        };
        let Some(output_offsets_bytes) =
            output_offset_count.checked_mul(std::mem::size_of::<u64>())
        else {
            fail!(RasterBorrowFailure::WorkspaceSpanOverflow);
        };
        let Some(validation_scratch_bytes) = validation_count
            .checked_mul(std::mem::size_of::<PgaccelResidentRasterValidationScratch>())
        else {
            fail!(RasterBorrowFailure::WorkspaceSpanOverflow);
        };
        if rule_count != self.expected_rule_count
            || output_offset_count != self.prepared.preflight.layout.output_offsets().len()
            || output_pixels.1 != self.prepared.preflight.layout.output_pixels_bytes()
            || row_action_count != row_count
            || validation_count != 1
        {
            fail!(RasterBorrowFailure::WorkspaceLengthMismatch);
        }
        let input = PgaccelResidentRasterView {
            abi_version: PGACCEL_RESIDENT_RASTER_ABI_VERSION,
            flags: 0,
            pixels: input.pixels,
            pixels_bytes: input.pixels_len,
            band_offsets: input.band_offsets,
            band_offsets_bytes,
            rows: input.rows.cast::<PgaccelResidentRasterRow>(),
            rows_bytes,
            bands: input.bands.cast::<PgaccelResidentRasterBand>(),
            bands_bytes,
            nulls: input.nulls,
            nulls_bytes: input.nulls_len,
            row_count: input.rows_len,
            band_count: input.bands_len,
        };
        let request = PgaccelRasterReclassResidentRequest {
            abi_version: PGACCEL_RESIDENT_RASTER_ABI_VERSION,
            flags: 0,
            input,
            first_row: 0,
            count: row_count,
            output_pixel_type: self.prepared.preflight.layout.output_pixel_type().tag(),
            pad: 0,
            rules: rules.cast::<PgaccelResidentRasterReclassRule>(),
            rules_bytes,
            rule_count,
            output_offsets,
            output_offsets_bytes,
            output_pixels: output_pixels.0,
            output_pixels_bytes: output_pixels.1,
            row_actions,
            row_actions_bytes: row_action_count,
            validation_scratch,
            validation_scratch_bytes,
            max_total_pixels: self.prepared.preflight.layout.total_pixels(),
            max_chunk_pixels: self.max_chunk_pixels,
        };
        // SAFETY: every request pointer is owned either by this workspace or
        // by the live resident column borrow, and the process queue was
        // prepared before the borrow was acquired.
        self.borrow_outcome =
            RasterBorrowOutcome::Native(unsafe { raster_reclass_resident_launch(&request) });
        self.borrow_outcome
    }

    /// Validate the raw result and device scratch, perform allocation-free
    /// D2H copies into precharged storage, and consume W into publishable T.
    pub fn finalize(mut self) -> Result<RasterOutputArtifact, ResidentLoadError> {
        if !self.prepared.preflight.layout.requires_launch() {
            return match self.borrow_outcome {
                RasterBorrowOutcome::EmptyAccepted => {
                    self.prepared.reconstruct(&[], &[]).map_err(execution_error)
                }
                RasterBorrowOutcome::Failure(failure) => Err(borrow_failure_error(failure)),
                RasterBorrowOutcome::Native(_) => Err(invalid_output(
                    "empty resident raster workspace retained an unexpected native outcome",
                )),
                RasterBorrowOutcome::Pending => Err(invalid_output(
                    "empty resident raster workspace finalized before input validation",
                )),
            };
        }
        let outcome = match self.borrow_outcome {
            RasterBorrowOutcome::Native(outcome) => outcome,
            RasterBorrowOutcome::Failure(failure) => return Err(borrow_failure_error(failure)),
            RasterBorrowOutcome::Pending | RasterBorrowOutcome::EmptyAccepted => {
                return Err(invalid_output(
                    "resident raster workspace finalized before native launch",
                ));
            }
        };
        raster_reclass_resident_launch_result(outcome).map_err(ResidentLoadError::Gpu)?;
        let validation_scratch = self.validation_scratch.as_ref().ok_or_else(|| {
            invalid_output("resident raster validation buffer disappeared before readback")
        })?;
        validation_scratch
            .copy_to_slice(&mut self.host_validation_scratch)
            .map_err(ResidentLoadError::Gpu)?;
        let scratch = self
            .host_validation_scratch
            .first()
            .ok_or_else(|| invalid_output("resident raster validation readback buffer is empty"))?;
        validate_launch_scratch(
            scratch,
            self.prepared.preflight.layout.output_offsets(),
            0,
            self.prepared.preflight.layout.row_count(),
        )
        .map_err(ResidentLoadError::Gpu)?;
        self.row_actions
            .as_ref()
            .ok_or_else(|| {
                invalid_output("resident raster row-action buffer disappeared before readback")
            })?
            .copy_to_slice(&mut self.host_row_actions)
            .map_err(ResidentLoadError::Gpu)?;
        if let Some(output_pixels) = &self.output_pixels {
            output_pixels
                .copy_to_slice(&mut self.host_output_pixels)
                .map_err(ResidentLoadError::Gpu)?;
        } else if !self.host_output_pixels.is_empty() {
            return Err(invalid_output(
                "resident raster output buffer disappeared before readback",
            ));
        }
        self.prepared
            .reconstruct(&self.host_output_pixels, &self.host_row_actions)
            .map_err(execution_error)
    }
}

/// Final generation-stamped derived result retained by the residency store.
/// It owns no launch buffer or duplicate source WKB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterOutputArtifact {
    output: RasterReconstructedOutput,
}

impl RasterOutputArtifact {
    #[must_use]
    pub const fn output(&self) -> &RasterReconstructedOutput {
        &self.output
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.output.row_count()
    }

    #[must_use]
    pub fn accounting(&self) -> Option<ResidentByteAccounting> {
        let offsets = self
            .output
            .exact
            .offsets
            .len()
            .checked_mul(std::mem::size_of::<u64>())?;
        let nulls = self.output.nulls.as_ref().map_or(0, |nulls| nulls.len());
        let retained = offsets
            .checked_add(self.output.exact.bytes.len())?
            .checked_add(nulls)?;
        Some(ResidentByteAccounting {
            device_bytes: 0,
            retained_host_exact_bytes: u64::try_from(retained).ok()?,
        })
    }
}

impl DerivedArtifact for RasterOutputArtifact {
    fn device_bytes(&self) -> u64 {
        0
    }

    fn retained_host_exact_bytes(&self) -> u64 {
        self.accounting()
            .map_or(u64::MAX, |accounting| accounting.retained_host_exact_bytes)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Exact cache identity for a canonical RQS2 contract.
pub fn raster_artifact_identity(
    spec: &RasterQuerySpec,
) -> Result<DerivedArtifactIdentity, RasterSpecCodecError> {
    Ok(DerivedArtifactIdentity::from_canonical_words(
        spec.encode_words()?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterOutputValue<'a> {
    Null,
    Wkb(&'a [u8]),
}

/// Childless one-row-per-input cursor. ReScan rewinds only after its caller has
/// repeated catalog and generation validation for the resolved artifact.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RasterOutputCursor {
    next_row: usize,
}

impl RasterOutputCursor {
    #[must_use]
    pub const fn new() -> Self {
        Self { next_row: 0 }
    }

    pub fn next<'a>(
        &mut self,
        artifact: &'a RasterOutputArtifact,
    ) -> Option<RasterOutputValue<'a>> {
        let row = self.next_row;
        if row >= artifact.row_count() {
            return None;
        }
        self.next_row += 1;
        Some(match artifact.output.is_null(row) {
            Some(true) => RasterOutputValue::Null,
            Some(false) => RasterOutputValue::Wkb(
                artifact
                    .output
                    .value(row)
                    .expect("non-NULL reconstructed row has exact WKB"),
            ),
            None => unreachable!("row was bounded by the reconstructed output count"),
        })
    }

    pub const fn reset(&mut self) {
        self.next_row = 0;
    }

    #[must_use]
    pub const fn position(&self) -> usize {
        self.next_row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use crate::adapters::extractors::raster::parse_resident_raster;
    use crate::engine::raster::{
        RasterExecutionSnapshot, RasterPixelType, RasterReclassRule, RasterReclassSpec,
        preflight_raster_execution,
    };
    use crate::engine::residency::{
        ResidentRasterBand, ResidentRasterData, ResidentRasterRow, RetainedExactValues,
    };

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNT_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
                let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
            }
            // SAFETY: this allocator transparently delegates the same layout.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if COUNT_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
                let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
            }
            // SAFETY: this allocator transparently delegates the same layout.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` was returned by the delegated system allocator.
            unsafe { System.dealloc(ptr, layout) };
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if COUNT_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
                let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
            }
            // SAFETY: the allocation and layouts are delegated unchanged.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    fn allocation_count(f: impl FnOnce()) -> usize {
        struct CountingGuard;

        impl Drop for CountingGuard {
            fn drop(&mut self) {
                COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
            }
        }

        ALLOCATION_COUNT.with(|count| count.set(0));
        COUNT_ALLOCATIONS.with(|enabled| {
            assert!(
                !enabled.replace(true),
                "allocation counter is not reentrant"
            );
        });
        let guard = CountingGuard;
        f();
        drop(guard);
        ALLOCATION_COUNT.with(Cell::get)
    }

    const HEADER_BYTES: usize = 61;

    fn spec(rule_destination: i64) -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 1,
            raster_attno: 1,
            raster_type_oid: 2,
            function_oid: 3,
            catalog_fingerprint: vec![4, 5].into_boxed_slice(),
            reclass: RasterReclassSpec {
                output_pixel_type: RasterPixelType::UInt8,
                rules: vec![RasterReclassRule {
                    source: 1,
                    destination: rule_destination,
                }]
                .into_boxed_slice(),
            },
        }
    }

    fn raster(pixel: u8) -> Vec<u8> {
        let mut value = Vec::new();
        value.push(1);
        value.extend_from_slice(&0_u16.to_le_bytes());
        value.extend_from_slice(&1_u16.to_le_bytes());
        for metadata in [1.0_f64, -1.0, 0.0, 0.0, 0.0, 0.0] {
            value.extend_from_slice(&metadata.to_le_bytes());
        }
        value.extend_from_slice(&4326_i32.to_le_bytes());
        value.extend_from_slice(&1_u16.to_le_bytes());
        value.extend_from_slice(&1_u16.to_le_bytes());
        assert_eq!(value.len(), HEADER_BYTES);
        value.extend_from_slice(&[4, 0, pixel]);
        value
    }

    fn snapshot(values: &[Option<Vec<u8>>]) -> RasterExecutionSnapshot {
        let mut pixels = Vec::new();
        let mut band_offsets = vec![0_u64];
        let mut rows = Vec::new();
        let mut bands = Vec::new();
        let mut nulls = Vec::new();
        let mut exact_offsets = vec![0_u64];
        let mut exact_bytes = Vec::new();
        let mut saw_null = false;
        for value in values {
            let Some(value) = value else {
                rows.push(ResidentRasterRow::default());
                nulls.push(1);
                exact_offsets.push(exact_bytes.len() as u64);
                saw_null = true;
                continue;
            };
            let parsed = parse_resident_raster(value).expect("test raster parses");
            rows.push(ResidentRasterRow {
                width: u32::from(parsed.header.width),
                height: u32::from(parsed.header.height),
                first_band: bands.len() as u32,
                band_count: parsed.bands.len() as u32,
                srid: parsed.header.srid,
                scale_x: parsed.header.scale_x,
                scale_y: parsed.header.scale_y,
                ip_x: parsed.header.ip_x,
                ip_y: parsed.header.ip_y,
                skew_x: parsed.header.skew_x,
                skew_y: parsed.header.skew_y,
                ..ResidentRasterRow::default()
            });
            for band in parsed.bands {
                bands.push(ResidentRasterBand {
                    pixel_type: u32::from(band.pixel_type.code()),
                    flags: 0,
                    nodata: band.nodata,
                });
                pixels.extend_from_slice(&band.pixels);
                band_offsets.push(pixels.len() as u64);
            }
            nulls.push(0);
            exact_bytes.extend_from_slice(value);
            exact_offsets.push(exact_bytes.len() as u64);
        }
        let data = ResidentRasterData {
            pixels,
            band_offsets,
            rows,
            bands,
            nulls: saw_null.then_some(nulls),
            exact: RetainedExactValues {
                offsets: exact_offsets.into_boxed_slice(),
                bytes: exact_bytes.into_boxed_slice(),
            },
        };
        RasterExecutionSnapshot {
            stats: data.stats().expect("test stats"),
            exact: data.exact,
        }
    }

    fn unallocated_workspace() -> RasterLaunchWorkspace {
        let spec = spec(7);
        let prepared = PreparedRasterArtifact::new(
            preflight_raster_execution(&spec, snapshot(&[Some(raster(1))]))
                .expect("nonempty preflight"),
        );
        let output_bytes = prepared.preflight.layout.output_pixels_bytes();
        let row_count = prepared.preflight.layout.row_count();
        RasterLaunchWorkspace {
            prepared,
            expected_type_oid: spec.raster_type_oid,
            rules: None,
            output_offsets: None,
            output_pixels: None,
            row_actions: None,
            validation_scratch: None,
            host_output_pixels: vec![0; output_bytes].into_boxed_slice(),
            host_row_actions: vec![0; row_count].into_boxed_slice(),
            host_validation_scratch: vec![PgaccelResidentRasterValidationScratch::default(); 1]
                .into_boxed_slice(),
            expected_rule_count: spec.reclass.rules.len(),
            max_chunk_pixels: 1,
            borrow_outcome: RasterBorrowOutcome::Pending,
        }
    }

    fn dangling<T>() -> *mut T {
        std::ptr::NonNull::<T>::dangling().as_ptr()
    }

    fn valid_borrow_view() -> RasterBorrowView {
        RasterBorrowView {
            type_oid: 2,
            pixels: dangling::<u8>(),
            pixels_len: 1,
            band_offsets: dangling::<u64>(),
            band_offsets_len: 2,
            rows: dangling::<ResidentRasterRow>(),
            rows_len: 1,
            bands: dangling::<ResidentRasterBand>(),
            bands_len: 1,
            nulls: std::ptr::null(),
            nulls_len: 0,
        }
    }

    fn valid_device_view() -> RasterWorkspaceDeviceView {
        RasterWorkspaceDeviceView {
            rules: Some((dangling::<RasterReclassRule>(), 1)),
            output_offsets: Some((dangling::<u64>(), 2)),
            output_pixels: Some((dangling::<u8>(), 1)),
            row_actions: Some((dangling::<u8>(), 1)),
            validation_scratch: Some((dangling::<PgaccelResidentRasterValidationScratch>(), 1)),
        }
    }

    fn capture_without_allocation(
        workspace: &mut RasterLaunchWorkspace,
        input: RasterBorrowInput,
        device: RasterWorkspaceDeviceView,
    ) -> RasterBorrowOutcome {
        let mut outcome = RasterBorrowOutcome::Pending;
        let allocations = allocation_count(|| {
            outcome = workspace.capture_launch(input, device);
        });
        assert_eq!(allocations, 0, "resident borrow path allocated");
        outcome
    }

    fn assert_borrow_failure(
        mut workspace: RasterLaunchWorkspace,
        input: RasterBorrowInput,
        device: RasterWorkspaceDeviceView,
        expected: RasterBorrowFailure,
    ) {
        assert_eq!(
            capture_without_allocation(&mut workspace, input, device),
            RasterBorrowOutcome::Failure(expected)
        );
    }

    #[test]
    fn identity_is_the_complete_canonical_rqs2() {
        let first = raster_artifact_identity(&spec(7)).expect("first identity");
        let same = raster_artifact_identity(&spec(7)).expect("same identity");
        let changed = raster_artifact_identity(&spec(8)).expect("changed identity");
        assert_eq!(first, same);
        assert_ne!(first, changed);
        assert_eq!(
            first.canonical_words(),
            spec(7).encode_words().expect("canonical RQS2")
        );
    }

    #[test]
    fn validation_scratch_is_bound_to_the_exact_launched_slice() {
        let offsets = [0, 3, 7, 12, 12];
        let mut scratch = PgaccelResidentRasterValidationScratch {
            failures: 0,
            pad: 0,
            first_output_offset: 3,
            last_output_offset: 12,
        };
        validate_launch_scratch(&scratch, &offsets, 1, 2).expect("nonzero chunk bounds validate");

        scratch.first_output_offset = 0;
        assert_eq!(
            validate_launch_scratch(&scratch, &offsets, 1, 2)
                .expect_err("stale first offset must fail")
                .status,
            GpuStatusDetail::ShapeMismatch
        );
        scratch.first_output_offset = 3;
        scratch.last_output_offset = 7;
        assert_eq!(
            validate_launch_scratch(&scratch, &offsets, 1, 2)
                .expect_err("stale last offset must fail")
                .status,
            GpuStatusDetail::ShapeMismatch
        );
    }

    #[test]
    fn validation_scratch_corruption_precedes_bound_checks() {
        let offsets = [0, 4];
        let mut scratch = PgaccelResidentRasterValidationScratch {
            failures: 0,
            pad: 1,
            first_output_offset: u64::MAX,
            last_output_offset: u64::MAX,
        };
        assert_eq!(
            validate_launch_scratch(&scratch, &offsets, 0, 1)
                .expect_err("noncanonical scratch pad must fail first")
                .status,
            GpuStatusDetail::InvalidDescriptor
        );
        scratch.pad = 0;
        scratch.failures = crate::gpu::PGACCEL_RASTER_VALIDATION_CAPACITY;
        assert_eq!(
            validate_launch_scratch(&scratch, &offsets, 0, 1)
                .expect_err("device failure bits must fail before bounds")
                .status,
            GpuStatusDetail::CapacityOverflow
        );
        scratch.failures = 0;
        assert_eq!(
            validate_launch_scratch(&scratch, &offsets, usize::MAX, 1)
                .expect_err("launched row range overflow must fail")
                .status,
            GpuStatusDetail::NumericOverflow
        );
    }

    #[test]
    fn every_resident_borrow_branch_is_allocation_free() {
        let mut public_workspace = unallocated_workspace();
        let public_input = ResidentColumnView::Empty {
            type_oid: pgrx::pg_sys::Oid::from(2),
        };
        let mut public_outcome = RasterBorrowOutcome::Pending;
        let public_allocations = allocation_count(|| {
            public_outcome = public_workspace.launch(&public_input);
        });
        assert_eq!(public_allocations, 0);
        assert_eq!(
            public_outcome,
            RasterBorrowOutcome::Failure(RasterBorrowFailure::NonRasterInput)
        );

        let mut repeated = unallocated_workspace();
        repeated.borrow_outcome = RasterBorrowOutcome::EmptyAccepted;
        assert_borrow_failure(
            repeated,
            RasterBorrowInput::Other,
            valid_device_view(),
            RasterBorrowFailure::RepeatedLaunch,
        );

        let spec = spec(7);
        let prepared = PreparedRasterArtifact::new(
            preflight_raster_execution(&spec, snapshot(&[])).expect("empty preflight"),
        );
        let mut empty = RasterLaunchWorkspace::build(&spec, prepared)
            .expect("empty workspace construction is device-free");
        assert_eq!(
            capture_without_allocation(
                &mut empty,
                RasterBorrowInput::Empty { type_oid: 2 },
                RasterWorkspaceDeviceView::default(),
            ),
            RasterBorrowOutcome::EmptyAccepted
        );

        let prepared = PreparedRasterArtifact::new(
            preflight_raster_execution(&spec, snapshot(&[])).expect("empty preflight"),
        );
        assert_borrow_failure(
            RasterLaunchWorkspace::build(&spec, prepared)
                .expect("empty workspace construction is device-free"),
            RasterBorrowInput::Empty { type_oid: 99 },
            RasterWorkspaceDeviceView::default(),
            RasterBorrowFailure::EmptyInputMismatch,
        );

        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Other,
            valid_device_view(),
            RasterBorrowFailure::NonRasterInput,
        );

        for corrupt in [
            RasterBorrowView {
                band_offsets_len: usize::MAX,
                ..valid_borrow_view()
            },
            RasterBorrowView {
                rows_len: usize::MAX,
                ..valid_borrow_view()
            },
            RasterBorrowView {
                bands_len: usize::MAX,
                ..valid_borrow_view()
            },
        ] {
            assert_borrow_failure(
                unallocated_workspace(),
                RasterBorrowInput::Raster(corrupt),
                valid_device_view(),
                RasterBorrowFailure::InputSpanOverflow,
            );
        }

        for changed in [
            RasterBorrowView {
                type_oid: 99,
                ..valid_borrow_view()
            },
            RasterBorrowView {
                rows_len: 0,
                ..valid_borrow_view()
            },
        ] {
            assert_borrow_failure(
                unallocated_workspace(),
                RasterBorrowInput::Raster(changed),
                valid_device_view(),
                RasterBorrowFailure::InputTypeOrRowCountChanged,
            );
        }

        let mut missing = valid_device_view();
        missing.rules = None;
        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Raster(valid_borrow_view()),
            missing,
            RasterBorrowFailure::MissingRules,
        );
        missing = valid_device_view();
        missing.output_offsets = None;
        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Raster(valid_borrow_view()),
            missing,
            RasterBorrowFailure::MissingOutputOffsets,
        );
        missing = valid_device_view();
        missing.output_pixels = None;
        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Raster(valid_borrow_view()),
            missing,
            RasterBorrowFailure::MissingOutputPixels,
        );
        missing = valid_device_view();
        missing.row_actions = None;
        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Raster(valid_borrow_view()),
            missing,
            RasterBorrowFailure::MissingRowActions,
        );
        missing = valid_device_view();
        missing.validation_scratch = None;
        assert_borrow_failure(
            unallocated_workspace(),
            RasterBorrowInput::Raster(valid_borrow_view()),
            missing,
            RasterBorrowFailure::MissingValidationScratch,
        );

        for overflow in [
            RasterWorkspaceDeviceView {
                rules: Some((dangling::<RasterReclassRule>(), usize::MAX)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                output_offsets: Some((dangling::<u64>(), usize::MAX)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                validation_scratch: Some((
                    dangling::<PgaccelResidentRasterValidationScratch>(),
                    usize::MAX,
                )),
                ..valid_device_view()
            },
        ] {
            assert_borrow_failure(
                unallocated_workspace(),
                RasterBorrowInput::Raster(valid_borrow_view()),
                overflow,
                RasterBorrowFailure::WorkspaceSpanOverflow,
            );
        }

        for mismatch in [
            RasterWorkspaceDeviceView {
                rules: Some((dangling::<RasterReclassRule>(), 2)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                output_offsets: Some((dangling::<u64>(), 3)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                output_pixels: Some((dangling::<u8>(), 2)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                row_actions: Some((dangling::<u8>(), 2)),
                ..valid_device_view()
            },
            RasterWorkspaceDeviceView {
                validation_scratch: Some((dangling::<PgaccelResidentRasterValidationScratch>(), 2)),
                ..valid_device_view()
            },
        ] {
            assert_borrow_failure(
                unallocated_workspace(),
                RasterBorrowInput::Raster(valid_borrow_view()),
                mismatch,
                RasterBorrowFailure::WorkspaceLengthMismatch,
            );
        }
    }

    #[test]
    fn only_successful_reconstruction_becomes_publishable() {
        let spec = spec(7);
        let preflight =
            preflight_raster_execution(&spec, snapshot(&[None, Some(raster(1)), Some(raster(2))]))
                .expect("preflight");
        let prepared = PreparedRasterArtifact::new(preflight.clone());
        assert_eq!(
            prepared.published_accounting().retained_host_exact_bytes,
            preflight.accounting.reconstructed_output_bytes
        );
        assert_eq!(
            prepared.transient_peak_bytes(),
            Some(
                preflight.accounting.prelaunch_reserved_bytes
                    + preflight.accounting.post_launch_host_bytes
            )
        );
        assert!(matches!(
            PreparedRasterArtifact::new(preflight.clone()).reconstruct(&[7, 0], &[0, 2, 1]),
            Err(RasterExecutionError::RowActionMismatch { .. })
        ));
        let artifact = prepared
            .reconstruct(&[7, 0], &[0, 2, 2])
            .expect("valid output publishes");
        assert_eq!(
            artifact.accounting(),
            Some(PreparedRasterArtifact::new(preflight).published_accounting())
        );
    }

    #[test]
    fn cursor_preserves_null_rows_and_rewinds_for_rescan() {
        let spec = spec(7);
        let prepared = PreparedRasterArtifact::new(
            preflight_raster_execution(&spec, snapshot(&[None, Some(raster(1))]))
                .expect("preflight"),
        );
        let artifact = prepared.reconstruct(&[7], &[0, 2]).expect("artifact");
        let mut cursor = RasterOutputCursor::new();
        assert_eq!(cursor.next(&artifact), Some(RasterOutputValue::Null));
        assert!(matches!(
            cursor.next(&artifact),
            Some(RasterOutputValue::Wkb(value)) if value[HEADER_BYTES..] == [4, 0, 7]
        ));
        assert_eq!(cursor.next(&artifact), None);
        assert_eq!(cursor.position(), 2);
        cursor.reset();
        assert_eq!(cursor.position(), 0);
        assert_eq!(cursor.next(&artifact), Some(RasterOutputValue::Null));
    }

    #[test]
    fn empty_artifact_has_exact_offset_only_accounting() {
        let spec = spec(7);
        let prepared = PreparedRasterArtifact::new(
            preflight_raster_execution(&spec, snapshot(&[])).expect("empty preflight"),
        );
        assert_eq!(prepared.transient_peak_bytes(), Some(24));
        let mut workspace =
            RasterLaunchWorkspace::build(&spec, prepared).expect("empty workspace needs no GPU");
        assert_eq!(
            workspace.accounting(),
            Some(ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: 24,
            })
        );
        assert_eq!(
            workspace.published_accounting(),
            ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: 8,
            }
        );
        assert_eq!(
            workspace.launch(&ResidentColumnView::Empty {
                type_oid: pgrx::pg_sys::Oid::from(spec.raster_type_oid),
            }),
            RasterBorrowOutcome::EmptyAccepted
        );
        let artifact = workspace.finalize().expect("empty artifact");
        assert_eq!(artifact.row_count(), 0);
        assert_eq!(
            artifact.accounting(),
            Some(ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: 8,
            })
        );
        assert_eq!(RasterOutputCursor::new().next(&artifact), None);
    }
}

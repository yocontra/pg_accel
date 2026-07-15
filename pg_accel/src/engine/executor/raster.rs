//! Store-neutral artifact boundary for childless resident raster execution.

use std::time::Instant;

use pgrx::pg_sys;

use crate::engine::ffi::syscache::{PostgisRasterCatalogIdentity, postgis_raster_datum_from_wkb};
use crate::engine::raster::{
    RasterExecutionError, RasterExecutionPreflight, RasterExecutionSizing, RasterExecutionSnapshot,
    RasterQuerySpec, RasterReclassRule, RasterReconstructedOutput, RasterSpecCodecError,
    preflight_raster_execution, reconstruct_raster_output, revalidate_raster_catalog,
    size_empty_raster_execution, size_raster_execution,
};
use crate::engine::residency::{
    ArtifactEnsureOutcome, DerivedArtifact, DerivedArtifactIdentity, ResidentByteAccounting,
    ResidentColumnRef, ResidentColumnView, ResidentInputBundle, ResidentLoadError,
    ResidentRasterBand, ResidentRasterRow, ResidentRasterStats, RetainedExactValues,
    SelectedRelation, SelectedRelationsEnsureOutcome, StagedTransformPreflight,
    StagedTransformWorkspace, ensure_selected_relations, ensure_staged_device_transform_artifact,
    with_derived_artifact_inputs,
};
#[cfg(feature = "pg_test")]
use crate::gpu::injected_raster_resident_failure;
use crate::gpu::{
    ExprDeviceBuffer, GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail,
    PGACCEL_RESIDENT_RASTER_ABI_VERSION, PgaccelRasterReclassResidentRequest,
    PgaccelResidentRasterBand, PgaccelResidentRasterReclassRule, PgaccelResidentRasterRow,
    PgaccelResidentRasterValidationScratch, PgaccelResidentRasterView, RasterResidentLaunchOutcome,
    prepare_raster_reclass_resident, raster_reclass_resident_launch,
    raster_reclass_resident_launch_result, raster_reclass_resident_validation,
};

pub(crate) const RASTER_OUTPUT_MEMORY_CONTEXT_NAME: &str = "pg_accel_raster_output";

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

#[cfg(feature = "pg_test")]
thread_local! {
    static TEST_RASTER_KERNEL_FAILURE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Inject a failing raw result after the real resident kernel has returned.
#[cfg(feature = "pg_test")]
pub(crate) fn with_test_raster_kernel_failure<R>(f: impl FnOnce() -> R) -> R {
    struct FailureGuard;

    impl Drop for FailureGuard {
        fn drop(&mut self) {
            TEST_RASTER_KERNEL_FAILURE_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    TEST_RASTER_KERNEL_FAILURE_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = FailureGuard;
    f()
}

#[cfg(feature = "pg_test")]
fn test_raster_kernel_failure_enabled() -> bool {
    TEST_RASTER_KERNEL_FAILURE_DEPTH.with(|depth| depth.get() > 0)
}

/// Fully decoded childless raster execution contract. The canonical RQS2
/// words are also the complete derived-artifact cache identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterExecPlan {
    spec: RasterQuerySpec,
    identity: DerivedArtifactIdentity,
    selected_relation: SelectedRelation,
    column: ResidentColumnRef,
}

impl RasterExecPlan {
    pub fn decode_words(words: &[i32]) -> Result<Self, RasterSpecCodecError> {
        Self::from_spec(RasterQuerySpec::decode_words(words)?)
    }

    pub fn from_spec(spec: RasterQuerySpec) -> Result<Self, RasterSpecCodecError> {
        let identity = raster_artifact_identity(&spec)?;
        let relid = pgrx::pg_sys::Oid::from(spec.relation_oid);
        let attno = i16::try_from(spec.raster_attno).map_err(|_| {
            RasterSpecCodecError::InvalidSpec(
                crate::engine::raster::RasterSpecError::InvalidRasterAttno(spec.raster_attno),
            )
        })?;
        Ok(Self {
            selected_relation: SelectedRelation {
                relid,
                columns: vec![attno],
            },
            column: ResidentColumnRef { relid, attno },
            spec,
            identity,
        })
    }

    #[must_use]
    pub const fn spec(&self) -> &RasterQuerySpec {
        &self.spec
    }

    #[must_use]
    pub const fn identity(&self) -> &DerivedArtifactIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn selected_relation(&self) -> &SelectedRelation {
        &self.selected_relation
    }

    #[must_use]
    pub const fn column(&self) -> ResidentColumnRef {
        self.column
    }

    /// First-execution boundary after Begin/rescan: reprove the
    /// replacement-sensitive catalog, ensure the exact resident relation
    /// generation, and build or resolve the generation-stamped output
    /// artifact.
    ///
    /// # Safety
    /// Must run on the PostgreSQL backend main thread.
    pub unsafe fn ensure_ready(&self) -> Result<RasterExecReady, ResidentLoadError> {
        // SAFETY: ensure_ready's contract puts us on the PostgreSQL backend main
        // thread at the Begin/ReScan boundary, which is exactly
        // revalidate_raster_catalog's requirement.
        let catalog = unsafe { revalidate_raster_catalog(&self.spec) }.map_err(|error| {
            ResidentLoadError::Loader(format!("raster catalog revalidation failed: {error}"))
        })?;
        let selected = ensure_selected_relations(std::slice::from_ref(&self.selected_relation))?;
        let artifact = self.ensure_artifact()?;
        let (row_count, accounting) = with_derived_artifact_inputs::<RasterOutputArtifact, _>(
            self.selected_relation.relid,
            &self.identity,
            std::slice::from_ref(&self.column),
            |resolved| (resolved.artifact.row_count(), resolved.accounting),
        )?;
        Ok(RasterExecReady {
            catalog,
            selected,
            artifact,
            row_count,
            accounting,
        })
    }

    fn ensure_artifact(&self) -> Result<ArtifactEnsureOutcome, ResidentLoadError> {
        let owner = self.selected_relation.relid;
        let columns = std::slice::from_ref(&self.column);
        ensure_staged_device_transform_artifact(
            owner,
            &self.identity,
            std::slice::from_ref(&owner),
            columns,
            |inputs| {
                let sizing = size_from_inputs(&self.spec, inputs)?;
                transform_preflight(sizing)
            },
            |sizing, inputs| {
                let snapshot = snapshot_from_inputs(&self.spec, inputs)?;
                let preflight =
                    preflight_raster_execution(&self.spec, snapshot).map_err(execution_error)?;
                if preflight.accounting != sizing.accounting {
                    return Err(ResidentLoadError::Loader(
                        "resident raster sizing changed while preparing its charged snapshot"
                            .to_owned(),
                    ));
                }
                Ok(PreparedRasterArtifact::new(preflight))
            },
            |prepared| RasterLaunchWorkspace::build(&self.spec, prepared),
            |workspace, dispatch| {
                let column = dispatch.column(0)?;
                let _ = workspace.launch(&column);
                Ok(())
            },
            |workspace| {
                let artifact = workspace.finalize()?;
                // SAFETY: ensure_artifact is only reached from ensure_ready (backend main
                // thread contract), and ensure_staged_device_transform_artifact runs this
                // finalize closure synchronously on the calling thread.
                unsafe { revalidate_raster_catalog(&self.spec) }.map_err(|error| {
                    ResidentLoadError::Loader(format!(
                        "raster catalog changed before artifact publication: {error}"
                    ))
                })?;
                Ok(artifact)
            },
        )
    }
}

/// Evidence established on the first tuple request after BeginCustomScan or
/// ReScanCustomScan.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterExecReady {
    pub catalog: PostgisRasterCatalogIdentity,
    pub selected: SelectedRelationsEnsureOutcome,
    pub artifact: ArtifactEnsureOutcome,
    pub row_count: usize,
    pub accounting: ResidentByteAccounting,
}

#[derive(Debug)]
struct RasterOutputMemoryContext {
    parent: pg_sys::MemoryContext,
    context: pg_sys::MemoryContext,
    output_slot: *mut pg_sys::TupleTableSlot,
}

impl RasterOutputMemoryContext {
    fn new(
        parent: pg_sys::MemoryContext,
        output_slot: *mut pg_sys::TupleTableSlot,
    ) -> Result<Self, ResidentLoadError> {
        if parent.is_null() {
            return Err(ResidentLoadError::Loader(
                "raster CustomScan has no executor query memory context".to_owned(),
            ));
        }
        Ok(Self {
            parent,
            context: std::ptr::null_mut(),
            output_slot,
        })
    }

    /// Clear the slot while its Datum is still live, then release all prior
    /// output allocations in one reset.
    ///
    /// # Safety
    /// Must run on the backend main thread while `output_slot` and any owned
    /// context remain live under the executor query context.
    unsafe fn clear_and_reset(&mut self) {
        if !self.output_slot.is_null() {
            // Clear even when no context exists: a prior NULL virtual tuple
            // still leaves the slot nonempty and must not be stored over.
            // SAFETY: the scan slot remains executor-owned until EndCustomScan.
            unsafe { pg_sys::ExecClearTuple(self.output_slot) };
        }
        if self.context.is_null() {
            return;
        }
        // SAFETY: clearing the slot removed its last reference to allocations
        // owned by this dedicated child context.
        unsafe { pg_sys::MemoryContextReset(self.context) };
    }

    /// Import one exact raster Datum inside the bounded output context.
    ///
    /// # Safety
    /// Must run on the PostgreSQL backend main thread with a freshly proved
    /// catalog identity and external WKB bytes live for this call.
    unsafe fn import(
        &mut self,
        catalog: &PostgisRasterCatalogIdentity,
        wkb: &[u8],
    ) -> Result<pg_sys::Datum, String> {
        let context = unsafe { self.ensure_context()? };
        // SAFETY: context is our live child of the executor query context.
        let previous = unsafe { pg_sys::MemoryContextSwitchTo(context) };
        let guard = MemoryContextSwitchGuard { previous };
        // SAFETY: the catalog and WKB contracts are upheld by the caller; the
        // importer catches PostgreSQL ERRORs and returns them as Rust errors.
        let result = unsafe { postgis_raster_datum_from_wkb(catalog, wkb) };
        drop(guard);
        if result.is_err() {
            // No slot references a value from the failed import. Free any
            // temporary bytea/importer allocations before escalating.
            unsafe { pg_sys::MemoryContextReset(context) };
        }
        result
    }

    /// # Safety
    /// Must run on the PostgreSQL backend main thread while `parent` is live.
    unsafe fn ensure_context(&mut self) -> Result<pg_sys::MemoryContext, String> {
        if self.context.is_null() {
            // SAFETY: parent is the live EState query context captured at
            // BeginCustomScan. PostgreSQL owns the child on ERROR unwind.
            self.context = unsafe {
                pg_sys::AllocSetContextCreateInternal(
                    self.parent,
                    c"pg_accel_raster_output".as_ptr(),
                    pg_sys::ALLOCSET_DEFAULT_MINSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_INITSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_MAXSIZE as pg_sys::Size,
                )
            };
            if self.context.is_null() {
                return Err(format!(
                    "could not create {RASTER_OUTPUT_MEMORY_CONTEXT_NAME} memory context"
                ));
            }
        }
        Ok(self.context)
    }

    /// Clear any live output Datum before deleting its owning context.
    ///
    /// # Safety
    /// Must run on the backend main thread before the executor scan slot or
    /// query memory context is destroyed.
    unsafe fn release(&mut self) {
        if !self.output_slot.is_null() {
            // Clear NULL and non-NULL virtual tuples alike before End returns.
            // SAFETY: the slot is still owned by this CustomScanState.
            unsafe { pg_sys::ExecClearTuple(self.output_slot) };
        }
        if self.context.is_null() {
            return;
        }
        // SAFETY: this is the dedicated child context owned by this state.
        unsafe { pg_sys::MemoryContextDelete(self.context) };
        self.context = std::ptr::null_mut();
    }
}

struct MemoryContextSwitchGuard {
    previous: pg_sys::MemoryContext,
}

impl Drop for MemoryContextSwitchGuard {
    fn drop(&mut self) {
        if !self.previous.is_null() {
            // SAFETY: the previous CurrentMemoryContext remains live for the
            // synchronous backend callback that created this guard.
            unsafe { pg_sys::MemoryContextSwitchTo(self.previous) };
        }
    }
}

/// Childless CustomScan state for one exact RQS2 transform.
pub struct RasterExecState {
    plan: RasterExecPlan,
    cursor: RasterOutputCursor,
    ready: Option<RasterExecReady>,
    output_memory: RasterOutputMemoryContext,
    rows_dispatched: u64,
    batches_executed: u64,
    dispatch_time_us: u64,
}

impl RasterExecState {
    /// Validate the immutable plan/slot contract without loading, dispatching,
    /// or publishing a derived artifact. PostgreSQL invokes BeginCustomScan
    /// for plain EXPLAIN, so all execution work must remain lazy.
    ///
    /// # Safety
    /// Must run on the PostgreSQL backend main thread. `output_slot` must be
    /// the initialized scan slot for the childless raster CustomScan.
    pub unsafe fn begin(
        plan: RasterExecPlan,
        output_slot: *mut pg_sys::TupleTableSlot,
        query_context: pg_sys::MemoryContext,
    ) -> Result<Self, ResidentLoadError> {
        unsafe { validate_output_slot(&plan, output_slot) }?;
        Ok(Self {
            plan,
            cursor: RasterOutputCursor::new(),
            ready: None,
            output_memory: RasterOutputMemoryContext::new(query_context, output_slot)?,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        })
    }

    /// Invalidate readiness and rewind output. The first subsequent tuple
    /// request must reprove catalog and resident generations before use.
    /// # Safety
    /// Must run on the PostgreSQL backend main thread while the scan slot and
    /// executor query context captured at BeginCustomScan remain live.
    pub unsafe fn reset_for_rescan(&mut self) {
        // SAFETY: ReScanCustomScan runs before the executor reuses this slot.
        unsafe { self.output_memory.clear_and_reset() };
        self.ready = None;
        self.cursor.reset();
        self.rows_dispatched = 0;
        self.batches_executed = 0;
        self.dispatch_time_us = 0;
    }

    /// Establish the exact execution snapshot once for the current scan pass.
    ///
    /// # Safety
    /// Must run on the PostgreSQL backend main thread.
    unsafe fn ensure_ready_for_execution(&mut self) -> Result<(), ResidentLoadError> {
        if self.ready.is_some() {
            return Ok(());
        }
        let started = Instant::now();
        let ready = unsafe { self.plan.ensure_ready() }?;
        let (rows_dispatched, batches_executed, dispatch_time_us) =
            execution_counters(&ready, started.elapsed());
        self.ready = Some(ready);
        self.rows_dispatched = rows_dispatched;
        self.batches_executed = batches_executed;
        self.dispatch_time_us = dispatch_time_us;
        Ok(())
    }

    /// Materialize the next reconstructed raster into a one-column virtual
    /// tuple. The derived artifact and its raw dependency remain pinned under
    /// the store borrow until any non-NULL bytes have been copied to a Datum.
    ///
    /// # Safety
    /// Must run on the PostgreSQL backend main thread. `output_slot` must be
    /// the initialized scan slot validated by [`Self::begin`].
    unsafe fn next(
        &mut self,
        output_slot: *mut pg_sys::TupleTableSlot,
    ) -> Result<*mut pg_sys::TupleTableSlot, ResidentLoadError> {
        if output_slot != self.output_memory.output_slot {
            return Err(ResidentLoadError::Loader(
                "raster CustomScan output slot changed after BeginCustomScan".to_owned(),
            ));
        }
        // PostgreSQL has finished consuming the prior virtual tuple. Clear its
        // slot reference before resetting the context that owns its Datum.
        unsafe { self.output_memory.clear_and_reset() };
        unsafe { self.ensure_ready_for_execution() }?;
        let owner = self.plan.selected_relation.relid;
        let catalog = &self
            .ready
            .as_ref()
            .expect("raster execution readiness was just established")
            .catalog;
        let value = with_derived_artifact_inputs::<RasterOutputArtifact, _>(
            owner,
            &self.plan.identity,
            std::slice::from_ref(&self.plan.column),
            |resolved| match self.cursor.next(resolved.artifact) {
                Some(RasterOutputValue::Null) => Ok(Some((pg_sys::Datum::from(0), true))),
                Some(RasterOutputValue::Wkb(wkb)) => {
                    // SAFETY: executor callbacks run on the backend main thread;
                    // the WKB slice remains valid for this complete store borrow
                    // and first execution after Begin/ReScan proved this exact
                    // PostGIS importer.
                    unsafe { self.output_memory.import(catalog, wkb) }
                        .map_err(ResidentLoadError::Loader)
                        .map(|datum| Some((datum, false)))
                }
                None => Ok(None),
            },
        )??;
        let Some((datum, is_null)) = value else {
            return Ok(output_slot);
        };
        // SAFETY: the exact one-column slot shape and storage pointers were
        // validated at BeginCustomScan.
        unsafe {
            *(*output_slot).tts_values = datum;
            *(*output_slot).tts_isnull = is_null;
            pg_sys::ExecStoreVirtualTuple(output_slot);
        }
        Ok(output_slot)
    }

    #[must_use]
    pub const fn plan(&self) -> &RasterExecPlan {
        &self.plan
    }

    #[must_use]
    pub const fn ready(&self) -> Option<&RasterExecReady> {
        self.ready.as_ref()
    }
}

impl Drop for RasterExecState {
    fn drop(&mut self) {
        // SAFETY: RasterExecState is dropped synchronously by EndCustomScan on
        // the backend main thread. On ERROR unwind, the parent es_query_cxt
        // owns and releases this child even when EndCustomScan is bypassed.
        unsafe { self.output_memory.release() };
    }
}

fn execution_counters(ready: &RasterExecReady, elapsed: std::time::Duration) -> (u64, u64, u64) {
    let built = !matches!(ready.artifact, ArtifactEnsureOutcome::Hit);
    let rows = if built {
        u64::try_from(ready.row_count).unwrap_or(u64::MAX)
    } else {
        0
    };
    let batches = u64::from(built && ready.row_count > 0);
    let elapsed_us = if batches == 0 {
        0
    } else {
        u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
    };
    (rows, batches, elapsed_us)
}

unsafe fn validate_output_slot(
    plan: &RasterExecPlan,
    output_slot: *mut pg_sys::TupleTableSlot,
) -> Result<(), ResidentLoadError> {
    if output_slot.is_null() {
        return Err(ResidentLoadError::Loader(
            "raster CustomScan has no output slot".to_owned(),
        ));
    }
    // SAFETY: caller promises an initialized executor-owned slot.
    let descriptor = unsafe { (*output_slot).tts_tupleDescriptor };
    if descriptor.is_null() || unsafe { (*descriptor).natts } != 1 {
        return Err(ResidentLoadError::Loader(
            "raster CustomScan output must contain exactly one attribute".to_owned(),
        ));
    }
    // SAFETY: the descriptor has exactly one valid attribute.
    let attribute = unsafe { &*crate::engine::pg_compat::tuple_desc_attr(descriptor, 0) };
    if u32::from(attribute.atttypid) != plan.spec.raster_type_oid {
        return Err(ResidentLoadError::Loader(
            "raster CustomScan output type does not match its RQS2 contract".to_owned(),
        ));
    }
    // SAFETY: output_slot is initialized and valid for field reads.
    if unsafe { (*output_slot).tts_values.is_null() || (*output_slot).tts_isnull.is_null() } {
        return Err(ResidentLoadError::Loader(
            "raster CustomScan output slot has no value storage".to_owned(),
        ));
    }
    Ok(())
}

impl crate::engine::executor::state::ExecutorState for RasterExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: trait contract guarantees a valid CustomScanState and the
        // scan slot validated by RasterExecState::begin.
        let slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        // SAFETY: same trait contract; errors are hard execution failures and
        // must never fall through to the native PostGIS expression.
        unsafe { self.next(slot) }
            .unwrap_or_else(|error| pgrx::error!("pg_accel: raster execution failed: {error}"))
    }

    fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }

    fn batches_executed(&self) -> u64 {
        self.batches_executed
    }

    fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }
}

fn canonical_empty_snapshot() -> RasterExecutionSnapshot {
    RasterExecutionSnapshot {
        stats: ResidentRasterStats::empty(),
        exact: RetainedExactValues {
            offsets: vec![0].into_boxed_slice(),
            bytes: Box::default(),
        },
    }
}

fn input_parts<'a>(
    spec: &RasterQuerySpec,
    inputs: &'a ResidentInputBundle<'a>,
) -> Result<Option<(&'a ResidentRasterStats, &'a RetainedExactValues)>, ResidentLoadError> {
    let column = inputs.columns.first().ok_or_else(|| {
        ResidentLoadError::Loader("resident raster lifecycle resolved no input column".to_owned())
    })?;
    match column {
        ResidentColumnView::Raster {
            type_oid,
            stats,
            exact,
            ..
        } if u32::from(*type_oid) == spec.raster_type_oid => Ok(Some((stats, exact))),
        ResidentColumnView::Empty { type_oid } if u32::from(*type_oid) == spec.raster_type_oid => {
            Ok(None)
        }
        _ => Err(ResidentLoadError::Loader(
            "resident raster lifecycle resolved a different column type".to_owned(),
        )),
    }
}

fn size_from_inputs(
    spec: &RasterQuerySpec,
    inputs: ResidentInputBundle<'_>,
) -> Result<RasterExecutionSizing, ResidentLoadError> {
    match input_parts(spec, &inputs)? {
        Some((stats, exact)) => size_raster_execution(spec, stats, exact),
        None => size_empty_raster_execution(spec),
    }
    .map_err(execution_error)
}

fn snapshot_from_inputs(
    spec: &RasterQuerySpec,
    inputs: ResidentInputBundle<'_>,
) -> Result<RasterExecutionSnapshot, ResidentLoadError> {
    Ok(match input_parts(spec, &inputs)? {
        Some((stats, exact)) => RasterExecutionSnapshot {
            stats: stats.clone(),
            exact: exact.clone(),
        },
        None => canonical_empty_snapshot(),
    })
}

fn transform_preflight(
    sizing: RasterExecutionSizing,
) -> Result<StagedTransformPreflight<RasterExecutionSizing>, ResidentLoadError> {
    let transient_host_bytes = sizing
        .accounting
        .snapshot_host_bytes
        .checked_add(sizing.accounting.layout_host_bytes)
        .and_then(|bytes| bytes.checked_add(sizing.accounting.post_launch_host_bytes))
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
    Ok(StagedTransformPreflight {
        published_accounting: ResidentByteAccounting {
            device_bytes: 0,
            retained_host_exact_bytes: sizing.accounting.reconstructed_output_bytes,
        },
        transient_accounting: ResidentByteAccounting {
            device_bytes: sizing.accounting.device_artifact_bytes,
            retained_host_exact_bytes: transient_host_bytes,
        },
        prepared: sizing,
    })
}

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
        let outcome = unsafe { raster_reclass_resident_launch(&request) };
        #[cfg(feature = "pg_test")]
        let outcome = if test_raster_kernel_failure_enabled() {
            injected_raster_resident_failure()
        } else {
            outcome
        };
        self.borrow_outcome = RasterBorrowOutcome::Native(outcome);
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

impl StagedTransformWorkspace for RasterLaunchWorkspace {
    fn device_bytes(&self) -> u64 {
        self.accounting()
            .map_or(u64::MAX, |accounting| accounting.device_bytes)
    }

    fn host_bytes(&self) -> u64 {
        self.accounting()
            .map_or(u64::MAX, |accounting| accounting.retained_host_exact_bytes)
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

    use crate::adapters::extractors::raster::parse_resident_raster;
    use crate::engine::raster::{
        RasterExecutionSnapshot, RasterPixelType, RasterReclassRule, RasterReclassSpec,
        preflight_raster_execution,
    };
    use crate::engine::residency::{
        ResidentRasterBand, ResidentRasterData, ResidentRasterRow, RetainedExactValues,
    };

    fn allocation_count(f: impl FnOnce()) -> usize {
        struct CountingGuard(bool);

        impl Drop for CountingGuard {
            fn drop(&mut self) {
                if self.0 {
                    let _ = crate::engine::residency::finish_test_allocation_count();
                }
            }
        }

        crate::engine::residency::begin_test_allocation_count();
        let mut guard = CountingGuard(true);
        f();
        guard.0 = false;
        drop(guard);
        crate::engine::residency::finish_test_allocation_count()
    }

    const HEADER_BYTES: usize = 61;

    fn catalog_identity() -> PostgisRasterCatalogIdentity {
        PostgisRasterCatalogIdentity {
            extension_oid: pg_sys::Oid::from(10),
            schema_oid: pg_sys::Oid::from(11),
            raster_type_oid: pg_sys::Oid::from(2),
            summary_stats_type_oid: pg_sys::Oid::from(12),
            reclass_fn_oid: pg_sys::Oid::from(3),
            summary_stats_fn_oid: pg_sys::Oid::from(13),
            summary_stats_default_band_fn_oid: pg_sys::Oid::from(14),
            as_wkb_fn_oid: pg_sys::Oid::from(6),
            rast_from_wkb_fn_oid: pg_sys::Oid::from(7),
            reclass_impl_fn_oid: pg_sys::Oid::from(15),
            summary_stats_impl_fn_oid: pg_sys::Oid::from(16),
            fingerprint_words: vec![4, 5],
        }
    }

    fn spec(rule_destination: i64) -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 1,
            raster_attno: 1,
            raster_type_oid: 2,
            function_oid: 3,
            as_wkb_fn_oid: 6,
            rast_from_wkb_fn_oid: 7,
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
    fn begin_plan_decode_binds_exact_identity_relation_and_column() {
        let expected = spec(7);
        let words = expected.encode_words().expect("canonical RQS2");
        let plan = RasterExecPlan::decode_words(&words).expect("executor plan");
        assert_eq!(plan.spec(), &expected);
        assert_eq!(plan.identity().canonical_words(), words);
        assert_eq!(
            plan.selected_relation(),
            &SelectedRelation {
                relid: pgrx::pg_sys::Oid::from(expected.relation_oid),
                columns: vec![i16::try_from(expected.raster_attno).expect("test attno")],
            }
        );
        assert_eq!(
            plan.column(),
            ResidentColumnRef {
                relid: pgrx::pg_sys::Oid::from(expected.relation_oid),
                attno: i16::try_from(expected.raster_attno).expect("test attno"),
            }
        );
    }

    #[test]
    fn raster_output_context_is_query_bound_and_lazy() {
        let slot = dangling::<pg_sys::TupleTableSlot>();
        let error = RasterOutputMemoryContext::new(std::ptr::null_mut(), slot)
            .expect_err("a raster output context requires es_query_cxt");
        assert!(error.to_string().contains("query memory context"));

        let parent = dangling::<pg_sys::MemoryContextData>();
        let lifetime = std::mem::ManuallyDrop::new(
            RasterOutputMemoryContext::new(parent, slot).expect("lazy output lifetime"),
        );
        assert_eq!(lifetime.parent, parent);
        assert_eq!(lifetime.output_slot, slot);
        assert!(
            lifetime.context.is_null(),
            "BeginCustomScan must not allocate output memory for plain EXPLAIN"
        );
    }

    #[test]
    fn rescan_reuses_the_canonical_plan_identity_and_invalidates_readiness() {
        let plan = RasterExecPlan::from_spec(spec(7)).expect("executor plan");
        let identity = plan.identity().clone();
        let relation = plan.selected_relation().clone();
        let mut state = RasterExecState {
            plan,
            cursor: RasterOutputCursor { next_row: 9 },
            ready: Some(RasterExecReady {
                catalog: catalog_identity(),
                selected: SelectedRelationsEnsureOutcome {
                    evidence: Vec::new(),
                    loaded_relations: Vec::new(),
                    raw_load_ms: 0.0,
                },
                artifact: ArtifactEnsureOutcome::Built,
                row_count: 9,
                accounting: ResidentByteAccounting::default(),
            }),
            output_memory: RasterOutputMemoryContext {
                parent: dangling::<pg_sys::MemoryContextData>(),
                context: std::ptr::null_mut(),
                output_slot: std::ptr::null_mut(),
            },
            rows_dispatched: 9,
            batches_executed: 1,
            dispatch_time_us: 17,
        };
        // SAFETY: the output context remains lazy/null, so this pure unit test
        // does not call PostgreSQL memory or slot APIs.
        unsafe { state.reset_for_rescan() };
        assert_eq!(state.cursor.position(), 0);
        assert!(state.ready().is_none());
        assert!(state.output_memory.context.is_null());
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert_eq!(state.plan.identity(), &identity);
        assert_eq!(state.plan.selected_relation(), &relation);
    }

    #[test]
    fn executor_counters_charge_only_nonempty_builds() {
        let ready = |artifact, row_count| RasterExecReady {
            catalog: catalog_identity(),
            selected: SelectedRelationsEnsureOutcome {
                evidence: Vec::new(),
                loaded_relations: Vec::new(),
                raw_load_ms: 0.0,
            },
            artifact,
            row_count,
            accounting: ResidentByteAccounting::default(),
        };
        assert_eq!(
            execution_counters(
                &ready(ArtifactEnsureOutcome::Built, 3),
                std::time::Duration::from_micros(17),
            ),
            (3, 1, 17)
        );
        assert_eq!(
            execution_counters(
                &ready(ArtifactEnsureOutcome::Rebuilt, 0),
                std::time::Duration::from_micros(17),
            ),
            (0, 0, 0)
        );
        assert_eq!(
            execution_counters(
                &ready(ArtifactEnsureOutcome::Hit, 3),
                std::time::Duration::from_micros(17),
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn staged_lifecycle_accounting_separates_output_from_transient_workspace() {
        let spec = spec(7);
        let snapshot = snapshot(&[None, Some(raster(1)), Some(raster(2))]);
        let sizing = size_raster_execution(&spec, &snapshot.stats, &snapshot.exact)
            .expect("nonowning sizing");
        let expected = sizing.accounting;
        let staged = transform_preflight(sizing).expect("accounting fits");
        assert_eq!(
            staged.published_accounting,
            ResidentByteAccounting {
                device_bytes: 0,
                retained_host_exact_bytes: expected.reconstructed_output_bytes,
            }
        );
        assert_eq!(
            staged.transient_accounting,
            ResidentByteAccounting {
                device_bytes: expected.device_artifact_bytes,
                retained_host_exact_bytes: expected.snapshot_host_bytes
                    + expected.layout_host_bytes
                    + expected.post_launch_host_bytes,
            }
        );
        assert_eq!(
            staged
                .published_accounting
                .checked_total()
                .expect("published total")
                + staged
                    .transient_accounting
                    .checked_total()
                    .expect("transient total"),
            expected.peak_reserved_bytes
        );
    }

    #[test]
    fn first_borrow_sizing_allocates_nothing_for_nonempty_and_typed_empty_inputs() {
        let spec = spec(7);
        let snapshot = snapshot(&[None, Some(raster(1))]);
        let mut nonempty = None;
        let nonempty_allocations = allocation_count(|| {
            nonempty = Some(size_raster_execution(
                &spec,
                &snapshot.stats,
                &snapshot.exact,
            ));
        });
        assert_eq!(nonempty_allocations, 0);
        assert!(nonempty.expect("sizing ran").is_ok());

        let inputs = ResidentInputBundle {
            columns: vec![ResidentColumnView::Empty {
                type_oid: pgrx::pg_sys::Oid::from(spec.raster_type_oid),
            }],
            evidence: Vec::new(),
        };
        let mut empty = None;
        let empty_allocations = allocation_count(|| {
            empty = Some(size_from_inputs(&spec, inputs));
        });
        assert_eq!(empty_allocations, 0);
        let empty = empty.expect("typed-empty sizing ran").expect("sizing");
        assert_eq!(empty.row_count(), 0);
        assert_eq!(empty.accounting.snapshot_host_bytes, 8);
        assert_eq!(empty.accounting.prelaunch_reserved_bytes, 24);
        assert_eq!(empty.accounting.peak_reserved_bytes, 32);
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

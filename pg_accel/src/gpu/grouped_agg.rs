//! Safe ownership and lifecycle facade for the frozen grouped-aggregate ABI.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::engine::spec::abi;

use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelMemSpace,
    PgaccelStatus, PgaccelValTag, bridge,
};

const DESC_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
const OUT_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggOut>() as u32;
const WORKSPACE_REQ_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggWorkspaceReq>() as u32;

fn descriptor_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Descriptor,
        GpuOperation::ValidateDeviceInput,
        GpuStatusDetail::InvalidDescriptor,
        detail,
    )
}

fn capacity_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Descriptor,
        GpuOperation::ValidateDeviceOutput,
        GpuStatusDetail::CapacityOverflow,
        detail,
    )
}

fn allocation_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Memory,
        GpuOperation::BuildColumnBatch,
        GpuStatusDetail::OutOfMemory,
        detail,
    )
}

fn validate_descriptor_shell(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<()> {
    let key_count = usize::try_from(desc.key_count)
        .map_err(|_| descriptor_error("group key count does not fit usize"))?;
    let measure_count = usize::try_from(desc.measure_count)
        .map_err(|_| descriptor_error("measure count does not fit usize"))?;
    let dim_count = usize::try_from(desc.dim_count)
        .map_err(|_| descriptor_error("dimension count does not fit usize"))?;
    if desc.abi_version != abi::PGACCEL_OLAP_ABI_VERSION || desc.size_bytes != DESC_SIZE {
        return Err(descriptor_error(
            "grouped descriptor ABI version/size mismatch",
        ));
    }
    if key_count > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        || measure_count == 0
        || measure_count > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || dim_count > abi::PGACCEL_GROUPED_AGG_MAX_DIMS
        || desc.group_capacity == 0
    {
        return Err(descriptor_error(
            "grouped descriptor count/capacity is invalid",
        ));
    }
    if !matches!(
        desc.grouping_mode,
        abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX | abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
    ) || !matches!(
        desc.output_mode,
        abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE | abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
    ) {
        return Err(descriptor_error("grouped descriptor mode is invalid"));
    }
    if desc.execution_flags != 0
        || desc.flags != 0
        || desc.pad0 != 0
        || desc.pad1 != 0
        || desc.pad2 != 0
        || !desc.scratch.is_null()
        || desc.scratch_bytes != 0
        || desc.scratch_space != 0
        || desc.scratch_alignment != 0
    {
        return Err(descriptor_error(
            "resolved plan must use canonical execution/workspace fields",
        ));
    }
    if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        && desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
    {
        return Err(descriptor_error("HASH grouping requires COMPACT output"));
    }
    Ok(())
}

/// A resolver-owned descriptor whose input pointers remain valid for `'input`.
///
/// Pointer provenance cannot be proven by Rust because residency allocates
/// through AdaptiveCpp. Construction is therefore unsafe, while every later
/// lifecycle transition is safe and preserves the descriptor's borrow.
pub struct ResolvedGroupedAggPlan<'input> {
    desc: abi::PgaccelGroupedAggDesc,
    identity: Rc<()>,
    _input_lifetime: PhantomData<&'input [u8]>,
}

impl<'input> ResolvedGroupedAggPlan<'input> {
    /// Adopt a fully resolved descriptor.
    ///
    /// # Safety
    /// Every active descriptor pointer must address the frozen logical length
    /// in the declared AdaptiveCpp context and remain live for `'input`.
    pub unsafe fn from_abi(desc: abi::PgaccelGroupedAggDesc) -> GpuResult<Self> {
        validate_descriptor_shell(&desc)?;
        Ok(Self {
            desc,
            identity: Rc::new(()),
            _input_lifetime: PhantomData,
        })
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.desc.row_count
    }

    #[must_use]
    pub const fn group_capacity(&self) -> usize {
        self.desc.group_capacity
    }

    #[must_use]
    pub const fn descriptor(&self) -> &abi::PgaccelGroupedAggDesc {
        &self.desc
    }

    /// Use the plan's current row pointers as one lifecycle chunk.
    #[must_use]
    pub fn chunk(&self) -> GroupedAggChunk<'_, 'input> {
        GroupedAggChunk {
            desc: self.desc,
            _plan: PhantomData,
            _input_lifetime: PhantomData,
            _not_send_sync: PhantomData,
        }
    }

    /// Resolve one bounded contiguous row range from this plan.
    ///
    /// All row-shaped pointers are advanced together. Dictionary and
    /// dimension lookup pointers remain fixed because they describe the
    /// session-wide grouping shape, not fact-row storage.
    pub fn row_chunk(
        &self,
        first_row: usize,
        row_count: usize,
    ) -> GpuResult<GroupedAggChunk<'_, 'input>> {
        let end = first_row
            .checked_add(row_count)
            .ok_or_else(|| descriptor_error("grouped chunk row range overflow"))?;
        if end > self.desc.row_count {
            return Err(descriptor_error(
                "grouped chunk row range exceeds the resolved input",
            ));
        }

        let mut desc = self.desc;
        desc.row_count = row_count;
        advance_descriptor_rows(&mut desc, first_row)?;
        // SAFETY: `from_abi` established that every active pointer spans the
        // plan's full logical row count. The checked subrange above advances
        // only row-shaped pointers and retains the frozen session shape.
        unsafe { GroupedAggChunk::from_abi(self, desc) }
    }
}

fn value_width(tag: PgaccelValTag) -> GpuResult<usize> {
    match tag {
        PgaccelValTag::Bool => Ok(1),
        PgaccelValTag::Int32 | PgaccelValTag::Float32 | PgaccelValTag::Date => Ok(4),
        PgaccelValTag::Int64 | PgaccelValTag::Float64 | PgaccelValTag::Timestamp => Ok(8),
        PgaccelValTag::Null => Err(descriptor_error(
            "active grouped row column has a NULL physical tag",
        )),
    }
}

fn checked_byte_offset(rows: usize, element_bytes: usize) -> GpuResult<usize> {
    let bytes = rows
        .checked_mul(element_bytes)
        .ok_or_else(|| descriptor_error("grouped chunk pointer offset overflow"))?;
    if bytes > isize::MAX as usize {
        return Err(descriptor_error(
            "grouped chunk pointer offset exceeds Rust allocation bounds",
        ));
    }
    Ok(bytes)
}

fn advance_byte_pointer<T>(pointer: *const T, rows: usize) -> GpuResult<*const T> {
    if pointer.is_null() {
        return Ok(pointer);
    }
    let offset = checked_byte_offset(rows, std::mem::size_of::<T>())?;
    // SAFETY: the resolved-plan constructor requires every active pointer to
    // span its full logical row count, and the caller checked this subrange.
    Ok(unsafe { pointer.cast::<u8>().add(offset).cast::<T>() })
}

fn advance_usm_column(column: &mut super::PgaccelExprUsmCol, rows: usize) -> GpuResult<()> {
    if !column.values.is_null() {
        let offset = checked_byte_offset(rows, value_width(column.tag)?)?;
        // SAFETY: the resolved-plan constructor pins the complete typed input
        // span and `offset` names a checked row boundary inside that span.
        column.values = unsafe { column.values.cast::<u8>().add(offset).cast() };
    }
    column.nulls = advance_byte_pointer(column.nulls, rows)?;
    Ok(())
}

fn advance_measure_column(
    column: &mut abi::PgaccelGroupedAggMeasureCol,
    rows: usize,
) -> GpuResult<()> {
    if !column.values.is_null() {
        let element_bytes = usize::try_from(column.element_bytes)
            .map_err(|_| descriptor_error("grouped measure width does not fit usize"))?;
        if element_bytes == 0 {
            return Err(descriptor_error(
                "active grouped measure has zero element width",
            ));
        }
        let offset = checked_byte_offset(rows, element_bytes)?;
        // SAFETY: the resolved-plan constructor pins the complete measure
        // span and the checked row range cannot exceed it.
        column.values = unsafe { column.values.cast::<u8>().add(offset).cast() };
    }
    column.nulls = advance_byte_pointer(column.nulls, rows)?;
    Ok(())
}

fn advance_filter(filter: &mut abi::PgaccelGroupedAggFilter, rows: usize) -> GpuResult<()> {
    filter.mask = advance_byte_pointer(filter.mask, rows)?;
    Ok(())
}

fn advance_descriptor_rows(
    desc: &mut abi::PgaccelGroupedAggDesc,
    first_row: usize,
) -> GpuResult<()> {
    let key_count = usize::try_from(desc.key_count)
        .map_err(|_| descriptor_error("group key count does not fit usize"))?;
    for key in desc
        .keys
        .get_mut(..key_count)
        .ok_or_else(|| descriptor_error("grouped chunk key count exceeds the frozen descriptor"))?
    {
        if key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT {
            advance_usm_column(&mut key.values, first_row)?;
        }
    }

    let measure_count = usize::try_from(desc.measure_count)
        .map_err(|_| descriptor_error("measure count does not fit usize"))?;
    for measure in desc.measures.get_mut(..measure_count).ok_or_else(|| {
        descriptor_error("grouped chunk measure count exceeds the frozen descriptor")
    })? {
        advance_measure_column(&mut measure.value, first_row)?;
        advance_measure_column(&mut measure.rhs, first_row)?;
    }
    advance_filter(&mut desc.where_filter, first_row)?;
    for filter in desc
        .measure_filters
        .get_mut(..measure_count)
        .ok_or_else(|| {
            descriptor_error("grouped chunk filter count exceeds the frozen descriptor")
        })?
    {
        advance_filter(filter, first_row)?;
    }

    let dimension_count = usize::try_from(desc.dim_count)
        .map_err(|_| descriptor_error("dimension count does not fit usize"))?;
    for dimension in desc.dims.get_mut(..dimension_count).ok_or_else(|| {
        descriptor_error("grouped chunk dimension count exceeds the frozen descriptor")
    })? {
        advance_usm_column(&mut dimension.fact_key, first_row)?;
    }
    Ok(())
}

/// One row-pointer/count slice for a multi-call grouped aggregation.
pub struct GroupedAggChunk<'plan, 'input> {
    desc: abi::PgaccelGroupedAggDesc,
    _plan: PhantomData<&'plan ResolvedGroupedAggPlan<'input>>,
    _input_lifetime: PhantomData<&'input [u8]>,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl<'plan, 'input> GroupedAggChunk<'plan, 'input> {
    /// Adopt advanced row pointers for another chunk of `plan`.
    ///
    /// # Safety
    /// Active row pointers must be advanced consistently, remain valid for
    /// `desc.row_count`, and share the plan's AdaptiveCpp context.
    pub unsafe fn from_abi(
        plan: &'plan ResolvedGroupedAggPlan<'input>,
        desc: abi::PgaccelGroupedAggDesc,
    ) -> GpuResult<Self> {
        validate_descriptor_shell(&desc)?;
        if desc.row_count > plan.desc.row_count {
            return Err(descriptor_error(
                "chunk exceeds the row count used to size its workspace",
            ));
        }
        if !stable_shape_matches(&plan.desc, &desc) {
            return Err(descriptor_error("chunk changes a frozen plan shape"));
        }
        Ok(Self {
            desc,
            _plan: PhantomData,
            _input_lifetime: PhantomData,
            _not_send_sync: PhantomData,
        })
    }
}

fn val_eq(left: &super::PgaccelVal, right: &super::PgaccelVal) -> bool {
    left.tag == right.tag && left.data == right.data
}

fn filter_shape_eq(
    left: &abi::PgaccelGroupedAggFilter,
    right: &abi::PgaccelGroupedAggFilter,
) -> bool {
    left.kind == right.kind
        && left.predicate_source == right.predicate_source
        && left.predicate_measure_slot == right.predicate_measure_slot
        && left.predicate_range_count == right.predicate_range_count
        && left.value_cmp_opcode == right.value_cmp_opcode
        && left.pad0 == right.pad0
        && left.flags == right.flags
        && left.mask.is_null() == right.mask.is_null()
        && val_eq(&left.value_cmp_const, &right.value_cmp_const)
        && left
            .predicate_lo
            .iter()
            .zip(&right.predicate_lo)
            .all(|(a, b)| val_eq(a, b))
        && left
            .predicate_hi
            .iter()
            .zip(&right.predicate_hi)
            .all(|(a, b)| val_eq(a, b))
}

fn measure_col_shape_eq(
    left: &abi::PgaccelGroupedAggMeasureCol,
    right: &abi::PgaccelGroupedAggMeasureCol,
) -> bool {
    left.values.is_null() == right.values.is_null()
        && left.nulls.is_null() == right.nulls.is_null()
        && left.physical_type == right.physical_type
        && left.element_bytes == right.element_bytes
        && left.scale == right.scale
        && left.flags == right.flags
}

fn stable_shape_matches(
    left: &abi::PgaccelGroupedAggDesc,
    right: &abi::PgaccelGroupedAggDesc,
) -> bool {
    if left.abi_version != right.abi_version
        || left.size_bytes != right.size_bytes
        || left.grouping_mode != right.grouping_mode
        || left.output_mode != right.output_mode
        || left.key_count != right.key_count
        || left.group_capacity != right.group_capacity
        || left.measure_count != right.measure_count
        || left.flags != right.flags
        || left.dim_count != right.dim_count
        || !filter_shape_eq(&left.where_filter, &right.where_filter)
    {
        return false;
    }
    for index in 0..left.key_count as usize {
        let a = &left.keys[index];
        let b = &right.keys[index];
        if a.values.tag != b.values.tag
            || a.values.values.is_null() != b.values.values.is_null()
            || a.values.nulls.is_null() != b.values.nulls.is_null()
            || a.lookup_by_key != b.lookup_by_key
            || a.source != b.source
            || a.code_min != b.code_min
            || a.cardinality != b.cardinality
            || a.null_code != b.null_code
            || a.flags != b.flags
            || a.pad0 != b.pad0
        {
            return false;
        }
    }
    for index in 0..left.measure_count as usize {
        let a = &left.measures[index];
        let b = &right.measures[index];
        if !measure_col_shape_eq(&a.value, &b.value)
            || !measure_col_shape_eq(&a.rhs, &b.rhs)
            || a.op != b.op
            || a.agg_mask != b.agg_mask
            || a.accumulator_kind != b.accumulator_kind
            || a.state_bytes != b.state_bytes
            || a.flags != b.flags
            || a.pad0 != b.pad0
            || !filter_shape_eq(&left.measure_filters[index], &right.measure_filters[index])
        {
            return false;
        }
    }
    for index in 0..left.dim_count as usize {
        let a = &left.dims[index];
        let b = &right.dims[index];
        if a.fact_key.tag != b.fact_key.tag
            || a.fact_key.values.is_null() != b.fact_key.values.is_null()
            || a.fact_key.nulls.is_null() != b.fact_key.nulls.is_null()
            || a.match_by_key != b.match_by_key
            || a.multiplicity_by_key != b.multiplicity_by_key
            || a.key_min != b.key_min
            || a.key_count != b.key_count
            || a.flags != b.flags
            || a.pad0 != b.pad0
        {
            return false;
        }
    }
    true
}

/// Caller-owned, aligned USM workspace. The allocation and queue are
/// backend-local, so the owner is deliberately neither `Send` nor `Sync`.
pub struct GroupedAggWorkspace {
    ptr: Option<NonNull<c_void>>,
    requirement: abi::PgaccelGroupedAggWorkspaceReq,
    poisoned: bool,
    _not_send_sync: PhantomData<Rc<()>>,
}

fn workspace_query_descriptor(desc: &abi::PgaccelGroupedAggDesc) -> abi::PgaccelGroupedAggDesc {
    let mut desc = *desc;
    if desc.execution_flags == 0 {
        desc.execution_flags = abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    }
    desc
}

impl GroupedAggWorkspace {
    pub fn allocate(plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<Self> {
        Self::allocate_for_descriptor(&plan.desc)
    }

    fn allocate_for_descriptor(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<Self> {
        crate::ensure_backend_exit_callback();
        let query_desc = workspace_query_descriptor(desc);
        let mut requirement = abi::PgaccelGroupedAggWorkspaceReq {
            abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
            size_bytes: WORKSPACE_REQ_SIZE,
            bytes: 0,
            alignment: 0,
            space: 0,
            flags: 0,
        };
        // SAFETY: both references point to correctly pinned ABI structs for
        // the duration of the synchronous call.
        let status = unsafe {
            bridge::pgaccel_grouped_agg_workspace_requirements(
                std::ptr::from_ref(&query_desc),
                std::ptr::from_mut(&mut requirement),
            )
        };
        super::status_to_result(
            status,
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("workspace_requirements"),
        )?;
        if requirement.abi_version != abi::PGACCEL_OLAP_ABI_VERSION
            || requirement.size_bytes != WORKSPACE_REQ_SIZE
            || requirement.flags != 0
            || requirement.bytes == 0
            || requirement.alignment == 0
            || !requirement.alignment.is_power_of_two()
            || !matches!(
                requirement.space,
                x if x == PgaccelMemSpace::SharedUsm as i32
                    || x == PgaccelMemSpace::Device as i32
            )
        {
            return Err(descriptor_error(
                "kernel returned invalid workspace requirements",
            ));
        }
        let _ = u32::try_from(requirement.alignment)
            .map_err(|_| descriptor_error("workspace alignment exceeds ABI width"))?;

        let mut raw = std::ptr::null_mut();
        // SAFETY: raw is a valid out pointer and requirements were checked.
        let status = unsafe {
            bridge::pgaccel_grouped_agg_workspace_alloc(
                requirement.bytes,
                requirement.alignment,
                requirement.space,
                std::ptr::from_mut(&mut raw),
            )
        };
        super::status_to_result(
            status,
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("workspace_alloc"),
        )?;
        let ptr = NonNull::new(raw)
            .ok_or_else(|| allocation_error("workspace allocator returned NULL"))?;
        if !(ptr.as_ptr() as usize).is_multiple_of(requirement.alignment) {
            // SAFETY: pointer came from the matching allocator above.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(ptr.as_ptr()) };
            return Err(descriptor_error(
                "workspace allocator broke alignment contract",
            ));
        }

        crate::note_backend_gpu_owner_acquired();
        Ok(Self {
            ptr: Some(ptr),
            requirement,
            poisoned: false,
            _not_send_sync: PhantomData,
        })
    }

    #[must_use]
    pub const fn requirement(&self) -> &abi::PgaccelGroupedAggWorkspaceReq {
        &self.requirement
    }

    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn apply_to(&self, desc: &mut abi::PgaccelGroupedAggDesc) -> GpuResult<()> {
        desc.scratch = self.ptr.map_or(std::ptr::null_mut(), NonNull::as_ptr);
        desc.scratch_bytes = self.requirement.bytes;
        desc.scratch_space = self.requirement.space;
        desc.scratch_alignment = u32::try_from(self.requirement.alignment)
            .map_err(|_| descriptor_error("workspace alignment exceeds ABI width"))?;
        Ok(())
    }
}

impl Drop for GroupedAggWorkspace {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr.take() {
            // SAFETY: pointer came from the matching grouped allocator and is
            // released exactly once by this backend-local owner.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(ptr.as_ptr()) };
        }
    }
}

struct RawHostBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
    len_bytes: usize,
}

impl RawHostBuffer {
    fn zeroed(elements: usize, element_bytes: usize) -> GpuResult<Self> {
        let len_bytes = elements
            .checked_mul(element_bytes)
            .ok_or_else(|| capacity_error("output buffer byte length overflow"))?;
        if len_bytes == 0 {
            return Err(descriptor_error("output state has zero element width"));
        }
        let next_alignment = element_bytes
            .checked_next_power_of_two()
            .ok_or_else(|| capacity_error("output state alignment overflow"))?;
        let alignment = next_alignment.clamp(std::mem::align_of::<u64>(), 64);
        let layout = Layout::from_size_align(len_bytes, alignment)
            .map_err(|_| capacity_error("invalid output buffer layout"))?;
        // SAFETY: layout has nonzero size and valid power-of-two alignment.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).ok_or_else(|| allocation_error("output allocation failed"))?;
        Ok(Self {
            ptr,
            layout,
            len_bytes,
        })
    }

    fn as_mut_void(&mut self) -> *mut c_void {
        self.ptr.as_ptr().cast()
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: the allocation is live and initialized for len_bytes bytes.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len_bytes) }
    }
}

impl Drop for RawHostBuffer {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with this exact layout and is freed once.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

fn zero_vec<T: Clone + Default>(len: usize) -> GpuResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| allocation_error("output vector allocation failed"))?;
    values.resize(len, T::default());
    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedAggStateLane {
    Sum,
    Min,
    Max,
    SumSq,
    RhsSum,
}

struct MeasureOutputStorage {
    accumulator_kind: i32,
    state_bytes: usize,
    sum: Option<RawHostBuffer>,
    min: Option<RawHostBuffer>,
    max: Option<RawHostBuffer>,
    sumsq: Option<RawHostBuffer>,
    count: Option<Vec<u64>>,
    nonnull_count: Option<Vec<u64>>,
    rhs_sum: Option<RawHostBuffer>,
    rhs_count: Option<Vec<u64>>,
    rhs_nonnull_count: Option<Vec<u64>>,
}

impl MeasureOutputStorage {
    fn empty() -> Self {
        Self {
            accumulator_kind: 0,
            state_bytes: 0,
            sum: None,
            min: None,
            max: None,
            sumsq: None,
            count: None,
            nonnull_count: None,
            rhs_sum: None,
            rhs_count: None,
            rhs_nonnull_count: None,
        }
    }

    fn new(measure: &abi::PgaccelGroupedAggMeasure, capacity: usize) -> GpuResult<Self> {
        let mask = measure.agg_mask;
        if mask & !abi::PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN != 0 {
            return Err(descriptor_error(
                "measure contains unknown aggregate lane bits",
            ));
        }
        let state_bytes = usize::try_from(measure.state_bytes)
            .map_err(|_| descriptor_error("measure state width does not fit usize"))?;
        let mut output = Self::empty();
        output.accumulator_kind = measure.accumulator_kind;
        output.state_bytes = state_bytes;
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_SUM != 0 {
            output.sum = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_MIN != 0 {
            output.min = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_MAX != 0 {
            output.max = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ != 0 {
            output.sumsq = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_COUNT != 0 {
            output.count = Some(zero_vec(capacity)?);
        }
        let value_state = mask
            & (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                | abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ)
            != 0;
        let count_column = measure.op != abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
            && mask & abi::PGACCEL_GROUPED_AGG_LANE_COUNT != 0;
        if value_state || count_column {
            output.nonnull_count = Some(zero_vec(capacity)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_SUM != 0 {
            output.rhs_sum = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
            output.rhs_nonnull_count = Some(zero_vec(capacity)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_COUNT != 0 {
            output.rhs_count = Some(zero_vec(capacity)?);
        }
        Ok(output)
    }

    fn raw(&mut self) -> abi::PgaccelGroupedAggMeasureOut {
        abi::PgaccelGroupedAggMeasureOut {
            sum: self
                .sum
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            min: self
                .min
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            max: self
                .max
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            sumsq: self
                .sumsq
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            count: self
                .count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            nonnull_count: self
                .nonnull_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            rhs_sum: self
                .rhs_sum
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            rhs_count: self
                .rhs_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            rhs_nonnull_count: self
                .rhs_nonnull_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
        }
    }

    fn state(&self, lane: GroupedAggStateLane) -> Option<&[u8]> {
        match lane {
            GroupedAggStateLane::Sum => self.sum.as_ref(),
            GroupedAggStateLane::Min => self.min.as_ref(),
            GroupedAggStateLane::Max => self.max.as_ref(),
            GroupedAggStateLane::SumSq => self.sumsq.as_ref(),
            GroupedAggStateLane::RhsSum => self.rhs_sum.as_ref(),
        }
        .map(RawHostBuffer::as_bytes)
    }

    fn state_at<const WIDTH: usize>(
        &self,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<[u8; WIDTH]> {
        if self.state_bytes != WIDTH {
            return Err(descriptor_error(
                "aggregate state width does not match typed accessor",
            ));
        }
        let offset = group
            .checked_mul(WIDTH)
            .ok_or_else(|| capacity_error("aggregate state offset overflow"))?;
        let end = offset
            .checked_add(WIDTH)
            .ok_or_else(|| capacity_error("aggregate state end offset overflow"))?;
        self.state(lane)
            .and_then(|bytes| bytes.get(offset..end))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| capacity_error("aggregate state index is out of bounds"))
    }
}

fn key_width(tag: PgaccelValTag) -> Option<usize> {
    match tag {
        PgaccelValTag::Null => None,
        PgaccelValTag::Bool => Some(1),
        PgaccelValTag::Int32 | PgaccelValTag::Float32 | PgaccelValTag::Date => Some(4),
        PgaccelValTag::Int64 | PgaccelValTag::Float64 | PgaccelValTag::Timestamp => Some(8),
    }
}

fn key_output_nullable(desc: &abi::PgaccelGroupedAggDesc, key: &abi::PgaccelGroupedAggKey) -> bool {
    if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        && key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
    {
        !key.values.nulls.is_null()
    } else {
        key.null_code != abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE
    }
}

/// Host-visible buffers owned for one grouped-aggregate result.
pub struct GroupedAggOutputStorage {
    capacity: usize,
    dense_output: bool,
    active_groups: Option<Vec<u8>>,
    group_codes: Option<Vec<usize>>,
    key_values: [Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_nulls: [Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_types: [i32; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    measures: [MeasureOutputStorage; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
    plan_identity: Rc<()>,
}

impl GroupedAggOutputStorage {
    pub fn new(plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<Self> {
        let desc = &plan.desc;
        let capacity = desc.group_capacity;
        let key_count = usize::try_from(desc.key_count)
            .map_err(|_| descriptor_error("group key count does not fit usize"))?;
        let measure_count = usize::try_from(desc.measure_count)
            .map_err(|_| descriptor_error("measure count does not fit usize"))?;
        let dense_output = desc.output_mode == abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
        let mut active_groups = dense_output.then(|| zero_vec(capacity)).transpose()?;
        let group_codes = (!dense_output
            && desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX)
            .then(|| zero_vec(capacity))
            .transpose()?;
        let mut key_values: [Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS] =
            std::array::from_fn(|_| None);
        let mut key_nulls: [Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS] =
            std::array::from_fn(|_| None);
        let mut key_types = [0; abi::PGACCEL_GROUPED_AGG_MAX_KEYS];
        if !dense_output {
            for index in 0..key_count {
                let key = &desc.keys[index];
                let tag = if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
                    && key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
                {
                    key.values.tag
                } else {
                    PgaccelValTag::Int32
                };
                let width = key_width(tag)
                    .ok_or_else(|| descriptor_error("compact output key has invalid type"))?;
                key_values[index] = Some(RawHostBuffer::zeroed(capacity, width)?);
                key_types[index] = tag as i32;
                if key_output_nullable(desc, key) {
                    key_nulls[index] = Some(zero_vec(capacity)?);
                }
            }
        }
        let mut measures = std::array::from_fn(|_| MeasureOutputStorage::empty());
        for (index, storage) in measures.iter_mut().enumerate().take(measure_count) {
            *storage = MeasureOutputStorage::new(&desc.measures[index], capacity)?;
        }

        // Keep the allocation mutable so its pointer remains stable for raw().
        if let Some(active) = &mut active_groups {
            debug_assert_eq!(active.len(), capacity);
        }
        Ok(Self {
            capacity,
            dense_output,
            active_groups,
            group_codes,
            key_values,
            key_nulls,
            key_types,
            measures,
            plan_identity: Rc::clone(&plan.identity),
        })
    }

    fn validate_for_plan(&self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        if Rc::ptr_eq(&self.plan_identity, &plan.identity) {
            Ok(())
        } else {
            Err(descriptor_error(
                "grouped output storage belongs to a different resolved plan",
            ))
        }
    }

    fn raw(&mut self) -> abi::PgaccelGroupedAggOut {
        let keys = std::array::from_fn(|index| abi::PgaccelGroupedAggKeyOut {
            values: self.key_values[index]
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            nulls: self.key_nulls[index]
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            value_type: self.key_types[index],
            flags: 0,
        });
        let measures = std::array::from_fn(|index| self.measures[index].raw());
        abi::PgaccelGroupedAggOut {
            abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
            size_bytes: OUT_SIZE,
            group_capacity: self.capacity,
            output_space: PgaccelMemSpace::Host as i32,
            flags: 0,
            group_codes: self
                .group_codes
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            active_groups: self
                .active_groups
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            keys,
            measures,
            emitted_group_count: 0,
            selected_count: 0,
            uncertain_count: 0,
        }
    }

    fn finish(&self, raw: &abi::PgaccelGroupedAggOut) -> GpuResult<GroupedAggOutcome> {
        if raw.abi_version != abi::PGACCEL_OLAP_ABI_VERSION
            || raw.size_bytes != OUT_SIZE
            || raw.group_capacity != self.capacity
            || raw.output_space != PgaccelMemSpace::Host as i32
            || raw.flags != 0
            || raw.emitted_group_count > self.capacity
        {
            return Err(capacity_error(
                "kernel returned invalid grouped output metadata",
            ));
        }
        if self.dense_output {
            let active = self
                .active_groups
                .as_ref()
                .ok_or_else(|| descriptor_error("dense output lost active-group buffer"))?;
            let active_count = active.iter().copied().map(usize::from).sum::<usize>();
            if active.iter().any(|value| *value > 1) || active_count != raw.emitted_group_count {
                return Err(descriptor_error(
                    "dense active-group metadata is inconsistent",
                ));
            }
        }
        let result = GroupedAggResult {
            group_capacity: self.capacity,
            emitted_group_count: raw.emitted_group_count,
            selected_count: raw.selected_count,
            uncertain_count: raw.uncertain_count,
        };
        if result.uncertain_count == 0 {
            Ok(GroupedAggOutcome::Complete(result))
        } else {
            Ok(GroupedAggOutcome::NeedsRecheck(result))
        }
    }

    #[must_use]
    pub fn active_groups(&self) -> Option<&[u8]> {
        self.active_groups.as_deref()
    }

    #[must_use]
    pub fn group_codes(&self) -> Option<&[usize]> {
        self.group_codes.as_deref()
    }

    #[must_use]
    pub fn key_values(&self, index: usize) -> Option<&[u8]> {
        self.key_values
            .get(index)?
            .as_ref()
            .map(RawHostBuffer::as_bytes)
    }

    #[must_use]
    pub fn key_nulls(&self, index: usize) -> Option<&[u8]> {
        self.key_nulls.get(index)?.as_deref()
    }

    #[must_use]
    pub fn key_type(&self, index: usize) -> Option<i32> {
        self.key_values.get(index)?.as_ref()?;
        self.key_types.get(index).copied()
    }

    #[must_use]
    pub fn measure_state(&self, index: usize, lane: GroupedAggStateLane) -> Option<&[u8]> {
        self.measures.get(index)?.state(lane)
    }

    pub fn measure_i64_at(
        &self,
        index: usize,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<i64> {
        let measure = self
            .measures
            .get(index)
            .ok_or_else(|| capacity_error("aggregate measure index is out of bounds"))?;
        if measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_I64 {
            return Err(descriptor_error(
                "aggregate state is not an I64 accumulator",
            ));
        }
        Ok(i64::from_ne_bytes(measure.state_at(lane, group)?))
    }

    pub fn measure_f64_at(
        &self,
        index: usize,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<f64> {
        let measure = self
            .measures
            .get(index)
            .ok_or_else(|| capacity_error("aggregate measure index is out of bounds"))?;
        if measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_F64 {
            return Err(descriptor_error(
                "aggregate state is not an F64 accumulator",
            ));
        }
        Ok(f64::from_ne_bytes(measure.state_at(lane, group)?))
    }

    #[must_use]
    pub fn measure_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.count.as_deref()
    }

    #[must_use]
    pub fn measure_nonnull_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.nonnull_count.as_deref()
    }

    #[must_use]
    pub fn measure_rhs_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.rhs_count.as_deref()
    }

    #[must_use]
    pub fn measure_rhs_nonnull_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.rhs_nonnull_count.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupedAggResult {
    pub group_capacity: usize,
    pub emitted_group_count: usize,
    pub selected_count: u64,
    pub uncertain_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedAggOutcome {
    Complete(GroupedAggResult),
    NeedsRecheck(GroupedAggResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    Ready,
    Accumulating,
    Finalized,
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    Accumulate,
    Finalize,
    Reset,
}

fn lifecycle_flags(
    state: LifecycleState,
    action: LifecycleAction,
) -> GpuResult<(u32, LifecycleState)> {
    match (state, action) {
        (LifecycleState::Ready, LifecycleAction::Accumulate) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE,
            LifecycleState::Accumulating,
        )),
        (LifecycleState::Accumulating, LifecycleAction::Accumulate) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE,
            LifecycleState::Accumulating,
        )),
        (LifecycleState::Ready, LifecycleAction::Finalize) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE,
            LifecycleState::Finalized,
        )),
        (LifecycleState::Accumulating, LifecycleAction::Finalize) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE,
            LifecycleState::Finalized,
        )),
        (_, LifecycleAction::Reset) => {
            Ok((abi::PGACCEL_GROUPED_AGG_EXEC_RESET, LifecycleState::Ready))
        }
        (
            LifecycleState::Finalized | LifecycleState::Poisoned,
            LifecycleAction::Accumulate | LifecycleAction::Finalize,
        ) => Err(descriptor_error(
            "invalid grouped-aggregate lifecycle transition",
        )),
    }
}

const fn status_poisons_workspace(status: PgaccelStatus) -> bool {
    !matches!(status, PgaccelStatus::Ok | PgaccelStatus::ErrorUnsupported)
}

fn grouped_agg_kernel_error(status: PgaccelStatus, detail: i32) -> GpuError {
    if status == PgaccelStatus::Error
        && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW
    {
        GpuError::new(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            GpuStatusDetail::NumericOverflow,
        )
    } else {
        GpuError::from_status(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            status,
        )
    }
}

fn lifecycle_call_descriptor(
    desc: &abi::PgaccelGroupedAggDesc,
    flags: u32,
) -> abi::PgaccelGroupedAggDesc {
    let mut desc = *desc;
    desc.execution_flags = flags;
    if flags & abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE == 0 {
        desc.row_count = 0;
    }
    desc
}

fn execute_call(
    desc: &abi::PgaccelGroupedAggDesc,
    flags: u32,
    workspace: &mut GroupedAggWorkspace,
    mut output: Option<&mut GroupedAggOutputStorage>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    let mut desc = lifecycle_call_descriptor(desc, flags);
    workspace.apply_to(&mut desc)?;
    let mut raw_output = output.as_deref_mut().map(GroupedAggOutputStorage::raw);
    let output_ptr = raw_output
        .as_mut()
        .map_or(std::ptr::null_mut(), std::ptr::from_mut);
    // SAFETY: descriptor, optional output, and workspace remain live through
    // the synchronous FFI call and were built from pinned ABI types.
    let mut detail = abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    let status = unsafe {
        // SAFETY: `desc`, `raw_output`, workspace-applied buffers, and `detail`
        // remain live and exclusively accessible for this synchronous bridge call.
        bridge::pgaccel_grouped_agg_execute_ex(
            std::ptr::from_ref(&desc),
            output_ptr,
            std::ptr::from_mut(&mut detail),
        )
    };
    if !status.is_ok() {
        if status_poisons_workspace(status) {
            workspace.poisoned = true;
        }
        return Err(grouped_agg_kernel_error(status, detail));
    }
    if detail != abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE {
        workspace.poisoned = true;
        return Err(descriptor_error(
            "successful grouped aggregate returned a device error detail",
        ));
    }
    if flags & abi::PGACCEL_GROUPED_AGG_EXEC_RESET != 0 {
        workspace.poisoned = false;
    }
    finish_successful_call(workspace, output.as_deref(), raw_output.as_ref())
}

fn finish_successful_call(
    workspace: &mut GroupedAggWorkspace,
    output: Option<&GroupedAggOutputStorage>,
    raw_output: Option<&abi::PgaccelGroupedAggOut>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    match (output, raw_output) {
        (Some(storage), Some(raw)) => match storage.finish(raw) {
            Ok(outcome) => Ok(Some(outcome)),
            Err(error) => {
                workspace.poisoned = true;
                Err(error)
            }
        },
        (None, None) => Ok(None),
        _ => {
            workspace.poisoned = true;
            Err(descriptor_error("grouped output ownership mismatch"))
        }
    }
}

/// Execute RESET|ACCUMULATE|FINALIZE with an externally allocated workspace.
pub fn execute_grouped_agg_one_shot(
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &mut GroupedAggOutputStorage,
) -> GpuResult<GroupedAggOutcome> {
    output.validate_for_plan(plan)?;
    let mut workspace = GroupedAggWorkspace::allocate(plan)?;
    execute_call(
        &plan.desc,
        abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN,
        &mut workspace,
        Some(output),
    )?
    .ok_or_else(|| descriptor_error("finalized grouped call returned no outcome"))
}

/// Stateful multi-call facade reserved by the frozen chunk lifecycle.
///
/// The session owns no input pointer. `plan_shape` retains raw pointer values
/// only as equality tokens and is never submitted to native code. Every
/// accumulate/finalize/reset call must supply a freshly pinned plan or chunk,
/// which lets residency release its store borrow between synchronous calls.
pub struct GroupedAggSession {
    plan_shape: abi::PgaccelGroupedAggDesc,
    workspace_row_capacity: usize,
    workspace: GroupedAggWorkspace,
    state: LifecycleState,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl GroupedAggSession {
    pub fn start(plan: &ResolvedGroupedAggPlan<'_>, max_chunk_rows: usize) -> GpuResult<Self> {
        if plan.desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX {
            return Err(descriptor_error(
                "hash grouped aggregation is one-shot because workspace owners are chunk-relative",
            ));
        }
        let workspace_desc = bounded_workspace_descriptor(plan, max_chunk_rows)?;
        Ok(Self {
            plan_shape: plan.desc,
            workspace_row_capacity: workspace_desc.row_count,
            workspace: GroupedAggWorkspace::allocate_for_descriptor(&workspace_desc)?,
            state: LifecycleState::Ready,
            _not_send_sync: PhantomData,
        })
    }

    fn validate_plan(&self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        validate_session_plan(&self.plan_shape, plan)
    }

    pub fn accumulate(&mut self, chunk: &GroupedAggChunk<'_, '_>) -> GpuResult<()> {
        if chunk.desc.row_count > self.workspace_row_capacity {
            return Err(descriptor_error(
                "grouped chunk exceeds the session workspace row capacity",
            ));
        }
        if !stable_shape_matches(&self.plan_shape, &chunk.desc) {
            return Err(descriptor_error("chunk does not match session plan"));
        }
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Accumulate)?;
        match execute_call(&chunk.desc, flags, &mut self.workspace, None) {
            Ok(None) => {
                self.state = next;
                Ok(())
            }
            Ok(Some(_)) => Err(descriptor_error("accumulate unexpectedly finalized output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    pub fn finalize(
        &mut self,
        plan: &ResolvedGroupedAggPlan<'_>,
        output: &mut GroupedAggOutputStorage,
    ) -> GpuResult<GroupedAggOutcome> {
        validate_session_finalize_inputs(&self.plan_shape, plan, output)?;
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Finalize)?;
        match execute_call(&plan.desc, flags, &mut self.workspace, Some(output)) {
            Ok(Some(outcome)) => {
                self.state = next;
                Ok(outcome)
            }
            Ok(None) => Err(descriptor_error("finalize returned no grouped output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    pub fn reset(&mut self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        self.validate_plan(plan)?;
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Reset)?;
        match execute_call(&plan.desc, flags, &mut self.workspace, None) {
            Ok(None) => {
                self.state = next;
                Ok(())
            }
            Ok(Some(_)) => Err(descriptor_error("reset unexpectedly finalized output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    #[must_use]
    pub const fn workspace(&self) -> &GroupedAggWorkspace {
        &self.workspace
    }
}

fn bounded_workspace_descriptor(
    plan: &ResolvedGroupedAggPlan<'_>,
    max_chunk_rows: usize,
) -> GpuResult<abi::PgaccelGroupedAggDesc> {
    if max_chunk_rows == 0 {
        return Err(descriptor_error(
            "grouped session chunk limit must be greater than zero",
        ));
    }
    let row_count = plan.row_count().min(max_chunk_rows);
    // This is the zero-offset row chunk: every pointer and frozen shape field
    // remains identical to the already validated full plan.
    let mut desc = plan.desc;
    desc.row_count = row_count;
    desc.execution_flags =
        abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    Ok(desc)
}

fn validate_session_plan(
    plan_shape: &abi::PgaccelGroupedAggDesc,
    plan: &ResolvedGroupedAggPlan<'_>,
) -> GpuResult<()> {
    if plan.desc.row_count != plan_shape.row_count || !stable_shape_matches(plan_shape, &plan.desc)
    {
        return Err(descriptor_error(
            "resolved grouped plan changed during a bounded session",
        ));
    }
    Ok(())
}

fn validate_session_finalize_inputs(
    plan_shape: &abi::PgaccelGroupedAggDesc,
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &GroupedAggOutputStorage,
) -> GpuResult<()> {
    validate_session_plan(plan_shape, plan)?;
    output.validate_for_plan(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor_fixture(output_mode: i32) -> abi::PgaccelGroupedAggDesc {
        // SAFETY: every enum-bearing field has zero as a valid discriminant;
        // the fixture then fills all active frozen descriptor fields.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
        desc.size_bytes = DESC_SIZE;
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
        desc.output_mode = output_mode;
        desc.key_count = 1;
        desc.group_capacity = 4;
        desc.keys[0].values.tag = PgaccelValTag::Int32;
        desc.keys[0].source = abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
        desc.keys[0].cardinality = 4;
        desc.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        desc.measure_count = 1;
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].agg_mask =
            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[0].state_bytes = 8;
        desc
    }

    fn plan(output_mode: i32) -> ResolvedGroupedAggPlan<'static> {
        // SAFETY: tests never dispatch this pointer-free, zero-row fixture.
        unsafe { ResolvedGroupedAggPlan::from_abi(descriptor_fixture(output_mode)) }
            .expect("fixture is structurally valid")
    }

    #[test]
    fn output_storage_allocates_exact_requested_lanes() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(storage.active_groups().expect("dense active lane").len(), 4);
        assert!(storage.group_codes().is_none());
        assert_eq!(
            storage
                .measure_state(0, GroupedAggStateLane::Sum)
                .expect("sum state")
                .len(),
            32
        );
        assert!(storage.measure_state(0, GroupedAggStateLane::Min).is_none());
        assert_eq!(storage.measure_count(0).expect("count lane").len(), 4);
        assert_eq!(
            storage
                .measure_nonnull_count(0)
                .expect("validity lane")
                .len(),
            4
        );
        let raw = storage.raw();
        assert!(!raw.active_groups.is_null());
        assert!(raw.group_codes.is_null());
        assert!(raw.keys.iter().all(|key| key.values.is_null()));
        assert!(!raw.measures[0].sum.is_null());
        assert!(!raw.measures[0].count.is_null());
        assert!(!raw.measures[0].nonnull_count.is_null());
        assert!(raw.measures[0].min.is_null());
        assert!(raw.measures[1..].iter().all(|measure| measure.sum.is_null()
            && measure.count.is_null()
            && measure.nonnull_count.is_null()));
    }

    #[test]
    fn count_only_column_allocates_validity_but_count_star_does_not() {
        let mut column_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        column_desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        // SAFETY: this pointer-free fixture is used only to allocate host output.
        let column_plan = unsafe { ResolvedGroupedAggPlan::from_abi(column_desc) }
            .expect("count column fixture is structurally valid");
        let mut column_storage =
            GroupedAggOutputStorage::new(&column_plan).expect("count column output allocates");
        let column_raw = column_storage.raw();
        assert!(!column_raw.measures[0].count.is_null());
        assert!(!column_raw.measures[0].nonnull_count.is_null());

        let mut star_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        star_desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
        star_desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        // SAFETY: canonical COUNT_STAR input views are all zero bytes.
        star_desc.measures[0].value = unsafe { std::mem::zeroed() };
        // SAFETY: canonical COUNT_STAR input views are all zero bytes.
        star_desc.measures[0].rhs = unsafe { std::mem::zeroed() };
        // SAFETY: this pointer-free fixture is used only to allocate host output.
        let star_plan = unsafe { ResolvedGroupedAggPlan::from_abi(star_desc) }
            .expect("count star fixture is structurally valid");
        let mut star_storage =
            GroupedAggOutputStorage::new(&star_plan).expect("count star output allocates");
        let star_raw = star_storage.raw();
        assert!(!star_raw.measures[0].count.is_null());
        assert!(star_raw.measures[0].nonnull_count.is_null());
    }

    #[test]
    fn typed_measure_accessors_enforce_kind_width_and_bounds() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(
            storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 3)
                .expect("i64 state"),
            0
        );
        assert!(
            storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 4)
                .is_err()
        );
        assert!(
            storage
                .measure_f64_at(0, GroupedAggStateLane::Sum, 0)
                .is_err()
        );

        let mut f64_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        f64_desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_F64;
        // SAFETY: pointer-free fixture is used only to allocate host output.
        let f64_plan = unsafe { ResolvedGroupedAggPlan::from_abi(f64_desc) }
            .expect("f64 fixture is structurally valid");
        let f64_storage = GroupedAggOutputStorage::new(&f64_plan).expect("f64 output allocates");
        assert_eq!(
            f64_storage
                .measure_f64_at(0, GroupedAggStateLane::Sum, 0)
                .expect("f64 state"),
            0.0
        );

        let mut narrow_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        narrow_desc.measures[0].state_bytes = 4;
        // SAFETY: pointer-free fixture is used only to allocate host output.
        let narrow_plan = unsafe { ResolvedGroupedAggPlan::from_abi(narrow_desc) }
            .expect("narrow fixture is structurally valid");
        let narrow_storage =
            GroupedAggOutputStorage::new(&narrow_plan).expect("narrow output allocates");
        assert!(
            narrow_storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 0)
                .is_err()
        );
    }

    #[test]
    fn output_storage_is_bound_to_its_exact_resolved_plan() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut narrower = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        narrower.measures[0].state_bytes = 1;
        // SAFETY: pointer-free fixture is not dispatched.
        let other = unsafe { ResolvedGroupedAggPlan::from_abi(narrower) }
            .expect("shell accepts the alternate allocation shape");
        let storage = GroupedAggOutputStorage::new(&other).expect("narrow output allocates");
        assert!(storage.validate_for_plan(&plan).is_err());
        assert!(storage.validate_for_plan(&other).is_ok());
    }

    #[test]
    fn bounded_finalize_requires_storage_from_the_fresh_final_plan() {
        let initial = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        // SAFETY: the pointer-free descriptor is used only by pure validation.
        let final_plan = unsafe { ResolvedGroupedAggPlan::from_abi(initial.desc) }
            .expect("fresh final plan resolves");

        let initial_storage =
            GroupedAggOutputStorage::new(&initial).expect("initial storage allocates");
        assert!(
            validate_session_finalize_inputs(&initial.desc, &final_plan, &initial_storage).is_err(),
            "storage from the setup callback must not pass the final plan identity gate"
        );

        let final_storage =
            GroupedAggOutputStorage::new(&final_plan).expect("final storage allocates");
        validate_session_finalize_inputs(&initial.desc, &final_plan, &final_storage)
            .expect("storage allocated from the exact final plan must pass session validation");
    }

    #[test]
    fn workspace_query_uses_execute_flags_without_mutating_plan() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        assert_eq!(plan.desc.execution_flags, 0);
        let query = workspace_query_descriptor(plan.descriptor());
        assert_eq!(
            query.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN
        );
        assert_eq!(plan.desc.execution_flags, 0);

        let mut lifecycle = *plan.descriptor();
        lifecycle.execution_flags =
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        assert_eq!(
            workspace_query_descriptor(&lifecycle).execution_flags,
            lifecycle.execution_flags
        );
    }

    #[test]
    fn compact_output_allocates_codes_and_typed_keys() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert!(storage.active_groups().is_none());
        assert_eq!(storage.group_codes().expect("stable dense codes").len(), 4);
        assert_eq!(storage.key_values(0).expect("typed key bytes").len(), 16);
        assert!(storage.key_values(1).is_none());
        let raw = storage.raw();
        assert!(raw.active_groups.is_null());
        assert!(!raw.group_codes.is_null());
        assert!(!raw.keys[0].values.is_null());
        assert_eq!(raw.keys[0].value_type, PgaccelValTag::Int32 as i32);
        assert!(raw.keys[1].values.is_null());
    }

    #[test]
    fn hash_dimension_key_output_uses_materialized_int32_codes() {
        static LOOKUP: [i32; 4] = [0, 1, 2, 3];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].values.values = std::ptr::null();
        descriptor.keys[0].values.nulls = std::ptr::null();
        descriptor.keys[0].values.tag = PgaccelValTag::Null;
        descriptor.keys[0].lookup_by_key = LOOKUP.as_ptr();
        descriptor.keys[0].source = abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0;
        descriptor.dim_count = 1;
        descriptor.dims[0].fact_key.tag = PgaccelValTag::Int32;
        descriptor.dims[0].key_count = LOOKUP.len() as u32;
        // SAFETY: static lookup remains valid and the fixture is not dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash dimension fixture is structurally valid");
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(storage.key_values(0).expect("key bytes").len(), 16);
        assert_eq!(storage.key_type(0), Some(PgaccelValTag::Int32 as i32));
        let raw = storage.raw();
        assert_eq!(raw.keys[0].value_type, PgaccelValTag::Int32 as i32);
    }

    #[test]
    fn hash_fact_key_nullability_comes_only_from_input_sidecar() {
        static NULLS: [u8; 1] = [0];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].null_code = 0;
        // SAFETY: pointer-free fixture is not dispatched.
        let nonnullable = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash fact fixture is structurally valid");
        let mut output = GroupedAggOutputStorage::new(&nonnullable).expect("output allocates");
        assert!(output.key_nulls(0).is_none());
        assert!(output.raw().keys[0].nulls.is_null());

        descriptor.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        descriptor.keys[0].values.nulls = NULLS.as_ptr();
        // SAFETY: static sidecar remains valid and the fixture is not dispatched.
        let nullable = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("nullable hash fact fixture is structurally valid");
        let mut output = GroupedAggOutputStorage::new(&nullable).expect("output allocates");
        assert_eq!(output.key_nulls(0).expect("NULL lane").len(), 4);
        assert!(!output.raw().keys[0].nulls.is_null());
    }

    #[test]
    fn hash_fact_key_output_preserves_scalar_width() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].values.tag = PgaccelValTag::Int64;
        // SAFETY: zero-row fixture is not dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("int64 hash fact fixture is structurally valid");
        let mut output = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(output.key_values(0).expect("key bytes").len(), 32);
        assert_eq!(output.key_type(0), Some(PgaccelValTag::Int64 as i32));
        assert_eq!(output.raw().keys[0].value_type, PgaccelValTag::Int64 as i32);
    }

    #[test]
    fn hash_session_is_rejected_before_workspace_allocation() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].values.tag = PgaccelValTag::Int64;
        // SAFETY: pointer-free zero-row fixture is rejected before dispatch.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash fixture is structurally valid");
        assert!(GroupedAggSession::start(&plan, 1).is_err());
    }

    #[test]
    fn bounded_session_workspace_uses_executor_chunk_rows() {
        let mut plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        // The helper only copies this resolved fixture's frozen shape and is
        // never dispatched, so no row pointer is dereferenced by the test.
        plan.desc.row_count = 1_300_000;

        let bounded = bounded_workspace_descriptor(&plan, 256_000)
            .expect("bounded workspace descriptor resolves");
        assert_eq!(bounded.row_count, 256_000);
        assert_eq!(
            bounded.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
        );
        assert!(stable_shape_matches(plan.descriptor(), &bounded));
        assert_eq!(plan.row_count(), 1_300_000);
        assert!(bounded_workspace_descriptor(&plan, 0).is_err());
    }

    #[test]
    fn lifecycle_flags_reject_use_after_finalize_and_allow_reset() {
        let (first, accumulating) =
            lifecycle_flags(LifecycleState::Ready, LifecycleAction::Accumulate)
                .expect("first chunk");
        assert_eq!(
            first,
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
        );
        let (finalize, finalized) =
            lifecycle_flags(accumulating, LifecycleAction::Finalize).expect("finalize");
        assert_eq!(finalize, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        assert!(lifecycle_flags(finalized, LifecycleAction::Accumulate).is_err());
        assert_eq!(
            lifecycle_flags(finalized, LifecycleAction::Reset)
                .expect("reset")
                .1,
            LifecycleState::Ready
        );
    }

    #[test]
    fn non_accumulating_lifecycle_calls_submit_zero_rows() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 37;

        let finalize =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        assert_eq!(finalize.row_count, 0);
        assert_eq!(
            finalize.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE
        );

        let reset = lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_RESET);
        assert_eq!(reset.row_count, 0);
        assert_eq!(reset.execution_flags, abi::PGACCEL_GROUPED_AGG_EXEC_RESET);

        let accumulate =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE);
        assert_eq!(accumulate.row_count, 37);
        let one_shot =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN);
        assert_eq!(one_shot.row_count, 37);
    }

    #[test]
    fn only_real_execution_failures_poison_workspace() {
        assert!(!status_poisons_workspace(PgaccelStatus::Ok));
        assert!(!status_poisons_workspace(PgaccelStatus::ErrorUnsupported));
        for status in [
            PgaccelStatus::Error,
            PgaccelStatus::ErrorOom,
            PgaccelStatus::ErrorTimeout,
            PgaccelStatus::ErrorNoDevice,
            PgaccelStatus::InvalidArgument,
        ] {
            assert!(status_poisons_workspace(status), "{status:?}");
        }
    }

    #[test]
    fn numeric_overflow_detail_refines_only_generic_execution_errors() {
        let overflow = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW,
        );
        assert_eq!(overflow.status, GpuStatusDetail::NumericOverflow);

        let invalid = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID,
        );
        assert_eq!(invalid.status, GpuStatusDetail::ExecutionFailed);

        let oom = grouped_agg_kernel_error(
            PgaccelStatus::ErrorOom,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW,
        );
        assert_eq!(oom.status, GpuStatusDetail::OutOfMemory);
    }

    #[test]
    fn chunk_rejects_shape_drift() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut changed = plan.desc;
        changed.group_capacity = 5;
        // SAFETY: fixture has no live pointers; rejection happens on metadata.
        let result = unsafe { GroupedAggChunk::from_abi(&plan, changed) };
        assert!(result.is_err());
    }

    #[test]
    fn stable_shape_matches_native_row_lane_presence_fingerprint() {
        static KEY_VALUES: [i32; 8] = [0; 8];
        static MEASURE_VALUES: [i64; 8] = [0; 8];
        static RHS_VALUES: [i64; 8] = [0; 8];
        static NULLS: [u8; 8] = [0; 8];
        static MASK: [i8; 8] = [1; 8];
        static DIM_VALUES: [i32; 8] = [0; 8];
        static DIM_MATCH: [u8; 4] = [1; 4];
        static DIM_MULTIPLICITY: [u64; 4] = [1; 4];

        let mut base = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        base.row_count = 8;
        base.keys[0].values.values = KEY_VALUES.as_ptr().cast();
        base.keys[0].values.nulls = NULLS.as_ptr();
        base.keys[0].null_code = 0;
        base.measures[0].value.values = MEASURE_VALUES.as_ptr().cast();
        base.measures[0].value.nulls = NULLS.as_ptr();
        base.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        base.measures[0].value.element_bytes = 8;
        base.measures[0].rhs.values = RHS_VALUES.as_ptr().cast();
        base.measures[0].rhs.nulls = NULLS.as_ptr();
        base.measures[0].rhs.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        base.measures[0].rhs.element_bytes = 8;
        base.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR;
        base.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        base.where_filter.mask = MASK.as_ptr();
        base.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        base.measure_filters[0].mask = MASK.as_ptr();
        base.dim_count = 1;
        base.dims[0].fact_key.values = DIM_VALUES.as_ptr().cast();
        base.dims[0].fact_key.nulls = NULLS.as_ptr();
        base.dims[0].fact_key.tag = PgaccelValTag::Int32;
        base.dims[0].match_by_key = DIM_MATCH.as_ptr();
        base.dims[0].multiplicity_by_key = DIM_MULTIPLICITY.as_ptr();
        base.dims[0].key_count = 4;

        let mut mutations = Vec::new();
        let mut changed = base;
        changed.keys[0].values.values = std::ptr::null();
        mutations.push(("key values", changed));
        changed = base;
        changed.keys[0].values.nulls = std::ptr::null();
        mutations.push(("key nulls", changed));
        changed = base;
        changed.measures[0].value.values = std::ptr::null();
        mutations.push(("measure values", changed));
        changed = base;
        changed.measures[0].value.nulls = std::ptr::null();
        mutations.push(("measure nulls", changed));
        changed = base;
        changed.measures[0].rhs.values = std::ptr::null();
        mutations.push(("rhs values", changed));
        changed = base;
        changed.measures[0].rhs.nulls = std::ptr::null();
        mutations.push(("rhs nulls", changed));
        changed = base;
        changed.where_filter.mask = std::ptr::null();
        mutations.push(("where SQL mask", changed));
        changed = base;
        changed.measure_filters[0].mask = std::ptr::null();
        mutations.push(("measure SQL mask", changed));
        changed = base;
        changed.dims[0].fact_key.values = std::ptr::null();
        mutations.push(("dimension fact values", changed));
        changed = base;
        changed.dims[0].fact_key.nulls = std::ptr::null();
        mutations.push(("dimension fact nulls", changed));

        for (label, changed) in mutations {
            assert!(
                !stable_shape_matches(&base, &changed),
                "{label} presence drift must fail before native workspace poisoning"
            );
        }

        let mut advanced = base;
        advance_descriptor_rows(&mut advanced, 1).expect("advance row lanes");
        assert!(
            stable_shape_matches(&base, &advanced),
            "row-lane addresses may advance when their presence and fixed lookup identity stay stable"
        );
    }

    #[test]
    fn chunk_cannot_exceed_workspace_query_shape() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        // SAFETY: fixture has no live pointers and is not dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");
        let mut oversized = plan.desc;
        oversized.row_count = 9;
        // SAFETY: rejection happens before any pointer is adopted or dispatched.
        let result = unsafe { GroupedAggChunk::from_abi(&plan, oversized) };
        assert!(result.is_err());

        let mut smaller = plan.desc;
        smaller.row_count = 4;
        // SAFETY: fixture is pointer-free and the smaller shape is not dispatched.
        assert!(unsafe { GroupedAggChunk::from_abi(&plan, smaller) }.is_ok());
    }

    #[test]
    fn row_chunk_advances_every_fact_row_lane_and_keeps_lookups_fixed() {
        static KEYS: [i32; 8] = [0; 8];
        static VALUES: [i64; 8] = [0; 8];
        static NULLS: [u8; 8] = [0; 8];
        static MASK: [i8; 8] = [1; 8];
        static DIM_FACT_KEYS: [i32; 8] = [0; 8];
        static KEY_LOOKUP: [i32; 4] = [0; 4];
        static DIM_MATCH: [u8; 4] = [1; 4];

        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        descriptor.keys[0].values.values = KEYS.as_ptr().cast();
        descriptor.keys[0].values.nulls = NULLS.as_ptr();
        descriptor.keys[0].lookup_by_key = KEY_LOOKUP.as_ptr();
        descriptor.measures[0].value.values = VALUES.as_ptr().cast();
        descriptor.measures[0].value.nulls = NULLS.as_ptr();
        descriptor.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        descriptor.measures[0].value.element_bytes = 8;
        descriptor.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        descriptor.where_filter.mask = MASK.as_ptr();
        descriptor.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        descriptor.measure_filters[0].mask = MASK.as_ptr();
        descriptor.dim_count = 1;
        descriptor.dims[0].fact_key.values = DIM_FACT_KEYS.as_ptr().cast();
        descriptor.dims[0].fact_key.nulls = NULLS.as_ptr();
        descriptor.dims[0].fact_key.tag = PgaccelValTag::Int32;
        descriptor.dims[0].match_by_key = DIM_MATCH.as_ptr();

        // SAFETY: all static row lanes span the declared eight rows. This test
        // only inspects the derived descriptor and performs no GPU dispatch.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("row-shaped fixture is structurally valid");
        let chunk = plan.row_chunk(3, 2).expect("bounded row chunk resolves");

        assert_eq!(chunk.desc.row_count, 2);
        // SAFETY: index three is inside every eight-element static fixture.
        unsafe {
            assert_eq!(
                chunk.desc.keys[0].values.values,
                KEYS.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.keys[0].values.nulls, NULLS.as_ptr().add(3));
            assert_eq!(
                chunk.desc.measures[0].value.values,
                VALUES.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.measures[0].value.nulls, NULLS.as_ptr().add(3));
            assert_eq!(chunk.desc.where_filter.mask, MASK.as_ptr().add(3));
            assert_eq!(chunk.desc.measure_filters[0].mask, MASK.as_ptr().add(3));
            assert_eq!(
                chunk.desc.dims[0].fact_key.values,
                DIM_FACT_KEYS.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.dims[0].fact_key.nulls, NULLS.as_ptr().add(3));
        }
        assert_eq!(chunk.desc.keys[0].lookup_by_key, KEY_LOOKUP.as_ptr());
        assert_eq!(chunk.desc.dims[0].match_by_key, DIM_MATCH.as_ptr());
    }

    #[test]
    fn row_chunk_rejects_overflow_and_out_of_bounds_ranges() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        // SAFETY: pointer-free fixture is never dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");

        assert!(plan.row_chunk(7, 2).is_err());
        assert!(plan.row_chunk(usize::MAX, 2).is_err());
        assert!(plan.row_chunk(8, 0).is_ok());
    }

    #[test]
    fn hash_chunk_cannot_introduce_unallocated_null_key_output() {
        static NULLS: [u8; 1] = [0];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        // SAFETY: fixture is not dispatched and its active pointers remain NULL.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");
        let mut nullable = plan.desc;
        nullable.keys[0].values.nulls = NULLS.as_ptr();
        // SAFETY: the static sidecar is valid for the descriptor's input lifetime.
        assert!(unsafe { GroupedAggChunk::from_abi(&plan, nullable) }.is_err());
    }

    #[test]
    fn uncertain_results_are_never_complete() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        storage.active_groups.as_mut().expect("active groups")[0] = 1;
        let mut raw = storage.raw();
        raw.emitted_group_count = 1;
        raw.uncertain_count = 2;
        assert!(matches!(
            storage.finish(&raw).expect("metadata is valid"),
            GroupedAggOutcome::NeedsRecheck(GroupedAggResult {
                uncertain_count: 2,
                ..
            })
        ));
    }

    #[test]
    fn invalid_success_metadata_poisons_reusable_workspace() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        let mut raw = storage.raw();
        raw.group_capacity = storage.capacity + 1;
        let mut workspace = GroupedAggWorkspace {
            ptr: None,
            requirement: abi::PgaccelGroupedAggWorkspaceReq {
                abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
                size_bytes: WORKSPACE_REQ_SIZE,
                bytes: 1,
                alignment: 1,
                space: PgaccelMemSpace::SharedUsm as i32,
                flags: 0,
            },
            poisoned: false,
            _not_send_sync: PhantomData,
        };
        assert!(finish_successful_call(&mut workspace, Some(&storage), Some(&raw)).is_err());
        assert!(workspace.is_poisoned());
    }
}

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
    left.physical_type == right.physical_type
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
    for index in 0..abi::PGACCEL_GROUPED_AGG_MAX_KEYS {
        let a = &left.keys[index];
        let b = &right.keys[index];
        if a.values.tag != b.values.tag
            || (left.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
                && a.values.nulls.is_null() != b.values.nulls.is_null())
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
    for index in 0..abi::PGACCEL_GROUPED_AGG_MAX_MEASURES {
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
    for index in 0..abi::PGACCEL_GROUPED_AGG_MAX_DIMS {
        let a = &left.dims[index];
        let b = &right.dims[index];
        if a.fact_key.tag != b.fact_key.tag
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

fn workspace_query_descriptor(plan: &ResolvedGroupedAggPlan<'_>) -> abi::PgaccelGroupedAggDesc {
    let mut desc = plan.desc;
    desc.execution_flags = abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    desc
}

impl GroupedAggWorkspace {
    pub fn allocate(plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<Self> {
        let query_desc = workspace_query_descriptor(plan);
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
        if mask
            & (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                | abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ)
            != 0
        {
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
}

fn key_width(tag: PgaccelValTag) -> Option<usize> {
    match tag {
        PgaccelValTag::Null => None,
        PgaccelValTag::Bool => Some(1),
        PgaccelValTag::Int32 | PgaccelValTag::Float32 | PgaccelValTag::Date => Some(4),
        PgaccelValTag::Int64 | PgaccelValTag::Float64 | PgaccelValTag::Timestamp => Some(8),
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
                let tag = if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX {
                    PgaccelValTag::Int32
                } else {
                    key.values.tag
                };
                let width = key_width(tag)
                    .ok_or_else(|| descriptor_error("compact output key has invalid type"))?;
                key_values[index] = Some(RawHostBuffer::zeroed(capacity, width)?);
                key_types[index] = tag as i32;
                if key.null_code != abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE
                    || (desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
                        && !key.values.nulls.is_null())
                {
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

fn execute_call(
    mut desc: abi::PgaccelGroupedAggDesc,
    flags: u32,
    workspace: &mut GroupedAggWorkspace,
    mut output: Option<&mut GroupedAggOutputStorage>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    desc.execution_flags = flags;
    if flags & abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE == 0 {
        desc.row_count = 0;
    }
    workspace.apply_to(&mut desc)?;
    let mut raw_output = output.as_deref_mut().map(GroupedAggOutputStorage::raw);
    let output_ptr = raw_output
        .as_mut()
        .map_or(std::ptr::null_mut(), std::ptr::from_mut);
    // SAFETY: descriptor, optional output, and workspace remain live through
    // the synchronous FFI call and were built from pinned ABI types.
    let status =
        unsafe { bridge::pgaccel_grouped_agg_execute(std::ptr::from_ref(&desc), output_ptr) };
    if !status.is_ok() {
        if status_poisons_workspace(status) {
            workspace.poisoned = true;
        }
        return Err(GpuError::from_status(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            status,
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
        plan.desc,
        abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN,
        &mut workspace,
        Some(output),
    )?
    .ok_or_else(|| descriptor_error("finalized grouped call returned no outcome"))
}

/// Stateful multi-call facade reserved by the frozen chunk lifecycle.
pub struct GroupedAggSession<'plan, 'input> {
    plan: &'plan ResolvedGroupedAggPlan<'input>,
    workspace: GroupedAggWorkspace,
    state: LifecycleState,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl<'plan, 'input> GroupedAggSession<'plan, 'input> {
    pub fn start(plan: &'plan ResolvedGroupedAggPlan<'input>) -> GpuResult<Self> {
        Ok(Self {
            plan,
            workspace: GroupedAggWorkspace::allocate(plan)?,
            state: LifecycleState::Ready,
            _not_send_sync: PhantomData,
        })
    }

    pub fn accumulate(&mut self, chunk: &GroupedAggChunk<'_, 'input>) -> GpuResult<()> {
        if !stable_shape_matches(&self.plan.desc, &chunk.desc) {
            return Err(descriptor_error("chunk does not match session plan"));
        }
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Accumulate)?;
        match execute_call(chunk.desc, flags, &mut self.workspace, None) {
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
        output: &mut GroupedAggOutputStorage,
    ) -> GpuResult<GroupedAggOutcome> {
        output.validate_for_plan(self.plan)?;
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Finalize)?;
        match execute_call(self.plan.desc, flags, &mut self.workspace, Some(output)) {
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

    pub fn reset(&mut self) -> GpuResult<()> {
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Reset)?;
        match execute_call(self.plan.desc, flags, &mut self.workspace, None) {
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
    fn workspace_query_uses_execute_flags_without_mutating_plan() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        assert_eq!(plan.desc.execution_flags, 0);
        let query = workspace_query_descriptor(&plan);
        assert_eq!(
            query.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN
        );
        assert_eq!(plan.desc.execution_flags, 0);
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
    fn only_real_execution_failures_poison_workspace() {
        assert!(!status_poisons_workspace(PgaccelStatus::Ok));
        assert!(!status_poisons_workspace(PgaccelStatus::ErrorUnsupported));
        for status in [
            PgaccelStatus::Error,
            PgaccelStatus::ErrorOom,
            PgaccelStatus::ErrorTimeout,
            PgaccelStatus::ErrorNoDevice,
        ] {
            assert!(status_poisons_workspace(status), "{status:?}");
        }
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

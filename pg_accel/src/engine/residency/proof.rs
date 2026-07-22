//! Shared GPU-resident dataflow proof model.
//!
//! This module is intentionally independent from the legacy `PgaccelBatch`
//! host-pointer ABI. Planner admission, executor handoff, EXPLAIN, and report
//! gates should converge on these proof terms before any SQL CustomScan is
//! selectable again.

use std::ffi::{c_int, c_void};
use std::marker::PhantomData;
use std::rc::Rc;

use crate::gpu::{PgaccelMemSpace, PgaccelValTag};

/// Where a batch's value buffers currently live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchResidency {
    /// Typed host memory owned by Rust; current expression kernels read it
    /// through USM-accessible host pointers.
    HostColumnar,
    /// Host-writable shared USM. Device kernels can read it, but host staging
    /// still prevents selected SQL admission unless an upstream producer owns
    /// it as a resident device pipeline.
    SharedUsmHostWritable,
    /// Device-owned buffers. This is the target state for operator handoff.
    DeviceResident,
}

impl BatchResidency {
    #[must_use]
    pub const fn mem_space(self) -> PgaccelMemSpace {
        match self {
            Self::HostColumnar => PgaccelMemSpace::Host,
            Self::SharedUsmHostWritable => PgaccelMemSpace::SharedUsm,
            Self::DeviceResident => PgaccelMemSpace::Device,
        }
    }
}

/// Broad operator classes that can participate in a resident GPU dataflow.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentOperatorStage {
    Scan = 0,
    Expression = 1,
    Aggregate = 2,
    GroupedAggregate = 3,
    Join = 4,
    PreAgg = 5,
    Sort = 6,
    Window = 7,
    FunctionScan = 8,
    SrfTargetList = 9,
    H3 = 10,
    Postgis = 11,
    Raster = 12,
    VariableOutput = 13,
    FinalMaterialization = 14,
}

impl ResidentOperatorStage {
    pub const ALL: [Self; 15] = [
        Self::Scan,
        Self::Expression,
        Self::Aggregate,
        Self::GroupedAggregate,
        Self::Join,
        Self::PreAgg,
        Self::Sort,
        Self::Window,
        Self::FunctionScan,
        Self::SrfTargetList,
        Self::H3,
        Self::Postgis,
        Self::Raster,
        Self::VariableOutput,
        Self::FinalMaterialization,
    ];

    #[must_use]
    pub const fn from_i32(value: c_int) -> Option<Self> {
        match value {
            0 => Some(Self::Scan),
            1 => Some(Self::Expression),
            2 => Some(Self::Aggregate),
            3 => Some(Self::GroupedAggregate),
            4 => Some(Self::Join),
            5 => Some(Self::PreAgg),
            6 => Some(Self::Sort),
            7 => Some(Self::Window),
            8 => Some(Self::FunctionScan),
            9 => Some(Self::SrfTargetList),
            10 => Some(Self::H3),
            11 => Some(Self::Postgis),
            12 => Some(Self::Raster),
            13 => Some(Self::VariableOutput),
            14 => Some(Self::FinalMaterialization),
            _ => None,
        }
    }

    #[must_use]
    pub const fn to_i32(self) -> c_int {
        self as c_int
    }

    #[must_use]
    pub const fn bit(self) -> u32 {
        1u32 << (self as u32)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Expression => "expression",
            Self::Aggregate => "aggregate",
            Self::GroupedAggregate => "grouped_aggregate",
            Self::Join => "join",
            Self::PreAgg => "preagg",
            Self::Sort => "sort",
            Self::Window => "window",
            Self::FunctionScan => "function_scan",
            Self::SrfTargetList => "srf_target_list",
            Self::H3 => "h3",
            Self::Postgis => "postgis",
            Self::Raster => "raster",
            Self::VariableOutput => "variable_output",
            Self::FinalMaterialization => "final_materialization",
        }
    }
}

/// Reusable resident GPU operator classes.
///
/// This is intentionally broader than individual kernel ABIs. Focused kernels
/// should appear as measured specializations under one of these classes, not as
/// standalone planner/reporting concepts.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentOperatorClass {
    Unspecified = 0,
    ResidentSource = 1,
    ResidentExpression = 2,
    ResidentGroupAgg = 3,
    ResidentJoin = 4,
    ResidentPreAgg = 5,
    ResidentSortTopK = 6,
    ResidentWindow = 7,
    ResidentVariableOutput = 8,
    ResidentFinalMaterialization = 9,
}

impl ResidentOperatorClass {
    #[must_use]
    pub const fn from_i32(value: c_int) -> Option<Self> {
        match value {
            0 => Some(Self::Unspecified),
            1 => Some(Self::ResidentSource),
            2 => Some(Self::ResidentExpression),
            3 => Some(Self::ResidentGroupAgg),
            4 => Some(Self::ResidentJoin),
            5 => Some(Self::ResidentPreAgg),
            6 => Some(Self::ResidentSortTopK),
            7 => Some(Self::ResidentWindow),
            8 => Some(Self::ResidentVariableOutput),
            9 => Some(Self::ResidentFinalMaterialization),
            _ => None,
        }
    }

    #[must_use]
    pub const fn to_i32(self) -> c_int {
        self as c_int
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::ResidentSource => "resident_source",
            Self::ResidentExpression => "resident_expression",
            Self::ResidentGroupAgg => "resident_groupagg",
            Self::ResidentJoin => "resident_join",
            Self::ResidentPreAgg => "resident_preagg",
            Self::ResidentSortTopK => "resident_sort_topk",
            Self::ResidentWindow => "resident_window",
            Self::ResidentVariableOutput => "resident_variable_output",
            Self::ResidentFinalMaterialization => "resident_final_materialization",
        }
    }

    #[must_use]
    pub const fn is_specified(self) -> bool {
        !matches!(self, Self::Unspecified)
    }
}

/// Why this batch still crosses a CPU boundary.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuBoundaryReason {
    /// No CPU boundary is known for this batch.
    None = 0,
    /// Rows were read from PostgreSQL heap/slots before building columns.
    HostInputStaging = 1,
    /// A child PostgreSQL executor node produced rows via `ExecProcNode`.
    ExecProcNodeInput = 2,
    /// Intermediate state lives in a host hash table or tuple map.
    HostHashState = 3,
    /// Variable-cardinality output is buffered in host memory.
    HostVariableOutput = 4,
    /// PostgreSQL tuples are reconstructed before a downstream GPU consumer.
    HostTupleReconstruction = 5,
    /// Rows are only being materialized for PostgreSQL's final output.
    FinalOutputMaterialization = 6,
}

impl CpuBoundaryReason {
    #[must_use]
    pub const fn from_i32(value: c_int) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::HostInputStaging),
            2 => Some(Self::ExecProcNodeInput),
            3 => Some(Self::HostHashState),
            4 => Some(Self::HostVariableOutput),
            5 => Some(Self::HostTupleReconstruction),
            6 => Some(Self::FinalOutputMaterialization),
            _ => None,
        }
    }

    #[must_use]
    pub const fn to_i32(self) -> c_int {
        self as c_int
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "none: no CPU boundary",
            Self::HostInputStaging => {
                "host_input_staging: PostgreSQL heap/slot rows were read before GPU handoff"
            }
            Self::ExecProcNodeInput => {
                "execprocnode_input: a PostgreSQL child executor produced intermediate rows"
            }
            Self::HostHashState => {
                "host_hash_state: intermediate grouping/join state lives in host memory"
            }
            Self::HostVariableOutput => {
                "host_variable_output: variable-cardinality output is buffered on the host"
            }
            Self::HostTupleReconstruction => {
                "host_tuple_reconstruction: PostgreSQL tuples are reconstructed before a downstream GPU consumer"
            }
            Self::FinalOutputMaterialization => {
                "final_output_materialization: only bounded final output is materialized for PostgreSQL"
            }
        }
    }

    #[must_use]
    pub const fn blocks_resident_pipeline(self) -> bool {
        !matches!(self, Self::None | Self::FinalOutputMaterialization)
    }
}

/// Where a resident pipeline is allowed to materialize back to PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationBoundary {
    /// No PostgreSQL materialization occurs at this pipeline boundary.
    None,
    /// Only the bounded final result is materialized for PostgreSQL.
    FinalOutput,
    /// An intermediate host boundary exists and must prevent plan admission.
    HostIntermediate(CpuBoundaryReason),
}

impl MaterializationBoundary {
    #[must_use]
    pub const fn cpu_boundary(self) -> CpuBoundaryReason {
        match self {
            Self::None | Self::FinalOutput => CpuBoundaryReason::None,
            Self::HostIntermediate(reason) => reason,
        }
    }

    #[must_use]
    pub const fn allows_resident_pipeline(self) -> bool {
        !self.cpu_boundary().blocks_resident_pipeline()
    }

    #[must_use]
    pub const fn kind(self) -> ResidentMaterializationKind {
        match self {
            Self::None => ResidentMaterializationKind::None,
            Self::FinalOutput => ResidentMaterializationKind::FinalOutput,
            Self::HostIntermediate(_) => ResidentMaterializationKind::HostIntermediate,
        }
    }
}

/// Stable wire kind for materialization boundaries.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentMaterializationKind {
    None = 0,
    FinalOutput = 1,
    HostIntermediate = 2,
}

impl ResidentMaterializationKind {
    #[must_use]
    pub const fn from_i32(value: c_int) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::FinalOutput),
            2 => Some(Self::HostIntermediate),
            _ => None,
        }
    }

    #[must_use]
    pub const fn to_i32(self) -> c_int {
        self as c_int
    }

    #[must_use]
    pub const fn to_materialization(self, reason: CpuBoundaryReason) -> MaterializationBoundary {
        match self {
            Self::None => MaterializationBoundary::None,
            Self::FinalOutput => MaterializationBoundary::FinalOutput,
            Self::HostIntermediate => MaterializationBoundary::HostIntermediate(reason),
        }
    }
}

/// Device-side buffer descriptor used by the resident batch fabric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceBufferRef {
    pub ptr: *const c_void,
    pub bytes: usize,
    pub value_tag: Option<PgaccelValTag>,
    pub residency: BatchResidency,
    pub producer: ResidentOperatorStage,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl DeviceBufferRef {
    #[must_use]
    pub const fn new(
        ptr: *const c_void,
        bytes: usize,
        value_tag: Option<PgaccelValTag>,
        residency: BatchResidency,
        producer: ResidentOperatorStage,
    ) -> Self {
        Self {
            ptr,
            bytes,
            value_tag,
            residency,
            producer,
            _not_send_sync: PhantomData,
        }
    }

    #[must_use]
    pub fn is_valid(self) -> bool {
        self.bytes == 0 || !self.ptr.is_null()
    }

    #[must_use]
    pub const fn is_device_resident(self) -> bool {
        matches!(self.residency, BatchResidency::DeviceResident)
    }

    #[must_use]
    pub const fn mem_space(self) -> PgaccelMemSpace {
        self.residency.mem_space()
    }
}

/// Proof carried by batch-producing and batch-consuming code.
///
/// This is deliberately broader than the current expression ABI. It is the
/// shared checklist future planner admission should consume before exposing a
/// selected pg_accel CustomScan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentPipelineProof {
    operator_class: ResidentOperatorClass,
    stages: Vec<ResidentOperatorStage>,
    materialization: MaterializationBoundary,
    device_columns: usize,
    has_device_selection: bool,
    has_device_projection: bool,
    cpu_boundary: CpuBoundaryReason,
}

impl ResidentPipelineProof {
    #[must_use]
    pub fn host_staged(reason: CpuBoundaryReason) -> Self {
        assert!(
            reason.blocks_resident_pipeline(),
            "host-staged proof must name a blocking CPU boundary"
        );
        Self {
            operator_class: ResidentOperatorClass::Unspecified,
            stages: Vec::new(),
            materialization: MaterializationBoundary::HostIntermediate(reason),
            device_columns: 0,
            has_device_selection: false,
            has_device_projection: false,
            cpu_boundary: reason,
        }
    }

    #[must_use]
    pub fn device_resident(
        operator_class: ResidentOperatorClass,
        stages: Vec<ResidentOperatorStage>,
        materialization: MaterializationBoundary,
        device_columns: usize,
        has_device_selection: bool,
        has_device_projection: bool,
    ) -> Self {
        assert!(
            !stages.is_empty(),
            "resident pipeline proof must include at least one GPU operator stage"
        );
        assert!(
            materialization.allows_resident_pipeline(),
            "resident pipeline proof cannot include an intermediate host boundary"
        );
        assert!(
            operator_class.is_specified(),
            "resident pipeline proof must name a reusable operator class"
        );
        assert!(
            device_columns > 0,
            "resident pipeline proof must own at least one device column/buffer"
        );
        Self {
            operator_class,
            stages,
            materialization,
            device_columns,
            has_device_selection,
            has_device_projection,
            cpu_boundary: CpuBoundaryReason::None,
        }
    }

    #[must_use]
    pub const fn gpu_resident_pipeline(&self) -> bool {
        self.operator_class.is_specified()
            && !self.cpu_boundary.blocks_resident_pipeline()
            && self.device_columns > 0
            && self.materialization.allows_resident_pipeline()
    }

    #[must_use]
    pub const fn operator_class(&self) -> ResidentOperatorClass {
        self.operator_class
    }

    #[must_use]
    pub fn stages(&self) -> &[ResidentOperatorStage] {
        &self.stages
    }

    #[must_use]
    pub const fn materialization(&self) -> MaterializationBoundary {
        self.materialization
    }

    #[must_use]
    pub const fn device_columns(&self) -> usize {
        self.device_columns
    }

    #[must_use]
    pub const fn has_device_selection(&self) -> bool {
        self.has_device_selection
    }

    #[must_use]
    pub const fn has_device_projection(&self) -> bool {
        self.has_device_projection
    }

    #[must_use]
    pub const fn cpu_boundary(&self) -> CpuBoundaryReason {
        self.cpu_boundary
    }

    #[must_use]
    pub const fn boundary_label(&self) -> &'static str {
        self.cpu_boundary.label()
    }

    #[must_use]
    pub fn stage_mask(&self) -> u32 {
        stage_mask(&self.stages)
    }

    #[must_use]
    pub fn snapshot(&self) -> ResidentProofSnapshot {
        ResidentProofSnapshot {
            operator_class: self.operator_class,
            stage_mask: self.stage_mask(),
            materialization_kind: self.materialization.kind(),
            device_columns: self.device_columns as u32,
            has_device_selection: self.has_device_selection,
            has_device_projection: self.has_device_projection,
            cpu_boundary: self.cpu_boundary,
        }
    }
}

#[must_use]
pub fn stage_mask(stages: &[ResidentOperatorStage]) -> u32 {
    stages.iter().fold(0u32, |mask, stage| mask | stage.bit())
}

#[must_use]
pub fn stages_from_mask(mask: u32) -> Vec<ResidentOperatorStage> {
    ResidentOperatorStage::ALL
        .iter()
        .copied()
        .filter(|stage| mask & stage.bit() != 0)
        .collect()
}

/// Copyable proof representation used in CustomScan private data and executor state.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentProofSnapshot {
    pub operator_class: ResidentOperatorClass,
    pub stage_mask: u32,
    pub materialization_kind: ResidentMaterializationKind,
    pub device_columns: u32,
    pub has_device_selection: bool,
    pub has_device_projection: bool,
    pub cpu_boundary: CpuBoundaryReason,
}

impl ResidentProofSnapshot {
    #[must_use]
    pub const fn not_proven() -> Self {
        Self {
            operator_class: ResidentOperatorClass::Unspecified,
            stage_mask: 0,
            materialization_kind: ResidentMaterializationKind::None,
            device_columns: 0,
            has_device_selection: false,
            has_device_projection: false,
            cpu_boundary: CpuBoundaryReason::None,
        }
    }

    #[must_use]
    pub const fn host_staged(reason: CpuBoundaryReason) -> Self {
        assert!(
            reason.blocks_resident_pipeline(),
            "host-staged proof must name a blocking CPU boundary"
        );
        Self {
            operator_class: ResidentOperatorClass::Unspecified,
            stage_mask: 0,
            materialization_kind: ResidentMaterializationKind::HostIntermediate,
            device_columns: 0,
            has_device_selection: false,
            has_device_projection: false,
            cpu_boundary: reason,
        }
    }

    #[must_use]
    pub const fn gpu_resident_pipeline(self) -> bool {
        self.operator_class.is_specified()
            && self.stage_mask != 0
            && !self.cpu_boundary.blocks_resident_pipeline()
            && self.device_columns > 0
            && !matches!(
                self.materialization_kind,
                ResidentMaterializationKind::HostIntermediate
            )
    }

    #[must_use]
    pub const fn boundary_label(self) -> &'static str {
        self.cpu_boundary.label()
    }

    #[must_use]
    pub const fn operator_class_label(self) -> &'static str {
        self.operator_class.label()
    }

    pub fn to_proof(self) -> Result<ResidentPipelineProof, ResidentProofDecodeError> {
        if self.cpu_boundary.blocks_resident_pipeline() {
            return Ok(ResidentPipelineProof::host_staged(self.cpu_boundary));
        }
        if !self.operator_class.is_specified() {
            return Err(ResidentProofDecodeError::MissingOperatorClass);
        }
        let stages = stages_from_mask(self.stage_mask);
        if stages.is_empty() {
            return Err(ResidentProofDecodeError::MissingResidentStage);
        }
        if self.device_columns == 0 {
            return Err(ResidentProofDecodeError::MissingDeviceColumns);
        }
        Ok(ResidentPipelineProof::device_resident(
            self.operator_class,
            stages,
            self.materialization_kind
                .to_materialization(self.cpu_boundary),
            self.device_columns as usize,
            self.has_device_selection,
            self.has_device_projection,
        ))
    }
}

impl Default for ResidentProofSnapshot {
    fn default() -> Self {
        Self::not_proven()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentProofDecodeError {
    InvalidCpuBoundary(c_int),
    InvalidOperatorClass(c_int),
    InvalidMaterialization(c_int),
    MissingOperatorClass,
    MissingResidentStage,
    MissingDeviceColumns,
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(DeviceBufferRef: Send, Sync);

    #[test]
    fn batch_residency_maps_to_ffi_memory_space() {
        assert_eq!(
            BatchResidency::HostColumnar.mem_space(),
            PgaccelMemSpace::Host
        );
        assert_eq!(
            BatchResidency::SharedUsmHostWritable.mem_space(),
            PgaccelMemSpace::SharedUsm
        );
        assert_eq!(
            BatchResidency::DeviceResident.mem_space(),
            PgaccelMemSpace::Device
        );
    }

    #[test]
    fn resident_stage_wire_values_bits_labels_and_masks_are_exhaustive() {
        let expected_labels = [
            "scan",
            "expression",
            "aggregate",
            "grouped_aggregate",
            "join",
            "preagg",
            "sort",
            "window",
            "function_scan",
            "srf_target_list",
            "h3",
            "postgis",
            "raster",
            "variable_output",
            "final_materialization",
        ];

        let mut expected_mask = 0u32;
        for (index, stage) in ResidentOperatorStage::ALL.iter().copied().enumerate() {
            assert_eq!(stage.to_i32(), index as c_int);
            assert_eq!(ResidentOperatorStage::from_i32(index as c_int), Some(stage));
            assert_eq!(stage.bit(), 1u32 << index);
            assert_eq!(stage.label(), expected_labels[index]);
            expected_mask |= stage.bit();
        }
        assert_eq!(ResidentOperatorStage::from_i32(-1), None);
        assert_eq!(ResidentOperatorStage::from_i32(15), None);
        assert_eq!(stage_mask(&ResidentOperatorStage::ALL), expected_mask);
        assert_eq!(stages_from_mask(expected_mask), ResidentOperatorStage::ALL);
        assert_eq!(stages_from_mask(1u32 << 31), Vec::new());
        assert_eq!(
            stages_from_mask(
                ResidentOperatorStage::Scan.bit() | ResidentOperatorStage::Raster.bit()
            ),
            vec![ResidentOperatorStage::Scan, ResidentOperatorStage::Raster]
        );
    }

    #[test]
    fn operator_class_wire_values_labels_and_specification_are_exhaustive() {
        let cases = [
            (ResidentOperatorClass::Unspecified, "unspecified", false),
            (
                ResidentOperatorClass::ResidentSource,
                "resident_source",
                true,
            ),
            (
                ResidentOperatorClass::ResidentExpression,
                "resident_expression",
                true,
            ),
            (
                ResidentOperatorClass::ResidentGroupAgg,
                "resident_groupagg",
                true,
            ),
            (ResidentOperatorClass::ResidentJoin, "resident_join", true),
            (
                ResidentOperatorClass::ResidentPreAgg,
                "resident_preagg",
                true,
            ),
            (
                ResidentOperatorClass::ResidentSortTopK,
                "resident_sort_topk",
                true,
            ),
            (
                ResidentOperatorClass::ResidentWindow,
                "resident_window",
                true,
            ),
            (
                ResidentOperatorClass::ResidentVariableOutput,
                "resident_variable_output",
                true,
            ),
            (
                ResidentOperatorClass::ResidentFinalMaterialization,
                "resident_final_materialization",
                true,
            ),
        ];

        for (index, (class, label, specified)) in cases.into_iter().enumerate() {
            assert_eq!(class.to_i32(), index as c_int);
            assert_eq!(ResidentOperatorClass::from_i32(index as c_int), Some(class));
            assert_eq!(class.label(), label);
            assert_eq!(class.is_specified(), specified);
        }
        assert_eq!(ResidentOperatorClass::from_i32(-1), None);
        assert_eq!(ResidentOperatorClass::from_i32(10), None);
    }

    #[test]
    fn cpu_boundaries_and_materialization_kinds_roundtrip_exhaustively() {
        let boundaries = [
            (CpuBoundaryReason::None, "none: no CPU boundary", false),
            (
                CpuBoundaryReason::HostInputStaging,
                "host_input_staging: PostgreSQL heap/slot rows were read before GPU handoff",
                true,
            ),
            (
                CpuBoundaryReason::ExecProcNodeInput,
                "execprocnode_input: a PostgreSQL child executor produced intermediate rows",
                true,
            ),
            (
                CpuBoundaryReason::HostHashState,
                "host_hash_state: intermediate grouping/join state lives in host memory",
                true,
            ),
            (
                CpuBoundaryReason::HostVariableOutput,
                "host_variable_output: variable-cardinality output is buffered on the host",
                true,
            ),
            (
                CpuBoundaryReason::HostTupleReconstruction,
                "host_tuple_reconstruction: PostgreSQL tuples are reconstructed before a downstream GPU consumer",
                true,
            ),
            (
                CpuBoundaryReason::FinalOutputMaterialization,
                "final_output_materialization: only bounded final output is materialized for PostgreSQL",
                false,
            ),
        ];

        for (index, (boundary, label, blocks)) in boundaries.into_iter().enumerate() {
            assert_eq!(boundary.to_i32(), index as c_int);
            assert_eq!(CpuBoundaryReason::from_i32(index as c_int), Some(boundary));
            assert_eq!(boundary.label(), label);
            assert_eq!(boundary.blocks_resident_pipeline(), blocks);
        }
        assert_eq!(CpuBoundaryReason::from_i32(-1), None);
        assert_eq!(CpuBoundaryReason::from_i32(7), None);

        let materializations = [
            (
                ResidentMaterializationKind::None,
                MaterializationBoundary::None,
            ),
            (
                ResidentMaterializationKind::FinalOutput,
                MaterializationBoundary::FinalOutput,
            ),
            (
                ResidentMaterializationKind::HostIntermediate,
                MaterializationBoundary::HostIntermediate(CpuBoundaryReason::HostHashState),
            ),
        ];
        for (index, (kind, boundary)) in materializations.into_iter().enumerate() {
            assert_eq!(kind.to_i32(), index as c_int);
            assert_eq!(
                ResidentMaterializationKind::from_i32(index as c_int),
                Some(kind)
            );
            assert_eq!(
                kind.to_materialization(CpuBoundaryReason::HostHashState),
                boundary
            );
            assert_eq!(boundary.kind(), kind);
        }
        assert_eq!(ResidentMaterializationKind::from_i32(-1), None);
        assert_eq!(ResidentMaterializationKind::from_i32(3), None);

        assert_eq!(
            MaterializationBoundary::None.cpu_boundary(),
            CpuBoundaryReason::None
        );
        assert!(MaterializationBoundary::None.allows_resident_pipeline());
        assert_eq!(
            MaterializationBoundary::FinalOutput.cpu_boundary(),
            CpuBoundaryReason::None
        );
        assert!(MaterializationBoundary::FinalOutput.allows_resident_pipeline());
        let host = MaterializationBoundary::HostIntermediate(CpuBoundaryReason::ExecProcNodeInput);
        assert_eq!(host.cpu_boundary(), CpuBoundaryReason::ExecProcNodeInput);
        assert!(!host.allows_resident_pipeline());
    }

    #[test]
    fn device_buffer_descriptor_validates_pointer_and_residency_without_dereference() {
        let empty = DeviceBufferRef::new(
            std::ptr::null(),
            0,
            None,
            BatchResidency::HostColumnar,
            ResidentOperatorStage::Scan,
        );
        assert!(empty.is_valid());
        assert!(!empty.is_device_resident());
        assert_eq!(empty.mem_space(), PgaccelMemSpace::Host);

        let invalid = DeviceBufferRef::new(
            std::ptr::null(),
            8,
            Some(PgaccelValTag::Int64),
            BatchResidency::SharedUsmHostWritable,
            ResidentOperatorStage::Expression,
        );
        assert!(!invalid.is_valid());
        assert!(!invalid.is_device_resident());
        assert_eq!(invalid.mem_space(), PgaccelMemSpace::SharedUsm);

        let device = DeviceBufferRef::new(
            std::ptr::dangling(),
            8,
            Some(PgaccelValTag::Int64),
            BatchResidency::DeviceResident,
            ResidentOperatorStage::Aggregate,
        );
        assert!(device.is_valid());
        assert!(device.is_device_resident());
        assert_eq!(device.mem_space(), PgaccelMemSpace::Device);
    }

    #[test]
    fn pipeline_proofs_expose_canonical_host_and_device_evidence() {
        let host = ResidentPipelineProof::host_staged(CpuBoundaryReason::HostInputStaging);
        assert!(!host.gpu_resident_pipeline());
        assert_eq!(host.operator_class(), ResidentOperatorClass::Unspecified);
        assert!(host.stages().is_empty());
        assert_eq!(
            host.materialization(),
            MaterializationBoundary::HostIntermediate(CpuBoundaryReason::HostInputStaging)
        );
        assert_eq!(host.device_columns(), 0);
        assert!(!host.has_device_selection());
        assert!(!host.has_device_projection());
        assert_eq!(host.cpu_boundary(), CpuBoundaryReason::HostInputStaging);
        assert_eq!(
            host.boundary_label(),
            CpuBoundaryReason::HostInputStaging.label()
        );
        assert_eq!(host.stage_mask(), 0);
        assert_eq!(
            host.snapshot(),
            ResidentProofSnapshot::host_staged(CpuBoundaryReason::HostInputStaging)
        );

        let stages = vec![
            ResidentOperatorStage::Scan,
            ResidentOperatorStage::Expression,
            ResidentOperatorStage::FinalMaterialization,
        ];
        let device = ResidentPipelineProof::device_resident(
            ResidentOperatorClass::ResidentExpression,
            stages.clone(),
            MaterializationBoundary::FinalOutput,
            3,
            true,
            true,
        );
        assert!(device.gpu_resident_pipeline());
        assert_eq!(
            device.operator_class(),
            ResidentOperatorClass::ResidentExpression
        );
        assert_eq!(device.stages(), stages);
        assert_eq!(
            device.materialization(),
            MaterializationBoundary::FinalOutput
        );
        assert_eq!(device.device_columns(), 3);
        assert!(device.has_device_selection());
        assert!(device.has_device_projection());
        assert_eq!(device.cpu_boundary(), CpuBoundaryReason::None);
        assert_eq!(device.boundary_label(), CpuBoundaryReason::None.label());

        let snapshot = device.snapshot();
        assert!(snapshot.gpu_resident_pipeline());
        assert_eq!(snapshot.operator_class_label(), "resident_expression");
        assert_eq!(snapshot.boundary_label(), "none: no CPU boundary");
        assert_eq!(snapshot.to_proof().expect("snapshot proof"), device);
    }

    #[test]
    fn snapshot_decode_distinguishes_every_missing_resident_requirement() {
        let default = ResidentProofSnapshot::default();
        assert_eq!(default, ResidentProofSnapshot::not_proven());
        assert!(!default.gpu_resident_pipeline());
        assert_eq!(
            default.to_proof(),
            Err(ResidentProofDecodeError::MissingOperatorClass)
        );

        let missing_stage = ResidentProofSnapshot {
            operator_class: ResidentOperatorClass::ResidentSource,
            ..default
        };
        assert_eq!(
            missing_stage.to_proof(),
            Err(ResidentProofDecodeError::MissingResidentStage)
        );

        let missing_columns = ResidentProofSnapshot {
            stage_mask: ResidentOperatorStage::Scan.bit(),
            ..missing_stage
        };
        assert_eq!(
            missing_columns.to_proof(),
            Err(ResidentProofDecodeError::MissingDeviceColumns)
        );

        let host = ResidentProofSnapshot::host_staged(CpuBoundaryReason::HostHashState);
        assert!(!host.gpu_resident_pipeline());
        assert_eq!(host.operator_class_label(), "unspecified");
        assert_eq!(
            host.boundary_label(),
            CpuBoundaryReason::HostHashState.label()
        );
        assert_eq!(
            host.to_proof().expect("host proof"),
            ResidentPipelineProof::host_staged(CpuBoundaryReason::HostHashState)
        );
    }

    #[test]
    fn proof_constructors_reject_noncanonical_claims() {
        assert!(
            std::panic::catch_unwind(|| {
                ResidentPipelineProof::host_staged(CpuBoundaryReason::None)
            })
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| {
                ResidentProofSnapshot::host_staged(CpuBoundaryReason::FinalOutputMaterialization)
            })
            .is_err()
        );

        let valid_stage = vec![ResidentOperatorStage::Scan];
        for invalid in [
            std::panic::catch_unwind(|| {
                ResidentPipelineProof::device_resident(
                    ResidentOperatorClass::ResidentSource,
                    Vec::new(),
                    MaterializationBoundary::None,
                    1,
                    false,
                    false,
                )
            }),
            std::panic::catch_unwind(|| {
                ResidentPipelineProof::device_resident(
                    ResidentOperatorClass::ResidentSource,
                    valid_stage.clone(),
                    MaterializationBoundary::HostIntermediate(CpuBoundaryReason::HostInputStaging),
                    1,
                    false,
                    false,
                )
            }),
            std::panic::catch_unwind(|| {
                ResidentPipelineProof::device_resident(
                    ResidentOperatorClass::Unspecified,
                    valid_stage.clone(),
                    MaterializationBoundary::None,
                    1,
                    false,
                    false,
                )
            }),
            std::panic::catch_unwind(|| {
                ResidentPipelineProof::device_resident(
                    ResidentOperatorClass::ResidentSource,
                    valid_stage.clone(),
                    MaterializationBoundary::None,
                    0,
                    false,
                    false,
                )
            }),
        ] {
            assert!(invalid.is_err());
        }
    }
}

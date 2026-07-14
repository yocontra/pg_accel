//! Backend-local two-tier resident relation store.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use pgrx::{default, name, pg_sys, prelude::*};

use crate::engine::gucs;
use crate::gpu::{ExprDeviceBuffer, GpuError};

use super::domain::{
    ResidentByteAccounting, ResidentRasterBand, ResidentRasterRow, ResidentRasterStats,
    RetainedExactValues,
};
use super::geometry::{ResidentGeometryColumn, ResidentGeometryColumnView};
use super::ledger::{self, GenerationStamp, LedgerCharge};
use super::loader::{self, ColumnRequest, StagedRelation};

const PENDING_RELCACHE_CAPACITY: usize = 128;

#[derive(Debug, Clone, Copy)]
struct PendingRelcacheInvalidations {
    relids: [u32; PENDING_RELCACHE_CAPACITY],
    len: usize,
}

impl PendingRelcacheInvalidations {
    const fn empty() -> Self {
        Self {
            relids: [0; PENDING_RELCACHE_CAPACITY],
            len: 0,
        }
    }

    fn contains(self, relid: u32) -> bool {
        self.relids[..self.len].contains(&relid)
    }
}

thread_local! {
    static STORE: RefCell<RelationStore> = RefCell::new(RelationStore::default());
    static RELCACHE_CALLBACK_REGISTERED: Cell<bool> = const { Cell::new(false) };
    static PENDING_RELCACHE_CLEAR_ALL: Cell<bool> = const { Cell::new(false) };
    static PENDING_RELCACHE: Cell<PendingRelcacheInvalidations> =
        const { Cell::new(PendingRelcacheInvalidations::empty()) };
}

/// One raw, lossless relation column resident in GPU-readable memory.
pub enum ResidentColumn {
    Empty {
        type_oid: pg_sys::Oid,
    },
    Bool {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<u8>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    I32 {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<i32>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    I64 {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<i64>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    H3 {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<u64>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    Geometry {
        type_oid: pg_sys::Oid,
        data: ResidentGeometryColumn,
    },
    Raster {
        type_oid: pg_sys::Oid,
        pixels: Option<ExprDeviceBuffer<u8>>,
        band_offsets: ExprDeviceBuffer<u64>,
        rows: ExprDeviceBuffer<ResidentRasterRow>,
        bands: Option<ExprDeviceBuffer<ResidentRasterBand>>,
        nulls: Option<ExprDeviceBuffer<u8>>,
        exact: RetainedExactValues,
        stats: ResidentRasterStats,
    },
    F32 {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<f32>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    F64 {
        type_oid: pg_sys::Oid,
        values: ExprDeviceBuffer<f64>,
        nulls: Option<ExprDeviceBuffer<u8>>,
    },
    TextDictionary {
        type_oid: pg_sys::Oid,
        codes: ExprDeviceBuffer<i32>,
        nulls: Option<ExprDeviceBuffer<u8>>,
        labels: Vec<String>,
    },
}

impl ResidentColumn {
    #[must_use]
    pub const fn type_oid(&self) -> pg_sys::Oid {
        match self {
            Self::Empty { type_oid }
            | Self::Bool { type_oid, .. }
            | Self::I32 { type_oid, .. }
            | Self::I64 { type_oid, .. }
            | Self::H3 { type_oid, .. }
            | Self::Geometry { type_oid, .. }
            | Self::Raster { type_oid, .. }
            | Self::F32 { type_oid, .. }
            | Self::F64 { type_oid, .. }
            | Self::TextDictionary { type_oid, .. } => *type_oid,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Empty { .. } => 0,
            Self::Bool { values, .. } => values.len(),
            Self::I32 { values, .. } => values.len(),
            Self::I64 { values, .. } => values.len(),
            Self::H3 { values, .. } => values.len(),
            Self::Geometry { data, .. } => data.view().row_count,
            Self::Raster { rows, .. } => rows.len(),
            Self::F32 { values, .. } => values.len(),
            Self::F64 { values, .. } => values.len(),
            Self::TextDictionary { codes, .. } => codes.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn device_bytes(&self) -> Option<u64> {
        let checked = |len: usize, width: usize, nulls: usize| {
            len.checked_mul(width)
                .and_then(|bytes| bytes.checked_add(nulls))
                .and_then(|bytes| u64::try_from(bytes).ok())
        };
        match self {
            Self::Empty { .. } => Some(0),
            Self::Bool { values, nulls, .. } => checked(
                values.len(),
                1,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::I32 { values, nulls, .. } => checked(
                values.len(),
                4,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::I64 { values, nulls, .. } => checked(
                values.len(),
                8,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::H3 { values, nulls, .. } => checked(
                values.len(),
                8,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::Geometry { data, .. } => Some(data.accounting().device_bytes),
            Self::Raster {
                pixels,
                band_offsets,
                rows,
                bands,
                nulls,
                ..
            } => pixels
                .as_ref()
                .map_or(Some(0), |values| checked(values.len(), 1, 0))
                .and_then(|bytes| {
                    checked(band_offsets.len(), std::mem::size_of::<u64>(), 0)
                        .and_then(|offsets| bytes.checked_add(offsets))
                })
                .and_then(|bytes| {
                    checked(rows.len(), std::mem::size_of::<ResidentRasterRow>(), 0)
                        .and_then(|rows| bytes.checked_add(rows))
                })
                .and_then(|bytes| {
                    bands
                        .as_ref()
                        .map_or(Some(0), |bands| {
                            checked(bands.len(), std::mem::size_of::<ResidentRasterBand>(), 0)
                        })
                        .and_then(|bands| bytes.checked_add(bands))
                })
                .and_then(|bytes| {
                    bytes.checked_add(
                        u64::try_from(nulls.as_ref().map_or(0, ExprDeviceBuffer::len)).ok()?,
                    )
                }),
            Self::F32 { values, nulls, .. } => checked(
                values.len(),
                4,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::F64 { values, nulls, .. } => checked(
                values.len(),
                8,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
            Self::TextDictionary { codes, nulls, .. } => checked(
                codes.len(),
                4,
                nulls.as_ref().map_or(0, ExprDeviceBuffer::len),
            ),
        }
    }

    #[must_use]
    pub fn accounting(&self) -> Option<ResidentByteAccounting> {
        match self {
            Self::Geometry { data, .. } => Some(data.accounting()),
            Self::Raster { exact, stats, .. } => Some(ResidentByteAccounting {
                device_bytes: self.device_bytes()?,
                retained_host_exact_bytes: exact
                    .offsets
                    .len()
                    .checked_mul(std::mem::size_of::<u64>())
                    .and_then(|offsets| offsets.checked_add(exact.bytes.len()))
                    .and_then(|host| {
                        stats
                            .band_pixels
                            .len()
                            .checked_mul(std::mem::size_of::<u64>())
                            .and_then(|bytes| {
                                stats
                                    .band_rows
                                    .len()
                                    .checked_mul(std::mem::size_of::<u64>())
                                    .and_then(|band_rows| bytes.checked_add(band_rows))
                            })
                            .and_then(|bytes| {
                                stats
                                    .work_rows
                                    .len()
                                    .checked_mul(std::mem::size_of::<
                                        super::domain::ResidentRasterWorkRow,
                                    >())
                                    .and_then(|work_rows| bytes.checked_add(work_rows))
                            })
                            .and_then(|stats| host.checked_add(stats))
                    })
                    .and_then(|bytes| u64::try_from(bytes).ok())?,
            }),
            _ => Some(ResidentByteAccounting {
                device_bytes: self.device_bytes()?,
                retained_host_exact_bytes: 0,
            }),
        }
    }

    #[must_use]
    pub fn view(&self) -> ResidentColumnView<'_> {
        match self {
            Self::Empty { type_oid } => ResidentColumnView::Empty {
                type_oid: *type_oid,
            },
            Self::Bool {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::Bool {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::I32 {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::I32 {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::I64 {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::I64 {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::H3 {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::H3 {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::Geometry { type_oid, data } => ResidentColumnView::Geometry {
                type_oid: *type_oid,
                data: data.view(),
            },
            Self::Raster {
                type_oid,
                pixels,
                band_offsets,
                rows,
                bands,
                nulls,
                exact,
                stats,
            } => ResidentColumnView::Raster {
                type_oid: *type_oid,
                pixels: pixels.as_ref(),
                band_offsets,
                rows,
                bands: bands.as_ref(),
                nulls: nulls.as_ref(),
                exact,
                stats,
            },
            Self::F32 {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::F32 {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::F64 {
                type_oid,
                values,
                nulls,
            } => ResidentColumnView::F64 {
                type_oid: *type_oid,
                values,
                nulls: nulls.as_ref(),
            },
            Self::TextDictionary {
                type_oid,
                codes,
                nulls,
                labels,
            } => ResidentColumnView::TextDictionary {
                type_oid: *type_oid,
                codes,
                nulls: nulls.as_ref(),
                labels,
            },
        }
    }
}

/// Borrowed column view. Device pointers are reachable only while the store
/// callback holds the owning relation entry borrowed.
pub enum ResidentColumnView<'a> {
    Empty {
        type_oid: pg_sys::Oid,
    },
    Bool {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<u8>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    I32 {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<i32>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    I64 {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<i64>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    H3 {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<u64>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    Geometry {
        type_oid: pg_sys::Oid,
        data: ResidentGeometryColumnView<'a>,
    },
    Raster {
        type_oid: pg_sys::Oid,
        pixels: Option<&'a ExprDeviceBuffer<u8>>,
        band_offsets: &'a ExprDeviceBuffer<u64>,
        rows: &'a ExprDeviceBuffer<ResidentRasterRow>,
        bands: Option<&'a ExprDeviceBuffer<ResidentRasterBand>>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
        exact: &'a RetainedExactValues,
        stats: &'a ResidentRasterStats,
    },
    F32 {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<f32>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    F64 {
        type_oid: pg_sys::Oid,
        values: &'a ExprDeviceBuffer<f64>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
    },
    TextDictionary {
        type_oid: pg_sys::Oid,
        codes: &'a ExprDeviceBuffer<i32>,
        nulls: Option<&'a ExprDeviceBuffer<u8>>,
        labels: &'a [String],
    },
}

impl ResidentColumnView<'_> {
    #[must_use]
    pub const fn type_oid(&self) -> pg_sys::Oid {
        match self {
            Self::Empty { type_oid }
            | Self::Bool { type_oid, .. }
            | Self::I32 { type_oid, .. }
            | Self::I64 { type_oid, .. }
            | Self::H3 { type_oid, .. }
            | Self::Geometry { type_oid, .. }
            | Self::Raster { type_oid, .. }
            | Self::F32 { type_oid, .. }
            | Self::F64 { type_oid, .. }
            | Self::TextDictionary { type_oid, .. } => *type_oid,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Empty { .. } => 0,
            Self::Bool { values, .. } => values.len(),
            Self::I32 { values, .. } => values.len(),
            Self::I64 { values, .. } => values.len(),
            Self::H3 { values, .. } => values.len(),
            Self::Geometry { data, .. } => data.row_count,
            Self::Raster { rows, .. } => rows.len(),
            Self::F32 { values, .. } => values.len(),
            Self::F64 { values, .. } => values.len(),
            Self::TextDictionary { codes, .. } => codes.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Typed output decoder carried beside a resolved derived artifact.
pub enum ResidentKeyDecoder<'a> {
    Scalar(pg_sys::Oid),
    TextDictionary(&'a [String]),
}

impl ResidentKeyDecoder<'_> {
    #[must_use]
    pub fn decode_text(&self, code: i32) -> Option<&str> {
        match self {
            Self::TextDictionary(labels) => usize::try_from(code)
                .ok()
                .and_then(|index| labels.get(index))
                .map(String::as_str),
            Self::Scalar(_) => None,
        }
    }
}

/// A derived artifact owns any device buffers produced from raw columns and
/// any original host values retained for exact recheck or reconstruction.
pub trait DerivedArtifact: Any {
    fn device_bytes(&self) -> u64;

    fn retained_host_exact_bytes(&self) -> u64;

    fn as_any(&self) -> &dyn Any;
}

fn artifact_accounting<T: DerivedArtifact + ?Sized>(artifact: &T) -> ResidentByteAccounting {
    ResidentByteAccounting {
        device_bytes: artifact.device_bytes(),
        retained_host_exact_bytes: artifact.retained_host_exact_bytes(),
    }
}

/// Exact identity for one derived shape. The digest is an index accelerator;
/// correctness always compares the complete canonical word sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedArtifactIdentity {
    digest: u64,
    canonical_words: Box<[i32]>,
}

impl DerivedArtifactIdentity {
    #[must_use]
    pub fn from_canonical_words(words: Vec<i32>) -> Self {
        let digest = shape_digest(&words);
        Self {
            digest,
            canonical_words: words.into_boxed_slice(),
        }
    }

    #[must_use]
    pub const fn digest(&self) -> u64 {
        self.digest
    }

    #[must_use]
    pub fn canonical_words(&self) -> &[i32] {
        &self.canonical_words
    }
}

/// Ordered resident-column request used by bulk artifact preparation and use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentColumnRef {
    pub relid: pg_sys::Oid,
    pub attno: i16,
}

/// Relation version captured with a derived artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentDependencyStamp {
    pub relid: pg_sys::Oid,
    pub generation: u64,
    pub global_generation: u64,
    pub relfilenode: pg_sys::Oid,
}

impl ResidentDependencyStamp {
    fn from_relation(relation: &ResidentRelation) -> Self {
        Self {
            relid: relation.relid,
            generation: relation.generation.relation,
            global_generation: relation.generation.global,
            relfilenode: relation.relfilenode,
        }
    }
}

/// Host-only result of preparing a derived artifact. The store reserves the
/// declared device bytes before passing `prepared` to the device builder.
pub struct PreparedDerived<P> {
    pub prepared: P,
    pub device_bytes: u64,
}

/// Exact accounting and transient host preparation computed before a staged
/// artifact reserves its complete physical footprint.
pub struct StagedDerivedPreflight<P> {
    pub prepared: P,
    pub accounting: ResidentByteAccounting,
}

/// Exact persistent and transient accounting established before a staged
/// workspace allocates any buffers.
pub struct StagedTransformPreflight<P> {
    pub prepared: P,
    pub published_accounting: ResidentByteAccounting,
    pub transient_accounting: ResidentByteAccounting,
}

/// A non-publishable transform workspace that reports its complete physical
/// footprint. Host bytes include snapshots, layouts, and preallocated D2H
/// destinations, regardless of whether they contain exact source values.
pub trait StagedTransformWorkspace {
    fn device_bytes(&self) -> u64;

    fn host_bytes(&self) -> u64;
}

fn workspace_accounting<W: StagedTransformWorkspace + ?Sized>(
    workspace: &W,
) -> ResidentByteAccounting {
    ResidentByteAccounting {
        device_bytes: workspace.device_bytes(),
        retained_host_exact_bytes: workspace.host_bytes(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactEnsureOutcome {
    Hit,
    Built,
    Rebuilt,
}

/// All requested raw columns and dependency evidence under one store borrow.
pub struct ResidentInputBundle<'a> {
    pub columns: Vec<ResidentColumnView<'a>>,
    pub evidence: Vec<ResidentRelationEvidence>,
}

/// Nonallocating on-demand raw-input resolver for staged device dispatch.
///
/// Unlike [`ResidentInputBundle`], this value does not collect column views or
/// evidence. Each accessor performs a bounded lookup in the already-borrowed
/// store and returns a view/value directly.
pub struct ResidentDispatchBundle<'a> {
    store: &'a RelationStore,
    columns: &'a [ResidentColumnRef],
    dependencies: &'a [ResidentDependencyStamp],
}

impl ResidentDispatchBundle<'_> {
    #[must_use]
    pub const fn column_count(&self) -> usize {
        self.columns.len()
    }

    #[must_use]
    pub const fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }

    pub fn column(&self, index: usize) -> Result<ResidentColumnView<'_>, ResidentLoadError> {
        let request =
            self.columns
                .get(index)
                .ok_or(ResidentLoadError::DispatchColumnIndexOutOfBounds {
                    index,
                    column_count: self.columns.len(),
                })?;
        self.find_column(*request)
    }

    pub fn find_column(
        &self,
        column: ResidentColumnRef,
    ) -> Result<ResidentColumnView<'_>, ResidentLoadError> {
        if !self.columns.contains(&column) {
            return Err(ResidentLoadError::MissingColumn {
                relid: column.relid,
                attno: column.attno,
            });
        }
        let relation = self
            .store
            .entries
            .iter()
            .find(|entry| entry.relid == column.relid)
            .ok_or(ResidentLoadError::MissingRelation(column.relid))?;
        relation
            .columns
            .get(&column.attno)
            .map(ResidentColumn::view)
            .ok_or(ResidentLoadError::MissingColumn {
                relid: column.relid,
                attno: column.attno,
            })
    }

    pub fn evidence(&self, index: usize) -> Result<ResidentRelationEvidence, ResidentLoadError> {
        let dependency = self.dependencies.get(index).ok_or(
            ResidentLoadError::DispatchDependencyIndexOutOfBounds {
                index,
                dependency_count: self.dependencies.len(),
            },
        )?;
        self.evidence_for(dependency.relid)
    }

    pub fn evidence_for(
        &self,
        relid: pg_sys::Oid,
    ) -> Result<ResidentRelationEvidence, ResidentLoadError> {
        if !self
            .dependencies
            .iter()
            .any(|dependency| dependency.relid == relid)
        {
            return Err(ResidentLoadError::MissingRelation(relid));
        }
        self.store
            .entries
            .iter()
            .find(|entry| entry.relid == relid)
            .map(ResidentRelation::evidence)
            .ok_or(ResidentLoadError::MissingRelation(relid))
    }
}

/// Owned, generation-stamped exact geometry bytes for post-borrow rechecks.
///
/// `storage` is supplied by the caller after the artifact ledger reservation
/// and before this snapshot touches the store. The first `row_count + 1`
/// words are exact-value offsets; the remaining aligned bytes retain the
/// original flat GSERIALIZED values. Its complete allocation is charged to
/// the derived artifact that owns the snapshot.
pub struct ResidentGeometryExactSnapshot {
    column: ResidentColumnRef,
    dependency: ResidentDependencyStamp,
    type_oid: pg_sys::Oid,
    row_count: usize,
    exact_byte_count: usize,
    storage: Box<[u64]>,
}

impl ResidentGeometryExactSnapshot {
    #[must_use]
    pub const fn column(&self) -> ResidentColumnRef {
        self.column
    }

    #[must_use]
    pub const fn dependency(&self) -> ResidentDependencyStamp {
        self.dependency
    }

    #[must_use]
    pub const fn type_oid(&self) -> pg_sys::Oid {
        self.type_oid
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub fn retained_host_bytes(&self) -> Option<u64> {
        self.storage
            .len()
            .checked_mul(std::mem::size_of::<u64>())
            .and_then(|bytes| u64::try_from(bytes).ok())
    }

    #[must_use]
    pub fn exact_value(&self, row_index: usize) -> Option<&[u8]> {
        if row_index >= self.row_count {
            return None;
        }
        let offset_count = self.row_count.checked_add(1)?;
        let start = usize::try_from(*self.storage.get(row_index)?).ok()?;
        let end = usize::try_from(*self.storage.get(row_index.checked_add(1)?)?).ok()?;
        if start > end || end > self.exact_byte_count {
            return None;
        }
        let payload_bytes = self
            .storage
            .len()
            .checked_sub(offset_count)?
            .checked_mul(std::mem::size_of::<u64>())?;
        // SAFETY: storage is an aligned contiguous u64 allocation. The
        // offset table occupies offset_count words, and payload_bytes is the
        // complete remaining initialized allocation.
        let payload = unsafe {
            std::slice::from_raw_parts(
                self.storage.as_ptr().add(offset_count).cast(),
                payload_bytes,
            )
        };
        payload.get(start..end)
    }
}

fn build_geometry_exact_snapshot(
    column: ResidentColumnRef,
    dependency: ResidentDependencyStamp,
    type_oid: pg_sys::Oid,
    row_count: usize,
    exact: &RetainedExactValues,
    mut storage: Box<[u64]>,
) -> Result<ResidentGeometryExactSnapshot, ResidentLoadError> {
    let offset_count = row_count
        .checked_add(1)
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
    if exact.offsets.len() != offset_count {
        return Err(ResidentLoadError::Loader(
            "resident geometry exact offsets do not match the relation row count".to_owned(),
        ));
    }
    let payload_words = storage.len().checked_sub(offset_count).ok_or_else(|| {
        ResidentLoadError::Loader(
            "preallocated geometry snapshot is shorter than its offset table".to_owned(),
        )
    })?;
    let payload_bytes = payload_words
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
    if exact.bytes.len() > payload_bytes {
        return Err(ResidentLoadError::Loader(
            "preallocated geometry snapshot is shorter than its exact payload".to_owned(),
        ));
    }
    storage[..offset_count].copy_from_slice(&exact.offsets);
    // SAFETY: the mutable payload begins after offset_count in-bounds words
    // and covers payload_bytes initialized bytes in the owned allocation.
    let payload = unsafe {
        std::slice::from_raw_parts_mut(
            storage.as_mut_ptr().add(offset_count).cast::<u8>(),
            payload_bytes,
        )
    };
    payload[..exact.bytes.len()].copy_from_slice(&exact.bytes);
    Ok(ResidentGeometryExactSnapshot {
        column,
        dependency,
        type_oid,
        row_count,
        exact_byte_count: exact.bytes.len(),
        storage,
    })
}

/// Typed derived artifact plus raw inputs under one store borrow. No device
/// pointer reachable from this value may escape the callback receiving it.
pub struct ResolvedDerivedInputs<'a, T> {
    pub artifact: &'a T,
    pub columns: Vec<ResidentColumnView<'a>>,
    pub evidence: Vec<ResidentRelationEvidence>,
    /// Device-only portion of `accounting`, retained for existing consumers.
    pub device_bytes: u64,
    pub accounting: ResidentByteAccounting,
}

pub(super) struct DerivedEntry {
    digest: u64,
    canonical_words: Box<[i32]>,
    dependencies: Box<[ResidentDependencyStamp]>,
    artifact: Box<dyn DerivedArtifact>,
    charge: LedgerCharge,
}

impl DerivedEntry {
    fn bytes(&self) -> u64 {
        self.charge.bytes()
    }

    fn has_identity(&self, identity: &DerivedArtifactIdentity) -> bool {
        self.digest == identity.digest
            && self.canonical_words.as_ref() == identity.canonical_words()
    }

    fn release(self) {
        let Self {
            artifact,
            mut charge,
            ..
        } = self;
        drop(artifact);
        charge.release();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResidentRelationEvidence {
    pub relid: pg_sys::Oid,
    pub generation: u64,
    pub global_generation: u64,
    pub relfilenode: pg_sys::Oid,
    pub row_count: u64,
    pub raw_bytes: u64,
    pub raw_accounting: ResidentByteAccounting,
    pub derived_bytes: u64,
    pub loaded_at_us: i64,
    pub last_used_us: i64,
    pub load_ms: f64,
}

pub struct ResolvedArtifactBundle<'a, T> {
    pub artifact: &'a T,
    pub key_decoders: Vec<ResidentKeyDecoder<'a>>,
    pub evidence: Vec<ResidentRelationEvidence>,
}

pub(super) struct ResidentRelation {
    pub(super) relid: pg_sys::Oid,
    pub(super) relfilenode: pg_sys::Oid,
    pub(super) generation: GenerationStamp,
    pub(super) columns: BTreeMap<i16, ResidentColumn>,
    pub(super) row_count: u64,
    pub(super) loaded_at_us: i64,
    pub(super) last_used_us: i64,
    pub(super) load_ms: f64,
    pub(super) last_used_tick: u64,
    pub(super) pinned: bool,
    pub(super) raw_charge: LedgerCharge,
    pub(super) raw_accounting: ResidentByteAccounting,
    pub(super) first_use_scope: Option<CommandScope>,
    pub(super) derived: Vec<DerivedEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandScope {
    xid: u32,
    command_id: u32,
}

fn current_command_scope() -> CommandScope {
    #[cfg(test)]
    {
        CommandScope {
            xid: 1,
            command_id: 1,
        }
    }
    #[cfg(not(test))]
    {
        // SAFETY: backend-main-thread transaction metadata reads. Neither call
        // assigns a new XID or advances the command counter.
        unsafe {
            CommandScope {
                xid: pg_sys::GetTopTransactionIdIfAny().into(),
                command_id: pg_sys::GetCurrentCommandId(false),
            }
        }
    }
}

fn first_use_scope_for_load(
    trigger: loader::TriggerInstall,
    pinned: bool,
    auto_load: bool,
) -> Option<CommandScope> {
    (auto_load && !pinned && matches!(trigger, loader::TriggerInstall::New))
        .then(current_command_scope)
}

/// Drain relcache callbacks from new-trigger DDL before an explicit load
/// captures or publishes its post-DDL relation snapshot.
fn continue_explicit_load_after_trigger_install<T>(
    trigger: loader::TriggerInstall,
    drain_invalidations: impl FnOnce(),
    continue_load: impl FnOnce() -> T,
) -> T {
    if matches!(trigger, loader::TriggerInstall::New) {
        drain_invalidations();
    }
    continue_load()
}

impl ResidentRelation {
    fn evidence(&self) -> ResidentRelationEvidence {
        ResidentRelationEvidence {
            relid: self.relid,
            generation: self.generation.relation,
            global_generation: self.generation.global,
            relfilenode: self.relfilenode,
            row_count: self.row_count,
            raw_bytes: self.raw_bytes(),
            raw_accounting: self.raw_accounting,
            derived_bytes: self.derived_bytes(),
            loaded_at_us: self.loaded_at_us,
            last_used_us: self.last_used_us,
            load_ms: self.load_ms,
        }
    }

    fn raw_bytes(&self) -> u64 {
        self.raw_charge.bytes()
    }

    fn derived_bytes(&self) -> u64 {
        self.derived.iter().fold(0_u64, |total, entry| {
            total
                .checked_add(entry.bytes())
                .expect("derived charges exceed cluster u64 ledger")
        })
    }

    fn remove_derived(&mut self, index: usize) {
        self.derived.remove(index).release();
    }

    fn release(mut self) {
        for entry in self.derived.drain(..) {
            entry.release();
        }
        self.columns.clear();
        self.raw_charge.release();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PinSpec {
    columns: Vec<ColumnRequest>,
}

#[derive(Default)]
struct RelationStore {
    entries: Vec<ResidentRelation>,
    pins: BTreeMap<u32, PinSpec>,
    tick: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvictionKind {
    Derived,
    RawRelation,
}

impl RelationStore {
    fn touch(&mut self, relid: pg_sys::Oid) {
        self.tick = self.tick.wrapping_add(1).max(1);
        let now = now_us();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.relid == relid) {
            entry.last_used_tick = self.tick;
            entry.last_used_us = now;
        }
    }

    fn make_pin_durable(&mut self, relid: pg_sys::Oid) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.relid == relid) else {
            return false;
        };
        entry.pinned = true;
        entry.first_use_scope = None;
        true
    }

    fn remove_relation(&mut self, relid: pg_sys::Oid) -> bool {
        let mut removed = false;
        while let Some(index) = self.entries.iter().position(|entry| entry.relid == relid) {
            self.entries.remove(index).release();
            removed = true;
        }
        removed
    }

    fn evict_lru_unpinned(&mut self, protected: &BTreeSet<u32>) -> bool {
        let candidate = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.pinned && !protected.contains(&u32::from(entry.relid)))
            .min_by_key(|(_, entry)| entry.last_used_tick)
            .map(|(index, _)| index);
        candidate.is_some_and(|index| {
            self.entries.remove(index).release();
            true
        })
    }

    fn evict_lru_derived(&mut self) -> bool {
        let candidate = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.derived.is_empty())
            .min_by_key(|(_, entry)| entry.last_used_tick)
            .map(|(index, _)| index);
        candidate.is_some_and(|index| {
            self.entries[index].remove_derived(0);
            true
        })
    }

    #[cfg(test)]
    fn evict_one_for_budget(&mut self, except: pg_sys::Oid) -> Option<EvictionKind> {
        let protected = BTreeSet::from([u32::from(except)]);
        self.evict_one_for_budget_excluding(&protected)
    }

    fn evict_one_for_budget_excluding(
        &mut self,
        protected: &BTreeSet<u32>,
    ) -> Option<EvictionKind> {
        if self.evict_lru_derived() {
            Some(EvictionKind::Derived)
        } else if self.evict_lru_unpinned(protected) {
            Some(EvictionKind::RawRelation)
        } else {
            None
        }
    }

    fn release_all(&mut self) {
        for entry in self.entries.drain(..) {
            entry.release();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedRelation {
    pub relid: pg_sys::Oid,
    pub columns: Vec<i16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResidentRelationStatus {
    pub relid: pg_sys::Oid,
    pub columns: Vec<i16>,
    pub raw_bytes: u64,
    pub raw_accounting: ResidentByteAccounting,
    pub derived_bytes: u64,
    pub pinned: bool,
    pub generation: u64,
    pub loaded_at_us: i64,
    pub last_used_us: i64,
    pub load_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResidentLoadEstimate {
    pub relid: pg_sys::Oid,
    pub loaded: bool,
    pub pinned: bool,
    pub estimated_bytes: u64,
    pub last_load_ms: Option<f64>,
    pub amortization_queries: u32,
}

/// Planner-time view of the bytes that survive local LRU reclamation.
///
/// A selected plan replaces or reuses its selected raw relations, so their
/// full post-load snapshots are supplied to [`Self::projected_final_bytes`].
/// Unrelated unpinned raw relations and every derived artifact can be evicted;
/// only raw bytes pinned by an unrelated relation must survive locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentBudgetSnapshot {
    pub cluster_live_bytes: u64,
    pub current_backend_live_bytes: u64,
    pub other_backend_live_bytes: u64,
    pub pinned_unselected_raw_bytes: u64,
    pub evictable_or_replaced_local_bytes: u64,
}

impl ResidentBudgetSnapshot {
    /// Upper-bound the final cluster footprint after local eviction and
    /// selected-relation replacement. Returns `None` on byte-count overflow.
    #[must_use]
    pub fn projected_final_bytes(
        &self,
        selected_raw_bytes: u64,
        descriptor_artifact_bytes: u64,
    ) -> Option<u64> {
        self.other_backend_live_bytes
            .checked_add(self.pinned_unselected_raw_bytes)?
            .checked_add(selected_raw_bytes)?
            .checked_add(descriptor_artifact_bytes)
    }
}

/// Exact result of ensuring one selected descriptor dependency set.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedRelationsEnsureOutcome {
    pub evidence: Vec<ResidentRelationEvidence>,
    /// Relations staged during this call, including reloads after invalidation.
    pub loaded_relations: Vec<pg_sys::Oid>,
    /// Sum of raw relation staging times for `loaded_relations` only.
    pub raw_load_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidentLoadError {
    AutoLoadDisabled {
        relid: pg_sys::Oid,
    },
    MissingRelation(pg_sys::Oid),
    MissingColumn {
        relid: pg_sys::Oid,
        attno: i16,
    },
    BudgetExceeded {
        requested: u64,
        live: u64,
        budget: u64,
    },
    Gpu(GpuError),
    Loader(String),
    InvalidArtifactDependencies(String),
    ArtifactNotFound {
        digest: u64,
    },
    ArtifactTypeMismatch {
        digest: u64,
    },
    ArtifactDependencyChanged {
        relid: pg_sys::Oid,
    },
    DispatchColumnIndexOutOfBounds {
        index: usize,
        column_count: usize,
    },
    DispatchDependencyIndexOutOfBounds {
        index: usize,
        dependency_count: usize,
    },
    ArtifactAccountingOverflow,
    ArtifactAccountingMismatch {
        declared: ResidentByteAccounting,
        actual: ResidentByteAccounting,
    },
    TransformWorkspaceAccountingMismatch {
        declared: ResidentByteAccounting,
        actual: ResidentByteAccounting,
    },
}

impl From<String> for ResidentLoadError {
    fn from(detail: String) -> Self {
        Self::Loader(detail)
    }
}

impl fmt::Display for ResidentLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AutoLoadDisabled { relid } => write!(
                f,
                "relation OID {relid} is not resident and pg_accel.auto_load is off"
            ),
            Self::MissingRelation(relid) => write!(f, "relation OID {relid} is not resident"),
            Self::MissingColumn { relid, attno } => {
                write!(f, "relation OID {relid} has no resident attribute {attno}")
            }
            Self::BudgetExceeded {
                requested,
                live,
                budget,
            } => write!(
                f,
                "resident allocation of {requested} bytes exceeds cluster budget {budget} bytes ({live} bytes live); evict/unpin entries or raise pg_accel.resident_memory_budget_mb"
            ),
            Self::Gpu(error) => error.fmt(f),
            Self::Loader(detail) => f.write_str(detail),
            Self::InvalidArtifactDependencies(detail) => {
                write!(f, "invalid resident artifact dependencies: {detail}")
            }
            Self::ArtifactNotFound { digest } => {
                write!(f, "resident derived artifact {digest:#x} is not available")
            }
            Self::ArtifactTypeMismatch { digest } => write!(
                f,
                "resident derived artifact {digest:#x} has a different Rust type"
            ),
            Self::ArtifactDependencyChanged { relid } => write!(
                f,
                "resident derived artifact dependency relation OID {relid} changed during resolution"
            ),
            Self::DispatchColumnIndexOutOfBounds {
                index,
                column_count,
            } => write!(
                f,
                "resident dispatch column index {index} exceeds column count {column_count}"
            ),
            Self::DispatchDependencyIndexOutOfBounds {
                index,
                dependency_count,
            } => write!(
                f,
                "resident dispatch dependency index {index} exceeds dependency count {dependency_count}"
            ),
            Self::ArtifactAccountingOverflow => {
                f.write_str("resident derived artifact byte accounting overflows u64")
            }
            Self::ArtifactAccountingMismatch { declared, actual } => write!(
                f,
                "resident derived artifact accounting mismatch: declared {} device + {} retained host bytes, actual {} device + {} retained host bytes",
                declared.device_bytes,
                declared.retained_host_exact_bytes,
                actual.device_bytes,
                actual.retained_host_exact_bytes
            ),
            Self::TransformWorkspaceAccountingMismatch { declared, actual } => write!(
                f,
                "resident transform workspace accounting mismatch: declared {} device + {} host bytes, actual {} device + {} host bytes",
                declared.device_bytes,
                declared.retained_host_exact_bytes,
                actual.device_bytes,
                actual.retained_host_exact_bytes
            ),
        }
    }
}

impl std::error::Error for ResidentLoadError {}

#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn resident_relcache_callback(_arg: pg_sys::Datum, relid: pg_sys::Oid) {
    if relid == pg_sys::InvalidOid {
        let _ = PENDING_RELCACHE_CLEAR_ALL.try_with(|pending| pending.set(true));
        return;
    }
    let relevant = STORE
        .try_with(|store| match store.try_borrow() {
            Ok(store) => store.entries.iter().any(|entry| entry.relid == relid),
            Err(_) => true,
        })
        .unwrap_or(false);
    if !relevant {
        return;
    }
    let _ = PENDING_RELCACHE.try_with(|pending| {
        let mut value = pending.get();
        let raw = u32::from(relid);
        if value.contains(raw) {
            return;
        }
        if value.len == PENDING_RELCACHE_CAPACITY {
            let _ = PENDING_RELCACHE_CLEAR_ALL.try_with(|clear| clear.set(true));
            return;
        }
        value.relids[value.len] = raw;
        value.len += 1;
        pending.set(value);
    });
}

#[cfg(not(test))]
fn ensure_relcache_callback() {
    if RELCACHE_CALLBACK_REGISTERED.with(Cell::get) {
        return;
    }
    // SAFETY: backend-main-thread registration. The callback and Datum live
    // for the backend lifetime and the thread-local flag prevents duplicates.
    unsafe {
        pg_sys::CacheRegisterRelcacheCallback(
            Some(resident_relcache_callback),
            pg_sys::Datum::from(0),
        );
    }
    RELCACHE_CALLBACK_REGISTERED.with(|registered| registered.set(true));
}

fn prune_invalid_relations(
    store: &mut RelationStore,
    clear_all: bool,
    pending: &PendingRelcacheInvalidations,
    command_scope: CommandScope,
    mut generation_stamp: impl FnMut(pg_sys::Oid) -> GenerationStamp,
    mut current_relfilenode: impl FnMut(pg_sys::Oid) -> Option<pg_sys::Oid>,
) {
    let mut index = 0;
    while index < store.entries.len() {
        let keep = {
            let entry = &store.entries[index];
            !clear_all
                && !pending.contains(u32::from(entry.relid))
                && entry.generation == generation_stamp(entry.relid)
                && current_relfilenode(entry.relid) == Some(entry.relfilenode)
                && entry
                    .first_use_scope
                    .is_none_or(|scope| scope == command_scope)
        };
        if keep {
            index += 1;
        } else {
            store.entries.remove(index).release();
        }
    }
}

#[cfg(not(test))]
fn process_invalidations() {
    ensure_relcache_callback();
    let clear_all = PENDING_RELCACHE_CLEAR_ALL.with(|pending| pending.replace(false));
    let pending =
        PENDING_RELCACHE.with(|pending| pending.replace(PendingRelcacheInvalidations::empty()));
    let command_scope = current_command_scope();
    STORE.with(|store| {
        prune_invalid_relations(
            &mut store.borrow_mut(),
            clear_all,
            &pending,
            command_scope,
            ledger::generation_stamp,
            loader::current_relfilenode,
        );
    });
}

#[cfg(test)]
fn process_invalidations() {
    // Plain Rust unit tests install synthetic relation entries without a live
    // PostgreSQL catalog. A one-shot generation bump lets lifecycle tests
    // model invalidations precisely between immutable-borrow stages.
    let pending = TEST_PENDING_GENERATION_BUMP.with(Cell::take);
    if let Some(relid) = pending {
        STORE.with(|store| {
            if let Some(relation) = store
                .borrow_mut()
                .entries
                .iter_mut()
                .find(|entry| entry.relid == relid)
            {
                relation.generation.relation += 1;
            }
        });
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_PENDING_GENERATION_BUMP: Cell<Option<pg_sys::Oid>> = const { Cell::new(None) };
}

#[cfg(test)]
fn schedule_test_generation_bump(relid: pg_sys::Oid) {
    TEST_PENDING_GENERATION_BUMP.with(|pending| {
        assert!(pending.replace(Some(relid)).is_none());
    });
}

pub(super) fn cleanup_backend() {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.release_all();
        store.pins.clear();
    });
}

fn now_us() -> i64 {
    #[cfg(any(test, feature = "pg_test"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_micros());
        i64::try_from(micros).unwrap_or(i64::MAX)
    }
    #[cfg(all(not(test), not(feature = "pg_test")))]
    {
        // SAFETY: backend main thread; GetCurrentTimestamp is allocation-free.
        unsafe { pg_sys::GetCurrentTimestamp() }
    }
}

fn reserve_with_local_eviction(
    relid: pg_sys::Oid,
    bytes: u64,
) -> Result<LedgerCharge, ResidentLoadError> {
    reserve_with_local_eviction_excluding(&BTreeSet::from([u32::from(relid)]), bytes)
}

fn reserve_with_local_eviction_excluding(
    protected: &BTreeSet<u32>,
    bytes: u64,
) -> Result<LedgerCharge, ResidentLoadError> {
    crate::ensure_backend_exit_callback();
    #[cfg(test)]
    let budget = u64::MAX;
    #[cfg(not(test))]
    let budget = gucs::resident_memory_budget_bytes();
    reserve_with_eviction(bytes, budget, LedgerCharge::reserve, || {
        STORE.with(|store| {
            store
                .borrow_mut()
                .evict_one_for_budget_excluding(protected)
                .is_some()
        })
    })
}

fn reserve_transform_charges_excluding(
    protected: &BTreeSet<u32>,
    persistent_bytes: u64,
    transient_bytes: u64,
) -> Result<(LedgerCharge, LedgerCharge), ResidentLoadError> {
    crate::ensure_backend_exit_callback();
    #[cfg(test)]
    let budget = u64::MAX;
    #[cfg(not(test))]
    let budget = gucs::resident_memory_budget_bytes();
    reserve_transform_charges(
        persistent_bytes,
        transient_bytes,
        budget,
        LedgerCharge::reserve,
        || {
            STORE.with(|store| {
                store
                    .borrow_mut()
                    .evict_one_for_budget_excluding(protected)
                    .is_some()
            })
        },
    )
}

fn reserve_transform_charges<T>(
    persistent_bytes: u64,
    transient_bytes: u64,
    budget: u64,
    mut reserve: impl FnMut(u64, u64) -> Result<T, u64>,
    mut evict: impl FnMut() -> bool,
) -> Result<(T, T), ResidentLoadError> {
    let persistent = reserve_with_eviction(persistent_bytes, budget, &mut reserve, &mut evict)?;
    let transient = reserve_with_eviction(transient_bytes, budget, &mut reserve, &mut evict)?;
    Ok((persistent, transient))
}

fn reserve_with_eviction<T>(
    bytes: u64,
    budget: u64,
    mut reserve: impl FnMut(u64, u64) -> Result<T, u64>,
    mut evict: impl FnMut() -> bool,
) -> Result<T, ResidentLoadError> {
    loop {
        match reserve(bytes, budget) {
            Ok(charge) => return Ok(charge),
            Err(live) => {
                if !evict() {
                    return Err(ResidentLoadError::BudgetExceeded {
                        requested: bytes,
                        live,
                        budget,
                    });
                }
            }
        }
    }
}

fn install_staged(
    staged: StagedRelation,
    pinned: bool,
    first_use_scope: Option<CommandScope>,
    protected: &BTreeSet<u32>,
) -> Result<(), ResidentLoadError> {
    let accounting = staged.accounting().map_err(ResidentLoadError::Loader)?;
    let bytes = accounting
        .checked_total()
        .map_err(|error| ResidentLoadError::Loader(error.to_string()))?;
    STORE.with(|store| {
        store.borrow_mut().remove_relation(staged.relid());
    });
    let charge = reserve_with_local_eviction_excluding(protected, bytes)?;
    let mut relation = staged
        .materialize(charge, accounting)
        .map_err(ResidentLoadError::Loader)?;
    relation.pinned = pinned;
    relation.first_use_scope = first_use_scope;
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.tick = store.tick.wrapping_add(1).max(1);
        relation.last_used_tick = store.tick;
        store.entries.push(relation);
    });
    Ok(())
}

fn required_columns_for(
    request: &SelectedRelation,
) -> Result<Vec<ColumnRequest>, ResidentLoadError> {
    let pinned = STORE.with(|store| store.borrow().pins.get(&u32::from(request.relid)).cloned());
    let mut attnos = BTreeSet::new();
    attnos.extend(request.columns.iter().copied());
    if let Some(pin) = &pinned {
        attnos.extend(pin.columns.iter().map(|column| column.attno));
    }
    let existing = STORE.with(|store| {
        store
            .borrow()
            .entries
            .iter()
            .find(|entry| entry.relid == request.relid)
            .map(|entry| {
                entry
                    .columns
                    .iter()
                    .map(|(attno, column)| ColumnRequest {
                        attno: *attno,
                        type_oid: column.type_oid(),
                    })
                    .collect::<Vec<_>>()
            })
    });
    if let Some(existing) = existing {
        attnos.extend(existing.iter().map(|column| column.attno));
    }
    loader::resolve_attnos(request.relid, &attnos.into_iter().collect::<Vec<_>>())
        .map_err(ResidentLoadError::Loader)
}

fn has_requested_columns(entry: &ResidentRelation, attnos: &[i16]) -> bool {
    attnos.iter().all(|attno| entry.columns.contains_key(attno))
}

fn missing_load_is_authorized(force: bool, pinned: bool, auto_load: bool) -> bool {
    force || pinned || auto_load
}

fn ensure_one(
    request: &SelectedRelation,
    force: bool,
) -> Result<ResidentRelationEvidence, ResidentLoadError> {
    process_invalidations();
    let protected = BTreeSet::from([u32::from(request.relid)]);
    ensure_one_after_invalidations(request, force, &protected).map(|outcome| outcome.evidence)
}

struct EnsureOutcome {
    evidence: ResidentRelationEvidence,
    installed_trigger: bool,
    loaded: bool,
}

fn ensure_one_after_invalidations(
    request: &SelectedRelation,
    force: bool,
    protected: &BTreeSet<u32>,
) -> Result<EnsureOutcome, ResidentLoadError> {
    let present = STORE.with(|store| {
        store
            .borrow()
            .entries
            .iter()
            .find(|entry| entry.relid == request.relid)
            .is_some_and(|entry| has_requested_columns(entry, &request.columns))
    });
    if present {
        STORE.with(|store| store.borrow_mut().touch(request.relid));
        let evidence = STORE.with(|store| {
            store
                .borrow()
                .entries
                .iter()
                .find(|entry| entry.relid == request.relid)
                .map(ResidentRelation::evidence)
                .ok_or(ResidentLoadError::MissingRelation(request.relid))
        })?;
        return Ok(EnsureOutcome {
            evidence,
            installed_trigger: false,
            loaded: false,
        });
    }
    let pinned = STORE.with(|store| store.borrow().pins.contains_key(&u32::from(request.relid)));
    if !missing_load_is_authorized(force, pinned, gucs::auto_load()) {
        return Err(ResidentLoadError::AutoLoadDisabled {
            relid: request.relid,
        });
    }

    let columns = required_columns_for(request)?;
    let trigger =
        loader::ensure_invalidation_trigger(request.relid).map_err(ResidentLoadError::Loader)?;
    let staged =
        loader::stage_relation(request.relid, &columns).map_err(ResidentLoadError::Loader)?;
    let first_use_scope = first_use_scope_for_load(trigger, pinned, !force);
    install_staged(staged, pinned, first_use_scope, protected)?;
    let evidence = STORE.with(|store| {
        store
            .borrow()
            .entries
            .iter()
            .find(|entry| entry.relid == request.relid)
            .map(ResidentRelation::evidence)
            .ok_or(ResidentLoadError::MissingRelation(request.relid))
    })?;
    Ok(EnsureOutcome {
        evidence,
        installed_trigger: matches!(trigger, loader::TriggerInstall::New),
        loaded: true,
    })
}

fn finalize_batch_first_use(relids: &[pg_sys::Oid], scope: CommandScope) {
    if relids.is_empty() {
        return;
    }
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        for entry in &mut store.entries {
            if !entry.pinned && relids.contains(&entry.relid) {
                entry.first_use_scope = Some(scope);
            }
        }
    });
}

fn selected_relation_relids(requests: &[SelectedRelation]) -> BTreeSet<u32> {
    requests
        .iter()
        .map(|request| u32::from(request.relid))
        .collect()
}

/// Ensure every relation/attribute required by a selected resident plan is
/// loaded and begin-time revalidated.
pub fn ensure_selected_relations(
    requests: &[SelectedRelation],
) -> Result<SelectedRelationsEnsureOutcome, ResidentLoadError> {
    process_invalidations();
    // A selected batch is one executor dependency set. Keep every requested
    // relation protected while loading each member so a later dimension
    // reservation cannot evict an earlier fact (or vice versa).
    let protected = selected_relation_relids(requests);
    let mut evidence = Vec::with_capacity(requests.len());
    let mut newly_managed = Vec::new();
    let mut loaded_relations = Vec::new();
    let mut raw_load_ms = 0.0;
    for request in requests {
        let outcome = ensure_one_after_invalidations(request, false, &protected)?;
        if outcome.installed_trigger {
            newly_managed.push(request.relid);
        }
        if outcome.loaded {
            loaded_relations.push(request.relid);
            raw_load_ms += outcome.evidence.load_ms;
        }
        evidence.push(outcome.evidence);
    }
    // Trigger creation, ALTER TABLE and cursor work can each advance the
    // command counter. Normalize all one-command snapshots after the whole
    // batch so a later relation load cannot invalidate an earlier one from
    // the same selected plan.
    finalize_batch_first_use(&newly_managed, current_command_scope());
    Ok(SelectedRelationsEnsureOutcome {
        evidence,
        loaded_relations,
        raw_load_ms,
    })
}

/// Planner-visible state and byte estimate for first-use load costing.
pub fn estimate_selected_relation(
    request: &SelectedRelation,
) -> Result<ResidentLoadEstimate, ResidentLoadError> {
    process_invalidations();
    let columns = required_columns_for(request)?;
    let estimated_bytes = loader::estimate_resident_bytes(request.relid, &columns)
        .map_err(ResidentLoadError::Loader)?;
    let (loaded, pinned, last_load_ms) = STORE.with(|store| {
        let store = store.borrow();
        let entry = store
            .entries
            .iter()
            .find(|entry| entry.relid == request.relid);
        (
            entry.is_some_and(|entry| has_requested_columns(entry, &request.columns)),
            store.pins.contains_key(&u32::from(request.relid)),
            entry.map(|entry| entry.load_ms),
        )
    });
    Ok(ResidentLoadEstimate {
        relid: request.relid,
        loaded,
        pinned,
        estimated_bytes,
        last_load_ms,
        amortization_queries: crate::engine::cost::device_limits().auto_load_amortization_queries,
    })
}

fn summarize_local_budget_bytes(
    entries: impl IntoIterator<Item = (u32, bool, u64, u64)>,
    selected_relids: &BTreeSet<u32>,
) -> Option<(u64, u64)> {
    entries.into_iter().try_fold(
        (0_u64, 0_u64),
        |(local_total, pinned_unselected_raw), (relid, pinned, raw_bytes, derived_bytes)| {
            let relation_total = raw_bytes.checked_add(derived_bytes)?;
            let local_total = local_total.checked_add(relation_total)?;
            let pinned_unselected_raw = if pinned && !selected_relids.contains(&relid) {
                pinned_unselected_raw.checked_add(raw_bytes)?
            } else {
                pinned_unselected_raw
            };
            Some((local_total, pinned_unselected_raw))
        },
    )
}

/// Capture the exact non-evictable base for planner admission of one selected
/// descriptor dependency set.
///
/// `None` means a byte counter overflowed or the backend-local store no longer
/// agrees with its cluster-ledger slot. Callers must decline rather than admit
/// against an incomplete footprint in either case.
#[must_use]
pub fn resident_budget_snapshot(selected_relids: &[pg_sys::Oid]) -> Option<ResidentBudgetSnapshot> {
    process_invalidations();
    let selected_relids = selected_relids
        .iter()
        .map(|relid| u32::from(*relid))
        .collect::<BTreeSet<_>>();
    let (local_store_bytes, pinned_unselected_raw_bytes) = STORE.with(|store| {
        let store = store.borrow();
        summarize_local_budget_bytes(
            store.entries.iter().map(|entry| {
                (
                    u32::from(entry.relid),
                    entry.pinned,
                    entry.raw_bytes(),
                    entry.derived_bytes(),
                )
            }),
            &selected_relids,
        )
    })?;
    let (cluster_live_bytes, current_backend_live_bytes) = ledger::byte_snapshot();
    if local_store_bytes != current_backend_live_bytes
        || current_backend_live_bytes > cluster_live_bytes
    {
        return None;
    }
    Some(ResidentBudgetSnapshot {
        cluster_live_bytes,
        current_backend_live_bytes,
        other_backend_live_bytes: cluster_live_bytes.checked_sub(current_backend_live_bytes)?,
        pinned_unselected_raw_bytes,
        evictable_or_replaced_local_bytes: current_backend_live_bytes
            .checked_sub(pinned_unselected_raw_bytes)?,
    })
}

/// Exact cluster-wide resident allocation total at this instant.
#[must_use]
pub fn resident_live_bytes() -> u64 {
    ledger::total_bytes()
}

/// SQL-visible cluster ledger total for lifecycle and operations diagnostics.
#[pg_extern]
fn pg_accel_resident_live_bytes() -> i64 {
    i64::try_from(resident_live_bytes())
        .unwrap_or_else(|_| pgrx::error!("resident byte ledger exceeds SQL bigint"))
}

/// Borrow one resident column without allowing its device pointer to escape.
pub fn with_resident_column<R>(
    relid: pg_sys::Oid,
    attno: i16,
    callback: impl FnOnce(ResidentColumnView<'_>) -> R,
) -> Result<R, ResidentLoadError> {
    process_invalidations();
    STORE.with(|store| store.borrow_mut().touch(relid));
    STORE.with(|store| {
        let store = store.borrow();
        let relation = store
            .entries
            .iter()
            .find(|entry| entry.relid == relid)
            .ok_or(ResidentLoadError::MissingRelation(relid))?;
        let column = relation
            .columns
            .get(&attno)
            .ok_or(ResidentLoadError::MissingColumn { relid, attno })?;
        Ok(callback(column.view()))
    })
}

fn resident_geometry_snapshot_source<'bundle, 'resident>(
    requests: &[ResidentColumnRef],
    inputs: &'bundle ResidentInputBundle<'resident>,
    column: ResidentColumnRef,
) -> Result<
    (
        pg_sys::Oid,
        &'bundle ResidentGeometryColumnView<'resident>,
        ResidentDependencyStamp,
    ),
    ResidentLoadError,
> {
    if requests.len() != inputs.columns.len() {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "resident snapshot requests do not match supplied column views".to_owned(),
        ));
    }
    let index = requests
        .iter()
        .position(|request| *request == column)
        .ok_or(ResidentLoadError::MissingColumn {
            relid: column.relid,
            attno: column.attno,
        })?;
    let (type_oid, data) = match &inputs.columns[index] {
        ResidentColumnView::Geometry { type_oid, data } => (*type_oid, data),
        _ => {
            return Err(ResidentLoadError::Loader(format!(
                "resident attribute ({}, {}) is not a geometry column",
                u32::from(column.relid),
                column.attno,
            )));
        }
    };
    let evidence = inputs
        .evidence
        .iter()
        .find(|evidence| evidence.relid == column.relid)
        .ok_or(ResidentLoadError::MissingRelation(column.relid))?;
    let evidence_rows = usize::try_from(evidence.row_count).map_err(|_| {
        ResidentLoadError::Loader("resident geometry row count exceeds usize".to_owned())
    })?;
    if evidence_rows != data.row_count {
        return Err(ResidentLoadError::Loader(
            "resident geometry snapshot row count does not match dependency evidence".to_owned(),
        ));
    }
    Ok((
        type_oid,
        data,
        ResidentDependencyStamp {
            relid: evidence.relid,
            generation: evidence.generation,
            global_generation: evidence.global_generation,
            relfilenode: evidence.relfilenode,
        },
    ))
}

/// Exact aligned-word count required by [`snapshot_resident_geometry_exact`].
pub fn resident_geometry_exact_snapshot_words(
    requests: &[ResidentColumnRef],
    inputs: &ResidentInputBundle<'_>,
    column: ResidentColumnRef,
) -> Result<usize, ResidentLoadError> {
    let (_, data, _) = resident_geometry_snapshot_source(requests, inputs, column)?;
    data.row_count
        .checked_add(1)
        .and_then(|offset_words| {
            data.exact
                .bytes
                .len()
                .checked_add(std::mem::size_of::<u64>() - 1)
                .map(|bytes| bytes / std::mem::size_of::<u64>())
                .and_then(|payload_words| offset_words.checked_add(payload_words))
        })
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)
}

/// Fill caller-preallocated aligned storage from an already-borrowed input
/// bundle, without re-entering the resident store.
///
/// The caller must reserve the snapshot's exact padded retained-host charge
/// before invoking this helper. No device pointer escapes and no
/// device-to-host transfer is performed; the source exact values are already
/// host-resident.
pub fn snapshot_resident_geometry_exact(
    requests: &[ResidentColumnRef],
    inputs: &ResidentInputBundle<'_>,
    column: ResidentColumnRef,
    storage: Box<[u64]>,
) -> Result<ResidentGeometryExactSnapshot, ResidentLoadError> {
    let (type_oid, data, dependency) = resident_geometry_snapshot_source(requests, inputs, column)?;
    let expected_words = resident_geometry_exact_snapshot_words(requests, inputs, column)?;
    if storage.len() != expected_words {
        return Err(ResidentLoadError::Loader(
            "geometry snapshot allocation does not match its exact padded word count".to_owned(),
        ));
    }
    build_geometry_exact_snapshot(
        column,
        dependency,
        type_oid,
        data.row_count,
        data.exact,
        storage,
    )
}

fn canonical_dependency_relids(
    owner_relid: pg_sys::Oid,
    dependency_relids: &[pg_sys::Oid],
    columns: &[ResidentColumnRef],
) -> Result<BTreeSet<u32>, ResidentLoadError> {
    if owner_relid == pg_sys::InvalidOid {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "owner relation OID is invalid".to_owned(),
        ));
    }
    let mut dependencies = BTreeSet::from([u32::from(owner_relid)]);
    for relid in dependency_relids {
        if *relid == pg_sys::InvalidOid {
            return Err(ResidentLoadError::InvalidArtifactDependencies(
                "dependency relation OID is invalid".to_owned(),
            ));
        }
        dependencies.insert(u32::from(*relid));
    }
    let mut seen_columns = BTreeSet::new();
    for column in columns {
        if column.relid == pg_sys::InvalidOid || column.attno <= 0 {
            return Err(ResidentLoadError::InvalidArtifactDependencies(format!(
                "invalid resident column reference ({}, {})",
                u32::from(column.relid),
                column.attno
            )));
        }
        if !dependencies.contains(&u32::from(column.relid)) {
            return Err(ResidentLoadError::InvalidArtifactDependencies(format!(
                "column relation OID {} is not a declared dependency",
                u32::from(column.relid)
            )));
        }
        if !seen_columns.insert((u32::from(column.relid), column.attno)) {
            return Err(ResidentLoadError::InvalidArtifactDependencies(format!(
                "duplicate resident column reference ({}, {})",
                u32::from(column.relid),
                column.attno
            )));
        }
    }
    Ok(dependencies)
}

fn dependency_stamps(
    store: &RelationStore,
    dependency_relids: &BTreeSet<u32>,
) -> Result<Box<[ResidentDependencyStamp]>, ResidentLoadError> {
    dependency_relids
        .iter()
        .map(|raw_relid| {
            let relid = pg_sys::Oid::from(*raw_relid);
            store
                .entries
                .iter()
                .find(|entry| entry.relid == relid)
                .map(ResidentDependencyStamp::from_relation)
                .ok_or(ResidentLoadError::MissingRelation(relid))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

fn first_dependency_mismatch(
    store: &RelationStore,
    expected: &[ResidentDependencyStamp],
) -> Option<pg_sys::Oid> {
    expected.iter().find_map(|stamp| {
        let current = store
            .entries
            .iter()
            .find(|entry| entry.relid == stamp.relid)
            .map(ResidentDependencyStamp::from_relation);
        (current != Some(*stamp)).then_some(stamp.relid)
    })
}

fn dependency_relids_match(
    dependencies: &[ResidentDependencyStamp],
    requested: &BTreeSet<u32>,
) -> bool {
    dependencies.len() == requested.len()
        && dependencies
            .iter()
            .zip(requested)
            .all(|(dependency, requested)| u32::from(dependency.relid) == *requested)
}

fn resolve_column_views<'a>(
    store: &'a RelationStore,
    columns: &[ResidentColumnRef],
) -> Result<Vec<ResidentColumnView<'a>>, ResidentLoadError> {
    columns
        .iter()
        .map(|request| {
            let relation = store
                .entries
                .iter()
                .find(|entry| entry.relid == request.relid)
                .ok_or(ResidentLoadError::MissingRelation(request.relid))?;
            relation
                .columns
                .get(&request.attno)
                .map(ResidentColumn::view)
                .ok_or(ResidentLoadError::MissingColumn {
                    relid: request.relid,
                    attno: request.attno,
                })
        })
        .collect()
}

fn dependency_evidence(
    store: &RelationStore,
    dependencies: &[ResidentDependencyStamp],
) -> Result<Vec<ResidentRelationEvidence>, ResidentLoadError> {
    dependencies
        .iter()
        .map(|dependency| {
            store
                .entries
                .iter()
                .find(|entry| entry.relid == dependency.relid)
                .map(ResidentRelation::evidence)
                .ok_or(ResidentLoadError::MissingRelation(dependency.relid))
        })
        .collect()
}

/// Find or build a dependency-stamped derived artifact.
///
/// `prepare` runs while all requested resident inputs are held under one
/// immutable store borrow and must only create host-owned data. The exact
/// device-byte charge is reserved after that borrow ends and before `build`
/// may allocate device memory.
pub fn ensure_derived_artifact<T: DerivedArtifact, P>(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    dependency_relids: &[pg_sys::Oid],
    columns: &[ResidentColumnRef],
    prepare: impl FnOnce(ResidentInputBundle<'_>) -> Result<PreparedDerived<P>, String>,
    build: impl FnOnce(P) -> Result<T, String>,
) -> Result<ArtifactEnsureOutcome, ResidentLoadError> {
    if identity.canonical_words().is_empty() {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "canonical artifact identity is empty".to_owned(),
        ));
    }
    process_invalidations();
    let protected = canonical_dependency_relids(owner_relid, dependency_relids, columns)?;

    let mut replacing_stale = false;
    let hit = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let owner_index = store
            .entries
            .iter()
            .position(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let exact_index = store.entries[owner_index]
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity));
        let Some(exact_index) = exact_index else {
            return Ok(false);
        };
        let stored_dependencies = &store.entries[owner_index].derived[exact_index].dependencies;
        let stale = !dependency_relids_match(stored_dependencies, &protected)
            || first_dependency_mismatch(&store, stored_dependencies).is_some();
        if stale {
            store.entries[owner_index].remove_derived(exact_index);
            replacing_stale = true;
            return Ok(false);
        }
        if !store.entries[owner_index].derived[exact_index]
            .artifact
            .as_any()
            .is::<T>()
        {
            return Err(ResidentLoadError::ArtifactTypeMismatch {
                digest: identity.digest(),
            });
        }
        Ok(true)
    })?;
    if hit {
        for raw_relid in &protected {
            STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
        }
        return Ok(ArtifactEnsureOutcome::Hit);
    }

    for raw_relid in &protected {
        STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
    }
    let (prepared, captured_dependencies) = STORE.with(|store| {
        let store = store.borrow();
        let captured_dependencies = dependency_stamps(&store, &protected)?;
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        let prepared = prepare(ResidentInputBundle { columns, evidence })
            .map_err(ResidentLoadError::Loader)?;
        Ok::<_, ResidentLoadError>((prepared, captured_dependencies))
    })?;

    let charge = reserve_with_local_eviction_excluding(&protected, prepared.device_bytes)?;
    let artifact = build(prepared.prepared).map_err(ResidentLoadError::Loader)?;
    let declared = ResidentByteAccounting {
        device_bytes: prepared.device_bytes,
        retained_host_exact_bytes: 0,
    };
    let actual = artifact_accounting(&artifact);
    if actual != declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
    }

    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let owner = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        if let Some(index) = owner
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity))
        {
            owner.remove_derived(index);
            replacing_stale = true;
        }
        owner.derived.push(DerivedEntry {
            digest: identity.digest(),
            canonical_words: identity.canonical_words().to_vec().into_boxed_slice(),
            dependencies: captured_dependencies,
            artifact: Box::new(artifact),
            charge,
        });
        Ok(())
    })?;
    Ok(if replacing_stale {
        ArtifactEnsureOutcome::Rebuilt
    } else {
        ArtifactEnsureOutcome::Built
    })
}

/// Find or build a dependency-stamped artifact directly from resident device
/// inputs after reserving its complete ledger charge.
///
/// Unlike [`ensure_derived_artifact`], `build` runs while all requested raw
/// columns remain pinned under one immutable store borrow. This permits a
/// synchronous device-to-device transform without copying the source lanes to
/// host memory. `declared` must include both device allocations and retained
/// exact host values; the store charges their checked total before invoking
/// `build` and verifies both categories before publication.
/// Device failures stay typed as [`ResidentLoadError::Gpu`] so executor
/// boundaries can preserve domain-specific SQL error semantics.
///
/// The callback must not re-enter residency APIs or execute SPI/catalog work.
pub fn ensure_device_derived_artifact<T: DerivedArtifact>(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    dependency_relids: &[pg_sys::Oid],
    columns: &[ResidentColumnRef],
    declared: ResidentByteAccounting,
    build: impl FnOnce(ResidentInputBundle<'_>) -> Result<T, ResidentLoadError>,
) -> Result<ArtifactEnsureOutcome, ResidentLoadError> {
    if identity.canonical_words().is_empty() {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "canonical artifact identity is empty".to_owned(),
        ));
    }
    let declared_total = declared
        .checked_total()
        .map_err(|_| ResidentLoadError::ArtifactAccountingOverflow)?;
    process_invalidations();
    let protected = canonical_dependency_relids(owner_relid, dependency_relids, columns)?;

    let mut replacing_stale = false;
    let hit = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let owner_index = store
            .entries
            .iter()
            .position(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let exact_index = store.entries[owner_index]
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity));
        let Some(exact_index) = exact_index else {
            return Ok(false);
        };
        let stored_dependencies = &store.entries[owner_index].derived[exact_index].dependencies;
        let stale = !dependency_relids_match(stored_dependencies, &protected)
            || first_dependency_mismatch(&store, stored_dependencies).is_some();
        if stale {
            store.entries[owner_index].remove_derived(exact_index);
            replacing_stale = true;
            return Ok(false);
        }
        let artifact = &store.entries[owner_index].derived[exact_index].artifact;
        if !artifact.as_any().is::<T>() {
            return Err(ResidentLoadError::ArtifactTypeMismatch {
                digest: identity.digest(),
            });
        }
        let actual = artifact_accounting(artifact.as_ref());
        if actual != declared {
            return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
        }
        Ok(true)
    })?;
    if hit {
        for raw_relid in &protected {
            STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
        }
        return Ok(ArtifactEnsureOutcome::Hit);
    }

    let charge = reserve_with_local_eviction_excluding(&protected, declared_total)?;
    for raw_relid in &protected {
        STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
    }
    let (artifact, captured_dependencies) = STORE.with(|store| {
        let store = store.borrow();
        let captured_dependencies = dependency_stamps(&store, &protected)?;
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        let artifact = build(ResidentInputBundle { columns, evidence })?;
        Ok::<_, ResidentLoadError>((artifact, captured_dependencies))
    })?;
    let actual = artifact_accounting(&artifact);
    if actual != declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
    }

    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let owner = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        if let Some(index) = owner
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity))
        {
            owner.remove_derived(index);
            replacing_stale = true;
        }
        owner.derived.push(DerivedEntry {
            digest: identity.digest(),
            canonical_words: identity.canonical_words().to_vec().into_boxed_slice(),
            dependencies: captured_dependencies,
            artifact: Box::new(artifact),
            charge,
        });
        Ok(())
    })?;
    Ok(if replacing_stale {
        ArtifactEnsureOutcome::Rebuilt
    } else {
        ArtifactEnsureOutcome::Built
    })
}

/// Build one composite dependency-stamped artifact through explicit staged
/// borrows and one exact reserve-first ledger charge.
///
/// `preflight` runs only on a cache miss and may compute transient host state
/// needed to determine the exact physical accounting. After that exact charge
/// is reserved, `snapshot_prepare` may copy host-resident exact values and
/// finish host preparation under the same dependency generations. `build`
/// then allocates/uploads every artifact-owned host and device buffer outside
/// the store borrow. Finally, `dispatch` reborrows the raw inputs, requires the
/// same generations again, and must perform only checked pointer/device work:
/// it must not allocate, perform catalog/SPI work, or copy device metadata to
/// host. `finalize` runs outside the store borrow before one final dependency
/// check and publication.
///
/// The built artifact's device and retained-host byte categories must exactly
/// equal the preflight declaration; upper-bound or padded hidden accounting is
/// rejected.
#[allow(clippy::too_many_arguments)]
pub fn ensure_staged_device_derived_artifact<T: DerivedArtifact, P, S>(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    dependency_relids: &[pg_sys::Oid],
    columns: &[ResidentColumnRef],
    preflight: impl FnOnce(
        ResidentInputBundle<'_>,
    ) -> Result<StagedDerivedPreflight<P>, ResidentLoadError>,
    snapshot_prepare: impl FnOnce(P, ResidentInputBundle<'_>) -> Result<S, ResidentLoadError>,
    build: impl FnOnce(S) -> Result<T, ResidentLoadError>,
    dispatch: impl FnOnce(&mut T, ResidentDispatchBundle<'_>) -> Result<(), ResidentLoadError>,
    finalize: impl FnOnce(&mut T) -> Result<(), ResidentLoadError>,
) -> Result<ArtifactEnsureOutcome, ResidentLoadError> {
    if identity.canonical_words().is_empty() {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "canonical artifact identity is empty".to_owned(),
        ));
    }
    process_invalidations();
    let protected = canonical_dependency_relids(owner_relid, dependency_relids, columns)?;

    let mut replacing_stale = false;
    let hit = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let owner_index = store
            .entries
            .iter()
            .position(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let exact_index = store.entries[owner_index]
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity));
        let Some(exact_index) = exact_index else {
            return Ok(false);
        };
        let stored_dependencies = &store.entries[owner_index].derived[exact_index].dependencies;
        let stale = !dependency_relids_match(stored_dependencies, &protected)
            || first_dependency_mismatch(&store, stored_dependencies).is_some();
        if stale {
            store.entries[owner_index].remove_derived(exact_index);
            replacing_stale = true;
            return Ok(false);
        }
        if !store.entries[owner_index].derived[exact_index]
            .artifact
            .as_any()
            .is::<T>()
        {
            return Err(ResidentLoadError::ArtifactTypeMismatch {
                digest: identity.digest(),
            });
        }
        Ok(true)
    })?;
    if hit {
        for raw_relid in &protected {
            STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
        }
        return Ok(ArtifactEnsureOutcome::Hit);
    }

    for raw_relid in &protected {
        STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
    }
    let (preflight, captured_dependencies) = STORE.with(|store| {
        let store = store.borrow();
        let captured_dependencies = dependency_stamps(&store, &protected)?;
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        let preflight = preflight(ResidentInputBundle { columns, evidence })?;
        Ok::<_, ResidentLoadError>((preflight, captured_dependencies))
    })?;
    let declared = preflight.accounting;
    let declared_total = declared
        .checked_total()
        .map_err(|_| ResidentLoadError::ArtifactAccountingOverflow)?;
    let charge = reserve_with_local_eviction_excluding(&protected, declared_total)?;

    process_invalidations();
    let snapshot = STORE.with(|store| {
        let store = store.borrow();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        snapshot_prepare(
            preflight.prepared,
            ResidentInputBundle { columns, evidence },
        )
    })?;
    let mut artifact = build(snapshot)?;
    let actual = artifact_accounting(&artifact);
    if actual != declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
    }

    process_invalidations();
    STORE.with(|store| {
        let store = store.borrow();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        dispatch(
            &mut artifact,
            ResidentDispatchBundle {
                store: &store,
                columns,
                dependencies: &captured_dependencies,
            },
        )
    })?;
    finalize(&mut artifact)?;
    let actual = artifact_accounting(&artifact);
    if actual != declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
    }

    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let owner = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        if let Some(index) = owner
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity))
        {
            owner.remove_derived(index);
            replacing_stale = true;
        }
        owner.derived.push(DerivedEntry {
            digest: identity.digest(),
            canonical_words: identity.canonical_words().to_vec().into_boxed_slice(),
            dependencies: captured_dependencies,
            artifact: Box::new(artifact),
            charge,
        });
        Ok(())
    })?;
    Ok(if replacing_stale {
        ArtifactEnsureOutcome::Rebuilt
    } else {
        ArtifactEnsureOutcome::Built
    })
}

/// Build one dependency-stamped artifact through an exactly charged transient
/// workspace.
///
/// `preflight` declares the final published footprint and the complete
/// physical footprint of `W`. The store reserves those as distinct persistent
/// and transient ledger charges before `snapshot_prepare` allocates or copies
/// host state. `build_workspace` must preallocate every workspace-owned device
/// buffer and host destination, and the store rejects it unless its reported
/// accounting exactly matches the transient declaration.
///
/// `dispatch` runs under the checked raw-input borrow and therefore must be a
/// nonallocating raw launch. `finalize` consumes `W` outside that borrow while
/// both charges remain held; it may fill preallocated host destinations and
/// construct `T`, but it must not introduce an unaccounted allocation
/// category. The transient charge is released only after `T` exactly matches
/// the published declaration, then one final dependency check publishes `T`
/// with the persistent charge alone.
#[allow(clippy::too_many_arguments)]
pub fn ensure_staged_device_transform_artifact<
    T: DerivedArtifact,
    W: StagedTransformWorkspace,
    P,
    S,
>(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    dependency_relids: &[pg_sys::Oid],
    columns: &[ResidentColumnRef],
    preflight: impl FnOnce(
        ResidentInputBundle<'_>,
    ) -> Result<StagedTransformPreflight<P>, ResidentLoadError>,
    snapshot_prepare: impl FnOnce(P, ResidentInputBundle<'_>) -> Result<S, ResidentLoadError>,
    build_workspace: impl FnOnce(S) -> Result<W, ResidentLoadError>,
    dispatch: impl FnOnce(&mut W, ResidentDispatchBundle<'_>) -> Result<(), ResidentLoadError>,
    finalize: impl FnOnce(W) -> Result<T, ResidentLoadError>,
) -> Result<ArtifactEnsureOutcome, ResidentLoadError> {
    if identity.canonical_words().is_empty() {
        return Err(ResidentLoadError::InvalidArtifactDependencies(
            "canonical artifact identity is empty".to_owned(),
        ));
    }
    process_invalidations();
    let protected = canonical_dependency_relids(owner_relid, dependency_relids, columns)?;

    let mut replacing_stale = false;
    let hit = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let owner_index = store
            .entries
            .iter()
            .position(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let exact_index = store.entries[owner_index]
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity));
        let Some(exact_index) = exact_index else {
            return Ok(false);
        };
        let stored_dependencies = &store.entries[owner_index].derived[exact_index].dependencies;
        let stale = !dependency_relids_match(stored_dependencies, &protected)
            || first_dependency_mismatch(&store, stored_dependencies).is_some();
        if stale {
            store.entries[owner_index].remove_derived(exact_index);
            replacing_stale = true;
            return Ok(false);
        }
        if !store.entries[owner_index].derived[exact_index]
            .artifact
            .as_any()
            .is::<T>()
        {
            return Err(ResidentLoadError::ArtifactTypeMismatch {
                digest: identity.digest(),
            });
        }
        Ok(true)
    })?;
    if hit {
        for raw_relid in &protected {
            STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
        }
        return Ok(ArtifactEnsureOutcome::Hit);
    }

    for raw_relid in &protected {
        STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
    }
    let (preflight, captured_dependencies) = STORE.with(|store| {
        let store = store.borrow();
        let captured_dependencies = dependency_stamps(&store, &protected)?;
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        let preflight = preflight(ResidentInputBundle { columns, evidence })?;
        Ok::<_, ResidentLoadError>((preflight, captured_dependencies))
    })?;
    let published_declared = preflight.published_accounting;
    let transient_declared = preflight.transient_accounting;
    let published_total = published_declared
        .checked_total()
        .map_err(|_| ResidentLoadError::ArtifactAccountingOverflow)?;
    let transient_total = transient_declared
        .checked_total()
        .map_err(|_| ResidentLoadError::ArtifactAccountingOverflow)?;
    published_total
        .checked_add(transient_total)
        .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
    let (persistent_charge, transient_charge) =
        reserve_transform_charges_excluding(&protected, published_total, transient_total)?;

    process_invalidations();
    let snapshot = STORE.with(|store| {
        let store = store.borrow();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &captured_dependencies)?;
        snapshot_prepare(
            preflight.prepared,
            ResidentInputBundle { columns, evidence },
        )
    })?;
    let mut workspace = build_workspace(snapshot)?;
    let workspace_actual = workspace_accounting(&workspace);
    if workspace_actual != transient_declared {
        drop(workspace);
        return Err(ResidentLoadError::TransformWorkspaceAccountingMismatch {
            declared: transient_declared,
            actual: workspace_actual,
        });
    }

    process_invalidations();
    STORE.with(|store| {
        let store = store.borrow();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        dispatch(
            &mut workspace,
            ResidentDispatchBundle {
                store: &store,
                columns,
                dependencies: &captured_dependencies,
            },
        )
    })?;
    let artifact = finalize(workspace)?;
    let artifact_actual = artifact_accounting(&artifact);
    if artifact_actual != published_declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch {
            declared: published_declared,
            actual: artifact_actual,
        });
    }

    drop(transient_charge);
    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        if let Some(relid) = first_dependency_mismatch(&store, &captured_dependencies) {
            return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
        }
        let owner = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        if let Some(index) = owner
            .derived
            .iter()
            .position(|entry| entry.has_identity(identity))
        {
            owner.remove_derived(index);
            replacing_stale = true;
        }
        owner.derived.push(DerivedEntry {
            digest: identity.digest(),
            canonical_words: identity.canonical_words().to_vec().into_boxed_slice(),
            dependencies: captured_dependencies,
            artifact: Box::new(artifact),
            charge: persistent_charge,
        });
        Ok(())
    })?;
    Ok(if replacing_stale {
        ArtifactEnsureOutcome::Rebuilt
    } else {
        ArtifactEnsureOutcome::Built
    })
}

/// Resolve an exact dependency-stamped artifact and all requested raw inputs
/// under one immutable store borrow. Stale artifacts are removed before this
/// function returns an error, so callers may rebuild and retry once.
///
/// The callback must not re-enter residency APIs or execute SPI/catalog work:
/// the backend-local `RefCell` borrow intentionally stays live for the complete
/// synchronous callback so every exposed device pointer remains pinned.
pub fn with_derived_artifact_inputs<T: Any, R>(
    owner_relid: pg_sys::Oid,
    identity: &DerivedArtifactIdentity,
    columns: &[ResidentColumnRef],
    callback: impl FnOnce(ResolvedDerivedInputs<'_, T>) -> R,
) -> Result<R, ResidentLoadError> {
    process_invalidations();
    let dependencies = STORE.with(|store| {
        let store = store.borrow();
        let owner = store
            .entries
            .iter()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        owner
            .derived
            .iter()
            .find(|entry| entry.has_identity(identity))
            .map(|entry| entry.dependencies.clone())
            .ok_or(ResidentLoadError::ArtifactNotFound {
                digest: identity.digest(),
            })
    })?;

    if let Some(relid) = STORE.with(|store| {
        let store = store.borrow();
        first_dependency_mismatch(&store, &dependencies)
    }) {
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            if let Some(owner) = store
                .entries
                .iter_mut()
                .find(|entry| entry.relid == owner_relid)
                && let Some(index) = owner
                    .derived
                    .iter()
                    .position(|entry| entry.has_identity(identity))
            {
                owner.remove_derived(index);
            }
        });
        return Err(ResidentLoadError::ArtifactDependencyChanged { relid });
    }

    let dependency_relids = dependencies
        .iter()
        .map(|dependency| u32::from(dependency.relid))
        .collect::<BTreeSet<_>>();
    let dependency_oids = dependency_relids
        .iter()
        .copied()
        .map(pg_sys::Oid::from)
        .collect::<Vec<_>>();
    canonical_dependency_relids(owner_relid, &dependency_oids, columns)?;
    for raw_relid in &dependency_relids {
        STORE.with(|store| store.borrow_mut().touch(pg_sys::Oid::from(*raw_relid)));
    }

    STORE.with(|store| {
        let store = store.borrow();
        let owner = store
            .entries
            .iter()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let entry = owner
            .derived
            .iter()
            .find(|entry| entry.has_identity(identity))
            .ok_or(ResidentLoadError::ArtifactNotFound {
                digest: identity.digest(),
            })?;
        let artifact = entry.artifact.as_any().downcast_ref::<T>().ok_or(
            ResidentLoadError::ArtifactTypeMismatch {
                digest: identity.digest(),
            },
        )?;
        let columns = resolve_column_views(&store, columns)?;
        let evidence = dependency_evidence(&store, &entry.dependencies)?;
        let accounting = artifact_accounting(entry.artifact.as_ref());
        Ok(callback(ResolvedDerivedInputs {
            artifact,
            columns,
            evidence,
            device_bytes: accounting.device_bytes,
            accounting,
        }))
    })
}

/// Register or replace a shape-digested derived artifact on its owner relation.
pub fn register_derived_artifact<T: DerivedArtifact>(
    owner_relid: pg_sys::Oid,
    digest: u64,
    declared_bytes: u64,
    build: impl FnOnce() -> Result<T, String>,
) -> Result<(), ResidentLoadError> {
    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let relation = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let index = relation
            .derived
            .iter()
            .position(|entry| entry.digest == digest && entry.canonical_words.is_empty());
        if let Some(index) = index {
            relation.remove_derived(index);
        }
        Ok::<_, ResidentLoadError>(())
    })?;
    // Drop the old buffers and release their charge before reserving the replacement.
    // Holding both would require charging old+new transiently and can reject an
    // otherwise valid same-size refresh. A failed rebuild therefore leaves a
    // cache miss, never an unaccounted allocation or stale artifact.
    let charge = reserve_with_local_eviction(owner_relid, declared_bytes)?;
    let artifact = build().map_err(ResidentLoadError::Loader)?;
    let declared = ResidentByteAccounting {
        device_bytes: declared_bytes,
        retained_host_exact_bytes: 0,
    };
    let actual = artifact_accounting(&artifact);
    if actual != declared {
        drop(artifact);
        return Err(ResidentLoadError::ArtifactAccountingMismatch { declared, actual });
    }
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let relation = store
            .entries
            .iter_mut()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        relation.derived.push(DerivedEntry {
            digest,
            canonical_words: Box::default(),
            dependencies: Box::default(),
            artifact: Box::new(artifact),
            charge,
        });
        Ok(())
    })
}

pub fn with_derived_artifact<T: Any, R>(
    owner_relid: pg_sys::Oid,
    digest: u64,
    callback: impl FnOnce(&T) -> R,
) -> Result<R, ResidentLoadError> {
    process_invalidations();
    STORE.with(|store| store.borrow_mut().touch(owner_relid));
    STORE.with(|store| {
        let store = store.borrow();
        let relation = store
            .entries
            .iter()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let entry = relation
            .derived
            .iter()
            .find(|entry| entry.digest == digest && entry.canonical_words.is_empty())
            .ok_or(ResidentLoadError::ArtifactTypeMismatch { digest })?;
        let artifact = entry
            .artifact
            .as_any()
            .downcast_ref::<T>()
            .ok_or(ResidentLoadError::ArtifactTypeMismatch { digest })?;
        Ok(callback(artifact))
    })
}

/// Resolve a typed artifact with dictionary decoders and relation evidence.
/// Projection ordering remains a planner/codec concern and is intentionally
/// absent from this residency bundle.
pub fn with_resolved_artifact<T: Any, R>(
    owner_relid: pg_sys::Oid,
    digest: u64,
    evidence_relids: &[pg_sys::Oid],
    key_columns: &[(pg_sys::Oid, i16)],
    callback: impl FnOnce(ResolvedArtifactBundle<'_, T>) -> R,
) -> Result<R, ResidentLoadError> {
    process_invalidations();
    STORE.with(|store| {
        let store = store.borrow();
        let owner = store
            .entries
            .iter()
            .find(|entry| entry.relid == owner_relid)
            .ok_or(ResidentLoadError::MissingRelation(owner_relid))?;
        let entry = owner
            .derived
            .iter()
            .find(|entry| entry.digest == digest && entry.canonical_words.is_empty())
            .ok_or(ResidentLoadError::ArtifactTypeMismatch { digest })?;
        let artifact = entry
            .artifact
            .as_any()
            .downcast_ref::<T>()
            .ok_or(ResidentLoadError::ArtifactTypeMismatch { digest })?;
        let mut key_decoders = Vec::with_capacity(key_columns.len());
        for (relid, attno) in key_columns {
            let relation = store
                .entries
                .iter()
                .find(|entry| entry.relid == *relid)
                .ok_or(ResidentLoadError::MissingRelation(*relid))?;
            let column = relation
                .columns
                .get(attno)
                .ok_or(ResidentLoadError::MissingColumn {
                    relid: *relid,
                    attno: *attno,
                })?;
            key_decoders.push(match column {
                ResidentColumn::TextDictionary { labels, .. } => {
                    ResidentKeyDecoder::TextDictionary(labels)
                }
                other => ResidentKeyDecoder::Scalar(other.type_oid()),
            });
        }
        let evidence = evidence_relids
            .iter()
            .map(|relid| {
                store
                    .entries
                    .iter()
                    .find(|entry| entry.relid == *relid)
                    .map(ResidentRelation::evidence)
                    .ok_or(ResidentLoadError::MissingRelation(*relid))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(callback(ResolvedArtifactBundle {
            artifact,
            key_decoders,
            evidence,
        }))
    })
}

/// Stable FNV-1a digest of the strict wire encoding for derived-artifact keys.
#[must_use]
pub fn shape_digest(words: &[i32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn pin_relation(
    relid: pg_sys::Oid,
    columns: Option<Vec<String>>,
) -> Result<u64, ResidentLoadError> {
    let columns = loader::resolve_column_names(relid, columns.as_deref())
        .map_err(ResidentLoadError::Loader)?;
    process_invalidations();
    let trigger = loader::ensure_invalidation_trigger(relid).map_err(ResidentLoadError::Loader)?;
    continue_explicit_load_after_trigger_install(trigger, process_invalidations, || {
        let previous = STORE.with(|store| {
            store.borrow_mut().pins.insert(
                u32::from(relid),
                PinSpec {
                    columns: columns.clone(),
                },
            )
        });
        let request = SelectedRelation {
            relid,
            columns: columns.iter().map(|column| column.attno).collect(),
        };
        match ensure_one(&request, true) {
            Ok(evidence) => {
                let promoted =
                    STORE.with(|store| store.borrow_mut().make_pin_durable(request.relid));
                debug_assert!(
                    promoted,
                    "successful pin ensure must leave a resident entry"
                );
                Ok(evidence.row_count)
            }
            Err(error) => {
                STORE.with(|store| {
                    let mut store = store.borrow_mut();
                    if let Some(previous) = previous {
                        store.pins.insert(u32::from(relid), previous);
                    } else {
                        store.pins.remove(&u32::from(relid));
                    }
                });
                Err(error)
            }
        }
    })
}

fn unpin_relation(relid: pg_sys::Oid) -> bool {
    process_invalidations();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        let removed = store.pins.remove(&u32::from(relid)).is_some();
        if let Some(entry) = store.entries.iter_mut().find(|entry| entry.relid == relid) {
            entry.pinned = false;
        }
        removed
    })
}

fn refresh_relation(relid: pg_sys::Oid) -> Result<u64, ResidentLoadError> {
    process_invalidations();
    let pinned_attnos = STORE.with(|store| {
        store.borrow().pins.get(&u32::from(relid)).map(|pin| {
            pin.columns
                .iter()
                .map(|column| column.attno)
                .collect::<Vec<_>>()
        })
    });
    let (columns, update_pin) = if let Some(attnos) = pinned_attnos {
        (
            loader::resolve_attnos(relid, &attnos).map_err(ResidentLoadError::Loader)?,
            true,
        )
    } else {
        let columns = STORE
            .with(|store| {
                store
                    .borrow()
                    .entries
                    .iter()
                    .find(|entry| entry.relid == relid)
                    .map(|entry| {
                        entry
                            .columns
                            .iter()
                            .map(|(attno, column)| ColumnRequest {
                                attno: *attno,
                                type_oid: column.type_oid(),
                            })
                            .collect()
                    })
            })
            .ok_or(ResidentLoadError::MissingRelation(relid))?;
        (columns, false)
    };
    let trigger = loader::ensure_invalidation_trigger(relid).map_err(ResidentLoadError::Loader)?;
    let staged =
        continue_explicit_load_after_trigger_install(trigger, process_invalidations, || {
            loader::stage_relation(relid, &columns).map_err(ResidentLoadError::Loader)
        })?;
    let rows = staged.row_count();
    let pinned = STORE.with(|store| store.borrow().pins.contains_key(&u32::from(relid)));
    let first_use_scope = first_use_scope_for_load(trigger, pinned, false);
    let protected = BTreeSet::from([u32::from(relid)]);
    install_staged(staged, pinned, first_use_scope, &protected)?;
    if update_pin {
        STORE.with(|store| {
            if let Some(pin) = store.borrow_mut().pins.get_mut(&u32::from(relid)) {
                pin.columns = columns;
            }
        });
    }
    Ok(rows)
}

#[pg_extern]
fn _pg_accel_pin(table_oid: pg_sys::Oid, columns: default!(Option<Vec<String>>, "NULL")) -> i64 {
    let rows = pin_relation(table_oid, columns)
        .unwrap_or_else(|error| pgrx::error!("pg_accel_pin: {error}"));
    i64::try_from(rows).unwrap_or_else(|_| pgrx::error!("pg_accel_pin: row count exceeds bigint"))
}

#[pg_extern]
fn _pg_accel_unpin(table_oid: pg_sys::Oid) -> bool {
    unpin_relation(table_oid)
}

#[pg_extern]
fn _pg_accel_refresh(table_oid: pg_sys::Oid) -> i64 {
    let rows = refresh_relation(table_oid)
        .unwrap_or_else(|error| pgrx::error!("pg_accel_refresh: {error}"));
    i64::try_from(rows)
        .unwrap_or_else(|_| pgrx::error!("pg_accel_refresh: row count exceeds bigint"))
}

#[pg_extern]
fn _pg_accel_evict(table_oid: pg_sys::Oid) -> bool {
    process_invalidations();
    STORE.with(|store| store.borrow_mut().remove_relation(table_oid))
}

pgrx::extension_sql!(
    r"
CREATE FUNCTION pg_accel_pin(table_name regclass, columns text[] DEFAULT NULL)
RETURNS bigint LANGUAGE SQL VOLATILE PARALLEL UNSAFE
BEGIN ATOMIC
    SELECT _pg_accel_pin(table_name::oid, columns);
END;

CREATE FUNCTION pg_accel_unpin(table_name regclass)
RETURNS boolean LANGUAGE SQL VOLATILE PARALLEL UNSAFE
BEGIN ATOMIC
    SELECT _pg_accel_unpin(table_name::oid);
END;

CREATE FUNCTION pg_accel_refresh(table_name regclass)
RETURNS bigint LANGUAGE SQL VOLATILE PARALLEL UNSAFE
BEGIN ATOMIC
    SELECT _pg_accel_refresh(table_name::oid);
END;

CREATE FUNCTION pg_accel_evict(table_name regclass)
RETURNS boolean LANGUAGE SQL VOLATILE PARALLEL UNSAFE
BEGIN ATOMIC
    SELECT _pg_accel_evict(table_name::oid);
END;
",
    name = "pg_accel_residency_v2_sql",
    requires = [
        _pg_accel_pin,
        _pg_accel_unpin,
        _pg_accel_refresh,
        _pg_accel_evict
    ]
);

#[pg_extern]
#[allow(clippy::type_complexity)] // SQL column names are encoded in the tuple type.
fn pg_accel_resident_status() -> TableIterator<
    'static,
    (
        name!(relid, pg_sys::Oid),
        name!(columns, Vec<i32>),
        name!(raw_bytes, i64),
        name!(derived_bytes, i64),
        name!(pinned, bool),
        name!(generation, i64),
        name!(loaded_at, Option<i64>),
        name!(last_used, Option<i64>),
        name!(load_ms, Option<f64>),
    ),
> {
    process_invalidations();
    let rows = STORE.with(|store| {
        let store = store.borrow();
        let mut rows = store
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.relid,
                    entry
                        .columns
                        .keys()
                        .copied()
                        .map(i32::from)
                        .collect::<Vec<_>>(),
                    i64::try_from(entry.raw_bytes())
                        .expect("resident raw-byte charge exceeds SQL bigint"),
                    i64::try_from(entry.derived_bytes())
                        .expect("resident derived-byte charge exceeds SQL bigint"),
                    entry.pinned,
                    i64::try_from(entry.generation.relation).unwrap_or(i64::MAX),
                    Some(entry.loaded_at_us),
                    Some(entry.last_used_us),
                    Some(entry.load_ms),
                )
            })
            .collect::<Vec<_>>();
        for (raw_relid, pin) in &store.pins {
            let relid = pg_sys::Oid::from(*raw_relid);
            if store.entries.iter().any(|entry| entry.relid == relid) {
                continue;
            }
            rows.push((
                relid,
                pin.columns
                    .iter()
                    .map(|column| i32::from(column.attno))
                    .collect(),
                0,
                0,
                true,
                i64::try_from(ledger::generation_stamp(relid).relation)
                    .expect("resident generation exceeds SQL bigint"),
                None,
                None,
                None,
            ));
        }
        rows
    });
    TableIterator::new(rows)
}

#[derive(Debug)]
enum ResidencyTriggerError {
    Pgrx(pgrx::PgTriggerError),
    Contract(&'static str),
}

impl fmt::Display for ResidencyTriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pgrx(error) => error.fmt(f),
            Self::Contract(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for ResidencyTriggerError {}

impl From<pgrx::PgTriggerError> for ResidencyTriggerError {
    fn from(error: pgrx::PgTriggerError) -> Self {
        Self::Pgrx(error)
    }
}

fn validate_trigger_contract(
    level: PgTriggerLevel,
    when: PgTriggerWhen,
    operation: PgTriggerOperation,
) -> Result<(), ResidencyTriggerError> {
    if !matches!(level, PgTriggerLevel::Statement) {
        return Err(ResidencyTriggerError::Contract(
            "pg_accel_residency_invalidate must be attached FOR EACH STATEMENT; a row-level trigger is unsafe",
        ));
    }
    if !matches!(when, PgTriggerWhen::After) {
        return Err(ResidencyTriggerError::Contract(
            "pg_accel_residency_invalidate must be an AFTER trigger",
        ));
    }
    match operation {
        PgTriggerOperation::Insert
        | PgTriggerOperation::Update
        | PgTriggerOperation::Delete
        | PgTriggerOperation::Truncate => Ok(()),
    }
}

#[pg_trigger]
fn pg_accel_residency_invalidate<'a>(
    trigger: &'a PgTrigger<'a>,
) -> Result<Option<PgHeapTuple<'a, AllocatedByPostgres>>, ResidencyTriggerError> {
    if trigger.name()? != "__pg_accel_residency_v2_7d9e" {
        return Err(ResidencyTriggerError::Contract(
            "pg_accel_residency_invalidate may only be used by pg_accel_pin-managed triggers",
        ));
    }
    validate_trigger_contract(trigger.level(), trigger.when()?, trigger.op()?)?;
    if !trigger.extra_args()?.is_empty() {
        return Err(ResidencyTriggerError::Contract(
            "pg_accel residency invalidation trigger does not accept arguments",
        ));
    }
    ledger::note_relation_change(trigger.relid()?);
    Ok(None)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::rc::Rc;

    struct TestCountingAllocator;

    std::thread_local! {
        static COUNT_TEST_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static TEST_ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    fn record_test_allocation() {
        let _ = COUNT_TEST_ALLOCATIONS.try_with(|enabled| {
            if enabled.get() {
                let _ = TEST_ALLOCATION_COUNT.try_with(|count| {
                    count.set(count.get().saturating_add(1));
                });
            }
        });
    }

    // SAFETY: every operation delegates to `System` with the original layout
    // and pointer. The thread-local counters do not allocate.
    unsafe impl std::alloc::GlobalAlloc for TestCountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            record_test_allocation();
            // SAFETY: delegated with the caller-provided valid layout.
            unsafe { std::alloc::System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
            record_test_allocation();
            // SAFETY: delegated with the caller-provided valid layout.
            unsafe { std::alloc::System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            // SAFETY: delegated with the original allocation pointer/layout.
            unsafe { std::alloc::System.dealloc(ptr, layout) };
        }

        unsafe fn realloc(
            &self,
            ptr: *mut u8,
            layout: std::alloc::Layout,
            new_size: usize,
        ) -> *mut u8 {
            record_test_allocation();
            // SAFETY: delegated with the original pointer/layout and requested size.
            unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static TEST_COUNTING_ALLOCATOR: TestCountingAllocator = TestCountingAllocator;

    pub fn begin_test_allocation_count() {
        TEST_ALLOCATION_COUNT.with(|count| count.set(0));
        COUNT_TEST_ALLOCATIONS.with(|enabled| enabled.set(true));
    }

    pub fn finish_test_allocation_count() -> usize {
        COUNT_TEST_ALLOCATIONS.with(|enabled| enabled.set(false));
        TEST_ALLOCATION_COUNT.with(Cell::get)
    }

    static RESERVED_LIFECYCLE_TEST_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    #[test]
    fn shape_digest_is_stable_and_order_sensitive() {
        assert_eq!(shape_digest(&[1, -2, 3]), shape_digest(&[1, -2, 3]));
        assert_ne!(shape_digest(&[1, -2, 3]), shape_digest(&[3, -2, 1]));
    }

    #[test]
    fn pending_relcache_overflow_fails_closed() {
        let mut pending = PendingRelcacheInvalidations::empty();
        for index in 0..PENDING_RELCACHE_CAPACITY {
            pending.relids[index] = u32::try_from(index + 1).expect("fits");
            pending.len += 1;
        }
        assert!(pending.contains(1));
        assert!(!pending.contains(10_000));
    }

    #[test]
    fn new_trigger_scope_is_only_selected_for_unpinned_auto_load() {
        let scope = current_command_scope();
        assert_eq!(
            first_use_scope_for_load(loader::TriggerInstall::New, false, true),
            Some(scope),
        );
        assert_eq!(
            first_use_scope_for_load(loader::TriggerInstall::Existing, false, true),
            None,
        );
        assert_eq!(
            first_use_scope_for_load(loader::TriggerInstall::New, true, true),
            None,
        );
        assert_eq!(
            first_use_scope_for_load(loader::TriggerInstall::New, false, false),
            None,
        );
        assert_eq!(
            first_use_scope_for_load(loader::TriggerInstall::New, true, false),
            None,
        );
    }

    #[test]
    fn pin_intent_authorizes_missing_reload_when_auto_load_is_disabled() {
        assert!(missing_load_is_authorized(false, true, false));
        assert!(missing_load_is_authorized(true, false, false));
        assert!(missing_load_is_authorized(false, false, true));
        assert!(!missing_load_is_authorized(false, false, false));
    }

    #[test]
    fn explicit_load_drains_new_trigger_invalidations_before_continuing() {
        let events = RefCell::new(Vec::new());
        let staged = continue_explicit_load_after_trigger_install(
            loader::TriggerInstall::New,
            || events.borrow_mut().push("drain"),
            || {
                events.borrow_mut().push("stage");
                42
            },
        );
        assert_eq!(staged, 42);
        assert_eq!(*events.borrow(), ["drain", "stage"]);

        events.borrow_mut().clear();
        continue_explicit_load_after_trigger_install(
            loader::TriggerInstall::Existing,
            || events.borrow_mut().push("drain"),
            || events.borrow_mut().push("stage"),
        );
        assert_eq!(*events.borrow(), ["stage"]);
    }

    #[test]
    fn explicit_pin_promotes_existing_scoped_auto_load() {
        let mut store = RelationStore::default();
        let mut auto_loaded = empty_relation(75, false, 1);
        auto_loaded.first_use_scope = Some(CommandScope {
            xid: 4,
            command_id: 9,
        });
        store.entries.push(auto_loaded);

        assert!(store.make_pin_durable(pg_sys::Oid::from(75_u32)));
        assert!(store.entries[0].pinned);
        assert_eq!(store.entries[0].first_use_scope, None);
    }

    #[test]
    fn next_command_prunes_scoped_auto_load_but_preserves_durable_pin() {
        let first_scope = CommandScope {
            xid: 7,
            command_id: 3,
        };
        let mut store = RelationStore::default();
        let mut auto_loaded = empty_relation(80, false, 1);
        auto_loaded.first_use_scope = Some(first_scope);
        store.entries.push(auto_loaded);
        store.entries.push(empty_relation(90, true, 2));

        prune_invalid_relations(
            &mut store,
            false,
            &PendingRelcacheInvalidations::empty(),
            CommandScope {
                xid: first_scope.xid,
                command_id: first_scope.command_id + 1,
            },
            |_| GenerationStamp {
                global: 1,
                relation: 1,
            },
            |relid| Some(pg_sys::Oid::from(u32::from(relid) + 100)),
        );

        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].relid, pg_sys::Oid::from(90_u32));
        assert!(store.entries[0].pinned);
        assert_eq!(store.entries[0].first_use_scope, None);
    }

    #[test]
    fn commit_generation_bump_evicts_snapshot_but_preserves_pin_intent() {
        let relid = pg_sys::Oid::from(95_u32);
        let mut store = RelationStore::default();
        store.pins.insert(
            95,
            PinSpec {
                columns: Vec::new(),
            },
        );
        store.entries.push(empty_relation(95, true, 1));

        prune_invalid_relations(
            &mut store,
            false,
            &PendingRelcacheInvalidations::empty(),
            CommandScope {
                xid: 2,
                command_id: 1,
            },
            |_| GenerationStamp {
                global: 1,
                relation: 2,
            },
            |_| Some(pg_sys::Oid::from(195_u32)),
        );

        assert!(store.entries.is_empty());
        assert!(store.pins.contains_key(&u32::from(relid)));
    }

    #[test]
    fn invalidation_prunes_pending_generation_relfilenode_and_scope_changes() {
        let current_scope = CommandScope {
            xid: 11,
            command_id: 22,
        };
        let mut store = RelationStore::default();
        store.entries.extend(
            [10_u32, 20, 30, 40, 50, 60]
                .into_iter()
                .enumerate()
                .map(|(index, relid)| empty_relation(relid, false, index as u64 + 1)),
        );
        store.entries[4].first_use_scope = Some(CommandScope {
            xid: current_scope.xid,
            command_id: current_scope.command_id + 1,
        });

        let mut pending = PendingRelcacheInvalidations::empty();
        pending.relids[0] = 20;
        pending.len = 1;
        prune_invalid_relations(
            &mut store,
            false,
            &pending,
            current_scope,
            |relid| GenerationStamp {
                global: 1,
                relation: u64::from(u32::from(relid) == 30) + 1,
            },
            |relid| match u32::from(relid) {
                40 => Some(pg_sys::Oid::from(9_999_u32)),
                60 => None,
                raw => Some(pg_sys::Oid::from(raw + 100)),
            },
        );

        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].relid, pg_sys::Oid::from(10_u32));
    }

    #[test]
    fn invalidation_clear_all_does_not_consult_catalog_state() {
        let mut store = RelationStore::default();
        store.entries.push(empty_relation(70, false, 1));
        prune_invalid_relations(
            &mut store,
            true,
            &PendingRelcacheInvalidations::empty(),
            CommandScope {
                xid: 1,
                command_id: 1,
            },
            |_| panic!("clear-all must not read a generation"),
            |_| panic!("clear-all must not read a relfilenode"),
        );
        assert!(store.entries.is_empty());
    }

    #[test]
    fn trigger_contract_rejects_row_and_before_misuse() {
        assert!(
            validate_trigger_contract(
                PgTriggerLevel::Row,
                PgTriggerWhen::After,
                PgTriggerOperation::Insert,
            )
            .is_err()
        );
        assert!(
            validate_trigger_contract(
                PgTriggerLevel::Statement,
                PgTriggerWhen::Before,
                PgTriggerOperation::Update,
            )
            .is_err()
        );
        assert!(
            validate_trigger_contract(
                PgTriggerLevel::Statement,
                PgTriggerWhen::After,
                PgTriggerOperation::Truncate,
            )
            .is_ok()
        );
    }

    struct EmptyArtifact;

    impl DerivedArtifact for EmptyArtifact {
        fn device_bytes(&self) -> u64 {
            0
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            0
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct SizedArtifact(u64);

    impl DerivedArtifact for SizedArtifact {
        fn device_bytes(&self) -> u64 {
            self.0
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            0
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct AccountedArtifact {
        device_bytes: u64,
        retained_host_exact_bytes: u64,
    }

    impl DerivedArtifact for AccountedArtifact {
        fn device_bytes(&self) -> u64 {
            self.device_bytes
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            self.retained_host_exact_bytes
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct AccountedWorkspace {
        device_bytes: u64,
        host_bytes: u64,
    }

    impl StagedTransformWorkspace for AccountedWorkspace {
        fn device_bytes(&self) -> u64 {
            self.device_bytes
        }

        fn host_bytes(&self) -> u64 {
            self.host_bytes
        }
    }

    struct CompositeAccountedArtifact {
        base: AccountedArtifact,
        spatial: AccountedArtifact,
    }

    impl DerivedArtifact for CompositeAccountedArtifact {
        fn device_bytes(&self) -> u64 {
            self.base.device_bytes + self.spatial.device_bytes
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            self.base.retained_host_exact_bytes + self.spatial.retained_host_exact_bytes
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct GeometrySnapshotArtifact {
        snapshot: ResidentGeometryExactSnapshot,
    }

    impl DerivedArtifact for GeometrySnapshotArtifact {
        fn device_bytes(&self) -> u64 {
            0
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            self.snapshot
                .retained_host_bytes()
                .expect("test snapshot byte count")
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct MarkerArtifact(u8);

    impl DerivedArtifact for MarkerArtifact {
        fn device_bytes(&self) -> u64 {
            0
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            0
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct DropTrackedArtifact {
        bytes: u64,
        dropped: Rc<Cell<bool>>,
    }

    impl DerivedArtifact for DropTrackedArtifact {
        fn device_bytes(&self) -> u64 {
            self.bytes
        }

        fn retained_host_exact_bytes(&self) -> u64 {
            0
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl Drop for DropTrackedArtifact {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    fn empty_relation(relid: u32, pinned: bool, last_used_tick: u64) -> ResidentRelation {
        ResidentRelation {
            relid: pg_sys::Oid::from(relid),
            relfilenode: pg_sys::Oid::from(relid + 100),
            generation: GenerationStamp {
                global: 1,
                relation: 1,
            },
            columns: BTreeMap::new(),
            row_count: 0,
            loaded_at_us: 0,
            last_used_us: 0,
            load_ms: 0.0,
            last_used_tick,
            pinned,
            raw_charge: LedgerCharge::reserve(0, 0).expect("zero-byte charge"),
            raw_accounting: ResidentByteAccounting::default(),
            first_use_scope: None,
            derived: Vec::new(),
        }
    }

    fn install_test_relations(relids: &[u32]) {
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            store.entries.clear();
            store.entries.extend(
                relids
                    .iter()
                    .enumerate()
                    .map(|(index, relid)| empty_relation(*relid, false, index as u64 + 1)),
            );
        });
    }

    #[test]
    fn budget_eviction_drops_derived_before_raw_and_respects_pin() {
        let mut store = RelationStore::default();
        let mut old = empty_relation(100, false, 1);
        old.derived.push(DerivedEntry {
            digest: 7,
            canonical_words: Box::default(),
            dependencies: Box::default(),
            artifact: Box::new(EmptyArtifact),
            charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
        });
        store.entries.push(old);
        store.entries.push(empty_relation(200, false, 2));
        store.entries.push(empty_relation(300, true, 0));

        assert_eq!(
            store.evict_one_for_budget(pg_sys::InvalidOid),
            Some(EvictionKind::Derived)
        );
        assert_eq!(store.entries.len(), 3);
        assert!(store.entries[0].derived.is_empty());

        assert_eq!(
            store.evict_one_for_budget(pg_sys::InvalidOid),
            Some(EvictionKind::RawRelation)
        );
        assert_eq!(store.entries.len(), 2);
        assert!(
            store
                .entries
                .iter()
                .any(|entry| entry.relid == pg_sys::Oid::from(300_u32))
        );
    }

    #[test]
    fn budget_eviction_can_reclaim_artifacts_owned_by_pinned_relations() {
        let mut store = RelationStore::default();
        let mut pinned = empty_relation(300, true, 1);
        pinned.derived.push(DerivedEntry {
            digest: 8,
            canonical_words: Box::default(),
            dependencies: Box::default(),
            artifact: Box::new(EmptyArtifact),
            charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
        });
        store.entries.push(pinned);

        assert_eq!(
            store.evict_one_for_budget(pg_sys::InvalidOid),
            Some(EvictionKind::Derived)
        );
        assert_eq!(store.entries.len(), 1);
        assert!(store.entries[0].pinned);
        assert!(store.entries[0].derived.is_empty());
        assert_eq!(
            store.evict_one_for_budget(pg_sys::InvalidOid),
            None,
            "pin protects the raw relation after its artifact is reclaimed"
        );
    }

    #[test]
    fn planner_budget_projection_counts_only_surviving_local_bytes() {
        let selected = BTreeSet::from([10_u32]);
        let (local_total, pinned_unselected) = summarize_local_budget_bytes(
            [
                (10, true, 100, 7),
                (20, true, 200, 11),
                (30, false, 300, 13),
            ],
            &selected,
        )
        .expect("byte summary fits");
        assert_eq!(local_total, 631);
        assert_eq!(pinned_unselected, 200);

        let snapshot = ResidentBudgetSnapshot {
            cluster_live_bytes: 1_631,
            current_backend_live_bytes: local_total,
            other_backend_live_bytes: 1_000,
            pinned_unselected_raw_bytes: pinned_unselected,
            evictable_or_replaced_local_bytes: local_total - pinned_unselected,
        };
        assert_eq!(snapshot.evictable_or_replaced_local_bytes, 431);
        assert_eq!(snapshot.projected_final_bytes(150, 25), Some(1_375));
    }

    #[test]
    fn planner_budget_projection_fails_closed_on_overflow() {
        assert_eq!(
            summarize_local_budget_bytes([(10, false, u64::MAX, 1)], &BTreeSet::new()),
            None
        );
        let snapshot = ResidentBudgetSnapshot {
            cluster_live_bytes: u64::MAX,
            current_backend_live_bytes: 0,
            other_backend_live_bytes: u64::MAX,
            pinned_unselected_raw_bytes: 0,
            evictable_or_replaced_local_bytes: 0,
        };
        assert_eq!(snapshot.projected_final_bytes(1, 0), None);
    }

    #[test]
    fn exact_identity_disambiguates_digest_collisions() {
        let first = DerivedArtifactIdentity {
            digest: 77,
            canonical_words: vec![1, 2, 3].into_boxed_slice(),
        };
        let collision = DerivedArtifactIdentity {
            digest: 77,
            canonical_words: vec![1, 2, 4].into_boxed_slice(),
        };
        let entry = DerivedEntry {
            digest: 77,
            canonical_words: first.canonical_words.clone(),
            dependencies: Box::default(),
            artifact: Box::new(EmptyArtifact),
            charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
        };

        assert!(entry.has_identity(&first));
        assert!(!entry.has_identity(&collision));
    }

    #[test]
    fn dependency_stamps_detect_non_owner_changes() {
        let mut store = RelationStore::default();
        let owner = empty_relation(700, false, 1);
        let dimension = empty_relation(800, false, 2);
        let expected = [
            ResidentDependencyStamp::from_relation(&owner),
            ResidentDependencyStamp::from_relation(&dimension),
        ];
        store.entries.push(owner);
        store.entries.push(dimension);
        assert_eq!(first_dependency_mismatch(&store, &expected), None);

        store.entries[1].generation.relation += 1;
        assert_eq!(
            first_dependency_mismatch(&store, &expected),
            Some(pg_sys::Oid::from(800_u32))
        );
        store.entries[1].generation.relation -= 1;
        store.entries[1].relfilenode = pg_sys::Oid::from(9_999_u32);
        assert_eq!(
            first_dependency_mismatch(&store, &expected),
            Some(pg_sys::Oid::from(800_u32))
        );
    }

    #[test]
    fn protected_raw_dependencies_can_release_derived_artifacts() {
        let mut store = RelationStore::default();
        let mut protected_owner = empty_relation(900, false, 1);
        protected_owner.derived.push(DerivedEntry {
            digest: 1,
            canonical_words: vec![1].into_boxed_slice(),
            dependencies: Box::default(),
            artifact: Box::new(EmptyArtifact),
            charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
        });
        let mut evictable = empty_relation(901, false, 2);
        evictable.derived.push(DerivedEntry {
            digest: 2,
            canonical_words: vec![2].into_boxed_slice(),
            dependencies: Box::default(),
            artifact: Box::new(EmptyArtifact),
            charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
        });
        store.entries.extend([protected_owner, evictable]);

        let protected = BTreeSet::from([900_u32]);
        assert_eq!(
            store.evict_one_for_budget_excluding(&protected),
            Some(EvictionKind::Derived)
        );
        assert!(store.entries[0].derived.is_empty());
        assert_eq!(store.entries[1].derived.len(), 1);
        assert_eq!(store.entries[0].relid, pg_sys::Oid::from(900_u32));
    }

    #[test]
    fn finite_budget_replaces_owner_artifact_without_evicting_raw_dependencies() {
        let owner = pg_sys::Oid::from(910_u32);
        let dimension_a = pg_sys::Oid::from(911_u32);
        let dimension_b = pg_sys::Oid::from(912_u32);
        let evictable = pg_sys::Oid::from(913_u32);
        install_test_relations(&[910, 911, 912, 913]);
        STORE.with(|store| {
            for (index, relation) in store.borrow_mut().entries.iter_mut().enumerate() {
                relation.derived.push(DerivedEntry {
                    digest: 100 + index as u64,
                    canonical_words: vec![index as i32 + 1].into_boxed_slice(),
                    dependencies: Box::default(),
                    artifact: Box::new(SizedArtifact(8)),
                    charge: LedgerCharge::reserve(0, 0).expect("zero-byte artifact charge"),
                });
            }
        });
        let protected =
            canonical_dependency_relids(owner, &[dimension_a, dimension_b], &[]).expect("valid");
        let live = Cell::new(32_u64);
        let attempts = Cell::new(0_u32);
        let evicted = Cell::new(None);

        reserve_with_eviction(
            8,
            32,
            |requested, budget| {
                attempts.set(attempts.get() + 1);
                let next = live.get().checked_add(requested);
                if next.is_some_and(|next| next <= budget) {
                    live.set(next.expect("checked above"));
                    Ok(())
                } else {
                    Err(live.get())
                }
            },
            || {
                let outcome = STORE.with(|store| {
                    store
                        .borrow_mut()
                        .evict_one_for_budget_excluding(&protected)
                });
                evicted.set(outcome);
                if outcome.is_some() {
                    live.set(live.get().checked_sub(8).expect("modeled charge exists"));
                    true
                } else {
                    false
                }
            },
        )
        .expect("eviction makes room within the finite budget");

        assert_eq!(attempts.get(), 2);
        assert_eq!(evicted.get(), Some(EvictionKind::Derived));
        assert_eq!(live.get(), 32);
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            for relid in [owner, dimension_a, dimension_b] {
                assert!(store.entries.iter().any(|entry| entry.relid == relid));
            }
            let owner_entry = store
                .entries
                .iter_mut()
                .find(|entry| entry.relid == owner)
                .expect("protected owner remains");
            assert!(owner_entry.derived.is_empty());
            owner_entry.derived.push(DerivedEntry {
                digest: 999,
                canonical_words: vec![99].into_boxed_slice(),
                dependencies: Box::default(),
                artifact: Box::new(SizedArtifact(8)),
                charge: LedgerCharge::reserve(0, 0).expect("modeled replacement charge"),
            });
            assert_eq!(owner_entry.derived.len(), 1);
            assert!(store.entries.iter().any(|entry| entry.relid == evictable));
        });
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn transform_charges_evict_without_removing_protected_dependencies_and_release_on_failure() {
        struct ModeledCharge {
            bytes: u64,
            live: Rc<Cell<u64>>,
        }

        impl Drop for ModeledCharge {
            fn drop(&mut self) {
                self.live.set(
                    self.live
                        .get()
                        .checked_sub(self.bytes)
                        .expect("modeled charge is live"),
                );
            }
        }

        let owner = pg_sys::Oid::from(914_u32);
        let dimension = pg_sys::Oid::from(915_u32);
        let unrelated = pg_sys::Oid::from(916_u32);
        install_test_relations(&[914, 915, 916]);
        STORE.with(|store| {
            for (index, relation) in store.borrow_mut().entries.iter_mut().enumerate() {
                relation.derived.push(DerivedEntry {
                    digest: 200 + index as u64,
                    canonical_words: vec![index as i32 + 1].into_boxed_slice(),
                    dependencies: Box::default(),
                    artifact: Box::new(SizedArtifact(8)),
                    charge: LedgerCharge::reserve(0, 0).expect("modeled artifact charge"),
                });
            }
        });
        let protected = canonical_dependency_relids(owner, &[dimension], &[]).expect("valid");
        let live = Rc::new(Cell::new(24_u64));
        let reserve_live = Rc::clone(&live);
        let evict_live = Rc::clone(&live);
        let charges = reserve_transform_charges(
            8,
            8,
            32,
            move |requested, budget| {
                let Some(next) = reserve_live.get().checked_add(requested) else {
                    return Err(reserve_live.get());
                };
                if next > budget {
                    return Err(reserve_live.get());
                }
                reserve_live.set(next);
                Ok(ModeledCharge {
                    bytes: requested,
                    live: Rc::clone(&reserve_live),
                })
            },
            || {
                let evicted = STORE.with(|store| {
                    store
                        .borrow_mut()
                        .evict_one_for_budget_excluding(&protected)
                        .is_some()
                });
                if evicted {
                    evict_live.set(
                        evict_live
                            .get()
                            .checked_sub(8)
                            .expect("modeled evicted charge is live"),
                    );
                }
                evicted
            },
        )
        .expect("one eviction makes room for both transform charges");
        assert_eq!(live.get(), 32);
        STORE.with(|store| {
            let store = store.borrow();
            assert!(store.entries.iter().any(|entry| entry.relid == owner));
            assert!(store.entries.iter().any(|entry| entry.relid == dimension));
            assert!(store.entries.iter().any(|entry| entry.relid == unrelated));
            assert_eq!(
                store
                    .entries
                    .iter()
                    .map(|entry| entry.derived.len())
                    .sum::<usize>(),
                2
            );
        });
        drop(charges);
        assert_eq!(live.get(), 16);
        STORE.with(|store| store.borrow_mut().entries.clear());

        let failed_live = Rc::new(Cell::new(0_u64));
        let failed_reserve_live = Rc::clone(&failed_live);
        let result = reserve_transform_charges(
            8,
            1,
            8,
            move |requested, budget| {
                let next = failed_reserve_live
                    .get()
                    .checked_add(requested)
                    .ok_or(failed_reserve_live.get())?;
                if next > budget {
                    return Err(failed_reserve_live.get());
                }
                failed_reserve_live.set(next);
                Ok(ModeledCharge {
                    bytes: requested,
                    live: Rc::clone(&failed_reserve_live),
                })
            },
            || false,
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::BudgetExceeded {
                requested: 1,
                live: 8,
                budget: 8,
            })
        ));
        assert_eq!(failed_live.get(), 0);
    }

    #[test]
    fn finite_budget_batch_keeps_the_fact_or_returns_budget_exceeded() {
        let fact = pg_sys::Oid::from(920_u32);
        let dimension = pg_sys::Oid::from(921_u32);
        let unrelated = pg_sys::Oid::from(922_u32);
        let requests = [
            SelectedRelation {
                relid: fact,
                columns: vec![1],
            },
            SelectedRelation {
                relid: dimension,
                columns: vec![1],
            },
        ];
        let protected = selected_relation_relids(&requests);

        // When an unrelated entry can make room, it is evicted and the
        // earlier fact remains available for dimension preparation.
        install_test_relations(&[920, 922]);
        let live = Cell::new(16_u64);
        reserve_with_eviction(
            8,
            16,
            |requested, budget| {
                let next = live.get().checked_add(requested);
                if next.is_some_and(|next| next <= budget) {
                    live.set(next.expect("checked above"));
                    Ok(())
                } else {
                    Err(live.get())
                }
            },
            || {
                let evicted = STORE.with(|store| {
                    store
                        .borrow_mut()
                        .evict_one_for_budget_excluding(&protected)
                        .is_some()
                });
                if evicted {
                    live.set(live.get().checked_sub(8).expect("modeled charge exists"));
                }
                evicted
            },
        )
        .expect("unrelated relation makes room");
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            assert!(store.entries.iter().any(|entry| entry.relid == fact));
            assert!(!store.entries.iter().any(|entry| entry.relid == unrelated));
            store.entries.push(empty_relation(921, false, 3));
            assert!(
                [fact, dimension]
                    .into_iter()
                    .all(|relid| store.entries.iter().any(|entry| entry.relid == relid))
            );
            store.entries.clear();
        });

        // With no unprotected entry, fail before evicting the already loaded
        // fact. The caller receives the budget error instead of a later
        // MissingRelation during artifact preparation.
        install_test_relations(&[920]);
        let result = reserve_with_eviction(
            8,
            8,
            |_, _| Err::<(), _>(8),
            || {
                STORE.with(|store| {
                    store
                        .borrow_mut()
                        .evict_one_for_budget_excluding(&protected)
                        .is_some()
                })
            },
        );
        assert_eq!(
            result,
            Err(ResidentLoadError::BudgetExceeded {
                requested: 8,
                live: 8,
                budget: 8,
            })
        );
        STORE.with(|store| {
            let store = store.borrow();
            assert_eq!(store.entries.len(), 1);
            assert_eq!(store.entries[0].relid, fact);
        });
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn forced_digest_collisions_build_and_resolve_distinct_artifacts() {
        install_test_relations(&[1_000]);
        let owner = pg_sys::Oid::from(1_000_u32);
        let first = DerivedArtifactIdentity {
            digest: 88,
            canonical_words: vec![1, 2].into_boxed_slice(),
        };
        let second = DerivedArtifactIdentity {
            digest: 88,
            canonical_words: vec![1, 3].into_boxed_slice(),
        };
        for (marker, identity) in [(1_u8, &first), (2, &second)] {
            assert_eq!(
                ensure_derived_artifact(
                    owner,
                    identity,
                    &[owner],
                    &[],
                    |_| {
                        Ok(PreparedDerived {
                            prepared: (),
                            device_bytes: 0,
                        })
                    },
                    |()| Ok(MarkerArtifact(marker)),
                )
                .expect("artifact build succeeds"),
                ArtifactEnsureOutcome::Built
            );
        }
        STORE.with(|store| {
            let store = store.borrow();
            assert_eq!(store.entries[0].derived.len(), 2);
            assert!(store.entries[0].derived[0].has_identity(&first));
            assert!(store.entries[0].derived[1].has_identity(&second));
        });
        assert_eq!(
            with_derived_artifact_inputs::<MarkerArtifact, _>(owner, &first, &[], |resolved| {
                resolved.artifact.0
            })
            .expect("first collision resolves"),
            1
        );
        assert_eq!(
            with_derived_artifact_inputs::<MarkerArtifact, _>(owner, &second, &[], |resolved| {
                resolved.artifact.0
            })
            .expect("second collision resolves"),
            2
        );
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn artifact_hit_requires_the_exact_requested_dependency_set() {
        install_test_relations(&[1_010, 1_011, 1_012]);
        let owner = pg_sys::Oid::from(1_010_u32);
        let dimension_a = pg_sys::Oid::from(1_011_u32);
        let dimension_b = pg_sys::Oid::from(1_012_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![5, 4, 3]);
        assert_eq!(
            ensure_derived_artifact(
                owner,
                &identity,
                &[owner, dimension_a],
                &[],
                |_| Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 0,
                }),
                |()| Ok(MarkerArtifact(1)),
            )
            .expect("first artifact builds"),
            ArtifactEnsureOutcome::Built
        );
        assert_eq!(
            ensure_derived_artifact(
                owner,
                &identity,
                &[dimension_b, owner],
                &[],
                |_| Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 0,
                }),
                |()| Ok(MarkerArtifact(2)),
            )
            .expect("different dependency set rebuilds"),
            ArtifactEnsureOutcome::Rebuilt
        );

        STORE.with(|store| {
            let store = store.borrow();
            let entry = &store.entries[0].derived[0];
            assert_eq!(store.entries[0].derived.len(), 1);
            assert!(dependency_relids_match(
                &entry.dependencies,
                &BTreeSet::from([u32::from(owner), u32::from(dimension_b)])
            ));
        });
        assert_eq!(
            with_derived_artifact_inputs::<MarkerArtifact, _>(owner, &identity, &[], |resolved| {
                resolved.artifact.0
            })
            .expect("replacement resolves"),
            2
        );
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn byte_mismatch_drops_artifact_and_reserved_charge() {
        install_test_relations(&[1_100]);
        let owner = pg_sys::Oid::from(1_100_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 1]);
        let before = ledger::total_bytes();
        let result = ensure_derived_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |_| {
                Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 16,
                })
            },
            |()| {
                assert!(ledger::total_bytes() >= before.saturating_add(16));
                Ok(SizedArtifact(8))
            },
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactAccountingMismatch {
                declared: ResidentByteAccounting {
                    device_bytes: 16,
                    retained_host_exact_bytes: 0,
                },
                actual: ResidentByteAccounting {
                    device_bytes: 8,
                    retained_host_exact_bytes: 0,
                },
            })
        ));
        assert_eq!(ledger::total_bytes(), before);
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn host_retaining_artifacts_require_the_reserve_first_builder() {
        install_test_relations(&[1_125]);
        let owner = pg_sys::Oid::from(1_125_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 12]);
        let result = ensure_derived_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |_| {
                Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 8,
                })
            },
            |()| {
                Ok(AccountedArtifact {
                    device_bytes: 8,
                    retained_host_exact_bytes: 5,
                })
            },
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactAccountingMismatch {
                declared: ResidentByteAccounting {
                    device_bytes: 8,
                    retained_host_exact_bytes: 0
                },
                actual: ResidentByteAccounting {
                    device_bytes: 8,
                    retained_host_exact_bytes: 5
                }
            })
        ));
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn reserve_first_device_build_charges_device_and_retained_host_bytes() {
        install_test_relations(&[1_150]);
        let owner = pg_sys::Oid::from(1_150_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 15]);
        let accounting = ResidentByteAccounting {
            device_bytes: 16,
            retained_host_exact_bytes: 7,
        };
        let outcome =
            ensure_device_derived_artifact(owner, &identity, &[owner], &[], accounting, |inputs| {
                assert!(inputs.columns.is_empty());
                assert_eq!(inputs.evidence.len(), 1);
                Ok(AccountedArtifact {
                    device_bytes: 16,
                    retained_host_exact_bytes: 7,
                })
            })
            .expect("reserve-first build succeeds");
        assert_eq!(outcome, ArtifactEnsureOutcome::Built);
        STORE.with(|store| assert_eq!(store.borrow().entries[0].derived[0].bytes(), 23));
        with_derived_artifact_inputs::<AccountedArtifact, _>(owner, &identity, &[], |resolved| {
            assert_eq!(resolved.device_bytes, 16);
            assert_eq!(resolved.accounting, accounting);
        })
        .expect("accounted artifact resolves");
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn staged_device_lifecycle_prepares_only_on_miss_and_outside_raw_dispatch_borrow() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_151]);
        let owner = pg_sys::Oid::from(1_151_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 151]);
        let accounting = ResidentByteAccounting {
            device_bytes: 11,
            retained_host_exact_bytes: 13,
        };
        let before = ledger::total_bytes();
        let preflight_calls = Cell::new(0_u32);
        let snapshot_calls = Cell::new(0_u32);
        let build_calls = Cell::new(0_u32);
        let dispatch_calls = Cell::new(0_u32);
        let finalize_calls = Cell::new(0_u32);
        let build = || {
            ensure_staged_device_derived_artifact(
                owner,
                &identity,
                &[owner],
                &[],
                |inputs| {
                    preflight_calls.set(preflight_calls.get() + 1);
                    assert_eq!(inputs.evidence.len(), 1);
                    Ok(StagedDerivedPreflight {
                        prepared: (),
                        accounting,
                    })
                },
                |(), inputs| {
                    snapshot_calls.set(snapshot_calls.get() + 1);
                    assert_eq!(inputs.evidence.len(), 1);
                    assert!(ledger::total_bytes() >= before.saturating_add(24));
                    Ok(())
                },
                |()| {
                    build_calls.set(build_calls.get() + 1);
                    STORE.with(|store| {
                        let _borrow = store
                            .try_borrow_mut()
                            .expect("composite build must run outside the STORE borrow");
                    });
                    Ok(AccountedArtifact {
                        device_bytes: 11,
                        retained_host_exact_bytes: 13,
                    })
                },
                |artifact, inputs| {
                    dispatch_calls.set(dispatch_calls.get() + 1);
                    assert_eq!(inputs.dependency_count(), 1);
                    assert_eq!(inputs.column_count(), 0);
                    assert_eq!(inputs.evidence(0)?.relid, owner);
                    assert_eq!(artifact.device_bytes, 11);
                    assert!(STORE.with(|store| store.try_borrow_mut().is_err()));
                    Ok(())
                },
                |artifact| {
                    finalize_calls.set(finalize_calls.get() + 1);
                    assert_eq!(artifact.device_bytes, 11);
                    STORE.with(|store| {
                        let _borrow = store
                            .try_borrow_mut()
                            .expect("finalize must run outside the STORE borrow");
                    });
                    Ok(())
                },
            )
        };

        assert_eq!(
            build().expect("first lifecycle build"),
            ArtifactEnsureOutcome::Built
        );
        assert_eq!(
            build().expect("second lifecycle hit"),
            ArtifactEnsureOutcome::Hit
        );
        assert_eq!(preflight_calls.get(), 1);
        assert_eq!(snapshot_calls.get(), 1);
        assert_eq!(build_calls.get(), 1);
        assert_eq!(dispatch_calls.get(), 1);
        assert_eq!(finalize_calls.get(), 1);
        STORE.with(|store| store.borrow_mut().entries.clear());
        assert_eq!(ledger::total_bytes(), before);
    }

    #[test]
    fn staged_transform_lifecycle_holds_distinct_exact_charges_and_publishes_only_persistent() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_164]);
        let owner = pg_sys::Oid::from(1_164_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 164]);
        let published = ResidentByteAccounting {
            device_bytes: 7,
            retained_host_exact_bytes: 11,
        };
        let transient = ResidentByteAccounting {
            device_bytes: 13,
            retained_host_exact_bytes: 17,
        };
        let before = ledger::total_bytes();
        let preflight_calls = Cell::new(0_u32);
        let snapshot_calls = Cell::new(0_u32);
        let build_calls = Cell::new(0_u32);
        let dispatch_calls = Cell::new(0_u32);
        let finalize_calls = Cell::new(0_u32);
        let build = || {
            ensure_staged_device_transform_artifact(
                owner,
                &identity,
                &[owner],
                &[],
                |inputs| {
                    preflight_calls.set(preflight_calls.get() + 1);
                    assert_eq!(inputs.evidence.len(), 1);
                    assert!(STORE.with(|store| store.try_borrow_mut().is_err()));
                    Ok(StagedTransformPreflight {
                        prepared: (),
                        published_accounting: published,
                        transient_accounting: transient,
                    })
                },
                |(), inputs| {
                    snapshot_calls.set(snapshot_calls.get() + 1);
                    assert_eq!(inputs.evidence.len(), 1);
                    assert_eq!(ledger::total_bytes(), before.saturating_add(48));
                    assert!(STORE.with(|store| store.try_borrow_mut().is_err()));
                    Ok(())
                },
                |()| {
                    build_calls.set(build_calls.get() + 1);
                    assert_eq!(ledger::total_bytes(), before.saturating_add(48));
                    STORE.with(|store| {
                        let _borrow = store
                            .try_borrow_mut()
                            .expect("workspace build must run outside STORE borrow");
                    });
                    Ok(AccountedWorkspace {
                        device_bytes: 13,
                        host_bytes: 17,
                    })
                },
                |workspace, inputs| {
                    dispatch_calls.set(dispatch_calls.get() + 1);
                    assert_eq!(workspace.device_bytes, 13);
                    assert_eq!(inputs.dependency_count(), 1);
                    assert_eq!(ledger::total_bytes(), before.saturating_add(48));
                    assert!(STORE.with(|store| store.try_borrow_mut().is_err()));
                    Ok(())
                },
                |workspace| {
                    finalize_calls.set(finalize_calls.get() + 1);
                    assert_eq!(workspace.host_bytes, 17);
                    assert_eq!(ledger::total_bytes(), before.saturating_add(48));
                    STORE.with(|store| {
                        let _borrow = store
                            .try_borrow_mut()
                            .expect("transform finalize must run outside STORE borrow");
                    });
                    Ok(AccountedArtifact {
                        device_bytes: 7,
                        retained_host_exact_bytes: 11,
                    })
                },
            )
        };

        assert_eq!(
            build().expect("first transform build"),
            ArtifactEnsureOutcome::Built
        );
        assert_eq!(ledger::total_bytes(), before.saturating_add(18));
        assert_eq!(
            build().expect("second transform hit"),
            ArtifactEnsureOutcome::Hit
        );
        assert_eq!(preflight_calls.get(), 1);
        assert_eq!(snapshot_calls.get(), 1);
        assert_eq!(build_calls.get(), 1);
        assert_eq!(dispatch_calls.get(), 1);
        assert_eq!(finalize_calls.get(), 1);
        STORE.with(|store| store.borrow_mut().entries[0].generation.relation += 1);
        assert_eq!(
            build().expect("stale transform rebuild"),
            ArtifactEnsureOutcome::Rebuilt
        );
        assert_eq!(ledger::total_bytes(), before.saturating_add(18));
        assert_eq!(preflight_calls.get(), 2);
        assert_eq!(snapshot_calls.get(), 2);
        assert_eq!(build_calls.get(), 2);
        assert_eq!(dispatch_calls.get(), 2);
        assert_eq!(finalize_calls.get(), 2);
        STORE.with(|store| {
            let store = store.borrow();
            assert_eq!(store.entries[0].derived.len(), 1);
            assert_eq!(store.entries[0].derived[0].bytes(), 18);
        });
        STORE.with(|store| store.borrow_mut().entries.clear());
        assert_eq!(ledger::total_bytes(), before);
    }

    #[test]
    fn staged_transform_failures_release_both_charges_without_publication() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");

        fn assert_clean(owner_raw: u32, identity_word: i32, fail_stage: u8) {
            install_test_relations(&[owner_raw]);
            let owner = pg_sys::Oid::from(owner_raw);
            let identity = DerivedArtifactIdentity::from_canonical_words(vec![identity_word]);
            let before = ledger::total_bytes();
            let snapshot_called = Cell::new(false);
            let build_called = Cell::new(false);
            let dispatch_called = Cell::new(false);
            let finalize_called = Cell::new(false);
            let result = ensure_staged_device_transform_artifact(
                owner,
                &identity,
                &[owner],
                &[],
                |_| {
                    if fail_stage == 6 {
                        schedule_test_generation_bump(owner);
                    }
                    Ok(StagedTransformPreflight {
                        prepared: (),
                        published_accounting: ResidentByteAccounting {
                            device_bytes: 17,
                            retained_host_exact_bytes: 19,
                        },
                        transient_accounting: ResidentByteAccounting {
                            device_bytes: 23,
                            retained_host_exact_bytes: 29,
                        },
                    })
                },
                |(), _| {
                    snapshot_called.set(true);
                    if fail_stage == 0 {
                        Err(ResidentLoadError::Loader("snapshot failure".to_owned()))
                    } else {
                        if fail_stage == 7 {
                            schedule_test_generation_bump(owner);
                        }
                        Ok(())
                    }
                },
                |()| {
                    build_called.set(true);
                    if fail_stage == 1 {
                        Err(ResidentLoadError::Loader("workspace failure".to_owned()))
                    } else {
                        Ok(AccountedWorkspace {
                            device_bytes: if fail_stage == 2 { 24 } else { 23 },
                            host_bytes: 29,
                        })
                    }
                },
                |_, _| {
                    dispatch_called.set(true);
                    if fail_stage == 3 {
                        Err(ResidentLoadError::Loader("dispatch failure".to_owned()))
                    } else {
                        if fail_stage == 8 {
                            schedule_test_generation_bump(owner);
                        }
                        Ok(())
                    }
                },
                |_| {
                    finalize_called.set(true);
                    if fail_stage == 4 {
                        return Err(ResidentLoadError::Loader("finalize failure".to_owned()));
                    }
                    Ok(AccountedArtifact {
                        device_bytes: if fail_stage == 5 { 18 } else { 17 },
                        retained_host_exact_bytes: 19,
                    })
                },
            );
            match fail_stage {
                0 | 1 | 3 | 4 => {
                    assert!(matches!(result, Err(ResidentLoadError::Loader(_))));
                }
                2 => assert!(matches!(
                    result,
                    Err(ResidentLoadError::TransformWorkspaceAccountingMismatch { .. })
                )),
                5 => assert!(matches!(
                    result,
                    Err(ResidentLoadError::ArtifactAccountingMismatch { .. })
                )),
                6..=8 => assert!(matches!(
                    result,
                    Err(ResidentLoadError::ArtifactDependencyChanged { relid }) if relid == owner
                )),
                _ => unreachable!("unknown failure stage"),
            }
            if matches!(fail_stage, 2 | 6 | 7) {
                assert!(!dispatch_called.get());
            }
            if fail_stage == 6 {
                assert!(!snapshot_called.get());
                assert!(!build_called.get());
                assert!(!finalize_called.get());
            } else if fail_stage == 7 {
                assert!(snapshot_called.get());
                assert!(build_called.get());
                assert!(!finalize_called.get());
            } else if fail_stage == 8 {
                assert!(snapshot_called.get());
                assert!(build_called.get());
                assert!(dispatch_called.get());
                assert!(finalize_called.get());
            }
            assert_eq!(ledger::total_bytes(), before);
            STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
            STORE.with(|store| store.borrow_mut().entries.clear());
        }

        assert_clean(1_165, 165, 0);
        assert_clean(1_166, 166, 1);
        assert_clean(1_167, 167, 2);
        assert_clean(1_168, 168, 3);
        assert_clean(1_169, 169, 4);
        assert_clean(1_170, 170, 5);
        assert_clean(1_171, 171, 6);
        assert_clean(1_172, 172, 7);
        assert_clean(1_173, 173, 8);
    }

    #[test]
    fn staged_transform_accounting_overflow_prevents_reservation_and_callbacks() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_174]);
        let owner = pg_sys::Oid::from(1_174_u32);
        let before = ledger::total_bytes();
        let snapshot_called = Cell::new(false);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![174, 1]);
        let result = ensure_staged_device_transform_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |_| {
                Ok(StagedTransformPreflight {
                    prepared: (),
                    published_accounting: ResidentByteAccounting {
                        device_bytes: u64::MAX,
                        retained_host_exact_bytes: 0,
                    },
                    transient_accounting: ResidentByteAccounting {
                        device_bytes: 1,
                        retained_host_exact_bytes: 0,
                    },
                })
            },
            |(), _| {
                snapshot_called.set(true);
                Ok(())
            },
            |()| {
                Ok(AccountedWorkspace {
                    device_bytes: 1,
                    host_bytes: 0,
                })
            },
            |_, _| Ok(()),
            |_| Ok(EmptyArtifact),
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactAccountingOverflow)
        ));
        assert!(!snapshot_called.get());
        assert_eq!(ledger::total_bytes(), before);
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));

        let identity = DerivedArtifactIdentity::from_canonical_words(vec![174, 2]);
        let result = ensure_staged_device_transform_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |_| {
                Ok(StagedTransformPreflight {
                    prepared: (),
                    published_accounting: ResidentByteAccounting::default(),
                    transient_accounting: ResidentByteAccounting {
                        device_bytes: u64::MAX,
                        retained_host_exact_bytes: 1,
                    },
                })
            },
            |(), _| Ok(()),
            |()| {
                Ok(AccountedWorkspace {
                    device_bytes: 0,
                    host_bytes: 0,
                })
            },
            |_, _| Ok(()),
            |_| Ok(EmptyArtifact),
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactAccountingOverflow)
        ));
        assert_eq!(ledger::total_bytes(), before);
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn dispatch_resolver_valid_and_error_paths_allocate_nothing() {
        let requested = ResidentColumnRef {
            relid: pg_sys::Oid::from(1_163_u32),
            attno: 4,
        };
        let mut store = RelationStore::default();
        let mut relation = empty_relation(1_163, false, 1);
        relation.columns.insert(
            requested.attno,
            ResidentColumn::Empty {
                type_oid: pg_sys::Oid::from(23_u32),
            },
        );
        let dependency = ResidentDependencyStamp::from_relation(&relation);
        store.entries.push(relation);
        let columns = [requested];
        let dependencies = [dependency];
        let resolver = ResidentDispatchBundle {
            store: &store,
            columns: &columns,
            dependencies: &dependencies,
        };

        begin_test_allocation_count();
        let valid_column = matches!(resolver.column(0), Ok(ResidentColumnView::Empty { .. }));
        let valid_evidence = resolver
            .evidence(0)
            .is_ok_and(|evidence| evidence.relid == requested.relid);
        let invalid_column_index = matches!(
            resolver.column(1),
            Err(ResidentLoadError::DispatchColumnIndexOutOfBounds {
                index: 1,
                column_count: 1,
            })
        );
        let invalid_dependency_index = matches!(
            resolver.evidence(1),
            Err(ResidentLoadError::DispatchDependencyIndexOutOfBounds {
                index: 1,
                dependency_count: 1,
            })
        );
        let unrequested_column = matches!(
            resolver.find_column(ResidentColumnRef {
                relid: requested.relid,
                attno: 5,
            }),
            Err(ResidentLoadError::MissingColumn { attno: 5, .. })
        );
        let unrequested_relation = matches!(
            resolver.evidence_for(pg_sys::Oid::from(9_999_u32)),
            Err(ResidentLoadError::MissingRelation(relid))
                if relid == pg_sys::Oid::from(9_999_u32)
        );
        let allocation_count = finish_test_allocation_count();

        assert!(valid_column);
        assert!(valid_evidence);
        assert!(invalid_column_index);
        assert!(invalid_dependency_index);
        assert!(unrequested_column);
        assert!(unrequested_relation);
        assert_eq!(allocation_count, 0);
    }

    #[test]
    fn staged_device_lifecycle_rejects_dispatch_generation_mismatch() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_152]);
        let owner = pg_sys::Oid::from(1_152_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 152]);
        let dispatch_called = Cell::new(false);
        let result = ensure_staged_device_derived_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |inputs| {
                Ok(StagedDerivedPreflight {
                    prepared: inputs.evidence[0],
                    accounting: ResidentByteAccounting {
                        device_bytes: 0,
                        retained_host_exact_bytes: 0,
                    },
                })
            },
            |snapshot_evidence, inputs| {
                assert_eq!(snapshot_evidence, inputs.evidence[0]);
                Ok(())
            },
            |()| {
                STORE.with(|store| store.borrow_mut().entries[0].generation.relation += 1);
                Ok(EmptyArtifact)
            },
            |_, _| {
                dispatch_called.set(true);
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactDependencyChanged { relid }) if relid == owner
        ));
        assert!(!dispatch_called.get());
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn staged_device_lifecycle_failures_release_charge_without_publication() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        fn assert_clean(owner_raw: u32, identity_word: i32, fail_stage: u8) {
            install_test_relations(&[owner_raw]);
            let owner = pg_sys::Oid::from(owner_raw);
            let identity = DerivedArtifactIdentity::from_canonical_words(vec![identity_word]);
            let before = ledger::total_bytes();
            let result = ensure_staged_device_derived_artifact(
                owner,
                &identity,
                &[owner],
                &[],
                |_| {
                    Ok(StagedDerivedPreflight {
                        prepared: (),
                        accounting: ResidentByteAccounting {
                            device_bytes: 17,
                            retained_host_exact_bytes: 19,
                        },
                    })
                },
                |(), _| {
                    if fail_stage == 0 {
                        Err(ResidentLoadError::Loader("prepare failure".to_owned()))
                    } else {
                        Ok(())
                    }
                },
                |()| {
                    if fail_stage == 1 {
                        Err(ResidentLoadError::Loader("build failure".to_owned()))
                    } else {
                        Ok(AccountedArtifact {
                            device_bytes: 17,
                            retained_host_exact_bytes: 19,
                        })
                    }
                },
                |_, _| {
                    if fail_stage == 2 {
                        Err(ResidentLoadError::Loader("dispatch failure".to_owned()))
                    } else {
                        Ok(())
                    }
                },
                |_| {
                    if fail_stage == 3 {
                        Err(ResidentLoadError::Loader("finalize failure".to_owned()))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(matches!(result, Err(ResidentLoadError::Loader(_))));
            assert_eq!(ledger::total_bytes(), before);
            STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
            STORE.with(|store| store.borrow_mut().entries.clear());
        }

        assert_clean(1_153, 153, 0);
        assert_clean(1_154, 154, 1);
        assert_clean(1_155, 155, 2);
        assert_clean(1_158, 158, 3);
    }

    #[test]
    fn dependency_change_during_finalize_prevents_reserved_publication() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_156]);
        let owner = pg_sys::Oid::from(1_156_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![156]);
        let before = ledger::total_bytes();
        let result = ensure_staged_device_derived_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            |_| {
                Ok(StagedDerivedPreflight {
                    prepared: (),
                    accounting: ResidentByteAccounting {
                        device_bytes: 7,
                        retained_host_exact_bytes: 5,
                    },
                })
            },
            |(), _| Ok(()),
            |()| {
                Ok(AccountedArtifact {
                    device_bytes: 7,
                    retained_host_exact_bytes: 5,
                })
            },
            |_, _| Ok(()),
            |_| {
                STORE.with(|store| store.borrow_mut().entries[0].generation.relation += 1);
                Ok(())
            },
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactDependencyChanged { relid }) if relid == owner
        ));
        assert_eq!(ledger::total_bytes(), before);
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn staged_composite_accounting_matches_device_and_host_categories_exactly() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        install_test_relations(&[1_159]);
        let owner = pg_sys::Oid::from(1_159_u32);
        let exact_identity = DerivedArtifactIdentity::from_canonical_words(vec![159, 1]);
        let declared = ResidentByteAccounting {
            device_bytes: 16,
            retained_host_exact_bytes: 20,
        };
        let outcome = ensure_staged_device_derived_artifact(
            owner,
            &exact_identity,
            &[owner],
            &[],
            |_| {
                Ok(StagedDerivedPreflight {
                    prepared: (),
                    accounting: declared,
                })
            },
            |(), _| Ok(()),
            |()| {
                Ok(CompositeAccountedArtifact {
                    base: AccountedArtifact {
                        device_bytes: 5,
                        retained_host_exact_bytes: 7,
                    },
                    spatial: AccountedArtifact {
                        device_bytes: 11,
                        retained_host_exact_bytes: 13,
                    },
                })
            },
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .expect("exact composite accounting should publish");
        assert_eq!(outcome, ArtifactEnsureOutcome::Built);
        STORE.with(|store| assert_eq!(store.borrow().entries[0].derived[0].bytes(), 36));

        let mismatch_identity = DerivedArtifactIdentity::from_canonical_words(vec![159, 2]);
        let mismatch_dispatch_called = Cell::new(false);
        let mismatch = ensure_staged_device_derived_artifact(
            owner,
            &mismatch_identity,
            &[owner],
            &[],
            |_| {
                Ok(StagedDerivedPreflight {
                    prepared: (),
                    accounting: declared,
                })
            },
            |(), _| Ok(()),
            |()| {
                Ok(CompositeAccountedArtifact {
                    base: AccountedArtifact {
                        device_bytes: 6,
                        retained_host_exact_bytes: 6,
                    },
                    spatial: AccountedArtifact {
                        device_bytes: 11,
                        retained_host_exact_bytes: 13,
                    },
                })
            },
            |_, _| {
                mismatch_dispatch_called.set(true);
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(matches!(
            mismatch,
            Err(ResidentLoadError::ArtifactAccountingMismatch {
                declared: ResidentByteAccounting {
                    device_bytes: 16,
                    retained_host_exact_bytes: 20,
                },
                actual: ResidentByteAccounting {
                    device_bytes: 17,
                    retained_host_exact_bytes: 19,
                },
            })
        ));
        assert!(!mismatch_dispatch_called.get());
        STORE.with(|store| assert_eq!(store.borrow().entries[0].derived.len(), 1));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn exact_geometry_snapshot_accounts_padded_storage_bit_exactly() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        let column = ResidentColumnRef {
            relid: pg_sys::Oid::from(1_157_u32),
            attno: 3,
        };
        let dependency = ResidentDependencyStamp {
            relid: column.relid,
            generation: 7,
            global_generation: 9,
            relfilenode: pg_sys::Oid::from(88_u32),
        };
        let exact = RetainedExactValues {
            offsets: vec![0, 3, 3, 8].into_boxed_slice(),
            bytes: b"abcdefgh".to_vec().into_boxed_slice(),
        };
        let snapshot = build_geometry_exact_snapshot(
            column,
            dependency,
            pg_sys::Oid::from(42_u32),
            3,
            &exact,
            vec![0_u64; 8].into_boxed_slice(),
        )
        .expect("padded snapshot should build");
        assert_eq!(snapshot.retained_host_bytes(), Some(64));
        assert_eq!(snapshot.exact_value(0), Some(&b"abc"[..]));
        assert_eq!(snapshot.exact_value(1), Some(&b""[..]));
        assert_eq!(snapshot.exact_value(2), Some(&b"defgh"[..]));
        assert_eq!(snapshot.exact_value(3), None);
        assert_eq!(snapshot.dependency(), dependency);
    }

    #[test]
    fn staged_geometry_snapshot_uses_supplied_borrow_and_captured_dependency() {
        let _guard = RESERVED_LIFECYCLE_TEST_LOCK.lock().expect("test lock");
        let owner = pg_sys::Oid::from(1_162_u32);
        let column = ResidentColumnRef {
            relid: owner,
            attno: 2,
        };
        let mut point = vec![0_u8, 0, 0, 0, 0, 0x10, 0xe6, 0];
        point.extend_from_slice(&1_u32.to_le_bytes());
        point.extend_from_slice(&1_u32.to_le_bytes());
        point.extend_from_slice(&1.25_f64.to_le_bytes());
        point.extend_from_slice(&(-2.5_f64).to_le_bytes());
        let geometry = crate::engine::residency::geometry::materialize_resident_geometry_constant(
            &point, 1_024, 8,
        )
        .expect("test resident point");
        install_test_relations(&[1_162]);
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            store.entries[0].row_count = 1;
            store.entries[0].columns.insert(
                column.attno,
                ResidentColumn::Geometry {
                    type_oid: pg_sys::Oid::from(42_u32),
                    data: geometry,
                },
            );
        });
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![162, 2]);
        let requests = [column];
        let outcome = ensure_staged_device_derived_artifact(
            owner,
            &identity,
            &[owner],
            &requests,
            |inputs| {
                let words = resident_geometry_exact_snapshot_words(&requests, &inputs, column)?;
                let retained_host_exact_bytes = words
                    .checked_mul(std::mem::size_of::<u64>())
                    .and_then(|bytes| u64::try_from(bytes).ok())
                    .ok_or(ResidentLoadError::ArtifactAccountingOverflow)?;
                Ok(StagedDerivedPreflight {
                    prepared: words,
                    accounting: ResidentByteAccounting {
                        device_bytes: 0,
                        retained_host_exact_bytes,
                    },
                })
            },
            |words, inputs| {
                snapshot_resident_geometry_exact(
                    &requests,
                    &inputs,
                    column,
                    vec![0_u64; words].into_boxed_slice(),
                )
            },
            |snapshot| Ok(GeometrySnapshotArtifact { snapshot }),
            |artifact, inputs| {
                let evidence = inputs.evidence_for(owner)?;
                assert_eq!(
                    artifact.snapshot.dependency().generation,
                    evidence.generation
                );
                assert_eq!(
                    artifact.snapshot.dependency().global_generation,
                    evidence.global_generation
                );
                assert_eq!(
                    artifact.snapshot.dependency().relfilenode,
                    evidence.relfilenode
                );
                assert!(matches!(
                    inputs.find_column(column)?,
                    ResidentColumnView::Geometry { .. }
                ));
                Ok(())
            },
            |artifact| {
                STORE.with(|store| {
                    let _borrow = store
                        .try_borrow_mut()
                        .expect("snapshot finalize must run outside STORE borrow");
                });
                assert_eq!(artifact.snapshot.exact_value(0), Some(point.as_slice()));
                Ok(())
            },
        )
        .expect("staged snapshot should publish");
        assert_eq!(outcome, ArtifactEnsureOutcome::Built);
        with_derived_artifact_inputs::<GeometrySnapshotArtifact, _>(
            owner,
            &identity,
            &[],
            |resolved| {
                assert_eq!(
                    resolved.artifact.snapshot.exact_value(0),
                    Some(point.as_slice())
                );
            },
        )
        .expect("snapshot artifact resolves");
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn reserve_first_device_build_rejects_category_mismatch_and_releases_charge() {
        install_test_relations(&[1_160]);
        let owner = pg_sys::Oid::from(1_160_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 16]);
        let result = ensure_device_derived_artifact(
            owner,
            &identity,
            &[owner],
            &[],
            ResidentByteAccounting {
                device_bytes: 8,
                retained_host_exact_bytes: 5,
            },
            |_| {
                Ok(AccountedArtifact {
                    device_bytes: 9,
                    retained_host_exact_bytes: 4,
                })
            },
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactAccountingMismatch {
                declared: ResidentByteAccounting {
                    device_bytes: 8,
                    retained_host_exact_bytes: 5
                },
                actual: ResidentByteAccounting {
                    device_bytes: 9,
                    retained_host_exact_bytes: 4
                }
            })
        ));
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn nonzero_charge_dependency_race_drops_charge_and_built_artifact() {
        install_test_relations(&[1_200, 1_201]);
        let owner = pg_sys::Oid::from(1_200_u32);
        let dimension = pg_sys::Oid::from(1_201_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 2]);
        let before = ledger::total_bytes();
        let dropped = Rc::new(Cell::new(false));
        let dropped_by_artifact = Rc::clone(&dropped);
        let result = ensure_derived_artifact(
            owner,
            &identity,
            &[owner, dimension],
            &[],
            |_| {
                Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 23,
                })
            },
            move |()| {
                assert!(ledger::total_bytes() >= before.saturating_add(23));
                STORE.with(|store| store.borrow_mut().entries[1].generation.relation += 1);
                Ok(DropTrackedArtifact {
                    bytes: 23,
                    dropped: dropped_by_artifact,
                })
            },
        );
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactDependencyChanged { relid }) if relid == dimension
        ));
        assert!(dropped.get());
        assert_eq!(ledger::total_bytes(), before);
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn stale_artifact_is_removed_before_use() {
        install_test_relations(&[1_300, 1_301]);
        let owner = pg_sys::Oid::from(1_300_u32);
        let dimension = pg_sys::Oid::from(1_301_u32);
        let identity = DerivedArtifactIdentity::from_canonical_words(vec![9, 3]);
        ensure_derived_artifact(
            owner,
            &identity,
            &[owner, dimension],
            &[],
            |_| {
                Ok(PreparedDerived {
                    prepared: (),
                    device_bytes: 0,
                })
            },
            |()| Ok(EmptyArtifact),
        )
        .expect("artifact build succeeds");
        STORE.with(|store| store.borrow_mut().entries[1].generation.global += 1);

        let result =
            with_derived_artifact_inputs::<EmptyArtifact, _>(owner, &identity, &[], |_| ());
        assert!(matches!(
            result,
            Err(ResidentLoadError::ArtifactDependencyChanged { relid }) if relid == dimension
        ));
        STORE.with(|store| assert!(store.borrow().entries[0].derived.is_empty()));
        STORE.with(|store| store.borrow_mut().entries.clear());
    }

    #[test]
    fn batch_first_use_scope_normalizes_multiple_new_relations() {
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            store.entries.clear();
            let mut fact = empty_relation(400, false, 1);
            fact.first_use_scope = Some(CommandScope {
                xid: 8,
                command_id: 10,
            });
            let mut dimension = empty_relation(500, false, 2);
            dimension.first_use_scope = Some(CommandScope {
                xid: 8,
                command_id: 11,
            });
            let pinned = empty_relation(600, true, 3);
            store.entries.push(fact);
            store.entries.push(dimension);
            store.entries.push(pinned);
        });

        let final_scope = CommandScope {
            xid: 8,
            command_id: 12,
        };
        finalize_batch_first_use(
            &[
                pg_sys::Oid::from(400_u32),
                pg_sys::Oid::from(500_u32),
                pg_sys::Oid::from(600_u32),
            ],
            final_scope,
        );

        STORE.with(|store| {
            assert_eq!(store.borrow().entries.len(), 3);
            assert!(
                store
                    .borrow()
                    .entries
                    .iter()
                    .filter(|entry| !entry.pinned)
                    .all(|entry| entry.first_use_scope == Some(final_scope))
            );
            assert_eq!(store.borrow().entries[2].first_use_scope, None);
            store.borrow_mut().entries.clear();
        });
    }

    #[test]
    fn count_only_request_accepts_a_zero_column_resident_snapshot() {
        let relation = empty_relation(600, false, 1);
        assert!(has_requested_columns(&relation, &[]));
        assert_eq!(relation.raw_bytes(), 0);
        assert!(relation.columns.is_empty());
    }
}

#[cfg(feature = "pg_test")]
mod pg_tests {
    #[pgrx::pg_schema]
    mod tests {
        use std::collections::BTreeMap;

        use pgrx::{
            pg_sys,
            prelude::{Spi, pg_test},
        };

        use super::super::{
            LedgerCharge, PENDING_RELCACHE, PENDING_RELCACHE_CLEAR_ALL,
            PendingRelcacheInvalidations, ResidentLoadError, ResidentRelation, STORE,
            cleanup_backend, ledger, loader, now_us, process_invalidations,
            reserve_with_local_eviction,
        };

        fn test_relation_oid(name: &str) -> pg_sys::Oid {
            let raw = Spi::get_one::<i64>(&format!("SELECT '{name}'::regclass::oid::int8"))
                .expect("relation OID query succeeds")
                .expect("relation exists");
            pg_sys::Oid::from(u32::try_from(raw).expect("relation OID fits u32"))
        }

        fn install_catalog_backed_entry(relid: pg_sys::Oid) -> pg_sys::Oid {
            let relfilenode = loader::current_relfilenode(relid).expect("relation has relfilenode");
            STORE.with(|store| {
                let mut store = store.borrow_mut();
                store.remove_relation(relid);
                store.tick = store.tick.wrapping_add(1).max(1);
                let tick = store.tick;
                store.entries.push(ResidentRelation {
                    relid,
                    relfilenode,
                    generation: ledger::generation_stamp(relid),
                    columns: BTreeMap::new(),
                    row_count: 0,
                    loaded_at_us: now_us(),
                    last_used_us: now_us(),
                    load_ms: 0.0,
                    last_used_tick: tick,
                    pinned: false,
                    raw_charge: LedgerCharge::reserve(0, 0).expect("zero-byte charge"),
                    raw_accounting: super::super::ResidentByteAccounting::default(),
                    first_use_scope: None,
                    derived: Vec::new(),
                });
            });
            relfilenode
        }

        fn run_in_subtransaction(sql: &str) {
            // SAFETY: this mirrors PL/pgSQL's exception-block subtransaction
            // resource/context save and restore on the backend main thread.
            unsafe {
                let old_context = pg_sys::CurrentMemoryContext;
                let old_owner = pg_sys::CurrentResourceOwner;
                pg_sys::BeginInternalSubTransaction(std::ptr::null());
                pg_sys::MemoryContextSwitchTo(old_context);
                Spi::run(sql).expect("subtransaction statement succeeds");
                pg_sys::ReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::CurrentResourceOwner = old_owner;
            }
        }

        #[pg_test]
        fn live_relcache_and_relfilenode_invalidation_prune_resident_entries() {
            cleanup_backend();
            Spi::run("CREATE TEMP TABLE pgaccel_residency_live_inval (value int4)")
                .expect("temporary table creation succeeds");
            let relid = test_relation_oid("pgaccel_residency_live_inval");
            let old_relfilenode = install_catalog_backed_entry(relid);

            process_invalidations();
            STORE.with(|store| assert_eq!(store.borrow().entries.len(), 1));

            run_in_subtransaction("TRUNCATE pgaccel_residency_live_inval");
            let new_relfilenode =
                loader::current_relfilenode(relid).expect("relation still has relfilenode");
            assert_ne!(old_relfilenode, new_relfilenode);

            // Isolate the relfilenode predicate from the callback notification
            // produced by TRUNCATE; both independently fail closed.
            PENDING_RELCACHE_CLEAR_ALL.with(|pending| pending.set(false));
            PENDING_RELCACHE.with(|pending| pending.set(PendingRelcacheInvalidations::empty()));
            process_invalidations();
            STORE.with(|store| assert!(store.borrow().entries.is_empty()));

            install_catalog_backed_entry(relid);
            Spi::run("ALTER TABLE pgaccel_residency_live_inval ADD COLUMN marker int4")
                .expect("catalog-only ALTER TABLE succeeds");
            process_invalidations();
            STORE.with(|store| assert!(store.borrow().entries.is_empty()));
            cleanup_backend();
        }

        #[pg_test]
        fn live_resident_budget_guc_rejects_a_nonzero_reservation_at_zero() {
            cleanup_backend();
            Spi::run("SET LOCAL pg_accel.resident_memory_budget_mb = 0")
                .expect("resident budget GUC accepts zero");
            let error = reserve_with_local_eviction(pg_sys::InvalidOid, 1)
                .expect_err("one byte exceeds a zero-byte live budget");
            assert_eq!(
                error,
                ResidentLoadError::BudgetExceeded {
                    requested: 1,
                    live: 0,
                    budget: 0,
                }
            );
            cleanup_backend();
        }
    }
}

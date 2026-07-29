//! Typed operation and output-contract scaffolding for registry entries.
//!
//! These types are derived from [`FunctionAccelEntry`] metadata and give the
//! planner / executor wiring a typed place to validate shape and field
//! metadata before it commits to a dispatch path.

use std::fmt;

use super::{AccelStrategy, FunctionAccelEntry, OutputShape};

/// Typed identifier for the kernel family a registry entry routes to.
///
/// This is deliberately coarser than the function name. Current dispatchers
/// still route by OID/name internally, but this enum gives future code a
/// compile-checked way to separate broad kernel contracts without matching on
/// raw strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelOp {
    /// GPU spatial predicate path, usually returning a boolean Datum.
    SpatialPredicate,
    /// GPU spatial measurement path, such as distance, area, or length.
    SpatialMeasurement,
    /// GPU raster operation returning one scalar/raster Datum per input row.
    RasterScalar,
    /// GPU raster summary statistics returning a fixed-width record.
    RasterSummaryStats,
    /// GPU H3 operation returning one scalar Datum per input row.
    H3Scalar,
    /// GPU H3 operation returning CSR-style variable-length output.
    H3VarLen,
    /// GPU H3 operation returning fixed-width records.
    H3Record,
    /// Wire identity for the retired standalone sort executor.
    Sort,
    /// Wire identity for retired standalone reduction; reducing descriptors
    /// use the resident aggregate contract instead.
    Reduce,
    /// Wire identity for the retired standalone expression executor.
    Expr,
    /// Wire identity for the retired row-emitting hash join executor.
    HashJoin,
    /// Wire identity for the retired standalone window executor.
    Window,
    /// Wire/registry identity for scalar-inequality opportunities that are
    /// structurally declined before dispatch. No standalone NLJ kernel ships.
    NestedLoopIneq,
}

impl KernelOp {
    /// Build a typed kernel operation from an existing registry entry.
    #[must_use]
    pub fn from_entry(entry: &FunctionAccelEntry) -> Self {
        match entry.strategy {
            AccelStrategy::GpuSpatial => match entry.name {
                "st_area" | "st_distance" | "st_length" => Self::SpatialMeasurement,
                _ => Self::SpatialPredicate,
            },
            AccelStrategy::GpuRaster => match entry.output_shape {
                OutputShape::Record { .. } => Self::RasterSummaryStats,
                OutputShape::Scalar | OutputShape::VarLen => Self::RasterScalar,
            },
            AccelStrategy::GpuH3 => match entry.output_shape {
                OutputShape::Scalar => Self::H3Scalar,
                OutputShape::Record { .. } => Self::H3Record,
                OutputShape::VarLen => Self::H3VarLen,
            },
            AccelStrategy::GpuSort => Self::Sort,
            AccelStrategy::GpuReduce => Self::Reduce,
            AccelStrategy::GpuExpr => Self::Expr,
            AccelStrategy::GpuHashJoin => Self::HashJoin,
            AccelStrategy::GpuWindow => Self::Window,
            AccelStrategy::GpuNestedLoopIneq => Self::NestedLoopIneq,
        }
    }

    /// Return the acceleration strategy for this typed kernel family.
    #[must_use]
    pub const fn strategy(self) -> AccelStrategy {
        match self {
            Self::SpatialPredicate | Self::SpatialMeasurement => AccelStrategy::GpuSpatial,
            Self::RasterScalar | Self::RasterSummaryStats => AccelStrategy::GpuRaster,
            Self::H3Scalar | Self::H3VarLen | Self::H3Record => AccelStrategy::GpuH3,
            Self::Sort => AccelStrategy::GpuSort,
            Self::Reduce => AccelStrategy::GpuReduce,
            Self::Expr => AccelStrategy::GpuExpr,
            Self::HashJoin => AccelStrategy::GpuHashJoin,
            Self::Window => AccelStrategy::GpuWindow,
            Self::NestedLoopIneq => AccelStrategy::GpuNestedLoopIneq,
        }
    }
}

/// Type information for one output field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldTypeSpec {
    /// A concrete PostgreSQL type OID known at registration time.
    StaticOid(u32),
    /// Type must be resolved from `pg_proc` at execution/setup time.
    ///
    /// This mirrors the sentinel value `0` in `FunctionAccelEntry::output_field_types`.
    RuntimeResolved,
}

impl FieldTypeSpec {
    /// Convert a field OID into the typed representation.
    #[must_use]
    pub const fn from_type_oid(oid: u32) -> Self {
        if oid == 0 {
            Self::RuntimeResolved
        } else {
            Self::StaticOid(oid)
        }
    }

    /// Convert back to the OID/sentinel representation.
    #[must_use]
    pub const fn as_type_oid(self) -> u32 {
        match self {
            Self::StaticOid(oid) => oid,
            Self::RuntimeResolved => 0,
        }
    }
}

/// One named output field in an output contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldSpec {
    /// Column or element label used when building tuple descriptors.
    pub name: &'static str,
    /// PostgreSQL type contract for this field.
    pub field_type: FieldTypeSpec,
}

impl FieldSpec {
    /// Build a field spec from name/type metadata.
    #[must_use]
    pub const fn new(name: &'static str, type_oid: u32) -> Self {
        Self {
            name,
            field_type: FieldTypeSpec::from_type_oid(type_oid),
        }
    }

    /// Return the OID/sentinel value for this field.
    #[must_use]
    pub const fn type_oid(&self) -> u32 {
        self.field_type.as_type_oid()
    }
}

/// Typed output contract derived from [`OutputShape`] plus field metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputContract {
    /// One scalar Datum per input row.
    ///
    /// `field` is optional because many existing scalar predicate entries do
    /// not need FunctionScan tuple metadata.
    Scalar {
        /// Optional scalar field metadata.
        field: Option<FieldSpec>,
    },
    /// Fixed-width record output.
    Record {
        /// Fields emitted per input row, in tuple descriptor order.
        fields: Vec<FieldSpec>,
    },
    /// CSR-style variable-length output.
    VarLen {
        /// Element field emitted into the flat values buffer.
        element: FieldSpec,
    },
}

impl OutputContract {
    /// Build and validate an output contract from an existing registry entry.
    ///
    /// This is intentionally stricter than the raw fields: it rejects
    /// mismatched type/name vectors, record field-count drift, and varlen
    /// entries without exactly one element field. Existing runtime code does
    /// not call this automatically, so adding it does not change behavior.
    pub fn from_entry(entry: &FunctionAccelEntry) -> Result<Self, OutputContractError> {
        Self::from_shape_metadata(
            entry.output_shape,
            &entry.output_field_types,
            &entry.output_field_names,
        )
    }

    /// Build and validate an output contract from shape metadata.
    pub fn from_shape_metadata(
        shape: OutputShape,
        field_types: &[u32],
        field_names: &[&'static str],
    ) -> Result<Self, OutputContractError> {
        let fields = field_specs_from_metadata(field_types, field_names)?;

        match shape {
            OutputShape::Scalar => match fields.len() {
                0 => Ok(Self::Scalar { field: None }),
                1 => Ok(Self::Scalar {
                    field: fields.into_iter().next(),
                }),
                actual => Err(OutputContractError::ScalarFieldCountMismatch { actual }),
            },
            OutputShape::Record { field_count } => {
                let expected = field_count as usize;
                if expected == 0 {
                    return Err(OutputContractError::EmptyRecord);
                }
                if fields.len() != expected {
                    return Err(OutputContractError::RecordFieldCountMismatch {
                        expected,
                        actual: fields.len(),
                    });
                }
                Ok(Self::Record { fields })
            }
            OutputShape::VarLen => match fields.len() {
                1 => {
                    let element = fields
                        .into_iter()
                        .next()
                        .expect("checked exactly one field");
                    Ok(Self::VarLen { element })
                }
                actual => Err(OutputContractError::VarLenFieldCountMismatch { actual }),
            },
        }
    }

    /// Return the output shape represented by this contract.
    #[must_use]
    pub fn output_shape(&self) -> OutputShape {
        match self {
            Self::Scalar { .. } => OutputShape::Scalar,
            Self::Record { fields } => OutputShape::Record {
                field_count: fields.len() as u32,
            },
            Self::VarLen { .. } => OutputShape::VarLen,
        }
    }

    /// Return the output fields carried by this contract.
    #[must_use]
    pub fn fields(&self) -> &[FieldSpec] {
        match self {
            Self::Scalar { field: Some(field) } => std::slice::from_ref(field),
            Self::Scalar { field: None } => &[],
            Self::Record { fields } => fields,
            Self::VarLen { element } => std::slice::from_ref(element),
        }
    }
}

/// A complete typed dispatch contract for a registry entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOp {
    kernel: KernelOp,
    output: OutputContract,
}

impl DispatchOp {
    /// Build a typed dispatch operation from an existing registry entry.
    pub fn from_entry(entry: &FunctionAccelEntry) -> Result<Self, OutputContractError> {
        Ok(Self {
            kernel: KernelOp::from_entry(entry),
            output: OutputContract::from_entry(entry)?,
        })
    }

    /// Kernel family selected by the registry entry.
    #[must_use]
    pub const fn kernel(&self) -> KernelOp {
        self.kernel
    }

    /// Acceleration strategy represented by the kernel family.
    #[must_use]
    pub const fn strategy(&self) -> AccelStrategy {
        self.kernel.strategy()
    }

    /// Output contract expected from the dispatch result.
    #[must_use]
    pub const fn output(&self) -> &OutputContract {
        &self.output
    }
}

/// Validation failure while converting output metadata into a typed contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputContractError {
    /// Parallel metadata vectors do not describe the same number of fields.
    FieldMetadataLengthMismatch {
        /// Number of type OIDs supplied.
        types: usize,
        /// Number of field names supplied.
        names: usize,
    },
    /// Scalar outputs may carry zero or one field spec, never more.
    ScalarFieldCountMismatch {
        /// Number of field specs supplied.
        actual: usize,
    },
    /// Record outputs must declare at least one field.
    EmptyRecord,
    /// Record field metadata length disagrees with `OutputShape::Record`.
    RecordFieldCountMismatch {
        /// `OutputShape::Record.field_count`.
        expected: usize,
        /// Number of field specs supplied.
        actual: usize,
    },
    /// Varlen outputs must declare exactly one element field.
    VarLenFieldCountMismatch {
        /// Number of field specs supplied.
        actual: usize,
    },
}

impl fmt::Display for OutputContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FieldMetadataLengthMismatch { types, names } => write!(
                f,
                "output field metadata length mismatch: {types} type OIDs, {names} names",
            ),
            Self::ScalarFieldCountMismatch { actual } => {
                write!(f, "scalar output expected 0 or 1 fields, got {actual}")
            }
            Self::EmptyRecord => write!(f, "record output must declare at least one field"),
            Self::RecordFieldCountMismatch { expected, actual } => write!(
                f,
                "record output expected {expected} fields from output_shape, got {actual}",
            ),
            Self::VarLenFieldCountMismatch { actual } => {
                write!(
                    f,
                    "varlen output expected exactly 1 element field, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for OutputContractError {}

impl FunctionAccelEntry {
    /// Return the typed kernel family represented by this entry.
    #[must_use]
    pub fn kernel_op(&self) -> KernelOp {
        KernelOp::from_entry(self)
    }

    /// Validate and return the typed output contract represented by this
    /// entry's output fields.
    pub fn output_contract(&self) -> Result<OutputContract, OutputContractError> {
        OutputContract::from_entry(self)
    }

    /// Validate and return the full typed dispatch contract for this entry.
    pub fn dispatch_op(&self) -> Result<DispatchOp, OutputContractError> {
        DispatchOp::from_entry(self)
    }
}

fn field_specs_from_metadata(
    field_types: &[u32],
    field_names: &[&'static str],
) -> Result<Vec<FieldSpec>, OutputContractError> {
    if field_types.len() != field_names.len() {
        return Err(OutputContractError::FieldMetadataLengthMismatch {
            types: field_types.len(),
            names: field_names.len(),
        });
    }

    Ok(field_types
        .iter()
        .copied()
        .zip(field_names.iter().copied())
        .map(|(type_oid, name)| FieldSpec::new(name, type_oid))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        name: &'static str,
        strategy: AccelStrategy,
        output_shape: OutputShape,
        output_field_types: &[u32],
        output_field_names: &[&'static str],
    ) -> FunctionAccelEntry {
        FunctionAccelEntry {
            schema: "public",
            name,
            strategy,
            output_shape,
            output_field_types: output_field_types.to_vec(),
            output_field_names: output_field_names.to_vec(),
        }
    }

    #[test]
    fn kernel_operations_cover_every_strategy_and_shape_family() {
        let cases = [
            (
                entry(
                    "st_contains",
                    AccelStrategy::GpuSpatial,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::SpatialPredicate,
            ),
            (
                entry(
                    "st_area",
                    AccelStrategy::GpuSpatial,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::SpatialMeasurement,
            ),
            (
                entry(
                    "st_distance",
                    AccelStrategy::GpuSpatial,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::SpatialMeasurement,
            ),
            (
                entry(
                    "st_length",
                    AccelStrategy::GpuSpatial,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::SpatialMeasurement,
            ),
            (
                entry(
                    "st_mapalgebra",
                    AccelStrategy::GpuRaster,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::RasterScalar,
            ),
            (
                entry(
                    "st_mapalgebra_many",
                    AccelStrategy::GpuRaster,
                    OutputShape::VarLen,
                    &[23],
                    &["value"],
                ),
                KernelOp::RasterScalar,
            ),
            (
                entry(
                    "st_summarystats",
                    AccelStrategy::GpuRaster,
                    OutputShape::Record { field_count: 1 },
                    &[20],
                    &["count"],
                ),
                KernelOp::RasterSummaryStats,
            ),
            (
                entry(
                    "h3_get_resolution",
                    AccelStrategy::GpuH3,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::H3Scalar,
            ),
            (
                entry(
                    "h3_record",
                    AccelStrategy::GpuH3,
                    OutputShape::Record { field_count: 1 },
                    &[20],
                    &["cell"],
                ),
                KernelOp::H3Record,
            ),
            (
                entry(
                    "h3_grid_disk",
                    AccelStrategy::GpuH3,
                    OutputShape::VarLen,
                    &[20],
                    &["cell"],
                ),
                KernelOp::H3VarLen,
            ),
            (
                entry(
                    "sort",
                    AccelStrategy::GpuSort,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::Sort,
            ),
            (
                entry(
                    "reduce",
                    AccelStrategy::GpuReduce,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::Reduce,
            ),
            (
                entry(
                    "expr",
                    AccelStrategy::GpuExpr,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::Expr,
            ),
            (
                entry(
                    "hashjoin",
                    AccelStrategy::GpuHashJoin,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::HashJoin,
            ),
            (
                entry(
                    "window",
                    AccelStrategy::GpuWindow,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::Window,
            ),
            (
                entry(
                    "nested_loop",
                    AccelStrategy::GpuNestedLoopIneq,
                    OutputShape::Scalar,
                    &[],
                    &[],
                ),
                KernelOp::NestedLoopIneq,
            ),
        ];

        for (entry, expected) in cases {
            assert_eq!(KernelOp::from_entry(&entry), expected, "{}", entry.name);
            assert_eq!(entry.kernel_op(), expected, "{}", entry.name);
            assert_eq!(expected.strategy(), entry.strategy, "{}", entry.name);

            let dispatch = entry.dispatch_op().expect("valid dispatch contract");
            assert_eq!(dispatch.kernel(), expected);
            assert_eq!(dispatch.strategy(), entry.strategy);
            assert_eq!(dispatch.output().output_shape(), entry.output_shape);
        }
    }

    #[test]
    fn field_type_and_field_specs_roundtrip_runtime_and_static_oids() {
        let runtime = FieldSpec::new("runtime", 0);
        assert_eq!(runtime.field_type, FieldTypeSpec::RuntimeResolved);
        assert_eq!(runtime.type_oid(), 0);

        let static_field = FieldSpec::new("static", 23);
        assert_eq!(static_field.field_type, FieldTypeSpec::StaticOid(23));
        assert_eq!(static_field.type_oid(), 23);
        assert_eq!(FieldTypeSpec::from_type_oid(0).as_type_oid(), 0);
        assert_eq!(
            FieldTypeSpec::from_type_oid(u32::MAX).as_type_oid(),
            u32::MAX
        );
    }

    #[test]
    fn output_contracts_preserve_shapes_and_ordered_field_metadata() {
        let scalar = OutputContract::from_shape_metadata(OutputShape::Scalar, &[], &[])
            .expect("metadata-free scalar");
        assert_eq!(scalar, OutputContract::Scalar { field: None });
        assert_eq!(scalar.output_shape(), OutputShape::Scalar);
        assert!(scalar.fields().is_empty());

        let scalar_with_field =
            OutputContract::from_shape_metadata(OutputShape::Scalar, &[0], &["value"])
                .expect("scalar field");
        assert_eq!(scalar_with_field.fields(), &[FieldSpec::new("value", 0)]);

        let record = OutputContract::from_shape_metadata(
            OutputShape::Record { field_count: 2 },
            &[20, 0],
            &["count", "dynamic"],
        )
        .expect("record metadata");
        assert_eq!(
            record.fields(),
            &[FieldSpec::new("count", 20), FieldSpec::new("dynamic", 0)]
        );
        assert_eq!(
            record.output_shape(),
            OutputShape::Record { field_count: 2 }
        );

        let varlen = OutputContract::from_shape_metadata(OutputShape::VarLen, &[20], &["element"])
            .expect("varlen metadata");
        assert_eq!(varlen.fields(), &[FieldSpec::new("element", 20)]);
        assert_eq!(varlen.output_shape(), OutputShape::VarLen);

        let entry = entry(
            "record",
            AccelStrategy::GpuH3,
            OutputShape::Record { field_count: 2 },
            &[20, 0],
            &["count", "dynamic"],
        );
        assert_eq!(entry.output_contract().expect("entry contract"), record);
        assert_eq!(
            OutputContract::from_entry(&entry).expect("entry contract"),
            record
        );
    }

    #[test]
    fn output_contract_rejects_every_shape_and_metadata_mismatch() {
        let cases = [
            (
                OutputContract::from_shape_metadata(OutputShape::Scalar, &[20], &[]),
                OutputContractError::FieldMetadataLengthMismatch { types: 1, names: 0 },
            ),
            (
                OutputContract::from_shape_metadata(OutputShape::Scalar, &[20, 23], &["a", "b"]),
                OutputContractError::ScalarFieldCountMismatch { actual: 2 },
            ),
            (
                OutputContract::from_shape_metadata(
                    OutputShape::Record { field_count: 0 },
                    &[],
                    &[],
                ),
                OutputContractError::EmptyRecord,
            ),
            (
                OutputContract::from_shape_metadata(
                    OutputShape::Record { field_count: 2 },
                    &[20],
                    &["only"],
                ),
                OutputContractError::RecordFieldCountMismatch {
                    expected: 2,
                    actual: 1,
                },
            ),
            (
                OutputContract::from_shape_metadata(OutputShape::VarLen, &[], &[]),
                OutputContractError::VarLenFieldCountMismatch { actual: 0 },
            ),
            (
                OutputContract::from_shape_metadata(OutputShape::VarLen, &[20, 20], &["a", "b"]),
                OutputContractError::VarLenFieldCountMismatch { actual: 2 },
            ),
        ];

        for (actual, expected) in cases {
            assert_eq!(actual.expect_err("invalid contract"), expected);
        }
    }

    #[test]
    fn output_contract_errors_have_stable_operator_diagnostics() {
        let cases = [
            (
                OutputContractError::FieldMetadataLengthMismatch { types: 2, names: 1 },
                "output field metadata length mismatch: 2 type OIDs, 1 names",
            ),
            (
                OutputContractError::ScalarFieldCountMismatch { actual: 3 },
                "scalar output expected 0 or 1 fields, got 3",
            ),
            (
                OutputContractError::EmptyRecord,
                "record output must declare at least one field",
            ),
            (
                OutputContractError::RecordFieldCountMismatch {
                    expected: 6,
                    actual: 5,
                },
                "record output expected 6 fields from output_shape, got 5",
            ),
            (
                OutputContractError::VarLenFieldCountMismatch { actual: 0 },
                "varlen output expected exactly 1 element field, got 0",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_none());
        }
    }
}

//! Pure normalized shape builder for exact PostGIS Raster calls.

use super::{RasterQuerySpec, RasterReclassParseError, RasterSpecError, parse_exact_reclass_spec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterCommandShape {
    Select,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterRelationShape {
    UnqualifiedSingleBase { relation_oid: u32 },
    Unsupported,
}

/// Query features that would alter visibility, cardinality, projection, or
/// output order relative to the childless row contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RasterQueryFeatures(u32);

impl RasterQueryFeatures {
    pub const NONE: Self = Self(0);
    pub const QUAL: Self = Self(1 << 0);
    pub const SORT: Self = Self(1 << 1);
    pub const DISTINCT: Self = Self(1 << 2);
    pub const GROUP: Self = Self(1 << 3);
    pub const HAVING: Self = Self(1 << 4);
    pub const LIMIT_OR_OFFSET: Self = Self(1 << 5);
    pub const WINDOW: Self = Self(1 << 6);
    pub const TARGET_SRF: Self = Self(1 << 7);
    pub const SUBLINK: Self = Self(1 << 8);
    pub const SET_OPERATION: Self = Self(1 << 9);
    pub const CTE: Self = Self(1 << 10);
    pub const ROW_MARK: Self = Self(1 << 11);
    pub const ROW_SECURITY: Self = Self(1 << 12);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterCallShape {
    ReclassTextText {
        relation_oid: u32,
        raster_attno: i32,
        raster_type_oid: u32,
        function_oid: u32,
        result_type_oid: u32,
        expression: String,
        pixel_type: String,
    },
    SummaryStatsBand {
        relation_oid: u32,
        raster_attno: i32,
        raster_type_oid: u32,
        function_oid: u32,
        result_type_oid: u32,
        band: u32,
        exclude_nodata: bool,
    },
    SummaryStatsDefaultBand {
        relation_oid: u32,
        raster_attno: i32,
        raster_type_oid: u32,
        function_oid: u32,
        result_type_oid: u32,
        exclude_nodata: bool,
    },
    UnsupportedFunction {
        function_oid: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterCatalogContract {
    pub raster_type_oid: u32,
    pub summary_stats_type_oid: u32,
    pub reclass_fn_oid: u32,
    pub summary_stats_fn_oid: u32,
    pub summary_stats_default_band_fn_oid: u32,
    pub fingerprint: Box<[i32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterShapeInput {
    pub command: RasterCommandShape,
    pub relation: RasterRelationShape,
    pub features: RasterQueryFeatures,
    pub nonjunk_target_count: usize,
    pub call: RasterCallShape,
    pub estimated_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterPlannerCandidate {
    pub spec: RasterQuerySpec,
    pub estimated_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterPlannerDecline {
    UnsupportedCommand,
    UnsupportedRelation,
    UnsupportedQueryFeatures(u32),
    TargetCount(usize),
    UnsupportedFunctionOid(u32),
    SummaryStatsBitExactUnavailable(u32),
    RelationMismatch,
    RasterTypeMismatch,
    ResultTypeMismatch,
    CatalogFingerprintUnavailable,
    Reclass(RasterReclassParseError),
    InvalidSpec(RasterSpecError),
}

impl From<RasterSpecError> for RasterPlannerDecline {
    fn from(value: RasterSpecError) -> Self {
        Self::InvalidSpec(value)
    }
}

fn call_relation(call: &RasterCallShape) -> Option<(u32, i32, u32)> {
    match call {
        RasterCallShape::ReclassTextText {
            relation_oid,
            raster_attno,
            raster_type_oid,
            ..
        }
        | RasterCallShape::SummaryStatsBand {
            relation_oid,
            raster_attno,
            raster_type_oid,
            ..
        }
        | RasterCallShape::SummaryStatsDefaultBand {
            relation_oid,
            raster_attno,
            raster_type_oid,
            ..
        } => Some((*relation_oid, *raster_attno, *raster_type_oid)),
        RasterCallShape::UnsupportedFunction { .. } => None,
    }
}

/// Build the strict Reclass-only RQS2 plan contract from catalog-normalized facts.
pub fn build_raster_query_spec(
    input: RasterShapeInput,
    catalog: &RasterCatalogContract,
) -> Result<RasterPlannerCandidate, RasterPlannerDecline> {
    if input.command != RasterCommandShape::Select {
        return Err(RasterPlannerDecline::UnsupportedCommand);
    }
    let RasterRelationShape::UnqualifiedSingleBase { relation_oid } = input.relation else {
        return Err(RasterPlannerDecline::UnsupportedRelation);
    };
    if !input.features.is_empty() {
        return Err(RasterPlannerDecline::UnsupportedQueryFeatures(
            input.features.bits(),
        ));
    }
    if input.nonjunk_target_count != 1 {
        return Err(RasterPlannerDecline::TargetCount(
            input.nonjunk_target_count,
        ));
    }
    if catalog.fingerprint.is_empty() {
        return Err(RasterPlannerDecline::CatalogFingerprintUnavailable);
    }
    let Some((call_relation_oid, raster_attno, raster_type_oid)) = call_relation(&input.call)
    else {
        let RasterCallShape::UnsupportedFunction { function_oid } = input.call else {
            unreachable!();
        };
        return Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid));
    };
    if relation_oid != call_relation_oid {
        return Err(RasterPlannerDecline::RelationMismatch);
    }
    if raster_type_oid != catalog.raster_type_oid {
        return Err(RasterPlannerDecline::RasterTypeMismatch);
    }

    let (function_oid, reclass) = match input.call {
        RasterCallShape::ReclassTextText {
            function_oid,
            result_type_oid,
            expression,
            pixel_type,
            ..
        } => {
            if function_oid != catalog.reclass_fn_oid {
                return Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid));
            }
            if result_type_oid != catalog.raster_type_oid {
                return Err(RasterPlannerDecline::ResultTypeMismatch);
            }
            let reclass = parse_exact_reclass_spec(&expression, &pixel_type)
                .map_err(RasterPlannerDecline::Reclass)?;
            (function_oid, reclass)
        }
        RasterCallShape::SummaryStatsBand {
            function_oid,
            result_type_oid,
            band,
            exclude_nodata,
            ..
        } => {
            if function_oid != catalog.summary_stats_fn_oid {
                return Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid));
            }
            if result_type_oid != catalog.summary_stats_type_oid {
                return Err(RasterPlannerDecline::ResultTypeMismatch);
            }
            let _ = (band, exclude_nodata);
            return Err(RasterPlannerDecline::SummaryStatsBitExactUnavailable(
                function_oid,
            ));
        }
        RasterCallShape::SummaryStatsDefaultBand {
            function_oid,
            result_type_oid,
            exclude_nodata,
            ..
        } => {
            if function_oid != catalog.summary_stats_default_band_fn_oid {
                return Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid));
            }
            if result_type_oid != catalog.summary_stats_type_oid {
                return Err(RasterPlannerDecline::ResultTypeMismatch);
            }
            let _ = exclude_nodata;
            return Err(RasterPlannerDecline::SummaryStatsBitExactUnavailable(
                function_oid,
            ));
        }
        RasterCallShape::UnsupportedFunction { function_oid } => {
            return Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid));
        }
    };
    let spec = RasterQuerySpec {
        relation_oid,
        raster_attno,
        raster_type_oid,
        function_oid,
        catalog_fingerprint: catalog.fingerprint.clone(),
        reclass,
    };
    spec.validate()?;
    Ok(RasterPlannerCandidate {
        spec,
        estimated_rows: input.estimated_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> RasterCatalogContract {
        RasterCatalogContract {
            raster_type_oid: 60_001,
            summary_stats_type_oid: 60_002,
            reclass_fn_oid: 61_001,
            summary_stats_fn_oid: 61_002,
            summary_stats_default_band_fn_oid: 61_003,
            fingerprint: vec![1, 2, 3].into_boxed_slice(),
        }
    }

    fn input(call: RasterCallShape) -> RasterShapeInput {
        RasterShapeInput {
            command: RasterCommandShape::Select,
            relation: RasterRelationShape::UnqualifiedSingleBase { relation_oid: 42 },
            features: RasterQueryFeatures::NONE,
            nonjunk_target_count: 1,
            call,
            estimated_rows: 10_000,
        }
    }

    fn reclass(function_oid: u32) -> RasterCallShape {
        RasterCallShape::ReclassTextText {
            relation_oid: 42,
            raster_attno: 2,
            raster_type_oid: 60_001,
            function_oid,
            result_type_oid: 60_001,
            expression: "0:1,7:2".to_owned(),
            pixel_type: "8BUI".to_owned(),
        }
    }

    #[test]
    fn exact_reclass_builds_one_valid_rqs2_spec() {
        let catalog = catalog();
        let candidate = build_raster_query_spec(input(reclass(catalog.reclass_fn_oid)), &catalog)
            .expect("frozen Reclass shape");
        candidate.spec.validate().expect("valid RQS2");
        assert_eq!(candidate.spec.catalog_fingerprint.as_ref(), [1, 2, 3]);
    }

    #[test]
    fn exact_summary_stats_oids_are_classified_but_never_build_a_spec() {
        let catalog = catalog();
        let calls = [
            RasterCallShape::SummaryStatsBand {
                relation_oid: 42,
                raster_attno: 2,
                raster_type_oid: catalog.raster_type_oid,
                function_oid: catalog.summary_stats_fn_oid,
                result_type_oid: catalog.summary_stats_type_oid,
                band: 3,
                exclude_nodata: true,
            },
            RasterCallShape::SummaryStatsDefaultBand {
                relation_oid: 42,
                raster_attno: 2,
                raster_type_oid: catalog.raster_type_oid,
                function_oid: catalog.summary_stats_default_band_fn_oid,
                result_type_oid: catalog.summary_stats_type_oid,
                exclude_nodata: false,
            },
        ];
        for call in calls {
            let function_oid = match &call {
                RasterCallShape::SummaryStatsBand { function_oid, .. }
                | RasterCallShape::SummaryStatsDefaultBand { function_oid, .. } => *function_oid,
                _ => unreachable!(),
            };
            assert_eq!(
                build_raster_query_spec(input(call), &catalog),
                Err(RasterPlannerDecline::SummaryStatsBitExactUnavailable(
                    function_oid
                ))
            );
        }
    }

    #[test]
    fn same_name_overload_or_user_replacement_oid_cannot_build() {
        let catalog = catalog();
        for function_oid in [61_099, 99_001] {
            assert_eq!(
                build_raster_query_spec(input(reclass(function_oid)), &catalog),
                Err(RasterPlannerDecline::UnsupportedFunctionOid(function_oid))
            );
        }
        assert_eq!(
            build_raster_query_spec(
                input(RasterCallShape::UnsupportedFunction {
                    function_oid: 99_002,
                }),
                &catalog,
            ),
            Err(RasterPlannerDecline::UnsupportedFunctionOid(99_002))
        );
    }

    #[test]
    fn every_row_or_query_shape_modifier_declines() {
        let catalog = catalog();
        let call = reclass(catalog.reclass_fn_oid);
        let mut wrong_command = input(call.clone());
        wrong_command.command = RasterCommandShape::Unsupported;
        assert_eq!(
            build_raster_query_spec(wrong_command, &catalog),
            Err(RasterPlannerDecline::UnsupportedCommand)
        );

        let mut wrong_relation = input(call.clone());
        wrong_relation.relation = RasterRelationShape::Unsupported;
        assert_eq!(
            build_raster_query_spec(wrong_relation, &catalog),
            Err(RasterPlannerDecline::UnsupportedRelation)
        );

        for feature in [
            RasterQueryFeatures::QUAL,
            RasterQueryFeatures::SORT,
            RasterQueryFeatures::DISTINCT,
            RasterQueryFeatures::GROUP,
            RasterQueryFeatures::HAVING,
            RasterQueryFeatures::LIMIT_OR_OFFSET,
            RasterQueryFeatures::WINDOW,
            RasterQueryFeatures::TARGET_SRF,
            RasterQueryFeatures::SUBLINK,
            RasterQueryFeatures::SET_OPERATION,
            RasterQueryFeatures::CTE,
            RasterQueryFeatures::ROW_MARK,
            RasterQueryFeatures::ROW_SECURITY,
        ] {
            let mut modified = input(call.clone());
            modified.features = feature;
            assert_eq!(
                build_raster_query_spec(modified, &catalog),
                Err(RasterPlannerDecline::UnsupportedQueryFeatures(
                    feature.bits()
                ))
            );
        }

        let mut targets = input(call);
        targets.nonjunk_target_count = 2;
        assert_eq!(
            build_raster_query_spec(targets, &catalog),
            Err(RasterPlannerDecline::TargetCount(2))
        );
    }

    #[test]
    fn constants_types_results_and_relation_identity_are_exact() {
        let catalog = catalog();
        let mut wrong_relation = reclass(catalog.reclass_fn_oid);
        let RasterCallShape::ReclassTextText { relation_oid, .. } = &mut wrong_relation else {
            unreachable!();
        };
        *relation_oid = 43;
        assert_eq!(
            build_raster_query_spec(input(wrong_relation), &catalog),
            Err(RasterPlannerDecline::RelationMismatch)
        );

        let mut wrong_type = reclass(catalog.reclass_fn_oid);
        let RasterCallShape::ReclassTextText {
            raster_type_oid, ..
        } = &mut wrong_type
        else {
            unreachable!();
        };
        *raster_type_oid = 70_001;
        assert_eq!(
            build_raster_query_spec(input(wrong_type), &catalog),
            Err(RasterPlannerDecline::RasterTypeMismatch)
        );

        let mut wrong_result = reclass(catalog.reclass_fn_oid);
        let RasterCallShape::ReclassTextText {
            result_type_oid, ..
        } = &mut wrong_result
        else {
            unreachable!();
        };
        *result_type_oid = catalog.summary_stats_type_oid;
        assert_eq!(
            build_raster_query_spec(input(wrong_result), &catalog),
            Err(RasterPlannerDecline::ResultTypeMismatch)
        );

        let mut grammar = reclass(catalog.reclass_fn_oid);
        let RasterCallShape::ReclassTextText { expression, .. } = &mut grammar else {
            unreachable!();
        };
        *expression = "[0-1]:2".to_owned();
        assert!(matches!(
            build_raster_query_spec(input(grammar), &catalog),
            Err(RasterPlannerDecline::Reclass(_))
        ));
    }
}

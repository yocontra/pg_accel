//! Planner-neutral resident raster specification.

pub const RASTER_QUERY_SPEC_VERSION: u32 = 2;
pub const MAX_RASTER_RECLASS_RULES: usize = 64;
pub const MAX_RASTER_CATALOG_FINGERPRINT_WORDS: usize = 4_096;
const MIN_RASTER_INTEGER_VALUE: i64 = -2_147_483_648;
const MAX_RASTER_INTEGER_VALUE: i64 = 4_294_967_295;

/// Literal PostGIS `rt_pixtype` tag. Value 9 is intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RasterPixelType {
    Bool,
    UInt2,
    UInt4,
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Float32,
    Float64,
}

impl RasterPixelType {
    #[must_use]
    pub const fn tag(self) -> u32 {
        match self {
            Self::Bool => 0,
            Self::UInt2 => 1,
            Self::UInt4 => 2,
            Self::Int8 => 3,
            Self::UInt8 => 4,
            Self::Int16 => 5,
            Self::UInt16 => 6,
            Self::Int32 => 7,
            Self::UInt32 => 8,
            Self::Float32 => 10,
            Self::Float64 => 11,
        }
    }

    #[must_use]
    pub const fn from_tag(tag: u32) -> Option<Self> {
        match tag {
            0 => Some(Self::Bool),
            1 => Some(Self::UInt2),
            2 => Some(Self::UInt4),
            3 => Some(Self::Int8),
            4 => Some(Self::UInt8),
            5 => Some(Self::Int16),
            6 => Some(Self::UInt16),
            7 => Some(Self::Int32),
            8 => Some(Self::UInt32),
            10 => Some(Self::Float32),
            11 => Some(Self::Float64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn postgis_name(self) -> &'static str {
        match self {
            Self::Bool => "1BB",
            Self::UInt2 => "2BUI",
            Self::UInt4 => "4BUI",
            Self::Int8 => "8BSI",
            Self::UInt8 => "8BUI",
            Self::Int16 => "16BSI",
            Self::UInt16 => "16BUI",
            Self::Int32 => "32BSI",
            Self::UInt32 => "32BUI",
            Self::Float32 => "32BF",
            Self::Float64 => "64BF",
        }
    }

    #[must_use]
    pub fn from_canonical_postgis_name(name: &str) -> Option<Self> {
        match name {
            "1BB" => Some(Self::Bool),
            "2BUI" => Some(Self::UInt2),
            "4BUI" => Some(Self::UInt4),
            "8BSI" => Some(Self::Int8),
            "8BUI" => Some(Self::UInt8),
            "16BSI" => Some(Self::Int16),
            "16BUI" => Some(Self::UInt16),
            "32BSI" => Some(Self::Int32),
            "32BUI" => Some(Self::UInt32),
            "32BF" => Some(Self::Float32),
            "64BF" => Some(Self::Float64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn integer_bounds(self) -> Option<(i64, i64)> {
        match self {
            Self::Bool => Some((0, 1)),
            Self::UInt2 => Some((0, 3)),
            Self::UInt4 => Some((0, 15)),
            Self::Int8 => Some((i8::MIN as i64, i8::MAX as i64)),
            Self::UInt8 => Some((0, u8::MAX as i64)),
            Self::Int16 => Some((i16::MIN as i64, i16::MAX as i64)),
            Self::UInt16 => Some((0, u16::MAX as i64)),
            Self::Int32 => Some((i32::MIN as i64, i32::MAX as i64)),
            Self::UInt32 => Some((0, u32::MAX as i64)),
            Self::Float32 | Self::Float64 => None,
        }
    }
}

/// One unambiguous `source:destination` PostGIS reclassification mapping.
/// Rules are sorted by source and unique, so PostGIS first-match precedence
/// cannot change the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RasterReclassRule {
    pub source: i64,
    pub destination: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterReclassSpec {
    pub output_pixel_type: RasterPixelType,
    pub rules: Box<[RasterReclassRule]>,
}

/// Source-proved behavior of the admitted PostGIS 3.6.4 three-argument
/// `st_reclass(raster,text,text)` wrapper.
///
/// The wrapper supplies band 1 and NULL output nodata. `RASTER_reclass` turns
/// that NULL into `hasnodata = false`, and `rt_band_reclass` initializes the
/// new band to ordinary zero. Because its source-nodata branch is conditional
/// on output `hasnodata`, source nodata (including a NaN nodata marker) is
/// treated as an ordinary pixel. Singular integer expressions use `FLT_EQ`.
///
/// Proven from PostGIS 3.6.4 `rtpostgis.sql.in`, `rtpg_mapalgebra.c`,
/// `rt_mapalgebra.c`, and `librtcore.h`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterReclassSemantics {
    pub band: u32,
    pub unmatched_value: i64,
    pub output_has_nodata: bool,
    pub source_nodata_is_special: bool,
    pub singular_match_epsilon: f64,
}

impl RasterReclassSemantics {
    pub const POSTGIS_3_6_4_TEXT_TEXT: Self = Self {
        band: 1,
        unmatched_value: 0,
        output_has_nodata: false,
        source_nodata_is_special: false,
        singular_match_epsilon: 1.192_092_895_507_812_5e-7,
    };

    /// Apply PostGIS `FLT_EQ` for an admitted integer source expression.
    #[must_use]
    pub fn singular_rule_matches(source: i64, value: f64) -> bool {
        debug_assert!((MIN_RASTER_INTEGER_VALUE..=MAX_RASTER_INTEGER_VALUE).contains(&source));
        let source = source as f64;
        source == value
            || (source - value).abs() <= Self::POSTGIS_3_6_4_TEXT_TEXT.singular_match_epsilon
    }
}

/// Complete replacement-sensitive childless RQS2 Reclass plan contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterQuerySpec {
    pub relation_oid: u32,
    pub raster_attno: i32,
    pub raster_type_oid: u32,
    pub function_oid: u32,
    pub catalog_fingerprint: Box<[i32]>,
    pub reclass: RasterReclassSpec,
}

/// Fixed scan semantics the planner must prove before encoding a spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterScanContract {
    pub relation: RasterRelationContract,
    pub cardinality: RasterCardinalityContract,
    pub revalidation: RasterRevalidationContract,
    pub borrowing: RasterBorrowContract,
    pub order: RasterOrderContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterRelationContract {
    UnqualifiedSingleBaseRelation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterCardinalityContract {
    OnePerVisibleInputNullPreserving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterRevalidationContract {
    GenerationAndCatalogFingerprintAtBeginAndRescan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterBorrowContract {
    ReserveOutputBeforeInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterOrderContract {
    Unspecified,
}

impl RasterScanContract {
    pub const EXACT: Self = Self {
        relation: RasterRelationContract::UnqualifiedSingleBaseRelation,
        cardinality: RasterCardinalityContract::OnePerVisibleInputNullPreserving,
        revalidation: RasterRevalidationContract::GenerationAndCatalogFingerprintAtBeginAndRescan,
        borrowing: RasterBorrowContract::ReserveOutputBeforeInput,
        order: RasterOrderContract::Unspecified,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterExplainSpec {
    pub operation: &'static str,
    pub overload: &'static str,
    pub relation_oid: u32,
    pub raster_attno: i32,
    pub result_contract: &'static str,
    pub band: u32,
    pub reclass_rule_count: usize,
    pub output_pixel_type: &'static str,
    pub catalog_proof_words: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterSpecError {
    MissingRelationOid,
    InvalidRasterAttno(i32),
    MissingRasterTypeOid,
    MissingFunctionOid,
    EmptyCatalogFingerprint,
    CatalogFingerprintTooLong(usize),
    EmptyReclassRules,
    TooManyReclassRules(usize),
    FloatingReclassOutputUnsupported(RasterPixelType),
    ReclassRulesNotStrictlySorted,
    ReclassSourceOutOfRasterRange(i64),
    ReclassDestinationOutOfRange {
        destination: i64,
        output_pixel_type: RasterPixelType,
    },
}

impl std::fmt::Display for RasterSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RasterSpecError {}

impl RasterQuerySpec {
    pub fn validate(&self) -> Result<(), RasterSpecError> {
        if self.relation_oid == 0 {
            return Err(RasterSpecError::MissingRelationOid);
        }
        if !(1..=i32::from(i16::MAX)).contains(&self.raster_attno) {
            return Err(RasterSpecError::InvalidRasterAttno(self.raster_attno));
        }
        if self.raster_type_oid == 0 {
            return Err(RasterSpecError::MissingRasterTypeOid);
        }
        if self.function_oid == 0 {
            return Err(RasterSpecError::MissingFunctionOid);
        }
        if self.catalog_fingerprint.is_empty() {
            return Err(RasterSpecError::EmptyCatalogFingerprint);
        }
        if self.catalog_fingerprint.len() > MAX_RASTER_CATALOG_FINGERPRINT_WORDS {
            return Err(RasterSpecError::CatalogFingerprintTooLong(
                self.catalog_fingerprint.len(),
            ));
        }
        self.reclass.validate()
    }

    #[must_use]
    pub const fn scan_contract(&self) -> RasterScanContract {
        RasterScanContract::EXACT
    }

    #[must_use]
    pub fn explain(&self) -> RasterExplainSpec {
        RasterExplainSpec {
            operation: "Reclass",
            overload: "st_reclass(raster,text,text)",
            relation_oid: self.relation_oid,
            raster_attno: self.raster_attno,
            result_contract: "one output row per visible raster row",
            band: RasterReclassSemantics::POSTGIS_3_6_4_TEXT_TEXT.band,
            reclass_rule_count: self.reclass.rules.len(),
            output_pixel_type: self.reclass.output_pixel_type.postgis_name(),
            catalog_proof_words: self.catalog_fingerprint.len(),
        }
    }
}

impl RasterReclassSpec {
    #[must_use]
    pub const fn semantics(&self) -> RasterReclassSemantics {
        RasterReclassSemantics::POSTGIS_3_6_4_TEXT_TEXT
    }

    pub fn validate(&self) -> Result<(), RasterSpecError> {
        if self.rules.is_empty() {
            return Err(RasterSpecError::EmptyReclassRules);
        }
        if self.rules.len() > MAX_RASTER_RECLASS_RULES {
            return Err(RasterSpecError::TooManyReclassRules(self.rules.len()));
        }
        let Some((minimum, maximum)) = self.output_pixel_type.integer_bounds() else {
            return Err(RasterSpecError::FloatingReclassOutputUnsupported(
                self.output_pixel_type,
            ));
        };
        if self
            .rules
            .windows(2)
            .any(|rules| rules[0].source >= rules[1].source)
        {
            return Err(RasterSpecError::ReclassRulesNotStrictlySorted);
        }
        for rule in &self.rules {
            if !(MIN_RASTER_INTEGER_VALUE..=MAX_RASTER_INTEGER_VALUE).contains(&rule.source) {
                return Err(RasterSpecError::ReclassSourceOutOfRasterRange(rule.source));
            }
            if !(minimum..=maximum).contains(&rule.destination) {
                return Err(RasterSpecError::ReclassDestinationOutOfRange {
                    destination: rule.destination,
                    output_pixel_type: self.output_pixel_type,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterReclassParseError {
    UnsupportedPixelType(String),
    FloatingOutputUnsupported(RasterPixelType),
    EmptyExpression,
    TooManyRules(usize),
    EmptyRule(usize),
    InvalidMapping(usize),
    NonCanonicalInteger {
        rule: usize,
        side: &'static str,
    },
    SourceOutOfRasterRange(i64),
    DuplicateSource(i64),
    DestinationOutOfRange {
        destination: i64,
        output_pixel_type: RasterPixelType,
    },
    AllocationFailed,
}

impl std::fmt::Display for RasterReclassParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RasterReclassParseError {}

fn parse_canonical_integer(value: &str) -> Option<i64> {
    if value.is_empty()
        || value.starts_with('+')
        || value == "-0"
        || value.starts_with("00")
        || value.starts_with("-0")
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_digit() || index == 0 && byte == b'-')
    {
        return None;
    }
    let parsed = value.parse::<i64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

/// Parse the intentionally narrow exact-value subset of PostGIS 3.6.4
/// `reclassexpr`: `integer:integer[,integer:integer...]`.
///
/// Ranges, brackets, interpolation, floats, whitespace, duplicate sources,
/// out-of-raster-range sources, and out-of-range destinations decline before
/// path selection. With unique singular sources, PostGIS first-match
/// precedence is immaterial. Integer
/// destinations already fit the output type, so its `round()` and clamp steps
/// are exact no-ops. The proved three-argument wrapper supplies NULL nodata:
/// its output band has no nodata flag and unmatched pixels remain ordinary 0.
pub fn parse_exact_reclass_spec(
    expression: &str,
    pixel_type: &str,
) -> Result<RasterReclassSpec, RasterReclassParseError> {
    let output_pixel_type = RasterPixelType::from_canonical_postgis_name(pixel_type)
        .ok_or_else(|| RasterReclassParseError::UnsupportedPixelType(pixel_type.to_owned()))?;
    let Some((minimum, maximum)) = output_pixel_type.integer_bounds() else {
        return Err(RasterReclassParseError::FloatingOutputUnsupported(
            output_pixel_type,
        ));
    };
    if expression.is_empty() {
        return Err(RasterReclassParseError::EmptyExpression);
    }
    let count = expression.split(',').count();
    if count > MAX_RASTER_RECLASS_RULES {
        return Err(RasterReclassParseError::TooManyRules(count));
    }
    let mut rules = Vec::new();
    rules
        .try_reserve_exact(count)
        .map_err(|_| RasterReclassParseError::AllocationFailed)?;
    for (index, token) in expression.split(',').enumerate() {
        if token.is_empty() {
            return Err(RasterReclassParseError::EmptyRule(index));
        }
        let mut parts = token.split(':');
        let source = parts
            .next()
            .filter(|_| parts.clone().count() == 1)
            .ok_or(RasterReclassParseError::InvalidMapping(index))?;
        let destination = parts
            .next()
            .ok_or(RasterReclassParseError::InvalidMapping(index))?;
        let source = parse_canonical_integer(source).ok_or(
            RasterReclassParseError::NonCanonicalInteger {
                rule: index,
                side: "source",
            },
        )?;
        let destination = parse_canonical_integer(destination).ok_or(
            RasterReclassParseError::NonCanonicalInteger {
                rule: index,
                side: "destination",
            },
        )?;
        if !(MIN_RASTER_INTEGER_VALUE..=MAX_RASTER_INTEGER_VALUE).contains(&source) {
            return Err(RasterReclassParseError::SourceOutOfRasterRange(source));
        }
        if !(minimum..=maximum).contains(&destination) {
            return Err(RasterReclassParseError::DestinationOutOfRange {
                destination,
                output_pixel_type,
            });
        }
        rules.push(RasterReclassRule {
            source,
            destination,
        });
    }
    rules.sort_unstable_by_key(|rule| rule.source);
    if let Some(duplicate) = rules
        .windows(2)
        .find(|pair| pair[0].source == pair[1].source)
    {
        return Err(RasterReclassParseError::DuplicateSource(
            duplicate[0].source,
        ));
    }
    Ok(RasterReclassSpec {
        output_pixel_type,
        rules: rules.into_boxed_slice(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_reclass_parser_canonicalizes_unique_singular_rules() {
        let spec = parse_exact_reclass_spec("7:2,-1:3,0:1", "4BUI").expect("exact subset");
        assert_eq!(
            spec.rules.as_ref(),
            [
                RasterReclassRule {
                    source: -1,
                    destination: 3,
                },
                RasterReclassRule {
                    source: 0,
                    destination: 1,
                },
                RasterReclassRule {
                    source: 7,
                    destination: 2,
                },
            ]
        );
        spec.validate().expect("parsed spec validates");
    }

    #[test]
    fn exact_reclass_parser_declines_every_unproved_grammar_family() {
        for expression in [
            "0-1:2", "[0-1]:2", "0:1-2", "0.0:1", "0:1.0", "0:1,", " 0:1", "0 :1", "+0:1", "00:1",
            "-0:1", "0:1:2",
        ] {
            assert!(
                parse_exact_reclass_spec(expression, "8BUI").is_err(),
                "unproved grammar must decline: {expression}"
            );
        }
        assert_eq!(
            parse_exact_reclass_spec("0:1,0:2", "8BUI"),
            Err(RasterReclassParseError::DuplicateSource(0))
        );
        assert!(matches!(
            parse_exact_reclass_spec("0:256", "8BUI"),
            Err(RasterReclassParseError::DestinationOutOfRange { .. })
        ));
        assert_eq!(
            parse_exact_reclass_spec("0:1", "32BF"),
            Err(RasterReclassParseError::FloatingOutputUnsupported(
                RasterPixelType::Float32
            ))
        );
        assert!(parse_exact_reclass_spec("0:1", "8bui").is_err());
        assert_eq!(
            parse_exact_reclass_spec("4294967296:1", "8BUI"),
            Err(RasterReclassParseError::SourceOutOfRasterRange(
                4_294_967_296
            ))
        );
        assert_eq!(
            parse_exact_reclass_spec("-2147483649:1", "8BUI"),
            Err(RasterReclassParseError::SourceOutOfRasterRange(
                -2_147_483_649
            ))
        );
    }

    #[test]
    fn exact_reclass_semantics_cover_unmatched_nodata_float_and_nan() {
        let spec = parse_exact_reclass_spec("7:2", "8BUI").expect("exact subset");
        let semantics = spec.semantics();
        assert_eq!(semantics.band, 1);
        assert_eq!(semantics.unmatched_value, 0);
        assert!(!semantics.output_has_nodata);
        assert!(!semantics.source_nodata_is_special);
        assert!(RasterReclassSemantics::singular_rule_matches(7, 7.0));
        assert!(RasterReclassSemantics::singular_rule_matches(
            7,
            7.0 + semantics.singular_match_epsilon
        ));
        assert!(!RasterReclassSemantics::singular_rule_matches(
            7,
            semantics.singular_match_epsilon.mul_add(2.0, 7.0)
        ));
        assert!(!RasterReclassSemantics::singular_rule_matches(7, f64::NAN));
    }

    fn reclass_query_spec() -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 10,
            raster_attno: 2,
            raster_type_oid: 20,
            function_oid: 30,
            catalog_fingerprint: vec![1, 2, 3].into_boxed_slice(),
            reclass: parse_exact_reclass_spec("0:1", "8BUI").expect("canonical reclass"),
        }
    }

    #[test]
    fn reclass_only_row_contract_is_exact() {
        let spec = reclass_query_spec();
        spec.validate().expect("RQS2 Reclass spec is valid");
        assert_eq!(spec.scan_contract(), RasterScanContract::EXACT);
        assert_eq!(
            spec.scan_contract().relation,
            RasterRelationContract::UnqualifiedSingleBaseRelation
        );
        assert_eq!(
            spec.scan_contract().cardinality,
            RasterCardinalityContract::OnePerVisibleInputNullPreserving
        );
        assert_eq!(
            spec.scan_contract().revalidation,
            RasterRevalidationContract::GenerationAndCatalogFingerprintAtBeginAndRescan
        );
        assert_eq!(
            spec.scan_contract().borrowing,
            RasterBorrowContract::ReserveOutputBeforeInput
        );
        assert_eq!(spec.scan_contract().order, RasterOrderContract::Unspecified);
        let explain = spec.explain();
        assert_eq!(explain.operation, "Reclass");
        assert_eq!(explain.overload, "st_reclass(raster,text,text)");
        assert_eq!(explain.band, 1);
        assert_eq!(
            explain.result_contract,
            "one output row per visible raster row"
        );
    }

    #[test]
    fn pixel_tags_and_canonical_names_match_postgis() {
        let cases = [
            (0, "1BB"),
            (1, "2BUI"),
            (2, "4BUI"),
            (3, "8BSI"),
            (4, "8BUI"),
            (5, "16BSI"),
            (6, "16BUI"),
            (7, "32BSI"),
            (8, "32BUI"),
            (10, "32BF"),
            (11, "64BF"),
        ];
        for (tag, name) in cases {
            let pixel_type = RasterPixelType::from_tag(tag).expect("known tag");
            assert_eq!(pixel_type.tag(), tag);
            assert_eq!(pixel_type.postgis_name(), name);
            assert_eq!(
                RasterPixelType::from_canonical_postgis_name(name),
                Some(pixel_type)
            );
        }
        assert_eq!(RasterPixelType::from_tag(9), None);
    }

    #[test]
    fn reclass_rule_has_native_abi_layout() {
        assert_eq!(std::mem::size_of::<RasterReclassRule>(), 16);
        assert_eq!(std::mem::align_of::<RasterReclassRule>(), 8);
        assert_eq!(std::mem::offset_of!(RasterReclassRule, source), 0);
        assert_eq!(std::mem::offset_of!(RasterReclassRule, destination), 8);
    }
}

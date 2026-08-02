//! Strict canonical `i32` wire codec for [`RasterQuerySpec`].

use super::spec::{
    MAX_RASTER_CATALOG_FINGERPRINT_WORDS, MAX_RASTER_RECLASS_RULES, RASTER_QUERY_SPEC_VERSION,
    RasterPixelType, RasterQuerySpec, RasterReclassRule, RasterReclassSpec, RasterSpecError,
};

pub const RASTER_QUERY_SPEC_WIRE_MAGIC: i32 = 0x5251_5332; // "RQS2"
const RASTER_QUERY_SPEC_HEADER_WORDS: usize = 3;
const RASTER_QUERY_SPEC_FIXED_WORDS: usize = 6;
const RASTER_RECLASS_FIXED_WORDS: usize = 2;
const RASTER_RECLASS_RULE_WORDS: usize = 4;

pub const RASTER_QUERY_SPEC_MAX_WORDS: usize = RASTER_QUERY_SPEC_HEADER_WORDS
    + RASTER_QUERY_SPEC_FIXED_WORDS
    + 1
    + MAX_RASTER_CATALOG_FINGERPRINT_WORDS
    + RASTER_RECLASS_FIXED_WORDS
    + MAX_RASTER_RECLASS_RULES * RASTER_RECLASS_RULE_WORDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RasterSpecCodecError {
    InvalidSpec(RasterSpecError),
    Truncated {
        index: usize,
        context: &'static str,
    },
    InvalidMagic(i32),
    UnsupportedVersion(u32),
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    InvalidValue {
        index: usize,
        context: &'static str,
    },
    LimitExceeded {
        index: usize,
        context: &'static str,
        declared: usize,
        maximum: usize,
    },
    TrailingWords {
        index: usize,
        total: usize,
    },
    NonCanonical,
    LengthOverflow,
    AllocationFailed {
        context: &'static str,
    },
}

impl std::fmt::Display for RasterSpecCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RasterSpecCodecError {}

impl From<RasterSpecError> for RasterSpecCodecError {
    fn from(value: RasterSpecError) -> Self {
        Self::InvalidSpec(value)
    }
}

struct Encoder {
    words: Vec<i32>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            words: vec![
                RASTER_QUERY_SPEC_WIRE_MAGIC,
                RASTER_QUERY_SPEC_VERSION as i32,
                0,
            ],
        }
    }

    fn push(&mut self, value: i32) {
        self.words.push(value);
    }

    fn push_u32(&mut self, value: u32) {
        self.push(value as i32);
    }

    fn push_len(&mut self, value: usize) -> Result<(), RasterSpecCodecError> {
        self.push(i32::try_from(value).map_err(|_| RasterSpecCodecError::LengthOverflow)?);
        Ok(())
    }

    fn push_i64(&mut self, value: i64) {
        let bits = value as u64;
        self.push((bits >> 32) as i32);
        self.push(bits as u32 as i32);
    }

    fn finish(mut self) -> Result<Vec<i32>, RasterSpecCodecError> {
        if self.words.len() > RASTER_QUERY_SPEC_MAX_WORDS {
            return Err(RasterSpecCodecError::LimitExceeded {
                index: 2,
                context: "wire words",
                declared: self.words.len(),
                maximum: RASTER_QUERY_SPEC_MAX_WORDS,
            });
        }
        self.words[2] =
            i32::try_from(self.words.len()).map_err(|_| RasterSpecCodecError::LengthOverflow)?;
        Ok(self.words)
    }
}

struct Reader<'a> {
    words: &'a [i32],
    index: usize,
}

impl<'a> Reader<'a> {
    fn new(words: &'a [i32]) -> Self {
        Self { words, index: 0 }
    }

    fn next(&mut self, context: &'static str) -> Result<i32, RasterSpecCodecError> {
        let index = self.index;
        let value = self
            .words
            .get(index)
            .copied()
            .ok_or(RasterSpecCodecError::Truncated { index, context })?;
        self.index += 1;
        Ok(value)
    }

    fn u32(&mut self, context: &'static str) -> Result<u32, RasterSpecCodecError> {
        Ok(self.next(context)? as u32)
    }

    fn len(
        &mut self,
        context: &'static str,
        maximum: usize,
    ) -> Result<usize, RasterSpecCodecError> {
        let index = self.index;
        let raw = self.next(context)?;
        let value = usize::try_from(raw)
            .map_err(|_| RasterSpecCodecError::InvalidValue { index, context })?;
        if value > maximum {
            return Err(RasterSpecCodecError::LimitExceeded {
                index,
                context,
                declared: value,
                maximum,
            });
        }
        Ok(value)
    }

    fn i64(&mut self, context: &'static str) -> Result<i64, RasterSpecCodecError> {
        let high = self.next(context)? as u32 as u64;
        let low = self.next(context)? as u32 as u64;
        Ok(((high << 32) | low) as i64)
    }

    fn finish(self) -> Result<(), RasterSpecCodecError> {
        if self.index == self.words.len() {
            Ok(())
        } else {
            Err(RasterSpecCodecError::TrailingWords {
                index: self.index,
                total: self.words.len(),
            })
        }
    }
}

impl RasterQuerySpec {
    pub fn encode_words(&self) -> Result<Vec<i32>, RasterSpecCodecError> {
        self.validate()?;
        let mut encoder = Encoder::new();
        encoder.push_u32(self.relation_oid);
        encoder.push(self.raster_attno);
        encoder.push_u32(self.raster_type_oid);
        encoder.push_u32(self.function_oid);
        encoder.push_u32(self.as_wkb_fn_oid);
        encoder.push_u32(self.rast_from_wkb_fn_oid);
        encoder.push_len(self.catalog_fingerprint.len())?;
        encoder.words.extend_from_slice(&self.catalog_fingerprint);
        encoder.push_u32(self.reclass.output_pixel_type.tag());
        encoder.push_len(self.reclass.rules.len())?;
        for rule in &self.reclass.rules {
            encoder.push_i64(rule.source);
            encoder.push_i64(rule.destination);
        }
        encoder.finish()
    }

    pub fn decode_words(words: &[i32]) -> Result<Self, RasterSpecCodecError> {
        if words.len() > RASTER_QUERY_SPEC_MAX_WORDS {
            return Err(RasterSpecCodecError::LimitExceeded {
                index: 2,
                context: "wire words",
                declared: words.len(),
                maximum: RASTER_QUERY_SPEC_MAX_WORDS,
            });
        }
        let mut reader = Reader::new(words);
        let magic = reader.next("wire magic")?;
        if magic != RASTER_QUERY_SPEC_WIRE_MAGIC {
            return Err(RasterSpecCodecError::InvalidMagic(magic));
        }
        let version = reader.u32("wire version")?;
        if version != RASTER_QUERY_SPEC_VERSION {
            return Err(RasterSpecCodecError::UnsupportedVersion(version));
        }
        let declared_index = reader.index;
        let declared = reader.len("wire length", RASTER_QUERY_SPEC_MAX_WORDS)?;
        if declared != words.len() {
            return Err(RasterSpecCodecError::LengthMismatch {
                declared,
                actual: words.len(),
            });
        }
        if declared < RASTER_QUERY_SPEC_HEADER_WORDS + RASTER_QUERY_SPEC_FIXED_WORDS {
            return Err(RasterSpecCodecError::InvalidValue {
                index: declared_index,
                context: "wire length",
            });
        }
        let relation_oid = reader.u32("relation OID")?;
        let raster_attno = reader.next("raster attribute number")?;
        let raster_type_oid = reader.u32("raster type OID")?;
        let function_oid = reader.u32("function OID")?;
        let as_wkb_fn_oid = reader.u32("st_aswkb function OID")?;
        let rast_from_wkb_fn_oid = reader.u32("st_rastfromwkb function OID")?;
        let fingerprint_len = reader.len(
            "catalog fingerprint words",
            MAX_RASTER_CATALOG_FINGERPRINT_WORDS,
        )?;
        let fingerprint_start = reader.index;
        let fingerprint_end = fingerprint_start.checked_add(fingerprint_len).ok_or(
            RasterSpecCodecError::InvalidValue {
                index: fingerprint_start,
                context: "catalog fingerprint words",
            },
        )?;
        let fingerprint = words.get(fingerprint_start..fingerprint_end).ok_or(
            RasterSpecCodecError::Truncated {
                index: fingerprint_start,
                context: "catalog fingerprint words",
            },
        )?;
        let mut catalog_fingerprint = Vec::new();
        catalog_fingerprint
            .try_reserve_exact(fingerprint_len)
            .map_err(|_| RasterSpecCodecError::AllocationFailed {
                context: "catalog fingerprint words",
            })?;
        catalog_fingerprint.extend_from_slice(fingerprint);
        reader.index = fingerprint_end;

        let pixel_index = reader.index;
        let output_pixel_type = RasterPixelType::from_tag(reader.u32("output pixel tag")?).ok_or(
            RasterSpecCodecError::InvalidValue {
                index: pixel_index,
                context: "output pixel tag",
            },
        )?;
        let rule_count = reader.len("reclass rule count", MAX_RASTER_RECLASS_RULES)?;
        let mut rules = Vec::new();
        rules.try_reserve_exact(rule_count).map_err(|_| {
            RasterSpecCodecError::AllocationFailed {
                context: "reclass rules",
            }
        })?;
        for _ in 0..rule_count {
            rules.push(RasterReclassRule {
                source: reader.i64("reclass source")?,
                destination: reader.i64("reclass destination")?,
            });
        }
        reader.finish()?;
        let spec = Self {
            relation_oid,
            raster_attno,
            raster_type_oid,
            function_oid,
            as_wkb_fn_oid,
            rast_from_wkb_fn_oid,
            catalog_fingerprint: catalog_fingerprint.into_boxed_slice(),
            reclass: RasterReclassSpec {
                output_pixel_type,
                rules: rules.into_boxed_slice(),
            },
        };
        spec.validate()?;
        if spec.encode_words()?.as_slice() != words {
            return Err(RasterSpecCodecError::NonCanonical);
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reclass_spec() -> RasterQuerySpec {
        RasterQuerySpec {
            relation_oid: 11,
            raster_attno: 2,
            raster_type_oid: 22,
            function_oid: 33,
            as_wkb_fn_oid: 34,
            rast_from_wkb_fn_oid: 35,
            catalog_fingerprint: vec![i32::MIN, 0, i32::MAX].into_boxed_slice(),
            reclass: RasterReclassSpec {
                output_pixel_type: RasterPixelType::Int16,
                rules: vec![
                    RasterReclassRule {
                        source: -4,
                        destination: -100,
                    },
                    RasterReclassRule {
                        source: 9,
                        destination: 32_000,
                    },
                ]
                .into_boxed_slice(),
            },
        }
    }

    #[test]
    fn rqs2_reclass_roundtrips_canonically() {
        let spec = reclass_spec();
        let words = spec.encode_words().expect("valid encoding");
        assert_eq!(RasterQuerySpec::decode_words(&words), Ok(spec));
    }

    #[test]
    fn generated_bounded_raster_specs_roundtrip_and_fail_closed_when_corrupted() {
        let pixel_types = [
            RasterPixelType::Bool,
            RasterPixelType::UInt2,
            RasterPixelType::UInt4,
            RasterPixelType::Int8,
            RasterPixelType::UInt8,
            RasterPixelType::Int16,
            RasterPixelType::UInt16,
            RasterPixelType::Int32,
            RasterPixelType::UInt32,
        ];
        let mut state = 0xA076_1D64_78BD_642F_u64;

        for case in 0..512usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let output_pixel_type = pixel_types[state as usize % pixel_types.len()];
            let (minimum, maximum) = output_pixel_type
                .integer_bounds()
                .expect("generated types are integer types");
            let fingerprint_len = 1 + (state as usize % 64);
            let rule_count = 1 + ((state >> 8) as usize % MAX_RASTER_RECLASS_RULES);
            let mut catalog_fingerprint = Vec::with_capacity(fingerprint_len);
            for _ in 0..fingerprint_len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                catalog_fingerprint.push((state >> 32) as i32);
            }
            let destination_span = (maximum - minimum) as u64 + 1;
            let rules = (0..rule_count)
                .map(|index| RasterReclassRule {
                    source: i64::from(i32::MIN) + index as i64,
                    destination: minimum
                        + (state.wrapping_add(index as u64) % destination_span) as i64,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let nonzero = |value: u32| value.max(1);
            let spec = RasterQuerySpec {
                relation_oid: nonzero(state as u32),
                raster_attno: 1 + ((state >> 32) as u32 % i16::MAX as u32) as i32,
                raster_type_oid: nonzero(state.rotate_left(7) as u32),
                function_oid: nonzero(state.rotate_left(13) as u32),
                as_wkb_fn_oid: nonzero(state.rotate_left(29) as u32),
                rast_from_wkb_fn_oid: nonzero(state.rotate_left(43) as u32),
                catalog_fingerprint: catalog_fingerprint.into_boxed_slice(),
                reclass: RasterReclassSpec {
                    output_pixel_type,
                    rules,
                },
            };
            let words = spec
                .encode_words()
                .unwrap_or_else(|error| panic!("generated spec {case} failed: {error}"));
            let decoded = RasterQuerySpec::decode_words(&words)
                .unwrap_or_else(|error| panic!("generated wire {case} failed: {error}"));
            assert_eq!(decoded, spec, "generated spec {case}");
            assert_eq!(
                decoded.encode_words().expect("decoded spec re-encodes"),
                words,
                "generated spec {case} was not canonical"
            );

            let truncation = (state as usize % (words.len() - 1)) + 1;
            assert!(RasterQuerySpec::decode_words(&words[..truncation]).is_err());

            let mut bad_length = words.clone();
            bad_length[2] += 1;
            assert!(matches!(
                RasterQuerySpec::decode_words(&bad_length),
                Err(RasterSpecCodecError::LengthMismatch { .. })
            ));

            let pixel_tag = RASTER_QUERY_SPEC_HEADER_WORDS
                + RASTER_QUERY_SPEC_FIXED_WORDS
                + 1
                + spec.catalog_fingerprint.len();
            let mut bad_tag = words;
            bad_tag[pixel_tag] = 9;
            assert!(matches!(
                RasterQuerySpec::decode_words(&bad_tag),
                Err(RasterSpecCodecError::InvalidValue {
                    context: "output pixel tag",
                    ..
                })
            ));
        }
    }

    #[test]
    fn oversized_raster_wire_is_rejected_before_header_or_payload_decode() {
        let words = vec![0; RASTER_QUERY_SPEC_MAX_WORDS + 1];
        assert!(matches!(
            RasterQuerySpec::decode_words(&words),
            Err(RasterSpecCodecError::LimitExceeded {
                index: 2,
                context: "wire words",
                declared,
                maximum: RASTER_QUERY_SPEC_MAX_WORDS,
            }) if declared == RASTER_QUERY_SPEC_MAX_WORDS + 1
        ));
    }

    #[test]
    fn every_truncation_and_trailing_word_fails_closed() {
        let words = reclass_spec().encode_words().expect("valid encoding");
        for end in 0..words.len() {
            assert!(RasterQuerySpec::decode_words(&words[..end]).is_err());
        }
        let mut trailing = words;
        trailing.push(0);
        assert!(RasterQuerySpec::decode_words(&trailing).is_err());
    }

    #[test]
    fn invalid_tags_lengths_and_noncanonical_rules_decline() {
        let mut words = reclass_spec().encode_words().expect("valid encoding");
        words[0] = 0;
        assert!(matches!(
            RasterQuerySpec::decode_words(&words),
            Err(RasterSpecCodecError::InvalidMagic(0))
        ));

        let mut words = reclass_spec().encode_words().expect("valid encoding");
        words[1] += 1;
        assert!(matches!(
            RasterQuerySpec::decode_words(&words),
            Err(RasterSpecCodecError::UnsupportedVersion(_))
        ));

        let mut words = reclass_spec().encode_words().expect("valid encoding");
        words[2] -= 1;
        assert!(matches!(
            RasterQuerySpec::decode_words(&words),
            Err(RasterSpecCodecError::LengthMismatch { .. })
        ));

        let mut words = reclass_spec().encode_words().expect("valid encoding");
        let pixel_tag_index = RASTER_QUERY_SPEC_HEADER_WORDS
            + RASTER_QUERY_SPEC_FIXED_WORDS
            + 1
            + reclass_spec().catalog_fingerprint.len();
        words[pixel_tag_index] = 99;
        assert!(RasterQuerySpec::decode_words(&words).is_err());

        let mut unsorted = reclass_spec().encode_words().expect("valid encoding");
        let first_source = unsorted.len() - 8;
        let second_source = unsorted.len() - 4;
        unsorted.swap(first_source, second_source);
        unsorted.swap(first_source + 1, second_source + 1);
        assert!(RasterQuerySpec::decode_words(&unsorted).is_err());

        let mut out_of_range_source = reclass_spec().encode_words().expect("valid encoding");
        let second_source = out_of_range_source.len() - 4;
        out_of_range_source[second_source] = 1;
        out_of_range_source[second_source + 1] = 0;
        assert!(matches!(
            RasterQuerySpec::decode_words(&out_of_range_source),
            Err(RasterSpecCodecError::InvalidSpec(
                RasterSpecError::ReclassSourceOutOfRasterRange(4_294_967_296)
            ))
        ));
    }

    #[test]
    fn fingerprint_length_has_one_bounded_canonical_encoding() {
        let mut empty = reclass_spec().encode_words().expect("valid encoding");
        let length_index = RASTER_QUERY_SPEC_HEADER_WORDS + RASTER_QUERY_SPEC_FIXED_WORDS;
        let fingerprint_start = length_index + 1;
        let fingerprint_end = fingerprint_start + reclass_spec().catalog_fingerprint.len();
        empty.drain(fingerprint_start..fingerprint_end);
        empty[length_index] = 0;
        empty[2] = i32::try_from(empty.len()).expect("short test wire");
        assert!(matches!(
            RasterQuerySpec::decode_words(&empty),
            Err(RasterSpecCodecError::InvalidSpec(
                RasterSpecError::EmptyCatalogFingerprint
            ))
        ));

        let mut negative = reclass_spec().encode_words().expect("valid encoding");
        negative[length_index] = -1;
        assert!(matches!(
            RasterQuerySpec::decode_words(&negative),
            Err(RasterSpecCodecError::InvalidValue {
                context: "catalog fingerprint words",
                ..
            })
        ));

        let mut oversized = reclass_spec().encode_words().expect("valid encoding");
        oversized[length_index] = 4_097;
        assert!(matches!(
            RasterQuerySpec::decode_words(&oversized),
            Err(RasterSpecCodecError::LimitExceeded {
                context: "catalog fingerprint words",
                declared: 4_097,
                maximum: MAX_RASTER_CATALOG_FINGERPRINT_WORDS,
                ..
            })
        ));
    }

    #[test]
    fn arbitrary_bounded_words_never_panic_or_allocate_unboundedly() {
        let mut state = 0x9e37_79b9_u32;
        for len in 0..128_usize {
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                words.push(state as i32);
            }
            let result = std::panic::catch_unwind(|| RasterQuerySpec::decode_words(&words));
            assert!(result.is_ok(), "decoder panicked for {len} words");
        }
    }

    #[test]
    fn declared_schema_maximum_is_exact_for_largest_reclass_spec() {
        let mut spec = reclass_spec();
        spec.reclass.rules = (0..MAX_RASTER_RECLASS_RULES)
            .map(|source| RasterReclassRule {
                source: source as i64,
                destination: 0,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        spec.catalog_fingerprint = vec![0; MAX_RASTER_CATALOG_FINGERPRINT_WORDS].into_boxed_slice();
        assert_eq!(
            spec.encode_words().expect("maximum spec encodes").len(),
            RASTER_QUERY_SPEC_MAX_WORDS
        );
    }

    #[test]
    fn legacy_rqs1_wire_is_not_decodable_as_rqs2() {
        let mut words = reclass_spec().encode_words().expect("valid RQS2");
        words[0] = 0x5251_5331;
        words[1] = 1;
        assert!(matches!(
            RasterQuerySpec::decode_words(&words),
            Err(RasterSpecCodecError::InvalidMagic(0x5251_5331))
        ));
    }
}

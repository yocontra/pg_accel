//! Materialization of generic grouped-aggregate results into PostgreSQL slots.

use std::ffi::CString;
use std::rc::Rc;

use pgrx::pg_sys;

use super::artifact::{GroupDatum, GroupDomain};
use crate::engine::spec::{
    AggOutputProjection, AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind,
    AggregateSource, GroupKeyEncoding, GroupKeySource,
};
use crate::gpu::{GroupedAggOutcome, GroupedAggOutputStorage, GroupedAggStateLane, PgaccelValTag};

const BOOLOID: u32 = 16;
const INT8OID: u32 = 20;
const INT2OID: u32 = 21;
const INT4OID: u32 = 23;
const TEXTOID: u32 = 25;
const FLOAT4OID: u32 = 700;
const FLOAT8OID: u32 = 701;
const BPCHAROID: u32 = 1042;
const VARCHAROID: u32 = 1043;
const DATEOID: u32 = 1082;
const TIMESTAMPOID: u32 = 1114;
const TIMESTAMPTZOID: u32 = 1184;

#[derive(Debug)]
enum MaterializeError {
    Invalid(String),
    CountOverflow { measure_index: usize, count: u64 },
}

impl MaterializeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

enum GroupDecoder {
    Dense(Rc<[GroupDomain]>),
    H3Compact { type_oid: u32 },
}

/// Owns one complete device result and emits its materialized groups.
pub(super) struct DescriptorAggOutput {
    storage: GroupedAggOutputStorage,
    decoder: GroupDecoder,
    projection: AggOutputProjection,
    active_groups: Vec<usize>,
    cursor: usize,
}

impl DescriptorAggOutput {
    pub(super) fn new(
        storage: GroupedAggOutputStorage,
        outcome: GroupedAggOutcome,
        domains: Rc<[GroupDomain]>,
        resolved_spec: AggQuerySpec,
        projection: AggOutputProjection,
    ) -> Result<Self, String> {
        projection
            .validate(&resolved_spec)
            .map_err(|error| format!("aggregate output projection is invalid: {error}"))?;

        let result = match outcome {
            GroupedAggOutcome::Complete(result) => result,
            GroupedAggOutcome::NeedsRecheck(result) => {
                return Err(format!(
                    "generic grouped aggregate returned {} uncertain rows; childless execution cannot recheck",
                    result.uncertain_count
                ));
            }
        };
        if result.uncertain_count != 0 {
            return Err(format!(
                "complete grouped aggregate outcome contains {} uncertain rows",
                result.uncertain_count
            ));
        }
        let compact_h3 = match resolved_spec.group_keys.as_slice() {
            [key]
                if key.encoding == GroupKeyEncoding::Hash
                    && matches!(key.source, GroupKeySource::H3CellToParent { .. }) =>
            {
                Some(key.type_oid)
            }
            _ => None,
        };
        let (decoder, active_groups) = if let Some(type_oid) = compact_h3 {
            if !domains.is_empty() {
                return Err("compact H3 output must not carry a host dictionary".to_owned());
            }
            if storage.active_groups().is_some() {
                return Err("compact H3 output unexpectedly returned a dense bitmap".to_owned());
            }
            if storage.key_type(0) != Some(PgaccelValTag::Int64 as i32) {
                return Err(
                    "compact H3 output key is not an unsigned 64-bit physical lane".to_owned(),
                );
            }
            let key_values = storage
                .key_values(0)
                .ok_or_else(|| "compact H3 output key buffer is missing".to_owned())?;
            validate_h3_compact_key_buffers(
                key_values,
                storage.key_nulls(0),
                result.group_capacity,
            )?;
            (
                GroupDecoder::H3Compact { type_oid },
                (0..result.emitted_group_count).collect(),
            )
        } else {
            if domains.len() != resolved_spec.group_keys.len() {
                return Err(format!(
                    "group decoder count {} does not match query key count {}",
                    domains.len(),
                    resolved_spec.group_keys.len()
                ));
            }
            let mut expected_capacity = 1_usize;
            for (key_index, (key, domain)) in resolved_spec
                .group_keys
                .iter()
                .zip(domains.iter())
                .enumerate()
            {
                if key.type_oid != domain.type_oid || key.collation_oid != domain.collation_oid {
                    return Err(format!(
                        "group decoder {key_index} type/collation does not match the resolved query spec"
                    ));
                }
                let domain_cardinality = domain.cardinality()?;
                match key.encoding {
                    GroupKeyEncoding::DictionaryI32 {
                        cardinality,
                        null_code,
                    } if cardinality == domain_cardinality && null_code == domain.null_code => {}
                    _ => {
                        return Err(format!(
                            "group decoder {key_index} does not match its resolved dictionary encoding"
                        ));
                    }
                }
                expected_capacity = expected_capacity
                    .checked_mul(domain_cardinality as usize)
                    .ok_or_else(|| "group decoder radix product overflows usize".to_owned())?;
            }
            if expected_capacity != result.group_capacity {
                return Err(format!(
                    "group decoder capacity {expected_capacity} does not match kernel capacity {}",
                    result.group_capacity
                ));
            }
            let active = storage.active_groups().ok_or_else(|| {
                "generic grouped aggregate did not return dense active groups".to_owned()
            })?;
            if active.len() != result.group_capacity || active.iter().any(|value| *value > 1) {
                return Err("dense active-group bitmap has an invalid shape/value".to_owned());
            }
            let active_groups = active
                .iter()
                .enumerate()
                .filter_map(|(group, active)| (*active != 0).then_some(group))
                .collect::<Vec<_>>();
            if active_groups.len() != result.emitted_group_count {
                return Err(format!(
                    "dense active-group count {} does not match emitted group count {}",
                    active_groups.len(),
                    result.emitted_group_count
                ));
            }
            (GroupDecoder::Dense(domains), active_groups)
        };

        Ok(Self {
            storage,
            decoder,
            projection,
            active_groups,
            cursor: 0,
        })
    }

    /// Emit the next active group into `result_slot`, or return NULL at EOF.
    ///
    /// # Safety
    ///
    /// Must run on PostgreSQL's main backend thread. `result_slot` must be a
    /// valid slot with the exact tuple descriptor represented by `projection`.
    pub(super) unsafe fn next(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.cursor >= self.active_groups.len() {
            return std::ptr::null_mut();
        }
        if result_slot.is_null() {
            pgrx::error!(
                "pg_accel: generic grouped aggregate received a NULL result slot; refusing CPU fallback"
            );
        }

        // SAFETY: caller guarantees result_slot is a valid TupleTableSlot.
        let tuple_desc = unsafe { (*result_slot).tts_tupleDescriptor };
        if tuple_desc.is_null() {
            pgrx::error!(
                "pg_accel: generic grouped aggregate result slot has no tuple descriptor; refusing CPU fallback"
            );
        }
        // SAFETY: tuple_desc is non-NULL and belongs to result_slot.
        let natts = unsafe { (*tuple_desc).natts as usize };
        if natts != self.projection.slots.len() {
            pgrx::error!(
                "pg_accel: generic grouped aggregate slot has {natts} attributes, projection has {}; refusing CPU fallback",
                self.projection.slots.len()
            );
        }
        // SAFETY: caller guarantees a fully initialized slot.
        if unsafe { (*result_slot).tts_values.is_null() || (*result_slot).tts_isnull.is_null() } {
            pgrx::error!(
                "pg_accel: generic grouped aggregate result slot has no value storage; refusing CPU fallback"
            );
        }

        let group = self.active_groups[self.cursor];
        let group_codes = match &self.decoder {
            GroupDecoder::Dense(domains) => decode_group_codes(domains, group)
                .unwrap_or_else(|error| raise_materialize_error(MaterializeError::invalid(error))),
            GroupDecoder::H3Compact { .. } => Vec::new(),
        };

        // SAFETY: caller guarantees result_slot is a valid initialized slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };
        for (slot_index, projection) in self.projection.slots.iter().enumerate() {
            let (datum, is_null) = self
                .slot_datum(projection, &group_codes, group)
                .unwrap_or_else(|error| raise_materialize_error(error));
            // SAFETY: exact arity was checked above, so slot_index is in-bounds.
            unsafe {
                *(*result_slot).tts_values.add(slot_index) = datum;
                *(*result_slot).tts_isnull.add(slot_index) = is_null;
            }
        }
        self.cursor += 1;
        // SAFETY: every attribute was initialized above.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }

    fn slot_datum(
        &self,
        projection: &AggOutputSlot,
        group_codes: &[i32],
        group: usize,
    ) -> Result<(pg_sys::Datum, bool), MaterializeError> {
        match projection.source {
            AggOutputSource::GroupKey { key_index } => {
                let key_index = usize::try_from(key_index).map_err(|_| {
                    MaterializeError::invalid("group-key projection index exceeds usize")
                })?;
                if let GroupDecoder::H3Compact { type_oid } = &self.decoder {
                    if key_index != 0 {
                        return Err(MaterializeError::invalid(format!(
                            "compact H3 projection references missing group key {key_index}"
                        )));
                    }
                    return self.h3_group_datum(projection, *type_oid, group);
                }
                let code = *group_codes.get(key_index).ok_or_else(|| {
                    MaterializeError::invalid(format!(
                        "projection references missing group key {key_index}"
                    ))
                })?;
                let GroupDecoder::Dense(domains) = &self.decoder else {
                    unreachable!("compact H3 output returned above")
                };
                let domain = domains.get(key_index).ok_or_else(|| {
                    MaterializeError::invalid(format!(
                        "group decoder is missing domain {key_index}"
                    ))
                })?;
                let value = domain.decode(code).map_err(MaterializeError::invalid)?;
                // SAFETY: text allocation, when needed, occurs on the main
                // backend thread under next()'s contract.
                unsafe { group_datum(projection, domain, value) }
            }
            AggOutputSource::Aggregate {
                measure_index,
                source,
                kind,
            } => {
                let measure_index = usize::try_from(measure_index).map_err(|_| {
                    MaterializeError::invalid("aggregate projection index exceeds usize")
                })?;
                self.aggregate_datum(
                    measure_index,
                    source,
                    kind,
                    projection.source_type_oid,
                    group,
                )
            }
        }
    }

    fn h3_group_datum(
        &self,
        projection: &AggOutputSlot,
        type_oid: u32,
        group: usize,
    ) -> Result<(pg_sys::Datum, bool), MaterializeError> {
        if projection.result_type_oid != type_oid
            || projection.source_type_oid != type_oid
            || projection.result_typmod != -1
            || projection.result_collation_oid != 0
        {
            return Err(MaterializeError::invalid(
                "compact H3 projection does not match the catalog-proved h3index type",
            ));
        }
        let values = self
            .storage
            .key_values(0)
            .ok_or_else(|| MaterializeError::invalid("compact H3 key buffer is missing"))?;
        decode_h3_key(values, self.storage.key_nulls(0), group)
    }

    fn aggregate_datum(
        &self,
        measure_index: usize,
        source: AggregateSource,
        kind: AggregateKind,
        source_type_oid: u32,
        group: usize,
    ) -> Result<(pg_sys::Datum, bool), MaterializeError> {
        if source != AggregateSource::Value {
            return Err(MaterializeError::invalid(
                "Phase 5D output cannot materialize RHS aggregate lanes",
            ));
        }
        if kind == AggregateKind::Count {
            let count = self
                .storage
                .measure_count(measure_index)
                .and_then(|counts| counts.get(group))
                .copied()
                .ok_or_else(|| {
                    MaterializeError::invalid(format!(
                        "measure {measure_index} is missing its COUNT state for group {group}"
                    ))
                })?;
            let count = i64::try_from(count).map_err(|_| MaterializeError::CountOverflow {
                measure_index,
                count,
            })?;
            return Ok((pg_sys::Datum::from(count), false));
        }

        let nonnull_count = self
            .storage
            .measure_nonnull_count(measure_index)
            .and_then(|counts| counts.get(group))
            .copied()
            .ok_or_else(|| {
                MaterializeError::invalid(format!(
                    "measure {measure_index} is missing its non-NULL count for group {group}"
                ))
            })?;
        if nonnull_count == 0 {
            return Ok((pg_sys::Datum::from(0_usize), true));
        }

        let lane = match kind {
            AggregateKind::Sum => GroupedAggStateLane::Sum,
            AggregateKind::Min => GroupedAggStateLane::Min,
            AggregateKind::Max => GroupedAggStateLane::Max,
            AggregateKind::Count => unreachable!("COUNT returned above"),
            AggregateKind::Avg | AggregateKind::StddevSamp => {
                return Err(MaterializeError::invalid(
                    "Phase 5D output supports only COUNT, SUM, MIN, and MAX",
                ));
            }
        };

        match source_type_oid {
            INT4OID => {
                let value = self
                    .storage
                    .measure_i64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read I64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })?;
                match kind {
                    AggregateKind::Sum => Ok((pg_sys::Datum::from(value), false)),
                    AggregateKind::Min | AggregateKind::Max => {
                        let value = i32::try_from(value).map_err(|_| {
                            MaterializeError::invalid(format!(
                                "INT4 aggregate state {value} is outside the PostgreSQL int4 domain"
                            ))
                        })?;
                        Ok((pg_sys::Datum::from(value), false))
                    }
                    _ => unreachable!("aggregate kind was classified above"),
                }
            }
            INT8OID if matches!(kind, AggregateKind::Min | AggregateKind::Max) => {
                let value = self
                    .storage
                    .measure_i64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read I64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })?;
                Ok((pg_sys::Datum::from(value), false))
            }
            FLOAT8OID if matches!(kind, AggregateKind::Min | AggregateKind::Max) => {
                let value = self
                    .storage
                    .measure_f64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read F64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })?;
                Ok((pg_sys::Datum::from(value.to_bits()), false))
            }
            _ => Err(MaterializeError::invalid(format!(
                "aggregate output type OID {source_type_oid} and operation {kind:?} are not supported"
            ))),
        }
    }
}

fn validate_h3_compact_key_buffers(
    values: &[u8],
    nulls: Option<&[u8]>,
    group_capacity: usize,
) -> Result<(), String> {
    let expected_key_bytes = group_capacity
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or_else(|| "compact H3 key byte count overflows usize".to_owned())?;
    if values.len() != expected_key_bytes {
        return Err("compact H3 output key buffer has an invalid shape".to_owned());
    }
    let Some(nulls) = nulls else {
        return Ok(());
    };
    if nulls.len() != group_capacity || nulls.iter().any(|value| *value > 1) {
        return Err("compact H3 output NULL sidecar has an invalid shape/value".to_owned());
    }
    if nulls
        .iter()
        .zip(values.chunks_exact(std::mem::size_of::<u64>()))
        .any(|(is_null, payload)| *is_null == 1 && payload.iter().any(|byte| *byte != 0))
    {
        return Err("compact H3 NULL key payload is not canonical zero".to_owned());
    }
    Ok(())
}

fn decode_h3_key(
    values: &[u8],
    nulls: Option<&[u8]>,
    group: usize,
) -> Result<(pg_sys::Datum, bool), MaterializeError> {
    if let Some(nulls) = nulls {
        match nulls.get(group).copied() {
            Some(1) => return Ok((pg_sys::Datum::from(0_usize), true)),
            Some(0) => {}
            Some(value) => {
                return Err(MaterializeError::invalid(format!(
                    "compact H3 NULL sidecar contains noncanonical value {value}"
                )));
            }
            None => {
                return Err(MaterializeError::invalid(
                    "compact H3 NULL sidecar is shorter than emitted groups",
                ));
            }
        }
    }
    let offset = group
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or_else(|| MaterializeError::invalid("compact H3 key offset overflow"))?;
    let end = offset
        .checked_add(std::mem::size_of::<u64>())
        .ok_or_else(|| MaterializeError::invalid("compact H3 key end overflow"))?;
    let bytes: [u8; 8] = values
        .get(offset..end)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| MaterializeError::invalid("compact H3 key index is out of bounds"))?;
    Ok((pg_sys::Datum::from(u64::from_ne_bytes(bytes)), false))
}

fn decode_group_codes(domains: &[GroupDomain], group: usize) -> Result<Vec<i32>, String> {
    let mut remainder = group;
    let mut codes = vec![0_i32; domains.len()];
    for (index, domain) in domains.iter().enumerate().rev() {
        let cardinality = domain.values.len();
        if cardinality == 0 {
            return Err(format!("group domain {index} has zero cardinality"));
        }
        let digit = remainder % cardinality;
        remainder /= cardinality;
        codes[index] =
            i32::try_from(digit).map_err(|_| format!("group domain {index} code exceeds i32"))?;
    }
    if remainder != 0 {
        return Err(format!(
            "dense group index {group} exceeds its mixed-radix decoder capacity"
        ));
    }
    Ok(codes)
}

unsafe fn group_datum(
    projection: &AggOutputSlot,
    domain: &GroupDomain,
    value: &GroupDatum,
) -> Result<(pg_sys::Datum, bool), MaterializeError> {
    if projection.result_type_oid != domain.type_oid {
        return Err(MaterializeError::invalid(format!(
            "group output type OID {} does not match decoder type OID {}",
            projection.result_type_oid, domain.type_oid
        )));
    }
    let result_type_oid = projection.result_type_oid;
    let datum = match (result_type_oid, value) {
        (_, GroupDatum::Null) => return Ok((pg_sys::Datum::from(0_usize), true)),
        (BOOLOID, GroupDatum::Bool(value)) => pg_sys::Datum::from(usize::from(*value)),
        (INT2OID, GroupDatum::I32(value)) => {
            let value = i16::try_from(*value).map_err(|_| {
                MaterializeError::invalid(format!(
                    "group key {value} is outside the PostgreSQL int2 domain"
                ))
            })?;
            pg_sys::Datum::from(value)
        }
        (INT4OID | DATEOID, GroupDatum::I32(value)) => pg_sys::Datum::from(*value),
        (INT8OID | TIMESTAMPOID | TIMESTAMPTZOID, GroupDatum::I64(value)) => {
            pg_sys::Datum::from(*value)
        }
        (FLOAT4OID, GroupDatum::F32(value)) => pg_sys::Datum::from(value.to_bits() as usize),
        (FLOAT8OID, GroupDatum::F64(value)) => pg_sys::Datum::from(value.to_bits()),
        (TEXTOID | VARCHAROID | BPCHAROID, GroupDatum::Text(value)) => {
            // Use the result type's catalog input function so VARCHAR/BPCHAR
            // typmods apply exactly as PostgreSQL would apply them.
            // SAFETY: caller guarantees execution on PostgreSQL's main thread.
            unsafe { text_input_datum(value, result_type_oid, projection.result_typmod)? }
        }
        (_, GroupDatum::Unused) => {
            return Err(MaterializeError::invalid(
                "kernel marked an unused group dictionary entry active",
            ));
        }
        (type_oid, value) => {
            return Err(MaterializeError::invalid(format!(
                "group datum {value:?} does not match PostgreSQL type OID {type_oid}"
            )));
        }
    };
    Ok((datum, false))
}

unsafe fn text_input_datum(
    value: &str,
    result_type_oid: u32,
    result_typmod: i32,
) -> Result<pg_sys::Datum, MaterializeError> {
    let input = CString::new(value).map_err(|_| {
        MaterializeError::invalid("group text value contains an embedded zero byte")
    })?;
    let result_type_oid = pg_sys::Oid::from(result_type_oid);
    let mut input_function = pg_sys::InvalidOid;
    let mut io_parameter = pg_sys::InvalidOid;
    // SAFETY: all pointers are valid, and AOP validation proved a real result
    // type OID before output materialization begins.
    unsafe {
        pg_sys::getTypeInputInfo(
            result_type_oid,
            std::ptr::from_mut(&mut input_function),
            std::ptr::from_mut(&mut io_parameter),
        );
    }
    if input_function == pg_sys::InvalidOid {
        return Err(MaterializeError::invalid(format!(
            "PostgreSQL type OID {} has no input function",
            u32::from(result_type_oid)
        )));
    }
    // SAFETY: input is NUL-terminated for the duration of the synchronous
    // catalog input-function call; the OIDs came from getTypeInputInfo.
    Ok(unsafe {
        pg_sys::OidInputFunctionCall(
            input_function,
            input.as_ptr().cast_mut(),
            io_parameter,
            result_typmod,
        )
    })
}

fn raise_materialize_error(error: MaterializeError) -> ! {
    match error {
        MaterializeError::Invalid(message) => {
            pgrx::error!(
                "pg_accel: generic grouped aggregate output is invalid ({message}); refusing CPU fallback"
            )
        }
        MaterializeError::CountOverflow {
            measure_index,
            count,
        } => {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                format!(
                    "pg_accel: COUNT result {count} for measure {measure_index} exceeds PostgreSQL bigint; refusing CPU fallback"
                )
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(cardinality: usize) -> GroupDomain {
        GroupDomain {
            type_oid: INT4OID,
            collation_oid: 0,
            values: (0..cardinality)
                .map(|value| GroupDatum::I32(value as i32))
                .collect(),
            null_code: None,
        }
    }

    #[test]
    fn reverses_dense_mixed_radix_in_key_order() {
        let domains = [domain(2), domain(3), domain(4)];
        assert_eq!(decode_group_codes(&domains, 23), Ok(vec![1, 2, 3]));
        assert_eq!(decode_group_codes(&domains, 0), Ok(vec![0, 0, 0]));
    }

    #[test]
    fn rejects_group_outside_decoder_capacity() {
        let domains = [domain(2), domain(3)];
        assert!(decode_group_codes(&domains, 6).is_err());
    }

    #[test]
    fn zero_key_global_group_has_one_decoder_value() {
        assert_eq!(decode_group_codes(&[], 0), Ok(Vec::new()));
        assert!(decode_group_codes(&[], 1).is_err());
    }

    #[test]
    fn h3_decoder_roundtrips_unsigned_high_bits() {
        let mut values = Vec::new();
        values.extend_from_slice(&(1_u64 << 63).to_ne_bytes());
        values.extend_from_slice(&u64::MAX.to_ne_bytes());

        for (group, expected) in [(0, 1_u64 << 63), (1, u64::MAX)] {
            let (datum, is_null) =
                decode_h3_key(&values, None, group).expect("valid compact H3 key");
            assert!(!is_null);
            assert_eq!(datum.value() as u64, expected);
        }
    }

    #[test]
    fn h3_decoder_honors_sql_null_before_payload() {
        let (datum, is_null) =
            decode_h3_key(&u64::MAX.to_ne_bytes(), Some(&[1]), 0).expect("canonical NULL marker");
        assert!(is_null);
        assert_eq!(datum.value(), 0);
    }

    #[test]
    fn h3_decoder_rejects_noncanonical_null_byte() {
        assert!(decode_h3_key(&0_u64.to_ne_bytes(), Some(&[2]), 0).is_err());
    }

    #[test]
    fn h3_decoder_rejects_short_key_and_null_buffers() {
        assert!(decode_h3_key(&[0; 7], None, 0).is_err());
        assert!(decode_h3_key(&0_u64.to_ne_bytes(), Some(&[]), 0).is_err());
    }

    #[test]
    fn h3_compact_boundary_requires_zero_null_payload() {
        assert!(validate_h3_compact_key_buffers(&0_u64.to_ne_bytes(), Some(&[1]), 1).is_ok());
        assert!(validate_h3_compact_key_buffers(&u64::MAX.to_ne_bytes(), Some(&[1]), 1).is_err());
    }
}

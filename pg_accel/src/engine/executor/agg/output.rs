//! Materialization of generic grouped-aggregate results into PostgreSQL slots.

use std::ffi::CString;
use std::rc::Rc;

use pgrx::{AnyNumeric, IntoDatum, pg_sys};

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
const NUMERICOID: u32 = 1700;

/// PostgreSQL owns binary NUMERIC decoding and final AVG division. The GPU
/// has already performed every input-row transition and group reduction.
pub fn materialize_numeric_sum(
    mut wire: Vec<u8>,
    average_count: Option<i64>,
) -> Result<pg_sys::Datum, String> {
    let length = i32::try_from(wire.len()).map_err(|_| "NUMERIC result wire length exceeds i32")?;
    if average_count.is_some_and(|count| count <= 0) {
        return Err("NUMERIC AVG requires a positive non-NULL count".to_owned());
    }
    let mut input = pg_sys::StringInfoData {
        data: wire.as_mut_ptr().cast(),
        len: length,
        maxlen: length,
        cursor: 0,
    };
    // SAFETY: numeric_recv consumes this live, bounded StringInfo synchronously
    // and returns a fresh PostgreSQL NUMERIC; it does not retain or enlarge the
    // Rust-owned wire buffer. InvalidOid and -1 select unconstrained NUMERIC.
    let numeric = unsafe {
        pgrx::direct_function_call_as_datum(
            pg_sys::numeric_recv,
            &[
                Some(pg_sys::Datum::from(std::ptr::from_mut(&mut input))),
                Some(pg_sys::Datum::from(pg_sys::InvalidOid)),
                (-1_i32).into_datum(),
            ],
        )
    }
    .ok_or("PostgreSQL numeric_recv returned NULL")?;
    if input.cursor != length {
        return Err("NUMERIC receiver did not consume the complete result".to_owned());
    }
    let Some(count) = average_count else {
        return Ok(numeric);
    };
    // SAFETY: count is a positive int64, and numeric is the live, flat NUMERIC
    // returned above. These built-ins allocate fresh results in the active
    // PostgreSQL memory context; no Rust-owned varlena representation is used.
    let denominator =
        unsafe { pgrx::direct_function_call_as_datum(pg_sys::int8_numeric, &[count.into_datum()]) }
            .ok_or("PostgreSQL int8_numeric returned NULL")?;
    // SAFETY: both arguments are live NUMERIC Datums. PostgreSQL controls exact
    // division precision, rounding, special values and overflow SQLSTATE.
    let result = unsafe {
        pgrx::direct_function_call_as_datum(
            pg_sys::numeric_div,
            &[Some(numeric), Some(denominator)],
        )
    }
    .ok_or("PostgreSQL numeric_div returned NULL")?;
    // SAFETY: recv/int8_numeric created these two distinct palloc allocations;
    // numeric_div's result is a new allocation and retains neither input.
    unsafe {
        pg_sys::pfree(numeric.cast_mut_ptr());
        pg_sys::pfree(denominator.cast_mut_ptr());
    }
    Ok(result)
}

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
    CompactI64 { type_oid: u32 },
}

/// Compact outputs are contiguous; dense outputs already own a validated
/// activity bitmap. Neither needs a second, per-group index allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroupCursor {
    next: usize,
    remaining: usize,
}

impl GroupCursor {
    const fn new(emitted: usize) -> Self {
        Self {
            next: 0,
            remaining: emitted,
        }
    }

    fn peek(self, bitmap: Option<&[u8]>) -> Result<Option<usize>, String> {
        if self.remaining == 0 {
            return Ok(None);
        }
        match bitmap {
            None => Ok(Some(self.next)),
            Some(bitmap) => bitmap
                .get(self.next..)
                .and_then(|tail| tail.iter().position(|active| *active != 0))
                .map(|offset| Some(self.next + offset))
                .ok_or_else(|| "dense active-group bitmap ended before emitted count".to_owned()),
        }
    }

    fn advance(&mut self, group: usize) -> Result<(), String> {
        if group < self.next {
            return Err("group cursor moved backwards".into());
        }
        let next = group.checked_add(1).ok_or("group cursor overflow")?;
        let remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("group cursor advanced after EOF")?;
        self.next = next;
        self.remaining = remaining;
        Ok(())
    }
}

fn dense_group_cursor(
    active: &[u8],
    capacity: usize,
    emitted: usize,
) -> Result<GroupCursor, String> {
    if active.len() != capacity {
        return Err("dense active-group bitmap has an invalid shape/value".into());
    }
    let count = active
        .iter()
        .try_fold(0_usize, |count, value| match value {
            0 => Ok(count),
            1 => Ok(count + 1),
            _ => Err("dense active-group bitmap has an invalid shape/value".to_owned()),
        })?;
    if count != emitted {
        return Err(format!(
            "dense active-group count {count} does not match emitted group count {emitted}"
        ));
    }
    Ok(GroupCursor::new(emitted))
}

/// Owns one complete device result and emits its materialized groups.
pub(super) struct DescriptorAggOutput {
    storage: GroupedAggOutputStorage,
    decoder: GroupDecoder,
    projection: AggOutputProjection,
    cursor: GroupCursor,
}

impl DescriptorAggOutput {
    pub(super) fn new(
        storage: GroupedAggOutputStorage,
        outcome: GroupedAggOutcome,
        domains: Rc<[GroupDomain]>,
        resolved_spec: &AggQuerySpec,
        projection: AggOutputProjection,
    ) -> Result<Self, String> {
        projection
            .validate(resolved_spec)
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
        let compact_i64 = match resolved_spec.group_keys.as_slice() {
            [key]
                if key.encoding == GroupKeyEncoding::Hash
                    && (matches!(key.source, GroupKeySource::H3CellToParent { .. })
                        || matches!(key.source, GroupKeySource::FactColumn(column)
                            if matches!(column.type_oid, INT2OID | INT4OID | INT8OID)
                                && key.type_oid == column.type_oid
                                && column.relation_oid == resolved_spec.fact_rel
                                && column.attno > 0 && key.collation_oid == 0)) =>
            {
                Some(key.type_oid)
            }
            _ => None,
        };
        let (decoder, cursor) = if let Some(type_oid) = compact_i64 {
            if !domains.is_empty() {
                return Err("compact hash output must not carry a host dictionary".to_owned());
            }
            if storage.active_groups().is_some() {
                return Err("compact hash output unexpectedly returned a dense bitmap".to_owned());
            }
            if storage.key_type(0) != Some(PgaccelValTag::Int64 as i32) {
                return Err(
                    "compact hash output key is not an unsigned 64-bit physical lane".to_owned(),
                );
            }
            let key_values = storage
                .key_values(0)
                .ok_or_else(|| "compact hash output key buffer is missing".to_owned())?;
            validate_compact_integer_key_buffers(
                key_values,
                storage.key_nulls(0),
                result.group_capacity,
                result.emitted_group_count,
                type_oid,
            )?;
            (
                GroupDecoder::CompactI64 { type_oid },
                GroupCursor::new(result.emitted_group_count),
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
            let cursor =
                dense_group_cursor(active, result.group_capacity, result.emitted_group_count)?;
            (GroupDecoder::Dense(domains), cursor)
        };

        Ok(Self {
            storage,
            decoder,
            projection,
            cursor,
        })
    }

    /// Inspect a global INT8 result through the same finalization used by
    /// next(). Only available to live backend tests, never production callers.
    #[cfg(feature = "pg_test")]
    pub(super) fn test_global_int8_values(&self) -> Result<Vec<Option<i64>>, String> {
        if !matches!(&self.decoder, GroupDecoder::Dense(domains) if domains.is_empty())
            || self.storage.active_groups() != Some(&[1][..])
        {
            return Err("fixture requires one global output group".into());
        }
        self.projection
            .slots
            .iter()
            .map(|slot| {
                if slot.result_type_oid != INT8OID {
                    return Err("fixture requires INT8 output slots".into());
                }
                let (datum, is_null) = self
                    .slot_datum(slot, &[], 0)
                    .map_err(|error| format!("global result conversion failed: {error:?}"))?;
                // SAFETY: slot_datum produced a by-value INT8 Datum under this live
                // backend's projection; nullability is passed through unchanged.
                Ok(unsafe { <i64 as pgrx::FromDatum>::from_datum(datum, is_null) })
            })
            .collect()
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
        if self.cursor.remaining == 0 {
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

        let group = self
            .cursor
            .peek(self.storage.active_groups())
            .unwrap_or_else(|error| raise_materialize_error(MaterializeError::invalid(error)))
            .expect("nonempty cursor returned a group");
        let mut codes = [0_i32; crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_KEYS];
        let group_codes = match &self.decoder {
            GroupDecoder::Dense(domains) => {
                decode_group_codes_into(domains, group, &mut codes).unwrap_or_else(|error| {
                    raise_materialize_error(MaterializeError::invalid(error))
                });
                &codes[..domains.len()]
            }
            GroupDecoder::CompactI64 { .. } => &[],
        };

        // SAFETY: caller guarantees result_slot is a valid initialized slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };
        for (slot_index, projection) in self.projection.slots.iter().enumerate() {
            let (datum, is_null) = self
                .slot_datum(projection, group_codes, group)
                .unwrap_or_else(|error| raise_materialize_error(error));
            // SAFETY: exact arity was checked above, so slot_index is in-bounds.
            unsafe {
                *(*result_slot).tts_values.add(slot_index) = datum;
                *(*result_slot).tts_isnull.add(slot_index) = is_null;
            }
        }
        self.cursor
            .advance(group)
            .unwrap_or_else(|error| raise_materialize_error(MaterializeError::invalid(error)));
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
                if let GroupDecoder::CompactI64 { type_oid } = &self.decoder {
                    if key_index != 0 {
                        return Err(MaterializeError::invalid(format!(
                            "compact hash projection references missing group key {key_index}"
                        )));
                    }
                    return self.compact_i64_group_datum(projection, *type_oid, group);
                }
                let code = *group_codes.get(key_index).ok_or_else(|| {
                    MaterializeError::invalid(format!(
                        "projection references missing group key {key_index}"
                    ))
                })?;
                let GroupDecoder::Dense(domains) = &self.decoder else {
                    unreachable!("compact hash output returned above")
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

    fn compact_i64_group_datum(
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
                "compact hash projection does not match the catalog-proved key type",
            ));
        }
        let values = self
            .storage
            .key_values(0)
            .ok_or_else(|| MaterializeError::invalid("compact hash key buffer is missing"))?;
        decode_compact_integer_key(values, self.storage.key_nulls(0), group, type_oid)
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

        if matches!(source_type_oid, NUMERICOID | INT8OID)
            && matches!(kind, AggregateKind::Sum | AggregateKind::Avg)
        {
            let wire = self
                .storage
                .measure_numeric_wire_at(measure_index, group)
                .map_err(|error| {
                    MaterializeError::invalid(format!("invalid NUMERIC sum state: {error}"))
                })?;
            let count =
                i64::try_from(nonnull_count).map_err(|_| MaterializeError::CountOverflow {
                    measure_index,
                    count: nonnull_count,
                })?;
            let datum =
                materialize_numeric_sum(wire, (kind == AggregateKind::Avg).then_some(count))
                    .map_err(MaterializeError::invalid)?;
            return Ok((datum, false));
        }

        if kind == AggregateKind::Avg {
            if !matches!(source_type_oid, INT2OID | INT4OID) {
                return Err(MaterializeError::invalid(format!(
                    "exact integer AVG does not support source type OID {source_type_oid}"
                )));
            }
            let sum = self
                .storage
                .measure_i64_at(measure_index, GroupedAggStateLane::Sum, group)
                .map_err(|error| {
                    MaterializeError::invalid(format!(
                        "could not read I64 AVG sum state for measure {measure_index}: {error}"
                    ))
                })?;
            let count =
                i64::try_from(nonnull_count).map_err(|_| MaterializeError::CountOverflow {
                    measure_index,
                    count: nonnull_count,
                })?;
            let average = AnyNumeric::from(sum) / AnyNumeric::from(count);
            let datum = average.into_datum().ok_or_else(|| {
                MaterializeError::invalid("PostgreSQL numeric division returned NULL for AVG")
            })?;
            return Ok((datum, false));
        }

        let lane = match kind {
            AggregateKind::Sum => GroupedAggStateLane::Sum,
            AggregateKind::Min => GroupedAggStateLane::Min,
            AggregateKind::Max => GroupedAggStateLane::Max,
            AggregateKind::Count => unreachable!("COUNT returned above"),
            AggregateKind::Avg => unreachable!("AVG returned above"),
            AggregateKind::StddevSamp => {
                return Err(MaterializeError::invalid(
                    "Phase 5D output does not materialize STDDEV_SAMP",
                ));
            }
        };

        match source_type_oid {
            INT2OID if kind == AggregateKind::Sum => {
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
            INT2OID if matches!(kind, AggregateKind::Min | AggregateKind::Max) => {
                let value = self
                    .storage
                    .measure_i64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read I64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })?;
                let value = i16::try_from(value).map_err(|_| {
                    MaterializeError::invalid(format!(
                        "INT2 aggregate state {value} is outside the PostgreSQL int2 domain"
                    ))
                })?;
                Ok((pg_sys::Datum::from(value), false))
            }
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
            DATEOID if matches!(kind, AggregateKind::Min | AggregateKind::Max) => {
                let value = self
                    .storage
                    .measure_i64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read I64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })?;
                let value = i32::try_from(value).map_err(|_| {
                    MaterializeError::invalid(format!(
                        "DATE aggregate state {value} is outside the PostgreSQL date domain"
                    ))
                })?;
                Ok((pg_sys::Datum::from(value), false))
            }
            TIMESTAMPOID | TIMESTAMPTZOID
                if matches!(kind, AggregateKind::Min | AggregateKind::Max) =>
            {
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
            FLOAT4OID if matches!(kind, AggregateKind::Min | AggregateKind::Max) => {
                let value = self
                    .storage
                    .measure_f64_at(measure_index, lane, group)
                    .map_err(|error| {
                        MaterializeError::invalid(format!(
                            "could not read F64 aggregate state for measure {measure_index}: {error}"
                        ))
                    })? as f32;
                Ok((pg_sys::Datum::from(value.to_bits() as usize), false))
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

pub(super) fn validate_compact_integer_key_buffers(
    values: &[u8],
    nulls: Option<&[u8]>,
    group_capacity: usize,
    emitted_group_count: usize,
    type_oid: u32,
) -> Result<(), String> {
    let expected_key_bytes = group_capacity
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or_else(|| "compact hash key byte count overflows usize".to_owned())?;
    if values.len() != expected_key_bytes {
        return Err("compact hash output key buffer has an invalid shape".to_owned());
    }
    if emitted_group_count > group_capacity {
        return Err("compact hash emitted key count exceeds capacity".to_owned());
    }
    if let Some(nulls) = nulls {
        if nulls.len() != group_capacity || nulls.iter().any(|value| *value > 1) {
            return Err("compact hash output NULL sidecar has an invalid shape/value".to_owned());
        }
        if nulls
            .iter()
            .zip(values.chunks_exact(std::mem::size_of::<u64>()))
            .any(|(is_null, payload)| *is_null == 1 && payload.iter().any(|byte| *byte != 0))
        {
            return Err("compact hash NULL key payload is not canonical zero".to_owned());
        }
    }

    let mut previous_nonnull = None;
    for (index, payload) in values
        .chunks_exact(std::mem::size_of::<u64>())
        .take(emitted_group_count)
        .enumerate()
    {
        let is_null = nulls.is_some_and(|nulls| nulls[index] == 1);
        if is_null {
            if index != 0 {
                return Err("compact hash NULL key is not the first emitted group".to_owned());
            }
            continue;
        }
        let key_bytes: [u8; std::mem::size_of::<u64>()] = payload
            .try_into()
            .map_err(|_| "compact hash key payload has an invalid width".to_owned())?;
        let key = u64::from_ne_bytes(key_bytes);
        let signed = key as i64;
        if (type_oid == INT2OID && i16::try_from(signed).is_err())
            || (type_oid == INT4OID && i32::try_from(signed).is_err())
        {
            return Err("compact hash key is outside its logical integer domain".into());
        }
        if previous_nonnull.is_some_and(|previous| key <= previous) {
            return Err(
                "compact hash non-NULL keys are not strictly unsigned ascending".to_owned(),
            );
        }
        previous_nonnull = Some(key);
    }
    Ok(())
}

#[cfg(test)]
fn validate_compact_i64_key_buffers(
    values: &[u8],
    nulls: Option<&[u8]>,
    group_capacity: usize,
    emitted_group_count: usize,
) -> Result<(), String> {
    validate_compact_integer_key_buffers(
        values,
        nulls,
        group_capacity,
        emitted_group_count,
        INT8OID,
    )
}

fn decode_compact_integer_key(
    values: &[u8],
    nulls: Option<&[u8]>,
    group: usize,
    type_oid: u32,
) -> Result<(pg_sys::Datum, bool), MaterializeError> {
    let (datum, is_null) = decode_compact_i64_key(values, nulls, group)?;
    if is_null {
        return Ok((datum, true));
    }
    let value = datum.value() as i64;
    let invalid =
        || MaterializeError::invalid("compact hash key exceeds its logical integer domain");
    let datum = match type_oid {
        INT2OID => pg_sys::Datum::from(i16::try_from(value).map_err(|_| invalid())?),
        INT4OID => pg_sys::Datum::from(i32::try_from(value).map_err(|_| invalid())?),
        _ => datum, // BIGINT or the separately catalog-proved H3 layout.
    };
    Ok((datum, false))
}

fn decode_compact_i64_key(
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
                    "compact hash NULL sidecar contains noncanonical value {value}"
                )));
            }
            None => {
                return Err(MaterializeError::invalid(
                    "compact hash NULL sidecar is shorter than emitted groups",
                ));
            }
        }
    }
    let offset = group
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or_else(|| MaterializeError::invalid("compact hash key offset overflow"))?;
    let end = offset
        .checked_add(std::mem::size_of::<u64>())
        .ok_or_else(|| MaterializeError::invalid("compact hash key end overflow"))?;
    let bytes: [u8; 8] = values
        .get(offset..end)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| MaterializeError::invalid("compact hash key index is out of bounds"))?;
    Ok((pg_sys::Datum::from(u64::from_ne_bytes(bytes)), false))
}

fn decode_group_codes_into(
    domains: &[GroupDomain],
    group: usize,
    codes: &mut [i32],
) -> Result<(), String> {
    if domains.len() > codes.len() {
        return Err("group decoder exceeds its fixed-width code buffer".into());
    }
    let mut remainder = group;
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
    Ok(())
}

#[cfg(test)]
fn decode_group_codes(domains: &[GroupDomain], group: usize) -> Result<Vec<i32>, String> {
    let mut codes = vec![0; domains.len()];
    decode_group_codes_into(domains, group, &mut codes)?;
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

    fn h3_key_bytes(keys: &[u64]) -> Vec<u8> {
        let mut values = Vec::with_capacity(std::mem::size_of_val(keys));
        for key in keys {
            values.extend_from_slice(&key.to_ne_bytes());
        }
        values
    }

    #[test]
    fn group_cursor_matches_every_small_bitmap_and_stable_eof() {
        for len in 0..=12 {
            for mask in 0..(1_usize << len) {
                let bitmap = (0..len)
                    .map(|bit| u8::from(mask & (1 << bit) != 0))
                    .collect::<Vec<_>>();
                let expected = bitmap
                    .iter()
                    .enumerate()
                    .filter_map(|(index, active)| (*active == 1).then_some(index))
                    .collect::<Vec<_>>();
                let mut cursor =
                    dense_group_cursor(&bitmap, len, expected.len()).expect("valid bitmap");
                for group in expected {
                    assert_eq!(cursor.peek(Some(&bitmap)), Ok(Some(group)));
                    assert_eq!(
                        cursor.peek(Some(&bitmap)),
                        Ok(Some(group)),
                        "peek must not consume a tuple"
                    );
                    cursor.advance(group).expect("consume one group");
                }
                assert_eq!(cursor.peek(Some(&bitmap)), Ok(None));
                assert_eq!(cursor.peek(Some(&bitmap)), Ok(None));
                assert_eq!(cursor.remaining, 0);
            }
        }
    }

    #[test]
    fn group_cursor_eliminates_compact_and_dense_index_allocations() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        for rows in [0_usize, 1, 65_537, 1_000_017] {
            begin_test_allocation_count();
            let previous = (0..rows).collect::<Vec<usize>>();
            std::hint::black_box(&previous);
            let old_allocations = finish_test_allocation_count();
            assert_eq!(old_allocations, usize::from(rows != 0));
            assert_eq!(
                std::mem::size_of_val(previous.as_slice()),
                rows * size_of::<usize>()
            );
            let expected = previous.iter().sum::<usize>();
            begin_test_allocation_count();
            let mut cursor = GroupCursor::new(rows);
            let mut actual = 0;
            while let Some(group) = cursor.peek(None).expect("compact cursor") {
                actual += group;
                cursor.advance(group).expect("compact advance");
            }
            std::hint::black_box(actual);
            let allocations = finish_test_allocation_count();
            assert_eq!(actual, expected);
            assert_eq!(allocations, 0);
        }
        let bitmap = (0..65_537)
            .map(|index| u8::from(index % 97 == 0))
            .collect::<Vec<_>>();
        begin_test_allocation_count();
        let previous = bitmap
            .iter()
            .enumerate()
            .filter_map(|(group, active)| (*active != 0).then_some(group))
            .collect::<Vec<_>>();
        std::hint::black_box(&previous);
        let old_allocations = finish_test_allocation_count();
        assert!(old_allocations > 0);
        begin_test_allocation_count();
        let mut cursor =
            dense_group_cursor(&bitmap, bitmap.len(), previous.len()).expect("sparse output");
        let mut actual = 0;
        while let Some(group) = cursor.peek(Some(&bitmap)).expect("dense cursor") {
            actual += group;
            cursor.advance(group).expect("dense advance");
        }
        std::hint::black_box(actual);
        let allocations = finish_test_allocation_count();
        assert_eq!(actual, previous.iter().sum::<usize>());
        assert_eq!(allocations, 0);
    }

    #[test]
    fn group_cursor_rejects_bad_metadata_without_advancing() {
        for (bitmap, capacity, emitted) in [
            (&[0_u8, 2][..], 2, 1),
            (&[1][..], 2, 1),
            (&[1, 0][..], 1, 1),
            (&[1][..], 1, 0),
            (&[0][..], 1, 1),
        ] {
            assert!(dense_group_cursor(bitmap, capacity, emitted).is_err());
        }
        let mut cursor = GroupCursor::new(1);
        assert!(cursor.peek(Some(&[])).is_err());
        assert!(cursor.advance(usize::MAX).is_err());
        assert_eq!(cursor, GroupCursor::new(1));
        cursor.advance(0).expect("valid advance");
        let exhausted = cursor;
        assert!(cursor.advance(1).is_err());
        assert!(cursor.advance(0).is_err());
        assert_eq!(cursor, exhausted);
        let mut last = GroupCursor {
            next: usize::MAX - 1,
            remaining: 1,
        };
        assert_eq!(last.peek(None), Ok(Some(usize::MAX - 1)));
        last.advance(usize::MAX - 1)
            .expect("maximum representable end");
        assert_eq!(last.peek(None), Ok(None));
    }

    #[test]
    fn fixed_width_group_decoder_avoids_per_tuple_allocation() {
        use crate::engine::residency::{begin_test_allocation_count, finish_test_allocation_count};
        let domains = [domain(3), domain(5), domain(7)];
        let rows = 3 * 5 * 7;
        begin_test_allocation_count();
        let mut previous = 0;
        for group in 0..rows {
            let codes = decode_group_codes(&domains, group).expect("reference decoder");
            previous += codes.iter().sum::<i32>();
            std::hint::black_box(codes);
        }
        let allocations = finish_test_allocation_count();
        assert_eq!(allocations, rows);
        begin_test_allocation_count();
        let mut actual = 0;
        let mut codes = [-1; crate::engine::spec::abi::PGACCEL_GROUPED_AGG_MAX_KEYS];
        for group in 0..rows {
            decode_group_codes_into(&domains, group, &mut codes).expect("stack decoder");
            assert_eq!(
                &codes[..3],
                &[
                    (group / 35) as i32,
                    ((group / 7) % 5) as i32,
                    (group % 7) as i32
                ]
            );
            actual += codes[..3].iter().sum::<i32>();
            std::hint::black_box(&codes);
        }
        let allocations = finish_test_allocation_count();
        assert_eq!(allocations, 0);
        assert_eq!(actual, previous);
        assert!(decode_group_codes_into(&domains, 0, &mut codes[..2]).is_err());
        assert!(decode_group_codes_into(&domains, rows, &mut codes).is_err());
        decode_group_codes_into(&[], 0, &mut []).expect("global group");
        assert!(decode_group_codes_into(&[], 1, &mut []).is_err());
    }

    #[test]
    fn reverses_dense_mixed_radix_in_key_order() {
        let domains = [domain(2), domain(3), domain(4)];
        assert_eq!(decode_group_codes(&domains, 23), Ok(vec![1, 2, 3]));
        assert_eq!(decode_group_codes(&domains, 0), Ok(vec![0, 0, 0]));
    }

    #[test]
    fn narrow_hash_keys_validate_sign_extension_and_typed_datums() {
        for (type_oid, minimum, maximum) in [
            (INT2OID, i64::from(i16::MIN), i64::from(i16::MAX)),
            (INT4OID, i64::from(i32::MIN), i64::from(i32::MAX)),
        ] {
            let keys = [0_i64, 1, maximum, minimum, -1];
            let bytes = keys
                .iter()
                .flat_map(|key| key.to_ne_bytes())
                .collect::<Vec<_>>();
            validate_compact_integer_key_buffers(&bytes, None, keys.len(), keys.len(), type_oid)
                .expect("canonical unsigned ordering of sign-extended keys");
            for (index, key) in keys.into_iter().enumerate() {
                let (datum, is_null) = decode_compact_integer_key(&bytes, None, index, type_oid)
                    .expect("typed integer datum");
                assert!(!is_null);
                let expected = if type_oid == INT2OID {
                    pg_sys::Datum::from(i16::try_from(key).expect("smallint"))
                } else {
                    pg_sys::Datum::from(i32::try_from(key).expect("integer"))
                };
                assert_eq!(datum.value(), expected.value());
            }
            for invalid in [minimum - 1, maximum + 1, i64::from(u32::MAX)] {
                let bytes = invalid.to_ne_bytes();
                assert!(
                    validate_compact_integer_key_buffers(&bytes, None, 1, 1, type_oid).is_err()
                );
                assert!(decode_compact_integer_key(&bytes, None, 0, type_oid).is_err());
            }
            let bytes = 0_i64.to_ne_bytes();
            validate_compact_integer_key_buffers(&bytes, Some(&[1]), 1, 1, type_oid)
                .expect("NULL key");
            assert!(
                decode_compact_integer_key(&bytes, Some(&[1]), 0, type_oid)
                    .expect("NULL datum")
                    .1
            );
        }
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
    fn bigint_compact_decoder_preserves_signed_extremes_in_native_order() {
        // Native compact order is unsigned bit order, not SQL ORDER BY.
        let signed = [0_i64, 1, i64::MAX, i64::MIN, -2, -1];
        let values = signed
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect::<Vec<_>>();
        validate_compact_i64_key_buffers(&values, None, signed.len(), signed.len())
            .expect("native compact order");
        for (group, expected) in signed.into_iter().enumerate() {
            let (datum, is_null) =
                decode_compact_i64_key(&values, None, group).expect("BIGINT key");
            assert!(!is_null);
            assert_eq!(datum.value() as i64, expected);
        }
    }

    #[test]
    fn h3_decoder_roundtrips_unsigned_high_bits() {
        let mut values = Vec::new();
        values.extend_from_slice(&(1_u64 << 63).to_ne_bytes());
        values.extend_from_slice(&u64::MAX.to_ne_bytes());

        for (group, expected) in [(0, 1_u64 << 63), (1, u64::MAX)] {
            let (datum, is_null) =
                decode_compact_i64_key(&values, None, group).expect("valid compact hash key");
            assert!(!is_null);
            assert_eq!(datum.value() as u64, expected);
        }
    }

    #[test]
    fn h3_decoder_honors_sql_null_before_payload() {
        let (datum, is_null) = decode_compact_i64_key(&u64::MAX.to_ne_bytes(), Some(&[1]), 0)
            .expect("canonical NULL marker");
        assert!(is_null);
        assert_eq!(datum.value(), 0);
    }

    #[test]
    fn h3_decoder_rejects_noncanonical_null_byte() {
        assert!(decode_compact_i64_key(&0_u64.to_ne_bytes(), Some(&[2]), 0).is_err());
    }

    #[test]
    fn h3_decoder_rejects_short_key_and_null_buffers() {
        assert!(decode_compact_i64_key(&[0; 7], None, 0).is_err());
        assert!(decode_compact_i64_key(&0_u64.to_ne_bytes(), Some(&[]), 0).is_err());
    }

    #[test]
    fn h3_compact_boundary_requires_zero_null_payload() {
        assert!(validate_compact_i64_key_buffers(&0_u64.to_ne_bytes(), Some(&[1]), 1, 1).is_ok());
        assert!(
            validate_compact_i64_key_buffers(&u64::MAX.to_ne_bytes(), Some(&[1]), 1, 1).is_err()
        );
    }

    #[test]
    fn h3_compact_boundary_accepts_null_first_then_sorted_keys() {
        let values = h3_key_bytes(&[0, 2, 4]);
        validate_compact_i64_key_buffers(&values, Some(&[1, 0, 0]), 3, 3)
            .expect("NULL-first strictly ascending compact prefix");
    }

    #[test]
    fn h3_compact_boundary_rejects_null_after_value() {
        let values = h3_key_bytes(&[2, 0]);
        assert!(validate_compact_i64_key_buffers(&values, Some(&[0, 1]), 2, 2).is_err());
    }

    #[test]
    fn h3_compact_boundary_rejects_repeated_null_prefix() {
        let values = h3_key_bytes(&[0, 0]);
        assert!(validate_compact_i64_key_buffers(&values, Some(&[1, 1]), 2, 2).is_err());
    }

    #[test]
    fn h3_compact_boundary_rejects_emitted_count_above_capacity() {
        let values = h3_key_bytes(&[7]);
        assert!(validate_compact_i64_key_buffers(&values, None, 1, 2).is_err());
    }

    #[test]
    fn h3_compact_boundary_rejects_duplicate_nonnull_key() {
        let values = h3_key_bytes(&[7, 7]);
        assert!(validate_compact_i64_key_buffers(&values, None, 2, 2).is_err());
    }

    #[test]
    fn h3_compact_boundary_rejects_descending_nonnull_keys() {
        let values = h3_key_bytes(&[8, 7]);
        assert!(validate_compact_i64_key_buffers(&values, None, 2, 2).is_err());
    }

    #[test]
    fn h3_compact_boundary_uses_unsigned_order_and_ignores_unused_tail() {
        let values = h3_key_bytes(&[0x7fff_ffff_ffff_ffff, 0x8000_0000_0000_0000, u64::MAX, 9, 1]);
        validate_compact_i64_key_buffers(&values, None, 5, 3)
            .expect("high-bit keys are unsigned ascending; unused tail is outside the prefix");
    }

    #[test]
    fn nontext_group_materialization_enforces_type_null_and_domain_contracts() {
        let slot = |type_oid| AggOutputSlot {
            source: AggOutputSource::GroupKey { key_index: 0 },
            source_type_oid: type_oid,
            result_type_oid: type_oid,
            result_typmod: -1,
            result_collation_oid: 0,
            nullable: true,
        };
        let domain = |type_oid| GroupDomain {
            type_oid,
            collation_oid: 0,
            values: Vec::new(),
            null_code: None,
        };

        for (type_oid, value, expected) in [
            (BOOLOID, GroupDatum::Bool(true), 1_usize),
            (INT2OID, GroupDatum::I32(-7), (-7_i16) as usize),
            (INT4OID, GroupDatum::I32(-9), (-9_i32) as usize),
            (DATEOID, GroupDatum::I32(12), 12_usize),
            (INT8OID, GroupDatum::I64(17), 17_usize),
            (TIMESTAMPOID, GroupDatum::I64(19), 19_usize),
            (TIMESTAMPTZOID, GroupDatum::I64(23), 23_usize),
            (
                FLOAT4OID,
                GroupDatum::F32(1.25),
                1.25_f32.to_bits() as usize,
            ),
            (FLOAT8OID, GroupDatum::F64(2.5), 2.5_f64.to_bits() as usize),
        ] {
            // SAFETY: these non-text cases only perform scalar Datum conversion
            // and invoke no PostgreSQL catalog or allocation callbacks.
            let (datum, is_null) =
                unsafe { group_datum(&slot(type_oid), &domain(type_oid), &value) }
                    .expect("matching non-text group datum materializes");
            assert!(!is_null);
            assert_eq!(datum.value(), expected);
        }

        // SAFETY: NULL returns before any PostgreSQL callback.
        let (datum, is_null) =
            unsafe { group_datum(&slot(INT4OID), &domain(INT4OID), &GroupDatum::Null) }
                .expect("NULL group materializes");
        assert!(is_null);
        assert_eq!(datum.value(), 0);

        // SAFETY: each rejection occurs before any PostgreSQL callback.
        assert!(
            unsafe { group_datum(&slot(INT8OID), &domain(INT4OID), &GroupDatum::I32(1)) }.is_err()
        );
        assert!(
            unsafe { group_datum(&slot(INT2OID), &domain(INT2OID), &GroupDatum::I32(i32::MAX),) }
                .is_err()
        );
        assert!(
            unsafe { group_datum(&slot(INT4OID), &domain(INT4OID), &GroupDatum::Unused) }.is_err()
        );
        assert!(
            unsafe { group_datum(&slot(INT4OID), &domain(INT4OID), &GroupDatum::Bool(true)) }
                .is_err()
        );

        assert_eq!(
            validate_compact_i64_key_buffers(&[], None, usize::MAX, 0),
            Err("compact hash key byte count overflows usize".to_owned())
        );
    }
}

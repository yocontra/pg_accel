//! Bulk column extraction from `MinimalTuple` batches.
//!
//! Replaces the per-tuple `ExecForceStoreMinimalTuple` + `slot_getattr`
//! pattern with direct pointer reads into the MinimalTuple binary data.
//! For fixed-width, pass-by-value columns where all preceding columns are
//! also fixed-width, the byte offset from the data area is constant across
//! tuples and can be precomputed once per query.
//!
//! **Performance impact:** Eliminates ~150-200ns per tuple per column of
//! slot deformation overhead. For 1M-row batches this saves 150-200ms
//! per extraction pass.

#![allow(clippy::needless_range_loop)]

use pgrx::pg_sys;

/// Alignment characters used in PostgreSQL attribute descriptors.
const ALIGN_CHAR: u8 = b'c'; // 1-byte alignment
const ALIGN_SHORT: u8 = b's'; // 2-byte alignment
const ALIGN_INT: u8 = b'i'; // 4-byte alignment
const ALIGN_DOUBLE: u8 = b'd'; // 8-byte alignment

/// `MINIMAL_TUPLE_OFFSET` from PostgreSQL's `htup_details.h`.
///
/// MinimalTuple's `t_hoff` is computed the same as in a full HeapTuple,
/// so it includes this offset that accounts for the missing HeapTuple
/// header fields (t_choice, t_ctid). The actual data area in a
/// MinimalTuple starts at `mt + t_hoff - MINIMAL_TUPLE_OFFSET`.
///
/// Formula: `(offsetof(HeapTupleHeaderData, t_infomask2) - sizeof(uint32))
///            / MAXIMUM_ALIGNOF * MAXIMUM_ALIGNOF`
const fn minimal_tuple_offset() -> usize {
    let ht_infomask2_offset = std::mem::offset_of!(pg_sys::HeapTupleHeaderData, t_infomask2);
    let raw = ht_infomask2_offset - std::mem::size_of::<u32>();
    // Round down to MAXIMUM_ALIGNOF (8 on 64-bit).
    const MAX_ALIGN: usize = 8;
    (raw / MAX_ALIGN) * MAX_ALIGN
}

/// Pre-computed extraction info for a single column within a `TupleDesc`.
///
/// Built once at query start via [`AttExtractInfo::new`]. Used by the bulk
/// extraction functions to read values directly from `MinimalTuple` data.
pub struct AttExtractInfo {
    /// Byte offset of this attribute from the start of the data area
    /// (i.e., from byte position `t_hoff` within `MinimalTupleData`).
    ///
    /// Only valid when `can_fast_extract` is true.
    data_offset: usize,

    /// `attlen` from the tuple descriptor. Positive for fixed-width types.
    pub typlen: i16,

    /// PostgreSQL type OID for this attribute (`atttypid`).
    pub typid: pg_sys::Oid,

    /// 0-based index of this attribute (attno - 1). Used for null bitmap.
    att_index: usize,

    /// True if this column and ALL preceding columns are fixed-width
    /// (attlen > 0). When true, `data_offset` is correct for any tuple
    /// without preceding nulls.
    can_fast_extract: bool,
}

impl AttExtractInfo {
    /// Whether this column can use the fast extraction path.
    #[must_use]
    pub fn can_fast_extract(&self) -> bool {
        self.can_fast_extract
    }

    /// Byte offset of this attribute from the data area start.
    #[must_use]
    pub fn data_offset(&self) -> usize {
        self.data_offset
    }

    /// Create a dummy extraction info that always fails fast-extract.
    ///
    /// Used as a placeholder for `COUNT(*)` columns that do not need
    /// value extraction.
    #[must_use]
    pub fn dummy() -> Self {
        Self {
            data_offset: 0,
            typlen: 0,
            typid: pg_sys::InvalidOid,
            att_index: 0,
            can_fast_extract: false,
        }
    }
}

impl AttExtractInfo {
    /// Build extraction info for attribute `attno` (1-based) in `tupdesc`.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be a valid PostgreSQL `TupleDesc` pointer with at
    /// least `attno` attributes.
    pub unsafe fn new(tupdesc: pg_sys::TupleDesc, attno: i32) -> Self {
        let target_idx = (attno - 1) as usize;
        // SAFETY: tupdesc is a valid TupleDesc pointer per caller contract.
        let natts = unsafe { (*tupdesc).natts as usize };

        if target_idx >= natts {
            return Self {
                data_offset: 0,
                typlen: 0,
                typid: pg_sys::InvalidOid,
                att_index: target_idx,
                can_fast_extract: false,
            };
        }

        // Walk all attributes from 0 to target_idx, computing the running
        // byte offset. If any attribute is variable-width, mark as
        // not-fast-extractable.
        // SAFETY: tupdesc->attrs is a flexible array with natts entries.
        let attrs = unsafe { (*tupdesc).attrs.as_ptr() };
        let mut offset: usize = 0;
        let mut all_fixed = true;

        for i in 0..=target_idx {
            // SAFETY: i <= target_idx < natts, so attrs[i] is valid.
            let attr = unsafe { &*attrs.add(i) };
            let attlen = attr.attlen;

            if attlen <= 0 {
                all_fixed = false;
                break;
            }

            let align = attr.attalign as u8;
            offset = align_offset(offset, align);

            if i == target_idx {
                break;
            }

            offset += attlen as usize;
        }

        // SAFETY: target_idx < natts verified above.
        let target_attr = unsafe { &*attrs.add(target_idx) };

        Self {
            data_offset: offset,
            typlen: target_attr.attlen,
            typid: target_attr.atttypid,
            att_index: target_idx,
            can_fast_extract: all_fixed && target_attr.attlen > 0,
        }
    }
}

/// Align `offset` up to the boundary specified by the PG alignment char.
#[inline(always)]
fn align_offset(offset: usize, align_char: u8) -> usize {
    let boundary = match align_char {
        ALIGN_CHAR => 1,
        ALIGN_SHORT => 2,
        ALIGN_INT => 4,
        ALIGN_DOUBLE => 8,
        _ => 1,
    };
    (offset + boundary - 1) & !(boundary - 1)
}

/// Check whether any attribute before `target_idx` is null in the bitmap.
///
/// # Safety
///
/// `bits_ptr` must point to a valid null bitmap with at least
/// `target_idx` bits.
#[inline]
unsafe fn has_preceding_null(bits_ptr: *const u8, target_idx: usize) -> bool {
    for i in 0..target_idx {
        // SAFETY: bits_ptr has at least target_idx bits per caller contract.
        let byte = unsafe { *bits_ptr.add(i / 8) };
        let mask = 1u8 << (i % 8);
        if (byte & mask) == 0 {
            return true;
        }
    }
    false
}

/// Check whether a specific attribute is null in the bitmap.
/// In PostgreSQL's null bitmap, bit=1 means NOT NULL, bit=0 means NULL.
///
/// # Safety
///
/// `bits_ptr` must point to a valid null bitmap.
#[inline(always)]
unsafe fn att_is_null(bits_ptr: *const u8, att_index: usize) -> bool {
    // SAFETY: bitmap has at least att_index bits per caller contract.
    let byte = unsafe { *bits_ptr.add(att_index / 8) };
    let mask = 1u8 << (att_index % 8);
    (byte & mask) == 0
}

// ---------------------------------------------------------------------------
// Internal: fast-path read + slow-path fallback
// ---------------------------------------------------------------------------

/// Try the fast direct-read path. Returns `Some(value)` on success,
/// `None` if the caller should use the slow path.
///
/// # Safety
///
/// `mt` must be a valid MinimalTuple. `info` must match the tuple's schema.
#[inline(always)]
unsafe fn try_fast_read<T: Copy>(
    mt: pg_sys::MinimalTuple,
    info: &AttExtractInfo,
    has_null_flag: bool,
) -> Option<T> {
    if info.can_fast_extract
        && (!has_null_flag || !unsafe { has_preceding_null((*mt).t_bits.as_ptr(), info.att_index) })
    {
        // SAFETY: mt is valid. In a MinimalTuple, t_hoff includes
        // MINIMAL_TUPLE_OFFSET so data starts at t_hoff - offset.
        let data_start =
            unsafe { (mt as *const u8).add((*mt).t_hoff as usize - minimal_tuple_offset()) };
        Some(unsafe { *(data_start.add(info.data_offset).cast::<T>()) })
    } else {
        None
    }
}

/// Slow-path: deform tuple via PG slot machinery.
///
/// # Safety
///
/// `mt` and `fallback_slot` must be valid. Must run on main backend thread.
#[inline]
unsafe fn slow_getattr(
    mt: pg_sys::MinimalTuple,
    fallback_slot: *mut pg_sys::TupleTableSlot,
    attno: i32,
) -> (pg_sys::Datum, bool) {
    // SAFETY: mt is valid, fallback_slot is valid, on main thread.
    unsafe { pg_sys::ExecForceStoreMinimalTuple(mt, fallback_slot, false) };
    let mut is_null = false;
    let datum =
        unsafe { pg_sys::slot_getattr(fallback_slot, attno, std::ptr::addr_of_mut!(is_null)) };
    (datum, is_null)
}

// ---------------------------------------------------------------------------
// Bulk extraction functions
// ---------------------------------------------------------------------------

/// Shared extraction loop logic. Calls `convert` on each non-null datum
/// from the fast path (raw value of type T) or slow path.
///
/// # Safety
///
/// - All non-null entries in `tuples` must be valid `MinimalTuple` pointers.
/// - `info` must have been built from the correct `TupleDesc` for these tuples.
/// - `fallback_slot` must be a valid `TupleTableSlot`.
unsafe fn extract_typed<T: Copy, R>(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
    zero: R,
    convert_fast: fn(T) -> R,
    convert_slow: fn(pg_sys::Datum) -> R,
) -> (Vec<R>, Vec<u8>)
where
    R: Copy,
{
    let n = tuples.len();
    let mut values = Vec::with_capacity(n);
    let mut nulls = Vec::with_capacity(n);
    let attno = (info.att_index + 1) as i32;

    for &mt in tuples {
        if mt.is_null() {
            values.push(zero);
            nulls.push(1);
            continue;
        }

        // SAFETY: mt is a valid MinimalTuple per caller contract.
        let mt_ref = unsafe { &*mt };
        let has_null_flag = (mt_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

        // Check if our target attribute is null.
        if has_null_flag {
            // SAFETY: t_bits is valid when HEAP_HASNULL is set.
            if unsafe { att_is_null(mt_ref.t_bits.as_ptr(), info.att_index) } {
                values.push(zero);
                nulls.push(1);
                continue;
            }
        }

        // SAFETY: mt is valid, info matches schema.
        if let Some(val) = unsafe { try_fast_read::<T>(mt, info, has_null_flag) } {
            values.push(convert_fast(val));
            nulls.push(0);
        } else {
            // SAFETY: fallback_slot is valid, on main thread.
            let (datum, is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };
            if is_null {
                values.push(zero);
                nulls.push(1);
            } else {
                values.push(convert_slow(datum));
                nulls.push(0);
            }
        }
    }

    (values, nulls)
}

/// Extract a column of `i32` values from a batch of `MinimalTuple`s.
///
/// # Safety
///
/// - All non-null entries in `tuples` must be valid `MinimalTuple` pointers.
/// - `info` must have been built from the correct `TupleDesc` for these tuples.
/// - `fallback_slot` must be a valid `TupleTableSlot`.
pub unsafe fn extract_i32(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<i32>, Vec<u8>) {
    // SAFETY: caller guarantees all preconditions.
    unsafe {
        extract_typed::<i32, i32>(tuples, info, fallback_slot, 0, |v| v, |d| d.value() as i32)
    }
}

/// Extract a column of `i64` values from a batch of `MinimalTuple`s.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
pub unsafe fn extract_i64(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<i64>, Vec<u8>) {
    // SAFETY: caller guarantees all preconditions.
    unsafe {
        extract_typed::<i64, i64>(tuples, info, fallback_slot, 0, |v| v, |d| d.value() as i64)
    }
}

/// Extract a column of `f32` values from a batch of `MinimalTuple`s.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
pub unsafe fn extract_f32(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<f32>, Vec<u8>) {
    // SAFETY: caller guarantees all preconditions.
    unsafe {
        extract_typed::<f32, f32>(
            tuples,
            info,
            fallback_slot,
            0.0,
            |v| v,
            |d| f32::from_bits(d.value() as u32),
        )
    }
}

/// Extract a column of UUID values (16-byte fixed-width) from a batch of
/// `MinimalTuple`s.
///
/// PG storage: `pg_uuid_t` is 16 bytes, `typbyval = false`, so the Datum
/// returned by the slow path is a pointer to the 16-byte payload. Fast
/// path reads the inline 16 bytes directly at the attribute offset.
///
/// Hash-key shape for the GPU: each UUID is treated as host-order bytes
/// (the kernel reads them as two u64 halves and XORs `hash64()` mixes —
/// see `pgaccel_hash_agg.cpp::read_key_u64`). We therefore preserve raw
/// byte order rather than canonicalising to little-endian.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
pub unsafe fn extract_uuid(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<[u8; 16]>, Vec<u8>) {
    let n = tuples.len();
    let mut values = Vec::with_capacity(n);
    let mut nulls = Vec::with_capacity(n);
    let attno = (info.att_index + 1) as i32;

    for &mt in tuples {
        if mt.is_null() {
            values.push([0u8; 16]);
            nulls.push(1);
            continue;
        }

        // SAFETY: mt is a valid MinimalTuple per caller contract.
        let mt_ref = unsafe { &*mt };
        let has_null_flag = (mt_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

        if has_null_flag {
            // SAFETY: t_bits is valid when HEAP_HASNULL is set.
            if unsafe { att_is_null(mt_ref.t_bits.as_ptr(), info.att_index) } {
                values.push([0u8; 16]);
                nulls.push(1);
                continue;
            }
        }

        // SAFETY: mt is valid, info matches schema. UUID is 16-byte
        // fixed-width — try_fast_read::<[u8; 16]> reads at the
        // attribute offset directly when none of the predecessor
        // columns are NULL or variable-length.
        if let Some(val) = unsafe { try_fast_read::<[u8; 16]>(mt, info, has_null_flag) } {
            values.push(val);
            nulls.push(0);
        } else {
            // SAFETY: fallback_slot is valid, on main thread.
            let (datum, is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };
            if is_null {
                values.push([0u8; 16]);
                nulls.push(1);
            } else {
                // SAFETY: UUID is `typbyval = false`, so the Datum is a
                // pointer to a 16-byte pg_uuid_t payload owned by the
                // tuple. Copy the bytes out so the caller doesn't hold
                // a dangling reference once the tuple is freed.
                let p = datum.value() as *const u8;
                let mut bytes = [0u8; 16];
                unsafe {
                    std::ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), 16);
                }
                values.push(bytes);
                nulls.push(0);
            }
        }
    }

    (values, nulls)
}

/// PG `inet_struct` family byte values.
const PGSQL_AF_INET: u8 = 2; // IPv4 — uses 4 bytes of ipaddr
const PGSQL_AF_INET6: u8 = 3; // IPv6 — uses 16 bytes of ipaddr

/// Build a 24-byte canonical INET key from the inet_struct payload pointer.
///
/// Layout (matches `PGACCEL_KEY_INET` in `pgaccel_hash_join.h`):
///   byte 0      = family
///   byte 1      = bits   (netmask)
///   bytes 2-17  = ipaddr (16 bytes; IPv4 zero-padded after first 4)
///   bytes 18-23 = zero padding (u64 alignment)
///
/// IPv4 inputs zero-pad bytes 6-17 because PG storage leaves those
/// uninitialised — without canonicalisation, two semantically-equal
/// IPv4 addresses with different padding garbage would hash to
/// different buckets and miss equality.
///
/// # Safety
///
/// `vardata` must point to at least `min_payload_bytes` (≥ 6 for
/// IPv4, ≥ 18 for IPv6) of valid `inet_struct` payload.
unsafe fn canonicalize_inet_payload(vardata: *const u8, payload_len: usize) -> Option<[u8; 24]> {
    if payload_len < 6 {
        // Need at least family + bits + 4 bytes of IPv4 address.
        return None;
    }
    // SAFETY: caller guarantees ≥ payload_len readable bytes.
    let family = unsafe { *vardata };
    let bits = unsafe { *vardata.add(1) };
    let mut out = [0u8; 24];
    out[0] = family;
    out[1] = bits;
    match family {
        PGSQL_AF_INET => {
            // Copy 4 IPv4 bytes; bytes 6-17 stay zero from initialiser.
            unsafe {
                std::ptr::copy_nonoverlapping(vardata.add(2), out.as_mut_ptr().add(2), 4);
            }
        }
        PGSQL_AF_INET6 => {
            if payload_len < 18 {
                return None;
            }
            // Copy 16 IPv6 bytes verbatim.
            unsafe {
                std::ptr::copy_nonoverlapping(vardata.add(2), out.as_mut_ptr().add(2), 16);
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Extract a column of INET / CIDR values as 24-byte canonical hash
/// keys (see [`canonicalize_inet_payload`] for layout).
///
/// Variable-length input (varlena), so the fast-path tries to read the
/// inet_struct directly from the heap-tuple data area; on a slow read
/// we fall back to `slow_getattr` which detoasts via PG.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
pub unsafe fn extract_inet(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<[u8; 24]>, Vec<u8>) {
    let n = tuples.len();
    let mut values = Vec::with_capacity(n);
    let mut nulls = Vec::with_capacity(n);
    let attno = (info.att_index + 1) as i32;

    for &mt in tuples {
        if mt.is_null() {
            values.push([0u8; 24]);
            nulls.push(1);
            continue;
        }

        // SAFETY: mt is a valid MinimalTuple per caller contract.
        let mt_ref = unsafe { &*mt };
        let has_null_flag = (mt_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

        if has_null_flag {
            // SAFETY: t_bits is valid when HEAP_HASNULL is set.
            if unsafe { att_is_null(mt_ref.t_bits.as_ptr(), info.att_index) } {
                values.push([0u8; 24]);
                nulls.push(1);
                continue;
            }
        }

        // INET is varlena — always go through slow_getattr so PG handles
        // detoast / short header. The fast path used elsewhere reads
        // fixed-width inline data; mirroring it for varlena is fragile.
        // SAFETY: fallback_slot is valid, on main thread.
        let (datum, is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };
        if is_null || datum.value() == 0 {
            values.push([0u8; 24]);
            nulls.push(1);
            continue;
        }

        // Detoast and read inet_struct payload.
        // SAFETY: datum is a valid varlena pointer per the slot's tupdesc.
        // In `cfg(test)` builds, libtest spawns a fresh thread per test
        // and pgrx's guarded wrapper panics with "postgres FFI may not
        // be called from multiple threads"; the identity stub in
        // `src/pg_stubs.rs` is what the linker resolves
        // `pg_detoast_datum` against on macOS, so calling the raw extern
        // directly gives us a safe passthrough without the thread guard.
        // Tests only use non-TOAST flat varlenas, so identity is correct.
        #[cfg(not(test))]
        let detoasted = unsafe {
            pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>())
        };
        #[cfg(test)]
        let detoasted = {
            unsafe extern "C" {
                fn pg_detoast_datum(
                    datum: *mut pgrx::pg_sys::varlena,
                ) -> *mut pgrx::pg_sys::varlena;
            }
            unsafe { pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>()) }
        };
        if detoasted.is_null() {
            values.push([0u8; 24]);
            nulls.push(1);
            continue;
        }

        // SAFETY: detoasted is a valid flat varlena. vardata_any
        // returns a pointer to the inet_struct (1 family + 1 bits +
        // up to 16 ipaddr bytes); varsize_any_exhdr is the payload
        // length excluding the varlena header.
        let payload_len = unsafe { pgrx::varsize_any_exhdr(detoasted) };
        let vardata = unsafe { pgrx::vardata_any(detoasted).cast::<u8>() };

        // SAFETY: vardata is non-null when detoasted is non-null;
        // payload_len is the bytes beyond the header. canonicalize_inet_payload
        // re-checks payload_len against the family-specific minimum.
        if let Some(canon) = unsafe { canonicalize_inet_payload(vardata, payload_len) } {
            values.push(canon);
            nulls.push(0);
        } else {
            values.push([0u8; 24]);
            nulls.push(1);
        }
    }

    (values, nulls)
}

/// Diagnostic: compare fast-path vs slow-path extraction for first N tuples.
///
/// # Safety
///
/// Same requirements as [`extract_f64`].
pub unsafe fn diagnose_extraction(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
    n: usize,
) {
    let attno = (info.att_index + 1) as i32;
    let check = n.min(tuples.len());
    for i in 0..check {
        let mt = tuples[i];
        if mt.is_null() {
            continue;
        }
        let mt_ref = unsafe { &*mt };
        let t_hoff = mt_ref.t_hoff;
        let _has_null = (mt_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

        // Fast-path raw bytes
        let data_start = unsafe { (mt as *const u8).add(t_hoff as usize - minimal_tuple_offset()) };
        let raw_at_0 = unsafe { *(data_start.cast::<u32>()) };
        let _raw_at_off = if info.data_offset > 0 {
            unsafe { *(data_start.add(info.data_offset).cast::<u32>()) }
        } else {
            raw_at_0
        };

        // Slow-path
        let (datum, _is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };

        // Check Rust struct field offsets
        let rust_t_hoff_offset = unsafe {
            let base = mt as *const u8;
            let field = std::ptr::addr_of!((*mt).t_hoff).cast::<u8>();
            field.offset_from(base) as usize
        };
        let raw_byte_14 = unsafe { *((mt as *const u8).add(14)) };
        let rust_t_hoff_val = mt_ref.t_hoff;

        // Dump first 32 bytes of the MinimalTuple
        let mt_bytes: Vec<u8> = (0..32usize)
            .map(|j| unsafe { *((mt as *const u8).add(j)) })
            .collect();
        let hex: Vec<String> = mt_bytes.iter().map(|b| format!("{b:02x}")).collect();
        pgrx::debug1!(
            "pg_accel: DIAG[{}] rust_thoff_offset={} rust_thoff={} raw_byte14={} \
             data_offset={} slow_datum=0x{:016x} bytes=[{}]",
            i,
            rust_t_hoff_offset,
            rust_t_hoff_val,
            raw_byte_14,
            info.data_offset,
            datum.value(),
            hex.join(" "),
        );
    }
}

/// Extract a column as `f64` values from a batch of `MinimalTuple`s.
///
/// Type-aware extraction: handles float4, float8, int2, int4, int8 columns
/// correctly. Float4 values are promoted via f32; integer types are cast.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
#[allow(clippy::cast_precision_loss)]
pub unsafe fn extract_f64(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<f64>, Vec<u8>) {
    // SAFETY: caller guarantees all preconditions for all branches.
    unsafe {
        match info.typid {
            pg_sys::FLOAT4OID => {
                extract_typed::<f32, f64>(tuples, info, fallback_slot, 0.0, f64::from, |d| {
                    f64::from(f32::from_bits(d.value() as u32))
                })
            }
            pg_sys::INT2OID => {
                extract_typed::<i16, f64>(tuples, info, fallback_slot, 0.0, f64::from, |d| {
                    (d.value() as i16) as f64
                })
            }
            pg_sys::INT4OID => {
                extract_typed::<i32, f64>(tuples, info, fallback_slot, 0.0, f64::from, |d| {
                    (d.value() as i32) as f64
                })
            }
            pg_sys::INT8OID => extract_typed::<i64, f64>(
                tuples,
                info,
                fallback_slot,
                0.0,
                |v| v as f64,
                |d| (d.value() as i64) as f64,
            ),
            // Default: float8 or any other 8-byte numeric type.
            _ => extract_typed::<f64, f64>(
                tuples,
                info,
                fallback_slot,
                0.0,
                |v| v,
                |d| f64::from_bits(d.value() as u64),
            ),
        }
    }
}

/// Extract a column as raw `Datum` values from a batch of `MinimalTuple`s.
///
/// Uses the fast path for fixed-width types (1/2/4/8 byte), falls back
/// to `slot_getattr` for variable-width or unusual types.
///
/// # Safety
///
/// Same requirements as [`extract_i32`].
pub unsafe fn extract_datum(
    tuples: &[pg_sys::MinimalTuple],
    info: &AttExtractInfo,
    fallback_slot: *mut pg_sys::TupleTableSlot,
) -> (Vec<pg_sys::Datum>, Vec<u8>) {
    let n = tuples.len();
    let mut values = Vec::with_capacity(n);
    let mut nulls = Vec::with_capacity(n);
    let attno = (info.att_index + 1) as i32;

    for &mt in tuples {
        if mt.is_null() {
            values.push(pg_sys::Datum::from(0usize));
            nulls.push(1);
            continue;
        }

        // SAFETY: mt is a valid MinimalTuple per caller contract.
        let mt_ref = unsafe { &*mt };
        let has_null_flag = (mt_ref.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

        if has_null_flag {
            // SAFETY: t_bits is valid when HEAP_HASNULL is set.
            if unsafe { att_is_null(mt_ref.t_bits.as_ptr(), info.att_index) } {
                values.push(pg_sys::Datum::from(0usize));
                nulls.push(1);
                continue;
            }
        }

        // Fast path for fixed-width types with known preceding layout.
        if info.can_fast_extract
            && info.typlen > 0
            && (!has_null_flag
                || !unsafe { has_preceding_null(mt_ref.t_bits.as_ptr(), info.att_index) })
        {
            // SAFETY: t_hoff - MINIMAL_TUPLE_OFFSET + data_offset is
            // within the tuple data area.
            let data_start =
                unsafe { (mt as *const u8).add(mt_ref.t_hoff as usize - minimal_tuple_offset()) };
            let val_ptr = unsafe { data_start.add(info.data_offset) };
            let datum_val: usize = match info.typlen {
                1 => unsafe { *val_ptr as usize },
                2 => unsafe { *(val_ptr.cast::<u16>()) as usize },
                4 => unsafe { *(val_ptr.cast::<u32>()) as usize },
                8 => unsafe { *(val_ptr.cast::<u64>()) as usize },
                _ => {
                    // SAFETY: Unusual fixed-width type; fall back to slot.
                    let (d, is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };
                    values.push(d);
                    nulls.push(u8::from(is_null));
                    continue;
                }
            };
            values.push(pg_sys::Datum::from(datum_val));
            nulls.push(0);
        } else {
            // SAFETY: fallback_slot is valid, on main thread.
            let (datum, is_null) = unsafe { slow_getattr(mt, fallback_slot, attno) };
            values.push(datum);
            nulls.push(u8::from(is_null));
        }
    }

    (values, nulls)
}

// ---------------------------------------------------------------------------
// HeapTuple direct extraction (deferred materialization)
// ---------------------------------------------------------------------------

/// Read a fixed-width value directly from a HeapTuple's data area (public).
///
/// # Safety
///
/// `ht_data` must be a valid `HeapTupleHeader`. `info` must match the schema.
#[inline(always)]
pub unsafe fn try_fast_read_heap_pub<T: Copy>(
    ht_data: pg_sys::HeapTupleHeader,
    info: &AttExtractInfo,
) -> Option<T> {
    // SAFETY: delegates to try_fast_read_heap with same preconditions.
    unsafe { try_fast_read_heap(ht_data, info) }
}

/// Read a fixed-width value directly from a HeapTuple's data area.
///
/// For HeapTuples, the data area starts at `t_data + t_hoff` (no
/// `MINIMAL_TUPLE_OFFSET` adjustment needed unlike MinimalTuples).
///
/// # Safety
///
/// `ht_data` must be a valid `HeapTupleHeader`. `info` must match the schema.
#[inline(always)]
unsafe fn try_fast_read_heap<T: Copy>(
    ht_data: pg_sys::HeapTupleHeader,
    info: &AttExtractInfo,
) -> Option<T> {
    if !info.can_fast_extract {
        return None;
    }

    // SAFETY: ht_data is valid per caller contract.
    let hdr = unsafe { &*ht_data };
    let has_null_flag = (hdr.t_infomask & pg_sys::HEAP_HASNULL as u16) != 0;

    if has_null_flag && unsafe { has_preceding_null(hdr.t_bits.as_ptr(), info.att_index) } {
        return None;
    }

    // Check target attribute is not null.
    if has_null_flag && unsafe { att_is_null(hdr.t_bits.as_ptr(), info.att_index) } {
        return None;
    }

    // HeapTuple data starts at t_data + t_hoff (no MINIMAL_TUPLE_OFFSET).
    // SAFETY: ht_data is valid, t_hoff is within bounds, data_offset is precomputed.
    let data_start = unsafe { (ht_data as *const u8).add(hdr.t_hoff as usize) };
    Some(unsafe { *(data_start.add(info.data_offset).cast::<T>()) })
}

/// Extract f64 values from a batch of HeapTuple headers (for deferred materialization).
///
/// Returns `None` for a tuple if it requires slow-path extraction (variable-width
/// preceding columns or nulls in unexpected positions). Caller should fall back
/// to MinimalTuple extraction for those rows.
///
/// # Safety
///
/// All entries in `headers` must be valid `HeapTupleHeader` pointers.
/// `info` must match the schema of these tuples.
pub unsafe fn extract_f64_from_heap_headers(
    headers: &[pg_sys::HeapTupleHeader],
    info: &AttExtractInfo,
) -> (Vec<f64>, Vec<u8>) {
    let n = headers.len();
    let mut values = Vec::with_capacity(n);
    let mut nulls = Vec::with_capacity(n);

    // Select the appropriate type-punning based on the column's OID.
    for &hdr in headers {
        if hdr.is_null() {
            values.push(0.0);
            nulls.push(1);
            continue;
        }

        let val: Option<f64> = unsafe {
            match info.typid {
                t if t == pg_sys::FLOAT4OID => try_fast_read_heap::<f32>(hdr, info).map(f64::from),
                t if t == pg_sys::INT2OID => try_fast_read_heap::<i16>(hdr, info).map(f64::from),
                t if t == pg_sys::INT4OID => try_fast_read_heap::<i32>(hdr, info).map(f64::from),
                t if t == pg_sys::INT8OID => try_fast_read_heap::<i64>(hdr, info).map(|v| v as f64),
                _ => try_fast_read_heap::<f64>(hdr, info),
            }
        };

        if let Some(v) = val {
            values.push(v);
            nulls.push(0);
        } else {
            // Null or not fast-extractable — mark as null.
            // Caller should handle this via recheck.
            values.push(0.0);
            nulls.push(1);
        }
    }

    (values, nulls)
}

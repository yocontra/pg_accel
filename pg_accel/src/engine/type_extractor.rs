//! Converts between PostgreSQL Datums and GPU-friendly representations.
//!
//! Each [`TypeExtractor`] handles one PostgreSQL type, converting between
//! raw `Datum` values and [`GpuRepr`] for GPU kernel consumption.

use pgrx::pg_sys;

/// GPU-friendly representation of PostgreSQL values.
#[derive(Debug, Clone, PartialEq)]
pub enum GpuRepr {
    /// 64-bit floating point (`float8` / `double precision`).
    Float8(f64),
    /// 32-bit floating point (`float4` / `real`).
    Float4(f32),
    /// 64-bit signed integer (`int8` / `bigint`).
    Int8(i64),
    /// 32-bit signed integer (`int4` / `integer`).
    Int4(i32),
    /// Boolean value.
    Bool(bool),
    /// Timestamp as microseconds since 2000-01-01 00:00:00 UTC.
    Timestamp(i64),
    /// UTF-8 text data.
    Text(Vec<u8>),
    /// Opaque binary passthrough (geometry, etc.).
    Bytes(Vec<u8>),
    /// SQL NULL.
    Null,
}

/// Trait for extracting PostgreSQL datums into GPU representations.
///
/// # Safety
///
/// Implementations must correctly handle the datum format for their type.
/// Callers must ensure datum validity matches the expected OID.
pub trait TypeExtractor: Send + Sync {
    /// The PostgreSQL OID this extractor handles.
    fn oid(&self) -> pg_sys::Oid;

    /// Extract a datum into a [`GpuRepr`].
    ///
    /// # Safety
    ///
    /// Caller must ensure `datum` is valid for this type's OID and that
    /// `is_null` correctly reflects the datum's null status.
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr;

    /// Pack a [`GpuRepr`] back into a datum.
    ///
    /// Returns `None` if the repr is [`GpuRepr::Null`].
    ///
    /// # Safety
    ///
    /// Caller must ensure `repr` matches this extractor's type.
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum>;
}

// ---------------------------------------------------------------------------
// Concrete extractors
// ---------------------------------------------------------------------------

/// Extracts `float8` / `double precision` values.
pub struct Float8Extractor;

impl TypeExtractor for Float8Extractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::FLOAT8OID
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value float8.
        // PostgreSQL stores float8 as the raw bit pattern in the datum.
        let bits = datum.value() as u64;
        GpuRepr::Float8(f64::from_bits(bits))
    }

    #[allow(clippy::cast_possible_truncation)]
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Float8(v) => {
                // SAFETY: Reinterpret f64 bits as usize for datum storage.
                let bits = v.to_bits() as usize;
                Some(pg_sys::Datum::from(bits))
            }
            _ => None,
        }
    }
}

/// Extracts `float4` / `real` values.
pub struct Float4Extractor;

impl TypeExtractor for Float4Extractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::FLOAT4OID
    }

    #[allow(clippy::cast_possible_truncation)]
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value float4.
        // PostgreSQL stores float4 in the low 32 bits of the datum.
        let bits = datum.value() as u32;
        GpuRepr::Float4(f32::from_bits(bits))
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Float4(v) => {
                // SAFETY: Reinterpret f32 bits as usize for datum storage.
                let bits = v.to_bits() as usize;
                Some(pg_sys::Datum::from(bits))
            }
            _ => None,
        }
    }
}

/// Extracts `int8` / `bigint` values.
pub struct Int8Extractor;

impl TypeExtractor for Int8Extractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value int8.
        // On 64-bit platforms, int8 fits in the datum value directly.
        GpuRepr::Int8(datum.value() as i64)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Int8(v) => {
                // SAFETY: Reinterpret i64 as usize for datum storage.
                Some(pg_sys::Datum::from(*v as usize))
            }
            _ => None,
        }
    }
}

/// Extracts `int4` / `integer` values.
pub struct Int4Extractor;

impl TypeExtractor for Int4Extractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::INT4OID
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value int4.
        // PostgreSQL stores int4 in the low 32 bits of the datum.
        GpuRepr::Int4(datum.value() as i32)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Int4(v) => {
                // SAFETY: Reinterpret i32 as usize for datum storage.
                Some(pg_sys::Datum::from(*v as usize))
            }
            _ => None,
        }
    }
}

/// Extracts `bool` values.
pub struct BoolExtractor;

impl TypeExtractor for BoolExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::BOOLOID
    }

    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value bool.
        // PostgreSQL stores bool as 0 or 1 in the datum value.
        GpuRepr::Bool(datum.value() != 0)
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Bool(v) => {
                // SAFETY: Store bool as 0/1 in datum.
                let val = usize::from(*v);
                Some(pg_sys::Datum::from(val))
            }
            _ => None,
        }
    }
}

/// Extracts `timestamp` (without time zone) values.
///
/// PostgreSQL stores timestamps as microseconds since 2000-01-01 00:00:00.
pub struct TimestampExtractor;

impl TypeExtractor for TimestampExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::TIMESTAMPOID
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum holds a pass-by-value timestamp.
        // PostgreSQL stores timestamp as int64 microseconds since J2000.
        GpuRepr::Timestamp(datum.value() as i64)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Timestamp(v) => {
                // SAFETY: Reinterpret i64 microseconds as usize for datum.
                Some(pg_sys::Datum::from(*v as usize))
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Varlena helpers (used by Text and Bytea extractors)
// ---------------------------------------------------------------------------

/// PostgreSQL varlena header size (4 bytes for standard header).
const VARHDRSZ: usize = 4;

/// Detoast a varlena datum and copy its data bytes into an owned `Vec<u8>`.
///
/// # Safety
///
/// `datum` must be a valid pointer to a varlena value. Must be called on the
/// main backend thread (calls `pg_detoast_datum` and potentially `pfree`).
unsafe fn detoast_to_vec(datum: pg_sys::Datum) -> Vec<u8> {
    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
    // SAFETY: pg_detoast_datum handles compressed, external, and short varlena.
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    let ptr = detoasted as *const u8;

    // SAFETY: ptr points to a valid detoasted varlena.
    let (data_ptr, data_len) = unsafe { varlena_data_and_len(ptr) };
    // SAFETY: data_ptr..data_ptr+data_len is within the detoasted varlena.
    let bytes = unsafe { std::slice::from_raw_parts(data_ptr, data_len) }.to_vec();

    // Free the detoasted copy if pg_detoast_datum allocated a new one.
    if detoasted != original {
        // SAFETY: detoasted was allocated by pg_detoast_datum via palloc.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }

    bytes
}

/// Create a new varlena datum from a byte slice, allocated via `palloc`.
///
/// # Safety
///
/// Must be called on the main backend thread (calls `palloc`).
unsafe fn vec_to_varlena_datum(data: &[u8]) -> pg_sys::Datum {
    let total_size = VARHDRSZ + data.len();
    // SAFETY: palloc allocates in the current PG memory context.
    let ptr = unsafe { pg_sys::palloc(total_size) }.cast::<u8>();
    // SET_VARSIZE: store (total_size << 2) in the 4-byte header.
    let header = (total_size as u32) << 2;
    // SAFETY: ptr is freshly allocated with at least total_size bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(header.to_ne_bytes().as_ptr(), ptr, 4);
        if !data.is_empty() {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(VARHDRSZ), data.len());
        }
    }
    pg_sys::Datum::from(ptr as usize)
}

/// Extract the data pointer and length from a flat (detoasted) varlena.
///
/// Handles both 4-byte (standard) and 1-byte (short) varlena headers.
///
/// # Safety
///
/// `ptr` must point to a valid, detoasted (flat) varlena.
unsafe fn varlena_data_and_len(ptr: *const u8) -> (*const u8, usize) {
    // SAFETY: Caller guarantees ptr points to a valid flat varlena.
    let first_byte = unsafe { *ptr };
    if first_byte & 0x01 != 0 {
        // Short varlena: 1-byte header. Total size in bits 1-7.
        let total_size = (first_byte >> 1) as usize;
        let data_len = total_size.saturating_sub(1);
        // SAFETY: data starts 1 byte after the header.
        (unsafe { ptr.add(1) }, data_len)
    } else {
        // Standard 4-byte header. Total size in bits 2-31.
        // SAFETY: ptr points to at least 4 bytes (standard varlena header).
        let header = unsafe { u32::from_ne_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]) };
        let total_size = (header >> 2) as usize;
        let data_len = total_size.saturating_sub(VARHDRSZ);
        // SAFETY: data starts VARHDRSZ bytes after the header.
        (unsafe { ptr.add(VARHDRSZ) }, data_len)
    }
}

/// Extracts `text` values.
///
/// Text is a varlena type in PostgreSQL. This extractor detoasts the
/// datum and copies the UTF-8 bytes into an owned `Vec<u8>`.
pub struct TextExtractor;

impl TypeExtractor for TextExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::TEXTOID
    }

    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum is a valid text varlena pointer.
        // detoast_to_vec handles compressed, external, and short varlena.
        GpuRepr::Text(unsafe { detoast_to_vec(datum) })
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Text(bytes) => {
                // SAFETY: Allocate a new text varlena in the current memory
                // context via palloc.
                Some(unsafe { vec_to_varlena_datum(bytes) })
            }
            _ => None,
        }
    }
}

/// Extracts `bytea` values.
///
/// Bytea is a varlena type in PostgreSQL. This extractor detoasts the
/// datum and copies the raw bytes into an owned `Vec<u8>`.
pub struct ByteaExtractor;

impl TypeExtractor for ByteaExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::BYTEAOID
    }

    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // SAFETY: Caller guarantees datum is a valid bytea varlena pointer.
        GpuRepr::Bytes(unsafe { detoast_to_vec(datum) })
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Bytes(bytes) => {
                // SAFETY: Allocate a new bytea varlena in the current memory
                // context via palloc.
                Some(unsafe { vec_to_varlena_datum(bytes) })
            }
            _ => None,
        }
    }
}

/// Look up an appropriate [`TypeExtractor`] for the given OID.
///
/// Returns `None` if the OID is not supported.
#[must_use]
pub fn extractor_for_oid(oid: pg_sys::Oid) -> Option<Box<dyn TypeExtractor>> {
    match oid {
        pg_sys::FLOAT8OID => Some(Box::new(Float8Extractor)),
        pg_sys::FLOAT4OID => Some(Box::new(Float4Extractor)),
        pg_sys::INT8OID => Some(Box::new(Int8Extractor)),
        pg_sys::INT4OID => Some(Box::new(Int4Extractor)),
        pg_sys::BOOLOID => Some(Box::new(BoolExtractor)),
        pg_sys::TIMESTAMPOID => Some(Box::new(TimestampExtractor)),
        pg_sys::TEXTOID => Some(Box::new(TextExtractor)),
        pg_sys::BYTEAOID => Some(Box::new(ByteaExtractor)),
        _ => None,
    }
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used, dead_code)]
mod tests {
    use super::*;

    /// Helper: round-trip a value through extract then pack.
    unsafe fn round_trip(ext: &dyn TypeExtractor, datum: pg_sys::Datum) -> Option<pg_sys::Datum> {
        // SAFETY: Test-only; we construct known-valid datums.
        let repr = unsafe { ext.extract(datum, false) };
        // SAFETY: Test-only; repr matches the extractor type.
        unsafe { ext.pack(&repr) }
    }

    #[test]
    fn float8_round_trip() {
        let ext = Float8Extractor;
        let original = 3.140_001_f64;
        let datum = pg_sys::Datum::from(original.to_bits() as usize);
        // SAFETY: Test datum constructed from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(original));
    }

    #[test]
    fn float8_negative() {
        let ext = Float8Extractor;
        let original = -273.15_f64;
        let datum = pg_sys::Datum::from(original.to_bits() as usize);
        // SAFETY: Test datum constructed from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(original));
    }

    #[test]
    fn float4_round_trip() {
        let ext = Float4Extractor;
        let original = 2.718_5_f32;
        let datum = pg_sys::Datum::from(original.to_bits() as usize);
        // SAFETY: Test datum constructed from known f32 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f32::from_bits(d.value() as u32));
        assert_eq!(result, Some(original));
    }

    #[test]
    fn int4_round_trip() {
        let ext = Int4Extractor;
        let original: i32 = -42;
        let datum = pg_sys::Datum::from(original as usize);
        // SAFETY: Test datum constructed from known i32 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i32);
        assert_eq!(result, Some(original));
    }

    #[test]
    fn int8_round_trip() {
        let ext = Int8Extractor;
        let original: i64 = -1_000_000_000_000;
        let datum = pg_sys::Datum::from(original as usize);
        // SAFETY: Test datum constructed from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(original));
    }

    #[test]
    fn bool_round_trip_true() {
        let ext = BoolExtractor;
        let datum = pg_sys::Datum::from(1_usize);
        // SAFETY: Test datum constructed from known bool value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() != 0);
        assert_eq!(result, Some(true));
    }

    #[test]
    fn bool_round_trip_false() {
        let ext = BoolExtractor;
        let datum = pg_sys::Datum::from(0_usize);
        // SAFETY: Test datum constructed from known bool value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() != 0);
        assert_eq!(result, Some(false));
    }

    #[test]
    fn null_extract_returns_null() {
        let ext = Float8Extractor;
        // SAFETY: Datum value is irrelevant when is_null is true.
        let repr = unsafe { ext.extract(pg_sys::Datum::from(0_usize), true) };
        assert_eq!(repr, GpuRepr::Null);
    }

    #[test]
    fn null_pack_returns_none() {
        let ext = Int4Extractor;
        // SAFETY: Packing a Null repr.
        let packed = unsafe { ext.pack(&GpuRepr::Null) };
        assert!(packed.is_none());
    }

    #[test]
    fn extractor_for_oid_known() {
        assert!(extractor_for_oid(pg_sys::FLOAT8OID).is_some());
        assert!(extractor_for_oid(pg_sys::INT4OID).is_some());
        assert!(extractor_for_oid(pg_sys::BOOLOID).is_some());
    }

    #[test]
    fn extractor_for_oid_unknown() {
        // OID 0 (InvalidOid) should not match any extractor.
        assert!(extractor_for_oid(pg_sys::Oid::from(0_u32)).is_none());
    }

    #[test]
    fn extractor_for_oid_all_supported_types() {
        let supported = [
            pg_sys::FLOAT8OID,
            pg_sys::FLOAT4OID,
            pg_sys::INT8OID,
            pg_sys::INT4OID,
            pg_sys::BOOLOID,
            pg_sys::TIMESTAMPOID,
            pg_sys::TEXTOID,
            pg_sys::BYTEAOID,
        ];
        for oid in &supported {
            assert!(
                extractor_for_oid(*oid).is_some(),
                "Expected extractor for OID {oid:?}"
            );
        }
    }

    #[test]
    fn extractor_for_oid_returns_correct_oid() {
        let pairs: Vec<(pg_sys::Oid, pg_sys::Oid)> = vec![
            (pg_sys::FLOAT8OID, pg_sys::FLOAT8OID),
            (pg_sys::FLOAT4OID, pg_sys::FLOAT4OID),
            (pg_sys::INT8OID, pg_sys::INT8OID),
            (pg_sys::INT4OID, pg_sys::INT4OID),
            (pg_sys::BOOLOID, pg_sys::BOOLOID),
            (pg_sys::TIMESTAMPOID, pg_sys::TIMESTAMPOID),
            (pg_sys::TEXTOID, pg_sys::TEXTOID),
            (pg_sys::BYTEAOID, pg_sys::BYTEAOID),
        ];
        for (lookup_oid, expected_oid) in pairs {
            let ext = extractor_for_oid(lookup_oid).unwrap();
            assert_eq!(ext.oid(), expected_oid);
        }
    }

    #[test]
    fn extractor_for_unknown_oids() {
        // Various OIDs that are not in the supported set
        let unsupported = [
            pg_sys::Oid::from(0_u32),
            pg_sys::Oid::from(1_u32),
            pg_sys::Oid::from(99999_u32),
        ];
        for oid in &unsupported {
            assert!(extractor_for_oid(*oid).is_none());
        }
    }

    #[test]
    fn null_extract_all_extractors() {
        let extractors: Vec<Box<dyn TypeExtractor>> = vec![
            Box::new(Float8Extractor),
            Box::new(Float4Extractor),
            Box::new(Int8Extractor),
            Box::new(Int4Extractor),
            Box::new(BoolExtractor),
            Box::new(TimestampExtractor),
            Box::new(TextExtractor),
            Box::new(ByteaExtractor),
        ];
        for ext in &extractors {
            // SAFETY: Datum value is irrelevant when is_null is true.
            let repr = unsafe { ext.extract(pg_sys::Datum::from(0_usize), true) };
            assert_eq!(
                repr,
                GpuRepr::Null,
                "Null extract failed for {:?}",
                ext.oid()
            );
        }
    }

    #[test]
    fn pack_wrong_variant_returns_none() {
        // Each extractor should return None when given a mismatched GpuRepr variant.
        let ext_f8 = Float8Extractor;
        // SAFETY: Packing mismatched repr; no real datum access occurs
        // because pack returns None for wrong variants.
        assert!(unsafe { ext_f8.pack(&GpuRepr::Int4(42)) }.is_none());
        assert!(unsafe { ext_f8.pack(&GpuRepr::Bool(true)) }.is_none());
        assert!(unsafe { ext_f8.pack(&GpuRepr::Null) }.is_none());

        // SAFETY: Packing mismatched repr returns None without datum access.
        let ext_f4 = Float4Extractor;
        assert!(unsafe { ext_f4.pack(&GpuRepr::Float8(1.0)) }.is_none());
        assert!(unsafe { ext_f4.pack(&GpuRepr::Int8(1)) }.is_none());

        // SAFETY: Packing mismatched repr returns None without datum access.
        let ext_i8 = Int8Extractor;
        assert!(unsafe { ext_i8.pack(&GpuRepr::Int4(1)) }.is_none());
        assert!(unsafe { ext_i8.pack(&GpuRepr::Float8(1.0)) }.is_none());

        // SAFETY: Packing mismatched repr returns None without datum access.
        let ext_i4 = Int4Extractor;
        assert!(unsafe { ext_i4.pack(&GpuRepr::Int8(1)) }.is_none());
        assert!(unsafe { ext_i4.pack(&GpuRepr::Bool(false)) }.is_none());

        // SAFETY: Packing mismatched repr returns None without datum access.
        let ext_bool = BoolExtractor;
        assert!(unsafe { ext_bool.pack(&GpuRepr::Int4(1)) }.is_none());
        assert!(unsafe { ext_bool.pack(&GpuRepr::Float8(1.0)) }.is_none());

        // SAFETY: Packing mismatched repr returns None without datum access.
        let ext_ts = TimestampExtractor;
        assert!(unsafe { ext_ts.pack(&GpuRepr::Int4(1)) }.is_none());
        assert!(unsafe { ext_ts.pack(&GpuRepr::Bool(true)) }.is_none());
    }

    #[test]
    fn float8_special_values() {
        let ext = Float8Extractor;
        // Zero
        let datum = pg_sys::Datum::from(0.0_f64.to_bits() as usize);
        // SAFETY: Test datum from known f64.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Float8(0.0));

        // Infinity
        let datum = pg_sys::Datum::from(f64::INFINITY.to_bits() as usize);
        // SAFETY: Test datum constructed from known f64 bits.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Float8(f64::INFINITY));

        // Negative infinity
        let datum = pg_sys::Datum::from(f64::NEG_INFINITY.to_bits() as usize);
        // SAFETY: Test datum constructed from known f64 bits.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Float8(f64::NEG_INFINITY));

        // NaN
        let datum = pg_sys::Datum::from(f64::NAN.to_bits() as usize);
        // SAFETY: Test datum constructed from known f64 bits.
        let repr = unsafe { ext.extract(datum, false) };
        match repr {
            GpuRepr::Float8(v) => assert!(v.is_nan()),
            _ => panic!("Expected Float8"),
        }
    }

    #[test]
    fn float4_special_values() {
        let ext = Float4Extractor;
        let datum = pg_sys::Datum::from(f32::INFINITY.to_bits() as usize);
        // SAFETY: Test datum from known f32.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Float4(f32::INFINITY));

        let datum = pg_sys::Datum::from(f32::NAN.to_bits() as usize);
        // SAFETY: Test datum constructed from known f32 bits.
        let repr = unsafe { ext.extract(datum, false) };
        match repr {
            GpuRepr::Float4(v) => assert!(v.is_nan()),
            _ => panic!("Expected Float4"),
        }
    }

    #[test]
    fn int4_boundary_values() {
        let ext = Int4Extractor;

        // i32::MAX
        let datum = pg_sys::Datum::from(i32::MAX as usize);
        // SAFETY: Test datum from known i32.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Int4(i32::MAX));

        // i32::MIN
        let datum = pg_sys::Datum::from(i32::MIN as usize);
        // SAFETY: Test datum constructed from known i32 value.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Int4(i32::MIN));

        // Zero
        let datum = pg_sys::Datum::from(0_usize);
        // SAFETY: Test datum constructed from known i32 value.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Int4(0));
    }

    #[test]
    fn int8_boundary_values() {
        let ext = Int8Extractor;

        let datum = pg_sys::Datum::from(i64::MAX as usize);
        // SAFETY: Test datum from known i64.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Int8(i64::MAX));

        let datum = pg_sys::Datum::from(i64::MIN as usize);
        // SAFETY: Test datum constructed from known i64 value.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Int8(i64::MIN));
    }

    #[test]
    fn timestamp_round_trip() {
        let ext = TimestampExtractor;
        // Microseconds since J2000 epoch
        let original: i64 = 788_918_400_000_000; // some timestamp
        let datum = pg_sys::Datum::from(original as usize);
        // SAFETY: Test datum from known i64 timestamp.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(original));
    }

    #[test]
    fn timestamp_negative_value() {
        let ext = TimestampExtractor;
        // Negative = before J2000
        let original: i64 = -100_000_000;
        let datum = pg_sys::Datum::from(original as usize);
        // SAFETY: Test datum from known i64.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(original));
    }

    #[test]
    fn timestamp_zero() {
        let ext = TimestampExtractor;
        let datum = pg_sys::Datum::from(0_usize);
        // SAFETY: Test datum from known value.
        let repr = unsafe { ext.extract(datum, false) };
        assert_eq!(repr, GpuRepr::Timestamp(0));
    }

    #[test]
    fn gpu_repr_partial_eq() {
        assert_eq!(GpuRepr::Float8(1.0), GpuRepr::Float8(1.0));
        assert_ne!(GpuRepr::Float8(1.0), GpuRepr::Float8(2.0));
        assert_ne!(GpuRepr::Float8(1.0), GpuRepr::Float4(1.0));
        assert_ne!(GpuRepr::Int4(1), GpuRepr::Int8(1));
        assert_eq!(GpuRepr::Null, GpuRepr::Null);
        assert_ne!(GpuRepr::Null, GpuRepr::Bool(false));
        assert_eq!(GpuRepr::Text(vec![1, 2, 3]), GpuRepr::Text(vec![1, 2, 3]));
        assert_ne!(GpuRepr::Text(vec![1, 2, 3]), GpuRepr::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn gpu_repr_clone() {
        let original = GpuRepr::Float8(42.0);
        let cloned = original.clone();
        assert_eq!(original, cloned);

        let original = GpuRepr::Text(vec![104, 105]);
        let cloned = original.clone();
        assert_eq!(original, cloned);

        let original = GpuRepr::Null;
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn gpu_repr_debug() {
        assert!(format!("{:?}", GpuRepr::Float8(1.0)).contains("Float8"));
        assert!(format!("{:?}", GpuRepr::Float4(1.0)).contains("Float4"));
        assert!(format!("{:?}", GpuRepr::Int8(1)).contains("Int8"));
        assert!(format!("{:?}", GpuRepr::Int4(1)).contains("Int4"));
        assert!(format!("{:?}", GpuRepr::Bool(true)).contains("Bool"));
        assert!(format!("{:?}", GpuRepr::Timestamp(0)).contains("Timestamp"));
        assert!(format!("{:?}", GpuRepr::Text(vec![])).contains("Text"));
        assert!(format!("{:?}", GpuRepr::Bytes(vec![])).contains("Bytes"));
        assert!(format!("{:?}", GpuRepr::Null).contains("Null"));
    }

    #[test]
    fn varlena_data_and_len_standard_header() {
        // Standard 4-byte header: total_size = 12 (4 header + 8 data)
        // Header value = total_size << 2 = 12 << 2 = 48
        let total_size: u32 = 12;
        let header = total_size << 2;
        let header_bytes = header.to_ne_bytes();
        let mut buf = vec![0u8; 12];
        buf[0] = header_bytes[0];
        buf[1] = header_bytes[1];
        buf[2] = header_bytes[2];
        buf[3] = header_bytes[3];
        // Fill data portion with known bytes
        for (i, byte) in buf[4..].iter_mut().enumerate() {
            *byte = (i + 1) as u8;
        }

        // SAFETY: buf is a valid flat varlena with standard header.
        let (data_ptr, data_len) = unsafe { varlena_data_and_len(buf.as_ptr()) };
        assert_eq!(data_len, 8);
        // SAFETY: data_ptr points into buf.
        let data = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };
        assert_eq!(data, &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn varlena_data_and_len_short_header() {
        // Short varlena: 1-byte header where bit 0 is set.
        // total_size in bits 1-7. E.g. total_size=5 (1 header + 4 data)
        // header byte = (5 << 1) | 1 = 11
        let total_size: u8 = 5;
        let header_byte = (total_size << 1) | 1;
        let mut buf = vec![0u8; 5];
        buf[0] = header_byte;
        buf[1] = b'A';
        buf[2] = b'B';
        buf[3] = b'C';
        buf[4] = b'D';

        // SAFETY: buf is a valid flat short varlena.
        let (data_ptr, data_len) = unsafe { varlena_data_and_len(buf.as_ptr()) };
        assert_eq!(data_len, 4);
        // SAFETY: data_ptr points into buf which is still alive.
        let data = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };
        assert_eq!(data, b"ABCD");
    }

    #[test]
    fn varlena_data_and_len_short_header_minimal() {
        // Minimal short varlena: total_size=1 (header only, no data)
        let header_byte: u8 = (1 << 1) | 1; // = 3
        let buf = [header_byte];

        // SAFETY: buf is a valid minimal short varlena.
        let (_data_ptr, data_len) = unsafe { varlena_data_and_len(buf.as_ptr()) };
        assert_eq!(data_len, 0);
    }

    // -- f64 boundary values ---------------------------------------------------

    #[test]
    fn float8_min_max_round_trip() {
        let ext = Float8Extractor;

        // f64::MIN (most negative finite)
        let datum = pg_sys::Datum::from(f64::MIN.to_bits() as usize);
        // SAFETY: Test datum from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(f64::MIN));

        // f64::MAX (most positive finite)
        let datum = pg_sys::Datum::from(f64::MAX.to_bits() as usize);
        // SAFETY: Test datum from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(f64::MAX));

        // f64::MIN_POSITIVE (smallest positive normal)
        let datum = pg_sys::Datum::from(f64::MIN_POSITIVE.to_bits() as usize);
        // SAFETY: Test datum from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(f64::MIN_POSITIVE));

        // f64::EPSILON
        let datum = pg_sys::Datum::from(f64::EPSILON.to_bits() as usize);
        // SAFETY: Test datum from known f64 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f64::from_bits(d.value() as u64));
        assert_eq!(result, Some(f64::EPSILON));
    }

    // -- f32 boundary values ---------------------------------------------------

    #[test]
    fn float4_min_max_round_trip() {
        let ext = Float4Extractor;

        // f32::MIN
        let datum = pg_sys::Datum::from(f32::MIN.to_bits() as usize);
        // SAFETY: Test datum from known f32 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f32::from_bits(d.value() as u32));
        assert_eq!(result, Some(f32::MIN));

        // f32::MAX
        let datum = pg_sys::Datum::from(f32::MAX.to_bits() as usize);
        // SAFETY: Test datum from known f32 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f32::from_bits(d.value() as u32));
        assert_eq!(result, Some(f32::MAX));

        // f32::MIN_POSITIVE
        let datum = pg_sys::Datum::from(f32::MIN_POSITIVE.to_bits() as usize);
        // SAFETY: Test datum from known f32 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f32::from_bits(d.value() as u32));
        assert_eq!(result, Some(f32::MIN_POSITIVE));

        // f32::EPSILON
        let datum = pg_sys::Datum::from(f32::EPSILON.to_bits() as usize);
        // SAFETY: Test datum from known f32 bits.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| f32::from_bits(d.value() as u32));
        assert_eq!(result, Some(f32::EPSILON));
    }

    #[test]
    fn float4_negative_zero() {
        let ext = Float4Extractor;
        let neg_zero = (-0.0_f32).to_bits();
        let datum = pg_sys::Datum::from(neg_zero as usize);
        // SAFETY: Test datum from known f32 bits.
        let repr = unsafe { ext.extract(datum, false) };
        match repr {
            GpuRepr::Float4(v) => {
                assert!(v.is_sign_negative());
                assert_eq!(v, 0.0); // -0.0 == 0.0 in IEEE 754
            }
            _ => panic!("Expected Float4"),
        }
    }

    #[test]
    fn float8_negative_zero() {
        let ext = Float8Extractor;
        let neg_zero = (-0.0_f64).to_bits();
        let datum = pg_sys::Datum::from(neg_zero as usize);
        // SAFETY: Test datum from known f64 bits.
        let repr = unsafe { ext.extract(datum, false) };
        match repr {
            GpuRepr::Float8(v) => {
                assert!(v.is_sign_negative());
                assert_eq!(v, 0.0);
            }
            _ => panic!("Expected Float8"),
        }
    }

    // -- int4 round-trip at boundaries ----------------------------------------

    #[test]
    fn int4_max_round_trip() {
        let ext = Int4Extractor;
        let datum = pg_sys::Datum::from(i32::MAX as usize);
        // SAFETY: Test datum from known i32 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i32);
        assert_eq!(result, Some(i32::MAX));
    }

    #[test]
    fn int4_min_round_trip() {
        let ext = Int4Extractor;
        let datum = pg_sys::Datum::from(i32::MIN as usize);
        // SAFETY: Test datum from known i32 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i32);
        assert_eq!(result, Some(i32::MIN));
    }

    // -- int8 round-trip at boundaries ----------------------------------------

    #[test]
    fn int8_max_round_trip() {
        let ext = Int8Extractor;
        let datum = pg_sys::Datum::from(i64::MAX as usize);
        // SAFETY: Test datum from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(i64::MAX));
    }

    #[test]
    fn int8_min_round_trip() {
        let ext = Int8Extractor;
        let datum = pg_sys::Datum::from(i64::MIN as usize);
        // SAFETY: Test datum from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(i64::MIN));
    }

    #[test]
    fn int8_zero_round_trip() {
        let ext = Int8Extractor;
        let datum = pg_sys::Datum::from(0_usize);
        // SAFETY: Test datum from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(0_i64));
    }

    // -- timestamp boundary values --------------------------------------------

    #[test]
    fn timestamp_max_round_trip() {
        let ext = TimestampExtractor;
        let datum = pg_sys::Datum::from(i64::MAX as usize);
        // SAFETY: Test datum from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(i64::MAX));
    }

    #[test]
    fn timestamp_min_round_trip() {
        let ext = TimestampExtractor;
        let datum = pg_sys::Datum::from(i64::MIN as usize);
        // SAFETY: Test datum from known i64 value.
        let packed = unsafe { round_trip(&ext, datum) };
        let result = packed.map(|d| d.value() as i64);
        assert_eq!(result, Some(i64::MIN));
    }

    // -- bool edge cases ------------------------------------------------------

    #[test]
    fn bool_nonzero_values_are_true() {
        let ext = BoolExtractor;
        // PostgreSQL stores bool as 0/1, but any nonzero should be true.
        for val in [1_usize, 2, 255, usize::MAX] {
            let datum = pg_sys::Datum::from(val);
            // SAFETY: Test datum from known value.
            let repr = unsafe { ext.extract(datum, false) };
            assert_eq!(repr, GpuRepr::Bool(true), "Expected true for value {val}");
        }
    }

    // -- unknown type handling ------------------------------------------------

    #[test]
    fn extractor_for_high_oid_returns_none() {
        // Arbitrary high OIDs that don't correspond to supported types.
        assert!(extractor_for_oid(pg_sys::Oid::from(100_000_u32)).is_none());
        assert!(extractor_for_oid(pg_sys::Oid::from(u32::MAX)).is_none());
    }

    #[test]
    fn varlena_data_and_len_standard_header_minimal() {
        // Standard header with total_size = VARHDRSZ (4), meaning 0 data bytes.
        let total_size: u32 = 4;
        let header = total_size << 2;
        let buf = header.to_ne_bytes();

        // SAFETY: buf is a valid standard varlena with no data.
        let (_data_ptr, data_len) = unsafe { varlena_data_and_len(buf.as_ptr()) };
        assert_eq!(data_len, 0);
    }
}

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

/// Extracts `text` values.
///
/// Text is a varlena type in PostgreSQL. Full varlena deserialization
/// is deferred to Phase 2; this extractor stores the raw pointer bytes.
pub struct TextExtractor;

impl TypeExtractor for TextExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::TEXTOID
    }

    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // TODO(phase2): Proper varlena detoasting via pg_detoast_datum.
        // For now, store the raw pointer address as opaque bytes so the
        // type system round-trips without dereferencing invalid memory.
        let addr = datum.value();
        GpuRepr::Text(addr.to_ne_bytes().to_vec())
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Text(bytes) => {
                // TODO(phase2): Proper varlena allocation via pg_sys::palloc.
                // For now, reconstruct the pointer address from stored bytes.
                if bytes.len() == size_of::<usize>() {
                    let mut buf = [0u8; size_of::<usize>()];
                    buf.copy_from_slice(bytes);
                    let addr = usize::from_ne_bytes(buf);
                    Some(pg_sys::Datum::from(addr))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Extracts `bytea` values.
///
/// Bytea is a varlena type in PostgreSQL. Full varlena deserialization
/// is deferred to Phase 2; this extractor stores the raw pointer bytes.
pub struct ByteaExtractor;

impl TypeExtractor for ByteaExtractor {
    fn oid(&self) -> pg_sys::Oid {
        pg_sys::BYTEAOID
    }

    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null {
            return GpuRepr::Null;
        }
        // TODO(phase2): Proper varlena detoasting via pg_detoast_datum.
        // Store raw pointer address as opaque bytes for now.
        let addr = datum.value();
        GpuRepr::Bytes(addr.to_ne_bytes().to_vec())
    }

    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum> {
        match repr {
            GpuRepr::Bytes(bytes) => {
                // TODO(phase2): Proper varlena allocation via pg_sys::palloc.
                if bytes.len() == size_of::<usize>() {
                    let mut buf = [0u8; size_of::<usize>()];
                    buf.copy_from_slice(bytes);
                    let addr = usize::from_ne_bytes(buf);
                    Some(pg_sys::Datum::from(addr))
                } else {
                    None
                }
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

#[cfg(test)]
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
        let original = 3.14_f64;
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
        let original = 2.718_f32;
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
}

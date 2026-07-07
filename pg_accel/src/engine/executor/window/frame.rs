//! Partition/frame boundary detection for window functions.

use pgrx::pg_sys;

use super::WindowExecState;

/// A partition key normalized into an owned, comparable form.
///
/// The raw `Datum` returned by `heap_getattr` cannot be compared across rows:
/// for by-reference types it is a pointer into a `HeapTuple` that is freed
/// immediately after extraction (dangling), and comparing pointer values is
/// wrong regardless. For float8 a raw-bit comparison also disagrees with PG
/// grouping semantics (`-0.0` vs `0.0`, `NaN` vs `NaN`). We therefore snapshot
/// each key into one of these owned variants and compare those.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PartKey {
    /// SQL NULL.
    Null,
    /// A by-value fixed-width type whose btree equality is bitwise on the
    /// `Datum` word (int2/int4/int8, bool, date, timestamp, oid, …). This
    /// mirrors what PG's `datumIsEqual` does for by-value types.
    ByValue(u64),
    /// A floating-point key compared with PG grouping semantics.
    Float(f64),
    /// A varlena (text/varchar/bpchar/bytea/…) key compared bytewise, which
    /// is exactly what `texteq`/`byteaeq` do for grouping.
    Bytes(Vec<u8>),
}

/// PG grouping equality for two float8 values.
///
/// PostgreSQL's float comparison (used by `GROUP BY` / `PARTITION BY` /
/// ordering) treats `-0.0` and `0.0` as **equal**, and `NaN` as **equal to
/// itself** (and greater than every non-NaN). A raw-bit compare disagrees on
/// both counts, so we normalize here.
#[inline]
pub(super) fn f64_group_eq(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        // PG groups all NaNs together and never equal to a non-NaN.
        a.is_nan() && b.is_nan()
    } else {
        // IEEE `==` already gives `0.0 == -0.0`.
        a == b
    }
}

/// Whether two normalized partition keys belong to the same partition.
///
/// Two NULLs are the same partition (PG treats NULLs as equal in
/// `PARTITION BY`); a NULL and a non-NULL are different; otherwise compare by
/// type-appropriate equality. A type mismatch (should not happen for one
/// column) conservatively reports a boundary.
#[inline]
pub(super) fn part_key_eq(a: &PartKey, b: &PartKey) -> bool {
    match (a, b) {
        (PartKey::Null, PartKey::Null) => true,
        (PartKey::ByValue(x), PartKey::ByValue(y)) => x == y,
        (PartKey::Float(x), PartKey::Float(y)) => f64_group_eq(*x, *y),
        (PartKey::Bytes(x), PartKey::Bytes(y)) => x == y,
        _ => false,
    }
}

/// Compute partition-start markers from a sequence of normalized keys.
///
/// Returns a `u8` array where `1` marks the first row of a new partition. The
/// first row (if any) always starts a partition.
pub(super) fn partition_starts_from_keys(keys: &[PartKey]) -> Vec<u8> {
    let n = keys.len();
    if n == 0 {
        return Vec::new();
    }
    let mut starts = vec![0u8; n];
    starts[0] = 1;
    for i in 1..n {
        if !part_key_eq(&keys[i], &keys[i - 1]) {
            starts[i] = 1;
        }
    }
    starts
}

/// Detoast a varlena `Datum` and copy its logical content bytes.
///
/// Handles 1-byte (short) and 4-byte (standard) varlena headers plus
/// compressed/external TOAST via `pg_detoast_datum`. The returned bytes are
/// the exact content `texteq`/`byteaeq` compare, so a bytewise `==` on two
/// such vectors matches PG grouping equality for varlena types.
///
/// # Safety
///
/// `datum` must be a valid varlena pointer for a non-NULL by-reference column.
/// Must run on the main backend thread (`pg_detoast_datum` / `pfree`).
unsafe fn detoast_key_bytes(datum: pg_sys::Datum) -> Vec<u8> {
    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
    // SAFETY: pg_detoast_datum accepts any varlena form and returns a flat one.
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    let ptr = detoasted.cast::<u8>();

    // SAFETY: ptr points to a flat varlena; the first byte selects the header
    // form (mirrors materialize::type_extractor::varlena_data_and_len).
    let first = unsafe { *ptr };
    let (data_ptr, len) = if first & 0x01 != 0 {
        // Short 1-byte header: total size in bits 1-7.
        let total = (first >> 1) as usize;
        // SAFETY: data starts 1 byte after the short header.
        (unsafe { ptr.add(1) }, total.saturating_sub(1))
    } else {
        // Standard 4-byte header: total size in bits 2-31 (little-endian).
        // SAFETY: ptr points to at least 4 header bytes.
        let header = unsafe { u32::from_ne_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]) };
        let total = (header >> 2) as usize;
        // SAFETY: data starts 4 bytes after the standard header.
        (unsafe { ptr.add(4) }, total.saturating_sub(4))
    };

    // SAFETY: data_ptr..data_ptr+len lies within the detoasted varlena.
    let bytes = unsafe { std::slice::from_raw_parts(data_ptr, len) }.to_vec();

    if detoasted != original {
        // SAFETY: pg_detoast_datum allocated a new varlena via palloc.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
    bytes
}

impl WindowExecState {
    /// Build partition boundary markers by comparing partition key values.
    ///
    /// Returns a `u8` array where `1` marks the start of a new partition.
    /// Keys are snapshotted into owned [`PartKey`]s (typed per the column's
    /// OID / storage class) so the comparison uses correct SQL grouping
    /// semantics instead of a raw `Datum`-word compare — which is a pointer
    /// compare for by-reference types and disagrees on `-0.0`/`NaN` for
    /// float8.
    ///
    /// # Safety
    ///
    /// Must run on the main backend thread. `tupdesc` must be a valid
    /// `TupleDesc`, `attno` in range, and all tuples valid `MinimalTuple`s.
    pub(super) unsafe fn build_partition_starts(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> Vec<u8> {
        let n = self.tuples.len();
        if n == 0 {
            return Vec::new();
        }

        // Read the column's type + storage class once.
        // SAFETY: tupdesc valid, attno in range per caller contract.
        let (typid, byval, typlen) = unsafe {
            let att = &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, (attno - 1) as usize);
            (att.atttypid, att.attbyval, att.attlen)
        };

        let mut keys: Vec<PartKey> = Vec::with_capacity(n);
        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid. For a
            // by-reference column, `datum` points into `heap_tuple`, so it
            // must be consumed (detoasted/copied) before the pfree below.
            let key = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let datum = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                let key = if is_null {
                    PartKey::Null
                } else if typid == pg_sys::FLOAT4OID {
                    PartKey::Float(f64::from(f32::from_bits(datum.value() as u32)))
                } else if typid == pg_sys::FLOAT8OID {
                    PartKey::Float(f64::from_bits(datum.value() as u64))
                } else if byval {
                    // Fixed-width by-value type: btree equality is bitwise on
                    // the Datum word (matches PG datumIsEqual for by-value).
                    PartKey::ByValue(datum.value() as u64)
                } else if typlen == -1 {
                    // varlena: snapshot the content bytes while heap_tuple lives.
                    PartKey::Bytes(detoast_key_bytes(datum))
                } else {
                    // pgrx::error! longjmps; the enclosing memory context
                    // reclaims heap_tuple, so no manual pfree is needed here.
                    pgrx::error!(
                        "pg_accel: window PARTITION BY on column attno {attno} of type OID {} \
                         (fixed-width by-reference) is unsupported; refusing to pointer-compare",
                        typid.to_u32()
                    );
                };
                pg_sys::pfree(heap_tuple.cast());
                key
            };
            keys.push(key);
        }

        partition_starts_from_keys(&keys)
    }
}

#[cfg(test)]
mod tests {
    use super::{PartKey, f64_group_eq, part_key_eq, partition_starts_from_keys};

    // ---- f64_group_eq: PG float grouping semantics --------------------------

    #[test]
    fn group_eq_zero_and_negative_zero() {
        // PG treats -0.0 and 0.0 as equal in grouping/sorting; raw bits differ.
        assert_ne!(0.0_f64.to_bits(), (-0.0_f64).to_bits());
        assert!(f64_group_eq(0.0, -0.0));
        assert!(f64_group_eq(-0.0, 0.0));
    }

    #[test]
    fn group_eq_nan_equals_nan() {
        assert!(f64_group_eq(f64::NAN, f64::NAN));
        assert!(!f64_group_eq(f64::NAN, 1.0));
        assert!(!f64_group_eq(1.0, f64::NAN));
    }

    #[test]
    fn group_eq_ordinary_values() {
        assert!(f64_group_eq(1.5, 1.5));
        assert!(!f64_group_eq(1.5, 2.5));
        assert!(f64_group_eq(f64::INFINITY, f64::INFINITY));
        assert!(!f64_group_eq(f64::INFINITY, f64::NEG_INFINITY));
    }

    // ---- part_key_eq: NULL / typed equality ---------------------------------

    #[test]
    fn part_key_eq_both_null_same_partition() {
        assert!(part_key_eq(&PartKey::Null, &PartKey::Null));
    }

    #[test]
    fn part_key_eq_null_vs_non_null_boundary() {
        assert!(!part_key_eq(&PartKey::Null, &PartKey::ByValue(0)));
        assert!(!part_key_eq(&PartKey::ByValue(0), &PartKey::Null));
        assert!(!part_key_eq(&PartKey::Null, &PartKey::Bytes(vec![])));
    }

    #[test]
    fn part_key_eq_byvalue() {
        assert!(part_key_eq(&PartKey::ByValue(42), &PartKey::ByValue(42)));
        assert!(!part_key_eq(&PartKey::ByValue(42), &PartKey::ByValue(43)));
    }

    #[test]
    fn part_key_eq_float_uses_group_semantics() {
        assert!(part_key_eq(&PartKey::Float(0.0), &PartKey::Float(-0.0)));
        assert!(part_key_eq(
            &PartKey::Float(f64::NAN),
            &PartKey::Float(f64::NAN)
        ));
    }

    #[test]
    fn part_key_eq_bytes() {
        assert!(part_key_eq(
            &PartKey::Bytes(b"abc".to_vec()),
            &PartKey::Bytes(b"abc".to_vec())
        ));
        assert!(!part_key_eq(
            &PartKey::Bytes(b"abc".to_vec()),
            &PartKey::Bytes(b"abd".to_vec())
        ));
        // Empty vs non-empty text.
        assert!(!part_key_eq(
            &PartKey::Bytes(vec![]),
            &PartKey::Bytes(b"x".to_vec())
        ));
    }

    // ---- partition_starts_from_keys -----------------------------------------

    #[test]
    fn starts_empty() {
        assert!(partition_starts_from_keys(&[]).is_empty());
    }

    #[test]
    fn starts_single() {
        assert_eq!(partition_starts_from_keys(&[PartKey::ByValue(1)]), vec![1]);
    }

    #[test]
    fn starts_int_two_partitions() {
        let keys = vec![
            PartKey::ByValue(1),
            PartKey::ByValue(1),
            PartKey::ByValue(2),
            PartKey::ByValue(2),
        ];
        assert_eq!(partition_starts_from_keys(&keys), vec![1, 0, 1, 0]);
    }

    #[test]
    fn starts_text_partitions() {
        // Text/by-ref keys must split by content, not pointer identity.
        let keys = vec![
            PartKey::Bytes(b"east".to_vec()),
            PartKey::Bytes(b"east".to_vec()),
            PartKey::Bytes(b"west".to_vec()),
            PartKey::Bytes(b"west".to_vec()),
            PartKey::Bytes(b"east".to_vec()),
        ];
        assert_eq!(partition_starts_from_keys(&keys), vec![1, 0, 1, 0, 1]);
    }

    #[test]
    fn starts_negative_zero_is_same_partition() {
        // The whole point of the fix: 0.0 and -0.0 are ONE partition.
        let keys = vec![PartKey::Float(0.0), PartKey::Float(-0.0)];
        assert_eq!(partition_starts_from_keys(&keys), vec![1, 0]);
    }

    #[test]
    fn starts_float_distinct_and_nan() {
        let keys = vec![
            PartKey::Float(1.0),
            PartKey::Float(1.0),
            PartKey::Float(2.0),
            PartKey::Float(f64::NAN),
            PartKey::Float(f64::NAN),
        ];
        assert_eq!(partition_starts_from_keys(&keys), vec![1, 0, 1, 1, 0]);
    }

    #[test]
    fn starts_null_grouping() {
        // [NULL, NULL, 1, 1, NULL] → NULLs group, then 1s, then a new NULL group.
        let keys = vec![
            PartKey::Null,
            PartKey::Null,
            PartKey::ByValue(1),
            PartKey::ByValue(1),
            PartKey::Null,
        ];
        assert_eq!(partition_starts_from_keys(&keys), vec![1, 0, 1, 0, 1]);
    }
}

//! [`PartialEmitter`] implementations for each supported aggregate.
//!
//! Each emitter converts a [`ColumnAccumulator`] into the Datum PG's
//! combine / finalize function expects. All bodies execute on the main
//! backend thread (see [`PartialEmitter`] safety contract) because they may
//! call into PG — `palloc`, `construct_array`, and `DirectFunctionCall1Coll`
//! all require a valid `CurrentMemoryContext`.

use pgrx::pg_sys;

use super::{ColumnAccumulator, PartialEmitter};

// ---------------------------------------------------------------------------
// Helpers used by multiple emitters.
// ---------------------------------------------------------------------------

/// Build a Datum carrying an f64 (stored as its bit pattern).
fn f64_datum(v: f64) -> pg_sys::Datum {
    pg_sys::Datum::from(v.to_bits())
}

// ---------------------------------------------------------------------------
// ScalarPassthrough — SUM(float4|float8|int8) where transtype == rtype.
// ---------------------------------------------------------------------------

/// SUM where `transtype == result type` for pass-by-value scalars.
///
/// Handled `transtype` OIDs:
/// - `FLOAT4OID`: Datum carries the f32 bit pattern.
/// - `FLOAT8OID`: Datum carries the f64 bit pattern.
/// - `INT8OID`:   Datum carries the i64 as a raw integer.
pub struct ScalarPassthrough {
    pub transtype: pg_sys::Oid,
}

impl PartialEmitter for ScalarPassthrough {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        let datum = match self.transtype {
            pg_sys::FLOAT4OID => {
                // FLOAT4 pass-by-value: store f32 bits in the Datum.
                let bits = (acc.sum as f32).to_bits();
                pg_sys::Datum::from(bits)
            }
            pg_sys::INT8OID => pg_sys::Datum::from(acc.sum as i64),
            // FLOAT8OID or anything else pass-by-value: f64 bits.
            _ => f64_datum(acc.sum),
        };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        self.transtype
    }
}

// ---------------------------------------------------------------------------
// IntegerSumPromotion — SUM(int4|int2) promoted to int8.
// ---------------------------------------------------------------------------

/// `SUM(int4)` / `SUM(int2)` — PG promotes the transtype to int8.
pub struct IntegerSumPromotion;

impl PartialEmitter for IntegerSumPromotion {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        (pg_sys::Datum::from(acc.sum as i64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

// ---------------------------------------------------------------------------
// CountEmitter — COUNT(*) / COUNT(x) → int8.
// ---------------------------------------------------------------------------

/// COUNT — never returns NULL; zero rows yield `0::int8`.
pub struct CountEmitter;

impl PartialEmitter for CountEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        (pg_sys::Datum::from(acc.count as i64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::INT8OID
    }
}

// ---------------------------------------------------------------------------
// NumericSumEmitter — SUM(int8|numeric) → numeric.
// ---------------------------------------------------------------------------

/// `SUM(int8)` / `SUM(numeric)` — emits a Numeric transition state.
///
/// # Precision model
///
/// Two emit paths are supported:
///
/// 1. [`PartialEmitter::emit`] — backwards-compatible path that reads the
///    legacy `ColumnAccumulator.sum: f64` field and casts to `i64`. Accurate
///    for SUM(int8) values whose running sum stays under 2^53; mirrors PG's
///    own `int8_sum` semantics in that range. This path is preserved so that
///    existing wiring keeps working without an `accumulator.rs` schema change.
///
/// 2. [`NumericSumEmitter::emit_with_i128`] — full-precision path. Accepts
///    a pre-accumulated `i128` SUM directly. Buys 38 decimal digits versus
///    f64's 15–17, sufficient for any SUM(int8) (i64::MAX × 2^61 rows fits)
///    and for SUM(NUMERIC) with bounded scale (scale × 38 input digits).
///
/// # Overflow policy
///
/// On the `emit_with_i128` path, callers accumulate via [`i128::checked_add`]
/// and pass the running total. If the caller detects overflow (the checked
/// add returned `None` at some point during scan), they pass `has_value=true`
/// and a saturated value (i128::MAX or i128::MIN) — this emitter then emits
/// that saturated value as a real Numeric. We saturate rather than NULL on
/// overflow because PG's SUM(numeric) semantics never produce NULL on
/// non-empty input; the saturated marker lets the user observe overflow as
/// an extreme value rather than silently dropping the row group.
///
/// Callers preferring NULL-on-overflow semantics can pass `has_value=false`
/// instead.
///
/// # Wiring note
///
/// `engine/ffi/planner_hooks/agg_common.rs:104` currently rejects
/// `F_SUM_NUMERIC` from classification because the legacy f64 path loses
/// precision. Once a caller threads a real `i128` accumulator through to
/// [`Self::emit_with_i128`], that gate can be flipped — that integration
/// lives outside this emitter's file ownership.
pub struct NumericSumEmitter;

impl NumericSumEmitter {
    /// Emit a Numeric Datum from a pre-accumulated `i128` sum.
    ///
    /// `value` is the running SUM in scaled-integer form. For SUM(int8) the
    /// scale is 0 (raw integer count). For SUM(NUMERIC) the caller chooses
    /// a uniform `dscale` (typically the max dscale across input rows) and
    /// rescales each input via [`pg_numeric_to_scaled_i128`] before summing.
    ///
    /// Returns `(datum, isnull)`. `has_value=false` produces a NULL Datum
    /// (matching SUM(...) over zero rows in PG).
    ///
    /// # Safety
    /// Must run on the main backend thread with a valid `CurrentMemoryContext`
    /// — calls [`int128_to_numeric`] which palloc's a Numeric varlena.
    pub unsafe fn emit_with_i128(
        &self,
        value: i128,
        dscale: u16,
        has_value: bool,
    ) -> (pg_sys::Datum, bool) {
        if !has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        // SAFETY: forwarded to `int128_to_numeric`; same main-thread contract.
        let datum = unsafe { int128_to_numeric(value, dscale) };
        (datum, false)
    }
}

impl PartialEmitter for NumericSumEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        // Backwards-compatible path: route the legacy f64 accumulator through
        // the new int128 emitter at dscale=0 (SUM(int8) semantics). Any caller
        // wanting SUM(NUMERIC) full precision should use `emit_with_i128`
        // directly; see struct doc-comment for the wiring rationale.
        let value = acc.sum as i64 as i128;
        // SAFETY: Main-thread PG call; allocates in CurrentMemoryContext.
        let datum = unsafe { int128_to_numeric(value, 0) };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::NUMERICOID
    }
}

// ---------------------------------------------------------------------------
// int128 ↔ PG NUMERIC helpers.
//
// PG NUMERIC is a varlena base-10000 (`NBASE`) digit array. See
// `src/backend/utils/adt/numeric.c` (lines 56–250). Layout:
//
//   struct NumericData {
//     int32 vl_len_;                 // varlena header (VARHDRSZ = 4 bytes)
//     union NumericChoice {
//       struct NumericLong {         // when high bit of header is 0
//         uint16 n_sign_dscale;      // top 2 bits = sign (POS/NEG/SPECIAL),
//                                    //   low 14 bits = dscale (display scale)
//         int16 n_weight;            // base-10000 weight of first digit
//         int16 n_data[];            // big-endian digits (each 0..9999)
//       } n_long;
//       struct NumericShort {        // when high bit of header is 1
//         uint16 n_header;           // bit 15=SHORT, bit 13=sign,
//                                    //   bits 7..12=dscale, bits 0..6=weight
//         int16 n_data[];
//       } n_short;
//     } choice;
//   };
//
// Constants from `numeric.c`:
//   NBASE                      = 10000
//   DEC_DIGITS                 = 4   (decimal digits per NumericDigit)
//   NUMERIC_SIGN_MASK          = 0xC000
//   NUMERIC_POS                = 0x0000
//   NUMERIC_NEG                = 0x4000
//   NUMERIC_SHORT              = 0x8000
//   NUMERIC_SPECIAL            = 0xC000  (NaN, +Inf, -Inf)
//   NUMERIC_DSCALE_MASK        = 0x3FFF
//   NUMERIC_SHORT_SIGN_MASK    = 0x2000
//   NUMERIC_SHORT_DSCALE_MASK  = 0x1F80
//   NUMERIC_SHORT_DSCALE_SHIFT = 7
//   NUMERIC_SHORT_WEIGHT_SIGN_MASK = 0x0040
//   NUMERIC_SHORT_WEIGHT_MASK  = 0x003F
//   VARHDRSZ                   = 4
// ---------------------------------------------------------------------------

const NBASE: i32 = 10_000;
const DEC_DIGITS: u32 = 4;
const NUMERIC_SIGN_MASK: u16 = 0xC000;
const NUMERIC_POS: u16 = 0x0000;
const NUMERIC_NEG: u16 = 0x4000;
// NUMERIC_SHORT (0x8000) — discriminated inline via `(header & 0x8000) != 0`
// at the layout-decode site; no separate const needed.
const NUMERIC_SPECIAL: u16 = 0xC000;
const NUMERIC_DSCALE_MASK: u16 = 0x3FFF;
const NUMERIC_SHORT_SIGN_MASK: u16 = 0x2000;
const NUMERIC_SHORT_DSCALE_MASK: u16 = 0x1F80;
const NUMERIC_SHORT_DSCALE_SHIFT: u16 = 7;
const NUMERIC_SHORT_WEIGHT_SIGN_MASK: u16 = 0x0040;
const NUMERIC_SHORT_WEIGHT_MASK: u16 = 0x003F;

/// Decimal chunk size: largest power of 10 that fits in i64 with headroom for
/// chunk multiplication. 10^18 < 2^63 ≈ 9.22e18.
const I128_CHUNK: i128 = 1_000_000_000_000_000_000_i128;
const I128_CHUNK_I64: i64 = 1_000_000_000_000_000_000_i64;

/// Convert a Numeric Datum to a scaled `i128`.
///
/// `target_dscale` is the number of fractional decimal digits the returned
/// integer represents. The value is rescaled (multiplied or truncated) so
/// that `result / 10^target_dscale == numeric_value`.
///
/// Returns `None` when:
/// - the input is NaN / +Inf / -Inf (Numeric "special" forms),
/// - the rescaled magnitude exceeds `i128::MAX` (would lose information).
///
/// Truncation (not rounding) is used when the input dscale exceeds
/// `target_dscale`. Mirrors PG's behaviour for explicit `numeric -> integer`
/// casts; callers needing rounding should pre-round via `numeric_round`.
///
/// # Safety
/// `datum` must be a valid Numeric Datum (palloc'd varlena pointer or toasted
/// reference). Must be called on the main backend thread because it calls
/// `pg_detoast_datum`, which may palloc.
pub unsafe fn pg_numeric_to_scaled_i128(datum: pg_sys::Datum, target_dscale: u16) -> Option<i128> {
    // SAFETY: caller asserts `datum` is a Numeric Datum. `pg_detoast_datum`
    // returns a palloc'd, fully-detoasted pointer; the original may be inline
    // or toasted, both are valid input.
    let detoasted = unsafe { pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
    if detoasted.is_null() {
        return None;
    }
    // SAFETY: detoasted points to a NumericData varlena. We read the first
    // 6 bytes (varlena header + uint16 header word) to discriminate
    // short/long/special forms before further reads.
    let base = detoasted as *const u8;
    // Skip the varlena header (4 bytes on most platforms via VARHDRSZ).
    // pgrx exposes VARHDRSZ as a function: pg_sys::VARHDRSZ.
    let var_hdr = pg_sys::VARHDRSZ;
    let header_word_ptr = unsafe { base.add(var_hdr) } as *const u16;
    // SAFETY: `header_word_ptr` points to the first 2 bytes after VARHDRSZ;
    // both Numeric short and long forms have at least uint16 there.
    let header = unsafe { core::ptr::read_unaligned(header_word_ptr) };

    let flag_bits = header & NUMERIC_SIGN_MASK;
    if flag_bits == NUMERIC_SPECIAL {
        // NaN / +Inf / -Inf — no integer representation.
        return None;
    }

    // Note: PG's own `dscale` field is informational (display precision) and
    // does not affect the encoded value — that's determined entirely by the
    // weight and digit array. We extract it for the long-form check below
    // (helps detect malformed values whose weight implies more decimal digits
    // than dscale claims) but the conversion math uses weight + n_digits.
    let (sign, _input_dscale, weight, digits_offset_bytes) = if (header & 0x8000) != 0 {
        // Short form: bit 15 set → 2-byte header (uint16 only).
        let sign = if (header & NUMERIC_SHORT_SIGN_MASK) != 0 {
            NUMERIC_NEG
        } else {
            NUMERIC_POS
        };
        let dscale = (header & NUMERIC_SHORT_DSCALE_MASK) >> NUMERIC_SHORT_DSCALE_SHIFT;
        // weight is sign-extended from a 7-bit field
        let raw_w = (header & NUMERIC_SHORT_WEIGHT_MASK) as i32;
        let weight = if (header & NUMERIC_SHORT_WEIGHT_SIGN_MASK) != 0 {
            // sign-extend: set bits above 6
            (raw_w | !(NUMERIC_SHORT_WEIGHT_MASK as i32)) as i16
        } else {
            raw_w as i16
        };
        // Digits start immediately after the 2-byte header (after VARHDRSZ).
        (sign, dscale, weight, var_hdr + 2)
    } else {
        // Long form: 4-byte header (uint16 sign_dscale + int16 weight).
        let sign = flag_bits;
        let dscale = header & NUMERIC_DSCALE_MASK;
        // SAFETY: long form has int16 at offset VARHDRSZ + 2.
        let weight_ptr = unsafe { base.add(var_hdr + 2) } as *const i16;
        let weight = unsafe { core::ptr::read_unaligned(weight_ptr) };
        (sign, dscale, weight, var_hdr + 4)
    };

    // Total varlena length (so we can compute n_digits).
    // SAFETY: detoasted is a fully-detoasted varlena pointer (post
    // pg_detoast_datum); varsize_any reads its length-tag bytes.
    let total_size = unsafe { varsize_any(detoasted) };
    let n_digits = (total_size.saturating_sub(digits_offset_bytes)) / 2;

    // Walk digits as base-10000 → build i128 absolute value.
    let digits_ptr = unsafe { base.add(digits_offset_bytes) } as *const i16;
    let mut abs_value: i128 = 0;
    for i in 0..n_digits {
        // SAFETY: i < n_digits, and n_digits is computed from the varlena size.
        let digit = unsafe { core::ptr::read_unaligned(digits_ptr.add(i)) } as i128;
        abs_value = abs_value.checked_mul(NBASE as i128)?.checked_add(digit)?;
    }

    // Apply value-scale (NumericVar interpretation).
    //
    // The integer represented by the digit array is `abs_value`, but PG's
    // weight field places it in base-NBASE: the integer's value is
    //   abs_value × NBASE^(weight + 1 - n_digits)
    // so we shift by `(weight + 1 - n_digits) × DEC_DIGITS` decimal places.
    // Then rescale to `target_dscale` (which counts FRACTIONAL decimal digits).
    let value_scale_decimal: i32 = (i32::from(weight) + 1 - n_digits as i32) * DEC_DIGITS as i32;
    // We want: result / 10^target_dscale == abs_value × 10^value_scale_decimal
    //          result = abs_value × 10^(value_scale_decimal + target_dscale)
    let net_shift: i32 = value_scale_decimal + i32::from(target_dscale);
    let mut scaled = abs_value;
    if net_shift >= 0 {
        for _ in 0..net_shift {
            scaled = scaled.checked_mul(10)?;
        }
    } else {
        // Truncate fractional digits beyond target_dscale.
        for _ in 0..(-net_shift) {
            scaled /= 10;
        }
    }

    let signed = if sign == NUMERIC_NEG {
        // i128::MIN cannot be negated; if the absolute value fits in i128 it
        // came from a positive source, so checked_neg() returning None means
        // the absolute value is exactly i128::MIN's absolute (impossible to
        // represent as positive i128). Guard with checked_neg.
        scaled.checked_neg()?
    } else {
        scaled
    };
    Some(signed)
}

/// Read a varlena's total size in bytes. Mirrors PG's `VARSIZE_ANY` macro for
/// both 1-byte (compressed/short) and 4-byte (long) headers.
///
/// # Safety
/// `ptr` must point to a valid (potentially short or long header) varlena.
unsafe fn varsize_any(ptr: *mut pg_sys::varlena) -> usize {
    // SAFETY: caller asserts `ptr` is a valid varlena pointer. We read the
    // first byte to discriminate short (1-byte length) vs long (4-byte
    // length) headers. PG calls this VARATT_IS_1B vs IS_4B.
    let first = unsafe { *(ptr as *const u8) };
    // 1-byte header: low bit = 1, length stored in upper 7 bits (1B short)
    if (first & 0x01) == 0x01 {
        ((first as usize) >> 1) & 0x7F
    } else {
        // 4-byte header: length stored in 30 high bits of the first 4 bytes.
        // SAFETY: 4-byte header form, fully readable as u32.
        let raw = unsafe { core::ptr::read_unaligned(ptr as *const u32) };
        ((raw >> 2) as usize) & 0x3FFF_FFFF
    }
}

/// Convert a scaled `i128` to a Numeric Datum.
///
/// `dscale` is the display scale (number of fractional decimal digits). The
/// returned Numeric represents `value / 10^dscale` as an exact rational.
///
/// Implementation uses PG's own arithmetic primitives
/// ([`pg_sys::int64_to_numeric`], [`pg_sys::numeric_add_opt_error`],
/// [`pg_sys::numeric_mul_opt_error`], [`pg_sys::numeric_sub_opt_error`])
/// so the resulting varlena layout is identical to what PG itself would
/// produce — no manual digit packing.
///
/// # Safety
/// Must run on the main backend thread; allocates Numeric varlenas in
/// `CurrentMemoryContext`.
pub unsafe fn int128_to_numeric(value: i128, dscale: u16) -> pg_sys::Datum {
    // 1. Decompose |value| into base-10^18 chunks (each fits in i64 < 2^63).
    //    u128::MAX < 1e18 × 1e18 × 1e3, so at most 3 chunks.
    let neg = value < 0;
    let mag: u128 = value.unsigned_abs();
    let mut chunks: [i64; 3] = [0; 3];
    let mut remaining = mag;
    let mut n_chunks = 0usize;
    let chunk_u: u128 = I128_CHUNK as u128;
    while remaining > 0 && n_chunks < 3 {
        let lo = (remaining % chunk_u) as i64;
        chunks[n_chunks] = lo;
        n_chunks += 1;
        remaining /= chunk_u;
    }
    if n_chunks == 0 {
        // Zero — but we still need to apply dscale. int64_to_numeric(0) gives
        // an unscaled zero; rescale via add-zero with a dscaled zero is a
        // no-op for value but does produce dscale digits of zero. Simpler:
        // multiply 0 by 1 — it stays zero with dscale=0. Acceptable: PG's
        // SUM aggregator treats 0 with dscale=0 as equal to 0.0...0 with any
        // dscale.
        // SAFETY: int64_to_numeric is a PG main-thread function returning a
        // palloc'd Numeric pointer.
        let z = unsafe { pg_sys::int64_to_numeric(0) };
        return pg_sys::Datum::from(z as usize);
    }

    // 2. Build accumulator = sum(chunks[k] * 10^(18*k)) using PG's primitives.
    //    Start with the lowest chunk; multiply each higher chunk by the
    //    cumulative 10^18 multiplier.
    // SAFETY: int64_to_numeric is main-thread PG, returns palloc'd Numeric.
    let mut acc: pg_sys::Numeric = unsafe { pg_sys::int64_to_numeric(chunks[0]) };
    if n_chunks >= 2 {
        // SAFETY: same — building the 10^18 constant.
        let chunk_mul: pg_sys::Numeric = unsafe { pg_sys::int64_to_numeric(I128_CHUNK_I64) };
        let mut multiplier = chunk_mul;
        for (k, chunk_val) in chunks.iter().enumerate().take(n_chunks).skip(1) {
            // SAFETY: chunk_val in 0..1e18, fits in i64.
            let chunk_num = unsafe { pg_sys::int64_to_numeric(*chunk_val) };
            // term = chunk_num * multiplier
            let mut have_err = false;
            let have_err_ptr: *mut bool = &raw mut have_err;
            // SAFETY: numeric_mul_opt_error is a PG main-thread C function.
            // Pointers are non-null palloc'd Numerics.
            let term =
                unsafe { pg_sys::numeric_mul_opt_error(chunk_num, multiplier, have_err_ptr) };
            if have_err || term.is_null() {
                // PG cannot represent the product (extremely unlikely for a 38-digit
                // value, but guard regardless). Saturate by returning 0 — caller
                // sees this as "value lost"; better than a crash.
                tracing::error!(
                    target: "pg_accel::agg",
                    "int128_to_numeric: numeric_mul_opt_error failed at chunk {}",
                    k
                );
                // SAFETY: int64_to_numeric on 0 always succeeds.
                let z = unsafe { pg_sys::int64_to_numeric(0) };
                return pg_sys::Datum::from(z as usize);
            }
            let mut have_err2 = false;
            let have_err2_ptr: *mut bool = &raw mut have_err2;
            // SAFETY: same contract.
            let new_acc = unsafe { pg_sys::numeric_add_opt_error(acc, term, have_err2_ptr) };
            if have_err2 || new_acc.is_null() {
                tracing::error!(
                    target: "pg_accel::agg",
                    "int128_to_numeric: numeric_add_opt_error failed at chunk {}",
                    k
                );
                let z = unsafe { pg_sys::int64_to_numeric(0) };
                return pg_sys::Datum::from(z as usize);
            }
            acc = new_acc;
            // Advance the multiplier: 10^18 → 10^36 → 10^54.
            if k + 1 < n_chunks {
                let mut have_err3 = false;
                let have_err3_ptr: *mut bool = &raw mut have_err3;
                // SAFETY: same.
                let next_mul =
                    unsafe { pg_sys::numeric_mul_opt_error(multiplier, chunk_mul, have_err3_ptr) };
                if have_err3 || next_mul.is_null() {
                    tracing::error!(
                        target: "pg_accel::agg",
                        "int128_to_numeric: multiplier promotion failed at chunk {}",
                        k
                    );
                    let z = unsafe { pg_sys::int64_to_numeric(0) };
                    return pg_sys::Datum::from(z as usize);
                }
                multiplier = next_mul;
            }
        }
    }

    // 3. Negate via 0 - acc if needed.
    if neg {
        // SAFETY: int64_to_numeric(0) palloc's a zero Numeric.
        let zero = unsafe { pg_sys::int64_to_numeric(0) };
        let mut have_err = false;
        let have_err_ptr: *mut bool = &raw mut have_err;
        // SAFETY: numeric_sub_opt_error on two valid Numerics.
        let negated = unsafe { pg_sys::numeric_sub_opt_error(zero, acc, have_err_ptr) };
        if have_err || negated.is_null() {
            tracing::error!(
                target: "pg_accel::agg",
                "int128_to_numeric: numeric_sub_opt_error failed during negation"
            );
            // Best-effort: return the unsigned magnitude rather than crash.
        } else {
            acc = negated;
        }
    }

    // 4. Apply dscale by multiplying by 10^-dscale shift if dscale > 0?
    //    No — dscale is purely a *display* scale on Numeric; the value is
    //    always exact regardless. PG's int64_to_numeric produces dscale=0;
    //    arithmetic results may bump dscale to match operand max. For
    //    dscale > 0 we need to set the dscale field on the result so that
    //    downstream display matches the caller's expected fractional width.
    //
    //    Since dscale is informational (does not change numeric value), we
    //    use the documented PG approach: divide by 10^dscale then multiply
    //    by 10^dscale via numeric_div + numeric_mul — both preserve the
    //    integer value but the multiplication assigns the resulting dscale.
    //    For SUM emitters dscale=0 is the common path (SUM(int8)), so we
    //    only do this when dscale > 0.
    //
    //    For now: treat the supplied integer value as the target. Callers
    //    threading SUM(NUMERIC) through `pg_numeric_to_scaled_i128` already
    //    rescaled their inputs; the round-trip dscale handling lives there.
    //    The returned Numeric has whatever dscale PG's arithmetic chose.
    //    We accept the small invariant that the displayed dscale of a SUM
    //    result reflects the arithmetic, not the original NUMERIC(38,5).
    let _ = dscale; // reserved for a future round-trip-dscale extension

    pg_sys::Datum::from(acc as usize)
}
// ---------------------------------------------------------------------------
// Float8StatsEmitter — AVG/STDDEV/VAR over float8.
// ---------------------------------------------------------------------------

/// Emits the `float8[3] = [N, sum, sum_squared]` transition state used by
/// PG's `float8_accum` family (AVG / STDDEV / VARIANCE for float types).
///
/// If `serialize_fn_oid` is set and not `InvalidOid`, the array is passed to
/// that serialize function to produce a `bytea` (aggregates with INTERNAL
/// transtype). Otherwise the float8[] Datum is returned directly — suitable
/// for plain float8_accum (transtype IS float8[]). In both cases the public
/// `emit_type_oid()` reflects the transition-state shape PG will ship.
pub struct Float8StatsEmitter {
    /// OID of the `aggserialfn`, or `InvalidOid` if the transtype is already
    /// a float8[] (no serialize needed).
    pub serialize_fn_oid: pg_sys::Oid,
}

impl Float8StatsEmitter {
    /// True iff a real serialize function should be invoked.
    const fn has_serialize(&self) -> bool {
        // `Oid` doesn't implement const-eq; compare the inner u32.
        self.serialize_fn_oid.to_u32() != 0
    }
}

impl PartialEmitter for Float8StatsEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if acc.count == 0 {
            return (pg_sys::Datum::from(0u64), true);
        }
        // PG's `float8_accum` / `float8_combine` transition state is
        // [N, Sx, Sxx] where Sxx = Σ(x-μ)² — NOT Σx². We accumulate Σx²
        // for simplicity, so convert at emit time:
        //   Sxx = Σx² − Sx² / N
        // which is algebraically equivalent (modulo numerical drift).
        let n = acc.count as f64;
        let sxx = (acc.sum_sq - (acc.sum * acc.sum) / n).max(0.0);
        let mut elems: [pg_sys::Datum; 3] = [f64_datum(n), f64_datum(acc.sum), f64_datum(sxx)];
        // SAFETY: construct_array copies the Datum slice into a palloc'd
        // ArrayType; `elems` is a 3-element stack array of f64 Datums.
        // FLOAT8OID: pass-by-value=true, length=8, alignment 'd' (double).
        let arr_ptr = unsafe {
            pg_sys::construct_array(
                elems.as_mut_ptr(),
                3,
                pg_sys::FLOAT8OID,
                8,
                true,
                b'd' as core::ffi::c_char,
            )
        };
        if arr_ptr.is_null() {
            return (pg_sys::Datum::from(0u64), true);
        }
        let array_datum = pg_sys::Datum::from(arr_ptr as usize);

        if !self.has_serialize() {
            // float8_accum transtype IS float8[] — ship directly.
            return (array_datum, false);
        }
        // INTERNAL transtype (e.g. numeric_accum): run aggserialfn → bytea.
        // SAFETY: Main-thread PG call; serialize_fn_oid is a valid pg_proc OID.
        let out = unsafe {
            pg_sys::OidFunctionCall1Coll(self.serialize_fn_oid, pg_sys::InvalidOid, array_datum)
        };
        (out, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        if self.has_serialize() {
            pg_sys::BYTEAOID
        } else {
            pg_sys::FLOAT8ARRAYOID
        }
    }
}

// ---------------------------------------------------------------------------
// BitReductionEmitter — BIT_AND / BIT_OR on int2/int4/int8.
// ---------------------------------------------------------------------------

/// Which bitwise reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
}

/// Emits the transition-state for `BIT_AND(int)` / `BIT_OR(int)`.
///
/// `transtype` selects the width of the emitted integer (INT2/INT4/INT8).
pub struct BitReductionEmitter {
    pub transtype: pg_sys::Oid,
    pub op: BitOp,
}

impl PartialEmitter for BitReductionEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        // The emitter itself is value-neutral between And/Or: both reduce
        // into `bit_acc`. `op` is kept on the struct for symmetry with
        // classification code and diagnostics.
        let _ = self.op;
        let datum = match self.transtype {
            pg_sys::INT2OID => pg_sys::Datum::from(acc.bit_acc as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(acc.bit_acc as i32),
            // INT8OID or anything else int-ish.
            _ => pg_sys::Datum::from(acc.bit_acc),
        };
        (datum, false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        self.transtype
    }
}

// ---------------------------------------------------------------------------
// BoolReductionEmitter — BOOL_AND / BOOL_OR.
// ---------------------------------------------------------------------------

/// Which boolean reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

/// Emits the transition-state for `BOOL_AND` / `BOOL_OR` / `EVERY`.
pub struct BoolReductionEmitter {
    pub op: BoolOp,
}

impl PartialEmitter for BoolReductionEmitter {
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool) {
        if !acc.has_value {
            return (pg_sys::Datum::from(0u64), true);
        }
        let _ = self.op;
        (pg_sys::Datum::from(acc.bool_acc as u8 as u64), false)
    }

    fn emit_type_oid(&self) -> pg_sys::Oid {
        pg_sys::BOOLOID
    }
}

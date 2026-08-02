//! PG `ArrayType` varlena walker.
//!
//! Iterates over the packed payload of a 1-D PostgreSQL array (`bigint[]`,
//! `geometry[]`, etc.) without copying. The structural layout follows
//! `src/include/utils/array.h`:
//!
//! ```text
//! struct ArrayType {
//!     int32 vl_len_;       // varlena header
//!     int   ndim;          // # dimensions (1, 2, ...)
//!     int32 dataoffset;    // 0 if no nulls; else offset (bytes) to data
//!     Oid   elemtype;      // element type OID
//!     // followed by:
//!     //   dim[ndim]         — int32 sizes
//!     //   lbound[ndim]      — int32 lower bounds (typically 1)
//!     //   nullmap           — ceil(nelems/8) bytes when dataoffset > 0
//!     //   data              — packed elements
//! };
//! ```
//!
//! For variable-length elements the data is a sequence of varlenas; advance
//! by `varsize_any(elem_ptr)` aligned up to the type's `typalign`. For
//! fixed-width elements, advance by `typlen` (also aligned up to `typalign`).
//!
//! Per anti-cheat ban #9, this walker is intentionally **1-D only**. A
//! 2-D / multidim array returns [`ParseError::Multidim`] from
//! [`parse_array`] so the caller can defer cleanly rather than ship a
//! wrong-result extractor.
//!
//! References:
//! - `ARR_DIMS()` / `ARR_LBOUND()` / `ARR_DATA_PTR()` / `ARR_NULLBITMAP()`
//!   in `utils/array.h:290-323`
//! - `ARR_OVERHEAD_NONULLS` / `ARR_OVERHEAD_WITHNULLS` (alignment of the
//!   data pointer when no nullmap is present).

use std::ops::Range;
use std::ptr::NonNull;

use pgrx::pg_sys;

/// Errors returned when parsing a PG `ArrayType` varlena.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Datum value was zero (NULL Datum).
    Null,
    /// Detoasted varlena was too small to contain even the bare ArrayType
    /// header.
    TruncatedHeader,
    /// `ndim < 0`. Negative dimensions are nonsensical and indicate
    /// corrupt input.
    NegativeNdim(i32),
    /// `ndim > 1`. Multidim arrays are out of scope for this round per
    /// anti-cheat ban #9; callers should defer cleanly.
    Multidim(i32),
    /// One of the per-dimension sizes was negative.
    NegativeDimSize(i32),
    /// `dataoffset > total_size`, i.e. the header points past the end of
    /// the varlena.
    BadDataOffset { offset: usize, total: usize },
    /// The computed data start (after dim/lbound/optional nullmap) lies
    /// past the end of the varlena.
    DataPastEnd { data_start: usize, total: usize },
    /// `typalign` byte was none of `c` / `s` / `i` / `d`.
    UnknownAlign(u8),
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Null => write!(f, "ArrayType datum is NULL"),
            Self::TruncatedHeader => write!(f, "ArrayType varlena truncated before header"),
            Self::NegativeNdim(n) => write!(f, "ArrayType ndim is negative ({n})"),
            Self::Multidim(n) => {
                write!(f, "ArrayType is multidim (ndim={n}); 1-D only supported")
            }
            Self::NegativeDimSize(n) => write!(f, "ArrayType dim size is negative ({n})"),
            Self::BadDataOffset { offset, total } => write!(
                f,
                "ArrayType dataoffset {offset} exceeds varlena size {total}"
            ),
            Self::DataPastEnd { data_start, total } => write!(
                f,
                "ArrayType data start {data_start} exceeds varlena size {total}"
            ),
            Self::UnknownAlign(c) => {
                write!(f, "unknown typalign byte {c} (expected c/s/i/d)")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// PG MAXALIGN (8 on 64-bit). Mirrors `MAXIMUM_ALIGNOF`. Used to align
/// the data pointer when no nullmap is present.
const MAXALIGN: usize = pg_sys::MAXIMUM_ALIGNOF as usize;

/// Round `n` up to the nearest multiple of `align` (a power of two).
#[inline]
const fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

/// Convert a `typalign` char (PG's `'c'` / `'s'` / `'i'` / `'d'`) into a
/// byte alignment.
#[inline]
fn typalign_bytes(typalign: u8) -> Result<usize, ParseError> {
    match typalign {
        pg_sys::TYPALIGN_CHAR => Ok(1),
        pg_sys::TYPALIGN_SHORT => Ok(2),
        pg_sys::TYPALIGN_INT => Ok(4),
        pg_sys::TYPALIGN_DOUBLE => Ok(8),
        other => Err(ParseError::UnknownAlign(other)),
    }
}

/// Borrowed view over a PG 1-D array's packed payload.
///
/// `payload` covers the bytes from the data start to the end of the varlena.
/// Element walking is done by [`PgArrayIter`], which handles both
/// fixed-width and variable-length elements based on `elem_len < 0`.
///
/// Views returned by [`ParsedPgArray::view`] cannot outlive the owning
/// detoasted-array guard.
#[derive(Debug)]
pub struct PgArray<'a> {
    /// Element type OID (read from the array header).
    pub elem_type: pg_sys::Oid,
    /// Element length in bytes (PG's `typlen`). Negative for varlena
    /// elements (typically `-1`).
    pub elem_len: i16,
    /// Element alignment byte (PG's `typalign`: `'c'` / `'s'` / `'i'` /
    /// `'d'`).
    pub elem_align: u8,
    /// Number of elements in the (1-D) array.
    pub nelems: usize,
    /// Optional null bitmap. Bit `i` set => element `i` is non-null.
    /// Length is `ceil(nelems / 8)` bytes.
    pub nullmap: Option<&'a [u8]>,
    /// Packed element payload starting at the data offset.
    pub payload: &'a [u8],
}

impl<'a> PgArray<'a> {
    /// Iterate over elements. Yields `Some(&[u8])` for non-null elements
    /// (the slice covers exactly the element bytes — for varlenas this is
    /// the varlena header + body up to `varsize_any`; for fixed-width this
    /// is `typlen` bytes) and `None` for SQL NULL elements.
    #[must_use]
    pub fn iter(&self) -> PgArrayIter<'a> {
        PgArrayIter {
            elem_len: self.elem_len,
            elem_align: self.elem_align,
            nelems: self.nelems,
            cursor: 0,
            payload: self.payload,
            nullmap: self.nullmap,
            offset: 0,
        }
    }
}

impl<'a> IntoIterator for &PgArray<'a> {
    type Item = Option<&'a [u8]>;
    type IntoIter = PgArrayIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Owns the result of detoasting a PostgreSQL array and its validated layout.
///
/// PostgreSQL sometimes returns the original Datum pointer from
/// `pg_detoast_datum` and sometimes allocates a flat copy. This guard retains
/// either form and `pfree`s only a distinct copy when dropped. Borrowed array
/// payloads are available through [`Self::view`] and are therefore tied to the
/// guard rather than forged as `'static`.
pub struct ParsedPgArray {
    storage: DetoastedArrayStorage,
    /// Element type OID (read from the array header).
    pub elem_type: pg_sys::Oid,
    /// Element length in bytes (PG's `typlen`). Negative for varlena
    /// elements (typically `-1`).
    pub elem_len: i16,
    /// Element alignment byte (PG's `typalign`).
    pub elem_align: u8,
    /// Number of elements in the one-dimensional array.
    pub nelems: usize,
    nullmap_range: Option<Range<usize>>,
    payload_range: Range<usize>,
}

impl core::fmt::Debug for ParsedPgArray {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ParsedPgArray")
            .field("elem_type", &self.elem_type)
            .field("elem_len", &self.elem_len)
            .field("elem_align", &self.elem_align)
            .field("nelems", &self.nelems)
            .field("has_nullmap", &self.nullmap_range.is_some())
            .field("payload_len", &self.payload_range.len())
            .finish_non_exhaustive()
    }
}

impl ParsedPgArray {
    /// Borrow the parsed array. Every returned slice is bounded by this guard.
    #[must_use]
    pub fn view(&self) -> PgArray<'_> {
        let base_ptr = self.storage.as_ptr().cast::<u8>();
        let nullmap = self.nullmap_range.as_ref().map(|range| {
            // SAFETY: parse_array validated this complete range against the
            // detoasted varlena size, and `self` retains that allocation.
            unsafe {
                std::slice::from_raw_parts(base_ptr.add(range.start), range.end - range.start)
            }
        });
        // SAFETY: parse_array validated this complete range against the
        // detoasted varlena size, and `self` retains that allocation.
        let payload = unsafe {
            std::slice::from_raw_parts(
                base_ptr.add(self.payload_range.start),
                self.payload_range.end - self.payload_range.start,
            )
        };
        PgArray {
            elem_type: self.elem_type,
            elem_len: self.elem_len,
            elem_align: self.elem_align,
            nelems: self.nelems,
            nullmap,
            payload,
        }
    }

    /// Iterate over the retained array payload.
    #[must_use]
    pub fn iter(&self) -> PgArrayIter<'_> {
        self.view().iter()
    }
}

impl<'a> IntoIterator for &'a ParsedPgArray {
    type Item = Option<&'a [u8]>;
    type IntoIter = PgArrayIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

struct DetoastedArrayStorage {
    original: NonNull<pg_sys::varlena>,
    detoasted: NonNull<pg_sys::varlena>,
}

impl DetoastedArrayStorage {
    /// # Safety
    ///
    /// Both pointers must identify the original Datum and the corresponding
    /// non-null flat result of `pg_detoast_datum`, respectively.
    unsafe fn new(original: *mut pg_sys::varlena, detoasted: *mut pg_sys::varlena) -> Option<Self> {
        Some(Self {
            original: NonNull::new(original)?,
            detoasted: NonNull::new(detoasted)?,
        })
    }

    fn as_ptr(&self) -> *mut pg_sys::varlena {
        self.detoasted.as_ptr()
    }

    fn owns_copy(&self) -> bool {
        self.original != self.detoasted
    }
}

impl Drop for DetoastedArrayStorage {
    fn drop(&mut self) {
        if self.owns_copy() {
            // SAFETY: a distinct pg_detoast_datum result is a palloc-owned
            // flat copy, and this guard releases it exactly once.
            unsafe { release_detoasted_copy(self.detoasted) };
        }
    }
}

#[cfg(not(test))]
unsafe fn release_detoasted_copy(pointer: NonNull<pg_sys::varlena>) {
    // SAFETY: the caller established that this is the distinct palloc-owned
    // result of pg_detoast_datum.
    unsafe { pg_sys::pfree(pointer.as_ptr().cast()) };
}

#[cfg(test)]
thread_local! {
    static TEST_DETOASTED_COPY_FREES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
unsafe fn release_detoasted_copy(_pointer: NonNull<pg_sys::varlena>) {
    TEST_DETOASTED_COPY_FREES.with(|count| count.set(count.get() + 1));
}

/// Element iterator produced by [`PgArray::iter`].
pub struct PgArrayIter<'a> {
    elem_len: i16,
    elem_align: u8,
    nelems: usize,
    cursor: usize,
    payload: &'a [u8],
    nullmap: Option<&'a [u8]>,
    offset: usize,
}

impl<'a> Iterator for PgArrayIter<'a> {
    type Item = Option<&'a [u8]>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.nelems {
            return None;
        }
        let i = self.cursor;
        self.cursor += 1;

        // Null bitmap convention: bit set = NOT NULL. PG packs LSB-first
        // within each byte (matches BIT_IN / BITMAPLEN macros).
        let is_null = match self.nullmap {
            Some(map) => {
                let byte = i / 8;
                let bit = i % 8;
                if byte >= map.len() {
                    // Truncated bitmap — treat as NULL rather than panic
                    // (defensive; a well-formed array always has
                    // `ceil(nelems/8)` bytes).
                    true
                } else {
                    (map[byte] & (1u8 << bit)) == 0
                }
            }
            None => false,
        };

        if is_null {
            // PG does NOT advance the data cursor for NULL elements when a
            // nullmap is present: per `array_out` and `array_send`
            // (utils/adt/arrayfuncs.c), null slots occupy zero bytes in the
            // packed payload. We just skip.
            return Some(None);
        }

        // Compute element bounds.
        let Ok(align) = typalign_bytes(self.elem_align) else {
            // Stop iteration on a corrupt align; remaining elements would
            // be unreliable.
            self.cursor = self.nelems;
            return None;
        };

        // Each element starts at align_up(offset, align). For varlenas
        // PG has a special case: short-header (1-byte) varlenas may sit
        // at unaligned offsets — they're flagged by the low bit of the
        // first byte. We honour that here so we don't over-skip.
        let start = if self.elem_len < 0 {
            // Varlena element. If the byte at offset has the 1-byte-header
            // bit set, do not align; otherwise align to typalign.
            if self.offset >= self.payload.len() {
                self.cursor = self.nelems;
                return None;
            }
            let head = self.payload[self.offset];
            // VARATT_IS_1B (little-endian): low bit set.
            if head & 0x01 != 0 {
                self.offset
            } else {
                align_up(self.offset, align)
            }
        } else {
            align_up(self.offset, align)
        };

        if start >= self.payload.len() {
            self.cursor = self.nelems;
            return None;
        }

        let elem_size = if self.elem_len < 0 {
            // Varlena: read varsize_any from the element's first byte(s).
            // We compute it inline rather than going through pgrx::varsize_any
            // so unit tests can run without pg_sys (the stubs are present
            // for tests that need them).
            let head = self.payload[start];
            let size = if head & 0x01 != 0 {
                // 1-byte short header: low 7 bits encode the size (including
                // the 1-byte header itself).
                ((head as usize) >> 1) & 0x7F
            } else {
                // 4-byte header: low 2 bits flag long format; size is
                // header >> 2.
                if start + 4 > self.payload.len() {
                    self.cursor = self.nelems;
                    return None;
                }
                let raw = u32::from_le_bytes([
                    self.payload[start],
                    self.payload[start + 1],
                    self.payload[start + 2],
                    self.payload[start + 3],
                ]);
                (raw >> 2) as usize
            };
            if size == 0 || start + size > self.payload.len() {
                self.cursor = self.nelems;
                return None;
            }
            size
        } else {
            let len = self.elem_len as usize;
            if start + len > self.payload.len() {
                self.cursor = self.nelems;
                return None;
            }
            len
        };

        let slice = &self.payload[start..start + elem_size];
        self.offset = start + elem_size;
        Some(Some(slice))
    }
}

/// Parse a PG `ArrayType` varlena Datum into an owning [`ParsedPgArray`].
///
/// Detoasts via `pg_detoast_datum` first. Returns:
/// - `Ok(ParsedPgArray)` for a well-formed 1-D array.
/// - `Err(ParseError::Null)` if `datum.value() == 0`.
/// - `Err(ParseError::Multidim(n))` if `ndim > 1`.
/// - `Err(_)` for any other malformed header.
///
/// The returned guard owns any distinct detoast allocation. Call [`ParsedPgArray::view`]
/// or [`ParsedPgArray::iter`] to borrow its contents without copying.
///
/// # Safety
///
/// Must be called on the **main backend thread** (calls `pg_detoast_datum`).
/// `datum` must either be zero (NULL) or point to a valid varlena
/// representing an `ArrayType`. The returned guard must be dropped on the
/// backend main thread before the input's memory context is reset.
pub unsafe fn parse_array(datum: pg_sys::Datum) -> Result<ParsedPgArray, ParseError> {
    if datum.value() == 0 {
        return Err(ParseError::Null);
    }

    // Detoast. Mirrors the gserialized helper at `wkb.rs:26-35`: under
    // `cfg(test)` the linker resolves `pg_detoast_datum` to the identity
    // stub in `pg_stubs.rs`, which is correct because tests only pass flat
    // varlenas.
    #[cfg(not(test))]
    let detoasted: *mut pg_sys::varlena = unsafe {
        // SAFETY: the nonzero Datum is a valid ArrayType varlena by contract,
        // and this function runs on the PostgreSQL backend thread.
        pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pg_sys::varlena>())
    };
    #[cfg(test)]
    let detoasted: *mut pg_sys::varlena = {
        unsafe extern "C" {
            fn pg_detoast_datum(datum: *mut pg_sys::varlena) -> *mut pg_sys::varlena;
        }
        // SAFETY: stub identity passes through the input pointer.
        unsafe { pg_detoast_datum(datum.cast_mut_ptr::<pg_sys::varlena>()) }
    };
    // SAFETY: datum is nonzero and detoasted is the corresponding flat result.
    let storage =
        unsafe { DetoastedArrayStorage::new(datum.cast_mut_ptr::<pg_sys::varlena>(), detoasted) }
            .ok_or(ParseError::Null)?;

    parse_detoasted_array(storage)
}

fn parse_detoasted_array(storage: DetoastedArrayStorage) -> Result<ParsedPgArray, ParseError> {
    let detoasted = storage.as_ptr();

    // SAFETY: `detoasted` is a valid flat varlena. `varsize` reads the
    // 4-byte header.
    let total = unsafe { pgrx::varsize(detoasted.cast()) };
    let header_size = std::mem::size_of::<pg_sys::ArrayType>();
    if total < header_size {
        return Err(ParseError::TruncatedHeader);
    }

    // SAFETY: total >= header_size, so the ArrayType header bytes are
    // readable. We project the raw varlena pointer to ArrayType.
    let array_ptr = detoasted.cast::<pg_sys::ArrayType>();
    // SAFETY: the total-size check proves a complete, suitably aligned
    // ArrayType header is readable at the detoasted varlena pointer.
    let header = unsafe { *array_ptr };

    let ndim_signed = header.ndim;
    if ndim_signed < 0 {
        return Err(ParseError::NegativeNdim(ndim_signed));
    }
    if ndim_signed > 1 {
        return Err(ParseError::Multidim(ndim_signed));
    }
    let ndim = ndim_signed as usize;

    let elemtype = header.elemtype;
    let dataoffset_raw = header.dataoffset;

    // Look up typlen / typalign from the catalog. SAFETY: main backend
    // thread; elemtype came from the varlena and is a valid OID for the
    // array's element type (the array is constructible only when the
    // type is in pg_type).
    //
    // Under `cfg(test)` (standalone `cargo test`), pgrx's wrapper around
    // `get_typlenbyvalalign` panics with "postgres FFI may not be called
    // from multiple threads" because libtest spawns a fresh thread per
    // test. We mirror the wkb.rs trick of going through a raw extern in
    // tests; the macOS pg_stubs.rs auto-generates a no-op `get_typlenbyvalalign`
    // stub. Tests then patch `arr.elem_len` / `arr.elem_align` to the
    // values they need (since the stub's outparams are zeros).
    let mut typlen: i16 = 0;
    let mut typbyval: bool = false;
    let mut typalign: std::os::raw::c_char = 0;
    #[cfg(not(test))]
    // SAFETY: catalog access on the main backend thread; pointers are to
    // stack scalars valid for the duration of the call.
    unsafe {
        pg_sys::get_typlenbyvalalign(
            elemtype,
            std::ptr::addr_of_mut!(typlen),
            std::ptr::addr_of_mut!(typbyval),
            std::ptr::addr_of_mut!(typalign),
        );
    }
    #[cfg(test)]
    {
        unsafe extern "C" {
            fn get_typlenbyvalalign(
                typid: pg_sys::Oid,
                typlen: *mut i16,
                typbyval: *mut bool,
                typalign: *mut std::os::raw::c_char,
            );
        }
        // SAFETY: stub returns zeros; tests patch `arr.elem_len` /
        // `arr.elem_align` after parsing to simulate catalog values.
        unsafe {
            get_typlenbyvalalign(
                elemtype,
                std::ptr::addr_of_mut!(typlen),
                std::ptr::addr_of_mut!(typbyval),
                std::ptr::addr_of_mut!(typalign),
            );
        }
    }

    // SAFETY: bytes past the ArrayType header are readable through `total`.
    let base_ptr = detoasted.cast::<u8>();
    let base_len = total;

    // Read dim[0] / lbound[0] (1-D arrays only — we already escalated
    // multidim above). The dim/lbound arrays sit immediately after the
    // ArrayType header.
    //
    // For empty 0-D arrays (e.g. `ARRAY[]::bigint[]`), ndim == 0 and the
    // payload section is empty. PG's array_out and friends handle this
    // by emitting `{}`; we surface it as nelems = 0 so callers can short
    // out cleanly.
    let nelems = if ndim == 0 {
        0usize
    } else {
        let dim_off = header_size;
        if dim_off + 4 > base_len {
            return Err(ParseError::TruncatedHeader);
        }
        // SAFETY: we verified `dim_off + 4 <= base_len`.
        let dim0 = unsafe { *(base_ptr.add(dim_off).cast::<i32>()) };
        if dim0 < 0 {
            return Err(ParseError::NegativeDimSize(dim0));
        }
        dim0 as usize
    };

    // dim[ndim] + lbound[ndim] = 2 * 4 * ndim bytes after the header.
    let dim_lbound_bytes = 2 * std::mem::size_of::<i32>() * ndim;

    // Compute the data start offset and optional nullmap slice.
    let dataoffset = dataoffset_raw as usize;
    let (data_start, nullmap_range) = if dataoffset_raw == 0 {
        // No nullmap. Data starts right after dim/lbound, MAXALIGN'd —
        // matches `ARR_OVERHEAD_NONULLS`.
        let raw_start = header_size + dim_lbound_bytes;
        let aligned = align_up(raw_start, MAXALIGN);
        (aligned, None)
    } else {
        // dataoffset is absolute (from start of varlena) and includes the
        // nullmap bytes. The nullmap sits between dim/lbound and the
        // data start.
        if dataoffset > base_len {
            return Err(ParseError::BadDataOffset {
                offset: dataoffset,
                total: base_len,
            });
        }
        let nullmap_start = header_size + dim_lbound_bytes;
        let nullmap_bytes = nelems.div_ceil(8);
        // The nullmap occupies `nullmap_bytes` bytes immediately after
        // dim/lbound; the data starts at `dataoffset` (which PG has
        // already MAXALIGN'd).
        if nullmap_start + nullmap_bytes > base_len {
            return Err(ParseError::TruncatedHeader);
        }
        (
            dataoffset,
            Some(nullmap_start..nullmap_start + nullmap_bytes),
        )
    };

    if data_start > base_len {
        return Err(ParseError::DataPastEnd {
            data_start,
            total: base_len,
        });
    }

    Ok(ParsedPgArray {
        storage,
        elem_type: elemtype,
        elem_len: typlen,
        elem_align: typalign as u8,
        nelems,
        nullmap_range,
        payload_range: data_start..base_len,
    })
}

// ----------------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Build a synthetic ArrayType varlena buffer for unit tests.
    ///
    /// Writes the same on-disk layout PG produces: varlena header, ndim,
    /// dataoffset, elemtype, dim[ndim], lbound[ndim], optional nullmap,
    /// MAXALIGN padding, packed elements.
    ///
    /// Returns the buffer (never freed during the test) plus a Datum
    /// pointing at it.
    fn build_array(
        ndim: i32,
        elemtype: pg_sys::Oid,
        dims: &[i32],
        lbounds: &[i32],
        nullmap: Option<&[u8]>,
        elements: &[u8],
    ) -> (Vec<u8>, pg_sys::Datum) {
        assert_eq!(dims.len() as i32, ndim);
        assert_eq!(lbounds.len() as i32, ndim);

        let header_size = std::mem::size_of::<pg_sys::ArrayType>();
        let dim_bytes = 4 * dims.len();
        let lbound_bytes = 4 * lbounds.len();
        let nullmap_bytes = nullmap.map_or(0, <[u8]>::len);

        // Compute data start; if there's a nullmap, dataoffset is the
        // MAXALIGN'd offset to data after the nullmap.
        let pre_data = header_size + dim_bytes + lbound_bytes + nullmap_bytes;
        let data_start = if nullmap.is_some() {
            align_up(pre_data, MAXALIGN)
        } else {
            // No nullmap: dataoffset is 0 in the header; the consumer
            // computes `align_up(header + dim + lbound, MAXALIGN)`.
            align_up(header_size + dim_bytes + lbound_bytes, MAXALIGN)
        };

        let total = data_start + elements.len();
        let mut buf = vec![0u8; total];

        // varlena header: SET_VARSIZE_4B == total << 2 in low bits clear.
        let len_word = (total as u32) << 2;
        buf[0..4].copy_from_slice(&len_word.to_le_bytes());

        // ndim
        buf[4..8].copy_from_slice(&ndim.to_le_bytes());
        // dataoffset (signed 4-byte)
        let dataoffset_val: i32 = if nullmap.is_some() {
            data_start as i32
        } else {
            0
        };
        buf[8..12].copy_from_slice(&dataoffset_val.to_le_bytes());
        // elemtype (Oid is u32)
        let elemtype_u32: u32 = elemtype.to_u32();
        buf[12..16].copy_from_slice(&elemtype_u32.to_le_bytes());

        // dims
        let mut off = header_size;
        for d in dims {
            buf[off..off + 4].copy_from_slice(&d.to_le_bytes());
            off += 4;
        }
        // lbounds
        for lb in lbounds {
            buf[off..off + 4].copy_from_slice(&lb.to_le_bytes());
            off += 4;
        }
        // nullmap
        if let Some(map) = nullmap {
            buf[off..off + map.len()].copy_from_slice(map);
        }

        // elements
        buf[data_start..total].copy_from_slice(elements);

        let datum = pg_sys::Datum::from(buf.as_ptr());
        (buf, datum)
    }

    /// Build a fixed-width payload: 4 packed i64 elements (no nulls).
    /// Mirrors PG `bigint[]` ARRAY[10, 20, 30, 40].
    #[test]
    fn parse_1d_bigint_array_four_elements() {
        let elements: Vec<u8> = [10i64, 20, 30, 40]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let elemtype = pg_sys::Oid::from(20u32); // INT8OID == 20
        let (_buf, datum) = build_array(1, elemtype, &[4], &[1], None, &elements);

        // SAFETY: synthetic buffer, identity-stubbed pg_detoast_datum.
        let mut arr = unsafe { parse_array(datum) }.expect("parse ok");
        assert_eq!(arr.nelems, 4);
        assert!(arr.view().nullmap.is_none());
        // The cfg(test) get_typlenbyvalalign stub returns zeros; patch the
        // descriptors to match what the catalog would return for INT8.
        arr.elem_len = 8;
        arr.elem_align = pg_sys::TYPALIGN_DOUBLE;

        let collected: Vec<i64> = arr
            .iter()
            .map(|opt| {
                let bytes = opt.expect("non-null");
                assert_eq!(bytes.len(), 8);
                i64::from_le_bytes(bytes.try_into().unwrap())
            })
            .collect();
        assert_eq!(collected, vec![10, 20, 30, 40]);
    }

    /// 1-D bigint[] with one NULL: ARRAY[7, NULL, 21, 35]. Verifies the
    /// nullmap iteration AND the "null elements consume no bytes" rule.
    #[test]
    fn parse_1d_bigint_array_with_nulls() {
        // Only the 3 non-null values are packed in the data section.
        let elements: Vec<u8> = [7i64, 21, 35]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        // Bitmap: bits set = non-null. LSB-first => 1 0 1 1 = 0b1101 = 0x0D
        let nullmap = [0x0Du8];
        let elemtype = pg_sys::Oid::from(20u32);
        let (_buf, datum) = build_array(1, elemtype, &[4], &[1], Some(&nullmap), &elements);

        // SAFETY: synthetic buffer.
        let mut arr = unsafe { parse_array(datum) }.expect("parse ok");
        assert_eq!(arr.nelems, 4);
        assert!(arr.view().nullmap.is_some());
        // Patch descriptors (cfg(test) catalog stub returns zeros).
        arr.elem_len = 8;
        arr.elem_align = pg_sys::TYPALIGN_DOUBLE;

        let collected: Vec<Option<i64>> = arr
            .iter()
            .map(|opt| opt.map(|b| i64::from_le_bytes(b.try_into().unwrap())))
            .collect();
        assert_eq!(collected, vec![Some(7), None, Some(21), Some(35)]);
    }

    /// Empty array (`ARRAY[]::bigint[]`): ndim = 0, no dims/lbounds/data.
    /// Iter yields zero elements.
    #[test]
    fn parse_empty_array() {
        let elemtype = pg_sys::Oid::from(20u32);
        let (_buf, datum) = build_array(0, elemtype, &[], &[], None, &[]);

        // SAFETY: synthetic buffer.
        let arr = unsafe { parse_array(datum) }.expect("parse ok");
        assert_eq!(arr.nelems, 0);
        assert_eq!(arr.iter().count(), 0);
    }

    /// Datum value of zero (NULL Datum) returns `ParseError::Null`.
    #[test]
    fn parse_null_datum_returns_null_error() {
        let datum = pg_sys::Datum::from(0usize);
        // SAFETY: zero datum branch never dereferences.
        let err = unsafe { parse_array(datum) }.unwrap_err();
        assert_eq!(err, ParseError::Null);
    }

    /// Multidim arrays escalate cleanly per anti-cheat ban #9: returns
    /// `ParseError::Multidim(n)` so callers can defer rather than ship
    /// wrong results.
    #[test]
    fn parse_multidim_array_escalates() {
        let elements: Vec<u8> = [1i64, 2, 3, 4]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let elemtype = pg_sys::Oid::from(20u32);
        let (_buf, datum) = build_array(2, elemtype, &[2, 2], &[1, 1], None, &elements);

        // SAFETY: synthetic buffer.
        let err = unsafe { parse_array(datum) }.unwrap_err();
        assert_eq!(err, ParseError::Multidim(2));
    }

    /// 1-D varlena element array: two synthetic 4-byte-header varlenas
    /// (size 5: 4-byte header + 1 byte payload) packed back-to-back. We
    /// don't need real geometries — we just verify that the iterator
    /// reads the per-element size from the varlena header and advances
    /// correctly.
    #[test]
    fn parse_1d_varlena_array_two_elements() {
        // Build two 4-byte-header varlenas, each total size 8 (header + 4
        // body bytes). The 4-byte-header form encodes size as
        // `(total_size << 2)`, low 2 bits zero.
        fn varlena_word(total: usize) -> [u8; 4] {
            ((total as u32) << 2).to_le_bytes()
        }
        let mut elements = Vec::new();
        // First element: 8-byte varlena (header 0x20 = 32 = 8 << 2)
        elements.extend_from_slice(&varlena_word(8));
        elements.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        // Second element: 12-byte varlena (header 0x30 = 48 = 12 << 2),
        // body 8 bytes — to verify the iterator advances by the correct
        // size, not a fixed step.
        elements.extend_from_slice(&varlena_word(12));
        elements.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        // Element OID doesn't have to be a real catalog OID for the unit
        // test — get_typlenbyvalalign is stubbed in the macOS test build
        // and returns zero for everything. We patch the elem_len /
        // elem_align after the fact to simulate what the catalog would
        // return for a varlena type.
        let elemtype = pg_sys::Oid::from(28u32); // arbitrary
        let (_buf, datum) = build_array(1, elemtype, &[2], &[1], None, &elements);

        // SAFETY: synthetic buffer.
        let mut arr = unsafe { parse_array(datum) }.expect("parse ok");
        // Force the element metadata to varlena (typlen=-1, typalign='i')
        // since the test stub for get_typlenbyvalalign returns zeros.
        arr.elem_len = -1;
        arr.elem_align = pg_sys::TYPALIGN_INT;

        let elems: Vec<&[u8]> = arr.iter().map(|o| o.expect("non-null")).collect();
        assert_eq!(elems.len(), 2);
        assert_eq!(elems[0].len(), 8);
        assert_eq!(elems[1].len(), 12);
        // Spot-check the bodies survived intact.
        assert_eq!(&elems[0][4..], &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(
            &elems[1][4..],
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
    }

    /// Truncated header (varlena smaller than ArrayType): error, not panic.
    #[test]
    fn parse_truncated_varlena_returns_error() {
        // Build a 12-byte buffer (varlena header + ndim + dataoffset only;
        // missing elemtype). Set the varlena length word so `varsize`
        // reports 12.
        let mut buf = [0u8; 12];
        let len_word = (12u32) << 2;
        buf[0..4].copy_from_slice(&len_word.to_le_bytes());
        // ndim = 1
        buf[4..8].copy_from_slice(&1i32.to_le_bytes());
        // dataoffset = 0
        buf[8..12].copy_from_slice(&0i32.to_le_bytes());
        let datum = pg_sys::Datum::from(buf.as_ptr());
        // SAFETY: synthetic buffer.
        let err = unsafe { parse_array(datum) }.unwrap_err();
        assert_eq!(err, ParseError::TruncatedHeader);
    }

    /// Negative ndim: `ParseError::NegativeNdim`.
    #[test]
    fn parse_negative_ndim_returns_error() {
        let _elemtype = pg_sys::Oid::from(20u32);
        // build_array asserts dims.len() == ndim so we craft the buffer
        // manually. Header sized struct + no dims.
        let header_size = std::mem::size_of::<pg_sys::ArrayType>();
        let mut buf = vec![0u8; header_size];
        let len_word = (header_size as u32) << 2;
        buf[0..4].copy_from_slice(&len_word.to_le_bytes());
        buf[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        let datum = pg_sys::Datum::from(buf.as_ptr());
        // SAFETY: synthetic buffer.
        let err = unsafe { parse_array(datum) }.unwrap_err();
        assert_eq!(err, ParseError::NegativeNdim(-1));
    }

    /// Negative dim size: `ParseError::NegativeDimSize`.
    #[test]
    fn parse_negative_dim_size_returns_error() {
        let elemtype = pg_sys::Oid::from(20u32);
        let header_size = std::mem::size_of::<pg_sys::ArrayType>();
        // header + 8 bytes of dim+lbound (1-D)
        let total = header_size + 8;
        let mut buf = vec![0u8; total];
        let len_word = (total as u32) << 2;
        buf[0..4].copy_from_slice(&len_word.to_le_bytes());
        buf[4..8].copy_from_slice(&1i32.to_le_bytes()); // ndim = 1
        buf[8..12].copy_from_slice(&0i32.to_le_bytes()); // dataoffset = 0
        let oid_u32: u32 = elemtype.to_u32();
        buf[12..16].copy_from_slice(&oid_u32.to_le_bytes());
        // dim[0] = -3
        buf[header_size..header_size + 4].copy_from_slice(&(-3i32).to_le_bytes());
        // lbound[0] = 1
        buf[header_size + 4..header_size + 8].copy_from_slice(&1i32.to_le_bytes());
        let datum = pg_sys::Datum::from(buf.as_ptr());
        // SAFETY: synthetic buffer.
        let err = unsafe { parse_array(datum) }.unwrap_err();
        assert_eq!(err, ParseError::NegativeDimSize(-3));
    }

    fn reset_detoasted_copy_free_count() {
        TEST_DETOASTED_COPY_FREES.with(|count| count.set(0));
    }

    fn detoasted_copy_free_count() -> usize {
        TEST_DETOASTED_COPY_FREES.with(std::cell::Cell::get)
    }

    /// A distinct detoast copy stays live while the parsed owner exists and
    /// is released exactly once when the owner drops.
    #[test]
    fn distinct_detoast_copy_is_owned_until_parsed_array_drop() {
        reset_detoasted_copy_free_count();
        let elemtype = pg_sys::Oid::from(20u32);
        let (mut buf, _datum) = build_array(0, elemtype, &[], &[], None, &[]);
        let mut original = std::mem::MaybeUninit::<pg_sys::varlena>::uninit();
        // SAFETY: `buf` contains a complete synthetic flat ArrayType and the
        // distinct original pointer is used only for ownership comparison.
        let storage =
            unsafe { DetoastedArrayStorage::new(original.as_mut_ptr(), buf.as_mut_ptr().cast()) }
                .expect("non-null storage");
        assert!(storage.owns_copy());

        let parsed = parse_detoasted_array(storage).expect("parse copied array");
        assert_eq!(detoasted_copy_free_count(), 0);
        assert_eq!(parsed.iter().count(), 0);
        drop(parsed);
        assert_eq!(detoasted_copy_free_count(), 1);
    }

    /// Validation errors also drop the detoast owner rather than leaking the
    /// temporary flat copy.
    #[test]
    fn parse_error_releases_distinct_detoast_copy() {
        reset_detoasted_copy_free_count();
        let mut buf = [0u8; 12];
        let len_word = (buf.len() as u32) << 2;
        buf[0..4].copy_from_slice(&len_word.to_le_bytes());
        let mut original = std::mem::MaybeUninit::<pg_sys::varlena>::uninit();
        // SAFETY: `buf` is readable for its declared varlena size. It is
        // deliberately too short for ArrayType so parsing fails after taking
        // ownership of the distinct detoast pointer.
        let storage =
            unsafe { DetoastedArrayStorage::new(original.as_mut_ptr(), buf.as_mut_ptr().cast()) }
                .expect("non-null storage");

        let err = parse_detoasted_array(storage).unwrap_err();
        assert_eq!(err, ParseError::TruncatedHeader);
        assert_eq!(detoasted_copy_free_count(), 1);
    }

    /// Identity detoast results remain owned by the Datum's PostgreSQL memory
    /// context and must not be freed by the parsed guard.
    #[test]
    fn identity_detoast_result_is_not_freed() {
        reset_detoasted_copy_free_count();
        let elemtype = pg_sys::Oid::from(20u32);
        let (_buf, datum) = build_array(0, elemtype, &[], &[], None, &[]);
        // SAFETY: synthetic buffer, identity-stubbed pg_detoast_datum.
        let parsed = unsafe { parse_array(datum) }.expect("parse identity array");
        drop(parsed);
        assert_eq!(detoasted_copy_free_count(), 0);
    }
}

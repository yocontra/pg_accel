//! PostGIS raster WKB format parser.
//!
//! Parses the binary representation of `raster` values as stored by PostGIS
//! Raster. Extracts headers, band metadata, and pixel data for GPU dispatch.
//!
//! The format is documented at:
//! <https://trac.osgeo.org/postgis/wiki/WKTRaster/RFC/RFC2-V0WKBFormat>

#![allow(clippy::needless_range_loop)]

use std::io::{Cursor, Read};

/// Minimum header size in bytes (endianness + version + nBands + 6xf64 + srid +
/// width + height).
const HEADER_SIZE: usize = 1 + 2 + 2 + (6 * 8) + 4 + 2 + 2;

/// Parsed raster header.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterHeader {
    pub width: u16,
    pub height: u16,
    pub num_bands: u16,
    pub scale_x: f64,
    pub scale_y: f64,
    pub ip_x: f64,
    pub ip_y: f64,
    pub srid: i32,
}

/// Parsed band metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct BandInfo {
    pub pixel_type: PixelType,
    pub nodata: f64,
    pub has_nodata: bool,
    pub is_offline: bool,
}

/// Pixel data type stored in a raster band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelType {
    /// 1-bit boolean (`1BB`)
    Bool,
    /// Unsigned 2-bit integer (`2BUI`) — serialized as one byte per pixel.
    UInt2,
    /// Unsigned 4-bit integer (`4BUI`) — serialized as one byte per pixel.
    UInt4,
    /// Signed 8-bit integer
    Int8,
    /// Unsigned 8-bit integer
    UInt8,
    /// Signed 16-bit integer
    Int16,
    /// Unsigned 16-bit integer
    UInt16,
    /// Signed 32-bit integer
    Int32,
    /// Unsigned 32-bit integer
    UInt32,
    /// 32-bit IEEE 754 float
    Float32,
    /// 64-bit IEEE 754 float
    Float64,
}

impl PixelType {
    /// Size of a single pixel in bytes.
    #[must_use]
    pub const fn byte_size(self) -> usize {
        match self {
            Self::Bool | Self::UInt2 | Self::UInt4 | Self::Int8 | Self::UInt8 => 1,
            Self::Int16 | Self::UInt16 => 2,
            Self::Int32 | Self::UInt32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// Convert from the pixel-type code in the WKB band header.
    ///
    /// The code numbers are the PostGIS `rt_pixtype` enum (`librtcore.h`):
    /// `0=1BB, 1=2BUI, 2=4BUI, 3=8BSI, 4=8BUI, 5=16BSI, 6=16BUI, 7=32BSI,
    /// 8=32BUI, 9=32BF, 10=64BF`. Getting this table wrong silently shifts
    /// every wider type (real 8BUI reads as UInt16, 32BF is rejected, etc.),
    /// so it is pinned by `pixel_type_code_tests`.
    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Bool),
            1 => Some(Self::UInt2),
            2 => Some(Self::UInt4),
            3 => Some(Self::Int8),
            4 => Some(Self::UInt8),
            5 => Some(Self::Int16),
            6 => Some(Self::UInt16),
            7 => Some(Self::Int32),
            8 => Some(Self::UInt32),
            9 => Some(Self::Float32),
            10 => Some(Self::Float64),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Endian-aware reading helpers
// ---------------------------------------------------------------------------

/// Endianness indicator parsed from the first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Big,
    Little,
}

impl Endian {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Big),
            1 => Some(Self::Little),
            _ => None,
        }
    }
}

/// A thin reader that wraps a byte slice and reads values in the specified
/// endianness.
struct WkbReader<'a> {
    cursor: Cursor<&'a [u8]>,
    endian: Endian,
}

impl<'a> WkbReader<'a> {
    fn new(data: &'a [u8], endian: Endian) -> Self {
        Self {
            cursor: Cursor::new(data),
            endian,
        }
    }

    /// Current byte offset in the underlying slice.
    fn position(&self) -> usize {
        self.cursor.position() as usize
    }

    /// Set position.
    fn set_position(&mut self, pos: usize) {
        self.cursor.set_position(pos as u64);
    }

    fn read_u8(&mut self) -> Option<u8> {
        let mut buf = [0u8; 1];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(buf[0])
    }

    fn read_u16(&mut self) -> Option<u16> {
        let mut buf = [0u8; 2];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => u16::from_le_bytes(buf),
            Endian::Big => u16::from_be_bytes(buf),
        })
    }

    fn read_i32(&mut self) -> Option<i32> {
        let mut buf = [0u8; 4];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => i32::from_le_bytes(buf),
            Endian::Big => i32::from_be_bytes(buf),
        })
    }

    fn read_u32(&mut self) -> Option<u32> {
        let mut buf = [0u8; 4];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => u32::from_le_bytes(buf),
            Endian::Big => u32::from_be_bytes(buf),
        })
    }

    fn read_f64(&mut self) -> Option<f64> {
        let mut buf = [0u8; 8];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => f64::from_le_bytes(buf),
            Endian::Big => f64::from_be_bytes(buf),
        })
    }

    fn read_i8(&mut self) -> Option<i8> {
        self.read_u8().map(|b| b as i8)
    }

    fn read_i16(&mut self) -> Option<i16> {
        let mut buf = [0u8; 2];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => i16::from_le_bytes(buf),
            Endian::Big => i16::from_be_bytes(buf),
        })
    }

    fn read_f32(&mut self) -> Option<f32> {
        let mut buf = [0u8; 4];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(match self.endian {
            Endian::Little => f32::from_le_bytes(buf),
            Endian::Big => f32::from_be_bytes(buf),
        })
    }

    /// Read `n` bytes into a new vec.
    fn read_bytes(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.cursor.read_exact(&mut buf).ok()?;
        Some(buf)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse the raster header from raw PostGIS raster WKB bytes.
///
/// Returns `None` if the data is too short or the endianness byte is invalid.
#[must_use]
pub fn parse_header(data: &[u8]) -> Option<RasterHeader> {
    if data.len() < HEADER_SIZE {
        return None;
    }

    let endian = Endian::from_byte(data[0])?;
    let mut r = WkbReader::new(data, endian);

    // Skip endianness byte (already parsed).
    r.set_position(1);

    let _version = r.read_u16()?;
    let num_bands = r.read_u16()?;
    let scale_x = r.read_f64()?;
    let scale_y = r.read_f64()?;
    let ip_x = r.read_f64()?;
    let ip_y = r.read_f64()?;
    let _skew_x = r.read_f64()?;
    let _skew_y = r.read_f64()?;
    let srid = r.read_i32()?;
    let width = r.read_u16()?;
    let height = r.read_u16()?;

    Some(RasterHeader {
        width,
        height,
        num_bands,
        scale_x,
        scale_y,
        ip_x,
        ip_y,
        srid,
    })
}

/// Compute the byte offset where band `band_index` starts.
///
/// Returns `None` if the data is malformed or `band_index` is out of range.
fn band_offset(data: &[u8], band_index: usize) -> Option<usize> {
    let header = parse_header(data)?;
    if band_index >= header.num_bands as usize {
        return None;
    }

    let endian = Endian::from_byte(data[0])?;
    let mut r = WkbReader::new(data, endian);
    r.set_position(HEADER_SIZE);

    let pixel_count = header.width as usize * header.height as usize;

    // Skip preceding bands.
    for _ in 0..band_index {
        let flags_byte = r.read_u8()?;
        let pix_code = flags_byte >> 4;
        let is_offline = (flags_byte & 0x08) != 0;
        let pt = PixelType::from_code(pix_code)?;
        let nodata_size = pt.byte_size();

        // Skip nodata value.
        r.read_bytes(nodata_size)?;

        if is_offline {
            // Offline band: 1 byte band number + null-terminated path.
            let _band_num = r.read_u8()?;
            loop {
                let b = r.read_u8()?;
                if b == 0 {
                    break;
                }
            }
        } else {
            // In-memory band: skip pixel data.
            let data_size = pixel_count * pt.byte_size();
            r.read_bytes(data_size)?;
        }
    }

    Some(r.position())
}

/// Parse band metadata for the band at `band_index`.
///
/// Returns `None` if the data is malformed or `band_index` is out of range.
#[must_use]
pub fn parse_band_info(data: &[u8], band_index: usize) -> Option<BandInfo> {
    let offset = band_offset(data, band_index)?;
    let endian = Endian::from_byte(data[0])?;
    let mut r = WkbReader::new(data, endian);
    r.set_position(offset);

    let flags_byte = r.read_u8()?;
    let pix_code = flags_byte >> 4;
    let has_nodata = (flags_byte & 0x01) != 0;
    let is_offline = (flags_byte & 0x08) != 0;
    let pixel_type = PixelType::from_code(pix_code)?;

    let nodata = read_nodata(&mut r, pixel_type)?;

    Some(BandInfo {
        pixel_type,
        nodata,
        has_nodata,
        is_offline,
    })
}

/// Read the nodata value for the given pixel type, returning it as `f64`.
fn read_nodata(r: &mut WkbReader<'_>, pt: PixelType) -> Option<f64> {
    Some(match pt {
        PixelType::Bool | PixelType::UInt2 | PixelType::UInt4 | PixelType::UInt8 => {
            f64::from(r.read_u8()?)
        }
        PixelType::Int8 => f64::from(r.read_i8()?),
        PixelType::Int16 => f64::from(r.read_i16()?),
        PixelType::UInt16 => f64::from(r.read_u16()?),
        PixelType::Int32 => f64::from(r.read_i32()?),
        PixelType::UInt32 => f64::from(r.read_u32()?),
        PixelType::Float32 => f64::from(r.read_f32()?),
        PixelType::Float64 => r.read_f64()?,
    })
}

/// Read a single pixel value as `f64`.
fn read_pixel(r: &mut WkbReader<'_>, pt: PixelType) -> Option<f64> {
    read_nodata(r, pt) // same byte layout
}

/// Extract all pixel values from band `band_index` as a flat `Vec<f64>`.
///
/// The pixels are in row-major order: `[row0_col0, row0_col1, ..., rowN_colM]`.
/// Returns `None` if the data is malformed, `band_index` is out of range, or
/// the band is stored offline.
#[must_use]
pub fn extract_pixels_f64(data: &[u8], band_index: usize) -> Option<Vec<f64>> {
    let header = parse_header(data)?;
    let offset = band_offset(data, band_index)?;
    let endian = Endian::from_byte(data[0])?;
    let mut r = WkbReader::new(data, endian);
    r.set_position(offset);

    let flags_byte = r.read_u8()?;
    let pix_code = flags_byte >> 4;
    let is_offline = (flags_byte & 0x08) != 0;
    let pixel_type = PixelType::from_code(pix_code)?;

    // Skip nodata value.
    r.read_bytes(pixel_type.byte_size())?;

    if is_offline {
        // Cannot extract pixels from offline bands.
        return None;
    }

    let pixel_count = header.width as usize * header.height as usize;
    let mut pixels = Vec::with_capacity(pixel_count);
    for _ in 0..pixel_count {
        pixels.push(read_pixel(&mut r, pixel_type)?);
    }

    Some(pixels)
}

/// Patch the pixel data of band 0 in a raster WKB blob with new f32 values.
///
/// Clones the original WKB, locates band 0's pixel data region, and
/// overwrites it with values from `output_f32`. The output is written in
/// the band's original pixel type (converting from f32).
///
/// Returns `None` if the WKB is malformed or the band is offline.
#[must_use]
pub fn patch_band0_pixels(original_wkb: &[u8], output_f32: &[f32]) -> Option<Vec<u8>> {
    let header = parse_header(original_wkb)?;
    let offset = band_offset(original_wkb, 0)?;
    let endian = Endian::from_byte(original_wkb[0])?;

    // Read band metadata to find pixel data start.
    let mut r = WkbReader::new(original_wkb, endian);
    r.set_position(offset);

    let flags_byte = r.read_u8()?;
    let pix_code = flags_byte >> 4;
    let is_offline = (flags_byte & 0x08) != 0;
    let pixel_type = PixelType::from_code(pix_code)?;

    if is_offline {
        return None;
    }

    // Skip nodata value to find pixel data start.
    r.read_bytes(pixel_type.byte_size())?;
    let pixel_data_start = r.position();

    let pixel_count = header.width as usize * header.height as usize;
    if output_f32.len() < pixel_count {
        return None;
    }

    // Clone original and overwrite pixel region.
    let mut result = original_wkb.to_vec();
    let pixel_size = pixel_type.byte_size();
    let pixel_region = pixel_data_start..pixel_data_start + pixel_count * pixel_size;
    if pixel_region.end > result.len() {
        return None;
    }

    // Write each pixel value in the original pixel type (little-endian).
    let is_le = endian == Endian::Little;
    for i in 0..pixel_count {
        let val = output_f32[i];
        let off = pixel_data_start + i * pixel_size;
        write_pixel_at(&mut result, off, pixel_type, val as f64, is_le);
    }

    Some(result)
}

/// Write a single pixel value at a given offset in a mutable byte buffer.
fn write_pixel_at(buf: &mut [u8], offset: usize, pt: PixelType, val: f64, le: bool) {
    match pt {
        PixelType::Bool | PixelType::UInt2 | PixelType::UInt4 | PixelType::UInt8 => {
            buf[offset] = val as u8;
        }
        PixelType::Int8 => {
            buf[offset] = val as i8 as u8;
        }
        PixelType::Int16 => {
            let bytes = if le {
                (val as i16).to_le_bytes()
            } else {
                (val as i16).to_be_bytes()
            };
            buf[offset..offset + 2].copy_from_slice(&bytes);
        }
        PixelType::UInt16 => {
            let bytes = if le {
                (val as u16).to_le_bytes()
            } else {
                (val as u16).to_be_bytes()
            };
            buf[offset..offset + 2].copy_from_slice(&bytes);
        }
        PixelType::Int32 => {
            let bytes = if le {
                (val as i32).to_le_bytes()
            } else {
                (val as i32).to_be_bytes()
            };
            buf[offset..offset + 4].copy_from_slice(&bytes);
        }
        PixelType::UInt32 => {
            let bytes = if le {
                (val as u32).to_le_bytes()
            } else {
                (val as u32).to_be_bytes()
            };
            buf[offset..offset + 4].copy_from_slice(&bytes);
        }
        PixelType::Float32 => {
            let bytes = if le {
                (val as f32).to_le_bytes()
            } else {
                (val as f32).to_be_bytes()
            };
            buf[offset..offset + 4].copy_from_slice(&bytes);
        }
        PixelType::Float64 => {
            let bytes = if le {
                val.to_le_bytes()
            } else {
                val.to_be_bytes()
            };
            buf[offset..offset + 8].copy_from_slice(&bytes);
        }
    }
}

// ---------------------------------------------------------------------------
// PostGIS-extension parsers used by raster dispatch (st_clip, st_reclass)
// ---------------------------------------------------------------------------

/// Extract the outer ring of a POLYGON GSERIALIZED as a flat
/// `[x0, y0, x1, y1, ...]` `Vec<f64>`.
///
/// Used by `ST_Clip(rast, geom)` to translate the polygon argument into
/// the flat ring-vertex array consumed by `pgaccel_raster_clip`.
///
/// The input is a GSERIALIZED v2 POLYGON; only the first (outer) ring is
/// returned. Inner rings (holes) are intentionally dropped — clip is
/// expected to mask pixels outside the outer boundary, and PostGIS
/// pre-decomposes multi-ring clips on its side.
///
/// Returns `None` if the GSERIALIZED header is malformed, the geometry
/// is not a POLYGON, or the polygon has zero rings.
#[must_use]
pub fn extract_polygon_ring(gserialized: &[u8]) -> Option<Vec<f64>> {
    // GSERIALIZED v2 layout (PostGIS lwgeom/gserialized2.c):
    //   header (8 bytes)
    //     - srid:   3 bytes
    //     - flags:  1 byte (bit 0x01 = HAS_BBOX)
    //     - varhdr: 4 bytes (already stripped if pgrx detoasts)
    //   bbox (optional, 32 bytes if BBOX flag set for 2D polygons)
    //   geometry payload:
    //     u32 type (POLYGON = 3)
    //     u32 nrings
    //     u32[] ring_npoints
    //     f64[] coords (interleaved x, y, ...)
    //
    // Minimum length: 8-byte header + 4 (type) + 4 (nrings) + 4
    // (one ring's npoints) = 20 bytes for the smallest valid polygon.
    if gserialized.len() < 20 {
        return None;
    }

    // Header byte 3 (zero-indexed) holds the GSERIALIZED v2 flags. Bit
    // 0 = HAS_BBOX. The bbox is 8 fp32 (32 bytes) when present for 2D.
    let flags = gserialized.get(3).copied()?;
    let has_bbox = (flags & 0x01) != 0;
    let geom_start = if has_bbox { 8 + 32 } else { 8 };

    if gserialized.len() < geom_start + 8 {
        return None;
    }

    let type_off = geom_start;
    let geom_type = u32::from_le_bytes(gserialized[type_off..type_off + 4].try_into().ok()?);
    // POLYGON = 3 (per PostGIS WKB type codes).
    if geom_type != 3 {
        return None;
    }

    let nrings_off = type_off + 4;
    let nrings = u32::from_le_bytes(gserialized[nrings_off..nrings_off + 4].try_into().ok()?);
    if nrings == 0 {
        return None;
    }

    let ring_npts_off = nrings_off + 4;
    if gserialized.len() < ring_npts_off + 4 {
        return None;
    }
    let outer_npts = u32::from_le_bytes(
        gserialized[ring_npts_off..ring_npts_off + 4]
            .try_into()
            .ok()?,
    ) as usize;

    // No alignment padding for 2D polygons (matches polygon.rs:41).
    let coords_off = ring_npts_off + (nrings as usize) * 4;
    let coords_bytes = outer_npts * 16; // 16 bytes per (x, y) fp64 pair
    if gserialized.len() < coords_off + coords_bytes {
        return None;
    }

    let mut out = Vec::with_capacity(outer_npts * 2);
    for i in 0..outer_npts {
        let off = coords_off + i * 16;
        let x = f64::from_le_bytes(gserialized[off..off + 8].try_into().ok()?);
        let y = f64::from_le_bytes(gserialized[off + 8..off + 16].try_into().ok()?);
        out.push(x);
        out.push(y);
    }
    Some(out)
}

/// One range -> new-value rule (mirrors `pgaccel_reclass_rule`).
///
/// Reproduced here in `f64` form so callers can build the rule list
/// without touching the FFI struct, then convert before dispatch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PgaccelReclassRule {
    pub min_val: f64,
    pub max_val: f64,
    pub new_val: f64,
}

/// Parse a PostGIS reclass-rule string into `Vec<PgaccelReclassRule>`.
///
/// PostGIS `ST_Reclass` rule syntax (subset; see PostGIS docs for the
/// full grammar):
///
/// - Whole-rule string: comma-separated `range:newval` pairs.
/// - `range`: `[lo-hi)`, `(lo-hi]`, `[lo-hi]`, `(lo-hi)` — bracket type
///   indicates inclusive (`[]`) vs exclusive (`()`). We treat all
///   variants as `[lo, hi)` for simplicity (the GPU kernel itself uses
///   half-open intervals at raster_ops.cpp:505).
/// - Range without brackets: `lo-hi`. Same half-open treatment.
/// - Negative numbers and decimals are supported.
///
/// Examples:
/// - `"[0-100):0,[100-200):1"` → 2 rules: `[0,100)→0`, `[100,200)→1`
/// - `"0-50:99, 50-100:200"` (whitespace tolerated) → 2 rules
///
/// Returns `Some(Vec::new())` for an empty input or a string that
/// contains only commas/whitespace; returns `None` only if a non-empty
/// rule fails to parse (so callers can disambiguate "no rules" from
/// "malformed rules").
#[must_use]
pub fn parse_reclass_rules(text: &str) -> Option<Vec<PgaccelReclassRule>> {
    let mut rules = Vec::new();
    for raw in text.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        rules.push(parse_one_rule(token)?);
    }
    Some(rules)
}

fn parse_one_rule(token: &str) -> Option<PgaccelReclassRule> {
    // Split on the LAST ':' so embedded negatives in the range work.
    let colon = token.rfind(':')?;
    let (range_part, new_part) = token.split_at(colon);
    let new_str = new_part[1..].trim();
    let new_val: f64 = new_str.parse().ok()?;

    // Strip optional brackets (always treat as half-open [lo, hi) — see
    // raster_ops.cpp:505 for the kernel's interval semantics).
    let r = range_part.trim();
    let stripped =
        if (r.starts_with('[') || r.starts_with('(')) && (r.ends_with(']') || r.ends_with(')')) {
            &r[1..r.len() - 1]
        } else {
            r
        };

    // Find the LAST '-' that is preceded by a digit (so leading negatives
    // and middle hyphens don't both count as separators).
    let mut dash_idx: Option<usize> = None;
    let bytes = stripped.as_bytes();
    for (i, b) in bytes.iter().enumerate().skip(1) {
        if *b == b'-' && bytes[i - 1].is_ascii_digit() {
            dash_idx = Some(i);
        }
    }
    let dash = dash_idx?;
    let lo: f64 = stripped[..dash].trim().parse().ok()?;
    let hi: f64 = stripped[dash + 1..].trim().parse().ok()?;

    Some(PgaccelReclassRule {
        min_val: lo,
        max_val: hi,
        new_val,
    })
}

// ---------------------------------------------------------------------------
// Pure-Rust unit tests for the PostGIS `rt_pixtype` decode table.
//
// These run under `cargo test -p pg_accel --lib` (no PG backend). They pin
// the WKB pixel-type code numbers literally against the PostGIS
// `librtcore.h` `rt_pixtype` enum so a future edit cannot silently re-shift
// the table (the exact P0 corruption this fixes). Do NOT reuse the test
// builder as the oracle — assert the code integers directly.
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod pixel_type_code_tests {
    use super::PixelType;

    /// PostGIS `rt_pixtype` codes (librtcore.h):
    /// 0=1BB, 1=2BUI, 2=4BUI, 3=8BSI, 4=8BUI, 5=16BSI, 6=16BUI,
    /// 7=32BSI, 8=32BUI, 9=32BF, 10=64BF.
    #[test]
    fn from_code_matches_postgis_rt_pixtype_literally() {
        assert_eq!(PixelType::from_code(0), Some(PixelType::Bool), "0 = 1BB");
        assert_eq!(PixelType::from_code(1), Some(PixelType::UInt2), "1 = 2BUI");
        assert_eq!(PixelType::from_code(2), Some(PixelType::UInt4), "2 = 4BUI");
        assert_eq!(PixelType::from_code(3), Some(PixelType::Int8), "3 = 8BSI");
        assert_eq!(PixelType::from_code(4), Some(PixelType::UInt8), "4 = 8BUI");
        assert_eq!(PixelType::from_code(5), Some(PixelType::Int16), "5 = 16BSI");
        assert_eq!(
            PixelType::from_code(6),
            Some(PixelType::UInt16),
            "6 = 16BUI"
        );
        assert_eq!(PixelType::from_code(7), Some(PixelType::Int32), "7 = 32BSI");
        assert_eq!(
            PixelType::from_code(8),
            Some(PixelType::UInt32),
            "8 = 32BUI"
        );
        assert_eq!(
            PixelType::from_code(9),
            Some(PixelType::Float32),
            "9 = 32BF"
        );
        assert_eq!(
            PixelType::from_code(10),
            Some(PixelType::Float64),
            "10 = 64BF"
        );
    }

    #[test]
    fn from_code_byte_sizes_by_literal_code() {
        // 1BB / 2BUI / 4BUI / 8BSI / 8BUI all serialize as one byte per pixel.
        for code in [0u8, 1, 2, 3, 4] {
            assert_eq!(
                PixelType::from_code(code).unwrap().byte_size(),
                1,
                "code {code} must be a 1-byte pixel"
            );
        }
        for code in [5u8, 6] {
            assert_eq!(
                PixelType::from_code(code).unwrap().byte_size(),
                2,
                "code {code} must be a 2-byte pixel"
            );
        }
        for code in [7u8, 8, 9] {
            assert_eq!(
                PixelType::from_code(code).unwrap().byte_size(),
                4,
                "code {code} must be a 4-byte pixel"
            );
        }
        assert_eq!(PixelType::from_code(10).unwrap().byte_size(), 8, "64BF");
    }

    #[test]
    fn from_code_rejects_out_of_range() {
        assert!(PixelType::from_code(11).is_none());
        assert!(PixelType::from_code(12).is_none());
        assert!(PixelType::from_code(255).is_none());
    }

    /// Hand-build a single-band raster WKB with a literal pixel-type code and
    /// confirm band metadata + pixel value decode correctly. The builder here
    /// writes the code integer directly (it is NOT `pixel_type_to_code`), so
    /// this is an independent oracle for `from_code` + band parsing.
    fn hand_build_wkb(pix_code: u8, pixel_bytes: &[u8], nodata_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(1u8); // little-endian
        buf.extend_from_slice(&0u16.to_le_bytes()); // version
        buf.extend_from_slice(&1u16.to_le_bytes()); // nBands
        buf.extend_from_slice(&1.0f64.to_le_bytes()); // scaleX
        buf.extend_from_slice(&(-1.0f64).to_le_bytes()); // scaleY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewY
        buf.extend_from_slice(&0i32.to_le_bytes()); // srid
        buf.extend_from_slice(&1u16.to_le_bytes()); // width = 1
        buf.extend_from_slice(&1u16.to_le_bytes()); // height = 1
        let flags: u8 = (pix_code << 4) | 0x01; // hasNodata
        buf.push(flags);
        buf.extend_from_slice(nodata_bytes);
        buf.extend_from_slice(pixel_bytes);
        buf
    }

    #[test]
    fn hand_built_8bui_decodes() {
        // code 4 = 8BUI, one byte per pixel, value 200.
        let wkb = hand_build_wkb(4, &[200u8], &[0u8]);
        let band = super::parse_band_info(&wkb, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::UInt8);
        let px = super::extract_pixels_f64(&wkb, 0).unwrap();
        assert_eq!(px, vec![200.0]);
    }

    #[test]
    fn hand_built_16bsi_decodes_negative() {
        // code 5 = 16BSI, two LE bytes, value -1000.
        let wkb = hand_build_wkb(5, &(-1000i16).to_le_bytes(), &0i16.to_le_bytes());
        let band = super::parse_band_info(&wkb, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Int16);
        let px = super::extract_pixels_f64(&wkb, 0).unwrap();
        assert_eq!(px, vec![-1000.0]);
    }

    #[test]
    fn hand_built_32bf_decodes() {
        // code 9 = 32BF, four LE bytes, value 3.5.
        let wkb = hand_build_wkb(9, &3.5f32.to_le_bytes(), &0.0f32.to_le_bytes());
        let band = super::parse_band_info(&wkb, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Float32);
        let px = super::extract_pixels_f64(&wkb, 0).unwrap();
        assert_eq!(px, vec![3.5]);
    }

    #[test]
    fn hand_built_64bf_decodes() {
        // code 10 = 64BF, eight LE bytes.
        let wkb = hand_build_wkb(10, &2.5f64.to_le_bytes(), &0.0f64.to_le_bytes());
        let band = super::parse_band_info(&wkb, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Float64);
        let px = super::extract_pixels_f64(&wkb, 0).unwrap();
        assert_eq!(px, vec![2.5]);
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
mod test_helpers {
    use super::*;

    /// Build a minimal raster WKB blob for testing.
    ///
    /// Creates a little-endian, version-0 raster with the given dimensions,
    /// one band of the given pixel type, and all pixels set to `fill_value`.
    pub fn build_test_raster(
        width: u16,
        height: u16,
        srid: i32,
        pixel_type: PixelType,
        nodata: f64,
        fill_value: f64,
    ) -> Vec<u8> {
        let mut buf = Vec::new();

        // Endianness: little.
        buf.push(1);
        // Version: 0.
        buf.extend_from_slice(&0u16.to_le_bytes());
        // nBands: 1.
        buf.extend_from_slice(&1u16.to_le_bytes());
        // scaleX, scaleY.
        buf.extend_from_slice(&1.0f64.to_le_bytes());
        buf.extend_from_slice(&(-1.0f64).to_le_bytes());
        // ipX, ipY.
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        // skewX, skewY.
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        // SRID.
        buf.extend_from_slice(&srid.to_le_bytes());
        // Width, Height.
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());

        // Band header.
        let pix_code = pixel_type_to_code(pixel_type);
        let flags: u8 = (pix_code << 4) | 0x01; // hasNodata = true
        buf.push(flags);

        // Nodata value.
        write_pixel_value(&mut buf, pixel_type, nodata);

        // Pixel data.
        let pixel_count = width as usize * height as usize;
        for _ in 0..pixel_count {
            write_pixel_value(&mut buf, pixel_type, fill_value);
        }

        buf
    }

    fn pixel_type_to_code(pt: PixelType) -> u8 {
        // Must match the PostGIS `rt_pixtype` enum (librtcore.h).
        match pt {
            PixelType::Bool => 0,
            PixelType::UInt2 => 1,
            PixelType::UInt4 => 2,
            PixelType::Int8 => 3,
            PixelType::UInt8 => 4,
            PixelType::Int16 => 5,
            PixelType::UInt16 => 6,
            PixelType::Int32 => 7,
            PixelType::UInt32 => 8,
            PixelType::Float32 => 9,
            PixelType::Float64 => 10,
        }
    }

    fn write_pixel_value(buf: &mut Vec<u8>, pt: PixelType, val: f64) {
        match pt {
            PixelType::Bool | PixelType::UInt2 | PixelType::UInt4 | PixelType::UInt8 => {
                buf.push(val as u8);
            }
            PixelType::Int8 => buf.push(val as i8 as u8),
            PixelType::Int16 => {
                buf.extend_from_slice(&(val as i16).to_le_bytes());
            }
            PixelType::UInt16 => {
                buf.extend_from_slice(&(val as u16).to_le_bytes());
            }
            PixelType::Int32 => {
                buf.extend_from_slice(&(val as i32).to_le_bytes());
            }
            PixelType::UInt32 => {
                buf.extend_from_slice(&(val as u32).to_le_bytes());
            }
            PixelType::Float32 => {
                buf.extend_from_slice(&(val as f32).to_le_bytes());
            }
            PixelType::Float64 => {
                buf.extend_from_slice(&val.to_le_bytes());
            }
        }
    }
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::test_helpers::build_test_raster;
    use super::*;

    #[test]
    fn parse_header_basic() {
        let data = build_test_raster(3, 2, 4326, PixelType::UInt8, 0.0, 42.0);
        let hdr = parse_header(&data).unwrap();
        assert_eq!(hdr.width, 3);
        assert_eq!(hdr.height, 2);
        assert_eq!(hdr.num_bands, 1);
        assert_eq!(hdr.srid, 4326);
        assert!((hdr.scale_x - 1.0).abs() < f64::EPSILON);
        assert!((hdr.scale_y - (-1.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_header_too_short() {
        let data = vec![1u8; 10]; // way too short
        assert!(parse_header(&data).is_none());
    }

    #[test]
    fn parse_header_invalid_endian() {
        let mut data = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
        data[0] = 0xFF; // invalid endianness
        assert!(parse_header(&data).is_none());
    }

    #[test]
    fn parse_band_info_uint8() {
        let data = build_test_raster(2, 2, 0, PixelType::UInt8, 255.0, 1.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::UInt8);
        assert!(band.has_nodata);
        assert!(!band.is_offline);
        assert!((band.nodata - 255.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_band_info_out_of_range() {
        let data = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
        assert!(parse_band_info(&data, 1).is_none());
    }

    #[test]
    fn extract_pixels_uint8() {
        let data = build_test_raster(3, 2, 0, PixelType::UInt8, 0.0, 42.0);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 6);
        for &p in &pixels {
            assert!((p - 42.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn extract_pixels_float32() {
        let data = build_test_raster(2, 2, 0, PixelType::Float32, -9999.0, 3.14);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 4);
        for &p in &pixels {
            assert!((p - f64::from(3.14_f32)).abs() < 1e-5);
        }
    }

    #[test]
    fn extract_pixels_float64() {
        let data = build_test_raster(1, 1, 0, PixelType::Float64, f64::NAN, std::f64::consts::PI);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 1);
        assert!((pixels[0] - std::f64::consts::PI).abs() < f64::EPSILON);
    }

    #[test]
    fn extract_pixels_int16() {
        let data = build_test_raster(2, 3, 0, PixelType::Int16, -1.0, -100.0);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 6);
        for &p in &pixels {
            assert!((p - (-100.0)).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn extract_pixels_band_out_of_range() {
        let data = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
        assert!(extract_pixels_f64(&data, 5).is_none());
    }

    #[test]
    fn pixel_type_byte_sizes() {
        assert_eq!(PixelType::Bool.byte_size(), 1);
        assert_eq!(PixelType::Int8.byte_size(), 1);
        assert_eq!(PixelType::UInt8.byte_size(), 1);
        assert_eq!(PixelType::Int16.byte_size(), 2);
        assert_eq!(PixelType::UInt16.byte_size(), 2);
        assert_eq!(PixelType::Int32.byte_size(), 4);
        assert_eq!(PixelType::UInt32.byte_size(), 4);
        assert_eq!(PixelType::Float32.byte_size(), 4);
        assert_eq!(PixelType::Float64.byte_size(), 8);
    }

    #[test]
    fn empty_raster_no_pixels() {
        let data = build_test_raster(0, 0, 0, PixelType::UInt8, 0.0, 0.0);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert!(pixels.is_empty());
    }

    // -- Pixel type coverage (all from_code values) ---------------------------

    #[test]
    fn pixel_type_bool_roundtrip() {
        let data = build_test_raster(2, 2, 0, PixelType::Bool, 0.0, 1.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Bool);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 4);
        for &p in &pixels {
            assert!((p - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn pixel_type_int8_roundtrip() {
        let data = build_test_raster(1, 1, 0, PixelType::Int8, -1.0, -128.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Int8);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert_eq!(pixels.len(), 1);
        assert!((pixels[0] - (-128.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_type_uint16_roundtrip() {
        let data = build_test_raster(1, 1, 0, PixelType::UInt16, 0.0, 65535.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::UInt16);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert!((pixels[0] - 65535.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_type_int32_roundtrip() {
        let data = build_test_raster(1, 1, 0, PixelType::Int32, 0.0, -100_000.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::Int32);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert!((pixels[0] - (-100_000.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_type_uint32_roundtrip() {
        let data = build_test_raster(1, 1, 0, PixelType::UInt32, 0.0, 3_000_000.0);
        let band = parse_band_info(&data, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::UInt32);
        let pixels = extract_pixels_f64(&data, 0).unwrap();
        assert!((pixels[0] - 3_000_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pixel_type_from_code_invalid_returns_none() {
        // Codes 11+ are out of range for PostGIS rt_pixtype (0..=10).
        assert!(PixelType::from_code(11).is_none());
        assert!(PixelType::from_code(12).is_none());
        assert!(PixelType::from_code(255).is_none());
    }

    #[test]
    fn pixel_type_from_code_all_valid() {
        // PostGIS rt_pixtype (librtcore.h): 0=1BB, 1=2BUI, 2=4BUI, 3=8BSI,
        // 4=8BUI, 5=16BSI, 6=16BUI, 7=32BSI, 8=32BUI, 9=32BF, 10=64BF.
        assert_eq!(PixelType::from_code(0), Some(PixelType::Bool));
        assert_eq!(PixelType::from_code(1), Some(PixelType::UInt2));
        assert_eq!(PixelType::from_code(2), Some(PixelType::UInt4));
        assert_eq!(PixelType::from_code(3), Some(PixelType::Int8));
        assert_eq!(PixelType::from_code(4), Some(PixelType::UInt8));
        assert_eq!(PixelType::from_code(5), Some(PixelType::Int16));
        assert_eq!(PixelType::from_code(6), Some(PixelType::UInt16));
        assert_eq!(PixelType::from_code(7), Some(PixelType::Int32));
        assert_eq!(PixelType::from_code(8), Some(PixelType::UInt32));
        assert_eq!(PixelType::from_code(9), Some(PixelType::Float32));
        assert_eq!(PixelType::from_code(10), Some(PixelType::Float64));
    }

    // -- Multi-band tests -----------------------------------------------------

    /// Build a multi-band raster with `n` bands, all same pixel type and fill.
    fn build_multiband_raster(
        width: u16,
        height: u16,
        num_bands: u16,
        pixel_type: PixelType,
        nodata: f64,
        fill_value: f64,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        // Endianness: little.
        buf.push(1);
        // Version: 0.
        buf.extend_from_slice(&0u16.to_le_bytes());
        // nBands.
        buf.extend_from_slice(&num_bands.to_le_bytes());
        // scaleX, scaleY.
        buf.extend_from_slice(&1.0f64.to_le_bytes());
        buf.extend_from_slice(&(-1.0f64).to_le_bytes());
        // ipX, ipY.
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        // skewX, skewY.
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        // SRID.
        buf.extend_from_slice(&0i32.to_le_bytes());
        // Width, Height.
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());

        let pix_code = match pixel_type {
            PixelType::Bool => 0u8,
            PixelType::UInt2 => 1,
            PixelType::UInt4 => 2,
            PixelType::Int8 => 3,
            PixelType::UInt8 => 4,
            PixelType::Int16 => 5,
            PixelType::UInt16 => 6,
            PixelType::Int32 => 7,
            PixelType::UInt32 => 8,
            PixelType::Float32 => 9,
            PixelType::Float64 => 10,
        };

        let pixel_count = width as usize * height as usize;
        for _ in 0..num_bands {
            let flags: u8 = (pix_code << 4) | 0x01; // hasNodata = true
            buf.push(flags);
            // Write nodata
            write_pixel(&mut buf, pixel_type, nodata);
            // Write pixels
            for _ in 0..pixel_count {
                write_pixel(&mut buf, pixel_type, fill_value);
            }
        }
        buf
    }

    fn write_pixel(buf: &mut Vec<u8>, pt: PixelType, val: f64) {
        match pt {
            PixelType::Bool | PixelType::UInt2 | PixelType::UInt4 | PixelType::UInt8 => {
                buf.push(val as u8);
            }
            PixelType::Int8 => buf.push(val as i8 as u8),
            PixelType::Int16 => buf.extend_from_slice(&(val as i16).to_le_bytes()),
            PixelType::UInt16 => buf.extend_from_slice(&(val as u16).to_le_bytes()),
            PixelType::Int32 => buf.extend_from_slice(&(val as i32).to_le_bytes()),
            PixelType::UInt32 => buf.extend_from_slice(&(val as u32).to_le_bytes()),
            PixelType::Float32 => buf.extend_from_slice(&(val as f32).to_le_bytes()),
            PixelType::Float64 => buf.extend_from_slice(&val.to_le_bytes()),
        }
    }

    #[test]
    fn multiband_2_bands_header() {
        let data = build_multiband_raster(2, 2, 2, PixelType::UInt8, 0.0, 10.0);
        let hdr = parse_header(&data).unwrap();
        assert_eq!(hdr.num_bands, 2);
    }

    #[test]
    fn multiband_3_bands_parse_each() {
        let data = build_multiband_raster(1, 1, 3, PixelType::Float32, -9999.0, 1.5);
        for i in 0..3 {
            let band = parse_band_info(&data, i).unwrap();
            assert_eq!(band.pixel_type, PixelType::Float32);
            assert!(band.has_nodata);
            let pixels = extract_pixels_f64(&data, i).unwrap();
            assert_eq!(pixels.len(), 1);
            assert!((pixels[0] - f64::from(1.5_f32)).abs() < 1e-5);
        }
    }

    // -- Nodata flag tests ----------------------------------------------------

    #[test]
    fn band_without_nodata_flag() {
        // Build manually: hasNodata bit = 0
        let mut buf = Vec::new();
        buf.push(1u8); // LE
        buf.extend_from_slice(&0u16.to_le_bytes()); // version
        buf.extend_from_slice(&1u16.to_le_bytes()); // nBands
        buf.extend_from_slice(&1.0f64.to_le_bytes()); // scaleX
        buf.extend_from_slice(&(-1.0f64).to_le_bytes()); // scaleY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewY
        buf.extend_from_slice(&0i32.to_le_bytes()); // srid
        buf.extend_from_slice(&1u16.to_le_bytes()); // width
        buf.extend_from_slice(&1u16.to_le_bytes()); // height

        // Band: pixel type UInt8 (code 2), no flags -> hasNodata=false
        let flags: u8 = 2 << 4; // pix_code=2, no nodata bit
        buf.push(flags);
        buf.push(0u8); // nodata value (still written, just flag is off)
        buf.push(42u8); // 1 pixel

        let band = parse_band_info(&buf, 0).unwrap();
        assert_eq!(band.pixel_type, PixelType::UInt8);
        assert!(!band.has_nodata);
    }

    // -- Offline band test ----------------------------------------------------

    #[test]
    fn offline_band_detected_and_pixels_rejected() {
        let mut buf = Vec::new();
        buf.push(1u8); // LE
        buf.extend_from_slice(&0u16.to_le_bytes()); // version
        buf.extend_from_slice(&1u16.to_le_bytes()); // nBands
        buf.extend_from_slice(&1.0f64.to_le_bytes()); // scaleX
        buf.extend_from_slice(&(-1.0f64).to_le_bytes()); // scaleY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // ipY
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewX
        buf.extend_from_slice(&0.0f64.to_le_bytes()); // skewY
        buf.extend_from_slice(&0i32.to_le_bytes()); // srid
        buf.extend_from_slice(&1u16.to_le_bytes()); // width
        buf.extend_from_slice(&1u16.to_le_bytes()); // height

        // Band: pixel type UInt8 (code 2), offline bit (0x08) set
        let flags: u8 = (2 << 4) | 0x08;
        buf.push(flags);
        buf.push(0u8); // nodata
        buf.push(0u8); // band number
        buf.extend_from_slice(b"/tmp/band.dat\0"); // null-terminated path

        let band = parse_band_info(&buf, 0).unwrap();
        assert!(band.is_offline);
        assert_eq!(band.pixel_type, PixelType::UInt8);

        // Pixel extraction should return None for offline bands
        assert!(extract_pixels_f64(&buf, 0).is_none());
    }

    // -- Endianness tests -----------------------------------------------------

    #[test]
    fn big_endian_raster_header() {
        let mut buf = Vec::new();
        buf.push(0u8); // Big endian
        buf.extend_from_slice(&0u16.to_be_bytes()); // version
        buf.extend_from_slice(&1u16.to_be_bytes()); // nBands
        buf.extend_from_slice(&1.0f64.to_be_bytes()); // scaleX
        buf.extend_from_slice(&(-1.0f64).to_be_bytes()); // scaleY
        buf.extend_from_slice(&10.0f64.to_be_bytes()); // ipX
        buf.extend_from_slice(&20.0f64.to_be_bytes()); // ipY
        buf.extend_from_slice(&0.0f64.to_be_bytes()); // skewX
        buf.extend_from_slice(&0.0f64.to_be_bytes()); // skewY
        buf.extend_from_slice(&4326i32.to_be_bytes()); // srid
        buf.extend_from_slice(&3u16.to_be_bytes()); // width
        buf.extend_from_slice(&2u16.to_be_bytes()); // height

        // Band: UInt8 (code 2), hasNodata
        let flags: u8 = (2 << 4) | 0x01;
        buf.push(flags);
        buf.push(0u8); // nodata

        // 6 pixels
        for _ in 0..6 {
            buf.push(99u8);
        }

        let hdr = parse_header(&buf).unwrap();
        assert_eq!(hdr.width, 3);
        assert_eq!(hdr.height, 2);
        assert_eq!(hdr.srid, 4326);
        assert!((hdr.ip_x - 10.0).abs() < f64::EPSILON);
        assert!((hdr.ip_y - 20.0).abs() < f64::EPSILON);

        let pixels = extract_pixels_f64(&buf, 0).unwrap();
        assert_eq!(pixels.len(), 6);
        for &p in &pixels {
            assert!((p - 99.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn invalid_endian_byte_rejected() {
        let mut data = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
        data[0] = 2; // invalid: only 0 and 1 are valid
        assert!(parse_header(&data).is_none());
    }

    // -- Corrupt / truncated input tests --------------------------------------

    #[test]
    fn truncated_at_5_bytes() {
        let buf = vec![1u8, 0, 0, 0, 0]; // LE + partial version
        assert!(parse_header(&buf).is_none());
    }

    // -- extract_polygon_ring tests ------------------------------------------

    /// Build a minimal GSERIALIZED v2 POLYGON (no bbox flag) with one outer
    /// ring whose coords are the four corners of a unit square, in the
    /// canonical CCW orientation (closed: 5th point == 1st).
    fn build_polygon_unit_square_no_bbox() -> Vec<u8> {
        let mut buf = Vec::new();
        // Header: srid (3 bytes) + flags (1 byte) + varlena (4 bytes).
        buf.extend_from_slice(&[0u8; 3]); // srid bytes (zeroed)
        buf.push(0u8); // flags: HAS_BBOX = 0
        buf.extend_from_slice(&[0u8; 4]); // varlena placeholder
        // type = POLYGON (3) LE u32
        buf.extend_from_slice(&3u32.to_le_bytes());
        // nrings = 1
        buf.extend_from_slice(&1u32.to_le_bytes());
        // ring_npoints = 5
        buf.extend_from_slice(&5u32.to_le_bytes());
        // 5 (x, y) f64 pairs
        let pts: [(f64, f64); 5] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        for (x, y) in pts {
            buf.extend_from_slice(&x.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
        }
        buf
    }

    #[test]
    fn extract_polygon_ring_unit_square() {
        let buf = build_polygon_unit_square_no_bbox();
        let ring = extract_polygon_ring(&buf).unwrap();
        assert_eq!(ring.len(), 10, "5 points * 2 coords");
        assert!((ring[0] - 0.0).abs() < f64::EPSILON);
        assert!((ring[1] - 0.0).abs() < f64::EPSILON);
        assert!((ring[2] - 1.0).abs() < f64::EPSILON);
        assert!((ring[8] - 0.0).abs() < f64::EPSILON, "closing x");
        assert!((ring[9] - 0.0).abs() < f64::EPSILON, "closing y");
    }

    #[test]
    fn extract_polygon_ring_short_input_rejected() {
        let buf = vec![0u8; 8];
        assert!(extract_polygon_ring(&buf).is_none());
    }

    #[test]
    fn extract_polygon_ring_non_polygon_type_rejected() {
        // Build a header that claims POINT (type=1), not POLYGON.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0u8; 3]);
        buf.push(0u8);
        buf.extend_from_slice(&[0u8; 4]);
        buf.extend_from_slice(&1u32.to_le_bytes()); // POINT
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 16]); // dummy coord
        assert!(extract_polygon_ring(&buf).is_none());
    }

    #[test]
    fn extract_polygon_ring_truncated_coords() {
        // POLYGON header claims 3 points but has only 1 worth of bytes.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0u8; 3]);
        buf.push(0u8);
        buf.extend_from_slice(&[0u8; 4]);
        buf.extend_from_slice(&3u32.to_le_bytes()); // POLYGON
        buf.extend_from_slice(&1u32.to_le_bytes()); // nrings
        buf.extend_from_slice(&3u32.to_le_bytes()); // npoints
        buf.extend_from_slice(&[0u8; 16]); // only 1 point
        assert!(extract_polygon_ring(&buf).is_none());
    }

    // -- parse_reclass_rules tests --------------------------------------------

    #[test]
    fn parse_reclass_rules_canonical_form() {
        let rules = parse_reclass_rules("[0-100):0,[100-200):1,[200-300):2").unwrap();
        assert_eq!(rules.len(), 3);
        assert!((rules[0].min_val - 0.0).abs() < f64::EPSILON);
        assert!((rules[0].max_val - 100.0).abs() < f64::EPSILON);
        assert!((rules[0].new_val - 0.0).abs() < f64::EPSILON);
        assert!((rules[1].min_val - 100.0).abs() < f64::EPSILON);
        assert!((rules[1].max_val - 200.0).abs() < f64::EPSILON);
        assert!((rules[1].new_val - 1.0).abs() < f64::EPSILON);
        assert!((rules[2].new_val - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_reclass_rules_no_brackets() {
        let rules = parse_reclass_rules("0-50:99, 50-100:200").unwrap();
        assert_eq!(rules.len(), 2);
        assert!((rules[0].min_val - 0.0).abs() < f64::EPSILON);
        assert!((rules[0].max_val - 50.0).abs() < f64::EPSILON);
        assert!((rules[0].new_val - 99.0).abs() < f64::EPSILON);
        assert!((rules[1].max_val - 100.0).abs() < f64::EPSILON);
        assert!((rules[1].new_val - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_reclass_rules_decimals_and_negatives() {
        let rules = parse_reclass_rules("-1.5-2.5:3.0").unwrap();
        assert_eq!(rules.len(), 1);
        assert!((rules[0].min_val - (-1.5)).abs() < f64::EPSILON);
        assert!((rules[0].max_val - 2.5).abs() < f64::EPSILON);
        assert!((rules[0].new_val - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_reclass_rules_empty_input() {
        let rules = parse_reclass_rules("").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_reclass_rules_only_whitespace_commas() {
        let rules = parse_reclass_rules(" , , ").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_reclass_rules_malformed_returns_none() {
        // Missing colon
        assert!(parse_reclass_rules("0-100").is_none());
        // Missing dash
        assert!(parse_reclass_rules("100:5").is_none());
        // Non-numeric
        assert!(parse_reclass_rules("abc-def:0").is_none());
    }

    #[test]
    fn truncated_band_data() {
        // Valid header but band data cut short
        let mut buf = Vec::new();
        buf.push(1u8); // LE
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // 1 band
        buf.extend_from_slice(&1.0f64.to_le_bytes());
        buf.extend_from_slice(&(-1.0f64).to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0.0f64.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&4u16.to_le_bytes()); // width=4
        buf.extend_from_slice(&4u16.to_le_bytes()); // height=4
        // No band data at all — should fail on parse_band_info
        assert!(parse_band_info(&buf, 0).is_none());
        assert!(extract_pixels_f64(&buf, 0).is_none());
    }
}

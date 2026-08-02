//! PostGIS raster WKB format parser.
//!
//! Parses the binary representation of `raster` values as stored by PostGIS
//! Raster. Validates complete values and normalizes their bands for residency.
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
    pub version: u16,
    pub little_endian: bool,
    pub width: u16,
    pub height: u16,
    pub num_bands: u16,
    pub scale_x: f64,
    pub scale_y: f64,
    pub ip_x: f64,
    pub ip_y: f64,
    pub skew_x: f64,
    pub skew_y: f64,
    pub srid: i32,
}

/// One fully validated in-database raster band prepared for residency.
///
/// `pixels` retains the PostGIS pixel type but is normalized to native
/// little-endian byte order. The untouched WKB remains a separate retained
/// value and is the source of truth for exact output reconstruction.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentRasterBandInput {
    pub pixel_type: PixelType,
    pub nodata: f64,
    pub has_nodata: bool,
    pub is_nodata: bool,
    pub pixels: Vec<u8>,
}

/// One fully validated raster value prepared for the resident domain layout.
#[derive(Debug, Clone, PartialEq)]
pub struct ResidentRasterInput {
    pub header: RasterHeader,
    pub bands: Vec<ResidentRasterBandInput>,
}

/// Structural reason a raster value cannot enter the resident lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentRasterParseError {
    Truncated,
    InvalidEndian,
    UnsupportedVersion,
    InvalidMetadata,
    UnknownPixelType,
    InvalidBandFlags,
    OfflineBand,
    TrailingBytes,
    ByteCountOverflow,
}

impl std::fmt::Display for ResidentRasterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match self {
            Self::Truncated => "raster WKB is truncated",
            Self::InvalidEndian => "raster WKB has an invalid endian marker",
            Self::UnsupportedVersion => "raster WKB version is unsupported",
            Self::InvalidMetadata => "raster WKB metadata is invalid",
            Self::UnknownPixelType => "raster WKB contains an unknown pixel type",
            Self::InvalidBandFlags => "raster WKB contains invalid band flags",
            Self::OfflineBand => "offline raster bands cannot enter residency",
            Self::TrailingBytes => "raster WKB contains trailing bytes",
            Self::ByteCountOverflow => "raster WKB byte count overflows usize",
        };
        f.write_str(detail)
    }
}

impl std::error::Error for ResidentRasterParseError {}

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
    /// 8=32BUI, 10=32BF, 11=64BF` — value 9 is skipped in the enum across
    /// PostGIS 3.x. Getting this table wrong silently shifts
    /// every wider type (real 8BUI reads as UInt16, 32BF is rejected, etc.),
    /// so it is pinned by `pixel_type_code_tests`.
    pub const fn from_code(code: u8) -> Option<Self> {
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
            // PostGIS rt_pixtype (librtcore.h, stable-3.4 and stable-3.6 both):
            // PT_32BF=10, PT_64BF=11 — value 9 is skipped in the enum and has
            // been across PostGIS 3.x. Confirmed empirically against a live
            // PostGIS 3.6.3 via ST_AsBinary band bytes for every type.
            9 => None,
            10 => Some(Self::Float32),
            11 => Some(Self::Float64),
            _ => None,
        }
    }

    /// Literal PostGIS `rt_pixtype` tag used by the resident ABI.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Bool => 0,
            Self::UInt2 => 1,
            Self::UInt4 => 2,
            Self::Int8 => 3,
            Self::UInt8 => 4,
            Self::Int16 => 5,
            Self::UInt16 => 6,
            Self::Int32 => 7,
            Self::UInt32 => 8,
            Self::Float32 => 10,
            Self::Float64 => 11,
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
fn parse_header(data: &[u8]) -> Option<RasterHeader> {
    if data.len() < HEADER_SIZE {
        return None;
    }

    let endian = Endian::from_byte(data[0])?;
    let mut r = WkbReader::new(data, endian);

    // Skip endianness byte (already parsed).
    r.set_position(1);

    let version = r.read_u16()?;
    let num_bands = r.read_u16()?;
    let scale_x = r.read_f64()?;
    let scale_y = r.read_f64()?;
    let ip_x = r.read_f64()?;
    let ip_y = r.read_f64()?;
    let skew_x = r.read_f64()?;
    let skew_y = r.read_f64()?;
    let srid = r.read_i32()?;
    let width = r.read_u16()?;
    let height = r.read_u16()?;

    Some(RasterHeader {
        version,
        little_endian: endian == Endian::Little,
        width,
        height,
        num_bands,
        scale_x,
        scale_y,
        ip_x,
        ip_y,
        skew_x,
        skew_y,
        srid,
    })
}

/// Parse and validate a complete in-database PostGIS raster value.
///
/// This rejects unsupported versions, reserved flags, offline bands, integer
/// overflow, truncation, and trailing bytes in one pass. Multi-byte pixels are
/// normalized to little endian for the resident device contract; callers
/// retain `data` unchanged alongside the returned value for exact
/// reconstruction.
pub fn parse_resident_raster(data: &[u8]) -> Result<ResidentRasterInput, ResidentRasterParseError> {
    if data.len() < HEADER_SIZE {
        return Err(ResidentRasterParseError::Truncated);
    }
    let endian = Endian::from_byte(data[0]).ok_or(ResidentRasterParseError::InvalidEndian)?;
    let header = parse_header(data).ok_or(ResidentRasterParseError::Truncated)?;
    if header.version != 0 {
        return Err(ResidentRasterParseError::UnsupportedVersion);
    }
    if !(0..=999_999).contains(&header.srid)
        || !header.scale_x.is_finite()
        || !header.scale_y.is_finite()
        || !header.ip_x.is_finite()
        || !header.ip_y.is_finite()
        || !header.skew_x.is_finite()
        || !header.skew_y.is_finite()
    {
        return Err(ResidentRasterParseError::InvalidMetadata);
    }

    let pixel_count = usize::from(header.width)
        .checked_mul(usize::from(header.height))
        .ok_or(ResidentRasterParseError::ByteCountOverflow)?;
    let mut reader = WkbReader::new(data, endian);
    reader.set_position(HEADER_SIZE);
    let mut bands = Vec::new();
    bands
        .try_reserve_exact(usize::from(header.num_bands))
        .map_err(|_| ResidentRasterParseError::ByteCountOverflow)?;

    for _ in 0..header.num_bands {
        let flags = reader
            .read_u8()
            .ok_or(ResidentRasterParseError::Truncated)?;
        if flags & 0x10 != 0 {
            return Err(ResidentRasterParseError::InvalidBandFlags);
        }
        let pixel_type =
            PixelType::from_code(flags & 0x0f).ok_or(ResidentRasterParseError::UnknownPixelType)?;
        let is_offline = flags & 0x80 != 0;
        let has_nodata = flags & 0x40 != 0;
        let is_nodata = flags & 0x20 != 0;
        if is_offline {
            return Err(ResidentRasterParseError::OfflineBand);
        }
        if is_nodata && !has_nodata {
            return Err(ResidentRasterParseError::InvalidBandFlags);
        }

        let nodata =
            read_nodata(&mut reader, pixel_type).ok_or(ResidentRasterParseError::Truncated)?;
        let pixel_bytes = pixel_count
            .checked_mul(pixel_type.byte_size())
            .ok_or(ResidentRasterParseError::ByteCountOverflow)?;
        let mut pixels = reader
            .read_bytes(pixel_bytes)
            .ok_or(ResidentRasterParseError::Truncated)?;
        if endian == Endian::Big && pixel_type.byte_size() > 1 {
            for pixel in pixels.chunks_exact_mut(pixel_type.byte_size()) {
                pixel.reverse();
            }
        }
        bands.push(ResidentRasterBandInput {
            pixel_type,
            nodata: if has_nodata { nodata } else { 0.0 },
            has_nodata,
            is_nodata,
            pixels,
        });
    }
    if reader.position() != data.len() {
        return Err(ResidentRasterParseError::TrailingBytes);
    }
    Ok(ResidentRasterInput { header, bands })
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
    use super::{PixelType, parse_resident_raster};

    /// PostGIS `rt_pixtype` codes (librtcore.h):
    /// 0=1BB, 1=2BUI, 2=4BUI, 3=8BSI, 4=8BUI, 5=16BSI, 6=16BUI,
    /// 7=32BSI, 8=32BUI, 10=32BF, 11=64BF (9 skipped).
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
        assert_eq!(PixelType::from_code(9), None, "9 is skipped in rt_pixtype");
        assert_eq!(
            PixelType::from_code(10),
            Some(PixelType::Float32),
            "10 = 32BF"
        );
        assert_eq!(
            PixelType::from_code(11),
            Some(PixelType::Float64),
            "11 = 64BF"
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
        for code in [7u8, 8, 10] {
            assert_eq!(
                PixelType::from_code(code).unwrap().byte_size(),
                4,
                "code {code} must be a 4-byte pixel"
            );
        }
        assert_eq!(PixelType::from_code(11).unwrap().byte_size(), 8, "64BF");
    }

    #[test]
    fn from_code_rejects_out_of_range() {
        assert!(
            PixelType::from_code(9).is_none(),
            "9 is a skipped enum value"
        );
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
        let flags: u8 = pix_code | 0x40; // low nibble = pixtype, 0x40 = HASNODATA
        buf.push(flags);
        buf.extend_from_slice(nodata_bytes);
        buf.extend_from_slice(pixel_bytes);
        buf
    }

    #[test]
    fn hand_built_8bui_decodes() {
        // code 4 = 8BUI, one byte per pixel, value 200.
        let wkb = hand_build_wkb(4, &[200u8], &[0u8]);
        let parsed = parse_resident_raster(&wkb).unwrap();
        let band = &parsed.bands[0];
        assert_eq!(band.pixel_type, PixelType::UInt8);
        assert_eq!(band.pixels, [200]);
        assert_eq!(band.nodata, 0.0);
    }

    #[test]
    fn hand_built_16bsi_decodes_negative() {
        // code 5 = 16BSI, two LE bytes, value -1000.
        let wkb = hand_build_wkb(5, &(-1000i16).to_le_bytes(), &0i16.to_le_bytes());
        let parsed = parse_resident_raster(&wkb).unwrap();
        let band = &parsed.bands[0];
        assert_eq!(band.pixel_type, PixelType::Int16);
        assert_eq!(band.pixels, (-1000_i16).to_le_bytes());
        assert_eq!(band.nodata, 0.0);
    }

    #[test]
    fn hand_built_32bf_decodes() {
        // code 10 = 32BF, four LE bytes, value 3.5.
        let wkb = hand_build_wkb(10, &3.5f32.to_le_bytes(), &0.0f32.to_le_bytes());
        let parsed = parse_resident_raster(&wkb).unwrap();
        let band = &parsed.bands[0];
        assert_eq!(band.pixel_type, PixelType::Float32);
        assert_eq!(band.pixels, 3.5_f32.to_le_bytes());
        assert_eq!(band.nodata, 0.0);
    }

    #[test]
    fn hand_built_64bf_decodes() {
        // code 11 = 64BF, eight LE bytes.
        let wkb = hand_build_wkb(11, &2.5f64.to_le_bytes(), &0.0f64.to_le_bytes());
        let parsed = parse_resident_raster(&wkb).unwrap();
        let band = &parsed.bands[0];
        assert_eq!(band.pixel_type, PixelType::Float64);
        assert_eq!(band.pixels, 2.5_f64.to_le_bytes());
        assert_eq!(band.nodata, 0.0);
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
#[allow(clippy::wildcard_imports)]
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
        let flags: u8 = pix_code | 0x40; // low nibble = pixtype, 0x40 = HASNODATA
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
            PixelType::Float32 => 10,
            PixelType::Float64 => 11,
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
#[allow(clippy::unwrap_used, clippy::wildcard_imports)]
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
    fn pixel_type_from_code_invalid_returns_none() {
        // Code 9 is skipped and 12+ are out of range for PostGIS rt_pixtype.
        assert!(PixelType::from_code(9).is_none());
        assert!(PixelType::from_code(12).is_none());
        assert!(PixelType::from_code(255).is_none());
    }

    #[test]
    fn pixel_type_from_code_all_valid() {
        // PostGIS rt_pixtype (librtcore.h): 0=1BB, 1=2BUI, 2=4BUI, 3=8BSI,
        // 4=8BUI, 5=16BSI, 6=16BUI, 7=32BSI, 8=32BUI, 10=32BF, 11=64BF (9 skipped).
        assert_eq!(PixelType::from_code(0), Some(PixelType::Bool));
        assert_eq!(PixelType::from_code(1), Some(PixelType::UInt2));
        assert_eq!(PixelType::from_code(2), Some(PixelType::UInt4));
        assert_eq!(PixelType::from_code(3), Some(PixelType::Int8));
        assert_eq!(PixelType::from_code(4), Some(PixelType::UInt8));
        assert_eq!(PixelType::from_code(5), Some(PixelType::Int16));
        assert_eq!(PixelType::from_code(6), Some(PixelType::UInt16));
        assert_eq!(PixelType::from_code(7), Some(PixelType::Int32));
        assert_eq!(PixelType::from_code(8), Some(PixelType::UInt32));
        assert_eq!(PixelType::from_code(10), Some(PixelType::Float32));
        assert_eq!(PixelType::from_code(11), Some(PixelType::Float64));
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
            PixelType::Float32 => 10,
            PixelType::Float64 => 11,
        };

        let pixel_count = width as usize * height as usize;
        for _ in 0..num_bands {
            let flags: u8 = pix_code | 0x40; // low nibble = pixtype, 0x40 = HASNODATA
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
    fn resident_parser_accepts_every_postgis_pixel_tag() {
        let types = [
            PixelType::Bool,
            PixelType::UInt2,
            PixelType::UInt4,
            PixelType::Int8,
            PixelType::UInt8,
            PixelType::Int16,
            PixelType::UInt16,
            PixelType::Int32,
            PixelType::UInt32,
            PixelType::Float32,
            PixelType::Float64,
        ];
        for pixel_type in types {
            let wkb = build_test_raster(2, 1, 4_326, pixel_type, 0.0, 1.0);
            let parsed = parse_resident_raster(&wkb).expect("valid resident raster");
            assert_eq!(parsed.header.version, 0);
            assert!(parsed.header.little_endian);
            assert_eq!(parsed.bands.len(), 1);
            assert_eq!(parsed.bands[0].pixel_type.code(), pixel_type.code());
            assert_eq!(parsed.bands[0].pixels.len(), 2 * pixel_type.byte_size());
        }
    }

    #[test]
    fn resident_parser_preserves_multiband_metadata_and_nan_nodata() {
        let mut wkb = build_multiband_raster(1, 1, 2, PixelType::Float64, f64::NAN, 7.0);
        let second_band_offset = HEADER_SIZE + 1 + 8 + 8;
        wkb[second_band_offset] |= 0x20;
        let parsed = parse_resident_raster(&wkb).expect("valid multi-band raster");
        assert_eq!(parsed.bands.len(), 2);
        assert!(parsed.bands[0].nodata.is_nan());
        assert!(!parsed.bands[0].is_nodata);
        assert!(parsed.bands[1].is_nodata);
        assert_eq!(parsed.bands[1].pixels, 7.0_f64.to_le_bytes());
    }

    #[test]
    fn resident_parser_normalizes_big_endian_pixels() {
        let mut wkb = Vec::new();
        wkb.push(0);
        wkb.extend_from_slice(&0_u16.to_be_bytes());
        wkb.extend_from_slice(&1_u16.to_be_bytes());
        for value in [1.0_f64, -1.0, 10.0, 20.0, 0.25, -0.5] {
            wkb.extend_from_slice(&value.to_be_bytes());
        }
        wkb.extend_from_slice(&4_326_i32.to_be_bytes());
        wkb.extend_from_slice(&2_u16.to_be_bytes());
        wkb.extend_from_slice(&1_u16.to_be_bytes());
        wkb.push(6 | 0x40);
        wkb.extend_from_slice(&65_535_u16.to_be_bytes());
        wkb.extend_from_slice(&1_u16.to_be_bytes());
        wkb.extend_from_slice(&50_000_u16.to_be_bytes());

        let parsed = parse_resident_raster(&wkb).expect("valid big-endian raster");
        assert!(!parsed.header.little_endian);
        assert_eq!(parsed.header.skew_x, 0.25);
        assert_eq!(parsed.header.skew_y, -0.5);
        assert_eq!(parsed.bands[0].nodata, 65_535.0);
        assert_eq!(
            parsed.bands[0].pixels,
            [1_u16.to_le_bytes(), 50_000_u16.to_le_bytes()].concat()
        );
    }

    #[test]
    fn resident_parser_accepts_empty_rasters() {
        let wkb = build_test_raster(0, 0, 0, PixelType::UInt8, 0.0, 0.0);
        let parsed = parse_resident_raster(&wkb).expect("empty raster remains a value");
        assert_eq!(parsed.bands.len(), 1);
        assert!(parsed.bands[0].pixels.is_empty());

        let mut no_bands = wkb;
        no_bands[3..5].copy_from_slice(&0_u16.to_le_bytes());
        no_bands.truncate(HEADER_SIZE);
        assert!(
            parse_resident_raster(&no_bands)
                .expect("zero-band raster remains a value")
                .bands
                .is_empty()
        );
    }

    #[test]
    fn resident_parser_returns_stable_structural_declines() {
        let mut offline = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 1.0);
        offline[HEADER_SIZE] |= 0x80;
        assert_eq!(
            parse_resident_raster(&offline),
            Err(ResidentRasterParseError::OfflineBand)
        );

        let mut reserved_flag = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 1.0);
        reserved_flag[HEADER_SIZE] |= 0x10;
        assert_eq!(
            parse_resident_raster(&reserved_flag),
            Err(ResidentRasterParseError::InvalidBandFlags)
        );

        let mut bad_version = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 1.0);
        bad_version[1..3].copy_from_slice(&1_u16.to_le_bytes());
        assert_eq!(
            parse_resident_raster(&bad_version),
            Err(ResidentRasterParseError::UnsupportedVersion)
        );

        let mut trailing = build_test_raster(1, 1, 0, PixelType::UInt8, 0.0, 1.0);
        trailing.push(0);
        assert_eq!(
            parse_resident_raster(&trailing),
            Err(ResidentRasterParseError::TrailingBytes)
        );
    }
}

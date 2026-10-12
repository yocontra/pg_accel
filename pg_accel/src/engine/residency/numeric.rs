//! Lossless NUMERIC transport. PostgreSQL's numeric_send format is decoded
//! into native-endian base-10000 digits, never floating point or a fixed
//! decimal precision. This is input preparation, not host-side aggregation.

use crate::engine::spec::abi::PgaccelNumericView;
use crate::gpu::ExprDeviceBuffer;

pub const NUMERIC_POS: u16 = 0x0000;
pub const NUMERIC_NEG: u16 = 0x4000;
pub const NUMERIC_NAN: u16 = 0xc000;
pub const NUMERIC_PINF: u16 = 0xd000;
pub const NUMERIC_NINF: u16 = 0xf000;

/// Each digit contributes digit * 10000^(weight - digit_index). Offset is
/// measured in u16 digits, not bytes. NULL is a separate byte sidecar and
/// must not be confused with zero, NaN, or either infinity. No implicit pad.
pub type ResidentNumericRow = crate::engine::spec::abi::PgaccelNumericRow;

const _: () = assert!(std::mem::size_of::<ResidentNumericRow>() == 16);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResidentNumericData {
    pub rows: Vec<ResidentNumericRow>,
    pub digits: Vec<u16>,
    pub nulls: Option<Vec<u8>>,
}

fn validate_header(row: ResidentNumericRow) -> Result<(), String> {
    if row.dscale > 0x3fff {
        return Err("NUMERIC display scale exceeds PostgreSQL's stored limit".to_owned());
    }
    match row.sign {
        NUMERIC_POS | NUMERIC_NEG => Ok(()),
        // numeric_send preserves header-derived dscale even for specials:
        // PostgreSQL 18/19 emit 32 for infinity. Retain these bits exactly;
        // they have no finite-value scale meaning for aggregation.
        NUMERIC_NAN | NUMERIC_PINF | NUMERIC_NINF if row.ndigits == 0 && row.weight == 0 => Ok(()),
        _ => Err("invalid NUMERIC sign or special-value payload".to_owned()),
    }
}

impl ResidentNumericData {
    pub fn validate(&self) -> Result<(), String> {
        if self.nulls.as_ref().is_some_and(|nulls| {
            nulls.len() != self.rows.len() || nulls.iter().any(|null| *null > 1)
        }) {
            return Err("invalid NUMERIC NULL sidecar".to_owned());
        }
        let mut end = 0_u64;
        for (index, row) in self.rows.iter().copied().enumerate() {
            if row.digit_offset != end {
                return Err("NUMERIC digit offsets are not contiguous".to_owned());
            }
            validate_header(row)?;
            if self.nulls.as_ref().is_some_and(|nulls| nulls[index] != 0)
                && row
                    != (ResidentNumericRow {
                        digit_offset: end,
                        ..ResidentNumericRow::default()
                    })
            {
                return Err("NUMERIC NULL row has a value payload".to_owned());
            }
            end = end
                .checked_add(u64::from(row.ndigits))
                .ok_or("NUMERIC digit count overflow")?;
        }
        if usize::try_from(end).ok() != Some(self.digits.len())
            || self.digits.iter().any(|digit| *digit >= 10000)
        {
            return Err("invalid NUMERIC digit payload".to_owned());
        }
        Ok(())
    }

    pub fn device_bytes(&self) -> Option<u64> {
        self.rows
            .len()
            .checked_mul(std::mem::size_of::<ResidentNumericRow>())?
            .checked_add(self.digits.len().checked_mul(std::mem::size_of::<u16>())?)?
            .checked_add(self.nulls.as_ref().map_or(0, Vec::len))
            .and_then(|bytes| u64::try_from(bytes).ok())
    }

    /// Exact inverse of input decoding, also used by live GPU-buffer readback
    /// tests. Preserves display scale and special values, not just equality.
    pub fn wire_value(&self, index: usize) -> Result<Option<Vec<u8>>, String> {
        let row = *self
            .rows
            .get(index)
            .ok_or("NUMERIC row index out of range")?;
        validate_header(row)?;
        if let Some(nulls) = &self.nulls {
            match nulls.get(index) {
                Some(1) => return Ok(None),
                Some(0) => {}
                _ => return Err("invalid NUMERIC NULL sidecar".to_owned()),
            }
        }
        let start =
            usize::try_from(row.digit_offset).map_err(|_| "NUMERIC offset exceeds usize")?;
        let end = start
            .checked_add(usize::from(row.ndigits))
            .ok_or("NUMERIC end overflow")?;
        let digits = self
            .digits
            .get(start..end)
            .ok_or("NUMERIC digits out of range")?;
        let mut bytes = Vec::with_capacity(8 + digits.len() * 2);
        bytes.extend(row.ndigits.to_be_bytes());
        bytes.extend(row.weight.to_be_bytes());
        bytes.extend(row.sign.to_be_bytes());
        bytes.extend(row.dscale.to_be_bytes());
        for digit in digits {
            if *digit >= 10000 {
                return Err("invalid NUMERIC digit".to_owned());
            }
            bytes.extend(digit.to_be_bytes());
        }
        Ok(Some(bytes))
    }
}

#[derive(Default)]
pub(super) struct NumericColumnBuilder {
    data: ResidentNumericData,
    nulls: Vec<u8>,
    saw_null: bool,
}

impl NumericColumnBuilder {
    pub fn try_reserve_rows(&mut self, additional: usize) -> Result<(), String> {
        self.data
            .rows
            .try_reserve(additional)
            .map_err(|error| error.to_string())?;
        super::nulls::reserve(
            &mut self.nulls,
            self.saw_null,
            self.data.rows.len(),
            additional,
        )
    }

    /// Invalid input leaves the logical column unchanged. Capacity may grow,
    /// but no row, digit, or NULL marker is published until validation ends.
    pub fn push(&mut self, wire: Option<&[u8]>) -> Result<(), String> {
        let mut row = ResidentNumericRow {
            digit_offset: u64::try_from(self.data.digits.len())
                .map_err(|_| "NUMERIC offset overflow")?,
            ..ResidentNumericRow::default()
        };
        if let Some(bytes) = wire {
            if bytes.len() < 8 {
                return Err("truncated NUMERIC binary header".to_owned());
            }
            row.ndigits = u16::from_be_bytes([bytes[0], bytes[1]]);
            row.weight = i16::from_be_bytes([bytes[2], bytes[3]]);
            row.sign = u16::from_be_bytes([bytes[4], bytes[5]]);
            row.dscale = u16::from_be_bytes([bytes[6], bytes[7]]);
            validate_header(row)?;
            if bytes.len() != 8 + usize::from(row.ndigits) * 2
                || bytes[8..]
                    .chunks_exact(2)
                    .any(|digit| u16::from_be_bytes([digit[0], digit[1]]) >= 10000)
            {
                return Err("invalid NUMERIC binary digit payload".to_owned());
            }
        }
        self.try_reserve_rows(1)?;
        self.data
            .digits
            .try_reserve(usize::from(row.ndigits))
            .map_err(|error| error.to_string())?;
        if let Some(bytes) = wire {
            self.data.digits.extend(
                bytes[8..]
                    .chunks_exact(2)
                    .map(|digit| u16::from_be_bytes([digit[0], digit[1]])),
            );
        }
        self.data.rows.push(row);
        self.nulls.push(u8::from(wire.is_none()));
        self.saw_null |= wire.is_none();
        Ok(())
    }

    pub fn finish(mut self) -> Result<ResidentNumericData, String> {
        self.data.nulls = super::nulls::finish(self.nulls, self.saw_null, self.data.rows.len())?;
        self.data.validate()?;
        Ok(self.data)
    }
}

pub struct ResidentNumericColumn {
    rows: ExprDeviceBuffer<ResidentNumericRow>,
    digits: Option<ExprDeviceBuffer<u16>>,
    nulls: Option<ExprDeviceBuffer<u8>>,
    device_bytes: u64,
    bounds: NumericDigitBounds,
}

/// Input preparation metadata only: the digit arena extent used to budget an
/// exact accumulator. It is computed before upload, never from GPU readback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NumericDigitBounds {
    pub min_weight: i32,
    pub max_weight: i32,
    pub max_ndigits: u32,
}

impl NumericDigitBounds {
    /// Exact fixed SUM stride, including the native header, four carry limbs
    /// and eight-byte alignment. Invalid/unbounded metadata is never costable.
    #[must_use]
    pub fn sum_state_bytes(self) -> Option<u32> {
        if self.min_weight < -4096
            || self.max_weight > 32767
            || self.min_weight > self.max_weight
            || self.max_ndigits > u32::from(u16::MAX)
        {
            return None;
        }
        Some(16 + (2 * (self.max_weight - self.min_weight + 5) as u32).next_multiple_of(8))
    }

    fn from_rows(rows: &[ResidentNumericRow]) -> Self {
        let mut bounds = Self::default();
        let mut first = true;
        for row in rows.iter().filter(|row| row.ndigits != 0) {
            let high = i32::from(row.weight);
            let low = high - i32::from(row.ndigits) + 1;
            bounds.min_weight = if first {
                low
            } else {
                bounds.min_weight.min(low)
            };
            bounds.max_weight = if first {
                high
            } else {
                bounds.max_weight.max(high)
            };
            bounds.max_ndigits = bounds.max_ndigits.max(u32::from(row.ndigits));
            first = false;
        }
        bounds
    }
}

#[derive(Clone, Copy)]
pub struct ResidentNumericColumnView<'a> {
    pub rows: &'a ExprDeviceBuffer<ResidentNumericRow>,
    pub digits: Option<&'a ExprDeviceBuffer<u16>>,
    pub nulls: Option<&'a ExprDeviceBuffer<u8>>,
    pub bounds: NumericDigitBounds,
}

impl ResidentNumericColumnView<'_> {
    /// Borrowed ABI metadata, valid only while this resident view is borrowed.
    #[must_use]
    pub fn native_view(&self) -> PgaccelNumericView {
        PgaccelNumericView {
            rows: self.rows.as_ptr(),
            digits: self
                .digits
                .map_or(std::ptr::null(), ExprDeviceBuffer::as_ptr),
            digit_count: self.digits.map_or(0, |digits| digits.len() as u64),
            row_count: self.rows.len() as u64,
            min_weight: self.bounds.min_weight,
            max_weight: self.bounds.max_weight,
            max_ndigits: self.bounds.max_ndigits,
            reserved: 0,
        }
    }
}

impl ResidentNumericColumn {
    pub(super) fn materialize(data: &ResidentNumericData) -> Result<Self, String> {
        data.validate()?;
        let device_bytes = data
            .device_bytes()
            .ok_or("NUMERIC resident byte count overflow")?;
        let rows = ExprDeviceBuffer::copy_from_slice(&data.rows)
            .ok_or("NUMERIC row allocation/copy failed")?;
        let digits = if data.digits.is_empty() {
            None
        } else {
            Some(
                ExprDeviceBuffer::copy_from_slice(&data.digits)
                    .ok_or("NUMERIC digit allocation/copy failed")?,
            )
        };
        let nulls = data
            .nulls
            .as_ref()
            .map(|nulls| {
                ExprDeviceBuffer::copy_from_slice(nulls)
                    .ok_or("NUMERIC NULL allocation/copy failed")
            })
            .transpose()?;
        Ok(Self {
            rows,
            digits,
            nulls,
            device_bytes,
            bounds: NumericDigitBounds::from_rows(&data.rows),
        })
    }

    #[must_use]
    pub fn device_bytes(&self) -> u64 {
        self.device_bytes
    }

    #[must_use]
    pub fn view(&self) -> ResidentNumericColumnView<'_> {
        ResidentNumericColumnView {
            rows: &self.rows,
            digits: self.digits.as_ref(),
            nulls: self.nulls.as_ref(),
            bounds: self.bounds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_sum_stride_is_extent_based_and_checked() {
        assert_eq!(NumericDigitBounds::default().sum_state_bytes(), Some(32));
        let wide = NumericDigitBounds {
            min_weight: -4096,
            max_weight: 32767,
            max_ndigits: 36864,
        };
        assert_eq!(wide.sum_state_bytes(), Some(73752));
        for invalid in [
            NumericDigitBounds {
                min_weight: -4097,
                ..wide
            },
            NumericDigitBounds {
                max_weight: 32768,
                ..wide
            },
            NumericDigitBounds {
                min_weight: 32768,
                ..wide
            },
            NumericDigitBounds {
                max_ndigits: 65536,
                ..wide
            },
        ] {
            assert_eq!(invalid.sum_state_bytes(), None);
        }
    }

    #[test]
    fn digit_bounds_cover_extents_without_imposing_zero_or_scale() {
        let mut builder = NumericColumnBuilder::default();
        builder.push(None).expect("NULL fixture");
        builder
            .push(Some(&wire(0, NUMERIC_POS, 16383, &[])))
            .expect("zero fixture");
        builder
            .push(Some(&wire(0, NUMERIC_PINF, 32, &[])))
            .expect("infinity fixture");
        let mut data = builder.finish().expect("valid fixture");
        assert_eq!(
            NumericDigitBounds::from_rows(&data.rows),
            NumericDigitBounds::default()
        );
        data.rows.extend([
            ResidentNumericRow {
                weight: -8,
                ndigits: 3,
                ..ResidentNumericRow::default()
            },
            ResidentNumericRow {
                weight: -2,
                ndigits: 1,
                ..ResidentNumericRow::default()
            },
        ]);
        assert_eq!(
            NumericDigitBounds::from_rows(&data.rows),
            NumericDigitBounds {
                min_weight: -10,
                max_weight: -2,
                max_ndigits: 3,
            }
        );
        data.rows.push(ResidentNumericRow {
            weight: 32767,
            ndigits: 36864,
            ..ResidentNumericRow::default()
        });
        assert_eq!(
            NumericDigitBounds::from_rows(&data.rows),
            NumericDigitBounds {
                min_weight: -4096,
                max_weight: 32767,
                max_ndigits: 36864,
            }
        );
    }

    fn wire(weight: i16, sign: u16, scale: u16, digits: &[u16]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(
            u16::try_from(digits.len())
                .expect("valid NUMERIC test fixture")
                .to_be_bytes(),
        );
        bytes.extend(weight.to_be_bytes());
        bytes.extend(sign.to_be_bytes());
        bytes.extend(scale.to_be_bytes());
        for digit in digits {
            bytes.extend(digit.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn exact_wire_roundtrip_including_scale_extremes_and_specials() {
        let cases = [
            None,
            Some(wire(0, NUMERIC_POS, 4, &[])),
            Some(wire(0, NUMERIC_NEG, 4, &[12, 3400])),
            Some(wire(i16::MAX, NUMERIC_POS, 0, &[9999])),
            Some(wire(-4096, NUMERIC_NEG, 16383, &[10])),
            Some(wire(0, NUMERIC_NAN, 0, &[])),
            Some(wire(0, NUMERIC_PINF, 32, &[])),
            Some(wire(0, NUMERIC_NINF, 32, &[])),
        ];
        let mut builder = NumericColumnBuilder::default();
        for case in &cases {
            builder
                .push(case.as_deref())
                .expect("valid NUMERIC test fixture");
        }
        let data = builder.finish().expect("valid NUMERIC test fixture");
        assert_eq!(data.device_bytes(), Some(16 * 8 + 2 * 4 + 8));
        for (index, expected) in cases.iter().enumerate() {
            assert_eq!(
                data.wire_value(index).expect("valid NUMERIC test fixture"),
                *expected
            );
        }
        assert!(data.wire_value(cases.len()).is_err());
    }

    #[test]
    fn arbitrary_precision_not_limited_to_i128_or_column_scale() {
        let digits = vec![9999; 36864];
        let input = wire(32767, NUMERIC_POS, 16383, &digits);
        let mut builder = NumericColumnBuilder::default();
        builder
            .push(Some(&input))
            .expect("valid NUMERIC test fixture");
        let data = builder.finish().expect("valid NUMERIC test fixture");
        assert_eq!(data.digits, digits);
        assert_eq!(
            data.wire_value(0).expect("valid NUMERIC test fixture"),
            Some(input)
        );
        assert!(data.nulls.is_none());
    }

    #[test]
    fn malformed_binary_never_publishes_partial_row() {
        let good = wire(0, NUMERIC_POS, 2, &[42, 1200]);
        let mut builder = NumericColumnBuilder::default();
        builder
            .push(Some(&good))
            .expect("valid NUMERIC test fixture");
        for malformed in [
            vec![],
            good[..9].to_vec(),
            [good.clone(), vec![0]].concat(),
            wire(0, 7, 0, &[]),
            wire(0, NUMERIC_POS, 16384, &[]),
            wire(0, NUMERIC_POS, 0, &[10000]),
            wire(0, NUMERIC_NAN, 0, &[1]),
            wire(1, NUMERIC_PINF, 0, &[]),
            wire(0, NUMERIC_NINF, 16384, &[]),
        ] {
            assert!(builder.push(Some(&malformed)).is_err());
            assert_eq!(builder.data.rows.len(), 1);
            assert_eq!(builder.data.digits, [42, 1200]);
            assert_eq!(builder.nulls, [0]);
            assert!(builder.nulls.capacity() <= super::super::nulls::STAGING_ROWS);
            assert!(!builder.saw_null);
        }
        let mut data = builder.finish().expect("valid NUMERIC test fixture");
        data.rows[0].digit_offset = u64::MAX;
        assert!(data.validate().is_err());
        assert!(data.wire_value(0).is_err());
        data.rows[0].digit_offset = 0;
        data.nulls = Some(vec![1]);
        assert!(data.validate().is_err());
        data.nulls = Some(vec![2]);
        assert!(data.validate().is_err());
        data.nulls = Some(vec![]);
        assert!(data.validate().is_err());
    }

    #[test]
    fn late_numeric_null_preserves_exact_digits_and_display_scale() {
        let good = wire(0, NUMERIC_NEG, 4, &[42, 1200]);
        let mut builder = NumericColumnBuilder::default();
        builder.try_reserve_rows(8192).expect("reserve rows only");
        for _ in 0..16385 {
            builder.push(Some(&good)).expect("numeric prefix");
        }
        assert!(builder.nulls.capacity() <= super::super::nulls::STAGING_ROWS);
        assert_eq!(builder.nulls, [0]);
        assert!(builder.push(Some(&[0])).is_err());
        builder.push(None).expect("late NULL");
        builder.push(Some(&good)).expect("numeric suffix");
        let data = builder.finish().expect("exact numeric column");
        assert_eq!(data.rows.len(), 16387);
        for row in 0..16387 {
            assert_eq!(
                data.wire_value(row).expect("wire roundtrip"),
                (row != 16385).then(|| good.clone())
            );
        }
        assert_eq!(data.digits.len(), 16386 * 2);
    }
}

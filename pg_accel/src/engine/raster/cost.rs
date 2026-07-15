//! Pure work accounting and DeviceLimits gates for childless raster plans.

use crate::engine::cost::{DeviceLimits, PgCost};

/// Exact host metadata retained with one resident raster column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterResidentWork {
    pub row_count: u64,
    pub non_null_rows: u64,
    pub zero_grid_present_band_rows: u64,
    pub selected_band_rows: u64,
    pub selected_pixels: u64,
    pub input_wkb_bytes: u64,
    /// Exact native band-one bytes copied back from the device.
    pub reclass_output_pixel_bytes: Option<u64>,
    /// Exact reconstructed WKB bytes for the selected reclass output type.
    pub reclass_output_wkb_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterWorkEstimate {
    ResidentExact(RasterResidentWork),
    Unavailable,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterCostInput {
    pub work: RasterWorkEstimate,
    pub native_total_cost: PgCost,
}

/// Every Phase 6 variant is a decline. `Eligible` is intentionally absent:
/// no raster path can become selectable until Phase 7 supplies validated,
/// fingerprinted coefficients through the typed cost model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterCostGate {
    ExactResidentMetadataUnavailable,
    ResidentMetadataOverflow,
    InvalidResidentMetadata,
    NonRoundtrippableZeroGridBand {
        rows: u64,
    },
    SelectedBandMissing {
        present_rows: u64,
        required_rows: u64,
    },
    ReclassOutputBytesUnavailable,
    PixelsBelowDeviceMinimum {
        estimated: u64,
        required: u64,
    },
    InvalidNativeCost,
    UncalibratedCoefficients,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterCost {
    pub work: Option<RasterResidentWork>,
    pub launches: u64,
    /// Exact native band-one bytes returned from device work.
    pub device_output_bytes: Option<u64>,
    /// Exact full WKB bytes reconstructed on the host.
    pub wkb_construction_bytes: Option<u64>,
    /// No numeric total is published before calibration.
    pub total: Option<PgCost>,
    pub native_total: PgCost,
    pub gate: RasterCostGate,
}

fn declined(input: RasterCostInput, gate: RasterCostGate) -> RasterCost {
    RasterCost {
        work: None,
        launches: 0,
        device_output_bytes: None,
        wkb_construction_bytes: None,
        total: None,
        native_total: input.native_total_cost,
        gate,
    }
}

/// Account for exact resident work without inventing pixel counts or
/// publishing uncalibrated PostgreSQL cost units.
#[must_use]
pub fn estimate_raster_cost(input: RasterCostInput, limits: &DeviceLimits) -> RasterCost {
    let work = match input.work {
        RasterWorkEstimate::ResidentExact(work) => work,
        RasterWorkEstimate::Unavailable => {
            return declined(input, RasterCostGate::ExactResidentMetadataUnavailable);
        }
        RasterWorkEstimate::Overflow => {
            return declined(input, RasterCostGate::ResidentMetadataOverflow);
        }
    };
    if work.non_null_rows > work.row_count
        || work.zero_grid_present_band_rows > work.selected_band_rows
        || work.selected_band_rows > work.non_null_rows
        || work.selected_band_rows == 0 && work.selected_pixels != 0
    {
        return declined(input, RasterCostGate::InvalidResidentMetadata);
    }
    if work.zero_grid_present_band_rows != 0 {
        return declined(
            input,
            RasterCostGate::NonRoundtrippableZeroGridBand {
                rows: work.zero_grid_present_band_rows,
            },
        );
    }
    if work.selected_band_rows != work.non_null_rows {
        return declined(
            input,
            RasterCostGate::SelectedBandMissing {
                present_rows: work.selected_band_rows,
                required_rows: work.non_null_rows,
            },
        );
    }
    let (Some(device_output_bytes), Some(wkb_construction_bytes)) = (
        work.reclass_output_pixel_bytes,
        work.reclass_output_wkb_bytes,
    ) else {
        return declined(input, RasterCostGate::ReclassOutputBytesUnavailable);
    };
    let minimum = u64::try_from(limits.gpu_raster_min_pixels).unwrap_or(u64::MAX);
    if work.selected_pixels < minimum {
        return declined(
            input,
            RasterCostGate::PixelsBelowDeviceMinimum {
                estimated: work.selected_pixels,
                required: minimum,
            },
        );
    }
    let native = input.native_total_cost.get();
    if !native.is_finite() || native <= 0.0 {
        return declined(input, RasterCostGate::InvalidNativeCost);
    }
    let chunk_pixels = u64::try_from(limits.gpu_raster_max_chunk_pixels).unwrap_or(u64::MAX);
    let launches = if chunk_pixels == 0 {
        0
    } else {
        work.selected_pixels.div_ceil(chunk_pixels)
    };
    RasterCost {
        work: Some(work),
        launches,
        device_output_bytes: Some(device_output_bytes),
        wkb_construction_bytes: Some(wkb_construction_bytes),
        total: None,
        native_total: input.native_total_cost,
        gate: RasterCostGate::UncalibratedCoefficients,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(pixels: u64) -> RasterResidentWork {
        RasterResidentWork {
            row_count: 1_000,
            non_null_rows: 999,
            zero_grid_present_band_rows: 0,
            selected_band_rows: 999,
            selected_pixels: pixels,
            input_wkb_bytes: 1_000_000,
            reclass_output_pixel_bytes: Some(500_000),
            reclass_output_wkb_bytes: Some(750_000),
        }
    }

    fn input(work: RasterWorkEstimate, native: f64) -> RasterCostInput {
        RasterCostInput {
            work,
            native_total_cost: PgCost::new(native),
        }
    }

    #[test]
    fn exact_resident_metadata_is_required() {
        let cost = estimate_raster_cost(
            input(RasterWorkEstimate::Unavailable, 1_000_000.0),
            &DeviceLimits::cpu_only(),
        );
        assert_eq!(cost.gate, RasterCostGate::ExactResidentMetadataUnavailable);
        assert_eq!(cost.total, None);
    }

    #[test]
    fn overflow_and_missing_selected_bands_decline() {
        let limits = DeviceLimits::cpu_only();
        assert_eq!(
            estimate_raster_cost(input(RasterWorkEstimate::Overflow, 1_000_000.0), &limits,).gate,
            RasterCostGate::ResidentMetadataOverflow
        );

        let mut missing = work(u64::try_from(limits.gpu_raster_min_pixels).expect("limit fits"));
        missing.selected_band_rows -= 1;
        assert_eq!(
            estimate_raster_cost(
                input(RasterWorkEstimate::ResidentExact(missing), 1_000_000.0),
                &limits,
            )
            .gate,
            RasterCostGate::SelectedBandMissing {
                present_rows: 998,
                required_rows: 999,
            }
        );
    }

    #[test]
    fn exact_reclass_output_transfer_bytes_are_retained() {
        let limits = DeviceLimits::cpu_only();
        let pixels = u64::try_from(limits.gpu_raster_min_pixels).expect("limit fits");
        let resident = work(pixels);
        let cost = estimate_raster_cost(
            input(RasterWorkEstimate::ResidentExact(resident), 1_000_000.0),
            &limits,
        );
        assert_eq!(
            cost.device_output_bytes,
            resident.reclass_output_pixel_bytes
        );
        assert_eq!(
            cost.wkb_construction_bytes,
            resident.reclass_output_wkb_bytes
        );
    }

    #[test]
    fn zero_grid_present_band_is_not_roundtrippable() {
        let limits = DeviceLimits::cpu_only();
        let mut resident =
            work(u64::try_from(limits.gpu_raster_min_pixels).expect("device minimum fits u64"));
        resident.zero_grid_present_band_rows = 1;
        assert_eq!(
            estimate_raster_cost(
                input(RasterWorkEstimate::ResidentExact(resident), 1_000_000.0),
                &limits,
            )
            .gate,
            RasterCostGate::NonRoundtrippableZeroGridBand { rows: 1 }
        );
    }

    #[test]
    fn marginal_native_cost_and_uncalibrated_coefficients_cannot_admit() {
        let limits = DeviceLimits::cpu_only();
        let pixels = u64::try_from(limits.gpu_raster_min_pixels).expect("limit fits");
        for native in [f64::MIN_POSITIVE, 0.001, 1_000_000.0] {
            let cost = estimate_raster_cost(
                input(RasterWorkEstimate::ResidentExact(work(pixels)), native),
                &limits,
            );
            assert_eq!(cost.gate, RasterCostGate::UncalibratedCoefficients);
            assert_eq!(cost.total, None);
        }
    }

    #[test]
    fn invalid_native_costs_decline_before_calibration() {
        let limits = DeviceLimits::cpu_only();
        let pixels = u64::try_from(limits.gpu_raster_min_pixels).expect("limit fits");
        for native in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                estimate_raster_cost(
                    input(RasterWorkEstimate::ResidentExact(work(pixels)), native),
                    &limits,
                )
                .gate,
                RasterCostGate::InvalidNativeCost
            );
        }
    }

    #[test]
    fn chunking_is_exact_but_gate_remains_dark() {
        let limits = DeviceLimits::cpu_only();
        let chunk = u64::try_from(limits.gpu_raster_max_chunk_pixels).expect("limit fits");
        let cost = estimate_raster_cost(
            input(
                RasterWorkEstimate::ResidentExact(work(chunk * 2 + 1)),
                1_000_000.0,
            ),
            &limits,
        );
        assert_eq!(cost.launches, 3);
        assert_eq!(cost.gate, RasterCostGate::UncalibratedCoefficients);
    }
}

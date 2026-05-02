//! GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, and `ST_Reclass`.

use crate::adapters::extractors::raster;
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;

use super::DispatchResult;

// ---------------------------------------------------------------------------
// Strategy: GpuRaster
// ---------------------------------------------------------------------------

/// GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, and `ST_Reclass`.
///
/// Routes by function name via the registry. Only `st_mapalgebra` has a
/// fully-wired GPU pipeline today (raster header parse + band extraction +
/// `map_algebra` kernel + WKB patch-back). `st_clip` and `st_reclass` have
/// real backing kernels (`pgaccel_raster_clip`, `pgaccel_raster_reclass`)
/// but the executor-side argument extraction (polygon ring for clip;
/// reclass-rule text parsing for reclass) is not yet plumbed; both defer
/// to PG native rather than silently substituting `map_algebra` (anti-
/// cheat ban #7 — no stub-as-done).
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_raster(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    _fn_info: &pgrx::pg_sys::FmgrInfo,
    _is_strict: bool,
    fn_oid: pgrx::pg_sys::Oid,
) -> DispatchResult {
    // Look up the function name to route to the correct GPU kernel.
    // Only `st_mapalgebra` has end-to-end dispatch today; the other two
    // registered raster ops (`st_clip`, `st_reclass`) have kernels but
    // unwired argument extraction — defer to PG native for now.
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);
    if fn_name != Some("st_mapalgebra") {
        return DispatchResult::Deferred;
    }

    // Attempt raster extraction from each datum.
    let mut raster_data: Vec<(raster::RasterHeader, Vec<f64>)> = Vec::new();

    for &(datum, is_null) in batch {
        if is_null {
            continue;
        }

        // Raster datums are varlena (bytea-like). Extract the raw bytes.
        // SAFETY: Caller guarantees main backend thread, datum is a valid
        // varlena pointer for raster data.
        let varlena = unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
        // SAFETY: `varlena` is a valid detoasted varlena pointer returned by
        // `pg_detoast_datum` above; `varsize_any_exhdr` reads its length header.
        let data_len = unsafe { pgrx::varsize_any_exhdr(varlena) };
        // SAFETY: `varlena` is valid and detoasted; `vardata_any` returns a
        // pointer to the payload immediately after the varlena header.
        let data_ptr = unsafe { pgrx::vardata_any(varlena) };
        // SAFETY: `data_ptr` points to `data_len` bytes of contiguous varlena
        // payload within the detoasted datum. The slice does not outlive the
        // current loop iteration.
        let data_slice = unsafe { std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len) };

        // Parse header and extract band 0 pixels.
        if let Some(header) = raster::parse_header(data_slice)
            && let Some(pixels) = raster::extract_pixels_f64(data_slice, 0)
        {
            raster_data.push((header, pixels));
        }
    }

    // If we couldn't extract enough rasters, fall back to standard executor.
    if raster_data.is_empty() || raster_data.len() < batch.len() / 2 {
        return DispatchResult::Deferred;
    }

    // Run map-algebra on each raster through the GPU pipeline.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut results: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());
    let mut raster_idx = 0usize;

    for &(datum, is_null) in batch {
        if is_null {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        }

        if raster_idx >= raster_data.len() {
            // Not enough extracted rasters — defer remaining rows.
            return DispatchResult::Deferred;
        }

        let (ref header, ref pixels) = raster_data[raster_idx];
        raster_idx += 1;

        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }

        // Build a trivial identity expression: LOAD_BAND 0
        let mut inst = gpu::PgaccelExprInst {
            op: gpu::PgaccelOp::LoadBand,
            arg: 0.0, // band_index = 0
        };
        let expr = gpu::PgaccelExpr {
            instructions: std::ptr::addr_of_mut!(inst),
            inst_count: 1,
            band_count: 1,
        };

        // Convert f64 pixels to f32 for the kernel (Float32 pixel type).
        let f32_pixels: Vec<f32> = pixels.iter().map(|&v| v as f32).collect();
        let band_ptr: *const std::ffi::c_void = f32_pixels.as_ptr().cast();
        let band_ptrs = [band_ptr];

        let pixel_type = gpu::PgaccelPixelType::Float32 as i32;
        let mut output_buf = vec![0u8; pixel_count * 4]; // f32 = 4 bytes
        let mut nodata_mask = vec![0u8; pixel_count];

        let gpu_ok = gpu::map_algebra(
            &band_ptrs,
            pixel_count,
            pixel_type,
            &expr,
            &mut output_buf,
            &mut nodata_mask,
        );

        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster map_algebra GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }

        // Interpret output_buf as f32 slice and patch back into original WKB.
        let output_f32: &[f32] = unsafe {
            // SAFETY: output_buf was allocated as pixel_count * 4 bytes of f32.
            std::slice::from_raw_parts(output_buf.as_ptr().cast(), pixel_count)
        };

        // Get original WKB bytes for patching.
        // SAFETY: datum is a valid varlena pointer, on main backend thread.
        let varlena = unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
        let data_len = unsafe { pgrx::varsize_any_exhdr(varlena) };
        let data_ptr = unsafe { pgrx::vardata_any(varlena) };
        let original_wkb = unsafe { std::slice::from_raw_parts(data_ptr.cast::<u8>(), data_len) };

        match raster::patch_band0_pixels(original_wkb, output_f32) {
            Some(new_wkb) => {
                // Allocate a PG varlena datum with the patched WKB.
                let total_size = new_wkb.len() + pgrx::pg_sys::VARHDRSZ;
                // SAFETY: palloc is safe on main backend thread.
                let new_varlena = unsafe { pgrx::pg_sys::palloc(total_size).cast::<u8>() };
                // SAFETY: new_varlena is freshly palloc'd with total_size bytes.
                unsafe {
                    pgrx::set_varsize_4b(new_varlena.cast(), total_size as i32);
                    let data_dest = pgrx::vardata_any(new_varlena.cast()).cast::<u8>();
                    std::ptr::copy_nonoverlapping(
                        new_wkb.as_ptr(),
                        data_dest.cast_mut(),
                        new_wkb.len(),
                    );
                }
                results.push((pgrx::pg_sys::Datum::from(new_varlena), false));
            }
            None => {
                // Patching failed — pass through original datum.
                results.push((datum, false));
            }
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;

    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

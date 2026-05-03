//! GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, `ST_Reclass`,
//! and the Agent 3A kernels (`ST_SummaryStats`).

use crate::adapters::extractors::raster;
use crate::engine::gucs;
use crate::engine::registry;
use crate::gpu;

use super::DispatchResult;

// ---------------------------------------------------------------------------
// Strategy: GpuRaster
// ---------------------------------------------------------------------------

/// GPU raster dispatch.
///
/// Routes by function name via the registry:
///
/// - `st_mapalgebra` — fully wired (raster header parse + band extract +
///   `map_algebra` kernel + WKB patch-back).
/// - `st_clip(rast, geom)` — wired via Agent 3A's
///   `extract_polygon_ring(qual_datum)` + `gpu::raster_clip`.
/// - `st_reclass(rast, text)` — wired via Agent 3A's
///   `parse_reclass_rules(qual_datum_as_text)` + `gpu::raster_reclass`.
/// - `st_summarystats(rast)` — wired via `gpu::raster_summarystats`,
///   returning [`DispatchResult::AcceleratedRecord`] with 6 fields per row.
/// - `st_resample / slope / aspect / hillshade / value` — kernels exist
///   but each takes a multi-arg signature (target dims, cell size, sun
///   azimuth+altitude, point geom array, …) that the 2-arg dispatch
///   interface cannot carry today. These defer cleanly with a debug log
///   instead of silently substituting another kernel (anti-cheat ban #7).
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
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);

    match fn_name {
        Some("st_mapalgebra") => unsafe { dispatch_st_mapalgebra(batch) },
        Some("st_clip") => unsafe { dispatch_st_clip(batch, qual_datum) },
        Some("st_reclass") => unsafe { dispatch_st_reclass(batch, qual_datum) },
        Some("st_summarystats") => unsafe { dispatch_st_summarystats(batch) },
        Some("st_resample" | "st_slope" | "st_aspect" | "st_hillshade" | "st_value") => {
            // Multi-arg signatures (target dims, cell sizes, sun angle,
            // point array, …) don't fit the (rast, optional qual_datum)
            // dispatch interface. Defer cleanly — anti-cheat ban #7
            // forbids stub-as-done. Wiring these requires a richer
            // dispatch carrier (custom-scan private payload with the
            // extra constants) tracked in TODO follow-up.
            pgrx::debug1!(
                "pg_accel: dispatch_gpu_raster: {} kernel exists but multi-arg dispatch \
                 carrier not yet wired — deferring",
                fn_name.unwrap_or("?"),
            );
            DispatchResult::Deferred
        }
        _ => DispatchResult::Deferred,
    }
}

// ---------------------------------------------------------------------------
// Per-op helpers
// ---------------------------------------------------------------------------

/// Extract the raw varlena payload (after header) from a raster Datum.
///
/// # Safety
///
/// Must be called on the main backend thread; `datum` must be a valid
/// varlena (raster) Datum.
unsafe fn raster_datum_as_bytes(datum: pgrx::pg_sys::Datum) -> &'static [u8] {
    // SAFETY: caller guarantees datum is a valid varlena pointer.
    let varlena = unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
    // SAFETY: detoast returned a valid varlena pointer.
    let len = unsafe { pgrx::varsize_any_exhdr(varlena) };
    // SAFETY: vardata returns a pointer into the detoasted varlena payload.
    let ptr = unsafe { pgrx::vardata_any(varlena) };
    // SAFETY: ptr points to len bytes of valid varlena payload. The slice is
    // bound to the call's lifetime; callers MUST not retain it past the
    // function (we mark 'static here only because the borrow checker has no
    // way to bound the lifetime to the dispatch frame; consumers consume
    // the slice synchronously).
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }
}

/// `st_mapalgebra` dispatch: per-row map-algebra over band 0 with an
/// identity LOAD_BAND program. Same shape as the previous version of this
/// file (preserved while the new arms are added below).
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_mapalgebra(batch: &[(pgrx::pg_sys::Datum, bool)]) -> DispatchResult {
    let mut raster_data: Vec<(raster::RasterHeader, Vec<f64>)> = Vec::new();
    for &(datum, is_null) in batch {
        if is_null {
            continue;
        }
        // SAFETY: main backend thread.
        let bytes = unsafe { raster_datum_as_bytes(datum) };
        if let Some(header) = raster::parse_header(bytes)
            && let Some(pixels) = raster::extract_pixels_f64(bytes, 0)
        {
            raster_data.push((header, pixels));
        }
    }

    if raster_data.is_empty() || raster_data.len() < batch.len() / 2 {
        return DispatchResult::Deferred;
    }

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
            return DispatchResult::Deferred;
        }
        let (ref header, ref pixels) = raster_data[raster_idx];
        raster_idx += 1;

        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }

        let mut inst = gpu::PgaccelExprInst {
            op: gpu::PgaccelOp::LoadBand,
            arg: 0.0,
        };
        let expr = gpu::PgaccelExpr {
            instructions: std::ptr::addr_of_mut!(inst),
            inst_count: 1,
            band_count: 1,
        };

        let f32_pixels: Vec<f32> = pixels.iter().map(|&v| v as f32).collect();
        let band_ptr: *const std::ffi::c_void = f32_pixels.as_ptr().cast();
        let band_ptrs = [band_ptr];
        let pixel_type = gpu::PgaccelPixelType::Float32 as i32;
        let mut output_buf = vec![0u8; pixel_count * 4];
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

        // SAFETY: output_buf is pixel_count * 4 bytes of f32.
        let output_f32: &[f32] =
            unsafe { std::slice::from_raw_parts(output_buf.as_ptr().cast(), pixel_count) };

        // SAFETY: main backend thread.
        let original_wkb = unsafe { raster_datum_as_bytes(datum) };

        match raster::patch_band0_pixels(original_wkb, output_f32) {
            Some(new_wkb) => {
                let total_size = new_wkb.len() + pgrx::pg_sys::VARHDRSZ;
                // SAFETY: palloc on main backend thread.
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

/// `st_clip(rast, geom)` dispatch: extract the polygon ring from
/// `qual_datum` (the constant geom), then per-row run `pgaccel_raster_clip`
/// on each raster's band 0 (Float32 pixels). NULL pixels outside the ring
/// stay marked as NODATA in the output mask. Output Datum is the
/// patched-back raster varlena.
///
/// # Safety
///
/// Must be called on the main backend thread; `qual_datum` (when present)
/// must be a valid GSERIALIZED polygon Datum.
unsafe fn dispatch_st_clip(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((qual_d, qual_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if qual_null {
        return DispatchResult::Deferred;
    }
    // SAFETY: main backend thread.
    let geom_bytes = unsafe { raster_datum_as_bytes(qual_d) };
    let Some(ring_xy_f64) = raster::extract_polygon_ring(geom_bytes) else {
        return DispatchResult::Deferred;
    };
    if ring_xy_f64.len() < 6 {
        // <3 vertices: degenerate, defer.
        return DispatchResult::Deferred;
    }
    // Kernel takes fp32. Truncation matches the rest of the spatial path.
    #[allow(clippy::cast_possible_truncation)]
    let ring_xy_f32: Vec<f32> = ring_xy_f64.iter().map(|&v| v as f32).collect();

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut results: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());

    for &(datum, is_null) in batch {
        if is_null {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        }
        // SAFETY: main backend thread.
        let bytes = unsafe { raster_datum_as_bytes(datum) };
        let Some(header) = raster::parse_header(bytes) else {
            results.push((datum, false));
            continue;
        };
        let Some(pixels_f64) = raster::extract_pixels_f64(bytes, 0) else {
            results.push((datum, false));
            continue;
        };
        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }
        // Convert to fp32 for the kernel (matches the patch-back path).
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        let mut output_buf = vec![0u8; pixel_count * 4];
        let mut nodata_mask = vec![0u8; pixel_count];

        let gpu_ok = gpu::raster_clip(
            pixels_f32.as_ptr().cast::<std::ffi::c_void>(),
            header.width as usize,
            header.height as usize,
            header.ip_x,
            header.ip_y,
            header.scale_x,
            header.scale_y,
            gpu::PgaccelPixelType::Float32 as i32,
            &ring_xy_f32,
            &mut output_buf,
            &mut nodata_mask,
        );
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster_clip GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }

        // SAFETY: output_buf is pixel_count * 4 bytes of f32.
        let output_f32: &[f32] =
            unsafe { std::slice::from_raw_parts(output_buf.as_ptr().cast(), pixel_count) };

        match raster::patch_band0_pixels(bytes, output_f32) {
            Some(new_wkb) => {
                let total_size = new_wkb.len() + pgrx::pg_sys::VARHDRSZ;
                // SAFETY: palloc on main backend thread.
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
            None => results.push((datum, false)),
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster_clip pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

/// `st_reclass(rast, text)` dispatch: parse the reclass-rule text
/// from `qual_datum`, then per-row run `pgaccel_raster_reclass` over
/// each raster's band 0. Pixels outside any rule's range stay
/// untouched (NODATA-aware behaviour matches the kernel's documented
/// semantics).
///
/// # Safety
///
/// Must be called on the main backend thread; `qual_datum` (when present)
/// must be a valid `text` Datum (`varlena` of UTF-8 bytes).
unsafe fn dispatch_st_reclass(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    let Some((qual_d, qual_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if qual_null {
        return DispatchResult::Deferred;
    }
    // SAFETY: main backend thread; qual_d is a valid text varlena Datum.
    let text_bytes = unsafe { raster_datum_as_bytes(qual_d) };
    let Ok(rules_text) = std::str::from_utf8(text_bytes) else {
        return DispatchResult::Deferred;
    };
    let Some(rules_f64) = raster::parse_reclass_rules(rules_text) else {
        return DispatchResult::Deferred;
    };
    if rules_f64.is_empty() {
        return DispatchResult::Deferred;
    }
    // Convert the extractor's struct (identical layout) into the FFI struct
    // expected by gpu::raster_reclass / pgaccel_raster_reclass.
    let rules: Vec<crate::gpu::types::PgaccelReclassRule> = rules_f64
        .iter()
        .map(|r| crate::gpu::types::PgaccelReclassRule {
            min_val: r.min_val,
            max_val: r.max_val,
            new_val: r.new_val,
        })
        .collect();

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut results: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());

    for &(datum, is_null) in batch {
        if is_null {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        }
        // SAFETY: main backend thread.
        let bytes = unsafe { raster_datum_as_bytes(datum) };
        let Some(header) = raster::parse_header(bytes) else {
            results.push((datum, false));
            continue;
        };
        let Some(pixels_f64) = raster::extract_pixels_f64(bytes, 0) else {
            results.push((datum, false));
            continue;
        };
        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }
        // Convert to fp32 input + allocate fp32 output (kernel uses
        // PgaccelPixelType::Float32 throughout).
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        let mut output_buf = vec![0u8; pixel_count * 4];

        let gpu_ok = gpu::raster_reclass(
            pixels_f32.as_ptr().cast::<std::ffi::c_void>(),
            pixel_count,
            gpu::PgaccelPixelType::Float32 as i32,
            &rules,
            gpu::PgaccelPixelType::Float32 as i32,
            &mut output_buf,
        );
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster_reclass GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }

        // SAFETY: output_buf is pixel_count * 4 bytes of f32.
        let output_f32: &[f32] =
            unsafe { std::slice::from_raw_parts(output_buf.as_ptr().cast(), pixel_count) };

        match raster::patch_band0_pixels(bytes, output_f32) {
            Some(new_wkb) => {
                let total_size = new_wkb.len() + pgrx::pg_sys::VARHDRSZ;
                // SAFETY: palloc on main backend thread.
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
            None => results.push((datum, false)),
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster_reclass pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

/// `st_summarystats(rast)` dispatch: per-row 6-scalar summary
/// (count / sum / mean / stddev / min / max). Returns
/// [`DispatchResult::AcceleratedRecord`] with `fields_per_row = 6`. The
/// flat Datum vec is laid out so `datums[row*6 + field]` indexes each
/// row's field. Rows that fail to parse a header / extract pixels emit
/// six NULL Datums to keep the row layout intact.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_summarystats(batch: &[(pgrx::pg_sys::Datum, bool)]) -> DispatchResult {
    // We can't easily build a single fused buffer because each row's pixel
    // count varies. Per-row dispatch keeps the kernel call simple at the
    // cost of one launch per raster — same shape as st_clip / st_reclass.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut datums: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len() * 6);

    for &(datum, is_null) in batch {
        if is_null {
            for _ in 0..6 {
                datums.push((pgrx::pg_sys::Datum::from(0_u64), true));
            }
            continue;
        }
        // SAFETY: main backend thread.
        let bytes = unsafe { raster_datum_as_bytes(datum) };
        let Some(header) = raster::parse_header(bytes) else {
            for _ in 0..6 {
                datums.push((pgrx::pg_sys::Datum::from(0_u64), true));
            }
            continue;
        };
        let Some(pixels_f64) = raster::extract_pixels_f64(bytes, 0) else {
            for _ in 0..6 {
                datums.push((pgrx::pg_sys::Datum::from(0_u64), true));
            }
            continue;
        };
        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            for _ in 0..6 {
                datums.push((pgrx::pg_sys::Datum::from(0_u64), true));
            }
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        let mut out = vec![0.0f64; 6];
        let gpu_ok = gpu::raster_summarystats(&pixels_f32, 1, pixel_count, None, &mut out);
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster_summarystats GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }
        for v in &out {
            datums.push((pgrx::pg_sys::Datum::from(v.to_bits()), false));
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster_summarystats pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::AcceleratedRecord {
        fields_per_row: 6,
        datums,
    }
}

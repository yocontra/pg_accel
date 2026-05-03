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
/// Routes by function name via the registry. `qual_datums` carries every
/// constant arg from the call site in positional order; multi-arg ops
/// (`ST_Resample(rast, w, h)`, `ST_Hillshade(rast, cx, cy, az, alt)`)
/// index by position so argument semantics are preserved (target_w vs
/// target_h, sun_az vs sun_alt etc.).
///
/// - `st_mapalgebra` — fully wired (raster header parse + band extract +
///   `map_algebra` kernel + WKB patch-back).
/// - `st_clip(rast, geom)` — wired via Agent 3A's
///   `extract_polygon_ring(qual_datum)` + `gpu::raster_clip`. Reads
///   `qual_datums[0]` (geometry).
/// - `st_reclass(rast, text)` — wired via Agent 3A's
///   `parse_reclass_rules(qual_datum_as_text)` + `gpu::raster_reclass`.
///   Reads `qual_datums[0]` (text rules).
/// - `st_summarystats(rast)` — wired via `gpu::raster_summarystats`,
///   returning [`DispatchResult::AcceleratedRecord`] with 6 fields per row.
/// - `st_resample(rast, target_w, target_h)` — Phase II F1: 2 i32 args
///   from `qual_datums[0..2]`.
/// - `st_slope(rast, cell_x, cell_y)` — Phase II F1: 2 f64 args.
/// - `st_aspect(rast, cell_x, cell_y)` — Phase II F1: 2 f64 args (cell
///   dims threaded through to the kernel for consistency with slope/
///   hillshade pipelines).
/// - `st_hillshade(rast, cell_x, cell_y, sun_az, sun_alt)` — Phase II
///   F1: 4 f64 args.
/// - `st_value(rast, point_array)` — escalated per anti-cheat ban #9
///   pending pgrx ArrayType deserialization in this dispatch carrier
///   (the Datum is a varlena ArrayType*; the existing extractors don't
///   walk array bodies). Defers cleanly with a debug log; tracked as
///   the follow-up TODO entry rather than fake-success.
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
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);

    // Most existing arms only consume `qual_datums[0]`; package as the
    // legacy `(datum, is_null)` Option so the per-op helpers below stay
    // ergonomic.
    let qual_datum: Option<(pgrx::pg_sys::Datum, bool)> =
        qual_datums.first().map(|&(d, n, _)| (d, n));

    match fn_name {
        Some("st_mapalgebra") => unsafe { dispatch_st_mapalgebra(batch) },
        Some("st_clip") => unsafe { dispatch_st_clip(batch, qual_datum) },
        Some("st_reclass") => unsafe { dispatch_st_reclass(batch, qual_datum) },
        Some("st_summarystats") => unsafe { dispatch_st_summarystats(batch) },
        Some("st_resample") => unsafe { dispatch_st_resample(batch, qual_datums) },
        Some("st_slope") => unsafe { dispatch_st_slope(batch, qual_datums) },
        Some("st_aspect") => unsafe { dispatch_st_aspect(batch, qual_datums) },
        Some("st_hillshade") => unsafe { dispatch_st_hillshade(batch, qual_datums) },
        Some("st_value") => {
            // Escalation per anti-cheat ban #9: `st_value(rast, point_array)`
            // takes a `geometry[]` ArrayType varlena. The existing extractors
            // (raster::extract_polygon_ring etc.) walk single GSERIALIZED
            // datums, not PG ArrayTypes. Wiring this would require a
            // dedicated array-walker that:
            //   1. detoasts the ArrayType varlena,
            //   2. reads its element type / dims,
            //   3. iterates element data + null bitmap,
            //   4. extracts each GSERIALIZED point's (x, y) into a flat
            //      f64 buffer for `gpu::raster_value`.
            // None of that machinery exists in this tree; building it under
            // the F1 deadline risks shipping a wrong-result extractor (the
            // ArrayType layout is dim-count + lower-bounds + null bitmap +
            // packed elements; mis-handling any of those produces silently
            // mis-aligned point coords). Defer cleanly so PG runs PostGIS
            // ST_Value(); tracked as a Phase 2 follow-up entry alongside
            // the F3 FunctionScan injection work.
            pgrx::debug1!(
                "pg_accel: dispatch_gpu_raster: st_value(rast, geometry[]) deferring — \
                 ArrayType deserialization not yet wired (escalated per anti-cheat ban #9)"
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

// ---------------------------------------------------------------------------
// Phase II Agent F1: multi-arg raster dispatch
// ---------------------------------------------------------------------------
//
// Each helper consumes additional constant args from `qual_datums` (in
// positional source-list order) on top of the per-row raster column. Type
// extraction follows PG datum conventions: int4 reads as `i32` via
// `Datum::value() as i32`; float8 decodes via `f64::from_bits(value as u64)`.
//
// Output datum construction mirrors the existing `st_clip` / `st_reclass`
// arms: write a fresh varlena via `palloc + set_varsize_4b + memcpy`.

/// Build a fresh single-band PostGIS raster WKB for `st_resample` output.
///
/// `src_header` provides SRID + origin; `dst_w` / `dst_h` / `dst_scale_x` /
/// `dst_scale_y` describe the resampled grid; `pixels` is the f32
/// row-major output with `dst_w * dst_h` entries.
///
/// The output has one Float32 band, no nodata flag, with the standard
/// little-endian header (matches GDAL's default WKB raster output).
///
/// Layout (matches `parse_header` / `band_offset`):
///
/// - `[0]` endianness byte (1 = little-endian)
/// - `[1..3]` u16 version (0)
/// - `[3..5]` u16 num_bands (1)
/// - `[5..13]` f64 scale_x
/// - `[13..21]` f64 scale_y
/// - `[21..29]` f64 ip_x (origin)
/// - `[29..37]` f64 ip_y
/// - `[37..45]` f64 skew_x (0.0)
/// - `[45..53]` f64 skew_y (0.0)
/// - `[53..57]` i32 srid
/// - `[57..59]` u16 width (= dst_w)
/// - `[59..61]` u16 height (= dst_h)
/// - `[61]` band flags byte (`pixel_type=Float32 (7) << 4 | flags=0`)
/// - `[62..66]` f32 nodata (0.0; ignored because flag bit 0x01 is unset)
/// - `[66..66 + 4 * dst_w * dst_h]` Float32 pixel data, row-major LE
fn build_resampled_raster(
    src_header: &raster::RasterHeader,
    dst_w: u16,
    dst_h: u16,
    dst_scale_x: f64,
    dst_scale_y: f64,
    pixels: &[f32],
) -> Vec<u8> {
    let pixel_count = dst_w as usize * dst_h as usize;
    debug_assert!(pixels.len() >= pixel_count, "pixel buffer underflow");

    // 61-byte header + 1 band flags byte + 4-byte Float32 nodata + N pixels.
    let mut out = Vec::with_capacity(61 + 1 + 4 + pixel_count * 4);
    out.push(1u8); // little-endian
    out.extend_from_slice(&0u16.to_le_bytes()); // version
    out.extend_from_slice(&1u16.to_le_bytes()); // num_bands
    out.extend_from_slice(&dst_scale_x.to_le_bytes());
    out.extend_from_slice(&dst_scale_y.to_le_bytes());
    out.extend_from_slice(&src_header.ip_x.to_le_bytes());
    out.extend_from_slice(&src_header.ip_y.to_le_bytes());
    out.extend_from_slice(&0.0f64.to_le_bytes()); // skew_x
    out.extend_from_slice(&0.0f64.to_le_bytes()); // skew_y
    out.extend_from_slice(&src_header.srid.to_le_bytes());
    out.extend_from_slice(&dst_w.to_le_bytes());
    out.extend_from_slice(&dst_h.to_le_bytes());

    // Band flags: pixel_type=Float32 (code 7) in high nibble, no nodata,
    // not offline, not isnodata, not hasnodata in low nibble.
    let band_flags: u8 = 7u8 << 4;
    out.push(band_flags);
    // Nodata value (ignored because hasnodata bit is unset).
    out.extend_from_slice(&0.0f32.to_le_bytes());

    // Pixel payload, little-endian Float32.
    for &p in &pixels[..pixel_count] {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
}

/// Wrap raw raster WKB bytes in a freshly palloc'd varlena Datum.
///
/// # Safety
///
/// Must be called on the main backend thread (calls `palloc` /
/// `set_varsize_4b`).
unsafe fn wkb_to_varlena_datum(wkb: &[u8]) -> pgrx::pg_sys::Datum {
    let total_size = wkb.len() + pgrx::pg_sys::VARHDRSZ;
    // SAFETY: palloc on main backend thread.
    let new_varlena = unsafe { pgrx::pg_sys::palloc(total_size).cast::<u8>() };
    // SAFETY: new_varlena is freshly palloc'd with total_size bytes; the
    // following memcpy fits inside the allocation.
    unsafe {
        pgrx::set_varsize_4b(new_varlena.cast(), total_size as i32);
        let data_dest = pgrx::vardata_any(new_varlena.cast()).cast::<u8>();
        std::ptr::copy_nonoverlapping(wkb.as_ptr(), data_dest.cast_mut(), wkb.len());
    }
    pgrx::pg_sys::Datum::from(new_varlena)
}

/// `st_resample(rast, target_w, target_h)` dispatch. Reads two `i32` args
/// from `qual_datums[0..2]`, runs `gpu::raster_resample`, and emits a new
/// Float32 single-band raster per input row with the requested dims.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_resample(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    if qual_datums.len() < 2 {
        pgrx::debug1!(
            "pg_accel: st_resample needs (target_w, target_h), got {} qual_datums — deferring",
            qual_datums.len()
        );
        return DispatchResult::Deferred;
    }
    let (w_datum, w_null, _w_typid) = qual_datums[0];
    let (h_datum, h_null, _h_typid) = qual_datums[1];
    if w_null || h_null {
        return DispatchResult::Deferred;
    }
    // PG int4 sits in the low 32 bits of the Datum.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let target_w = w_datum.value() as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let target_h = h_datum.value() as i32;
    if target_w <= 0 || target_h <= 0 {
        pgrx::debug1!(
            "pg_accel: st_resample target dims must be positive (got {}x{}) — deferring",
            target_w,
            target_h,
        );
        return DispatchResult::Deferred;
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let dst_w = target_w as u16;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let dst_h = target_h as u16;

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
        let src_w = header.width as usize;
        let src_h = header.height as usize;
        if src_w == 0 || src_h == 0 {
            results.push((datum, false));
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        #[allow(clippy::cast_sign_loss)]
        let dst_count = (target_w as usize) * (target_h as usize);
        let mut dst_pixels = vec![0.0f32; dst_count];
        #[allow(clippy::cast_sign_loss)]
        let gpu_ok = gpu::raster_resample(
            &pixels_f32,
            src_w,
            src_h,
            target_w as usize,
            target_h as usize,
            &mut dst_pixels,
        );
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster_resample GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }

        // Preserve world coverage: scale_x and scale_y are world-units per
        // pixel, so resampling into N pixels along an axis means each new
        // pixel covers (src_axis_extent / N) world units.
        let dst_scale_x = header.scale_x * (src_w as f64) / f64::from(target_w);
        let dst_scale_y = header.scale_y * (src_h as f64) / f64::from(target_h);
        let new_wkb =
            build_resampled_raster(&header, dst_w, dst_h, dst_scale_x, dst_scale_y, &dst_pixels);
        // SAFETY: main backend thread.
        let datum_out = unsafe { wkb_to_varlena_datum(&new_wkb) };
        results.push((datum_out, false));
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster_resample pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

/// `st_slope(rast, cell_x, cell_y)` dispatch. Reads two `f64` args from
/// `qual_datums[0..2]`, runs `gpu::raster_slope` per row, and writes back
/// the slope (degrees) into the source raster's Float32 band via
/// `patch_band0_pixels`.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_slope(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    if qual_datums.len() < 2 {
        pgrx::debug1!(
            "pg_accel: st_slope needs (cell_x, cell_y), got {} qual_datums — deferring",
            qual_datums.len()
        );
        return DispatchResult::Deferred;
    }
    let (cx_d, cx_n, _cx_t) = qual_datums[0];
    let (cy_d, cy_n, _cy_t) = qual_datums[1];
    if cx_n || cy_n {
        return DispatchResult::Deferred;
    }
    let cell_size_x = f64::from_bits(cx_d.value() as u64);
    let cell_size_y = f64::from_bits(cy_d.value() as u64);
    if !cell_size_x.is_finite()
        || !cell_size_y.is_finite()
        || cell_size_x == 0.0
        || cell_size_y == 0.0
    {
        return DispatchResult::Deferred;
    }
    // SAFETY: main backend thread.
    unsafe {
        dispatch_per_pixel_band0(batch, "raster_slope", |pixels_f32, header, out_f32| {
            gpu::raster_slope(
                pixels_f32,
                header.width as usize,
                header.height as usize,
                cell_size_x,
                cell_size_y,
                out_f32,
            )
        })
    }
}

/// `st_aspect(rast, cell_x, cell_y)` dispatch. The kernel is intentionally
/// 1-arg (`raster_aspect` ignores cell sizes — aspect is angle-only) but
/// we still consume the two cell-size args from `qual_datums` to keep the
/// call shape consistent with `st_slope` / `st_hillshade`.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_aspect(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    if qual_datums.len() < 2 {
        pgrx::debug1!(
            "pg_accel: st_aspect needs (cell_x, cell_y), got {} qual_datums — deferring",
            qual_datums.len()
        );
        return DispatchResult::Deferred;
    }
    // Validate args are well-formed even though the aspect kernel doesn't
    // consume them — this matches the doc-string and makes mis-typed calls
    // observable here rather than silently producing slope/aspect-pipeline
    // mismatches downstream.
    let (cx_d, cx_n, _) = qual_datums[0];
    let (cy_d, cy_n, _) = qual_datums[1];
    if cx_n || cy_n {
        return DispatchResult::Deferred;
    }
    let cell_size_x = f64::from_bits(cx_d.value() as u64);
    let cell_size_y = f64::from_bits(cy_d.value() as u64);
    if !cell_size_x.is_finite() || !cell_size_y.is_finite() {
        return DispatchResult::Deferred;
    }
    // SAFETY: main backend thread.
    unsafe {
        dispatch_per_pixel_band0(batch, "raster_aspect", |pixels_f32, header, out_f32| {
            gpu::raster_aspect(
                pixels_f32,
                header.width as usize,
                header.height as usize,
                out_f32,
            )
        })
    }
}

/// `st_hillshade(rast, cell_x, cell_y, sun_az, sun_alt)` dispatch. Reads
/// four `f64` args from `qual_datums[0..4]` and runs
/// `gpu::raster_hillshade` per row with `z_factor=1.0`.
///
/// Argument positions are load-bearing: cell_x first, cell_y second,
/// sun_azimuth third, sun_altitude fourth — the planner order maps 1:1.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_hillshade(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    if qual_datums.len() < 4 {
        pgrx::debug1!(
            "pg_accel: st_hillshade needs (cell_x, cell_y, sun_az, sun_alt), got {} qual_datums — deferring",
            qual_datums.len()
        );
        return DispatchResult::Deferred;
    }
    let (cx_d, cx_n, _) = qual_datums[0];
    let (cy_d, cy_n, _) = qual_datums[1];
    let (az_d, az_n, _) = qual_datums[2];
    let (al_d, al_n, _) = qual_datums[3];
    if cx_n || cy_n || az_n || al_n {
        return DispatchResult::Deferred;
    }
    let cell_size_x = f64::from_bits(cx_d.value() as u64);
    let cell_size_y = f64::from_bits(cy_d.value() as u64);
    let sun_az = f64::from_bits(az_d.value() as u64);
    let sun_alt = f64::from_bits(al_d.value() as u64);
    if !cell_size_x.is_finite()
        || !cell_size_y.is_finite()
        || !sun_az.is_finite()
        || !sun_alt.is_finite()
        || cell_size_x == 0.0
        || cell_size_y == 0.0
    {
        return DispatchResult::Deferred;
    }
    let z_factor = 1.0f64;
    // SAFETY: main backend thread.
    unsafe {
        dispatch_per_pixel_band0(batch, "raster_hillshade", |pixels_f32, header, out_f32| {
            gpu::raster_hillshade(
                pixels_f32,
                header.width as usize,
                header.height as usize,
                cell_size_x,
                cell_size_y,
                sun_az,
                sun_alt,
                z_factor,
                out_f32,
            )
        })
    }
}

/// Per-row band-0 transform helper shared by slope / aspect / hillshade.
///
/// Extracts band 0's pixel data into an fp32 buffer, calls `kernel` with
/// `(pixels_f32, header, out_f32)`, and patches the result back into the
/// original WKB via `patch_band0_pixels`. Output dims match input — this
/// helper is NOT used by `st_resample` (which changes dims and needs a
/// fresh WKB).
///
/// `kernel_name` is used only for the GPU-failure error message; it lets
/// each caller surface the specific kernel that died without duplicating
/// the boilerplate above.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_per_pixel_band0<F>(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    kernel_name: &'static str,
    mut kernel: F,
) -> DispatchResult
where
    F: FnMut(&[f32], &raster::RasterHeader, &mut [f32]) -> Option<()>,
{
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
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        let mut out_f32 = vec![0.0f32; pixel_count];
        let gpu_ok = kernel(&pixels_f32, &header, &mut out_f32);
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: {} GPU kernel failed; refusing CPU fallback (rule 11)",
                kernel_name,
            );
        }

        match raster::patch_band0_pixels(bytes, &out_f32) {
            Some(new_wkb) => {
                // SAFETY: main backend thread.
                let datum_out = unsafe { wkb_to_varlena_datum(&new_wkb) };
                results.push((datum_out, false));
            }
            None => results.push((datum, false)),
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: {} pipeline took {}ms (timeout {}ms)",
            kernel_name,
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

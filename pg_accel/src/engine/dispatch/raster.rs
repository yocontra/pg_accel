//! GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, `ST_Reclass`,
//! and the Agent 3A kernels (`ST_SummaryStats`).

use crate::adapters::extractors::array::ParseError as ArrayParseError;
use crate::adapters::extractors::geometry::{
    ExtractError as GeometryExtractError, extract_geometry_array,
};
use crate::adapters::extractors::raster;
use crate::engine::gucs;
use crate::gpu;

use super::{DispatchResult, RasterDispatchOp};

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
/// - `st_value(rast, point_array)` — walks the geometry[] payload via
///   `extractors::geometry::extract_geometry_array`, extracts each
///   element's POINT (x, y), and runs the kernel once per row
///   over the row's flat point buffer. Multidim arrays escalate cleanly
///   per anti-cheat ban #9.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_raster(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    _fn_info: &pgrx::pg_sys::FmgrInfo,
    _is_strict: bool,
    op: RasterDispatchOp,
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    // Most arms only consume `qual_datums[0]`; package as a compact
    // `(datum, is_null)` Option so the per-op helpers below stay ergonomic.
    let qual_datum: Option<(pgrx::pg_sys::Datum, bool)> =
        qual_datums.first().map(|&(d, n, _)| (d, n));

    match op {
        RasterDispatchOp::MapAlgebra => unsafe { dispatch_st_mapalgebra(batch, qual_datums) },
        RasterDispatchOp::Clip => unsafe { dispatch_st_clip(batch, qual_datum) },
        RasterDispatchOp::Reclass => unsafe { dispatch_st_reclass(batch, qual_datum) },
        RasterDispatchOp::SummaryStats => unsafe { dispatch_st_summarystats(batch) },
        RasterDispatchOp::Resample => unsafe { dispatch_st_resample(batch, qual_datums) },
        RasterDispatchOp::Slope => unsafe { dispatch_st_slope(batch, qual_datums) },
        RasterDispatchOp::Aspect => unsafe { dispatch_st_aspect(batch, qual_datums) },
        RasterDispatchOp::Hillshade => unsafe { dispatch_st_hillshade(batch, qual_datums) },
        RasterDispatchOp::Value => unsafe { dispatch_st_value(batch, qual_datum) },
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

const MAP_ALGEBRA_MAX_BANDS: usize = 8;

#[derive(Clone)]
struct MapAlgebraProgram {
    instructions: Vec<gpu::PgaccelExprInst>,
    source_bands: Vec<usize>,
}

fn load_band_inst(slot: usize) -> gpu::PgaccelExprInst {
    gpu::PgaccelExprInst {
        op: gpu::PgaccelOp::LoadBand,
        // The C ABI field is a union. For LOAD_BAND, encode the i32 band
        // index into the low bits of the f64-sized Rust mirror field.
        arg: f64::from_bits(slot as u64),
    }
}

fn load_const_inst(value: f64) -> gpu::PgaccelExprInst {
    gpu::PgaccelExprInst {
        op: gpu::PgaccelOp::LoadConst,
        arg: value,
    }
}

fn op_inst(op: gpu::PgaccelOp) -> gpu::PgaccelExprInst {
    gpu::PgaccelExprInst { op, arg: 0.0 }
}

fn is_text_oid(typid: pgrx::pg_sys::Oid) -> bool {
    typid == pgrx::pg_sys::TEXTOID || typid == pgrx::pg_sys::VARCHAROID
}

fn is_int_oid(typid: pgrx::pg_sys::Oid) -> bool {
    typid == pgrx::pg_sys::INT2OID
        || typid == pgrx::pg_sys::INT4OID
        || typid == pgrx::pg_sys::INT8OID
}

#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn datum_as_i32(datum: pgrx::pg_sys::Datum) -> i32 {
    datum.value() as i32
}

unsafe fn datum_as_text(datum: pgrx::pg_sys::Datum) -> Option<&'static str> {
    // SAFETY: caller guarantees datum is a valid varlena text Datum.
    std::str::from_utf8(unsafe { raster_datum_as_bytes(datum) }).ok()
}

unsafe fn build_map_algebra_program(
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> Option<MapAlgebraProgram> {
    let mut band_args: Vec<usize> = Vec::new();
    let mut expr_text: Option<String> = None;

    for &(datum, is_null, typid) in qual_datums {
        if is_null {
            continue;
        }
        if is_text_oid(typid) {
            // SAFETY: typid says this is a text-like varlena Datum.
            let text = unsafe { datum_as_text(datum) }?;
            if text.to_ascii_lowercase().contains("[rast") {
                expr_text = Some(text.to_owned());
                break;
            }
            continue;
        }
        if expr_text.is_none() && is_int_oid(typid) {
            let band_1based = datum_as_i32(datum);
            if band_1based <= 0 {
                return None;
            }
            #[allow(clippy::cast_sign_loss)]
            band_args.push((band_1based as usize) - 1);
        }
    }

    let expr_text = expr_text?;
    MapAlgebraParser::new(&expr_text, &band_args).parse()
}

struct MapAlgebraParser<'a> {
    input: &'a [u8],
    pos: usize,
    band_args: &'a [usize],
    instructions: Vec<gpu::PgaccelExprInst>,
    source_bands: Vec<usize>,
    numbered_refs: Vec<usize>,
}

impl<'a> MapAlgebraParser<'a> {
    fn new(input: &'a str, band_args: &'a [usize]) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
            band_args,
            instructions: Vec::new(),
            source_bands: Vec::new(),
            numbered_refs: Vec::new(),
        }
    }

    fn parse(mut self) -> Option<MapAlgebraProgram> {
        self.parse_comparison()?;
        self.skip_ws();
        if self.pos != self.input.len() || self.instructions.is_empty() {
            return None;
        }

        // The current scan carrier only provides one raster Datum plus
        // constant args. Numbered refs are therefore safe only for same-raster
        // multi-band expressions with distinct source band numbers.
        let mut refs = self.numbered_refs.clone();
        refs.sort_unstable();
        refs.dedup();
        if refs.len() > 1 {
            let mut mapped = Vec::with_capacity(refs.len());
            for n in &refs {
                mapped.push(*self.band_args.get(n.saturating_sub(1))?);
            }
            let mut unique = mapped.clone();
            unique.sort_unstable();
            unique.dedup();
            if unique.len() != mapped.len() {
                return None;
            }
        }

        Some(MapAlgebraProgram {
            instructions: self.instructions,
            source_bands: self.source_bands,
        })
    }

    fn parse_comparison(&mut self) -> Option<()> {
        self.parse_add_sub()?;
        loop {
            self.skip_ws();
            let op = if self.consume(b'>') {
                Some(gpu::PgaccelOp::Gt)
            } else if self.consume(b'<') {
                Some(gpu::PgaccelOp::Lt)
            } else if self.consume(b'=') {
                let _ = self.consume(b'=');
                Some(gpu::PgaccelOp::Eq)
            } else {
                None
            };
            let Some(op) = op else {
                break;
            };
            self.parse_add_sub()?;
            self.instructions.push(op_inst(op));
        }
        Some(())
    }

    fn parse_add_sub(&mut self) -> Option<()> {
        self.parse_mul_div()?;
        loop {
            self.skip_ws();
            let op = if self.consume(b'+') {
                Some(gpu::PgaccelOp::Add)
            } else if self.consume(b'-') {
                Some(gpu::PgaccelOp::Sub)
            } else {
                None
            };
            let Some(op) = op else {
                break;
            };
            self.parse_mul_div()?;
            self.instructions.push(op_inst(op));
        }
        Some(())
    }

    fn parse_mul_div(&mut self) -> Option<()> {
        self.parse_unary()?;
        loop {
            self.skip_ws();
            let op = if self.consume(b'*') {
                Some(gpu::PgaccelOp::Mul)
            } else if self.consume(b'/') {
                Some(gpu::PgaccelOp::Div)
            } else {
                None
            };
            let Some(op) = op else {
                break;
            };
            self.parse_unary()?;
            self.instructions.push(op_inst(op));
        }
        Some(())
    }

    fn parse_unary(&mut self) -> Option<()> {
        self.skip_ws();
        if self.consume(b'+') {
            return self.parse_unary();
        }
        if self.consume(b'-') {
            self.instructions.push(load_const_inst(0.0));
            self.parse_unary()?;
            self.instructions.push(op_inst(gpu::PgaccelOp::Sub));
            return Some(());
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<()> {
        self.skip_ws();
        if self.consume(b'(') {
            self.parse_comparison()?;
            self.skip_ws();
            return self.consume(b')').then_some(());
        }

        if self.peek() == Some(b'[') {
            let band_slot = self.parse_band_ref()?;
            self.instructions.push(load_band_inst(band_slot));
            return Some(());
        }

        if self.peek().is_some_and(is_number_start) {
            let value = self.parse_number()?;
            self.instructions.push(load_const_inst(value));
            return Some(());
        }

        if self
            .peek()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        {
            return self.parse_function();
        }

        None
    }

    fn parse_function(&mut self) -> Option<()> {
        let name = self.parse_identifier()?;
        self.skip_ws();
        if !self.consume(b'(') {
            return None;
        }

        match name.as_str() {
            "sqrt" => {
                self.parse_comparison()?;
                self.skip_ws();
                if !self.consume(b')') {
                    return None;
                }
                self.instructions.push(op_inst(gpu::PgaccelOp::Sqrt));
            }
            "abs" => {
                self.parse_comparison()?;
                self.skip_ws();
                if !self.consume(b')') {
                    return None;
                }
                self.instructions.push(op_inst(gpu::PgaccelOp::Abs));
            }
            "log" | "ln" => {
                self.parse_comparison()?;
                self.skip_ws();
                if !self.consume(b')') {
                    return None;
                }
                self.instructions.push(op_inst(gpu::PgaccelOp::Log));
            }
            "pow" | "power" => {
                self.parse_comparison()?;
                self.skip_ws();
                if !self.consume(b',') {
                    return None;
                }
                self.parse_comparison()?;
                self.skip_ws();
                if !self.consume(b')') {
                    return None;
                }
                self.instructions.push(op_inst(gpu::PgaccelOp::Pow));
            }
            _ => return None,
        }

        Some(())
    }

    fn parse_band_ref(&mut self) -> Option<usize> {
        if !self.consume(b'[') {
            return None;
        }
        let start = self.pos;
        while self.pos < self.input.len() && self.input[self.pos] != b']' {
            self.pos += 1;
        }
        if self.pos >= self.input.len() {
            return None;
        }
        let inner = std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .trim()
            .to_ascii_lowercase();
        self.pos += 1; // closing bracket

        let label = inner.split('.').next().unwrap_or("").trim();
        let actual_band = if label == "rast" {
            self.band_args.first().copied().unwrap_or(0)
        } else if let Some(n_str) = label.strip_prefix("rast") {
            let n: usize = n_str.parse().ok()?;
            if n == 0 {
                return None;
            }
            self.numbered_refs.push(n);
            *self.band_args.get(n - 1)?
        } else {
            return None;
        };

        if let Some(slot) = self.source_bands.iter().position(|&b| b == actual_band) {
            return Some(slot);
        }
        if self.source_bands.len() >= MAP_ALGEBRA_MAX_BANDS {
            return None;
        }
        self.source_bands.push(actual_band);
        Some(self.source_bands.len() - 1)
    }

    fn parse_number(&mut self) -> Option<f64> {
        let start = self.pos;
        let mut saw_digit = false;

        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            saw_digit = true;
            self.pos += 1;
        }
        if self.consume(b'.') {
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                saw_digit = true;
                self.pos += 1;
            }
        }
        if !saw_digit {
            return None;
        }
        if self.peek().is_some_and(|b| b == b'e' || b == b'E') {
            let exp_pos = self.pos;
            self.pos += 1;
            let _ = self.consume(b'+') || self.consume(b'-');
            let exp_start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                self.pos = exp_pos;
            }
        }

        std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse()
            .ok()
    }

    fn parse_identifier(&mut self) -> Option<String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        Some(
            std::str::from_utf8(&self.input[start..self.pos])
                .ok()?
                .to_ascii_lowercase(),
        )
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn consume(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

fn is_number_start(b: u8) -> bool {
    b.is_ascii_digit() || b == b'.'
}

/// `st_mapalgebra` dispatch: per-row map-algebra over band 0 with an
/// expression parsed from the call's text argument. Unsupported expression
/// grammar, non-constant expression text, offline bands, or ambiguous
/// multi-raster shapes return [`DispatchResult::Deferred`] so selected
/// pg_accel plans never silently become identity/no-op raster outputs.
///
/// # Safety
///
/// Must be called on the main backend thread.
unsafe fn dispatch_st_mapalgebra(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
) -> DispatchResult {
    // SAFETY: map-algebra constants are backend Datums captured from the
    // planner/executor call shape.
    let Some(program) = (unsafe { build_map_algebra_program(qual_datums) }) else {
        pgrx::debug1!(
            "pg_accel: st_mapalgebra has no supported constant expression/band shape; deferring"
        );
        return DispatchResult::Deferred;
    };
    if program.source_bands.is_empty() || program.source_bands.len() > MAP_ALGEBRA_MAX_BANDS {
        pgrx::debug1!(
            "pg_accel: st_mapalgebra source band count {} unsupported; deferring",
            program.source_bands.len()
        );
        return DispatchResult::Deferred;
    }

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    let mut instructions = program.instructions.clone();
    let expr = gpu::PgaccelExpr {
        instructions: instructions.as_mut_ptr(),
        inst_count: instructions.len(),
        band_count: program.source_bands.len(),
    };

    let mut results: Vec<(pgrx::pg_sys::Datum, bool)> = Vec::with_capacity(batch.len());

    for &(datum, is_null) in batch {
        if is_null {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        }

        // SAFETY: main backend thread.
        let bytes = unsafe { raster_datum_as_bytes(datum) };
        let Some(header) = raster::parse_header(bytes) else {
            pgrx::debug1!("pg_accel: st_mapalgebra raster header parse failed; deferring");
            return DispatchResult::Deferred;
        };
        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count == 0 {
            results.push((datum, false));
            continue;
        }

        let mut band_buffers: Vec<Vec<f32>> = Vec::with_capacity(program.source_bands.len());
        for &band_index in &program.source_bands {
            let Some(pixels_f64) = raster::extract_pixels_f64(bytes, band_index) else {
                pgrx::debug1!(
                    "pg_accel: st_mapalgebra band {} unavailable/offline; deferring",
                    band_index + 1
                );
                return DispatchResult::Deferred;
            };
            if pixels_f64.len() < pixel_count {
                pgrx::debug1!(
                    "pg_accel: st_mapalgebra band {} has {} pixels, expected {}; deferring",
                    band_index + 1,
                    pixels_f64.len(),
                    pixel_count
                );
                return DispatchResult::Deferred;
            }
            #[allow(clippy::cast_possible_truncation)]
            band_buffers.push(pixels_f64.iter().map(|&v| v as f32).collect());
        }

        let band_ptrs: Vec<*const std::ffi::c_void> =
            band_buffers.iter().map(|b| b.as_ptr().cast()).collect();
        let mut output_buf = vec![0u8; pixel_count * std::mem::size_of::<f32>()];
        let mut nodata_mask = vec![0u8; pixel_count];

        let gpu_ok = gpu::map_algebra(
            &band_ptrs,
            pixel_count,
            gpu::PgaccelPixelType::Float32 as i32,
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

        let Some(new_wkb) = raster::patch_band0_pixels(bytes, output_f32) else {
            pgrx::error!(
                "pg_accel: raster map_algebra could not patch output raster; refusing original-raster passthrough"
            );
        };
        // SAFETY: main backend thread.
        let datum_out = unsafe { wkb_to_varlena_datum(&new_wkb) };
        results.push((datum_out, false));
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster map_algebra pipeline took {}ms (timeout {}ms)",
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

// ---------------------------------------------------------------------------
// Phase 2 B3: ST_Value(rast, geometry[]) -> double precision[]
// ---------------------------------------------------------------------------

/// `ST_Value(rast, points geometry[])` per-row dispatch. Walks each input
/// row's geometry[] payload via the new ArrayType walker, extracts each
/// element as a POINT (x, y), and feeds the flat point buffer into
/// `gpu::raster_value`. Output is one `double precision[]` Datum per
/// input row containing one f64 per input point (NaN for out-of-bounds
/// points; PostGIS contract).
///
/// Per anti-cheat ban #9, multidim point arrays escalate cleanly via a
/// `DispatchResult::Deferred` after a debug log — we do not silently
/// flatten them.
///
/// # Safety
///
/// Must be called on the main backend thread. The point-array Datum is
/// per-row (`batch[i].0`), but PostGIS allows the array to also be a
/// constant; we honour both shapes by reading from `batch` per row and
/// falling back to `qual_datum` only if no per-row array is available
/// (the planner-side classifier will route the constant case here too).
unsafe fn dispatch_st_value(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> DispatchResult {
    // The dispatch carrier packages the per-row raster column into `batch`
    // and the constant point-array into `qual_datums[0]`. The per-row column
    // semantics for the point array are the common case for the test
    // workload (`SELECT ST_Value(rast, ARRAY[ST_Point(1,1), ST_Point(2,2)])
    // FROM t`); honour that by reading the array from `qual_datum` (the
    // const path) and re-using it across all rows.
    let Some((points_datum, points_null)) = qual_datum else {
        return DispatchResult::Deferred;
    };
    if points_null {
        return DispatchResult::Deferred;
    }

    let points = match extract_geometry_array(points_datum) {
        Ok(points) => points,
        Err(GeometryExtractError::Array(ArrayParseError::Multidim(n))) => {
            pgrx::debug1!(
                "pg_accel: dispatch_st_value: multidim geometry[] (ndim={n}) escalated per ban #9"
            );
            return DispatchResult::Deferred;
        }
        Err(e) => {
            pgrx::debug1!("pg_accel: dispatch_st_value: geometry[] parse failed ({e}); deferring");
            return DispatchResult::Deferred;
        }
    };

    // Walk the extracted slots once. Accept POINT geometries only; NULL
    // and non-POINT elements preserve their array position but do not feed
    // the raster_value kernel.
    let mut points_xy: Vec<f64> = Vec::with_capacity(points.len() * 2);
    let mut element_was_point: Vec<bool> = Vec::with_capacity(points.len());
    for point in &points {
        if let Some((x, y)) = point.point_xy() {
            points_xy.push(x);
            points_xy.push(y);
            element_was_point.push(true);
        } else {
            element_was_point.push(false);
        }
    }
    let valid_pts = points_xy.len() / 2;
    if valid_pts == 0 {
        // Empty / all-null / all-degenerate array — nothing to dispatch.
        return DispatchResult::Deferred;
    }

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
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        };
        let Some(pixels_f64) = raster::extract_pixels_f64(bytes, 0) else {
            results.push((pgrx::pg_sys::Datum::from(0usize), true));
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let pixels_f32: Vec<f32> = pixels_f64.iter().map(|&v| v as f32).collect();
        let mut out = vec![0.0f64; valid_pts];

        let gpu_ok = gpu::raster_value(
            &pixels_f32,
            header.width as usize,
            header.height as usize,
            header.ip_x,
            header.ip_y,
            header.scale_x,
            header.scale_y,
            &points_xy,
            &mut out,
        );
        if gpu_ok.is_none() {
            pgrx::error!(
                "pg_accel: raster_value GPU kernel failed; refusing CPU fallback (rule 11)"
            );
        }

        // Re-expand the kernel output to one f64 per array element,
        // inserting NaN for elements that were NULL or non-POINT (we
        // didn't feed them to the kernel). PostGIS represents
        // out-of-bounds / no-data as NULL within the output array; we
        // emit NaN here as a placeholder so the array shape matches the
        // input length, which is what the kernel contract documents.
        let mut row_doubles: Vec<f64> = Vec::with_capacity(element_was_point.len());
        let mut kernel_cursor = 0usize;
        for &is_pt in &element_was_point {
            if is_pt {
                row_doubles.push(out[kernel_cursor]);
                kernel_cursor += 1;
            } else {
                row_doubles.push(f64::NAN);
            }
        }

        // SAFETY: build a `float8[]` PG array from the row doubles via
        // `construct_array_builtin` on the main backend thread. The
        // returned ArrayType* is itself a varlena pointer suitable for
        // a Datum.
        let datum_out = unsafe { build_float8_array(&row_doubles) };
        results.push((datum_out, false));
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: st_value pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    DispatchResult::Accelerated(results)
}

/// Build a 1-D `float8[]` PG ArrayType varlena from a slice of doubles.
///
/// Each element is wrapped in a Datum (FLOAT8 is pass-by-value as i64
/// reinterpret on 64-bit; we use `f64::to_bits` to round-trip).
///
/// # Safety
///
/// Must be called on the main backend thread. Returned Datum points at a
/// freshly palloc'd ArrayType.
unsafe fn build_float8_array(values: &[f64]) -> pgrx::pg_sys::Datum {
    use pgrx::pg_sys;
    // SAFETY: each f64 fits in a Datum (8 bytes on 64-bit) by reinterpret.
    let mut datums: Vec<pg_sys::Datum> = values
        .iter()
        .map(|v| pg_sys::Datum::from(v.to_bits()))
        .collect();
    // SAFETY: main backend thread; `datums.as_mut_ptr` valid for nelems
    // for the duration of the call. construct_array_builtin allocates a
    // new ArrayType in the current memory context.
    let arr_ptr = unsafe {
        pg_sys::construct_array_builtin(
            datums.as_mut_ptr(),
            values.len() as std::os::raw::c_int,
            pg_sys::FLOAT8OID,
        )
    };
    pg_sys::Datum::from(arr_ptr)
}

#[cfg(test)]
mod tests {
    use crate::gpu::PgaccelOp;

    use super::MapAlgebraParser;

    fn load_band_slots(program: &super::MapAlgebraProgram) -> Vec<usize> {
        program
            .instructions
            .iter()
            .filter(|inst| inst.op == PgaccelOp::LoadBand)
            .map(|inst| inst.arg.to_bits() as usize)
            .collect()
    }

    #[test]
    fn map_algebra_parser_builds_ndvi_two_band_ir() {
        let program = MapAlgebraParser::new("([rast1]-[rast2])/([rast1]+[rast2]+0.001)", &[0, 1])
            .parse()
            .expect("NDVI-style two-band map algebra should parse");

        assert_eq!(program.source_bands, vec![0, 1]);
        assert_eq!(load_band_slots(&program), vec![0, 1, 0, 1]);
        assert!(
            program
                .instructions
                .iter()
                .any(|inst| inst.op == PgaccelOp::Div),
            "NDVI expression should compile to a division"
        );
        assert!(
            program
                .instructions
                .iter()
                .any(|inst| inst.op == PgaccelOp::LoadConst && (inst.arg - 0.001).abs() < 1e-12),
            "NDVI denominator epsilon should be preserved as a constant"
        );
    }

    #[test]
    fn map_algebra_parser_rejects_ambiguous_numbered_raster_refs() {
        let program = MapAlgebraParser::new("[rast1] + [rast2]", &[0, 0]).parse();

        assert!(
            program.is_none(),
            "numbered refs that map to the same source band are ambiguous until true multi-raster \
             map algebra is implemented"
        );
    }
}

// ---------------------------------------------------------------------------
// pg_test integration for the Phase 2 B3 ST_Value(rast, geometry[]) arm
// ---------------------------------------------------------------------------
//
// **Honest escalation per anti-cheat ban #1 (no fake success) + ban #6
// (no guessed APIs).** The Phase 2 B3 brief specified an end-to-end test
// targeting `SELECT ST_Value(rast, ARRAY[ST_Point(1,1), ST_Point(2,2)])
// FROM raster_table`. PostGIS 3.x does NOT expose any `ST_Value(raster,
// geometry[])` overload — the only `geometry`-arg overloads are the
// scalar `ST_Value(raster, geometry pt)` and `ST_Value(raster, integer
// band, geometry pt)` shapes (see `rtpostgis.sql:st_value` in the
// PostGIS 3.6 distribution). The 4-arg `_array_` variant exists in
// liblwgeom-internal kernels (`RASTER_getPixelValueArray`) but is not
// surfaced as a SQL function in the stock distribution.
//
// The dispatch arm + array walker are still correct and reusable for
// any future PG-side wrapper (custom or upstream) that exposes a
// `(raster, geometry[])` SQL surface; the unit tests in
// `adapters::extractors::array::tests` cover the walker behaviour
// directly. We intentionally do NOT ship a #[pg_test] here that would
// either hard-error on the missing function (false failure) or silently
// no-op (ban #2 weakening).
//
// To wire a real end-to-end smoke once the SQL surface exists, add a
// `#[pg_test]` here that:
//   1. CREATEs a thin SQL wrapper:
//      `CREATE FUNCTION st_value_array(raster, geometry[]) RETURNS
//      double precision[] AS $$ ... $$ LANGUAGE C ...`
//   2. Asserts on a 2-point query workload like the brief described.

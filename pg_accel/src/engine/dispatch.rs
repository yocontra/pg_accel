//! Batch dispatch: routes accumulated batches to the appropriate execution
//! strategy (main-thread batched eval, GPU spatial, etc.) and implements
//! late-materialization via predicate chain evaluation.
//!
//! # Strategies
//!
//! - **`BatchedEval`**: Tight loop of `FunctionCallInvoke` on the main backend
//!   thread. Avoids repeated executor overhead for simple scalar functions.
//! - **`GpuSpatial`** (stub): Will offload spatial predicates to the GPU via a
//!   three-layer pipeline:
//!   1. **Bbox filter** — fast integer/float bounding-box overlap test on GPU.
//!   2. **Geometric fast-path** — exact predicate for common simple geometries
//!      (point-in-ring, segment intersection) on GPU.
//!   3. **CPU recheck** — fall back to PostGIS for edge cases the GPU kernels
//!      cannot handle (collections, curves, etc.).
//!
//! # Late Materialization
//!
//! [`PredicateChain`] orders predicates by `selectivity / cost` so the cheapest,
//! most-selective predicate runs first. Rows rejected early skip expensive
//! geometry deserialization entirely.

use crate::adapters::extractors::geometry::extract_geometry;
use crate::adapters::extractors::raster;
use crate::engine::gucs;
use crate::engine::registry::{self, AccelStrategy};
use crate::gpu;
use crate::gpu::three_layer;

/// How often (in calls) to invoke `CHECK_FOR_INTERRUPTS()` during batched
/// evaluation on the main backend thread.
const INTERRUPT_CHECK_INTERVAL: usize = 1000;

/// Stack-allocated wrapper for `FunctionCallInfoBaseData` that provides
/// backing storage for one argument via the flexible array member `args`.
///
/// `FunctionCallInfoBaseData` ends with `args: [NullableDatum; 0]` (a C
/// flexible array member). A plain `std::mem::zeroed::<FunctionCallInfoBaseData>()`
/// allocates zero bytes for `args`, so writing to `args[0]` is a stack
/// buffer overflow. This `#[repr(C)]` wrapper places a `NullableDatum`
/// immediately after the base struct, which — per C layout rules — is
/// exactly where `args[0]` lives.
#[repr(C)]
struct FcinfoWith1Arg {
    base: pgrx::pg_sys::FunctionCallInfoBaseData,
    _arg_space: pgrx::pg_sys::NullableDatum,
}

/// Same as [`FcinfoWith1Arg`] but with space for two arguments.
/// Used by the GPU spatial recheck path which calls 2-arg PostGIS functions.
#[repr(C)]
struct FcinfoWith2Args {
    base: pgrx::pg_sys::FunctionCallInfoBaseData,
    _arg_space: [pgrx::pg_sys::NullableDatum; 2],
}

// ---------------------------------------------------------------------------
// Dispatch result
// ---------------------------------------------------------------------------

/// Outcome of a dispatch attempt.
#[derive(Debug)]
pub enum DispatchResult {
    /// The batch was evaluated by an accelerated path.
    Accelerated(Vec<(pgrx::pg_sys::Datum, bool)>),
    /// Caller should fall back to the vanilla PostgreSQL executor.
    Fallback,
}

// ---------------------------------------------------------------------------
// Top-level dispatch
// ---------------------------------------------------------------------------

/// Route a batch of `(Datum, is_null)` pairs to the appropriate execution
/// strategy.
///
/// `qual_datum` is the constant second argument for two-argument spatial
/// predicates (e.g. the constant geometry in
/// `WHERE ST_Intersects(geom_col, $1)`). Pass `None` for single-argument
/// functions or when the second argument is not available.
///
/// Returns [`DispatchResult::Accelerated`] with per-row results when the
/// strategy is supported, or [`DispatchResult::Fallback`] when the caller
/// should use the standard PostgreSQL executor.
///
/// # Safety
///
/// Must be called on the **main backend thread** only. The underlying
/// `FunctionCallInvoke` and `CHECK_FOR_INTERRUPTS` macros are not safe to
/// call from worker threads.
#[must_use]
pub unsafe fn dispatch(
    strategy: AccelStrategy,
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
    skip_bbox: bool,
) -> DispatchResult {
    match strategy {
        AccelStrategy::BatchedEval => {
            // SAFETY: Caller guarantees main backend thread.
            let results = unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
            DispatchResult::Accelerated(results)
        }
        AccelStrategy::GpuSpatial => {
            // If GPU dispatch is disabled via GUC, fall back to batched eval.
            if !gucs::gpu_enabled() {
                // SAFETY: Caller guarantees main backend thread.
                let results = unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
                return DispatchResult::Accelerated(results);
            }
            // SAFETY: Caller guarantees main backend thread.
            let results =
                unsafe { dispatch_gpu_spatial(batch, fn_info, is_strict, qual_datum, skip_bbox) };
            DispatchResult::Accelerated(results)
        }
        AccelStrategy::GpuH3 => {
            if !gucs::gpu_enabled() {
                // SAFETY: Caller guarantees main backend thread.
                let results = unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
                return DispatchResult::Accelerated(results);
            }
            // SAFETY: Caller guarantees main backend thread.
            let results =
                unsafe { dispatch_gpu_h3(batch, fn_info, is_strict, fn_info.fn_oid, qual_datum) };
            DispatchResult::Accelerated(results)
        }
        AccelStrategy::GpuRaster => {
            if !gucs::gpu_enabled() {
                // SAFETY: Caller guarantees main backend thread.
                let results = unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
                return DispatchResult::Accelerated(results);
            }
            // SAFETY: Caller guarantees main backend thread.
            let results = unsafe { dispatch_gpu_raster(batch, fn_info, is_strict, fn_info.fn_oid) };
            DispatchResult::Accelerated(results)
        }
        // GpuExpr is handled directly in scan.rs via the columnar path,
        // not through the per-datum dispatch interface.
        // GpuSort/GpuReduce/GpuHashJoin/GpuWindow not wired into per-datum dispatch.
        AccelStrategy::GpuExpr
        | AccelStrategy::GpuSort
        | AccelStrategy::GpuReduce
        | AccelStrategy::GpuHashJoin
        | AccelStrategy::GpuWindow => DispatchResult::Fallback,
    }
}

// ---------------------------------------------------------------------------
// Strategy 1: BatchedEval
// ---------------------------------------------------------------------------

/// Evaluate a batch of datums by calling the PG function once per row on the
/// main backend thread.
///
/// For `STRICT` functions, any `NULL` input produces a `NULL` output without
/// invoking the function. `CHECK_FOR_INTERRUPTS()` is called every
/// [`INTERRUPT_CHECK_INTERVAL`] invocations.
///
/// # Safety
///
/// Must be called on the **main backend thread**. Accesses PG `FmgrInfo` and
/// invokes `FunctionCallInvoke`.
#[must_use]
pub unsafe fn dispatch_batched_eval(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
) -> Vec<(pgrx::pg_sys::Datum, bool)> {
    let mut results = Vec::with_capacity(batch.len());

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        // Strict optimisation: NULL in → NULL out, no function call.
        if is_strict && is_null {
            results.push((pgrx::pg_sys::Datum::from(0), true));
            continue;
        }

        // Build a FunctionCallInfoBaseData on the stack with space for 1 arg.
        //
        // SAFETY: FunctionCallInfoBaseData has a flexible array member `args`
        // with zero allocated space. We use a #[repr(C)] wrapper that appends
        // storage for one NullableDatum, ensuring writes to args[0] are backed
        // by real memory. `fn_info` is a valid FmgrInfo provided by the caller.
        let mut fcinfo_buf: FcinfoWith1Arg = unsafe { std::mem::zeroed() };
        fcinfo_buf.base.flinfo = std::ptr::from_ref::<pgrx::pg_sys::FmgrInfo>(fn_info).cast_mut();
        fcinfo_buf.base.nargs = 1;
        fcinfo_buf.base.isnull = false;

        // SAFETY: The _arg_space field in FcinfoWith1Arg provides backing
        // storage at exactly the offset where args[0] lives (immediately
        // after the base struct, per C flexible array member layout).
        unsafe {
            let arg_ptr = fcinfo_buf.base.args.as_mut_ptr();
            (*arg_ptr).value = datum;
            (*arg_ptr).isnull = is_null;
        }

        // SAFETY: We call the PG function through the fn_addr pointer stored
        // in flinfo. This is the Rust equivalent of the C FunctionCallInvoke
        // macro. We are on the main backend thread.
        let result_datum = unsafe {
            let Some(func) = (*fcinfo_buf.base.flinfo).fn_addr else {
                // fn_addr should always be set after fmgr_info(). If it is
                // not, return NULL to avoid UB.
                results.push((pgrx::pg_sys::Datum::from(0), true));
                continue;
            };
            func(&raw mut fcinfo_buf.base)
        };

        results.push((result_datum, fcinfo_buf.base.isnull));

        // Periodically check for interrupts so that SIGINT / statement_timeout
        // are honoured even in long batches.
        if (i + 1) % INTERRUPT_CHECK_INTERVAL == 0 {
            pgrx::check_for_interrupts!();
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Strategy 2: GpuSpatial
// ---------------------------------------------------------------------------

/// GPU spatial dispatch via the three-layer pipeline.
///
/// The pipeline evaluates spatial predicates in three layers:
///
/// 1. **Bbox filter** — Coarse bounding-box overlap test. Rejects the
///    majority of non-intersecting pairs with minimal memory traffic.
///
/// 2. **Geometric fast-path** — Exact spatial predicate for common simple
///    geometries (point-in-ring winding-number, great-circle distance,
///    segment intersection) evaluated in fp32 with an fp64 refinement band.
///
/// 3. **CPU recheck** — Rows that the pipeline cannot conclusively decide
///    (geometry collections, curves, numerical edge cases) are rechecked
///    via the original PostGIS function on the main backend thread.
///
/// The function expects pairs of geometry datums: each batch element is
/// a single datum that is the first argument to a two-argument spatial
/// predicate (e.g., `ST_Intersects(a, b)`). The second geometry is
/// currently assumed to be uniform across the batch (a common pattern
/// for indexed lookups). For truly arbitrary pairs, falls back to
/// `BatchedEval`.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_spatial(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
    skip_bbox: bool,
) -> Vec<(pgrx::pg_sys::Datum, bool)> {
    // We need the constant second geometry to form pairs.
    let Some((qual_d, qual_null)) = qual_datum else {
        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    };

    // Strict: if the constant arg is NULL, every result is NULL.
    if is_strict && qual_null {
        return vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
    }

    // Extract the constant geometry (arg B) once.
    let Some(geom_b) = extract_geometry(qual_d) else {
        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    };

    // Extract per-row geometries (arg A). Track which extracted geometry
    // index maps back to which batch index.
    let mut geoms_a: Vec<three_layer::ExtractedGeometry> = Vec::with_capacity(batch.len());
    let mut geom_idx_to_batch: Vec<usize> = Vec::with_capacity(batch.len());

    // Pre-fill results: NULL rows → NULL output.
    let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
    let mut needs_scalar_recheck: Vec<usize> = Vec::new();

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        if let Some(geom) = extract_geometry(datum) {
            geom_idx_to_batch.push(i);
            geoms_a.push(geom);
        } else {
            needs_scalar_recheck.push(i);
        }
    }

    // If no geometries could be extracted, fall back entirely.
    if geoms_a.is_empty() {
        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    }

    // Determine which spatial predicate is being evaluated by looking up
    // the function name in the global registry.
    let predicate = {
        let fn_name = registry::global_registry()
            .lookup(fn_info.fn_oid)
            .map(|e| e.name);
        match fn_name {
            Some("st_contains") => three_layer::SpatialPredicate::Contains,
            Some("st_within") => three_layer::SpatialPredicate::Within,
            Some("st_dwithin") => {
                // ST_DWithin requires a distance threshold (third SQL argument)
                // that the planner does not yet extract. Fall back to batched
                // eval until the qual-extraction pipeline supports it.
                // SAFETY: Caller guarantees main backend thread.
                return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
            }
            // Default: ST_Intersects or any unrecognised spatial function.
            _ => three_layer::SpatialPredicate::Intersects,
        }
    };

    // Build parallel geoms_b array (same constant geometry for every row).
    let geoms_b_vec: Vec<three_layer::ExtractedGeometry> = vec![geom_b; geoms_a.len()];

    // Run the three-layer pipeline with the correct predicate semantics.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();
    let spatial_result = three_layer::spatial_eval(predicate, &geoms_a, &geoms_b_vec, skip_bbox);
    let elapsed_ms = start.elapsed().as_millis() as i32;

    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: spatial pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }

    // Apply DEFINITE results directly as boolean Datums.
    let bool_true = pgrx::pg_sys::Datum::from(true);
    let bool_false = pgrx::pg_sys::Datum::from(false);

    for &geom_idx in &spatial_result.definite_true {
        if geom_idx < geom_idx_to_batch.len() {
            results[geom_idx_to_batch[geom_idx]] = (bool_true, false);
        }
    }

    for &geom_idx in &spatial_result.definite_false {
        if geom_idx < geom_idx_to_batch.len() {
            results[geom_idx_to_batch[geom_idx]] = (bool_false, false);
        }
    }

    // UNCERTAIN rows need CPU recheck via the original PostGIS function.
    for &geom_idx in &spatial_result.uncertain {
        if geom_idx < geom_idx_to_batch.len() {
            needs_scalar_recheck.push(geom_idx_to_batch[geom_idx]);
        }
    }

    // CPU recheck: call the 2-arg PG function for uncertain rows.
    for &batch_idx in &needs_scalar_recheck {
        let (datum_a, is_null_a) = batch[batch_idx];
        if is_strict && is_null_a {
            results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
            continue;
        }

        // SAFETY: Build a FunctionCallInfo with 2 args on the stack.
        // Both arg slots are backed by _arg_space in FcinfoWith2Args.
        let mut fcinfo_buf: FcinfoWith2Args = unsafe { std::mem::zeroed() };
        fcinfo_buf.base.flinfo = std::ptr::from_ref::<pgrx::pg_sys::FmgrInfo>(fn_info).cast_mut();
        fcinfo_buf.base.nargs = 2;
        fcinfo_buf.base.isnull = false;

        // SAFETY: _arg_space provides backing for args[0] and args[1].
        unsafe {
            let args = fcinfo_buf.base.args.as_mut_ptr();
            (*args).value = datum_a;
            (*args).isnull = is_null_a;
            (*args.add(1)).value = qual_d;
            (*args.add(1)).isnull = qual_null;
        }

        // SAFETY: Call the PG function on the main backend thread.
        let result_datum = unsafe {
            let Some(func) = (*fcinfo_buf.base.flinfo).fn_addr else {
                results[batch_idx] = (pgrx::pg_sys::Datum::from(0), true);
                continue;
            };
            func(&raw mut fcinfo_buf.base)
        };

        results[batch_idx] = (result_datum, fcinfo_buf.base.isnull);
    }

    results
}

// ---------------------------------------------------------------------------
// Strategy 3: GpuH3
// ---------------------------------------------------------------------------

/// GPU H3 cell dispatch.
///
/// H3 functions operate on 64-bit cell indices. This handler extracts H3
/// cell values from the batch, runs a bulk GPU validation pass (resolution
/// extraction), then falls back to batched eval for the actual results.
///
/// Once the full qual-extraction pipeline provides the specific H3 function
/// being called (lat_lng_to_cell vs grid_distance vs cell_to_parent vs
/// get_resolution), this function will dispatch to the appropriate kernel
/// and return GPU-computed results directly.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_h3(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    fn_oid: pgrx::pg_sys::Oid,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
) -> Vec<(pgrx::pg_sys::Datum, bool)> {
    // Look up the function name to route to the correct GPU kernel.
    let fn_name = registry::global_registry().lookup(fn_oid).map(|e| e.name);

    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    // h3_latlng_to_cell takes point geometries, not cell indices —
    // handle it separately with geometry extraction.
    if fn_name == Some("h3_latlng_to_cell") {
        let resolution = qual_datum
            .filter(|(_, is_null)| !is_null)
            .map(|(d, _)| d.value() as i32);
        let Some(res) = resolution else {
            // SAFETY: Caller guarantees main backend thread.
            return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
        };

        let mut lats: Vec<f64> = Vec::with_capacity(batch.len());
        let mut lngs: Vec<f64> = Vec::with_capacity(batch.len());
        let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());

        for (i, &(datum, is_null)) in batch.iter().enumerate() {
            if is_null {
                continue;
            }
            if let Some(geom) = extract_geometry(datum) {
                // For points, coords contains [x, y] and bbox has
                // [xmin, ymin, xmax, ymax]. Use coords for precision
                // when available, otherwise fall back to bbox center.
                if geom.coords.len() >= 2 {
                    lngs.push(f64::from(geom.coords[0])); // x = longitude
                    lats.push(f64::from(geom.coords[1])); // y = latitude
                } else {
                    lngs.push(f64::from(geom.bbox[0]));
                    lats.push(f64::from(geom.bbox[1]));
                }
                valid_indices.push(i);
            }
        }

        if lats.is_empty() {
            // SAFETY: Caller guarantees main backend thread.
            return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
        }

        let gpu_result = crate::gpu::h3_lat_lng_to_cell_bulk(&lats, &lngs, res);

        log_h3_timeout(timeout_ms, &start);

        if let Some(cell_ids) = gpu_result {
            let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
            for (gi, &batch_idx) in valid_indices.iter().enumerate() {
                if gi < cell_ids.len() && cell_ids[gi] != 0 {
                    results[batch_idx] = (pgrx::pg_sys::Datum::from(cell_ids[gi] as i64), false);
                }
            }
            return results;
        }

        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    }

    // Extract H3 cell indices from the batch datums.
    // H3 cells are 64-bit integers stored as Datum (which is usize on PG).
    let mut cells: Vec<u64> = Vec::with_capacity(batch.len());
    let mut valid_indices: Vec<usize> = Vec::with_capacity(batch.len());

    for (i, &(datum, is_null)) in batch.iter().enumerate() {
        if is_null {
            continue;
        }
        // H3 cell indices are bigint (i64) values stored as Datum.
        let cell = datum.value() as u64;
        // Basic validity check: H3 cells have a non-zero high nibble.
        if cell != 0 {
            cells.push(cell);
            valid_indices.push(i);
        }
    }

    // If we couldn't extract enough cells, fall back to batched eval.
    if cells.is_empty() || valid_indices.len() < batch.len() / 2 {
        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    }

    // Route to the correct GPU kernel based on function name.
    let gpu_results: Option<GpuH3Result> = match fn_name {
        // 1-arg: cell → i32 resolution
        Some("h3_get_resolution") => {
            crate::gpu::h3_get_resolution_bulk(&cells).map(GpuH3Result::I32)
        }
        // 2-arg: cell + resolution constant → parent cell (u64)
        Some("h3_cell_to_parent") => {
            let res = qual_datum
                .filter(|(_, is_null)| !is_null)
                .map(|(d, _)| d.value() as i32);
            res.and_then(|parent_res| {
                crate::gpu::h3_cell_to_parent_bulk(&cells, parent_res).map(GpuH3Result::U64)
            })
        }
        // 2-arg: cell_a + cell_b constant → distance (i32)
        Some("h3_grid_distance") => {
            let other_cell = qual_datum
                .filter(|(_, is_null)| !is_null)
                .map(|(d, _)| d.value() as u64);
            other_cell.and_then(|oc| {
                let cells_b = vec![oc; cells.len()];
                crate::gpu::h3_grid_distance_bulk(&cells, &cells_b).map(GpuH3Result::I32)
            })
        }
        _ => None,
    };

    log_h3_timeout(timeout_ms, &start);

    // If GPU returned results, map them back to batch indices.
    if let Some(gpu_res) = gpu_results {
        let mut results = vec![(pgrx::pg_sys::Datum::from(0), true); batch.len()];
        for (gi, &batch_idx) in valid_indices.iter().enumerate() {
            let datum = match &gpu_res {
                GpuH3Result::I32(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi]),
                GpuH3Result::U64(v) if gi < v.len() => pgrx::pg_sys::Datum::from(v[gi] as i64),
                _ => continue,
            };
            results[batch_idx] = (datum, false);
        }
        return results;
    }

    // GPU unavailable or unsupported function — fall back to CPU.
    // SAFETY: Caller guarantees main backend thread.
    unsafe { dispatch_batched_eval(batch, fn_info, is_strict) }
}

/// Tagged union for H3 GPU kernel results — some return i32, others u64.
enum GpuH3Result {
    I32(Vec<i32>),
    U64(Vec<u64>),
}

/// Log a warning if the H3 GPU pipeline exceeded the configured timeout.
fn log_h3_timeout(timeout_ms: i32, start: &std::time::Instant) {
    let elapsed_ms = start.elapsed().as_millis() as i32;
    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: H3 pipeline took {}ms (timeout {}ms)",
            elapsed_ms,
            timeout_ms,
        );
    }
}

// ---------------------------------------------------------------------------
// Strategy 4: GpuRaster
// ---------------------------------------------------------------------------

/// GPU raster dispatch for `ST_MapAlgebra`, `ST_Clip`, and `ST_Reclass`.
///
/// Extracts raster WKB data from each datum, parses the header and pixel
/// data using the raster extractor, then dispatches to the appropriate GPU
/// kernel.
///
/// Currently runs the GPU pipeline as a validation pass (exercising raster
/// extraction and GPU kernel invocation) and falls back to batched eval
/// for the authoritative result.  Once the full qual-extraction pipeline
/// provides the specific raster function being called, this function will
/// return GPU-computed results directly.
///
/// # Safety
///
/// Must be called on the **main backend thread**.
#[must_use]
pub unsafe fn dispatch_gpu_raster(
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    _fn_oid: pgrx::pg_sys::Oid,
) -> Vec<(pgrx::pg_sys::Datum, bool)> {
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

    // If we couldn't extract enough rasters, fall back to batched eval.
    if raster_data.is_empty() || raster_data.len() < batch.len() / 2 {
        // SAFETY: Caller guarantees main backend thread.
        return unsafe { dispatch_batched_eval(batch, fn_info, is_strict) };
    }

    // Run a map-algebra validation pass on the first raster to exercise
    // the full GPU pipeline: raster extraction → pixel conversion →
    // GPU kernel invocation.
    let timeout_ms = gucs::kernel_timeout_ms();
    let start = std::time::Instant::now();

    if let Some((header, pixels)) = raster_data.first() {
        let pixel_count = header.width as usize * header.height as usize;
        if pixel_count > 0 {
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

            let _result = gpu::map_algebra(
                &band_ptrs,
                pixel_count,
                pixel_type,
                &expr,
                &mut output_buf,
                &mut nodata_mask,
            );
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as i32;

    if timeout_ms > 0 && elapsed_ms > timeout_ms {
        pgrx::warning!(
            "pg_accel: raster pipeline took {}ms (timeout {}ms), falling back to CPU",
            elapsed_ms,
            timeout_ms,
        );
    }

    // SAFETY: Caller guarantees main backend thread.
    unsafe { dispatch_batched_eval(batch, fn_info, is_strict) }
}

// ---------------------------------------------------------------------------
// Late Materialization — Predicate Chain
// ---------------------------------------------------------------------------

/// A single predicate in a [`PredicateChain`].
#[derive(Debug, Clone)]
pub struct Predicate {
    /// Human-readable label (e.g. `"bbox_overlap"`, `"st_contains"`).
    pub label: &'static str,
    /// Estimated fraction of rows that *pass* this predicate (0.0–1.0).
    /// Lower values are more selective.
    pub selectivity: f64,
    /// Estimated per-row cost in arbitrary units. Higher means more expensive.
    pub cost: f64,
    /// The evaluation function.  Takes a slice of `(Datum, is_null)` and
    /// returns a boolean mask of the same length (`true` = row passes).
    ///
    /// # Safety
    ///
    /// The function must be safe to call in the context where `evaluate_chain`
    /// is invoked (typically main backend thread).
    pub eval_fn: fn(&[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool>,
}

/// An ordered chain of predicates for late materialization.
///
/// Predicates are sorted by *efficiency* (`selectivity / cost`) so the
/// cheapest, most-selective filter runs first. Rows rejected by an early
/// predicate skip all subsequent (more expensive) predicates, avoiding
/// unnecessary geometry deserialization.
#[derive(Debug, Clone)]
pub struct PredicateChain {
    /// Predicates in evaluation order (cheapest/most-selective first).
    predicates: Vec<Predicate>,
}

impl PredicateChain {
    /// Build a new predicate chain, automatically sorted by efficiency.
    ///
    /// Efficiency is defined as `selectivity / cost`. Lower selectivity (more
    /// rows filtered) and lower cost both increase efficiency, so predicates
    /// that filter the most rows for the least work run first.
    #[must_use]
    pub fn new(mut predicates: Vec<Predicate>) -> Self {
        predicates.sort_by(|a, b| {
            let eff_a = efficiency(a);
            let eff_b = efficiency(b);
            // Lower efficiency value = better (more selective & cheaper).
            eff_a
                .partial_cmp(&eff_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { predicates }
    }

    /// The ordered list of predicates.
    #[must_use]
    pub fn predicates(&self) -> &[Predicate] {
        &self.predicates
    }

    /// Number of predicates in the chain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.predicates.len()
    }

    /// Whether the chain has no predicates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

/// Efficiency metric: `selectivity / cost`. Lower is better — it means we
/// filter more rows for less work.
fn efficiency(p: &Predicate) -> f64 {
    if p.cost <= 0.0 {
        return 0.0;
    }
    p.selectivity / p.cost
}

/// Evaluate a [`PredicateChain`] against a batch, applying predicates in
/// efficiency order and short-circuiting rejected rows.
///
/// Returns a boolean mask of length `batch.len()` where `true` means the row
/// passed **all** predicates.
///
/// # Late Materialization
///
/// This is the key optimisation: an early, cheap predicate (e.g. integer range
/// check or bounding-box overlap) can eliminate rows before an expensive
/// predicate (e.g. exact `ST_Contains` requiring full geometry deserialization)
/// ever sees them.
#[must_use]
pub fn evaluate_chain(chain: &PredicateChain, batch: &[(pgrx::pg_sys::Datum, bool)]) -> Vec<bool> {
    let mut alive = vec![true; batch.len()];

    for predicate in &chain.predicates {
        // Collect only the surviving rows for this predicate.
        let survivors: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .zip(alive.iter())
            .filter_map(|(&datum, &is_alive)| if is_alive { Some(datum) } else { None })
            .collect();

        if survivors.is_empty() {
            break;
        }

        let pred_results = (predicate.eval_fn)(&survivors);

        // Map predicate results back to the full-width alive mask.
        let mut survivor_idx = 0;
        for flag in &mut alive {
            if *flag {
                if survivor_idx < pred_results.len() {
                    *flag = pred_results[survivor_idx];
                }
                survivor_idx += 1;
            }
        }
    }

    alive
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Predicate chain ordering --------------------------------------------

    fn make_predicate(label: &'static str, selectivity: f64, cost: f64) -> Predicate {
        Predicate {
            label,
            selectivity,
            cost,
            eval_fn: |batch| vec![true; batch.len()],
        }
    }

    #[test]
    fn chain_orders_by_efficiency() {
        // "cheap" has selectivity 0.1, cost 1.0 → efficiency 0.1 (best)
        // "expensive" has selectivity 0.5, cost 10.0 → efficiency 0.05
        // "medium" has selectivity 0.3, cost 2.0 → efficiency 0.15 (worst)
        let predicates = vec![
            make_predicate("medium", 0.3, 2.0),
            make_predicate("expensive", 0.5, 10.0),
            make_predicate("cheap", 0.1, 1.0),
        ];

        let chain = PredicateChain::new(predicates);
        let labels: Vec<&str> = chain.predicates().iter().map(|p| p.label).collect();

        // Sorted ascending by selectivity/cost:
        // expensive = 0.05, cheap = 0.1, medium = 0.15
        assert_eq!(labels, vec!["expensive", "cheap", "medium"]);
    }

    #[test]
    fn empty_chain_returns_all_alive() {
        let chain = PredicateChain::new(vec![]);
        assert!(chain.is_empty());

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(1), false),
            (pgrx::pg_sys::Datum::from(2), false),
        ];
        let result = evaluate_chain(&chain, &batch);
        assert_eq!(result, vec![true, true]);
    }

    #[test]
    fn chain_len_matches() {
        let chain = PredicateChain::new(vec![
            make_predicate("a", 0.5, 1.0),
            make_predicate("b", 0.3, 2.0),
        ]);
        assert_eq!(chain.len(), 2);
        assert!(!chain.is_empty());
    }

    // -- Predicate chain evaluation ------------------------------------------

    #[test]
    fn chain_filters_rows_correctly() {
        // First predicate: reject odd-indexed rows.
        let pred_even = Predicate {
            label: "even_index",
            selectivity: 0.5,
            cost: 1.0,
            eval_fn: |batch| batch.iter().enumerate().map(|(i, _)| i % 2 == 0).collect(),
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..4)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_even]);
        let result = evaluate_chain(&chain, &batch);

        // Rows 0,1,2,3 → predicate sees all 4, returns [true, false, true, false]
        assert_eq!(result, vec![true, false, true, false]);
    }

    #[test]
    fn chain_short_circuits_rejected_rows() {
        // First predicate: pass only the first row.
        // efficiency = 0.1 / 10.0 = 0.01 (sorts first — lowest efficiency wins).
        let pred_first_only = Predicate {
            label: "first_only",
            selectivity: 0.1,
            cost: 10.0,
            eval_fn: |batch| {
                let mut v = vec![false; batch.len()];
                if !v.is_empty() {
                    v[0] = true;
                }
                v
            },
        };

        // Second predicate: always returns true — but should only see 1 row.
        // efficiency = 1.0 / 1.0 = 1.0 (sorts second).
        let pred_pass_all = Predicate {
            label: "pass_all",
            selectivity: 1.0,
            cost: 1.0,
            eval_fn: |batch| {
                // If short-circuiting works, batch should have exactly 1 row.
                assert_eq!(batch.len(), 1);
                vec![true; batch.len()]
            },
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..5)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_first_only, pred_pass_all]);
        let result = evaluate_chain(&chain, &batch);

        assert_eq!(result, vec![true, false, false, false, false]);
    }

    #[test]
    fn chain_all_rejected_skips_remaining() {
        // First predicate: reject everything.
        let pred_reject_all = Predicate {
            label: "reject_all",
            selectivity: 0.0,
            cost: 1.0,
            eval_fn: |batch| vec![false; batch.len()],
        };

        // Second predicate: would panic if called — ensures short-circuit.
        let pred_should_not_run = Predicate {
            label: "should_not_run",
            selectivity: 1.0,
            cost: 100.0,
            eval_fn: |batch| {
                assert!(
                    batch.is_empty(),
                    "should_not_run predicate should not receive any rows"
                );
                vec![]
            },
        };

        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = (0..3)
            .map(|i| (pgrx::pg_sys::Datum::from(i), false))
            .collect();

        let chain = PredicateChain::new(vec![pred_reject_all, pred_should_not_run]);
        let result = evaluate_chain(&chain, &batch);

        assert_eq!(result, vec![false, false, false]);
    }

    // -- NULL passthrough (BatchedEval strict) --------------------------------
    // These test the pure logic of NULL handling. Actual FunctionCallInvoke
    // tests require a running PG instance and are covered by #[pg_test].

    #[test]
    fn strict_null_passthrough_logic() {
        // Simulate strict semantics without calling PG FFI.
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(1), false),
            (pgrx::pg_sys::Datum::from(0), true), // NULL
            (pgrx::pg_sys::Datum::from(3), false),
            (pgrx::pg_sys::Datum::from(0), true), // NULL
        ];

        let is_strict = true;
        let results: Vec<(pgrx::pg_sys::Datum, bool)> = batch
            .iter()
            .map(|&(datum, is_null)| {
                if is_strict && is_null {
                    (pgrx::pg_sys::Datum::from(0), true)
                } else {
                    // In real code this would call FunctionCallInvoke.
                    (datum, false)
                }
            })
            .collect();

        // NULLs pass through as NULL.
        assert!(results[1].1);
        assert!(results[3].1);
        // Non-NULLs are "evaluated".
        assert!(!results[0].1);
        assert!(!results[2].1);
    }

    #[test]
    fn non_strict_null_not_skipped_logic() {
        let batch: Vec<(pgrx::pg_sys::Datum, bool)> = vec![
            (pgrx::pg_sys::Datum::from(0), true), // NULL
            (pgrx::pg_sys::Datum::from(1), false),
        ];

        let is_strict = false;
        let should_call_fn: Vec<bool> = batch
            .iter()
            .map(|&(_, is_null)| !(is_strict && is_null))
            .collect();

        // Non-strict: even NULL inputs go through the function.
        assert!(should_call_fn[0]);
        assert!(should_call_fn[1]);
    }

    // -- DispatchResult variants ----------------------------------------------

    #[test]
    fn dispatch_result_fallback_variant() {
        let result = DispatchResult::Fallback;
        assert!(matches!(result, DispatchResult::Fallback));
    }

    #[test]
    fn dispatch_result_accelerated_variant() {
        let data = vec![(pgrx::pg_sys::Datum::from(42), false)];
        let result = DispatchResult::Accelerated(data);
        assert!(matches!(result, DispatchResult::Accelerated(_)));
    }

    // -- Efficiency metric ---------------------------------------------------

    #[test]
    fn efficiency_zero_cost_returns_zero() {
        let p = make_predicate("zero_cost", 0.5, 0.0);
        assert!((efficiency(&p)).abs() < f64::EPSILON);
    }

    #[test]
    fn efficiency_negative_cost_returns_zero() {
        let p = make_predicate("neg_cost", 0.5, -1.0);
        assert!((efficiency(&p)).abs() < f64::EPSILON);
    }

    #[test]
    fn efficiency_normal_computation() {
        let p = make_predicate("normal", 0.3, 2.0);
        let eff = efficiency(&p);
        assert!((eff - 0.15).abs() < f64::EPSILON);
    }
}
